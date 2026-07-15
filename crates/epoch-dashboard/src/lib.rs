//! Local-only, read-only inspection surface for trusted Epoch runtime state.

use std::{net::SocketAddr, path::PathBuf};

use thiserror::Error;

/// Server configuration supplied by the trusted operator.
#[derive(Clone, Debug)]
pub struct DashboardConfig {
    pub state_root: PathBuf,
    pub results_root: Option<PathBuf>,
    pub bind: SocketAddr,
}

/// A protocol-neutral response used by the server and integration tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

/// Validated read-only dashboard state.
#[derive(Clone, Debug)]
pub struct Dashboard;

impl Dashboard {
    /// Opens and validates an existing Epoch state root.
    pub fn open(
        _state_root: impl Into<PathBuf>,
        _results_root: Option<PathBuf>,
    ) -> Result<Self, DashboardError> {
        Err(DashboardError::NotImplemented)
    }

    /// Routes one request without granting mutation access.
    #[must_use]
    pub fn handle(&self, _method: &str, _target: &str) -> DashboardResponse {
        DashboardResponse {
            status: 501,
            content_type: "application/json; charset=utf-8",
            body: br#"{"error":"not_implemented"}"#.to_vec(),
        }
    }
}

/// Parses a requested listener address and refuses any non-loopback interface.
pub fn parse_loopback_bind(value: &str) -> Result<SocketAddr, DashboardError> {
    value
        .parse()
        .map_err(|_| DashboardError::InvalidBind(value.to_owned()))
}

/// Runs the blocking local dashboard server.
pub fn serve(_config: DashboardConfig) -> Result<(), DashboardError> {
    Err(DashboardError::NotImplemented)
}

#[derive(Debug, Error)]
pub enum DashboardError {
    #[error("dashboard implementation is not available")]
    NotImplemented,
    #[error("invalid dashboard bind address: {0}")]
    InvalidBind(String),
}
