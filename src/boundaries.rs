//! Границы между сервисами и нормализация их ключей.
//!
//! Межъязыковое сопоставление работает, только если Python
//! `@app.get("/users/{id}")` и TypeScript `fetch("/users/1")` сводятся к
//! одному ключу, поэтому нормализация живёт в ядре и общая для всех
//! адаптеров.

use indexmap::IndexMap;

use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pyfunction, gen_stub_pymethods};

/// Один *порт* на межъязыковой границе, найденный в исходнике.
///
/// Граница — это контракт между сервисами, который не разрешает ни один
/// компилятор: HTTP-маршрут, gRPC-метод, топик очереди, активность
/// Temporal. Каждая сторона контракта (сервер — `exposes`, клиент —
/// `consumes`) и есть порт.
///
/// Координаты 1-based и указывают на место порта (декоратор маршрута,
/// вызов `fetch`, вызов `publish`), чтобы адаптер смог сопоставить его с
/// объемлющим объявлением.
#[gen_stub_pyclass]
#[pyclass(module = "callix._core", frozen, get_all, from_py_object)]
#[derive(Clone)]
pub struct BoundaryRef {
    /// Семейство границы: `http` | `grpc` | `queue` | `temporal`.
    pub mechanism: String,
    /// `server` (предоставляет контракт) или `client` (потребляет).
    pub role: String,
    /// Нормализованный ключ сопоставления, например `GET /users/{}`.
    pub key: String,
    pub line: u32,
    pub col: u32,
    /// Уверенность экстрактора: 1.0 — литерал, меньше — вывод по контексту.
    pub confidence: f64,
    /// Человекочитаемый контекст: метод, путь, топик, фреймворк.
    pub detail: IndexMap<String, String>,
}

#[gen_stub_pymethods]
#[pymethods]
impl BoundaryRef {
    #[new]
    #[pyo3(signature = (mechanism, role, key, line, col, confidence = 1.0, detail = None))]
    fn new(
        mechanism: String,
        role: String,
        key: String,
        line: u32,
        col: u32,
        confidence: f64,
        detail: Option<IndexMap<String, String>>,
    ) -> Self {
        Self {
            mechanism,
            role,
            key,
            line,
            col,
            confidence,
            detail: detail.unwrap_or_default(),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "BoundaryRef({} {} {:?} at {}:{})",
            self.mechanism, self.role, self.key, self.line, self.col
        )
    }
}

/// Схлопывает параметр пути в `{}`, если сегмент им является.
fn collapse_params(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;

    while !rest.is_empty() {
        // {id} — FastAPI/Starlette
        if let Some(start) = rest.find('{')
            && let Some(end) = rest[start..].find('}')
        {
            out.push_str(&rest[..start]);
            out.push_str("{}");
            rest = &rest[start + end + 1..];
            continue;
        }
        break;
    }
    out.push_str(rest);

    // <int:id> — Flask
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { break };
        result.push_str(&rest[..start]);
        result.push_str("{}");
        rest = &rest[start + end + 1..];
    }
    result.push_str(rest);

    // :id — Express, только в начале сегмента, чтобы двоеточие внутри
    // сегмента (`/v1/users/123:activate`, `sha256:abc`) осталось как есть.
    result
        .split('/')
        .enumerate()
        .map(|(i, segment)| {
            if i > 0 && segment.starts_with(':') && segment.len() > 1 {
                "{}".to_string()
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Приводит маршрут или URL к ключу, не зависящему от хоста и параметров.
///
/// Убирает схему и хост, query и фрагмент; схлопывает параметры пути всех
/// стилей и конкретные числовые id (`/users/1` встречается с `/users/{}`);
/// снимает завершающий слэш, кроме корня.
#[gen_stub_pyfunction]
#[pyfunction]
pub fn normalize_http_path(raw: &str) -> String {
    let mut path = raw.trim().to_string();

    if let Some((_scheme, after)) = path.split_once("://") {
        path = match after.find('/') {
            Some(slash) => after[slash..].to_string(),
            None => "/".to_string(),
        };
    }
    path = path
        .split_once('?')
        .map_or(path.as_str(), |(before, _)| before)
        .to_string();
    path = path
        .split_once('#')
        .map_or(path.as_str(), |(before, _)| before)
        .to_string();
    if !path.starts_with('/') {
        path.insert(0, '/');
    }

    path = collapse_params(&path);

    // Числовые сегменты — тоже параметры.
    path = path
        .split('/')
        .map(|segment| {
            if !segment.is_empty() && segment.chars().all(char::is_numeric) {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    if path.len() > 1 {
        let trimmed = path.trim_end_matches('/');
        path = if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() };
    }
    path
}
