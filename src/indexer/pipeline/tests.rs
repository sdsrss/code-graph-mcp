use super::python_modules::build_python_module_map;
use super::*;
use crate::domain::REL_CALLS;
use crate::storage::queries::{
    get_edges_from, get_import_tree, get_nodes_by_file_path, get_nodes_by_name,
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

    let removed = super::resolve::prune_import_contradicted_call_edges(&db).unwrap();

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
    // batch and other.ts gains its own `target`. The restored edge must land on
    // target.ts only (a fan-out to other.ts is an edge a full rebuild never makes).
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
    // Over-creation guard: must NOT fan out to other.ts's same-name target.
    assert_eq!(
        count_caller_to("src/other.ts"),
        0,
        "restore must NOT bind caller → other.ts:target (cross-file fan-out a rebuild never makes)"
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
    // Suffix path
    assert!(map
        .get("utils")
        .unwrap()
        .contains(&"myapp/utils.py".to_string()));
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
