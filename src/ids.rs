//! Deterministic node IDs: the same input yields the same ID across runs,
//! which is what makes incremental updates and diffing possible.

use pyo3::prelude::*;
use pyo3_stub_gen::derive::gen_stub_pyfunction;
use sha2::{Digest, Sha256};

fn digest16(key: &str) -> String {
    let hash = Sha256::digest(key.as_bytes());
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn node_id(project_name: &str, qualified_name: &str, kind: &str) -> String {
    digest16(&format!("{project_name}::{kind}::{qualified_name}"))
}

pub fn boundary_id(mechanism: &str, key: &str) -> String {
    digest16(&format!("boundary::{mechanism}::{key}"))
}

/// Stable node ID: sha256("{project}::{kind}::{qualified_name}")[:16].
#[gen_stub_pyfunction]
#[pyfunction]
pub fn make_node_id(project_name: &str, qualified_name: &str, kind: &str) -> String {
    node_id(project_name, qualified_name, kind)
}

/// Stable boundary ID — independent of both language and project, so a
/// server in one language and a client in another produce the same node.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn make_boundary_id(mechanism: &str, key: &str) -> String {
    boundary_id(mechanism, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digests below are `hashlib.sha256(key.encode()).hexdigest()[:16]`
    /// over the documented format strings, computed independently in CPython.
    /// They are pinned so the formula cannot drift: an ID change silently
    /// invalidates every stored graph.
    #[test]
    fn node_id_is_the_documented_digest() {
        // sha256("proj::function::mod.fn")
        assert_eq!(make_node_id("proj", "mod.fn", "function"), "a153ff68a458153f");
        // The order is project, kind, qualified_name — not the argument order.
        assert_eq!(node_id("p", "q", "k"), "5006a78699e8efc7");
        assert_eq!(digest16("p::k::q"), "5006a78699e8efc7");
    }

    #[test]
    fn node_id_handles_empty_and_non_ascii_input() {
        // sha256("::::")
        assert_eq!(make_node_id("", "", ""), "d00d9be38948fb14");
        // Hashing is over UTF-8 bytes, so a non-ASCII name is stable too.
        assert_eq!(
            make_node_id("проект", "модуль.функция", "module"),
            "6ebab212e7948397"
        );
    }

    #[test]
    fn boundary_id_is_the_documented_digest() {
        // sha256("boundary::http::GET /users/{}")
        assert_eq!(make_boundary_id("http", "GET /users/{}"), "2c3c7cba2c035619");
        assert_eq!(make_boundary_id("", ""), "661add9eea6c91c5");
        assert_eq!(make_boundary_id("queue", "orders.créés"), "fa5cae387842009b");
    }

    /// Boundary IDs must not depend on project or language — that is what
    /// collapses a server and its cross-language client into one node.
    #[test]
    fn boundary_id_depends_only_on_mechanism_and_key() {
        assert_eq!(
            make_boundary_id("http", "GET /users/{}"),
            boundary_id("http", "GET /users/{}")
        );
        assert_ne!(
            make_boundary_id("http", "GET /users/{}"),
            make_boundary_id("grpc", "GET /users/{}")
        );
        // The `::` separator is not escaped, so two different splits of the
        // same concatenation do collide. Harmless in practice — a mechanism
        // is one of a fixed set of short words — but pinned here so the
        // ambiguity stays a known property rather than a surprise.
        assert_eq!(make_boundary_id("a::b", "c"), make_boundary_id("a", "b::c"));
    }

    #[test]
    fn digest_is_16_lowercase_hex_chars() {
        for key in ["", "x", "a::b::c", "ünïcøde"] {
            let digest = digest16(key);
            assert_eq!(digest.len(), 16, "key {key:?}");
            assert!(
                digest.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{digest} is not lowercase hex"
            );
        }
    }
}
