//! Drift guards for the stack budget of the off-main-thread index pipeline.
//!
//! `walk_for_relations` recurses once per AST level up to `MAX_RELATION_DEPTH`,
//! so a sub-kilobyte source file can drive it to the cap. A stack overflow there
//! is an `abort`, not a panic, so it bypasses the serve loop's per-request
//! `catch_unwind` and takes the whole stdio session down — the one failure mode
//! the `panic = "abort"` note in Cargo.toml exists to prevent.
//!
//! The margin is build-profile dependent (unoptimized frames measured ~8x the
//! release ones), which is exactly why it is pinned by a constant and guarded
//! here instead of being left to whatever the release optimizer happens to buy.

use code_graph_mcp::domain::{INDEX_THREAD_STACK_SIZE, MAX_RELATION_DEPTH};
use code_graph_mcp::parser::relations::extract_relations;

/// A JS expression nested `depth` levels deep, each level a call so the walker
/// does real per-frame work rather than skipping through a thin arm.
fn nested_calls(depth: usize) -> String {
    format!("const y = {}1{};", "g(".repeat(depth), ")".repeat(depth))
}

/// The walk must survive its own recursion cap on a thread sized by
/// [`INDEX_THREAD_STACK_SIZE`] — the size `spawn_startup_indexing` uses.
///
/// Non-vacuous by construction: the input nests far past the cap, so the
/// assertion below only holds if the walker actually recursed all the way down
/// to `MAX_RELATION_DEPTH` and stopped there. A walk that bailed early (or an
/// input the grammar flattened) yields a different count and fails.
#[test]
fn relation_walk_survives_depth_cap_on_index_thread() {
    let src = nested_calls(MAX_RELATION_DEPTH * 2);

    let rels = std::thread::Builder::new()
        .stack_size(INDEX_THREAD_STACK_SIZE)
        .spawn(move || extract_relations(&src, "javascript").unwrap())
        .expect("spawn sized index thread")
        .join()
        .expect("walk must not unwind");

    // Each `g(...)` level costs two AST levels (call_expression + arguments),
    // so a cap of N levels admits ~N/2 calls. Tolerance absorbs the couple of
    // wrapper levels (program / statement / declarator) at the top of the tree.
    let expected = MAX_RELATION_DEPTH / 2;
    let calls = rels.iter().filter(|r| r.relation == "calls").count();
    assert!(
        calls.abs_diff(expected) <= 2,
        "expected ~{expected} call relations at the depth cap, got {calls} \
         (total relations {}) — the walk did not reach MAX_RELATION_DEPTH, so \
         this test is no longer measuring the deepest stack",
        rels.len()
    );
}

/// The startup index thread must keep an explicit stack size. `thread::spawn`'s
/// 2 MiB default is under the unoptimized peak of the walk above, and the
/// resulting abort is invisible to every panic-based defense in the server.
///
/// Scans by symbol, not by fixed path, so splitting `mcp/server` into more
/// files cannot silently retire the guard.
#[test]
fn startup_index_thread_declares_its_stack_size() {
    let server_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/server");
    let mut body = None;
    let mut scanned = 0usize;

    let mut stack = vec![server_dir.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src/mcp/server") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            scanned += 1;
            let src = std::fs::read_to_string(&path).expect("read source file");
            if let Some(start) = src.find("fn spawn_startup_indexing") {
                // Body ends at the next method declared at the same indentation.
                // Matching on the visibility spelling would miss `pub(super) fn`
                // and friends and run the region to EOF, which then trips the
                // no-`thread::spawn` assertion on an unrelated method — a false
                // red rather than a false green, but still noise.
                let rest = &src[start..];
                let end = rest[1..]
                    .match_indices("\n    ")
                    .map(|(i, _)| i + 1)
                    .find(|&i| {
                        let line = rest[i..].lines().nth(1).unwrap_or("").trim_start();
                        line.starts_with("fn ")
                            || (line.starts_with("pub") && line.contains(" fn "))
                    })
                    .unwrap_or(rest.len());
                body = Some(rest[..end].to_string());
            }
        }
    }

    assert!(scanned > 0, "guard scanned no files — src/mcp/server moved");
    let body = body.expect(
        "fn spawn_startup_indexing not found under src/mcp/server — \
         the guard must be repointed at wherever the index thread is now spawned",
    );
    // Match the CALL, not the constant's name: the spawn site is introduced by a
    // comment that names the constant, so asserting on the bare identifier passed
    // with `.stack_size(...)` deleted — the guard was vacuous until a mutation
    // test caught it.
    assert!(
        body.contains(".stack_size(crate::domain::INDEX_THREAD_STACK_SIZE)"),
        "spawn_startup_indexing must pass domain::INDEX_THREAD_STACK_SIZE to \
         Builder::stack_size (naming the constant in a comment is not sizing the thread)"
    );
    assert!(
        !body.contains("std::thread::spawn("),
        "spawn_startup_indexing must not use std::thread::spawn (2 MiB default \
         stack is below the unoptimized peak of the relation walk)"
    );
}
