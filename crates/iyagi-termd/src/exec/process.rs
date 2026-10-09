//! Child-process slot and pipe pump for Exec runs (ticket O07).
//!
//! The [`ChildSlot`] keeps exactly one owner for the reaped/unreaped
//! transition: every path that learns the exit status stores it exactly
//! once, and the ladder in [`ChildSlot::stop_sync`] never returns before
//! the OS reap is confirmed (02 §9). The async surface is a thin
//! `spawn_blocking` wrapper so the ladder's grace windows never hold an
//! async worker.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Child;

use super::output::{LineCutter, StreamTap, MAX_LINE_BYTES};
use super::ownership::{self, ConfirmedExit};

/// Final observed status of one child: OS exit code (`None` when the OS
/// reports none, e.g. a signal kill) and whether the stop ladder forced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub killed: bool,
}

enum ChildState {
    Live(Box<Child>),
    /// A concurrent force-killer owns the reap right now.
    Reaping,
    Exited(ExitInfo),
}

pub(crate) struct ChildSlot {
    state: Mutex<ChildState>,
}

impl ChildSlot {
    pub(crate) fn new(child: Child) -> Self {
        ChildSlot {
            state: Mutex::new(ChildState::Live(Box::new(child))),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ChildState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // Hold the child-state lock across a PID signal, so another observer
    // cannot reap and allow PID reuse between the liveness check and signal.
    fn signal_unreaped(&self, signal: impl FnOnce(u32)) {
        let state = self.lock();
        if let ChildState::Live(child) = &*state {
            if let Some(pid) = child.id() {
                signal(pid);
            }
        }
    }

    /// Non-blocking probe: `None` while alive (or being reaped elsewhere).
    pub(crate) fn poll_exit(&self) -> Option<ExitInfo> {
        let mut state = self.lock();
        if let ChildState::Exited(info) = *state {
            return Some(info);
        }
        if let ChildState::Live(child) = &mut *state {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let info = ExitInfo {
                        code: status.code(),
                        killed: false,
                    };
                    *state = ChildState::Exited(info);
                    return Some(info);
                }
                _ => return None, // alive (or transient error: keep probing)
            }
        }
        None
    }

    /// Take the child for the force path. `None` when somebody else owns the
    /// reap or the child already exited.
    fn take_for_force(&self) -> Option<Box<Child>> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, ChildState::Reaping) {
            ChildState::Live(child) => Some(child),
            other => {
                *state = other;
                None
            }
        }
    }

    pub(crate) fn record_exited(&self, info: ExitInfo) {
        let mut state = self.lock();
        if matches!(&*state, ChildState::Exited(_)) {
            return; // exactly-once
        }
        *state = ChildState::Exited(info);
    }

    /// The full cancellation ladder (02 §9): interrupt → grace → terminate →
    /// grace → kill → **unbounded wait for the reap**. Blocking; run via
    /// `spawn_blocking`.
    ///
    /// Classification: an exit observed before the terminate step ran is
    /// [`ConfirmedExit::Exited`]; anything after the ladder started
    /// signaling is [`ConfirmedExit::Killed`] — the run only ended because
    /// the ladder intervened.
    pub(crate) fn stop_sync(
        &self,
        grace_interrupt: Duration,
        grace_kill: Duration,
    ) -> ConfirmedExit {
        self.stop_sync_with_group(grace_interrupt, grace_kill, || {}, || {})
    }

    pub(crate) fn stop_sync_with_group(
        &self,
        grace_interrupt: Duration,
        grace_kill: Duration,
        terminate_group: impl FnOnce(),
        kill_group: impl FnOnce(),
    ) -> ConfirmedExit {
        let t0 = Instant::now();

        // Step 1: graceful interrupt (Windows piped children: no-op).
        self.signal_unreaped(|pid| {
            let _ = ownership::send_interrupt(pid);
        });
        if self.await_exit_within(grace_interrupt) {
            return self.exit_after_interrupt();
        }

        // Step 2: terminate the tree (SIGTERM / taskkill /T /F).
        terminate_group();
        self.signal_unreaped(|pid| {
            let _ = ownership::terminate_tree(pid);
        });
        if self.await_exit_within(grace_kill) {
            return ConfirmedExit::Killed {
                elapsed_ms: t0.elapsed().as_millis(),
            };
        }

        // Step 3: kill + confirmed reap. This wait has no deadline — the
        // caller must not observe completion before termination is
        // confirmed (02 §9).
        kill_group();
        self.signal_unreaped(|pid| {
            let _ = ownership::kill_pid(pid);
        });
        match self.take_for_force() {
            Some(mut child) => {
                let _ = child.start_kill();
                let code = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break status.code(),
                        _ => std::thread::sleep(ownership::LADDER_POLL),
                    }
                };
                self.record_exited(ExitInfo { code, killed: true });
            }
            None => {
                // Another stopper owns the reap (or it already exited):
                // still do not return before the status is known.
                while self.poll_exit().is_none() {
                    std::thread::sleep(ownership::LADDER_POLL);
                }
            }
        }
        ConfirmedExit::Killed {
            elapsed_ms: t0.elapsed().as_millis(),
        }
    }

    fn await_exit_within(&self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if self.poll_exit().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(ownership::LADDER_POLL);
        }
    }

    fn exit_after_interrupt(&self) -> ConfirmedExit {
        match self.poll_exit() {
            Some(info) => ConfirmedExit::Exited { code: info.code },
            // Only called right after await_exit_within said otherwise.
            None => ConfirmedExit::Exited { code: None },
        }
    }
}

/// Stream one pipe into the [`StreamTap`]: bounded spool + artifact-like
/// sink + the 1 MiB raw line cap (03 §2). Ends at EOF; `done` is set so the
/// handle can wait for exit *and* full output capture.
pub(crate) fn pipe_pump<R: tokio::io::AsyncRead + Unpin + Send>(
    mut reader: R,
    tap: Arc<StreamTap>,
) -> impl std::future::Future<Output = std::io::Result<()>> + Send {
    struct Completion(Arc<StreamTap>);
    impl Drop for Completion {
        fn drop(&mut self) {
            if !self.0.done.load(std::sync::atomic::Ordering::Acquire) {
                // Runtime shutdown or task cancellation can drop a pump
                // before EOF. Invalidate its stream and unblock ownership
                // cleanup; it is not a valid complete provider output.
                self.0.note_cut();
                self.0
                    .done
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
    let completion = Completion(tap.clone());
    async move {
        let _completion = completion;
        let mut cutter = LineCutter::new(MAX_LINE_BYTES);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut chunk).await {
                Ok(n) => n,
                Err(error) => {
                    // An incomplete stream invalidates its result, but cannot
                    // keep a reaped process waiting forever for EOF.
                    tap.note_cut();
                    tap.done.store(true, std::sync::atomic::Ordering::Release);
                    return Err(error);
                }
            };
            if n == 0 {
                break;
            }
            let cuts = cutter.feed(&chunk[..n], |line| tap.line(line));
            for _ in 0..cuts {
                tap.note_cut();
            }
        }
        if let Some(fragment) = cutter.finish() {
            tap.line(&fragment);
        }
        tap.done.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}
