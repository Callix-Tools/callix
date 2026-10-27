//! Project macros that annotate a declaration and expand to nothing.
//!
//! `class MY_EXPORT Foo {}`, `int x GUARDED_BY(m);` and `API int (*f)(int);`
//! parse into the wrong shape because the grammar does not run the preprocessor.
//! The adapter therefore erases such macros before it parses: every byte of the
//! token (and, for a function-like macro, of its argument list) becomes a space.
//! Newlines are kept and nothing moves, so lines, byte columns and UTF-8 validity
//! are preserved and every span stays a position in the original file.

use std::borrow::Cow;
use std::collections::HashMap;

use super::visitor::MacroTable;

/// How one erasable macro is erased.
#[derive(Clone, Debug, PartialEq)]
pub struct Erase {
    /// Erased together with the balanced parentheses that follow it.
    pub function_like: bool,
    /// What stays in place of an object-like macro: empty, or the storage
    /// keywords (`extern`, `static`) its body carries. It always fits the name.
    pub replacement: String,
}

/// The macros of the project that may be erased, by name.
pub type Erasable = HashMap<String, Erase>;

/// Calling conventions, the inline family and `__extension__`: they say nothing
/// the graph records.
const NEUTRAL: &[&str] = &[
    "__extension__",
    "__cdecl",
    "__stdcall",
    "__fastcall",
    "__thiscall",
    "__vectorcall",
    "inline",
    "__inline",
    "__inline__",
    "__forceinline",
];

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
}

fn tokenize(body: &str) -> Option<Vec<Tok>> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(body[start..i].to_string()));
        } else if c == b'"' {
            i += 1;
            let start = i;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= b.len() {
                return None;
            }
            out.push(Tok::Str(body[start..i].to_string()));
            i += 1;
        } else if c.is_ascii() {
            out.push(Tok::Punct(c as char));
            i += 1;
        } else {
            return None;
        }
    }
    Some(out)
}

/// The index after the group that opens at `at` (`toks[at]` must be `open`).
fn skip_group(toks: &[Tok], at: usize, open: char, close: char) -> Option<usize> {
    if toks.get(at) != Some(&Tok::Punct(open)) {
        return None;
    }
    let mut depth = 0i32;
    let mut i = at;
    while i < toks.len() {
        if let Tok::Punct(c) = &toks[i] {
            if *c == open {
                depth += 1;
            } else if *c == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
        }
        i += 1;
    }
    None
}

/// The storage keywords a body consists of, or `None` when the body is anything
/// but annotations (empty, attributes, calling conventions, inline, `extern "C"`,
/// storage keywords and macros already known to be erasable).
fn classify_body(body: &str, known: &Erasable) -> Option<Vec<String>> {
    let toks = tokenize(body)?;
    let mut storage: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Ident(w) if matches!(w.as_str(), "__attribute__" | "__attribute" | "__declspec") => {
                i = skip_group(&toks, i + 1, '(', ')')?;
            }
            Tok::Punct('[') => {
                if toks.get(i + 1) != Some(&Tok::Punct('[')) {
                    return None;
                }
                i = skip_group(&toks, i, '[', ']')?;
            }
            Tok::Ident(w) if NEUTRAL.contains(&w.as_str()) => i += 1,
            Tok::Ident(w) if w == "extern" => {
                if let Some(Tok::Str(s)) = toks.get(i + 1) {
                    if s == "C" {
                        i += 2;
                        continue;
                    }
                    return None;
                }
                storage.push("extern".to_string());
                i += 1;
            }
            Tok::Ident(w) if w == "static" => {
                storage.push("static".to_string());
                i += 1;
            }
            Tok::Ident(w) => {
                let other = known.get(w.as_str())?;
                if other.function_like {
                    i = skip_group(&toks, i + 1, '(', ')')?;
                } else {
                    storage.extend(other.replacement.split_whitespace().map(String::from));
                    i += 1;
                }
            }
            _ => return None,
        }
    }
    Some(storage)
}

fn classify_name(name: &str, defs: &[super::visitor::MacroDef], known: &Erasable) -> Option<Erase> {
    let first = defs.first()?;
    let function_like = first.params.is_some();
    let mut storage: Option<Vec<String>> = None;
    for def in defs {
        if def.params.is_some() != function_like {
            return None;
        }
        let found = classify_body(&def.body, known)?;
        if let Some(previous) = &storage
            && *previous != found
        {
            return None;
        }
        storage = Some(found);
    }
    let storage = storage?;
    if function_like && !storage.is_empty() {
        return None;
    }
    let replacement = storage.join(" ");
    if replacement.len() > name.len() {
        return None;
    }
    Some(Erase { function_like, replacement })
}

/// The macros of the table that may be erased.
///
/// A name qualifies only when EVERY definition of it (every `#if` arm) is
/// erasable and all have the same form. Computed as a monotone fixed point that
/// starts from nothing, so `GUARDED_BY(x)` -> `THREAD_ANNOTATION(guarded_by(x))`
/// -> `__attribute__((x))` resolves, and a cycle simply never enters the set.
pub fn erasable(table: &MacroTable) -> Erasable {
    let mut known = Erasable::new();
    loop {
        let mut changed = false;
        for (name, defs) in table {
            if known.contains_key(name) {
                continue;
            }
            if let Some(erase) = classify_name(name, defs, &known) {
                known.insert(name.clone(), erase);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    known
}

/// `erasable` over several tables read as one: a name qualifies when every
/// definition of it, in any of the tables, is erasable. The defs are deduplicated
/// the way `declared_macros` does, and the result does not depend on table order.
pub fn erasable_in(tables: &[&MacroTable]) -> Erasable {
    if let [only] = tables {
        return erasable(only);
    }
    let mut merged = MacroTable::new();
    for table in tables {
        for (name, defs) in table.iter() {
            let entry = merged.entry(name.clone()).or_default();
            for def in defs {
                if !entry.contains(def) {
                    entry.push(def.clone());
                }
            }
        }
    }
    erasable(&merged)
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident_cont(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// From a `#` that starts a line to the end of the directive (the index of its
/// newline), following backslash continuations.
fn skip_directive(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        if s[i] == b'\n' {
            let mut j = i;
            if j > 0 && s[j - 1] == b'\r' {
                j -= 1;
            }
            if j > 0 && s[j - 1] == b'\\' {
                i += 1;
                continue;
            }
            break;
        }
        i += 1;
    }
    i
}

fn skip_block_comment(s: &[u8], mut i: usize) -> usize {
    i += 2;
    while i + 1 < s.len() && !(s[i] == b'*' && s[i + 1] == b'/') {
        i += 1;
    }
    (i + 2).min(s.len())
}

/// A string or character literal starting at `i`; an unterminated one ends at
/// its line.
fn skip_quoted(s: &[u8], mut i: usize) -> usize {
    let quote = s[i];
    i += 1;
    while i < s.len() {
        let c = s[i];
        if c == b'\\' {
            i += 2;
        } else if c == quote {
            return i + 1;
        } else if c == b'\n' {
            return i;
        } else {
            i += 1;
        }
    }
    s.len()
}

/// A preprocessing number, so `1'000` does not open a character literal.
fn skip_number(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        let c = s[i];
        // A digit separator (`1'000`) is part of the number; so is the sign of an exponent.
        let separator = c == b'\'' && i + 1 < s.len() && s[i + 1].is_ascii_alphanumeric();
        let exponent_sign = (c == b'+' || c == b'-') && i > 0 && matches!(s[i - 1], b'e' | b'E' | b'p' | b'P');
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || separator || exponent_sign {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// A raw string whose opening quote is at `i`.
fn skip_raw_string(s: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && j - i <= 17 && s[j] != b'(' && !s[j].is_ascii_whitespace() && s[j] != b')' && s[j] != b'\\' {
        j += 1;
    }
    if j >= s.len() || s[j] != b'(' {
        return i + 1;
    }
    let mut closing = vec![b')'];
    closing.extend_from_slice(&s[i + 1..j]);
    closing.push(b'"');
    let mut k = j + 1;
    while k + closing.len() <= s.len() {
        if s[k..k + closing.len()] == closing[..] {
            return k + closing.len();
        }
        k += 1;
    }
    s.len()
}

/// The index after the `)` that closes the `(` at `at`, if there is one.
fn balanced_end(s: &[u8], at: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = at;
    while k < s.len() {
        match s[k] {
            b'(' => {
                depth += 1;
                k += 1;
            }
            b')' => {
                depth -= 1;
                k += 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            b'"' | b'\'' => k = skip_quoted(s, k),
            b'/' if s.get(k + 1) == Some(&b'/') => {
                while k < s.len() && s[k] != b'\n' {
                    k += 1;
                }
            }
            b'/' if s.get(k + 1) == Some(&b'*') => k = skip_block_comment(s, k),
            _ => k += 1,
        }
    }
    None
}

/// `source` with every use of an erasable macro blanked out, byte for byte.
///
/// Preprocessor lines, comments, string and character literals, raw strings and
/// numbers are never touched. An object-like macro is overwritten by its
/// replacement (empty, or `extern`/`static`) padded with spaces; a function-like
/// one only when a `(` follows, and then through the matching `)`. Returns the
/// input itself, uncopied, when nothing matched.
pub fn blank<'a>(source: &'a [u8], erasable: &Erasable) -> Cow<'a, [u8]> {
    if erasable.is_empty() {
        return Cow::Borrowed(source);
    }
    let n = source.len();
    let mut out: Option<Vec<u8>> = None;
    let mut i = 0;
    let mut line_start = true;
    while i < n {
        let c = source[i];
        match c {
            b'\n' => {
                line_start = true;
                i += 1;
            }
            b' ' | b'\t' | b'\r' | 0x0b | 0x0c => i += 1,
            b'#' if line_start => i = skip_directive(source, i),
            b'/' if source.get(i + 1) == Some(&b'/') => {
                while i < n && source[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if source.get(i + 1) == Some(&b'*') => i = skip_block_comment(source, i),
            b'"' | b'\'' => {
                line_start = false;
                i = skip_quoted(source, i);
            }
            b'0'..=b'9' => {
                line_start = false;
                i = skip_number(source, i);
            }
            c if is_ident_start(c) => {
                line_start = false;
                let start = i;
                while i < n && is_ident_cont(source[i]) {
                    i += 1;
                }
                let word = &source[start..i];
                if i < n && source[i] == b'"' && matches!(word, b"R" | b"u8R" | b"uR" | b"UR" | b"LR") {
                    i = skip_raw_string(source, i);
                    continue;
                }
                let Some(erase) = std::str::from_utf8(word).ok().and_then(|w| erasable.get(w)) else {
                    continue;
                };
                if !erase.function_like {
                    let buf = out.get_or_insert_with(|| source.to_vec());
                    for (k, slot) in buf[start..i].iter_mut().enumerate() {
                        *slot = erase.replacement.as_bytes().get(k).copied().unwrap_or(b' ');
                    }
                } else {
                    let mut j = i;
                    while j < n && source[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < n
                        && source[j] == b'('
                        && let Some(end) = balanced_end(source, j)
                    {
                        let buf = out.get_or_insert_with(|| source.to_vec());
                        for slot in &mut buf[start..end] {
                            if *slot != b'\n' && *slot != b'\r' {
                                *slot = b' ';
                            }
                        }
                        i = end;
                    }
                }
            }
            _ => {
                line_start = false;
                i += 1;
            }
        }
    }
    match out {
        Some(bytes) => Cow::Owned(bytes),
        None => Cow::Borrowed(source),
    }
}

/// [`blank`] for text (a macro body about to be parsed).
pub fn blank_string(text: &str, erasable: &Erasable) -> String {
    match blank(text.as_bytes(), erasable) {
        Cow::Borrowed(_) => text.to_string(),
        Cow::Owned(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::visitor::MacroDef;
    use super::*;

    fn table(items: &[(&str, Option<&[&str]>, &str)]) -> MacroTable {
        let mut t = MacroTable::new();
        for (name, params, body) in items {
            t.entry(name.to_string()).or_default().push(MacroDef {
                params: params.map(|p| p.iter().map(|s| s.to_string()).collect()),
                variadic: false,
                body: body.to_string(),
            });
        }
        t
    }

    fn standard() -> Erasable {
        erasable(&table(&[
            ("EXPORT", None, ""),
            ("GUARDED_BY", Some(&["x"]), "THREAD(guarded_by(x))"),
            ("THREAD", Some(&["x"]), "__attribute__((x))"),
        ]))
    }

    #[test]
    fn fixed_point_follows_a_chain_of_macros() {
        let known = standard();
        assert!(known.contains_key("EXPORT"));
        assert!(known["GUARDED_BY"].function_like);
        assert!(known.contains_key("THREAD"));
    }

    #[test]
    fn one_non_annotation_arm_disqualifies() {
        let known = erasable(&table(&[("STATUS", None, ""), ("STATUS", None, "int")]));
        assert!(!known.contains_key("STATUS"));
        let mixed = erasable(&table(&[("M", None, ""), ("M", Some(&["x"]), "")]));
        assert!(!mixed.contains_key("M"));
    }

    #[test]
    fn a_cycle_terminates_and_is_not_erasable() {
        let known = erasable(&table(&[("A", None, "B"), ("B", None, "A")]));
        assert!(known.is_empty());
    }

    #[test]
    fn defs_split_between_tables_are_judged_together() {
        let own = table(&[("EXPORT", None, ""), ("GUARDED_BY", Some(&["x"]), "THREAD(guarded_by(x))")]);
        let foreign = table(&[
            ("EXPORT", None, "__attribute__((visibility(\"default\")))"),
            ("THREAD", Some(&["x"]), "__attribute__((x))"),
        ]);
        let known = erasable_in(&[&own, &foreign]);
        assert!(known.contains_key("EXPORT"));
        assert!(known.contains_key("GUARDED_BY"));
        assert!(known.contains_key("THREAD"));
        let bad = table(&[("EXPORT", None, "int")]);
        assert!(!erasable_in(&[&own, &bad]).contains_key("EXPORT"));
        assert!(!erasable_in(&[&bad, &own]).contains_key("EXPORT"));
        assert!(erasable_in(&[&own]).contains_key("EXPORT"));
    }

    #[test]
    fn storage_keyword_must_fit_the_name() {
        let known = erasable(&table(&[("LUA_API", None, "extern"), ("local", None, "static")]));
        assert_eq!(known["LUA_API"].replacement, "extern");
        assert!(!known.contains_key("local"));
        assert_eq!(blank(b"LUA_API int f(void);", &known).as_ref(), b"extern  int f(void);");
    }

    #[test]
    fn attribute_bodies_are_erasable() {
        let known = erasable(&table(&[
            ("VIS", None, "__attribute__((visibility(\"default\")))"),
            ("CXX11", None, "[[nodiscard]]"),
            ("CALL", None, "__stdcall"),
            ("REAL", None, "int"),
        ]));
        assert!(known.contains_key("VIS") && known.contains_key("CXX11") && known.contains_key("CALL"));
        assert!(!known.contains_key("REAL"));
    }

    #[test]
    fn blank_keeps_length_and_newlines() {
        let known = standard();
        let src = "class EXPORT Foo { int x GUARDED_BY(\n m) ; };\n";
        let out = blank(src.as_bytes(), &known);
        let text = std::str::from_utf8(&out).unwrap();
        assert_eq!(text.len(), src.len());
        assert_eq!(text.matches('\n').count(), 2);
        assert!(!text.contains("EXPORT") && !text.contains("GUARDED_BY"));
        assert!(text.starts_with("class "));
        assert!(text.contains("Foo { int x"));
        assert!(text.trim_end().ends_with("};"));
    }

    #[test]
    fn blank_returns_borrowed_when_nothing_matches() {
        let known = standard();
        assert!(matches!(blank(b"int f(void);", &known), Cow::Borrowed(_)));
        // A function-like name without a parenthesis is left alone.
        assert!(matches!(blank(b"int GUARDED_BY = 1;", &known), Cow::Borrowed(_)));
    }

    #[test]
    fn blank_skips_directives_comments_strings_and_numbers() {
        let known = standard();
        let src = "#define EXPORT \\\n  EXPORT\n// EXPORT\n/* EXPORT */\nchar *s = \"EXPORT\";\nauto r = R\"x(EXPORT)x\";\nint n = 1'000; int EXPORT_X;\n";
        assert!(matches!(blank(src.as_bytes(), &known), Cow::Borrowed(_)));
        let out = blank(b"int a = 1'000; EXPORT int b;", &known);
        assert!(!std::str::from_utf8(&out).unwrap().contains("EXPORT"));
    }
}
