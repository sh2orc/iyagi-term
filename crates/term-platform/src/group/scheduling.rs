//! Per-process scheduling tier (spec `08-pressure-relief.md` §2 양보).
//!
//! Two callers share this entry point:
//! * the group backends, for a managed workload's verified members, and
//! * the daemon, for **direct shells** — they have no OS group at all
//!   (02-runner §3), so the observed tree from `08 §1.2` is the membership.
//!
//! Discipline (01-contracts §1 pid-reuse defence): every identity is
//! re-verified with [`crate::identity::process_identity`] immediately before
//! the OS call. A pid that now belongs to a different process is **skipped
//! and counted**, never signalled. Nothing outside the supplied membership is
//! ever touched.
//!
//! Reversibility (08 §0-4): a platform that cannot put a process back on its
//! original tier returns `Unsupported` here and reports the capability
//! `scheduling_yield: unsupported` — the relief policy then applies nothing.

use std::io;

use term_contracts::ids::ProcessIdentity;

use super::{SchedulingOutcome, SchedulingTier};

/// Apply `tier` to every identity that still is the recorded process.
///
/// macOS uses `setpriority(PRIO_DARWIN_PROCESS, pid, PRIO_DARWIN_BG)` — the
/// `taskpolicy -b` background policy. Descendants created afterwards inherit
/// it, so the restore pass must re-walk the *current* membership, not the
/// membership recorded when the yield was applied.
///
/// Windows uses `SetPriorityClass(BELOW_NORMAL_PRIORITY_CLASS)` per verified
/// member. Job CPU rate control is deliberately untouched: weight-based and
/// hard-cap rate control are mutually exclusive flags, so writing a weight
/// would clobber a user-configured `cpu_max_cores` hard cap (03 §6).
///
/// Linux is unsupported here: an unprivileged daemon can raise `nice` but
/// cannot lower it again under the default `RLIMIT_NICE` (08 §2 table). The
/// reversible Linux path is the delegated cgroup's `cpu.weight`, which is a
/// group operation, not a per-pid one.
pub fn set_process_scheduling(
    identities: &[ProcessIdentity],
    tier: SchedulingTier,
) -> io::Result<SchedulingOutcome> {
    imp::set_process_scheduling(identities, tier)
}

/// What tier the OS reports for a live pid *from another process*, when that
/// is observable at all (`None` otherwise). This is the read-back that
/// [`set_process_scheduling`] verifies each member with, and the same probe
/// acceptance tests use to prove the yield really reached the OS.
pub fn observed_tier(pid: u32) -> Option<SchedulingTier> {
    imp::observed_tier(pid)
}

/// Suspend (`SIGSTOP`) or resume (`SIGCONT`) every identity that still is the
/// recorded process — the resource guard's reversible stop (08 §5). The same
/// per-member identity discipline as [`set_process_scheduling`]: a reused pid
/// is skipped and counted, never signalled.
///
/// SIGSTOP/SIGCONT cannot be caught, blocked or ignored, so a successful
/// `kill` is its own read-back; the membership walk is refreshed before every
/// pass so descendants that appeared meanwhile are caught on the next tick.
/// A resumed member continues on its original scheduling tier — the guard
/// composes with relief's yield rather than replacing it.
pub fn set_process_suspend(
    identities: &[ProcessIdentity],
    suspend: bool,
) -> io::Result<SchedulingOutcome> {
    suspend_imp::apply(identities, suspend)
}

/// Whether [`set_process_scheduling`] can do anything at all on this build.
/// Callers use it to skip the (not free) membership walk on platforms where
/// the per-pid path is unsupported — Linux yields through the delegated
/// cgroup's `cpu.weight`, never per pid.
pub const fn per_process_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

mod suspend_imp {
    use super::*;

    /// Unix per-pid suspend/resume. Linux included: unlike `nice`, SIGSTOP
    /// needs no privilege to reverse, so the per-pid path is safe even
    /// without a delegated cgroup (the cgroup `freeze` path is preferred
    /// when a group exists).
    #[cfg(unix)]
    pub(super) fn apply(
        identities: &[ProcessIdentity],
        suspend: bool,
    ) -> io::Result<SchedulingOutcome> {
        let sig = if suspend {
            libc::SIGSTOP
        } else {
            libc::SIGCONT
        };
        let mut outcome = SchedulingOutcome::default();
        for identity in identities {
            if !still_the_same_process(identity) {
                outcome.skipped_reused += 1;
                continue;
            }
            let rc = unsafe { libc::kill(identity.pid as libc::pid_t, sig) };
            if rc == 0 {
                outcome.applied += 1;
            } else {
                count_failure_unless_vanished(identity, &mut outcome);
            }
        }
        Ok(outcome)
    }

    #[cfg(windows)]
    pub(super) fn apply(
        _identities: &[ProcessIdentity],
        _suspend: bool,
    ) -> io::Result<SchedulingOutcome> {
        // Job-object suspension is deliberately not implemented in R1; the
        // capability is reported unsupported and the guard stays dormant.
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "per-process suspend/resume unavailable on this backend",
        ))
    }
}

/// Re-verify one recorded identity. `true` = still the same process.
#[cfg(any(unix, windows))]
fn still_the_same_process(recorded: &ProcessIdentity) -> bool {
    crate::identity::process_identity(recorded.pid).is_some_and(|live| live.same_process(recorded))
}

/// A member that exited between the verification and the OS call is **not** a
/// failure: it is simply no longer a member. Counting it would pin the
/// session's `partial` flag on forever and make the controller re-apply the
/// tier on every tick (a shell running a build forks short-lived children
/// constantly).
#[cfg(any(unix, windows))]
fn count_failure_unless_vanished(identity: &ProcessIdentity, outcome: &mut SchedulingOutcome) {
    if still_the_same_process(identity) {
        outcome.failed += 1;
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    /// `<mach/task_policy.h>`: the two darwin-background bits of
    /// `proc_bsdshortinfo::pbsi_flags`. Internal is what a process sets on
    /// itself, external is what another process set on it — the daemon only
    /// ever produces the external one, but a restore must clear neither more
    /// nor less than "is this process in the background band".
    const PROC_FLAG_DARWINBG: u32 = 0x8000;
    const PROC_FLAG_EXT_DARWINBG: u32 = 0x10000;

    /// Read-back of the darwin-background band.
    ///
    /// `getpriority(PRIO_DARWIN_PROCESS, pid)` is NOT usable here: the kernel
    /// answers a cross-process query with 0 whatever the target's state is
    /// (only a process asking about *itself* gets 1). `proc_pidinfo`'s BSD
    /// short info does report the external bit, so that is the verification.
    fn darwin_background(pid: u32) -> io::Result<bool> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid pid"));
        }
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdshortinfo).cast(),
                size,
            )
        };
        if read != size {
            return Err(io::Error::last_os_error());
        }
        Ok(info.pbsi_flags & (PROC_FLAG_DARWINBG | PROC_FLAG_EXT_DARWINBG) != 0)
    }

    pub(super) fn observed_tier(pid: u32) -> Option<SchedulingTier> {
        match darwin_background(pid) {
            Ok(true) => Some(SchedulingTier::Background),
            Ok(false) => Some(SchedulingTier::Normal),
            Err(_) => None,
        }
    }

    pub(super) fn set_process_scheduling(
        identities: &[ProcessIdentity],
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        let background = tier == SchedulingTier::Background;
        let want = if background { libc::PRIO_DARWIN_BG } else { 0 };
        let mut outcome = SchedulingOutcome::default();
        for identity in identities {
            if !still_the_same_process(identity) {
                outcome.skipped_reused += 1;
                continue;
            }
            let rc = unsafe {
                libc::setpriority(libc::PRIO_DARWIN_PROCESS, identity.pid as libc::id_t, want)
            };
            // Applied only when the read-back agrees (§5 step 2 discipline
            // applied per pid).
            let applied =
                rc == 0 && matches!(darwin_background(identity.pid), Ok(bg) if bg == background);
            if applied {
                outcome.applied += 1;
            } else {
                count_failure_unless_vanished(identity, &mut outcome);
            }
        }
        Ok(outcome)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetPriorityClass, OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
        NORMAL_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    };

    pub(super) fn observed_tier(pid: u32) -> Option<SchedulingTier> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let class = unsafe { GetPriorityClass(handle) };
        let _ = unsafe { CloseHandle(handle) };
        match class {
            0 => None,
            c if c == BELOW_NORMAL_PRIORITY_CLASS.0 => Some(SchedulingTier::Background),
            _ => Some(SchedulingTier::Normal),
        }
    }

    pub(super) fn set_process_scheduling(
        identities: &[ProcessIdentity],
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        let want = match tier {
            SchedulingTier::Background => BELOW_NORMAL_PRIORITY_CLASS,
            SchedulingTier::Normal => NORMAL_PRIORITY_CLASS,
        };
        let mut outcome = SchedulingOutcome::default();
        for identity in identities {
            if !still_the_same_process(identity) {
                outcome.skipped_reused += 1;
                continue;
            }
            // Job CPU rate control is never touched here: a user-configured
            // hard cap (`cpu_max_cores`) and a weight are mutually exclusive
            // job flags, so a weight write would silently drop the cap.
            let rights = PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION;
            let Ok(handle) = (unsafe { OpenProcess(rights, false, identity.pid) }) else {
                count_failure_unless_vanished(identity, &mut outcome);
                continue;
            };
            let applied = unsafe { SetPriorityClass(handle, want) }.is_ok()
                && unsafe { GetPriorityClass(handle) } == want.0;
            let _ = unsafe { CloseHandle(handle) };
            if applied {
                outcome.applied += 1;
            } else {
                count_failure_unless_vanished(identity, &mut outcome);
            }
        }
        Ok(outcome)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::*;

    pub(super) fn observed_tier(_pid: u32) -> Option<SchedulingTier> {
        None
    }

    pub(super) fn set_process_scheduling(
        _identities: &[ProcessIdentity],
        _tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            // Linux: an unprivileged process may raise nice but cannot lower
            // it again (RLIMIT_NICE), and an irreversible relief is never
            // applied (08 §0-4). The delegated-cgroup path is the group
            // method `cpu.weight`, not this per-pid entry point.
            "no reversible per-process scheduling yield here: nice cannot be \
             restored without CAP_SYS_NICE (RLIMIT_NICE)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn background_is_applied_restored_and_never_touches_a_reused_pid() {
        use crate::group::testutil::spawn_lived_child;
        use crate::identity;

        let mut child = spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        assert_eq!(
            observed_tier(ident.pid),
            Some(SchedulingTier::Normal),
            "a fresh child is not in the background band"
        );

        let out = set_process_scheduling(std::slice::from_ref(&ident), SchedulingTier::Background)
            .expect("background");
        assert_eq!(
            out,
            SchedulingOutcome {
                applied: 1,
                failed: 0,
                skipped_reused: 0
            }
        );
        assert_eq!(
            observed_tier(ident.pid),
            Some(SchedulingTier::Background),
            "read-back shows the darwin background policy"
        );

        let out = set_process_scheduling(std::slice::from_ref(&ident), SchedulingTier::Normal)
            .expect("restore");
        assert_eq!(out.applied, 1);
        assert_eq!(
            observed_tier(ident.pid),
            Some(SchedulingTier::Normal),
            "restore cleared the policy (08 §0-4: every relief is reversible)"
        );

        // A recorded identity whose pid now belongs to another process is
        // skipped and counted — the live process is NOT changed.
        let stale = ProcessIdentity {
            pid: ident.pid,
            start_token: format!("{}-stale", ident.start_token),
            boot_id: ident.boot_id.clone(),
        };
        let out = set_process_scheduling(std::slice::from_ref(&stale), SchedulingTier::Background)
            .expect("skip");
        assert_eq!(
            out,
            SchedulingOutcome {
                applied: 0,
                failed: 0,
                skipped_reused: 1
            }
        );
        assert_eq!(
            observed_tier(ident.pid),
            Some(SchedulingTier::Normal),
            "the reused pid was never touched"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    /// 떠난 멤버는 절대 실패가 아니다: 실패로 세면 세션이 영원히 `partial`로
    /// 남아 매 틱 재적용을 부른다(빌드 중인 셸은 단명 자식을 계속 만든다).
    /// 호출 전에 이미 떠났으면 신원 재검증이 걸러 내고(`skipped_reused`),
    /// 호출 도중에 떠났으면 [`count_failure_unless_vanished`]가 걸러 낸다.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_member_that_exited_is_never_counted_as_a_failure() {
        use crate::group::testutil::spawn_lived_child;
        use crate::identity;

        let mut child = spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        let _ = child.kill();
        let _ = child.wait();

        let out = set_process_scheduling(std::slice::from_ref(&ident), SchedulingTier::Background)
            .expect("gone member");
        assert_eq!(out.applied, 0);
        assert_eq!(out.failed, 0, "떠난 프로세스는 실패가 아니다: {out:?}");
        assert!(!out.is_partial(), "세션이 partial로 굳지 않는다");

        // 그리고 같은 판정이 호출 직전에 죽은 프로세스에도 적용된다.
        let mut outcome = SchedulingOutcome::default();
        count_failure_unless_vanished(&ident, &mut outcome);
        assert_eq!(outcome, SchedulingOutcome::default());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_per_pid_yield_is_unsupported_because_it_cannot_be_undone() {
        let err = set_process_scheduling(&[], SchedulingTier::Background)
            .expect_err("per-pid nice is one-way for an unprivileged daemon");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("RLIMIT_NICE"));
        assert_eq!(observed_tier(std::process::id()), None);
    }

    #[test]
    fn an_empty_membership_is_a_no_op_where_supported() {
        match set_process_scheduling(&[], SchedulingTier::Normal) {
            Ok(outcome) => assert_eq!(outcome, SchedulingOutcome::default()),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
        }
    }
}
