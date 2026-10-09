//! Launch the independent macOS observer before the target's waiting helper.
use super::gated::GateConfig;
use std::{
    io,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use term_contracts::ids::WorkloadId;
use term_platform::GroupHandle;

struct PendingChild(Child);
impl Drop for PendingChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn launch(config: &GateConfig, workload: &WorkloadId) -> io::Result<GroupHandle> {
    // A new 0700 directory, using the same short suffix as the launch gate.
    // Failed spawn, handshake, or reaper creation removes only this directory.
    let directory = tempfile::Builder::new()
        .prefix("o")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&config.directory)?;
    let endpoint = directory.path().join("g");
    let endpoint = endpoint
        .to_str()
        .ok_or_else(|| io::Error::other("invalid guardian path"))?;
    let mut child = PendingChild(
        Command::new(&config.helper_program)
            .args(["--exec-guardian", endpoint, workload.as_str()])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?,
    );
    let deadline = Instant::now() + config.timeout;
    let group = loop {
        if let Some(identity) = term_platform::identity::process_identity(child.0.id()) {
            match term_platform::group::macos_guardian::connect_group(workload, endpoint, &identity)
            {
                Ok(group) => break group,
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => {}
            }
        }
        if let Some(status) = child.0.try_wait()? {
            return Err(io::Error::other(format!(
                "observer guardian exited before handshake ({status})"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "observer guardian startup deadline",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A daemon crash skips these destructors, preserving the guardian and
    // endpoint. The guardian itself removes its socket/private directory after
    // the recovered acknowledgement; launchd then reaps the orphan observer.
    std::thread::Builder::new()
        .name("observer-reaper".into())
        .spawn(move || {
            let _ = child.0.wait();
            drop(directory);
        })?;
    Ok(group)
}
