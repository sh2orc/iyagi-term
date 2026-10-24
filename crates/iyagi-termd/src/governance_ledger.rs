//! 크래시 안전 장부 — 자원 가드·완화가 OS에 걸어 둔 조작(SIGSTOP·cgroup
//! freeze·background 등급)을 디스크에 남긴다.
//!
//! 정지·양보 상태는 컨트롤러 메모리에만 있다. 정상 종료는 재개 경로를 돌지만,
//! 데몬이 kill -9·jetsam·패닉으로 죽으면 그 경로가 없다:
//!
//! * SIGSTOP된 트리는 tty hangup으로도 깨어나지 않는다 — 정지된 프로세스는
//!   SIGHUP을 처리하지 못하고, 셸이 끝나지 않으니 고아 프로세스 그룹의
//!   SIGHUP+SIGCONT도 오지 않는다. RAM을 쥔 채 T 상태로 영원히 남는다.
//! * 위임 cgroup의 `cgroup.freeze=1`도 그대로 남는다.
//! * 양보된 트리는 background 등급(효율 코어 고정·디스크 throttle)이 수명
//!   내내 남는다.
//!
//! 그래서 조작이 성공할 때마다 그 프로세스 신원을 이 장부에 적고, 데몬 기동 때
//! [`recover`]가 아직 같은 프로세스인 것만(신원 검증 — pid 재사용 방어) 되돌린
//! 뒤 장부를 비운다. 쓰기는 조작이 성공할 때뿐이라 드물다(temp+rename 원자적).
//! 장부가 비면 파일을 지운다.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use term_contracts::ids::{ProcessIdentity, WorkloadId};
use term_platform::group::{scheduling, SchedulingTier};

const FORMAT_VERSION: u32 = 1;

/// 한 워크로드에 걸린 조작.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// SIGSTOP이 걸린 신원(부모 먼저 순서로 기록된다).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    suspended: Vec<ProcessIdentity>,
    /// 위임 cgroup Linux의 freeze 경로(그룹 정지는 SIGSTOP이 아니다).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frozen_cgroup: Option<String>,
    /// background 등급이 걸린 신원.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    yielded: Vec<ProcessIdentity>,
    /// 위임 cgroup Linux의 양보 경로(`cpu.weight`를 낮춘 그룹).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    weighted_cgroup: Option<String>,
}

impl Entry {
    fn is_empty(&self) -> bool {
        self.suspended.is_empty()
            && self.frozen_cgroup.is_none()
            && self.yielded.is_empty()
            && self.weighted_cgroup.is_none()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

/// 데몬이 하나만 소유하는 장부.
pub struct GovernanceLedger {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, Entry>>,
}

/// 기동 복구의 결과(로그용).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    /// SIGCONT를 보낸 프로세스 수.
    pub resumed: usize,
    /// 원래 등급으로 되돌린 프로세스 수.
    pub restored: usize,
    /// freeze를 풀거나 `cpu.weight`를 되돌린 cgroup 수.
    pub thawed_cgroups: usize,
    /// 이미 없거나 다른 프로세스가 된 신원(건드리지 않았다).
    pub skipped: usize,
}

impl GovernanceLedger {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// 정지가 걸렸다(부분 적용 포함 — 걸린 것이 하나라도 있으면 남긴다).
    pub fn note_suspended(
        &self,
        workload_id: &WorkloadId,
        identities: Vec<ProcessIdentity>,
        frozen_cgroup: Option<String>,
    ) {
        self.update(workload_id, |entry| {
            merge_identities(&mut entry.suspended, identities);
            if frozen_cgroup.is_some() {
                entry.frozen_cgroup = frozen_cgroup;
            }
        });
    }

    /// 정지가 풀렸다.
    pub fn note_resumed(&self, workload_id: &WorkloadId) {
        self.update(workload_id, |entry| {
            entry.suspended.clear();
            entry.frozen_cgroup = None;
        });
    }

    /// background 등급이 걸렸다.
    pub fn note_yielded(
        &self,
        workload_id: &WorkloadId,
        identities: Vec<ProcessIdentity>,
        weighted_cgroup: Option<String>,
    ) {
        self.update(workload_id, |entry| {
            merge_identities(&mut entry.yielded, identities);
            if weighted_cgroup.is_some() {
                entry.weighted_cgroup = weighted_cgroup;
            }
        });
    }

    /// 원래 등급으로 돌아왔다.
    pub fn note_restored(&self, workload_id: &WorkloadId) {
        self.update(workload_id, |entry| {
            entry.yielded.clear();
            entry.weighted_cgroup = None;
        });
    }

    /// 끝난 워크로드의 항목을 버린다(그 프로세스들은 이미 없다).
    pub fn retain(&self, live: &HashSet<&str>) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let before = entries.len();
        entries.retain(|id, _| live.contains(id.as_str()));
        if entries.len() != before {
            persist(&self.path, &entries);
        }
    }

    /// 정상 종료: 재개를 마친 뒤 장부를 비운다.
    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.clear();
        persist(&self.path, &entries);
    }

    fn update(&self, workload_id: &WorkloadId, change: impl FnOnce(&mut Entry)) {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let before = entries.get(workload_id.as_str()).cloned();
        let entry = entries.entry(workload_id.as_str().to_string()).or_default();
        change(entry);
        if entry.is_empty() {
            entries.remove(workload_id.as_str());
        }
        if entries.get(workload_id.as_str()) != before.as_ref() {
            persist(&self.path, &entries);
        }
    }
}

fn merge_identities(into: &mut Vec<ProcessIdentity>, identities: Vec<ProcessIdentity>) {
    for identity in identities {
        if !into.iter().any(|known| known.same_process(&identity)) {
            into.push(identity);
        }
    }
}

/// 장부를 원자적으로 쓴다(비었으면 파일을 지운다). 실패는 로그만 — 장부는
/// 크래시 대비 보험이고, 쓰지 못했다고 조작을 되돌릴 이유는 없다.
fn persist(path: &Path, entries: &BTreeMap<String, Entry>) {
    if entries.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, "governance ledger could not be removed"),
        }
        return;
    }
    let file = LedgerFile {
        version: FORMAT_VERSION,
        entries: entries.clone(),
    };
    let Ok(bytes) = serde_json::to_vec(&file) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    let result = std::fs::write(&temp, &bytes).and_then(|()| std::fs::rename(&temp, path));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        tracing::warn!(%error, "governance ledger could not be written");
    }
}

/// 데몬 기동 때 한 번(싱글턴 락을 잡은 뒤, 텔레메트리가 돌기 전): 지난
/// 데몬이 걸어 둔 조작 중 아직 같은 프로세스인 것을 되돌리고 장부를 지운다.
pub fn recover(path: &Path) -> RecoveryReport {
    let mut report = RecoveryReport::default();
    let Ok(bytes) = std::fs::read(path) else {
        return report;
    };
    let file: LedgerFile = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, "governance ledger unreadable; discarding it");
            let _ = std::fs::remove_file(path);
            return report;
        }
    };
    for (workload_id, entry) in file.entries {
        if let Some(cgroup) = &entry.frozen_cgroup {
            if thaw_cgroup(cgroup) {
                report.thawed_cgroups += 1;
            }
        }
        if let Some(cgroup) = &entry.weighted_cgroup {
            if restore_cgroup_weight(cgroup) {
                report.thawed_cgroups += 1;
            }
        }
        // 깨우는 순서(자식 먼저)는 플랫폼이 지금의 트리 깊이로 정한다.
        if !entry.suspended.is_empty() {
            match scheduling::set_process_suspend(&entry.suspended, false) {
                Ok(outcome) => {
                    report.resumed += outcome.applied;
                    report.skipped += outcome.skipped_reused + outcome.skipped_denied;
                }
                Err(error) => {
                    tracing::warn!(%workload_id, %error, "ledger recovery: resume failed")
                }
            }
        }
        if !entry.yielded.is_empty() {
            match scheduling::set_process_scheduling(&entry.yielded, SchedulingTier::Normal) {
                Ok(outcome) => {
                    report.restored += outcome.applied;
                    report.skipped += outcome.skipped_reused + outcome.skipped_denied;
                }
                Err(error) => {
                    tracing::warn!(%workload_id, %error, "ledger recovery: restore failed")
                }
            }
        }
    }
    let _ = std::fs::remove_file(path);
    report
}

/// 위임 cgroup의 freeze를 푼다(Linux). 경로가 없으면(이미 정리됨) 거짓.
#[cfg(target_os = "linux")]
fn thaw_cgroup(path: &str) -> bool {
    let freeze = Path::new(path).join("cgroup.freeze");
    freeze.exists() && std::fs::write(&freeze, b"0").is_ok()
}

#[cfg(not(target_os = "linux"))]
fn thaw_cgroup(_path: &str) -> bool {
    false
}

/// 위임 cgroup의 `cpu.weight`를 기본값(100)으로 되돌린다(Linux).
#[cfg(target_os = "linux")]
fn restore_cgroup_weight(path: &str) -> bool {
    let weight = Path::new(path).join("cpu.weight");
    weight.exists() && std::fs::write(&weight, b"100").is_ok()
}

#[cfg(not(target_os = "linux"))]
fn restore_cgroup_weight(_path: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_token: format!("start-{pid}"),
            boot_id: "boot".to_string(),
        }
    }

    fn read(path: &Path) -> Option<LedgerFile> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    #[test]
    fn ledger_tracks_operations_and_removes_the_file_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance-ledger.json");
        let ledger = GovernanceLedger::new(path.clone());
        let a = WorkloadId::generate();
        let b = WorkloadId::generate();
        ledger.note_suspended(&a, vec![identity(10), identity(11)], None);
        ledger.note_yielded(&b, vec![identity(20)], None);
        let file = read(&path).expect("written");
        assert_eq!(file.version, FORMAT_VERSION);
        assert_eq!(file.entries[a.as_str()].suspended.len(), 2);
        assert_eq!(file.entries[b.as_str()].yielded.len(), 1);
        // 같은 신원을 다시 적어도 중복되지 않는다(부분 재시도).
        ledger.note_suspended(&a, vec![identity(11), identity(12)], None);
        assert_eq!(read(&path).unwrap().entries[a.as_str()].suspended.len(), 3);
        ledger.note_resumed(&a);
        assert!(!read(&path).unwrap().entries.contains_key(a.as_str()));
        ledger.note_restored(&b);
        assert!(!path.exists(), "비면 파일을 지운다");
    }

    #[test]
    fn retain_drops_finished_workloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let ledger = GovernanceLedger::new(path.clone());
        let a = WorkloadId::generate();
        let b = WorkloadId::generate();
        ledger.note_yielded(&a, vec![identity(1)], None);
        ledger.note_yielded(&b, vec![identity(2)], None);
        let live: HashSet<&str> = [a.as_str()].into_iter().collect();
        ledger.retain(&live);
        let file = read(&path).unwrap();
        assert!(file.entries.contains_key(a.as_str()));
        assert!(!file.entries.contains_key(b.as_str()));
        ledger.clear();
        assert!(!path.exists());
    }

    /// 지난 데몬의 장부: 이미 없는(또는 다른 프로세스가 된) 신원은 건드리지
    /// 않고 세기만 하고, 장부는 지운다. 망가진 장부도 지운다.
    #[test]
    fn recover_skips_stale_identities_and_removes_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let ledger = GovernanceLedger::new(path.clone());
        let stale = ProcessIdentity {
            pid: u32::MAX - 7,
            start_token: "gone".to_string(),
            boot_id: "other-boot".to_string(),
        };
        ledger.note_suspended(&WorkloadId::generate(), vec![stale.clone()], None);
        ledger.note_yielded(&WorkloadId::generate(), vec![stale], None);
        let report = recover(&path);
        assert_eq!(report.resumed, 0);
        assert_eq!(report.restored, 0);
        assert!(!path.exists());

        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(recover(&path), RecoveryReport::default());
        assert!(!path.exists());
        // 장부가 없으면 아무것도 하지 않는다.
        assert_eq!(recover(&path), RecoveryReport::default());
    }

    /// 실제로 SIGSTOP된 자식 프로세스를 기동 복구가 깨운다(Unix).
    #[cfg(unix)]
    #[test]
    fn recover_resumes_a_really_stopped_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let identity = term_platform::identity::process_identity(pid).expect("identity");
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        GovernanceLedger::new(path.clone()).note_suspended(
            &WorkloadId::generate(),
            vec![identity],
            None,
        );
        let report = recover(&path);
        assert_eq!(report.resumed, 1);
        assert!(!path.exists());
        let _ = child.kill();
        let _ = child.wait();
    }
}
