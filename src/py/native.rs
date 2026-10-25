//! PyO3's surface, as far as the adapters use it, in plain Rust.
//!
//! This is what the crate builds against without the `python` feature. It is
//! deliberately not a general Python object model: values are JSON-shaped
//! (`Value`), a `PyDict` is an insertion-ordered map of them behind a lock, and
//! `Py<T>` / `Bound<T>` are reference-counted handles. Where real PyO3 would
//! call into the interpreter, this does the obvious Rust thing, which keeps
//! the two builds behaving alike: dictionaries keep insertion order, mutation
//! through a shared handle is visible to every holder, and a `Span` stored in
//! metadata is a `Span`.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;

use crate::span::Span;

pub mod prelude {
    pub use callix_compat::{pyclass, pyfunction, pymethods, pymodule};

    pub use super::{Bound, Py, PyAny, PyErr, PyResult, Python};
}

pub mod derive {
    pub use callix_compat::{
        gen_stub_pyclass, gen_stub_pyclass_enum, gen_stub_pymethods, gen_stub_pyfunction,
    };
}

pub mod types {
    pub use super::{PyBytes, PyDict, PyList};
}

pub mod exceptions {
    pub use super::{PyException, PyOSError, PyRuntimeError, PyValueError};
}

/// The token that proves the interpreter is held. There is none here, so it is
/// a marker the signatures keep carrying.
#[derive(Clone, Copy)]
pub struct Python<'py>(PhantomData<&'py ()>);

impl Python<'_> {
    /// Runs `f` with a token, as `Python::attach` does.
    pub fn attach<R>(f: impl for<'py> FnOnce(Python<'py>) -> R) -> R {
        f(Python(PhantomData))
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An error with the name of the exception it would have been.
#[derive(Debug, Clone)]
pub struct PyErr {
    kind: &'static str,
    message: String,
}

impl PyErr {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// `"AdapterError"`, `"ParseError"`, `"ValueError"`…
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PyErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for PyErr {}

impl From<std::ffi::NulError> for PyErr {
    fn from(err: std::ffi::NulError) -> Self {
        Self::new("ValueError", err.to_string())
    }
}

impl From<std::io::Error> for PyErr {
    fn from(err: std::io::Error) -> Self {
        Self::new("OSError", err.to_string())
    }
}

pub type PyResult<T> = Result<T, PyErr>;

macro_rules! exception {
    ($($name:ident),* $(,)?) => {$(
        pub struct $name;

        impl $name {
            pub fn new_err(message: impl Into<String>) -> PyErr {
                PyErr::new(stringify!($name), message)
            }
        }
    )*};
}

exception!(PyException, PyOSError, PyRuntimeError, PyValueError);

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// What a metadata dictionary can hold: JSON, plus the `Span` the visitors
/// store directly.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Dict(IndexMap<String, Value>),
    Span(Span),
}

impl fmt::Display for Value {
    /// `str(value)`, as far as the callers rely on it: a string prints bare.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::None => f.write_str("None"),
            Value::Bool(true) => f.write_str("True"),
            Value::Bool(false) => f.write_str("False"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => f.write_str(s),
            Value::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Value::Dict(map) => {
                f.write_str("{")?;
                for (i, (key, item)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{key}: {item}")?;
                }
                f.write_str("}")
            }
            Value::Span(s) => {
                write!(f, "Span({}, {}, {}, {})", s.start_line, s.start_col, s.end_line, s.end_col)
            }
        }
    }
}

/// Anything that can be stored in a dictionary.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

macro_rules! into_value_int {
    ($($t:ty),*) => {$(
        impl IntoValue for $t {
            fn into_value(self) -> Value {
                Value::Int(i64::try_from(self).unwrap_or(i64::MAX))
            }
        }
    )*};
}

into_value_int!(i32, i64, u32, u64, usize);

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Float(self)
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::Str(self.to_owned())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::Str(self)
    }
}

impl IntoValue for &String {
    fn into_value(self) -> Value {
        Value::Str(self.clone())
    }
}

impl IntoValue for Span {
    fn into_value(self) -> Value {
        Value::Span(self)
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Value {
        self.map_or(Value::None, IntoValue::into_value)
    }
}

impl<T: IntoValue> IntoValue for Vec<T> {
    fn into_value(self) -> Value {
        Value::List(self.into_iter().map(IntoValue::into_value).collect())
    }
}

impl<T: IntoValue + Clone> IntoValue for &Vec<T> {
    fn into_value(self) -> Value {
        Value::List(self.iter().cloned().map(IntoValue::into_value).collect())
    }
}

impl<T: IntoValue + Clone> IntoValue for &[T] {
    fn into_value(self) -> Value {
        Value::List(self.iter().cloned().map(IntoValue::into_value).collect())
    }
}

impl<T: IntoValue + Clone> IntoValue for &T
where
    T: sealed::NotPrimitive,
{
    fn into_value(self) -> Value {
        self.clone().into_value()
    }
}

mod sealed {
    /// Marks the handle types for which a reference stores a snapshot.
    pub trait NotPrimitive {}
}

/// What `extract::<T>()` can produce.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Option<Self>;
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Float(x) => Some(*x),
            Value::Int(n) => Some(*n as f64),
            _ => None,
        }
    }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }
}

impl FromValue for u32 {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(n) => u32::try_from(*n).ok(),
            _ => None,
        }
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
}

impl FromValue for Span {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Span(s) => Some(*s),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// An owned, shared reference to a `T`.
pub struct Py<T>(Bound<'static, T>);

impl<T> Py<T> {
    pub fn new(_py: Python<'_>, value: T) -> PyResult<Self> {
        Ok(Self(Bound(Arc::new(value), PhantomData)))
    }

    /// The value, for the immutable (`frozen`) classes.
    pub fn get(&self) -> &T {
        &self.0.0
    }

    pub fn clone_ref(&self, _py: Python<'_>) -> Self {
        Self(self.0.clone())
    }

    /// A borrowed `Bound`. `Bound` is covariant in its lifetime, so the
    /// `'static` one stored here narrows to whatever the caller holds.
    pub fn bind<'py>(&'py self, _py: Python<'py>) -> &'py Bound<'py, T> {
        &self.0
    }
}

impl<T> Clone for Py<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> fmt::Debug for Py<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Py(..)")
    }
}

/// A reference that is known to be used while the interpreter is held.
pub struct Bound<'py, T>(Arc<T>, PhantomData<&'py ()>);

impl<'py, T> Bound<'py, T> {
    pub fn unbind(self) -> Py<T> {
        Py(Bound(self.0, PhantomData))
    }

    pub fn py(&self) -> Python<'py> {
        Python(PhantomData)
    }
}

impl<T> Clone for Bound<'_, T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0), PhantomData)
    }
}

impl<T> std::ops::Deref for Bound<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// A dynamically typed value.
#[derive(Debug, Clone, PartialEq)]
pub struct PyAny(pub Value);

impl Bound<'_, PyAny> {
    pub fn extract<T: FromValue>(&self) -> PyResult<T> {
        T::from_value(&self.0.0)
            .ok_or_else(|| PyValueError::new_err(format!("cannot convert {} to the requested type", self.0.0)))
    }

    pub fn is_none(&self) -> bool {
        self.0.0 == Value::None
    }

    pub fn value(&self) -> &Value {
        &self.0.0
    }
}

impl fmt::Display for Bound<'_, PyAny> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.0.fmt(f)
    }
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

/// A dictionary with string keys, in insertion order.
#[derive(Default)]
pub struct PyDict {
    map: Mutex<IndexMap<String, Value>>,
}

impl PyDict {
    pub fn new(_py: Python<'_>) -> Bound<'_, PyDict> {
        Bound(Arc::new(Self::default()), PhantomData)
    }

    fn lock(&self) -> MutexGuard<'_, IndexMap<String, Value>> {
        self.map.lock().expect("metadata lock poisoned")
    }
}

impl Bound<'_, PyDict> {
    pub fn set_item<K: AsRef<str>, V: IntoValue>(&self, key: K, value: V) -> PyResult<()> {
        self.0.lock().insert(key.as_ref().to_owned(), value.into_value());
        Ok(())
    }

    pub fn get_item<K: AsRef<str>>(&self, key: K) -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(self
            .0
            .lock()
            .get(key.as_ref())
            .map(|value| Bound(Arc::new(PyAny(value.clone())), PhantomData)))
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.lock().len()
    }

    /// A snapshot of the entries, in insertion order.
    pub fn entries(&self) -> Vec<(String, Value)> {
        self.0.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn update(&self, other: &Bound<'_, PyDict>) -> PyResult<()> {
        let entries = other.entries();
        let mut map = self.0.lock();
        for (key, value) in entries {
            map.insert(key, value);
        }
        Ok(())
    }

    pub fn eq(&self, other: &Bound<'_, PyDict>) -> PyResult<bool> {
        Ok(self.entries() == other.entries())
    }

    pub fn as_mapping(&self) -> &Self {
        self
    }
}

impl IntoValue for &Bound<'_, PyDict> {
    fn into_value(self) -> Value {
        Value::Dict(self.entries().into_iter().collect())
    }
}

impl IntoValue for Bound<'_, PyDict> {
    fn into_value(self) -> Value {
        (&self).into_value()
    }
}

/// A list, built once and stored.
#[derive(Default)]
pub struct PyList {
    items: Vec<Value>,
}

impl PyList {
    pub fn new<'py, T: IntoValue>(
        _py: Python<'py>,
        items: impl IntoIterator<Item = T>,
    ) -> PyResult<Bound<'py, PyList>> {
        let items = items.into_iter().map(IntoValue::into_value).collect();
        Ok(Bound(Arc::new(PyList { items }), PhantomData))
    }

    pub fn empty(_py: Python<'_>) -> Bound<'_, PyList> {
        Bound(Arc::new(PyList::default()), PhantomData)
    }
}

impl IntoValue for Bound<'_, PyList> {
    fn into_value(self) -> Value {
        Value::List(self.0.items.clone())
    }
}

impl IntoValue for &Bound<'_, PyList> {
    fn into_value(self) -> Value {
        Value::List(self.0.items.clone())
    }
}

/// Source bytes handed to a boundary extractor.
pub struct PyBytes(pub Vec<u8>);

impl Bound<'_, PyBytes> {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0.0
    }
}

#[allow(dead_code)]
type Unused = BTreeMap<(), ()>;
