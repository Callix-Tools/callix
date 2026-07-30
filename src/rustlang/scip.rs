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

#[cfg(test)]
mod tests {
    use super::*;

    // --- a minimal protobuf writer -------------------------------------
    //
    // Encoding by hand is the point: the decoder is hand-written, so a
    // hand-written encoder is the only independent check on it. Wire types
    // are spelled out rather than derived from the field, so a test can
    // deliberately write a field with the wrong one.

    fn varint(value: u64, out: &mut Vec<u8>) {
        let mut rest = value;
        loop {
            let byte = (rest & 0x7F) as u8;
            rest >>= 7;
            if rest == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn tag(field_number: u32, wire_type: u8, out: &mut Vec<u8>) {
        varint((u64::from(field_number) << 3) | u64::from(wire_type), out);
    }

    fn varint_field(field_number: u32, value: u64, out: &mut Vec<u8>) {
        tag(field_number, WIRE_VARINT, out);
        varint(value, out);
    }

    fn bytes_field(field_number: u32, payload: &[u8], out: &mut Vec<u8>) {
        tag(field_number, WIRE_LEN, out);
        varint(payload.len() as u64, out);
        out.extend_from_slice(payload);
    }

    fn packed(values: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            varint(*value, &mut out);
        }
        out
    }

    fn occurrence(symbol: &str, roles: u64, range: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        bytes_field(OCC_RANGE, &packed(range), &mut out);
        bytes_field(OCC_SYMBOL, symbol.as_bytes(), &mut out);
        varint_field(OCC_ROLES, roles, &mut out);
        out
    }

    fn document(path: &str, occurrences: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        bytes_field(DOC_RELATIVE_PATH, path.as_bytes(), &mut out);
        for occurrence in occurrences {
            bytes_field(DOC_OCCURRENCES, occurrence, &mut out);
        }
        out
    }

    fn index(documents: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for doc in documents {
            bytes_field(INDEX_DOCUMENTS, doc, &mut out);
        }
        out
    }

    /// Collects a whole index into a comparable shape.
    type Decoded = Vec<(String, Vec<(String, u32, u32, u32)>)>;

    fn decode(data: &[u8]) -> Decoded {
        let mut out: Decoded = Vec::new();
        for_each_document(data, |path, occurrences| {
            out.push((
                path,
                occurrences
                    .into_iter()
                    .map(|o| (o.symbol, o.roles, o.start_line, o.start_col))
                    .collect(),
            ));
        });
        out
    }

    /// Runs the decoder on another thread and fails if it has not returned
    /// within a generous budget. Malformed input must terminate, not spin —
    /// `read_varint` returning None inside a `while i < buf.len()` loop is
    /// exactly the shape that hangs when a cursor stops advancing.
    fn decode_within_a_second(data: Vec<u8>) -> Decoded {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = sender.send(decode(&data));
        });
        let decoded = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the decoder did not terminate on malformed input");
        worker.join().expect("the decoder panicked");
        decoded
    }

    // --- the well-formed case -----------------------------------------

    #[test]
    fn decodes_documents_and_their_occurrences() {
        let data = index(&[
            document(
                "src/lib.rs",
                &[
                    occurrence("rust-analyzer cargo callix 0.1.0 ids/node_id().", 1, &[12, 7, 12, 14]),
                    occurrence("local 3", 0, &[20, 4, 20, 9]),
                ],
            ),
            document("src/main.rs", &[occurrence("main().", 1, &[0, 0])]),
        ]);

        assert_eq!(
            decode(&data),
            vec![
                (
                    "src/lib.rs".to_string(),
                    vec![
                        (
                            "rust-analyzer cargo callix 0.1.0 ids/node_id().".to_string(),
                            1,
                            12,
                            7
                        ),
                        ("local 3".to_string(), 0, 20, 4),
                    ]
                ),
                ("src/main.rs".to_string(), vec![("main().".to_string(), 1, 0, 0)]),
            ]
        );
    }

    #[test]
    fn accepts_the_unpacked_range_encoding() {
        // proto3 allows repeated scalars unpacked; rust-analyzer emits packed,
        // but a conformant writer may not.
        let mut occ = Vec::new();
        varint_field(OCC_RANGE, 4, &mut occ);
        varint_field(OCC_RANGE, 9, &mut occ);
        bytes_field(OCC_SYMBOL, b"x().", &mut occ);
        varint_field(OCC_ROLES, ROLE_DEFINITION.into(), &mut occ);

        assert_eq!(
            decode(&index(&[document("a.rs", &[occ])])),
            vec![("a.rs".to_string(), vec![("x().".to_string(), 1, 4, 9)])]
        );
    }

    /// A range shorter than `[start_line, start_col]` cannot place the symbol,
    /// so the occurrence is dropped rather than defaulted to 0:0.
    #[test]
    fn drops_an_occurrence_without_a_usable_range() {
        let short = occurrence("x().", 1, &[7]);
        let empty = occurrence("y().", 1, &[]);
        let decoded = decode(&index(&[document("a.rs", &[short, empty])]));
        assert_eq!(decoded, vec![("a.rs".to_string(), Vec::new())]);
    }

    #[test]
    fn a_document_may_be_empty() {
        // A zero-length payload for a nested message must not confuse the
        // walk: the path is empty, the occurrence list is empty, and the
        // document still shows up.
        let mut data = Vec::new();
        bytes_field(INDEX_DOCUMENTS, &[], &mut data);
        assert_eq!(decode(&data), vec![(String::new(), Vec::new())]);

        // A zero-length occurrence inside a real document is skipped.
        let mut doc = Vec::new();
        bytes_field(DOC_RELATIVE_PATH, b"a.rs", &mut doc);
        bytes_field(DOC_OCCURRENCES, &[], &mut doc);
        assert_eq!(
            decode(&index(&[doc])),
            vec![("a.rs".to_string(), Vec::new())]
        );

        assert!(decode(&[]).is_empty());
    }

    /// A symbol name is decoded lossily rather than failing the whole index.
    #[test]
    fn invalid_utf8_in_a_symbol_is_replaced() {
        let mut occ = Vec::new();
        bytes_field(OCC_RANGE, &packed(&[1, 2]), &mut occ);
        bytes_field(OCC_SYMBOL, &[b'a', 0xFF, b'b'], &mut occ);
        let decoded = decode(&index(&[document("a.rs", &[occ])]));
        assert_eq!(decoded[0].1[0].0, "a\u{FFFD}b");
    }

    // --- unknown fields ------------------------------------------------

    /// SCIP carries far more than the three fields read here, and a newer
    /// rust-analyzer may add more. Every wire type has to be skippable, or
    /// one unknown field derails the rest of the message.
    #[test]
    fn unknown_fields_are_skipped_for_every_wire_type() {
        let unknown_field = 99;
        for (wire_type, payload) in [
            (WIRE_VARINT, vec![0x96, 0x01]),       // 150
            (WIRE_I64, vec![1, 2, 3, 4, 5, 6, 7, 8]),
            (WIRE_LEN, vec![3, b'a', b'b', b'c']), // length-prefixed
            (WIRE_I32, vec![1, 2, 3, 4]),
        ] {
            let mut doc = Vec::new();
            tag(unknown_field, wire_type, &mut doc);
            doc.extend_from_slice(&payload);
            // The fields that matter come *after* the unknown one, so a
            // mis-skip loses them.
            bytes_field(DOC_RELATIVE_PATH, b"a.rs", &mut doc);
            bytes_field(DOC_OCCURRENCES, &occurrence("x().", 1, &[5, 6]), &mut doc);

            assert_eq!(
                decode(&index(&[doc])),
                vec![("a.rs".to_string(), vec![("x().".to_string(), 1, 5, 6)])],
                "wire type {wire_type} was not skipped cleanly"
            );
        }
    }

    /// Groups (wire types 3 and 4) and the two unassigned values end the walk
    /// rather than being guessed at.
    #[test]
    fn an_unsupported_wire_type_stops_the_walk_without_panicking() {
        for wire_type in [3u8, 4, 6, 7] {
            let mut doc = Vec::new();
            bytes_field(DOC_RELATIVE_PATH, b"a.rs", &mut doc);
            tag(99, wire_type, &mut doc);
            bytes_field(DOC_OCCURRENCES, &occurrence("x().", 1, &[5, 6]), &mut doc);

            // Everything before the bad tag survives; everything after is lost.
            assert_eq!(
                decode_within_a_second(index(&[doc])),
                vec![("a.rs".to_string(), Vec::new())],
                "wire type {wire_type}"
            );
        }
    }

    // --- malformed input ------------------------------------------------

    #[test]
    fn truncation_mid_varint_terminates() {
        let mut data = index(&[document(
            "src/lib.rs",
            &[occurrence("x().", 1, &[10, 20])],
        )]);
        // A dangling continuation byte: the varint never ends.
        data.push(0x80);
        assert_eq!(
            decode_within_a_second(data),
            vec![("src/lib.rs".to_string(), vec![("x().".to_string(), 1, 10, 20)])]
        );

        // Truncated in the middle of a tag varint, before anything is read.
        assert!(decode_within_a_second(vec![0x80, 0x80, 0x80]).is_empty());
    }

    #[test]
    fn truncation_mid_payload_terminates() {
        let full = index(&[document("src/lib.rs", &[occurrence("x().", 1, &[10, 20])])]);
        // Every prefix of a valid index must decode to *something* without
        // panicking or spinning.
        for cut in 0..full.len() {
            let decoded = decode_within_a_second(full[..cut].to_vec());
            assert!(decoded.len() <= 1, "prefix of length {cut}");
        }

        // A length that overruns the buffer is rejected outright.
        let mut lying = Vec::new();
        tag(INDEX_DOCUMENTS, WIRE_LEN, &mut lying);
        varint(4096, &mut lying);
        lying.extend_from_slice(b"short");
        assert!(decode_within_a_second(lying).is_empty());
    }

    #[test]
    fn a_varint_longer_than_ten_bytes_is_rejected() {
        // Ten bytes is the maximum for a u64; the tenth uses shift 63.
        let mut ten = vec![0xFFu8; 9];
        ten.push(0x01);
        let mut i = 0;
        assert!(read_varint(&ten, &mut i).is_some());
        assert_eq!(i, 10);

        // Eleven continuation bytes overflow and must yield None rather than
        // shifting past the width of a u64.
        let eleven = [0xFFu8; 11];
        let mut i = 0;
        assert_eq!(read_varint(&eleven, &mut i), None);

        // And the same buffer as a field value must not hang the walk.
        let mut data = Vec::new();
        tag(INDEX_DOCUMENTS, WIRE_VARINT, &mut data);
        data.extend_from_slice(&[0xFFu8; 12]);
        assert!(decode_within_a_second(data).is_empty());
    }

    #[test]
    fn read_varint_reports_truncation_rather_than_a_partial_value() {
        let mut i = 0;
        assert_eq!(read_varint(&[], &mut i), None);
        let mut i = 0;
        assert_eq!(read_varint(&[0x80], &mut i), None);
        // Multi-byte values are little-endian base 128.
        let mut i = 0;
        assert_eq!(read_varint(&[0x96, 0x01], &mut i), Some(150));
        assert_eq!(i, 2);
    }

    #[test]
    fn packed_varints_stop_at_the_first_truncated_value() {
        let mut values = Vec::new();
        let mut buf = packed(&[1, 2, 300]);
        buf.push(0x80); // dangling
        packed_varints(&buf, &mut values);
        assert_eq!(values, [1, 2, 300]);
    }

    // --- the roles bitfield ---------------------------------------------

    /// `symbol_roles` is a bit set: `ReadAccess`, `Generated`, `Test` and
    /// others live alongside `Definition`, so the check has to be a mask and
    /// not an equality.
    #[test]
    fn role_definition_is_a_mask_not_a_value() {
        assert_eq!(ROLE_DEFINITION, 0x1);

        let definition_plus_others = 0x1 | 0x8 | 0x20;
        assert_ne!(definition_plus_others & ROLE_DEFINITION, 0);

        let no_definition = 0x8 | 0x20;
        assert_eq!(no_definition & ROLE_DEFINITION, 0);
    }

    /// An occurrence with no roles at all is a plain reference.
    ///
    /// Asserted through the decoder rather than as `0 & ROLE_DEFINITION`,
    /// which is arithmetic that cannot fail and so tests nothing.
    #[test]
    fn an_occurrence_with_no_roles_is_not_a_definition() {
        let decoded = decode(&index(&[document(
            "a.rs",
            &[occurrence("x().", 0, &[1, 2])],
        )]));
        assert_eq!(decoded[0].1[0].1, 0);
        assert_eq!(decoded[0].1[0].1 & ROLE_DEFINITION, 0);
    }

    #[test]
    fn roles_survive_the_round_trip_with_other_bits_set() {
        let roles = 0x1 | 0x2 | 0x40;
        let decoded = decode(&index(&[document(
            "a.rs",
            &[occurrence("x().", u64::from(roles), &[1, 2])],
        )]));
        let recorded = decoded[0].1[0].1;
        assert_eq!(recorded, roles);
        assert_ne!(recorded & ROLE_DEFINITION, 0);
    }
}
