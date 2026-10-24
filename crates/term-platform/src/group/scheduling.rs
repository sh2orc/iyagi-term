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
/// member and restores the class each member had before (a HIGH or IDLE
/// process comes back as itself, not as NORMAL). Job CPU rate control is
/// deliberately untouched: weight-based and hard-cap rate control are
/// mutually exclusive flags, so writing a weight would clobber a
/// user-configured `cpu_max_cores` hard cap (03 §6).
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
///
/// The order is chosen here, not by the caller: a stop signals parents before
/// their descendants (a stopped parent forks nothing new mid-pass), a resume
/// wakes descendants first — a job-control shell that woke first would reap
/// the still-stopped foreground job as "Stopped" and take the terminal back.
pub fn set_process_suspend(
    identities: &[ProcessIdentity],
    suspend: bool,
) -> io::Result<SchedulingOutcome> {
    let ordered = tree_order(identities, suspend);
    suspend_imp::apply(&ordered, suspend)
}

/// How many times [`suspend_walked`] re-walks the membership after the first
/// stop. Parents are stopped first, so each pass only finds what a
/// still-running ancestor forked inside the previous pass's window.
pub const SUSPEND_RESCAN_PASSES: usize = 4;

/// Stop every member `walk` returns, then re-walk up to
/// [`SUSPEND_RESCAN_PASSES`] times to stop children forked between the walk
/// and the signal (a build's `rustc`, a test runner's workers) — those would
/// otherwise keep running until the next tick. Returns the combined outcome
/// and every identity a pass covered (the crash-safe ledger records them).
///
/// Only the first walk's error is returned: once something is stopped, a
/// failed re-walk (the root just exited) ends the loop with what was done.
pub fn suspend_walked(
    mut walk: impl FnMut() -> io::Result<Vec<ProcessIdentity>>,
) -> io::Result<(SchedulingOutcome, Vec<ProcessIdentity>)> {
    let mut covered = walk()?;
    let mut outcome = set_process_suspend(&covered, true)?;
    let mut seen: std::collections::HashSet<(u32, String)> = covered
        .iter()
        .map(|identity| (identity.pid, identity.start_token.clone()))
        .collect();
    for _ in 0..SUSPEND_RESCAN_PASSES {
        let Ok(members) = walk() else { break };
        let fresh: Vec<ProcessIdentity> = members
            .into_iter()
            .filter(|identity| seen.insert((identity.pid, identity.start_token.clone())))
            .collect();
        if fresh.is_empty() {
            break;
        }
        outcome += set_process_suspend(&fresh, true)?;
        covered.extend(fresh);
    }
    Ok((outcome, covered))
}

/// Order identities by their depth in the live process tree: ancestors first
/// for a stop, descendants first for a resume (stable among equal depths).
/// A pid the snapshot does not know is newer than the snapshot, and so is
/// every process it could have forked — it is ordered as a leaf. Without a
/// native snapshot (Windows, where suspend is unsupported anyway) the input
/// order is kept.
fn tree_order(identities: &[ProcessIdentity], parents_first: bool) -> Vec<ProcessIdentity> {
    if identities.len() < 2 {
        return identities.to_vec();
    }
    match crate::native_proc::snapshot(crate::proc_scan::SHARED_SCAN_MAX_AGE) {
        Some(snapshot) => order_by_depth(&snapshot, identities, parents_first),
        None => identities.to_vec(),
    }
}

fn order_by_depth(
    snapshot: &crate::native_proc::Snapshot,
    identities: &[ProcessIdentity],
    parents_first: bool,
) -> Vec<ProcessIdentity> {
    let mut ordered = identities.to_vec();
    let depth = |pid: u32| -> usize {
        // A ppid cycle (pid reuse racing the listing) ends at the bound.
        const MAX_DEPTH: usize = 512;
        let Some(mut entry) = snapshot.get(pid) else {
            return MAX_DEPTH + 1;
        };
        let mut depth = 0;
        while depth < MAX_DEPTH && entry.ppid != entry.pid && entry.ppid != 0 {
            let Some(parent) = snapshot.get(entry.ppid) else {
                break;
            };
            entry = parent;
            depth += 1;
        }
        depth
    };
    if parents_first {
        ordered.sort_by_cached_key(|identity| depth(identity.pid));
    } else {
        ordered.sort_by_cached_key(|identity| std::cmp::Reverse(depth(identity.pid)));
    }
    ordered
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
                // errno first: the re-verification below makes syscalls.
                let denied = io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
                count_failure_unless_vanished(identity, denied, &mut outcome);
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
///
/// A live member the OS refused (`denied`: `EPERM` / access denied — a
/// setuid helper such as `sudo` under another uid, an elevated Windows
/// process) is not a failure either: no retry will ever succeed, so it is
/// counted in `skipped_denied` and leaves the outcome complete.
#[cfg(any(unix, windows))]
fn count_failure_unless_vanished(
    identity: &ProcessIdentity,
    denied: bool,
    outcome: &mut SchedulingOutcome,
) {
    if !still_the_same_process(identity) {
        return;
    }
    if denied {
        outcome.skipped_denied += 1;
    } else {
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
            let denied = rc != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            // Applied only when the read-back agrees (§5 step 2 discipline
            // applied per pid).
            let applied =
                rc == 0 && matches!(darwin_background(identity.pid), Ok(bg) if bg == background);
            if applied {
                outcome.applied += 1;
            } else {
                count_failure_unless_vanished(identity, denied, &mut outcome);
            }
        }
        Ok(outcome)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED};
    use windows::Win32::System::Threading::{
        GetPriorityClass, OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    };

    /// The priority class each yielded process had before the yield, keyed by
    /// pid + start token. A restore puts exactly that class back — a user's
    /// `start /high` build must not come back as NORMAL (08 §0-4). Children
    /// born during the yield inherit BELOW_NORMAL and have no entry: NORMAL
    /// is what they would have started with. Entries of processes that exited
    /// while yielded are pruned on the next restore pass.
    static ORIGINAL_CLASS: Mutex<BTreeMap<(u32, String), u32>> = Mutex::new(BTreeMap::new());

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

    /// The class to move a process to, or `None` when it already is where the
    /// tier wants it. Only BELOW_NORMAL is ever written by a yield, so a
    /// restore only undoes BELOW_NORMAL — any other class was chosen by the
    /// user (possibly during the yield) and is left alone.
    fn target_class(
        tier: SchedulingTier,
        current: u32,
        original: &mut BTreeMap<(u32, String), u32>,
        key: (u32, String),
    ) -> Option<u32> {
        let below = BELOW_NORMAL_PRIORITY_CLASS.0;
        match tier {
            // IDLE is already below the band: raising it would not yield.
            SchedulingTier::Background if current == below || current == IDLE_PRIORITY_CLASS.0 => {
                None
            }
            SchedulingTier::Background => {
                original.entry(key).or_insert(current);
                Some(below)
            }
            SchedulingTier::Normal => {
                let before = original.remove(&key);
                (current == below).then(|| before.unwrap_or(NORMAL_PRIORITY_CLASS.0))
            }
        }
    }

    pub(super) fn set_process_scheduling(
        identities: &[ProcessIdentity],
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        let mut original = ORIGINAL_CLASS.lock().unwrap_or_else(|p| p.into_inner());
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
            let handle = match unsafe { OpenProcess(rights, false, identity.pid) } {
                Ok(handle) => handle,
                Err(error) => {
                    let denied = error.code() == E_ACCESSDENIED;
                    count_failure_unless_vanished(identity, denied, &mut outcome);
                    continue;
                }
            };
            let current = unsafe { GetPriorityClass(handle) };
            let key = (identity.pid, identity.start_token.clone());
            let applied = current != 0
                && match target_class(tier, current, &mut original, key) {
                    None => true,
                    Some(want) => {
                        unsafe { SetPriorityClass(handle, PROCESS_CREATION_FLAGS(want)) }.is_ok()
                            && unsafe { GetPriorityClass(handle) } == want
                    }
                };
            let _ = unsafe { CloseHandle(handle) };
            if applied {
                outcome.applied += 1;
            } else {
                count_failure_unless_vanished(identity, false, &mut outcome);
            }
        }
        if tier == SchedulingTier::Normal {
            original.retain(|(pid, start_token), _| {
                crate::identity::process_identity(*pid)
                    .is_some_and(|live| live.start_token == *start_token)
            });
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
                skipped_reused: 0,
                skipped_denied: 0
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
                skipped_reused: 1,
                skipped_denied: 0
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
        count_failure_unless_vanished(&ident, false, &mut outcome);
        assert_eq!(outcome, SchedulingOutcome::default());
    }

    /// 다른 uid의 프로세스(setuid `sudo` 등)는 EPERM으로 거부된다 — 재시도해도
    /// 영원히 실패하므로 `failed`가 아니라 `skipped_denied`로 세고 결과를
    /// partial로 만들지 않는다. 살아 있는 같은 프로세스여야 센다(떠난
    /// 멤버는 어느 쪽에도 세지 않는다).
    #[cfg(any(unix, windows))]
    #[test]
    fn a_denied_member_is_counted_apart_and_keeps_the_outcome_complete() {
        let me = crate::identity::process_identity(std::process::id()).expect("own identity");

        let mut denied = SchedulingOutcome::default();
        count_failure_unless_vanished(&me, true, &mut denied);
        assert_eq!(denied.skipped_denied, 1);
        assert_eq!(denied.failed, 0);
        assert!(!denied.is_partial(), "거부는 재시도 대상이 아니다");

        let mut failed = SchedulingOutcome::default();
        count_failure_unless_vanished(&me, false, &mut failed);
        assert_eq!(failed.failed, 1);
        assert!(failed.is_partial());
    }

    /// 실제 커널의 EPERM: Linux는 `/proc/1/stat`이 누구에게나 읽혀 init의
    /// 신원이 검증되고, 일반 사용자의 SIGSTOP은 거부될 뿐 전달되지 않는다.
    /// (macOS는 다른 uid 프로세스의 신원을 읽을 수 없어 검증 단계에서 이미
    /// 빠진다 — 이 경로에 오지 않는다.) root로 돌거나 `hidepid`로 init이
    /// 보이지 않으면 건너뛴다.
    #[cfg(target_os = "linux")]
    #[test]
    fn signalling_another_uid_is_denied_not_partial() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let Some(init) = crate::identity::process_identity(1) else {
            return;
        };
        let out = set_process_suspend(std::slice::from_ref(&init), true).expect("suspend");
        assert_eq!(
            out,
            SchedulingOutcome {
                applied: 0,
                failed: 0,
                skipped_reused: 0,
                skipped_denied: 1,
            }
        );
    }

    fn entry(pid: u32, ppid: u32) -> crate::native_proc::ProcEntry {
        crate::native_proc::ProcEntry {
            pid,
            ppid,
            start_token: format!("t{pid}"),
            start_epoch_ms: 0,
            uid: 0,
            name: format!("p{pid}"),
        }
    }

    fn ident(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_token: format!("t{pid}"),
            boot_id: "boot".into(),
        }
    }

    /// 정지는 부모 먼저(멈춘 부모는 그사이 fork하지 않는다), 재개는 자식
    /// 먼저(먼저 깬 잡 제어 셸이 아직 멈춘 포그라운드 잡을 "Stopped"로 거둬
    /// 터미널을 가져가지 않게). 입력 순서와 무관하고, 스냅샷에 없는 pid는
    /// 스냅샷보다 새 프로세스이므로 잎으로 본다.
    #[test]
    fn suspend_orders_parents_first_and_resume_orders_children_first() {
        // 1 ─ 10(셸) ─ 20(make) ─ 30(cc)
        //                       └ 31(cc)
        let snapshot = crate::native_proc::Snapshot::from_entries(vec![
            entry(1, 0),
            entry(10, 1),
            entry(20, 10),
            entry(30, 20),
            entry(31, 20),
        ]);
        let scrambled = [ident(30), ident(99), ident(10), ident(31), ident(20)];
        let pids = |list: Vec<ProcessIdentity>| list.iter().map(|i| i.pid).collect::<Vec<_>>();

        let stop = pids(order_by_depth(&snapshot, &scrambled, true));
        assert_eq!(
            stop,
            [10, 20, 30, 31, 99],
            "부모 먼저, 같은 깊이는 입력 순서"
        );
        let resume = pids(order_by_depth(&snapshot, &scrambled, false));
        assert_eq!(resume, [99, 30, 31, 20, 10], "자식 먼저");
    }

    /// pid 재사용 경주로 ppid가 순환해도 깊이 계산은 끝난다.
    #[test]
    fn a_ppid_cycle_does_not_hang_the_ordering() {
        let snapshot =
            crate::native_proc::Snapshot::from_entries(vec![entry(5, 6), entry(6, 5), entry(7, 6)]);
        let ordered = order_by_depth(&snapshot, &[ident(7), ident(5), ident(6)], true);
        assert_eq!(ordered.len(), 3);
    }

    /// 훑기와 정지 사이에 fork된 자식은 재훑기가 잡는다. 두 번째 훑기에서
    /// 처음 보인 멤버도 멈추고, 결과와 덮은 신원 목록에 합쳐진다.
    #[cfg(unix)]
    #[test]
    fn suspend_walked_stops_members_that_appear_on_a_rescan() {
        use crate::group::testutil::spawn_lived_child;

        let mut first = spawn_lived_child();
        let mut late = spawn_lived_child();
        let a = crate::identity::process_identity(first.id()).expect("first identity");
        let b = crate::identity::process_identity(late.id()).expect("late identity");
        let mut walks = 0;
        let (outcome, covered) = suspend_walked(|| {
            walks += 1;
            Ok(if walks == 1 {
                vec![a.clone()]
            } else {
                vec![a.clone(), b.clone()]
            })
        })
        .expect("suspend");
        assert_eq!(outcome.applied, 2, "{outcome:?}");
        assert_eq!(covered.len(), 2);
        assert_eq!(walks, 3, "새 멤버가 없는 훑기에서 멈춘다");

        let resumed = set_process_suspend(&covered, false).expect("resume");
        assert_eq!(resumed.applied, 2);
        for child in [&mut first, &mut late] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// 첫 훑기의 오류만 돌려준다 — 무언가 멈춘 뒤의 재훑기 실패(루트가 막
    /// 끝남)는 그때까지 한 일로 끝낸다.
    #[cfg(unix)]
    #[test]
    fn suspend_walked_reports_only_the_first_walk_error() {
        let err = suspend_walked(|| Err(io::Error::new(io::ErrorKind::NotFound, "gone")))
            .expect_err("first walk failed");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        let mut walks = 0;
        let (outcome, covered) = suspend_walked(|| {
            walks += 1;
            if walks == 1 {
                Ok(Vec::new())
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
            }
        })
        .expect("later walk errors end the loop");
        assert_eq!(outcome, SchedulingOutcome::default());
        assert!(covered.is_empty());
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
