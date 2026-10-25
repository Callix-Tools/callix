#![allow(dead_code)] // each test binary uses a different part

//! Shared by the integration tests.

use serde_json::Value;

pub const SAMPLE: &str = include_str!("../data/sample.json");

/// Parses JSON Lines, asserts the dump is structurally valid and returns it.
pub fn check_lsif(text: &str) -> Vec<Value> {
    if let Err(problems) = callix_export::lsif::check::validate(text) {
        panic!("invalid LSIF:\n  {}", problems.join("\n  "));
    }
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

pub fn labelled<'a>(elements: &'a [Value], label: &str) -> Vec<&'a Value> {
    elements.iter().filter(|e| e["type"] == "vertex" && e["label"] == label).collect()
}

pub fn edges<'a>(elements: &'a [Value], label: &str) -> Vec<&'a Value> {
    elements.iter().filter(|e| e["type"] == "edge" && e["label"] == label).collect()
}
