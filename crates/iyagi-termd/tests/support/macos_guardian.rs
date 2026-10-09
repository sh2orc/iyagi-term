//! Real process-boundary tests: kill the launcher while its guardian and
//! target survive. No installed provider, credentials or unrelated PID is used.
use super::*;
use std::process::{Child, Command, Stdio};
use term_contracts::{
    ids::{ProcessIdentity, WorkloadId},
    workload::GroupRecoveryIdentity,
};

struct Launcher(Child);
impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Owned(ProcessIdentity);
impl Drop for Owned {
    fn drop(&mut self) {
        if alive(&self.0) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &self.0.pid.to_string()])
                .status();
        }
    }
}
fn alive(identity: &ProcessIdentity) -> bool {
    term_platform::process_identity(identity.pid).is_some_and(|p| p.same_process(identity))
}

#[tokio::test]
async fn launcher_fixture() {
    let Some(directory) = std::env::var_os("IYAGI_GUARDIAN_LAUNCH_FIXTURE") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let store = Arc::new(FaultStore::default());
    let supervisor = gated_supervisor(store.clone(), &directory);
    let mut request = marker_request(&directory.join("unused"));
    request.program = "/bin/sleep".into();
    request.argv = vec!["60".into()];
    let handle = supervisor.spawn(request).await.unwrap();
    let record = store
        .log
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.state == ExecState::Spawned)
        .unwrap()
        .clone();
    std::fs::write(
        directory.join("owner.pending"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    std::fs::rename(
        directory.join("owner.pending"),
        directory.join("owner.json"),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_secs(30)).await;
    handle.stop(Duration::ZERO, Duration::ZERO).await.unwrap();
}

async fn crashed_launcher() -> (tempfile::TempDir, ExecRecord, Owned, Owned) {
    let directory = tempfile::tempdir().unwrap();
    let mut launcher = Launcher(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "macos_guardian::launcher_fixture", "--nocapture"])
            .env("IYAGI_GUARDIAN_LAUNCH_FIXTURE", directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    eventually(|| directory.path().join("owner.json").is_file()).await;
    let record: ExecRecord =
        serde_json::from_slice(&std::fs::read(directory.path().join("owner.json")).unwrap())
            .unwrap();
    let GroupRecoveryIdentity::MacosGuardian { guardian, .. } =
        record.group_identity.as_ref().unwrap()
    else {
        panic!("missing guardian ownership");
    };
    use std::os::unix::fs::PermissionsExt;
    let parent = Path::new(record.group_reference.as_ref().unwrap())
        .parent()
        .unwrap();
    assert_eq!(
        std::fs::metadata(parent).unwrap().permissions().mode() & 0o077,
        0
    );
    let guardian = Owned(guardian.clone());
    let target = Owned(record.identity.clone().unwrap());
    assert!(alive(&target.0));
    assert!(alive(&guardian.0));
    launcher.0.kill().unwrap();
    launcher.0.wait().unwrap();
    assert!(
        alive(&target.0),
        "target unexpectedly depended on launcher lifetime"
    );
    assert!(alive(&guardian.0), "observer died with the launcher");
    (directory, record, target, guardian)
}

#[tokio::test]
async fn guardian_survives_launcher_kill_and_recovers_the_original_owned_tree() {
    let (_directory, record, target, guardian) = crashed_launcher().await;
    let platform = term_platform::group::select_backend();
    let workload = WorkloadId::parse(record.id.as_str()).unwrap();
    let group = platform
        .recover_group(
            &workload,
            record.group_reference.as_ref().unwrap(),
            record.group_identity.as_ref().unwrap(),
        )
        .unwrap();
    platform.verify_recovered_root(&group, &target.0).unwrap();
    let mut wrong_root = target.0.clone();
    wrong_root.start_token = "wrong-birth".into();
    assert!(platform.verify_recovered_root(&group, &wrong_root).is_err());
    assert!(!platform.is_empty(&group).unwrap());
    let usage = platform.sample_group(&group, 1).unwrap();
    assert_ne!(
        usage.coverage,
        term_contracts::metrics::UsageCoverage::Group
    );
    assert!(platform.retire_recovered_group(&group).is_err());
    platform
        .terminate_owned(&group, term_platform::StopPhase::Force)
        .unwrap();
    eventually(|| platform.is_empty(&group).unwrap_or(false)).await;
    assert!(!alive(&target.0));
    assert!(
        alive(&guardian.0),
        "observer must retain exit evidence until durable ack"
    );
    platform.retire_recovered_group(&group).unwrap();
    eventually(|| !alive(&guardian.0)).await;
}

#[tokio::test]
async fn guardian_rejects_wrong_birth_wrong_socket_peer_and_missing_observer() {
    let (_directory, record, target, guardian) = crashed_launcher().await;
    let platform = term_platform::group::select_backend();
    let workload = WorkloadId::parse(record.id.as_str()).unwrap();
    let proof = record.group_identity.as_ref().unwrap();
    let mut wrong = proof.clone();
    if let GroupRecoveryIdentity::MacosGuardian { guardian, .. } = &mut wrong {
        guardian.start_token = "wrong-birth".into();
    }
    assert!(platform
        .recover_group(&workload, record.group_reference.as_ref().unwrap(), &wrong)
        .is_err());
    // Reach peer authentication through an otherwise valid private parent.
    // A public temp directory would reject this before checking the peer PID.
    let fake_path = Path::new(record.group_reference.as_ref().unwrap())
        .parent()
        .unwrap()
        .join("impostor");
    let _listener = std::os::unix::net::UnixListener::bind(&fake_path).unwrap();
    let error = term_platform::group::macos_guardian::connect_group(
        &workload,
        fake_path.to_str().unwrap(),
        &guardian.0,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "guardian socket belongs to another process"
    );
    assert!(alive(&target.0));
    assert!(alive(&guardian.0));
    let group = platform
        .recover_group(&workload, record.group_reference.as_ref().unwrap(), proof)
        .unwrap();
    drop(guardian); // Simulate loss of the independent observer itself.
    eventually(|| platform.is_empty(&group).is_err()).await;
    assert!(platform
        .recover_group(&workload, record.group_reference.as_ref().unwrap(), proof)
        .is_err());
    assert!(
        alive(&target.0),
        "missing observer must never become a successful cleanup"
    );
}
