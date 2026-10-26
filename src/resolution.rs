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
//! * `external` - the backend located the definition outside the project: the
//!   standard library, a dependency.
//! * `unlocated` - the backend answered, but the answer is not a place callix could
//!   bind to a node (a position inside an import statement, a file it does not
//!   know). The edge points at a synthetic external symbol keyed by the call site.
//! * `name` - matched by name in a symbol table, with no type information. Only
//!   the adapters whose backend is a table (PHP's own, the C family's) write it,
//!   and `ambiguous` accompanies it when several declarations carried the name.

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
    External,
    Unlocated,
    Name,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Value => "value",
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
