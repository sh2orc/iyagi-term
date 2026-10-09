//! Connection authentication (spec `01-contracts.md` §3).
//!
//! * control role — bearer of the per-start token file (`<data>/runtime/token`,
//!   0600 on Unix). The daemon never logs it.
//! * data role — a one-shot `data_token` issued with a control `hello`
//!   result (5 s TTL, single use), bound to the issuing control connection.
//!
//! Comparison of the control token is constant-time (length-independent
//! early exit would leak the token length only, which the base64 encoding
//! fixes anyway; the comparison itself does not short-circuit).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use term_contracts::ids::ConnectionId;

/// Constant-time equality over the raw token bytes.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let max = a.len().max(b.len());
    let mut diff: u32 = (a.len() ^ b.len()) as u32;
    for i in 0..max {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as u32;
    }
    diff == 0
}

/// One-shot data tokens: issue on control hello, redeem on data hello.
pub struct DataTokens {
    ttl: Duration,
    entries: Mutex<HashMap<String, (ConnectionId, Instant)>>,
}

impl DataTokens {
    pub fn new(ttl: Duration) -> Self {
        DataTokens {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Fresh random 32-byte token, base64-encoded, bound to `control`.
    /// Prunes expired entries opportunistically.
    pub fn issue(&self, control: ConnectionId) -> String {
        let bytes: [u8; 32] = rand::random();
        let token = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut entries = self.lock();
        let now = Instant::now();
        entries.retain(|_, (_, exp)| *exp > now);
        entries.insert(token.clone(), (control, now + self.ttl));
        token
    }

    /// Redeem: single-use (removed even when expired) and TTL-checked.
    /// Returns the issuing control connection on success.
    pub fn redeem(&self, token: &str) -> Option<ConnectionId> {
        let mut entries = self.lock();
        let (control, expires) = entries.remove(token)?;
        if Instant::now() > expires {
            return None;
        }
        Some(control)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (ConnectionId, Instant)>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "secreT"));
        assert!(!constant_time_eq("secret", "secre"));
        assert!(!constant_time_eq("", "x"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn data_tokens_are_single_use_and_expire() {
        let tokens = DataTokens::new(Duration::from_millis(50));
        let control = ConnectionId::generate();
        let t = tokens.issue(control.clone());
        assert_eq!(tokens.redeem(&t), Some(control.clone()));
        assert_eq!(tokens.redeem(&t), None, "single use");
        assert_eq!(tokens.len(), 0);

        let t2 = tokens.issue(control.clone());
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(tokens.redeem(&t2), None, "expired token is rejected");
    }

    #[test]
    fn issue_prunes_expired_entries() {
        let tokens = DataTokens::new(Duration::from_millis(20));
        let control = ConnectionId::generate();
        tokens.issue(control.clone());
        tokens.issue(control.clone());
        assert_eq!(tokens.len(), 2);
        std::thread::sleep(Duration::from_millis(40));
        tokens.issue(control);
        assert_eq!(tokens.len(), 1, "expired entries pruned on issue");
    }
}
