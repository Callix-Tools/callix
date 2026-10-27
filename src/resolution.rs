//! How an edge came to point where it does.
//!
//! A resolver's answer is one of several quite different things, and a graph that
//! stores them all as "resolved" says more than it knows. Every `CALLS`,
//! `REFERENCES`, `HAS_TYPE` and `INHERITS_FROM` edge a resolution pass writes
//! carries `metadata["resolution"]`, one of:
//!
//! * `definition` - the backend located a definition in the project and the edge
//!   points at it. For a call, that definition is something that can be called.
//! * `value` - a call whose target is a variable, a parameter or a field: the
//!   backend found what the callee was *read from*, not what it is. The edge says
//!   "this calls `a`", and `a = f; a()` is still a call of `f` that nobody has
//!   drawn.
//! * `flow` - a call that landed on a variable, a parameter or a field, followed
//!   through assignments and call arguments to a callable it can be shown to hold
//!   (`via` names the holder). Flow-insensitive: a holder assigned two callables
//!   gets an edge to each. Written by the `flow` pass, never by a backend.
//! * `external` - the backend located the definition outside the project: the
//!   standard library, a dependency.
//! * `unlocated` - the backend answered, but the answer is not a place callix could
//!   bind to a node (a position inside an import statement, a file it does not
//!   know). The edge points at a synthetic external symbol keyed by the call site.
//! * `name` - matched by name in a symbol table, with no type information. Only
//!   the adapters whose backend is a table (PHP's own, the C family's) write it,
//!   and Rust's structure pass, for a `use` path or a `mod x;` it binds by path,
//!   and `ambiguous` accompanies it when several declarations carried the name.

use crate::py::prelude::*;
use crate::node::NodeKind;
use crate::relation::RelationKind;

/// The `metadata` key on an edge.
pub const KEY: &str = "resolution";
/// Set (to `true`) beside `name` when more than one declaration matched.
pub const AMBIGUOUS_KEY: &str = "ambiguous";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Definition,
    Value,
    Flow,
    External,
    Unlocated,
    Name,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Value => "value",
            Self::Flow => "flow",
            Self::External => "external",
            Self::Unlocated => "unlocated",
            Self::Name => "name",
        }
    }

    /// What a backend's answer amounts to.
    ///
    /// `bound` is the kind of node the answer was bound to, `None` when it was not
    /// bound to any; `origin` is the backend's classification of where the
    /// definition lives (`internal`, `stdlib`, `third_party`, `unknown`).
    pub fn classify(relation: RelationKind, bound: Option<NodeKind>, origin: &str) -> Self {
        match bound {
            Some(kind) if relation == RelationKind::Calls => match kind {
                NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute => Self::Value,
                // A position inside an import statement, a module, a file: the
                // backend pointed at where the name came from, not at the callee.
                NodeKind::Import | NodeKind::Module | NodeKind::File | NodeKind::TypeAlias => Self::Unlocated,
                _ => Self::Definition,
            },
            Some(_) => Self::Definition,
            None => match origin {
                "stdlib" | "third_party" => Self::External,
                _ => Self::Unlocated,
            },
        }
    }
}

/// How the `CALLS` edges of a graph were resolved, counted from the edges.
///
/// Read by the command line only; the extension module has no caller for it.
#[cfg_attr(feature = "python", allow(dead_code))]
#[derive(Debug, Clone, Copy, Default)]
pub struct CallCounts {
    pub total: usize,
    pub definition: usize,
    pub value: usize,
    pub flow: usize,
    pub external: usize,
    pub unlocated: usize,
    pub name: usize,
    /// Of `name`: edges where several declarations carried the name.
    pub ambiguous: usize,
}

#[cfg_attr(feature = "python", allow(dead_code))]
impl CallCounts {
    /// Counts the edges that record how they were resolved.
    pub(crate) fn of(py: Python<'_>, graph: &crate::graph::Graph) -> Self {
        let mut counts = Self::default();
        for relation in graph.relation_slice() {
            let relation = relation.get();
            if relation.kind != RelationKind::Calls {
                continue;
            }
            counts.total += 1;
            let metadata = relation.metadata.bind(py);
            let how = metadata
                .get_item(KEY)
                .ok()
                .flatten()
                .and_then(|v| v.extract::<String>().ok())
                .unwrap_or_default();
            match how.as_str() {
                "definition" => counts.definition += 1,
                "value" => counts.value += 1,
                "flow" => counts.flow += 1,
                "external" => counts.external += 1,
                "unlocated" => counts.unlocated += 1,
                "name" => {
                    counts.name += 1;
                    if metadata
                        .get_item(AMBIGUOUS_KEY)
                        .ok()
                        .flatten()
                        .and_then(|v| v.extract::<bool>().ok())
                        .unwrap_or(false)
                    {
                        counts.ambiguous += 1;
                    }
                }
                _ => {}
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_that_lands_on_a_variable_found_no_callee() {
        let r = Resolution::classify(RelationKind::Calls, Some(NodeKind::Variable), "internal");
        assert_eq!(r, Resolution::Value);
        let r = Resolution::classify(RelationKind::Calls, Some(NodeKind::Parameter), "internal");
        assert_eq!(r, Resolution::Value);
    }

    #[test]
    fn a_read_of_a_variable_is_a_definition() {
        let r = Resolution::classify(RelationKind::References, Some(NodeKind::Variable), "internal");
        assert_eq!(r, Resolution::Definition);
    }

    #[test]
    fn a_reference_to_a_field_or_a_struct_is_a_definition() {
        for kind in [NodeKind::Attribute, NodeKind::Class, NodeKind::Variable] {
            let r = Resolution::classify(RelationKind::References, Some(kind), "internal");
            assert_eq!(r, Resolution::Definition);
        }
    }

    #[test]
    fn a_call_of_a_function_is_a_definition() {
        let r = Resolution::classify(RelationKind::Calls, Some(NodeKind::Function), "internal");
        assert_eq!(r, Resolution::Definition);
    }

    #[test]
    fn outside_the_project_is_external_and_an_unbindable_answer_is_unlocated() {
        assert_eq!(Resolution::classify(RelationKind::Calls, None, "stdlib"), Resolution::External);
        assert_eq!(Resolution::classify(RelationKind::Calls, None, "third_party"), Resolution::External);
        assert_eq!(Resolution::classify(RelationKind::Calls, None, "internal"), Resolution::Unlocated);
        assert_eq!(Resolution::classify(RelationKind::Calls, None, "unknown"), Resolution::Unlocated);
    }
}
