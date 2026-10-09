//! Spawning (and shutting down) a real `iyagi-termd` on a hermetic temp
//! data dir, mirroring the integration-test harness but standalone.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::wire::Conn;

/// One daemon process bound to its own data dir.
pub struct DaemonProc {
    pub child: Child,
    pub data_dir: PathBuf,
    pub token: String,
    pub endpoint: String,
    pub pid: u32,
    pub stderr_path: PathBuf,
}

impl DaemonProc {
    /// Spawn and wait until the runtime token/endpoint files exist.
    /// `config_overrides` becomes a `IYAGI_TEST_CONFIG` JSON file.
    pub fn spawn(
        daemon_bin: &Path,
        tag: &str,
        config_overrides: Option<Value>,
    ) -> Result<DaemonProc, String> {
        // macOS의 Unix-domain socket SUN_LEN(~104) 한계: $TMPDIR(/var/folders/…)
        // 는 경로가 길어 소켓 바인딩이 실패한다. 항상 /tmp 아래의 짧은
        // 고정 루트를 쓰고, 태그+짧은 uuid만 붙인다(Windows 파이프는 경로
        // 무관 — 동작 동일).
        let temp_root = if cfg!(unix) {
            std::path::PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let data_dir = temp_root.join(format!(
            "iyagi-bench-{tag}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir_all(&data_dir).map_err(|e| format!("data dir: {e}"))?;

        let stderr_path = data_dir.join("daemon-stderr.log");
        let stderr_file =
            std::fs::File::create(&stderr_path).map_err(|e| format!("stderr file: {e}"))?;
        let mut cmd = Command::new(daemon_bin);
        cmd.arg("--data-dir")
            .arg(&data_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file))
            .env("RUST_LOG", "iyagi_termd_lib=info,iyagi_termd=info");
        if let Some(overrides) = config_overrides {
            let cfg_path = data_dir.join("bench-config.json");
            std::fs::write(&cfg_path, overrides.to_string())
                .map_err(|e| format!("write override config: {e}"))?;
            cmd.env("IYAGI_TEST_CONFIG", &cfg_path);
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("spawn daemon {:?}: {e}", daemon_bin))?;
        let pid = child.id();
        let mut proc = DaemonProc {
            child,
            data_dir,
            token: String::new(),
            endpoint: String::new(),
            pid,
            stderr_path,
        };
        proc.wait_for_ready()?;
        Ok(proc)
    }

    fn wait_for_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let token_path = self.data_dir.join("runtime/token");
        let endpoint_path = self.data_dir.join("runtime/endpoint");
        while Instant::now() < deadline {
            if let Ok(status) = self.child.try_wait() {
                if status.is_some() {
                    return Err(format!(
                        "daemon exited during startup. stderr tail:\n{}",
                        self.stderr_tail()
                    ));
                }
            }
            if let (Ok(token), Ok(endpoint)) = (
                std::fs::read_to_string(&token_path),
                std::fs::read_to_string(&endpoint_path),
            ) {
                let token = token.trim().to_string();
                let endpoint = endpoint.trim().to_string();
                if !token.is_empty() && !endpoint.is_empty() {
                    self.token = token;
                    self.endpoint = endpoint;
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "daemon not ready within 30s at {:?}. stderr tail:\n{}",
            self.data_dir,
            self.stderr_tail()
        ))
    }

    /// Last `max_bytes` of daemon stderr (diagnostics on failure).
    pub fn stderr_tail(&self) -> String {
        const MAX: u64 = 4096;
        let Ok(meta) = std::fs::metadata(&self.stderr_path) else {
            return String::new();
        };
        let start = meta.len().saturating_sub(MAX);
        let Ok(mut file) = std::fs::File::open(&self.stderr_path) else {
            return String::new();
        };
        use std::io::Seek;
        if file.seek(std::io::SeekFrom::Start(start)).is_err() {
            return String::new();
        }
        let mut buf = String::new();
        use std::io::Read;
        let _ = file.read_to_string(&mut buf);
        buf
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Graceful shutdown through the daemon's own RPC (falls back to kill).
/// Never fails hard: the return notes whether the exit code was clean.
pub fn shutdown(daemon: &mut DaemonProc, control: Option<&mut Conn>) -> Result<(), String> {
    if let Some(conn) = control {
        let _ = conn.request(
            "daemon.shutdown",
            serde_json::json!({"request_id": crate::wire::uuid_v4(), "stop_workloads": true}),
        );
        // The daemon replies first, then exits; wait up to 15 s.
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match daemon.child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(format!("daemon exited {status}"))
                    };
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(format!("wait daemon: {e}")),
            }
        }
    }
    daemon.kill();
    Err("daemon shutdown timed out; killed".into())
}

/// Remove the hermetic data dir (best-effort).
pub fn cleanup_data_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}
