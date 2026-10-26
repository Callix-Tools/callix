//! Following a value to the callable it holds.
//!
//! A type checker answers "what is the thing named here?", and for `a = f; a()`
//! the thing named is `a`. Without more, the call lands on a variable (`value` in
//! `metadata["resolution"]`), and the edge to `f` that a reader would draw is
//! missing. This pass adds it, from two facts the visitors record and the resolver
//! binds:
//!
//! * an **assignment** `x = y` (role `alias`, enclosed by the variable `x`) says
//!   `x` holds whatever `y` resolves to;
//! * a **return** `return y` (role `alias`, detail `return`, enclosed by the
//!   function) says calling the function yields whatever `y` resolves to, and
//!   `x = f()` (role `alias`, detail `result`, at `f`) says `x` holds what `f`
//!   returns; `f()()` is a call of whatever `f` returns (role `call`, detail
//!   `result`);
//! * an **argument** `g(y)` (role `read`, detail `arg:LINE:COL:INDEX` or
//!   `arg:LINE:COL:=NAME`) says the matching parameter of whichever function the
//!   call at `LINE:COL` resolves to holds whatever `y` resolves to.
//!
//! Then every `CALLS` edge that landed on a variable, parameter or attribute gets
//! one more edge per callable it can be shown to hold, marked
//! `resolution: "flow"` with the holder in `via`.
//!
//! What this is, and is not: it is **flow-insensitive and context-insensitive**.
//! A variable assigned two functions holds both; a parameter holds the argument of
//! every call site that passes it. That over-approximates exactly where a call
//! graph usually does, and the `flow` label says so. It follows names and
//! attribute chains only - not containers, return values or calls - and it
//! follows nothing it cannot resolve to a node of the project.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::graph::Graph;
use crate::node::NodeKind;
use crate::occurrence::OccurrenceRef;
use crate::py::prelude::*;
use crate::py::types::PyDict;
use crate::relation::{Relation, RelationKind};
use crate::resolution;

/// How far a chain of holders is followed: `a = b; b = c; c = f`.
const MAX_DEPTH: usize = 8;

/// Names a method's receiver parameter goes by, which a call's first argument
/// does not bind.
const RECEIVERS: [&str; 3] = ["self", "cls", "this"];

/// A holder that holds "whatever this function returns".
const RESULT: &str = "result:";

/// Constructor method names, by language, for an argument to a class call.
const CONSTRUCTORS: [&str; 3] = ["__init__", "constructor", "__construct"];

pub(crate) enum Slot {
    Index(usize),
    Keyword(String),
}

struct Arg {
    file: String,
    call_line: u32,
    call_col: u32,
    slot: Slot,
    target: String,
}

/// Collects what a resolution pass learns, and turns it into edges at the end.
#[derive(Default)]
pub(crate) struct Flow {
    /// Where each resolved call site points: (file, line, col) -> callee id.
    calls: HashMap<(String, u32, u32), String>,
    args: Vec<Arg>,
    /// What a variable, parameter or attribute holds.
    holds: HashMap<String, Vec<String>>,
    /// What a function returns, when it returns a name.
    returns: HashMap<String, Vec<String>>,
    /// Calls of a call's result: (caller, function whose result is called, span, file).
    result_calls: Vec<(String, String, Option<crate::span::Span>, String)>,
    /// How many calls are stacked below a call of a call's result, by the place of
    /// its argument list: `f()(x)` has `f` with depth 1 at the `(x)`.
    result_sites: HashMap<(String, u32, u32), (String, usize)>,
    /// The same calls with their depth, for the edges (`result_calls` holds depth 1).
    deep_calls: Vec<(String, String, crate::span::Span, usize)>,
    /// `for x in it:`: (the scope that loops, what `it` resolved to, where).
    iterations: Vec<(String, String, crate::span::Span, Option<String>)>,
    /// `recv.m()`: (the scope that calls, what `recv` resolved to, `m`, line, column
    /// of `m`).
    receivers: Vec<(String, String, String, u32, u32)>,
    /// The decorators of a definition, outermost first: the decorator's node (None
    /// when nothing in the project is known of it) and whether it is a call.
    decorators: HashMap<String, BTreeMap<usize, Option<(String, bool)>>>,
    /// Definitions whose name stands for something else after decoration.
    decorated: HashSet<String>,
    /// The keys a call through a container was made with: `table["a"]()` is a call
    /// at `table` with the path `["a"]`.
    call_paths: HashMap<(String, u32, u32), Vec<Key>>,
}

/// Where the evaluation of what a holder holds starts: a holder, or one token.
enum Item {
    Holder(String),
    Token(String),
}

/// A key a container is read or written with.
///
/// Written in a token as `i1` (the integer 1), `s'a'` (a string), `*` (anything),
/// `r1:3` (positions 1 up to 3) or `@ID` (whatever the variable `ID` holds).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Key {
    Lit(String),
    Wild,
    Range(i64, i64),
    Var(String),
}

/// Keys separated by `;`, with the separators of the token grammar escaped.
pub(crate) fn escape_key(text: &str) -> String {
    text.replace('%', "%25").replace(';', "%3B").replace('|', "%7C")
}

fn unescape_key(text: &str) -> String {
    text.replace("%7C", "|").replace("%3B", ";").replace("%25", "%")
}

impl Key {
    fn parse(text: &str) -> Key {
        if text == "*" {
            return Key::Wild;
        }
        if let Some(range) = text.strip_prefix('r')
            && let Some((low, high)) = range.split_once(':')
            && let (Ok(low), Ok(high)) = (low.parse(), high.parse())
        {
            return Key::Range(low, high);
        }
        if let Some(holder) = text.strip_prefix('@') {
            return Key::Var(holder.to_owned());
        }
        Key::Lit(unescape_key(text))
    }

    fn integer(&self) -> Option<i64> {
        match self {
            Key::Lit(text) => text.strip_prefix('i')?.parse().ok(),
            _ => None,
        }
    }
}

/// The keys of an encoded path (`i0;s'a'`).
pub(crate) fn parse_path(text: &str) -> Vec<Key> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(';').map(Key::parse).collect()
}

/// A slice taken of a slice, or an index taken of a slice, is one key.
fn normalize(path: Vec<Key>) -> Option<Vec<Key>> {
    let mut out: Vec<Key> = Vec::new();
    for key in path {
        match (out.last(), &key) {
            (Some(Key::Range(low, high)), Key::Lit(_)) => {
                let index = key.integer().map(|n| low + n);
                match index {
                    Some(index) if index < *high => {
                        out.pop();
                        out.push(Key::Lit(format!("i{index}")));
                    }
                    _ => return None,
                }
            }
            (Some(Key::Range(..)), Key::Wild | Key::Var(_)) => {}
            (Some(Key::Range(low, high)), Key::Range(low2, high2)) => {
                let merged = Key::Range(low + low2, (*high).min(low + high2));
                out.pop();
                out.push(merged);
            }
            _ => out.push(key),
        }
    }
    Some(out)
}

impl Flow {
    /// What a call through `key` of a container is: `table["a"]()` has the path
    /// `a`. Recorded by the adapter where the call is bound.
    pub(crate) fn call_path(&mut self, caller_id: &str, line: u32, col: u32, path: Vec<Key>) {
        self.call_paths.insert((caller_id.to_owned(), line, col), path);
    }

    /// The holder `holder_id` holds `token`, whatever kind of token it is.
    pub(crate) fn hold(&mut self, holder_id: &str, token: String) {
        if holder_id != token {
            self.holds.entry(holder_id.to_owned()).or_default().push(token);
        }
    }

    /// The literal values `holder` can be shown to hold.
    fn literals_of(&self, graph: &Graph, holder: &str) -> Vec<Key> {
        let mut found: Vec<Key> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = vec![holder];
        while let Some(current) = stack.pop() {
            for held in self.holds.get(current).into_iter().flatten() {
                if !seen.insert(held.as_str()) {
                    continue;
                }
                if let Some(literal) = held.strip_prefix("lit:") {
                    found.push(Key::Lit(unescape_key(literal)));
                } else if matches!(
                    graph.node_kind(held),
                    Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute)
                ) {
                    stack.push(held.as_str());
                }
            }
        }
        found
    }

    /// Whether the key an element was stored under can be the key it is read with.
    fn compatible(&self, graph: &Graph, stored: &Key, read: &Key) -> bool {
        match (stored, read) {
            (Key::Wild, _) | (_, Key::Wild) => true,
            (Key::Var(holder), other) | (other, Key::Var(holder)) => {
                let values = self.literals_of(graph, holder);
                values.is_empty() || values.iter().any(|value| self.compatible(graph, value, other))
            }
            (Key::Lit(a), Key::Lit(b)) => a == b,
            (Key::Range(low, high), other) | (other, Key::Range(low, high)) => match other {
                Key::Range(low2, high2) => low < high2 && low2 < high,
                _ => other.integer().is_some_and(|n| *low <= n && n < *high),
            },
        }
    }

    /// Every callable `start` can be shown to hold when it is read with `path`.
    ///
    /// A held name is followed; `at:KEYS|TOKEN` is an element stored under KEYS and
    /// counts only when the read can mean those keys; `get:KEYS|ID` is a value read
    /// out of `ID`; `elem:ID` is any element of `ID`; `result:F` is what `F`
    /// returns.
    fn resolve(&self, graph: &Graph, start: &str, path: &[Key]) -> Vec<String> {
        self.resolve_item(graph, Item::Holder(start.to_owned()), path)
    }

    /// What calling `function` returns, as callables.
    fn results_of(&self, graph: &Graph, function: &str) -> Vec<String> {
        self.resolve_item(graph, Item::Token(format!("{RESULT}{function}")), &[])
    }

    fn resolve_item(&self, graph: &Graph, start: Item, path: &[Key]) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut stack: Vec<(Item, Vec<Key>, usize)> = vec![(start, path.to_vec(), 0)];
        while let Some((item, path, depth)) = stack.pop() {
            if depth > 3 * MAX_DEPTH {
                continue;
            }
            match item {
                Item::Holder(holder) => {
                    if !seen.insert((holder.clone(), format!("{path:?}"))) {
                        continue;
                    }
                    for token in self.holds.get(&holder).into_iter().flatten() {
                        stack.push((Item::Token(token.clone()), path.clone(), depth + 1));
                    }
                }
                Item::Token(token) => {
                    if let Some(rest) = token.strip_prefix("at:") {
                        let Some((keys, payload)) = rest.split_once('|') else {
                            continue;
                        };
                        let keys = parse_path(keys);
                        if path.is_empty() || keys.len() > path.len() {
                            continue;
                        }
                        if !keys.iter().zip(path.iter()).all(|(stored, read)| self.compatible(graph, stored, read)) {
                            continue;
                        }
                        stack.push((Item::Token(payload.to_owned()), path[keys.len()..].to_vec(), depth + 1));
                    } else if let Some(rest) = token.strip_prefix("get:") {
                        let Some((keys, holder)) = rest.split_once('|') else {
                            continue;
                        };
                        let mut joined = parse_path(keys);
                        joined.extend(path.iter().cloned());
                        if let Some(joined) = normalize(joined) {
                            stack.push((Item::Holder(holder.to_owned()), joined, depth + 1));
                        }
                    } else if let Some(holder) = token.strip_prefix("elem:") {
                        let mut joined = vec![Key::Wild];
                        joined.extend(path.iter().cloned());
                        if let Some(joined) = normalize(joined) {
                            stack.push((Item::Holder(holder.to_owned()), joined, depth + 1));
                        }
                    } else if token.starts_with("lit:") {
                        continue;
                    } else if let Some(function) = token.strip_prefix(RESULT) {
                        for returned in self.returns.get(function).into_iter().flatten() {
                            stack.push((Item::Token(returned.clone()), path.clone(), depth + 1));
                        }
                    } else {
                        match graph.node_kind(&token) {
                            Some(NodeKind::Function | NodeKind::Method | NodeKind::Class) => {
                                if !found.contains(&token) {
                                    found.push(token);
                                }
                            }
                            Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                                stack.push((Item::Holder(token), path.clone(), depth + 1));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        found
    }

    /// Notes where a call site points, so an argument can find its callee.
    pub(crate) fn call(&mut self, file: &str, occurrence: &OccurrenceRef, target: Option<&str>) {
        self.call_at(file, occurrence.line, occurrence.col, target);
    }

    /// The same, for an adapter that keeps its occurrences in a shape of its own.
    pub(crate) fn call_at(&mut self, file: &str, line: u32, col: u32, target: Option<&str>) {
        if let Some(target) = target {
            self.calls.insert((file.to_owned(), line, col), target.to_owned());
        }
    }

    /// A name handed to the call at `line:col` as argument `slot`.
    pub(crate) fn arg_at(&mut self, file: &str, line: u32, col: u32, slot: Slot, target: &str) {
        self.args.push(Arg {
            file: file.to_owned(),
            call_line: line,
            call_col: col,
            slot,
            target: target.to_owned(),
        });
    }

    /// `x = y`: the variable holds what the right-hand side resolved to.
    pub(crate) fn alias(&mut self, variable_id: &str, target: &str) {
        if variable_id != target {
            self.holds.entry(variable_id.to_owned()).or_default().push(target.to_owned());
        }
    }

    /// `return y`: calling the function yields what `y` resolved to.
    pub(crate) fn returned(&mut self, function_id: &str, target: &str) {
        self.returns.entry(function_id.to_owned()).or_default().push(target.to_owned());
    }

    /// `x = f()`: the variable holds whatever `f` returns.
    pub(crate) fn alias_result(&mut self, variable_id: &str, function_id: &str) {
        self.holds
            .entry(variable_id.to_owned())
            .or_default()
            .push(format!("{RESULT}{function_id}"));
    }

    /// `f()()`: the outer call is a call of whatever `f` returns.
    pub(crate) fn result_call(
        &mut self,
        caller_id: &str,
        function_id: &str,
        span: crate::span::Span,
        file: &str,
    ) {
        self.result_calls.push((caller_id.to_owned(), function_id.to_owned(), Some(span), file.to_owned()));
    }

    /// `f()()()` made by `caller_id`: a call of what `f()()` returns. `site` is the
    /// place of this call's own argument list, where its arguments are keyed.
    pub(crate) fn result_call_deep(
        &mut self,
        caller_id: &str,
        function_id: &str,
        span: crate::span::Span,
        file: &str,
        depth: usize,
        site: Option<(u32, u32)>,
    ) {
        if let Some((line, col)) = site {
            self.result_sites.insert((file.to_owned(), line, col), (function_id.to_owned(), depth));
        }
        self.deep_calls.push((caller_id.to_owned(), function_id.to_owned(), span, depth));
    }

    /// The callables a call `depth` calls deep reaches: what `function` returns,
    /// then what those return, `depth` times.
    fn level_targets(&self, graph: &Graph, function: &str, depth: usize) -> Vec<String> {
        let mut current: Vec<String> = vec![function.to_owned()];
        for _ in 0..depth {
            let mut next: Vec<String> = Vec::new();
            for callee in &current {
                for found in self.results_of(graph, callee) {
                    if !next.contains(&found) {
                        next.push(found);
                    }
                }
            }
            current = next
                .into_iter()
                .filter(|c| matches!(graph.node_kind(c), Some(NodeKind::Function | NodeKind::Method | NodeKind::Class)))
                .collect();
        }
        current
    }

    /// `for x in it:`: the scope calls `__iter__` and `__next__` of the class
    /// `it` holds an instance of.
    ///
    /// `variable` is the loop variable when the body calls it: it holds what the
    /// iterable hands out.
    pub(crate) fn iterate(
        &mut self,
        caller_id: &str,
        held: &str,
        span: crate::span::Span,
        variable: Option<String>,
    ) {
        self.iterations.push((caller_id.to_owned(), held.to_owned(), span, variable));
    }

    /// The `index`th decorator (0 is the outermost) of the definition `function_id`.
    pub(crate) fn decorator(&mut self, function_id: &str, index: usize, target: Option<&str>, is_call: bool) {
        self.decorators
            .entry(function_id.to_owned())
            .or_default()
            .insert(index, target.map(|t| (t.to_owned(), is_call)));
    }

    /// `recv.m()` made by `caller_id`, with `m` at `line:col`.
    pub(crate) fn receiver(&mut self, caller_id: &str, held: &str, name: &str, line: u32, col: u32) {
        self.receivers.push((caller_id.to_owned(), held.to_owned(), name.to_owned(), line, col));
    }

    /// `@dec def f` makes `f` whatever `dec(f)` returns, and the decorator's own
    /// parameter holds `f`. Stacked decorators apply from the bottom up. Anything
    /// the project knows nothing of - a library decorator, a class - leaves the
    /// definition as it is.
    fn bind_decorators(&mut self, graph: &Graph) {
        if self.decorators.is_empty() {
            return;
        }
        let mut parameters: HashMap<String, Vec<String>> = HashMap::new();
        for relation in graph.relation_slice() {
            let relation = relation.get();
            if relation.kind == RelationKind::Declares
                && graph.node_kind(&relation.target_id) == Some(NodeKind::Parameter)
            {
                parameters.entry(relation.source_id.clone()).or_default().push(relation.target_id.clone());
            }
        }
        let first_parameter = |function: &str| -> Option<String> {
            let list = parameters.get(function)?;
            let skips = graph.node_kind(function) == Some(NodeKind::Method)
                && list
                    .first()
                    .and_then(|id| graph.node_map().get(id))
                    .is_some_and(|n| RECEIVERS.contains(&n.get().name.as_str()));
            list.get(usize::from(skips)).cloned()
        };
        let is_function = |id: &str| matches!(graph.node_kind(id), Some(NodeKind::Function | NodeKind::Method));
        let decorators = std::mem::take(&mut self.decorators);
        for (definition, chain) in &decorators {
            if !is_function(definition) {
                continue;
            }
            let mut value: Option<Vec<String>> = Some(vec![definition.clone()]);
            for (index, entry) in chain.iter().rev() {
                let Some(tokens) = value.clone() else {
                    break;
                };
                let Some((target, is_call)) = entry else {
                    value = None;
                    break;
                };
                let callees: Vec<String> = if *is_call {
                    let key = format!("decorator:{definition}:{index}");
                    self.holds.insert(key.clone(), vec![format!("{RESULT}{target}")]);
                    self.callables(graph, &key).into_iter().filter(|c| is_function(c)).collect()
                } else if is_function(target) {
                    vec![target.clone()]
                } else {
                    Vec::new()
                };
                if callees.is_empty() {
                    value = None;
                    break;
                }
                for callee in &callees {
                    if let Some(parameter) = first_parameter(callee) {
                        let held = self.holds.entry(parameter).or_default();
                        for token in &tokens {
                            if !held.contains(token) {
                                held.push(token.clone());
                            }
                        }
                    }
                }
                value = Some(callees.iter().map(|c| format!("{RESULT}{c}")).collect());
            }
            if let Some(value) = value {
                self.holds.insert(format!("decorated:{definition}"), value);
                self.decorated.insert(definition.clone());
            }
        }
        self.decorators = decorators;
    }

    /// What a loop variable holds: the elements of a container, what a generator
    /// yields, what an iterator's `__next__` returns.
    fn bind_iterations(&mut self, graph: &Graph) {
        if self.iterations.is_empty() {
            return;
        }
        let by_name: HashMap<String, String> = graph
            .node_map()
            .values()
            .map(|n| n.get())
            .map(|n| (n.qualified_name.clone(), n.id.clone()))
            .collect();
        let mut learned: Vec<(String, String)> = Vec::new();
        for (_, held, _, variable) in &self.iterations {
            let Some(variable) = variable else {
                continue;
            };
            match graph.node_kind(held) {
                // `for f in handlers`: the elements `handlers` holds.
                Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                    learned.push((variable.clone(), format!("elem:{held}")));
                    for class in self.instances(graph, held) {
                        learned.extend(self.next_results(graph, &by_name, &class, variable));
                    }
                }
                // `for f in gen()`: what the generator yields.
                Some(NodeKind::Function | NodeKind::Method) => {
                    learned.push((variable.clone(), format!("{RESULT}{held}")));
                }
                // `for f in Cls()`: what `__next__` returns.
                Some(NodeKind::Class) => {
                    learned.extend(self.next_results(graph, &by_name, held, variable));
                }
                _ => {}
            }
        }
        for (variable, held) in learned {
            let list = self.holds.entry(variable).or_default();
            if !list.contains(&held) {
                list.push(held);
            }
        }
    }

    fn next_results(
        &self,
        graph: &Graph,
        by_name: &HashMap<String, String>,
        class: &str,
        variable: &str,
    ) -> Vec<(String, String)> {
        let Some(name) = graph.node_map().get(class).map(|n| n.get().qualified_name.clone()) else {
            return Vec::new();
        };
        by_name
            .get(&format!("{name}.__next__"))
            .map(|next| vec![(variable.to_owned(), format!("{RESULT}{next}"))])
            .unwrap_or_default()
    }

    /// A call argument, when the occurrence says it is one.
    pub(crate) fn arg(&mut self, file: &str, occurrence: &OccurrenceRef, target: &str) {
        if let Some(detail) = occurrence.detail.as_deref() {
            self.arg_detail(file, detail, target);
        }
    }

    /// The same, from the `arg:LINE:COL:SLOT` text itself.
    pub(crate) fn arg_detail(&mut self, file: &str, detail: &str, target: &str) {
        let Some(detail) = detail.strip_prefix("arg:") else {
            return;
        };
        let mut parts = detail.splitn(3, ':');
        let (Some(line), Some(col), Some(slot)) = (parts.next(), parts.next(), parts.next()) else {
            return;
        };
        let (Ok(call_line), Ok(call_col)) = (line.parse(), col.parse()) else {
            return;
        };
        // `g(f())`: the argument is what `f` returns.
        let (slot, result) = match slot.strip_suffix(":result") {
            Some(slot) => (slot, true),
            None => (slot, false),
        };
        let held = if result { format!("{RESULT}{target}") } else { target.to_owned() };
        let target = held.as_str();
        let slot = match slot.strip_prefix('=') {
            Some(name) => Slot::Keyword(name.to_owned()),
            None => match slot.parse() {
                Ok(index) => Slot::Index(index),
                Err(_) => return,
            },
        };
        self.args.push(Arg {
            file: file.to_owned(),
            call_line,
            call_col,
            slot,
            target: target.to_owned(),
        });
    }

    /// Binds the arguments to parameters, follows the holders, and writes the
    /// `flow` edges. Returns how many it wrote.
    pub(crate) fn finish(mut self, py: Python<'_>, graph: &mut Graph) -> PyResult<usize> {
        if self.holds.is_empty()
            && self.args.is_empty()
            && self.result_calls.is_empty()
            && self.iterations.is_empty()
            && self.receivers.is_empty()
            && self.decorators.is_empty()
            && self.call_paths.is_empty()
            && self.deep_calls.is_empty()
        {
            return Ok(0);
        }
        self.bind_decorators(graph);
        self.bind_arguments(graph);
        self.bind_iterations(graph);
        self.follow(py, graph)
    }

    fn bind_arguments(&mut self, graph: &Graph) {
        if self.args.is_empty() {
            return;
        }
        // Parameters in declaration order, per function.
        let mut parameters: HashMap<&str, Vec<&str>> = HashMap::new();
        for relation in graph.relation_slice() {
            let relation = relation.get();
            if relation.kind == RelationKind::Declares
                && graph.node_kind(&relation.target_id) == Some(NodeKind::Parameter)
            {
                parameters.entry(relation.source_id.as_str()).or_default().push(relation.target_id.as_str());
            }
        }
        let by_name: HashMap<&str, &str> = graph
            .node_map()
            .values()
            .map(|n| n.get())
            .map(|n| (n.qualified_name.as_str(), n.id.as_str()))
            .collect();

        let args = std::mem::take(&mut self.args);
        // A call through a parameter or a variable (`a(b)`) hands its arguments to
        // whatever `a` holds, which is known only once the arguments of the other
        // calls are bound: so the binding is repeated until nothing new is learned.
        for _ in 0..4 {
            let mut learned: Vec<(String, String)> = Vec::new();
            for arg in &args {
                let key = (arg.file.clone(), arg.call_line, arg.call_col);
                let callees: Vec<String> = if let Some(site_callee) = self.calls.get(&key).cloned() {
                    match graph.node_kind(&site_callee) {
                        Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                            self.callables(graph, &site_callee)
                        }
                        _ => vec![site_callee],
                    }
                } else if let Some((function, depth)) = self.result_sites.get(&key) {
                    self.level_targets(graph, function, *depth)
                } else {
                    continue;
                };
                for mut callee in callees {
                    // `A(x)` is a call of A's constructor.
                    if graph.node_kind(&callee) == Some(NodeKind::Class) {
                        let class_name = graph.node_map().get(&callee).map(|n| n.get().qualified_name.clone());
                        let constructor = class_name.and_then(|class_name| {
                            CONSTRUCTORS.iter().find_map(|ctor| {
                                by_name
                                    .get(format!("{class_name}.{ctor}").as_str())
                                    .or_else(|| by_name.get(format!("{class_name}\\{ctor}").as_str()))
                                    .copied()
                            })
                        });
                        match constructor {
                            Some(id) => callee = id.to_owned(),
                            None => continue,
                        }
                    }
                    let Some(list) = parameters.get(callee.as_str()) else {
                        continue;
                    };
                    let skips_receiver = graph.node_kind(&callee) == Some(NodeKind::Method)
                        && list
                            .first()
                            .and_then(|id| graph.node_map().get(*id))
                            .is_some_and(|n| RECEIVERS.contains(&n.get().name.as_str()));
                    let parameter = match &arg.slot {
                        Slot::Index(index) => list.get(index + usize::from(skips_receiver)).copied(),
                        Slot::Keyword(name) => list
                            .iter()
                            .copied()
                            .find(|id| graph.node_map().get(*id).is_some_and(|n| n.get().name == *name)),
                    };
                    if let Some(parameter) = parameter {
                        learned.push((parameter.to_owned(), arg.target.clone()));
                    }
                }
            }
            let mut changed = false;
            for (parameter, target) in learned {
                let held = self.holds.entry(parameter).or_default();
                if !held.contains(&target) {
                    held.push(target);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Every callable a holder can be followed to.
    fn callables(&self, graph: &Graph, start: &str) -> Vec<String> {
        self.resolve(graph, start, &[])
    }

    /// The classes a holder can be shown to hold an instance of: `c = Cls()`,
    /// or a parameter handed `Cls()`.
    fn instances(&self, graph: &Graph, start: &str) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut frontier = vec![start];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for holder in frontier {
                for held in self.holds.get(holder).into_iter().flatten() {
                    if !seen.insert(held.as_str()) {
                        continue;
                    }
                    if let Some(function) = held.strip_prefix(RESULT) {
                        if graph.node_kind(function) == Some(NodeKind::Class) {
                            found.push(function.to_owned());
                        }
                        continue;
                    }
                    if matches!(
                        graph.node_kind(held),
                        Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute)
                    ) {
                        next.push(held.as_str());
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        found
    }

    /// The method or attribute `name` of the classes a receiver can be shown to
    /// be an instance of, for the calls the type checker did not answer.
    fn receiver_calls(&self, py: Python<'_>, graph: &Graph, added: &mut Vec<Relation>) -> PyResult<()> {
        if self.receivers.is_empty() {
            return Ok(());
        }
        let by_name: HashMap<String, String> = graph
            .node_map()
            .values()
            .map(|n| n.get())
            .map(|n| (n.qualified_name.clone(), n.id.clone()))
            .collect();
        // A call the type checker already placed on a definition is left alone.
        let mut placed: HashSet<(String, u32, u32)> = HashSet::new();
        let mut declarer: HashMap<String, String> = HashMap::new();
        let mut bases: HashMap<String, Vec<String>> = HashMap::new();
        let mut subclasses: HashMap<String, Vec<String>> = HashMap::new();
        for relation in graph.relation_slice() {
            let relation = relation.get();
            match relation.kind {
                RelationKind::Declares => {
                    declarer.insert(relation.target_id.clone(), relation.source_id.clone());
                }
                RelationKind::InheritsFrom => {
                    bases.entry(relation.source_id.clone()).or_default().push(relation.target_id.clone());
                    subclasses.entry(relation.target_id.clone()).or_default().push(relation.source_id.clone());
                }
                RelationKind::Calls
                    if matches!(
                        graph.node_kind(&relation.target_id),
                        Some(NodeKind::Function | NodeKind::Method | NodeKind::Class)
                    ) =>
                {
                    let metadata = relation.metadata.bind(py);
                    if let Some(span) = metadata
                        .get_item("span")
                        .ok()
                        .flatten()
                        .and_then(|v| v.extract::<crate::span::Span>().ok())
                    {
                        placed.insert((relation.source_id.clone(), span.start_line, span.start_col));
                    }
                }
                _ => {}
            }
        }
        let mut attributes: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for node in graph.node_map().values().map(|n| n.get()) {
            if node.kind == NodeKind::Attribute {
                attributes.entry(node.name.clone()).or_default().push((node.qualified_name.clone(), node.id.clone()));
            }
        }
        let closure = |start: &str, edges: &HashMap<String, Vec<String>>| -> Vec<String> {
            let mut seen: Vec<String> = vec![start.to_owned()];
            let mut index = 0;
            while index < seen.len() {
                for next in edges.get(&seen[index]).into_iter().flatten() {
                    if !seen.contains(next) {
                        seen.push(next.clone());
                    }
                }
                index += 1;
            }
            seen
        };

        let mut done: HashSet<(String, String, u32, u32)> = HashSet::new();
        for (caller, held, name, line, col) in &self.receivers {
            if placed.contains(&(caller.clone(), *line, *col)) {
                continue;
            }
            // `self` is an instance of the class of its method, or of a subclass.
            let is_self = graph.node_kind(held) == Some(NodeKind::Parameter)
                && graph
                    .node_map()
                    .get(held)
                    .is_some_and(|n| RECEIVERS.contains(&n.get().name.as_str()));
            let mut classes: Vec<String> = Vec::new();
            if is_self {
                if let Some(class) = declarer.get(held).and_then(|f| declarer.get(f)) {
                    if graph.node_kind(class) == Some(NodeKind::Class) {
                        classes = closure(class, &subclasses);
                    }
                }
            } else {
                classes = self.instances(graph, held);
            }
            for class in classes {
                // The class, then what it inherits from.
                for owner in closure(&class, &bases) {
                    let Some(owner_name) = graph.node_map().get(&owner).map(|n| n.get().qualified_name.clone()) else {
                        continue;
                    };
                    let mut targets: Vec<(String, String)> = Vec::new();
                    if let Some(method) = by_name.get(&format!("{owner_name}.{name}")) {
                        if graph.node_kind(method) == Some(NodeKind::Method) {
                            targets.push((method.clone(), owner_name.clone()));
                        }
                    }
                    for (qname, id) in attributes.get(name.as_str()).into_iter().flatten() {
                        if qname.starts_with(&format!("{owner_name}.")) {
                            for callable in self.callables(graph, id) {
                                targets.push((callable, qname.clone()));
                            }
                        }
                    }
                    for (target, via) in targets {
                        if !done.insert((caller.clone(), target.clone(), *line, *col)) {
                            continue;
                        }
                        let carried = PyDict::new(py);
                        carried.set_item(
                            "span",
                            crate::span::Span { start_line: *line, start_col: *col, end_line: *line, end_col: *col + name.len() as u32 },
                        )?;
                        carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                        carried.set_item("via", via)?;
                        added.push(Relation {
                            source_id: caller.clone(),
                            target_id: target,
                            kind: RelationKind::Calls,
                            metadata: carried.unbind(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// A call of a decorated definition is a call of what the decorator made of it:
    /// the edge to the definition is replaced when the definition is not among
    /// those, and a decorator that is itself a call (`@make(x)`) is called by the
    /// scope the definition sits in. Returns the positions of the edges replaced.
    fn decorated_calls(
        &self,
        py: Python<'_>,
        graph: &Graph,
        added: &mut Vec<Relation>,
    ) -> PyResult<HashSet<usize>> {
        let mut dropped: HashSet<usize> = HashSet::new();
        if self.decorated.is_empty() {
            return Ok(dropped);
        }
        let mut results: HashMap<&str, Vec<String>> = HashMap::new();
        for definition in &self.decorated {
            results.insert(definition.as_str(), self.callables(graph, &format!("decorated:{definition}")));
        }
        for (position, relation) in graph.relation_slice().iter().enumerate() {
            let relation = relation.get();
            if relation.kind != RelationKind::Calls {
                continue;
            }
            let Some(made) = results.get(relation.target_id.as_str()) else {
                continue;
            };
            if made.is_empty() {
                continue;
            }
            let metadata = relation.metadata.bind(py);
            let name = graph
                .node_map()
                .get(&relation.target_id)
                .map(|n| n.get().qualified_name.clone())
                .unwrap_or_default();
            for callable in made {
                if *callable == relation.target_id {
                    continue;
                }
                let carried = PyDict::new(py);
                if let Some(span) = metadata
                    .get_item("span")
                    .ok()
                    .flatten()
                    .and_then(|v| v.extract::<crate::span::Span>().ok())
                {
                    carried.set_item("span", span)?;
                }
                carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                carried.set_item("via", format!("@{name}"))?;
                added.push(Relation {
                    source_id: relation.source_id.clone(),
                    target_id: callable.clone(),
                    kind: RelationKind::Calls,
                    metadata: carried.unbind(),
                });
            }
            if !made.contains(&relation.target_id) {
                dropped.insert(position);
            }
        }
        // `@make(x)`: the scope calls what `make(x)` returned, on the definition.
        let mut scope_of: HashMap<&str, &str> = HashMap::new();
        for relation in graph.relation_slice() {
            let relation = relation.get();
            if relation.kind == RelationKind::Declares && self.decorators.contains_key(&relation.target_id) {
                scope_of.insert(relation.target_id.as_str(), relation.source_id.as_str());
            }
        }
        for (definition, chain) in &self.decorators {
            let Some(scope) = scope_of.get(definition.as_str()) else {
                continue;
            };
            for (index, entry) in chain {
                let Some((target, true)) = entry else {
                    continue;
                };
                let key = format!("decorator:{definition}:{index}");
                for callable in self.callables(graph, &key) {
                    let carried = PyDict::new(py);
                    carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                    let via = graph.node_map().get(target).map(|n| n.get().qualified_name.clone()).unwrap_or_default();
                    carried.set_item("via", format!("{via}()"))?;
                    added.push(Relation {
                        source_id: (*scope).to_owned(),
                        target_id: callable,
                        kind: RelationKind::Calls,
                        metadata: carried.unbind(),
                    });
                }
            }
        }
        Ok(dropped)
    }

    fn follow(&self, py: Python<'_>, graph: &mut Graph) -> PyResult<usize> {
        let mut added: Vec<Relation> = Vec::new();
        self.receiver_calls(py, graph, &mut added)?;
        let dropped = self.decorated_calls(py, graph, &mut added)?;
        if !self.iterations.is_empty() {
            let by_name: HashMap<String, String> = graph
                .node_map()
                .values()
                .map(|n| n.get())
                .map(|n| (n.qualified_name.clone(), n.id.clone()))
                .collect();
            for (caller, held, span, _) in &self.iterations {
                // `for x in Cls():` resolves to the class itself.
                let classes = if graph.node_kind(held) == Some(NodeKind::Class) {
                    vec![held.clone()]
                } else {
                    self.instances(graph, held)
                };
                for class in classes {
                    let Some(class_name) = graph.node_map().get(&class).map(|n| n.get().qualified_name.clone())
                    else {
                        continue;
                    };
                    for protocol in ["__iter__", "__next__"] {
                        let Some(target) = by_name.get(&format!("{class_name}.{protocol}")) else {
                            continue;
                        };
                        let carried = PyDict::new(py);
                        carried.set_item("span", *span)?;
                        carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                        carried.set_item("via", class_name.clone())?;
                        added.push(Relation {
                            source_id: caller.clone(),
                            target_id: target.clone(),
                            kind: RelationKind::Calls,
                            metadata: carried.unbind(),
                        });
                    }
                }
            }
        }
        for relation in graph.relation_slice() {
            let relation = relation.get();
            if relation.kind != RelationKind::Calls {
                continue;
            }
            let metadata = relation.metadata.bind(py);
            let is_value = metadata
                .get_item(resolution::KEY)
                .ok()
                .flatten()
                .and_then(|v| v.extract::<String>().ok())
                .is_some_and(|how| how == resolution::Resolution::Value.as_str());
            if !is_value {
                continue;
            }
            let path: Vec<Key> = metadata
                .get_item("span")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<crate::span::Span>().ok())
                .and_then(|span| {
                    self.call_paths.get(&(relation.source_id.clone(), span.start_line, span.start_col)).cloned()
                })
                .unwrap_or_default();
            for callable in self.resolve(graph, &relation.target_id, &path) {
                let carried = PyDict::new(py);
                if let Some(span) = metadata
                    .get_item("span")
                    .ok()
                    .flatten()
                    .and_then(|v| v.extract::<crate::span::Span>().ok())
                {
                    carried.set_item("span", span)?;
                }
                if let Some(file) = metadata
                    .get_item("file")
                    .ok()
                    .flatten()
                    .and_then(|v| v.extract::<String>().ok())
                {
                    carried.set_item("file", file)?;
                }
                carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                let holder = graph
                    .node_map()
                    .get(&relation.target_id)
                    .map(|n| n.get().qualified_name.clone())
                    .unwrap_or_default();
                carried.set_item("via", holder)?;
                added.push(Relation {
                    source_id: relation.source_id.clone(),
                    target_id: callable,
                    kind: RelationKind::Calls,
                    metadata: carried.unbind(),
                });
            }
        }
        let mut result_edges: Vec<(&String, &String, Option<crate::span::Span>, usize)> = Vec::new();
        for (caller, function, span, _) in &self.result_calls {
            result_edges.push((caller, function, *span, 1));
        }
        for (caller, function, span, depth) in &self.deep_calls {
            result_edges.push((caller, function, Some(*span), *depth));
        }
        for (caller, function, span, depth) in result_edges {
            for callable in self.level_targets(graph, function, depth) {
                let carried = PyDict::new(py);
                if let Some(span) = span {
                    carried.set_item("span", span)?;
                }
                carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                let name = graph.node_map().get(function).map(|n| n.get().qualified_name.clone()).unwrap_or_default();
                carried.set_item("via", format!("{name}{}", "()".repeat(depth)))?;
                added.push(Relation {
                    source_id: caller.clone(),
                    target_id: callable,
                    kind: RelationKind::Calls,
                    metadata: carried.unbind(),
                });
            }
        }
        let count = added.len();
        if !dropped.is_empty() {
            let mut index = 0usize;
            graph.retain_relations(|_| {
                let keep = !dropped.contains(&index);
                index += 1;
                keep
            });
        }
        for relation in added {
            graph.push_relation(Py::new(py, relation)?);
        }
        Ok(count)
    }
}
