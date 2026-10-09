//! 프로세스 트리 열거(에이전트 감지 지원): 루트 PID들의 자손을 한 번의
//! sysinfo 패스로 모은다. 자원 계량(`telemetry`)과 달리 이름/argv만 필요하므로
//! 갱신 종류를 cmd/exe로 제한해 틱 비용을 줄인다. 소유권 판정이나 종료
//! 신호에는 쓰지 않는다 — 그것은 `group` 백엔드의 책임이다.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// 열거된 프로세스 한 개(감지 매칭에 필요한 필드만).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessBrief {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub exe: Option<String>,
    pub cmd: Vec<String>,
}

fn descendant_pids(root: Pid, children: &BTreeMap<Pid, Vec<Pid>>) -> Vec<Pid> {
    // Observed PPIDs may cycle after PID reuse or a refresh race.
    // Mark on enqueue: each process can consume memory at most once.
    let mut seen = HashSet::from([root]);
    let mut out = vec![root];
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for &kid in kids {
                if seen.insert(kid) {
                    queue.push_back(kid);
                    out.push(kid);
                }
            }
        }
    }
    out
}

/// 데몬 전체에서 하나뿐인 열거용 sysinfo 인스턴스(모듈 소유). 증분 갱신으로
/// 재사용한다.
static SYSTEM: Mutex<Option<System>> = Mutex::new(None);

/// 각 루트 PID에 대해 (루트 포함) 관찰된 자손 트리를 돌려준다.
/// 루트가 이미 없으면 그 항목은 빈 벡트다. 한 번의 프로세스 갱신으로
/// 모든 루트를 처리한다.
pub fn scan_process_trees(root_pids: &[u32]) -> HashMap<u32, Vec<ProcessBrief>> {
    let mut out: HashMap<u32, Vec<ProcessBrief>> = HashMap::with_capacity(root_pids.len());
    if root_pids.is_empty() {
        return out;
    }

    let mut guard = SYSTEM.lock().unwrap_or_else(|p| p.into_inner());
    let system = guard.get_or_insert_with(System::new);
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );

    // PPID → children. BTreeMap은 자식 열거 순서를 pid 정렬로 고정해
    // 감지 결과가 틱마다 흔들리지 않게 한다.
    let mut children: BTreeMap<Pid, Vec<Pid>> = BTreeMap::new();
    let mut by_pid: HashMap<Pid, &sysinfo::Process> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
        by_pid.insert(*pid, process);
    }

    let brief = |pid: Pid| -> ProcessBrief {
        let process = by_pid.get(&pid);
        ProcessBrief {
            pid: pid.as_u32(),
            ppid: process
                .and_then(|p| p.parent())
                .map(|p| p.as_u32())
                .unwrap_or(0),
            name: process
                .map(|p| p.name().to_string_lossy().into_owned())
                .unwrap_or_default(),
            exe: process
                .and_then(|p| p.exe())
                .map(|e| e.to_string_lossy().into_owned()),
            cmd: process
                .map(|p| {
                    p.cmd()
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default(),
        }
    };

    for &root in root_pids {
        let root_pid = Pid::from_u32(root);
        if !by_pid.contains_key(&root_pid) {
            out.insert(root, Vec::new());
            continue;
        }
        let tree = descendant_pids(root_pid, &children)
            .into_iter()
            .map(brief)
            .collect();
        out.insert(root, tree);
    }
    out
}

/// 한 pid의 시작 시각(Unix epoch 초). 없거나 읽을 수 없으면 `None`.
///
/// 에이전트 세션 식별의 잠금 파일 대조에 쓴다(spec `02-runner.md` §8):
/// fd 열거가 막힌 환경에서 "프로세스가 시작될 즈음 만들어진 잠금 파일"을
/// 찾는 기준이다. 소유권 판정에는 쓰지 않는다 — 그것은 `identity`의
/// `start_token`(마이크로초 해상도)이 맡는다.
pub fn process_start_time_secs(pid: u32) -> Option<u64> {
    with_refreshed(
        pid,
        ProcessRefreshKind::nothing(),
        |process| match process.start_time() {
            0 => None,
            secs => Some(secs),
        },
    )
}

/// 한 pid의 현재 작업 디렉터리. 권한이 없거나 프로세스가 사라졌으면 `None`.
pub fn process_cwd(pid: u32) -> Option<String> {
    with_refreshed(
        pid,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
        |process| {
            process
                .cwd()
                .map(|path| path.to_string_lossy().into_owned())
                .filter(|text| !text.is_empty())
        },
    )
}

/// 조상 pid 사슬(가까운 부모부터). pid 0(커널)에서 멈추고 pid 1(init)까지는
/// 포함한다. 순환(관찰 경합으로 생길 수 있다)과 [`ANCESTOR_MAX`] 상한에서도
/// 멈춘다.
///
/// hook 프로세스가 어느 pane에서 났는지 데몬이 대조할 때 쓴다 — 환경 변수가
/// 없는(사용자가 CLI를 직접 실행한) 경우의 마지막 근거다.
pub fn ancestor_pids(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::from([pid]);
    let mut guard = SYSTEM.lock().unwrap_or_else(|p| p.into_inner());
    let system = guard.get_or_insert_with(System::new);

    let mut current = pid;
    while out.len() < ANCESTOR_MAX {
        let target = Pid::from_u32(current);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[target]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let Some(parent) = system.process(target).and_then(|p| p.parent()) else {
            break;
        };
        let parent = parent.as_u32();
        // pid 0은 조상으로 의미가 없고, 이미 본 pid는 순환이다.
        if parent == 0 || !seen.insert(parent) {
            break;
        }
        out.push(parent);
        if parent == 1 {
            break;
        }
        current = parent;
    }
    out
}

/// 조상 사슬 상한. 계약의 `ancestor_pids` 상한(64)과 같은 값이다.
const ANCESTOR_MAX: usize = 64;

/// 딱 이 pid만 갱신하고 프로세스 항목을 읽는다. 없어진 프로세스는 갱신에서
/// 제거되므로 오래된 값을 돌려주지 않는다.
fn with_refreshed<T>(
    pid: u32,
    kind: ProcessRefreshKind,
    read: impl FnOnce(&sysinfo::Process) -> Option<T>,
) -> Option<T> {
    let target = Pid::from_u32(pid);
    let mut guard = SYSTEM.lock().unwrap_or_else(|p| p.into_inner());
    let system = guard.get_or_insert_with(System::new);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[target]), true, kind);
    read(system.process(target)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::Command;

    #[test]
    fn cyclic_process_inventory_is_bounded_and_keeps_other_descendants() {
        let p = Pid::from_u32;
        let children = BTreeMap::from([
            (p(1), vec![p(1), p(2), p(3)]),
            (p(2), vec![p(1), p(3)]),
            (p(3), vec![p(4)]),
            (p(4), vec![p(2)]),
        ]);
        assert_eq!(
            descendant_pids(p(1), &children),
            vec![p(1), p(2), p(3), p(4)]
        );
    }

    #[test]
    fn single_process_tree_contains_root() {
        // Use the test process so this also runs on Windows without Unix sleep.
        let pid = std::process::id();
        let trees = scan_process_trees(&[pid]);
        let tree = trees.get(&pid).expect("tree for test process");
        assert!(!tree.is_empty());
        assert_eq!(tree[0].pid, pid);
        assert!(!tree[0].name.is_empty());
    }

    #[test]
    fn missing_root_yields_empty_tree() {
        let trees = scan_process_trees(&[u32::MAX - 17]);
        assert!(trees.get(&(u32::MAX - 17)).is_some_and(|t| t.is_empty()));
    }

    #[test]
    fn start_time_and_cwd_answer_for_this_process_and_not_for_a_ghost() {
        let me = std::process::id();
        let started = process_start_time_secs(me).expect("own start time");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(started > 0 && started <= now + 5, "start_time {started}");
        assert_eq!(process_start_time_secs(u32::MAX - 17), None);

        // cwd는 플랫폼/권한에 따라 없을 수 있다 — 있으면 실제 경로여야 한다.
        if let Some(cwd) = process_cwd(me) {
            assert!(std::path::Path::new(&cwd).is_dir(), "cwd was {cwd:?}");
        }
        assert_eq!(process_cwd(u32::MAX - 17), None);
    }

    #[test]
    fn ancestors_start_at_the_parent_and_are_bounded() {
        let chain = ancestor_pids(std::process::id());
        assert!(!chain.is_empty(), "test harness always has a parent");
        assert!(chain.len() <= ANCESTOR_MAX);
        assert!(!chain.contains(&std::process::id()), "no self, no cycle");
        assert!(!chain.contains(&0));
        // 같은 pid가 두 번 나오지 않는다(순환 방어).
        let unique: std::collections::HashSet<_> = chain.iter().collect();
        assert_eq!(unique.len(), chain.len(), "chain was {chain:?}");
        // 없는 pid는 빈 사슬이다.
        assert!(ancestor_pids(u32::MAX - 17).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn nested_child_appears_in_tree() {
        // sh가 sleep을 실행하면 sleep은 sh의 자손이어야 한다.
        let mut sh = Command::new("sh")
            .arg("-c")
            .arg("sleep 2")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sh");
        // sh -c는 곧바로 exec할 수 있으니 이름이 아닌 구조로 확인한다:
        // 루트 아래 자식이 하나 이상 있거나, 루트 자체가 sleep이다.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let trees = scan_process_trees(&[sh.id()]);
        let tree = trees.get(&sh.id()).expect("tree");
        // Reap our child even if an observation assertion fails. On macOS a
        // cached process name can remain `bash` after exec while exe/cmd are
        // refreshed; the current executable is valid evidence of exec'd sleep.
        let _ = sh.kill();
        let _ = sh.wait();
        assert!(!tree.is_empty());
        assert!(
            tree.len() > 1
                || tree[0].name == "sleep"
                || tree[0].exe.as_deref().is_some_and(|exe| {
                    std::path::Path::new(exe)
                        .file_name()
                        .is_some_and(|name| name == "sleep")
                }),
            "expected a descendant or exec'd sleep, got {tree:?}"
        );
    }
}
