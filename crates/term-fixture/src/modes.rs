//! Fixture mode implementations (`06-verification.md` §2 table, all modes
//! plus the O07 `agent-fake` scenario player).
//!
//! Every mode is deterministic given its arguments, performs no network
//! access, and touches only files it was explicitly pointed at.

use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::rng::XorShift64;

/// Page size used to touch allocated memory so RSS actually grows.
const PAGE: usize = 4096;

pub fn echo() -> i32 {
    // Raw byte round-trip: read a chunk, write it back, flush. Used for
    // UTF-8 split and paste comparisons, so no interpretation of the bytes.
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 4096];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => return 0,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).is_err() {
                    return 1;
                }
                if stdout.flush().is_err() {
                    return 1;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return 1,
        }
    }
}

pub fn flood(bytes: u64, chunk: usize, seed: u64, hold_ms: u64) -> i32 {
    let chunk = chunk.max(1);
    let mut rng = XorShift64::new(seed);
    let mut hasher = Sha256::new();
    let mut stdout = std::io::stdout();
    let mut remaining = bytes as usize;
    let mut written = 0usize;
    let mut buf = vec![0u8; chunk.min(remaining.max(1))];
    // The pattern is mapped onto printable ASCII ('!'..'~'): still a pure
    // function of (position, seed) and hash-verifiable, AND it survives
    // every terminal transport — Rust's Windows console-mode stdout rejects
    // non-UTF-8 byte sequences, and ConPTY runs the stream through conhost.
    while remaining > 0 {
        let take = buf.len().min(remaining);
        rng.fill(&mut buf);
        for b in buf.iter_mut() {
            *b = b'!' + (*b % 94);
        }
        let out = &buf[..take];
        if let Err(e) = stdout.write_all(out) {
            eprintln!("term-fixture flood: stdout write failed after {written} bytes: {e}");
            return 1;
        }
        hasher.update(out);
        written += take;
        remaining -= take;
    }
    let _ = stdout.flush();
    let hash = hasher.finalize();
    eprintln!(
        "term-fixture flood bytes={} sha256={}",
        bytes,
        hash.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    if hold_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(hold_ms));
    }
    0
}

pub fn tui(seed: u64) -> i32 {
    let mut rng = XorShift64::new(seed);
    let mut out = std::io::stdout();
    fn w(out: &mut impl Write, s: &str) {
        let _ = out.write_all(s.as_bytes());
    }

    // Drain terminal query responses (CPR / DA / size replies) on a side
    // thread so the fixture never blocks on them; responses are counted only.
    let responses = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = responses.clone();
    let reader = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 256];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(_) => break,
            }
        }
    });

    w(&mut out, "\x1b[?1049h"); // enter alternate screen
    w(&mut out, "\x1b[?25l"); // hide cursor
    let start = Instant::now();
    for i in 0..40u32 {
        // Seeded cursor moves + erase + colors; content varies per seed.
        let row = 1 + (rng.next_u64() % 24) as u32;
        let col = 1 + (rng.next_u64() % 60) as u32;
        let fg = 30 + (rng.next_u64() % 8) as u32;
        let bg = 90 + (rng.next_u64() % 8) as u32;
        let _ = out.write_all(
            format!("\x1b[{row};{col}H\x1b[{fg};{bg}m\x1b[2Kseed={seed} frame={i}\x1b[0m")
                .as_bytes(),
        );
        // Device attributes + cursor position + size queries (responses land
        // on the drain thread).
        w(&mut out, "\x1b[c\x1b[6n\x1b[18t");
        let _ = out.flush();
        std::thread::sleep(Duration::from_millis(10));
        if start.elapsed() >= Duration::from_millis(400) {
            break;
        }
    }
    w(&mut out, "\x1b[?25h"); // show cursor
    w(&mut out, "\x1b[?1049l"); // leave alternate screen — screen restore check
    let _ = out.flush();
    eprintln!(
        "term-fixture tui seed={} query_responses={}",
        seed,
        responses.load(std::sync::atomic::Ordering::Relaxed)
    );
    // Exit without joining the blocked stdin reader thread.
    drop(reader);
    0
}

pub fn memory(mib: usize, hold_ms: u64) -> i32 {
    let len = mib.saturating_mul(1024 * 1024);
    let mut buf: Vec<u8> = vec![0u8; len];
    // Touch every page with volatile writes plus a read-back so the
    // allocator actually commits pages and RSS grows (no madvise tricks).
    let mut acc: u8 = 0;
    let (pages, _) = buf.as_chunks_mut::<PAGE>();
    for (page, b) in pages.iter_mut().enumerate() {
        unsafe { std::ptr::write_volatile(b.as_mut_ptr(), page as u8) };
        acc = acc.wrapping_add(b[0]);
    }
    std::hint::black_box(acc);
    eprintln!("term-fixture memory mib={} touched", mib);
    std::thread::sleep(Duration::from_millis(hold_ms));
    drop(buf);
    0
}

pub fn cpu(workers: usize, duration_ms: u64) -> i32 {
    let workers = workers.max(1);
    let deadline = Instant::now() + Duration::from_millis(duration_ms);
    let mut handles = Vec::with_capacity(workers);
    for i in 0..workers {
        handles.push(std::thread::spawn(move || {
            let mut rng = XorShift64::new(0xC0FF_EE00 + i as u64);
            let mut sink = 0u64;
            while Instant::now() < deadline {
                sink = sink.wrapping_add(rng.next_u64());
            }
            std::hint::black_box(sink);
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    eprintln!(
        "term-fixture cpu workers={} duration_ms={}",
        workers, duration_ms
    );
    0
}

pub fn tree(children: usize, depth: usize, hold_ms: u64, root_early_exit_ms: Option<u64>) -> i32 {
    if depth == 0 {
        std::thread::sleep(Duration::from_millis(hold_ms));
        return 0;
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return 1,
    };
    let mut kids = Vec::with_capacity(children);
    for _ in 0..children {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("tree")
            .arg("--children")
            .arg(children.to_string())
            .arg("--depth")
            .arg((depth - 1).to_string())
            .arg("--hold-ms")
            .arg(hold_ms.to_string());
        if let Some(early) = root_early_exit_ms {
            cmd.arg("--root-early-exit-ms").arg(early.to_string());
            // Descendants a root leaves behind survive the terminal hangup,
            // as real ones do (`nohup`, dev servers, MCP helpers). On Unix,
            // when the PTY's session leader exits the kernel SIGHUPs the
            // foreground process group; without this the children would die
            // with the root and "root exit with live owned descendants"
            // (B08) could not be exercised at all — the workload would then
            // correctly be SUCCEEDED because nothing is left. SIG_IGN is
            // inherited across execve, so deeper levels keep it too.
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                // SAFETY: signal(2) is async-signal-safe; the closure runs
                // in the forked child right before exec and touches no
                // shared state.
                unsafe {
                    cmd.pre_exec(|| {
                        libc::signal(libc::SIGHUP, libc::SIG_IGN);
                        Ok(())
                    });
                }
            }
        }
        match cmd.spawn() {
            Ok(c) => kids.push(c),
            Err(_) => return 1,
        }
    }
    if let Some(early) = root_early_exit_ms {
        // Root leaves descendants behind on purpose (B08-style check: root
        // exit must not be misread as workload completion).
        std::thread::sleep(Duration::from_millis(early));
        return 0;
    }
    let mut code = 0;
    for mut k in kids {
        match k.wait() {
            Ok(st) if !st.success() => code = 1,
            Err(_) => code = 1,
            _ => {}
        }
    }
    code
}

pub fn escape() -> i32 {
    #[cfg(unix)]
    {
        // Detach into a new session to exercise ownership-observation limits.
        let rc = unsafe { libc::setsid() };
        println!("term-fixture escape: setsid() = {rc}");
        std::thread::sleep(Duration::from_millis(300));
        0
    }
    #[cfg(windows)]
    {
        println!("term-fixture escape: setsid is a Unix concept; Windows no-op");
        0
    }
}

pub fn side_effect(file: &Path) -> i32 {
    match OpenOptions::new().write(true).create_new(true).open(file) {
        Ok(mut f) => {
            let body = format!("term-fixture side-effect pid={}\n", std::process::id());
            let _ = f.write_all(body.as_bytes());
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!("term-fixture side-effect: {file:?} already exists (duplicate run)");
            3
        }
        Err(e) => {
            eprintln!("term-fixture side-effect: {e}");
            2
        }
    }
}

pub fn gate_observer(marker: &Path) -> i32 {
    // Written AT START: existence proves the target process was created.
    // The gate invariant under test is "marker file ABSENT before RELEASE".
    let body = format!("pid={}\n", std::process::id());
    let wrote = std::fs::write(marker, &body)
        .or_else(|_| {
            // A re-run on the same path must still be observable.
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .create(true)
                .open(marker)
                .and_then(|mut f| {
                    f.write_all(body.as_bytes())?;
                    f.sync_all()
                })
        })
        .is_ok();
    if !wrote {
        eprintln!("term-fixture gate-observer: cannot write marker {marker:?}");
        return 2;
    }
    // Hold briefly so start/exit are separately observable over the gate.
    std::thread::sleep(Duration::from_millis(300));
    0
}

pub fn io(bytes: u64, file: &PathBuf) -> i32 {
    let total = bytes as usize;
    let mut rng = XorShift64::new(7);
    let mut buf = vec![0u8; 64 * 1024];
    let t0 = Instant::now();
    match std::fs::File::create(file) {
        Ok(mut f) => {
            let mut left = total;
            while left > 0 {
                let take = buf.len().min(left);
                rng.fill(&mut buf);
                if f.write_all(&buf[..take]).is_err() {
                    return 1;
                }
                left -= take;
            }
            let _ = f.sync_all();
        }
        Err(e) => {
            eprintln!("term-fixture io: create failed: {e}");
            return 1;
        }
    }
    let write_ms = t0.elapsed().as_millis();
    let t1 = Instant::now();
    match std::fs::File::open(file) {
        Ok(mut f) => {
            let mut left = total;
            while left > 0 {
                let take = buf.len().min(left);
                match f.read(&mut buf[..take]) {
                    Ok(0) => {
                        eprintln!("term-fixture io: short read");
                        return 1;
                    }
                    Ok(n) => left -= n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return 1,
                }
            }
        }
        Err(e) => {
            eprintln!("term-fixture io: open failed: {e}");
            return 1;
        }
    }
    let read_ms = t1.elapsed().as_millis();
    eprintln!("term-fixture io bytes={total} write_ms={write_ms} read_ms={read_ms}");
    0
}

pub fn exit(code: i32, delay_ms: u64) -> i32 {
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
    code
}

// ---- agent-fake (O07: docs/orchestration/03-adapters.md §7) ----------------
//
// Scenario player for the fake agent adapter: the daemon passes the scripted
// scenario as base64 JSON in argv, this mode replays it on stdout as JSONL
// protocol lines. Timing sleeps are wall clock — the daemon side owns the
// timing policy. Mirrors iyagi-termd's `agent_runtime::fake::FakeScript`
// shape; `result`/`late_result_after_cancel` values stay opaque JSON.

/// O07 fake adapter scenario (wire mirror of FakeScript).
#[derive(Debug, Deserialize)]
pub struct AgentScenario {
    #[serde(default)]
    pub steps: Vec<AgentStep>,
    /// Cancel accepted but alive: keep playing regardless (03 §7); the
    /// supervisor's force-kill path is what gets exercised.
    #[serde(default)]
    pub ignore_interrupt: bool,
    /// Hold after the last step before exiting (late exit after cancel).
    #[serde(default)]
    pub exit_late_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentStep {
    Started {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        turn_id: Option<String>,
    },
    Activity {
        text: String,
    },
    Approval {
        request_id: String,
        question: String,
    },
    Usage {
        #[serde(default)]
        input_tokens: Option<u64>,
        #[serde(default)]
        output_tokens: Option<u64>,
        #[serde(default)]
        cost_usd_micros: Option<u64>,
    },
    Result {
        value: serde_json::Value,
    },
    LateResultAfterCancel {
        value: serde_json::Value,
    },
    Fail {
        code: String,
        message: String,
    },
    Disconnect,
    Delay {
        ms: u64,
    },
    FileWrite {
        path: String,
        bytes: usize,
    },
    FloodStdout {
        bytes: u64,
    },
    NoNewline {
        bytes: u64,
    },
    PartialJson,
    ExitLate {
        ms: u64,
    },
}

/// Replay the scenario. Every protocol line is flushed immediately so the
/// pipe reader observes the stream incrementally.
pub fn agent_fake(scenario_b64: &str) -> i32 {
    use base64::Engine;
    let decoded = match base64::engine::general_purpose::STANDARD.decode(scenario_b64) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("term-fixture agent-fake: scenario base64 decode failed: {e}");
            return 2;
        }
    };
    let scenario: AgentScenario = match serde_json::from_slice(&decoded) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("term-fixture agent-fake: scenario JSON parse failed: {e}");
            return 2;
        }
    };
    // 03 §7 "cancel accepted but alive": with ignore_interrupt the child
    // must survive the ladder's interrupt step (SIGINT on Unix) and end
    // only at terminate/kill — mirroring the in-process engine, which keeps
    // playing the script after cancel. Without this, default SIGINT
    // disposition ends the child in the first grace window and the stop
    // ladder correctly reports Exited{code: None}, not Killed. Windows
    // piped children get no interrupt signal at all, so nothing to do.
    #[cfg(unix)]
    if scenario.ignore_interrupt {
        // SIGINT = 2, SIG_IGN = 1 — same libc-by-hand style as the
        // supervisor's ownership signals (no libc dependency here).
        extern "C" {
            fn signal(signum: i32, handler: isize) -> isize;
        }
        unsafe {
            signal(2, 1);
        }
    }
    eprintln!(
        "term-fixture agent-fake pid={} steps={} ignore_interrupt={} exit_late_ms={}",
        std::process::id(),
        scenario.steps.len(),
        scenario.ignore_interrupt,
        scenario.exit_late_ms
    );
    let mut out = std::io::stdout().lock();
    for step in &scenario.steps {
        match step {
            AgentStep::Started {
                session_id,
                turn_id,
            } => emit(
                &mut out,
                serde_json::json!({
                    "t": "started",
                    "session_id": session_id,
                    "turn_id": turn_id,
                }),
            ),
            AgentStep::Activity { text } => {
                emit(&mut out, serde_json::json!({"t": "activity", "text": text}))
            }
            AgentStep::Approval {
                request_id,
                question,
            } => {
                emit(
                    &mut out,
                    serde_json::json!({
                        "t": "approval",
                        "request_id": request_id,
                        "question": question,
                    }),
                );
                // Wait for one answer line on stdin (EOF = nobody answering;
                // the scenario continues either way).
                let stdin = std::io::stdin();
                let mut line = String::new();
                let _ = stdin.lock().read_line(&mut line);
            }
            AgentStep::Usage {
                input_tokens,
                output_tokens,
                cost_usd_micros,
            } => emit(
                &mut out,
                serde_json::json!({
                    "t": "usage",
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "cost_usd_micros": cost_usd_micros,
                }),
            ),
            AgentStep::Result { value } => {
                emit(&mut out, serde_json::json!({"t": "result", "value": value}))
            }
            // The child cannot observe the daemon's fencing gate, so the
            // late result replays as a plain final here; stale-token drop
            // semantics are exercised by the in-process engine.
            AgentStep::LateResultAfterCancel { value } => {
                emit(&mut out, serde_json::json!({"t": "result", "value": value}))
            }
            AgentStep::Fail { code, message } => emit(
                &mut out,
                serde_json::json!({"t": "failed", "code": code, "message": message}),
            ),
            AgentStep::Disconnect => emit(&mut out, serde_json::json!({"t": "disconnect"})),
            AgentStep::Delay { ms } => std::thread::sleep(Duration::from_millis(*ms)),
            AgentStep::FileWrite { path, bytes } => {
                // Declared write first (the exec-side police hook reads the
                // marker), then the actual deterministic bytes.
                emit(
                    &mut out,
                    serde_json::json!({"t": "file_write", "path": path, "bytes": bytes}),
                );
                if let Err(e) = write_pattern_file(Path::new(path), *bytes) {
                    eprintln!("term-fixture agent-fake: file_write {path:?} failed: {e}");
                    return 2;
                }
            }
            AgentStep::FloodStdout { bytes } => {
                // Newline-terminated chunks: many bounded lines (the spool
                // must evict while the sink keeps everything).
                let mut rng = XorShift64::new(0xA007_FA7E);
                let payload = 64 * 1024 - 1; // pattern bytes per line
                let mut block = vec![0u8; payload];
                let mut remaining = *bytes as usize;
                while remaining > 0 {
                    let take = payload.min(remaining);
                    rng.fill(&mut block);
                    for b in block[..take].iter_mut() {
                        *b = b'!' + (*b % 94);
                    }
                    if out.write_all(&block[..take]).is_err() {
                        eprintln!("term-fixture agent-fake: flood write failed");
                        return 1;
                    }
                    if out.write_all(b"\n").is_err() {
                        return 1;
                    }
                    remaining -= take;
                }
                let _ = out.flush();
            }
            AgentStep::NoNewline { bytes } => {
                // 1 MiB+ without any newline: the reader must cut at the cap
                // and mark the run RESULT_INVALID (03 §2).
                let mut remaining = *bytes as usize;
                let block = vec![b'x'; 64 * 1024];
                while remaining > 0 {
                    let take = block.len().min(remaining);
                    if out.write_all(&block[..take]).is_err() {
                        return 1;
                    }
                    remaining -= take;
                }
                let _ = out.flush();
            }
            AgentStep::PartialJson => {
                // E19: truncated final line, no newline, then exit 0 — the
                // adapter must mark RESULT_INVALID, not success.
                let _ = out.write_all(br#"{"t":"result","value":{"kind":"report","report_t"#);
                let _ = out.flush();
                return 0;
            }
            AgentStep::ExitLate { ms } => {
                std::thread::sleep(Duration::from_millis(*ms));
                return 0;
            }
        }
    }
    let _ = out.flush();
    if scenario.exit_late_ms > 0 {
        std::thread::sleep(Duration::from_millis(scenario.exit_late_ms));
    }
    0
}

fn emit<W: Write>(out: &mut W, value: serde_json::Value) {
    let mut line = serde_json::to_string(&value).unwrap_or_else(|_| "{}".into());
    line.push('\n');
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// Fixed printable pattern (seed-independent position math keeps it a pure
/// function of the requested size).
fn write_pattern_file(path: &Path, bytes: usize) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    let block = vec![b'w'; 64 * 1024];
    let mut remaining = bytes;
    let mut salt = 0u8;
    while remaining > 0 {
        let take = block.len().min(remaining);
        let mut chunk = block[..take].to_vec();
        for b in chunk.iter_mut() {
            *b = b'a' + (salt % 26);
        }
        salt = salt.wrapping_add(1);
        file.write_all(&chunk)?;
        remaining -= take;
    }
    file.sync_all()
}
