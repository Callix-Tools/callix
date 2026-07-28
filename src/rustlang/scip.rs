//! A decoder for the subset of SCIP the resolver consumes.
//!
//! SCIP (<https://github.com/sourcegraph/scip>) is the protobuf index emitted
//! by `rust-analyzer scip`. Only three things are needed per occurrence — the
//! symbol, the roles bitfield, and the start of the range — so rather than
//! pull in a protobuf runtime the wire format is decoded directly. The field
//! numbers come from
//! `scip.proto`:
//!
//! * `Index.documents` = 2
//! * `Document.relative_path` = 1, `Document.occurrences` = 2
//! * `Occurrence.range` = 1 (packed `int32`), `Occurrence.symbol` = 2,
//!   `Occurrence.symbol_roles` = 3
//!
//! Coordinates in SCIP are 0-based: `[start_line, start_char, ...]`.

/// The `Occurrence.symbol_roles` bit set at a definition site.
pub const ROLE_DEFINITION: u32 = 0x1;

// The wire types we handle.
const WIRE_VARINT: u8 = 0;
const WIRE_I64: u8 = 1;
const WIRE_LEN: u8 = 2;
const WIRE_I32: u8 = 5;

// Field numbers from scip.proto.
const INDEX_DOCUMENTS: u32 = 2;
const DOC_RELATIVE_PATH: u32 = 1;
const DOC_OCCURRENCES: u32 = 2;
const OCC_RANGE: u32 = 1;
const OCC_SYMBOL: u32 = 2;
const OCC_ROLES: u32 = 3;

/// An occurrence range carries at least `[start_line, start_col]`.
const RANGE_MIN_LEN: usize = 2;

/// One occurrence of a symbol in a document. Coordinates are 0-based.
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

/// A base-128 varint at offset `i`; None when the buffer is truncated.
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

/// Iterates a message's fields, handing each to `visit`.
///
/// Unknown-but-well-formed fields are yielded too — callers simply skip them.
/// A corrupt buffer ends the walk rather than crashing the process.
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
            // Groups (3/4) are not used by SCIP.
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
        // packed — the common case
        (OCC_RANGE, Field::Bytes(bytes)) => packed_varints(bytes, &mut range),
        // the unpacked variant is valid too
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

/// Yields `(relative_path, occurrences)` for every document in the index.
///
/// Documents are streamed one at a time so the caller can fold each into its
/// lookup tables and drop it — on large workspaces the index alone weighs
/// hundreds of megabytes.
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
