//! Места использования символов и классификация происхождения импортов —
//! общее для всех языковых визиторов.

use std::collections::HashSet;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

use crate::span::Span;

/// Место использования, которое резолвер потом привяжет к определению.
///
/// Координаты 1-based. Роли:
/// `call` — место вызова функции/метода; `read` — чтение идентификатора;
/// `write` — цель присваивания; `annotation` — аннотация типа или правая
/// часть TypeAlias; `base` — базовый класс в списке наследования.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all, from_py_object)]
#[derive(Clone)]
pub struct OccurrenceRef {
    pub role: String,
    pub line: u32,
    pub col: u32,
    pub enclosing_id: String,
    pub span: Span,
}

#[gen_stub_pymethods]
#[pymethods]
impl OccurrenceRef {
    #[new]
    fn new(role: String, line: u32, col: u32, enclosing_id: String, span: Span) -> Self {
        Self { role, line, col, enclosing_id, span }
    }

    fn __repr__(&self) -> String {
        format!(
            "OccurrenceRef({} at {}:{} in {:?})",
            self.role, self.line, self.col, self.enclosing_id
        )
    }
}

/// Определяет происхождение импорта по заранее посчитанным наборам имён.
///
/// Значения (ложатся в `Node.metadata["origin"]`): `stdlib`, `internal`,
/// `third_party`, `unknown` — последнее означает «ни в одном наборе»
/// (транзитивная зависимость или её отсутствие).
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen)]
#[derive(Default)]
pub struct ImportClassifier {
    stdlib: HashSet<String>,
    third_party: HashSet<String>,
    internal: HashSet<String>,
}

#[gen_stub_pymethods]
#[pymethods]
impl ImportClassifier {
    #[new]
    #[pyo3(signature = (stdlib = None, third_party = None, internal = None))]
    fn new(
        stdlib: Option<HashSet<String>>,
        third_party: Option<HashSet<String>>,
        internal: Option<HashSet<String>>,
    ) -> Self {
        Self {
            stdlib: stdlib.unwrap_or_default(),
            third_party: third_party.unwrap_or_default(),
            internal: internal.unwrap_or_default(),
        }
    }

    /// Происхождение по имени модуля верхнего уровня.
    #[pyo3(name = "classify")]
    fn py_classify(&self, top_level: &str) -> &'static str {
        self.classify(top_level)
    }

    fn __repr__(&self) -> String {
        format!(
            "ImportClassifier(stdlib={}, third_party={}, internal={})",
            self.stdlib.len(),
            self.third_party.len(),
            self.internal.len()
        )
    }
}

impl ImportClassifier {
    pub fn from_sets(
        stdlib: HashSet<String>,
        third_party: HashSet<String>,
        internal: HashSet<String>,
    ) -> Self {
        Self { stdlib, third_party, internal }
    }

    pub fn classify(&self, top_level: &str) -> &'static str {
        if self.stdlib.contains(top_level) {
            return "stdlib";
        }
        if self.internal.contains(top_level) {
            return "internal";
        }
        if self.third_party.contains(top_level) {
            return "third_party";
        }
        "unknown"
    }
}
