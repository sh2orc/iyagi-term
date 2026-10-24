//! 네이티브 프로세스 열거·계량(macOS libproc, Linux `/proc`).
//!
//! sysinfo의 macOS 갱신은 갱신 종류와 상관없이 모든 프로세스마다
//! `KERN_PROCARGS2`(argv+env 전체)를 두 번씩 읽는다. 프로세스 800개면 전체
//! 스캔 한 번에 9 ms쯤 들었고, 셸 pane마다 2초에 한 번씩 돌아 데몬 CPU의
//! 절반 이상이 커널 시간이었다(같은 사용자의 모든 env — API 키 포함 — 가 초당
//! 열 번 넘게 데몬 힙을 거쳐 가기도 했다). 여기서는:
//!
//! * 목록: pid·ppid·시작 시각·uid·이름만 읽는다(macOS `proc_listallpids` +
//!   `PROC_PIDTBSDINFO`, 약 1 µs/pid; Linux `/proc/<pid>/stat`).
//!   [`snapshot`]은 최대 나이 안이면 캐시를 공유한다 — 같은 틱의 셸 관측·
//!   에이전트 감시·그룹 멤버 열거가 스캔 한 번을 나눠 쓴다.
//! * 계량: [`pid_usage`]가 한 pid의 CPU(회수된 자식 포함)·footprint·디스크
//!   IO를 읽는다(macOS `proc_pid_rusage(RUSAGE_INFO_V4)`, Linux
//!   `stat`/`status`/`io`). footprint는 Activity Monitor의 "메모리"와 같은
//!   기준이다 — RSS 합은 공유 페이지를 이중으로 세고 압축된 페이지를 빼서
//!   ±2배까지 어긋났다.
//! * argv·env는 필요한 pid에만([`argv`], [`env_var`]) — 에이전트 감지가
//!   트리 멤버에 대해서만, 프로세스마다 한 번 읽는다.
//!
//! Windows는 이 모듈이 비어 있다([`snapshot`]이 `None`) — 호출자는 sysinfo
//! 경로로 돌아간다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use term_contracts::ids::ProcessIdentity;

/// 열거된 프로세스 한 개.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    /// [`crate::identity`]와 같은 형식의 시작 토큰(macOS µs, Linux tick).
    pub start_token: String,
    /// 시작 시각(Unix epoch ms). 새 프로세스 판정에 쓴다.
    pub start_epoch_ms: u64,
    pub uid: u32,
    pub name: String,
}

/// 한 시점의 프로세스 목록과 부모→자식 색인.
#[derive(Debug)]
pub struct Snapshot {
    pub taken_at: Instant,
    entries: HashMap<u32, ProcEntry>,
    children: HashMap<u32, Vec<u32>>,
}

impl Snapshot {
    /// 목록으로 스냅샷을 만든다(자식 목록은 pid 순 — 감지 결과가 틱마다
    /// 흔들리지 않게).
    pub fn from_entries(entries: Vec<ProcEntry>) -> Self {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for entry in &entries {
            if entry.ppid != entry.pid {
                children.entry(entry.ppid).or_default().push(entry.pid);
            }
        }
        for kids in children.values_mut() {
            kids.sort_unstable();
        }
        Self {
            taken_at: Instant::now(),
            entries: entries.into_iter().map(|e| (e.pid, e)).collect(),
            children,
        }
    }

    pub fn get(&self, pid: u32) -> Option<&ProcEntry> {
        self.entries.get(&pid)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &ProcEntry> {
        self.entries.values()
    }

    pub fn children(&self, pid: u32) -> &[u32] {
        self.children.get(&pid).map(Vec::as_slice).unwrap_or(&[])
    }

    /// 루트(포함)와 관찰된 자손 pid(BFS, 부모 먼저). 루트가 없으면 빈 목록.
    /// pid 재사용·갱신 경주로 ppid가 순환해도 각 pid는 한 번만 담긴다.
    pub fn descendants(&self, root: u32) -> Vec<u32> {
        if !self.entries.contains_key(&root) {
            return Vec::new();
        }
        let mut seen = std::collections::HashSet::from([root]);
        let mut out = vec![root];
        let mut index = 0;
        while index < out.len() {
            let pid = out[index];
            index += 1;
            for &kid in self.children(pid) {
                if seen.insert(kid) {
                    out.push(kid);
                }
            }
        }
        out
    }

    /// 스냅샷 시점의 신원(pid 재사용 방어 — [`ProcessIdentity::same_process`]).
    pub fn identity(&self, pid: u32) -> Option<ProcessIdentity> {
        self.entries.get(&pid).map(|entry| ProcessIdentity {
            pid,
            start_token: entry.start_token.clone(),
            boot_id: crate::identity::boot_id(),
        })
    }

    /// 기록된 신원이 스냅샷 시점에도 같은 프로세스인가.
    pub fn is_alive(&self, recorded: &ProcessIdentity) -> bool {
        self.entries
            .get(&recorded.pid)
            .is_some_and(|entry| entry.start_token == recorded.start_token)
    }
}

/// 공유 스냅샷 캐시.
static SHARED: Mutex<Option<Arc<Snapshot>>> = Mutex::new(None);

/// 최대 나이 안의 공유 스냅샷, 없으면 새로 찍는다. `Duration::ZERO`면 항상
/// 새로 찍는다(정지·양보 같은 집행 경로). 미지원 플랫폼은 `None`.
pub fn snapshot(max_age: Duration) -> Option<Arc<Snapshot>> {
    let mut shared = SHARED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(cached) = shared.as_ref() {
        if !max_age.is_zero() && cached.taken_at.elapsed() <= max_age {
            return Some(Arc::clone(cached));
        }
    }
    let fresh = Arc::new(Snapshot::from_entries(imp::list()?));
    *shared = Some(Arc::clone(&fresh));
    Some(fresh)
}

/// 한 pid의 계량값(누적 카운터).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PidUsage {
    pub start_token: String,
    pub start_epoch_ms: u64,
    pub ppid: u32,
    /// 자기 CPU 시간(user+system, ms).
    pub cpu_ms: u64,
    /// 회수된(wait된) 자식들의 CPU 시간 합(ms). 짧게 살다 간 컴파일러·테스트
    /// 프로세스의 CPU가 여기로 들어온다.
    pub child_cpu_ms: u64,
    /// 물리 footprint(macOS)·익명+공유+스왑(Linux). 없으면 `None`.
    pub footprint_bytes: Option<u64>,
    pub resident_bytes: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

/// 한 pid의 계량값. 사라졌거나 읽을 수 없으면 `None`.
pub fn pid_usage(pid: u32) -> Option<PidUsage> {
    imp::usage(pid)
}

/// 실행 파일 경로.
pub fn exe_path(pid: u32) -> Option<String> {
    imp::exe(pid)
}

/// argv(env는 읽어도 버린다).
pub fn argv(pid: u32) -> Option<Vec<String>> {
    imp::args_and_env(pid).map(|(argv, _)| argv)
}

/// 환경 변수 하나의 값. 프로세스가 없거나 읽을 수 없거나 변수가 없으면 `None`.
pub fn env_var(pid: u32, key: &str) -> Option<String> {
    let (_, env) = imp::args_and_env(pid)?;
    let prefix = format!("{key}=");
    env.into_iter()
        .find_map(|pair| pair.strip_prefix(&prefix).map(str::to_string))
}

/// 이 플랫폼에 네이티브 경로가 있는가.
pub fn supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// `KERN_PROCARGS2` 버퍼를 (argv, env)로 푼다:
/// `[argc: i32][exec_path\0][\0 padding…][argv0\0]…[argv(n-1)\0][env0\0]…`.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buf: &[u8]) -> Option<(Vec<String>, Vec<String>)> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let mut rest = &buf[4..];
    // exec_path
    let end = rest.iter().position(|b| *b == 0)?;
    rest = &rest[end..];
    // 정렬용 NUL 채움
    let start = rest.iter().position(|b| *b != 0).unwrap_or(rest.len());
    rest = &rest[start..];
    let mut strings = rest.split(|b| *b == 0);
    let argv: Vec<String> = strings
        .by_ref()
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let env: Vec<String> = strings
        .take_while(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Some((argv, env))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::mem;
    use std::sync::OnceLock;

    use super::{PidUsage, ProcEntry};

    fn bsd_info(pid: u32) -> Option<libc::proc_bsdinfo> {
        let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
        let size = mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // 안전성: 크기를 함께 넘기는 표준 libproc 호출이다.
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (read == size && info.pbi_status != libc::SZOMB).then_some(info)
    }

    fn c_name(raw: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn start_of(info: &libc::proc_bsdinfo) -> (String, u64) {
        let micros = (info.pbi_start_tvsec as i128) * 1_000_000 + info.pbi_start_tvusec as i128;
        (
            micros.to_string(),
            u64::try_from(micros / 1_000).unwrap_or(0),
        )
    }

    pub fn list() -> Option<Vec<ProcEntry>> {
        // 안전성: NULL 버퍼로 개수를 묻고, 여유를 둔 버퍼에 다시 받는다.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return None;
        }
        let mut pids = vec![0 as libc::c_int; count as usize + 256];
        let bytes = (pids.len() * mem::size_of::<libc::c_int>()) as libc::c_int;
        let got = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if got <= 0 {
            return None;
        }
        pids.truncate(got as usize);
        Some(
            pids.into_iter()
                .filter(|pid| *pid > 0)
                .filter_map(|pid| {
                    let pid = pid as u32;
                    let info = bsd_info(pid)?;
                    let (start_token, start_epoch_ms) = start_of(&info);
                    let name = {
                        let long = c_name(&info.pbi_name);
                        if long.is_empty() {
                            c_name(&info.pbi_comm)
                        } else {
                            long
                        }
                    };
                    Some(ProcEntry {
                        pid,
                        ppid: info.pbi_ppid,
                        start_token,
                        start_epoch_ms,
                        uid: info.pbi_uid,
                        name,
                    })
                })
                .collect(),
        )
    }

    /// `mach_timebase_info_data_t`. libc의 바인딩은 폐기 예정(mach2 권장)이라
    /// libSystem의 같은 함수를 직접 선언한다.
    #[repr(C)]
    struct TimebaseInfo {
        numer: u32,
        denom: u32,
    }

    extern "C" {
        fn mach_timebase_info(info: *mut TimebaseInfo) -> libc::c_int;
    }

    /// mach 절대 시간 단위 → ns 배율(Apple Silicon 125/3, Intel 1/1).
    fn timebase() -> (u64, u64) {
        static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
        *TIMEBASE.get_or_init(|| {
            let mut info = TimebaseInfo { numer: 0, denom: 0 };
            // 안전성: 출력 구조체 하나를 채우는 호출이다.
            let ok = unsafe { mach_timebase_info(&mut info) } == 0;
            if ok && info.numer > 0 && info.denom > 0 {
                (u64::from(info.numer), u64::from(info.denom))
            } else {
                (1, 1)
            }
        })
    }

    fn mach_to_ms(units: u64) -> u64 {
        let (numer, denom) = timebase();
        let nanos = u128::from(units) * u128::from(numer) / u128::from(denom);
        u64::try_from(nanos / 1_000_000).unwrap_or(u64::MAX)
    }

    pub fn usage(pid: u32) -> Option<PidUsage> {
        let info = bsd_info(pid)?;
        let mut rusage: libc::rusage_info_v4 = unsafe { mem::zeroed() };
        // 안전성: V4 구조체 크기의 버퍼를 넘기는 표준 호출이다.
        let ok = unsafe {
            libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&mut rusage as *mut libc::rusage_info_v4).cast(),
            )
        } == 0;
        if !ok {
            return None;
        }
        let (start_token, start_epoch_ms) = start_of(&info);
        Some(PidUsage {
            start_token,
            start_epoch_ms,
            ppid: info.pbi_ppid,
            cpu_ms: mach_to_ms(rusage.ri_user_time.saturating_add(rusage.ri_system_time)),
            child_cpu_ms: mach_to_ms(
                rusage
                    .ri_child_user_time
                    .saturating_add(rusage.ri_child_system_time),
            ),
            footprint_bytes: Some(rusage.ri_phys_footprint),
            resident_bytes: rusage.ri_resident_size,
            read_bytes: rusage.ri_diskio_bytesread,
            written_bytes: rusage.ri_diskio_byteswritten,
        })
    }

    pub fn exe(pid: u32) -> Option<String> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // 안전성: 버퍼 크기를 함께 넘긴다.
        let len = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
            )
        };
        (len > 0).then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
    }

    pub fn args_and_env(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size: libc::size_t = 0;
        // 안전성: 크기 조회 → 그 크기의 버퍼로 다시 읽는 표준 sysctl 순서.
        let probe = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if probe != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        let read = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        buf.truncate(size);
        super::parse_procargs2(&buf)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::fs::MetadataExt;
    use std::sync::OnceLock;

    use super::{PidUsage, ProcEntry};

    fn clock_ticks() -> u64 {
        static TICKS: OnceLock<u64> = OnceLock::new();
        *TICKS.get_or_init(|| {
            // 안전성: 인자 없는 sysconf 질의다.
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
        })
    }

    /// 부팅 시각(epoch 초) — `/proc/stat`의 `btime`.
    fn boot_epoch_secs() -> u64 {
        static BOOT: OnceLock<u64> = OnceLock::new();
        *BOOT.get_or_init(|| {
            std::fs::read_to_string("/proc/stat")
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find_map(|line| line.strip_prefix("btime "))
                        .and_then(|value| value.trim().parse().ok())
                })
                .unwrap_or(0)
        })
    }

    /// `stat` 한 줄 → (comm, 나머지 필드). comm에는 공백·괄호가 들어갈 수
    /// 있어 마지막 `)` 뒤부터 센다. 좀비·죽은 항목은 `None`.
    fn split_stat(stat: &str) -> Option<(String, Vec<&str>)> {
        let open = stat.find('(')?;
        let (comm, after) = stat[open + 1..].rsplit_once(')')?;
        let fields: Vec<&str> = after.split_whitespace().collect();
        if matches!(fields.first().copied(), Some("Z" | "X" | "x")) {
            return None;
        }
        Some((comm.to_string(), fields))
    }

    /// `fields[0]`은 state(3번 필드)다 — n번 필드는 `fields[n - 3]`.
    fn field(fields: &[&str], n: usize) -> Option<u64> {
        fields.get(n - 3)?.parse().ok()
    }

    fn start_of(fields: &[&str]) -> Option<(String, u64)> {
        let ticks = field(fields, 22)?;
        let epoch_ms = boot_epoch_secs()
            .saturating_mul(1_000)
            .saturating_add(ticks.saturating_mul(1_000) / clock_ticks());
        Some((ticks.to_string(), epoch_ms))
    }

    pub fn list() -> Option<Vec<ProcEntry>> {
        let dir = std::fs::read_dir("/proc").ok()?;
        Some(
            dir.filter_map(Result::ok)
                .filter_map(|entry| {
                    let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                    let (name, fields) = split_stat(&stat)?;
                    let (start_token, start_epoch_ms) = start_of(&fields)?;
                    let uid = entry.metadata().map(|m| m.uid()).unwrap_or(u32::MAX);
                    Some(ProcEntry {
                        pid,
                        ppid: field(&fields, 4)? as u32,
                        start_token,
                        start_epoch_ms,
                        uid,
                        name,
                    })
                })
                .collect(),
        )
    }

    fn status_kib(status: &str, key: &str) -> Option<u64> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
    }

    fn io_bytes(io: &str, key: &str) -> u64 {
        io.lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn usage(pid: u32) -> Option<PidUsage> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, fields) = split_stat(&stat)?;
        let (start_token, start_epoch_ms) = start_of(&fields)?;
        let ticks_to_ms = |ticks: u64| ticks.saturating_mul(1_000) / clock_ticks();
        let cpu = field(&fields, 14)?.saturating_add(field(&fields, 15)?);
        let child = field(&fields, 16)?.saturating_add(field(&fields, 17)?);
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let anon = status_kib(&status, "RssAnon:");
        let shmem = status_kib(&status, "RssShmem:");
        let swap = status_kib(&status, "VmSwap:");
        let footprint =
            anon.map(|anon| (anon + shmem.unwrap_or(0) + swap.unwrap_or(0)).saturating_mul(1024));
        let resident = status_kib(&status, "VmRSS:")
            .unwrap_or(0)
            .saturating_mul(1024);
        // `io`는 다른 사용자의 프로세스면 읽을 수 없다 — 0으로 둔다.
        let io = std::fs::read_to_string(format!("/proc/{pid}/io")).unwrap_or_default();
        Some(PidUsage {
            start_token,
            start_epoch_ms,
            ppid: field(&fields, 4)? as u32,
            cpu_ms: ticks_to_ms(cpu),
            child_cpu_ms: ticks_to_ms(child),
            footprint_bytes: footprint,
            resident_bytes: resident,
            read_bytes: io_bytes(&io, "read_bytes:"),
            written_bytes: io_bytes(&io, "write_bytes:"),
        })
    }

    pub fn exe(pid: u32) -> Option<String> {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn nul_split(bytes: &[u8]) -> Vec<String> {
        bytes
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    pub fn args_and_env(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
        let argv = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        Some((nul_split(&argv), nul_split(&env)))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::{PidUsage, ProcEntry};

    pub fn list() -> Option<Vec<ProcEntry>> {
        None
    }

    pub fn usage(_pid: u32) -> Option<PidUsage> {
        None
    }

    pub fn exe(_pid: u32) -> Option<String> {
        None
    }

    pub fn args_and_env(_pid: u32) -> Option<(Vec<String>, Vec<String>)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, ppid: u32) -> ProcEntry {
        ProcEntry {
            pid,
            ppid,
            start_token: format!("t{pid}"),
            start_epoch_ms: 0,
            uid: 501,
            name: format!("p{pid}"),
        }
    }

    #[test]
    fn descendants_are_breadth_first_and_cycle_safe() {
        let snap = Snapshot::from_entries(vec![
            entry(1, 0),
            entry(10, 1),
            entry(11, 10),
            entry(12, 10),
            entry(13, 11),
            // ppid 순환(재사용·경주): 무한 루프가 되면 안 된다.
            entry(20, 21),
            entry(21, 20),
        ]);
        assert_eq!(snap.descendants(10), vec![10, 11, 12, 13]);
        assert_eq!(snap.descendants(20), vec![20, 21]);
        assert!(snap.descendants(99).is_empty(), "없는 루트는 빈 트리");
        let id = snap.identity(11).unwrap();
        assert!(snap.is_alive(&id));
        let reused = ProcessIdentity {
            start_token: "other".into(),
            ..id
        };
        assert!(!snap.is_alive(&reused));
    }

    #[test]
    fn procargs2_parses_argv_and_env() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/bin/claude\0\0\0\0");
        buf.extend_from_slice(b"claude\0--resume\0");
        buf.extend_from_slice(b"HOME=/Users/x\0IYAGI_WORKLOAD_ID=abc\0\0garbage");
        let (argv, env) = parse_procargs2(&buf).expect("parses");
        assert_eq!(argv, vec!["claude", "--resume"]);
        assert_eq!(env, vec!["HOME=/Users/x", "IYAGI_WORKLOAD_ID=abc"]);
        assert!(parse_procargs2(&[1, 0]).is_none(), "짧은 버퍼는 모름");
    }

    /// 아래 시험이 띄우는 도우미: 표식 env가 있으면 잠시 살아 있는다. 시스템
    /// 바이너리(`/bin/sleep` 등)는 macOS가 argv·env를 숨기므로 테스트 바이너리
    /// 자신을 자식으로 쓴다.
    #[test]
    #[ignore = "helper process for live_snapshot_and_usage_read_this_process"]
    fn sleeper_helper() {
        if std::env::var_os("IYAGI_TEST_MARKER").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    /// 실제 커널을 읽는다: 이 테스트 프로세스가 목록에 있고, 자기 자식이
    /// 트리로 보이고, 계량·argv·env가 읽힌다.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn live_snapshot_and_usage_read_this_process() {
        let me = std::process::id();
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "native_proc::tests::sleeper_helper",
                "--exact",
                "--ignored",
                "--test-threads=1",
            ])
            .env("IYAGI_TEST_MARKER", "native-proc")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn helper");
        let snap = snapshot(Duration::ZERO).expect("supported platform");
        assert!(snap.len() > 1);
        let mine = snap.get(me).expect("self listed");
        assert_eq!(
            mine.start_token,
            crate::identity::process_identity(me).unwrap().start_token,
            "신원 토큰 형식이 identity와 같다"
        );
        assert!(snap.descendants(me).contains(&child.id()));
        let usage = pid_usage(me).expect("self usage");
        assert!(usage.footprint_bytes.unwrap_or(usage.resident_bytes) > 0);
        assert_eq!(usage.start_token, mine.start_token);
        // fork 직후 exec 전에는 부모의 인자·환경이 보인다 — exec를 기다린다.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !argv(child.id())
            .unwrap_or_default()
            .iter()
            .any(|arg| arg == "native_proc::tests::sleeper_helper")
        {
            assert!(Instant::now() < deadline, "child never exec'd");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            env_var(child.id(), "IYAGI_TEST_MARKER").as_deref(),
            Some("native-proc")
        );
        // 공유 캐시: 최대 나이 안에서는 같은 스냅샷.
        let a = snapshot(Duration::from_secs(60)).unwrap();
        let b = snapshot(Duration::from_secs(60)).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let _ = child.kill();
        let _ = child.wait();
    }
}
