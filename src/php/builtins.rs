//! Names that are PHP syntax, not symbols.
//!
//! `isset`, `empty`, `eval` and `die` parse as ordinary `function_call_expression`s,
//! and every magic constant (`__DIR__`, `__CLASS__`, ...) parses as a bare `name`.
//! None of them is declared anywhere, so left alone each one became an
//! `EXTERNAL_SYMBOL` with an edge to it. The grammar already gives most other
//! constructs (`exit`, `unset`, `list`, `print`, `echo`, `include`, ...) their own
//! node; the list is complete on purpose, so a grammar change that turns them into
//! calls cannot reintroduce the edges. Matching is case-insensitive, as in PHP.

/// Constructs that look like a call of a function when written `name(...)`.
const LANGUAGE_CONSTRUCTS: &[&str] = &[
    "isset",
    "empty",
    "eval",
    "exit",
    "die",
    "unset",
    "list",
    "array",
    "print",
    "echo",
    "include",
    "include_once",
    "require",
    "require_once",
];

/// Constants the engine defines and no program can declare.
/// `__COMPILER_HALT_OFFSET__` is technically an ordinary constant but is never declarable.
const MAGIC_CONSTANTS: &[&str] = &[
    "__line__",
    "__file__",
    "__dir__",
    "__function__",
    "__class__",
    "__trait__",
    "__method__",
    "__namespace__",
    "__property__",
    "__compiler_halt_offset__",
];

pub(super) fn is_language_construct(name: &str) -> bool {
    LANGUAGE_CONSTRUCTS.iter().any(|c| c.eq_ignore_ascii_case(name))
}

pub(super) fn is_magic_constant(name: &str) -> bool {
    MAGIC_CONSTANTS.iter().any(|c| c.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_match_case_insensitively() {
        assert!(is_language_construct("isset"));
        assert!(is_language_construct("ISSET"));
        assert!(is_language_construct("Die"));
        assert!(!is_language_construct("is_set"));
        assert!(!is_language_construct("issetx"));
    }

    #[test]
    fn magic_constants_match_case_insensitively() {
        assert!(is_magic_constant("__DIR__"));
        assert!(is_magic_constant("__dir__"));
        assert!(!is_magic_constant("__DIRX__"));
    }
}
