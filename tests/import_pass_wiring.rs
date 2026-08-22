//! Drift guard for the `imports` axis, the sibling of `call_pass_wiring.rs` and
//! `reference_pass_wiring.rs`.
//!
//! Writing an import extractor is only half the change — it does nothing until
//! a row in `IMPORT_PASSES` (src/parser/relations/imports.rs) names it for some
//! (language, node kind). These rows used to be arms of `walk_for_relations`'s
//! match, where the same half-change was possible and just as invisible: the
//! extractor compiles, its unit tests pass, and the index simply lacks the
//! edges. `imports` is the axis most prone to it, because no two grammars agree
//! on the spelling — `import_declaration` alone means one shape in Swift and a
//! different one in Java.
//!
//! Scanning the source is the point: a hand-kept list of expected names here
//! would be the same forgettable arm one layer up.
//!
//! Division of labour with the compiler: every extractor named by the table is
//! private to imports.rs, so deleting its row makes it dead code and
//! `-D warnings` already fails the build. This guard covers what that does not:
//! an extractor reachable from somewhere OTHER than the table (called by a
//! sibling extractor, or widened for a test) stays live to the compiler and
//! still unwired. Two rows delegate to `pub(super)` functions that live in
//! dart.rs and rust.rs; those are covered by the compiler's dead-code check
//! through their thin wrappers here, which the scanner does see.

use std::fs;
use std::path::{Path, PathBuf};

fn imports_rs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/parser/relations/imports.rs")
}

/// A top-level `fn extract_*` TAKING AN `&ImportCtx` — the axis-extractor
/// signature. The sibling guards get away with the bare name prefix because
/// calls.rs holds call extractors and nothing else; imports.rs also holds the
/// specifier/dotted-name sub-walkers that `extract_import_names` recurses
/// through, and those are not table rows and never will be. Keying on the
/// PARAMETER TYPE rather than listing their names keeps the rule true by
/// construction: a hardcoded exclusion list is the shape that silently stops
/// covering things, which is the failure this whole file exists to prevent.
/// Matching `&ImportCtx` rather than a parameter name means renaming `ctx`
/// cannot blind it.
///
/// Scans the SIGNATURE, not the line. An earlier version required both tokens
/// on one source line, which the current names happen to satisfy at 82 columns
/// — but rustfmt wraps past 100, so an extractor with a longer name would have
/// formatted across lines and become invisible to this scan, unwired and
/// unreported. The `>= EXPECTED` floor below cannot catch that: it equals the
/// current count exactly, so an uncounted extractor leaves the total unchanged.
fn scan_file_for_import_extractors(path: &Path) -> Vec<String> {
    // Leading newline so a declaration at byte 0 is found by the same
    // `\nfn extract_` anchor as every other one. imports.rs never starts with
    // an extractor, so this only ever mattered for the negative control — which
    // is exactly what a negative control is for: it caught this scanner's own
    // off-by-one before the scanner could quietly under-report production code.
    let src = format!(
        "\n{}",
        fs::read_to_string(path).expect("readable source file")
    );
    let mut out = Vec::new();
    for (idx, _) in src.match_indices("\nfn extract_") {
        let after = &src[idx + 1..];
        // The signature runs to the opening brace of the body; a wrapped one
        // spans several lines and the type sits on whichever of them rustfmt
        // chose.
        let Some(brace) = after.find('{') else {
            continue;
        };
        let signature = &after[..brace];
        if !signature.contains("&ImportCtx") {
            continue;
        }
        let Some(rest) = signature.strip_prefix("fn extract_") else {
            continue;
        };
        let Some(name_end) = rest.find('(') else {
            continue;
        };
        out.push(format!("extract_{}", &rest[..name_end]));
    }
    out
}

/// Every language named by an IMPORT_PASSES row, derived from the table rather
/// than listed here. A hardcoded list would go stale the moment a row is added
/// — the same shape this file argues against for the extractor scan, and the
/// one that lets a new language reach production with nothing ever having
/// observed it emit an edge.
fn languages_in_table(table: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (idx, _) in table.match_indices("langs: &[") {
        let after = &table[idx + "langs: &[".len()..];
        let Some(end) = after.find(']') else { continue };
        for piece in after[..end].split(',') {
            let name = piece.trim().trim_matches('"');
            if !name.is_empty() && !out.iter().any(|o: &String| o == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn table_region(src: &str) -> &str {
    let start = src
        .find("pub(super) const IMPORT_PASSES:")
        .expect("IMPORT_PASSES table must exist in src/parser/relations/imports.rs");
    let rest = &src[start..];
    let end = rest
        .find("\n];")
        .expect("IMPORT_PASSES must be a terminated slice literal");
    &rest[..end]
}

#[test]
fn import_passes_wire_every_extractor() {
    let src = fs::read_to_string(imports_rs()).expect("imports.rs readable");
    let table = table_region(&src);

    let defined = scan_file_for_import_extractors(&imports_rs());
    // The floor is the CURRENT count of table-named extractors, not a round
    // number below it: a scanner that silently stops matching the declaration
    // style makes this guard vacuous, and slack in the floor absorbs exactly
    // that failure.
    assert!(
        defined.len() >= 12,
        "the scanner found only {} import extractors — it has probably stopped matching \
         the declaration style, which would make this guard vacuous: {defined:?}",
        defined.len()
    );

    let unwired: Vec<&String> = defined
        .iter()
        .filter(|name| !table.contains(name.as_str()))
        .collect();

    assert!(
        unwired.is_empty(),
        "these import extractors exist but no IMPORT_PASSES row names them, so they emit \
         nothing at index time: {unwired:?}\n\
         Add a row to IMPORT_PASSES in src/parser/relations/imports.rs (language, node kind, \
         extractor) — writing the extractor is only half the change."
    );
}

#[test]
fn scanner_sees_a_planted_extractor() {
    // Negative control: the guard above proves nothing about a FUTURE extractor
    // unless the scanner would actually notice one.
    let dir = tempfile::tempdir().expect("tempdir");
    let planted = dir.path().join("planted.rs");
    fs::write(
        &planted,
        "fn extract_klingon_import(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {}\n\
         // a nested helper must NOT be picked up:\n    \
         fn extract_indented_helper(x: u8) {}\n\
         // a WRAPPED signature must be picked up — rustfmt splits past 100 cols,\n\
         // and the scan that missed this shape is the one this control replaces:\n\
         fn extract_wrapped_import(\n    ctx: &ImportCtx,\n    results: &mut Vec<ParsedRelation>,\n) {}\n\
         // a top-level extract_* that is NOT an axis extractor must be ignored:\n\
         fn extract_sub_walker(node: &Node, out: &mut Vec<String>) {}\n",
    )
    .expect("write planted file");

    assert_eq!(
        scan_file_for_import_extractors(&planted),
        vec![
            "extract_klingon_import".to_string(),
            "extract_wrapped_import".to_string(),
        ],
        "the scanner must find newly declared import extractors in BOTH the one-line \
         and the rustfmt-wrapped form, and must ignore an indented helper and a \
         top-level extract_* that takes no &ImportCtx"
    );
}

#[test]
fn every_tabled_language_has_a_parity_row() {
    // The table is the inventory; `import_axis_parity.rs` is the proof of life.
    // A language added to IMPORT_PASSES without a parity row is a handler nobody
    // has ever seen produce an edge — the exact state this axis kept ending up in.
    let table = fs::read_to_string(imports_rs()).expect("imports.rs readable");
    let table = table_region(&table);
    let parity = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/import_axis_parity.rs"),
    )
    .expect("parity table readable");

    let langs = languages_in_table(table);
    assert!(
        langs.len() >= 8,
        "the table-language scan found only {} entries — it has probably stopped \
         matching the row style, which would make this guard vacuous: {langs:?}",
        langs.len()
    );

    let mut missing = Vec::new();
    for lang in &langs {
        if !parity.contains(&format!("lang: \"{lang}\"")) {
            missing.push(lang.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "these languages have an IMPORT_PASSES row but no row in \
         tests/import_axis_parity.rs, so nothing has ever observed them emitting an \
         import edge: {missing:?}"
    );
}
