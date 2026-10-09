//! Verifier inputs are the candidate's raw blobs, without checkout executables.
use super::git::{run_git, run_git_bytes, GitError, GitResult};
use std::{collections::BTreeMap, path::Path};

fn invalid(message: &str) -> GitError {
    GitError::Git {
        args: vec!["verification-input".into()],
        stderr: message.into(),
    }
}

const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 512 * 1024 * 1024;
type CandidateFiles = BTreeMap<String, (String, String)>;

pub fn create(repo: &Path, commit: &str, destination: &Path) -> GitResult<()> {
    let entries = candidate_files(repo, commit)?;
    // Never invoke checkout conversion: conditional/worktree-local config can
    // introduce filters that were not visible when examining the source repo.
    // Hook/fsmonitor/submodule neutralization comes from git_command() (via
    // run_git), which picks the platform-correct core.hooksPath. Passing our own
    // `-c core.hooksPath=/dev/null` here would override it (later -c wins) and
    // reopen `\dev\null\<hook>` on Windows.
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            &destination.to_string_lossy(),
            commit,
        ],
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    }
    // Populate only the index; omitting -u prevents worktree checkout/filtering.
    run_git(destination, &["read-tree", "--reset", commit])?;
    let mut total = 0u64;
    for (name, (mode, oid)) in entries {
        let size = run_git(repo, &["cat-file", "-s", &oid])?
            .parse::<u64>()
            .map_err(|_| invalid("invalid verification blob size"))?;
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("verification input size overflow"))?;
        if size > MAX_BLOB_BYTES || total > MAX_INPUT_BYTES {
            return Err(invalid("verification candidate exceeds input byte limit"));
        }
        let bytes = run_git_bytes(repo, &["cat-file", "blob", &oid])?;
        if bytes.len() as u64 != size {
            return Err(invalid("verification blob size changed"));
        }
        let path = destination.join(&name);
        let mut directory = destination.to_path_buf();
        for component in Path::new(&name).parent().unwrap().components() {
            directory.push(component);
            match std::fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => return Err(invalid("verification parent is not an owned directory")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&directory)?
                }
                Err(e) => return Err(e.into()),
            }
        }
        if mode == "120000" {
            let target =
                std::str::from_utf8(&bytes).map_err(|_| invalid("non-UTF-8 input link"))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &path)?;
            #[cfg(not(unix))]
            {
                let _ = target;
                return Err(invalid("verification symlinks are unsupported on this OS"));
            }
        } else {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(if mode == "100755" { 0o755 } else { 0o644 });
            }
            let mut file = options.open(&path)?;
            file.write_all(&bytes)?;
        }
    }
    validate(destination, commit)
}

fn candidate_files(root: &Path, commit: &str) -> GitResult<CandidateFiles> {
    let tree = run_git_bytes(root, &["ls-tree", "-r", "-z", "--full-tree", commit])?;
    if tree.len() > 8 * 1024 * 1024 {
        return Err(invalid("verification input tree exceeds limit"));
    }
    let mut expected = BTreeMap::new();
    for record in tree.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| invalid("invalid candidate tree entry"))?;
        let fields = std::str::from_utf8(&record[..tab])
            .map_err(|_| invalid("invalid tree metadata"))?
            .split(' ')
            .collect::<Vec<_>>();
        let name = std::str::from_utf8(&record[tab + 1..])
            .map_err(|_| invalid("non-UTF-8 verification input path"))?;
        if fields.len() != 3
            || fields[1] != "blob"
            || !matches!(fields[0], "100644" | "100755" | "120000")
            || term_contracts::mission::validation::validate_allowed_path(name).is_err()
            || !Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(invalid("unsupported verification input entry"));
        }
        if expected
            .insert(
                name.to_owned(),
                (fields[0].to_owned(), fields[2].to_owned()),
            )
            .is_some()
        {
            return Err(invalid("duplicate verification input path"));
        }
    }
    Ok(expected)
}

pub fn validate(root: &Path, commit: &str) -> GitResult<()> {
    if run_git(root, &["rev-parse", "HEAD"])? != commit {
        return Err(invalid("verification HEAD differs from candidate"));
    }
    let expected = candidate_files(root, commit)?;
    // Inspect every physical entry, including ignored/untracked files. Do not
    // ask Git's clean filters whether modified bytes should count as clean.
    let mut pending = vec![root.to_path_buf()];
    let canonical_root = root.canonicalize()?;
    let mut seen = 0usize;
    let mut regular = Vec::new();
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(directory)? {
            let path = item?.path();
            let name = path
                .strip_prefix(root)
                .map_err(|_| invalid("verification path escaped root"))?
                .to_str()
                .ok_or_else(|| invalid("non-UTF-8 input path"))?
                .to_owned();
            if name == ".git" {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let Some((mode, oid)) = expected.get(&name) else {
                return Err(invalid("untracked verification input"));
            };
            seen += 1;
            if mode == "120000" {
                if !metadata.file_type().is_symlink()
                    || !path.canonicalize()?.starts_with(&canonical_root)
                {
                    return Err(invalid("verification symlink escapes candidate"));
                }
                let target = std::fs::read_link(&path)?;
                let target = target
                    .to_str()
                    .ok_or_else(|| invalid("non-UTF-8 input symlink"))?;
                if run_git_bytes(root, &["cat-file", "blob", oid])? != target.as_bytes() {
                    return Err(invalid("verification symlink differs from candidate"));
                }
            } else {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(invalid("verification file type changed"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if (metadata.permissions().mode() & 0o111 != 0) != (mode == "100755") {
                        return Err(invalid("verification executable mode changed"));
                    }
                }
                regular.push((name, oid.clone()));
            }
        }
    }
    if seen != expected.len() {
        return Err(invalid(
            "candidate files are missing from verification input",
        ));
    }
    for batch in regular.chunks(16) {
        let mut args = vec!["hash-object", "--no-filters", "--"];
        args.extend(batch.iter().map(|(name, _)| name.as_str()));
        let output = run_git(root, &args)?;
        let hashes = output.lines().collect::<Vec<_>>();
        if hashes.len() != batch.len()
            || hashes
                .iter()
                .zip(batch)
                .any(|(hash, (_, expected))| *hash != expected)
        {
            return Err(invalid("verification file bytes differ from candidate"));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn repo(root: &Path) -> String {
        run_git(root, &["init", "-q"]).unwrap();
        std::fs::write(root.join("input"), b"base\n").unwrap();
        std::fs::write(root.join(".gitattributes"), b"input filter=untrusted\n").unwrap();
        std::fs::write(root.join(".gitignore"), b"ignored\n").unwrap();
        std::fs::write(root.join("한글\nfile.txt"), b"literal filename\n").unwrap();
        symlink("input", root.join("link")).unwrap();
        run_git(root, &["add", "."]).unwrap();
        run_git(root, &["commit", "-qm", "candidate"]).unwrap();
        run_git(root, &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn verifier_checkout_skips_hooks_filters_and_compares_raw_blob_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let commit = repo(&root);
        let marker = dir.path().join("executed");
        let executable = dir.path().join("unexpected-hook");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf executed >> '{}'\nprintf 'base\\n'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::copy(&executable, root.join(".git/hooks/post-checkout")).unwrap();
        for key in [
            "filter.untrusted.smudge",
            "filter.untrusted.clean",
            "filter.untrusted.process",
        ] {
            run_git(&root, &["config", key, executable.to_str().unwrap()]).unwrap();
        }
        run_git(&root, &["config", "filter.untrusted.required", "true"]).unwrap();
        let workspace = dir.path().join("verification");
        create(&root, &commit, &workspace).unwrap();
        assert!(!marker.exists());
        std::fs::write(workspace.join("input"), b"changed\n").unwrap();
        assert!(validate(&workspace, &commit).is_err());
        assert!(
            !marker.exists(),
            "raw hashing must not run the clean filter"
        );
        std::fs::write(workspace.join("input"), b"base\n").unwrap();
        validate(&workspace, &commit).unwrap();
        std::fs::write(workspace.join("ignored"), b"injected\n").unwrap();
        assert!(validate(&workspace, &commit).is_err());
        std::fs::remove_file(workspace.join("ignored")).unwrap();
        std::fs::set_permissions(
            workspace.join("input"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(validate(&workspace, &commit).is_err());
    }

    #[test]
    fn verifier_rejects_missing_files_and_links_to_external_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let commit = repo(&root);
        let workspace = dir.path().join("verification");
        create(&root, &commit, &workspace).unwrap();
        std::fs::remove_file(workspace.join("link")).unwrap();
        assert!(validate(&workspace, &commit).is_err());
        symlink(root.join("input"), workspace.join("link")).unwrap();
        assert!(validate(&workspace, &commit).is_err());
    }

    #[test]
    fn raw_materialization_skips_conditional_filters_and_line_ending_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        repo(&root);
        std::fs::write(
            root.join(".gitattributes"),
            b"input filter=untrusted\ncrlf.txt text eol=crlf\n",
        )
        .unwrap();
        std::fs::write(root.join("crlf.txt"), b"raw candidate\n").unwrap();
        run_git(&root, &["add", "."]).unwrap();
        run_git(&root, &["commit", "-qm", "attributes"]).unwrap();
        let commit = run_git(&root, &["rev-parse", "HEAD"]).unwrap();
        let marker = dir.path().join("conditional-executed");
        let executable = dir.path().join("conditional-filter");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\nprintf executed > '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = dir.path().join("only-verification.config");
        std::fs::write(
            &config,
            format!(
                "[filter \"untrusted\"]\n smudge = {}\n clean = {}\n required = true\n",
                executable.display(),
                executable.display()
            ),
        )
        .unwrap();
        run_git(
            &root,
            &[
                "config",
                "includeIf.gitdir:**/worktrees/verification.path",
                config.to_str().unwrap(),
            ],
        )
        .unwrap();
        let workspace = dir.path().join("verification");
        create(&root, &commit, &workspace).unwrap();
        assert_eq!(
            run_git(&workspace, &["config", "filter.untrusted.smudge"]).unwrap(),
            executable.to_string_lossy()
        );
        assert!(!marker.exists(), "worktree-only filter must never execute");
        assert_eq!(
            std::fs::read(workspace.join("crlf.txt")).unwrap(),
            b"raw candidate\n"
        );
        validate(&workspace, &commit).unwrap();
    }
}
