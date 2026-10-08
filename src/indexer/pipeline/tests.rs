use super::index_files::{module_stems_of, sentinel_name_matches_stem};
use super::python_modules::build_python_module_map;
use super::*;
use crate::domain::REL_CALLS;
use crate::storage::queries::{
    get_all_file_hashes, get_edges_from, get_import_tree, get_nodes_by_file_path, get_nodes_by_name,
};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_full_index_pipeline() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    fs::write(
        project_dir.path().join("src/auth.ts"),
        r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();

    assert!(result.files_indexed > 0);
    assert!(result.nodes_created > 0);
    assert!(result.edges_created > 0);

    // Verify nodes are in DB
    let nodes = get_nodes_by_name(db.conn(), "handleLogin").unwrap();
    assert_eq!(nodes.len(), 1);

    // Verify edges: handleLogin → calls → validateToken
    let edges = get_edges_from(db.conn(), nodes[0].id).unwrap();
    assert!(
        edges.iter().any(|e| e.relation == REL_CALLS),
        "should have call edges"
    );

    // Verify context string was built
    assert!(
        nodes[0].context_string.is_some(),
        "context string should be set after Phase 3"
    );
}

#[test]
fn test_progress_reports_files_then_finalizing_heartbeats() {
    // Statusline liveness contract: batch progress arrives as `Files` events with
    // a moving done-count, and the post-batch full-graph phases emit `Finalizing`
    // heartbeats. Regression guard for the frozen "indexing N/M (100%)"
    // statusline — before IndexPhase existed the whole tail was silent, so a
    // stale-mtime gate couldn't tell "long tail phase" from "indexer died".
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    fs::write(
        project_dir.path().join("src/a.ts"),
        "function alpha(): number { return 1; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/b.ts"),
        "function beta(): number { return alpha(); }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let events = std::cell::RefCell::new(Vec::new());
    let cb = |phase: IndexPhase, done: usize, total: usize| {
        events.borrow_mut().push((phase, done, total));
    };
    let result = run_full_index(&db, project_dir.path(), None, Some(&cb)).unwrap();
    let events = events.into_inner();

    let files_done_max = events
        .iter()
        .filter(|(p, _, _)| *p == IndexPhase::Files)
        .map(|(_, d, _)| *d)
        .max()
        .expect("at least one Files event");
    assert_eq!(
        files_done_max, result.files_indexed,
        "Files events should reach the final indexed count"
    );

    assert!(
        events
            .iter()
            .filter(|(p, _, _)| *p == IndexPhase::Finalizing)
            .count()
            >= 2,
        "tail phases must emit Finalizing heartbeats, got {:?}",
        events
    );
    assert_eq!(
        events.last().unwrap().0,
        IndexPhase::Finalizing,
        "the last event must come from the tail phases, got {:?}",
        events
    );
}

#[test]
fn test_remove_indexing_status_older_than() {
    let project_dir = TempDir::new().unwrap();
    let cg = project_dir.path().join(crate::domain::CODE_GRAPH_DIR);
    fs::create_dir_all(&cg).unwrap();
    let status = cg.join(INDEXING_STATUS_FILE);
    fs::write(&status, r#"{"s":"indexing","d":5,"t":10}"#).unwrap();

    // Fresh file + generous max_age → kept (a live indexer's file must survive).
    remove_indexing_status_older_than(project_dir.path(), std::time::Duration::from_secs(3600));
    assert!(status.exists(), "fresh progress file must not be removed");

    // Zero max_age treats any mtime as stale → removed (the killed-server orphan).
    remove_indexing_status_older_than(project_dir.path(), std::time::Duration::ZERO);
    assert!(!status.exists(), "stale progress file must be removed");

    // Absent file → no-op, no panic.
    remove_indexing_status_older_than(project_dir.path(), std::time::Duration::ZERO);
}

#[test]
fn test_full_index_atomic_inside_outer_transaction() {
    // L6: MCP rebuild_index wraps DELETE FROM files + run_full_index in ONE outer
    // transaction so external readers never see the empty mid-rebuild window and a
    // failed rebuild rolls back to the old index. That requires run_full_index's
    // phase transactions to be nestable SAVEPOINTs, not `unchecked_transaction`
    // (which issues BEGIN and errors "cannot start a transaction within a
    // transaction" inside an open transaction). This test runs the exact rebuild
    // shape; before the savepoint conversion it fails on the first nested BEGIN.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    fs::write(
        project_dir.path().join("src/a.ts"),
        r#"
function alpha(): number { return beta(); }
function beta(): number { return 1; }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    // Seed an "old" index so the DELETE below actually clears prior state.
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(!get_nodes_by_name(db.conn(), "alpha").unwrap().is_empty());

    // Rewrite the source so the rebuild produces different symbols.
    fs::write(
        project_dir.path().join("src/a.ts"),
        r#"
function gamma(): number { return delta(); }
function delta(): number { return 2; }
"#,
    )
    .unwrap();

    // Exactly what tool_rebuild_index does: one outer transaction around
    // DELETE FROM files + run_full_index (its phase savepoints nest inside it).
    let result = {
        let tx = db.conn().unchecked_transaction().unwrap();
        tx.execute("DELETE FROM files", []).unwrap();
        let r = run_full_index(&db, project_dir.path(), None, None).unwrap();
        tx.commit().unwrap();
        r
    };
    assert!(result.nodes_created > 0);

    // New symbols present, old ones gone — the rebuild committed atomically.
    assert!(
        !get_nodes_by_name(db.conn(), "gamma").unwrap().is_empty(),
        "rebuilt node present"
    );
    assert!(
        get_nodes_by_name(db.conn(), "alpha").unwrap().is_empty(),
        "old node cleared"
    );
}

#[test]
fn test_duplicate_inline_route_handlers_resolve_per_occurrence() {
    // Two inline handlers for the SAME method+path in one file (valid:
    // conditional / overloaded registration). Before the per-occurrence line
    // suffix in route_handler_name both materialized under one synthetic name
    // "GET /dup", so name-based edge resolution cross-linked their calls
    // (handler-1's logA AND logB attributed to both) and fanned routes_to into a
    // cartesian product (src{N}×tgt{N}). Each handler must resolve 1:1.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    fs::write(
        project_dir.path().join("routes.js"),
        r#"
const express = require('express');
const app = express();
function logA() { console.log('a'); }
function logB() { console.log('b'); }
app.get('/dup', (req, res) => { logA(); res.send('1'); });
app.get('/dup', (req, res) => { logB(); res.send('2'); });
app.get('/unique', (req, res) => { logA(); });
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let conn = db.conn();

    // Two distinct handler nodes for the same /dup route (per-occurrence identity).
    let dup_nodes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name LIKE 'GET /dup%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        dup_nodes, 2,
        "each /dup registration is its own handler node"
    );

    // routes_to: exactly one self-edge per registration (3), NOT a cartesian fan-out.
    let routes_to: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE relation = 'routes_to'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        routes_to, 3,
        "one routes_to per registration; no same-name cartesian fan-out"
    );

    // calls must not cross-link: exactly one /dup handler calls logA (the first),
    // exactly one calls logB (the second) — 1 each, not 2 each.
    let dup_to = |callee: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM edges e \
             JOIN nodes s ON s.id = e.source_id JOIN nodes t ON t.id = e.target_id \
             WHERE e.relation = 'calls' AND s.name LIKE 'GET /dup%' AND t.name = ?1",
            [callee],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        dup_to("logA"),
        1,
        "only the first /dup handler calls logA (no cross-link)"
    );
    assert_eq!(
        dup_to("logB"),
        1,
        "only the second /dup handler calls logB (no cross-link)"
    );
}

#[test]
fn test_cross_language_bare_name_call_resolution() {
    // Regression: Rust method call `hasher.update(...)` was resolving to
    // JS `function update()` via global bare-name lookup, producing phantom
    // Rust → JS call edges in mixed projects. Fix: same-file > same-language
    // tiers; drop call edges with no same-language candidate.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    fs::create_dir_all(project_dir.path().join("scripts")).unwrap();

    fs::write(
        project_dir.path().join("src/hasher.rs"),
        r#"
pub fn caller_rs() {
    let mut h = Hasher::new();
    h.update(&[1, 2, 3]);
    h.finalize();
}
"#,
    )
    .unwrap();

    fs::write(
        project_dir.path().join("scripts/helper.js"),
        r#"
function update() { return 1; }
function caller_js() { update(); }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let rust_caller =
        crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "caller_rs").unwrap();
    let rust_caller = rust_caller
        .iter()
        .find(|n| n.file_path == "src/hasher.rs")
        .expect("Rust caller_rs should be indexed");
    let edges = get_edges_from(db.conn(), rust_caller.node.id).unwrap();
    for e in &edges {
        if e.relation != REL_CALLS {
            continue;
        }
        let tgt_path: Option<String> = db
            .conn()
            .query_row(
                "SELECT f.path FROM nodes n JOIN files f ON n.file_id = f.id WHERE n.id = ?1",
                [e.target_id],
                |row| row.get(0),
            )
            .ok();
        assert!(
            !tgt_path.as_deref().unwrap_or("").ends_with(".js"),
            "Rust caller must not resolve calls into JS; got edge → {:?}",
            tgt_path,
        );
    }

    let js_caller =
        crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "caller_js").unwrap();
    let js_caller = js_caller
        .iter()
        .find(|n| n.file_path == "scripts/helper.js")
        .expect("JS caller_js should be indexed");
    let js_edges = get_edges_from(db.conn(), js_caller.node.id).unwrap();
    let js_call_targets: Vec<i64> = js_edges
        .iter()
        .filter(|e| e.relation == REL_CALLS)
        .map(|e| e.target_id)
        .collect();
    assert!(
        !js_call_targets.is_empty(),
        "JS caller_js → update edge within same file should still resolve"
    );
}

#[test]
fn test_intra_class_method_call_edges_resolve() {
    // Regression: class-based languages qualify a method's enclosing scope as
    // `Class.method`, but the node's bare `name` is just `method`. Phase-2 source
    // resolution matched only bare node_names, so EVERY intra-class method →
    // sibling-method call edge was silently dropped (TS/JS/Python/Java/Ruby).
    // Rust/Go were unaffected (bare scope), which masked the bug. Fix: also match
    // the relation's qualified source_name against each node's qualified_name.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();

    fs::write(
        project_dir.path().join("src/svc.py"),
        r#"
class UserSvc:
    def get_user(self, uid):
        return self._fetch(uid)
    def _fetch(self, uid):
        return uid
"#,
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/Svc.java"),
        r#"
class Svc {
    void run() { helper(); }
    void helper() {}
}
"#,
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/svc.ts"),
        r#"
class TsSvc {
    outer(): void { this.inner(); }
    inner(): void {}
}
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Each outer method must have a calls edge to its sibling.
    for (caller, callee) in [
        ("get_user", "_fetch"),
        ("run", "helper"),
        ("outer", "inner"),
    ] {
        let nodes = get_nodes_by_name(db.conn(), caller).unwrap();
        let node = nodes
            .first()
            .unwrap_or_else(|| panic!("{caller} should be indexed"));
        let edges = get_edges_from(db.conn(), node.id).unwrap();
        let has_call = edges.iter().any(|e| {
            if e.relation != REL_CALLS {
                return false;
            }
            let tgt: Option<String> = db
                .conn()
                .query_row("SELECT name FROM nodes WHERE id = ?1", [e.target_id], |r| {
                    r.get(0)
                })
                .ok();
            tgt.as_deref() == Some(callee)
        });
        assert!(has_call, "{caller} → {callee} method-call edge was dropped");
    }
}

#[test]
fn test_js_require_creates_external_import_edges() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::write(
        project_dir.path().join("app.js"),
        r#"
const fs = require('fs');
const path = require('path');
const lifecycle = require('./lifecycle');

function main() { fs.readFileSync('x'); }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let imports: Vec<String> = db
        .conn()
        .prepare(
            "SELECT DISTINCT n2.name FROM edges e
         JOIN nodes n ON n.id = e.source_id
         JOIN files f ON f.id = n.file_id
         JOIN nodes n2 ON n2.id = e.target_id
         WHERE f.path = 'app.js' AND e.relation = 'imports'",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    assert!(
        imports.contains(&"fs".to_string()),
        "imports: {:?}",
        imports
    );
    assert!(
        imports.contains(&"path".to_string()),
        "imports: {:?}",
        imports
    );
    assert!(
        imports.contains(&"lifecycle".to_string()),
        "imports: {:?}",
        imports
    );
}

#[test]
fn test_js_same_name_cross_file_prefers_closest_path() {
    // Regression: when JS defines the same helper name in multiple files
    // (e.g., `readJson` in both `claude-plugin/scripts/lifecycle.js` and
    // `scripts/install-e2e.test.js`), a caller in `claude-plugin/scripts/*`
    // used to fan out an edge to every same-language match, producing
    // false-positive callers across unrelated modules. The resolver must
    // pick the candidate with the longest common path prefix to the
    // caller file (and prefer non-test files) rather than all.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("pkg/scripts")).unwrap();
    fs::create_dir_all(project_dir.path().join("tests")).unwrap();

    fs::write(
        project_dir.path().join("pkg/scripts/lifecycle.js"),
        r#"
function readJson(p) { return 1; }
module.exports = { readJson };
"#,
    )
    .unwrap();

    fs::write(
        project_dir.path().join("pkg/scripts/session-init.js"),
        r#"
function syncLifecycleConfig() { readJson('x'); }
"#,
    )
    .unwrap();

    fs::write(
        project_dir.path().join("tests/helpers.test.js"),
        r#"
function readJson(p) { return 2; }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Find the caller node
    let caller =
        crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "syncLifecycleConfig")
            .unwrap();
    let caller = caller
        .iter()
        .find(|n| n.file_path == "pkg/scripts/session-init.js")
        .expect("syncLifecycleConfig should be indexed");

    let edges = get_edges_from(db.conn(), caller.node.id).unwrap();
    let call_edges: Vec<i64> = edges
        .iter()
        .filter(|e| e.relation == REL_CALLS)
        .map(|e| e.target_id)
        .collect();

    // Resolve target paths
    let target_paths: Vec<String> =
        call_edges
            .iter()
            .filter_map(|tid| {
                db.conn().query_row(
            "SELECT f.path FROM nodes n JOIN files f ON n.file_id = f.id WHERE n.id = ?1",
            [*tid], |row| row.get(0)
        ).ok()
            })
            .collect();

    // Must pick exactly the same-dir candidate, not fan out to the test file.
    assert!(
        target_paths.iter().any(|p| p == "pkg/scripts/lifecycle.js"),
        "should resolve to same-dir readJson; got {:?}",
        target_paths
    );
    assert!(
        !target_paths.iter().any(|p| p == "tests/helpers.test.js"),
        "should NOT fan out to unrelated test-file readJson; got {:?}",
        target_paths
    );
}

#[test]
fn test_prune_keeps_edge_when_caller_content_truncated() {
    // L12: the import-contradiction prune reads sn.code_content to check for a
    // qualified `.name(` call (the keep-guard). truncate_code_content caps content
    // at 4096 and appends a "..." sentinel, so a qualified call beyond the cap is
    // sliced off → instr=0 false negative → the real edge is false-pruned. The fix
    // skips pruning when the caller's content is truncated (ends in the sentinel).
    use crate::domain::{REL_CALLS, REL_IMPORTS};
    use crate::storage::queries::{insert_edge, insert_node, upsert_file, FileRecord, NodeRecord};
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let conn = db.conn();

    let mk_file = |path: &str| {
        upsert_file(
            conn,
            &FileRecord {
                path: path.into(),
                blake3_hash: path.into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap()
    };
    let mk_fn = |file_id: i64, name: &str, code: &str| {
        insert_node(
            conn,
            &NodeRecord {
                file_id,
                node_type: "function".into(),
                name: name.into(),
                qualified_name: None,
                start_line: 1,
                end_line: 2,
                code_content: code.into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap()
    };

    // Two same-name targets in different files (the ambiguity the prune arbitrates).
    let f_a = mk_file("a.py");
    let f_b = mk_file("b.py");
    let save_a = mk_fn(f_a, "save", "def save(r):\n    return True\n");
    let save_b = mk_fn(f_b, "save", "def save(i):\n    return True\n");

    // Caller whose code_content is TRUNCATED (ends in the "..." sentinel) and does
    // NOT literally contain ".save(" — the qualified call was sliced off by the cap.
    let f_caller = mk_file("caller.py");
    let run_trunc = mk_fn(
        f_caller,
        "run",
        "def run():\n    a_very_long_body_that_was_cut_off...",
    );
    // Caller file imports `save` bound to save_b (a DIFFERENT node than the call target).
    insert_edge(conn, run_trunc, save_b, REL_IMPORTS, None).unwrap();
    // The (import-contradicted) call edge run -> save_a we must NOT false-prune.
    insert_edge(conn, run_trunc, save_a, REL_CALLS, None).unwrap();

    // Control caller: identical contradiction but NON-truncated content → must still prune.
    let f_caller2 = mk_file("caller2.py");
    let run_ok = mk_fn(f_caller2, "run2", "def run2():\n    return helper()\n");
    insert_edge(conn, run_ok, save_b, REL_IMPORTS, None).unwrap();
    insert_edge(conn, run_ok, save_a, REL_CALLS, None).unwrap();

    let removed = super::resolve::prune_import_contradicted_call_edges(
        &db,
        &super::resolve::PostPassScope::Global,
    )
    .unwrap();

    let edge_exists = |src: i64, tgt: i64| -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE source_id=?1 AND target_id=?2 AND relation=?3",
            rusqlite::params![src, tgt, REL_CALLS],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    };
    assert!(
        edge_exists(run_trunc, save_a),
        "truncated caller: the call edge must be KEPT (instr can't see beyond the 4096 cap)"
    );
    assert!(
        !edge_exists(run_ok, save_a),
        "non-truncated caller: the genuinely import-contradicted edge must still be pruned"
    );
    assert_eq!(
        removed, 1,
        "exactly the non-truncated control edge is pruned"
    );
}

#[test]
fn test_import_binding_resolves_call_over_path_proximity() {
    // Import-aware call resolution: when a bare call's name matches same-name
    // defs in multiple files, an explicit `from X import name` in the caller's
    // file must bind the call to the IMPORTED definition — even when a
    // different same-name def is closer by path. Without import-awareness,
    // refine_ambiguous_targets picks the path-closest (wrong) target, which
    // prune_import_contradicted_call_edges then deletes, leaving the call with
    // NO edge at all (correct import-bound edge never positively created).
    // Python is used here because its import edges already resolve
    // module-path-aware (resolve_python_module_targets), isolating the
    // call-resolution gap; JS module-specifier resolution is a separate cycle.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("app/core")).unwrap();
    fs::create_dir_all(project_dir.path().join("app/util")).unwrap();

    // Path-closest same-name def — the WRONG target for the call below.
    fs::write(
        project_dir.path().join("app/core/helper.py"),
        r#"
def process():
    return 1
"#,
    )
    .unwrap();

    // Imported same-name def — the RIGHT target, farther by path prefix.
    fs::write(
        project_dir.path().join("app/util/helper.py"),
        r#"
def process():
    return 2
"#,
    )
    .unwrap();

    // Caller sits next to app/core/helper.py but explicitly imports the util one.
    fs::write(
        project_dir.path().join("app/core/caller.py"),
        r#"
from app.util.helper import process

def run():
    return process()
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let caller = crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "run").unwrap();
    let caller = caller
        .iter()
        .find(|n| n.file_path == "app/core/caller.py")
        .expect("run should be indexed");

    let edges = get_edges_from(db.conn(), caller.node.id).unwrap();
    let call_target_paths: Vec<String> =
        edges
            .iter()
            .filter(|e| e.relation == REL_CALLS)
            .filter_map(|e| {
                db.conn().query_row(
            "SELECT f.path FROM nodes n JOIN files f ON n.file_id = f.id WHERE n.id = ?1",
            [e.target_id], |row| row.get(0),
        ).ok()
            })
            .collect();

    assert!(
        call_target_paths.iter().any(|p| p == "app/util/helper.py"),
        "run() must resolve to the IMPORTED process (app/util/helper.py); got {:?}",
        call_target_paths
    );
    assert!(
        !call_target_paths.iter().any(|p| p == "app/core/helper.py"),
        "run() must NOT resolve to the path-closest non-imported process (app/core/helper.py); got {:?}",
        call_target_paths
    );
}

#[test]
fn test_js_named_import_resolves_via_module_specifier() {
    // Cycle 2: JS/TS import edges must resolve via the module specifier
    // (`from '../util/helper'`), not by path-proximity name matching. Two files
    // define `process`; the caller imports the farther one explicitly. The
    // import edge must bind to the specifier-resolved file, not the path-closest
    // same-name node (which is what refine_ambiguous_targets picks today).
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src/core")).unwrap();
    fs::create_dir_all(project_dir.path().join("src/util")).unwrap();

    fs::write(
        project_dir.path().join("src/core/helper.ts"),
        "export function process() { return 1; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/util/helper.ts"),
        "export function process() { return 2; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/core/caller.ts"),
        r#"
import { process } from '../util/helper';

export function run() { return process(); }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let import_target_paths: Vec<String> = db
        .conn()
        .prepare(
            "SELECT tf.path FROM edges e
         JOIN nodes sn ON sn.id = e.source_id
         JOIN files sf ON sf.id = sn.file_id
         JOIN nodes tn ON tn.id = e.target_id
         JOIN files tf ON tf.id = tn.file_id
         WHERE e.relation = 'imports' AND sf.path = 'src/core/caller.ts'
           AND tn.name = 'process'",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();

    assert!(
        import_target_paths
            .iter()
            .any(|p| p == "src/util/helper.ts"),
        "import must resolve via specifier to src/util/helper.ts; got {:?}",
        import_target_paths
    );
    assert!(
        !import_target_paths
            .iter()
            .any(|p| p == "src/core/helper.ts"),
        "import must NOT bind to the path-closest src/core/helper.ts; got {:?}",
        import_target_paths
    );
}

#[test]
fn test_exported_const_value_forms_import_edge() {
    // INDEX_VERSION 39: a top-level `export const X = <value>` is extracted as a
    // `constant` node, so `import { X } from './config'` resolves to it and forms a
    // REL_IMPORTS edge. Previously the const was not a symbol, so the import bound to
    // the `<external>` sentinel and the cross-module dependency was invisible to
    // tour/affected/impact/project_map (feedback_const_export_no_import_edge).
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();

    fs::write(
        project_dir.path().join("src/config.ts"),
        r#"
export const API_URL = "https://example.com";
export const DEFAULT_CONFIG = { timeout: 5000, retries: 3 };

const NOT_EXPORTED = 42;
"#,
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/api.ts"),
        r#"
import { API_URL, DEFAULT_CONFIG } from './config';

export function fetchData() { return API_URL + DEFAULT_CONFIG.timeout; }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // The exported value const is a real `constant` node; the non-exported one is not.
    let const_types: Vec<String> = db
        .conn()
        .prepare(
            "SELECT n.type FROM nodes n JOIN files f ON f.id = n.file_id
         WHERE n.name = 'API_URL' AND f.path = 'src/config.ts'",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        const_types,
        vec!["constant".to_string()],
        "export const value must be extracted as exactly one `constant` node"
    );

    let not_exported: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'NOT_EXPORTED'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        not_exported, 0,
        "a non-exported top-level const must not be extracted"
    );

    // `import { API_URL }` resolves to the const node in its defining file, not <external>.
    let resolved_targets: Vec<String> = db
        .conn()
        .prepare(
            "SELECT tf.path FROM edges e
         JOIN nodes tn ON tn.id = e.target_id
         JOIN files tf ON tf.id = tn.file_id
         WHERE e.relation = 'imports' AND tn.name = 'API_URL'",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        resolved_targets.iter().any(|p| p == "src/config.ts"),
        "import {{ API_URL }} must resolve to the const in src/config.ts; got {:?}",
        resolved_targets
    );
    assert!(
        !resolved_targets.iter().any(|p| p.contains("external")),
        "import must NOT bind to the <external> sentinel; got {:?}",
        resolved_targets
    );
}

#[test]
fn test_js_import_binds_call_over_path_proximity() {
    // Cycle 2 end-to-end (TS analog of test_import_binding_resolves_call_over_path_proximity):
    // once JS imports resolve via specifier, the Cycle-1 bind repoints the bare
    // call to the imported target instead of the path-closest same-name def.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src/core")).unwrap();
    fs::create_dir_all(project_dir.path().join("src/util")).unwrap();

    fs::write(
        project_dir.path().join("src/core/helper.ts"),
        "export function process() { return 1; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/util/helper.ts"),
        "export function process() { return 2; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/core/caller.ts"),
        r#"
import { process } from '../util/helper';

export function run() { return process(); }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let caller = crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "run").unwrap();
    let caller = caller
        .iter()
        .find(|n| n.file_path == "src/core/caller.ts")
        .expect("run should be indexed");

    let edges = get_edges_from(db.conn(), caller.node.id).unwrap();
    let call_target_paths: Vec<String> =
        edges
            .iter()
            .filter(|e| e.relation == REL_CALLS)
            .filter_map(|e| {
                db.conn().query_row(
            "SELECT f.path FROM nodes n JOIN files f ON n.file_id = f.id WHERE n.id = ?1",
            [e.target_id], |row| row.get(0),
        ).ok()
            })
            .collect();

    assert!(
        call_target_paths.iter().any(|p| p == "src/util/helper.ts"),
        "run() must resolve to the IMPORTED process (src/util/helper.ts); got {:?}",
        call_target_paths
    );
    assert!(
        !call_target_paths.iter().any(|p| p == "src/core/helper.ts"),
        "run() must NOT resolve to the path-closest non-imported process (src/core/helper.ts); got {:?}",
        call_target_paths
    );
}

#[test]
fn test_commonjs_destructured_require_binds_call() {
    // Cycle 3: `const { process } = require('../util/helper')` must resolve the
    // bare call process() to the required file's export, not the path-closest
    // same-name def. CommonJS analog of the ES-import case (this project's own
    // plugin JS uses require). Extraction emits a per-name import stamped with
    // the specifier; Cycle 2 resolution + Cycle 1 bind do the rest.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src/core")).unwrap();
    fs::create_dir_all(project_dir.path().join("src/util")).unwrap();

    fs::write(
        project_dir.path().join("src/core/helper.js"),
        "function process() { return 1; }\nmodule.exports = { process };\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/util/helper.js"),
        "function process() { return 2; }\nmodule.exports = { process };\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/core/caller.js"),
        r#"
const { process } = require('../util/helper');

function run() { return process(); }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let caller = crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "run").unwrap();
    let caller = caller
        .iter()
        .find(|n| n.file_path == "src/core/caller.js")
        .expect("run should be indexed");

    let edges = get_edges_from(db.conn(), caller.node.id).unwrap();
    let call_target_paths: Vec<String> =
        edges
            .iter()
            .filter(|e| e.relation == REL_CALLS)
            .filter_map(|e| {
                db.conn().query_row(
            "SELECT f.path FROM nodes n JOIN files f ON n.file_id = f.id WHERE n.id = ?1",
            [e.target_id], |row| row.get(0),
        ).ok()
            })
            .collect();

    assert!(
        call_target_paths.iter().any(|p| p == "src/util/helper.js"),
        "run() must resolve to the required process (src/util/helper.js); got {:?}",
        call_target_paths
    );
    assert!(
        !call_target_paths.iter().any(|p| p == "src/core/helper.js"),
        "run() must NOT resolve to the path-closest non-required process (src/core/helper.js); got {:?}",
        call_target_paths
    );
}

#[test]
fn test_commonjs_namespace_require_binds_member_call() {
    // Cycle 4: `const helper = require('../util/helper'); helper.process()` must
    // resolve the member call to the required module's export, not the
    // path-closest same-name def. JS discards the receiver today (extract_callee
    // returns Bare for non-Rust), so `helper.process()` resolves "process" by
    // proximity. Capturing the receiver + tracking the require-namespace binding
    // lets it bind to the required file.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src/core")).unwrap();
    fs::create_dir_all(project_dir.path().join("src/util")).unwrap();

    fs::write(
        project_dir.path().join("src/core/helper.js"),
        "function process() { return 1; }\nmodule.exports = { process };\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/util/helper.js"),
        "function process() { return 2; }\nmodule.exports = { process };\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/core/caller.js"),
        r#"
const helper = require('../util/helper');

function run() { return helper.process(); }
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let caller = crate::storage::queries::get_nodes_with_files_by_name(db.conn(), "run").unwrap();
    let caller = caller
        .iter()
        .find(|n| n.file_path == "src/core/caller.js")
        .expect("run should be indexed");

    let edges = get_edges_from(db.conn(), caller.node.id).unwrap();
    let call_target_paths: Vec<String> =
        edges
            .iter()
            .filter(|e| e.relation == REL_CALLS)
            .filter_map(|e| {
                db.conn().query_row(
            "SELECT f.path FROM nodes n JOIN files f ON n.file_id = f.id WHERE n.id = ?1",
            [e.target_id], |row| row.get(0),
        ).ok()
            })
            .collect();

    assert!(
        call_target_paths.iter().any(|p| p == "src/util/helper.js"),
        "helper.process() must resolve to the required module (src/util/helper.js); got {:?}",
        call_target_paths
    );
    assert!(
        !call_target_paths.iter().any(|p| p == "src/core/helper.js"),
        "helper.process() must NOT resolve to the path-closest non-required process (src/core/helper.js); got {:?}",
        call_target_paths
    );
}

#[test]
fn test_js_module_level_test_callback_calls_resolve() {
    // Regression: helpers defined in a JS test file that are called only
    // from inside `test(() => {...})` / `describe(() => {...})` callbacks
    // used to be reported as orphan by dead-code, because the anonymous
    // arrow callback body attributed its calls to `<anonymous>`, a name
    // that resolves to no node. Module-level call_expressions inside JS
    // test files must attribute to `<module>` so a same-file edge lands.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    fs::write(
        project_dir.path().join("helpers.test.js"),
        r#"
function mkHome() { return '/tmp/x'; }
function writeJson(p, v) { }

test('uses helpers', () => {
    const h = mkHome();
    writeJson(h, { a: 1 });
});
"#,
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Both helper names must have at least one incoming call edge.
    for helper in ["mkHome", "writeJson"] {
        let cnt: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM edges e
             JOIN nodes tn ON tn.id = e.target_id
             JOIN files tf ON tf.id = tn.file_id
             WHERE tn.name = ?1 AND tf.path = 'helpers.test.js' AND e.relation = 'calls'",
                [helper],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            cnt >= 1,
            "{} should have at least one incoming call edge from the test callback, got {}",
            helper,
            cnt
        );
    }
}

#[test]
fn test_incremental_index() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Initial index
    fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Modify file
    fs::write(project_dir.path().join("a.ts"), "function bar() {}").unwrap();

    // Incremental index
    let result = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(result.files_indexed, 1);

    let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
    assert_eq!(foo.len(), 0);
    let bar = get_nodes_by_name(db.conn(), "bar").unwrap();
    assert_eq!(bar.len(), 1);
}

/// A file whose HASH read fails must stay EXISTING on the non-cached path.
///
/// The end-to-end mirror of `test_scan_directory_cached_stat_failure_is_not_deletion`
/// (merkle.rs), for the entry point that had no guard: `hash_files_parallel`
/// warns and drops a file it cannot read, so the path fell out of
/// `current_hashes` and `compute_diff` called a live file DELETED — Phase 0 then
/// cascaded its nodes and its callers' edges away. Mode 0o000 on the file
/// reproduces it exactly: the walk lists it, `metadata()` succeeds (the parent
/// is still searchable), `File::open` fails with EACCES (audit 2026-09-05
/// CORE-01).
#[test]
#[cfg(unix)]
fn test_incremental_hash_failure_is_not_deletion() {
    use std::os::unix::fs::PermissionsExt;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    let a = project_dir.path().join("a.ts");
    fs::write(&a, "function alpha() {}").unwrap();
    fs::write(
        project_dir.path().join("b.ts"),
        "function beta() { alpha(); }",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(get_nodes_by_name(db.conn(), "alpha").unwrap().len(), 1);

    fs::set_permissions(&a, fs::Permissions::from_mode(0o000)).unwrap();
    let read_denied = fs::File::open(&a).is_err();
    let stat_ok = fs::metadata(&a).is_ok();
    let indexed = run_incremental_index(&db, project_dir.path(), None, None);
    // Restore before unwrapping so a failure can't leave an unreadable file.
    fs::set_permissions(&a, fs::Permissions::from_mode(0o644)).unwrap();
    indexed.unwrap();

    if !read_denied || !stat_ok {
        // root (or an FS that ignores the mode) — the precondition this test
        // needs does not hold here, and asserting anyway would pass for the
        // wrong reason.
        eprintln!("skipped: mode 0o000 did not deny the read (running as root?)");
        return;
    }
    assert_eq!(
        get_nodes_by_name(db.conn(), "alpha").unwrap().len(),
        1,
        "a file that only failed to hash must not be treated as deleted — \
         its nodes and its callers' edges go with it"
    );
}

#[test]
fn test_incremental_propagates_dirty_context() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Initial: B (in b.ts) calls A (in a.ts)
    fs::write(project_dir.path().join("a.ts"), "function alpha() {}").unwrap();
    fs::write(
        project_dir.path().join("b.ts"),
        "function beta() { alpha(); }",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let beta_nodes = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert_eq!(beta_nodes.len(), 1);
    let beta_ctx_before = beta_nodes[0].context_string.clone().unwrap_or_default();

    // Change A: rename function (alpha -> alphaRenamed)
    fs::write(
        project_dir.path().join("a.ts"),
        "function alphaRenamed() {}",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // beta's context_string should be updated (calls list changed because
    // the old alpha node is gone and edge was cascade-deleted)
    let beta_nodes_after = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert_eq!(beta_nodes_after.len(), 1);
    let beta_ctx_after = beta_nodes_after[0]
        .context_string
        .clone()
        .unwrap_or_default();
    assert_ne!(beta_ctx_before, beta_ctx_after);
}

// Regression (#3): when an incremental index runs with model=None (the watcher /
// drift path, which avoids holding the model lock across I/O), a cross-file dirty
// node's context_string is regenerated — so its existing vector is now STALE and
// must be invalidated (dropped) so the background embedder re-selects it. Before
// the fix the stale vector survived until a full rebuild.
#[test]
fn test_edge_flip_invalidates_caller_vector() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    // open_with_vec → vec_enabled()=true so the model=None invalidation branch runs.
    let db = Database::open_with_vec(&db_dir.path().join("index.db")).unwrap();
    assert!(db.vec_enabled(), "test requires vec tables");

    // beta (b.ts) calls alpha (a.ts)
    fs::write(project_dir.path().join("a.ts"), "function alpha() {}").unwrap();
    fs::write(
        project_dir.path().join("b.ts"),
        "function beta() { alpha(); }",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let beta_nodes = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert_eq!(beta_nodes.len(), 1);
    let beta_id = beta_nodes[0].id;

    // Seed a fake vector for beta (indexing ran with model=None, so no real embed).
    let fake: Vec<f32> = vec![0.1; crate::domain::EMBEDDING_DIM];
    crate::storage::queries::insert_node_vector(db.conn(), beta_id, &fake).unwrap();
    assert!(
        crate::storage::queries::get_node_embedding(db.conn(), beta_id).is_ok(),
        "fake vector must be present before the edge flip"
    );

    // Flip the edge: rename alpha → alphaRenamed. beta is a cross-file caller, so
    // its context_string is regenerated (callee set changed) but its node row is NOT
    // deleted (b.ts unchanged) — exactly the case the AFTER DELETE trigger misses.
    fs::write(
        project_dir.path().join("a.ts"),
        "function alphaRenamed() {}",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let beta_after = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert_eq!(beta_after.len(), 1);
    assert_eq!(
        beta_after[0].id, beta_id,
        "beta node id stays stable (not recreated)"
    );
    // Its stale vector must be gone, and beta re-selectable by the background embedder.
    assert!(
        crate::storage::queries::get_node_embedding(db.conn(), beta_id).is_err(),
        "stale vector for cross-file dirty node must be invalidated when model=None"
    );
    let unembedded = crate::storage::queries::get_unembedded_nodes(db.conn(), 50).unwrap();
    assert!(
        unembedded.iter().any(|(id, _)| *id == beta_id),
        "beta must be re-selectable by the background embedder after invalidation"
    );
}

#[test]
fn test_cross_language_structural_edges_isolated() {
    // v31 regression (#3): structural relations (imports/inherits/implements/
    // exports/routes_to) must NOT fall through to the global all-language name
    // pool. Before the fix a Rust `use anyhow::Result` bound an `imports` edge to
    // a markdown "Result" heading, and `require('fs')` bound to a Rust `fs`
    // symbol — cross-language phantom edges stamped `extracted` (unfilterable),
    // polluting deps/project_map/cycles/find_references. Same-language gating (+
    // the `<external>` sentinel for genuine externals) must eliminate them.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();

    // Rust `use anyhow::Result` (import target_name "Result") + a Rust fn `fs`.
    fs::write(
        project_dir.path().join("src/lib.rs"),
        "use anyhow::Result;\npub fn foo() -> Result<()> { Ok(()) }\npub fn fs() {}\n",
    )
    .unwrap();
    // Markdown heading "Result" — the cross-language collision target.
    fs::write(project_dir.path().join("README.md"), "# Result\n\nDocs.\n").unwrap();
    // JS `require('fs')` (import target_name "fs") collides with the Rust `fs` fn.
    fs::write(
        project_dir.path().join("app.js"),
        "const fs = require('fs');\nfunction g() { return fs; }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let conn = db.conn();

    // Sanity: the markdown collision target exists (so a passing test can't be a
    // false pass from the heading simply not being indexed).
    let md_result: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM nodes n JOIN files f ON f.id = n.file_id \
         WHERE n.name = 'Result' AND f.language = 'markdown'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        md_result, 1,
        "markdown 'Result' heading must be indexed (the collision target)"
    );

    // No structural edge may cross language to a NON-external target. The
    // `<external>` sentinel (language 'external') is the only allowed
    // not-same-language import/implements target.
    let cross_lang_structural: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges e \
         JOIN nodes s ON s.id = e.source_id JOIN files fs ON fs.id = s.file_id \
         JOIN nodes t ON t.id = e.target_id JOIN files ft ON ft.id = t.file_id \
         WHERE e.relation IN ('imports','inherits','implements','exports','routes_to') \
           AND fs.language IS NOT ft.language \
           AND COALESCE(ft.language,'') != 'external'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cross_lang_structural, 0,
        "structural edges must bind same-language only; got {cross_lang_structural} cross-language"
    );
}

#[test]
fn test_cross_family_structural_edges_preserved() {
    // Regression caught by adversarial review of the #3 fix: structural edges
    // WITHIN one language family (js/ts/tsx) must SURVIVE. detect_language gives
    // different strings per family member (.ts->typescript, .tsx->tsx), so a
    // same-EXACT-language gate dropped a real `.tsx` class extending a `.ts` base
    // (inherits gone; implements degraded to a phantom <external>). Family-
    // compatibility filtering must keep these while still dropping different-family
    // phantoms (covered by test_cross_language_structural_edges_isolated).
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();

    fs::write(
        project_dir.path().join("src/base.ts"),
        "export class Base {}\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/iface.ts"),
        "export interface Iface { go(): void; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/comp.tsx"),
        "class DerTsx extends Base {}\nclass ImplTsx implements Iface { go() {} }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let conn = db.conn();

    // inherits: DerTsx(.tsx) -> Base(.ts) binds to the REAL ts node (not dropped).
    let inherits_to_base: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges e \
         JOIN nodes s ON s.id = e.source_id \
         JOIN nodes t ON t.id = e.target_id JOIN files ft ON ft.id = t.file_id \
         WHERE e.relation = 'inherits' AND s.name = 'DerTsx' \
           AND t.name = 'Base' AND ft.path = 'src/base.ts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        inherits_to_base, 1,
        "cross-family inherits (.tsx -> .ts) must bind to the real base class"
    );

    // implements: ImplTsx(.tsx) -> Iface(.ts) binds to the real node, not <external>.
    let implements_to_iface: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges e \
         JOIN nodes s ON s.id = e.source_id \
         JOIN nodes t ON t.id = e.target_id JOIN files ft ON ft.id = t.file_id \
         WHERE e.relation = 'implements' AND s.name = 'ImplTsx' \
           AND t.name = 'Iface' AND ft.path = 'src/iface.ts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        implements_to_iface, 1,
        "cross-family implements (.tsx -> .ts) must bind to the real interface"
    );
    let external_iface: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM nodes n JOIN files f ON f.id = n.file_id \
         WHERE n.name = 'Iface' AND f.path = '<external>'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        external_iface, 0,
        "no phantom <external>/Iface when the real interface exists"
    );
}

#[test]
fn test_phase2c_restore_binds_only_original_target_file() {
    // v31 regression (#4) + the previously-untested happy path. The Phase-2c
    // incremental inbound-edge restore must re-bind a saved cross-file edge ONLY
    // to the same-name node in the file the edge originally pointed into — not
    // every same-name node in the batch. caller.ts → target() resolves into
    // target.ts; an incremental then re-indexes target.ts AND other.ts in one
    // batch and other.ts gains its own `target`.
    //
    // The over-creation assertion USED to be `caller → other.ts == 0`, justified
    // as "an edge a full rebuild never makes". That justification was never
    // checked, and it is false: a rebuild of this exact final tree produces BOTH
    // `caller → target.ts:target` and `caller → other.ts:target`, each
    // `ambiguous` (measured 2026-09-11). A bare call fans out to every same-name
    // candidate — the same rule D#24 is about — so the old assertion pinned that
    // divergence as the contract, in TypeScript, where D#24's own tests pinned it
    // in Python.
    //
    // So the guard is stated against a control rebuild instead of against a
    // remembered number. That is strictly stronger for its actual subject: if
    // the restore over-creates, the incremental carries an edge the rebuild does
    // not, and the equality fails. What it gives up is attribution — the final
    // graph can no longer say WHICH mechanism made the other.ts edge, because
    // D#24's fan-out round now makes it legitimately.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("src/caller.ts"),
        "function caller() { target(); }",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/target.ts"),
        "function target() {}",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/other.ts"),
        "function unrelated() {}",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let count_caller_to = |path: &str| -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM edges e \
             JOIN nodes s ON s.id = e.source_id JOIN files fs ON fs.id = s.file_id \
             JOIN nodes t ON t.id = e.target_id JOIN files ft ON ft.id = t.file_id \
             WHERE e.relation = 'calls' AND s.name = 'caller' AND t.name = 'target' \
               AND fs.path = 'src/caller.ts' AND ft.path = ?1",
                [path],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        count_caller_to("src/target.ts"),
        1,
        "initial: caller → target.ts:target"
    );
    assert_eq!(
        count_caller_to("src/other.ts"),
        0,
        "initial: other.ts has no target yet"
    );

    // Re-index BOTH target.ts (keep `target`) and other.ts (ADD a `target`) in one batch.
    fs::write(
        project_dir.path().join("src/target.ts"),
        "function target() { return 1; }",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/other.ts"),
        "function unrelated() {}\nfunction target() {}",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // Happy path: the cascade-deleted edge was restored to the NEW target.ts node.
    assert_eq!(
        count_caller_to("src/target.ts"),
        1,
        "restore must rebind caller → target.ts:target to the new node id"
    );
    // What this tree can still prove: the incremental agrees with a rebuild.
    //
    // It can NOT prove the v31 #4 over-creation guard any more, and saying so is
    // the point. `idx_edges_unique` is on (source_id, target_id, relation,
    // metadata), so a restore that wrongly bound caller → other.ts writes the
    // very row D#24's fan-out round writes correctly, and the database dedupes
    // them. No count and no set comparison over this tree can separate the two.
    // The guard moved to `restore_does_not_fan_out_to_a_same_name_sibling`,
    // which uses a tree the fan-out round provably never touches.
    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();
    assert_eq!(
        graph_projection_with_confidence(&db),
        graph_projection_with_confidence(&control),
        "an incrementally grown index must carry the same edges as a rebuild of the same tree"
    );
}

#[test]
fn restore_does_not_fan_out_to_a_same_name_sibling() {
    // v31 #4, re-homed. The Phase-2c inbound-edge restore keys on
    // `(target_file_id, name)` — the file the edge ORIGINALLY pointed into — so
    // a batch that re-indexes the original target alongside a sibling gaining
    // the same name must not hand the restored edge to both.
    //
    // The sibling test above used to carry this guard and can no longer: there,
    // D#24's fan-out round writes the same row a wrong restore would, and
    // `idx_edges_unique` collapses them. Here the call is IMPORT-BOUND, so
    // `CONF_CASE` labels it `inferred` rather than `ambiguous`, and the fan-out
    // round — which selects `ambiguous` callers only — provably never opens
    // caller.ts. Anything that appears pointing at other.ts came from the
    // restore.
    //
    // Measured control (2026-09-11): a rebuild of this final tree produces
    // `caller.ts:caller --calls--> target.ts:target` at `inferred` and NO edge
    // into other.ts, so the correct answer here is verified rather than
    // remembered — which is exactly what the old form of this guard was missing.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("src/caller.ts"),
        "import { target } from './target';\nfunction caller() { target(); }",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/target.ts"),
        "export function target() {}",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/other.ts"),
        "function unrelated() {}",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Re-index BOTH in one batch: target.ts keeps `target`, other.ts gains one.
    fs::write(
        project_dir.path().join("src/target.ts"),
        "export function target() { return 1; }",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/other.ts"),
        "function unrelated() {}\nexport function target() {}",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let inc = graph_projection_with_confidence(&db);
    assert!(
        inc.iter().any(|(s, r, t, c)| s == "src/caller.ts:caller"
            && r == REL_CALLS
            && t == "src/target.ts:target"
            && c == "inferred"),
        "happy path: the import-bound edge is restored to the new target.ts node, still \
         import-bound and so still `inferred` — if this is `ambiguous` the fan-out round \
         would be in scope and the guard below would stop meaning anything: {inc:?}"
    );
    assert!(
        !inc.iter().any(|(s, r, t, _)| s == "src/caller.ts:caller"
            && r == REL_CALLS
            && t == "src/other.ts:target"),
        "restore bound the saved edge to a same-name node in a DIFFERENT file (v31 #4): {inc:?}"
    );

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();
    assert_eq!(
        inc,
        graph_projection_with_confidence(&control),
        "an incrementally grown index must carry the same edges as a rebuild of the same tree"
    );
}

/// Every edge in the index, spelled `src.name --relation--> dst.name`, sorted.
/// The whole-graph comparison the PIPE-02 acceptance criterion is written in:
/// "terminal edge set identical, line by line, to a fresh rebuild".
fn edge_set(db: &Database) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT fs.path || '.' || ns.name || ' --' || e.relation || '--> ' \
                 || ft.path || '.' || nt.name \
             FROM edges e \
             JOIN nodes ns ON ns.id = e.source_id JOIN files fs ON fs.id = ns.file_id \
             JOIN nodes nt ON nt.id = e.target_id JOIN files ft ON ft.id = nt.file_id \
             ORDER BY 1",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.filter_map(Result::ok).collect()
}

/// Index `files` into a brand-new database — the control every differential in
/// this pair compares against.
fn fresh_index_of(files: &[(&str, &str)]) -> (TempDir, TempDir, Database) {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    for (name, body) in files {
        let path = project_dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    (project_dir, db_dir, db)
}

/// (caller start line, callee name) of every `calls` edge whose caller is named `name`.
fn calls_from_named(db: &Database, name: &str) -> Vec<(i64, String)> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT ns.start_line, nt.name FROM edges e \
             JOIN nodes ns ON ns.id = e.source_id JOIN nodes nt ON nt.id = e.target_id \
             WHERE e.relation = 'calls' AND ns.name = ?1 ORDER BY 1, 2",
        )
        .unwrap();
    let rows = stmt
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    rows.filter_map(Result::ok).collect()
}

/// D#70: a call's source was bound by NAME to every same-named node in the file,
/// so same-named twins shared each other's callees — nested route handlers in a
/// Python test file (flask: 37 `def index()` in one file, 125 of 168 wrong
/// inferred edges), cfg twins in Rust. Each twin must keep only its own calls.
#[test]
fn test_same_named_functions_in_one_file_keep_their_own_calls() {
    let py = "def helper_a():\n    return 1\n\ndef helper_b():\n    return 2\n\n\
              def test_one():\n    def index():\n        return helper_a()\n    return index\n\n\
              def test_two():\n    def index():\n        return helper_b()\n    return index\n";
    let rs = "#[cfg(unix)]\nfn lock() {\n    unix_impl();\n}\n#[cfg(not(unix))]\nfn lock() {\n    other_impl();\n}\n\
              fn unix_impl() {}\nfn other_impl() {}\n";
    let (_p, _d, db) = fresh_index_of(&[("app.py", py), ("lib.rs", rs)]);
    let index = calls_from_named(&db, "index");
    assert_eq!(
        index,
        vec![(8, "helper_a".to_string()), (13, "helper_b".to_string())]
    );
    let lock = calls_from_named(&db, "lock");
    assert_eq!(lock.len(), 2, "one call per twin, got {lock:?}");
    assert!(
        lock[0].0 < lock[1].0 && lock[0].1 == "unix_impl" && lock[1].1 == "other_impl",
        "got {lock:?}"
    );
}

/// D#86: a member call on an object can only run a method. Bound by bare name it
/// reached free functions: `words.push(x)` a nested `const push`, `JSON.stringify`
/// a project `function stringify`, `s.clear()` a free `clear()`, `ctx.get()` a
/// module-level `def get`. Module-binding calls and bare calls keep their targets.
#[test]
fn test_member_call_on_an_object_never_binds_a_free_function() {
    let files: &[(&str, &str)] = &[
        (
            "a.js",
            "function outer() { const push = () => 1; return push(); }\n\
             function other(words) { words.push(1); }\n\
             function f(v) { return JSON.stringify(v); }\n\
             const u = require('./util');\nfunction g(v) { return u.stringify(v); }\n",
        ),
        ("util.js", "function stringify(v) { return v; }\nmodule.exports = { stringify };\n"),
        (
            "x.cc",
            "void clear() {}\nstruct Cache { void clear() {} };\n\
             void run(std::string& s) { s.clear(); }\nvoid both(Cache& c) { c.clear(); clear(); }\n",
        ),
        ("m.py", "import helpers\n\ndef get():\n    pass\n\ndef h(ctx):\n    ctx.get(1)\n\ndef k():\n    helpers.util()\n"),
        ("helpers.py", "def util():\n    pass\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    // x.cc's free `clear` and `Cache::clear` share a name: judge by the target's type.
    let target_types = |caller: &str| -> Vec<String> {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT DISTINCT nt.type FROM edges e JOIN nodes ns ON ns.id = e.source_id \
                 JOIN nodes nt ON nt.id = e.target_id \
                 WHERE e.relation = 'calls' AND ns.name = ?1 AND nt.name = 'clear' ORDER BY 1",
            )
            .unwrap();
        stmt.query_map([caller], |r| r.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    // `s` is a `std::string`: no project `clear` of any kind runs.
    assert!(
        target_types("run").is_empty(),
        "s.clear() on a std::string bound a project clear"
    );
    assert_eq!(
        target_types("both"),
        vec!["function".to_string(), "method".to_string()]
    );
    for wrong in [
        "a.js.other --calls--> a.js.push",
        "a.js.f --calls--> util.js.stringify",
        "m.py.h --calls--> m.py.get",
    ] {
        assert!(
            !has(wrong),
            "member call bound a free function: {wrong}\n{edges:#?}"
        );
    }
    for right in [
        "a.js.outer --calls--> a.js.push",
        "a.js.g --calls--> util.js.stringify",
        "m.py.k --calls--> helpers.py.util",
    ] {
        assert!(has(right), "lost: {right}\n{edges:#?}");
    }
}

/// D#89: a member call whose receiver's class the source writes down binds
/// that class's method, not every same-named one — `this`/`self`, a C++ local,
/// parameter or field's declared type, JS `new T()`, a TS `: T`, Python
/// `super()`. A class the project does not define binds nothing; a subclass's
/// override is bound too, except through `super()`.
#[test]
fn test_typed_receiver_binds_its_own_class_method() {
    let files: &[(&str, &str)] = &[
        (
            "x.cc",
            "struct A { void run() {} void go() { this->run(); } };\n\
             struct B { void run() {} };\n\
             struct H { B field; void use() { field.run(); } };\n\
             void f(A& a, B* b, std::string s) { a.run(); b->run(); s.run(); }\n",
        ),
        (
            "a.ts",
            "class P { run() {} go() { this.run(); } }\n\
             class Q { run() {} }\n\
             function h() { const q = new Q(); q.run(); }\n\
             function k(p: P) { p.run(); }\n\
             function m() { const c = new AbortController(); c.run(); }\n",
        ),
        (
            "m.py",
            "import click\n\n\
             def echo():\n    pass\n\n\
             def cmd():\n    click.echo('x')\n\n\
             class Base:\n    def run(self):\n        pass\n\n    def go(self):\n        self.run()\n\n\
             class Sub(Base):\n    def run(self):\n        super().run()\n\n\
             class Leaf(Sub):\n    def run(self):\n        pass\n\n\
             class Other:\n    def run(self):\n        pass\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let calls = |caller: &str| -> Vec<String> {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT DISTINCT COALESCE(nt.qualified_name, nt.name) FROM edges e \
                 JOIN nodes ns ON ns.id = e.source_id JOIN nodes nt ON nt.id = e.target_id \
                 WHERE e.relation = 'calls' AND (ns.qualified_name = ?1 OR ns.name = ?1) \
                 AND nt.name IN ('run', 'echo') ORDER BY 1",
            )
            .unwrap();
        stmt.query_map([caller], |r| r.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    for (caller, want) in [
        ("A.go", vec!["A.run"]),
        ("H.use", vec!["B.run"]),
        ("f", vec!["A.run", "B.run"]),
        ("P.go", vec!["P.run"]),
        ("h", vec!["Q.run"]),
        ("k", vec!["P.run"]),
        ("m", vec![]),
        ("cmd", vec![]),
        ("Base.go", vec!["Base.run", "Leaf.run", "Sub.run"]),
        ("Sub.run", vec!["Base.run"]),
    ] {
        assert_eq!(calls(caller), want, "calls of {caller}");
    }
}

/// The same receiver typing on the pending-call sweep: a typed call buffered
/// because no candidate existed yet binds its class's method when a later run
/// adds it, not every same-named one.
#[test]
fn test_pending_typed_receiver_binds_its_own_class_method() {
    // `r.ts` gives `run` a candidate from the start, so `q.run()` is not merely
    // unresolved: its class is unknown, and it must stay buffered for `Q`.
    let (project, _d, db) = fresh_index_of(&[
        (
            "a.ts",
            "import { Q } from './q';\nfunction h() { const q = new Q(); q.run(); }\n\
             function m() { const c = new AbortController(); c.run(); }\n",
        ),
        ("r.ts", "export class R { run() {} }\n"),
    ]);
    assert!(
        !edge_set(&db)
            .iter()
            .any(|e| e.starts_with("a.ts.") && e.ends_with(".run")),
        "a class the project does not define binds nothing"
    );
    fs::write(
        project.path().join("q.ts"),
        "export class Q { run() {} }\nexport class S { run() {} }\n",
    )
    .unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let edges = edge_set(&db);
    let runs: Vec<_> = edges
        .iter()
        .filter(|e| e.starts_with("a.ts.") && e.ends_with(".run"))
        .collect();
    assert_eq!(runs, ["a.ts.h --calls--> q.ts.run"], "{edges:#?}");
}

/// D#89, the C++ spellings a class name alone gets wrong: a nested class
/// (`SkipList::Iterator`, defined in its outer class's body, its methods
/// qualified `Iterator.Next`) is not the top-level `Iterator`, and of two
/// same-named classes the caller's own file's wins.
#[test]
fn test_typed_receiver_tells_same_named_cpp_classes_apart() {
    let files: &[(&str, &str)] = &[
        ("iter.h", "class Iterator {\n public:\n  virtual void Next() = 0;\n};\n"),
        (
            "skiplist.h",
            "template <class K> class SkipList {\n public:\n  class Iterator {\n   public:\n    void Next();\n  };\n};\n\
             template <class K> void SkipList<K>::Iterator::Next() {}\n\
             void walk(SkipList<int>::Iterator it) { it.Next(); }\n",
        ),
        ("user.cc", "void drain(Iterator* it) { it->Next(); }\n"),
        ("b1.cc", "class Bench { public: void Run() {} };\nint main() { Bench b; b.Run(); }\n"),
        ("b2.cc", "class Bench { public: void Run() {} };\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let meta_of = |caller: &str, callee: &str| -> Vec<(String, Option<String>)> {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT ft.path, e.metadata FROM edges e \
                 JOIN nodes ns ON ns.id = e.source_id JOIN nodes nt ON nt.id = e.target_id \
                 JOIN files ft ON ft.id = nt.file_id \
                 WHERE e.relation = 'calls' AND ns.name = ?1 AND nt.name = ?2 ORDER BY 1",
            )
            .unwrap();
        stmt.query_map([caller, callee], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    // Spelled with its outer class: the nested class's method, typed.
    assert_eq!(
        meta_of("walk", "Next"),
        vec![(
            "skiplist.h".to_string(),
            Some(r#"{"q":"rtype","v":"SkipList<int>::Iterator"}"#.to_string())
        )]
    );
    // A bare `Iterator` is the top-level one, which defines no `Next`: not
    // claimed as the nested class's, only an untyped member call.
    let drain = meta_of("drain", "Next");
    assert!(
        drain
            .iter()
            .all(|(_, m)| m.as_deref().is_some_and(|m| m.contains(r#""amb":1"#))),
        "{drain:?}"
    );
    assert_eq!(
        meta_of("main", "Run"),
        vec![(
            "b1.cc".to_string(),
            Some(r#"{"q":"rtype","v":"Bench"}"#.to_string())
        )]
    );
}

/// Every `calls` edge as `caller-path.caller -> callee-path.callee [metadata]
/// confidence`, sorted: what an incremental run must agree on with a rebuild.
fn call_edges_with_confidence(db: &Database) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT fs.path || '.' || ns.name || ' -> ' || ft.path || '.' \
                 || COALESCE(nt.qualified_name, nt.name) || ' ' \
                 || COALESCE(e.metadata, '-') || ' ' || COALESCE(e.confidence, '-') \
             FROM edges e \
             JOIN nodes ns ON ns.id = e.source_id JOIN files fs ON fs.id = ns.file_id \
             JOIN nodes nt ON nt.id = e.target_id JOIN files ft ON ft.id = nt.file_id \
             WHERE e.relation = 'calls' ORDER BY 1",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.filter_map(Result::ok).collect()
}

/// Index `before`, write `after` over it (a file mapped to None is deleted),
/// index incrementally, and require the calls a fresh index of the result has.
fn assert_incremental_matches_rebuild(before: &[(&str, &str)], after: &[(&str, Option<&str>)]) {
    let (project, _d, db) = fresh_index_of(before);
    let mut tree: Vec<(String, String)> = before
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_string()))
        .collect();
    for (path, body) in after {
        tree.retain(|(p, _)| p != path);
        match body {
            Some(b) => {
                fs::write(project.path().join(path), b).unwrap();
                tree.push((path.to_string(), b.to_string()));
            }
            None => fs::remove_file(project.path().join(path)).unwrap(),
        }
    }
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let files: Vec<(&str, &str)> = tree.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let (_p2, _d2, control) = fresh_index_of(&files);
    assert_eq!(
        call_edges_with_confidence(&db),
        call_edges_with_confidence(&control),
        "incremental after {after:?} must equal a rebuild"
    );
}

/// Pre-ship review of D#89: every way an incremental run bound a typed call
/// differently from a rebuild.
#[test]
fn test_typed_receiver_incremental_matches_rebuild() {
    // Editing the callee file restored `f->Run()` to every `Run` in it. (`.hpp`:
    // a `.h` of plain structs is read as C, which has no member calls.)
    let a_hpp = "struct Foo { void Run() {} };\nstruct Bar { void Run() {} };\n";
    assert_incremental_matches_rebuild(
        &[("a.hpp", a_hpp), ("b.cc", "void g(Foo* f) { f->Run(); }\n")],
        &[("a.hpp", Some(&format!("{a_hpp}// touched\n")))],
    );
    // Renaming one of two same-named bases requeues the `super()` call, which
    // must re-resolve as the typed call a rebuild sees — not as an untyped
    // member call binding the caller's file's unrelated `__init__`.
    let sub = "from a import Base\n\nclass Helper:\n    def __init__(self):\n        pass\n\n\
               class Sub(Base):\n    def __init__(self):\n        super().__init__()\n";
    assert_incremental_matches_rebuild(
        &[
            (
                "a.py",
                "class Base:\n    def __init__(self):\n        pass\n",
            ),
            (
                "b.py",
                "class Base:\n    def __init__(self):\n        pass\n",
            ),
            ("c.py", sub),
        ],
        &[(
            "b.py",
            Some("class Base2:\n    def __init__(self):\n        pass\n"),
        )],
    );
    // Removing the base's method: the override calling `super()` is the only
    // same-named method in its file and binds nothing, as in a rebuild.
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def deconstruct(self):\n        pass\n"),
            (
                "g.py",
                "from a import Field\n\nclass Gen(Field):\n    def deconstruct(self):\n        super().deconstruct()\n",
            ),
            ("x.py", "class Other:\n    def deconstruct(self):\n        pass\n"),
        ],
        &[("a.py", Some("class Field:\n    pass\n"))],
    );
    // ... and to an unrelated class's `step` beside the override it had.
    let sub = "from base import Base\n\nclass Sub(Base):\n    def step(self):\n        pass\n\n\
               class Unrelated:\n    def step(self):\n        pass\n";
    assert_incremental_matches_rebuild(
        &[
            (
                "base.py",
                "class Base:\n    def run(self):\n        self.step()\n\n    def step(self):\n        pass\n",
            ),
            ("sub.py", sub),
        ],
        &[("sub.py", Some(&format!("{sub}# touched\n")))],
    );
    // An untyped member call (here `super()` onto two same-named bases) bound
    // the candidates nearest its caller; editing one base's file must not
    // re-bind it to every `__init__` there.
    let forms = "class Field:\n    def __init__(self):\n        pass\n\n\
                 class CharField(Field):\n    def __init__(self):\n        pass\n";
    assert_incremental_matches_rebuild(
        &[
            ("db_field.py", "class Field:\n    def __init__(self):\n        pass\n"),
            ("forms_field.py", forms),
            (
                "gis.py",
                "from db_field import Field\n\nclass Geo(Field):\n    def __init__(self):\n        super().__init__()\n",
            ),
        ],
        &[("forms_field.py", Some(&format!("{forms}# touched\n")))],
    );
    // A buffered call whose class appears later: no noise-name bind, and the
    // edge is classified like a rebuild's.
    assert_incremental_matches_rebuild(
        &[
            (
                "other.py",
                "class Other:\n    def build(self):\n        pass\n\n    def launch(self):\n        pass\n",
            ),
            ("b.py", "def g():\n    x = Foo()\n    x.build()\n    x.launch()\n"),
        ],
        &[("c.py", Some("class Foo:\n    pass\n"))],
    );
    // A noise-named method the class gains later is bound, as a rebuild binds it.
    assert_incremental_matches_rebuild(
        &[
            ("c.py", "class Foo:\n    pass\n"),
            (
                "b.py",
                "from c import Foo\n\ndef g():\n    x = Foo()\n    x.build()\n",
            ),
        ],
        &[(
            "c.py",
            Some("class Foo:\n    def build(self):\n        pass\n"),
        )],
    );
}

// D#97 M-N3: a typed call binds by the whole project's class structure — which
// classes carry its receiver's name, which are nested, which subclass which, and
// which of those define the method. An edit that changes that structure changes
// what a rebuild binds for callers in files the edit never touched. One shape per
// test, so each reports on its own.

const TYPED_USER_PY: &str = "def g():\n    f = Field()\n    f.clean()\n";

/// Two same-named classes: the caller's own file's `Field` wins, but a name two
/// classes share seeds no overrides. Deleting the other makes the name unique
/// and `Char.clean` an override — for a caller with no edge into the deleted file
/// (django: deleting forms/fields.py, 19 edges).
const B_PY: &str = "class Field:\n    def clean(self):\n        pass\n\n\
                    def g():\n    f = Field()\n    f.clean()\n";
const CHAR_PY: &str =
    "from b import Field\n\nclass Char(Field):\n    def clean(self):\n        pass\n";

#[test]
fn test_class_delete_re_resolves_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            ("b.py", B_PY),
            ("sub.py", CHAR_PY),
        ],
        &[("a.py", None)],
    );
}

/// Renaming a class away has the same effect as deleting it; renaming one TO
/// the name has the reverse one.
#[test]
fn test_class_rename_re_resolves_untouched_typed_callers() {
    let a = "class Field:\n    def clean(self):\n        pass\n";
    let widget = "class Widget:\n    def clean(self):\n        pass\n";
    let files = [("b.py", B_PY), ("sub.py", CHAR_PY)];
    assert_incremental_matches_rebuild(
        &[("a.py", a), files[0], files[1]],
        &[("a.py", Some(widget))],
    );
    assert_incremental_matches_rebuild(
        &[("a.py", widget), files[0], files[1]],
        &[("a.py", Some(a))],
    );
}

/// A subclass added later overrides the method: its override runs too.
#[test]
fn test_new_subclass_override_reaches_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            ("user.py", TYPED_USER_PY),
        ],
        &[(
            "sub.py",
            Some("from a import Field\n\nclass Char(Field):\n    def clean(self):\n        pass\n"),
        )],
    );
}

/// The twin defines no `clean`: only its name's uniqueness moves — shared to
/// unique when it goes, unique to shared (overrides no longer followed) when a
/// class is renamed to it.
#[test]
fn test_methodless_class_delete_re_resolves_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    pass\n"),
            ("b.py", B_PY),
            ("sub.py", CHAR_PY),
        ],
        &[("a.py", None)],
    );
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Widget:\n    pass\n"),
            ("b.py", B_PY),
            ("sub.py", CHAR_PY),
        ],
        &[("a.py", Some("class Field:\n    pass\n"))],
    );
}

/// A subclass of a subclass: the typed class is an ancestor two levels up.
#[test]
fn test_new_grandchild_override_reaches_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            (
                "sub.py",
                "from a import Field\n\nclass Char(Field):\n    def clean(self):\n        pass\n",
            ),
            ("user.py", TYPED_USER_PY),
        ],
        &[(
            "slug.py",
            Some("from sub import Char\n\nclass Slug(Char):\n    def clean(self):\n        pass\n"),
        )],
    );
}

/// A grandchild changes its base: `Field`, two levels up, is in no moved row, so
/// only the ancestor closure of the old base reaches its callers.
#[test]
fn test_grandchild_changed_base_drops_override_from_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            (
                "sub.py",
                "from a import Field\n\nclass Char(Field):\n    def clean(self):\n        pass\n",
            ),
            ("o.py", "class Other:\n    pass\n"),
            (
                "slug.py",
                "from sub import Char\n\nclass Slug(Char):\n    def clean(self):\n        pass\n",
            ),
            ("user.py", TYPED_USER_PY),
        ],
        &[(
            "slug.py",
            Some("from o import Other\n\nclass Slug(Other):\n    def clean(self):\n        pass\n"),
        )],
    );
}

/// The drift is derived from rows free of line numbers, so an edit that only
/// moves code pulls no typed caller into the fan-out round — the ordinary edit
/// pays for two snapshots and nothing else. Positive control in the same tree:
/// adding the override does pull `user.py`.
#[test]
fn test_moving_code_is_no_class_drift() {
    let sub = "from a import Field\n\nclass Char(Field):\n    def other(self):\n        pass\n";
    let (project, _d, db) = fresh_index_of(&[
        ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
        ("sub.py", sub),
        ("user.py", TYPED_USER_PY),
    ]);
    let drift_after = |body: String| {
        fs::write(project.path().join("sub.py"), body).unwrap();
        let paths = vec!["sub.py".to_string()];
        super::resolve::snapshot_definition_counts(db.conn(), &paths).unwrap();
        index_files(
            &db,
            project.path(),
            &paths,
            &std::collections::HashMap::new(),
            None,
            &[],
            None,
        )
        .unwrap();
        let typed = super::resolve::typed_callers_of_class_drift(db.conn()).unwrap();
        super::resolve::drop_fanout_temps(db.conn()).unwrap();
        typed
    };
    assert_eq!(
        drift_after(format!("# moved\n\n{sub}\n# touched\n")),
        Vec::<String>::new()
    );
    assert_eq!(
        drift_after(format!("{sub}\n    def clean(self):\n        pass\n")),
        vec!["user.py".to_string()],
        "control: a new override must count as drift"
    );
}

/// A call typed by a name two classes share follows no overrides, so a new
/// override of it changes nothing a rebuild binds and pulls no caller (django's
/// `Field`: without this filter one such edit re-extracted callers for +1.1 s
/// and moved no edge). Control: the same edit under a unique name does pull.
#[test]
fn test_override_of_a_shared_class_name_is_no_drift() {
    let drift = |shared: bool| {
        let mut files = vec![
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            ("user.py", TYPED_USER_PY),
        ];
        if shared {
            files.push(("b.py", "class Field:\n    def clean(self):\n        pass\n"));
        }
        let (project, _d, db) = fresh_index_of(&files);
        let paths = vec!["sub.py".to_string()];
        fs::write(
            project.path().join("sub.py"),
            "from a import Field\n\nclass Char(Field):\n    def clean(self):\n        pass\n",
        )
        .unwrap();
        super::resolve::snapshot_definition_counts(db.conn(), &paths).unwrap();
        index_files(
            &db,
            project.path(),
            &paths,
            &std::collections::HashMap::new(),
            None,
            &[],
            None,
        )
        .unwrap();
        let typed = super::resolve::typed_callers_of_class_drift(db.conn()).unwrap();
        super::resolve::drop_fanout_temps(db.conn()).unwrap();
        typed
    };
    assert_eq!(drift(true), Vec::<String>::new());
    assert_eq!(drift(false), vec!["user.py".to_string()], "control");
}

/// A class whose name another class shares gains the method, turned from a free
/// function of the same file: `assemble`'s count is unchanged (no D#24 round) and
/// the caller's edge went to another file (nothing to restore), yet a rebuild
/// binds the class's own method where the untyped call bound `Other.assemble`.
/// (A noise name such as `build` is not covered: the untyped call binds nothing
/// and leaves no edge or buffered row for the drift to find.)
#[test]
fn test_free_function_turned_method_re_resolves_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            (
                "a.py",
                "class Foo:\n    pass\n\n\ndef assemble():\n    pass\n",
            ),
            ("c.py", "class Foo:\n    pass\n"),
            (
                "o.py",
                "class Other:\n    def assemble(self):\n        pass\n",
            ),
            (
                "b.py",
                "from a import Foo\n\ndef g():\n    x = Foo()\n    x.assemble()\n",
            ),
        ],
        &[(
            "a.py",
            Some("class Foo:\n    def assemble(self):\n        pass\n"),
        )],
    );
}

/// The same with a method name too common to guess at (`build`): the untyped
/// call binds nothing, so only a buffered row remembers it.
#[test]
fn test_noise_named_method_gained_later_reaches_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Foo:\n    pass\n\n\ndef build():\n    pass\n"),
            ("c.py", "class Foo:\n    pass\n"),
            ("o.py", "class Other:\n    def build(self):\n        pass\n"),
            (
                "b.py",
                "from a import Foo\n\ndef g():\n    x = Foo()\n    x.build()\n",
            ),
        ],
        &[(
            "a.py",
            Some("class Foo:\n    def build(self):\n        pass\n"),
        )],
    );
}

/// An existing subclass gains the override.
#[test]
fn test_new_override_method_reaches_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            (
                "sub.py",
                "from a import Field\n\nclass Char(Field):\n    def other(self):\n        pass\n",
            ),
            ("user.py", TYPED_USER_PY),
        ],
        &[(
            "sub.py",
            Some(
                "from a import Field\n\nclass Char(Field):\n    def other(self):\n        pass\n\n    \
                 def clean(self):\n        pass\n",
            ),
        )],
    );
}

/// A subclass that stops inheriting the class is no longer an override of it.
#[test]
fn test_changed_base_drops_override_from_untouched_typed_callers() {
    assert_incremental_matches_rebuild(
        &[
            ("a.py", "class Field:\n    def clean(self):\n        pass\n"),
            ("o.py", "class Other:\n    pass\n"),
            (
                "sub.py",
                "from a import Field\n\nclass Char(Field):\n    def clean(self):\n        pass\n",
            ),
            ("user.py", TYPED_USER_PY),
        ],
        &[(
            "sub.py",
            Some("from o import Other\n\nclass Char(Other):\n    def clean(self):\n        pass\n"),
        )],
    );
}

/// C++: a nested `SkipList::Iterator` answers to a bare `Iterator` only when no
/// top-level class has that name (leveldb: deleting iterator.h, 861 edges).
#[test]
fn test_top_level_class_delete_re_resolves_untouched_cpp_callers() {
    assert_incremental_matches_rebuild(
        &[
            (
                "iterator.hpp",
                "class Iterator {\n public:\n  void Next() {}\n};\n",
            ),
            (
                "skiplist.hpp",
                "class SkipList {\n public:\n  class Iterator {\n   public:\n    void Next() {}\n  };\n};\n",
            ),
            ("user.cc", "void g(Iterator* it) { it->Next(); }\n"),
        ],
        &[("iterator.hpp", None)],
    );
}

// D#97: C++ calls through a field the caller's own file never declares — an
// out-of-line member (fields in the header), a gtest `TEST_F` body (fields of
// the fixture) — are typed from the fields the class's file recorded.

/// The callee (path.qualified) of every call from a caller named `caller`.
fn callees_of(db: &Database, caller: &str) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT ft.path || '.' || COALESCE(nt.qualified_name, nt.name) FROM edges e \
             JOIN nodes ns ON ns.id = e.source_id \
             JOIN nodes nt ON nt.id = e.target_id JOIN files ft ON ft.id = nt.file_id \
             WHERE e.relation = 'calls' AND ns.name = ?1 ORDER BY 1",
        )
        .unwrap();
    let rows = stmt.query_map([caller], |r| r.get::<_, String>(0)).unwrap();
    rows.filter_map(Result::ok).collect()
}

const DB_IMPL_HPP: &str = "#include <string>\n\
    class SnapshotList {\n public:\n  void Delete(int s) {}\n  void clear() {}\n};\n\
    class DBImpl {\n public:\n  void Delete(int k) {}\n  void Release(int s);\n\
     private:\n  SnapshotList snapshots_ GUARDED_BY(mutex_);\n  std::string saved_;\n};\n";
const DB_IMPL_CC: &str = "#include \"db_impl.hpp\"\n\
    void DBImpl::Release(int s) {\n  snapshots_.Delete(s);\n  saved_.clear();\n}\n";

#[test]
fn test_cpp_out_of_line_member_binds_its_field_type() {
    let (_p, _d, db) = fresh_index_of(&[("db_impl.hpp", DB_IMPL_HPP), ("db_impl.cc", DB_IMPL_CC)]);
    // Not `DBImpl.Delete`; and `std::string::clear` is no project method.
    assert_eq!(
        callees_of(&db, "Release"),
        vec!["db_impl.hpp.SnapshotList.Delete".to_string()]
    );
}

/// The same in a `.h` header, the most common C++ layout: detected as C by its
/// extension and re-parsed as C++ by its content, its fields must still be recorded.
#[test]
fn test_cpp_field_in_a_dot_h_header_is_typed() {
    let (_p, _d, db) = fresh_index_of(&[("db_impl.h", DB_IMPL_HPP), ("db_impl.cc", DB_IMPL_CC)]);
    assert_eq!(
        callees_of(&db, "Release"),
        vec!["db_impl.h.SnapshotList.Delete".to_string()]
    );
}

/// A chain whose final type is no project class yet stays untyped; renaming a
/// class to that name must reach the untouched caller, as a rebuild types it.
#[test]
fn test_class_renamed_to_a_chain_s_final_type_re_resolves_untouched_callers() {
    let a = "class B;\nclass A {\n public:\n  B* get() { return nullptr; }\n};\n";
    let x = "class X {\n public:\n  void run() {}\n};\n";
    let c = "#include \"a.hpp\"\nvoid f(A* a) {\n  a->get()->run();\n}\n";
    assert_incremental_matches_rebuild(
        &[
            ("a.hpp", a),
            ("b.hpp", "class Bx {\n public:\n  void run() {}\n};\n"),
            ("x.hpp", x),
            ("c.cc", c),
        ],
        &[("b.hpp", Some("class B {\n public:\n  void run() {}\n};\n"))],
    );
    // Even a library type: a project class named `string` makes a rebuild type
    // `std::string` by its last name.
    let a =
        "#include <string>\nclass A {\n public:\n  std::string* get() { return nullptr; }\n};\n";
    let x = "class X {\n public:\n  int size() { return 0; }\n};\n";
    let c = "#include \"a.hpp\"\nvoid f(A* a) {\n  a->get()->size();\n}\n";
    assert_incremental_matches_rebuild(
        &[
            ("a.hpp", a),
            (
                "b.hpp",
                "class stringx {\n public:\n  int size() { return 0; }\n};\n",
            ),
            ("x.hpp", x),
            ("c.cc", c),
        ],
        &[(
            "b.hpp",
            Some("class string {\n public:\n  int size() { return 0; }\n};\n"),
        )],
    );
}

/// A method returning its template's parameter returns whatever instantiates
/// it, even when the parameter is named like a project class.
#[test]
fn test_cpp_chain_through_a_template_parameter_return_stays_untyped() {
    let (_p, _d, db) = fresh_index_of(&[(
        "w.hpp",
        "class Iterator {\n public:\n  virtual void Next() {}\n};\n\
         class Other {\n public:\n  void Next() {}\n};\n\
         template <typename Iterator>\n\
         class Wrapper {\n public:\n  Iterator* inner() { return nullptr; }\n};\n\
         class User {\n  void Go();\n  Wrapper<Other>* w_;\n};\n\
         void User::Go() {\n  w_->inner()->Next();\n}\n",
    )]);
    let callees = callees_of(&db, "Go");
    assert!(
        callees.contains(&"w.hpp.Other.Next".to_string()),
        "the instantiating class stays a candidate: {callees:?}"
    );
}

/// The same through a reference return, and through a template base class the
/// chain's class inherits the method from.
#[test]
fn test_cpp_template_parameter_return_via_reference_or_base_stays_untyped() {
    let classes = "class Iterator {\n public:\n  virtual void Next() {}\n};\n\
                   class Other {\n public:\n  void Next() {}\n};\n";
    let (_p, _d, db) = fresh_index_of(&[(
        "r.hpp",
        &format!(
            "{classes}template <typename Iterator>\n\
             class Wrapper {{\n public:\n  const Iterator& inner() {{ return *p_; }}\n  Iterator* p_;\n}};\n\
             class User {{\n  void Go();\n  Wrapper<Other>* w_;\n}};\n\
             void User::Go() {{\n  w_->inner().Next();\n}}\n"
        ),
    )]);
    let callees = callees_of(&db, "Go");
    assert!(
        callees.contains(&"r.hpp.Other.Next".to_string()),
        "reference: {callees:?}"
    );
    let (_p, _d, db) = fresh_index_of(&[(
        "b.hpp",
        &format!(
            "{classes}template <typename Iterator>\n\
             class Base {{\n public:\n  Iterator* inner() {{ return nullptr; }}\n}};\n\
             class Wrapper : public Base<Other> {{}};\n\
             class User {{\n  void Go();\n  Wrapper* w_;\n}};\n\
             void User::Go() {{\n  w_->inner()->Next();\n}}\n"
        ),
    )]);
    let callees = callees_of(&db, "Go");
    assert!(
        callees.contains(&"b.hpp.Other.Next".to_string()),
        "base: {callees:?}"
    );
}

#[test]
fn test_cpp_field_declared_in_a_base_class_is_typed() {
    let (_p, _d, db) = fresh_index_of(&[
        (
            "base.hpp",
            "class Arena {\n public:\n  void Allocate(int n) {}\n};\n\
             class Pool {\n public:\n  void Allocate(int n) {}\n};\n\
             class Base {\n protected:\n  Arena* arena_;\n};\n\
             class Derived : public Base {\n public:\n  void Grow();\n};\n",
        ),
        (
            "derived.cc",
            "#include \"base.hpp\"\nvoid Derived::Grow() {\n  arena_->Allocate(1);\n}\n",
        ),
    ]);
    assert_eq!(
        callees_of(&db, "Grow"),
        vec!["base.hpp.Arena.Allocate".to_string()]
    );
}

#[test]
fn test_cpp_gtest_body_types_the_fixture_field() {
    let (_p, _d, db) = fresh_index_of(&[(
        "db_test.cc",
        "class DB {\n public:\n  virtual void Put(int v) {}\n};\n\
         class DBTest {\n public:\n  DB* db_;\n  void Put(int v) {}\n};\n\
         TEST_F(DBTest, Get) {\n  db_->Put(1);\n}\n",
    )]);
    assert_eq!(
        callees_of(&db, "DBTest.Get"),
        vec!["db_test.cc.DB.Put".to_string()]
    );
}

/// A supertype is a type. A C++ class shares its name with its constructor, and
/// `testing::Test` with any project method named `Test`: leveldb had 93 `inherits`
/// edges into methods, which made unrelated classes each other's subclasses.
#[test]
fn test_inherits_never_targets_a_function_or_method() {
    let (_p, _d, db) = fresh_index_of(&[
        (
            "table_test.cc",
            "class Constructor {\n public:\n  explicit Constructor(int n) {}\n};\n\
             class BlockConstructor : public Constructor {\n public:\n  BlockConstructor() : Constructor(1) {}\n};\n\
             class Harness {\n public:\n  void Test(int n) {}\n};\n\
             void Reporter() {}\nclass Logger : public Reporter {};\n",
        ),
        (
            "db_test.cc",
            "class DBTest : public testing::Test {\n public:\n  void Put(int v) {}\n};\n",
        ),
    ]);
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT s.name || ' -> ' || t.type || ' ' || COALESCE(t.qualified_name, t.name) \
             FROM edges e JOIN nodes s ON s.id = e.source_id JOIN nodes t ON t.id = e.target_id \
             WHERE e.relation = 'inherits' ORDER BY 1",
        )
        .unwrap();
    let got: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        got.iter()
            .all(|e| !e.contains(" -> function ") && !e.contains(" -> method ")),
        "{got:#?}"
    );
    assert!(
        got.contains(&"BlockConstructor -> class Constructor".to_string()),
        "{got:#?}"
    );
}

const VERSION_HPP: &str = "class Version {\n public:\n  void Ref() {}\n};\n\
    class Other {\n public:\n  void Ref() {}\n};\n\
    class VersionSet {\n public:\n  Version* current() const { return current_; }\n\
     private:\n  Version* current_;\n};\n\
    class BlockBuilder {\n public:\n  void Add(int k) {}\n};\n\
    class DBImpl {\n public:\n  void Get();\n  void Add(int k) {}\n\
     private:\n  struct Rep {\n    BlockBuilder index_block;\n  };\n  \
    VersionSet* versions_;\n  Rep* rep_;\n};\n";
const GET_CC: &str = "#include \"version.hpp\"\n\
    void DBImpl::Get() {\n  versions_->current()->Ref();\n  Rep* r = rep_;\n  r->index_block.Add(1);\n}\n";

/// A receiver that is a chain of fields and calls is typed through the recorded
/// field types and return types: `versions_->current()->Ref()` binds
/// `Version::Ref`, `r->index_block.Add()` binds `BlockBuilder::Add`, where each
/// bound every same-named method (leveldb: 23 of the wrong same-file edges).
#[test]
fn test_cpp_chained_receiver_binds_through_fields_and_returns() {
    let (_p, _d, db) = fresh_index_of(&[("version.hpp", VERSION_HPP), ("get.cc", GET_CC)]);
    assert_eq!(
        callees_of(&db, "Get"),
        vec![
            "version.hpp.BlockBuilder.Add".to_string(),
            "version.hpp.Version.Ref".to_string(),
            "version.hpp.VersionSet.current".to_string()
        ]
    );
}

/// A chain whose step is inherited (`env_.target()` on an `ErrorEnv` that gets
/// `target()` from `EnvWrapper`) is typed on a FULL index too, where the
/// cross-file `inherits` edges are resolved in the same deferred pass: loading
/// the class hierarchy before them left it empty, so a rebuild typed nothing
/// that an incremental run typed. The field's type is namespaced
/// (`test::ErrorEnv`), which is no nesting.
#[test]
fn test_cpp_inherited_chain_step_is_typed_on_a_full_index() {
    let (_p, _d, db) = fresh_index_of(&[
        (
            "env.hpp",
            "class Env {\n public:\n  virtual void GetChildren() {}\n};\n\
             class EnvWrapper : public Env {\n public:\n  Env* target() const { return t_; }\n\
              private:\n  Env* t_;\n};\n",
        ),
        (
            "testutil.hpp",
            "#include \"env.hpp\"\nnamespace test {\nclass ErrorEnv : public EnvWrapper {};\n}\n",
        ),
        (
            "other.cc",
            "class Lister {\n public:\n  void GetChildren() {}\n};\n",
        ),
        (
            "corruption_test.cc",
            "#include \"testutil.hpp\"\nclass CorruptionTest {\n public:\n  void Corrupt();\n\
              private:\n  test::ErrorEnv env_;\n};\n\
             void CorruptionTest::Corrupt() {\n  env_.target()->GetChildren();\n}\n",
        ),
    ]);
    assert_eq!(
        callees_of(&db, "Corrupt"),
        vec![
            "env.hpp.Env.GetChildren".to_string(),
            "env.hpp.EnvWrapper.target".to_string()
        ]
    );
    // `target()` is what `test::ErrorEnv` inherits: decided, not the untyped
    // guess (the only same-named method would bind either way).
    let target: Vec<String> = call_edges_with_confidence(&db)
        .into_iter()
        .filter(|e| e.contains("-> env.hpp.EnvWrapper.target "))
        .collect();
    assert_eq!(target.len(), 1, "{target:?}");
    assert!(
        !target[0].contains(r#""amb""#) && target[0].ends_with(" inferred"),
        "{target:?}"
    );
}

/// The header changes what `current()` returns; the `.cc` caller is untouched.
#[test]
fn test_cpp_return_type_change_re_resolves_untouched_chain_callers() {
    let other = VERSION_HPP.replace(
        "Version* current() const { return current_; }",
        "Other* current() const { return nullptr; }",
    );
    assert_incremental_matches_rebuild(
        &[("version.hpp", VERSION_HPP), ("get.cc", GET_CC)],
        &[("version.hpp", Some(&other))],
    );
    // Only a field in the middle of `r->index_block.Add()` changes type; the
    // call's edge into `BlockBuilder::Add` would be restored as it was.
    let retyped = VERSION_HPP
        .replace("BlockBuilder index_block;", "Other index_block;")
        .replace(
            "class Other {\n public:\n  void Ref() {}\n};",
            "class Other {\n public:\n  void Ref() {}\n  void Add(int k) {}\n};",
        );
    assert_incremental_matches_rebuild(
        &[("version.hpp", VERSION_HPP), ("get.cc", GET_CC)],
        &[("version.hpp", Some(&retyped))],
    );
}

/// Re-indexing a base's file restores the edges into it by name: a subclass's
/// `inherits` edge must come back to the class, not also to its same-named
/// constructors (leveldb: editing env.h left 24 such edges a rebuild lacks).
#[test]
fn test_restored_inherits_edge_skips_the_constructor() {
    let base = "class WritableFile {\n public:\n  WritableFile() = default;\n  \
                WritableFile(const WritableFile&) = delete;\n  virtual void Close() = 0;\n};\n";
    let sub = "#include \"env.hpp\"\nclass StringSink : public WritableFile {\n public:\n  \
               void Close() override {}\n};\n";
    let (project, _d, db) = fresh_index_of(&[("env.hpp", base), ("sink.cc", sub)]);
    let edited = format!("{base}// touched\n");
    fs::write(project.path().join("env.hpp"), &edited).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, control) = fresh_index_of(&[("env.hpp", &edited), ("sink.cc", sub)]);
    assert_eq!(edge_set(&db), edge_set(&control));
    assert!(
        edge_set(&db)
            .contains(&"sink.cc.StringSink --inherits--> env.hpp.WritableFile".to_string()),
        "{:#?}",
        edge_set(&db)
    );
}

/// A class that does not define the method runs what it inherits, or, called
/// through a pointer, an override: `db->Get()` on a `DB` whose `Get` is pure
/// virtual (no node) binds `DBImpl::Get`, not the caller's own `DBTest::Get`.
#[test]
fn test_typed_call_on_a_class_without_the_method_binds_overrides() {
    let (_p, _d, db) = fresh_index_of(&[(
        "db.cc",
        "class DB {\n public:\n  virtual void Get() = 0;\n};\n\
         class DBImpl : public DB {\n public:\n  void Get() override {}\n};\n\
         class DBTest {\n public:\n  void Get() {}\n  void Check(DB* db) { db->Get(); }\n};\n",
    )]);
    assert_eq!(
        callees_of(&db, "Check"),
        vec!["db.cc.DBImpl.Get".to_string()]
    );
}

const BASE_PY: &str = "class Base:\n    def helper(self):\n        pass\n";
const SUB_PY: &str =
    "from base import Base\n\n\nclass Sub(Base):\n    def run(self):\n        self.helper()\n";
const OTHER_PY: &str = "class Other:\n    def helper(self):\n        pass\n";

/// `self.helper()` in a class that inherits `helper` binds the base's, decided,
/// not every `helper` in reach.
#[test]
fn test_typed_call_binds_the_inherited_method() {
    let (_p, _d, db) = fresh_index_of(&[
        ("base.py", BASE_PY),
        ("sub.py", SUB_PY),
        ("other.py", OTHER_PY),
    ]);
    let edges: Vec<String> = call_edges_with_confidence(&db)
        .into_iter()
        .filter(|e| e.starts_with("sub.py.run"))
        .collect();
    assert_eq!(
        edges,
        vec![r#"sub.py.run -> base.py.Base.helper {"q":"rtype","v":"Sub"} inferred"#.to_string()]
    );
}

/// Two bases both defining the method: either may be the one that runs, so
/// both are candidates and neither is decided (`amb`; same-file edges keep the
/// `extracted` tier).
#[test]
fn test_typed_call_inheriting_the_method_from_two_bases_is_ambiguous() {
    let (_p, _d, db) = fresh_index_of(&[(
        "m.py",
        "class A:\n    def helper(self):\n        pass\n\n\
         class B:\n    def helper(self):\n        pass\n\n\
         class C(A, B):\n    pass\n\n\
         def run():\n    c = C()\n    c.helper()\n",
    )]);
    let edges: Vec<String> = call_edges_with_confidence(&db)
        .into_iter()
        .filter(|e| e.starts_with("m.py.run") && e.contains(".helper "))
        .collect();
    assert_eq!(
        edges,
        vec![
            r#"m.py.run -> m.py.A.helper {"amb":1,"q":"rtype","v":"C"} extracted"#.to_string(),
            r#"m.py.run -> m.py.B.helper {"amb":1,"q":"rtype","v":"C"} extracted"#.to_string(),
        ]
    );
}

/// What a class inherits moves with its bases: a base gaining or losing the
/// method, or its file going away, re-resolves the untouched subclass's calls.
#[test]
fn test_inherited_method_change_re_resolves_untouched_subclass_callers() {
    let bare = "class Base:\n    pass\n";
    let files = [("sub.py", SUB_PY), ("other.py", OTHER_PY)];
    assert_incremental_matches_rebuild(
        &[("base.py", bare), files[0], files[1]],
        &[("base.py", Some(BASE_PY))],
    );
    assert_incremental_matches_rebuild(
        &[("base.py", BASE_PY), files[0], files[1]],
        &[("base.py", Some(bare))],
    );
    assert_incremental_matches_rebuild(
        &[("base.py", BASE_PY), files[0], files[1]],
        &[("base.py", None)],
    );
    // `helper` moves from a free function into the base: its count is unchanged
    // (no D#24 round) and the caller's edge went to `Other` (nothing to restore).
    assert_incremental_matches_rebuild(
        &[
            (
                "base.py",
                "class Base:\n    pass\n\n\ndef helper():\n    pass\n",
            ),
            files[0],
            files[1],
        ],
        &[("base.py", Some(BASE_PY))],
    );
    // The middle class drops its mixin: the call bound `Mixin.helper` in a file
    // the run never opens.
    let mixin = (
        "mixin.py",
        "class Mixin:\n    def helper(self):\n        pass\n",
    );
    assert_incremental_matches_rebuild(
        &[
            (
                "base.py",
                "from mixin import Mixin\n\n\nclass Base(Mixin):\n    pass\n",
            ),
            mixin,
            files[0],
            files[1],
        ],
        &[("base.py", Some("class Base:\n    pass\n"))],
    );
    // The middle class is renamed: `Sub(Base)` loses its base without its file
    // changing, and its call bound `Mixin.helper`.
    assert_incremental_matches_rebuild(
        &[
            (
                "base.py",
                "from mixin import Mixin\n\n\nclass Base(Mixin):\n    pass\n",
            ),
            mixin,
            files[0],
            files[1],
        ],
        &[(
            "base.py",
            Some("from mixin import Mixin\n\n\nclass Base2(Mixin):\n    pass\n"),
        )],
    );
    // Two levels down: `Leaf(Sub)` calls what `Sub` inherits.
    let leaf =
        "from sub import Sub\n\n\nclass Leaf(Sub):\n    def go(self):\n        self.helper()\n";
    assert_incremental_matches_rebuild(
        &[("base.py", bare), files[0], files[1], ("leaf.py", leaf)],
        &[("base.py", Some(BASE_PY))],
    );
}

/// The header changes the field's type; the `.cc` caller is untouched.
#[test]
fn test_cpp_field_type_change_re_resolves_untouched_callers() {
    let other = DB_IMPL_HPP
        .replace("SnapshotList snapshots_", "Other snapshots_")
        .replace(
            "class DBImpl {",
            "class Other {\n public:\n  void Delete(int s) {}\n};\nclass DBImpl {",
        );
    assert_incremental_matches_rebuild(
        &[("db_impl.hpp", DB_IMPL_HPP), ("db_impl.cc", DB_IMPL_CC)],
        &[("db_impl.hpp", Some(&other))],
    );
    // A field that appears later: the call was an untyped member call.
    let without = DB_IMPL_HPP.replace("  SnapshotList snapshots_ GUARDED_BY(mutex_);\n", "");
    assert_incremental_matches_rebuild(
        &[("db_impl.hpp", &without), ("db_impl.cc", DB_IMPL_CC)],
        &[("db_impl.hpp", Some(DB_IMPL_HPP))],
    );
}

/// A typed call its class did not decide (two same-named bases) is as much a
/// guess as an untyped one: `ambiguous`, not the `inferred` a typed bind earns.
#[test]
fn test_undecided_typed_call_is_classified_ambiguous() {
    let (_p, _d, db) = fresh_index_of(&[
        (
            "a.py",
            "class Base:\n    def __init__(self):\n        pass\n",
        ),
        (
            "b.py",
            "class Base:\n    def __init__(self):\n        pass\n",
        ),
        (
            "c.py",
            "class Sub(Base):\n    def __init__(self):\n        super().__init__()\n",
        ),
    ]);
    let edges = call_edges_with_confidence(&db);
    let sub: Vec<_> = edges
        .iter()
        .filter(|e| e.starts_with("c.py.__init__"))
        .collect();
    assert_eq!(sub.len(), 2, "{edges:#?}");
    assert!(
        sub.iter()
            .all(|e| e.ends_with(" ambiguous") && e.contains(r#""amb":1"#)),
        "{sub:#?}"
    );
}

/// Pre-ship review of D#89: receivers the typing claimed for the wrong class.
#[test]
fn test_typed_receiver_does_not_claim_a_look_alike_class() {
    let files: &[(&str, &str)] = &[
        // An unrelated class named like a subclass is no override.
        (
            "base.py",
            "class Base:\n    def run(self):\n        self.step()\n\n    def step(self):\n        pass\n",
        ),
        ("impl1.py", "from base import Base\n\nclass Impl(Base):\n    def step(self):\n        pass\n"),
        ("impl2.py", "class Impl:\n    def step(self):\n        pass\n"),
        // A library class named like a project class is not it.
        ("req.ts", "export class Request { json() {} }\nexport class Other { json() {} }\n"),
        ("h.ts", "import { Request } from 'express';\nfunction handler(req: Request) { req.json(); }\n"),
        ("mu.h", "class mutex { public: void lock() {} };\n"),
        ("u.cc", "void f() { std::mutex m; m.lock(); }\n"),
        // A nested class's method defined in another file is not the top-level
        // class's.
        ("iter.h", "class Iterator {\n public:\n  virtual void Next() = 0;\n};\n"),
        ("skiplist.h", "class SkipList {\n public:\n  class Iterator {\n   public:\n    void Next();\n  };\n};\n"),
        ("skiplist.cc", "void SkipList::Iterator::Next() {}\n"),
        ("user.cc", "void drain(Iterator* it) { it->Next(); }\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = call_edges_with_confidence(&db);
    let from =
        |caller: &str| -> Vec<&String> { edges.iter().filter(|e| e.starts_with(caller)).collect() };
    let run: Vec<_> = from("base.py.run ->");
    assert!(
        run.iter().any(|e| e.contains("impl1.py.Impl.step")),
        "the override is bound: {run:?}"
    );
    assert!(
        !run.iter().any(|e| e.contains("impl2.py")),
        "an unrelated Impl is no override: {run:?}"
    );
    for caller in ["h.ts.handler ->", "u.cc.f ->", "user.cc.drain ->"] {
        let typed: Vec<_> = from(caller)
            .into_iter()
            .filter(|e| e.contains(r#""q":"rtype""#) && !e.contains(r#""amb":1"#))
            .collect();
        assert!(
            typed.is_empty(),
            "{caller} claimed a look-alike class: {typed:?}"
        );
    }
    assert!(
        from("u.cc.f ->").is_empty(),
        "std::mutex binds nothing: {edges:#?}"
    );
}

/// D#90: a nested function its factory returns in an object literal
/// (`return { attemptUpgrade }`) is a member of what the factory returns, so a
/// member call on that object reaches it; a nested helper nobody exposes stays
/// out of reach, as does a top-level function.
#[test]
fn test_member_call_reaches_a_function_its_factory_returns() {
    let files: &[(&str, &str)] = &[
        (
            "stub.js",
            "function makeStub() {\n  function attemptUpgrade() {}\n  const reset = () => 1;\n  \
             function hidden() {}\n  function renamed() {}\n  \
             return { attemptUpgrade, reset: reset, again: renamed };\n}\n\
             module.exports = { makeStub };\n",
        ),
        (
            "use.js",
            "const { makeStub } = require('./stub');\n\
             function go() { const stub = makeStub(); stub.attemptUpgrade(); stub.reset(); \
             stub.hidden(); stub.renamed(); }\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    assert!(
        has("use.js.go --calls--> stub.js.attemptUpgrade"),
        "{edges:#?}"
    );
    assert!(has("use.js.go --calls--> stub.js.reset"), "{edges:#?}");
    // Not returned, or returned under another member name.
    assert!(!has("use.js.go --calls--> stub.js.hidden"), "{edges:#?}");
    assert!(!has("use.js.go --calls--> stub.js.renamed"), "{edges:#?}");
}

/// D#71: a Rust call's syntax fixes whether its callee takes `self`. A bare
/// `f()` never runs a method (`drop(guard)` bound the project's own
/// `impl Drop::drop` 27 times in this repo), and `x.f()` only runs a method
/// that takes `self` (`status.success()` bound `JsonRpcResponse::success(id, v)`,
/// `.spawn()` a test's `McpClient::spawn(root)`). Same-file (batch) and
/// cross-file (deferred) resolution both apply it.
#[test]
fn test_rust_call_shape_decides_whether_the_callee_takes_self() {
    let lib = "pub struct Guard;\nimpl Drop for Guard {\n    fn drop(&mut self) {}\n}\n\
               impl Guard {\n    pub fn seal(&mut self) {}\n}\n\
               pub struct Resp;\nimpl Resp {\n    pub fn success(v: i32) -> Self { Resp }\n    \
               pub fn status(&self) -> i32 { 0 }\n}\n\
               pub struct Client;\nimpl Client {\n    pub fn spawn(root: &str) -> Self { Client }\n}\n\
               pub fn release(g: Guard) { drop(g); }\n\
               pub fn check(o: &std::process::Output) -> bool { o.status.success() }\n\
               pub fn build(c: &mut std::process::Command) { c.arg(\"x\").spawn(); }\n\
               pub fn ask(r: &Resp) -> i32 { r.status() }\n\
               pub fn nested(h: &Holder) -> i32 { h.resp.status() }\n\
               pub fn make() -> Resp { Resp::success(1) }\n\
               pub struct Holder { pub resp: Resp }\n";
    let other = "pub fn release_elsewhere(g: crate::Guard) { seal(g); }\n\
                 pub fn check_elsewhere(o: &std::process::Output) -> bool { o.status.success() }\n\
                 pub fn spawn_elsewhere() { std::process::Command::new(\"x\").spawn(); }\n\
                 pub fn ask_elsewhere(h: &crate::Holder) -> i32 { h.resp.status() }\n";
    let (_p, _d, db) = fresh_index_of(&[("lib.rs", lib), ("other.rs", other)]);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    let bound: Vec<&str> = [
        "lib.rs.release --calls--> lib.rs.drop",
        "lib.rs.check --calls--> lib.rs.success",
        "lib.rs.build --calls--> lib.rs.spawn",
        "other.rs.release_elsewhere --calls--> lib.rs.seal",
        "other.rs.check_elsewhere --calls--> lib.rs.success",
        "other.rs.spawn_elsewhere --calls--> lib.rs.spawn",
    ]
    .into_iter()
    .filter(|e| has(e))
    .collect();
    let lost: Vec<&str> = [
        "lib.rs.ask --calls--> lib.rs.status",
        "lib.rs.nested --calls--> lib.rs.status",
        "lib.rs.make --calls--> lib.rs.success",
        "other.rs.ask_elsewhere --calls--> lib.rs.status",
    ]
    .into_iter()
    .filter(|e| !has(e))
    .collect();
    assert!(
        bound.is_empty() && lost.is_empty(),
        "call shape ignored: {bound:#?}\nlost: {lost:#?}\n{edges:#?}"
    );
}

/// The same rule on the incremental paths: a bare call buffered because nothing
/// matched must not bind a method a later run adds (pending sweep), and an edge
/// into a re-indexed file must not be restored onto a same-named method that a
/// fresh index would never bind (Phase 2c restore).
#[test]
fn test_rust_call_shape_holds_on_incremental_paths() {
    let (project, _d, db) = fresh_index_of(&[
        (
            "a.rs",
            "pub fn release(g: G) { seal(g); }\npub fn go() { helper(); }\n",
        ),
        ("b.rs", "pub fn helper() {}\n"),
    ]);
    assert!(
        edge_set(&db)
            .iter()
            .any(|e| e == "a.rs.go --calls--> b.rs.helper"),
        "control: {:#?}",
        edge_set(&db)
    );
    fs::write(
        project.path().join("g.rs"),
        "pub struct G;\nimpl G {\n    pub fn seal(&mut self) {}\n}\n",
    )
    .unwrap();
    fs::write(
        project.path().join("b.rs"),
        "pub struct H;\nimpl H {\n    pub fn helper(&self) {}\n}\n",
    )
    .unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let edges = edge_set(&db);
    let bound: Vec<&str> = [
        "a.rs.release --calls--> g.rs.seal",
        "a.rs.go --calls--> b.rs.helper",
    ]
    .into_iter()
    .filter(|w| edges.iter().any(|e| e == w))
    .collect();
    assert!(bound.is_empty(), "{bound:#?}\n{edges:#?}");
}

/// D#112: Rust has no overloading, default or variadic parameters, so a call
/// passing N arguments reaches only a function taking N (a method: N besides
/// `self`; a path call `T::f(x, a)` passes `self` itself). An atomic's
/// `.load(Ordering::Acquire)` bound the project's
/// `ProjectClassNames::load(&mut self, db, candidates)` 11 times in this repo,
/// and `super::resolve::member_call_candidates(a, b, c)` its same-named method
/// twin, which takes four. Same-file, cross-file and incremental paths all
/// apply it.
#[test]
fn test_rust_call_arity_decides_which_function_a_call_reaches() {
    let lib = "pub struct Classes;\nimpl Classes {\n    \
               pub fn load(&mut self, db: i32, candidates: &[i64]) {}\n}\n\
               pub fn wait(flag: &std::sync::atomic::AtomicBool) -> bool {\n    \
               flag.load(std::sync::atomic::Ordering::Acquire)\n}\n\
               pub fn fill(c: &mut Classes) { c.load(1, &[]); }\n\
               pub fn ufcs(c: &mut Classes) { Classes::load(c, 1, &[]); }\n";
    let other = "pub fn watch(flag: &std::sync::atomic::AtomicBool) -> bool {\n    \
                 flag.load(std::sync::atomic::Ordering::Relaxed)\n}\n\
                 pub fn refill(c: &mut crate::lib::Classes) { c.load(2, &[]); }\n";
    let resolve = "pub fn pick(m: Option<&str>, c: Vec<i64>, db: i32) {}\n\
                   pub struct Names;\nimpl Names {\n    \
                   pub fn pick(&mut self, m: Option<&str>, c: Vec<i64>, db: i32) {}\n}\n";
    let index = "pub fn choose() { super::resolve::pick(None, vec![], 1); }\n";
    let (project, _d, db) = fresh_index_of(&[
        ("lib.rs", lib),
        ("other.rs", other),
        ("pipeline/resolve.rs", resolve),
        ("pipeline/index.rs", index),
    ]);
    // Targets by qualified name: the method twin and the free `pick` share a name.
    let calls = |db: &Database| -> Vec<String> {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT fs.path || '.' || ns.name || ' --calls--> ' \
                     || ft.path || '.' || COALESCE(nt.qualified_name, nt.name) \
                 FROM edges e \
                 JOIN nodes ns ON ns.id = e.source_id JOIN files fs ON fs.id = ns.file_id \
                 JOIN nodes nt ON nt.id = e.target_id JOIN files ft ON ft.id = nt.file_id \
                 WHERE e.relation = 'calls' ORDER BY 1",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.filter_map(Result::ok).collect()
    };
    let check = |edges: &[String], when: &str| {
        let has = |e: &str| edges.iter().any(|x| x == e);
        let bound: Vec<&str> = [
            "lib.rs.wait --calls--> lib.rs.Classes.load",
            "other.rs.watch --calls--> lib.rs.Classes.load",
            // The method twin takes `self` besides the three arguments.
            "pipeline/index.rs.choose --calls--> pipeline/resolve.rs.Names.pick",
        ]
        .into_iter()
        .filter(|e| has(e))
        .collect();
        let lost: Vec<&str> = [
            "lib.rs.fill --calls--> lib.rs.Classes.load",
            "lib.rs.ufcs --calls--> lib.rs.Classes.load",
            "other.rs.refill --calls--> lib.rs.Classes.load",
            "pipeline/index.rs.choose --calls--> pipeline/resolve.rs.pick",
        ]
        .into_iter()
        .filter(|e| !has(e))
        .collect();
        assert!(
            bound.is_empty() && lost.is_empty(),
            "{when}: arity ignored: {bound:#?}\nlost: {lost:#?}\n{edges:#?}"
        );
    };
    check(&calls(&db), "full index");
    // Re-index lib.rs: other.rs's edges into it are restored (Phase 2c), and
    // other.rs's calls resolve again through the deferred pass.
    fs::write(project.path().join("lib.rs"), format!("{lib}\n")).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    check(&calls(&db), "after lib.rs changed");
    fs::write(project.path().join("other.rs"), format!("{other}\n")).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    check(&calls(&db), "after other.rs changed");
    // Pending sweep: calls buffered because nothing matched bind only a method
    // of their arity when a later run adds one.
    fs::write(
        project.path().join("early.rs"),
        "pub fn early(x: &X) { x.inner.lonely(1); x.inner.alone(1); }\n",
    )
    .unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    fs::write(
        project.path().join("late.rs"),
        "pub struct Late;\nimpl Late {\n    pub fn lonely(&self, a: i32, b: i32) {}\n    \
         pub fn alone(&self, a: i32) {}\n}\n",
    )
    .unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let edges = calls(&db);
    assert!(
        !edges
            .iter()
            .any(|e| e == "early.rs.early --calls--> late.rs.Late.lonely")
            && edges
                .iter()
                .any(|e| e == "early.rs.early --calls--> late.rs.Late.alone"),
        "pending sweep: {edges:#?}"
    );
}

/// Pre-release review F3: a path through a crate or module (`tokio::spawn(f)`)
/// names no type, so it cannot pass `self` — only `Type::f(x)` calls a method
/// that way. Once the arity rule left `Command::spawn(&mut self)` as the only
/// one-parameter `spawn`, 137 tokio test calls bound it at `inferred`.
#[test]
fn test_rust_module_path_call_never_reaches_a_method() {
    let files: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod process;\npub mod rt;\npub use rt::spawn;\n",
        ),
        (
            "src/process.rs",
            "pub struct Command;\nimpl Command {\n    pub fn spawn(&mut self) {}\n}\n",
        ),
        ("src/rt.rs", "pub fn spawn<F>(f: F, id: u64) {}\n"),
        (
            "tests/t.rs",
            "fn t() {\n    mycrate::spawn(async {});\n    mycrate::rt::spawn(1, 2);\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let spawn_edges: Vec<&String> = edges
        .iter()
        .filter(|e| e.starts_with("tests/t.rs.t --calls-->"))
        .collect();
    // `rt::spawn(1, 2)` reaches the free function (control); the one-argument
    // module call reaches nothing.
    assert_eq!(
        spawn_edges,
        vec!["tests/t.rs.t --calls--> src/rt.rs.spawn"],
        "{edges:#?}"
    );
}

/// Pre-release review F4: `self.f()` in a trait's default method is a method
/// call. Outside an `impl` it was downgraded to a bare call, which the D#71
/// rule then let bind only functions NOT taking `self` — the correct
/// `Greeter::label` was dropped for another file's free `label()`.
#[test]
fn test_rust_self_call_in_a_trait_default_method_binds_the_method() {
    let files: &[(&str, &str)] = &[
        (
            "greet.rs",
            "pub trait Greeter {\n    fn label(&self) -> String { String::new() }\n    \
             fn greet(&self) -> String { self.label() }\n}\n",
        ),
        ("free.rs", "pub fn label() -> String { String::new() }\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let from_greet: Vec<&String> = edges
        .iter()
        .filter(|e| e.starts_with("greet.rs.greet --calls-->"))
        .collect();
    assert_eq!(
        from_greet,
        vec!["greet.rs.greet --calls--> greet.rs.label"],
        "{edges:#?}"
    );
}

/// D#126: the module-path rule above read a lowercase last segment as a module,
/// but a primitive (`u32`) and a `#[allow(non_camel_case_types)]` struct are
/// types, and `Type::f(&x)` passes `self` through them. 0.159.0 bound both
/// calls; 0.160.0 dropped them.
#[test]
fn test_rust_ufcs_through_a_lowercase_type_reaches_the_method() {
    let files: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod codec;\npub mod db;\npub mod rt;\n"),
        (
            "src/codec.rs",
            "pub trait Encode {\n    fn encode_to(&self, buf: &mut Vec<u8>);\n}\n\
             impl Encode for u32 {\n    fn encode_to(&self, buf: &mut Vec<u8>) {}\n}\n",
        ),
        (
            "src/db.rs",
            "#[allow(non_camel_case_types)]\npub struct sqlite3_db;\n\
             impl sqlite3_db {\n    pub fn close_db(&mut self) {}\n}\n",
        ),
        (
            "src/rt.rs",
            "pub struct Handle;\nimpl Handle {\n    pub fn close_db(&mut self) {}\n}\n",
        ),
        (
            "src/use_it.rs",
            "use crate::codec::Encode;\nuse crate::db::sqlite3_db;\n\
             fn t(v: u32, d: &mut sqlite3_db, b: &mut Vec<u8>) {\n    \
             u32::encode_to(&v, b);\n    sqlite3_db::close_db(d);\n    rt::close_db(d);\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let calls: Vec<&String> = edges
        .iter()
        .filter(|e| e.starts_with("src/use_it.rs.t --calls-->"))
        .collect();
    // The two UFCS calls reach their methods; `rt::close_db(d)` names a module,
    // so it still reaches no method (control for the rule D#126 narrows).
    assert!(
        calls.contains(&&"src/use_it.rs.t --calls--> src/codec.rs.encode_to".to_string()),
        "{edges:#?}"
    );
    assert!(
        calls.contains(&&"src/use_it.rs.t --calls--> src/db.rs.close_db".to_string()),
        "{edges:#?}"
    );
    assert!(
        !calls.iter().any(|e| e.ends_with("src/rt.rs.close_db")),
        "{edges:#?}"
    );
}

/// D#119: a bare `f()` or a module path `m::f()` never reaches a function of an
/// `impl` or `trait` block, whether or not it takes `self` — only `Type::f()`,
/// `Self::f()` or `x.f()` do. On tokio-1.41.1, 92 bare `spawn(fut)` calls bound
/// `Handle::spawn(me: &Arc<Self>, ..)`, whose first parameter is not `self`.
/// A turbofish or a qualified self no longer hides the path (`Block::<u8>::new`
/// was a bare `new`), and a fn nested in a method is still a free function.
#[test]
fn test_rust_bare_and_module_calls_never_reach_an_associated_fn() {
    let files: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod block;\npub mod rt;\npub mod other;\npub mod io;\npub mod tr;\n",
        ),
        (
            "src/rt.rs",
            "pub struct Handle;\nimpl Handle {\n    pub fn spawn(me: u8) {}\n}\n",
        ),
        (
            "src/block.rs",
            "pub struct Block<T>(T);\nimpl<T> Block<T> {\n    pub fn new(x: T) {}\n}\n",
        ),
        ("src/other.rs", "pub fn new(x: u8) {}\npub fn genf<T>() {}\n"),
        (
            "src/io.rs",
            "pub struct Interest;\nimpl Interest {\n    pub fn to_mio(&self) {\n        \
             fn mio_add(a: u8) {}\n        mio_add(1);\n    }\n}\n",
        ),
        (
            "src/tr.rs",
            "pub trait Tr {\n    fn helper() -> u8 { 0 }\n    fn go(&self, a: i32) { Self::helper(); }\n}\n\
             pub struct S;\nimpl Tr for S {\n    fn go(&self, a: i32) {}\n}\n",
        ),
        (
            "src/use_it.rs",
            "use crate::rt::Handle;\nuse crate::block::Block;\nuse crate::tr::{S, Tr};\n\
             fn bare() {\n    spawn(1);\n}\n\
             fn module() {\n    rt::spawn(1);\n}\n\
             fn typed() {\n    Handle::spawn(1);\n}\n\
             fn turbofish() {\n    Block::<u8>::new(0);\n}\n\
             fn qself(s: S) {\n    <S as Tr>::go(&s, 1);\n}\n\
             fn generic() {\n    other::genf::<u8>();\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let from = |caller: &str| -> Vec<String> {
        let prefix = format!("{caller} --calls--> ");
        edges
            .iter()
            .filter_map(|e| e.strip_prefix(&prefix).map(str::to_string))
            .collect()
    };
    assert_eq!(
        from("src/use_it.rs.bare"),
        Vec::<String>::new(),
        "{edges:#?}"
    );
    assert_eq!(
        from("src/use_it.rs.module"),
        Vec::<String>::new(),
        "{edges:#?}"
    );
    assert_eq!(
        from("src/use_it.rs.typed"),
        vec!["src/rt.rs.spawn"],
        "{edges:#?}"
    );
    assert_eq!(
        from("src/use_it.rs.turbofish"),
        vec!["src/block.rs.new"],
        "{edges:#?}"
    );
    assert_eq!(
        from("src/use_it.rs.qself"),
        vec!["src/tr.rs.go"],
        "{edges:#?}"
    );
    assert_eq!(
        from("src/use_it.rs.generic"),
        vec!["src/other.rs.genf"],
        "{edges:#?}"
    );
    assert_eq!(
        from("src/io.rs.to_mio"),
        vec!["src/io.rs.mio_add"],
        "{edges:#?}"
    );
    assert_eq!(from("src/tr.rs.go"), vec!["src/tr.rs.helper"], "{edges:#?}");
}

/// D#119: an integration test, an example, a bench or another package is a
/// different crate, and reaches only `pub` items of a library: never a
/// `pub(crate)` one, never a private free function. tokio's `mpsc::channel(n)`
/// from `tests/` bound `chan.rs`'s `pub(crate) fn channel(semaphore)` beside the
/// public `bounded.rs` one. Inside the crate both stay reachable, and a trait
/// impl's method (written without `pub`) stays reachable from outside.
#[test]
fn test_rust_call_from_another_crate_reaches_only_pub_items() {
    let files: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod mpsc;\npub mod util;\npub mod tr;\n"),
        (
            "src/mpsc/mod.rs",
            "mod bounded;\nmod chan;\npub use bounded::channel;\n",
        ),
        (
            "src/mpsc/bounded.rs",
            "pub fn channel(buffer: usize) {\n    chan::channel(1);\n}\n",
        ),
        ("src/mpsc/chan.rs", "pub(crate) fn channel(semaphore: u8) {}\n"),
        ("src/util.rs", "fn helper(a: u8) {}\n"),
        (
            "src/tr.rs",
            "pub trait Tr {\n    fn go(&self);\n}\npub struct S;\nimpl Tr for S {\n    fn go(&self) {}\n}\n",
        ),
        (
            "tests/t.rs",
            "use mycrate::tr::Tr;\nfn t(s: mycrate::tr::S) {\n    mycrate::mpsc::channel(1);\n    \
             helper(1);\n    s.go();\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let from = |caller: &str| -> Vec<String> {
        let prefix = format!("{caller} --calls--> ");
        edges
            .iter()
            .filter_map(|e| e.strip_prefix(&prefix).map(str::to_string))
            .collect()
    };
    let t = from("tests/t.rs.t");
    assert!(
        t.contains(&"src/mpsc/bounded.rs.channel".to_string()),
        "{edges:#?}"
    );
    assert!(
        !t.contains(&"src/mpsc/chan.rs.channel".to_string()),
        "{edges:#?}"
    );
    assert!(!t.contains(&"src/util.rs.helper".to_string()), "{edges:#?}");
    assert!(t.contains(&"src/tr.rs.go".to_string()), "{edges:#?}");
    // In the crate, `pub(crate)` is reachable (control).
    assert_eq!(
        from("src/mpsc/bounded.rs.channel"),
        vec!["src/mpsc/chan.rs.channel"],
        "{edges:#?}"
    );
}

/// The restore path applies crate visibility as a fresh resolution does: when
/// a library function goes from `pub` to `pub(crate)`, the integration test's
/// edge to it is not carried over onto the new node (rebuild drops it too).
#[test]
fn test_rust_crate_visibility_holds_when_an_edge_is_restored() {
    let files = |vis: &str| -> Vec<(String, String)> {
        vec![
            (
                "Cargo.toml".into(),
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n".into(),
            ),
            ("src/lib.rs".into(), "pub mod chan;\n".into()),
            (
                "src/chan.rs".into(),
                format!("{vis} fn channel(s: u8) {{}}\n"),
            ),
            (
                "tests/t.rs".into(),
                "fn t() {\n    mycrate::chan::channel(1);\n}\n".into(),
            ),
        ]
    };
    fn borrow(v: &[(String, String)]) -> Vec<(&str, &str)> {
        v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
    }
    let before = files("pub");
    let (project, _d, db) = fresh_index_of(&borrow(&before));
    let edge = "tests/t.rs.t --calls--> src/chan.rs.channel".to_string();
    assert!(
        edge_set(&db).contains(&edge),
        "control: {:#?}",
        edge_set(&db)
    );
    let after = files("pub(crate)");
    fs::write(project.path().join("src/chan.rs"), &after[2].1).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, fresh) = fresh_index_of(&borrow(&after));
    assert!(!edge_set(&fresh).contains(&edge), "{:#?}", edge_set(&fresh));
    assert_eq!(edge_set(&db), edge_set(&fresh));
}

/// `module::Type::f()` names the module by its file and the type by the
/// method's owner: the path filter matched the whole chain against one or the
/// other, so `runtime::Builder::new()` and tokio's
/// `task::Notified::<T>::from_raw(ptr)` bound nothing.
#[test]
fn test_rust_module_then_type_path_reaches_the_method() {
    let files: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        ),
        ("src/lib.rs", "pub mod runtime;\npub mod other;\n"),
        (
            "src/runtime/mod.rs",
            "mod builder;\npub use builder::Builder;\n",
        ),
        (
            "src/runtime/builder.rs",
            "pub struct Builder;\nimpl Builder {\n    pub fn new() -> Builder { Builder }\n}\n",
        ),
        (
            "src/other.rs",
            "pub struct Builder;\nimpl Builder {\n    pub fn new() -> Builder { Builder }\n}\n",
        ),
        (
            "tests/t.rs",
            "fn t() {\n    mycrate::runtime::Builder::new();\n}\n\
             fn u() {\n    mycrate::runtime::Builder::<u8>::new();\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    for caller in ["tests/t.rs.t", "tests/t.rs.u"] {
        let prefix = format!("{caller} --calls--> ");
        let got: Vec<&str> = edges
            .iter()
            .filter_map(|e| e.strip_prefix(&prefix))
            .collect();
        assert_eq!(
            got,
            vec!["src/runtime/builder.rs.new"],
            "{caller}: {edges:#?}"
        );
    }
}

/// D#117: a Rust `x.f()` with no candidate yet was not buffered (only the
/// `{"q":"member"}` shape `x.inner.f()` was), so an incremental run never bound
/// a method a later file added, where a rebuild binds it.
#[test]
fn test_rust_receiver_call_binds_a_method_added_later() {
    let early = (
        "early.rs",
        "pub fn early(x: &X) {\n    x.lonely(1);\n    x.inner.alone(1);\n}\n",
    );
    let late = (
        "late.rs",
        "pub struct Late;\nimpl Late {\n    pub fn lonely(&self, a: i32) {}\n    \
         pub fn alone(&self, a: i32) {}\n}\n",
    );
    let (project, _d, db) = fresh_index_of(&[early]);
    fs::write(project.path().join(late.0), late.1).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, fresh) = fresh_index_of(&[early, late]);
    let edges = edge_set(&fresh);
    assert!(
        edges.contains(&"early.rs.early --calls--> late.rs.lonely".to_string()),
        "rebuild control: {edges:#?}"
    );
    assert_eq!(edge_set(&db), edges);
}

/// The buffered `x.f()` of D#117, when two methods of that name arrive in one
/// later run: a rebuild binds neither (the unique-method rule), and so must the
/// incremental run. The sweep binds both by name; the new duplicate definition
/// then re-resolves the caller (D#24's fan-out), which drops them.
#[test]
fn test_rust_buffered_receiver_call_binds_no_ambiguous_method() {
    let early = ("early.rs", "pub fn early(x: &X) {\n    x.lonely(1);\n}\n");
    let a = (
        "a.rs",
        "pub struct A;\nimpl A {\n    pub fn lonely(&self, a: i32) {}\n}\n",
    );
    // At different distances, so proximity alone would pick `a.rs`.
    let b = (
        "deep/er/b.rs",
        "pub struct B;\nimpl B {\n    pub fn lonely(&self, a: i32) {}\n}\n",
    );
    let (project, _d, db) = fresh_index_of(&[early]);
    fs::write(project.path().join(a.0), a.1).unwrap();
    fs::create_dir_all(project.path().join("deep/er")).unwrap();
    fs::write(project.path().join(b.0), b.1).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, fresh) = fresh_index_of(&[early, a, b]);
    assert!(
        !edge_set(&fresh)
            .iter()
            .any(|e| e.starts_with("early.rs.early --calls-->")),
        "rebuild control: {:#?}",
        edge_set(&fresh)
    );
    assert_eq!(edge_set(&db), edge_set(&fresh));
}

/// D#124 F9: `use crate::a::widget` with `widget` defined only in `c.rs` binds
/// `c.rs` by name; when `a.rs` later gains `widget`, a rebuild binds the named
/// module's item and an incremental run kept the `c.rs` edges (D#45 fixed only
/// the other direction).
#[test]
fn test_rust_use_rebinds_when_the_named_module_gains_the_item() {
    let files = |a: &'static str| -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
            ),
            ("src/lib.rs", "mod a;\nmod c;\nmod user;\n"),
            ("src/a.rs", a),
            ("src/c.rs", "pub fn widget() {}\n"),
            (
                "src/user.rs",
                "use crate::a::widget;\nfn go() {\n    widget();\n}\n",
            ),
        ]
    };
    let before = "pub fn other() {}\n";
    let after = "pub fn other() {}\npub fn widget() {}\n";
    let (project, _d, db) = fresh_index_of(&files(before));
    fs::write(project.path().join("src/a.rs"), after).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, fresh) = fresh_index_of(&files(after));
    let want = edge_set(&fresh);
    assert!(
        want.contains(&"src/user.rs.go --calls--> src/a.rs.widget".to_string()),
        "rebuild control: {want:#?}"
    );
    assert_eq!(edge_set(&db), want);
}

/// Review of D#119 F2: `module::Type::f()` split into a file part and an owner
/// part bound `io::Error::new(..)` — std's, through `use std::io` — to the
/// project's own `io/error.rs` `Error::new`. A leading segment that names a std
/// module is not split; a project module name still is.
#[test]
fn test_rust_std_module_path_is_not_split_onto_a_project_type() {
    let files: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", "pub mod io;\npub mod net;\npub mod runtime;\n"),
        ("src/io/mod.rs", "mod error;\n"),
        (
            "src/io/error.rs",
            "pub struct Error;\nimpl Error {\n    pub fn new(k: u8, m: &str) -> Error { Error }\n}\n",
        ),
        ("src/runtime/mod.rs", "mod builder;\n"),
        (
            "src/runtime/builder.rs",
            "pub struct Builder;\nimpl Builder {\n    pub fn build(a: u8, b: u8) {}\n}\n",
        ),
        (
            "src/net/mod.rs",
            "use std::io;\nfn uses_std_io() {\n    io::Error::new(io::ErrorKind::Other, \"x\");\n}\n\
             fn uses_own() {\n    runtime::Builder::build(1, 2);\n}\n",
        ),
        (
            "src/fmt.rs",
            "pub struct Error;\nimpl Error {\n    pub fn new(k: u8, m: &str) -> Error { Error }\n}\n",
        ),
        (
            "src/other.rs",
            "fn full_std() {\n    std::fmt::Error::new(1, \"x\");\n}\n\
             fn full_core() {\n    core::fmt::Error::new(1, \"x\");\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    assert!(
        !edges
            .iter()
            .any(|e| e.starts_with("src/net/mod.rs.uses_std_io --calls-->")),
        "{edges:#?}"
    );
    // Written out in full (review round 2): `[std, fmt]` matched `src/fmt.rs`
    // through the split's file arm.
    for caller in ["src/other.rs.full_std", "src/other.rs.full_core"] {
        assert!(
            !edges
                .iter()
                .any(|e| e.starts_with(&format!("{caller} --calls-->"))),
            "{caller}: {edges:#?}"
        );
    }
    assert!(
        edges.contains(
            &"src/net/mod.rs.uses_own --calls--> src/runtime/builder.rs.build".to_string()
        ),
        "control: {edges:#?}"
    );
}

/// D#71 / D#45: a Rust `use` names the module its item lives in, and resolving
/// the import by the item's name alone bound every same-named item in the
/// crate. `use crate::storage::queries::helpers::test_db` in graph/routes.rs
/// bound three `test_db`s in graph/ (the closest paths) and not the imported
/// one; a rebuild bound `use crate::a::widget` to a same-named `c::widget`
/// too. The module path now picks the file; a path that names no item there
/// (a re-export) falls back to the name, as before.
#[test]
fn test_rust_use_binds_the_item_its_module_path_names() {
    let files: &[(&str, &str)] = &[
        ("src/a.rs", "pub fn widget() {}\n"),
        ("src/c.rs", "pub fn widget() {}\n"),
        (
            "src/b.rs",
            "use crate::a::widget;\npub fn f() {\n    widget();\n}\n",
        ),
        ("src/storage/queries/helpers.rs", "pub fn test_db() {}\n"),
        ("src/graph/query.rs", "pub fn test_db() {}\n"),
        (
            "src/graph/routes.rs",
            "#[cfg(test)]\nmod tests {\n    use crate::storage::queries::helpers::test_db;\n    \
             fn t() {\n        test_db();\n    }\n}\n",
        ),
        ("src/x/mod.rs", "pub fn helper() {}\n"),
        ("src/x/w.rs", "pub fn helper() {}\n"),
        (
            "src/x/y.rs",
            "use super::helper;\npub fn g() {\n    helper();\n}\n",
        ),
        ("src/re.rs", "pub use crate::a::widget;\n"),
        (
            "src/d.rs",
            "use crate::re::widget;\npub fn h() {\n    widget();\n}\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    let bound: Vec<&str> = [
        "src/b.rs.f --calls--> src/c.rs.widget",
        "src/b.rs.<module> --imports--> src/c.rs.widget",
        "src/graph/routes.rs.t --calls--> src/graph/query.rs.test_db",
        "src/x/y.rs.g --calls--> src/x/w.rs.helper",
    ]
    .into_iter()
    .filter(|e| has(e))
    .collect();
    let lost: Vec<&str> = [
        "src/b.rs.f --calls--> src/a.rs.widget",
        "src/b.rs.<module> --imports--> src/a.rs.widget",
        "src/graph/routes.rs.t --calls--> src/storage/queries/helpers.rs.test_db",
        "src/x/y.rs.g --calls--> src/x/mod.rs.helper",
    ]
    .into_iter()
    .filter(|e| !has(e))
    .collect();
    assert!(
        bound.is_empty() && lost.is_empty(),
        "bound past the module path: {bound:#?}\nlost: {lost:#?}\n{edges:#?}"
    );
    // A re-export names no item in its module: the name still resolves.
    assert!(
        edges
            .iter()
            .any(|e| e.starts_with("src/d.rs.h --calls--> ") && e.ends_with(".widget")),
        "{edges:#?}"
    );
}

/// D#45's incremental half: adding a same-named item elsewhere leaves the
/// imported one bound, and the graph equals a fresh index of the same tree.
#[test]
fn test_rust_use_binding_survives_a_new_same_named_item() {
    let a = ("src/a.rs", "pub fn widget() {}\n");
    let b = (
        "src/b.rs",
        "use crate::a::widget;\npub fn f() {\n    widget();\n}\n",
    );
    let c = ("src/c.rs", "pub fn widget() {}\n");
    let (project, _d, db) = fresh_index_of(&[a, b]);
    fs::write(project.path().join(c.0), c.1).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, fresh) = fresh_index_of(&[a, b, c]);
    assert_eq!(edge_set(&db), edge_set(&fresh));
    assert!(
        !edge_set(&fresh)
            .iter()
            .any(|e| e == "src/b.rs.f --calls--> src/c.rs.widget"),
        "{:#?}",
        edge_set(&fresh)
    );
    // Re-parsing the importer alone resolves its `use` against the whole tree.
    let b2 = (
        b.0,
        "use crate::a::widget;\n\npub fn f() {\n    widget();\n}\n",
    );
    fs::write(project.path().join(b2.0), b2.1).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p3, _d3, fresh2) = fresh_index_of(&[a, b2, c]);
    assert_eq!(edge_set(&db), edge_set(&fresh2));
}

/// The same exclusion on the pending-call sweep: a member call buffered because
/// no candidate existed yet must not bind a free function that a LATER run adds.
#[test]
fn test_pending_member_call_never_binds_a_later_free_function() {
    let (project, _d, db) = fresh_index_of(&[(
        "a.js",
        "function f(v) { return JSON.stringify(v); }\nfunction g() { return helper(); }\n",
    )]);
    fs::write(
        project.path().join("util.js"),
        "function stringify(v) { return v; }\nfunction helper() {}\nmodule.exports = { stringify, helper };\n",
    )
    .unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let edges = edge_set(&db);
    assert!(
        edges
            .iter()
            .any(|e| e == "a.js.g --calls--> util.js.helper"),
        "bare call control: {edges:#?}"
    );
    assert!(
        !edges
            .iter()
            .any(|e| e == "a.js.f --calls--> util.js.stringify"),
        "{edges:#?}"
    );
}

/// A bare call inside a C++ member function finds its own class's member first
/// (implicit `this`), and a gtest `TEST_F(DBTest, X)` body is a member of a class
/// derived from `DBTest`. It used to bind every same-file method of that name:
/// leveldb's db_test.cc also defines `ModelDB::Put` and `Handler::Put`, and 175 of
/// 180 wrong same-file bare-call edges there were test bodies reaching them.
#[test]
fn test_cpp_bare_call_in_a_member_prefers_its_own_class() {
    let cc = "struct ModelDB { void Put() {} };\nstruct DBTest { void Put() {} void Run(); };\n\
              void Free() {}\nTEST_F(DBTest, Recovery) { Put(); }\nTEST_F(DBTest, UsesFree) { Free(); }\n\
              void DBTest::Run() { Put(); }\nvoid Loose() { Put(); }\n\
              void DBTest::Forward() { ModelDB::Put(); }\n\
              struct Status { Status() {} static Status NotFound() { return Status(); } };\n";
    let (_p, _d, db) = fresh_index_of(&[("db_test.cc", cc)]);
    let qualified_targets = |caller: &str| -> Vec<String> {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT DISTINCT nt.qualified_name FROM edges e JOIN nodes ns ON ns.id = e.source_id \
                 JOIN nodes nt ON nt.id = e.target_id \
                 WHERE e.relation = 'calls' AND ns.name = ?1 ORDER BY 1",
            )
            .unwrap();
        stmt.query_map([caller], |r| r.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    };
    assert_eq!(qualified_targets("DBTest.Recovery"), vec!["DBTest.Put"]);
    assert_eq!(qualified_targets("Run"), vec!["DBTest.Put"]);
    assert_eq!(qualified_targets("DBTest.UsesFree"), vec!["Free"]);
    // A free function has no class: its bare call still reaches every candidate.
    assert_eq!(
        qualified_targets("Loose"),
        vec!["DBTest.Put", "ModelDB.Put"]
    );
    // A qualified call names its class: `ModelDB::Put()` is not an implicit `this`.
    assert!(qualified_targets("Forward").contains(&"ModelDB.Put".to_string()));
    // `Status()` inside a Status member is a constructor call: the class stays.
    assert!(qualified_targets("NotFound").contains(&"Status".to_string()));
}

const PIPE02_A_PY: &str = "def target():\n    return 1\n";
const PIPE02_B_PY: &str = "from a import target\n\n\ndef caller():\n    return target()\n";

/// PIPE-02 (audit 2026-08-29): deleting a file and restoring it must leave the
/// index where a rebuild of the same tree would.
///
/// Both states are asserted, because both were wrong and in OPPOSITE directions.
/// The recovery channel for a deleted file's inbound relations re-resolves them
/// by the TARGET NODE's name, and an import's identity is its SPECIFIER:
///   - vanished state: the requeue minted an `<external>` sentinel named after
///     the imported SYMBOL (`target`), where extraction mints one named after the
///     specifier (`a`), and dropped the module-level edge outright (its target
///     name is `<module>`, which resolves to nothing);
///   - restored state: nothing re-emitted b.py's imports, because only a file
///     whose CONTENT changed does — and the stale sentinel then satisfied
///     `prune_import_contradicted_call_edges`, deleting the call edge the pending
///     sweep had just recovered. Every later run replayed the prune.
///
/// A single terminal assert would pass on a fix that repointed the sentinel while
/// leaving the vanished state wrong, so the intermediate control is not optional.
#[test]
fn test_delete_then_restore_converges_with_fresh_rebuild() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let write_a = || fs::write(project_dir.path().join("a.py"), PIPE02_A_PY).unwrap();
    fs::write(project_dir.path().join("b.py"), PIPE02_B_PY).unwrap();
    write_a();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(
        edge_set(&db)
            .iter()
            .any(|e| e == "b.py.caller --calls--> a.py.target"),
        "precondition: the call edge exists before the delete"
    );

    // --- vanished state ---
    fs::remove_file(project_dir.path().join("a.py")).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    let (_pd, _dd, control_without_a) = fresh_index_of(&[("b.py", PIPE02_B_PY)]);
    assert_eq!(
        edge_set(&db),
        edge_set(&control_without_a),
        "after deleting a.py the edge set must equal a fresh index of the a-less tree"
    );

    // --- restored state ---
    write_a();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    let (_pd2, _dd2, control_with_a) =
        fresh_index_of(&[("a.py", PIPE02_A_PY), ("b.py", PIPE02_B_PY)]);
    assert_eq!(
        edge_set(&db),
        edge_set(&control_with_a),
        "after restoring a.py the edge set must equal a fresh rebuild of the same tree"
    );
    assert!(
        edge_set(&db)
            .iter()
            .any(|e| e == "b.py.caller --calls--> a.py.target"),
        "the call edge must be back — it is what impact/callgraph answer from"
    );
}

/// The other half of the same sweep: a file the index has NEVER seen, appearing
/// next to an importer that could not resolve it. No deletion is involved, so
/// nothing buffers or requeues anything — the only record that the specifier
/// failed to resolve is the `<external>` sentinel, and the sweep has to read it.
#[test]
fn test_newly_added_file_revives_its_importers_stale_sentinel() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    fs::write(project_dir.path().join("b.py"), PIPE02_B_PY).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(
        edge_set(&db)
            .iter()
            .any(|e| e.contains("--imports--> <external>.a")),
        "precondition: b.py's import of the missing module is an <external> sentinel"
    );

    fs::write(project_dir.path().join("a.py"), PIPE02_A_PY).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let (_pd, _dd, control) = fresh_index_of(&[("a.py", PIPE02_A_PY), ("b.py", PIPE02_B_PY)]);
    assert_eq!(
        edge_set(&db),
        edge_set(&control),
        "adding a.py must re-extract b.py, whose specifier now resolves"
    );
}

#[test]
fn test_sentinel_name_matches_stem_spellings() {
    // Every spelling a specifier reaches a file stem by must match...
    for spelling in [
        "util",
        "./util",
        "../lib/util",
        "@scope/util",
        "./util.js",
        "pkg.util",
        "a.b.util",
    ] {
        assert!(
            sentinel_name_matches_stem(spelling, "util"),
            "{spelling} names a file with stem `util`"
        );
    }
    // ...and unrelated names must not, or every added file would drag every
    // unresolved importer in the project into its run.
    for spelling in ["utils", "util_helper", "./other", "pkg.other", ""] {
        assert!(
            !sentinel_name_matches_stem(spelling, "util"),
            "{spelling} does not name a file with stem `util`"
        );
    }
    assert!(
        !sentinel_name_matches_stem("util", ""),
        "an empty stem must never match — it would match everything"
    );
}

#[test]
fn test_module_stems_of_directory_indexes() {
    assert_eq!(module_stems_of("src/util.ts"), vec!["util".to_string()]);
    // A directory index is spoken as the DIRECTORY, so both names count.
    assert_eq!(
        module_stems_of("src/util/index.ts"),
        vec!["index".to_string(), "util".to_string()]
    );
    assert_eq!(
        module_stems_of("pkg/__init__.py"),
        vec!["__init__".to_string(), "pkg".to_string()]
    );
    assert_eq!(
        module_stems_of("src/foo/mod.rs"),
        vec!["mod".to_string(), "foo".to_string()]
    );
}

#[test]
fn test_deleted_file_cleanup() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    fs::remove_file(project_dir.path().join("a.ts")).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
    assert_eq!(foo.len(), 0);
}

#[test]
fn test_build_python_module_map() {
    let mut paths = HashSet::new();
    paths.insert("myapp/utils.py".into());
    paths.insert("myapp/__init__.py".into());
    paths.insert("src/myapp/models.py".into());

    let map = build_python_module_map(&paths);

    // Full dotted path
    assert!(map
        .get("myapp.utils")
        .unwrap()
        .contains(&"myapp/utils.py".to_string()));
    // NOT the bare suffix: `myapp/` carries `__init__.py`, so it is a package,
    // and `import utils` inside a package names the standard library — never
    // the sibling (PEP 328, absolute imports by default). This assertion used
    // to require the opposite, which is what let `import logging` bind to
    // `accelerate/logging.py` across every indexed Python project
    // (audit 2026-08-22 P2-4).
    assert!(
        !map.contains_key("utils"),
        "a module inside a package must not be reachable by its bare name: {:?}",
        map.get("utils")
    );
    // `src/myapp/` has no `__init__.py`, so it IS an import root and its own
    // children keep their bare names — the `src/` layout this map exists for.
    assert!(map
        .get("models")
        .unwrap()
        .contains(&"src/myapp/models.py".to_string()));
    // __init__.py maps to package
    assert!(map
        .get("myapp")
        .unwrap()
        .contains(&"myapp/__init__.py".to_string()));
    // Nested with src/ prefix
    assert!(map
        .get("myapp.models")
        .unwrap()
        .contains(&"src/myapp/models.py".to_string()));
}

#[test]
fn test_python_from_import_resolution() {
    // Test `from myapp.utils import helper` creates correct cross-file edge
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::create_dir_all(project_dir.path().join("myapp")).unwrap();
    fs::write(
        project_dir.path().join("myapp/utils.py"),
        "def helper():\n    return 42\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("myapp/main.py"),
        "from myapp.utils import helper\n\ndef main():\n    helper()\n",
    )
    .unwrap();

    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(result.edges_created > 0, "should create import edges");

    // Verify dependency: main.py -> utils.py
    let deps = get_import_tree(db.conn(), "myapp/main.py", "outgoing", 1).unwrap();
    assert!(
        deps.iter().any(|d| d.file_path == "myapp/utils.py"),
        "main.py should depend on utils.py, got: {:?}",
        deps.iter().map(|d| &d.file_path).collect::<Vec<_>>()
    );
}

#[test]
fn test_python_import_module_resolution() {
    // Test `import myutils` creates correct cross-file edge
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("myutils.py"),
        "def do_something():\n    pass\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("main.py"),
        "import myutils\n\ndef main():\n    myutils.do_something()\n",
    )
    .unwrap();

    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(result.edges_created > 0, "should create import edges");

    // Verify dependency: main.py -> myutils.py
    let deps = get_import_tree(db.conn(), "main.py", "outgoing", 1).unwrap();
    assert!(
        deps.iter().any(|d| d.file_path == "myutils.py"),
        "main.py should depend on myutils.py, got: {:?}",
        deps.iter().map(|d| &d.file_path).collect::<Vec<_>>()
    );
}

#[test]
fn test_python_external_import_creates_virtual_nodes() {
    // Test that external imports create virtual nodes in <external> file
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("app.py"),
        "import os\nfrom collections import OrderedDict\nfrom flask import Flask\n\ndef main():\n    pass\n",
    ).unwrap();

    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(result.files_indexed > 0, "should index the file");

    // Verify <external> file was created with virtual nodes
    let ext_nodes = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
    let ext_names: Vec<&str> = ext_nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        ext_names.contains(&"os"),
        "should have virtual node for 'os', got: {:?}",
        ext_names
    );
    assert!(
        ext_names.contains(&"collections"),
        "should have virtual node for 'collections', got: {:?}",
        ext_names
    );
    assert!(
        ext_names.contains(&"flask"),
        "should have virtual node for 'flask', got: {:?}",
        ext_names
    );

    // Verify dependency_graph shows <external> as a dependency
    let deps = get_import_tree(db.conn(), "app.py", "outgoing", 1).unwrap();
    assert!(
        deps.iter().any(|d| d.file_path == "<external>"),
        "app.py should show <external> dependency, got: {:?}",
        deps.iter().map(|d| &d.file_path).collect::<Vec<_>>()
    );
}

#[test]
fn test_python_mixed_internal_external_imports() {
    // Test project with both internal and external imports
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::create_dir_all(project_dir.path().join("myapp")).unwrap();
    fs::write(
        project_dir.path().join("myapp/utils.py"),
        "def helper():\n    return 42\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("myapp/main.py"),
        "import os\nfrom myapp.utils import helper\nfrom flask import Flask\n\ndef main():\n    helper()\n",
    ).unwrap();

    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(result.edges_created > 0);

    // Should have internal dependency
    let deps = get_import_tree(db.conn(), "myapp/main.py", "outgoing", 1).unwrap();
    let dep_files: Vec<&str> = deps.iter().map(|d| d.file_path.as_str()).collect();
    assert!(
        dep_files.contains(&"myapp/utils.py"),
        "should depend on internal utils.py, got: {:?}",
        dep_files
    );

    // Should also have external dependency
    assert!(
        dep_files.contains(&"<external>"),
        "should depend on <external>, got: {:?}",
        dep_files
    );
}

#[test]
fn test_index_stats_skipped_large_file() {
    // Verify that IndexResult.stats tracks files skipped due to size
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Create a normal file
    fs::write(project_dir.path().join("small.ts"), "function ok() {}").unwrap();

    // Create a file exceeding max_file_size() (1 MiB by default)
    let big_content = "a".repeat(11 * 1024 * 1024);
    fs::write(project_dir.path().join("huge.ts"), &big_content).unwrap();

    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(result.files_indexed, 1, "should index the small file");
    assert_eq!(
        result.stats.files_skipped_size, 1,
        "should track the large file skip"
    );
}

#[test]
fn test_query_time_refresh_does_not_rehash_an_oversize_file() {
    // P2 (2026-08-16 audit §四): `ensure_file_indexed` runs on the QUERY path —
    // every result set that mentions a file reaches it. It used to hash the file
    // before doing anything else, so a source over `max_file_size()` (1 MiB by
    // default: a minified bundle, a generated table) was re-read in full on every
    // `show`/`search`/`callgraph` that named it, to reach a pipeline that refuses
    // to parse it anyway.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    let big = project_dir.path().join("huge.ts");
    fs::write(&big, "a".repeat(2 * 1024 * 1024)).unwrap();
    fs::write(project_dir.path().join("small.ts"), "function ok() {}").unwrap();
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(result.stats.files_skipped_size, 1, "precondition: skipped");

    // Unchanged file: no work either way.
    assert!(!ensure_file_indexed(&db, project_dir.path(), "huge.ts", None).unwrap());

    // Change its CONTENT. Before the size gate this hashed 2 MiB, saw a mismatch
    // and ran the whole pipeline for a file that yields zero symbols; now the
    // stat-plus-lookup answers "nothing here to refresh" without a read.
    fs::write(&big, "b".repeat(2 * 1024 * 1024)).unwrap();
    assert!(
        !ensure_file_indexed(&db, project_dir.path(), "huge.ts", None).unwrap(),
        "a content change in a file the indexer will never parse must not \
         re-run the pipeline on the query path"
    );

    // Negative control — the gate must be SIZE-scoped, not a blanket opt-out.
    // Widening it to skip every file turns this red.
    fs::write(
        project_dir.path().join("small.ts"),
        "function ok() {}\nfunction added() {}",
    )
    .unwrap();
    assert!(
        ensure_file_indexed(&db, project_dir.path(), "small.ts", None).unwrap(),
        "an ordinary edited file must still refresh"
    );
}

#[test]
fn test_query_time_refresh_purges_symbols_of_a_file_that_grew_past_the_limit() {
    // The other half of the size gate: it must NOT fire while the DB still holds
    // symbols for the file. A file indexed under the limit that later grows past
    // it has stale nodes, and the refresh is what removes them — an unconditional
    // early return would leave them queryable forever.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    let path = project_dir.path().join("grower.ts");
    fs::write(&path, "export function willVanish() {}").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let before: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'willVanish'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, 1, "precondition: the symbol is indexed");

    // Grow past the limit, keeping the symbol's own text present in the file.
    let mut grown = String::from("export function willVanish() {}\n// ");
    grown.push_str(&"x".repeat(2 * 1024 * 1024));
    fs::write(&path, grown).unwrap();

    assert!(
        ensure_file_indexed(&db, project_dir.path(), "grower.ts", None).unwrap(),
        "a file that grew past the limit still has work to do: purge its symbols"
    );
    let after: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE name = 'willVanish'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        after, 0,
        "symbols of a now-oversize file must be purged, not stranded"
    );
}

#[test]
fn test_index_stats_skipped_parse_error() {
    // Verify that IndexResult.stats tracks files skipped due to parse errors
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Create a valid file
    fs::write(project_dir.path().join("good.ts"), "function ok() {}").unwrap();

    // Create a file with an unsupported extension that detect_language returns None for
    // (this is filtered by detect_language returning None, not a parse error)
    // Instead, we just verify the default stats are zero for parse errors
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(result.stats.files_skipped_parse, 0);
    assert_eq!(result.stats.files_skipped_read, 0);
    assert_eq!(result.stats.files_skipped_hash, 0);
}

/// Audit 2026-09-02 P2-1: a file `read_to_string` rejects (Latin-1 source — the
/// common shape in legacy C/C++/Java trees) returned `PreParseOutcome::Nothing`,
/// so NO `files` row was ever recorded. The parse-failure branch four lines
/// below already recorded a `SkippedFile` precisely so the file stops re-diffing
/// and its stale symbols go away; the read-failure branch did neither.
///
/// Two consequences, both asserted here: the file is listed as changed on every
/// single run forever, and symbols indexed while it was still UTF-8 outlive the
/// change that made it unreadable.
#[test]
fn a_non_utf8_file_is_recorded_once_instead_of_re_diffing_forever() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let legacy = project_dir.path().join("legacy.c");

    // Phase 1: valid UTF-8, indexed normally.
    fs::write(&legacy, "int caf\u{e9}_count(void) { return 1; }\n").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        get_nodes_by_name(db.conn(), "café_count").unwrap().len(),
        1,
        "precondition: the symbol is indexed while the file is still UTF-8"
    );

    // Phase 2: re-encoded to Latin-1 — same bytes semantically, invalid UTF-8.
    // 0xE9 is a lone continuation byte, exactly what `read_to_string` rejects.
    fs::write(&legacy, b"int caf\xE9_count(void) { return 1; }\n").unwrap();
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        result.stats.files_skipped_read, 1,
        "precondition: the read really did fail"
    );

    // The stale symbol must be gone: the file's current content has no symbols
    // this indexer can see, and answering queries with the old ones is wrong.
    assert_eq!(
        get_nodes_by_name(db.conn(), "café_count").unwrap().len(),
        0,
        "symbols from the pre-Latin-1 revision must be purged"
    );

    // And the identity must be on record, so the next run diffs it as unchanged
    // rather than re-reading and re-warning about it forever.
    let hashes = get_all_file_hashes(db.conn()).unwrap();
    let recorded = hashes
        .get("legacy.c")
        .expect("an unreadable-but-present file still needs a `files` row");
    assert_eq!(
        *recorded,
        crate::indexer::merkle::hash_file(&legacy).unwrap(),
        "the recorded hash must be the file's real content hash"
    );
}

/// Pre-tag review P2-2: the first cut of the branch above called `hash_file`,
/// an INDEPENDENT second open, so a transient I/O failure on the first read
/// (fd exhaustion under the rayon fan-out, EIO, a concurrent non-atomic writer)
/// would fail read 1, succeed read 2, and record a `files` row whose hash
/// matches disk — after the symbols had been purged. `compute_diff` then sees
/// the file as settled and never re-offers it, so the purge is permanent. Under
/// the old `Nothing` that case was self-healing.
///
/// Asserts the surviving contract: an unreadable file records NO identity and
/// keeps whatever the index already knows, so the next run tries again.
///
/// This is the TRANSIENT case, forced through a test seam: the read fails while
/// the file stays readable, so a second open — which is what the first cut did —
/// succeeds and hands back a hash of content nobody validated. `mode 000` cannot
/// stand in for it, because then BOTH opens fail and the fixed and broken
/// versions behave identically (measured: the mutation stayed green until this
/// seam existed).
#[test]
fn a_transient_read_failure_records_no_identity_and_keeps_its_symbols() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let src = project_dir.path().join("readable.ts");

    fs::write(&src, "export function keeper() { return 1; }\n").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        get_nodes_by_name(db.conn(), "keeper").unwrap().len(),
        1,
        "precondition: indexed while readable"
    );
    let hash_before = get_all_file_hashes(db.conn())
        .unwrap()
        .get("readable.ts")
        .cloned()
        .expect("precondition: a files row exists");

    // New content, so a run that wrongly re-reads and records would record a
    // DIFFERENT hash — otherwise the assertion below could not tell.
    fs::write(&src, "export function keeper() { return 2; }\n").unwrap();
    *super::index_files::FORCE_READ_FAILURE.lock().unwrap() = Some(src.clone());
    let result = run_full_index(&db, project_dir.path(), None, None);
    *super::index_files::FORCE_READ_FAILURE.lock().unwrap() = None;
    let result = result.unwrap();

    assert_eq!(
        result.stats.files_skipped_read, 1,
        "precondition: the seam really did fail the read"
    );
    assert_eq!(
        get_nodes_by_name(db.conn(), "keeper").unwrap().len(),
        1,
        "a transient read failure must NOT purge the file's symbols"
    );
    assert_eq!(
        get_all_file_hashes(db.conn()).unwrap().get("readable.ts"),
        Some(&hash_before),
        "no identity may be recorded for bytes we never held: recording the hash of a \
         SECOND read makes compute_diff treat the file as settled, so the purge above \
         would never be undone"
    );
}

/// The permanent-I/O-failure twin of the test above: nothing readable at all.
///
/// Unix-only: `mode 000` is how an unreadable file is produced here, and
/// Windows ACLs do not answer to `set_permissions`. The branch under test is
/// platform-independent, so covering it on one platform covers it.
#[cfg(unix)]
#[test]
fn an_unreadable_file_records_no_identity_and_keeps_its_symbols() {
    use std::os::unix::fs::PermissionsExt;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let src = project_dir.path().join("readable.ts");

    fs::write(&src, "export function keeper() { return 1; }\n").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        get_nodes_by_name(db.conn(), "keeper").unwrap().len(),
        1,
        "precondition: indexed while readable"
    );
    let hash_before = get_all_file_hashes(db.conn())
        .unwrap()
        .get("readable.ts")
        .cloned()
        .expect("precondition: a files row exists");

    // Change the content AND make it unreadable, so a run that wrongly records
    // an identity would record the NEW hash — distinguishable from the old one.
    fs::write(&src, "export function keeper() { return 2; }\n").unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
    // Capability probe rather than a uid check: root (and some CI containers)
    // read mode-000 files anyway, and a test that silently asserts nothing is
    // worse than one that says why it stopped.
    if fs::read(&src).is_ok() {
        fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
        eprintln!("skipping: this process can read a mode-000 file, so the branch is unreachable");
        return;
    }
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap(); // so TempDir can clean up

    assert_eq!(
        result.stats.files_skipped_read, 1,
        "precondition: the read really did fail"
    );
    assert_eq!(
        get_nodes_by_name(db.conn(), "keeper").unwrap().len(),
        1,
        "an unreadable file must NOT have its symbols purged — the failure is \
         environmental and the next run may well succeed"
    );
    assert_eq!(
        get_all_file_hashes(db.conn()).unwrap().get("readable.ts"),
        Some(&hash_before),
        "no identity may be recorded for bytes we never held; recording the \
         current hash would make compute_diff treat the file as settled forever"
    );
}

#[test]
fn test_index_stats_default() {
    // IndexStats should implement Default
    let stats = IndexStats::default();
    assert_eq!(stats.files_skipped_size, 0);
    assert_eq!(stats.files_skipped_parse, 0);
    assert_eq!(stats.files_skipped_read, 0);
    assert_eq!(stats.files_skipped_hash, 0);
    assert_eq!(stats.files_skipped_language, 0);
}

#[test]
fn test_python_external_survives_incremental_index() {
    // Test that <external> pseudo-file persists across incremental re-indexes
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("app.py"),
        "import os\n\ndef main():\n    pass\n",
    )
    .unwrap();

    // Full index → creates <external> with "os" node
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let ext_before = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
    assert!(
        !ext_before.is_empty(),
        "should have external nodes after full index"
    );

    // Modify file slightly
    fs::write(
        project_dir.path().join("app.py"),
        "import os\n\ndef main():\n    return 1\n",
    )
    .unwrap();

    // Incremental index → <external> should survive
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    let ext_after = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
    assert!(
        !ext_after.is_empty(),
        "external nodes should survive incremental index"
    );

    // Verify dependency still visible
    let deps = get_import_tree(db.conn(), "app.py", "outgoing", 1).unwrap();
    assert!(
        deps.iter().any(|d| d.file_path == "<external>"),
        "app.py should still show <external> dependency after incremental index"
    );
}

#[test]
fn test_repair_null_context_strings_drains_past_one_page() {
    // Audit 2026-09-07 CORE-17. `get_nodes_missing_context` caps its result at
    // 10,000 rows; `repair_null_context_strings` took ONE page and returned, and
    // `spawn_startup_repair` runs it exactly once per process. An index carrying
    // more than a page of NULL context strings therefore never finished
    // repairing — and the log line prints a count that is always <= the cap, so
    // nothing said it had been truncated. The contract asserted here is the one
    // the function's name claims, stated absolutely rather than in terms of the
    // page size: after a repair, no NULL context strings are left.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("a.ts"),
        "function alpha() { return 1; }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Hang synthetic nodes off the real file row, so the INNER JOIN on `files`
    // in `get_nodes_with_files_by_ids` finds them and they are genuinely
    // repairable. 10,500 is a hardcoded count deliberately larger than the
    // page, not one computed from it.
    let file_id: i64 = db
        .conn()
        .query_row("SELECT id FROM files LIMIT 1", [], |r| r.get(0))
        .unwrap();
    {
        // `savepoint` is the pipeline's own idiom; a bare `unchecked_transaction`
        // here would trip the hardening guard that scans this directory.
        let sp = db.savepoint("sp_seed_null_contexts").unwrap();
        {
            let mut stmt = db
                .conn()
                .prepare(
                    "INSERT INTO nodes (file_id, type, name, start_line, end_line, \
                     code_content, context_string) VALUES (?1, 'function', ?2, 1, 1, 'x', NULL)",
                )
                .unwrap();
            for i in 0..10_500 {
                stmt.execute((file_id, format!("synthetic_{i}"))).unwrap();
            }
        }
        sp.commit().unwrap();
    }

    let nulls_before: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE context_string IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        nulls_before > 10_000,
        "precondition: the fixture must exceed one page, got {nulls_before}"
    );

    let repaired = repair_null_context_strings(&db, None).unwrap();

    let nulls_after: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes WHERE context_string IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        nulls_after, 0,
        "one repair pass must drain every NULL context string, not just the first page \
         ({repaired} repaired, {nulls_after} still NULL)"
    );
}

#[test]
fn test_repair_null_context_strings() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Index a file so nodes get context strings
    fs::write(
        project_dir.path().join("a.ts"),
        r#"
function alpha() { return 1; }
function beta() { alpha(); }
"#,
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Verify context strings exist after index
    let alpha_nodes = get_nodes_by_name(db.conn(), "alpha").unwrap();
    assert_eq!(alpha_nodes.len(), 1);
    assert!(
        alpha_nodes[0].context_string.is_some(),
        "alpha should have context_string after index"
    );

    let beta_nodes = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert_eq!(beta_nodes.len(), 1);
    assert!(
        beta_nodes[0].context_string.is_some(),
        "beta should have context_string after index"
    );

    // Simulate Phase 3 failure: NULL out context_strings
    db.conn()
        .execute("UPDATE nodes SET context_string = NULL", [])
        .unwrap();

    // Verify they are now NULL
    let alpha_after_null = get_nodes_by_name(db.conn(), "alpha").unwrap();
    assert!(
        alpha_after_null[0].context_string.is_none(),
        "alpha context_string should be NULL after simulated failure"
    );

    // Run repair
    let repaired = repair_null_context_strings(&db, None).unwrap();
    assert!(repaired > 0, "should repair at least 1 node");

    // Verify context strings were restored
    let alpha_repaired = get_nodes_by_name(db.conn(), "alpha").unwrap();
    assert!(
        alpha_repaired[0].context_string.is_some(),
        "alpha should have context_string after repair"
    );

    let beta_repaired = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert!(
        beta_repaired[0].context_string.is_some(),
        "beta should have context_string after repair"
    );
}

#[test]
fn test_rust_implements_creates_sentinel_for_external_trait() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("main.rs"),
        r#"
use std::io::{self, Write};
use std::fmt;

struct MyWriter;

impl Write for MyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { Ok(buf.len()) }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl fmt::Display for MyWriter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MyWriter")
    }
}
"#,
    )
    .unwrap();

    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(result.files_indexed > 0);

    // Verify sentinel nodes created for external traits
    let ext_nodes = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
    let ext_names: Vec<&str> = ext_nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(
        ext_names.contains(&"Write"),
        "should have sentinel for Write, got: {:?}",
        ext_names
    );
    // fmt::Display keeps path prefix (as parsed by tree-sitter)
    assert!(
        ext_names.contains(&"fmt::Display"),
        "should have sentinel for fmt::Display, got: {:?}",
        ext_names
    );

    // Verify sentinel type is "trait"
    let write_node = ext_nodes.iter().find(|n| n.name == "Write").unwrap();
    assert_eq!(
        write_node.node_type, "trait",
        "sentinel should be type 'trait'"
    );

    // Verify implements edges exist: MyWriter → Write, MyWriter → Display
    let edges: Vec<(String, String)> = db
        .conn()
        .prepare(
            "SELECT ns.name, nt.name FROM edges e
         JOIN nodes ns ON ns.id = e.source_id
         JOIN nodes nt ON nt.id = e.target_id
         WHERE e.relation = 'implements'",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(
        edges.contains(&("MyWriter".into(), "Write".into())),
        "should have MyWriter→Write implements edge, got: {:?}",
        edges
    );
    assert!(
        edges.contains(&("MyWriter".into(), "fmt::Display".into())),
        "should have MyWriter→fmt::Display implements edge, got: {:?}",
        edges
    );
}

/// ensure_file_indexed must (a) be a no-op when on-disk hash matches the
/// stored hash, and (b) actually pick up post-edit content when it doesn't.
/// This is the contract the MCP `ensure_file_fresh_opt` wrapper relies on
/// to close the post-Edit→pre-incremental-index window.
#[test]
fn test_ensure_file_indexed_picks_up_post_edit_changes() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Initial state: file with `alpha`
    fs::write(project_dir.path().join("a.ts"), "function alpha() {}\n").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let names_before: Vec<String> = get_nodes_by_name(db.conn(), "alpha")
        .unwrap()
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(names_before, vec!["alpha".to_string()]);

    // No-op when hashes match
    let did = ensure_file_indexed(&db, project_dir.path(), "a.ts", None).unwrap();
    assert!(!did, "matching hash must be a no-op (got reindex)");

    // Edit on disk; old `alpha` removed, new `beta` added
    fs::write(project_dir.path().join("a.ts"), "function beta() {}\n").unwrap();
    let did2 = ensure_file_indexed(&db, project_dir.path(), "a.ts", None).unwrap();
    assert!(did2, "hash mismatch must trigger a reindex");

    // alpha gone, beta present — post-Edit query would now see fresh state
    assert!(
        get_nodes_by_name(db.conn(), "alpha").unwrap().is_empty(),
        "old alpha must be evicted by single-file reindex"
    );
    let beta = get_nodes_by_name(db.conn(), "beta").unwrap();
    assert_eq!(
        beta.len(),
        1,
        "new beta must appear after single-file reindex"
    );
    assert_eq!(beta[0].name, "beta");

    // Calling again with no on-disk change is a no-op
    let did3 = ensure_file_indexed(&db, project_dir.path(), "a.ts", None).unwrap();
    assert!(!did3, "second call with no edit must no-op");

    // Deleting the file from disk drops the row
    fs::remove_file(project_dir.path().join("a.ts")).unwrap();
    let did4 = ensure_file_indexed(&db, project_dir.path(), "a.ts", None).unwrap();
    assert!(did4, "missing file must trigger row cleanup");
    assert!(
        get_nodes_by_name(db.conn(), "beta").unwrap().is_empty(),
        "beta must be cascade-deleted with its file"
    );
}

/// Root-cause test for `feedback_incremental_edge_timing.md`: file B
/// (existing, unchanged) bare-name calls `foo()`. file A is added later
/// with `function foo() {}`. Phase 2 of B's first index pass dropped the
/// edge because `foo` was unresolvable; before this fix, A's later index
/// never re-resolved B's call → permanently missing edge in incremental
/// mode (only `rebuild-index` recovered it).
///
/// New behavior: B's drop becomes a `pending_unresolved_calls` row; A's
/// index pass sweeps pending and promotes the row into a real edge.
#[test]
fn test_pending_unresolved_call_resolves_when_callee_added_later() {
    use crate::storage::queries::{count_pending_unresolved_calls, get_node_ids_by_name};

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Step 1: B exists alone with bare-name call to foo (foo undefined).
    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { foo(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Phase 2 dropped the edge (no same-file/same-language target) and
    // buffered the row instead.
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        1,
        "B's call to undefined foo must land in pending_unresolved_calls"
    );

    let caller_b_id = get_node_ids_by_name(db.conn(), "caller_b")
        .unwrap()
        .into_iter()
        .next()
        .expect("caller_b must exist")
        .0;

    // Verify NO edge yet (foo doesn't exist in DB).
    let pre_edges = crate::storage::queries::get_edges_from(db.conn(), caller_b_id).unwrap();
    assert!(
        pre_edges.iter().all(|e| e.relation != REL_CALLS),
        "no calls edge should exist yet — foo is undefined"
    );

    // Step 2: A is added with foo(). Incremental index picks it up; the
    // pending sweep at end of index_files promotes B's buffered call into
    // a real edge.
    fs::write(
        project_dir.path().join("a.ts"),
        "export function foo() {}\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let foo_id = get_node_ids_by_name(db.conn(), "foo")
        .unwrap()
        .into_iter()
        .next()
        .expect("foo must exist after A indexed")
        .0;

    let post_edges = crate::storage::queries::get_edges_from(db.conn(), caller_b_id).unwrap();
    let calls_to_foo: Vec<_> = post_edges
        .iter()
        .filter(|e| e.relation == REL_CALLS && e.target_id == foo_id)
        .collect();
    assert_eq!(
        calls_to_foo.len(),
        1,
        "incremental index must promote pending call → calls edge caller_b → foo; \
         got edges: {:?}",
        post_edges
            .iter()
            .map(|e| (&e.relation, e.target_id))
            .collect::<Vec<_>>()
    );

    // Pending row must be drained after successful resolution.
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        0,
        "resolved pending row must be deleted after edge insertion"
    );
}

/// Bounded retention (SCHEMA v10, D#77): a pending row that fails to resolve
/// for `PENDING_CALL_MAX_ATTEMPTS` consecutive sweeps is evicted — ~99% of
/// buffered rows are never-resolvable external/builtin calls that otherwise
/// accumulate until the next INDEX_VERSION wipe. Below the threshold the
/// incremental-edge-timing guarantee is untouched (see the boundary test).
#[test]
fn test_pending_evicted_after_max_failed_sweeps() {
    use super::resolve::resolve_pending_calls;
    use crate::domain::PENDING_CALL_MAX_ATTEMPTS;
    use crate::storage::queries::count_pending_unresolved_calls;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { neverDefinedAnywhere(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(count_pending_unresolved_calls(db.conn()).unwrap(), 1);

    // The full index above already ran one sweep (attempts = 1). Sweep until
    // one shy of the threshold: the row must still be buffered.
    for _ in 0..(PENDING_CALL_MAX_ATTEMPTS - 2) {
        resolve_pending_calls(&db, &Default::default()).unwrap();
    }
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        1,
        "row must survive below PENDING_CALL_MAX_ATTEMPTS failed sweeps"
    );

    // The threshold-crossing sweep evicts it.
    resolve_pending_calls(&db, &Default::default()).unwrap();
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        0,
        "row must be evicted once it has failed PENDING_CALL_MAX_ATTEMPTS sweeps"
    );
}

/// Retention must count RESOLUTION OPPORTUNITIES, not wall-clock ticks.
///
/// A pending row can only become resolvable when a node appears, and nodes only
/// appear when a batch parses files. Aging on a batch that parsed nothing spends
/// the row's budget on passes it could never have survived differently: the
/// file-watcher and the periodic rescan fire on their own schedule, so a repo
/// with an unresolved forward reference burned attempts at the poll rate and
/// evicted the row before the callee was ever written. Once evicted, only a
/// re-index of the CALLER re-buffers it — the edge stays missing until then.
///
/// Measured on this repo at audit time: every buffered row sat at attempts = 4
/// after 26h and 4 scans, i.e. ~2 weeks to the 50-attempt ceiling on ambient
/// ticks alone.
#[test]
fn test_empty_incremental_tick_does_not_age_pending_rows() {
    use crate::domain::PENDING_CALL_MAX_ATTEMPTS;
    use crate::storage::queries::{count_pending_unresolved_calls, get_node_ids_by_name};

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { lateFoo(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(count_pending_unresolved_calls(db.conn()).unwrap(), 1);
    let attempts_after_index: i64 = db
        .conn()
        .query_row("SELECT attempts FROM pending_unresolved_calls", [], |r| {
            r.get(0)
        })
        .unwrap();

    // Ticks with an empty diff — the watcher/periodic-rescan shape.
    for _ in 0..(PENDING_CALL_MAX_ATTEMPTS + 5) {
        run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    }
    let attempts_now: i64 = db
        .conn()
        .query_row("SELECT attempts FROM pending_unresolved_calls", [], |r| {
            r.get(0)
        })
        .unwrap_or(-1);
    assert_eq!(
        attempts_now, attempts_after_index,
        "a batch that parsed nothing gave the row no chance to resolve, so it \
         must not consume an attempt (-1 = the row was evicted outright)"
    );

    // The point of not aging: the callee can still arrive and bind.
    fs::write(
        project_dir.path().join("a.ts"),
        "export function lateFoo() {}\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    let caller_id = get_node_ids_by_name(db.conn(), "caller_b")
        .unwrap()
        .into_iter()
        .next()
        .expect("caller_b must exist")
        .0;
    let foo_id = get_node_ids_by_name(db.conn(), "lateFoo")
        .unwrap()
        .into_iter()
        .next()
        .expect("lateFoo must exist")
        .0;
    let edges = crate::storage::queries::get_edges_from(db.conn(), caller_id).unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e.relation == REL_CALLS && e.target_id == foo_id),
        "the forward reference must still bridge after idle ticks"
    );
}

/// Boundary guard for the incremental-edge-timing guarantee under bounded
/// retention: a row aged to ONE sweep short of eviction must still bridge —
/// resolution in the same sweep wins over eviction (resolved rows are drained
/// before survivors age).
#[test]
fn test_pending_at_eviction_boundary_still_resolves() {
    use super::resolve::resolve_pending_calls;
    use crate::domain::PENDING_CALL_MAX_ATTEMPTS;
    use crate::storage::queries::{count_pending_unresolved_calls, get_node_ids_by_name};

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { lateFoo(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Age to the brink: attempts = MAX - 1 (full index swept once already).
    for _ in 0..(PENDING_CALL_MAX_ATTEMPTS - 2) {
        resolve_pending_calls(&db, &Default::default()).unwrap();
    }
    assert_eq!(count_pending_unresolved_calls(db.conn()).unwrap(), 1);

    // Callee arrives — the incremental pass's sweep must resolve, not evict.
    fs::write(
        project_dir.path().join("a.ts"),
        "export function lateFoo() {}\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let caller_id = get_node_ids_by_name(db.conn(), "caller_b")
        .unwrap()
        .into_iter()
        .next()
        .expect("caller_b must exist")
        .0;
    let foo_id = get_node_ids_by_name(db.conn(), "lateFoo")
        .unwrap()
        .into_iter()
        .next()
        .expect("lateFoo must exist")
        .0;
    let edges = crate::storage::queries::get_edges_from(db.conn(), caller_id).unwrap();
    assert!(
        edges
            .iter()
            .any(|e| e.relation == REL_CALLS && e.target_id == foo_id),
        "a row at the eviction boundary must still resolve when the callee arrives"
    );
    assert_eq!(count_pending_unresolved_calls(db.conn()).unwrap(), 0);
}

/// Cross-language pending must NOT resolve cross-language. If B (TS)
/// calls `update()` and a later-indexed Rust file defines `fn update()`,
/// the pending row must stay buffered, not silently bind cross-language
/// (memory `feedback_edge_resolution_same_language.md`'s canonical
/// false-positive class).
#[test]
fn test_pending_unresolved_call_does_not_cross_language() {
    use crate::storage::queries::count_pending_unresolved_calls;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // TS file with bare-name call to `update`
    fs::write(
        project_dir.path().join("client.ts"),
        "function caller_ts() { update(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(count_pending_unresolved_calls(db.conn()).unwrap(), 1);

    // Rust file with `update` — different language, must NOT match.
    fs::write(project_dir.path().join("hasher.rs"), "fn update() {}\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // Pending row stays — sweep refused cross-language resolution.
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        1,
        "cross-language target must NOT resolve a TS pending call to a Rust fn"
    );
}

/// One caller with N undefined references must produce N pending rows;
/// when a single later-added file defines all N, all rows must resolve in
/// a single sweep. Real codebases hit this whenever a "barrel" or shared
/// utility module gets added after its consumers.
#[test]
fn test_pending_resolves_multiple_calls_in_same_caller() {
    use crate::storage::queries::{count_pending_unresolved_calls, get_node_ids_by_name};

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // B has three undefined call targets — foo, bar, baz.
    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { foo(); bar(); baz(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        3,
        "three bare-name calls must produce three pending rows"
    );

    // A defines all three.
    fs::write(
        project_dir.path().join("a.ts"),
        "export function foo() {}\nexport function bar() {}\nexport function baz() {}\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        0,
        "all three pending rows must drain once their targets exist"
    );

    // All three resolved into real edges.
    let caller_b_id = get_node_ids_by_name(db.conn(), "caller_b")
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let edges = crate::storage::queries::get_edges_from(db.conn(), caller_b_id).unwrap();
    let calls_count = edges.iter().filter(|e| e.relation == REL_CALLS).count();
    assert_eq!(
        calls_count,
        3,
        "caller_b must have exactly three calls edges (foo, bar, baz); got {} edges total: {:?}",
        calls_count,
        edges
            .iter()
            .map(|e| (&e.relation, e.target_id))
            .collect::<Vec<_>>()
    );
}

/// When the caller's source file is reindexed (e.g. user edits B), the
/// cascade FK on pending_unresolved_calls(source_id) must drop B's pending
/// rows so a fresh Phase 2 can re-buffer them with the current source IDs.
/// This is the schema's load-bearing self-cleaning property — we test it
/// explicitly so a future migration that drops or weakens the FK fails
/// loudly here rather than leaking pending rows for ever-removed callers.
#[test]
fn test_pending_cascade_deletes_when_caller_file_reindexed() {
    use crate::storage::queries::count_pending_unresolved_calls;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // B with undefined target → pending row created.
    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { undefined_target(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(count_pending_unresolved_calls(db.conn()).unwrap(), 1);

    // Edit B to remove the call entirely. caller_b's old node gets
    // cascade-deleted on reindex (Phase 1 deletes prior rows), and its
    // pending row must follow it via ON DELETE CASCADE on source_id.
    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { /* call removed */ }\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        0,
        "pending row must be cascade-deleted when its source caller is removed/reindexed"
    );
}

/// Inverse-direction symmetry test for `feedback_incremental_edge_timing.md`:
/// existing edge B → A.foo gets cascade-deleted when A is removed, and B
/// is NOT in changed_paths (deletion doesn't re-extract B). Without Phase 0
/// pre-cascade buffering, B has neither edge nor pending row — a permanent
/// silent edge loss until full rebuild. The Phase 0 buffer (added by this
/// fix) must capture B's call as a pending row before cascade fires.
#[test]
fn test_pending_buffers_on_callee_file_deletion() {
    use crate::storage::queries::{count_pending_unresolved_calls, get_node_ids_by_name};

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Initial: A defines foo, B calls foo — edge B.caller_b → A.foo exists.
    fs::write(
        project_dir.path().join("a.ts"),
        "export function foo() {}\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("b.ts"),
        "function caller_b() { foo(); }\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // No pending rows yet — call resolved at index time.
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        0,
        "fully-resolvable call must not produce a pending row"
    );

    let caller_b_id = get_node_ids_by_name(db.conn(), "caller_b")
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let foo_id_pre = get_node_ids_by_name(db.conn(), "foo")
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let edges_pre = crate::storage::queries::get_edges_from(db.conn(), caller_b_id).unwrap();
    assert!(
        edges_pre
            .iter()
            .any(|e| e.relation == REL_CALLS && e.target_id == foo_id_pre),
        "edge caller_b → foo must exist pre-deletion"
    );

    // Delete A. Phase 0 must buffer B's now-orphaned call into pending
    // BEFORE cascade strips the edge.
    fs::remove_file(project_dir.path().join("a.ts")).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // foo is gone.
    assert!(
        get_node_ids_by_name(db.conn(), "foo").unwrap().is_empty(),
        "foo must be cascade-deleted with file a.ts"
    );

    // B's edge to old foo is gone, but pending row holds the call.
    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        1,
        "Phase 0 must buffer the orphaned inbound call into pending"
    );

    // Re-add A — pending sweep promotes the buffered call to a fresh edge.
    fs::write(
        project_dir.path().join("a.ts"),
        "export function foo() {}\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    assert_eq!(
        count_pending_unresolved_calls(db.conn()).unwrap(),
        0,
        "pending must drain once foo reappears"
    );

    let foo_id_post = get_node_ids_by_name(db.conn(), "foo")
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let edges_post = crate::storage::queries::get_edges_from(db.conn(), caller_b_id).unwrap();
    assert!(
        edges_post
            .iter()
            .any(|e| e.relation == REL_CALLS && e.target_id == foo_id_post),
        "edge caller_b → foo must reappear post re-add via pending sweep"
    );
}

#[test]
fn test_is_safe_relative_path() {
    // Safe: ordinary relative paths, `./` prefix, interior `..` that stays in-root.
    assert!(is_safe_relative_path("src/lib.rs"));
    assert!(is_safe_relative_path("a.ts"));
    assert!(is_safe_relative_path("./src/x.rs"));
    assert!(is_safe_relative_path("a/b/../c.rs")); // net depth stays >= 0
    assert!(is_safe_relative_path("")); // empty → downstream treats as no-op
                                        // Unsafe: absolute root, leading `..`, or a `..` that climbs above the root.
    assert!(!is_safe_relative_path("/etc/passwd"));
    assert!(!is_safe_relative_path("../outside.ts"));
    assert!(!is_safe_relative_path("../../etc/passwd"));
    assert!(!is_safe_relative_path("a/../../b.rs")); // dips below root mid-path
    #[cfg(windows)]
    assert!(!is_safe_relative_path(r"C:\windows\system32"));
}

/// Defense-in-depth: `ensure_file_indexed` must refuse to touch a file outside
/// the project root, whether reached by an absolute path or a `..`-escape. The
/// MCP freshness wrapper (`ensure_file_fresh_opt`) forwards the client's raw
/// `file_path` without `normalize_user_path`, so this leaf is what stops an
/// unnormalized path from hashing/indexing arbitrary files into the project DB.
/// Such a path is a no-op (`Ok(false)`), like other non-indexable inputs.
#[test]
fn test_ensure_file_indexed_rejects_out_of_root_path() {
    let base = TempDir::new().unwrap();
    let project_root = base.path().join("proj");
    fs::create_dir_all(&project_root).unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // A real source file OUTSIDE the project root (in base/), reachable via `..`.
    fs::write(base.path().join("outside.ts"), "function secret() {}\n").unwrap();
    // An absolute-path target outside the project entirely.
    let elsewhere = TempDir::new().unwrap();
    let abs_outside = elsewhere.path().join("abs_secret.ts");
    fs::write(&abs_outside, "function absSecret() {}\n").unwrap();

    // Establish the project index with one legitimate in-root file.
    fs::write(project_root.join("ok.ts"), "function inRoot() {}\n").unwrap();
    run_full_index(&db, &project_root, None, None).unwrap();

    // `..`-escape: project_root/../outside.ts resolves to base/outside.ts.
    let did = ensure_file_indexed(&db, &project_root, "../outside.ts", None).unwrap();
    assert!(!did, "a `..`-escaping path must be a no-op, not a reindex");

    // Absolute path outside the project.
    let did_abs =
        ensure_file_indexed(&db, &project_root, abs_outside.to_str().unwrap(), None).unwrap();
    assert!(!did_abs, "an absolute out-of-root path must be a no-op");

    // Neither external symbol leaked into the project DB.
    assert!(
        get_nodes_by_name(db.conn(), "secret").unwrap().is_empty(),
        "a `..`-escaping file must not be indexed into the project DB"
    );
    assert!(
        get_nodes_by_name(db.conn(), "absSecret")
            .unwrap()
            .is_empty(),
        "an absolute out-of-root file must not be indexed into the project DB"
    );

    // The guard must not over-block: a legitimate in-root edit still reindexes.
    fs::write(project_root.join("ok.ts"), "function inRootEdited() {}\n").unwrap();
    let did_ok = ensure_file_indexed(&db, &project_root, "ok.ts", None).unwrap();
    assert!(did_ok, "an in-root edited file must still reindex");
    assert_eq!(
        get_nodes_by_name(db.conn(), "inRootEdited").unwrap().len(),
        1
    );
}

/// D#240: a query-time refresh may index a path only under the key the
/// indexer's own scan would store for that file. The scan never follows a
/// symlink (`merkle::symlink_skip_candidate`) and stores each file under one
/// `/`-joined spelling of its on-disk names, but the refresh indexed ANY
/// string-safe key whose `is_file()` held. `./src/a.rs`, `src/./a.rs` and
/// `link/a.rs` (through `link -> src`) each added a second row for one file,
/// so every symbol in it existed twice and by-name lookups answered
/// "Ambiguous symbol"; `ext/secret.rs` (through `ext -> <outside dir>`)
/// indexed a file from outside the project. The next incremental scan
/// deletes such a row, and the next query naming the spelling adds it back.
#[test]
fn test_refresh_adds_no_row_for_a_key_the_scan_never_stores() {
    let base = TempDir::new().unwrap();
    let root = base.path().join("proj");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn d240_target() {}\n").unwrap();
    let outside = base.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.rs"), "pub fn d240_secret() {}\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(root.join("src"), root.join("link")).unwrap();
        symlink(&outside, root.join("ext")).unwrap();
        symlink(root.join("src/a.rs"), root.join("alias.rs")).unwrap();
        symlink(outside.join("secret.rs"), root.join("src/leak.rs")).unwrap();
    }
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, &root, None, None).unwrap();
    let keys = |db: &Database| {
        let mut keys: Vec<String> = get_all_file_hashes(db.conn())
            .unwrap()
            .into_keys()
            .collect();
        keys.sort();
        keys
    };
    let before = keys(&db);
    assert_eq!(before, ["src/a.rs"], "the scan stores one key");

    #[allow(unused_mut)]
    let mut refused = vec![
        "./src/a.rs",
        "src/./a.rs",
        "src/../src/a.rs",
        "src//a.rs",
        "src/a.rs/",
        // Other spellings of `src/a.rs` on a case-insensitive filesystem (the
        // macOS and Windows CI legs); on a case-sensitive one no such file
        // exists, so these cannot fail there.
        "SRC/a.rs",
        "src/A.rs",
    ];
    #[cfg(unix)]
    refused.extend(["link/a.rs", "ext/secret.rs", "alias.rs", "src/leak.rs"]);
    #[cfg(windows)]
    refused.push(r"src\a.rs");
    // Every spelling is tried before failing, so one run names them all.
    let mut indexed = Vec::new();
    for spelling in refused {
        let did = ensure_file_indexed(&db, &root, spelling, None).unwrap();
        let after = keys(&db);
        if did || after != before {
            indexed.push(format!("{spelling} (re-indexed: {did}, files: {after:?})"));
            apply_file_refreshes(&db, &root, &[spelling.to_string()], &[], None).unwrap();
        }
    }
    assert!(
        indexed.is_empty(),
        "indexed under a non-scan key: {indexed:#?}"
    );
    assert_eq!(
        get_nodes_by_name(db.conn(), "d240_target").unwrap().len(),
        1,
        "one node per symbol"
    );
    assert!(
        get_nodes_by_name(db.conn(), "d240_secret")
            .unwrap()
            .is_empty(),
        "a file outside the project must not be indexed"
    );

    // Not over-blocked: a file the scan would store, new since the index,
    // is still indexed on demand, under its own key.
    fs::create_dir_all(root.join("src/deep")).unwrap();
    fs::write(root.join("src/deep/new.rs"), "pub fn d240_new() {}\n").unwrap();
    assert!(ensure_file_indexed(&db, &root, "src/deep/new.rs", None).unwrap());
    assert_eq!(keys(&db), ["src/a.rs", "src/deep/new.rs"]);
    // And an edit to an indexed file still re-indexes it.
    fs::write(root.join("src/a.rs"), "pub fn d240_edited() {}\n").unwrap();
    assert!(ensure_file_indexed(&db, &root, "src/a.rs", None).unwrap());
    assert_eq!(
        get_nodes_by_name(db.conn(), "d240_edited").unwrap().len(),
        1
    );
}

/// D#262: the scan also stores no key for a file its walker filters out —
/// ignored by `.gitignore` (at any level), `.ignore` or `.git/info/exclude`,
/// inside a hidden directory, or under `node_modules` / `vendor` / `target` —
/// but the refresh indexed any such file a caller named. One `deps` on each
/// turned a unique name into "Ambiguous symbol … 4 matches" (0.166.0) until
/// the next incremental scan deleted the rows, and the next query re-added
/// them.
#[test]
fn test_refresh_adds_no_row_for_a_file_the_scan_filters_out() {
    let project = TempDir::new().unwrap();
    let root = project.path();
    // `.gitignore` applies inside a git repository only.
    fs::create_dir_all(root.join(".git/info")).unwrap();
    fs::write(root.join(".git/info/exclude"), "src/excluded.rs\n").unwrap();
    fs::write(root.join(".gitignore"), "src/gen.rs\nbuild/\n").unwrap();
    fs::write(root.join(".ignore"), "src/skip.rs\n").unwrap();
    fs::create_dir_all(root.join("src/sub")).unwrap();
    fs::write(root.join("src/sub/.gitignore"), "local.rs\n").unwrap();
    let decoy = "pub fn d262_x() {}\n";
    fs::write(root.join("src/a.rs"), decoy).unwrap();
    let filtered = [
        "src/gen.rs",
        "build/out.rs",
        "src/skip.rs",
        "src/excluded.rs",
        "src/sub/local.rs",
        ".hidden/x.rs",
        "node_modules/pkg/index.js",
        "vendor/v.rs",
        "target/debug/t.rs",
    ];
    for rel in filtered {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let src = if rel.ends_with(".js") {
            "export function d262_x() {}\n"
        } else {
            decoy
        };
        fs::write(path, src).unwrap();
    }
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, root, None, None).unwrap();
    let keys = |db: &Database| {
        let mut keys: Vec<String> = get_all_file_hashes(db.conn())
            .unwrap()
            .into_keys()
            .collect();
        keys.sort();
        keys
    };
    // The scan's own verdict, so the fixture cannot pass by filtering nothing.
    let before = keys(&db);
    assert_eq!(before, ["src/a.rs"], "the scan stores one key");

    let mut indexed = Vec::new();
    for rel in filtered {
        let did = ensure_file_indexed(&db, root, rel, None).unwrap();
        let after = keys(&db);
        if did || after != before {
            indexed.push(format!("{rel} (re-indexed: {did}, files: {after:?})"));
            apply_file_refreshes(&db, root, &[rel.to_string()], &[], None).unwrap();
        }
    }
    assert!(
        indexed.is_empty(),
        "indexed a file the scan filters out: {indexed:#?}"
    );
    assert_eq!(get_nodes_by_name(db.conn(), "d262_x").unwrap().len(), 1);

    // Not over-blocked: new files the scan would store, beside ignored ones
    // and under a directory with its own `.gitignore`, are still indexed.
    fs::write(root.join("src/b.rs"), "pub fn d262_b() {}\n").unwrap();
    fs::write(root.join("src/sub/kept.rs"), "pub fn d262_kept() {}\n").unwrap();
    assert!(ensure_file_indexed(&db, root, "src/b.rs", None).unwrap());
    assert!(ensure_file_indexed(&db, root, "src/sub/kept.rs", None).unwrap());
    assert_eq!(keys(&db), ["src/a.rs", "src/b.rs", "src/sub/kept.rs"]);
}

/// D#240, the other half: a row an older version created under such a key is
/// dropped when that key is refreshed again — what the next incremental scan
/// does to it anyway, since its walk never sees the path.
#[test]
fn test_refresh_drops_a_row_stored_under_a_key_the_scan_never_stores() {
    let project_dir = TempDir::new().unwrap();
    let root = project_dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn d240_target() {}\n").unwrap();
    #[allow(unused_mut)]
    let mut aliases = vec!["./src/a.rs", "src/./a.rs", "src/../src/a.rs", "src//a.rs"];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("src"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("src/a.rs"), root.join("alias.rs")).unwrap();
        aliases.extend(["link/a.rs", "alias.rs"]);
    }
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, root, None, None).unwrap();
    let hash = crate::indexer::merkle::hash_file(&root.join("src/a.rs")).unwrap();

    let mut kept = Vec::new();
    for alias in aliases {
        // The row 0.165.2 wrote for `get_ast_node {file_path: <alias>}`.
        apply_file_refreshes(&db, root, &[], &[(alias.to_string(), hash.clone())], None).unwrap();
        assert_eq!(
            get_nodes_by_name(db.conn(), "d240_target").unwrap().len(),
            2,
            "{alias}: fixture: the alias row duplicates the symbol"
        );
        let did = ensure_file_indexed(&db, root, alias, None).unwrap();
        let keys: Vec<String> = get_all_file_hashes(db.conn())
            .unwrap()
            .into_keys()
            .collect();
        if !did || keys != ["src/a.rs"] {
            kept.push(format!("{alias} (dropped: {did}, files: {keys:?})"));
            apply_file_refreshes(&db, root, &[alias.to_string()], &[], None).unwrap();
        }
    }
    assert!(kept.is_empty(), "alias rows kept: {kept:#?}");
    assert_eq!(
        get_nodes_by_name(db.conn(), "d240_target").unwrap().len(),
        1
    );
}

/// D#240: a row is created only for a key whose every name the directory
/// listing holds verbatim — what tells `SRC/a.rs` from `src/a.rs` on a
/// case-insensitive filesystem. A directory that can be entered but not
/// listed (mode 0311) is the Linux shape of the same disagreement: `lstat`
/// reaches the file, the scan cannot list the directory and stores nothing
/// in it, so neither may the refresh.
#[cfg(unix)]
#[test]
fn test_refresh_creates_no_row_in_a_directory_the_scan_cannot_list() {
    use std::os::unix::fs::PermissionsExt;
    let project_dir = TempDir::new().unwrap();
    let root = project_dir.path();
    fs::create_dir_all(root.join("src/hidden")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn d240_target() {}\n").unwrap();
    fs::write(root.join("src/hidden/x.rs"), "pub fn d240_unlisted() {}\n").unwrap();
    let hidden = root.join("src/hidden");
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o311)).unwrap();
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let _restore = Restore(hidden.clone());
    if fs::read_dir(&hidden).is_ok() {
        eprintln!("skipped: this user can list a mode-0311 directory (root?)");
        return;
    }
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, root, None, None).unwrap();
    assert!(
        root.join("src/hidden/x.rs").is_file(),
        "fixture: the file is reachable by path"
    );

    assert!(!ensure_file_indexed(&db, root, "src/hidden/x.rs", None).unwrap());
    assert!(get_nodes_by_name(db.conn(), "d240_unlisted")
        .unwrap()
        .is_empty());
}

#[test]
fn test_std_import_prunes_same_named_project_call_phantom() {
    // IDX v53 differential. `use std::mem::swap; swap(&mut a, &mut b)` used to
    // fabricate `calls → project::swap` (an unrelated helper that merely shares
    // the name), because the call is bare and the only same-language candidate
    // is the project's own. v52 stopped the phantom IMPORT edge by dropping std
    // uses entirely, but the CALL phantom survived — nothing recorded that this
    // file's `swap` refers to something outside the project.
    //
    // Binding the std use to the `<external>` sentinel gives the existing
    // `prune_import_contradicted_call_edges` the contradiction it needs: the
    // caller's file imports `swap` bound to a DIFFERENT node than the edge's
    // target, and does not import that target — so the phantom is removed.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();

    // The bait: an unrelated project symbol that happens to be called `swap`.
    fs::write(
        project_dir.path().join("src/util.rs"),
        "pub fn swap(v: &mut Vec<u8>) { v.reverse(); }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/caller.rs"),
        "use std::mem::swap;\n\
         pub fn reorder(a: &mut u8, b: &mut u8) {\n    swap(a, b);\n}\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let reorder = get_nodes_by_name(db.conn(), "reorder").unwrap();
    let reorder_id = reorder.first().expect("reorder must be indexed").id;
    let util_swap = get_nodes_by_name(db.conn(), "swap")
        .unwrap()
        .into_iter()
        .find(|n| {
            crate::storage::queries::get_file_path(db.conn(), n.file_id)
                .unwrap()
                .is_some_and(|p| p.ends_with("util.rs"))
        })
        .expect("the project's own `swap` must be indexed");

    let phantom = get_edges_from(db.conn(), reorder_id)
        .unwrap()
        .into_iter()
        .any(|e| e.relation == REL_CALLS && e.target_id == util_swap.id);
    assert!(
        !phantom,
        "`use std::mem::swap` then `swap(a, b)` must not resolve to the project's \
         unrelated `util.rs::swap` — the std import is what disambiguates it"
    );
    // The negative control lives in
    // `test_std_import_prune_does_not_eat_real_cross_file_calls`: an absence
    // assertion is satisfied just as well by a mechanism that deletes everything.
}

#[test]
fn test_std_import_prune_does_not_eat_real_cross_file_calls() {
    // Negative control for the test above: a genuine cross-file call to a
    // project function must survive, including in a file that also imports std.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();

    fs::write(
        project_dir.path().join("src/util.rs"),
        "pub fn tidy(v: &mut Vec<u8>) { v.sort(); }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/caller.rs"),
        "use std::mem::swap;\n\
         use crate::util::tidy;\n\
         pub fn run(v: &mut Vec<u8>) {\n    tidy(v);\n}\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let run_id = get_nodes_by_name(db.conn(), "run").unwrap()[0].id;
    let tidy_id = get_nodes_by_name(db.conn(), "tidy")
        .unwrap()
        .into_iter()
        .find(|n| {
            crate::storage::queries::get_file_path(db.conn(), n.file_id)
                .unwrap()
                .is_some_and(|p| p.ends_with("util.rs"))
        })
        .expect("tidy must be indexed")
        .id;

    let calls_tidy = get_edges_from(db.conn(), run_id)
        .unwrap()
        .into_iter()
        .any(|e| e.relation == REL_CALLS && e.target_id == tidy_id);
    assert!(
        calls_tidy,
        "a real cross-file call must survive the std-import external binding"
    );
}

#[test]
fn test_query_time_refresh_never_deletes_the_external_pseudo_file() {
    // `<external>` anchors the sentinel nodes that unresolved imports bind to.
    // It has no on-disk counterpart, so the query-time freshness resync
    // classified it as a DELETED file and dropped the row — CASCADE taking every
    // sentinel node and every edge into them. Any read command that displays or
    // resolves an external name reached it: `show HashMap` did it while printing
    // "Symbol not found", i.e. a read-only query that reported failure still
    // destroyed part of the index, and a later incremental pass did not restore
    // it (only a file whose CONTENT changed re-emits its import relations).
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    fs::write(
        project_dir.path().join("src/a.rs"),
        "use std::collections::HashMap;\npub fn run() {}\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let external_nodes = |db: &Database| -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM nodes n JOIN files f ON f.id = n.file_id WHERE f.path = ?1",
                [crate::domain::EXTERNAL_FILE_PATH],
                |r| r.get(0),
            )
            .unwrap()
    };

    let before = external_nodes(&db);
    assert!(
        before > 0,
        "fixture must produce sentinel nodes, or this test proves nothing"
    );
    let edges_before: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
        .unwrap();

    // The exact call a read command makes for a node whose file is `<external>`.
    let changed = ensure_file_indexed(
        &db,
        project_dir.path(),
        crate::domain::EXTERNAL_FILE_PATH,
        None,
    )
    .unwrap();
    assert!(
        !changed,
        "the pseudo-file has no content to refresh — reporting a change would \
         also make callers re-run their query for nothing"
    );

    assert_eq!(
        external_nodes(&db),
        before,
        "query-time refresh deleted the <external> pseudo-file and its sentinels"
    );
    let edges_after: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(edges_after, edges_before, "cascade took the import edges");

    // Negative control: a genuinely deleted REAL file must still be dropped —
    // that is the branch this guard sits in front of, and short-circuiting it
    // for everything would satisfy the assertions above.
    fs::remove_file(project_dir.path().join("src/a.rs")).unwrap();
    assert!(
        ensure_file_indexed(&db, project_dir.path(), "src/a.rs", None).unwrap(),
        "a real file that disappeared must still be pruned"
    );
}

#[test]
fn test_dead_code_ignore_prefixes_are_separator_normalized() {
    // The CLI half of the `ignore_paths` fix shipped with zero coverage: reverting
    // `ignore.iter().map(normalize_rel_str)` left the whole suite green. The
    // prefixes are matched with `starts_with` against `/`-stored paths, so a
    // Windows user's `--ignore src\generated` excludes nothing and the tool
    // OVER-reports dead code. Asserted at the query, because the CLI's own
    // normalization is a no-op on a Unix host by construction.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src/generated")).unwrap();
    fs::write(
        project_dir.path().join("src/generated/gen.rs"),
        "pub fn generated_orphan() { let _ = 1; let _ = 2; let _ = 3; }\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("src/real.rs"),
        "pub fn real_orphan() { let _ = 1; let _ = 2; let _ = 3; }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let names = |ignore: &[String]| -> Vec<String> {
        crate::storage::queries::dead_code_report(db.conn(), None, None, false, 1, ignore)
            .unwrap()
            .items
            .into_iter()
            .map(|r| r.name)
            .collect()
    };

    assert!(
        names(&[]).contains(&"generated_orphan".to_string()),
        "precondition: the generated orphan is reported when nothing is ignored"
    );

    // The `/` spelling is the stored one and must exclude.
    let unix = names(&["src/generated".to_string()]);
    assert!(
        !unix.contains(&"generated_orphan".to_string()),
        "got {unix:?}"
    );
    assert!(
        unix.contains(&"real_orphan".to_string()),
        "the ignore prefix must not swallow unrelated files: {unix:?}"
    );

    // A `\`-spelled prefix, once normalized the way the CLI/MCP entry points do,
    // must behave identically — that equality IS the contract.
    let normalized = crate::indexer::merkle::normalize_rel_str_on(r"src\generated", true);
    assert_eq!(normalized, "src/generated");
    assert_eq!(
        names(&[normalized]),
        unix,
        "a backslash-spelled ignore prefix must exclude exactly what the forward-slash one does"
    );
}

#[test]
fn test_index_files_normalizes_caller_path_order() {
    // Every caller builds its file list from HashMap iteration — `run_full_index`
    // from `scan_directory`'s map, both incremental entries from `compute_diff` —
    // so the order handed to `index_files` is arbitrary and varies run to run.
    // `index_files` sorts (and dedups) it, which is what makes the first-wins
    // bindings inside a batch reproducible. Node ids are minted in processing
    // order, so "processed sorted" is observable as ids ascending with the sorted
    // path even though this caller passes the reverse.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    for name in ["a.py", "b.py", "c.py"] {
        fs::write(project_dir.path().join(name), "def f():\n    pass\n").unwrap();
    }

    // Reverse order, plus a duplicate: a caller-side hash map can hand over
    // either, and neither may change what lands in the DB.
    let caller_order: Vec<String> =
        vec!["c.py".into(), "b.py".into(), "a.py".into(), "b.py".into()];
    let result = index_files(
        &db,
        project_dir.path(),
        &caller_order,
        &std::collections::HashMap::new(),
        None,
        &[],
        None,
    )
    .unwrap();

    assert_eq!(
        result.files_indexed, 3,
        "a duplicated path must be indexed once, not twice"
    );

    let first_id = |path: &str| {
        get_nodes_by_file_path(db.conn(), path)
            .unwrap()
            .iter()
            .map(|n| n.id)
            .min()
            .expect("indexed file must have nodes")
    };
    let (a, b, c) = (first_id("a.py"), first_id("b.py"), first_id("c.py"));
    assert!(
        a < b && b < c,
        "files must be processed in sorted order regardless of the caller's order; got a={a} b={b} c={c}"
    );
}

#[test]
fn test_external_sentinel_type_prefers_implements_over_import() {
    // One name can reach the `<external>` sentinel from both channels: an
    // unresolved `impl Write for …` (implements → `trait`) and an unresolved
    // `use std::io::Write` (imports → `module`). Sorted file order puts the
    // import LAST here, so a last-write-wins map would stamp the node `module`
    // and the sentinel's type would track file order rather than meaning.
    // Precedence is fixed instead: implements is the specific claim and wins.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    fs::write(
        project_dir.path().join("a_impl.rs"),
        "pub struct Sink;\n\nimpl Write for Sink {\n    fn go(&self) {}\n}\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("b_import.rs"),
        "use std::io::Write;\n\npub fn touch() {}\n",
    )
    .unwrap();

    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let ext = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
    let writes: Vec<&crate::storage::queries::NodeResult> =
        ext.iter().filter(|n| n.name == "Write").collect();
    assert_eq!(
        writes.len(),
        1,
        "both channels must share ONE sentinel node, got: {:?}",
        writes.iter().map(|n| &n.node_type).collect::<Vec<_>>()
    );
    assert_eq!(
        writes[0].node_type, "trait",
        "the implements channel must win over the later import channel"
    );
}

/// The frozen-mtime skip, asserted where a USER would notice it.
///
/// `merkle::test_scan_directory_cached_detects_content_change_under_frozen_mtime`
/// pins the same defect one layer down, at the scan. That is the right place for
/// the decision, but it cannot show the consequence: the scan returning a short
/// hash map is only a bug because `run_incremental_index_cached` then reports
/// zero files indexed and leaves the previous symbols in the database. Every MCP
/// tool reaches this function through `ensure_indexed` →
/// `run_incremental_with_cache_restore`, so a file stuck here is a file whose
/// stale symbols `callgraph` / `project_map` / `find_dead_code` keep serving.
///
/// The edit is length-changing, which is what the size half of `FileStamp`
/// catches; the mtime is restamped to the previous value byte-for-byte, which is
/// what a coarse-granularity filesystem does for free.
#[test]
fn test_cached_incremental_sees_a_content_edit_under_a_frozen_mtime() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    fs::create_dir_all(project_dir.path().join("src")).unwrap();
    let file = project_dir.path().join("src/a.ts");
    fs::write(&file, "function alpha(): number { return 1; }\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let (_first, cache) =
        run_incremental_index_cached(&db, project_dir.path(), None, None, None).unwrap();
    assert_eq!(
        get_nodes_by_name(db.conn(), "alpha").unwrap().len(),
        1,
        "precondition: the first pass must index the original symbol"
    );

    let frozen = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(
        &file,
        "function beta(): number { return 2; }\nfunction gamma(): number { return 3; }\n",
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(frozen)
        .unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().modified().unwrap(),
        frozen,
        "precondition: the restamp must actually freeze the mtime, else this test \
         passes for the wrong reason"
    );

    let (second, _cache2) =
        run_incremental_index_cached(&db, project_dir.path(), None, Some(&cache), None).unwrap();

    assert_eq!(
        second.files_indexed, 1,
        "the edited file was skipped, so the incremental pass was a no-op — mtime \
         equality was taken as proof of freshness"
    );
    assert_eq!(
        get_nodes_by_name(db.conn(), "beta").unwrap().len(),
        1,
        "the new symbol never reached the database"
    );
    assert!(
        get_nodes_by_name(db.conn(), "alpha").unwrap().is_empty(),
        "the deleted symbol is still being served — every MCP tool reads through \
         this path"
    );
}

// ---------------------------------------------------------------------------
// Cross-batch resolution (audit 2026-08-02 P0-1 / P1-2 / P1-9).
//
// The fixture shapes here are the measured P0 reproduction: the SAME four
// meaningful files must produce the SAME graph whether they share a batch or
// sit in different ones. A single-batch corpus is a null control for anything
// touching "which batch a file lands in" (feedback_edge_exclusion_verify_by_
// index_diff), so the multi-batch leg pads past BATCH_SIZE with filler files.
// ---------------------------------------------------------------------------

/// (path, name, type) for every node; (src_path, src_name, relation,
/// target_path:target_name, metadata) for every edge — both sorted, ids and
/// timestamps projected away so two independently-built DBs can be compared.
#[allow(clippy::type_complexity)]
fn graph_projection(
    db: &Database,
) -> (
    Vec<(String, String, String)>,
    Vec<(String, String, String, String, Option<String>)>,
) {
    let mut nodes: Vec<(String, String, String)> = db
        .conn()
        .prepare("SELECT f.path, n.name, n.type FROM nodes n JOIN files f ON f.id = n.file_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    nodes.sort();
    let mut edges: Vec<(String, String, String, String, Option<String>)> = db
        .conn()
        .prepare(
            "SELECT sf.path, sn.name, e.relation, tf.path || ':' || tn.name, e.metadata
             FROM edges e
             JOIN nodes sn ON sn.id = e.source_id
             JOIN files sf ON sf.id = sn.file_id
             JOIN nodes tn ON tn.id = e.target_id
             JOIN files tf ON tf.id = tn.file_id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    edges.sort();
    (nodes, edges)
}

fn write_cross_batch_fixture(root: &std::path::Path, filler_count: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("aaa_impl.rs"),
        "use crate::zzz_trait::MyTrait;\npub struct Foo;\nimpl MyTrait for Foo {}\n",
    )
    .unwrap();
    fs::write(src.join("zzz_trait.rs"), "pub trait MyTrait {}\n").unwrap();
    fs::write(
        src.join("aaa_child.ts"),
        "import { Base } from './zzz_base';\nexport class Child extends Base {}\n",
    )
    .unwrap();
    fs::write(src.join("zzz_base.ts"), "export class Base {}\n").unwrap();
    // Heritage axis (INDEX_VERSION 62, audit P1-3): every declaration kind that
    // learned to emit inheritance edges gets a cross-batch pair too. A new axis
    // that is only ever exercised inside ONE batch proves nothing about the
    // deferred-resolution path, which is where this repo's edge losses live.
    fs::write(
        src.join("aaa_iface.java"),
        "interface Shape extends Drawable { }\n",
    )
    .unwrap();
    fs::write(src.join("zzz_drawable.java"), "interface Drawable { }\n").unwrap();
    fs::write(
        src.join("aaa_obj.kt"),
        "object Registry : BaseRegistry { }\n",
    )
    .unwrap();
    fs::write(src.join("zzz_registry.kt"), "open class BaseRegistry { }\n").unwrap();
    fs::write(
        src.join("aaa_level.dart"),
        "enum Level implements Ordered { low }\n",
    )
    .unwrap();
    fs::write(src.join("zzz_ordered.dart"), "abstract class Ordered { }\n").unwrap();
    // Go receiver qualification (P1-4): the caller lives in the other batch, so
    // the method's edge has to survive deferred resolution with its new
    // `qualified_name` in place.
    fs::write(
        src.join("aaa_server.go"),
        "package p\ntype Server struct{}\nfunc (s *Server) Start() error { return nil }\n",
    )
    .unwrap();
    fs::write(
        src.join("zzz_caller.go"),
        "package p\nfunc Boot(s *Server) { s.Start() }\n",
    )
    .unwrap();
    // Sorted order puts aaa_* + mmm_* in batch 1 and zzz_* in batch 2 once the
    // total crosses BATCH_SIZE.
    for i in 0..filler_count {
        fs::write(src.join(format!("mmm_{i:04}.js")), "// filler\n").unwrap();
    }
}

#[test]
fn test_cross_batch_relations_match_single_batch_control() {
    let filler = super::index_files::BATCH_SIZE - 2; // 4 meaningful files → total = BATCH_SIZE + 2
    let multi_dir = TempDir::new().unwrap();
    let multi_db_dir = TempDir::new().unwrap();
    write_cross_batch_fixture(multi_dir.path(), filler);
    let multi_db = Database::open(&multi_db_dir.path().join("index.db")).unwrap();
    run_full_index(&multi_db, multi_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control_db_dir = TempDir::new().unwrap();
    write_cross_batch_fixture(control_dir.path(), 0);
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, control_dir.path(), None, None).unwrap();

    let meaningful = [
        "src/aaa_impl.rs",
        "src/zzz_trait.rs",
        "src/aaa_child.ts",
        "src/zzz_base.ts",
        // Heritage axis, INDEX_VERSION 62. These MUST be listed here: the
        // comparison below is restricted to `meaningful`, so a new fixture file
        // that is not in this list contributes an empty-vs-empty diff and the
        // axis reads as verified while never having been compared at all.
        "src/aaa_iface.java",
        "src/zzz_drawable.java",
        "src/aaa_obj.kt",
        "src/zzz_registry.kt",
        "src/aaa_level.dart",
        "src/zzz_ordered.dart",
        "src/aaa_server.go",
        "src/zzz_caller.go",
    ];
    let (multi_nodes, multi_edges) = graph_projection(&multi_db);
    let (control_nodes, control_edges) = graph_projection(&control_db);

    // The multi-batch tree's graph, restricted to the four meaningful files,
    // must equal the single-batch control's graph over the same files. Before
    // the deferred pass this failed three ways at once: implements/imports
    // bound to `<external>` phantoms and the inherits edge vanished.
    let restrict_nodes = |nodes: &[(String, String, String)]| -> Vec<(String, String, String)> {
        nodes
            .iter()
            .filter(|(p, _, _)| meaningful.contains(&p.as_str()))
            .cloned()
            .collect()
    };
    let restrict_edges = |edges: &[(String, String, String, String, Option<String>)]| -> Vec<_> {
        edges
            .iter()
            .filter(|(sp, _, _, tgt, _)| {
                meaningful.contains(&sp.as_str())
                    && meaningful.iter().any(|m| tgt.starts_with(&format!("{m}:")))
            })
            .cloned()
            .collect()
    };
    assert_eq!(
        restrict_nodes(&multi_nodes),
        restrict_nodes(&control_nodes),
        "multi-batch node set diverged from the single-batch control"
    );
    assert_eq!(
        restrict_edges(&multi_edges),
        restrict_edges(&control_edges),
        "multi-batch edge set diverged from the single-batch control"
    );

    // The three specific edges the P0 reproduction lost, asserted positively
    // (presence-first: an empty projection comparison could pass vacuously if
    // extraction itself broke — feedback_mutation_test_the_guard).
    let has_edge = |edges: &[(String, String, String, String, Option<String>)],
                    src_name: &str,
                    relation: &str,
                    tgt: &str| {
        edges
            .iter()
            .any(|(_, sn, r, t, _)| sn == src_name && r == relation && t == tgt)
    };
    for (edges, label) in [(&multi_edges, "multi"), (&control_edges, "control")] {
        assert!(
            has_edge(
                edges,
                "Foo",
                crate::domain::REL_IMPLEMENTS,
                "src/zzz_trait.rs:MyTrait"
            ),
            "{label}: implements Foo→MyTrait missing or bound to a phantom"
        );
        assert!(
            has_edge(
                edges,
                "Child",
                crate::domain::REL_INHERITS,
                "src/zzz_base.ts:Base"
            ),
            "{label}: inherits Child→Base missing"
        );
        assert!(
            has_edge(
                edges,
                "<module>",
                crate::domain::REL_IMPORTS,
                "src/zzz_base.ts:Base"
            ) || has_edge(
                edges,
                "Child",
                crate::domain::REL_IMPORTS,
                "src/zzz_base.ts:Base"
            ),
            "{label}: imports of Base did not bind to the real node"
        );

        // Heritage axis (INDEX_VERSION 62): presence-first, for the same reason
        // as the three above. Each of these declaration kinds emitted NOTHING
        // before the fix, so without a positive assertion the equality check
        // over the restricted projection would compare empty to empty and pass.
        assert!(
            has_edge(
                edges,
                "Shape",
                crate::domain::REL_INHERITS,
                "src/zzz_drawable.java:Drawable"
            ),
            "{label}: java `interface extends` edge missing across the batch boundary"
        );
        assert!(
            has_edge(
                edges,
                "Registry",
                crate::domain::REL_INHERITS,
                "src/zzz_registry.kt:BaseRegistry"
            ),
            "{label}: kotlin `object :` edge missing across the batch boundary"
        );
        assert!(
            has_edge(
                edges,
                "Level",
                crate::domain::REL_IMPLEMENTS,
                "src/zzz_ordered.dart:Ordered"
            ),
            "{label}: dart `enum implements` edge missing across the batch boundary"
        );
    }

    // P1-4: the Go method keeps its receiver-qualified name on BOTH paths. A
    // node-level assertion, not an edge one — the defect was that two types'
    // same-named methods were one indistinguishable symbol.
    for (nodes, label) in [(&multi_nodes, "multi"), (&control_nodes, "control")] {
        assert!(
            nodes
                .iter()
                .any(|(p, n, _)| p == "src/aaa_server.go" && n == "Start"),
            "{label}: the Go method node is missing entirely"
        );
    }

    // P1-9: nothing in either tree is genuinely external, so no `<external>`
    // sentinel may survive the run (the multi-batch tree minted them for
    // MyTrait/Base before the deferred pass existed, and nothing ever reaped
    // them). aaa_impl.rs's `use crate::…` is statically-internal, aaa_child's
    // specifier resolves — both must end on real nodes.
    for (nodes, label) in [(&multi_nodes, "multi"), (&control_nodes, "control")] {
        let sentinels: Vec<_> = nodes
            .iter()
            .filter(|(p, _, _)| p == crate::domain::EXTERNAL_FILE_PATH)
            .collect();
        assert!(
            sentinels.is_empty(),
            "{label}: orphan/phantom <external> sentinels survived: {sentinels:?}"
        );
    }
}

#[test]
fn test_incremental_rename_converges_to_full_rebuild() {
    // Audit 2026-08-02 P1-2 reproduction: renaming a symbol inside a CHANGED
    // file must re-resolve the unchanged caller's edges the way a full rebuild
    // would — before the fix the calls edge vanished (restore missed by name,
    // nothing requeued) and the graph diverged from a fresh rebuild forever.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("db.py"), "def save():\n    pass\n").unwrap();
    fs::write(src.join("other.py"), "def save():\n    pass\n").unwrap();
    fs::write(
        src.join("x.py"),
        "from db import save\n\ndef f():\n    save()\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Presence first (a missing-map assertion reads None != Some as pass —
    // feedback_mutation_test_the_guard).
    let (_, edges) = graph_projection(&db);
    assert!(
        edges
            .iter()
            .any(|(_, sn, r, t, _)| sn == "f" && r == REL_CALLS && t == "src/db.py:save"),
        "precondition: f → db.py:save call edge must exist, got {edges:?}"
    );

    // Rename save → store in db.py only; x.py and other.py stay untouched.
    fs::write(src.join("db.py"), "def store():\n    pass\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // Control: fresh full index of the SAME final tree.
    let control_db_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();

    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    assert!(
        inc_edges
            .iter()
            .any(|(_, sn, r, t, _)| sn == "f" && r == REL_CALLS && t == "src/other.py:save"),
        "incremental rename dropped the caller's edge instead of re-resolving it: {inc_edges:?}"
    );
    assert_eq!(
        inc_nodes, full_nodes,
        "incremental node set diverged from a fresh full rebuild"
    );
    assert_eq!(
        inc_edges, full_edges,
        "incremental edge set diverged from a fresh full rebuild"
    );
}

#[test]
fn test_file_grown_past_size_limit_stops_lying_and_stops_rediffing() {
    // Indexing audit 2026-08-02 IDX-1. A file that grows past max_file_size is
    // skipped by Phase 1a, which used to mean `upsert_file` and
    // `delete_nodes_by_file` never ran for it: the index kept answering with
    // symbols the file no longer contains, AND its stored hash never advanced,
    // so compute_diff re-reported it as changed on every single run forever.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("big.ts"), "export class Wide {}\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Presence first: the symbol must really be in the graph before we assert
    // that it leaves (feedback_mutation_test_the_guard).
    let (nodes, _) = graph_projection(&db);
    assert!(
        nodes
            .iter()
            .any(|(p, n, _)| p == "src/big.ts" && n == "Wide"),
        "precondition: Wide must be indexed while the file is small, got {nodes:?}"
    );

    // Grow it past the 1 MiB default limit, renaming the symbol on the way so a
    // stale node is unmistakable. Padding goes in a comment so the file stays
    // valid TypeScript — the ONLY reason it is skipped is its size.
    let padding = "// ".to_string() + &"x".repeat(1_100_000) + "\n";
    fs::write(
        src.join("big.ts"),
        format!("{padding}export class Renamed {{}}\n"),
    )
    .unwrap();
    let first = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        first.stats.files_skipped_size, 1,
        "the grown file must be skipped for size, not indexed"
    );

    let (nodes, _) = graph_projection(&db);
    assert!(
        !nodes
            .iter()
            .any(|(p, n, _)| p == "src/big.ts" && n == "Wide"),
        "stale symbol survived in a file that is no longer parsed: {nodes:?}"
    );

    // And the hash must have advanced: a second incremental over an unchanged
    // tree has nothing to do. Before the fix this re-hashed and re-ran the whole
    // pipeline on every run, forever.
    let second = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        second.stats.files_skipped_size, 0,
        "an unchanged oversize file must not be re-processed on the next run"
    );
    assert_eq!(
        second.files_indexed, 0,
        "an unchanged tree must report no work"
    );

    // Converge with a fresh rebuild of the same final tree.
    let control_db_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    assert_eq!(
        inc_nodes, full_nodes,
        "node set diverged from fresh rebuild"
    );
    assert_eq!(
        inc_edges, full_edges,
        "edge set diverged from fresh rebuild"
    );
}

#[test]
fn test_incremental_delete_converges_to_full_rebuild_for_non_call_edges() {
    // Indexing audit 2026-08-02 P1-5 reproduction. Phase 0 buffered ONLY
    // `calls` before the cascade-delete, so deleting b.ts destroyed a.ts's
    // `imports`/`inherits` edges into it while a.ts itself never changed —
    // and a.ts's hash still matched, so nothing re-extracted them. A full
    // rebuild of the same final tree re-resolves them onto the `<external>`
    // sentinel, so incremental and full diverged permanently.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("b.ts"), "export class Base {}\n").unwrap();
    fs::write(
        src.join("a.ts"),
        "import { Base } from './b';\n\nexport class Child extends Base {}\n\n\
         export function useIt() { return new Child(); }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Presence first: assert the real cross-file edges exist BEFORE the delete,
    // so a later "they are gone" assertion cannot pass vacuously by matching
    // nothing at either end (feedback_mutation_test_the_guard).
    let (_, edges) = graph_projection(&db);
    assert!(
        edges.iter().any(|(sp, _, r, t, _)| sp == "src/a.ts"
            && r == crate::domain::REL_IMPORTS
            && t == "src/b.ts:Base"),
        "precondition: a.ts must import b.ts:Base, got {edges:?}"
    );
    assert!(
        edges
            .iter()
            .any(|(sp, sn, r, _, _)| sp == "src/a.ts" && sn == "Child" && r == "inherits"),
        "precondition: Child must inherit Base, got {edges:?}"
    );

    // Delete the target file. a.ts is untouched and stays out of the changed set.
    fs::remove_file(src.join("b.ts")).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // Control: fresh full index of the SAME final tree (a.ts alone).
    let control_db_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();

    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    // Direct statement of the defect: the import survives as an <external>
    // binding rather than evaporating. Asserted explicitly (not only via the
    // set equality below) so the failure message names the lost edge.
    assert!(
        full_edges
            .iter()
            .any(|(sp, _, r, t, _)| sp == "src/a.ts"
                && r == crate::domain::REL_IMPORTS
                && t == "<external>:Base"),
        "control: a fresh rebuild should bind the now-missing import to the sentinel, got {full_edges:?}"
    );
    assert!(
        inc_edges
            .iter()
            .any(|(sp, _, r, t, _)| sp == "src/a.ts"
                && r == crate::domain::REL_IMPORTS
                && t == "<external>:Base"),
        "incremental delete dropped the unchanged file's import edge instead of re-resolving it: {inc_edges:?}"
    );
    assert_eq!(
        inc_nodes, full_nodes,
        "incremental node set diverged from a fresh full rebuild after a delete"
    );
    assert_eq!(
        inc_edges, full_edges,
        "incremental edge set diverged from a fresh full rebuild after a delete"
    );
}

#[test]
fn test_multi_batch_incremental_rename_survives_and_converges() {
    // Pre-tag review Critical-1 (2026-08-02): a restore-miss requeue captured
    // the source node's CURRENT id; when the source file sat in a LATER batch
    // of the same run, that batch's cascade-delete turned the id dangling and
    // the deferred pass aborted the whole run on the edges FK — leaving the
    // index missing every deferred edge with no self-heal. Needs BOTH legs the
    // earlier tests lacked: >BATCH_SIZE changed files AND a pre-existing index.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("aaa_target.py"), "def save():\n    pass\n").unwrap();
    fs::write(
        src.join("zzz_caller.py"),
        "from aaa_target import save\n\ndef f():\n    save()\n",
    )
    .unwrap();
    let filler = super::index_files::BATCH_SIZE;
    for i in 0..filler {
        fs::write(src.join(format!("mmm_{i:04}.py")), "# filler\n").unwrap();
    }

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let (_, edges) = graph_projection(&db);
    assert!(
        edges
            .iter()
            .any(|(_, sn, r, t, _)| sn == "f" && r == REL_CALLS && t == "src/aaa_target.py:save"),
        "precondition: cross-batch call edge must exist, got {edges:?}"
    );

    // Rename the batch-1 symbol AND rewrite every file, so the whole tree is
    // in the changed set and the caller lands in a later batch than the
    // renamed target.
    fs::write(src.join("aaa_target.py"), "def store():\n    pass\n").unwrap();
    fs::write(
        src.join("zzz_caller.py"),
        "from aaa_target import save\n\n# touched\ndef f():\n    save()\n",
    )
    .unwrap();
    for i in 0..filler {
        fs::write(src.join(format!("mmm_{i:04}.py")), "# filler touched\n").unwrap();
    }
    run_incremental_index(&db, project_dir.path(), None, None)
        .expect("multi-batch incremental with a rename must not abort (dangling requeue FK)");

    // And it must converge to what a fresh rebuild of the final tree says.
    let control_db_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    assert_eq!(
        inc_nodes, full_nodes,
        "node set diverged from fresh rebuild"
    );
    assert_eq!(
        inc_edges, full_edges,
        "edge set diverged from fresh rebuild"
    );
}

/// Fixture for the two `buffer_inbound_before_node_purge` dangling-source guards.
///
/// `aaa_base.ts` is the delete target; `mid_user.ts` and `zzz_user.ts` hold
/// NON-`calls` inbound edges into it (`imports` + `inherits`), which is the edge
/// class Phase 0 buffers into `deferred` with the source node's CURRENT id.
///
/// The `BATCH_SIZE` filler files are NOT decoration: with a tiny tree SQLite
/// hands the just-freed rowid straight back on reinsert, so a dangling id
/// silently lands on a live row and the FK never fires — the defect hides
/// itself. The filler also forces the holders into a later batch than the
/// delete, which is the ordering that makes the captured id go stale at all.
fn dangling_source_fixture(src: &std::path::Path) -> usize {
    fs::create_dir_all(src).unwrap();
    fs::write(src.join("aaa_base.ts"), "export class Base {}\n").unwrap();
    for name in ["mid_user.ts", "zzz_user.ts"] {
        fs::write(
            src.join(name),
            "import { Base } from './aaa_base';\n\nexport class Child extends Base {}\n",
        )
        .unwrap();
    }
    let filler = super::index_files::BATCH_SIZE;
    for i in 0..filler {
        fs::write(src.join(format!("fil_{i:04}.ts")), "export const x = 1;\n").unwrap();
    }
    filler
}

/// Both holders must really own the buffered edge class before either leg can
/// claim the guard did anything — otherwise "the run did not abort" passes
/// vacuously on a tree that never had an edge to dangle.
fn assert_inbound_precondition(db: &Database) {
    let (_, edges) = graph_projection(db);
    for holder in ["src/mid_user.ts", "src/zzz_user.ts"] {
        assert!(
            edges.iter().any(|(sp, _, r, t, _)| sp == holder
                && r == crate::domain::REL_IMPORTS
                && t == "src/aaa_base.ts:Base"),
            "precondition: {holder} must import aaa_base.ts:Base, got {edges:?}"
        );
    }
}

#[test]
fn test_delete_with_holder_in_same_run_does_not_dangle_on_fk() {
    // Guard leg 1: `run_file_paths.contains(source_path)`.
    //
    // Phase 0 buffers a deleted file's inbound non-`calls` edges so an unchanged
    // holder does not lose them (audit P1-5). It captures the holder's CURRENT
    // node id. When the holder is ALSO in this run's changed set, a later batch
    // purges and reinserts its nodes under fresh ids, so the buffered id is
    // dangling by the time the deferred pass runs — `FOREIGN KEY constraint
    // failed` (787) aborts the WHOLE index run, not just this one edge.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    let filler = dangling_source_fixture(&src);

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_inbound_precondition(&db);

    // Delete the target AND rewrite every other file, so both holders land in
    // this run's changed set and in a batch after the delete.
    fs::remove_file(src.join("aaa_base.ts")).unwrap();
    for name in ["mid_user.ts", "zzz_user.ts"] {
        fs::write(
            src.join(name),
            "import { Base } from './aaa_base';\n\n// touched\nexport class Child extends Base {}\n",
        )
        .unwrap();
    }
    for i in 0..filler {
        fs::write(src.join(format!("fil_{i:04}.ts")), "export const x = 2;\n").unwrap();
    }

    run_incremental_index(&db, project_dir.path(), None, None).expect(
        "deleting a file whose inbound-edge holders are themselves in this run's changed set \
         must not buffer their pre-purge node ids (edges FK 787 aborts the entire run)",
    );

    // Not aborting is necessary but not sufficient: the run could have survived
    // by dropping the edges instead. Converge with a fresh rebuild.
    let control_db_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    assert_eq!(
        inc_nodes, full_nodes,
        "node set diverged from fresh rebuild"
    );
    assert_eq!(
        inc_edges, full_edges,
        "edge set diverged from fresh rebuild"
    );
}

#[test]
fn test_delete_with_holder_also_deleted_does_not_dangle_on_fk() {
    // Guard leg 2: `delete_set.contains(source_path)`.
    //
    // Same buffer, different way for the id to go stale: the holder is deleted
    // in the SAME run. Phase 0 walks `delete_paths` in order, so a holder deleted
    // after the target still has live nodes when its edges are buffered — and no
    // nodes at all by the time the deferred pass tries to insert them. Distinct
    // from leg 1: the holder is never re-indexed, so `run_file_paths` does not
    // cover it and leg 1's clause alone leaves this open.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    let filler = dangling_source_fixture(&src);

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_inbound_precondition(&db);

    // Delete the target and ONE holder (sorting after it), leaving the other
    // holder untouched so the buffer still has real work to do.
    fs::remove_file(src.join("aaa_base.ts")).unwrap();
    fs::remove_file(src.join("mid_user.ts")).unwrap();
    for i in 0..filler {
        fs::write(src.join(format!("fil_{i:04}.ts")), "export const x = 3;\n").unwrap();
    }

    run_incremental_index(&db, project_dir.path(), None, None).expect(
        "deleting a file together with one of its inbound-edge holders must not buffer the \
         holder's about-to-be-purged node ids (edges FK 787 aborts the entire run)",
    );

    let control_db_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_db_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    // The surviving holder must still carry the re-resolved import; a run that
    // "passed" by losing it is the P1-5 regression wearing a green badge.
    assert!(
        inc_edges
            .iter()
            .any(|(sp, _, r, t, _)| sp == "src/zzz_user.ts"
                && r == crate::domain::REL_IMPORTS
                && t == "<external>:Base"),
        "surviving holder lost its import instead of re-resolving to the sentinel: {inc_edges:?}"
    );
    assert_eq!(
        inc_nodes, full_nodes,
        "node set diverged from fresh rebuild"
    );
    assert_eq!(
        inc_edges, full_edges,
        "edge set diverged from fresh rebuild"
    );
}

/// Fixture for the oversize-purge dangling-TARGET guard (audit 2026-08-16 P0-1).
///
/// `aaa_caller.ts` holds every inbound edge class into `mid_target.ts` that the
/// purge has to survive: `imports` (module-level), `inherits` (class), and
/// `calls` (method body). `mid_target.ts` is the file that grows past
/// `max_file_size` and gets its nodes purged with no reinsert.
///
/// The filler files sort AFTER the target on purpose. Node ids are minted in
/// sorted-path order, so the target's ids sit in the MIDDLE of the range and
/// SQLite can never hand them back on a later insert — with a two-file tree the
/// freed rowids are the max and come straight back, landing a dangling id on a
/// live row and hiding the defect (feedback_mutation_test_the_guard).
fn oversize_purge_fixture(src: &std::path::Path) -> usize {
    fs::create_dir_all(src).unwrap();
    fs::write(
        src.join("aaa_caller.ts"),
        "import { helperX, BaseX } from './mid_target';\n\n\
         export class Svc extends BaseX {\n  run() { return helperX(); }\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("mid_target.ts"),
        "export function helperX(): number { return 1; }\nexport class BaseX {}\n",
    )
    .unwrap();
    let filler = 50;
    for i in 0..filler {
        fs::write(
            src.join(format!("zzz_fil_{i:04}.ts")),
            format!("export function fil{i:04}(): number {{ return {i}; }}\n"),
        )
        .unwrap();
    }
    filler
}

/// The caller must really own all three inbound edge classes before the purge,
/// or "the run did not abort" passes vacuously.
fn assert_oversize_purge_precondition(db: &Database) {
    let (_, edges) = graph_projection(db);
    for (relation, target) in [
        (crate::domain::REL_IMPORTS, "src/mid_target.ts:helperX"),
        (crate::domain::REL_IMPORTS, "src/mid_target.ts:BaseX"),
        (crate::domain::REL_INHERITS, "src/mid_target.ts:BaseX"),
        (crate::domain::REL_CALLS, "src/mid_target.ts:helperX"),
    ] {
        assert!(
            edges
                .iter()
                .any(|(sp, _, r, t, _)| sp == "src/aaa_caller.ts" && r == relation && t == target),
            "precondition: aaa_caller.ts must hold {relation} → {target}, got {edges:?}"
        );
    }
}

/// Grow `path` past `max_file_size` while keeping it valid TypeScript, so the
/// ONLY reason Phase 1a skips it is its size.
fn grow_past_size_limit(path: &std::path::Path) {
    let body = fs::read_to_string(path).unwrap();
    let padding = "// ".to_string() + &"x".repeat(1_100_000) + "\n";
    fs::write(path, format!("{padding}{body}")).unwrap();
}

#[test]
fn test_oversize_purge_drops_its_nodes_from_the_name_map() {
    // Audit 2026-08-16 P0-1. `global_name_map` is loaded ONCE before the batch
    // loop and pruned per batch from `batch_parsed` — which holds only the files
    // that PARSED. A file skipped for size still gets `delete_nodes_by_file`, so
    // its ids stay in the map pointing at rows that no longer exist. The deferred
    // pass then resolves the caller's requeued `imports` onto those dead ids and
    // `FOREIGN KEY constraint failed` (787) aborts the WHOLE run — after the
    // batch savepoint already committed the target's new hash, so compute_diff
    // never offers the file again and the caller's edges are lost for good.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    oversize_purge_fixture(&src);

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_oversize_purge_precondition(&db);

    grow_past_size_limit(&src.join("mid_target.ts"));
    let result = run_incremental_index(&db, project_dir.path(), None, None).expect(
        "purging an oversize file's nodes must also drop them from this run's name map \
         (a deferred edge onto a dead id aborts the entire run on the edges FK 787)",
    );
    assert_eq!(
        result.stats.files_skipped_size, 1,
        "precondition: the grown file must be skipped for size, not indexed"
    );

    // Not aborting is necessary, not sufficient: the run could have survived by
    // binding the caller to a phantom. Converge with a fresh rebuild of the same
    // tree — which is the only definition of "correct" that does not depend on
    // the arm the fix happened to take.
    let control_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    assert_eq!(
        inc_nodes, full_nodes,
        "node set diverged from a fresh rebuild of the oversize tree"
    );
    assert_eq!(
        inc_edges, full_edges,
        "edge set diverged from a fresh rebuild of the oversize tree"
    );
}

#[test]
fn test_oversize_file_shrinking_back_restores_its_own_symbols() {
    // The other end of the P0-1 window: the aborting run had ALREADY committed
    // the target's new hash, so `compute_diff` never offered the file again and
    // nothing about it could recover — not even after the file shrank back,
    // because the run that would re-index it kept aborting on the same dead ids.
    // With the map pruned, the shrink-back run completes and the file's own
    // symbols return to exactly what a fresh rebuild produces.
    //
    // KNOWN GAP (not this fix, and not caused by the skipped-file path): the
    // caller's `imports` edges stay bound to the `<external>` sentinels they
    // were legitimately re-resolved onto while the target had no symbols. The
    // caller's own content never changed, so nothing re-extracts it, and no
    // channel re-binds a sentinel edge when the real symbol comes back — the
    // same sequence on the DELETE path (remove the file, index, restore it,
    // index) leaves the identical residue on HEAD without any skipped file
    // involved. It also costs the `calls` edge, which DOES heal through
    // `pending_unresolved_calls` and is then deleted again by
    // `prune_import_contradicted_call_edges`, since the stale sentinel import
    // binds the same name to a different node. Asserted here at the level this
    // fix actually reaches — node convergence — rather than pinned as expected
    // output at the edge level, which would cement the residue.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    oversize_purge_fixture(&src);
    let original = fs::read_to_string(src.join("mid_target.ts")).unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_oversize_purge_precondition(&db);

    grow_past_size_limit(&src.join("mid_target.ts"));
    run_incremental_index(&db, project_dir.path(), None, None)
        .expect("oversize purge must not abort the run");
    let (nodes, _) = graph_projection(&db);
    assert!(
        !nodes.iter().any(|(p, _, _)| p == "src/mid_target.ts"),
        "precondition: the purged file must hold no symbols while it is oversize, got {nodes:?}"
    );

    fs::write(src.join("mid_target.ts"), &original).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None)
        .expect("re-indexing the shrunk file must not abort the run");

    let control_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, _) = graph_projection(&db);
    let (full_nodes, _) = graph_projection(&control_db);
    // Presence first: an empty projection would compare equal to an empty one.
    for name in ["helperX", "BaseX"] {
        assert!(
            inc_nodes
                .iter()
                .any(|(p, n, _)| p == "src/mid_target.ts" && n == name),
            "{name} must be back in the graph after the file shrank below the cap, \
             got {inc_nodes:?}"
        );
    }
    // Restricted to real project files: the `<external>` pseudo-file still holds
    // the two sentinels the caller's stale imports keep alive (the KNOWN GAP
    // above — `reap_orphan_external_nodes` only removes sentinels nothing points
    // at). Every real file must match a fresh rebuild exactly.
    let project_only = |nodes: &[(String, String, String)]| -> Vec<(String, String, String)> {
        nodes
            .iter()
            .filter(|(p, _, _)| p != crate::domain::EXTERNAL_FILE_PATH)
            .cloned()
            .collect()
    };
    assert_eq!(
        project_only(&inc_nodes),
        project_only(&full_nodes),
        "project node set diverged from fresh rebuild after the file shrank back"
    );
}

/// Fixture for the cross-batch leg of the oversize purge. `aaa_target.ts` lands
/// in batch 1, `zzz_caller.ts` in the last batch, so the caller resolves its
/// relations from a `global_name_map` that an EARLIER batch purged — the
/// batch-time face of P0-1, distinct from the deferred pass's.
fn cross_batch_oversize_fixture(src: &std::path::Path) -> usize {
    fs::create_dir_all(src).unwrap();
    fs::write(
        src.join("aaa_target.ts"),
        "export function helperX(): number { return 1; }\nexport class BaseX {}\n",
    )
    .unwrap();
    fs::write(
        src.join("zzz_caller.ts"),
        "import { helperX, BaseX } from './aaa_target';\n\n\
         export class Svc extends BaseX {\n  run() { return helperX(); }\n}\n",
    )
    .unwrap();
    let filler = super::index_files::BATCH_SIZE;
    for i in 0..filler {
        fs::write(
            src.join(format!("mmm_{i:04}.ts")),
            format!("export function fil{i:04}(): number {{ return {i}; }}\n"),
        )
        .unwrap();
    }
    filler
}

#[test]
fn test_oversize_purge_in_an_earlier_batch_does_not_dangle_for_a_later_one() {
    // Cross-batch leg of P0-1. The stale ids a skipped file leaves in
    // `global_name_map` are read TWICE: by the deferred pass after the loop, and
    // by every LATER batch when it seeds `name_to_ids` from the map (the
    // `!batch_file_paths.contains(path)` filter at Phase 2). A single-batch
    // corpus exercises only the first, so it is a null control for the second —
    // this repo's edge work diffs across the batch boundary for exactly that
    // reason (feedback_edge_exclusion_verify_by_index_diff). Here the caller
    // sits in a later batch than the file that was purged, so its `imports` /
    // `inherits` resolve against the dead ids at BATCH time.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    let filler = cross_batch_oversize_fixture(&src);

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let (_, edges) = graph_projection(&db);
    assert!(
        edges
            .iter()
            .any(|(sp, _, r, t, _)| sp == "src/zzz_caller.ts"
                && r == crate::domain::REL_INHERITS
                && t == "src/aaa_target.ts:BaseX"),
        "precondition: the caller must inherit across the batch boundary, got {edges:?}"
    );

    // Grow the target past the cap AND touch every other file, so the whole tree
    // is in the changed set and the caller lands in a batch after the purge.
    grow_past_size_limit(&src.join("aaa_target.ts"));
    for i in 0..filler {
        fs::write(
            src.join(format!("mmm_{i:04}.ts")),
            format!("// touched\nexport function fil{i:04}(): number {{ return {i}; }}\n"),
        )
        .unwrap();
    }
    fs::write(
        src.join("zzz_caller.ts"),
        "import { helperX, BaseX } from './aaa_target';\n\n// touched\n\
         export class Svc extends BaseX {\n  run() { return helperX(); }\n}\n",
    )
    .unwrap();

    let result = run_incremental_index(&db, project_dir.path(), None, None).expect(
        "a later batch must not resolve against the ids an earlier batch's oversize purge \
         freed (edges FK 787 aborts the entire run)",
    );
    assert_eq!(
        result.stats.files_skipped_size, 1,
        "precondition: the grown file must be skipped for size, not indexed"
    );

    let control_dir = TempDir::new().unwrap();
    let control_db = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control_db, project_dir.path(), None, None).unwrap();
    let (inc_nodes, inc_edges) = graph_projection(&db);
    let (full_nodes, full_edges) = graph_projection(&control_db);
    assert_eq!(
        inc_nodes, full_nodes,
        "node set diverged from a fresh rebuild across the batch boundary"
    );
    assert_eq!(
        inc_edges, full_edges,
        "edge set diverged from a fresh rebuild across the batch boundary"
    );
}

#[test]
fn test_deferred_only_run_still_classifies_edge_confidence() {
    // The P2 that rides with P0-1. The confidence post-pass is gated on the run
    // having done observable work, and the gate listed three producers:
    // indexed files, deleted files, the pending sweep. Phase 2b-final is a
    // fourth — it inserts cross-file by-name binds, exactly the shape Phase 2e
    // downgrades off the `extracted` column default — and a run whose ONLY
    // changed file is skipped for size hits none of the other three: nothing
    // parsed, nothing deleted, and the sweep is itself gated on parsing. The
    // purge still requeues that file's inbound `references`, the deferred pass
    // re-binds them, and ungated they keep `extracted`, the TOP tier, having
    // never been classified.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let a = project_dir.path().join("src/a");
    let b = project_dir.path().join("src/b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    // Same-directory candidate wins the initial bind; the far one is what the
    // requeued reference has left to bind to once the near one is purged.
    fs::write(a.join("aaa_ref.ts"), "export const wired = handlerX;\n").unwrap();
    fs::write(
        a.join("mid_dup.ts"),
        "export function handlerX(): number { return 1; }\n",
    )
    .unwrap();
    fs::write(
        b.join("zzz_alt.ts"),
        "export function handlerX(): number { return 2; }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let ref_confidences = |db: &Database| -> Vec<(String, String)> {
        db.conn()
            .prepare(
                "SELECT tf.path, e.confidence FROM edges e
                 JOIN nodes sn ON sn.id = e.source_id
                 JOIN files sf ON sf.id = sn.file_id
                 JOIN nodes tn ON tn.id = e.target_id
                 JOIN files tf ON tf.id = tn.file_id
                 WHERE e.relation = 'references' AND sf.path = 'src/a/aaa_ref.ts'
                   AND tn.name = 'handlerX'
                 ORDER BY tf.path",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        ref_confidences(&db),
        vec![("src/a/mid_dup.ts".to_string(), "ambiguous".to_string())],
        "precondition: the reference binds to the near candidate and is classified"
    );

    grow_past_size_limit(&a.join("mid_dup.ts"));
    let result = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        result.files_indexed, 0,
        "precondition: the only changed file must be skipped, so nothing is indexed"
    );

    // The requeued reference re-bound to the surviving candidate. That edge was
    // written by the deferred pass and by nothing else, so its confidence is the
    // whole observable difference the gate makes.
    assert_eq!(
        ref_confidences(&db),
        vec![("src/b/zzz_alt.ts".to_string(), "inferred".to_string())],
        "a deferred-pass edge on an otherwise idle run must still be classified, \
         not left on the `extracted` column default"
    );
}

// ---------------------------------------------------------------------------
// Run-completion marker (audit 2026-08-16 P1-2).
// ---------------------------------------------------------------------------

fn run_marker(db: &Database) -> Option<String> {
    crate::storage::queries::get_meta(
        db.conn(),
        crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT,
    )
    .unwrap()
}

fn set_run_marker(db: &Database) {
    crate::storage::queries::set_meta(
        db.conn(),
        crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT,
        "1",
    )
    .unwrap();
}

#[test]
fn test_index_run_marker_is_cleared_by_a_completed_run() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.ts"),
        "export function alpha(): number { return 1; }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        run_marker(&db),
        None,
        "a run that reached the deferred commit must leave no in-flight marker"
    );

    // An untouched tree is the NORMAL state after a crash — the user restarts and
    // edits nothing. The diff is empty, so without the marker driving it there is
    // no run left to rebuild the abandoned edges, ever.
    set_run_marker(&db);
    let result = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        result.files_indexed, 1,
        "an interrupted marker must force a re-index even when nothing changed on disk"
    );
    assert_eq!(
        run_marker(&db),
        None,
        "the recovery run completed, so it must clear the marker"
    );
}

#[test]
fn test_interrupted_run_marker_escalates_incremental_to_full_reindex() {
    // Simulates the kill window: hashes committed, cross-file edges never
    // written. The marker is the only thing that survives it, because
    // `compute_diff` sees hashes that say every file is current.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("aaa_base.ts"), "export class Base {}\n").unwrap();
    fs::write(
        src.join("bbb_child.ts"),
        "import { Base } from './aaa_base';\n\nexport class Child extends Base {}\n",
    )
    .unwrap();
    fs::write(src.join("ccc_other.ts"), "export const other = 1;\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Destroy the cross-file edges the killed run would never have written, then
    // leave its marker behind. Hashes stay put — that is the whole trap.
    db.conn()
        .execute(
            "DELETE FROM edges WHERE relation IN ('inherits', 'imports')",
            [],
        )
        .unwrap();
    set_run_marker(&db);

    // One unrelated file changes. The diff alone would re-index exactly that one.
    fs::write(src.join("ccc_other.ts"), "export const other = 2;\n").unwrap();
    let result = run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    assert_eq!(
        result.files_indexed, 3,
        "an interrupted marker must escalate the one-file diff to the whole tree"
    );
    assert_eq!(
        run_marker(&db),
        None,
        "the escalated run completed, so it must clear the marker it inherited"
    );

    let (_, edges) = graph_projection(&db);
    assert!(
        edges.iter().any(|(sp, _, r, t, _)| sp == "src/bbb_child.ts"
            && r == crate::domain::REL_INHERITS
            && t == "src/aaa_base.ts:Base"),
        "the re-index must restore the cross-file edge the interrupted run lost, got {edges:?}"
    );
}

#[test]
fn test_query_time_refresh_preserves_an_interrupted_run_marker() {
    // `ensure_file_indexed` runs the same pipeline for ONE file on the query
    // path. Letting it clear the marker would retire the crash evidence without
    // doing the full re-index it exists to trigger — the next incremental would
    // then run its ordinary diff and the abandoned edges would stay lost.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.ts"),
        "export function alpha(): number { return 1; }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    set_run_marker(&db);

    fs::write(
        src.join("a.ts"),
        "export function alpha(): number { return 2; }\n",
    )
    .unwrap();
    let refreshed = ensure_file_indexed(&db, project_dir.path(), "src/a.ts", None).unwrap();
    assert!(
        refreshed,
        "precondition: the edited file must be re-indexed"
    );
    assert_eq!(
        run_marker(&db).as_deref(),
        Some("1"),
        "a single-file query-time refresh must leave the interrupted-run marker standing"
    );
}

/// Like `graph_projection`, but carries `confidence`.
///
/// The existing projection deliberately stops at (path, name, relation, target,
/// metadata), so every incremental-converges-to-rebuild test in this file is
/// blind to a confidence divergence — the one thing Phase 2e writes. The scoped
/// post-passes are exactly the code that could produce one, so they get a
/// projection that can see it.
fn graph_projection_with_confidence(db: &Database) -> Vec<(String, String, String, String)> {
    let mut edges: Vec<(String, String, String, String)> = db
        .conn()
        .prepare(
            "SELECT sf.path || ':' || sn.name, e.relation, tf.path || ':' || tn.name,
                    COALESCE(e.confidence, '<null>')
             FROM edges e
             JOIN nodes sn ON sn.id = e.source_id
             JOIN files sf ON sf.id = sn.file_id
             JOIN nodes tn ON tn.id = e.target_id
             JOIN files tf ON tf.id = tn.file_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    edges.sort();
    edges
}

#[test]
fn a_third_file_reclassifies_an_edge_between_two_files_it_never_touched() {
    // The completeness case for `PostPassScope::Files`, and the only one the
    // file arms cannot reach.
    //
    // `b.py` calls `helper()` bare and `a.py` defines it — one definition, so
    // Phase 2e labels that cross-file edge `inferred`. Adding a SECOND `helper`
    // in `c.py` makes the name ambiguous, and the edge that has to be relabelled
    // runs between two files this run did not open. Only the name arm — the
    // names whose node count moved — can see it.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.py"), "def helper():\n    pass\n").unwrap();
    fs::write(src.join("b.py"), "def caller():\n    helper()\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let before = graph_projection_with_confidence(&db);
    let edge_of = |rows: &[(String, String, String, String)]| -> Option<String> {
        rows.iter()
            .find(|(s, r, t, _)| s == "src/b.py:caller" && r == REL_CALLS && t == "src/a.py:helper")
            .map(|(_, _, _, c)| c.clone())
    };
    assert_eq!(
        edge_of(&before).as_deref(),
        Some("inferred"),
        "precondition: one definition of `helper`, so the cross-file call is inferred, not ambiguous: {before:?}"
    );

    // The only file this run sees is c.py. a.py and b.py are byte-identical.
    fs::write(src.join("c.py"), "def helper():\n    pass\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();

    let inc = graph_projection_with_confidence(&db);
    let full = graph_projection_with_confidence(&control);
    assert_eq!(
        edge_of(&inc).as_deref(),
        Some("ambiguous"),
        "a second `helper` appeared, so the untouched b.py -> a.py edge must be reclassified: {inc:?}"
    );
    // Full-set equality, as of D#24.
    //
    // This used to compare only the edges the incremental index HOLDS, with a
    // comment explaining that a rebuild also carries `b.py:caller ->
    // c.py:helper` — a bare call fans out to every same-name candidate — and
    // that the incremental run never re-resolved b.py because b.py did not
    // change. That was a real divergence, older than this scope and independent
    // of it, deliberately not pinned here so as not to encode a wrong shape as
    // expected. `fan_out_to_new_duplicate_definitions` closed it, so the weaker
    // comparison is no longer what this test can honestly make.
    assert_eq!(
        inc, full,
        "an incrementally grown index must carry the same edges as a rebuild of the same tree"
    );
}

/// `pending_unresolved_calls` as `caller file:caller -> target name`, sorted.
fn pending_projection(db: &Database) -> Vec<String> {
    let mut rows: Vec<String> = db
        .conn()
        .prepare(
            "SELECT f.path || ':' || n.name || ' -> ' || p.target_name
             FROM pending_unresolved_calls p
             JOIN nodes n ON n.id = p.source_id JOIN files f ON f.id = n.file_id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_self_call_follows_its_types_method_into_and_out_of_a_nearer_file() {
    // Pre-tag review, finding 1. A `self.m()` binds the nearest methods of its
    // type: own file, else crate, else (from a trait impl) the workspace. So a
    // method of that type appearing in, or leaving, another file of the crate
    // moves the answer of a caller this run never opens — and nothing
    // re-extracted it: the incremental index kept an edge into crate `b` after
    // `a/src/y.rs` gained `Foo::m`, and after y.rs was deleted it had a buffered
    // row instead of the edge a rebuild binds.
    let project_dir = TempDir::new().unwrap();
    let root = project_dir.path();
    let w = |rel: &str, body: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    };
    w("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");
    for pkg in ["a", "b"] {
        w(
            &format!("{pkg}/Cargo.toml"),
            &format!("[package]\nname = \"{pkg}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        );
    }
    w(
        "b/src/lib.rs",
        "pub struct Foo;\nimpl Foo {\n    pub fn m(&self) {}\n}\n",
    );
    w(
        "a/src/lib.rs",
        "pub mod x;\npub mod y;\npub trait Tr { fn go(&self); }\n",
    );
    w(
        "a/src/x.rs",
        "pub struct Foo;\nimpl crate::Tr for Foo {\n    fn go(&self) { self.m() }\n}\n",
    );
    w("a/src/y.rs", "pub fn unrelated() {}\n");

    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, root, None, None).unwrap();
    let rebuild = || {
        let dir = TempDir::new().unwrap();
        let control = Database::open(&dir.path().join("index.db")).unwrap();
        run_full_index(&control, root, None, None).unwrap();
        (
            graph_projection_with_confidence(&control),
            pending_projection(&control),
            dir,
        )
    };
    let go_calls = |rows: &[(String, String, String, String)]| -> Vec<String> {
        rows.iter()
            .filter(|(s, r, _, _)| s == "a/src/x.rs:go" && r == REL_CALLS)
            .map(|(_, _, t, _)| t.clone())
            .collect()
    };

    // The crate gains `Foo::m`: the call binds it, not crate b's.
    w(
        "a/src/y.rs",
        "pub fn unrelated() {}\nimpl crate::x::Foo {\n    pub fn m(&self) {}\n}\n",
    );
    run_incremental_index(&db, root, None, None).unwrap();
    let (full, full_pending, _d1) = rebuild();
    assert_eq!(
        go_calls(&full),
        vec!["a/src/y.rs:m".to_string()],
        "control: {full:?}"
    );
    assert_eq!(
        graph_projection_with_confidence(&db),
        full,
        "after the method appeared"
    );
    assert_eq!(
        pending_projection(&db),
        full_pending,
        "after the method appeared"
    );

    // And loses it again, in a run that only deletes a file.
    fs::remove_file(root.join("a/src/y.rs")).unwrap();
    run_incremental_index(&db, root, None, None).unwrap();
    let (full, full_pending, _d2) = rebuild();
    assert_eq!(
        go_calls(&full),
        vec!["b/src/lib.rs:m".to_string()],
        "control: {full:?}"
    );
    assert_eq!(
        graph_projection_with_confidence(&db),
        full,
        "after the file was deleted"
    );
    assert_eq!(
        pending_projection(&db),
        full_pending,
        "after the file was deleted"
    );
}

#[test]
fn a_self_type_path_call_follows_its_types_method_into_a_nearer_file() {
    // The `Self::m(self)` spelling (`"stype"`) of the case above: its answer
    // moves the same way when the crate gains `Foo::m` (pre-tag review round
    // 2: dropping `stype` from the re-extraction survived the suite).
    let project_dir = TempDir::new().unwrap();
    let root = project_dir.path();
    let w = |rel: &str, body: &str| {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    };
    w("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");
    for pkg in ["a", "b"] {
        w(
            &format!("{pkg}/Cargo.toml"),
            &format!("[package]\nname = \"{pkg}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        );
    }
    w(
        "b/src/lib.rs",
        "pub struct Foo;\nimpl Foo {\n    pub fn m(&self) {}\n}\n",
    );
    w(
        "a/src/lib.rs",
        "pub mod x;\npub mod y;\npub trait Tr { fn go(&self); }\n",
    );
    w(
        "a/src/x.rs",
        "pub struct Foo;\nimpl crate::Tr for Foo {\n    fn go(&self) { Self::m(self) }\n}\n",
    );
    w("a/src/y.rs", "pub fn unrelated() {}\n");
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, root, None, None).unwrap();
    w(
        "a/src/y.rs",
        "pub fn unrelated() {}\nimpl crate::x::Foo {\n    pub fn m(&self) {}\n}\n",
    );
    run_incremental_index(&db, root, None, None).unwrap();
    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, root, None, None).unwrap();
    let full = graph_projection_with_confidence(&control);
    assert!(
        full.iter()
            .any(|(s, r, t, _)| s == "a/src/x.rs:go" && r == REL_CALLS && t == "a/src/y.rs:m"),
        "control: {full:?}"
    );
    assert_eq!(graph_projection_with_confidence(&db), full);
    assert_eq!(pending_projection(&db), pending_projection(&control));
}

#[test]
fn a_rust_untyped_same_file_method_call_is_labelled_by_its_name_count() {
    // D#162. `self.0.poll()` names only `poll`: a tuple field is untyped, so the
    // resolver binds the file's own `poll` — here another type's, the wrapper
    // pattern that made 185 of 233 such tokio edges wrong. Its label follows the
    // name's count as a cross-file guess does, and a definition appearing in a
    // file this run never opens relabels it through the name arm alone. The
    // bind itself is decided from the caller's own file, so that run has no
    // reason to re-extract the caller.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub mod inner;\npub mod wrap;\n").unwrap();
    fs::write(
        src.join("inner.rs"),
        "pub struct Inner;\nimpl Inner {\n    pub fn poll(&self) -> i32 { 1 }\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("wrap.rs"),
        "use crate::inner::Inner;\npub struct Wrapper(Inner);\npub struct Other;\n\
         impl Other {\n    pub fn poll(&self) -> i32 { 2 }\n    pub fn spin(&self) -> i32 { 3 }\n}\n\
         impl Wrapper {\n    pub fn get(&self) -> i32 { self.0.poll() }\n    \
         pub fn turn(&self) -> i32 { self.0.spin() }\n    pub fn first(&self) -> Other { Other }\n    \
         pub fn head(&self) -> i32 { self.first().poll() }\n}\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let conf = |rows: &[(String, String, String, String)], s: &str, t: &str| -> Option<String> {
        rows.iter()
            .find(|(a, r, b, _)| a == s && r == REL_CALLS && b == t)
            .map(|(_, _, _, c)| c.clone())
    };
    let before = graph_projection_with_confidence(&db);
    assert_eq!(
        conf(&before, "src/wrap.rs:get", "src/wrap.rs:poll").as_deref(),
        Some("ambiguous"),
        "`poll` has two definitions, so a by-name member call is ambiguous: {before:?}"
    );
    assert_eq!(
        conf(&before, "src/wrap.rs:head", "src/wrap.rs:poll").as_deref(),
        Some("ambiguous"),
        "a call on a call's result (`q` chain) is the same guess: {before:?}"
    );
    assert_eq!(
        conf(&before, "src/wrap.rs:turn", "src/wrap.rs:spin").as_deref(),
        Some("inferred"),
        "`spin` has one definition: {before:?}"
    );
    assert_eq!(
        conf(&before, "src/wrap.rs:head", "src/wrap.rs:first").as_deref(),
        Some("extracted"),
        "a `self` call chose its target by the impl's type: {before:?}"
    );

    let wrap_ids = |db: &Database| -> Vec<i64> {
        let mut v: Vec<i64> = db
            .conn()
            .prepare("SELECT n.id FROM nodes n JOIN files f ON f.id = n.file_id WHERE f.path = 'src/wrap.rs'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v.sort_unstable();
        v
    };
    let wrap_before = wrap_ids(&db);

    // A second `spin`, in a file this run is the only one to see.
    fs::write(
        src.join("other.rs"),
        "pub struct C;\nimpl C {\n    pub fn spin(&self) -> i32 { 4 }\n}\n",
    )
    .unwrap();
    // lib.rs is left alone on purpose: a crate root's edit re-resolves through
    // its own path (D#136), which would re-extract wrap.rs for another reason.
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();
    let inc = graph_projection_with_confidence(&db);
    let full = graph_projection_with_confidence(&control);
    assert_eq!(
        conf(&inc, "src/wrap.rs:turn", "src/wrap.rs:spin").as_deref(),
        Some("ambiguous"),
        "a second `spin` elsewhere relabels the untouched same-file edge: {inc:?}"
    );
    assert_eq!(
        inc, full,
        "incremental edge set diverged from a rebuild of the same tree"
    );
    assert_eq!(
        wrap_ids(&db),
        wrap_before,
        "the same-file bind does not depend on other files, so the new `spin` must not \
         re-extract wrap.rs"
    );
}

#[test]
fn deleting_the_duplicate_puts_the_untouched_edge_back() {
    // The other direction, and the reason the name scope is a symmetric
    // difference rather than "names this run inserted": removing the second
    // definition has to move the same untouched edge back to `inferred`. A scope
    // built only from what a run ADDS would leave it permanently `ambiguous`,
    // which the confidence floor then hides from callgraph/impact.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.py"), "def helper():\n    pass\n").unwrap();
    fs::write(src.join("b.py"), "def caller():\n    helper()\n").unwrap();
    fs::write(src.join("c.py"), "def helper():\n    pass\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    let start = graph_projection_with_confidence(&db);
    assert!(
        start.iter().any(|(s, r, t, c)| s == "src/b.py:caller"
            && r == REL_CALLS
            && t == "src/a.py:helper"
            && c == "ambiguous"),
        "precondition: two definitions, so the call is ambiguous: {start:?}"
    );

    fs::remove_file(src.join("c.py")).unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();

    let inc = graph_projection_with_confidence(&db);
    let full = graph_projection_with_confidence(&control);
    assert!(
        inc.iter().any(|(s, r, t, c)| s == "src/b.py:caller"
            && r == REL_CALLS
            && t == "src/a.py:helper"
            && c == "inferred"),
        "the duplicate is gone, so the untouched edge must go back to inferred: {inc:?}"
    );
    assert_eq!(
        inc, full,
        "an incrementally grown index must carry the same confidences as a rebuild of the same tree"
    );
}

#[test]
fn a_new_same_name_definition_reaches_an_untouched_bare_caller() {
    // D#24, deferred 2026-09-08 while adding scope-completeness tests for
    // CORE-06, now pinned. A bare call fans out to EVERY same-name candidate, so
    // a rebuild of the final tree carries `b.py:caller -> a.py:helper` AND
    // `b.py:caller -> c.py:helper`. The incremental run that introduced c.py
    // carries only the first, because b.py did not change and its relations are
    // therefore never re-emitted: the post-passes relabel confidence on edges
    // that exist, they do not create the one that should now exist.
    //
    // Independent of `PostPassScope` — the deferred note records it reproducing
    // unchanged with SCOPED_POST_PASS_MAX_FILES forced to 0 (always Global), so
    // it is older than that scope. User-visible as `callgraph`/`impact`
    // under-reporting a caller until the caller's own file is next touched.
    //
    // The sibling test above compares only the edges the incremental index
    // HOLDS, deliberately, so as not to pin this wrong shape as expected. This
    // one asserts the missing edge directly.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.py"), "def helper():\n    pass\n").unwrap();
    fs::write(src.join("b.py"), "def caller():\n    helper()\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let has_edge = |rows: &[(String, String, String, String)], target: &str| -> bool {
        rows.iter()
            .any(|(s, r, t, _)| s == "src/b.py:caller" && r == REL_CALLS && t == target)
    };
    let before = graph_projection_with_confidence(&db);
    assert!(
        has_edge(&before, "src/a.py:helper") && !has_edge(&before, "src/c.py:helper"),
        "precondition: one `helper`, one edge: {before:?}"
    );

    // One run, one file: c.py appears with a second `helper`. a.py and b.py are
    // byte-identical and are never opened by this run.
    fs::write(src.join("c.py"), "def helper():\n    pass\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();

    let inc = graph_projection_with_confidence(&db);
    let full = graph_projection_with_confidence(&control);

    // Control on the control: the rebuild really does fan out, so the assertion
    // below is about the incremental path and not about what a bare call means.
    assert!(
        has_edge(&full, "src/a.py:helper") && has_edge(&full, "src/c.py:helper"),
        "control: a rebuild fans the bare call out to both definitions: {full:?}"
    );
    assert!(
        has_edge(&inc, "src/c.py:helper"),
        "a file appearing with a duplicate name must reach the bare-name callers of that \
         name; the incremental index under-reports the caller until b.py is next touched: {inc:?}"
    );
    assert_eq!(
        inc, full,
        "incremental edge set diverged from a rebuild of the same tree"
    );
}

#[test]
fn an_ordinary_edit_does_not_drag_callers_into_a_fanout_round() {
    // The precision half of D#24, and the one its own success criteria put a
    // number on: the fan-out round must fire on a NEW same-name definition, not
    // on every edit that happens to touch a file defining a called name.
    // Otherwise each keystroke-scale refresh re-extracts the callers too, which
    // is the interactive budget v0.143.0 spent a release bounding.
    //
    // Observed through node ids, because "did not fire" has no other external
    // signal — `IndexResult` reports round one only. Re-extraction replaces a
    // file's nodes, so b.py keeping its ids IS the statement that nothing
    // re-extracted b.py.
    //
    // The tree starts with TWO `helper`s deliberately, and that is what makes
    // this a test of the count predicate. A first draft started with one, so
    // b.py's edge was `inferred`, and the caller query's `ambiguous` filter
    // refused it no matter what the counts said — relaxing
    // `a.cnt > COALESCE(b.cnt, 0)` to `>=` left the test GREEN. With the edge
    // already `ambiguous`, the confidence filter is satisfied before the count
    // is consulted, so the count is the only thing left holding the round back.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.py"), "def helper():\n    pass\n").unwrap();
    fs::write(src.join("b.py"), "def caller():\n    helper()\n").unwrap();
    fs::write(src.join("c.py"), "def helper():\n    pass\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(
        graph_projection_with_confidence(&db)
            .iter()
            .any(|(s, r, _, c)| s == "src/b.py:caller" && r == REL_CALLS && c == "ambiguous"),
        "precondition: two `helper`s, so the caller's edge is already `ambiguous` and the \
         confidence filter cannot be what holds the fan-out round back"
    );

    let node_ids_of = |path: &str| -> Vec<i64> {
        let mut v: Vec<i64> = db
            .conn()
            .prepare("SELECT n.id FROM nodes n JOIN files f ON f.id = n.file_id WHERE f.path = ?1")
            .unwrap()
            .query_map([path], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v.sort_unstable();
        v
    };
    let b_before = node_ids_of("src/b.py");
    assert!(!b_before.is_empty(), "precondition: b.py has nodes");

    // a.py changes CONTENT but still defines exactly one `helper`. The count
    // does not rise, so no bare-name caller has anything new to learn.
    fs::write(src.join("a.py"), "def helper():\n    return 1\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        node_ids_of("src/b.py"),
        b_before,
        "an edit that adds no same-name definition must not re-extract the caller"
    );

    // Contrast arm, so the assertion above cannot pass by the round being dead:
    // a THIRD `helper` appears, and now b.py must be re-extracted.
    fs::write(src.join("d.py"), "def helper():\n    pass\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_ne!(
        node_ids_of("src/b.py"),
        b_before,
        "a new same-name definition must re-extract the bare-name caller — if this still \
         matches, the arm above proves nothing"
    );
}

#[test]
fn a_new_same_name_definition_reaches_an_untouched_bare_reference() {
    // D#24's other half, found by pre-ship review of the first fix (2026-09-11).
    //
    // `CONF_CASE` labels TWO relations `ambiguous`, not one: `classify_edge_confidence`
    // binds `CONF_WHERE`'s parameters as `params![REL_CALLS, REL_REFERENCES, ...]`
    // (resolve.rs). A `references` edge therefore fans out to every same-name
    // candidate exactly as a `calls` edge does — and the first version of the
    // fan-out round filtered `e.relation = 'calls'` alone, so it repaired half
    // the population and silently left the other half. Neither the code, the
    // spec, nor the INDEX_VERSION note said `references` anywhere, so all three
    // read as if the repair were complete.
    //
    // Rust rather than Python because `references` is where a type is MENTIONED:
    // `Option<Widget>` in a signature is a reference to `Widget`, not a call.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.rs"), "pub struct Widget { pub a: i32 }\n").unwrap();
    fs::write(
        src.join("b.rs"),
        "pub fn caller() -> Option<Widget> { None }\n",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let refs_to = |rows: &[(String, String, String, String)], target: &str| -> bool {
        rows.iter()
            .any(|(s, r, t, _)| s == "src/b.rs:caller" && r == "references" && t == target)
    };
    let before = graph_projection_with_confidence(&db);
    assert!(
        refs_to(&before, "src/a.rs:Widget"),
        "precondition: the signature's type mention is a `references` edge: {before:?}"
    );

    // One run, one file: c.rs appears with a second `Widget`. b.rs is
    // byte-identical and is never opened by this run.
    fs::write(src.join("c.rs"), "pub struct Widget { pub b: i32 }\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();

    let inc = graph_projection_with_confidence(&db);
    let full = graph_projection_with_confidence(&control);
    assert!(
        refs_to(&full, "src/a.rs:Widget") && refs_to(&full, "src/c.rs:Widget"),
        "control: a rebuild fans the bare type mention out to both definitions: {full:?}"
    );
    assert!(
        refs_to(&inc, "src/c.rs:Widget"),
        "a `references` edge fans out by bare name exactly as a `calls` edge does, so the \
         fan-out round must cover it too: {inc:?}"
    );
    assert_eq!(
        inc, full,
        "incremental edge set diverged from a rebuild of the same tree"
    );
}

/// Node count for one file path — the observable for "was this re-extracted,
/// and did its symbol set stay the same".
fn node_count_of(db: &Database, path: &str) -> i64 {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM nodes n JOIN files f ON f.id = n.file_id WHERE f.path = ?1",
            [path],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn a_second_fanout_round_finds_nothing_to_do() {
    // Termination for D#24's fan-out round, asserted rather than assumed — and
    // this is the SECOND version of this test, because the first one was vacuous
    // and two independent pre-ship reviewers caught it.
    //
    // The claim: round two re-extracts files whose symbol sets it does not
    // change, so no name's definition count can rise again and a hypothetical
    // round three would have an empty trigger set. Production runs the round
    // exactly once per `index_files` call, so nothing loops — but "runs once"
    // only describes a fixed point if that claim holds; otherwise it is hiding
    // an unfinished repair.
    //
    // The first version drove `run_incremental_index` twice and asserted the
    // second run indexed nothing. That never reached the round: an unchanged
    // tree has an empty diff, so `fanout_possible` is false and BOTH the
    // snapshot and the round are skipped. Its assertions were held up by the
    // empty-diff guard and would have passed with
    // `fan_out_to_new_duplicate_definitions` deleted outright. A reviewer
    // confirmed it with an instrumented run: `dirty_seed=0 fanout_possible=false`.
    //
    // So this version states the claim directly. It takes the count snapshot
    // over the CALLER paths — the path set a round three would actually have —
    // and asserts the recount yields nobody.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    // b.py is both the bare-name CALLER the round pulls in and the DEFINER of a
    // second duplicated name (`shared`, also in e.py) that an outside file
    // (d.py) calls bare. Without that second role the assertion below cannot
    // fail for any reason: re-extracting a file nobody references leaves no
    // candidate for the recount to return, so a broken count predicate would
    // still produce an empty set and the test would pass vacuously — which is
    // the trap this test fell into twice already.
    fs::write(src.join("a.py"), "def helper():\n    pass\n").unwrap();
    fs::write(
        src.join("b.py"),
        "def caller():\n    helper()\n\n\ndef shared():\n    pass\n",
    )
    .unwrap();
    fs::write(src.join("e.py"), "def shared():\n    pass\n").unwrap();
    fs::write(src.join("d.py"), "def user():\n    shared()\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(
        graph_projection_with_confidence(&db)
            .iter()
            .any(|(s, r, t, c)| s == "src/d.py:user"
                && r == REL_CALLS
                && t == "src/b.py:shared"
                && c == "ambiguous"),
        "precondition: d.py holds an ambiguous bare call into b.py, so a wrongly \
         risen `shared` would surface d.py in the recount"
    );

    fs::write(src.join("c.py"), "def helper():\n    pass\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    let after_first = graph_projection_with_confidence(&db);
    assert!(
        after_first
            .iter()
            .any(|(s, r, t, _)| s == "src/b.py:caller" && r == REL_CALLS && t == "src/c.py:helper"),
        "precondition: the fan-out round ran and created the edge: {after_first:?}"
    );

    // Round three, by hand, over round two's own path set — and it must really
    // RE-EXTRACT, not merely re-run. A second `run_incremental_index` here would
    // find an unchanged tree and do nothing at all, which makes "no name rose"
    // true for the wrong reason; that is the shape of the vacuity described
    // above, one level deeper. So `index_files` is driven directly on the caller
    // path, exactly as the round drives it, and the recount is taken across that
    // re-extraction.
    let callers = vec!["src/b.py".to_string()];
    super::resolve::snapshot_definition_counts(db.conn(), &callers).unwrap();
    let before_ids = node_count_of(&db, "src/b.py");
    index_files(
        &db,
        project_dir.path(),
        &callers,
        &std::collections::HashMap::new(),
        None,
        &[],
        None,
    )
    .unwrap();
    assert!(
        before_ids > 0 && node_count_of(&db, "src/b.py") == before_ids,
        "precondition: b.py was re-extracted and still holds the same symbols"
    );
    let next = super::resolve::bare_name_callers_of_new_duplicates(db.conn(), &Default::default())
        .unwrap();
    assert!(
        next.is_empty(),
        "the fan-out round is not a fixed point — a third round would re-extract {next:?}"
    );

    assert_eq!(
        graph_projection_with_confidence(&db),
        after_first,
        "a second pass moved the graph"
    );
}

#[test]
fn a_name_moving_across_languages_still_enters_the_drift_scope() {
    // Regression test for a divergence found in pre-ship review 2026-09-08.
    //
    // `cg_namecount` — the input Phase 2e classifies from — is keyed
    // (name, LANGUAGE), but `snapshot_scope_name_counts` /
    // `scope_names_from_count_drift` USED to count `GROUP BY n.name` alone. A run
    // that moves a name from a Python file to a JavaScript file inside its own
    // scope then left the name-only count unchanged (1 before, 1 after) while the
    // (helper, python) count really fell from 2 to 1, so `helper` never entered
    // `cg_scope_names`.
    //
    // The edge that has to be relabelled runs between two files the run never
    // opened, so neither file arm reaches it — the name arm is the only thing
    // that can. Both count queries are keyed (name, language) now; this asserts
    // the incremental result matches the rebuild, which it did not before.
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let src = project_dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.py"), "def helper():\n    pass\n").unwrap();
    fs::write(src.join("b.py"), "def caller():\n    helper()\n").unwrap();
    fs::write(src.join("x.py"), "def helper():\n    pass\n").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    let edge_of = |rows: &[(String, String, String, String)]| -> Option<String> {
        rows.iter()
            .find(|(s, r, t, _)| s == "src/b.py:caller" && r == REL_CALLS && t == "src/a.py:helper")
            .map(|(_, _, _, c)| c.clone())
    };
    let before = graph_projection_with_confidence(&db);
    assert_eq!(
        edge_of(&before).as_deref(),
        Some("ambiguous"),
        "precondition: two python `helper`s, so the cross-file call is ambiguous: {before:?}"
    );

    // ONE run, two files: x.py loses `helper`, x.js gains one. Name-only count
    // over this run's paths: `helper` = 1 before, 1 after — no drift observed.
    fs::write(src.join("x.py"), "def other():\n    pass\n").unwrap();
    fs::write(src.join("x.js"), "function helper() {}\n").unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    let control_dir = TempDir::new().unwrap();
    let control = Database::open(&control_dir.path().join("index.db")).unwrap();
    run_full_index(&control, project_dir.path(), None, None).unwrap();

    let inc = graph_projection_with_confidence(&db);
    let full = graph_projection_with_confidence(&control);
    assert_eq!(
        edge_of(&full).as_deref(),
        Some("inferred"),
        "control: a rebuild sees one python `helper` and labels the edge inferred: {full:?}"
    );
    assert_eq!(
        edge_of(&inc).as_deref(),
        Some("inferred"),
        "the python `helper` count fell 2 -> 1, so the untouched b.py -> a.py edge must be \
         reclassified; if this fails, the drift snapshot has stopped keying on \
         (name, language) and `helper` is no longer entering cg_scope_names: {inc:?}"
    );
}

/// Stamp the index as built by a NEWER `INDEX_VERSION` than this binary — the
/// state a long-lived server from before an upgrade finds after the upgraded
/// binary rebuilt the index under it.
fn stamp_index_as_newer(db_path: &std::path::Path) {
    let db = Database::open(db_path).unwrap();
    db.conn()
        .pragma_update(None, "application_id", crate::domain::INDEX_VERSION + 1)
        .unwrap();
}

/// An older binary must not write into an index a newer one owns. The open path
/// already refused to WIPE it ("an older binary must not clobber a newer
/// index"), but then went on indexing: every file it touched was re-parsed with
/// the older grammar and stored under the newer version stamp, and the newer
/// binary never re-parses an unchanged file. Reproduced 2026-09-25 with a
/// 0.153.0 binary (v71) against a 0.156.0 index (v72): a function after a
/// `for r in &raw {}` loop vanished from the index and stayed gone after the
/// newer binary ran again.
#[test]
fn an_older_binary_does_not_write_into_a_newer_index() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("index.db");
    fs::write(project_dir.path().join("a.rs"), "fn first_fn() {}\n").unwrap();
    {
        let db = Database::open(&db_path).unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();
    }
    stamp_index_as_newer(&db_path);

    let db = Database::open(&db_path).unwrap();
    assert!(
        db.newer_index_version().is_some(),
        "precondition: this handle sees a newer index"
    );

    fs::write(
        project_dir.path().join("a.rs"),
        "fn first_fn() {}\nfn second_fn() {}\n",
    )
    .unwrap();
    let incremental = run_incremental_index(&db, project_dir.path(), None, None);
    assert!(
        incremental.is_err(),
        "an incremental run must refuse, not write an older parse under the newer stamp"
    );
    let full = run_full_index(&db, project_dir.path(), None, None);
    assert!(full.is_err(), "a full run must refuse too");
    assert!(
        get_nodes_by_name(db.conn(), "second_fn")
            .unwrap()
            .is_empty(),
        "nothing this binary parsed may reach the newer index"
    );
    assert_eq!(
        get_nodes_by_name(db.conn(), "first_fn").unwrap().len(),
        1,
        "the newer binary's data must be left as it was"
    );
}

/// A file listed as parse-damaged by a verdict THIS binary did not produce is
/// re-parsed on the next incremental run, even though its content is unchanged.
///
/// The damage is simulated the way an older binary leaves it: the file's hash
/// matches the tree, a symbol is missing, and the file is named in
/// `parse_error_files`. `control.rs` is damaged identically but NOT listed, so
/// a pass here cannot come from a run that happens to re-parse everything.
/// `broken.rs` really does fail to parse: once this binary has confirmed that
/// verdict, it must not be re-parsed on every run.
#[test]
fn a_parse_damage_verdict_from_another_binary_is_re_examined() {
    use crate::storage::queries::set_meta;
    use crate::storage::schema::META_KEY_PARSE_ERROR_FILES;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    fs::write(
        project_dir.path().join("hurt.rs"),
        "fn hurt_a() {}\nfn hurt_b() {}\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("control.rs"),
        "fn ctl_a() {}\nfn ctl_b() {}\n",
    )
    .unwrap();
    fs::write(
        project_dir.path().join("broken.rs"),
        "fn broken( {\nfn after_broken() {}\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        db.parse_error_files().unwrap(),
        vec!["broken.rs".to_string()],
        "precondition: only broken.rs really fails to parse"
    );

    // The older binary's leftovers.
    db.conn()
        .execute("DELETE FROM nodes WHERE name IN ('hurt_b', 'ctl_b')", [])
        .unwrap();
    set_meta(
        db.conn(),
        META_KEY_PARSE_ERROR_FILES,
        r#"["broken.rs","hurt.rs"]"#,
    )
    .unwrap();

    let healed = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        get_nodes_by_name(db.conn(), "hurt_b").unwrap().len(),
        1,
        "hurt.rs is named by a verdict this binary never produced, so it must be re-parsed"
    );
    assert!(
        get_nodes_by_name(db.conn(), "ctl_b").unwrap().is_empty(),
        "control: an unlisted, unchanged file must not be re-parsed"
    );
    assert_eq!(healed.files_indexed, 1, "only hurt.rs is re-examined");
    assert_eq!(
        db.parse_error_files().unwrap(),
        vec!["broken.rs".to_string()],
        "hurt.rs parses clean now; broken.rs keeps its verdict"
    );

    let settled = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        settled.files_indexed, 0,
        "a verdict this binary produced itself is not re-examined on every run"
    );
}

/// An index built before the verified-verdict key existed names its damaged
/// files with no record of which binary parsed them; every one is re-examined
/// once. This is the path that repairs indexes already written by an older
/// binary before this fix shipped.
#[test]
fn an_index_without_verified_verdicts_re_examines_every_listed_file_once() {
    use crate::storage::queries::delete_meta;
    use crate::storage::schema::META_KEY_PARSE_ERROR_FILES_VERIFIED;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    fs::write(
        project_dir.path().join("broken.rs"),
        "fn broken( {\nfn after_broken() {}\n",
    )
    .unwrap();
    fs::write(project_dir.path().join("fine.rs"), "fn fine() {}\n").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    delete_meta(db.conn(), META_KEY_PARSE_ERROR_FILES_VERIFIED).unwrap();

    let first = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        first.files_indexed, 1,
        "broken.rs's unverified verdict is re-examined"
    );
    let second = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(second.files_indexed, 0, "and then it is verified");
    assert_eq!(
        db.parse_error_files().unwrap(),
        vec!["broken.rs".to_string()]
    );
}

/// The check has to read the index as it is NOW. A handle opened while the
/// index was current keeps a version read at open; the newer binary stamping
/// the index afterwards is exactly the case this guard exists for.
#[test]
fn a_handle_opened_before_the_index_became_newer_stops_writing() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("index.db");
    fs::write(project_dir.path().join("a.rs"), "fn first_fn() {}\n").unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    stamp_index_as_newer(&db_path);
    fs::write(
        project_dir.path().join("a.rs"),
        "fn first_fn() {}\nfn second_fn() {}\n",
    )
    .unwrap();
    assert!(
        run_incremental_index(&db, project_dir.path(), None, None).is_err(),
        "the same handle must refuse once the index on disk is newer"
    );
    assert!(get_nodes_by_name(db.conn(), "second_fn")
        .unwrap()
        .is_empty());
}

/// Direction matters: an index built by an OLDER version is owed a rebuild, and
/// until then a refresh through a reader handle still writes the current parse.
#[test]
fn a_refresh_over_an_older_index_still_writes() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("index.db");
    fs::write(project_dir.path().join("a.rs"), "fn first_fn() {}\n").unwrap();
    {
        let db = Database::open(&db_path).unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();
        db.conn()
            .pragma_update(None, "application_id", crate::domain::INDEX_VERSION - 1)
            .unwrap();
    }
    let reader = Database::open_nondestructive(&db_path).unwrap();
    assert!(reader.newer_index_version().is_none());
    fs::write(
        project_dir.path().join("a.rs"),
        "fn first_fn() {}\nfn second_fn() {}\n",
    )
    .unwrap();
    assert!(ensure_file_indexed(&reader, project_dir.path(), "a.rs", None).unwrap());
    assert_eq!(
        get_nodes_by_name(reader.conn(), "second_fn").unwrap().len(),
        1
    );
}

/// Write the verdict set a different binary would leave: `hurt.rs` listed as
/// damaged with a symbol missing, its hash matching the tree.
fn simulate_foreign_damage(db: &Database, listed: &[&str], missing_symbol: &str) {
    use crate::storage::queries::set_meta;
    use crate::storage::schema::META_KEY_PARSE_ERROR_FILES;
    db.conn()
        .execute("DELETE FROM nodes WHERE name = ?1", [missing_symbol])
        .unwrap();
    set_meta(
        db.conn(),
        META_KEY_PARSE_ERROR_FILES,
        &serde_json::to_string(listed).unwrap(),
    )
    .unwrap();
}

/// A verified set another binary wrote is not this binary's word. The format
/// 0.157's first cut used (a bare array) carries no version, and a set stamped
/// with a different INDEX_VERSION came from a different parser.
#[test]
fn a_verified_set_from_another_version_is_not_trusted() {
    use crate::storage::queries::set_meta;
    use crate::storage::schema::META_KEY_PARSE_ERROR_FILES_VERIFIED;

    for foreign in [
        r#"["broken.rs","hurt.rs"]"#.to_string(),
        serde_json::json!({
            "v": crate::domain::INDEX_VERSION + 1,
            "files": {
                "broken.rs": "x",
                "hurt.rs": crate::indexer::merkle::hash_bytes(b"fn hurt_a() {}\nfn hurt_b() {}\n"),
            }
        })
        .to_string(),
        serde_json::json!({
            "v": crate::domain::INDEX_VERSION - 1,
            "files": {
                "broken.rs": "x",
                "hurt.rs": crate::indexer::merkle::hash_bytes(b"fn hurt_a() {}\nfn hurt_b() {}\n"),
            }
        })
        .to_string(),
    ] {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        fs::write(
            project_dir.path().join("hurt.rs"),
            "fn hurt_a() {}\nfn hurt_b() {}\n",
        )
        .unwrap();
        fs::write(
            project_dir.path().join("broken.rs"),
            "fn broken( {\nfn after_broken() {}\n",
        )
        .unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();
        simulate_foreign_damage(&db, &["broken.rs", "hurt.rs"], "hurt_b");
        set_meta(db.conn(), META_KEY_PARSE_ERROR_FILES_VERIFIED, &foreign).unwrap();

        run_incremental_index(&db, project_dir.path(), None, None).unwrap();
        assert_eq!(
            get_nodes_by_name(db.conn(), "hurt_b").unwrap().len(),
            1,
            "a verified set from another version must not vouch for hurt.rs: {foreign}"
        );
    }
}

/// A verdict is about one content. A binary that does not maintain the verified
/// set can re-index an edited file and store its own parse; the stored hash
/// then moves, and this binary's old verdict must stop vouching for the file.
#[test]
fn a_verified_verdict_does_not_survive_a_content_change() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    fs::write(
        project_dir.path().join("broken.rs"),
        "fn broken( {\nfn after_broken() {}\n",
    )
    .unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();
    assert!(db.unverified_parse_error_files().unwrap().is_empty());

    // Another binary re-indexes an edited, still-broken file: new hash stored,
    // and its parse lacks `later_fn`.
    let edited = "fn broken( {\nfn after_broken() {}\nfn later_fn() {}\n";
    fs::write(project_dir.path().join("broken.rs"), edited).unwrap();
    db.conn()
        .execute(
            "UPDATE files SET blake3_hash = ?1 WHERE path = 'broken.rs'",
            [crate::indexer::merkle::hash_bytes(edited.as_bytes())],
        )
        .unwrap();
    assert!(get_nodes_by_name(db.conn(), "later_fn").unwrap().is_empty());

    let run = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(run.files_indexed, 1, "the verdict was for the old content");
    assert_eq!(get_nodes_by_name(db.conn(), "later_fn").unwrap().len(), 1);
}

/// When the main set stops naming a file, its verified entry goes with it, so a
/// later re-listing by another binary is not vouched for by a stale entry.
#[test]
fn a_verified_entry_leaves_with_its_verdict() {
    use crate::storage::queries::delete_meta;
    use crate::storage::schema::META_KEY_PARSE_ERROR_FILES;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    fs::write(
        project_dir.path().join("broken.rs"),
        "fn broken( {\nfn after_broken() {}\n",
    )
    .unwrap();
    fs::write(project_dir.path().join("other.rs"), "fn other() {}\n").unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Another binary's fold dropped broken.rs from the main set.
    delete_meta(db.conn(), META_KEY_PARSE_ERROR_FILES).unwrap();
    // A run of ours that parses something else folds both sets.
    fs::write(
        project_dir.path().join("other.rs"),
        "fn other() {}\nfn other2() {}\n",
    )
    .unwrap();
    run_incremental_index(&db, project_dir.path(), None, None).unwrap();

    // Another binary lists broken.rs again, now with a symbol missing.
    simulate_foreign_damage(&db, &["broken.rs"], "after_broken");
    let run = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
    assert_eq!(
        run.files_indexed, 1,
        "the old verified entry must not vouch for the new listing"
    );
}

/// D#132: the Rust files of a two-package workspace whose calls go through
/// `use`. `other` depends on `my-crate` (spelled `my_crate` in paths).
fn d132_workspace() -> Vec<(&'static str, &'static str)> {
    let same = "pub struct Semaphore;\nimpl Semaphore {\n    pub fn new(n: u8) -> Semaphore { Semaphore }\n}\n";
    vec![
        ("Cargo.toml", "[workspace]\nmembers = [\"mycrate\", \"other\"]\n"),
        (
            "mycrate/Cargo.toml",
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "mycrate/src/lib.rs",
            "pub mod sync;\npub mod time;\nmod util;\nmod a;\nmod a2;\nmod a3;\nmod a4;\nmod a5;\n\
             mod a6;\nmod a7;\nmod b;\nmod blk;\nmod re;\nmod m;\npub mod deep;\npub mod shallow;\n\
             mod mem;\n",
        ),
        (
            "mycrate/src/sync/mod.rs",
            "mod mutex;\npub use mutex::Mutex;\npub mod oneshot;\npub mod broadcast;\npub mod batch;\n\
             pub mod semaphore;\npub fn new(v: u8) {}\n",
        ),
        (
            "mycrate/src/sync/mutex.rs",
            "pub struct Mutex;\nimpl Mutex {\n    pub fn new(v: u8) -> Mutex { Mutex }\n}\n",
        ),
        ("mycrate/src/sync/oneshot.rs", "pub fn channel() {}\n"),
        ("mycrate/src/sync/broadcast.rs", "pub fn channel() {}\n"),
        ("mycrate/src/sync/batch.rs", same),
        ("mycrate/src/sync/semaphore.rs", same),
        (
            "mycrate/src/time.rs",
            "pub struct Instant;\nimpl Instant {\n    pub fn now() -> Instant { Instant }\n}\n",
        ),
        ("mycrate/src/util.rs", "pub fn tempdir() {}\n"),
        // A project file named like the std module a `use` names.
        ("mycrate/src/mem.rs", "pub fn swap(a: u8, b: u8) {}\n"),
        (
            "mycrate/src/a.rs",
            "use std::sync::Mutex;\nuse std::time::Instant;\nuse std::mem::swap;\n\
             fn std_mutex() {\n    Mutex::new(1);\n}\nfn std_instant() {\n    Instant::now();\n}\n\
             fn std_swap() {\n    swap(1, 2);\n}\n",
        ),
        (
            "mycrate/src/a2.rs",
            "use std::sync;\nfn std_module() {\n    sync::Mutex::new(1);\n}\n",
        ),
        (
            "mycrate/src/a3.rs",
            "use crate::sync;\nfn own_module() {\n    sync::Mutex::new(1);\n}\n",
        ),
        (
            "mycrate/src/a4.rs",
            "use crate::sync::batch::Semaphore;\nfn own_semaphore() {\n    Semaphore::new(1);\n}\n",
        ),
        (
            "mycrate/src/a5.rs",
            "use crate::sync::oneshot::channel as oneshot_channel;\n\
             fn renamed() {\n    oneshot_channel();\n}\n",
        ),
        (
            "mycrate/src/a6.rs",
            "use crate::sync::batch::Semaphore;\nfn glob_block() {\n    use crate::sync::semaphore::*;\n    \
             Semaphore::new(1);\n}\n",
        ),
        (
            "mycrate/src/a7.rs",
            "use crate::deep::Thing;\nfn rank() {\n    Thing::make(1);\n}\n",
        ),
        ("mycrate/src/deep/mod.rs", "mod thing;\npub use thing::Thing;\n"),
        (
            "mycrate/src/deep/thing.rs",
            "pub struct Thing;\nimpl Thing {\n    pub fn make(n: u8) {}\n}\n",
        ),
        (
            "mycrate/src/shallow/thing.rs",
            "pub struct Thing;\nimpl Thing {\n    pub fn make(n: u8) {}\n}\n",
        ),
        ("mycrate/src/shallow/mod.rs", "mod thing;\n"),
        (
            "mycrate/src/b.rs",
            "use crate::sync::Mutex;\nfn own_file_level() {\n    Mutex::new(1);\n}\n\
             mod tests {\n    use std::sync::Mutex;\n    fn std_in_tests() {\n        Mutex::new(1);\n    }\n}\n\
             mod tests2 {\n    use super::*;\n    fn inherited() {\n        Mutex::new(1);\n    }\n}\n",
        ),
        (
            "mycrate/src/blk.rs",
            "fn block_use() {\n    use std::sync::Mutex;\n    Mutex::new(1);\n}\n",
        ),
        (
            "mycrate/src/re.rs",
            "pub use std::sync::Mutex;\nfn reexport_std() {\n    Mutex::new(1);\n}\n",
        ),
        (
            "mycrate/src/m.rs",
            "use tempfile::tempdir;\nfn in_macro() {\n    assert!(tempdir() == ());\n}\n",
        ),
        (
            "mycrate/tests/it.rs",
            "use my_crate::sync::Mutex;\nfn it_type() {\n    Mutex::new(1);\n}\n\
             mod support {\n    pub(crate) mod helpers;\n}\nuse support::helpers;\n\
             fn it_support() {\n    helpers::assist();\n}\n",
        ),
        ("mycrate/tests/support/helpers.rs", "pub fn assist() {}\n"),
        (
            "other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "other/src/lib.rs",
            "mod chan;\nmod local;\nextern crate my_crate as mc;\nuse my_crate::sync::oneshot::channel;\n\
             use my_crate::sync::{oneshot, broadcast};\nuse my_crate::sync::Mutex;\n\
             use tempfile::tempdir;\nuse mc::sync::oneshot as os2;\n\
             fn ws_bare() {\n    channel();\n}\nfn ws_module() {\n    oneshot::channel();\n}\n\
             fn ws_type() {\n    Mutex::new(1);\n}\nfn foreign() {\n    tempdir();\n}\n\
             fn via_extern_alias() {\n    os2::channel();\n}\n",
        ),
        (
            "other/src/chan.rs",
            "pub fn channel() {}\npub fn tempdir() {}\npub fn assist() {}\n",
        ),
        (
            "other/src/local.rs",
            "use my_crate::sync;\nfn sync() {}\nfn local_fn() {\n    sync();\n}\n",
        ),
    ]
}

/// D#132, the accepted shapes: a call through a name a `use` binds resolves
/// through the path the `use` writes. Each row names the caller, the edges it
/// must have, and the edges the name alone used to give it.
///
/// Deliberately left as before (no row): a glob other than `use super::*`
/// (`use a::b::*` proves nothing about a name), a path opening with a name no
/// `use` binds (`io::Error::new` with no `use`), `crate::`/`self::`/`super::`
/// written in the call itself, and a bare call of a project item under its own
/// name (resolved through the import edge, as before).
#[test]
fn test_rust_calls_resolve_through_use() {
    const MUTEX_NEW: &str = "mycrate/src/sync/mutex.rs.new";
    const ONESHOT: &str = "mycrate/src/sync/oneshot.rs.channel";
    const BROADCAST: &str = "mycrate/src/sync/broadcast.rs.channel";
    #[rustfmt::skip]
    let rows: &[(&str, &[&str], &[&str])] = &[
        // Foreign roots bind no project item.
        ("mycrate/src/a.rs.std_mutex", &[], &[MUTEX_NEW]),
        ("mycrate/src/a.rs.std_instant", &[], &["mycrate/src/time.rs.now"]),
        ("mycrate/src/a.rs.std_swap", &[], &["mycrate/src/mem.rs.swap"]),
        ("mycrate/src/a2.rs.std_module", &[], &[MUTEX_NEW]),
        ("mycrate/src/blk.rs.block_use", &[], &[MUTEX_NEW]),
        ("mycrate/src/re.rs.reexport_std", &[], &[MUTEX_NEW]),
        ("mycrate/src/m.rs.in_macro", &[], &["mycrate/src/util.rs.tempdir"]),
        ("other/src/lib.rs.foreign", &[], &["other/src/chan.rs.tempdir", "mycrate/src/util.rs.tempdir"]),
        // Project roots bind the item the path names.
        ("mycrate/src/a3.rs.own_module", &[MUTEX_NEW], &[]),
        ("mycrate/src/a4.rs.own_semaphore", &["mycrate/src/sync/batch.rs.new"], &["mycrate/src/sync/semaphore.rs.new"]),
        ("mycrate/src/a5.rs.renamed", &[ONESHOT], &[BROADCAST]),
        // A re-export: the item whose module shares the most of the path.
        ("mycrate/src/a7.rs.rank", &["mycrate/src/deep/thing.rs.make"], &["mycrate/src/shallow/thing.rs.make"]),
        // Scope: a `use` binds in its own module or block.
        ("mycrate/src/b.rs.own_file_level", &[MUTEX_NEW], &[]),
        ("mycrate/src/b.rs.std_in_tests", &[], &[MUTEX_NEW]),
        ("mycrate/src/b.rs.inherited", &[MUTEX_NEW], &[]),
        // Another package of the workspace, by its crate name.
        ("other/src/lib.rs.ws_bare", &[ONESHOT], &[BROADCAST, "other/src/chan.rs.channel"]),
        ("other/src/lib.rs.ws_module", &[ONESHOT], &[BROADCAST, "other/src/chan.rs.channel"]),
        ("other/src/lib.rs.ws_type", &[MUTEX_NEW], &[
            "mycrate/src/sync/mod.rs.new", "mycrate/src/sync/batch.rs.new", "mycrate/src/sync/semaphore.rs.new",
        ]),
        ("other/src/lib.rs.via_extern_alias", &[ONESHOT], &[BROADCAST, "other/src/chan.rs.channel"]),
        ("mycrate/tests/it.rs.it_type", &[MUTEX_NEW], &[]),
        // A test crate's module in a subdirectory.
        ("mycrate/tests/it.rs.it_support", &["mycrate/tests/support/helpers.rs.assist"], &["other/src/chan.rs.assist"]),
        // A local item of the call's namespace beats a `use` of the name.
        ("other/src/local.rs.local_fn", &["other/src/local.rs.sync"], &[]),
        // A glob between the call and a `use` may be where the name comes from:
        // resolved by name, as before.
        ("mycrate/src/a6.rs.glob_block", &["mycrate/src/sync/semaphore.rs.new"], &[]),
    ];
    let (_p, _d, db) = fresh_index_of(&d132_workspace());
    let edges = edge_set(&db);
    let mut bad = Vec::new();
    for (caller, want, wrong) in rows {
        let calls: Vec<&str> = edges
            .iter()
            .filter_map(|e| e.strip_prefix(&format!("{caller} --calls--> ")))
            .collect();
        for w in *want {
            if !calls.contains(w) {
                bad.push(format!("{caller}: missing {w} (has {calls:?})"));
            }
        }
        for w in *wrong {
            if calls.contains(w) {
                bad.push(format!("{caller}: wrong {w}"));
            }
        }
        if want.is_empty() && !calls.is_empty() {
            bad.push(format!("{caller}: a foreign item's call bound {calls:?}"));
        }
    }
    // The import of a crate-name path binds the file the path names.
    for (edge, want) in [
        (
            "other/src/lib.rs.<module> --imports--> mycrate/src/sync/oneshot.rs.channel",
            true,
        ),
        (
            "other/src/lib.rs.<module> --imports--> other/src/chan.rs.channel",
            false,
        ),
        (
            "other/src/lib.rs.<module> --imports--> mycrate/src/sync/broadcast.rs.channel",
            false,
        ),
    ] {
        if edges.iter().any(|e| e == edge) != want {
            bad.push(format!("import {edge}: want {want}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// Index `before`, apply `after` (None deletes), index incrementally, and
/// require every edge (with metadata and confidence for calls) a fresh index of
/// the result has. Returns the rebuild's edge set for the caller's control
/// assertions.
fn assert_all_edges_match_rebuild(
    before: &[(&str, &str)],
    after: &[(&str, Option<&str>)],
) -> Vec<String> {
    let (project, _d, db) = fresh_index_of(before);
    let mut tree: Vec<(String, String)> = before
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_string()))
        .collect();
    for (path, body) in after {
        tree.retain(|(p, _)| p != path);
        match body {
            Some(b) => {
                let full = project.path().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, b).unwrap();
                tree.push((path.to_string(), b.to_string()));
            }
            None => fs::remove_file(project.path().join(path)).unwrap(),
        }
    }
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let files: Vec<(&str, &str)> = tree.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let (_p2, _d2, control) = fresh_index_of(&files);
    assert_eq!(
        call_edges_with_confidence(&db),
        call_edges_with_confidence(&control),
        "calls after {after:?} must equal a rebuild"
    );
    let want = edge_set(&control);
    assert_eq!(
        edge_set(&db),
        want,
        "edges after {after:?} must equal a rebuild"
    );
    want
}

/// D#132 parity: each rule that reads a `use` gives an incremental run the
/// edges a rebuild gives, when the caller changes and when the crate the `use`
/// names gains or loses the item.
#[test]
fn test_rust_use_anchoring_incremental_matches_rebuild() {
    fn tree(wa: &'static str, wc: &'static str) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"mycrate\", \"other\"]\n",
            ),
            (
                "mycrate/Cargo.toml",
                "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "mycrate/src/lib.rs",
                "pub mod wa;\npub mod wc;\npub mod sync;\n",
            ),
            ("mycrate/src/wa.rs", wa),
            ("mycrate/src/wc.rs", wc),
            (
                "mycrate/src/sync.rs",
                "pub struct Mutex;\nimpl Mutex {\n    pub fn new(v: u8) -> Mutex { Mutex }\n}\n",
            ),
            (
                "mycrate/src/user.rs",
                "use crate::sync::Mutex;\nfn own() {\n    Mutex::new(1);\n}\n",
            ),
            (
                "other/Cargo.toml",
                "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "other/src/lib.rs",
                "mod own;\nmod modpath;\nuse my_crate::wa::widget;\nfn go() {\n    widget();\n}\n",
            ),
            ("other/src/own.rs", "pub fn widget() {}\n"),
            // No import names `widget` here: only the call's own resolution
            // (the pending buffer, the re-export fan-out) can follow it.
            (
                "other/src/modpath.rs",
                "use my_crate::wa;\nfn go_module() {\n    wa::widget();\n}\n",
            ),
        ]
    }
    const WA_WITHOUT: &str = "pub fn other() {}\n";
    const WA_WITH: &str = "pub fn other() {}\npub fn widget() {}\n";
    const WC_WITH: &str = "pub fn widget() {}\n";
    const WC_WITHOUT: &str = "pub fn unrelated() {}\n";
    let go_to = |edges: &[String], file: &str| {
        edges.contains(&format!("other/src/lib.rs.go --calls--> {file}.widget"))
            && edges.contains(&format!(
                "other/src/modpath.rs.go_module --calls--> {file}.widget"
            ))
    };

    // The named module gains the item: the call moves off the other module's.
    let want = assert_all_edges_match_rebuild(
        &tree(WA_WITHOUT, WC_WITH),
        &[("mycrate/src/wa.rs", Some(WA_WITH))],
    );
    assert!(
        go_to(&want, "mycrate/src/wa.rs") && !go_to(&want, "mycrate/src/wc.rs"),
        "{want:#?}"
    );
    // ...and loses it again.
    let want = assert_all_edges_match_rebuild(
        &tree(WA_WITH, WC_WITH),
        &[("mycrate/src/wa.rs", Some(WA_WITHOUT))],
    );
    assert!(go_to(&want, "mycrate/src/wc.rs"), "{want:#?}");
    // No such item in the crate yet (only another package's), then the named
    // module gains it: through the import, and through a module path the
    // import names no item for (`wa::widget()`: only the pending buffer).
    let want = assert_all_edges_match_rebuild(
        &tree(WA_WITHOUT, WC_WITHOUT),
        &[("mycrate/src/wa.rs", Some(WA_WITH))],
    );
    assert!(go_to(&want, "mycrate/src/wa.rs"), "{want:#?}");
    // Another module of the crate gains it while the named one has none.
    let want = assert_all_edges_match_rebuild(
        &tree(WA_WITHOUT, WC_WITHOUT),
        &[("mycrate/src/wc.rs", Some(WC_WITH))],
    );
    assert!(go_to(&want, "mycrate/src/wc.rs"), "{want:#?}");
    // The same through `crate::`, which bound the importer to the `<external>`
    // sentinel for good: its call stayed pruned until a rebuild.
    let single = |a: &'static str| -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "mod a;\nmod user;\n"),
            ("src/a.rs", a),
            (
                "src/user.rs",
                "use crate::a::widget;\nfn go() {\n    widget();\n}\n",
            ),
        ]
    };
    let want = assert_all_edges_match_rebuild(&single(WA_WITHOUT), &[("src/a.rs", Some(WA_WITH))]);
    assert!(
        want.contains(&"src/user.rs.go --calls--> src/a.rs.widget".to_string()),
        "{want:#?}"
    );
    // The caller's `use` turns to std's: the call leaves the project.
    let want = assert_all_edges_match_rebuild(
        &tree(WA_WITHOUT, WC_WITH),
        &[(
            "mycrate/src/user.rs",
            Some("use std::sync::Mutex;\nfn own() {\n    Mutex::new(1);\n}\n"),
        )],
    );
    assert!(
        !want
            .iter()
            .any(|e| e.starts_with("mycrate/src/user.rs.own --calls-->")),
        "{want:#?}"
    );
    // ...and back.
    let want = assert_all_edges_match_rebuild(
        &[
            tree(WA_WITHOUT, WC_WITH),
            vec![(
                "mycrate/src/user.rs",
                "use std::sync::Mutex;\nfn own() {\n    Mutex::new(1);\n}\n",
            )],
        ]
        .concat(),
        &[(
            "mycrate/src/user.rs",
            Some("use crate::sync::Mutex;\nfn own() {\n    Mutex::new(1);\n}\n"),
        )],
    );
    assert!(
        want.contains(&"mycrate/src/user.rs.own --calls--> mycrate/src/sync.rs.new".to_string()),
        "{want:#?}"
    );
}

/// D#132: the re-export fan-out re-extracts a caller only when the new
/// definition could change its answer. Every caller of a re-exported
/// `Mutex::new` used to be re-extracted when any `new` appeared in the crate
/// (141 files on tokio for one `Probe::new`).
#[test]
fn test_rust_reexport_fanout_only_for_an_admissible_definition() {
    let tree =
        |extra: (&'static str, &'static str)| -> Vec<(&'static str, &'static str)> {
            vec![
            ("Cargo.toml", "[workspace]\nmembers = [\"mycrate\", \"other\"]\n"),
            (
                "mycrate/Cargo.toml",
                "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("mycrate/src/lib.rs", "pub mod sync;\npub mod probe;\n"),
            ("mycrate/src/sync/mod.rs", "mod mutex;\npub use mutex::Mutex;\n"),
            (
                "mycrate/src/sync/mutex.rs",
                "pub struct Mutex;\nimpl Mutex {\n    pub fn new(v: u8) -> Mutex { Mutex }\n}\n",
            ),
            ("mycrate/src/probe.rs", "pub fn other() {}\n"),
            (
                "other/Cargo.toml",
                "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "other/src/lib.rs",
                // Through the module, so no import names `Mutex` (an import's own
                // fan-out would re-extract the file whatever this one decides).
                "use my_crate::sync;\nfn go() {\n    sync::Mutex::new(1);\n}\n",
            ),
            extra,
        ]
        };
    let base = tree(("mycrate/src/unused.rs", "\n"));
    let refresh_from = |base: &[(&str, &str)], path: &str, body: &str| -> Vec<String> {
        let (project, _d, db) = fresh_index_of(base);
        assert!(
            edge_set(&db).contains(
                &"other/src/lib.rs.go --calls--> mycrate/src/sync/mutex.rs.new".to_string()
            ),
            "precondition: the call binds the re-exported Mutex::new"
        );
        let paths = vec![path.to_string()];
        super::resolve::snapshot_definition_counts(db.conn(), &paths).unwrap();
        let full = project.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, body).unwrap();
        index_files(
            &db,
            project.path(),
            &paths,
            &std::collections::HashMap::new(),
            None,
            &[],
            None,
        )
        .unwrap();
        super::resolve::bare_name_callers_of_new_duplicates(
            db.conn(),
            &super::resolve::collect_rust_crates(project.path()),
        )
        .unwrap()
    };
    let refresh = |path: &str, body: &str| refresh_from(&base, path, body);
    // Another type's `new`: the path `sync::Mutex` cannot reach it.
    const PROBE: &str =
        "pub fn other() {}\npub struct Probe;\nimpl Probe {\n    pub fn new(v: u8) -> Probe { Probe }\n}\n";
    let unrelated = refresh("mycrate/src/probe.rs", PROBE);
    assert!(unrelated.is_empty(), "{unrelated:?}");
    // A `Mutex::new` as close to the path as the bound one: the answer changes.
    let second = "pub struct Mutex;\nimpl Mutex {\n    pub fn new(v: u8) -> Mutex { Mutex }\n}\n";
    // Bound to two re-exports at once (`ambiguous`), the call is still judged by
    // its path, not re-extracted for every new `new` as a bare-name guess is.
    let tied = tree(("mycrate/src/sync/rival.rs", second));
    let unrelated = refresh_from(&tied, "mycrate/src/probe.rs", PROBE);
    assert!(unrelated.is_empty(), "{unrelated:?}");
    let rival = refresh("mycrate/src/sync/rival.rs", second);
    assert_eq!(rival, vec!["other/src/lib.rs".to_string()]);
    assert_all_edges_match_rebuild(&base, &[("mycrate/src/sync/rival.rs", Some(second))]);
}

/// (caller file.fn, callee file.qualified name) of every `calls` edge.
fn calls_by_qualified_name(db: &Database) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT fs.path || '.' || ns.name || ' -> ' || ft.path || '.' \
                 || COALESCE(nt.qualified_name, nt.name) \
             FROM edges e \
             JOIN nodes ns ON ns.id = e.source_id JOIN files fs ON fs.id = ns.file_id \
             JOIN nodes nt ON nt.id = e.target_id JOIN files ft ON ft.id = nt.file_id \
             WHERE e.relation = 'calls' ORDER BY 1",
        )
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.filter_map(Result::ok).collect()
}

/// The project types D#112's corpus calls into: same-named methods on several
/// types, so a call resolved by its name alone binds the wrong one.
const D112_TYPES: &str = "use std::sync::atomic::AtomicU16;\nuse std::sync::Arc;\n\
pub struct Loader;\n\
impl Loader {\n    pub fn load(&self, o: u8) -> u8 { o }\n}\n\
pub struct Direction;\n\
impl Direction {\n    pub fn as_str(&self) -> &str { \"\" }\n}\n\
pub struct Savepoint;\n\
impl Savepoint {\n    pub fn commit(self) {}\n}\n\
pub struct Buf;\n\
impl Buf {\n    pub fn put_slice(&mut self, s: &[u8]) {}\n    pub fn path(&self) {}\n}\n\
pub struct Widget;\n\
impl Widget {\n    pub fn new() -> Widget { Widget }\n    pub fn spin(&self) {}\n}\n\
pub struct Gadget;\n\
impl Gadget {\n    pub fn spin(&self) {}\n}\n\
pub trait Ext {\n    fn helper(&self) {}\n}\n\
impl Ext for String {}\n\
pub struct Scale;\n\
impl Scale {\n    pub fn weigh(&self) -> u8 { 0 }\n}\n\
pub trait Weigh {\n    fn weigh(&self) -> u8;\n}\n\
impl Weigh for AtomicU16 {\n    fn weigh(&self) -> u8 { 1 }\n}\n\
pub struct File;\n\
impl File {\n    pub fn sync_all(&self) {}\n}\n\
cfg_net! {\n    pub struct Stream;\n}\n\
impl Stream {\n    pub fn peek(&self) {}\n}\n\
pub trait Spinner {\n    fn whirl(&self);\n}\n\
impl Spinner for Arc<Widget> {\n    fn whirl(&self) {}\n}\n\
impl Gadget {\n    pub fn whirl(&self) {}\n}\n";

fn d112_tree() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "mod types;\nmod foreign;\nmod own;\nmod untyped;\nmod sync;\nmod loom;\nmod anchored;\n"),
        ("src/sync/mod.rs", "pub mod mutex;\npub use mutex::Mutex;\n"),
        (
            "src/sync/mutex.rs",
            "pub struct Mutex;\nimpl Mutex {\n    pub fn lock(&self) {}\n}\n",
        ),
        ("src/loom/mod.rs", "mod std_mutex;\npub use std_mutex::Mutex;\n"),
        (
            "src/loom/std_mutex.rs",
            "pub struct Mutex;\nimpl Mutex {\n    pub fn lock(&self) {}\n}\n",
        ),
        (
            "src/anchored.rs",
            "use crate::loom::Mutex;\nuse crate::sync::mutex::Mutex as AsyncMutex;\n\
             fn via_reexport(m: &Mutex) {\n    m.lock();\n}\n\
             fn named_module(m: &AsyncMutex) {\n    m.lock();\n}\n\
             struct Holder {\n    m: Mutex,\n}\n\
             impl Holder {\n    fn field_reexport(&self) {\n        self.m.lock();\n    }\n}\n",
        ),
        ("src/types.rs", D112_TYPES),
        (
            "src/foreign.rs",
            "use std::sync::atomic::{AtomicU8, AtomicU16};\nuse std::sync::Arc;\n\
             use bytes::BytesMut;\nuse tempfile::{tempdir, NamedTempFile};\n\
             use rusqlite::Transaction;\n\
             static COUNT: AtomicU8 = AtomicU8::new(0);\n\
             struct Dir;\nimpl Dir {\n    fn as_str(&self) -> &str { \"\" }\n}\n\
             struct Holder {\n    flag: AtomicU8,\n    inner: Inner,\n    name: String,\n    shared: Arc<AtomicU8>,\n}\n\
             struct Inner {\n    flag: AtomicU8,\n}\n\
             fn tempfile() -> NamedTempFile {\n    NamedTempFile::new().unwrap()\n}\n\
             fn let_annotated() {\n    let a: AtomicU8 = make();\n    a.load(1);\n}\n\
             fn let_ctor() {\n    let a = AtomicU8::new(0);\n    a.load(1);\n}\n\
             fn param_ref(a: &AtomicU8) {\n    a.load(1);\n}\n\
             fn param_mut(mut a: &mut AtomicU8) {\n    a.load(1);\n}\n\
             fn arc_param(a: Arc<AtomicU8>) {\n    a.load(1);\n}\n\
             fn arc_ctor() {\n    let a = Arc::new(AtomicU8::new(0));\n    a.load(1);\n}\n\
             fn static_recv() {\n    COUNT.load(1);\n}\n\
             fn prelude_string() {\n    let s = String::new();\n    s.as_str();\n}\n\
             fn string_param(s: &String) {\n    s.as_str();\n}\n\
             fn format_macro() {\n    let s = format!(\"x\");\n    s.as_str();\n}\n\
             fn to_string_call(n: u8) {\n    let s = n.to_string();\n    s.as_str();\n}\n\
             fn primitive_param(n: u8) {\n    n.as_str();\n}\n\
             fn crate_ctor() {\n    let mut b = BytesMut::new();\n    b.put_slice(b\"x\");\n}\n\
             fn local_fn_return() {\n    let t = tempfile();\n    t.path();\n}\n\
             fn foreign_fn_unwrap() {\n    let d = tempdir().unwrap();\n    d.path();\n}\n\
             fn foreign_fn_try() -> Result<(), ()> {\n    let d = tempdir()?;\n    d.path();\n    Ok(())\n}\n\
             fn crate_zero_arg() {\n    let d = tempfile::Builder::make();\n    d.path();\n}\n\
             fn crate_param(tx: Transaction<'_>) {\n    tx.commit();\n}\n\
             fn trait_default_kept() {\n    let s = String::new();\n    s.helper();\n}\n\
             fn impl_for_foreign() {\n    let a = AtomicU16::new(0);\n    a.weigh();\n}\n\
             impl Holder {\n    fn self_field(&self) {\n        self.flag.load(1);\n    }\n    \
             fn self_chain(&self) {\n        self.inner.flag.load(1);\n    }\n    \
             fn self_string(&self) {\n        self.name.as_str();\n    }\n    \
             fn self_arc(&self) {\n        self.shared.load(1);\n    }\n}\n\
             fn local_field(h: &Holder) {\n    h.flag.load(1);\n}\n\
             fn std_file(f: &std::fs::File) {\n    f.sync_all();\n}\n\
             fn macro_owner(a: &AtomicU8) {\n    a.peek();\n}\n",
        ),
        (
            "src/own.rs",
            "use crate::types::{Widget, Gadget};\nuse crate::types::Widget as W;\n\
             struct Holder2 {\n    w: Widget,\n}\n\
             fn own_ctor() {\n    let w = Widget::new();\n    w.spin();\n}\n\
             fn own_annotated() {\n    let w: Widget = make();\n    w.spin();\n}\n\
             fn own_param(w: &Widget) {\n    w.spin();\n}\n\
             fn own_literal() {\n    let g = Gadget {};\n    g.spin();\n}\n\
             fn own_renamed() {\n    let w = W::new();\n    w.spin();\n}\n\
             fn own_shadowed() {\n    let w = Gadget {};\n    let w = Widget::new();\n    w.spin();\n}\n\
             impl Holder2 {\n    fn own_field(&self) {\n        self.w.spin();\n    }\n}\n\
             struct Queue {\n    head: Option<Widget>,\n}\n\
             impl Queue {\n    fn own_peeled(&mut self) -> Option<()> {\n        let w = self.head?;\n        w.spin();\n        None\n    }\n}\n\
             fn arc_own(w: std::sync::Arc<Widget>) {\n    w.spin();\n}\n\
             fn arc_via(w: std::sync::Arc<Widget>) {\n    w.whirl();\n}\n",
        ),
        (
            "src/untyped.rs",
            "use std::sync::atomic::AtomicU8;\n\
             trait Tr {}\n\
             fn closure_param(v: Vec<u8>) {\n    let a = AtomicU8::new(0);\n    v.iter().for_each(|a| { a.load(1); });\n}\n\
             fn generic_param<T: Tr>(a: T) {\n    a.load(1);\n}\n\
             fn impl_trait(a: impl Tr) {\n    a.load(1);\n}\n\
             fn dyn_trait(a: &dyn Tr) {\n    a.load(1);\n}\n\
             fn shadowed_untyped() {\n    let a = AtomicU8::new(0);\n    let a = pick();\n    a.load(1);\n}\n\
             fn if_let(o: Option<u8>) {\n    let a = AtomicU8::new(0);\n    if let Some(a) = o {\n        a.load(1);\n    }\n}\n\
             fn method_return(h: &Holder) {\n    let a = h.get();\n    a.load(1);\n}\n\
             fn destructured() {\n    let (a, b) = pair();\n    a.load(1);\n}\n\
             fn crate_builder() {\n    let b = mycrate::types::Widget::builder();\n    b.spin();\n}\n\
             fn generic_shadow<Gadget: Tr>(g: Gadget) {\n    g.spin();\n}\n",
        ),
    ]
}

/// D#112, the accepted shapes: a Rust method call whose receiver's type the
/// source writes down binds only what that type can run. A std or foreign
/// type (`AtomicU8`, `String`, `BytesMut`) runs no method of another project
/// struct; a project type runs its own. Each row: the caller, the edges it
/// must have, the edges it must not.
///
/// The `untyped.rs` rows are the receivers deliberately left untyped (see
/// `rust_receiver.rs`): they resolve by name as before, so the project's one
/// `load` taking `self` and one argument is still what they bind.
#[test]
fn test_rust_receiver_types_by_shape() {
    const LOAD: &str = "src/types.rs.Loader.load";
    // Same file: `as_str` is too common a name to bind across files.
    const AS_STR: &str = "src/foreign.rs.Dir.as_str";
    const PUT: &str = "src/types.rs.Buf.put_slice";
    const PATH: &str = "src/types.rs.Buf.path";
    const COMMIT: &str = "src/types.rs.Savepoint.commit";
    const W_SPIN: &str = "src/types.rs.Widget.spin";
    const G_SPIN: &str = "src/types.rs.Gadget.spin";
    #[rustfmt::skip]
    let rows: &[(&str, &[&str], &[&str])] = &[
        // std / foreign receivers bind no other project type's method.
        ("src/foreign.rs.let_annotated", &[], &[LOAD]),
        ("src/foreign.rs.let_ctor", &[], &[LOAD]),
        ("src/foreign.rs.param_ref", &[], &[LOAD]),
        ("src/foreign.rs.param_mut", &[], &[LOAD]),
        ("src/foreign.rs.arc_param", &[], &[LOAD]),
        ("src/foreign.rs.arc_ctor", &[], &[LOAD]),
        ("src/foreign.rs.static_recv", &[], &[LOAD]),
        ("src/foreign.rs.prelude_string", &[], &[AS_STR]),
        ("src/foreign.rs.string_param", &[], &[AS_STR]),
        ("src/foreign.rs.format_macro", &[], &[AS_STR]),
        ("src/foreign.rs.to_string_call", &[], &[AS_STR]),
        ("src/foreign.rs.primitive_param", &[], &[AS_STR]),
        ("src/foreign.rs.crate_ctor", &[], &[PUT]),
        ("src/foreign.rs.local_fn_return", &[], &[PATH]),
        ("src/foreign.rs.foreign_fn_unwrap", &[], &[PATH]),
        ("src/foreign.rs.foreign_fn_try", &[], &[PATH]),
        ("src/foreign.rs.crate_zero_arg", &[], &[PATH]),
        ("src/foreign.rs.crate_param", &[], &[COMMIT]),
        ("src/foreign.rs.self_field", &[], &[LOAD]),
        ("src/foreign.rs.self_chain", &[], &[LOAD]),
        ("src/foreign.rs.self_string", &[], &[AS_STR]),
        ("src/foreign.rs.self_arc", &[], &[LOAD]),
        ("src/foreign.rs.local_field", &[], &[LOAD]),
        // A std type named like a project struct is not that struct, and a
        // type an item macro defines (no node) is still the project's.
        ("src/foreign.rs.std_file", &[], &["src/types.rs.File.sync_all"]),
        ("src/foreign.rs.macro_owner", &[], &["src/types.rs.Stream.peek"]),
        // A project trait's method still runs on a std type, and so does a
        // project impl's for that very type.
        ("src/foreign.rs.trait_default_kept", &["src/types.rs.Ext.helper"], &[]),
        ("src/foreign.rs.impl_for_foreign", &["src/types.rs.AtomicU16.weigh"], &["src/types.rs.Scale.weigh"]),
        // A project receiver binds its own type's method only.
        ("src/own.rs.own_ctor", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.own_annotated", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.own_param", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.own_literal", &[G_SPIN], &[W_SPIN]),
        ("src/own.rs.own_renamed", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.own_shadowed", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.own_field", &[W_SPIN], &[G_SPIN]),
        // `Option<T>` peeled by `?`; behind `Arc`, the argument's methods and
        // the pointer's own impls.
        ("src/own.rs.own_peeled", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.arc_own", &[W_SPIN], &[G_SPIN]),
        ("src/own.rs.arc_via", &["src/types.rs.Arc.whirl"], &["src/types.rs.Gadget.whirl"]),
        // Same-named project types: the one the `use` path names, through a
        // re-export (the module sharing most of the path) or directly.
        ("src/anchored.rs.via_reexport", &["src/loom/std_mutex.rs.Mutex.lock"], &["src/sync/mutex.rs.Mutex.lock"]),
        ("src/anchored.rs.named_module", &["src/sync/mutex.rs.Mutex.lock"], &["src/loom/std_mutex.rs.Mutex.lock"]),
        ("src/anchored.rs.field_reexport", &["src/loom/std_mutex.rs.Mutex.lock"], &["src/sync/mutex.rs.Mutex.lock"]),
        // Left untyped: resolved by name, as before.
        ("src/untyped.rs.closure_param", &[LOAD], &[]),
        ("src/untyped.rs.generic_param", &[LOAD], &[]),
        ("src/untyped.rs.impl_trait", &[LOAD], &[]),
        ("src/untyped.rs.dyn_trait", &[LOAD], &[]),
        ("src/untyped.rs.shadowed_untyped", &[LOAD], &[]),
        ("src/untyped.rs.if_let", &[LOAD], &[]),
        ("src/untyped.rs.method_return", &[LOAD], &[]),
        ("src/untyped.rs.destructured", &[LOAD], &[]),
        // A workspace crate's `T::f()` may return anything (a builder): not a
        // `T`, so not `T`'s own `spin` (two `spin`s: resolved by name, none).
        ("src/untyped.rs.crate_builder", &[], &["src/types.rs.Widget.spin"]),
        // A generic parameter is whatever instantiates it, even when a project
        // type shares its name (two `spin`s: resolved by name, none).
        ("src/untyped.rs.generic_shadow", &[], &["src/types.rs.Gadget.spin"]),
    ];
    let (_p, _d, db) = fresh_index_of(&d112_tree());
    let edges = calls_by_qualified_name(&db);
    let mut bad = Vec::new();
    for (caller, want, wrong) in rows {
        let calls: Vec<&str> = edges
            .iter()
            .filter_map(|e| e.strip_prefix(&format!("{caller} -> ")))
            .collect();
        for w in *want {
            if !calls.contains(w) {
                bad.push(format!("{caller}: missing {w} (has {calls:?})"));
            }
        }
        for w in *wrong {
            if calls.contains(w) {
                bad.push(format!("{caller}: wrong {w}"));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// D#112 parity: every input a receiver type is read from gives an incremental
/// run the edges a rebuild gives: the caller's `let` and its file's struct
/// fields (read in the caller's own file), and the methods other files define
/// (read at resolution: the type gaining or losing its own method, a project
/// method appearing for a std receiver).
#[test]
fn test_rust_receiver_types_incremental_matches_rebuild() {
    fn tree(
        caller: &'static str,
        widget: &'static str,
        other: &'static str,
    ) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "mod widget;\nmod other;\nmod caller;\n"),
            ("src/widget.rs", widget),
            ("src/other.rs", other),
            ("src/caller.rs", caller),
        ]
    }
    const CALLER: &str = "use std::sync::atomic::AtomicU8;\nuse crate::widget::Widget;\n\
        struct Holder {\n    flag: AtomicU8,\n}\n\
        fn go() {\n    let w = Widget::new();\n    w.spin();\n}\n\
        fn atomic() {\n    let a = AtomicU8::new(0);\n    a.load(1);\n}\n\
        impl Holder {\n    fn field(&self) {\n        self.flag.load(1);\n    }\n}\n";
    const WIDGET: &str =
        "pub struct Widget;\nimpl Widget {\n    pub fn new() -> Widget { Widget }\n}\n";
    const WIDGET_SPIN: &str = "pub struct Widget;\nimpl Widget {\n    pub fn new() -> Widget { Widget }\n    pub fn spin(&self) {}\n}\n";
    const ONE_SPIN: &str = "pub struct Gadget;\nimpl Gadget {\n    pub fn spin(&self) {}\n}\n";
    const TWO_SPINS: &str = "pub struct Gadget;\nimpl Gadget {\n    pub fn spin(&self) {}\n}\n\
        pub struct Gizmo;\nimpl Gizmo {\n    pub fn spin(&self) {}\n}\n";
    const LOADER: &str =
        "pub struct Loader;\nimpl Loader {\n    pub fn load(&self, o: u8) -> u8 { o }\n}\n";
    let has = |edges: &[String], e: &str| edges.iter().any(|x| x == e);
    const GO_W: &str = "src/caller.rs.go --calls--> src/widget.rs.spin";
    const GO_O: &str = "src/caller.rs.go --calls--> src/other.rs.spin";

    // The type gains its own method: the call moves off the other type's.
    let want = assert_all_edges_match_rebuild(
        &tree(CALLER, WIDGET, ONE_SPIN),
        &[("src/widget.rs", Some(WIDGET_SPIN))],
    );
    assert!(has(&want, GO_W) && !has(&want, GO_O), "{want:#?}");
    // ...while two other types define it (the untyped call bound neither).
    let want = assert_all_edges_match_rebuild(
        &tree(CALLER, WIDGET, TWO_SPINS),
        &[("src/widget.rs", Some(WIDGET_SPIN))],
    );
    assert!(has(&want, GO_W) && !has(&want, GO_O), "{want:#?}");
    // An unrelated edit runs the pending sweep over the buffered call while two
    // other types still define it: it stays unbound, as a rebuild leaves it.
    let want = assert_all_edges_match_rebuild(
        &tree(CALLER, WIDGET, TWO_SPINS),
        &[(
            "src/lib.rs",
            Some("mod widget;\nmod other;\nmod caller;\n// touched\n"),
        )],
    );
    assert!(
        !want
            .iter()
            .any(|e| e.starts_with("src/caller.rs.go --calls--> src/other.rs")),
        "{want:#?}"
    );
    // ...and loses it again: resolved by name as before.
    let want = assert_all_edges_match_rebuild(
        &tree(CALLER, WIDGET_SPIN, ONE_SPIN),
        &[("src/widget.rs", Some(WIDGET))],
    );
    assert!(has(&want, GO_O), "{want:#?}");
    // A project method a std receiver cannot run appears: still no edge.
    let want = assert_all_edges_match_rebuild(
        &tree(CALLER, WIDGET, ONE_SPIN),
        &[("src/other.rs", Some(LOADER))],
    );
    assert!(
        !want
            .iter()
            .any(|e| e.starts_with("src/caller.rs.atomic --calls-->")
                || e.starts_with("src/caller.rs.field --calls-->")),
        "{want:#?}"
    );
    // The caller's `let` and its struct's field change type: the receiver
    // becomes the project's `Loader`.
    let retyped = CALLER
        .replace(
            "let a = AtomicU8::new(0);",
            "let a = crate::other::Loader {};",
        )
        .replace("flag: AtomicU8,", "flag: crate::other::Loader,");
    let retyped: &'static str = Box::leak(retyped.into_boxed_str());
    let want = assert_all_edges_match_rebuild(
        &tree(CALLER, WIDGET, LOADER),
        &[("src/caller.rs", Some(retyped))],
    );
    assert!(
        has(&want, "src/caller.rs.atomic --calls--> src/other.rs.load")
            && has(&want, "src/caller.rs.field --calls--> src/other.rs.load"),
        "{want:#?}"
    );
    // ...and back to std's.
    let want = assert_all_edges_match_rebuild(
        &tree(retyped, WIDGET, LOADER),
        &[("src/caller.rs", Some(CALLER))],
    );
    assert!(
        !has(&want, "src/caller.rs.atomic --calls--> src/other.rs.load")
            && !has(&want, "src/caller.rs.field --calls--> src/other.rs.load"),
        "{want:#?}"
    );

    // Same-named types: the one the `use` names (through a re-export) gains
    // its method while the other has one, then loses it again.
    let anchored = |loom: &'static str| -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "mod sync;\nmod loom;\nmod caller;\n"),
            ("src/sync/mod.rs", "pub mod mutex;\npub use mutex::Mutex;\n"),
            (
                "src/sync/mutex.rs",
                "pub struct Mutex;\nimpl Mutex {\n    pub fn lock(&self) {}\n}\n",
            ),
            (
                "src/loom/mod.rs",
                "mod std_mutex;\npub use std_mutex::Mutex;\n",
            ),
            ("src/loom/std_mutex.rs", loom),
            (
                "src/caller.rs",
                "use crate::loom::Mutex;\nfn go(m: &Mutex) {\n    m.lock();\n}\n",
            ),
        ]
    };
    const LOOM: &str = "pub struct Mutex;\n";
    const LOOM_LOCK: &str = "pub struct Mutex;\nimpl Mutex {\n    pub fn lock(&self) {}\n}\n";
    let want = assert_all_edges_match_rebuild(
        &anchored(LOOM),
        &[("src/loom/std_mutex.rs", Some(LOOM_LOCK))],
    );
    assert!(
        has(
            &want,
            "src/caller.rs.go --calls--> src/loom/std_mutex.rs.lock"
        ) && !has(&want, "src/caller.rs.go --calls--> src/sync/mutex.rs.lock"),
        "{want:#?}"
    );
    let want = assert_all_edges_match_rebuild(
        &anchored(LOOM_LOCK),
        &[("src/loom/std_mutex.rs", Some(LOOM))],
    );
    assert!(
        has(&want, "src/caller.rs.go --calls--> src/sync/mutex.rs.lock"),
        "{want:#?}"
    );
}

/// A typed call on a struct of its own file that lacks the method binds only
/// what could run. Whether the struct has a `Deref` is read from other files,
/// and the call may then have bound nothing and left no row behind (`clone` is
/// a name the default chain drops): an incremental run that adds or removes
/// the `Deref`, or the struct's own method, must still give a rebuild's edges.
#[test]
fn test_rust_receiver_runnable_candidates_incremental_matches_rebuild() {
    fn tree(caller: &'static str, other: &'static str) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "mod caller;\nmod other;\n"),
            ("src/caller.rs", caller),
            ("src/other.rs", other),
        ]
    }
    let has = |edges: &[String], e: &str| edges.iter().any(|x| x == e);
    const NOTHING: &str = "pub struct Unrelated;\n";
    const DEREF: &str = "impl std::ops::Deref for crate::caller::Inner {\n\
        type Target = crate::caller::Token;\n\
        fn deref(&self) -> &crate::caller::Token { unimplemented!() }\n}\n";
    const OWN_CLONE: &str = "impl Clone for crate::caller::Inner {\n\
        fn clone(&self) -> Self { crate::caller::Inner }\n}\n";
    for (method, token_impl) in [
        ("poke", "impl Token {\n    pub fn poke(&self) {}\n}\n"),
        (
            "clone",
            "impl Clone for Token {\n    fn clone(&self) -> Self { Token }\n}\n",
        ),
    ] {
        // `Inner` has no `method`; `Token`, in the same file, does.
        let caller: &'static str = Box::leak(
            format!(
                "pub struct Inner;\npub struct Token;\n{token_impl}\
                 pub struct Holder {{ inner: Inner }}\n\
                 impl Holder {{\n    pub fn go(&self) {{ self.inner.{method}(); }}\n}}\n"
            )
            .into_boxed_str(),
        );
        let to_token = format!("src/caller.rs.go --calls--> src/caller.rs.{method}");
        // `Inner` gains a `Deref` in another file: `Token`'s method may run now.
        let want = assert_all_edges_match_rebuild(
            &tree(caller, NOTHING),
            &[("src/other.rs", Some(DEREF))],
        );
        assert!(has(&want, &to_token), "{method}: {want:#?}");
        // ...and loses it: no other type's method runs on an `Inner`.
        let want = assert_all_edges_match_rebuild(
            &tree(caller, DEREF),
            &[("src/other.rs", Some(NOTHING))],
        );
        assert!(!has(&want, &to_token), "{method}: {want:#?}");
    }
    // `Inner` gains its own `poke` through a split impl in another file: the
    // call, which bound nothing and left no row, now binds it; and back.
    let caller: &'static str = "pub struct Inner;\npub struct Token;\n\
        impl Token {\n    pub fn poke(&self) {}\n}\n\
        pub struct Holder { inner: Inner }\n\
        impl Holder {\n    pub fn go(&self) { self.inner.poke(); }\n}\n";
    const OWN_POKE: &str = "impl crate::caller::Inner {\n    pub fn poke(&self) {}\n}\n";
    let want =
        assert_all_edges_match_rebuild(&tree(caller, NOTHING), &[("src/other.rs", Some(OWN_POKE))]);
    assert!(
        has(&want, "src/caller.rs.go --calls--> src/other.rs.poke"),
        "{want:#?}"
    );
    assert!(
        !has(&want, "src/caller.rs.go --calls--> src/caller.rs.poke"),
        "{want:#?}"
    );
    let want =
        assert_all_edges_match_rebuild(&tree(caller, OWN_POKE), &[("src/other.rs", Some(NOTHING))]);
    assert!(
        !want
            .iter()
            .any(|e| e.starts_with("src/caller.rs.go --calls-->")),
        "{want:#?}"
    );
    // `Inner` gains its own `clone` in another file, and loses it again: never
    // `Token`'s. (Its own is cross-file, where the default chain drops the name
    // `clone` as it always has; the parity is the point.)
    let caller: &'static str = "pub struct Inner;\npub struct Token;\n\
        impl Clone for Token {\n    fn clone(&self) -> Self { Token }\n}\n\
        pub struct Holder { inner: Inner }\n\
        impl Holder {\n    pub fn go(&self) { self.inner.clone(); }\n}\n";
    let want = assert_all_edges_match_rebuild(
        &tree(caller, NOTHING),
        &[("src/other.rs", Some(OWN_CLONE))],
    );
    let to_token = "src/caller.rs.go --calls--> src/caller.rs.clone";
    assert!(!has(&want, to_token), "{want:#?}");
    let want = assert_all_edges_match_rebuild(
        &tree(caller, OWN_CLONE),
        &[("src/other.rs", Some(NOTHING))],
    );
    assert!(!has(&want, to_token), "{want:#?}");
}

/// D#136: a package with both `src/lib.rs` and `src/main.rs` builds two crates.
/// `twin` declares `user`, `engine`, `deep` (inline, holding the file
/// `deep/inner.rs`) and `shared` from lib.rs; `cli` and `shared` from main.rs;
/// `orphan.rs` from neither. Each root re-exports an item the other root
/// defines under that name. `veiled`'s lib.rs declares `gated` inside a macro
/// the resolver cannot read; `pathy`'s main.rs declares `a.rs` through `#[path]`.
fn d136_twin_roots() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"twin\", \"veiled\", \"pathy\"]\n",
        ),
        (
            "twin/Cargo.toml",
            "[package]\nname = \"twin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "twin/src/lib.rs",
            "pub mod user;\npub mod shared;\nmod engine;\nmod deep {\n    pub mod inner;\n}\n\
             pub use engine::{helper, Cog};\npub fn run() {}\npub fn serve() {}\npub fn start() {}\n\
             pub struct Gear;\nimpl Gear {\n    pub fn new() -> Gear {\n        Gear\n    }\n}\n\
             mod t {\n    use crate::run;\n    use crate::helper;\n    fn lib_inline() {\n        run();\n    }\n\
             \x20   fn lib_reexport() {\n        helper();\n    }\n}\n",
        ),
        (
            "twin/src/main.rs",
            "mod cli;\nmod shared;\npub use cli::start;\nfn run() {}\npub fn helper() {}\nfn serve() {}\n\
             fn main() {}\nstruct Gear;\nimpl Gear {\n    fn new() -> Gear {\n        Gear\n    }\n}\n\
             struct Cog;\nimpl Cog {\n    fn spin() {}\n}\n\
             mod tests {\n    use crate::run;\n    use crate::start;\n    fn main_inline() {\n        run();\n    }\n\
             \x20   fn main_reexport() {\n        start();\n    }\n}\n",
        ),
        (
            "twin/src/user.rs",
            "use crate::run;\nuse crate::helper;\nuse crate::Gear;\nuse crate::Cog;\n\
             fn u_use() {\n    run();\n}\nfn u_reexport() {\n    helper();\n}\n\
             fn u_path() {\n    Gear::new();\n}\nfn u_reexport_path() {\n    Cog::spin();\n}\n",
        ),
        (
            "twin/src/engine.rs",
            "use super::run;\npub fn helper() {}\npub struct Cog;\nimpl Cog {\n    pub fn spin() {}\n}\n\
             fn e_super() {\n    run();\n}\n",
        ),
        (
            "twin/src/deep/inner.rs",
            "use crate::run;\nfn d_use() {\n    run();\n}\n",
        ),
        (
            "twin/src/shared.rs",
            "use crate::run;\nfn s_use() {\n    run();\n}\n",
        ),
        (
            "twin/src/orphan.rs",
            "use crate::run;\nfn o_use() {\n    run();\n}\n",
        ),
        (
            "twin/src/cli.rs",
            "use crate::run;\nuse crate::Gear;\nuse twin::serve;\npub fn start() {}\n\
             fn c_use() {\n    run();\n}\nfn c_ext() {\n    serve();\n}\nfn c_path() {\n    Gear::new();\n}\n",
        ),
        (
            "twin/tests/it.rs",
            "use twin::run;\nuse twin::helper;\nfn it_use() {\n    run();\n}\n\
             fn it_reexport() {\n    helper();\n}\n",
        ),
        (
            "veiled/Cargo.toml",
            "[package]\nname = \"veiled\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "veiled/src/lib.rs",
            "macro_rules! gate {\n    ($($i:item)*) => { $($i)* };\n}\ngate! {\n    pub mod gated;\n}\n\
             pub fn run() {}\n",
        ),
        ("veiled/src/main.rs", "mod gated;\nfn run() {}\nfn main() {}\n"),
        (
            "veiled/src/gated.rs",
            "use crate::run;\nfn g_use() {\n    run();\n}\n",
        ),
        (
            "pathy/Cargo.toml",
            "[package]\nname = \"pathy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("pathy/src/lib.rs", "pub mod a;\npub fn run() {}\n"),
        (
            "pathy/src/main.rs",
            "#[path = \"a.rs\"]\nmod alias;\nfn run() {}\nfn main() {}\n",
        ),
        ("pathy/src/a.rs", "use crate::run;\nfn p_use() {\n    run();\n}\n"),
    ]
}

/// D#136, the accepted shapes: `crate::` (and `super::`/`self::` reaching the
/// root) names the root of the crate the caller's file is compiled into, and a
/// package's crate name names its library.
///
/// A file's crate is read from the `mod` items at the top level of lib.rs and
/// main.rs: lib.rs and main.rs themselves are their own; a file under a module
/// only one root declares is that root's. A module both declare is compiled into
/// both crates, and one neither declares visibly (`#[path]`, no declaration, a
/// root with a `mod` inside a macro call) is unknown: both roots, as before.
#[test]
fn test_rust_crate_root_by_file_membership() {
    const LIB_RUN: &str = "twin/src/lib.rs.run";
    const MAIN_RUN: &str = "twin/src/main.rs.run";
    #[rustfmt::skip]
    let rows: &[(&str, &[&str], &[&str])] = &[
        // lib.rs's module tree.
        ("twin/src/user.rs.u_use", &[LIB_RUN], &[MAIN_RUN]),
        ("twin/src/engine.rs.e_super", &[LIB_RUN], &[MAIN_RUN]),
        ("twin/src/deep/inner.rs.d_use", &[LIB_RUN], &[MAIN_RUN]),
        ("twin/src/lib.rs.lib_inline", &[LIB_RUN], &[MAIN_RUN]),
        // A path through a type at the root, and through a re-exported one.
        ("twin/src/user.rs.u_path", &["twin/src/lib.rs.new"], &["twin/src/main.rs.new"]),
        ("twin/src/user.rs.u_reexport_path", &["twin/src/engine.rs.spin"], &["twin/src/main.rs.spin"]),
        // A re-export at lib.rs's root: found elsewhere in the library, never
        // in main.rs.
        ("twin/src/user.rs.u_reexport", &["twin/src/engine.rs.helper"], &["twin/src/main.rs.helper"]),
        ("twin/src/lib.rs.lib_reexport", &["twin/src/engine.rs.helper"], &["twin/src/main.rs.helper"]),
        // main.rs's module tree.
        ("twin/src/main.rs.main_inline", &[MAIN_RUN], &[LIB_RUN]),
        ("twin/src/cli.rs.c_use", &[MAIN_RUN], &[LIB_RUN]),
        ("twin/src/cli.rs.c_path", &["twin/src/main.rs.new"], &["twin/src/lib.rs.new"]),
        ("twin/src/main.rs.main_reexport", &["twin/src/cli.rs.start"], &["twin/src/lib.rs.start"]),
        // The package's name is its library, from main.rs's tree or a test.
        ("twin/src/cli.rs.c_ext", &["twin/src/lib.rs.serve"], &["twin/src/main.rs.serve"]),
        ("twin/tests/it.rs.it_use", &[LIB_RUN], &[MAIN_RUN]),
        ("twin/tests/it.rs.it_reexport", &["twin/src/engine.rs.helper"], &["twin/src/main.rs.helper"]),
        // Compiled into both crates, or unknown: both, as before.
        ("twin/src/shared.rs.s_use", &[LIB_RUN, MAIN_RUN], &[]),
        ("twin/src/orphan.rs.o_use", &[LIB_RUN, MAIN_RUN], &[]),
        ("veiled/src/gated.rs.g_use", &["veiled/src/lib.rs.run", "veiled/src/main.rs.run"], &[]),
        ("pathy/src/a.rs.p_use", &["pathy/src/lib.rs.run", "pathy/src/main.rs.run"], &[]),
    ];
    let (_p, _d, db) = fresh_index_of(&d136_twin_roots());
    let edges = edge_set(&db);
    let mut bad = Vec::new();
    for (caller, want, wrong) in rows {
        let calls: Vec<&str> = edges
            .iter()
            .filter_map(|e| e.strip_prefix(&format!("{caller} --calls--> ")))
            .collect();
        for w in *want {
            if !calls.contains(w) {
                bad.push(format!("{caller}: missing {w} (has {calls:?})"));
            }
        }
        for w in *wrong {
            if calls.contains(w) {
                bad.push(format!("{caller}: wrong {w}"));
            }
        }
    }
    // The `use` itself imports the same root's item.
    for (importer, target, want) in [
        ("twin/src/user.rs", LIB_RUN, true),
        ("twin/src/user.rs", MAIN_RUN, false),
        ("twin/src/cli.rs", MAIN_RUN, true),
        ("twin/src/cli.rs", LIB_RUN, false),
        ("twin/src/cli.rs", "twin/src/lib.rs.serve", true),
        ("twin/src/cli.rs", "twin/src/main.rs.serve", false),
        ("twin/tests/it.rs", LIB_RUN, true),
        ("twin/tests/it.rs", MAIN_RUN, false),
        ("twin/tests/it.rs", "twin/src/engine.rs.helper", true),
        ("twin/tests/it.rs", "twin/src/main.rs.helper", false),
        ("twin/src/shared.rs", LIB_RUN, true),
        ("twin/src/shared.rs", MAIN_RUN, true),
    ] {
        let edge = format!("{importer}.<module> --imports--> {target}");
        if edges.contains(&edge) != want {
            bad.push(format!("import {edge}: want {want}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// D#136 parity: a root gaining or losing a `mod` moves the files under that
/// module to another crate, and an incremental run re-resolves them as a
/// rebuild does, re-extracting only those files.
#[test]
fn test_rust_crate_root_membership_incremental_matches_rebuild() {
    const LIB: &str = "pub mod user;\npub fn run() {}\n";
    const MAIN: &str = "mod cli;\nfn run() {}\nfn main() {}\n";
    const MAIN_USER: &str = "mod cli;\nmod user;\nfn run() {}\nfn main() {}\n";
    let tree = |lib: &'static str, main: Option<&'static str>| {
        let mut t = vec![
            (
                "Cargo.toml",
                "[package]\nname = \"twin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", lib),
            ("src/user.rs", "use crate::run;\nfn u() {\n    run();\n}\n"),
            ("src/cli.rs", "use crate::run;\nfn c() {\n    run();\n}\n"),
        ];
        if let Some(m) = main {
            t.push(("src/main.rs", m));
        }
        t
    };
    let has = |edges: &[String], e: &str| edges.iter().any(|x| x == e);
    const U_LIB: &str = "src/user.rs.u --calls--> src/lib.rs.run";
    const U_MAIN: &str = "src/user.rs.u --calls--> src/main.rs.run";
    const C_LIB: &str = "src/cli.rs.c --calls--> src/lib.rs.run";

    // main.rs gains `mod user;`: user.rs is in both crates now.
    let want =
        assert_all_edges_match_rebuild(&tree(LIB, Some(MAIN)), &[("src/main.rs", Some(MAIN_USER))]);
    assert!(has(&want, U_LIB) && has(&want, U_MAIN), "{want:#?}");
    // ...and loses it.
    let want =
        assert_all_edges_match_rebuild(&tree(LIB, Some(MAIN_USER)), &[("src/main.rs", Some(MAIN))]);
    assert!(has(&want, U_LIB) && !has(&want, U_MAIN), "{want:#?}");
    // lib.rs drops `mod user;` while main.rs declares it: main's only.
    let want = assert_all_edges_match_rebuild(
        &tree(LIB, Some(MAIN_USER)),
        &[("src/lib.rs", Some("pub fn run() {}\n"))],
    );
    assert!(!has(&want, U_LIB) && has(&want, U_MAIN), "{want:#?}");
    // main.rs appears in a library, and goes away again.
    let want = assert_all_edges_match_rebuild(&tree(LIB, None), &[("src/main.rs", Some(MAIN))]);
    assert!(has(&want, U_LIB) && !has(&want, U_MAIN), "{want:#?}");
    let want = assert_all_edges_match_rebuild(&tree(LIB, Some(MAIN)), &[("src/main.rs", None)]);
    assert!(has(&want, U_LIB) && has(&want, C_LIB), "{want:#?}");
    // main.rs appears and claims `cli`, defining nothing it names: `cli.rs`
    // leaves the library, and only the record of the roots says so.
    let want = assert_all_edges_match_rebuild(
        &tree(LIB, None),
        &[("src/main.rs", Some("mod cli;\nfn main() {}\n"))],
    );
    assert!(has(&want, U_LIB) && !has(&want, C_LIB), "{want:#?}");
    // lib.rs hides its modules in a macro call: every file is unknown.
    let want = assert_all_edges_match_rebuild(
        &tree(LIB, Some(MAIN)),
        &[(
            "src/lib.rs",
            Some("macro_rules! m {\n    ($($i:item)*) => { $($i)* };\n}\nm! {\n    pub mod user;\n}\npub fn run() {}\n"),
        )],
    );
    assert!(has(&want, U_LIB) && has(&want, U_MAIN), "{want:#?}");
    // main.rs gains an item named like one lib.rs re-exports: the library's
    // `use` of it does not move there.
    let reexport = |main: &'static str| {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"twin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                "pub mod user;\nmod engine;\npub use engine::helper;\n",
            ),
            ("src/engine.rs", "pub fn helper() {}\n"),
            (
                "src/user.rs",
                "use crate::helper;\nfn u() {\n    helper();\n}\n",
            ),
            ("src/main.rs", main),
        ]
    };
    let want = assert_all_edges_match_rebuild(
        &reexport("fn main() {}\n"),
        &[("src/main.rs", Some("fn helper() {}\nfn main() {}\n"))],
    );
    assert!(
        has(&want, "src/user.rs.u --calls--> src/engine.rs.helper")
            && !has(&want, "src/user.rs.u --calls--> src/main.rs.helper"),
        "{want:#?}"
    );

    // Only the files a membership change moved are re-extracted, once: each
    // run records what it resolved against.
    let reindexed = |edits: &[&'static str]| -> Vec<usize> {
        let (project, _d, db) = fresh_index_of(&tree(LIB, Some(MAIN)));
        edits
            .iter()
            .map(|after| {
                fs::write(project.path().join("src/main.rs"), after).unwrap();
                run_incremental_index(&db, project.path(), None, None)
                    .unwrap()
                    .files_indexed
            })
            .collect()
    };
    const MAIN_EXTRA: &str = "mod cli;\nfn run() {}\nfn main() {}\nfn extra() {}\n";
    const MAIN_USER_EXTRA: &str = "mod cli;\nmod user;\nfn run() {}\nfn main() {}\nfn extra() {}\n";
    assert_eq!(reindexed(&[MAIN_EXTRA]), [1]);
    assert_eq!(reindexed(&[MAIN_USER, MAIN_USER_EXTRA]), [2, 1]);
}

/// Every entry point that indexes a file, for the parity tests below.
#[derive(Clone, Copy, Debug)]
enum IndexPath {
    /// `run_incremental_index` (the CLI, the server's startup drift check).
    Incremental,
    /// `run_incremental_index_cached` with the directory cache of the last
    /// scan (the file watcher).
    Cached,
    /// `ensure_file_indexed` per edited file (an MCP tool's `file_path`).
    Refresh,
    /// `resync_stale_files` over the edited files (a read command's result
    /// refresh).
    Resync,
}

/// Index `before`, apply each step of `steps` in turn through `path` alone,
/// and after each require the edges a rebuild of the tree then has; finally
/// run an ordinary incremental index and require them again. Returns the last
/// rebuild's edges.
fn assert_path_matches_rebuild(
    before: &[(&str, &str)],
    steps: &[&[(&str, Option<&str>)]],
    path: IndexPath,
) -> Vec<String> {
    path_vs_rebuild(before, steps, path, true)
}

/// [`assert_path_matches_rebuild`] on `calls` edges alone: for a step whose
/// other edges drift for a documented reason (a `references` edge a rebuild
/// binds to a type added elsewhere, CHANGELOG Not covered).
fn assert_path_calls_match_rebuild(
    before: &[(&str, &str)],
    steps: &[&[(&str, Option<&str>)]],
    path: IndexPath,
) -> Vec<String> {
    path_vs_rebuild(before, steps, path, false)
}

fn path_vs_rebuild(
    before: &[(&str, &str)],
    steps: &[&[(&str, Option<&str>)]],
    path: IndexPath,
    all_edges: bool,
) -> Vec<String> {
    let (project, _d, db) = fresh_index_of(before);
    let root = project.path();
    let (_, mut cache) = crate::indexer::merkle::scan_directory_cached(root, None).unwrap();
    let mut tree: Vec<(String, String)> = before
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_string()))
        .collect();
    let mut want = Vec::new();
    let mut want_calls = Vec::new();
    for after in steps {
        for (p, body) in after.iter() {
            tree.retain(|(q, _)| q != p);
            match body {
                Some(b) => {
                    let full = root.join(p);
                    fs::create_dir_all(full.parent().unwrap()).unwrap();
                    fs::write(full, b).unwrap();
                    tree.push((p.to_string(), b.to_string()));
                }
                None => fs::remove_file(root.join(p)).unwrap(),
            }
        }
        let edited: Vec<String> = after.iter().map(|(p, _)| p.to_string()).collect();
        match path {
            IndexPath::Incremental => {
                run_incremental_index(&db, root, None, None).unwrap();
            }
            IndexPath::Cached => {
                cache = run_incremental_index_cached(&db, root, None, Some(&cache), None)
                    .unwrap()
                    .1;
            }
            IndexPath::Refresh => {
                for p in &edited {
                    ensure_file_indexed(&db, root, p, None).unwrap();
                }
            }
            IndexPath::Resync => {
                let outcome = crate::indexer::resync::resync_stale_files(
                    &db,
                    root,
                    &edited,
                    64,
                    RefreshScope::IncludeNew,
                );
                assert!(!outcome.is_partial(), "{outcome:?}");
            }
        }
        let files: Vec<(&str, &str)> = tree.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
        let (_p2, _d2, control) = fresh_index_of(&files);
        want = edge_set(&control);
        want_calls = call_edges_with_confidence(&control);
        assert_eq!(
            call_edges_with_confidence(&db),
            want_calls,
            "{path:?}: calls after {after:?} must equal a rebuild"
        );
        if all_edges {
            assert_eq!(
                edge_set(&db),
                want,
                "{path:?}: edges after {after:?} must equal a rebuild"
            );
        }
    }
    run_incremental_index(&db, root, None, None).unwrap();
    if all_edges {
        assert_eq!(
            edge_set(&db),
            want,
            "{path:?}: edges after {steps:?} and an incremental run must equal a rebuild"
        );
    } else {
        assert_eq!(
            call_edges_with_confidence(&db),
            want_calls,
            "{path:?}: calls after {steps:?} and an incremental run must equal a rebuild"
        );
    }
    want
}

/// Batch-1 review B1: every path that indexes a file applies the same
/// re-extraction triggers, so each leaves the index equal to a rebuild: a
/// root's `mod` edit (D#136, which only the incremental run checked, so a
/// query's refresh of main.rs left `engine.rs` bound to the library for good),
/// a `use`'s module gaining the item (D#132), and a type's impl turning from
/// std's to the project's struct's (D#112).
#[test]
fn test_rust_reextraction_triggers_hold_on_every_indexing_path() {
    let twin = |main: &'static str| {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"twin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod user;\npub fn run() {}\n"),
            ("src/main.rs", main),
            ("src/user.rs", "use crate::run;\nfn u() {\n    run();\n}\n"),
            ("src/cli.rs", "use crate::run;\nfn c() {\n    run();\n}\n"),
        ]
    };
    const MAIN: &str = "mod cli;\nfn run() {}\nfn main() {}\n";
    const MAIN_USER: &str = "mod cli;\nmod user;\nfn run() {}\nfn main() {}\n";
    let uses = |wa: &'static str| {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod wa;\npub mod wc;\npub mod user;\n"),
            ("src/wa.rs", wa),
            ("src/wc.rs", "pub fn widget() {}\n"),
            (
                "src/user.rs",
                "use crate::wa::widget;\nfn go() {\n    widget();\n}\n",
            ),
        ]
    };
    let typed = |e: &'static str| {
        vec![
            (
                "Cargo.toml",
                "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod t;\npub mod x;\npub mod e;\n"),
            ("src/t.rs", "pub trait Ext {\n    fn ext(&self) {}\n}\n"),
            ("src/e.rs", e),
            (
                "src/x.rs",
                "use crate::t::Ext;\nuse std::time::Duration;\n\
                 pub fn g() {\n    let d: Duration = Duration::ZERO;\n    d.ext();\n}\n",
            ),
        ]
    };
    const E_OWN: &str = "use crate::t::Ext;\npub struct Duration;\n\
        impl Ext for Duration {\n    fn ext(&self) {}\n}\n";
    const E_STD: &str = "use crate::t::Ext;\nuse std::time::Duration;\n\
        impl Ext for Duration {\n    fn ext(&self) {}\n}\n";
    let has = |edges: &[String], e: &str| edges.iter().any(|x| x == e);
    for path in [
        IndexPath::Incremental,
        IndexPath::Cached,
        IndexPath::Refresh,
        IndexPath::Resync,
    ] {
        let want =
            assert_path_matches_rebuild(&twin(MAIN), &[&[("src/main.rs", Some(MAIN_USER))]], path);
        assert!(
            has(&want, "src/user.rs.u --calls--> src/main.rs.run")
                && has(&want, "src/user.rs.u --calls--> src/lib.rs.run"),
            "{path:?}: {want:#?}"
        );
        // ...and loses it again through the same path: the record of the
        // roots the first refresh stored is what tells the second one.
        let want = assert_path_matches_rebuild(
            &twin(MAIN),
            &[
                &[("src/main.rs", Some(MAIN_USER))],
                &[("src/main.rs", Some(MAIN))],
            ],
            path,
        );
        assert!(
            !has(&want, "src/user.rs.u --calls--> src/main.rs.run"),
            "{path:?}: {want:#?}"
        );
        let want = assert_path_matches_rebuild(
            &uses("pub fn other() {}\n"),
            &[&[("src/wa.rs", Some("pub fn other() {}\npub fn widget() {}\n"))]],
            path,
        );
        assert!(
            has(&want, "src/user.rs.go --calls--> src/wa.rs.widget")
                && !has(&want, "src/user.rs.go --calls--> src/wc.rs.widget"),
            "{path:?}: {want:#?}"
        );
        // std's `Duration` keeps no impl of the project's `Duration`: the
        // trait's default method is the one candidate. Once the impl is on
        // std's type, two methods answer and the call waits, as a rebuild's.
        let want =
            assert_path_matches_rebuild(&typed(E_OWN), &[&[("src/e.rs", Some(E_STD))]], path);
        assert!(
            !want.iter().any(|e| e.starts_with("src/x.rs.g --calls-->")),
            "{path:?}: {want:#?}"
        );
        let want =
            assert_path_matches_rebuild(&typed(E_STD), &[&[("src/e.rs", Some(E_OWN))]], path);
        assert!(
            has(&want, "src/x.rs.g --calls--> src/t.rs.ext")
                && !has(&want, "src/x.rs.g --calls--> src/e.rs.ext"),
            "{path:?}: {want:#?}"
        );
    }
}

/// Batch-1 review B2: a project type named like the std type a file uses does
/// not take that file's calls away from an impl on std's type. rustc calls
/// `impl Ext for std::time::Duration` on `std::time::Duration`; a rebuild
/// dropped the edge once any file defined `struct Duration`, and an incremental
/// run kept it, so the two disagreed.
#[test]
fn test_rust_std_receiver_keeps_its_impl_beside_a_same_named_project_type() {
    let tree = |y: Option<&'static str>| {
        let mut t = vec![
            (
                "Cargo.toml",
                "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "src/lib.rs",
                if y.is_some() {
                    "pub mod t;\npub mod x;\npub mod e;\npub mod y;\n"
                } else {
                    "pub mod t;\npub mod x;\npub mod e;\n"
                },
            ),
            ("src/t.rs", "pub trait Ext {\n    fn ext(&self);\n}\n"),
            (
                "src/e.rs",
                "impl crate::t::Ext for std::time::Duration {\n    fn ext(&self) {}\n}\n",
            ),
            (
                "src/x.rs",
                "use crate::t::Ext;\nuse std::time::Duration;\n\
                 pub fn g() {\n    let d: Duration = Duration::ZERO;\n    d.ext();\n}\n",
            ),
        ];
        if let Some(y) = y {
            t.push(("src/y.rs", y));
        }
        t
    };
    const Y: &str = "pub struct Duration;\nimpl Duration {\n    pub fn ext(&self) {}\n}\n";
    const RIGHT: &str = "src/x.rs.g -> src/e.rs.Duration.ext";
    let (_p, _d, db) = fresh_index_of(&tree(None));
    assert!(calls_by_qualified_name(&db).contains(&RIGHT.to_string()));
    // The project's `Duration` appears, with no method (nothing re-resolves
    // `x.rs` by name) and with one of that name. Calls only: the `references`
    // edge `e.rs` writes to `std::time::Duration` binds the new struct by name
    // on a rebuild and not incrementally, which predates D#112 (see
    // CHANGELOG, Not covered).
    const LIB_Y: &str = "pub mod t;\npub mod x;\npub mod e;\npub mod y;\n";
    for y in ["pub struct Duration;\n", Y] {
        assert_incremental_matches_rebuild(
            &tree(None),
            &[("src/lib.rs", Some(LIB_Y)), ("src/y.rs", Some(y))],
        );
    }
    let (_p, _d, db) = fresh_index_of(&tree(Some(Y)));
    let calls = calls_by_qualified_name(&db);
    assert!(calls.contains(&RIGHT.to_string()), "{calls:#?}");
    assert!(
        !calls.contains(&"src/x.rs.g -> src/y.rs.Duration.ext".to_string()),
        "{calls:#?}"
    );
}

/// Batch-1 review H1: a std receiver keeps a project trait's method that a
/// blanket impl (`impl<T: Display> Shout for T`), an impl on `&str` or `[u8]`,
/// or an impl on the type of a suffixed literal (`7u32`) defines, as rustc
/// calls them and as 0.161.0 bound them; and still binds no other project
/// type's method of that name. A blanket impl binds whether or not its trait is
/// in the caller's scope (batch-1 review round 2 withdrew the scope test).
#[test]
fn test_rust_std_receiver_keeps_blanket_reference_slice_and_literal_impls() {
    let (_p, _d, db) = fresh_index_of(&[
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod ext;\npub mod decoy;\npub mod recv;\npub mod noscope;\n",
        ),
        // The blanket impl's trait is not in scope here: rustc rejects the
        // call, and it binds all the same (no scope test).
        (
            "src/noscope.rs",
            "pub fn no_trait(t: std::thread::Thread) {\n    t.shout();\n}\n",
        ),
        (
            "src/ext.rs",
            "use std::fmt::Display;\n\
             pub trait Shout {\n    fn shout(&self);\n}\n\
             impl<T: Display + ?Sized> Shout for T {\n    fn shout(&self) {}\n}\n\
             pub trait NumExt {\n    fn twice(&self);\n}\n\
             impl NumExt for u32 {\n    fn twice(&self) {}\n}\n\
             impl NumExt for u64 {\n    fn twice(&self) {}\n}\n\
             pub trait StrExt {\n    fn yell(&self);\n}\n\
             impl StrExt for &str {\n    fn yell(&self) {}\n}\n\
             pub trait SliceExt {\n    fn first_two(&self);\n}\n\
             impl SliceExt for [u8] {\n    fn first_two(&self) {}\n}\n\
             pub trait Loud {\n    fn loud(&self);\n}\n\
             impl Loud for str {\n    fn loud(&self) {}\n}\n\
             pub trait Quiet {\n    fn hush(&self);\n}\n\
             impl Quiet for String {\n    fn hush(&self) {}\n}\n\
             impl Quiet for str {\n    fn hush(&self) {}\n}\n\
             pub struct B;\nimpl B {\n    pub fn shout(&self) {}\n}\n\
             pub trait Addr {\n    fn addrs(&self);\n}\n\
             impl Addr for std::net::SocketAddrV4 {\n    fn addrs(&self) {}\n}\n\
             impl<T: Addr + ?Sized> Addr for &T {\n    fn addrs(&self) {}\n}\n",
        ),
        (
            "src/decoy.rs",
            "pub struct Horn;\nimpl Horn {\n    pub fn shout(&self) {}\n    pub fn yell(&self) {}\n    \
             pub fn first_two(&self) {}\n    pub fn twice(&self) {}\n    pub fn loud(&self) {}\n}\n",
        ),
        (
            "src/recv.rs",
            "use crate::ext::{Addr, Loud, NumExt, Quiet, Shout, SliceExt, StrExt};\n\
             pub fn blanket() {\n    let s = String::new();\n    s.shout();\n}\n\
             pub fn str_literal() {\n    \"hi\".yell();\n}\n\
             pub fn str_param(s: &str) {\n    s.yell();\n}\n\
             pub fn slice(b: &[u8]) {\n    b.first_two();\n}\n\
             pub fn literal() {\n    7u32.twice();\n}\n\
             pub fn string_deref() {\n    let s = String::new();\n    s.loud();\n}\n\
             pub fn vec_deref(v: Vec<u8>) {\n    v.first_two();\n}\n\
             pub fn exact_first(a: std::net::SocketAddrV4) {\n    a.addrs();\n}\n\
             pub fn exact_string() {\n    let s = String::new();\n    s.hush();\n}\n",
        ),
    ]);
    let calls = calls_by_qualified_name(&db);
    let mut bad = Vec::new();
    for (caller, right, wrong) in [
        ("blanket", "src/ext.rs.T.shout", "src/decoy.rs.Horn.shout"),
        (
            "str_literal",
            "src/ext.rs.&str.yell",
            "src/decoy.rs.Horn.yell",
        ),
        (
            "str_param",
            "src/ext.rs.&str.yell",
            "src/decoy.rs.Horn.yell",
        ),
        (
            "slice",
            "src/ext.rs.[u8].first_two",
            "src/decoy.rs.Horn.first_two",
        ),
        ("literal", "src/ext.rs.u32.twice", "src/decoy.rs.Horn.twice"),
        ("literal", "src/ext.rs.u32.twice", "src/ext.rs.u64.twice"),
        // A `String` derefs to `str`, a `Vec` to a slice.
        (
            "string_deref",
            "src/ext.rs.str.loud",
            "src/decoy.rs.Horn.loud",
        ),
        (
            "vec_deref",
            "src/ext.rs.[u8].first_two",
            "src/decoy.rs.Horn.first_two",
        ),
        // An impl on the type itself answers before a blanket one.
        (
            "exact_first",
            "src/ext.rs.SocketAddrV4.addrs",
            "src/ext.rs.&T.addrs",
        ),
        (
            "exact_string",
            "src/ext.rs.String.hush",
            "src/ext.rs.str.hush",
        ),
        // A one-letter project struct is no type parameter.
        ("blanket", "src/ext.rs.T.shout", "src/ext.rs.B.shout"),
    ] {
        let from = |t: &str| format!("src/recv.rs.{caller} -> {t}");
        if !calls.contains(&from(right)) {
            bad.push(format!("{caller}: missing {right}"));
        }
        if calls.contains(&from(wrong)) {
            bad.push(format!("{caller}: wrong {wrong}"));
        }
    }
    for (right, wrong) in [
        ("src/ext.rs.T.shout", "src/decoy.rs.Horn.shout"),
        ("src/ext.rs.T.shout", "src/ext.rs.B.shout"),
    ] {
        let from = |t: &str| format!("src/noscope.rs.no_trait -> {t}");
        if !calls.contains(&from(right)) {
            bad.push(format!("no_trait: missing {right}"));
        }
        if calls.contains(&from(wrong)) {
            bad.push(format!("no_trait: wrong {wrong}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}\n{calls:#?}");
}

/// Batch-1 review round 2's r1 fixture, cut down: `Yell` and a blanket impl
/// of it in `ext.rs`, and a `String` receiver calling `yell` in `ca.rs`.
fn blanket_tree(ext: &'static str, lib: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", lib),
        ("src/ext.rs", ext),
        (
            "src/ca.rs",
            "use crate::ext::Yell;\npub fn a1(s: String) {\n    s.yell();\n}\n",
        ),
    ]
}

const BLANKET_LIB: &str = "pub mod ext;\npub mod ca;\n";
const BLANKET_LIB_TT: &str = "pub mod ext;\npub mod ca;\npub mod tt;\n";
const EXT_BLANKET: &str = "use std::fmt::Display;\npub trait Yell {\n    fn yell(&self) -> String;\n}\n\
    impl<T: Display> Yell for T {\n    fn yell(&self) -> String {\n        String::new()\n    }\n}\n";
const EVERY_PATH: [IndexPath; 4] = [
    IndexPath::Incremental,
    IndexPath::Cached,
    IndexPath::Refresh,
    IndexPath::Resync,
];

/// Batch-1 review round 2, HIGH: whether an impl is a blanket impl is read
/// from its own header (its self type is a parameter of its own `<…>` list),
/// so a `pub struct T;` added in another file, or in the impl's own, changes
/// nothing, on a rebuild as on every incremental path. It was decided by
/// whether any project type was named `T`: a rebuild dropped the blanket
/// binding and an incremental run kept it. A parameter of any name counts
/// (`impl<St> … for St`); a concrete `T` that the impl imports does not.
#[test]
fn test_rust_blanket_impl_is_decided_by_its_own_header() {
    const EXT_BLANKET_T: &str = "use std::fmt::Display;\npub trait Yell {\n    fn yell(&self) -> String;\n}\n\
        impl<T: Display> Yell for T {\n    fn yell(&self) -> String {\n        String::new()\n    }\n}\n\
        pub struct T;\n";
    const EDGE: &str = "src/ca.rs.a1 --calls--> src/ext.rs.yell";
    let before = blanket_tree(EXT_BLANKET, BLANKET_LIB);
    for path in EVERY_PATH {
        // Calls only: `ext.rs`'s `references` edge to `T` binds the new
        // struct by name on a rebuild alone (CHANGELOG, Not covered).
        let want = assert_path_calls_match_rebuild(
            &before,
            &[&[
                ("src/lib.rs", Some(BLANKET_LIB_TT)),
                ("src/tt.rs", Some("pub struct T;\n")),
            ]],
            path,
        );
        assert!(want.iter().any(|e| e == EDGE), "{path:?}: {want:#?}");
        let want =
            assert_path_matches_rebuild(&before, &[&[("src/ext.rs", Some(EXT_BLANKET_T))]], path);
        assert!(want.iter().any(|e| e == EDGE), "{path:?}: {want:#?}");
    }
    const EXT_ST: &str = "pub trait Yell {\n    fn yell(&self) -> String;\n}\n\
        impl<St: AsRef<str> + ?Sized> Yell for St {\n    fn yell(&self) -> String {\n        String::new()\n    }\n}\n";
    let (_p, _d, db) = fresh_index_of(&blanket_tree(EXT_ST, BLANKET_LIB));
    let calls = calls_by_qualified_name(&db);
    assert!(
        calls.contains(&"src/ca.rs.a1 -> src/ext.rs.St.yell".to_string()),
        "{calls:#?}"
    );
    const EXT_CONCRETE: &str =
        "use crate::tt::T;\npub trait Yell {\n    fn yell(&self) -> String;\n}\n\
        impl Yell for T {\n    fn yell(&self) -> String {\n        String::new()\n    }\n}\n";
    let mut tree = blanket_tree(EXT_CONCRETE, BLANKET_LIB_TT);
    tree.push(("src/tt.rs", "pub struct T;\n"));
    let (_p, _d, db) = fresh_index_of(&tree);
    let calls = calls_by_qualified_name(&db);
    assert!(
        !calls.iter().any(|c| c.starts_with("src/ca.rs.a1 -> ")),
        "{calls:#?}"
    );
}

/// Batch-1 review round 2, MEDIUM: a blanket impl was admitted only when the
/// caller's file imported some item of the impl's file, so `use
/// crate::ext3::util; s.trim()` bound it and the same call without that import
/// did not. The test is withdrawn: both bind it, as 24a8586 (0.161.0) does on
/// the review's r1 fixture (`w1`, `w2` and `w3` all call `T.trim`), and every
/// indexing path agrees with a rebuild while the import comes and goes.
#[test]
fn test_rust_blanket_impl_binds_without_a_scope_test() {
    const EXT3: &str = "pub trait Trimmy {\n    fn trim(&self) -> usize;\n}\n\
        impl<T: AsRef<str>> Trimmy for T {\n    fn trim(&self) -> usize {\n        0\n    }\n}\n\
        pub fn util() {}\n";
    const W1: &str =
        "use crate::ext3::util;\npub fn w1(s: String) {\n    util();\n    let _ = s.trim();\n}\n";
    const W2: &str = "pub fn w2(s: String) {\n    let _ = s.trim();\n}\n";
    const W2_UTIL: &str =
        "use crate::ext3::util;\npub fn w2(s: String) {\n    let _ = s.trim();\n}\n";
    let tree = [
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod ext3;\npub mod cw1;\npub mod cw2;\n"),
        ("src/ext3.rs", EXT3),
        ("src/cw1.rs", W1),
        ("src/cw2.rs", W2),
    ];
    let (_p, _d, db) = fresh_index_of(&tree);
    let calls = calls_by_qualified_name(&db);
    for caller in ["src/cw1.rs.w1", "src/cw2.rs.w2"] {
        assert!(
            calls.contains(&format!("{caller} -> src/ext3.rs.T.trim")),
            "{caller}: {calls:#?}"
        );
    }
    for path in EVERY_PATH {
        assert_path_matches_rebuild(
            &tree,
            &[
                &[("src/cw2.rs", Some(W2_UTIL))],
                &[("src/cw2.rs", Some(W2))],
            ],
            path,
        );
    }
}

/// Batch-1 review round 2: re-resolving a receiver-typed call whose target's
/// file was re-indexed must not go through the pending buffer, which keeps one
/// row per caller and name: tokio's `localset_future_drives_all_local_futs`
/// has a `task::spawn_local(..)` waiting there, and its `local.spawn_local(..)`
/// lost its `LocalSet.spawn_local` edge when an unrelated edit re-indexed
/// `task/local.rs`.
#[test]
fn test_rust_typed_call_re_resolved_beside_a_waiting_same_named_call() {
    const LOCAL: &str = "pub struct LocalSet;\nimpl LocalSet {\n    \
        pub fn new() -> Self {\n        LocalSet\n    }\n    \
        pub fn spawn_local(&self, x: u8) {}\n}\n";
    const LOCAL_EDITED: &str = "pub struct LocalSet;\nimpl LocalSet {\n    \
        pub fn new() -> Self {\n        LocalSet\n    }\n    \
        pub fn spawn_local(&self, x: u8) {}\n}\npub fn other() {}\n";
    let tree = [
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod local;\npub mod task;\npub mod user;\n",
        ),
        ("src/local.rs", LOCAL),
        ("src/task.rs", "pub fn yield_now() {}\n"),
        (
            "src/user.rs",
            // `task.rs` has no `spawn_local`: that call waits in the buffer.
            "use crate::local::LocalSet;\nuse crate::task;\npub fn f() {\n    \
             let local = LocalSet::new();\n    local.spawn_local(1);\n    task::spawn_local(2);\n}\n",
        ),
    ];
    const EDGE: &str = "src/user.rs.f --calls--> src/local.rs.spawn_local";
    for path in EVERY_PATH {
        let want =
            assert_path_matches_rebuild(&tree, &[&[("src/local.rs", Some(LOCAL_EDITED))]], path);
        assert!(want.iter().any(|e| e == EDGE), "{path:?}: {want:#?}");
    }
}

/// Batch-1 review round 2: an impl header rewritten from a blanket impl to an
/// impl on a concrete `T` keeps its method's qualified name (`T.yell`), so the
/// caller's saved edge was restored by that name on every incremental path,
/// while a rebuild asked the receiver's type again and dropped it. A call typed
/// by its receiver is now re-resolved, not restored; the reverse edit binds
/// it again.
#[test]
fn test_rust_blanket_impl_turned_concrete_re_resolves_its_typed_callers() {
    const EXT_CONCRETE: &str =
        "pub struct T;\npub trait Yell {\n    fn yell(&self) -> String;\n}\n\
        impl Yell for T {\n    fn yell(&self) -> String {\n        String::new()\n    }\n}\n";
    const EDGE: &str = "src/ca.rs.a1 --calls--> src/ext.rs.yell";
    for path in EVERY_PATH {
        let want = assert_path_matches_rebuild(
            &blanket_tree(EXT_BLANKET, BLANKET_LIB),
            &[&[("src/ext.rs", Some(EXT_CONCRETE))]],
            path,
        );
        assert!(!want.iter().any(|e| e == EDGE), "{path:?}: {want:#?}");
        let want = assert_path_matches_rebuild(
            &blanket_tree(EXT_CONCRETE, BLANKET_LIB),
            &[&[("src/ext.rs", Some(EXT_BLANKET))]],
            path,
        );
        assert!(want.iter().any(|e| e == EDGE), "{path:?}: {want:#?}");
    }
}

/// Batch-1 review H2: a package whose library is named in `[lib] name` (or
/// that a dependency renames with `package = …`) is found by that name, so a
/// `use corelib::engine_start` binds core-pkg's item and not a same-named one
/// elsewhere. A manifest whose names cannot be read keeps such a path's
/// resolution by name, as before D#132, instead of dropping it.
#[test]
fn test_rust_crate_found_by_its_lib_name_or_dependency_rename() {
    let tree = |core_manifest: &'static str, app_manifest: &'static str, root: &'static str| {
        let lib: &'static str = Box::leak(
            format!(
                "use {root}::engine_start;\nuse {root}::Motor;\n\
                 pub fn run() {{\n    engine_start();\n    let m = Motor::new();\n    m.rev();\n}}\n"
            )
            .into_boxed_str(),
        );
        vec![
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"core-pkg\", \"app\"]\n",
            ),
            ("core-pkg/Cargo.toml", core_manifest),
            (
                "core-pkg/src/lib.rs",
                "pub fn engine_start() {}\npub struct Motor;\nimpl Motor {\n    \
                 pub fn new() -> Motor {\n        Motor\n    }\n    pub fn rev(&self) {}\n}\n",
            ),
            ("app/Cargo.toml", app_manifest),
            ("app/src/lib.rs", lib),
        ]
    };
    const PKG: &str = "[package]\nname = \"core-pkg\"\nversion = \"0.1.0\"\n";
    const LIB_NAMED: &str =
        "[package]\nname = \"core-pkg\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"corelib\"\n";
    const LIB_UNREADABLE: &str =
        "lib = { name = \"corelib\" }\n[package]\nname = \"core-pkg\"\nversion = \"0.1.0\"\n";
    const APP: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
    const APP_RENAME: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
        [dependencies]\nengine = { path = \"../core-pkg\", package = \"core-pkg\" }\n";
    let want = [
        "app/src/lib.rs.run -> core-pkg/src/lib.rs.engine_start",
        "app/src/lib.rs.run -> core-pkg/src/lib.rs.Motor.new",
        "app/src/lib.rs.run -> core-pkg/src/lib.rs.Motor.rev",
    ];
    let mut bad = Vec::new();
    for (case, core, app, root) in [
        ("lib name", LIB_NAMED, APP, "corelib"),
        ("dependency rename", PKG, APP_RENAME, "engine"),
        ("unreadable lib name", LIB_UNREADABLE, APP, "corelib"),
    ] {
        let (_p, _d, db) = fresh_index_of(&tree(core, app, root));
        let calls = calls_by_qualified_name(&db);
        for w in want {
            if !calls.contains(&w.to_string()) {
                bad.push(format!("{case}: missing {w} (has {calls:?})"));
            }
        }
    }
    // A dependency the project does not hold still binds nothing.
    let (_p, _d, db) = fresh_index_of(&tree(PKG, APP, "serde"));
    let calls = calls_by_qualified_name(&db);
    if calls
        .iter()
        .any(|c| c.starts_with("app/src/lib.rs.run -> "))
    {
        bad.push(format!("foreign crate bound: {calls:?}"));
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// Batch-1 review M2: a renamed project import binds the item its path names,
/// not every import of the item's original name.
#[test]
fn test_rust_renamed_import_binds_only_its_own_path() {
    let (_p, _d, db) = fresh_index_of(&[
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod a;\npub mod b;\npub mod ren;\n"),
        ("src/a.rs", "pub fn helper() {}\n"),
        ("src/b.rs", "pub fn helper() {}\n"),
        (
            "src/ren.rs",
            "use crate::a::helper as a_helper;\nuse crate::b::helper;\n\
             fn n2() {\n    a_helper();\n}\n",
        ),
    ]);
    let calls = calls_by_qualified_name(&db);
    assert!(
        calls.contains(&"src/ren.rs.n2 -> src/a.rs.helper".to_string())
            && !calls.contains(&"src/ren.rs.n2 -> src/b.rs.helper".to_string()),
        "{calls:#?}"
    );
}

/// D#120 fixture: one caller file reaching every accepted and every refused
/// shape of a renamed import (see `test_js_renamed_import_call_shapes` for the
/// parser half).
const D120_TREE: &[(&str, &str)] = &[
    (
        "src/x.js",
        "export function load() { return 1; }\nexport const arrow = () => 2;\n",
    ),
    (
        "src/x2.js",
        "function outer() { function load() { return 3; } return load(); }\nmodule.exports = { outer };\n",
    ),
    (
        "src/cjs.js",
        "function clearCache() { return 4; }\nmodule.exports = { clearCache };\n",
    ),
    (
        "src/alias.js",
        "function realLoad() { return 5; }\nclass Model {\n  load() { return 6; }\n}\nmodule.exports = { load: realLoad, Model };\n",
    ),
    (
        "src/exp.js",
        "function realSave() { return 7; }\nexports.save = realSave;\n",
    ),
    ("src/val.js", "export const load = 5;\n"),
    ("src/reexp.js", "export { load } from './x';\n"),
    // The export map publishes a class as `load`: the file's own function
    // `load` is not what the import reaches.
    (
        "src/map2.js",
        "function load() { return 11; }\nclass Real {}\nmodule.exports = { load: Real };\n",
    ),
    // A second `realLoad`: the call binds one of two same-named functions by
    // the export map, not by name, so it stays `inferred`.
    ("src/dup.js", "export function realLoad() { return 10; }\n"),
    (
        "src/other.js",
        "export function m1() {}\nexport function resolve() {}\n",
    ),
    (
        "src/use.js",
        "import { load as loadModel, arrow as arr } from './x';\n\
         import { resolve as m1 } from 'path';\n\
         import { load as v } from './val';\n\
         import { load as viaBarrel } from './reexp';\n\
         const { clearCache: clearBinaryCache } = require('./cjs');\n\
         const { load: aliasLoad } = require('./alias');\n\
         const saveIt = require('./exp').save;\n\
         const { load: n } = require('./x2');\n\
         const { load: viaClassMap } = require('./map2');\n\
         function load() { return 8; }\n\
         function clearCache() { return 9; }\n\
         function go() { loadModel(); arr(); m1(); v(); viaBarrel(); clearBinaryCache(); aliasLoad(); saveIt(); n(); viaClassMap(); }\n",
    ),
];

/// The edges D120_TREE must and must not have from `use.js`'s `go`.
fn check_d120_edges(edges: &[String], when: &str) {
    let has = |e: &str| edges.iter().any(|x| x == e);
    let lost: Vec<&str> = [
        "src/use.js.go --calls--> src/x.js.load",
        "src/use.js.go --calls--> src/x.js.arrow",
        "src/use.js.go --calls--> src/cjs.js.clearCache",
        // `module.exports = { load: realLoad }` exports `realLoad` as `load`.
        "src/use.js.go --calls--> src/alias.js.realLoad",
        "src/use.js.go --calls--> src/exp.js.realSave",
    ]
    .into_iter()
    .filter(|e| !has(e))
    .collect();
    let bound: Vec<&str> = [
        // A rename usually avoids a same-file function of the export's name.
        "src/use.js.go --calls--> src/use.js.load",
        "src/use.js.go --calls--> src/use.js.clearCache",
        // F6: a class method sharing the export's name is not the export.
        "src/use.js.go --calls--> src/alias.js.load",
        // A package's export runs no project code, whatever its names.
        "src/use.js.go --calls--> src/other.js.resolve",
        "src/use.js.go --calls--> src/other.js.m1",
        "src/use.js.go --calls--> src/dup.js.realLoad",
        "src/use.js.go --calls--> src/map2.js.load",
        // Nor the class the map does publish as `load` (review M1).
        "src/use.js.go --calls--> src/map2.js.Real",
        // A nested function is no export; a constant is not callable code.
        "src/use.js.go --calls--> src/x2.js.load",
        "src/use.js.go --calls--> src/val.js.load",
    ]
    .into_iter()
    .filter(|e| has(e))
    .collect();
    assert!(
        lost.is_empty() && bound.is_empty(),
        "{when}: lost {lost:#?}\nbound {bound:#?}\n{edges:#?}"
    );
}

/// D#120: a call through a renamed import binds the export in the file the
/// specifier names — on a full index and after every file is touched in turn.
#[test]
fn test_js_renamed_import_binds_the_export() {
    let (project, _d, db) = fresh_index_of(D120_TREE);
    check_d120_edges(&edge_set(&db), "full index");
    let aliased: Vec<String> = call_edges_with_confidence(&db)
        .into_iter()
        .filter(|e| e.starts_with("src/use.js.go -> src/alias.js.realLoad "))
        .collect();
    assert_eq!(
        aliased,
        [
            r#"src/use.js.go -> src/alias.js.realLoad {"js_module":"./alias","q":"imp","v":"load"} inferred"#
        ]
    );
    for (f, _) in D120_TREE {
        let body = fs::read_to_string(project.path().join(f)).unwrap();
        fs::write(project.path().join(f), format!("{body}\n")).unwrap();
        run_incremental_index(&db, project.path(), None, None).unwrap();
        check_d120_edges(&edge_set(&db), &format!("after {f} changed"));
    }
    let files: Vec<(&str, String)> = D120_TREE
        .iter()
        .map(|(f, _)| (*f, fs::read_to_string(project.path().join(f)).unwrap()))
        .collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(f, b)| (*f, b.as_str())).collect();
    let (_p2, _d2, control) = fresh_index_of(&files);
    assert_eq!(
        call_edges_with_confidence(&db),
        call_edges_with_confidence(&control),
        "every file touched must equal a rebuild"
    );
}

/// D#120 F1: the export renamed away. The requeued edge must not be bound by
/// name in another file (`y.js:load`), and a later sweep must not either.
#[test]
fn test_js_renamed_import_export_renamed_away_matches_rebuild() {
    let before: &[(&str, &str)] = &[
        ("x.js", "export function load() {}\n"),
        ("y.js", "export function load() {}\n"),
        (
            "a.js",
            "import { load as m } from './x';\nconst { load: c } = require('./x');\nfunction go() { m(); c(); }\n",
        ),
    ];
    assert_incremental_matches_rebuild(before, &[("x.js", Some("export function load2() {}\n"))]);
    let (project, _d, db) = fresh_index_of(before);
    assert!(edge_set(&db).contains(&"a.js.go --calls--> x.js.load".to_string()));
    fs::write(project.path().join("x.js"), "export function load2() {}\n").unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    // A second run sweeps the buffered row again.
    fs::write(project.path().join("y.js"), "export function load() {}\n\n").unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let calls: Vec<String> = edge_set(&db)
        .into_iter()
        .filter(|e| e.starts_with("a.js.go --calls-->"))
        .collect();
    assert!(calls.is_empty(), "{calls:#?}");
}

/// D#120 F2: an export the file gains later binds on the incremental run, and
/// a changed CommonJS export map rebinds — as a rebuild of each tree does.
#[test]
fn test_js_renamed_import_export_added_later_matches_rebuild() {
    let caller = (
        "a.js",
        "import { load as m } from './x';\nconst { save: s } = require('./z');\nfunction go() { m(); s(); }\n",
    );
    let z = "function one() {}\nfunction two() {}\nmodule.exports = { save: one };\n";
    let before: &[(&str, &str)] = &[
        ("x.js", "export function other() {}\n"),
        ("z.js", z),
        caller,
    ];
    let added = "export function other() {}\nexport function load() {}\n";
    assert_incremental_matches_rebuild(before, &[("x.js", Some(added))]);
    let moved = "function one() {}\nfunction two() {}\nmodule.exports = { save: two };\n";
    assert_incremental_matches_rebuild(before, &[("z.js", Some(moved))]);
    // Not vacuous: the incremental run binds both.
    let (project, _d, db) = fresh_index_of(before);
    fs::write(project.path().join("x.js"), added).unwrap();
    fs::write(project.path().join("z.js"), moved).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let edges = edge_set(&db);
    for want in [
        "a.js.go --calls--> x.js.load",
        "a.js.go --calls--> z.js.two",
    ] {
        assert!(
            edges.contains(&want.to_string()),
            "{want} missing: {edges:#?}"
        );
    }
    assert!(
        !edges.contains(&"a.js.go --calls--> z.js.one".to_string()),
        "{edges:#?}"
    );
}

/// D#120 F6: the export map changed so that only a class method shares the
/// export's name; and the exporting file deleted, then restored.
#[test]
fn test_js_renamed_import_export_removed_or_deleted_matches_rebuild() {
    let before: &[(&str, &str)] = &[
        (
            "alias.js",
            "function realLoad() {}\nclass Model {\n  load() {}\n}\nmodule.exports = { load: realLoad, Model };\n",
        ),
        (
            "a.js",
            "const { load: m } = require('./alias');\nfunction go() { m(); }\n",
        ),
    ];
    let no_export =
        "function realLoad() {}\nclass Model {\n  load() {}\n}\nmodule.exports = { Model };\n";
    assert_incremental_matches_rebuild(before, &[("alias.js", Some(no_export))]);
    assert_incremental_matches_rebuild(before, &[("alias.js", None)]);
    let (project, _d, db) = fresh_index_of(before);
    let alias = fs::read_to_string(project.path().join("alias.js")).unwrap();
    fs::remove_file(project.path().join("alias.js")).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    assert!(!edge_set(&db)
        .iter()
        .any(|e| e.starts_with("a.js.go --calls-->")));
    fs::write(project.path().join("alias.js"), alias).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let edges = edge_set(&db);
    assert!(
        edges.contains(&"a.js.go --calls--> alias.js.realLoad".to_string())
            && !edges.contains(&"a.js.go --calls--> alias.js.load".to_string()),
        "{edges:#?}"
    );
}

/// D#120 F5: a rename inside one function is that function's; a sibling's own
/// `m()` and a parameter `m` keep their bare resolution.
#[test]
fn test_js_renamed_import_is_scoped() {
    let (_p, _d, db) = fresh_index_of(&[
        ("x.js", "export function load() {}\n"),
        ("z.js", "export function m() {}\n"),
        (
            "a.js",
            "function f() { const { load: m } = require('./x'); m(); }\n\
             function g() { m(); }\n\
             function h(m) { m(); }\n",
        ),
    ]);
    let edges = edge_set(&db);
    assert!(
        edges.contains(&"a.js.f --calls--> x.js.load".to_string()),
        "{edges:#?}"
    );
    for wrong in [
        "a.js.f --calls--> z.js.m",
        "a.js.g --calls--> x.js.load",
        "a.js.h --calls--> x.js.load",
    ] {
        assert!(!edges.contains(&wrong.to_string()), "{wrong}: {edges:#?}");
    }
}

/// D#120 review BLOCKER: the export-map lookup runs once per renamed-import
/// call, on a full index before any `ANALYZE`. Driven from `idx_edges_relation`
/// it scanned every `exports` edge in the repo per call — 999 calls over
/// 20,000 `exports` edges took a 5,000-file index from 1.83 s to 3.89 s. With
/// no statistics (every fresh index), it must seek the file's `<module>` edges.
#[test]
fn test_js_export_map_query_seeks_the_module_node_edges() {
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let stats: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'sqlite_stat1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if stats > 0 {
        db.conn().execute_batch("DELETE FROM sqlite_stat1").unwrap();
    }
    let sql = format!("EXPLAIN QUERY PLAN {}", super::resolve::js_export_map_sql());
    let mut stmt = db.conn().prepare(&sql).unwrap();
    let n = stmt.parameter_count();
    let plan: Vec<String> = stmt
        .query_map(
            rusqlite::params_from_iter(std::iter::repeat_n("x", n)),
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|l| l.starts_with("SEARCH e ") && l.contains("(source_id=? AND relation=?)")),
        "{plan:#?}"
    );
    assert!(
        !plan.iter().any(|l| l.contains("idx_edges_relation")),
        "{plan:#?}"
    );
}

/// D#120 review HIGH-1: an ESM renamed import whose file is missing at index
/// time. Its `<external>` sentinel is named after the symbol, not the
/// specifier, so a file appearing later never re-extracts the importer: the
/// call must wait in the buffer, since a rebuild binds it.
#[test]
fn test_js_renamed_import_file_appearing_later_matches_rebuild() {
    let x = "export function load() {}\n";
    let a = (
        "a.js",
        "import { load as m } from './x';\nfunction go() { m(); }\n",
    );
    let want = "a.js.go --calls--> x.js.load".to_string();
    // Added after the first index.
    let before: &[(&str, &str)] = &[a, ("b.js", "export function other() {}\n")];
    assert_incremental_matches_rebuild(before, &[("x.js", Some(x))]);
    let (project, _d, db) = fresh_index_of(before);
    fs::write(project.path().join("x.js"), x).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    assert!(edge_set(&db).contains(&want), "added: {:#?}", edge_set(&db));
    // Deleted, then restored.
    let (project, _d, db) = fresh_index_of(&[a, ("x.js", x)]);
    assert!(edge_set(&db).contains(&want));
    fs::remove_file(project.path().join("x.js")).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    assert!(!edge_set(&db)
        .iter()
        .any(|e| e.starts_with("a.js.go --calls-->")));
    fs::write(project.path().join("x.js"), x).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    assert!(
        edge_set(&db).contains(&want),
        "restored: {:#?}",
        edge_set(&db)
    );
    // A package names no file of the project: nothing to wait for.
    let (_p, _d, db) = fresh_index_of(&[(
        "p.js",
        "import { resolve as r } from 'path';\nfunction go() { r(); }\n",
    )]);
    assert_eq!(
        crate::storage::queries::count_pending_unresolved_calls(db.conn()).unwrap(),
        0
    );
}

/// D#120 review MEDIUM-1: the import reaches what the file exports under that
/// name, never a same-named function the file keeps private — beside an ESM
/// `export { realLoad as load }` (no export map recorded), a CommonJS map that
/// omits the key, a key published from another file, or no export at all.
#[test]
fn test_js_renamed_import_binds_only_what_the_file_exports() {
    let tree: &[(&str, &str)] = &[
        (
            "esm.js",
            "function realLoad() {}\nfunction load() {}\nexport { realLoad as load };\n",
        ),
        (
            "map.js",
            "function load() {}\nclass K {}\nmodule.exports = { other: K };\n",
        ),
        ("impl.js", "function go() {}\nmodule.exports = { go };\n"),
        (
            "fwd.js",
            "const impl = require('./impl').go;\nfunction load() {}\nexports.load = impl;\n",
        ),
        ("plain.js", "function load() {}\n"),
        // `load` is published, but as `other`.
        (
            "key.js",
            "function load() {}\nmodule.exports = { other: load };\n",
        ),
        // The export names a nested function only by name resolution.
        (
            "nested.js",
            "const load = require('./ok').load;\n\
             function outer() { function load() {} return load; }\n\
             module.exports = { load, outer };\n",
        ),
        // The map is reassigned: `load` publishes `realLoad`, not `load`.
        (
            "remap.js",
            "function load() {}\nfunction realLoad() {}\n\
             module.exports = { load };\nmodule.exports = { load: realLoad };\n",
        ),
        ("ok.js", "export function load() {}\n"),
        (
            "a.js",
            "import { load as e } from './esm';\n\
             const { load: m } = require('./map');\n\
             const { load: f } = require('./fwd');\n\
             import { load as p } from './plain';\n\
             const { load: k } = require('./key');\n\
             const { load: n } = require('./nested');\n\
             const { load: r } = require('./remap');\n\
             import { load as o } from './ok';\n\
             function go() { e(); m(); f(); p(); k(); n(); r(); o(); }\n",
        ),
    ];
    let want = [
        "a.js.go --calls--> ok.js.load",
        "a.js.go --calls--> remap.js.realLoad",
    ];
    let calls = |db: &Database| -> Vec<String> {
        edge_set(db)
            .into_iter()
            .filter(|e| e.starts_with("a.js.go --calls-->"))
            .collect()
    };
    let (project, _d, db) = fresh_index_of(tree);
    assert_eq!(calls(&db), want);
    for (f, body) in tree {
        fs::write(project.path().join(f), format!("{body}\n")).unwrap();
        run_incremental_index(&db, project.path(), None, None).unwrap();
        assert_eq!(calls(&db), want, "after {f}");
    }
}

/// D#120 review MEDIUM-2 (M20): a self-import only the sweep resolves. `../x`
/// named `x.js` (no `load`: buffered); deleting `x.js` makes it name the
/// caller's own file, which the deletion does not re-extract. A rebuild's
/// deferred pass never binds a function to itself; the sweep must not either.
#[test]
fn test_js_renamed_import_sweep_binds_no_self_call() {
    let before: &[(&str, &str)] = &[
        ("x.js", "export function other() {}\n"),
        (
            "x/index.js",
            "import { load as m } from '../x';\nexport function load() { m(); }\n",
        ),
        ("z.js", "export function z() {}\n"),
    ];
    let after: &[(&str, Option<&str>)] =
        &[("x.js", None), ("z.js", Some("export function z() {}\n\n"))];
    assert_incremental_matches_rebuild(before, after);
    let (project, _d, db) = fresh_index_of(before);
    assert_eq!(
        crate::storage::queries::count_pending_unresolved_calls(db.conn()).unwrap(),
        1,
        "not vacuous: the call waits for x.js to export `load`"
    );
    fs::remove_file(project.path().join("x.js")).unwrap();
    fs::write(project.path().join("z.js"), "export function z() {}\n\n").unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    assert!(
        !edge_set(&db).contains(&"x/index.js.load --calls--> x/index.js.load".to_string()),
        "{:#?}",
        edge_set(&db)
    );
}

// ── F1: `.code-graph/source-roots.json` (tasks/specs/grep-hook-source-roots.md) ──

fn read_source_roots(project: &std::path::Path) -> Option<Vec<String>> {
    let raw = fs::read_to_string(project.join(".code-graph").join(SOURCE_ROOTS_FILE)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["version"], 1);
    Some(
        v["roots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_string())
            .collect(),
    )
}

/// A project laid out the way the coding eval's networkx checkout is: the
/// package dir named after the package, docs that hold only markdown, a root
/// script with no dir, and a name the grep hook must escape.
fn source_roots_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let p = project.path();
    for (path, body) in [
        (
            "networkx/classes/graph.py",
            "def add_node(n):\n    return n\n",
        ),
        ("tests/test_graph.py", "def test_x():\n    pass\n"),
        ("c++/lib.cpp", "int f() { return 1; }\n"),
        ("doc/index.md", "# Title\n\ntext\n"),
        ("config/settings.json", "{\"a\": 1}\n"),
        ("setup.py", "def main():\n    pass\n"),
    ] {
        fs::create_dir_all(p.join(path).parent().unwrap()).unwrap();
        fs::write(p.join(path), body).unwrap();
    }
    fs::create_dir_all(p.join(".code-graph")).unwrap();
    project
}

#[test]
fn full_index_lists_top_level_dirs_that_hold_indexed_code() {
    let project = source_roots_project();
    let db = Database::open(&project.path().join(".code-graph/index.db")).unwrap();
    run_full_index(&db, project.path(), None, None).unwrap();
    assert_eq!(
        read_source_roots(project.path()).expect("source-roots.json written"),
        vec!["c++", "networkx", "tests"],
        "markdown-only and json-only dirs are not source roots; a root file has no dir"
    );
}

#[test]
fn incremental_index_keeps_source_roots_current_with_the_files_table() {
    let project = source_roots_project();
    let p = project.path();
    let db = Database::open(&p.join(".code-graph/index.db")).unwrap();
    run_full_index(&db, p, None, None).unwrap();

    fs::create_dir_all(p.join("benchmarks")).unwrap();
    fs::write(p.join("benchmarks/bench.py"), "def run():\n    pass\n").unwrap();
    fs::remove_file(p.join("c++/lib.cpp")).unwrap();
    run_incremental_index(&db, p, None, None).unwrap();
    assert_eq!(
        read_source_roots(p).unwrap(),
        vec!["benchmarks", "networkx", "tests"],
        "a new code dir joins, a dir whose last code file is gone leaves"
    );

    // Parity: a from-scratch full index of the same tree lists the same roots.
    let fresh = TempDir::new().unwrap();
    fs::create_dir_all(fresh.path().join(".code-graph")).unwrap();
    let fresh_db = Database::open(&fresh.path().join(".code-graph/index.db")).unwrap();
    // Same files, different root.
    for rel in [
        "benchmarks/bench.py",
        "networkx/classes/graph.py",
        "tests/test_graph.py",
    ] {
        fs::create_dir_all(fresh.path().join(rel).parent().unwrap()).unwrap();
        fs::copy(p.join(rel), fresh.path().join(rel)).unwrap();
    }
    run_full_index(&fresh_db, fresh.path(), None, None).unwrap();
    assert_eq!(read_source_roots(fresh.path()), read_source_roots(p));
}

#[test]
fn source_roots_are_written_only_beside_an_index_in_a_code_graph_dir() {
    // A snapshot or a test builds into a DB elsewhere; the manifest must not
    // appear next to it, nor in the project it indexed.
    let project = source_roots_project();
    let elsewhere = TempDir::new().unwrap();
    let db = Database::open(&elsewhere.path().join("staging.db")).unwrap();
    run_full_index(&db, project.path(), None, None).unwrap();
    assert!(!elsewhere.path().join(SOURCE_ROOTS_FILE).exists());
    assert!(read_source_roots(project.path()).is_none());
}

// D7: a member call reaches a function assigned to a member in another file
// (express's `res.send = function send(){}` called as `res.send(...)`).
#[test]
fn member_call_reaches_a_member_assigned_function_across_files() {
    let files: &[(&str, &str)] = &[
        (
            "lib/response.js",
            "var res = module.exports = {};\n\
             res.send = function send(body) { return body; };\n\
             res.json = function json(obj) { return this.send(JSON.stringify(obj)); };\n",
        ),
        (
            "lib/router.js",
            "function handle(req, res) { res.json({ ok: true }); }\nmodule.exports = handle;\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    assert!(
        has("lib/router.js.handle --calls--> lib/response.js.json"),
        "{edges:#?}"
    );
    assert!(
        has("lib/response.js.json --calls--> lib/response.js.send"),
        "{edges:#?}"
    );
}

// Pre-tag review 2026-09-29: a test's mocks (`global.fetch = …`, a stub's
// `fake.end = …` inside a test callback) are no nodes, so production calls of
// the same names bind no test stub.
#[test]
fn production_calls_bind_no_test_mock() {
    let files: &[(&str, &str)] = &[
        (
            "src/api.js",
            "async function getUser(id) { return fetch('/u/' + id); }\n\
             function close(stream) { stream.end(); stream.destroy(); }\n\
             module.exports = { getUser, close };\n",
        ),
        (
            "test/api.test.js",
            "const { getUser, close } = require('../src/api');\n\
             global.fetch = async () => ({ ok: true });\n\
             it('closes', () => {\n\
               const fake = {};\n\
               fake.end = function () {};\n\
               fake.destroy = () => {};\n\
               close(fake);\n\
             });\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let into_test: Vec<&String> = edges
        .iter()
        .filter(|e| e.starts_with("src/api.js.") && e.contains("--calls--> test/"))
        .collect();
    assert!(into_test.is_empty(), "{into_test:#?}");
}

// D7: `send(req)` through `var send = require('send')` calls the package. With
// `res.send = function send() {}` now a node in the same file, the same-file
// tier bound it there at `extracted` (express: sendFile/sendfile → res.send,
// acceptsEncodings → req.accepts, format → res.vary). A name the file binds to a
// package import cannot mean a function the same file defines; a call through
// `this` still reaches it, and a cross-file project function of that name (a
// workspace package in a monorepo) stays reachable.
#[test]
fn a_bare_call_through_a_package_import_never_binds_a_same_file_function() {
    let files: &[(&str, &str)] = &[
        (
            "lib/response.js",
            "var send = require('send');\nvar vary = require('vary');\nvar res = module.exports = {};\n\
             res.send = function send(body) { return body; };\n\
             res.vary = function (field) { return field; };\n\
             res.sendFile = function sendFile(path) { var file = send(this.req, path); return file; };\n\
             res.format = function (obj) { vary(this, 'Accept'); return this.send(obj); };\n\
             function wrapper() { function send(x) { return x; } return send(1); }\n",
        ),
        (
            "lib/view.js",
            "var path = require('path');\nvar resolve = path.resolve;\n\
             View.prototype.lookup = function lookup(name) { return resolve(this.root, name); };\n\
             View.prototype.resolve = function resolve(dir, file) { return dir + file; };\n",
        ),
        (
            "packages/app/main.js",
            "import { helper } from '@org/lib';\nexport function main() { return helper(1); }\n",
        ),
        ("packages/lib/index.js", "export function helper(x) { return x; }\n"),
        ("lib/utils.js", "var send = require('send');\nexports.mime = send.mime;\n"),
        (
            "lib/download.js",
            "var resolve = require('node:path').resolve;\nvar join = require('path').join;\n\
             function download(p) { return resolve(join(p, 'x')); }\n",
        ),
    ];
    let (project, _d, db) = fresh_index_of(files);
    let check = |db: &Database, when: &str| {
        let edges = edge_set(db);
        let has = |e: &str| edges.iter().any(|x| x == e);
        assert!(
            !has("lib/response.js.sendFile --calls--> lib/response.js.send"),
            "{when}: {edges:#?}"
        );
        assert!(
            !has("lib/response.js.format --calls--> lib/response.js.vary"),
            "{when}: {edges:#?}"
        );
        // `require('send')` imports the package, not the file's own `res.send`.
        assert!(
            !has("lib/response.js.<module> --imports--> lib/response.js.send"),
            "{when}: {edges:#?}"
        );
        // `this.send(obj)` is a call on the object: kept.
        assert!(
            has("lib/response.js.format --calls--> lib/response.js.send"),
            "{when}: {edges:#?}"
        );
        // `resolve` is `path.resolve`: a package's member, not View's method —
        // and `path` is Node's own module, so not another file's `resolve` either.
        assert!(
            !has("lib/view.js.lookup --calls--> lib/view.js.resolve"),
            "{when}: {edges:#?}"
        );
        assert!(
            !has("lib/download.js.download --calls--> lib/view.js.resolve"),
            "{when}: {edges:#?}"
        );
        // Nor another file's: a package import names a module, never a function
        // that happens to share its last segment.
        assert!(
            !has("lib/utils.js.<module> --imports--> lib/response.js.send"),
            "{when}: {edges:#?}"
        );
        // A nested `function send` shadows the import: that call is local.
        assert!(
            has("lib/response.js.wrapper --calls--> lib/response.js.send"),
            "{when}: {edges:#?}"
        );
        // A package name the project itself holds: the cross-file bind stays.
        assert!(
            has("packages/app/main.js.main --calls--> packages/lib/index.js.helper"),
            "{when}: {edges:#?}"
        );
    };
    check(&db, "full index");
    // The pending sweep re-resolves buffered calls on the next run.
    fs::write(
        project.path().join("lib/other.js"),
        "function other() { return 1; }\n",
    )
    .unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    check(&db, "after an incremental run");
}

// D7 / C3: express's tests load the package as `require('..')` / `require('../')`,
// and its index.js is `module.exports = require('./lib/express')`. Neither was an
// import edge, so `affected lib/view.js` reached 1 of the 74 test files that load
// it and `deps index.js` said "<external>".
#[test]
fn a_directory_require_and_a_re_exporting_index_are_file_imports() {
    let files: &[(&str, &str)] = &[
        (
            "index.js",
            "'use strict';\nmodule.exports = require('./lib/express');\n",
        ),
        (
            "lib/express.js",
            "var proto = require('./application');\n\
             exports = module.exports = function createApplication() { return proto; };\n",
        ),
        (
            "lib/application.js",
            "var app = exports = module.exports = {};\napp.init = function init() {};\n",
        ),
        ("test/a.js", "var express = require('..');\nexpress();\n"),
        ("test/b.js", "var express = require('../');\nexpress();\n"),
        (
            "test/sub/c.js",
            "var express = require('../..');\nexpress();\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    for e in [
        "index.js.<module> --imports--> lib/express.js.<module>",
        "lib/express.js.<module> --imports--> lib/application.js.<module>",
        "test/a.js.<module> --imports--> index.js.<module>",
        "test/b.js.<module> --imports--> index.js.<module>",
        "test/sub/c.js.<module> --imports--> index.js.<module>",
    ] {
        assert!(has(e), "missing {e}: {edges:#?}");
    }
}

// C4: flask's examples call `url_for("x")` after `from flask import url_for`,
// a re-export of `helpers.url_for` the module lookup cannot follow; the name
// chain bound the import to `Flask.url_for` too, and every call of the importer
// with it (caller precision 0/7). A `from m import x` names no method, and the
// unique import then prunes the method from the calls. Calls themselves keep
// methods of other files: `self.app.f()` and `imported_obj.f()` carry no
// member metadata in Python either, and a first cut that filtered calls
// dropped 18 correct flask edges for 7 wrong ones (SCIP oracle).
#[test]
fn a_python_from_import_binds_no_method() {
    let files: &[(&str, &str)] = &[
        (
            "flask/__init__.py",
            "from .helpers import url_for as url_for\n",
        ),
        (
            "flask/app.py",
            "class Flask:\n    def url_for(self, endpoint):\n        return endpoint\n\n\
             \x20   def do_teardown(self):\n        return 1\n",
        ),
        (
            "flask/helpers.py",
            "def url_for(endpoint):\n    return endpoint\n",
        ),
        (
            "flask/globals.py",
            "class ProxyMixin:\n    def _get_current_object(self):\n        return self\n",
        ),
        (
            "flask/ctx.py",
            "from .globals import app_ctx\n\nclass AppContext:\n    def pop(self):\n        \
             self.app.do_teardown()\n        return app_ctx._get_current_object()\n",
        ),
        (
            "examples/auth.py",
            "from flask import url_for\n\ndef login():\n    return url_for('x')\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    assert!(
        has("examples/auth.py.<module> --imports--> flask/helpers.py.url_for"),
        "{edges:#?}"
    );
    assert!(
        !has("examples/auth.py.<module> --imports--> flask/app.py.url_for"),
        "{edges:#?}"
    );
    assert!(
        has("examples/auth.py.login --calls--> flask/helpers.py.url_for"),
        "{edges:#?}"
    );
    assert!(
        !has("examples/auth.py.login --calls--> flask/app.py.url_for"),
        "{edges:#?}"
    );
    assert!(
        has("flask/ctx.py.pop --calls--> flask/app.py.do_teardown"),
        "{edges:#?}"
    );
    assert!(
        has("flask/ctx.py.pop --calls--> flask/globals.py._get_current_object"),
        "{edges:#?}"
    );
}

// D10 (2026-09-29 usage evaluation): a relative import — `from .globals
// import _cv_app`, the only form inside flask's own package — bound the
// `<external>` sentinel `.globals` (103 of flask's 402 external imports), so
// `affected src/flask/signals.py` named no test to re-run and no call inside
// the package was ever import-scoped. A relative module resolves against the
// importer's package; a name that is no node of it is a submodule or a
// variable, and binds that file, never a same-named node elsewhere.
#[test]
fn a_python_relative_import_binds_its_package_file() {
    let files: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        ("pkg/signals.py", "request_started = object()\n"),
        ("pkg/typing.py", "X = 1\n"),
        ("pkg/helpers.py", "def get_flag():\n    return 1\n"),
        (
            "pkg/sub/tools.py",
            "def get_flag():\n    return 2\n\ndef request_started():\n    return 3\n",
        ),
        (
            "pkg/app.py",
            "from .signals import request_started\nfrom . import typing as ft\n\
             from .helpers import get_flag\nfrom .helpers import *\n\n\
             def run():\n    return get_flag()\n",
        ),
        ("pkg/sub/__init__.py", ""),
        (
            "pkg/sub/mod.py",
            "from ..helpers import get_flag\nfrom .. import signals\n",
        ),
        ("top.py", "from . import nothing\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    for e in [
        "pkg/app.py.<module> --imports--> pkg/signals.py.<module>",
        "pkg/app.py.<module> --imports--> pkg/typing.py.<module>",
        "pkg/app.py.<module> --imports--> pkg/helpers.py.get_flag",
        "pkg/app.py.<module> --imports--> pkg/helpers.py.<module>",
        "pkg/app.py.run --calls--> pkg/helpers.py.get_flag",
        "pkg/sub/mod.py.<module> --imports--> pkg/helpers.py.get_flag",
        "pkg/sub/mod.py.<module> --imports--> pkg/signals.py.<module>",
    ] {
        assert!(has(e), "missing {e}: {edges:#?}");
    }
    for e in [
        "pkg/app.py.<module> --imports--> pkg/sub/tools.py.request_started",
        "pkg/app.py.run --calls--> pkg/sub/tools.py.get_flag",
    ] {
        assert!(!has(e), "unexpected {e}: {edges:#?}");
    }
    // Above the project root a relative import names nothing of the project.
    assert!(
        has("top.py.<module> --imports--> <external>.."),
        "{edges:#?}"
    );
    assert!(
        !edges
            .iter()
            .any(|e| e.starts_with("pkg/") && e.contains("<external>..")),
        "{edges:#?}"
    );
}

// The same binding survives an incremental run: a relative module that appears
// later rebinds its importer (its sentinel `.late` names the new stem), and an
// edited module keeps the import edge into its re-created `<module>` node.
#[test]
fn a_python_relative_import_is_rebound_incrementally() {
    let before: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        (
            "pkg/app.py",
            "from .late import thing\nfrom .signals import started\n\n\
             def run():\n    return thing()\n",
        ),
        ("pkg/signals.py", "started = 1\n"),
    ];
    let (project, _d, db) = fresh_index_of(before);
    let late = "def thing():\n    return 1\n";
    let signals = "started = 2\nstopped = 3\n";
    fs::write(project.path().join("pkg/late.py"), late).unwrap();
    fs::write(project.path().join("pkg/signals.py"), signals).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, control) = fresh_index_of(&[
        before[0],
        before[1],
        ("pkg/signals.py", signals),
        ("pkg/late.py", late),
    ]);
    let edges = edge_set(&db);
    assert_eq!(
        edges,
        edge_set(&control),
        "incremental must equal a rebuild"
    );
    for e in [
        "pkg/app.py.<module> --imports--> pkg/late.py.thing",
        "pkg/app.py.run --calls--> pkg/late.py.thing",
        "pkg/app.py.<module> --imports--> pkg/signals.py.<module>",
    ] {
        assert!(edges.iter().any(|x| x == e), "missing {e}: {edges:#?}");
    }
}

/// Index `before`, write `after` over it (None deletes), index incrementally,
/// and require the WHOLE edge set of a fresh index of the result — imports and
/// every other relation, not only calls.
fn assert_incremental_edges_match_rebuild(
    before: &[(&str, &str)],
    after: &[(&str, Option<&str>)],
) -> Vec<String> {
    let (project, _d, db) = fresh_index_of(before);
    let mut tree: Vec<(String, String)> = before
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_string()))
        .collect();
    for (path, body) in after {
        tree.retain(|(p, _)| p != path);
        let abs = project.path().join(path);
        match body {
            Some(b) => {
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                fs::write(&abs, b).unwrap();
                tree.push((path.to_string(), b.to_string()));
            }
            None => fs::remove_file(&abs).unwrap(),
        }
    }
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let files: Vec<(&str, &str)> = tree.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let (_p2, _d2, control) = fresh_index_of(&files);
    let edges = edge_set(&db);
    assert_eq!(
        edges,
        edge_set(&control),
        "incremental after {after:?} must equal a rebuild"
    );
    edges
}

// D9 (2026-09-29 usage evaluation): `from flask import url_for` names a
// re-export (`flask/__init__.py` does `from .helpers import url_for`). When the
// module the re-export points at appears in a LATER run, the importer kept the
// import it resolved while `url_for` did not exist yet, and that stale import
// then pruned the call a rebuild binds to the new definition.
#[test]
fn a_python_reexport_importer_is_rebound_when_the_definition_appears() {
    let before: &[(&str, &str)] = &[
        (
            "flask/__init__.py",
            "from .helpers import url_for as url_for\n",
        ),
        (
            "flask/app.py",
            "class Flask:\n    def url_for(self, endpoint):\n        return endpoint\n",
        ),
        (
            "examples/auth.py",
            "from flask import url_for\n\ndef login():\n    return url_for('x')\n",
        ),
    ];
    let edges = assert_incremental_edges_match_rebuild(
        before,
        &[(
            "flask/helpers.py",
            Some("def url_for(endpoint):\n    return endpoint\n"),
        )],
    );
    assert!(
        edges
            .iter()
            .any(|e| e == "examples/auth.py.login --calls--> flask/helpers.py.url_for"),
        "{edges:#?}"
    );
}

// The same importer when the re-exported module already exists and only gains
// the definition in a later edit.
#[test]
fn a_python_reexport_importer_is_rebound_when_the_definition_is_added() {
    let before: &[(&str, &str)] = &[
        (
            "flask/__init__.py",
            "from .helpers import url_for as url_for\n",
        ),
        ("flask/helpers.py", "def other():\n    return 1\n"),
        (
            "flask/app.py",
            "class Flask:\n    def url_for(self, endpoint):\n        return endpoint\n",
        ),
        (
            "examples/auth.py",
            "from flask import url_for\n\ndef login():\n    return url_for('x')\n",
        ),
    ];
    let edges = assert_incremental_edges_match_rebuild(
        before,
        &[(
            "flask/helpers.py",
            Some("def other():\n    return 1\n\ndef url_for(endpoint):\n    return endpoint\n"),
        )],
    );
    assert!(
        edges
            .iter()
            .any(|e| e == "examples/auth.py.login --calls--> flask/helpers.py.url_for"),
        "{edges:#?}"
    );
}

// D9 on the JS/TS re-export paths: a CommonJS package entry that re-exports a
// module (`module.exports = require('./lib/helpers')`, C3) and an ESM
// `export { x } from './helpers'`, each when the re-exported definition
// appears in a later run as a new file or as an edit.
const CJS_ENTRY: (&str, &str) = ("index.js", "module.exports = require('./lib/helpers')\n");
const CJS_IMPORTER: (&str, &str) = (
    "test/a.js",
    "const { urlFor } = require('..')\nfunction t() {\n  return urlFor('x')\n}\nmodule.exports = t\n",
);
const CJS_OTHER: &str = "exports.other = function () {\n  return 1\n}\n";
const CJS_DEF: &str =
    "exports.other = function () {\n  return 1\n}\nexports.urlFor = function (e) {\n  return e\n}\n";
const TS_ENTRY: (&str, &str) = ("src/index.ts", "export { urlFor } from './helpers'\n");
const TS_IMPORTER: (&str, &str) = (
    "src/app.ts",
    "import { urlFor } from './index'\nexport function t() {\n  return urlFor('x')\n}\n",
);
const TS_OTHER: &str = "export function other() {\n  return 1\n}\n";
const TS_DEF: &str =
    "export function other() {\n  return 1\n}\nexport function urlFor(e: string) {\n  return e\n}\n";

#[test]
fn a_cjs_reexport_importer_is_rebound_when_the_module_appears() {
    assert_incremental_edges_match_rebuild(
        &[CJS_ENTRY, CJS_IMPORTER],
        &[("lib/helpers.js", Some(CJS_DEF))],
    );
}

#[test]
fn a_cjs_reexport_importer_is_rebound_when_the_definition_is_added() {
    assert_incremental_edges_match_rebuild(
        &[CJS_ENTRY, CJS_IMPORTER, ("lib/helpers.js", CJS_OTHER)],
        &[("lib/helpers.js", Some(CJS_DEF))],
    );
}

#[test]
fn an_esm_reexport_importer_is_rebound_when_the_module_appears() {
    assert_incremental_edges_match_rebuild(
        &[TS_ENTRY, TS_IMPORTER],
        &[("src/helpers.ts", Some(TS_DEF))],
    );
}

#[test]
fn an_esm_reexport_importer_is_rebound_when_the_definition_is_added() {
    assert_incremental_edges_match_rebuild(
        &[TS_ENTRY, TS_IMPORTER, ("src/helpers.ts", TS_OTHER)],
        &[("src/helpers.ts", Some(TS_DEF))],
    );
}

// The reverse direction of every shape above: the definition goes away again,
// by an edit or with its file.
#[test]
fn a_reexport_importer_is_rebound_when_the_definition_goes_away() {
    let py = [
        (
            "flask/__init__.py",
            "from .helpers import url_for as url_for\n",
        ),
        (
            "flask/app.py",
            "class Flask:\n    def url_for(self, endpoint):\n        return endpoint\n",
        ),
        (
            "examples/auth.py",
            "from flask import url_for\n\ndef login():\n    return url_for('x')\n",
        ),
        (
            "flask/helpers.py",
            "def other():\n    return 1\n\ndef url_for(endpoint):\n    return endpoint\n",
        ),
    ];
    assert_incremental_edges_match_rebuild(
        &py,
        &[("flask/helpers.py", Some("def other():\n    return 1\n"))],
    );
    assert_incremental_edges_match_rebuild(&py, &[("flask/helpers.py", None)]);
    let cjs = [CJS_ENTRY, CJS_IMPORTER, ("lib/helpers.js", CJS_DEF)];
    assert_incremental_edges_match_rebuild(&cjs, &[("lib/helpers.js", Some(CJS_OTHER))]);
    assert_incremental_edges_match_rebuild(&cjs, &[("lib/helpers.js", None)]);
    let ts = [TS_ENTRY, TS_IMPORTER, ("src/helpers.ts", TS_DEF)];
    assert_incremental_edges_match_rebuild(&ts, &[("src/helpers.ts", Some(TS_OTHER))]);
    assert_incremental_edges_match_rebuild(&ts, &[("src/helpers.ts", None)]);
}

// The Python shapes a `from m import x` binding can take besides the re-export:
// bound by name to another module's `x` (the named module had none), and the
// same import when the module defines `x` in a later run.
#[test]
fn a_python_from_import_bound_elsewhere_is_rebound_when_its_module_defines_it() {
    let before: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        ("pkg/helpers.py", "def other():\n    return 1\n"),
        ("pkg/tools.py", "def url_for(endpoint):\n    return 2\n"),
        (
            "app.py",
            "from pkg.helpers import url_for\n\ndef login():\n    return url_for('x')\n",
        ),
    ];
    let edges = assert_incremental_edges_match_rebuild(
        before,
        &[(
            "pkg/helpers.py",
            Some("def other():\n    return 1\n\ndef url_for(endpoint):\n    return endpoint\n"),
        )],
    );
    assert!(
        edges
            .iter()
            .any(|e| e == "app.py.login --calls--> pkg/helpers.py.url_for"),
        "{edges:#?}"
    );
}

// D9's fan-out arm re-extracts only the importers a rebuild binds differently:
// not one that found its name in the module it names, not one in a language
// the new definition cannot bind, and a D10A `<module>` fallback only when its
// file gained a name.
#[test]
fn the_module_import_fanout_pulls_only_importers_a_rebuild_rebinds() {
    let base: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        ("pkg/helpers.py", "def url_for(e):\n    return e\n"),
        (
            "app.py",
            "from pkg.helpers import url_for\n\ndef a():\n    return url_for(1)\n",
        ),
        (
            "other.py",
            "from pkg import url_for\n\ndef b():\n    return url_for(2)\n",
        ),
        ("lib/helpers.js", "exports.urlFor = function (e) {\n  return e\n}\n"),
        (
            "main.js",
            "const { urlFor } = require('./lib/helpers')\nfunction m() {\n  return urlFor(1)\n}\nmodule.exports = m\n",
        ),
        (
            "web.js",
            "const { url_for } = require('./lib/nothing')\nfunction w() {\n  return url_for(1)\n}\nmodule.exports = w\n",
        ),
        ("rel/__init__.py", ""),
        ("rel/helpers.py", "X = 1\n\ndef f():\n    return 1\n"),
        ("rel/user.py", "from .helpers import X\n"),
        ("rel/sub.py", "from . import helpers\n"),
        ("uses_json.py", "import json\n"),
    ];
    let refresh = |path: &str, body: &str| -> Vec<String> {
        let (project, _d, db) = fresh_index_of(base);
        let paths = vec![path.to_string()];
        super::resolve::snapshot_definition_counts(db.conn(), &paths).unwrap();
        let full = project.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, body).unwrap();
        index_files(
            &db,
            project.path(),
            &paths,
            &std::collections::HashMap::new(),
            None,
            &[],
            None,
        )
        .unwrap();
        super::resolve::bare_name_callers_of_new_duplicates(db.conn(), &Default::default()).unwrap()
    };
    // `app.py` found `url_for` in `pkg.helpers`; `web.js` is JavaScript.
    assert_eq!(
        refresh("pkg/tools.py", "def url_for(e):\n    return 2\n"),
        vec!["other.py".to_string()]
    );
    // A module import names a module: `json` is no project definition, and a
    // new file's `<module>` rebinds no import of another module.
    let json = refresh("pkg/j.py", "def json():\n    return 1\n");
    assert!(json.is_empty(), "{json:?}");
    // `main.js` found `urlFor` in the file its specifier names.
    let js = refresh(
        "lib/more.js",
        "exports.urlFor = function (e) {\n  return 2\n}\n",
    );
    assert!(js.is_empty(), "{js:?}");
    // An edit that adds no name leaves the fallback alone; one that does not.
    let body_only = refresh("rel/helpers.py", "X = 1\n\ndef f():\n    return 2\n");
    assert!(body_only.is_empty(), "{body_only:?}");
    assert_eq!(
        refresh(
            "rel/helpers.py",
            "X = 1\n\ndef f():\n    return 1\n\ndef g():\n    return 3\n"
        ),
        vec!["rel/sub.py".to_string(), "rel/user.py".to_string()]
    );
}

// D6 (2026-09-29 usage evaluation): a route whose handler lives in another
// file — the canonical `import { getUser } from './ctrl'; app.get('/users',
// getUser)` — is stored as the handler's self-edge, inside the HANDLER's file,
// and nothing records the route's file. hono lost one such edge
// (`app.use(mw1, mw2)` in `types.test.ts`, bound to `hono.test.ts`'s `mw2`)
// whenever `hono.test.ts` was re-indexed.
const ROUTE_CTRL: (&str, &str) = (
    "ctrl.ts",
    "export function getUser(c: any) {\n  return c\n}\n",
);
const ROUTE_FILE: (&str, &str) = (
    "routes.ts",
    "import { getUser } from './ctrl'\nimport { Hono } from 'hono'\nconst app = new Hono()\napp.get('/users', getUser)\n",
);

#[test]
fn an_imported_handler_route_survives_an_edit_of_the_handler_file() {
    let edges = assert_incremental_edges_match_rebuild(
        &[ROUTE_CTRL, ROUTE_FILE],
        &[(
            "ctrl.ts",
            Some("export function getUser(c: any) {\n  return c.json(1)\n}\n"),
        )],
    );
    assert!(
        edges
            .iter()
            .any(|e| e == "ctrl.ts.getUser --routes_to--> ctrl.ts.getUser"),
        "{edges:#?}"
    );
}

#[test]
fn an_imported_handler_route_goes_when_the_route_file_drops_it() {
    assert_incremental_edges_match_rebuild(
        &[ROUTE_CTRL, ROUTE_FILE],
        &[(
            "routes.ts",
            Some("import { getUser } from './ctrl'\nimport { Hono } from 'hono'\nconst app = new Hono()\n"),
        )],
    );
    assert_incremental_edges_match_rebuild(&[ROUTE_CTRL, ROUTE_FILE], &[("routes.ts", None)]);
}

#[test]
fn an_imported_handler_route_follows_its_handler_to_another_file() {
    let other = (
        "legacy/ctrl.ts",
        "export function getUser(c: any) {\n  return 2\n}\n",
    );
    assert_incremental_edges_match_rebuild(&[ROUTE_CTRL, ROUTE_FILE, other], &[("ctrl.ts", None)]);
    assert_incremental_edges_match_rebuild(
        &[ROUTE_CTRL, ROUTE_FILE, other],
        &[(
            "ctrl.ts",
            Some("export function other() {\n  return 1\n}\n"),
        )],
    );
}

// hono's own shape, minimized: no import, the route file routes to a local
// `const` that is no node, and the name pool binds another file's `mw2`.
#[test]
fn a_route_bound_by_name_to_another_file_survives_its_edit() {
    let handler = "const mw2 =\n  () =>\n  async (c: any, next: any) => {\n    await next()\n  }\n";
    let edges = assert_incremental_edges_match_rebuild(
        &[
            ("src/hono.test.ts", handler),
            (
                "src/types.test.ts",
                "import { Hono } from 'hono'\nconst app = new Hono()\n\
                 const mw1 = createMiddleware(async () => {})\n\
                 const mw2 = createMiddleware(async () => {})\n\
                 app.use(mw1, mw2).get('/', (c: any) => c.json(1))\n",
            ),
        ],
        &[("src/hono.test.ts", Some(&format!("{handler}// touched\n")))],
    );
    assert!(
        edges
            .iter()
            .any(|e| e == "src/hono.test.ts.mw2 --routes_to--> src/hono.test.ts.mw2"),
        "{edges:#?}"
    );
}

// A route bound by name follows the handler a rebuild would pick when a
// closer same-name handler appears. (One appearing where none existed is not
// covered: the route and its value reference were dropped with no record,
// and only calls keep one, in `pending_unresolved_calls`.)
#[test]
fn a_route_bound_by_name_follows_a_closer_same_name_handler() {
    let routes = (
        "api/routes.ts",
        "import { Hono } from 'hono'\nconst app = new Hono()\napp.get('/users', getUser)\n",
    );
    let far = (
        "far/away/h.ts",
        "export function getUser(c: any) {\n  return 1\n}\n",
    );
    let near = "export function getUser(c: any) {\n  return 2\n}\n";
    let edges = assert_incremental_edges_match_rebuild(&[routes, far], &[("api/h.ts", Some(near))]);
    assert!(
        !edges
            .iter()
            .any(|e| e.starts_with("far/away/h.ts.getUser --routes_to-->")),
        "{edges:#?}"
    );
}

/// The context string of the node `name` in `path`.
fn context_string_of(db: &Database, path: &str, name: &str) -> Option<String> {
    db.conn()
        .query_row(
            "SELECT n.context_string FROM nodes n JOIN files f ON f.id = n.file_id \
             WHERE f.path = ?1 AND n.name = ?2",
            [path, name],
            |r| r.get(0),
        )
        .unwrap()
}

// The handler's context string names its route, so it follows the route file.
#[test]
fn an_imported_handler_context_string_follows_its_route_file() {
    let (project, _d, db) = fresh_index_of(&[ROUTE_CTRL, ROUTE_FILE]);
    let dropped =
        "import { getUser } from './ctrl'\nimport { Hono } from 'hono'\nconst app = new Hono()\n";
    fs::write(project.path().join("routes.ts"), dropped).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, control) = fresh_index_of(&[ROUTE_CTRL, ("routes.ts", dropped)]);
    assert_eq!(
        context_string_of(&db, "ctrl.ts", "getUser"),
        context_string_of(&control, "ctrl.ts", "getUser")
    );
}

// D10B (2026-09-29 usage evaluation): a Python call on a receiver the source
// leaves untyped — an attribute of `self` (`self.serializer.tag()`), or a name
// a relative import binds (`_cv_app.get()`) — has no qualifier, so the same-file
// tier bound the caller's own file's `tag` / `get` as if it were a bare call, at
// `extracted`. On flask that was 12 of the 22 wrong `extracted` edges. Such a
// bind is a by-name guess and is labelled like one; a local receiver (`ctx.f()`,
// `q` member) and a call on the instance keep `extracted`.
#[test]
fn a_python_untyped_receiver_same_file_bind_is_labelled_by_its_name_count() {
    let files: &[(&str, &str)] = &[
        ("pkg/__init__.py", ""),
        (
            "pkg/globals.py",
            "class _Var:\n    def get(self):\n        return 1\n\n_cv_app = _Var()\n",
        ),
        (
            "pkg/tag.py",
            "from .globals import _cv_app\n\n\nclass JSONTag:\n    def tag(self, value):\n        \
             return value\n\n    def to_json(self, value):\n        return self.serializer.tag(value)\n\n    \
             def spin(self):\n        return self.engine.whirl()\n\n    def both(self, value):\n        \
             self.serializer.tag(value)\n        return self.tag(value)\n\n    def current(self):\n        \
             return _cv_app.get()\n\n\nclass TaggedJSONSerializer:\n    def tag(self, value):\n        \
             return value\n\n    def whirl(self):\n        return 1\n\n\nclass Store:\n    \
             def get(self):\n        return 2\n\n\ndef use(ctx):\n    return ctx.whirl()\n",
        ),
    ];
    let (project, _d, db) = fresh_index_of(files);
    let confs = |rows: &[(String, String, String, String)], s: &str, t: &str| -> Vec<String> {
        let mut v: Vec<String> = rows
            .iter()
            .filter(|(a, r, b, _)| a == s && r == REL_CALLS && b == t)
            .map(|(_, _, _, c)| c.clone())
            .collect();
        v.sort();
        v
    };
    let rows = graph_projection_with_confidence(&db);
    assert_eq!(
        confs(&rows, "pkg/tag.py:to_json", "pkg/tag.py:tag"),
        ["ambiguous", "ambiguous"],
        "both same-file `tag`s are a guess among two definitions: {rows:#?}"
    );
    assert_eq!(
        confs(&rows, "pkg/tag.py:current", "pkg/tag.py:get"),
        ["ambiguous"],
        "`get` has two definitions: {rows:#?}"
    );
    assert_eq!(
        confs(&rows, "pkg/tag.py:spin", "pkg/tag.py:whirl"),
        ["inferred"],
        "`whirl` has one definition: {rows:#?}"
    );
    assert!(
        confs(&rows, "pkg/tag.py:both", "pkg/tag.py:tag").contains(&"extracted".to_string()),
        "`self.tag()` names the class, whatever else the caller does: {rows:#?}"
    );
    assert_eq!(
        confs(&rows, "pkg/tag.py:use", "pkg/tag.py:whirl"),
        ["extracted"],
        "a local receiver keeps its label (measured right 63 of 80 times): {rows:#?}"
    );

    // A second `whirl`, in a file this run is the only one to see, relabels the
    // untouched same-file edge; the result equals a rebuild.
    let engine = "class Engine:\n    def whirl(self):\n        return 3\n";
    fs::write(project.path().join("pkg/engine.py"), engine).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, control) =
        fresh_index_of(&[files[0], files[1], files[2], ("pkg/engine.py", engine)]);
    let inc = graph_projection_with_confidence(&db);
    assert_eq!(
        confs(&inc, "pkg/tag.py:spin", "pkg/tag.py:whirl"),
        ["ambiguous"],
        "{inc:#?}"
    );
    assert_eq!(
        inc,
        graph_projection_with_confidence(&control),
        "incremental must equal a rebuild"
    );
}

// B9 (2026-09-29 usage evaluation): a member call on a Node built-in module
// (`path.resolve(p)`, `fs.renameSync(a, b)`) runs Node's code, yet it bound a
// project function or method of that name elsewhere (express: a test's
// `path.resolve` → `View.prototype.resolve`; this repo: `fs.renameSync` → a
// test's `renameSync` mock) — 8 of 8 such edges over three corpora wrong on
// reading the code. A bare call through the same binding was already dropped
// (D7); a member call on it now is too. A parameter of that name is not the
// module, and another package (a workspace package in a monorepo) is untouched.
#[test]
fn a_member_call_on_a_node_builtin_module_binds_no_project_function() {
    let files: &[(&str, &str)] = &[
        (
            "lib/view.js",
            "function View() {}\nView.prototype.resolve = function resolve(dir, file) { return dir + file; };\n\
             View.prototype.readFile = function readFile(p) { return p; };\nmodule.exports = View;\n",
        ),
        (
            "lib/static.js",
            "var path = require('path');\nvar fs = require('node:fs');\nconst { promises: fsp } = require('fs');\n\
             function serve(p) {\n  path.resolve(p);\n  require('path').resolve(p);\n  fs.promises.readFile(p);\n  \
             return fsp.readFile(p);\n}\nfunction local(path) { return path.resolve(1); }\nmodule.exports = serve;\n",
        ),
        (
            "packages/app/main.js",
            "import * as lib from '@org/lib';\nexport function main() { return lib.helper(1); }\n",
        ),
        ("packages/lib/index.js", "export function helper(x) { return x; }\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    for e in [
        "lib/static.js.serve --calls--> lib/view.js.resolve",
        "lib/static.js.serve --calls--> lib/view.js.readFile",
    ] {
        assert!(
            !has(e),
            "a call into Node's own module reached {e}: {edges:#?}"
        );
    }
    assert!(
        has("lib/static.js.local --calls--> lib/view.js.resolve"),
        "a parameter named `path` is not the module: {edges:#?}"
    );
    assert!(
        has("packages/app/main.js.main --calls--> packages/lib/index.js.helper"),
        "a package the project holds stays reachable: {edges:#?}"
    );
}

// D#193(2): a bare Python name in a value position (`@cache`, `register(cache)`)
// bound a same-named METHOD of another file: `@cache` from functools drew
// `references -> Store.cache`. A bare name reaches a module-level binding, a
// builtin or an enclosing function's local; a class member of another file is
// none of them. Same-file methods stay reachable (`x = property(getx)` in the
// class body), and so does another file's module-level function.
#[test]
fn a_python_bare_name_reference_binds_no_method_of_another_file() {
    let files: &[(&str, &str)] = &[
        (
            "app/store.py",
            "class Store:\n    def cache(self):\n        return {}\n\n\n\
             class Ctx:\n    def contextmanager(self):\n        return None\n",
        ),
        ("app/hooks.py", "def on_load():\n    return 0\n"),
        (
            "app/util.py",
            "from functools import cache\nfrom contextlib import contextmanager\n\
             from app.hooks import on_load\n\n\n\
             @cache\ndef load():\n    return 1\n\n\n\
             @contextmanager\ndef opened():\n    yield 1\n\n\n\
             def register(f):\n    return f\n\n\n\
             def wire():\n    register(cache)\n    return register(on_load)\n\n\n\
             class Prop:\n    def getx(self):\n        return 1\n    x = property(getx)\n",
        ),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    for e in [
        "app/util.py.load --references--> app/store.py.cache",
        "app/util.py.opened --references--> app/store.py.contextmanager",
        "app/util.py.wire --references--> app/store.py.cache",
    ] {
        assert!(
            !has(e),
            "a bare name reached another file's method: {e}: {edges:#?}"
        );
    }
    assert!(
        has("app/util.py.wire --references--> app/hooks.py.on_load"),
        "another file's module-level function stays reachable: {edges:#?}"
    );
    assert!(
        edges
            .iter()
            .any(|x| x.ends_with("--references--> app/util.py.getx")),
        "a method referenced from its own class body stays reachable: {edges:#?}"
    );
}

// D#192(1) full-index half: `from .util import helper` looked `helper` up among
// ALL nodes of util.py and bound the method `Box.helper`. A module attribute is
// never a class member; with no module-level `helper`, the relative import
// binds the module it names (D10), as for a variable.
#[test]
fn a_python_module_resolved_from_import_binds_no_method() {
    let files: &[(&str, &str)] = &[
        ("pkg/__init__.py", "from .util import helper as helper\n"),
        (
            "pkg/util.py",
            "class Box:\n    def helper(self):\n        return 2\n\n\ndef other():\n    return 0\n",
        ),
        ("pkg/plain.py", "from .util import other\n"),
    ];
    let (_p, _d, db) = fresh_index_of(files);
    let edges = edge_set(&db);
    let has = |e: &str| edges.iter().any(|x| x == e);
    assert!(
        !has("pkg/__init__.py.<module> --imports--> pkg/util.py.helper"),
        "{edges:#?}"
    );
    assert!(
        has("pkg/__init__.py.<module> --imports--> pkg/util.py.<module>"),
        "{edges:#?}"
    );
    assert!(
        has("pkg/plain.py.<module> --imports--> pkg/util.py.other"),
        "a module-level function still binds by its module: {edges:#?}"
    );
}

// D#192(1) incremental half: util.py loses `def helper` and gains a method of
// that name. Phase 2c restored the importer's `imports` and `calls` edges by
// bare name onto `Box.helper`; a rebuild binds neither. The whole edge set,
// not only calls, must equal the rebuild's.
#[test]
fn a_python_edge_is_never_restored_onto_a_method_it_did_not_point_at() {
    let before: &[(&str, &str)] = &[
        (
            "app.py",
            "from pkg import helper\n\n\ndef main():\n    return helper()\n",
        ),
        ("pkg/__init__.py", "from .util import helper as helper\n"),
        ("pkg/util.py", "def helper():\n    return 1\n"),
    ];
    let after_util =
        "\n\nclass Box:\n    def helper(self):\n        return 2\n\n\ndef other():\n    return 0\n";
    let (project, _d, db) = fresh_index_of(before);
    fs::write(project.path().join("pkg/util.py"), after_util).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, control) = fresh_index_of(&[before[0], before[1], ("pkg/util.py", after_util)]);
    assert_eq!(
        edge_set(&db),
        edge_set(&control),
        "incremental must equal a rebuild"
    );
    // A method that kept its qualified name is still restored in place.
    let before: &[(&str, &str)] = &[
        (
            "app.py",
            "from pkg.util import Box\n\n\ndef main(b):\n    return Box().helper()\n",
        ),
        ("pkg/__init__.py", ""),
        (
            "pkg/util.py",
            "class Box:\n    def helper(self):\n        return 2\n",
        ),
    ];
    let after_util =
        "class Box:\n    def helper(self):\n        return 3\n\n\ndef tail():\n    return 0\n";
    let (project, _d, db) = fresh_index_of(before);
    let edges_before = edge_set(&db);
    assert!(
        edges_before
            .iter()
            .any(|x| x == "app.py.main --calls--> pkg/util.py.helper"),
        "{edges_before:#?}"
    );
    fs::write(project.path().join("pkg/util.py"), after_util).unwrap();
    run_incremental_index(&db, project.path(), None, None).unwrap();
    let (_p2, _d2, control) = fresh_index_of(&[before[0], before[1], ("pkg/util.py", after_util)]);
    assert_eq!(edge_set(&db), edge_set(&control));
    assert!(edge_set(&db)
        .iter()
        .any(|x| x == "app.py.main --calls--> pkg/util.py.helper"));
}

// D#192(2): `from . import newmod` written before `newmod.py` exists binds the
// package's `__init__.py` (D10: a name that is no node of the module binds the
// file it names). When `newmod.py` appears, a rebuild binds the import to it,
// but the incremental run never re-extracted the importer: the edge stayed on
// `__init__.py` and `affected pkg/newmod.py` named no dependent. An appearing
// module re-extracts the files whose relative import sits on its package.
#[test]
fn a_python_module_appearing_after_its_relative_import_takes_the_import() {
    let mut diverged = Vec::new();
    for (before, appearing) in [
        (
            vec![
                ("pkg/__init__.py", ""),
                (
                    "pkg/app.py",
                    "from . import newmod\n\n\ndef main():\n    return newmod.f()\n",
                ),
            ],
            ("pkg/newmod.py", "def f():\n    return 1\n"),
        ),
        (
            vec![
                ("pkg/__init__.py", ""),
                ("pkg/sub/__init__.py", ""),
                (
                    "pkg/sub/app.py",
                    "from .. import consts\nfrom . import tools\n\n\ndef main():\n    return consts.VALUE\n",
                ),
            ],
            ("pkg/consts.py", "VALUE = 1\n\n\ndef helper():\n    return 2\n"),
        ),
        (
            vec![
                ("pkg/__init__.py", ""),
                ("pkg/sub/__init__.py", ""),
                ("pkg/sub/app.py", "from . import tools\n"),
            ],
            ("pkg/sub/tools/__init__.py", "def run():\n    return 0\n"),
        ),
    ] {
        let (project, _d, db) = fresh_index_of(&before);
        let path = project.path().join(appearing.0);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, appearing.1).unwrap();
        run_incremental_index(&db, project.path(), None, None).unwrap();
        let mut tree = before.clone();
        tree.push(appearing);
        let (_p2, _d2, control) = fresh_index_of(&tree);
        let (incremental, rebuild) = (edge_set(&db), edge_set(&control));
        if incremental != rebuild {
            diverged.push(format!(
                "{} appeared:\n  incremental {incremental:?}\n  rebuild     {rebuild:?}",
                appearing.0
            ));
        }
    }
    assert!(
        diverged.is_empty(),
        "incremental must equal a rebuild:\n{}",
        diverged.join("\n")
    );
}
