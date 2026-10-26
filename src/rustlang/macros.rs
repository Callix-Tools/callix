//! `macro_rules!` expansion, at the level of tokens.
//!
//! The calls a macro makes are the calls its expansion makes. The rules are matched
//! against the invocation's tokens the way the compiler would, for the shapes that
//! matter to a call graph: literal tokens, the usual fragment specifiers, groups and
//! one level of repetition. Every token keeps the place it was written - in the
//! invocation, for what an argument supplied, or in the macro's body - so that a
//! callee that came from an argument can be answered by the resolver where it is
//! written.
//!
//! What it does not do: hygiene, `$crate`, nested repetition, macros defined in
//! another file than the one that uses them, and procedural macros, which are not
//! `macro_rules!` and are invisible here.

use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use crate::span::Span;
use crate::ts;

/// A token and where it was written.
#[derive(Clone, Debug)]
pub struct Tok {
    pub text: String,
    pub span: Span,
    /// The token was supplied by the invocation, not written in the macro's body.
    pub from_args: bool,
}

enum Pattern {
    Lit(String),
    Bind { name: String, fragment: String },
    Repeat { items: Vec<Pattern>, separator: Option<String>, op: char },
    Group { open: String, close: String, items: Vec<Pattern> },
}

enum Body {
    Tok(Tok),
    Var(String),
    Repeat { items: Vec<Body>, separator: Option<Tok>, },
}

#[derive(Clone, Debug)]
enum Bound {
    Tokens(Vec<Tok>),
    Each(Vec<Bound>),
}

pub struct MacroRules {
    rules: Vec<(Vec<Pattern>, Vec<Body>)>,
}

/// The name and the rules of a `macro_rules!` definition.
pub fn parse_definition(definition: TsNode<'_>, source: &[u8]) -> Option<(String, MacroRules)> {
    let name = ts::text(definition.child_by_field_name("name").or_else(|| ts::child_of_type(definition, "identifier"))?, source)
        .into_owned();
    let mut rules = Vec::new();
    for rule in ts::children(definition).into_iter().filter(|c| c.kind() == "macro_rule") {
        let pattern = ts::child_of_type(rule, "token_tree_pattern")?;
        let body = ts::children(rule).into_iter().find(|c| c.kind() == "token_tree")?;
        let pattern_items = parse_pattern_group(pattern, source)?;
        // The outer delimiters of the body are not part of the expansion.
        let mut body_items = Vec::new();
        for child in ts::children(body) {
            if !matches!(child.kind(), "{" | "}" | "(" | ")" | "[" | "]") || child.parent().map(|p| p.id()) != Some(body.id()) {
                body_items.extend(parse_body(child, source));
            }
        }
        rules.push((pattern_items, body_items));
    }
    Some((name, MacroRules { rules }))
}

fn parse_pattern_group(node: TsNode<'_>, source: &[u8]) -> Option<Vec<Pattern>> {
    let mut items = Vec::new();
    let children = ts::children(node);
    // Skip the delimiters of the group itself.
    for child in children.iter().skip(1).take(children.len().saturating_sub(2)) {
        items.push(parse_pattern(*child, source)?);
    }
    Some(items)
}

fn parse_pattern(node: TsNode<'_>, source: &[u8]) -> Option<Pattern> {
    Some(match node.kind() {
        "token_binding_pattern" => {
            let name = ts::child_of_type(node, "metavariable")?;
            let fragment = ts::child_of_type(node, "fragment_specifier")?;
            Pattern::Bind {
                name: ts::text(name, source).trim_start_matches('$').to_string(),
                fragment: ts::text(fragment, source).into_owned(),
            }
        }
        "token_repetition_pattern" => {
            let children = ts::children(node);
            // `$ ( items ) sep? op`
            let open = children.iter().position(|c| c.kind() == "(")?;
            let close = children.iter().rposition(|c| c.kind() == ")")?;
            let mut items = Vec::new();
            for child in &children[open + 1..close] {
                items.push(parse_pattern(*child, source)?);
            }
            let after: Vec<&TsNode<'_>> = children[close + 1..].iter().collect();
            let op = after.last().map(|c| ts::text(**c, source).chars().next().unwrap_or('*')).unwrap_or('*');
            let separator = if after.len() >= 2 { Some(ts::text(*after[0], source).into_owned()) } else { None };
            Pattern::Repeat { items, separator, op }
        }
        "token_tree_pattern" => {
            let children = ts::children(node);
            let open = ts::text(*children.first()?, source).into_owned();
            let close = ts::text(*children.last()?, source).into_owned();
            Pattern::Group { open, close, items: parse_pattern_group(node, source)? }
        }
        _ => Pattern::Lit(ts::text(node, source).into_owned()),
    })
}

fn parse_body(node: TsNode<'_>, source: &[u8]) -> Vec<Body> {
    match node.kind() {
        "metavariable" => vec![Body::Var(ts::text(node, source).trim_start_matches('$').to_string())],
        "token_repetition" => {
            let children = ts::children(node);
            let (Some(open), Some(close)) = (
                children.iter().position(|c| c.kind() == "("),
                children.iter().rposition(|c| c.kind() == ")"),
            ) else {
                return Vec::new();
            };
            let mut items = Vec::new();
            for child in &children[open + 1..close] {
                items.extend(parse_body(*child, source));
            }
            let after = &children[close + 1..];
            let separator = if after.len() >= 2 {
                Some(Tok { text: ts::text(after[0], source).into_owned(), span: ts::span(after[0]), from_args: false })
            } else {
                None
            };
            vec![Body::Repeat { items, separator }]
        }
        "token_tree" => ts::children(node).into_iter().flat_map(|c| parse_body(c, source)).collect(),
        // `$` alone, which introduces a metavariable or a repetition.
        "$" => Vec::new(),
        _ if node.child_count() == 0 => vec![Body::Tok(Tok {
            text: ts::text(node, source).into_owned(),
            span: ts::span(node),
            from_args: false,
        })],
        _ => ts::children(node).into_iter().flat_map(|c| parse_body(c, source)).collect(),
    }
}

/// The tokens of a node, delimiters included, each with its place.
pub fn flatten(node: TsNode<'_>, source: &[u8], from_args: bool, out: &mut Vec<Tok>) {
    if node.child_count() == 0 {
        out.push(Tok { text: ts::text(node, source).into_owned(), span: ts::span(node), from_args });
        return;
    }
    for child in ts::children(node) {
        flatten(child, source, from_args, out);
    }
}

fn is_open(text: &str) -> bool {
    matches!(text, "(" | "[" | "{")
}

fn is_close(text: &str) -> bool {
    matches!(text, ")" | "]" | "}")
}

/// The end of the balanced group that opens at `start`.
fn group_end(tokens: &[Tok], start: usize) -> usize {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if is_open(&token.text) {
            depth += 1;
        } else if is_close(&token.text) {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return index + 1;
            }
        }
    }
    tokens.len()
}

type Bindings = HashMap<String, Bound>;

fn match_items(items: &[Pattern], tokens: &[Tok], mut at: usize, bindings: &mut Bindings) -> Option<usize> {
    for (index, item) in items.iter().enumerate() {
        match item {
            Pattern::Lit(text) => {
                if tokens.get(at)?.text != *text {
                    return None;
                }
                at += 1;
            }
            Pattern::Group { open, close, items: inner } => {
                if tokens.get(at)?.text != *open {
                    return None;
                }
                let end = group_end(tokens, at);
                if tokens.get(end - 1)?.text != *close {
                    return None;
                }
                let consumed = match_items(inner, &tokens[..end - 1], at + 1, bindings)?;
                if consumed != end - 1 {
                    return None;
                }
                at = end;
            }
            Pattern::Bind { name, fragment } => {
                let end = fragment_end(fragment, tokens, at, items.get(index + 1))?;
                bindings.insert(name.clone(), Bound::Tokens(tokens[at..end].to_vec()));
                at = end;
            }
            Pattern::Repeat { items: inner, separator, op } => {
                let mut each: Vec<Bindings> = Vec::new();
                loop {
                    if *op == '?' && each.len() == 1 {
                        break;
                    }
                    let mut local = Bindings::new();
                    let Some(next) = match_items(inner, tokens, at, &mut local) else {
                        break;
                    };
                    if next == at {
                        break;
                    }
                    each.push(local);
                    at = next;
                    match separator {
                        Some(sep) if tokens.get(at).is_some_and(|t| t.text == *sep) => {
                            // A separator with nothing after it ends the repetition.
                            if at + 1 < tokens.len() {
                                at += 1;
                            } else {
                                break;
                            }
                        }
                        Some(_) => break,
                        None => {}
                    }
                }
                if *op == '+' && each.is_empty() {
                    return None;
                }
                // Each variable of the repetition is bound to one entry per round.
                let mut names: Vec<String> = Vec::new();
                for round in &each {
                    for key in round.keys() {
                        if !names.contains(key) {
                            names.push(key.clone());
                        }
                    }
                }
                collect_repeat_names(inner, &mut names);
                for name in names {
                    let entries = each.iter().filter_map(|round| round.get(&name).cloned()).collect();
                    bindings.insert(name, Bound::Each(entries));
                }
            }
        }
    }
    Some(at)
}

fn collect_repeat_names(items: &[Pattern], names: &mut Vec<String>) {
    for item in items {
        match item {
            Pattern::Bind { name, .. } if !names.contains(name) => names.push(name.clone()),
            Pattern::Repeat { items, .. } | Pattern::Group { items, .. } => collect_repeat_names(items, names),
            _ => {}
        }
    }
}

/// Where a fragment ends. A fragment that is one token takes one; the others take
/// everything up to the literal that follows them in the pattern, or up to a
/// separator at the group's own depth.
fn fragment_end(fragment: &str, tokens: &[Tok], at: usize, next: Option<&Pattern>) -> Option<usize> {
    let first = tokens.get(at)?;
    match fragment {
        "ident" | "lifetime" | "literal" => Some(at + 1),
        "tt" => Some(if is_open(&first.text) { group_end(tokens, at) } else { at + 1 }),
        "block" => (first.text == "{").then(|| group_end(tokens, at)),
        _ => {
            let stop: Option<&str> = match next {
                Some(Pattern::Lit(text)) => Some(text.as_str()),
                _ => None,
            };
            let mut index = at;
            while index < tokens.len() {
                let token = &tokens[index];
                if is_open(&token.text) {
                    index = group_end(tokens, index);
                    continue;
                }
                if stop.is_some_and(|s| token.text == s) || (stop.is_none() && matches!(token.text.as_str(), "," | ";" | "=>")) {
                    break;
                }
                index += 1;
            }
            (index > at).then_some(index)
        }
    }
}

fn transcribe(items: &[Body], bindings: &Bindings, out: &mut Vec<Tok>) {
    for item in items {
        match item {
            Body::Tok(token) => out.push(token.clone()),
            // (a repeated variable used outside its repetition, or an unbound one, is nothing)
            Body::Var(name) => {
                if let Some(Bound::Tokens(tokens)) = bindings.get(name) {
                    out.extend(tokens.iter().cloned());
                }
            }
            Body::Repeat { items: inner, separator } => {
                let mut names = Vec::new();
                variables(inner, &mut names);
                let rounds = names
                    .iter()
                    .filter_map(|n| match bindings.get(n) {
                        Some(Bound::Each(entries)) => Some(entries.len()),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0);
                for round in 0..rounds {
                    let mut local = bindings.clone();
                    for name in &names {
                        if let Some(Bound::Each(entries)) = bindings.get(name)
                            && let Some(entry) = entries.get(round)
                        {
                            local.insert(name.clone(), entry.clone());
                        }
                    }
                    if round > 0
                        && let Some(sep) = separator
                    {
                        out.push(sep.clone());
                    }
                    transcribe(inner, &local, out);
                }
            }
        }
    }
}

fn variables(items: &[Body], names: &mut Vec<String>) {
    for item in items {
        match item {
            Body::Var(name) if !names.contains(name) => names.push(name.clone()),
            Body::Repeat { items, .. } => variables(items, names),
            _ => {}
        }
    }
}

/// The tokens a macro invocation expands to, by the first rule that matches.
pub fn expand(rules: &MacroRules, arguments: &[Tok]) -> Option<Vec<Tok>> {
    for (pattern, body) in &rules.rules {
        let mut bindings = Bindings::new();
        if match_items(pattern, arguments, 0, &mut bindings) == Some(arguments.len()) {
            let mut out = Vec::new();
            transcribe(body, &bindings, &mut out);
            return Some(out);
        }
    }
    None
}

const KEYWORDS: [&str; 22] = [
    "if", "while", "match", "for", "loop", "return", "fn", "in", "as", "let", "move", "async",
    "await", "else", "unsafe", "mut", "ref", "impl", "dyn", "where", "struct", "enum",
];

/// The calls in a stream of tokens: a name followed by a parenthesised group, as
/// `f(x)`, `a::f(x)` and `.m(x)`. A macro invocation (`name ! (…)`) is reported on
/// its own, with the tokens of its arguments, so that the caller can expand it.
pub enum Found<'a> {
    Call(&'a Tok),
    Macro(&'a Tok, &'a [Tok]),
}

pub fn scan(tokens: &[Tok]) -> Vec<Found<'_>> {
    let mut found = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let identifier = token.text.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
            && token.text.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !identifier || KEYWORDS.contains(&token.text.as_str()) {
            continue;
        }
        let previous = index.checked_sub(1).and_then(|i| tokens.get(i)).map(|t| t.text.as_str());
        if matches!(previous, Some("fn" | "struct" | "enum" | "trait" | "mod" | "type" | "macro_rules")) {
            continue;
        }
        match tokens.get(index + 1).map(|t| t.text.as_str()) {
            Some("(") => found.push(Found::Call(token)),
            Some("!") if tokens.get(index + 2).is_some_and(|t| is_open(&t.text)) => {
                let end = group_end(tokens, index + 2);
                found.push(Found::Macro(token, &tokens[index + 3..end.saturating_sub(1).max(index + 3)]));
            }
            _ => {}
        }
    }
    found
}
