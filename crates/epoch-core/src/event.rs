use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BranchId, EpochId, EventId, SessionId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventActor {
    Agent,
    Supervisor,
    Tool,
    Gateway,
    Operator,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Started,
    Succeeded,
    Failed,
    Denied,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EventKind(String);

impl EventKind {
    /// Creates a normalized event kind.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidEventKind`] when the value is empty, longer than 128 bytes, or contains
    /// characters outside lowercase ASCII letters, digits, dots, and underscores.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidEventKind> {
        let value = value.into();
        let is_valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'_'
            });
        if is_valid {
            Ok(Self(value))
        } else {
            Err(InvalidEventKind(value))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for EventKind {
    type Err = InvalidEventKind;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid event kind: {0:?}")]
pub struct InvalidEventKind(String);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Event {
    pub event_id: EventId,
    pub sequence: u64,
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub epoch_id: Option<EpochId>,
    pub causal_parent: Option<EventId>,
    pub monotonic_ns: u64,
    pub occurred_at_unix_ms: i64,
    pub actor: EventActor,
    pub kind: EventKind,
    pub input_hash: Option<String>,
    pub output_hash: Option<String>,
    pub status: EventStatus,
    pub payload_json: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_accepts_namespaced_values() {
        let kind = EventKind::new("process.exec_started").expect("valid kind");
        assert_eq!(kind.as_str(), "process.exec_started");
    }

    #[test]
    fn event_kind_rejects_ambiguous_or_unbounded_values() {
        for invalid in ["", "Process.Exec", "process exec", "process/exec"] {
            assert!(EventKind::new(invalid).is_err(), "{invalid:?} should fail");
        }
        assert!(EventKind::new("a".repeat(129)).is_err());
    }
}
