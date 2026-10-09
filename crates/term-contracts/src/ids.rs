//! Identifier and numeric wire types (spec `01-contracts.md` §1).

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Decimal-string encoded `u64`. JSON numbers cannot carry the full SQLite
/// signed-INTEGER range losslessly in every consumer, so byte counts and
/// monotonic sequences travel as strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct U64String(String);

impl Ord for U64String {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.get().cmp(&other.get())
    }
}

impl PartialOrd for U64String {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("U64String must be 0..=9223372036854775807 in decimal, got {0}")]
pub struct U64RangeError(String);

impl U64String {
    /// SQLite signed INTEGER bound: persisted numbers never exceed 2^63-1.
    pub const MAX: u64 = i64::MAX as u64;

    pub fn new(value: u64) -> Result<Self, U64RangeError> {
        if value > Self::MAX {
            return Err(U64RangeError(value.to_string()));
        }
        Ok(Self(value.to_string()))
    }

    pub fn get(&self) -> u64 {
        self.0.parse().expect("validated decimal on construction")
    }

    pub fn parse(s: &str) -> Result<Self, U64RangeError> {
        // Strict decimal: no signs, whitespace, or non-ASCII digits.
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(U64RangeError(s.to_string()));
        }
        match s.parse::<u64>() {
            Ok(v) if v <= Self::MAX => Ok(Self(v.to_string())),
            _ => Err(U64RangeError(s.to_string())),
        }
    }
}

impl From<U64String> for String {
    fn from(value: U64String) -> Self {
        value.0
    }
}

impl TryFrom<String> for U64String {
    type Error = U64RangeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl fmt::Display for U64String {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Marker newtypes for UUID v4 identifiers. They prevent mixing e.g. a session
/// id with a workload id at type level; each parses/validates as UUID v4.
macro_rules! uuid_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
        #[serde(try_from = "String", into = "String")]
        #[ts(as = "String")]
        pub struct $name(String);

        impl $name {
            pub fn generate() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            pub fn parse(s: &str) -> Result<Self, IdParseError> {
                match Uuid::parse_str(s) {
                    Ok(u) if u.get_version_num() == 4 => Ok(Self(u.to_string())),
                    _ => Err(IdParseError {
                        kind: stringify!($name),
                        value: s.to_string(),
                    }),
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdParseError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }
    };
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{kind} must be a UUID v4 string, got {value:?}")]
pub struct IdParseError {
    pub kind: &'static str,
    pub value: String,
}

uuid_newtype!(
    /// User-visible launch request id; re-sending it must never run the CLI twice.
    RequestId
);
uuid_newtype!(
    /// Workload = resource policy unit.
    WorkloadId
);
uuid_newtype!(
    /// PTY session id.
    SessionId
);
uuid_newtype!(
    /// UI view (pane attachment) id.
    ViewId
);
uuid_newtype!(
    /// Local IPC connection id.
    ConnectionId
);
uuid_newtype!(
    /// OS boot identity; unreliable platforms disable ownership restoration.
    BootId
);

/// PID + start token + boot id triple. PID reuse protection: all three must
/// match before any signal or ownership claim (spec `01-contracts.md` §1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Linux `/proc/<pid>/stat` starttime, Windows process creation time,
    /// macOS native start time — opaque decimal/string per platform.
    pub start_token: String,
    pub boot_id: String,
}

impl ProcessIdentity {
    pub fn same_process(&self, other: &ProcessIdentity) -> bool {
        self.pid == other.pid
            && self.start_token == other.start_token
            && self.boot_id == other.boot_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u64string_orders_numerically_and_normalizes_equivalent_inputs() {
        let mut values = [
            U64String::new(100).unwrap(),
            U64String::new(9).unwrap(),
            U64String::new(10).unwrap(),
        ];
        values.sort();
        assert_eq!(values.map(|v| v.get()), [9, 10, 100]);
        let canonical = U64String::parse("00010").unwrap();
        assert_eq!(canonical, U64String::new(10).unwrap());
        assert_eq!(serde_json::to_value(canonical).unwrap(), "10");
        assert!(
            U64String::new(9_223_372_036_854_775_806).unwrap()
                < U64String::new(U64String::MAX).unwrap()
        );
    }

    #[test]
    fn u64string_accepts_only_strict_decimal_in_range() {
        assert!(U64String::parse("0").is_ok());
        assert!(U64String::parse("9223372036854775807").is_ok());
        assert_eq!(
            U64String::parse("9223372036854775807").unwrap().get(),
            i64::MAX as u64
        );
        assert!(U64String::parse("9223372036854775808").is_err()); // over SQLite bound
        assert!(U64String::parse("+1").is_err());
        assert!(U64String::parse("-1").is_err());
        assert!(U64String::parse(" 1").is_err());
        assert!(U64String::parse("1 ").is_err());
        assert!(U64String::parse("").is_err());
        assert!(U64String::parse("0x10").is_err());
        assert!(U64String::new(u64::MAX).is_err());
    }

    #[test]
    fn ids_require_uuid_v4() {
        let id = RequestId::generate();
        assert!(RequestId::parse(id.as_str()).is_ok());
        // v1-shaped uuid rejected
        assert!(RequestId::parse("e2f5c8e0-6b1a-11d0-a08c-0020af31e880").is_err());
        assert!(RequestId::parse("not-a-uuid").is_err());
    }

    #[test]
    fn identity_compares_all_three_parts() {
        let a = ProcessIdentity {
            pid: 42,
            start_token: "12345".into(),
            boot_id: "b1".into(),
        };
        let same = a.clone();
        let mut reused_pid = a.clone();
        reused_pid.start_token = "99999".into();
        assert!(a.same_process(&same));
        assert!(!a.same_process(&reused_pid));
    }
}
