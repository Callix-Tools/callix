//! Boundaries between services, and the normalization of their keys.
//!
//! Cross-language matching only works if Python's
//! `@app.get("/users/{id}")` and TypeScript's `fetch("/users/1")` reduce to
//! the same key, so normalization lives in the core and is shared by every
//! adapter.

use indexmap::IndexMap;

use pyo3::prelude::*;
use pyo3::types::PyBytes;
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

/// Runs user-supplied boundary extractors over one file.
///
/// The protocol deliberately differs from graphlens's, which handed the
/// extractor a tree-sitter node: a Rust CST node cannot cross into Python. An
/// extractor here receives the source bytes and the project-relative path and
/// decides for itself how to read them — which is also what a mechanism
/// expressed by file layout, rather than by a call, actually needs.
///
/// ```python
/// class DjangoUrls:
///     def extract(self, source: bytes, file_path: str) -> list[BoundaryRef]:
///         ...
/// ```
///
/// Anything the extractor raises propagates: a broken extractor should be
/// noticed, not silently produce an empty graph.
pub(crate) fn run_extractors(
    py: Python<'_>,
    extractors: Option<&Py<PyAny>>,
    source: &[u8],
    file_path: &str,
) -> PyResult<Vec<BoundaryRef>> {
    let Some(extractors) = extractors else {
        return Ok(Vec::new());
    };
    let payload = PyBytes::new(py, source);
    let mut refs = Vec::new();
    for extractor in extractors.bind(py).try_iter()? {
        let found = extractor?.call_method1("extract", (&payload, file_path))?;
        for item in found.try_iter()? {
            refs.push(item?.extract::<BoundaryRef>()?);
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_every_path_parameter_style() {
        // {id} — FastAPI, Starlette, Spring, Laravel
        assert_eq!(normalize_http_path("/users/{id}"), "/users/{}");
        // :id — Express, gin, echo
        assert_eq!(normalize_http_path("/users/:id"), "/users/{}");
        // <converter:name> — Flask
        assert_eq!(normalize_http_path("/users/<int:id>"), "/users/{}");
        assert_eq!(normalize_http_path("/users/<uuid:id>/x"), "/users/{}/x");
        // A concrete id in a client URL has to meet the server's parameter.
        assert_eq!(normalize_http_path("/users/1"), "/users/{}");
        assert_eq!(normalize_http_path("/users/1/posts/2"), "/users/{}/posts/{}");
        // Several parameters, several styles, one path.
        assert_eq!(normalize_http_path("/a/<int:id>/b/{x}/c/:y"), "/a/{}/b/{}/c/{}");
        assert_eq!(normalize_http_path("/{a}{b}/c"), "/{}{}/c");
    }

    /// Two forms that are NOT collapsed. `$id` (shell-style interpolation in
    /// a template URL) and `*` / `*rest` (gin's catch-all) both stay
    /// verbatim, so a route written that way will not meet a client's
    /// concrete path. Pinned so the gap is a known one.
    #[test]
    fn dollar_and_wildcard_parameters_are_left_alone() {
        assert_eq!(normalize_http_path("/users/$id"), "/users/$id");
        assert_eq!(normalize_http_path("/users/*"), "/users/*");
        assert_eq!(normalize_http_path("/static/*filepath"), "/static/*filepath");
    }

    #[test]
    fn strips_scheme_host_query_and_fragment() {
        assert_eq!(
            normalize_http_path("https://api.example.com/v1/users/{id}"),
            "/v1/users/{}"
        );
        assert_eq!(normalize_http_path("/users?page=1&size=2"), "/users");
        assert_eq!(normalize_http_path("/users#frag"), "/users");
        // A URL with no path at all reduces to the root.
        assert_eq!(normalize_http_path("https://api.example.com"), "/");
        assert_eq!(normalize_http_path("http://host/"), "/");
    }

    #[test]
    fn drops_the_trailing_slash_except_at_the_root() {
        assert_eq!(normalize_http_path("/users/"), "/users");
        assert_eq!(normalize_http_path("/users/:id/"), "/users/{}");
        assert_eq!(normalize_http_path("/"), "/");
        assert_eq!(normalize_http_path("//"), "/");
    }

    #[test]
    fn normalizes_a_relative_path_and_trims_whitespace() {
        assert_eq!(normalize_http_path("users/1"), "/users/{}");
        assert_eq!(normalize_http_path("   /a/b   "), "/a/b");
        assert_eq!(normalize_http_path(""), "/");
    }

    #[test]
    fn an_already_normalized_path_is_a_fixed_point() {
        for path in ["/", "/users", "/users/{}", "/users/{}/posts/{}", "/a/b/c"] {
            assert_eq!(normalize_http_path(path), path, "{path} is not stable");
            assert_eq!(
                normalize_http_path(&normalize_http_path(path)),
                normalize_http_path(path)
            );
        }
    }

    /// A colon inside a segment is data, not a parameter: gRPC-style verbs
    /// (`/v1/users/123:activate`) and digests (`sha256:abc`) must survive.
    #[test]
    fn a_colon_inside_a_segment_is_not_a_parameter() {
        assert_eq!(normalize_http_path("/v1/users/123:activate"), "/v1/users/123:activate");
        assert_eq!(normalize_http_path("/sha256:abc"), "/sha256:abc");
        // A bare colon is not a name either.
        assert_eq!(normalize_http_path("/x/:/y"), "/x/:/y");
    }

    /// The cross-language merge is exactly this: pairs written in different
    /// frameworks must reduce to one key, or the boundary node splits in two.
    #[test]
    fn pairs_from_different_languages_agree() {
        let pairs = [
            ("/users/{id}", "/users/:id"),
            ("/users/{id}", "/users/<int:id>"),
            ("/users/{user_id}", "/users/{id}"),
            ("/users/1", "/users/{id}"),
            ("https://api.example.com/v1/users/{id}", "/v1/users/:id"),
            ("/users/", "/users"),
            ("/users?page=1", "/users/"),
        ];
        for (left, right) in pairs {
            assert_eq!(
                normalize_http_path(left),
                normalize_http_path(right),
                "{left} and {right} do not merge"
            );
        }
    }

    #[test]
    fn collapse_params_leaves_an_unclosed_delimiter_alone() {
        // An unterminated `{` or `<` must neither hang nor swallow the rest.
        assert_eq!(collapse_params("/users/{id"), "/users/{id");
        assert_eq!(collapse_params("/users/<int:id"), "/users/<int:id");
        assert_eq!(collapse_params("/a}b"), "/a}b");
    }
}
