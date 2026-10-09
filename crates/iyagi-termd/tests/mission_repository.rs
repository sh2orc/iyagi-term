use iyagi_termd_lib::mission::{artifacts::ArtifactStore, MissionService};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};
use term_contracts::{ids::ConnectionId, mission::types::Id};
use term_storage::Storage;

fn git(path: &Path, args: &[&str]) -> String {
    iyagi_termd_lib::workspace::git::run_git_for_test(path, args)
}
fn repository(path: &Path) {
    std::fs::create_dir(path).unwrap();
    git(path, &["init", "-q"]);
    std::fs::write(path.join("README.md"), "base\n").unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "-qm", "base"]);
}
fn service(storage: &Arc<Storage>, root: &Path) -> MissionService {
    MissionService::new(
        storage.clone(),
        ArtifactStore::new(storage.clone(), root.join("artifacts")),
    )
}
fn inspect(service: &MissionService, path: &Path) -> Value {
    service
        .handle(
            &ConnectionId::generate(),
            "repository.inspect",
            &json!({"path":path}),
        )
        .unwrap()
        .result
}

#[test]
fn linked_worktrees_subdirectories_and_symlinks_share_one_repository_identity() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    repository(&repo);
    let linked = root.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::create_dir(linked.join("subdirectory")).unwrap();
    let storage = Arc::new(Storage::open(root.path().join("db")).unwrap());
    let svc = service(&storage, root.path());
    let first = inspect(&svc, &repo);
    let second = inspect(&svc, &linked.join("subdirectory"));
    assert_eq!(first["repository_id"], second["repository_id"]);
    assert_ne!(first["canonical_path"], second["canonical_path"]);
    assert_eq!(
        second["canonical_path"],
        linked.canonicalize().unwrap().to_str().unwrap()
    );
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&linked, &alias).unwrap();
        assert_eq!(
            inspect(&svc, &alias)["repository_id"],
            first["repository_id"]
        );
    }
    let stored = storage.mission_configs("repository").unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0]["common_dir"],
        repo.join(".git").canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(stored[0]["object_format"], "sha1");
    let other = root.path().join("other");
    repository(&other);
    assert_ne!(
        inspect(&svc, &other)["repository_id"],
        first["repository_id"]
    );
}

#[test]
fn legacy_path_registration_is_upgraded_without_changing_its_id() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    repository(&repo);
    let linked = root.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let storage = Arc::new(Storage::open(root.path().join("db")).unwrap());
    let id = Id::generate();
    storage.save_mission_config(Id::generate(), "legacy", &"a".repeat(64), "repository", 0,
        json!({"id":id,"canonical_path":repo.canonicalize().unwrap(),"object_format":null,"created_at":"2026-09-16T00:00:00Z"}), "2026-09-16T00:00:00Z".into()).unwrap();
    assert_eq!(
        inspect(&service(&storage, root.path()), &linked)["repository_id"],
        json!(id)
    );
    let rows = storage.mission_configs("repository").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["revision"], "2");
    assert!(rows[0]["common_dir"].is_string());
    assert_eq!(
        rows[0]["canonical_path"],
        json!(repo.canonicalize().unwrap())
    );
}

#[test]
fn concurrent_registration_from_separate_services_returns_one_id() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    repository(&repo);
    let linked = root.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let storage = Arc::new(Storage::open(root.path().join("db")).unwrap());
    let second_storage = Arc::new(Storage::open(root.path().join("db")).unwrap());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [repo, linked]
        .into_iter()
        .zip([storage.clone(), second_storage])
        .map(|(path, store)| {
            let svc = service(&store, root.path());
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                inspect(&svc, &path)["repository_id"].clone()
            })
        })
        .collect();
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(ids[0], ids[1]);
    assert_eq!(storage.mission_configs("repository").unwrap().len(), 1);
}

#[test]
fn storage_rejects_duplicate_or_changed_common_directory_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::open(root.path().join("db")).unwrap();
    let id = Id::generate();
    let original = json!({"id":id,"canonical_path":"/fixture/repo","common_dir":"/fixture/repo/.git","object_format":"sha1"});
    let save = |doc, rev| {
        storage.save_mission_config(
            Id::generate(),
            "repository.register",
            &"b".repeat(64),
            "repository",
            rev,
            doc,
            "2026-09-16T00:00:00Z".into(),
        )
    };
    let saved = save(original.clone(), 0).unwrap().document;
    let mut duplicate = original.clone();
    duplicate["id"] = json!(Id::generate());
    duplicate["canonical_path"] = json!("/fixture/linked");
    assert!(save(duplicate, 0).is_err());
    for common in [Value::Null, json!("/fixture/other/.git")] {
        let mut changed = original.clone();
        changed["common_dir"] = common;
        assert!(save(changed, 1).is_err());
    }
    assert_eq!(storage.mission_configs("repository").unwrap(), vec![saved]);
}

#[test]
fn ambiguous_legacy_ids_are_preserved_and_not_silently_merged() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    repository(&repo);
    let linked = root.path().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let storage = Arc::new(Storage::open(root.path().join("db")).unwrap());
    for path in [&repo, &linked] {
        storage.save_mission_config(Id::generate(), "legacy", &"c".repeat(64), "repository", 0,
            json!({"id":Id::generate(),"canonical_path":path.canonicalize().unwrap(),"object_format":null}), "2026-09-16T00:00:00Z".into()).unwrap();
    }
    let before = storage.mission_configs("repository").unwrap();
    let err = service(&storage, root.path())
        .handle(
            &ConnectionId::generate(),
            "repository.inspect",
            &json!({"path":linked}),
        )
        .err()
        .expect("ambiguous repository registration must fail");
    assert_eq!(
        err.code,
        term_contracts::mission::MissionErrorCode::InvalidState
    );
    assert_eq!(storage.mission_configs("repository").unwrap(), before);
}

#[test]
fn inspection_reports_actionable_git_reasons_and_verification_support() {
    let root = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(root.path().join("db")).unwrap());
    let svc = service(&storage, root.path());
    let conn = ConnectionId::generate();
    let plain = root.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let error = svc
        .handle(&conn, "repository.inspect", &json!({"path":plain}))
        .err()
        .expect("a plain directory is not a repository");
    assert_eq!(
        error.code,
        term_contracts::mission::MissionErrorCode::InvalidArgument
    );
    assert_eq!(
        error.details.reason_code.as_deref(),
        Some("not_a_repository")
    );
    let unborn = root.path().join("unborn");
    std::fs::create_dir(&unborn).unwrap();
    git(&unborn, &["init", "-q"]);
    let error = svc
        .handle(&conn, "repository.inspect", &json!({"path":unborn}))
        .err()
        .expect("a repository without commits has no base");
    assert_eq!(
        error.code,
        term_contracts::mission::MissionErrorCode::InvalidArgument
    );
    assert_eq!(error.details.reason_code.as_deref(), Some("no_commits"));
    let repo = root.path().join("repo");
    repository(&repo);
    assert_eq!(
        inspect(&svc, &repo)["verification_supported"],
        cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file()
    );
}
