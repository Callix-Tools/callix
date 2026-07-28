//! Декодер того подмножества SCIP, которое нужно резолверу.
//!
//! SCIP (<https://github.com/sourcegraph/scip>) — protobuf-индекс, который
//! пишет `rust-analyzer scip`. Из каждого occurrence нужны только три вещи:
//! символ, битовое поле ролей и начало диапазона, — поэтому вместо
//! protobuf-рантайма формат разбирается напрямую. Номера полей взяты из
//! `scip.proto`:
//!
//! * `Index.documents` = 2
//! * `Document.relative_path` = 1, `Document.occurrences` = 2
//! * `Occurrence.range` = 1 (packed `int32`), `Occurrence.symbol` = 2,
//!   `Occurrence.symbol_roles` = 3
//!
//! Координаты в SCIP 0-based: `[start_line, start_char, ...]`.

/// Бит `Occurrence.symbol_roles`, поднятый на месте определения.
pub const ROLE_DEFINITION: u32 = 0x1;

// Типы wire-формата, которые мы разбираем.
const WIRE_VARINT: u8 = 0;
const WIRE_I64: u8 = 1;
const WIRE_LEN: u8 = 2;
const WIRE_I32: u8 = 5;

// Номера полей из scip.proto.
const INDEX_DOCUMENTS: u32 = 2;
const DOC_RELATIVE_PATH: u32 = 1;
const DOC_OCCURRENCES: u32 = 2;
const OCC_RANGE: u32 = 1;
const OCC_SYMBOL: u32 = 2;
const OCC_ROLES: u32 = 3;

/// Диапазон occurrence содержит как минимум `[start_line, start_col]`.
const RANGE_MIN_LEN: usize = 2;

/// Одно вхождение символа в документ. Координаты 0-based.
pub struct ScipOccurrence {
    pub symbol: String,
    pub roles: u32,
    pub start_line: u32,
    pub start_col: u32,
}

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

/// Base-128 varint по смещению `i`; None на обрыве буфера.
fn read_varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut shift = 0;
    let mut result: u64 = 0;
    loop {
        let byte = *buf.get(*i)?;
        *i += 1;
        result |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Разбирает поля сообщения, отдавая каждое в `visit`.
///
/// Неизвестные, но корректные поля тоже отдаются — их просто пропускают.
/// Битый буфер обрывает обход, а не роняет процесс.
fn for_each_field(buf: &[u8], mut visit: impl FnMut(u32, Field<'_>)) {
    let mut i = 0;
    while i < buf.len() {
        let Some(tag) = read_varint(buf, &mut i) else {
            return;
        };
        let field_number = (tag >> 3) as u32;
        let wire_type = (tag & 0x7) as u8;
        match wire_type {
            WIRE_VARINT => match read_varint(buf, &mut i) {
                Some(value) => visit(field_number, Field::Varint(value)),
                None => return,
            },
            WIRE_LEN => {
                let Some(length) = read_varint(buf, &mut i) else {
                    return;
                };
                let end = i.saturating_add(length as usize);
                let Some(slice) = buf.get(i..end) else {
                    return;
                };
                visit(field_number, Field::Bytes(slice));
                i = end;
            }
            WIRE_I64 => i += 8,
            WIRE_I32 => i += 4,
            // Группы (3/4) в SCIP не используются.
            _ => return,
        }
    }
}

fn packed_varints(buf: &[u8], out: &mut Vec<u64>) {
    let mut i = 0;
    while i < buf.len() {
        match read_varint(buf, &mut i) {
            Some(value) => out.push(value),
            None => return,
        }
    }
}

fn parse_occurrence(buf: &[u8]) -> Option<ScipOccurrence> {
    let mut symbol = String::new();
    let mut roles: u32 = 0;
    let mut range: Vec<u64> = Vec::new();

    for_each_field(buf, |field_number, value| match (field_number, value) {
        // packed — обычный случай
        (OCC_RANGE, Field::Bytes(bytes)) => packed_varints(bytes, &mut range),
        // распакованный вариант тоже допустим
        (OCC_RANGE, Field::Varint(value)) => range.push(value),
        (OCC_SYMBOL, Field::Bytes(bytes)) => {
            symbol = String::from_utf8_lossy(bytes).into_owned();
        }
        (OCC_ROLES, Field::Varint(value)) => roles = value as u32,
        _ => {}
    });

    if range.len() < RANGE_MIN_LEN {
        return None;
    }
    Some(ScipOccurrence {
        symbol,
        roles,
        start_line: range[0] as u32,
        start_col: range[1] as u32,
    })
}

fn parse_document(buf: &[u8]) -> (String, Vec<ScipOccurrence>) {
    let mut relative_path = String::new();
    let mut occurrences = Vec::new();
    for_each_field(buf, |field_number, value| {
        if let Field::Bytes(bytes) = value {
            match field_number {
                DOC_RELATIVE_PATH => relative_path = String::from_utf8_lossy(bytes).into_owned(),
                DOC_OCCURRENCES => {
                    if let Some(occurrence) = parse_occurrence(bytes) {
                        occurrences.push(occurrence);
                    }
                }
                _ => {}
            }
        }
    });
    (relative_path, occurrences)
}

/// Отдаёт `(relative_path, occurrences)` для каждого документа индекса.
///
/// Документы идут по одному, чтобы вызывающий сворачивал каждый в свои
/// таблицы и сразу отпускал — на больших воркспейсах индекс сам по себе
/// весит сотни мегабайт.
pub fn for_each_document(data: &[u8], mut visit: impl FnMut(String, Vec<ScipOccurrence>)) {
    for_each_field(data, |field_number, value| {
        if field_number == INDEX_DOCUMENTS
            && let Field::Bytes(bytes) = value
        {
            let (relative_path, occurrences) = parse_document(bytes);
            visit(relative_path, occurrences);
        }
    });
}
