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

use std::collections::{HashMap, HashSet};

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
    /// `for x in it:`: (the scope that loops, what `it` resolved to, where).
    iterations: Vec<(String, String, crate::span::Span)>,
}

impl Flow {
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

    /// `for x in it:`: the scope calls `__iter__` and `__next__` of the class
    /// `it` holds an instance of.
    pub(crate) fn iterate(&mut self, caller_id: &str, held: &str, span: crate::span::Span) {
        self.iterations.push((caller_id.to_owned(), held.to_owned(), span));
    }

    /// A call argument, when the occurrence says it is one.
    pub(crate) fn arg(&mut self, file: &str, occurrence: &OccurrenceRef, target: &str) {
        let Some(detail) = occurrence.detail.as_deref().and_then(|d| d.strip_prefix("arg:")) else {
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
        {
            return Ok(0);
        }
        self.bind_arguments(graph);
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
                let Some(site_callee) = self.calls.get(&(arg.file.clone(), arg.call_line, arg.call_col)).cloned()
                else {
                    continue;
                };
                let callees: Vec<String> = match graph.node_kind(&site_callee) {
                    Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                        self.callables(graph, &site_callee)
                    }
                    _ => vec![site_callee],
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
        let mut found = Vec::new();
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
                        // Whatever the function returns is held in turn.
                        for returned in self.returns.get(function).into_iter().flatten() {
                            match graph.node_kind(returned) {
                                Some(NodeKind::Function | NodeKind::Method | NodeKind::Class) => {
                                    found.push(returned.clone());
                                }
                                Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                                    next.push(returned.as_str());
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }
                    match graph.node_kind(held) {
                        Some(NodeKind::Function | NodeKind::Method | NodeKind::Class) => {
                            found.push(held.clone());
                        }
                        Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                            next.push(held.as_str());
                        }
                        _ => {}
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

    fn follow(&self, py: Python<'_>, graph: &mut Graph) -> PyResult<usize> {
        let mut added: Vec<Relation> = Vec::new();
        if !self.iterations.is_empty() {
            let by_name: HashMap<String, String> = graph
                .node_map()
                .values()
                .map(|n| n.get())
                .map(|n| (n.qualified_name.clone(), n.id.clone()))
                .collect();
            for (caller, held, span) in &self.iterations {
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
            for callable in self.callables(graph, &relation.target_id) {
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
        for (caller, function, span, file) in &self.result_calls {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack: Vec<&str> = self.returns.get(function).into_iter().flatten().map(String::as_str).collect();
            let mut targets: Vec<String> = Vec::new();
            while let Some(returned) = stack.pop() {
                if !seen.insert(returned) {
                    continue;
                }
                match graph.node_kind(returned) {
                    Some(NodeKind::Function | NodeKind::Method | NodeKind::Class) => targets.push(returned.to_owned()),
                    Some(NodeKind::Variable | NodeKind::Parameter | NodeKind::Attribute) => {
                        targets.extend(self.callables(graph, returned));
                    }
                    _ => {}
                }
            }
            for callable in targets {
                let carried = PyDict::new(py);
                if let Some(span) = span {
                    carried.set_item("span", *span)?;
                }
                carried.set_item(resolution::KEY, resolution::Resolution::Flow.as_str())?;
                let name = graph.node_map().get(function).map(|n| n.get().qualified_name.clone()).unwrap_or_default();
                carried.set_item("via", format!("{name}()"))?;
                let _ = file;
                added.push(Relation {
                    source_id: caller.clone(),
                    target_id: callable,
                    kind: RelationKind::Calls,
                    metadata: carried.unbind(),
                });
            }
        }
        let count = added.len();
        for relation in added {
            graph.push_relation(Py::new(py, relation)?);
        }
        Ok(count)
    }
}
