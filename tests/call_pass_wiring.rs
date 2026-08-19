//! Drift guard for the `calls` axis, the sibling of `reference_pass_wiring.rs`.
//!
//! Writing a call extractor is only half the change — it does nothing until a
//! row in `CALL_PASSES` (src/parser/relations/calls.rs) names it for some
//! (language, node kind). These rows used to be arms of `walk_for_relations`'s
//! match, where the same half-change was possible and just as invisible: the
//! extractor compiles, its unit tests pass, and the index simply lacks the
//! edges. Tree-sitter's disagreement about what a call node is called is what
//! makes this axis prone to it — `call_expression` / `call` /
//! `method_invocation` / `invocation_expression` / three PHP kinds / a Dart
//! `selector` / a Bash `command`.
//!
//! Scanning the source is the point: a hand-kept list of expected names here
//! would be the same forgettable arm one layer up. The row-level invariants that
//! can be checked as DATA (no duplicate slot, no inert row) live next to the
//! table itself, in `calls::table_tests`.
//!
//! Division of labour with the compiler: while every extractor is private to
//! calls.rs, deleting its row makes it dead code and `-D warnings` already fails
//! the build — verified by mutation. This guard covers what that does not: an
//! extractor reachable from somewhere OTHER than the table (called by a sibling
//! extractor, or widened to `pub(super)` for a test) is live to the compiler and
//! still unwired.

use std::fs;
use std::path::{Path, PathBuf};

fn calls_rs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/parser/relations/calls.rs")
}

/// Same parsing rule everywhere in this file: a top-level `fn extract_*` in
/// calls.rs. That file holds call extractors and nothing else, so the name
/// prefix is the whole rule.
fn scan_file_for_call_extractors(path: &Path) -> Vec<String> {
    let src = fs::read_to_string(path).expect("readable source file");
    src.lines()
        .filter_map(|line| {
            // Top-level only: an indented `fn extract_…` is a nested helper, and
            // the table cannot name it anyway.
            let rest = line.strip_prefix("fn extract_")?;
            let name_end = rest.find('(')?;
            Some(format!("extract_{}", &rest[..name_end]))
        })
        .collect()
}

fn table_region(src: &str) -> &str {
    let start = src
        .find("pub(super) const CALL_PASSES:")
        .expect("CALL_PASSES table must exist in src/parser/relations/calls.rs");
    let rest = &src[start..];
    // The table is a `&[...]` literal terminated by the first `];` at column 0.
    let end = rest
        .find("\n];")
        .expect("CALL_PASSES must be a terminated slice literal");
    &rest[..end]
}

#[test]
fn call_passes_wire_every_extractor() {
    let src = fs::read_to_string(calls_rs()).expect("calls.rs readable");
    let table = table_region(&src);

    let defined = scan_file_for_call_extractors(&calls_rs());
    // The floor is the CURRENT count, not a round number below it: a scanner that
    // silently stops matching the declaration style makes this guard vacuous, and
    // a floor with slack absorbs exactly that failure. Adding an extractor means
    // raising this by one, which is the point — it is a second place the addition
    // has to be acknowledged.
    assert!(
        defined.len() >= 12,
        "the scanner found only {} call extractors — it has probably stopped matching \
         the declaration style, which would make this guard vacuous: {defined:?}",
        defined.len()
    );

    let unwired: Vec<&String> = defined
        .iter()
        .filter(|name| !table.contains(name.as_str()))
        .collect();

    assert!(
        unwired.is_empty(),
        "these call extractors exist but no CALL_PASSES row names them, so they emit \
         nothing at index time: {unwired:?}\n\
         Add a row to CALL_PASSES in src/parser/relations/calls.rs (language, node kind, \
         extractor) — writing the extractor is only half the change.",
        unwired = unwired
    );
}

#[test]
fn scanner_sees_a_planted_extractor() {
    // Negative control: the guard above proves nothing about a FUTURE extractor
    // unless the scanner would actually notice one. A planted declaration must be
    // picked up by the same parsing code.
    let dir = tempfile::tempdir().expect("tempdir");
    let planted = dir.path().join("planted.rs");
    fs::write(
        &planted,
        "fn extract_klingon_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {}\n\
         // a nested helper must NOT be picked up:\n    \
         fn extract_indented_helper(x: u8) {}\n",
    )
    .expect("write planted file");

    assert_eq!(
        scan_file_for_call_extractors(&planted),
        vec!["extract_klingon_call".to_string()],
        "the scanner must find a newly declared call extractor, and only top-level ones"
    );
}
