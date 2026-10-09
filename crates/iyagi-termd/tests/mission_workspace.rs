//! O1 workspace integration tests (ticket O06, 06 §3 W01–W05 + E16): real
//! temporary Git repositories, detached worktrees, capture scoping, and
//! deterministic/conflicting integration. User checkouts stay untouched.

use iyagi_termd_lib::workspace::{
    add_detached_worktree, capture, ensure_clean, integrate, repository_identity, WorkspaceLeases,
};
use iyagi_termd_lib::workspace::{integration::IntegrationSource, CapturedCandidate};
use term_contracts::mission::types::Id;

fn integration_source(candidate: &CapturedCandidate) -> IntegrationSource {
    IntegrationSource {
        candidate_id: candidate.candidate_id.clone(),
        source_run_ids: candidate.source_run_ids.clone(),
        base_oid: candidate.base_oid.clone(),
        commit_oid: candidate.commit_oid.clone(),
        tree_oid: candidate.tree_oid.clone(),
    }
}

fn frozen_ref_case(remove_ref: bool, replace_object: bool) {
    let (repo, base) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let writer = work.path().join("writer");
    let mission = Id::generate();
    add_detached_worktree(repo.path(), &base, &writer).unwrap();
    std::fs::write(writer.join("source.txt"), "recorded candidate\n").unwrap();
    let original = capture(
        &writer,
        &mission,
        vec![Id::generate()],
        &["source.txt".into()],
        &base,
    )
    .unwrap();
    std::fs::write(writer.join("source.txt"), "later unrelated contents\n").unwrap();
    let later = capture(
        &writer,
        &mission,
        vec![Id::generate()],
        &["source.txt".into()],
        &base,
    )
    .unwrap();
    let reference = iyagi_termd_lib::workspace::git::candidate_ref(
        mission.as_str(),
        original.candidate_id.as_str(),
    );
    if remove_ref {
        git(repo.path(), &["update-ref", "-d", &reference]);
    } else {
        git(repo.path(), &["update-ref", &reference, &later.commit_oid]);
    }
    if replace_object {
        git(
            repo.path(),
            &["replace", &original.commit_oid, &later.commit_oid],
        );
        let raw = std::process::Command::new("git")
            .current_dir(repo.path())
            .env_remove("GIT_NO_REPLACE_OBJECTS")
            .args(["show", &format!("{}:source.txt", original.commit_oid)])
            .output()
            .unwrap();
        assert!(raw.status.success());
        assert_eq!(
            raw.stdout, b"later unrelated contents\n",
            "replacement really changes ordinary Git reads"
        );
    }
    let refs_before = git(repo.path(), &["show-ref"]);
    let destination = work.path().join("integration");
    add_detached_worktree(repo.path(), &base, &destination).unwrap();
    let sources = [integration_source(&original)];
    let (outcome, integrated) =
        integrate(repo.path(), &destination, &mission, &sources, &base).unwrap();
    assert_eq!(outcome.sources, sources);
    assert_eq!(
        std::fs::read_to_string(destination.join("source.txt")).unwrap(),
        "recorded candidate\n"
    );
    assert_eq!(integrated.unwrap().tree_oid, original.tree_oid);
    // Existing private/replacement refs remain exactly as the user set them.
    let refs_after = git(repo.path(), &["show-ref"]);
    assert!(refs_before
        .lines()
        .all(|line| refs_after.lines().any(|after| after == line)));
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), base);
    assert!(git(repo.path(), &["status", "--porcelain"]).is_empty());
}

#[test]
fn integration_uses_captured_commit_when_private_ref_moves() {
    frozen_ref_case(false, false);
}

#[test]
fn integration_uses_captured_commit_when_private_ref_is_deleted() {
    frozen_ref_case(true, false);
}

#[test]
fn integration_ignores_git_replacement_objects_without_deleting_them() {
    frozen_ref_case(false, true);
}

#[test]
fn integration_validates_every_source_before_applying_any_patch() {
    let (repo, base) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let writer = work.path().join("writer");
    let mission = Id::generate();
    add_detached_worktree(repo.path(), &base, &writer).unwrap();
    std::fs::write(writer.join("source.txt"), "source\n").unwrap();
    let candidate = capture(
        &writer,
        &mission,
        vec![Id::generate()],
        &["source.txt".into()],
        &base,
    )
    .unwrap();
    let valid = integration_source(&candidate);
    let destination = work.path().join("integration");
    add_detached_worktree(repo.path(), &base, &destination).unwrap();
    let refs_before = git(repo.path(), &["show-ref"]);
    let base_tree = git(&destination, &["write-tree"]);
    for failure in [
        "tree",
        "symbolic_commit",
        "missing_commit",
        "not_commit",
        "symbolic_base",
        "duplicate",
    ] {
        let mut bad = valid.clone();
        bad.candidate_id = Id::generate();
        match failure {
            "tree" => bad.tree_oid = base_tree.clone(),
            "symbolic_commit" => bad.commit_oid = "HEAD".into(),
            "missing_commit" => bad.commit_oid = "0".repeat(40),
            "not_commit" => bad.commit_oid = valid.tree_oid.clone(),
            "symbolic_base" => bad.base_oid = "HEAD".into(),
            "duplicate" => bad.candidate_id = valid.candidate_id.clone(),
            _ => unreachable!(),
        }
        assert!(
            integrate(
                repo.path(),
                &destination,
                &mission,
                &[valid.clone(), bad],
                &base
            )
            .is_err(),
            "{failure}"
        );
        assert_eq!(git(&destination, &["rev-parse", "HEAD"]), base, "{failure}");
        assert_eq!(git(&destination, &["write-tree"]), base_tree, "{failure}");
        assert!(
            git(&destination, &["status", "--porcelain"]).is_empty(),
            "{failure}"
        );
        assert_eq!(git(repo.path(), &["show-ref"]), refs_before, "{failure}");
    }
}

#[test]
fn integration_applies_repair_relative_to_its_own_input() {
    let (repo, base) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let writer = work.path().join("writer");
    let mission = Id::generate();
    add_detached_worktree(repo.path(), &base, &writer).unwrap();
    std::fs::write(writer.join("source.txt"), "first version\n").unwrap();
    let first = capture(
        &writer,
        &mission,
        vec![Id::generate()],
        &["source.txt".into()],
        &base,
    )
    .unwrap();
    std::fs::write(writer.join("source.txt"), "repaired version\n").unwrap();
    let repair = capture(
        &writer,
        &mission,
        vec![Id::generate()],
        &["source.txt".into()],
        &first.commit_oid,
    )
    .unwrap();
    let destination = work.path().join("integration");
    add_detached_worktree(repo.path(), &base, &destination).unwrap();
    let sources = [integration_source(&first), integration_source(&repair)];
    let (outcome, integrated) =
        integrate(repo.path(), &destination, &mission, &sources, &base).unwrap();
    assert_eq!(outcome.sources, sources);
    assert_eq!(integrated.unwrap().tree_oid, repair.tree_oid);
    assert_eq!(
        std::fs::read_to_string(destination.join("source.txt")).unwrap(),
        "repaired version\n"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), base);
}

#[test]
fn integration_refuses_modified_or_wrong_base_workspaces_before_writing() {
    let (repo, base) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let destination = work.path().join("integration");
    let mission = Id::generate();
    add_detached_worktree(repo.path(), &base, &destination).unwrap();
    std::fs::write(destination.join("retained.txt"), "keep this\n").unwrap();
    assert!(integrate(repo.path(), &destination, &mission, &[], &base).is_err());
    assert_eq!(
        std::fs::read_to_string(destination.join("retained.txt")).unwrap(),
        "keep this\n"
    );
    git(&destination, &["add", "retained.txt"]);
    git(&destination, &["commit", "-qm", "different base"]);
    let moved = git(&destination, &["rev-parse", "HEAD"]);
    assert!(integrate(repo.path(), &destination, &mission, &[], &base).is_err());
    assert_eq!(git(&destination, &["rev-parse", "HEAD"]), moved);
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), base);
}

#[cfg(unix)]
#[test]
fn integration_patch_ignores_external_diff_and_text_conversion_config() {
    use std::os::unix::fs::PermissionsExt;
    let (repo, _) = base_repo();
    std::fs::write(repo.path().join("base.txt"), "before\nold\nafter\n").unwrap();
    git(repo.path(), &["add", "base.txt"]);
    git(repo.path(), &["commit", "-qm", "context lines"]);
    let base = git(repo.path(), &["rev-parse", "HEAD"]);
    let work = tempfile::tempdir().unwrap();
    let writer = work.path().join("writer");
    let mission = Id::generate();
    add_detached_worktree(repo.path(), &base, &writer).unwrap();
    std::fs::write(writer.join("source.txt"), "actual contents\n").unwrap();
    std::fs::write(writer.join("base.txt"), "before\nnew\nafter\n").unwrap();
    let source = capture(
        &writer,
        &mission,
        vec![Id::generate()],
        &["source.txt".into(), "base.txt".into()],
        &base,
    )
    .unwrap();
    let hook = work.path().join("external-diff");
    std::fs::write(&hook, "#!/bin/sh\ntouch external-diff-ran\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        repo.path(),
        &["config", "diff.external", hook.to_str().unwrap()],
    );
    git(
        repo.path(),
        &["config", "diff.raw.textconv", hook.to_str().unwrap()],
    );
    git(repo.path(), &["config", "diff.noprefix", "true"]);
    git(repo.path(), &["config", "color.ui", "always"]);
    git(repo.path(), &["config", "diff.outputIndicatorNew", ">"]);
    git(repo.path(), &["config", "diff.outputIndicatorOld", "<"]);
    git(repo.path(), &["config", "diff.outputIndicatorContext", "!"]);
    std::fs::write(repo.path().join(".git/info/attributes"), "*.txt diff=raw\n").unwrap();
    let destination = work.path().join("integration");
    add_detached_worktree(repo.path(), &base, &destination).unwrap();
    let (_, candidate) = integrate(
        repo.path(),
        &destination,
        &mission,
        &[integration_source(&source)],
        &base,
    )
    .unwrap();
    assert_eq!(candidate.unwrap().tree_oid, source.tree_oid);
    assert!(!destination.join("external-diff-ran").exists());
    assert!(!repo.path().join("external-diff-ran").exists());
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    iyagi_termd_lib::workspace::git::run_git_for_test(dir, args)
}

/// A base repo with `base.txt` and one commit.
fn base_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("temp repo");
    let path = dir.path();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "test@iyagi.local"]);
    git(path, &["config", "user.name", "test"]);
    std::fs::write(path.join("base.txt"), "base\n").expect("write");
    git(path, &["add", "-A"]);
    git(path, &["commit", "-m", "base", "-q"]);
    let head = git(path, &["rev-parse", "HEAD"]);
    (dir, head)
}

#[test]
fn w01_dirty_repository_blocks_start_without_touching_it() {
    let (dir, _head) = base_repo();
    std::fs::write(dir.path().join("uncommitted.txt"), "user work").expect("write");
    std::fs::write(dir.path().join("staged.txt"), "staged").expect("write");
    git(dir.path(), &["add", "staged.txt"]);
    let error = ensure_clean(dir.path()).expect_err("dirty");
    assert!(error.to_string().contains("dirty worktree"));
    // Nothing was stashed, committed, or reset.
    assert!(dir.path().join("uncommitted.txt").exists());
    assert!(dir.path().join("staged.txt").exists());
    let status = git(dir.path(), &["status", "--porcelain"]);
    assert_eq!(status.lines().count(), 2);
}

#[test]
fn w02_parallel_worktrees_leave_the_user_checkout_untouched() {
    let (dir, head) = base_repo();
    let mission = Id::generate();
    let work_root = tempfile::tempdir().expect("work root");
    let api_worktree = work_root.path().join("api");
    let ui_worktree = work_root.path().join("ui");
    add_detached_worktree(dir.path(), &head, &api_worktree).expect("api worktree");
    add_detached_worktree(dir.path(), &head, &ui_worktree).expect("ui worktree");

    std::fs::write(api_worktree.join("api.txt"), "api change\n").expect("write api");
    std::fs::write(ui_worktree.join("ui.txt"), "ui change\n").expect("write ui");

    let api_candidate = capture(
        &api_worktree,
        &mission,
        vec![Id::generate()],
        &["api.txt".to_string()],
        &head,
    )
    .expect("api capture");
    let ui_candidate = capture(
        &ui_worktree,
        &mission,
        vec![Id::generate()],
        &["ui.txt".to_string()],
        &head,
    )
    .expect("ui capture");
    assert_eq!(api_candidate.manifest.entries.len(), 1);
    assert_eq!(ui_candidate.manifest.entries.len(), 1);

    // The user checkout never moved and still has only base.txt.
    let user_status = git(dir.path(), &["status", "--porcelain"]);
    assert!(
        user_status.is_empty(),
        "user checkout changed: {user_status}"
    );
    assert!(dir.path().join("base.txt").exists());
    assert!(!dir.path().join("api.txt").exists());
    // Per-source manifests recorded the real paths.
    assert_eq!(api_candidate.manifest.entries[0].path, "api.txt");
    assert_eq!(ui_candidate.manifest.entries[0].path, "ui.txt");
}

#[test]
fn w03_conflicting_changes_record_a_conflict_without_implicit_ours() {
    let (dir, head) = base_repo();
    let mission = Id::generate();
    let work_root = tempfile::tempdir().expect("work root");
    let a = work_root.path().join("a");
    let b = work_root.path().join("b");
    add_detached_worktree(dir.path(), &head, &a).expect("worktree a");
    add_detached_worktree(dir.path(), &head, &b).expect("worktree b");
    std::fs::write(a.join("shared.txt"), "version A\n").expect("write");
    std::fs::write(
        b.join("shared.txt"),
        "version B with more lines\nsecond line\n",
    )
    .expect("write");
    let candidate_a = capture(
        &a,
        &mission,
        vec![Id::generate()],
        &["shared.txt".to_string()],
        &head,
    )
    .expect("capture a");
    let candidate_b = capture(
        &b,
        &mission,
        vec![Id::generate()],
        &["shared.txt".to_string()],
        &head,
    )
    .expect("capture b");

    let integration_root = work_root.path().join("integration");
    add_detached_worktree(dir.path(), &head, &integration_root).expect("integration worktree");
    let (outcome, integrated) = integrate(
        dir.path(),
        &integration_root,
        &mission,
        &[
            integration_source(&candidate_a),
            integration_source(&candidate_b),
        ],
        &head,
    )
    .expect("integrate call");
    // The second candidate conflicts; nothing was silently chosen.
    let (conflicted, paths) = outcome.conflict.expect("conflict recorded");
    assert_eq!(conflicted, candidate_b.candidate_id);
    assert!(paths.iter().any(|p| p.contains("shared.txt")), "{paths:?}");
    assert!(integrated.is_none(), "no candidate minted on conflict");
    assert_eq!(outcome.sources.len(), 1, "first source applied");
}

#[cfg(unix)]
#[test]
fn integration_records_literal_conflict_paths_from_the_index() {
    let (repo, base) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let mission = Id::generate();
    let filename = "결과 \"quoted\"\ttab\nline.txt ";
    let mut sources = Vec::new();
    for (name, contents) in [("a", "version A\n"), ("b", "version B\n")] {
        let writer = work.path().join(name);
        add_detached_worktree(repo.path(), &base, &writer).unwrap();
        std::fs::write(writer.join(filename), contents).unwrap();
        let captured = capture(
            &writer,
            &mission,
            vec![Id::generate()],
            &[filename.into()],
            &base,
        )
        .unwrap();
        sources.push(integration_source(&captured));
    }
    let destination = work.path().join("integration");
    add_detached_worktree(repo.path(), &base, &destination).unwrap();
    let (outcome, candidate) =
        integrate(repo.path(), &destination, &mission, &sources, &base).unwrap();
    assert!(candidate.is_none());
    assert_eq!(
        outcome.conflict,
        Some((sources[1].candidate_id.clone(), vec![filename.into()]))
    );
    assert!(std::fs::read_to_string(destination.join(filename))
        .unwrap()
        .contains("<<<<<<<"));
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), base);
    assert!(git(repo.path(), &["status", "--porcelain"]).is_empty());
}

#[test]
fn w04_out_of_scope_and_git_changes_are_rejected_with_workspace_retained() {
    let (dir, head) = base_repo();
    let mission = Id::generate();
    let work_root = tempfile::tempdir().expect("work root");
    let worktree = work_root.path().join("w");
    add_detached_worktree(dir.path(), &head, &worktree).expect("worktree");
    std::fs::write(worktree.join("outside.txt"), "leak").expect("write");
    let error = capture(&worktree, &mission, vec![], &["src/".to_string()], &head)
        .expect_err("out of scope");
    assert!(error.to_string().contains("outside the allowed scope"));
    // Workspace retained for review: the file was not reverted or deleted.
    assert!(worktree.join("outside.txt").exists());
}

#[test]
fn w05_candidates_are_immutable_and_differ_per_source() {
    let (dir, head) = base_repo();
    let mission = Id::generate();
    let work_root = tempfile::tempdir().expect("work root");
    let worktree = work_root.path().join("w");
    add_detached_worktree(dir.path(), &head, &worktree).expect("worktree");
    std::fs::write(worktree.join("v1.txt"), "v1").expect("write");
    let first = capture(
        &worktree,
        &mission,
        vec![Id::generate()],
        &["v1.txt".to_string()],
        &head,
    )
    .expect("first capture");
    // Second capture on the same worktree adds a file: a NEW candidate id,
    // the first candidate's ref still points at its own commit.
    std::fs::write(worktree.join("v2.txt"), "v2").expect("write");
    let second = capture(
        &worktree,
        &mission,
        vec![Id::generate()],
        &["v1.txt".to_string(), "v2.txt".to_string()],
        &head,
    )
    .expect("second capture");
    assert_ne!(first.candidate_id, second.candidate_id);
    assert_ne!(first.commit_oid, second.commit_oid);
    let first_ref = format!(
        "refs/iyagi/missions/{}/candidates/{}",
        mission, first.candidate_id
    );
    assert_eq!(
        git(dir.path(), &["rev-parse", &first_ref]),
        first.commit_oid
    );
}

#[test]
fn e16_exclusive_writer_lease() {
    let leases = WorkspaceLeases::new();
    let workspace = Id::generate();
    let first = leases
        .acquire(workspace.clone(), Id::generate())
        .expect("first");
    let error = leases
        .acquire(workspace.clone(), Id::generate())
        .expect_err("second");
    assert!(error.to_string().contains("already has writer run"));
    leases.release(&workspace, first.token).expect("release");
    leases
        .acquire(workspace, Id::generate())
        .expect("after release");
}

#[test]
fn repository_identity_uses_common_dir() {
    let (dir, head) = base_repo();
    let identity = repository_identity(dir.path()).expect("identity");
    assert_eq!(identity.head_oid, head);
    assert_eq!(identity.object_format, "sha1");
    assert!(identity.common_dir.exists());
}

#[test]
fn capture_refuses_an_agent_selected_branch_without_moving_it() {
    let (dir, head) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("writer");
    add_detached_worktree(dir.path(), &head, &path).unwrap();
    git(&path, &["checkout", "-b", "user-feature"]);
    std::fs::write(path.join("new.txt"), "private change").unwrap();
    let error = capture(&path, &Id::generate(), vec![], &["new.txt".into()], &head).unwrap_err();
    assert!(error.to_string().contains("detached HEAD"));
    assert_eq!(git(dir.path(), &["rev-parse", "user-feature"]), head);
    assert!(path.join("new.txt").is_file());
}

#[test]
fn scope_checks_both_sides_of_a_staged_rename_and_provider_commits() {
    let (dir, head) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("writer");
    add_detached_worktree(dir.path(), &head, &path).unwrap();
    git(&path, &["mv", "base.txt", "renamed.txt"]);
    assert!(capture(
        &path,
        &Id::generate(),
        vec![],
        &["renamed.txt".into()],
        &head
    )
    .is_err());
    git(&path, &["commit", "-qm", "agent rename"]);
    assert!(capture(
        &path,
        &Id::generate(),
        vec![],
        &["renamed.txt".into()],
        &head
    )
    .is_err());
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head);
}

#[cfg(unix)]
#[test]
fn capture_preserves_literal_unicode_quote_newline_and_trailing_space_paths() {
    let (dir, head) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("writer");
    add_detached_worktree(dir.path(), &head, &path).unwrap();
    let filename = "\u{acb0}\u{acfc} \"quoted\"\nline.txt ";
    std::fs::write(path.join(filename), "actual bytes").unwrap();
    let captured = capture(&path, &Id::generate(), vec![], &[filename.into()], &head).unwrap();
    assert_eq!(captured.manifest.entries[0].path, filename);
    assert_eq!(captured.manifest.entries[0].bytes, 12);
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head);
}

#[cfg(unix)]
#[test]
fn capture_rejects_symlink_chains_that_escape_the_workspace() {
    use std::os::unix::fs::symlink;
    let (dir, head) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("writer");
    let outside = tempfile::NamedTempFile::new().unwrap();
    add_detached_worktree(dir.path(), &head, &path).unwrap();
    symlink(outside.path(), path.join("indirect")).unwrap();
    symlink("indirect", path.join("link")).unwrap();
    let error = capture(
        &path,
        &Id::generate(),
        vec![],
        &["indirect".into(), "link".into()],
        &head,
    )
    .unwrap_err();
    assert!(error.to_string().contains("outside the workspace"));
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), head);
}

#[cfg(unix)]
#[test]
fn capture_does_not_invoke_repository_commit_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, head) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("writer");
    add_detached_worktree(dir.path(), &head, &path).unwrap();
    let hooks = work.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-commit");
    std::fs::write(&hook, "#!/bin/sh\ntouch hook-was-run\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &path,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    std::fs::write(path.join("new.txt"), "captured").unwrap();
    capture(&path, &Id::generate(), vec![], &["new.txt".into()], &head).unwrap();
    assert!(!path.join("hook-was-run").exists());
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head);
}

#[test]
fn composing_the_same_dependency_input_is_stable_and_uses_no_user_index() {
    let (dir, head) = base_repo();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("writer");
    let mission = Id::generate();
    let run = Id::generate();
    add_detached_worktree(dir.path(), &head, &path).unwrap();
    std::fs::write(path.join("source.txt"), "from dependency").unwrap();
    let source = capture(&path, &mission, vec![], &["source.txt".into()], &head).unwrap();
    std::fs::write(dir.path().join("user-staged.txt"), "unrelated user work").unwrap();
    git(dir.path(), &["add", "user-staged.txt"]);
    let index_before = git(dir.path(), &["write-tree"]);
    let sources = [(source.base_oid, source.commit_oid)];
    let compose = || {
        iyagi_termd_lib::workspace::git::compose_task_input(
            dir.path(),
            work.path(),
            mission.as_str(),
            run.as_str(),
            &head,
            &sources,
        )
        .unwrap()
    };
    let first = compose();
    let second = compose();
    assert_eq!(
        first, second,
        "input identity is reproducible across retries"
    );
    assert_eq!(
        git(dir.path(), &["show", &format!("{first}:source.txt")]),
        "from dependency"
    );
    assert_eq!(git(dir.path(), &["write-tree"]), index_before);
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head);
    assert!(!dir.path().join("source.txt").exists());
}
