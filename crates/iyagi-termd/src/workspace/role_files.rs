//! Repository role instruction files (spec docs/orchestration/10-role-instructions.md
//! §2): `.iyagi/roles/<role>.md` is read from an immutable commit tree, never
//! from the working tree or index. Entries that exist but are not regular
//! UTF-8 files within the byte bound are rejected so a mission start never
//! drops them silently. Pinning the text into mission artifacts is the
//! mission service's job (10 §3).

use std::path::Path;

use term_contracts::mission::types::Role;

use super::git::{run_git_bytes, validate_oid, GitError};

/// Repository-relative, `/`-separated directory holding role instructions.
pub const ROLE_INSTRUCTION_DIR: &str = ".iyagi/roles";

/// Every role in `Role` declaration order (10 §3 processing order).
pub const ROLE_ORDER: [Role; 10] = [
    Role::Lead,
    Role::Researcher,
    Role::Architect,
    Role::Builder,
    Role::TestAuthor,
    Role::Reviewer,
    Role::Specialist,
    Role::Diagnostician,
    Role::Integrator,
    Role::Documenter,
];

/// One role instruction read from a commit tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleFile {
    pub role: Role,
    /// Repository-relative path, e.g. `.iyagi/roles/reviewer.md`.
    pub path: String,
    /// Git blob object id of the file at the commit.
    pub blob_oid: String,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RoleFileError {
    /// The entry exists but violates 10 §2; the mission start is rejected.
    #[error("{path} {reason}")]
    Invalid { path: String, reason: String },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// File stem for a role: the `Role` wire value (`test_author`).
pub fn role_file_stem(role: Role) -> &'static str {
    match role {
        Role::Lead => "lead",
        Role::Researcher => "researcher",
        Role::Architect => "architect",
        Role::Builder => "builder",
        Role::TestAuthor => "test_author",
        Role::Reviewer => "reviewer",
        Role::Specialist => "specialist",
        Role::Diagnostician => "diagnostician",
        Role::Integrator => "integrator",
        Role::Documenter => "documenter",
    }
}

/// Repository-relative instruction path for a role.
pub fn role_file_path(role: Role) -> String {
    format!("{ROLE_INSTRUCTION_DIR}/{}.md", role_file_stem(role))
}

struct TreeEntry {
    mode: String,
    kind: String,
    oid: String,
    size: Option<usize>,
    path: Vec<u8>,
}

/// Leaf entries under the role directory at `commit`. A missing directory,
/// or a symlinked `.iyagi`, lists nothing because Git never follows links in
/// a tree. Non-UTF-8 names are kept as bytes and simply never match a role.
fn list_role_entries(repo: &Path, commit: &str) -> Result<Vec<TreeEntry>, GitError> {
    validate_oid(commit)?;
    let listing = run_git_bytes(
        repo,
        &[
            "ls-tree",
            "-r",
            "-z",
            "-l",
            "--full-tree",
            commit,
            "--",
            ROLE_INSTRUCTION_DIR,
        ],
    )?;
    let malformed = || GitError::Git {
        args: vec!["ls-tree".into()],
        stderr: "unexpected ls-tree output".into(),
    };
    let mut entries = Vec::new();
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(malformed)?;
        let meta = std::str::from_utf8(&record[..tab]).map_err(|_| malformed())?;
        let mut fields = meta.split_ascii_whitespace();
        let (Some(mode), Some(kind), Some(oid), Some(size), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(malformed());
        };
        entries.push(TreeEntry {
            mode: mode.to_owned(),
            kind: kind.to_owned(),
            oid: oid.to_owned(),
            size: size.parse().ok(),
            path: record[tab + 1..].to_vec(),
        });
    }
    Ok(entries)
}

/// Read the instruction files for `roles` at `commit` (10 §2), in
/// [`ROLE_ORDER`]. Roles without a file, or whose file is whitespace only,
/// are omitted. An existing entry that is not a regular blob (symlink,
/// submodule), exceeds `max_bytes`, or is not UTF-8 is an error.
pub fn read_role_files(
    repo: &Path,
    commit: &str,
    roles: &[Role],
    max_bytes: usize,
) -> Result<Vec<RoleFile>, RoleFileError> {
    let entries = list_role_entries(repo, commit)?;
    let mut files = Vec::new();
    for role in ROLE_ORDER.into_iter().filter(|role| roles.contains(role)) {
        let path = role_file_path(role);
        let Some(entry) = entries.iter().find(|entry| entry.path == path.as_bytes()) else {
            continue;
        };
        let invalid = |reason: String| RoleFileError::Invalid {
            path: path.clone(),
            reason,
        };
        if entry.kind != "blob" || !matches!(entry.mode.as_str(), "100644" | "100755") {
            return Err(invalid(
                "must be a regular file; symlinks and submodules are not allowed".into(),
            ));
        }
        let Some(size) = entry.size else {
            return Err(invalid("has no readable object size".into()));
        };
        if size > max_bytes {
            return Err(invalid(format!(
                "is {size} bytes; the limit is {max_bytes} bytes"
            )));
        }
        validate_oid(&entry.oid)?;
        let bytes = run_git_bytes(repo, &["cat-file", "blob", &entry.oid])?;
        if bytes.len() != size {
            return Err(invalid("does not match its recorded object size".into()));
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return Err(invalid("is not valid UTF-8".into()));
        };
        if text.trim().is_empty() {
            continue;
        }
        files.push(RoleFile {
            role,
            blob_oid: entry.oid.clone(),
            text,
            path,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::git::run_git_for_test as git;

    const MAX: usize = 64;

    fn repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        repo
    }

    fn write_role(repo: &Path, name: &str, body: &[u8]) {
        let dir = repo.join(ROLE_INSTRUCTION_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn commit(repo: &Path) -> String {
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "roles"]);
        git(repo, &["rev-parse", "HEAD"])
    }

    fn invalid_reason(result: Result<Vec<RoleFile>, RoleFileError>) -> (String, String) {
        match result {
            Err(RoleFileError::Invalid { path, reason }) => (path, reason),
            other => panic!("expected an invalid role file, got {other:?}"),
        }
    }

    #[test]
    fn stems_are_the_role_wire_values() {
        for role in ROLE_ORDER {
            assert_eq!(
                serde_json::to_value(role).unwrap(),
                serde_json::json!(role_file_stem(role))
            );
        }
    }

    #[test]
    fn reads_requested_roles_from_the_commit_in_role_order() {
        let repo = repository();
        write_role(repo.path(), "reviewer.md", b"Check tests first.\n");
        write_role(repo.path(), "lead.md", "계획은 작게 나눈다.\n".as_bytes());
        write_role(repo.path(), "builder.md", b" \n\t\n");
        write_role(repo.path(), "documenter.md", b"Not requested.\n");
        write_role(repo.path(), "README.md", b"Ignored.\n");
        write_role(repo.path(), "reviwer.md", b"Typo is ignored.\n");
        let base = commit(repo.path());
        // Working-tree edits after the commit are never read.
        write_role(repo.path(), "reviewer.md", b"Uncommitted.\n");
        std::fs::remove_file(repo.path().join(ROLE_INSTRUCTION_DIR).join("lead.md")).unwrap();

        let files = read_role_files(
            repo.path(),
            &base,
            &[Role::Reviewer, Role::Builder, Role::Lead, Role::Architect],
            MAX,
        )
        .unwrap();

        assert_eq!(
            files.iter().map(|file| file.role).collect::<Vec<_>>(),
            vec![Role::Lead, Role::Reviewer]
        );
        assert_eq!(files[0].path, ".iyagi/roles/lead.md");
        assert_eq!(files[0].text, "계획은 작게 나눈다.\n");
        assert_eq!(files[1].text, "Check tests first.\n");
        assert_eq!(
            files[1].blob_oid,
            git(
                repo.path(),
                &["rev-parse", &format!("{base}:.iyagi/roles/reviewer.md")]
            )
        );
    }

    #[test]
    fn missing_directory_reads_nothing() {
        let repo = repository();
        let base = commit(repo.path());
        assert!(read_role_files(repo.path(), &base, &ROLE_ORDER, MAX)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn oversized_and_non_utf8_files_are_rejected() {
        let repo = repository();
        write_role(repo.path(), "reviewer.md", &[b'x'; MAX + 1]);
        let base = commit(repo.path());
        let (path, reason) =
            invalid_reason(read_role_files(repo.path(), &base, &[Role::Reviewer], MAX));
        assert_eq!(path, ".iyagi/roles/reviewer.md");
        assert!(reason.contains("limit"), "{reason}");
        // The same entry is fine when the role is not requested.
        assert!(read_role_files(repo.path(), &base, &[Role::Lead], MAX)
            .unwrap()
            .is_empty());

        write_role(repo.path(), "reviewer.md", &[0xff, 0xfe, b'\n']);
        let base = commit(repo.path());
        let (_, reason) =
            invalid_reason(read_role_files(repo.path(), &base, &[Role::Reviewer], MAX));
        assert!(reason.contains("UTF-8"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_role_files_are_rejected() {
        let repo = repository();
        write_role(repo.path(), "notes.md", b"Real text.\n");
        std::os::unix::fs::symlink(
            "notes.md",
            repo.path().join(ROLE_INSTRUCTION_DIR).join("builder.md"),
        )
        .unwrap();
        let base = commit(repo.path());
        let (path, reason) =
            invalid_reason(read_role_files(repo.path(), &base, &[Role::Builder], MAX));
        assert_eq!(path, ".iyagi/roles/builder.md");
        assert!(reason.contains("regular file"), "{reason}");
    }

    #[test]
    fn submodule_entries_are_rejected() {
        let repo = repository();
        let base = commit(repo.path());
        git(
            repo.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{base},.iyagi/roles/integrator.md"),
            ],
        );
        git(repo.path(), &["commit", "-qm", "gitlink"]);
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let (path, reason) = invalid_reason(read_role_files(
            repo.path(),
            &head,
            &[Role::Integrator],
            MAX,
        ));
        assert_eq!(path, ".iyagi/roles/integrator.md");
        assert!(reason.contains("regular file"), "{reason}");
    }

    #[test]
    fn abbreviated_commits_are_refused() {
        let repo = repository();
        let base = commit(repo.path());
        assert!(matches!(
            read_role_files(repo.path(), &base[..12], &ROLE_ORDER, MAX),
            Err(RoleFileError::Git(_))
        ));
    }
}
