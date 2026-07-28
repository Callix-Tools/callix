//! Boundaries between services, and the normalization of their keys.
//!
//! Cross-language matching only works if Python's
//! `@app.get("/users/{id}")` and TypeScript's `fetch("/users/1")` reduce to
//! the same key, so normalization lives in the core and is shared by every
//! adapter.

use indexmap::IndexMap;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

/// A single *port* on a cross-language boundary, found in the source.
///
/// A boundary is a contract between services that no compiler resolves: an
/// HTTP route, a gRPC method, a queue topic, a Temporal activity. Each side
/// of the contract (the server `exposes`, the client `consumes`) is a port.
///
/// Coordinates are 1-based and point at the port's site (a route decorator,
/// a `fetch` call, a `publish` call) so the adapter can match it to the
/// enclosing declaration.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all, from_py_object)]
#[derive(Clone)]
pub struct BoundaryRef {
    /// The boundary family: `http` | `grpc` | `queue` | `temporal`.
    pub mechanism: String,
    /// `server` (provides the contract) or `client` (consumes it).
    pub role: String,
    /// The normalized matching key, for example `GET /users/{}`.
    pub key: String,
    pub line: u32,
    pub col: u32,
    /// The extractor's confidence: 1.0 for a literal, less when inferred
    /// from context.
    pub confidence: f64,
    /// Human-readable context: method, path, topic, framework.
    pub detail: IndexMap<String, String>,
}

#[gen_stub_pymethods]
#[pymethods]
impl BoundaryRef {
    #[new]
    #[pyo3(signature = (mechanism, role, key, line, col, confidence = 1.0, detail = None))]
    fn new(
        mechanism: String,
        role: String,
        key: String,
        line: u32,
        col: u32,
        confidence: f64,
        detail: Option<IndexMap<String, String>>,
    ) -> Self {
        Self {
            mechanism,
            role,
            key,
            line,
            col,
            confidence,
            detail: detail.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "BoundaryRef({} {} {:?} at {}:{})",
            self.mechanism, self.role, self.key, self.line, self.col
        )
    }
}

/// Collapses a path parameter into `{}` if the segment is one.
fn collapse_params(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;

    while !rest.is_empty() {
        // {id} — FastAPI/Starlette
        if let Some(start) = rest.find('{')
            && let Some(end) = rest[start..].find('}')
        {
            out.push_str(&rest[..start]);
            out.push_str("{}");
            rest = &rest[start + end + 1..];
            continue;
        }
        break;
    }
    out.push_str(rest);

    // <int:id> — Flask
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { break };
        result.push_str(&rest[..start]);
        result.push_str("{}");
        rest = &rest[start + end + 1..];
    }
    result.push_str(rest);

    // :id is Express style, only at the start of a segment, so a colon
    // inside a segment (`/v1/users/123:activate`, `sha256:abc`) stays put.
    result
        .split('/')
        .enumerate()
        .map(|(i, segment)| {
            if i > 0 && segment.starts_with(':') && segment.len() > 1 {
                "{}".to_string()
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Reduces a route or URL to a key independent of host and parameters.
///
/// Strips scheme and host, query and fragment; collapses path parameters of
/// every style along with concrete numeric ids (`/users/1` has to meet
/// `/users/{}`); drops the trailing slash except at the root.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn normalize_http_path(raw: &str) -> String {
    let mut path = raw.trim().to_string();

    if let Some((_scheme, after)) = path.split_once("://") {
        path = match after.find('/') {
            Some(slash) => after[slash..].to_string(),
            None => "/".to_string(),
        };
    }
    path = path
        .split_once('?')
        .map_or(path.as_str(), |(before, _)| before)
        .to_string();
    path = path
        .split_once('#')
        .map_or(path.as_str(), |(before, _)| before)
        .to_string();
    if !path.starts_with('/') {
        path.insert(0, '/');
    }

    path = collapse_params(&path);

    // Numeric segments are parameters too.
    path = path
        .split('/')
        .map(|segment| {
            if !segment.is_empty() && segment.chars().all(char::is_numeric) {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    if path.len() > 1 {
        let trimmed = path.trim_end_matches('/');
        path = if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() };
    }
    path
}
