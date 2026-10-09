//! `iyagi-termd --launch-helper <endpoint> <nonce>` (spec `02-runner.md` §3).
//!
//! THE invariant: the target process is NEVER created before RELEASE. On
//! timeout, EOF or Abort the helper exits without creating anything.
//!
//! Windows: connects with a sync File handle over the named pipe, spawns the
//! target (inherit console/env, already inside the Job via the parent) and
//! reports `Exited{code}` through the gate.
//! Unix: execs the target (keeps the controlling terminal); success is
//! signalled by the gate stream's close-on-exec EOF. `term_pty::GateClient`
//! implements both platform behaviors in `run_target`.

use std::time::Instant;

use term_pty::gate::GateClient;

use crate::sessions::SyncStream;

/// 헬퍼가 RELEASE를 기다리는 상한 — 명세의 게이트 시한과 같은 값이다.
/// `GATE_TIMEOUT_MS`는 밀리초 상수라 `from_secs`로 읽으면 5000초가 되어
/// (Unix는 데몬 쪽 시한이 대신 끊어 주지만) Windows에선 helper가 ConPTY 안에
/// 고아로 남았다.
pub(crate) fn helper_deadline() -> std::time::Duration {
    std::time::Duration::from_millis(term_contracts::gate::GATE_TIMEOUT_MS)
}

pub fn run(endpoint: &str, nonce: &str) -> i32 {
    let deadline = Instant::now() + helper_deadline();
    let stream = match SyncStream::connect(endpoint) {
        Ok(stream) => stream,
        Err(e) => {
            tracing::debug!(error = %e, "launch-helper: gate connect failed");
            return 2;
        }
    };
    // Bounded reads so `await_release` honors its deadline on a silent
    // daemon (Unix: SO_RCVTIMEO; Windows: the pipe reader thread's
    // `recv_timeout` — see `SyncStream`).
    if let Err(e) = stream.set_read_timeout(Some(std::time::Duration::from_millis(50))) {
        tracing::debug!(error = %e, "launch-helper: gate read timeout not set");
        return 2;
    }
    let identity = match term_platform::identity::current_process_identity() {
        Some(identity) => identity,
        None => return 3,
    };
    let mut client = GateClient::new(stream);
    if let Err(e) = client.hello(nonce, &identity) {
        tracing::debug!(error = %e, "launch-helper: gate hello failed");
        return 4;
    }
    // Abort / timeout / EOF ⇒ exit WITHOUT creating the target.
    let target = match client.await_release(deadline) {
        Ok(target) => target,
        Err(e) => {
            tracing::debug!(error = %e, "launch-helper: no release; target not created");
            return 5;
        }
    };
    tracing::debug!("launch-helper: release received; creating target");
    match client.run_target(target) {
        Ok(code) => code,
        Err(e) => {
            tracing::debug!(error = %e, "launch-helper: target run failed");
            6
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 회귀 방지: 시한은 초가 아니라 밀리초 상수에서 온다(5 s ≪ 10 s).
    #[test]
    fn helper_deadline_is_the_gate_timeout_in_milliseconds() {
        assert_eq!(helper_deadline(), term_pty::gate::default_timeout());
        assert!(helper_deadline() <= std::time::Duration::from_secs(10));
    }
}
