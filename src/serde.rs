//! (Де)сериализация значений метаданных.
//!
//! Метаданные — открытый `dict[str, object]`. Здесь они приводятся к
//! JSON-совместимому виду; `Span` переживает round-trip через тег
//! `{"__span__": [l, c, l, c]}`, поэтому разобранный граф равен исходному.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

use crate::span::Span;

pub const SPAN_TAG: &str = "__span__";
const SPAN_LEN: usize = 4;

pub fn span_to_list<'py>(py: Python<'py>, span: Span) -> Bound<'py, PyList> {
    PyList::new(
        py,
        [span.start_line, span.start_col, span.end_line, span.end_col],
    )
    .expect("4 ints always build a list")
}

/// Собирает Span из списка 4 целых; None, если форма не та.
pub fn span_from_list(value: &Bound<'_, PyAny>) -> Option<Span> {
    let list = value.cast::<PyList>().ok()?;
    if list.len() != SPAN_LEN {
        return None;
    }
    let mut nums = [0u32; SPAN_LEN];
    for (i, slot) in nums.iter_mut().enumerate() {
        let item = list.get_item(i).ok()?;
        // bool — подкласс int в Python, но координатой быть не может.
        if item.is_instance_of::<PyBool>() {
            return None;
        }
        *slot = item.extract::<u32>().ok()?;
    }
    Some(Span {
        start_line: nums[0],
        start_col: nums[1],
        end_line: nums[2],
        end_col: nums[3],
    })
}

/// Приводит значение метаданных к JSON-совместимому виду.
/// Всё, что не легло ни в одну ветку, становится `str(value)`.
pub fn encode_value<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    if let Ok(span) = value.cast::<Span>() {
        let tagged = PyDict::new(py);
        tagged.set_item(SPAN_TAG, span_to_list(py, *span.get()))?;
        return Ok(tagged.into_any());
    }
    // bool до int: в Python bool — подкласс int.
    if value.is_instance_of::<PyBool>() || value.is_none() {
        return Ok(value.clone());
    }
    if value.is_instance_of::<PyString>()
        || value.is_instance_of::<PyInt>()
        || value.is_instance_of::<PyFloat>()
    {
        return Ok(value.clone());
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        let out = PyDict::new(py);
        for (key, item) in dict.iter() {
            out.set_item(key.str()?, encode_value(py, &item)?)?;
        }
        return Ok(out.into_any());
    }
    if let Ok(seq) = value.cast::<PyList>() {
        return encode_seq(py, seq.iter());
    }
    if let Ok(seq) = value.cast::<PyTuple>() {
        return encode_seq(py, seq.iter());
    }
    Ok(value.str()?.into_any())
}

fn encode_seq<'py>(
    py: Python<'py>,
    items: impl Iterator<Item = Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let out = PyList::empty(py);
    for item in items {
        out.append(encode_value(py, &item)?)?;
    }
    Ok(out.into_any())
}

/// Обратное к [`encode_value`].
pub fn decode_value<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    if let Ok(dict) = value.cast::<PyDict>() {
        if dict.len() == 1
            && let Some(tagged) = dict.get_item(SPAN_TAG)?
            && let Some(span) = span_from_list(&tagged)
        {
            return Ok(span.into_pyobject(py)?.into_any());
        }
        let out = PyDict::new(py);
        for (key, item) in dict.iter() {
            out.set_item(key.str()?, decode_value(py, &item)?)?;
        }
        return Ok(out.into_any());
    }
    if let Ok(list) = value.cast::<PyList>() {
        let out = PyList::empty(py);
        for item in list.iter() {
            out.append(decode_value(py, &item)?)?;
        }
        return Ok(out.into_any());
    }
    Ok(value.clone())
}

/// Кодирует весь словарь метаданных.
pub fn encode_metadata<'py>(
    py: Python<'py>,
    metadata: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    for (key, value) in metadata.iter() {
        out.set_item(key.str()?, encode_value(py, &value)?)?;
    }
    Ok(out)
}

/// Декодирует метаданные; не-словарь даёт пустой словарь, а не ошибку.
pub fn decode_metadata<'py>(
    py: Python<'py>,
    metadata: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    let Some(value) = metadata else { return Ok(out) };
    let Ok(dict) = value.cast::<PyDict>() else {
        return Ok(out);
    };
    for (key, item) in dict.iter() {
        out.set_item(key.str()?, decode_value(py, &item)?)?;
    }
    Ok(out)
}
