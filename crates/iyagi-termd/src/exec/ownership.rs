//! OS ownership signals for Exec children: identity capture and the
//! cancellation ladder (docs/orchestration/02-engine.md §9, ticket O07).
//!
//! Ladder (02 §9): interrupt → `grace_interrupt` → terminate →
//! `grace_kill` → kill → **always** wait for the reap. The lease/reservation
//! is released only after [`ConfirmedExit`] (never before confirmed
//! termination).
//!
//! Platform notes:
//! * Unix: interrupt = SIGINT, terminate = SIGTERM, kill = SIGKILL.
//! * Windows: piped children share no console, so CTRL_BREAK delivery is
//!   not reliable — the interrupt step is a documented no-op and the ladder
//!   falls through to `taskkill /T /F` (tree, force) as the terminate/force
//!   step, exactly like the R1 stop path (02-runner §7: "Windows R1 is
//!   force-only").
//! * Signaling happens only while the supervisor still holds the unreaped
//!   child (`try_wait` said alive immediately before), so a recycled PID of
//!   an unrelated process is never signaled.

use std::time::Duration;

use term_contracts::ids::ProcessIdentity;

/// Poll cadence of the ladder's grace windows.
pub(crate) const LADDER_POLL: Duration = Duration::from_millis(15);

/// Result of a fully confirmed stop: the child was reaped before this
/// value existed (02 §9: no lease release before confirmed termination).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmedExit {
    /// The child exited on its own (or honored the interrupt) before the
    /// force step ran. `code` is `None` when the OS reports no code
    /// (signal-terminated on Unix).
    Exited { code: Option<i32> },
    /// The terminate/force step had to run. `elapsed_ms` covers the whole
    /// ladder from stop() entry to the confirmed reap.
    Killed { elapsed_ms: u128 },
}

// PID + start token + boot id for a live child (`term_platform::identity`
// probes; `None` means unverifiable — never treated as a match).
pub(crate) fn capture_identity(pid: u32) -> Option<ProcessIdentity> {
    term_platform::identity::process_identity(pid)
}

/// Graceful interrupt signal. Windows piped children: documented no-op
/// (returns `false`); the force path is what actually stops them.
pub(crate) fn send_interrupt(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SIGINT = 2. `kill` comes from the always-linked platform libc.
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid as i32, 2) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Terminate step: SIGTERM on Unix; `taskkill /PID <pid> /T /F` (tree force)
/// on Windows — R1 has no universal graceful CLI signal there.
pub(crate) fn terminate_tree(pid: u32) -> bool {
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid as i32, 15) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Not a shell string: a fixed argv against a system binary.
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Final kill step fallback: the direct child gets `TerminateProcess` /
/// SIGKILL through the tokio handle's `start_kill` (see `process.rs`).
pub(crate) fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid as i32, 9) == 0 }
    }
    #[cfg(not(unix))]
    {
        terminate_tree(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_a_live_own_child_carries_pid_and_token() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "10", "127.0.0.1"])
                .spawn()
                .expect("spawn ping")
        } else {
            std::process::Command::new("sleep")
                .arg("10")
                .spawn()
                .expect("spawn sleep")
        };
        let identity = capture_identity(child.id()).expect("live child identity");
        assert_eq!(identity.pid, child.id());
        assert!(!identity.start_token.is_empty());
        assert!(!identity.boot_id.is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn interrupt_on_windows_is_a_documented_no_op() {
        // Never signals a PID we do not own; only the shape is asserted.
        if cfg!(windows) {
            assert!(!send_interrupt(0));
        }
    }
}
