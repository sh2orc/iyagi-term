//! Task/Attempt UUID v4 newtypes.
//!
//! term-contracts ships RequestId/WorkloadId/SessionId/... but deliberately
//! not every persistence id; tasks and attempts are storage-side concepts in
//! R1 (spec §1: attempt = one execution, retry = new attempt). Same shape as
//! the contract newtypes: UUID v4 only.

use std::fmt;

use uuid::Uuid;

macro_rules! storage_uuid_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
            fn try_from(value: String) -> Result<Self, IdParseError> {
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

storage_uuid_newtype!(
    /// User intent ("run this CLI once"); one row per launch.
    TaskId
);

storage_uuid_newtype!(
    /// A single execution of a task; retries create a new ordinal attempt.
    AttemptId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_require_uuid_v4() {
        let id = TaskId::generate();
        assert!(TaskId::parse(id.as_str()).is_ok());
        assert!(AttemptId::parse("not-a-uuid").is_err());
        assert!(TaskId::parse("e2f5c8e0-6b1a-11d0-a08c-0020af31e880").is_err());
    }
}
