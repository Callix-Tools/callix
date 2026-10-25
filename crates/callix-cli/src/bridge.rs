//! The embedded interpreter that runs the callix adapters.
//!
//! The native module is registered under the name `_core` before the
//! interpreter starts, so no Python package needs to be installed: the binary
//! carries the analysis engines, the driver script and, at run time, only needs
//! the interpreter's standard library.

use std::ffi::CString;

use pyo3::prelude::*;
use pyo3::types::PyModule;
use serde::Deserialize;
use serde_json::{Value, json};

use callix::_core;

use crate::failure::{Failure, Result};

const DRIVER: &str = include_str!("driver.py");

/// What the driver reports for each language it ran.
#[derive(Debug, Deserialize)]
pub struct LanguageReport {
    pub language: String,
    pub status: Option<String>,
    pub nodes: u64,
    pub relations: u64,
    pub seconds: f64,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeInfo {
    pub languages: Vec<LanguageReport>,
    pub empty: bool,
}

pub struct Bridge {
    driver: Py<PyModule>,
}

fn describe(py: Python<'_>, err: &PyErr) -> String {
    let kind = err.get_type(py).name().map_or_else(|_| "Error".to_owned(), |n| n.to_string());
    format!("{kind}: {}", err.value(py))
}

impl Bridge {
    /// Starts the interpreter and loads the driver.
    pub fn start() -> Result<Self> {
        pyo3::append_to_inittab!(_core);
        Python::initialize();
        Python::attach(|py| {
            let code = CString::new(DRIVER).map_err(|e| Failure::new(e.to_string()))?;
            let module = PyModule::from_code(py, &code, c"callix_driver.py", c"callix_driver")
                .map_err(|e| Failure::new(format!("cannot start the analysis engine: {}", describe(py, &e))))?;
            Ok(Self { driver: module.unbind() })
        })
    }

    fn call(&self, request: &Value) -> Result<(String, Value)> {
        Python::attach(|py| {
            let handle = self
                .driver
                .bind(py)
                .getattr("handle")
                .map_err(|e| Failure::new(describe(py, &e)))?;
            let (graph, info): (String, String) = handle
                .call1((request.to_string(),))
                .and_then(|answer| answer.extract())
                .map_err(|e| Failure::new(describe(py, &e)))?;
            Ok((graph, serde_json::from_str(&info)?))
        })
    }

    /// Analyses `path` and returns the merged graph as JSON.
    pub fn analyze(
        &self,
        path: &std::path::Path,
        languages: &[String],
        resolve: bool,
        pretty: bool,
    ) -> Result<(String, AnalyzeInfo)> {
        let request = json!({
            "op": "analyze",
            "path": path.to_string_lossy(),
            "languages": if languages.is_empty() { Value::Null } else { json!(languages) },
            "resolve": resolve,
            "indent": if pretty { json!(2) } else { Value::Null },
        });
        let (graph, info) = self.call(&request)?;
        Ok((graph, serde_json::from_value(info)?))
    }

    pub fn merge(
        &self,
        graphs: &[std::path::PathBuf],
        allow_shared: bool,
        pretty: bool,
    ) -> Result<(String, Value)> {
        self.call(&json!({
            "op": "merge",
            "graphs": graphs.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
            "allow_shared": allow_shared,
            "indent": if pretty { json!(2) } else { Value::Null },
        }))
    }

    pub fn diff(&self, old: &std::path::Path, new: &std::path::Path) -> Result<Value> {
        let (_, info) = self.call(&json!({
            "op": "diff",
            "old": old.to_string_lossy(),
            "new": new.to_string_lossy(),
        }))?;
        Ok(info)
    }

    pub fn environment(&self) -> Result<Value> {
        let (_, info) = self.call(&json!({"op": "environment"}))?;
        Ok(info)
    }
}
