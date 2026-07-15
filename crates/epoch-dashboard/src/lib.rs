//! Local-only, read-only inspection surface for trusted Epoch runtime state.

mod assets;
mod backends;
mod benchmarks;
mod read_model;

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};

use benchmarks::BenchmarkReader;
use read_model::{MAX_STANDARD_PAGE, MAX_TIMELINE_PAGE, ReadModel, StateError, TimelineFilters};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;
use tiny_http::{Header, Method, Response, Server, StatusCode};

const MAX_TARGET_BYTES: usize = 2_048;
const MAX_QUERY_BYTES: usize = 1_024;
const MAX_QUERY_PAIRS: usize = 12;
const MAX_OFFSET: u64 = 1_000_000;

const SECURITY_HEADERS: [(&str, &str); 8] = [
    ("Cache-Control", "no-store"),
    (
        "Content-Security-Policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; object-src 'none'",
    ),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    (
        "Permissions-Policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    ),
    ("Referrer-Policy", "no-referrer"),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
];

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
    pub headers: Vec<(&'static str, &'static str)>,
    pub body: Vec<u8>,
}

/// Validated read-only dashboard state.
#[derive(Clone, Debug)]
pub struct Dashboard {
    model: ReadModel,
    benchmarks: BenchmarkReader,
}

impl Dashboard {
    /// Opens and validates an existing Epoch state root without creating or migrating it.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, corrupt, symlinked, or schema-incompatible trusted state.
    pub fn open(
        state_root: impl Into<PathBuf>,
        results_root: Option<PathBuf>,
    ) -> Result<Self, DashboardError> {
        let state_root = state_root.into();
        let model = ReadModel::open(&state_root)?;
        let results_root = results_root.unwrap_or_else(|| state_root.join("benchmarks"));
        Ok(Self {
            model,
            benchmarks: BenchmarkReader::new(results_root),
        })
    }

    /// Routes one request without granting mutation access.
    #[must_use]
    pub fn handle(&self, method: &str, target: &str) -> DashboardResponse {
        if method != "GET" && method != "HEAD" {
            let mut response =
                json_error(405, "method_not_allowed", "Only GET and HEAD are available");
            response.headers.push(("Allow", "GET, HEAD"));
            return response;
        }
        let parsed = match ParsedTarget::parse(target) {
            Ok(parsed) => parsed,
            Err(error) => return request_error(error),
        };
        let mut response = self.route(&parsed);
        if method == "HEAD" {
            response.body.clear();
        }
        response
    }

    fn route(&self, target: &ParsedTarget) -> DashboardResponse {
        match target.path.as_str() {
            "/" if target.query.is_empty() => {
                static_response("text/html; charset=utf-8", assets::INDEX_HTML)
            }
            "/assets/app.css" if target.query.is_empty() => {
                static_response("text/css; charset=utf-8", assets::APP_CSS)
            }
            "/assets/app.js" if target.query.is_empty() => {
                static_response("text/javascript; charset=utf-8", assets::APP_JS)
            }
            "/api/v1/backends" if target.query.is_empty() => json_response(&backends::detect()),
            "/api/v1/benchmarks" if target.query.is_empty() => {
                json_response(&self.benchmarks.read())
            }
            "/api/v1/sessions" => self.sessions(target),
            _ => self.dynamic_route(target),
        }
    }

    fn dynamic_route(&self, target: &ParsedTarget) -> DashboardResponse {
        let segments = target
            .path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        match segments.as_slice() {
            ["api", "v1", "sessions", session_id] if target.query.is_empty() => {
                model_response(self.model.session(session_id))
            }
            ["api", "v1", "sessions", session_id, "epochs"] => self.epochs(target, session_id),
            ["api", "v1", "sessions", session_id, "diffs"] => self.diffs(target, session_id),
            ["api", "v1", "sessions", session_id, "capabilities"] if target.query.is_empty() => {
                model_response(self.model.capabilities(session_id))
            }
            ["api", "v1", "sessions", session_id, "effects"] if target.query.is_empty() => {
                model_response(self.model.effects(session_id))
            }
            ["api", "v1", "branches", branch_id, "timeline"] => self.timeline(target, branch_id),
            _ => json_error(
                404,
                "not_found",
                "The requested dashboard resource does not exist",
            ),
        }
    }

    fn sessions(&self, target: &ParsedTarget) -> DashboardResponse {
        if let Err(error) = target.allow(&["status", "offset", "limit"]) {
            return request_error(error);
        }
        let status = target.get("status");
        if status.is_some_and(|value| !SESSION_STATES.contains(&value)) {
            return request_error(RequestError::InvalidQuery);
        }
        let (offset, limit) = match target.page(MAX_STANDARD_PAGE, 50) {
            Ok(page) => page,
            Err(error) => return request_error(error),
        };
        model_response(self.model.sessions(status, offset, limit))
    }

    fn timeline(&self, target: &ParsedTarget, branch_id: &str) -> DashboardResponse {
        if let Err(error) = target.allow(&["actor", "kind", "status", "offset", "limit"]) {
            return request_error(error);
        }
        let actor = target.get("actor");
        let status = target.get("status");
        let kind = target.get("kind");
        if actor.is_some_and(|value| !EVENT_ACTORS.contains(&value))
            || status.is_some_and(|value| !EVENT_STATUSES.contains(&value))
            || kind.is_some_and(|value| !valid_event_kind(value))
        {
            return request_error(RequestError::InvalidQuery);
        }
        let (offset, limit) = match target.page(MAX_TIMELINE_PAGE, 50) {
            Ok(page) => page,
            Err(error) => return request_error(error),
        };
        model_response(self.model.timeline(
            branch_id,
            &TimelineFilters {
                actor,
                kind,
                status,
            },
            offset,
            limit,
        ))
    }

    fn epochs(&self, target: &ParsedTarget, session_id: &str) -> DashboardResponse {
        if let Err(error) = target.allow(&["offset", "limit"]) {
            return request_error(error);
        }
        let (offset, limit) = match target.page(MAX_STANDARD_PAGE, 50) {
            Ok(page) => page,
            Err(error) => return request_error(error),
        };
        model_response(self.model.epochs(session_id, offset, limit))
    }

    fn diffs(&self, target: &ParsedTarget, session_id: &str) -> DashboardResponse {
        if let Err(error) = target.allow(&["offset", "limit"]) {
            return request_error(error);
        }
        let (offset, limit) = match target.page(50, 25) {
            Ok(page) => page,
            Err(error) => return request_error(error),
        };
        model_response(self.model.diffs(session_id, offset, limit))
    }
}

/// Parses a requested listener address and refuses any non-loopback interface.
///
/// # Errors
///
/// Returns an error when the address is invalid or exposes the unauthenticated dashboard beyond
/// the local host.
pub fn parse_loopback_bind(value: &str) -> Result<SocketAddr, DashboardError> {
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| DashboardError::InvalidBind)?;
    if !address.ip().is_loopback() {
        return Err(DashboardError::NonLoopbackBind);
    }
    Ok(address)
}

/// Runs the blocking local dashboard server.
///
/// # Errors
///
/// Returns an error when trusted state cannot be opened, the bind is not loopback, the listener
/// cannot start, or a response cannot be written.
pub fn serve(config: DashboardConfig) -> Result<(), DashboardError> {
    if !config.bind.ip().is_loopback() {
        return Err(DashboardError::NonLoopbackBind);
    }
    let dashboard = Dashboard::open(config.state_root, config.results_root)?;
    let server =
        Server::http(config.bind).map_err(|error| DashboardError::Server(error.to_string()))?;
    for request in server.incoming_requests() {
        let method = match request.method() {
            Method::Get => "GET",
            Method::Head => "HEAD",
            _ => request.method().as_str(),
        };
        let dashboard_response = dashboard.handle(method, request.url());
        let mut response = Response::from_data(dashboard_response.body)
            .with_status_code(StatusCode(dashboard_response.status));
        response.add_header(header("Content-Type", dashboard_response.content_type));
        for (name, value) in dashboard_response.headers {
            response.add_header(header(name, value));
        }
        request
            .respond(response)
            .map_err(|error| DashboardError::Server(error.to_string()))?;
    }
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("compiled dashboard headers are valid")
}

fn model_response<T: Serialize>(result: Result<T, StateError>) -> DashboardResponse {
    match result {
        Ok(value) => json_response(&value),
        Err(StateError::NotFound(_)) => json_error(
            404,
            "not_found",
            "The requested runtime record does not exist",
        ),
        Err(StateError::InvalidIdentifier | StateError::InvalidPage) => {
            json_error(400, "invalid_request", "The request parameters are invalid")
        }
        Err(_) => json_error(
            503,
            "state_unavailable",
            "Trusted runtime state could not be read safely",
        ),
    }
}

fn static_response(content_type: &'static str, body: &'static str) -> DashboardResponse {
    response(200, content_type, body.as_bytes().to_vec())
}

fn json_response(value: &impl Serialize) -> DashboardResponse {
    match safe_json(value) {
        Ok(body) => response(200, "application/json; charset=utf-8", body),
        Err(()) => json_error(
            500,
            "serialization_failed",
            "The dashboard response could not be encoded",
        ),
    }
}

fn json_error(status: u16, code: &'static str, message: &'static str) -> DashboardResponse {
    let value = json!({ "error": code, "message": message });
    let body =
        safe_json(&value).unwrap_or_else(|()| br#"{"error":"serialization_failed"}"#.to_vec());
    response(status, "application/json; charset=utf-8", body)
}

fn response(status: u16, content_type: &'static str, body: Vec<u8>) -> DashboardResponse {
    DashboardResponse {
        status,
        content_type,
        headers: SECURITY_HEADERS
            .iter()
            .map(|(name, value)| (*name, *value))
            .collect(),
        body,
    }
}

fn safe_json(value: &impl Serialize) -> Result<Vec<u8>, ()> {
    serde_json::to_string(value)
        .map(|encoded| {
            encoded
                .replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029")
                .into_bytes()
        })
        .map_err(|_| ())
}

fn request_error(error: RequestError) -> DashboardResponse {
    let message = match error {
        RequestError::TargetTooLong => "The request target exceeds the dashboard limit",
        RequestError::InvalidPath => "The request path is invalid",
        RequestError::InvalidQuery => "The query parameters are invalid",
    };
    json_error(400, "invalid_request", message)
}

#[derive(Debug)]
struct ParsedTarget {
    path: String,
    query: BTreeMap<String, String>,
}

impl ParsedTarget {
    fn parse(target: &str) -> Result<Self, RequestError> {
        if target.len() > MAX_TARGET_BYTES {
            return Err(RequestError::TargetTooLong);
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        validate_path(path)?;
        if query.len() > MAX_QUERY_BYTES || query.contains('#') {
            return Err(RequestError::InvalidQuery);
        }
        let mut parsed_query = BTreeMap::new();
        if !query.is_empty() {
            for (index, pair) in query.split('&').enumerate() {
                if index >= MAX_QUERY_PAIRS || pair.is_empty() {
                    return Err(RequestError::InvalidQuery);
                }
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                let key = percent_decode(key)?;
                let value = percent_decode(value)?;
                if key.is_empty()
                    || key.len() > 32
                    || value.len() > 256
                    || parsed_query.insert(key, value).is_some()
                {
                    return Err(RequestError::InvalidQuery);
                }
            }
        }
        Ok(Self {
            path: path.to_owned(),
            query: parsed_query,
        })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.query.get(name).map(String::as_str)
    }

    fn allow(&self, names: &[&str]) -> Result<(), RequestError> {
        if self.query.keys().all(|key| names.contains(&key.as_str())) {
            Ok(())
        } else {
            Err(RequestError::InvalidQuery)
        }
    }

    fn page(&self, maximum: usize, default: usize) -> Result<(u64, usize), RequestError> {
        let offset = self
            .get("offset")
            .map(str::parse::<u64>)
            .transpose()
            .map_err(|_| RequestError::InvalidQuery)?
            .unwrap_or(0);
        let limit = self
            .get("limit")
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| RequestError::InvalidQuery)?
            .unwrap_or(default);
        if offset > MAX_OFFSET || !(1..=maximum).contains(&limit) {
            return Err(RequestError::InvalidQuery);
        }
        Ok((offset, limit))
    }
}

fn validate_path(path: &str) -> Result<(), RequestError> {
    if !path.starts_with('/')
        || path.contains('%')
        || path.contains('\\')
        || path.contains("//")
        || path.contains('#')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
    {
        Err(RequestError::InvalidPath)
    } else {
        Ok(())
    }
}

fn percent_decode(value: &str) -> Result<String, RequestError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = bytes.get(index + 1).and_then(|byte| hex(*byte));
                let low = bytes.get(index + 2).and_then(|byte| hex(*byte));
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(RequestError::InvalidQuery);
                };
                output.push((high << 4) | low);
                index += 3;
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    let decoded = String::from_utf8(output).map_err(|_| RequestError::InvalidQuery)?;
    if decoded.contains('\0') || decoded.chars().any(char::is_control) {
        Err(RequestError::InvalidQuery)
    } else {
        Ok(decoded)
    }
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn valid_event_kind(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_')
        })
}

const SESSION_STATES: &[&str] = &[
    "created",
    "starting",
    "running",
    "suspended",
    "checkpointing",
    "restoring",
    "completed",
    "failed",
];
const EVENT_ACTORS: &[&str] = &["agent", "supervisor", "tool", "gateway", "operator"];
const EVENT_STATUSES: &[&str] = &["started", "succeeded", "failed", "denied", "unknown"];

#[derive(Clone, Copy, Debug)]
enum RequestError {
    TargetTooLong,
    InvalidPath,
    InvalidQuery,
}

#[derive(Debug, Error)]
pub enum DashboardError {
    #[error("dashboard bind must be an IP socket address such as 127.0.0.1:8080")]
    InvalidBind,
    #[error("dashboard refuses non-loopback binds because it has no authentication boundary")]
    NonLoopbackBind,
    #[error(transparent)]
    State(#[from] StateError),
    #[error("dashboard server failed: {0}")]
    Server(String),
}

impl DashboardError {
    #[must_use]
    pub const fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidBind
                | Self::NonLoopbackBind
                | Self::State(
                    StateError::MissingStateRoot
                        | StateError::MissingDatabase
                        | StateError::InvalidStateRoot
                        | StateError::InvalidDatabaseFile
                )
        )
    }
}
