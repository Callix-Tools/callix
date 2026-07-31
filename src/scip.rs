//! A decoder for the subset of SCIP the resolvers consume, and the position
//! tables built on top of it.
//!
//! SCIP (<https://github.com/sourcegraph/scip>) is the protobuf index format
//! emitted by `rust-analyzer scip` and by `scip-clang`. Only three things are
//! needed per occurrence — the symbol, the roles bitfield, and the start of
//! the range — so rather than pull in a protobuf runtime the wire format is
//! decoded directly. The field numbers come from `scip.proto`:
//!
//! * `Index.documents` = 2
//! * `Document.relative_path` = 1, `Document.occurrences` = 2
//! * `Occurrence.range` = 1 (packed `int32`), `Occurrence.symbol` = 2,
//!   `Occurrence.symbol_roles` = 3
//!
//! Coordinates in SCIP are 0-based: `[start_line, start_char, ...]`.
//!
//! [`ScipIndex`] is the part above the raw decoder that is *also*
//! indexer-agnostic: which document and position a symbol occurs at, and
//! where — if anywhere in this index — it is defined. Verified empirically
//! rather than assumed: `rust-analyzer`'s `local N` convention for a
//! scope-local symbol and its position-keyed synthetic names for things with
//! no stable identifier turn out to be exactly what `scip-clang` emits too, so
//! one set of tables serves both. What is *not* shared is the symbol
//! **scheme** — `rust-analyzer cargo callix 0.1.0 ids/node_id().` reads
//! nothing like `cxx . . $ leveldb/Arena#Allocate(4b58d6ff543a4736).` — so
//! classifying an unresolved symbol's origin (stdlib, third-party, unknown)
//! stays with each resolver.

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

// ---------------------------------------------------------------------------
// Position tables
// ---------------------------------------------------------------------------

use std::collections::HashMap;

/// A definition's location: the document index and 0-based coordinates.
pub type ScipLocation = (u32, u32, u32);

/// What a position resolves to.
pub enum ScipAnswer {
    /// Defined somewhere in this index — a candidate for an internal edge.
    /// Whether that document is actually one the caller's project walked is
    /// for the caller to find out; a header outside the project tree lands
    /// here too; and the shared `apply_resolutions_inner` already treats a
    /// file it does not recognize as external, so nothing further is needed
    /// here to handle that case correctly.
    Definition { doc: u32, line: u32, col: u32 },
    /// Named, but never defined in this index — a symbol from outside what
    /// was indexed (the standard library, a dependency, a vendored header).
    External { symbol: String },
}

/// The position tables built from decoding a SCIP index.
///
/// Everything here reads only the document path, the occurrence's position
/// and its symbol string — nothing that depends on how a particular indexer
/// spells a symbol. See the module docs for why one implementation serves
/// both `rust-analyzer scip` and `scip-clang`.
#[derive(Default)]
pub struct ScipIndex {
    docs: Vec<String>,
    doc_index: HashMap<String, u32>,
    /// The symbol pool: one string for many occurrences.
    symbols: Vec<String>,
    symbol_index: HashMap<String, u32>,
    /// Document → `(line, column)` → symbol. Coordinates are 0-based.
    by_doc: Vec<HashMap<(u32, u32), u32>>,
    /// Global symbol → the location of its definition.
    defs: HashMap<u32, ScipLocation>,
    /// Document → a `local …` symbol → its definition site within the
    /// document.
    local_defs: Vec<HashMap<u32, (u32, u32)>>,
}

impl ScipIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn doc_path(&self, doc: u32) -> &str {
        &self.docs[doc as usize]
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    fn intern_symbol(&mut self, symbol: &str) -> u32 {
        if let Some(index) = self.symbol_index.get(symbol) {
            return *index;
        }
        let index = self.symbols.len() as u32;
        self.symbols.push(symbol.to_string());
        self.symbol_index.insert(symbol.to_string(), index);
        index
    }

    fn intern_doc(&mut self, relative_path: &str) -> u32 {
        if let Some(index) = self.doc_index.get(relative_path) {
            return *index;
        }
        let index = self.docs.len() as u32;
        self.docs.push(relative_path.to_string());
        self.doc_index.insert(relative_path.to_string(), index);
        self.by_doc.push(HashMap::new());
        self.local_defs.push(HashMap::new());
        index
    }

    /// Folds a SCIP index into the lookup tables.
    pub fn ingest(&mut self, data: &[u8]) {
        for_each_document(data, |relative_path, occurrences| {
            let mut doc: Option<u32> = None;
            for occurrence in occurrences {
                if occurrence.symbol.is_empty() {
                    continue;
                }
                // A document is only created once it has something to find.
                let doc = *doc.get_or_insert_with(|| self.intern_doc(&relative_path));
                let is_local = occurrence.symbol.starts_with("local ");
                let index = self.intern_symbol(&occurrence.symbol);
                let position = (occurrence.start_line, occurrence.start_col);
                // Last one wins here while `defs` below keeps the first, and
                // the asymmetry is deliberate rather than an oversight. Two
                // occurrences can share a start position — a derive or a macro
                // expansion maps several symbols onto one source range — and
                // there is no principled way to prefer one, so the tie-break is
                // arbitrary either way. `defs` keeps the first because a
                // symbol's definition site should not depend on how many
                // references follow it.
                self.by_doc[doc as usize].insert(position, index);
                if occurrence.roles & ROLE_DEFINITION == 0 {
                    continue;
                }
                if is_local {
                    self.local_defs[doc as usize].entry(index).or_insert(position);
                } else {
                    self.defs.entry(index).or_insert((doc, position.0, position.1));
                }
            }
        });
    }

    /// The symbol at a document-relative, 0-based position, resolved to
    /// either its definition site or an external symbol name.
    pub fn lookup(&self, relative_doc: &str, line: u32, col: u32) -> Option<ScipAnswer> {
        let doc = *self.doc_index.get(relative_doc)?;
        let symbol = *self.by_doc[doc as usize].get(&(line, col))?;
        let name = &self.symbols[symbol as usize];
        if name.starts_with("local ") {
            let (l, c) = *self.local_defs[doc as usize].get(&symbol)?;
            return Some(ScipAnswer::Definition { doc, line: l, col: c });
        }
        if let Some(&(d, l, c)) = self.defs.get(&symbol) {
            return Some(ScipAnswer::Definition { doc: d, line: l, col: c });
        }
        Some(ScipAnswer::External { symbol: name.clone() })
    }
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

    // --- ScipIndex --------------------------------------------------------

    #[test]
    fn a_reference_resolves_to_its_definition_site() {
        let mut idx = ScipIndex::new();
        idx.ingest(&index(&[
            document("def.h", &[occurrence("pkg/Widget#make().", 1, &[10, 4])]),
            document("use.cc", &[occurrence("pkg/Widget#make().", 0, &[20, 8])]),
        ]));
        match idx.lookup("use.cc", 20, 8) {
            Some(ScipAnswer::Definition { doc, line, col }) => {
                assert_eq!(idx.doc_path(doc), "def.h");
                assert_eq!((line, col), (10, 4));
            }
            _ => panic!("expected a definition"),
        }
    }

    #[test]
    fn a_symbol_with_no_definition_anywhere_is_external() {
        let mut idx = ScipIndex::new();
        idx.ingest(&index(&[document("use.cc", &[occurrence("std/free().", 0, &[3, 1])])]));
        match idx.lookup("use.cc", 3, 1) {
            Some(ScipAnswer::External { symbol }) => assert_eq!(symbol, "std/free()."),
            _ => panic!("expected an external symbol"),
        }
    }

    #[test]
    fn a_local_symbol_resolves_within_its_own_document_only() {
        // `local 1` in two different documents names two unrelated things —
        // resolving one must never leak into the other's definition site.
        let mut idx = ScipIndex::new();
        idx.ingest(&index(&[
            document(
                "a.cc",
                &[
                    occurrence("local 1", 1, &[1, 0]),
                    occurrence("local 1", 0, &[2, 0]),
                ],
            ),
            document(
                "b.cc",
                &[
                    occurrence("local 1", 1, &[5, 0]),
                    occurrence("local 1", 0, &[6, 0]),
                ],
            ),
        ]));
        match idx.lookup("a.cc", 2, 0) {
            Some(ScipAnswer::Definition { doc, line, col }) => {
                assert_eq!(idx.doc_path(doc), "a.cc");
                assert_eq!((line, col), (1, 0));
            }
            _ => panic!("expected a.cc's own local definition"),
        }
        match idx.lookup("b.cc", 6, 0) {
            Some(ScipAnswer::Definition { doc, line, col }) => {
                assert_eq!(idx.doc_path(doc), "b.cc");
                assert_eq!((line, col), (5, 0));
            }
            _ => panic!("expected b.cc's own local definition, not a.cc's"),
        }
    }

    #[test]
    fn no_occurrence_at_a_position_is_none_not_a_guess() {
        let mut idx = ScipIndex::new();
        idx.ingest(&index(&[document("a.cc", &[occurrence("f().", 1, &[1, 0])])]));
        assert!(idx.lookup("a.cc", 99, 99).is_none());
        assert!(idx.lookup("unknown.cc", 1, 0).is_none());
    }

    #[test]
    fn an_empty_index_is_empty_and_answers_nothing() {
        let idx = ScipIndex::new();
        assert!(idx.is_empty());
        assert_eq!(idx.doc_count(), 0);
        assert_eq!(idx.symbol_count(), 0);
        assert!(idx.lookup("a.cc", 0, 0).is_none());
    }

    #[test]
    fn ingest_accumulates_across_calls() {
        // A resolver's `prepare` re-creates the index rather than ingesting
        // twice into the same one, but ingest itself has no reason to refuse
        // a second call — nothing here assumes it is the only one.
        let mut idx = ScipIndex::new();
        idx.ingest(&index(&[document("a.cc", &[occurrence("f().", 1, &[1, 0])])]));
        idx.ingest(&index(&[document("b.cc", &[occurrence("g().", 1, &[2, 0])])]));
        assert_eq!(idx.doc_count(), 2);
        assert!(idx.lookup("a.cc", 1, 0).is_some());
        assert!(idx.lookup("b.cc", 2, 0).is_some());
    }
}
