//! Frozen verifier environment and OS permissions. Output is separate from input.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use term_contracts::mission::{MissionErrorCode, MissionRpcError};

const PROFILE: &str = r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)
(allow file-read*)
(allow file-write* (subpath (param "OUTPUT")))
(allow file-write-data (literal "/dev/null"))
(allow mach-lookup (global-name "com.apple.system.opendirectoryd.libinfo"))
"#;

/// Suffix marking the output sibling of a verification workspace.
const OUTPUT_SUFFIX: &str = ".verification-output";
/// Layout markers of the daemon data tree that candidate code must not read
/// (paths.rs: `<data>/data/missions/<mission>/workspaces/<id>`).
const MISSIONS_SEGMENT: &str = "missions";
const DATA_SEGMENT: &str = "data";
/// HOME credential stores the verifier must not read.
const CREDENTIAL_DIRS: [&str; 6] = [
    ".ssh",
    ".aws",
    ".gcloud",
    ".gnupg",
    ".kube",
    ".config/gcloud",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Isolation {
    pub backend: String,
    pub output: String,
    pub env: BTreeMap<String, String>,
    pub allow_network: bool,
    /// HOME credential read denies resolved once by `prepare` and replayed by
    /// `launch`, so the frozen argv compared at release and restart recovery
    /// does not depend on the daemon's HOME or on which credential dirs exist
    /// at that moment. `None` marks a launch frozen before they were recorded.
    #[serde(default)]
    pub credential_read_denies: Option<Vec<String>>,
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::Command;

    fn python() -> String {
        let output = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    #[test]
    fn native_verifier_denies_input_host_links_network_and_child_escape() {
        let dir = tempfile::tempdir().unwrap();
        // Mirror the daemon layout so the appended read denies cover the
        // whole data tree, not just this workspace's ancestors. The script
        // also walks every ancestor like realpath (Node, git) must, while
        // listing those ancestors or statting a sibling mission stays denied.
        let base = dir.path().canonicalize().unwrap();
        let root = base
            .join("data")
            .join("missions")
            .join("mission")
            .join("workspaces")
            .join("candidate");
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("input");
        std::fs::write(&input, b"unchanged").unwrap();
        let host = root.parent().unwrap().join("host");
        std::fs::write(&host, b"host unchanged").unwrap();
        let token = base.join("runtime").join("token");
        std::fs::create_dir_all(token.parent().unwrap()).unwrap();
        std::fs::write(&token, b"control token").unwrap();
        let other = base
            .join("data")
            .join("missions")
            .join("other")
            .join("workspaces")
            .join("ws")
            .join("file");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"other mission").unwrap();
        let isolation = Isolation::prepare(&root, false).unwrap();
        let script = r#"import os,pathlib,socket,subprocess,sys
root=pathlib.Path.cwd();out=pathlib.Path(os.environ['IYAGI_VERIFICATION_OUTPUT']);host=pathlib.Path(sys.argv[1])
assert root.joinpath('input').read_bytes()==b'unchanged'
def denied(fn):
 try:fn()
 except PermissionError:return
 raise AssertionError('operation escaped verifier policy')
denied(lambda:root.joinpath('input').write_text('changed'))
denied(lambda:root.joinpath('new').write_text('new'))
denied(lambda:host.write_text('changed'))
denied(lambda:root.joinpath('input').unlink())
denied(lambda:root.joinpath('input').rename(out/'moved'))
denied(lambda:os.link(root/'input',out/'hardlink'))
denied(lambda:pathlib.Path(sys.argv[3]).read_bytes())
denied(lambda:pathlib.Path(sys.argv[4]).read_bytes())
for p in [root,out]:
 for a in [p,*p.parents]:os.lstat(a)
 if sys.version_info>=(3,10):assert os.path.realpath(p,strict=True)==str(p)
 if os.path.exists('/bin/realpath'):assert subprocess.run(['/bin/realpath',p],capture_output=True,text=True).stdout.strip()==str(p)
denied(lambda:os.listdir(root.parent))
denied(lambda:os.lstat(root.parents[2]/'other'))
os.symlink(root/'input',out/'symlink')
denied(lambda:(out/'symlink').write_text('changed'))
out.joinpath('allowed').write_text('output')
for family,kind,target in [(socket.AF_INET,socket.SOCK_STREAM,('127.0.0.1',9)),(socket.AF_INET,socket.SOCK_DGRAM,('127.0.0.1',9)),(socket.AF_UNIX,socket.SOCK_STREAM,sys.argv[2])]:
 s=socket.socket(family,kind)
 denied(lambda:s.connect(target) if kind==socket.SOCK_STREAM else s.sendto(b'x',target))
 s.close()
child=subprocess.run(['/bin/sh','-c','printf child > "$IYAGI_VERIFICATION_OUTPUT/child"; printf bad > input'],capture_output=True)
assert child.returncode!=0 and out.joinpath('child').read_text()=='child'
assert os.environ['HOME']==str(out) and os.environ['TMPDIR']==str(out)
assert all(k not in os.environ for k in ['ANTHROPIC_API_KEY','OPENAI_API_KEY','SSH_AUTH_SOCK','NODE_OPTIONS','PYTHONPATH','BASH_ENV'])
print('verified boundaries')
"#;
        // $TMPDIR 아래 깊은 경로는 SUN_LEN(macOS 104바이트)을 넘는다 — 소켓은
        // /tmp 아래 짧은 경로에 둔다. 위치와 무관하게 네트워크는 기본 거부다.
        let socket_dir = tempfile::Builder::new()
            .prefix("iyagi-vi-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = socket_dir.path().join("owned.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let args = vec![
            "-c".into(),
            script.into(),
            host.to_string_lossy().into_owned(),
            socket.to_string_lossy().into_owned(),
            token.to_string_lossy().into_owned(),
            other.to_string_lossy().into_owned(),
        ];
        let (program, argv) = isolation.launch(&python(), &args);
        let output = Command::new(program)
            .args(argv)
            .current_dir(&root)
            .env_clear()
            .envs(&isolation.env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(input).unwrap(), b"unchanged");
        assert_eq!(std::fs::read(host).unwrap(), b"host unchanged");
    }

    #[test]
    fn explicitly_permitted_network_connects_to_an_owned_listener() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("candidate");
        std::fs::create_dir(&root).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let isolation = Isolation::prepare(&root, true).unwrap();
        let args = vec![
            "-c".into(),
            format!(
                "import socket; s=socket.create_connection(('127.0.0.1',{}),timeout=2); s.close()",
                listener.local_addr().unwrap().port()
            ),
        ];
        let (program, argv) = isolation.launch(&python(), &args);
        let output = Command::new(program)
            .args(argv)
            .current_dir(&root)
            .env_clear()
            .envs(&isolation.env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn output_is_not_adopted_and_environment_changes_change_the_frozen_launch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("candidate");
        std::fs::create_dir(&root).unwrap();
        let first = Isolation::prepare(&root, false).unwrap();
        assert!(Isolation::prepare(&root, false).is_err());
        assert!(first.valid(&root, false));
        let mut changed = first.clone();
        changed
            .env
            .insert("PATH".into(), "/different/toolchain".into());
        assert!(changed.valid(&root, false));
        assert_ne!(
            first.launch("/bin/true", &[]),
            changed.launch("/bin/true", &[])
        );
        changed
            .env
            .insert("OPENAI_API_KEY".into(), "must-not-inherit".into());
        assert!(!changed.valid(&root, false));
        changed = first.clone();
        changed.output = root.to_string_lossy().into_owned();
        assert!(!changed.valid(&root, false));
    }

    #[test]
    fn launch_replays_recorded_credential_denies_instead_of_the_live_home() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("candidate");
        std::fs::create_dir(&root).unwrap();
        let first = Isolation::prepare(&root, false).unwrap();
        assert_eq!(
            first.credential_read_denies,
            Some(resolve_credential_read_denies())
        );
        // Recovery rebuilds the argv from the stored contract; a restart under
        // another HOME, or a credential dir appearing, must not change it.
        let stored = serde_json::to_value(&first).unwrap();
        let replayed: Isolation = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(
            first.launch("/bin/true", &[]),
            replayed.launch("/bin/true", &[])
        );
        let mut moved = first.clone();
        moved.credential_read_denies = Some(vec!["/frozen/home/.ssh".into()]);
        assert!(moved.valid(&root, false));
        let (_, argv) = moved.launch("/bin/true", &[]);
        assert!(argv[1].contains("(deny file-read* (subpath \"/frozen/home/.ssh\"))\n"));
        for live in resolve_credential_read_denies() {
            assert!(!argv[1].contains(&format!("(subpath \"{}\")", sbpl_escape(&live))));
        }
        assert!(argv[1].contains("(allow file-read-metadata (literal "));
        moved.credential_read_denies = Some(vec!["relative/.ssh".into()]);
        assert!(!moved.valid(&root, false));
        // A launch frozen before the denies were recorded keeps the exact
        // profile it was spawned with, so its manifest still matches.
        let mut legacy = stored;
        legacy
            .as_object_mut()
            .unwrap()
            .remove("credential_read_denies");
        let legacy: Isolation = serde_json::from_value(legacy).unwrap();
        assert!(legacy.credential_read_denies.is_none() && legacy.valid(&root, false));
        let (_, argv) = legacy.launch("/bin/true", &[]);
        assert!(!argv[1].contains("file-read-metadata"));
        for live in resolve_credential_read_denies() {
            assert!(argv[1].contains(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                sbpl_escape(&live)
            )));
        }
    }

    #[test]
    fn profile_denies_daemon_tree_and_credentials_but_reallows_workspace_reads() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base
            .join("data")
            .join("missions")
            .join("mission")
            .join("workspaces")
            .join("candidate");
        std::fs::create_dir_all(&root).unwrap();
        let isolation = Isolation::prepare(&root, false).unwrap();
        let (_, argv) = isolation.launch("/bin/true", &[]);
        let profile = &argv[1];
        // Seatbelt applies the last matching filter: the daemon-tree deny
        // must follow the broad read allow, and the workspace/output
        // re-allows must follow that deny.
        let broad = profile.find("(allow file-read*)\n").unwrap();
        let deny = profile
            .find(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                base.display()
            ))
            .unwrap();
        let output = profile
            .find("(allow file-read* (subpath (param \"OUTPUT\")))\n")
            .unwrap();
        let workspace = profile
            .find(&format!(
                "(allow file-read* (subpath \"{}\"))\n",
                root.display()
            ))
            .unwrap();
        assert!(broad < deny);
        assert!(deny < output && deny < workspace);
        // Realpath walks need metadata on each denied ancestor of the
        // re-allowed trees, as exact literals only, after the deny.
        let workspaces = root.parent().unwrap();
        for ancestor in workspaces.ancestors().take_while(|a| a.starts_with(&base)) {
            let rule = format!(
                "(allow file-read-metadata (literal \"{}\"))\n",
                ancestor.display()
            );
            assert!(profile.find(&rule).unwrap() > deny);
        }
        for outside in [base.parent().unwrap(), root.as_path()] {
            assert!(!profile.contains(&format!("(literal \"{}\")", outside.display())));
        }
        assert!(!profile.contains("(allow file-read-metadata (subpath"));
        assert_eq!(profile.matches("(allow file-read-metadata").count(), 5);
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for name in CREDENTIAL_DIRS {
                let credential =
                    std::fs::canonicalize(home.join(name)).unwrap_or_else(|_| home.join(name));
                assert!(profile.contains(&format!(
                    "(deny file-read* (subpath \"{}\"))\n",
                    credential.display()
                )));
            }
        }
    }
}

fn denied(message: &str) -> MissionRpcError {
    MissionRpcError::new(MissionErrorCode::PolicyDenied, message)
}

/// Escape a path for interpolation into an SBPL double-quoted literal.
fn sbpl_escape(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Resolve the HOME credential read denies. Subpath filters match canonical
/// paths only, so existing directories are canonicalized (resolving symlinked
/// dotfile stores); absent paths keep their literal spelling so the deny still
/// binds once they appear. This reads the daemon's HOME and filesystem, so
/// `Isolation::prepare` records the result instead of every `launch` call
/// recomputing it.
fn resolve_credential_read_denies() -> Vec<String> {
    let mut denies: Vec<String> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        let home = PathBuf::from(home);
        for name in CREDENTIAL_DIRS {
            let path = std::fs::canonicalize(home.join(name)).unwrap_or_else(|_| home.join(name));
            if let Some(literal) = path.to_str() {
                if !denies.iter().any(|deny| deny == literal) {
                    denies.push(literal.to_owned());
                }
            }
        }
    }
    denies
}

/// Read confinement appended to the profile (04 §5 isolation). The broad
/// `file-read*` allow stays because real toolchains read HOME and system
/// trees too widely to enumerate; these rules carve the high-value state
/// back out. Seatbelt applies the last matching filter, so the denies are
/// emitted after the broad allow and the workspace/output re-allows after
/// the denies. Residual risk, accepted: user files outside this set
/// (browser profiles, ~/.netrc, other dotfiles) remain readable and could
/// still reach the verification log or, with allowed_network, the network.
///
/// `credentials` are the denies recorded at prepare time. `None` rebuilds a
/// launch frozen before they were recorded exactly as it was spawned (live
/// credential resolution, no ancestor metadata re-allows), so an in-flight
/// verification still matches its manifest after a daemon upgrade.
fn read_confinement_rules(output: &str, credentials: Option<&[String]>) -> String {
    let output_path = Path::new(output);
    let mut denies: Vec<String> = Vec::new();
    let mut rules = String::new();
    // The workspace is minted at `<data>/data/missions/<mission>/workspaces/
    // <id>` and the output is its sibling (pipeline::prepare_verification,
    // paths.rs), so the output's ancestors locate the daemon-owned tree.
    if let Some(workspaces) = output_path.parent() {
        if let Some(missions) = workspaces.parent().and_then(Path::parent) {
            // When the minted structure holds, deny the whole data tree
            // (runtime token, iyagi.db, journals, config, other missions'
            // workspaces and artifacts); otherwise fall back to the closest
            // daemon-owned ancestor.
            let daemon_data = missions
                .parent()
                .filter(|data| {
                    data.file_name().and_then(|name| name.to_str()) == Some(DATA_SEGMENT)
                        && missions.file_name().and_then(|name| name.to_str())
                            == Some(MISSIONS_SEGMENT)
                })
                .and_then(Path::parent);
            if let Some(literal) = daemon_data.unwrap_or(missions).to_str() {
                denies.push(literal.to_owned());
            }
        }
    }
    let recorded = credentials.is_some();
    let credentials = match credentials {
        Some(credentials) => credentials.to_vec(),
        None => resolve_credential_read_denies(),
    };
    for credential in credentials {
        if !denies.contains(&credential) {
            denies.push(credential);
        }
    }
    for deny in &denies {
        rules.push_str(&format!(
            "(deny file-read* (subpath \"{}\"))\n",
            sbpl_escape(deny)
        ));
    }
    rules.push_str("(allow file-read* (subpath (param \"OUTPUT\")))\n");
    // `valid` pins the output to the workspace sibling, so this rebuilds
    // the canonical workspace path without more state.
    let workspace = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(OUTPUT_SUFFIX))
        .map(|name| output_path.with_file_name(name));
    if let Some(workspace) = workspace.as_deref().and_then(Path::to_str) {
        rules.push_str(&format!(
            "(allow file-read* (subpath \"{}\"))\n",
            sbpl_escape(workspace)
        ));
    }
    if recorded {
        // `file-read*` includes `file-read-metadata`, so a deny above the
        // re-allowed workspace/output also fails lstat/getattrlist on every
        // ancestor down to them, and realpath walks (Node module resolution,
        // git's strbuf_realpath, tools canonicalizing cwd, HOME or TMPDIR)
        // die with EPERM. Re-allow metadata only, only on those exact
        // directories (`literal`, never `subpath`): sibling missions, other
        // workspaces and runtime files stay unreadable, and the ancestors
        // themselves still cannot be listed.
        let mut ancestors: Vec<&Path> = Vec::new();
        let allowed_trees = [Some(output_path), workspace.as_deref()];
        for allowed in allowed_trees.into_iter().flatten() {
            for ancestor in allowed.ancestors().skip(1) {
                if !ancestors.contains(&ancestor)
                    && denies.iter().any(|deny| ancestor.starts_with(deny))
                {
                    ancestors.push(ancestor);
                }
            }
        }
        for ancestor in ancestors.iter().filter_map(|ancestor| ancestor.to_str()) {
            rules.push_str(&format!(
                "(allow file-read-metadata (literal \"{}\"))\n",
                sbpl_escape(ancestor)
            ));
        }
    }
    rules
}

impl Isolation {
    pub fn require_supported() -> Result<(), MissionRpcError> {
        if !cfg!(target_os = "macos") || !Path::new("/usr/bin/sandbox-exec").is_file() {
            return Err(MissionRpcError::with_details(
                MissionErrorCode::CapabilityUnsupported,
                "this OS has no verified isolated verification executor",
                term_contracts::mission::MissionErrorDetails {
                    reason_code: Some("verification_unsupported_os".into()),
                    ..Default::default()
                },
            ));
        }
        Ok(())
    }

    pub fn prepare(root: &Path, allow_network: bool) -> Result<Self, MissionRpcError> {
        Self::require_supported()?;
        let output = Self::output_path(root)?;
        // A retry gets a new workspace. Never adopt a pre-existing path or link.
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&output).map_err(|_| {
            denied("verification output directory already exists or cannot be created")
        })?;
        let mut paths = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|p| p.is_absolute())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            paths = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
                .into_iter()
                .map(PathBuf::from)
                .collect();
        }
        let path = std::env::join_paths(paths)
            .map_err(|_| denied("verification PATH is invalid"))?
            .to_str()
            .ok_or_else(|| denied("verification PATH is not UTF-8"))?
            .to_owned();
        let output = output
            .to_str()
            .ok_or_else(|| denied("verification output path is not UTF-8"))?
            .to_owned();
        let mut env = Self::environment(&output);
        env.insert("PATH".into(), path);
        Ok(Self {
            backend: "macos_seatbelt_v1".into(),
            output,
            env,
            allow_network,
            credential_read_denies: Some(resolve_credential_read_denies()),
        })
    }

    pub fn output_path(root: &Path) -> Result<PathBuf, MissionRpcError> {
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| denied("invalid verification workspace"))?;
        Ok(root.with_file_name(format!("{name}{OUTPUT_SUFFIX}")))
    }

    fn environment(output: &str) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        for name in [
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "IYAGI_VERIFICATION_OUTPUT",
        ] {
            env.insert(name.into(), output.into());
        }
        env.insert("CARGO_TARGET_DIR".into(), format!("{output}/target"));
        env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        env.insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
        env.insert("LANG".into(), "en_US.UTF-8".into());
        env
    }

    pub fn valid(&self, root: &Path, network: bool) -> bool {
        let mut expected = Self::environment(&self.output);
        let Some(path) = self.env.get("PATH") else {
            return false;
        };
        if std::env::split_paths(path).any(|p| !p.is_absolute()) {
            return false;
        }
        expected.insert("PATH".into(), path.clone());
        self.backend == "macos_seatbelt_v1"
            && self.allow_network == network
            && Self::output_path(root).ok().as_deref() == Some(Path::new(&self.output))
            && self.env == expected
            && self
                .credential_read_denies
                .iter()
                .flatten()
                .all(|deny| Path::new(deny).is_absolute())
    }

    pub fn launch(&self, program: &str, argv: &[String]) -> (PathBuf, Vec<String>) {
        let mut profile = PROFILE.to_owned();
        // Exec manifests deliberately omit environment values. Bind this
        // non-secret environment to the immutable launch argv for recovery.
        profile.push_str(&format!(
            "; environment-sha256:{:x}\n",
            Sha256::digest(serde_json::to_vec(&self.env).expect("verifier environment"))
        ));
        profile.push_str(&read_confinement_rules(
            &self.output,
            self.credential_read_denies.as_deref(),
        ));
        if self.allow_network {
            profile.push_str("(allow network*)\n(allow mach-lookup (global-name \"com.apple.SystemConfiguration.DNSConfiguration\") (global-name \"com.apple.SystemConfiguration.configd\") (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.trustd.agent\"))\n");
        }
        let mut args = vec![
            "-p".into(),
            profile,
            "-D".into(),
            format!("OUTPUT={}", self.output),
            program.into(),
        ];
        args.extend_from_slice(argv);
        (PathBuf::from("/usr/bin/sandbox-exec"), args)
    }
}
