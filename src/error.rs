use pyo3::PyErr;
use pyo3::exceptions::{PyException, PyOSError};
use pyo3_stub_gen::create_exception;

// Модуль передаётся токенами (не строкой) — stringify! даёт "callix._core".
create_exception!(callix._core, CallixError, PyException, "Базовая ошибка callix.");
create_exception!(callix._core, ParseError, CallixError, "Не удалось разобрать исходник.");
create_exception!(callix._core, ResolverTimeout, ParseError, "Резолвер не уложился в таймаут.");
create_exception!(callix._core, DuplicateNodeError, CallixError, "Узел с таким ID уже есть в графе.");
create_exception!(callix._core, SerializationError, CallixError, "Ошибка (де)сериализации графа.");
create_exception!(callix._core, AdapterError, CallixError, "Ошибка на этапе работы адаптера.");

#[derive(thiserror::Error, Debug)]
pub enum CoreError {
    #[error("failed to parse {path}: {reason}")]
    Parse { path: String, reason: String },
    #[error("resolver timed out after {0}ms")]
    Timeout(u64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<CoreError> for PyErr {
    fn from(e: CoreError) -> PyErr {
        match e {
            CoreError::Parse { .. } => ParseError::new_err(e.to_string()),
            CoreError::Timeout(_) => ResolverTimeout::new_err(e.to_string()),
            CoreError::Io(io) => PyOSError::new_err(io.to_string()),
        }
    }
}
