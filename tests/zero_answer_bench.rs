//! D#229 offline measurement (not part of the default suite).
//!
//! For each name in `CG_D229_NAMES` (one per line), the call sites the
//! zero-answer disclosure counts in the index at `CG_D229_ROOT`, with the
//! flags each counting rule filters on, written as JSON lines to
//! `CG_D229_OUT`. `scripts/zero_answer/README.md` has the recipe that joins
//! them with rust-analyzer's callers.
//!
//!     CG_D229_ROOT=… CG_D229_NAMES=… CG_D229_OUT=… \
//!         cargo test --release --test zero_answer_bench -- --ignored --nocapture

use std::io::Write;
use std::path::PathBuf;

#[test]
#[ignore = "offline measurement: needs CG_D229_ROOT, CG_D229_NAMES and CG_D229_OUT"]
fn zero_answer_call_sites() {
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
    let root = PathBuf::from(env("CG_D229_ROOT"));
    let names = std::fs::read_to_string(env("CG_D229_NAMES")).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(
        &root
            .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
            .join("index.db"),
    )
    .unwrap();
    let mut out = std::fs::File::create(env("CG_D229_OUT")).unwrap();
    for name in names.lines().filter(|l| !l.is_empty()) {
        let start = std::time::Instant::now();
        let b = code_graph_mcp::graph::boundaries::for_empty_result(db.conn(), &root, name, &[])
            .unwrap();
        let ms = start.elapsed().as_millis();
        let (calls, sites) = match b.as_ref().and_then(|b| b.calls.as_ref()) {
            None => (serde_json::Value::Null, vec![]),
            Some(c) => (
                serde_json::json!(c.len()),
                c.iter()
                    .map(|s| {
                        serde_json::json!({
                            "file": s.file_path,
                            "line": s.line,
                            "resolved": s.resolved,
                        })
                    })
                    .collect(),
            ),
        };
        let complete = b.as_ref().map(|b| b.complete());
        writeln!(
            out,
            "{}",
            serde_json::json!({"name": name, "calls": calls, "sites": sites, "complete": complete, "ms": ms})
        )
        .unwrap();
    }
}
