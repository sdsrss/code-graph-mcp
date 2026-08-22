//! End-to-end tests for the bare-name call qualifier resolver rules.
//! See docs/superpowers/specs/2026-05-11-bare-name-call-qualifier-design.md.

use code_graph_mcp::indexer::pipeline::{run_full_index, run_incremental_index};
use code_graph_mcp::storage::db::Database;
use std::fs;
use tempfile::TempDir;

fn write(dir: &std::path::Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&p, content).unwrap();
}

fn callers_of(db: &Database, target_name: &str) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT COALESCE(src.qualified_name, src.name) FROM edges e
         JOIN nodes tgt ON tgt.id = e.target_id
         JOIN nodes src ON src.id = e.source_id
         WHERE e.relation = 'calls' AND tgt.name = ?",
        )
        .unwrap();
    let rows = stmt
        .query_map([target_name], |r| r.get::<_, String>(0))
        .unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

fn callers_of_in_file(db: &Database, target_name: &str, file_rel: &str) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT COALESCE(src.qualified_name, src.name) FROM edges e
         JOIN nodes tgt ON tgt.id = e.target_id
         JOIN nodes src ON src.id = e.source_id
         JOIN files f ON f.id = tgt.file_id
         WHERE e.relation = 'calls' AND tgt.name = ? AND f.path = ?",
        )
        .unwrap();
    let rows = stmt
        .query_map([target_name, file_rel], |r| r.get::<_, String>(0))
        .unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

#[test]
fn chain_builder_drops_intermediate_callers() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Project has a function literally named `create` in src/snapshot/mod.rs.
    write(root, "src/snapshot/mod.rs", "pub fn create() {}\n");
    // Caller does a builder chain — `.create(true)` is a method on OpenOptions,
    // NOT the project's snapshot::create.
    write(
        root,
        "src/caller.rs",
        r#"
        use std::fs::OpenOptions;
        pub fn caller() {
            OpenOptions::new().create(true).open("/tmp/x").ok();
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "create");
    assert!(
        !callers.iter().any(|c| c.contains("caller")),
        "snapshot::create must NOT have `caller` as caller (it called .create() in a builder chain), got: {:?}",
        callers
    );
}

#[test]
fn bare_name_qualifier_drops_phantom_callers_for_file_create() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Project has snapshot::create.
    write(root, "src/snapshot/mod.rs", "pub fn create() {}\n");
    // Caller calls std::fs::File::create — Path qualifier with first segment
    // "File" which is NOT a project module → drop.
    write(
        root,
        "src/caller.rs",
        r#"
        use std::fs::File;
        pub fn caller() { let _ = File::create("/tmp/x"); }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "create");
    assert!(
        !callers.iter().any(|c| c.contains("caller")),
        "snapshot::create must NOT have `caller` (caller called std::fs::File::create), got: {:?}",
        callers
    );
}

#[test]
fn path_qualifier_picks_module_specific_candidate() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Two project modules each with a `create` fn.
    write(root, "src/snapshot/mod.rs", "pub fn create() {}\n");
    write(root, "src/builder/mod.rs", "pub fn create() {}\n");
    // Caller explicitly targets snapshot::create.
    write(
        root,
        "src/caller.rs",
        r#"
        pub fn caller() { crate::snapshot::create(); }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    // snapshot::create gets the caller; builder::create does not.
    let snap = callers_of_in_file(&db, "create", "src/snapshot/mod.rs");
    let bld = callers_of_in_file(&db, "create", "src/builder/mod.rs");

    assert!(
        snap.iter().any(|c| c.contains("caller")),
        "snapshot::create should have caller, got: {:?}",
        snap
    );
    assert!(
        !bld.iter().any(|c| c.contains("caller")),
        "builder::create should NOT have caller (qualifier was snapshot), got: {:?}",
        bld
    );
}

#[test]
fn self_method_within_impl_uses_correct_type() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write(
        root,
        "src/db.rs",
        r#"
        pub struct Db;
        impl Db {
            pub fn caller(&self) { self.helper(); }
            pub fn helper(&self) {}
        }
    "#,
    );
    // Sibling type with same-named method — must NOT win.
    write(
        root,
        "src/other.rs",
        r#"
        pub struct Other;
        impl Other {
            pub fn helper(&self) {}
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let db_helper = callers_of_in_file(&db, "helper", "src/db.rs");
    let other_helper = callers_of_in_file(&db, "helper", "src/other.rs");

    assert!(
        db_helper.iter().any(|c| c.contains("caller")),
        "Db::helper should have Db::caller, got: {:?}",
        db_helper
    );
    assert!(
        !other_helper.iter().any(|c| c.contains("caller")),
        "Other::helper should NOT have Db::caller, got: {:?}",
        other_helper
    );
}

#[test]
fn self_method_resolves_across_split_impl_blocks() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Db's caller is in db_a.rs; Db's helper is in db_b.rs (impl block split).
    write(
        root,
        "src/db_a.rs",
        r#"
        pub struct Db;
        impl Db {
            pub fn caller(&self) { self.helper(); }
        }
    "#,
    );
    write(
        root,
        "src/db_b.rs",
        r#"
        impl crate::db_a::Db {
            pub fn helper(&self) {}
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let helpers = callers_of_in_file(&db, "helper", "src/db_b.rs");
    assert!(
        helpers.iter().any(|c| c.contains("caller")),
        "Db::helper in db_b.rs should have Db::caller from db_a.rs, got: {:?}",
        helpers
    );
}

#[test]
fn non_rust_callgraph_unchanged() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // JS file with simple function call — must not be qualifier-filtered.
    write(
        root,
        "src/util.js",
        r#"
        function helper() {}
        function caller() { helper(); }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let mut stmt = db
        .conn()
        .prepare(
            "SELECT COUNT(*) FROM edges e
         JOIN nodes src ON src.id = e.source_id
         JOIN nodes tgt ON tgt.id = e.target_id
         WHERE e.relation = 'calls'
           AND src.name = 'caller'
           AND tgt.name = 'helper'",
        )
        .unwrap();
    let count: i64 = stmt.query_row([], |r| r.get(0)).unwrap();
    assert_eq!(
        count, 1,
        "JS caller→helper edge must survive (no qualifier filtering for non-Rust)"
    );
}

#[test]
fn path_qualifier_resolves_single_file_rust_mod() {
    // Regression: path_filter_candidates only looked for "/domain/" or
    // "domain/" directory boundaries, so `crate::domain::foo()` resolving to
    // a function in `src/domain.rs` (single-file mod, no directory) silently
    // dropped — every cross-file qualified call into a single-file mod marked
    // the target as dead code. Accept `<last_seg>.rs` suffix too.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/domain.rs",
        r#"
        pub fn helper_in_domain() -> i32 { 42 }
    "#,
    );
    write(
        root,
        "src/main.rs",
        r#"
        pub fn caller() -> i32 {
            crate::domain::helper_in_domain()
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "helper_in_domain");
    assert!(
        callers.iter().any(|c| c == "caller"),
        "caller→crate::domain::helper_in_domain() must resolve when target lives in src/domain.rs (single-file mod); got: {:?}",
        callers
    );
}

#[test]
fn same_file_generic_impl_method_edges_dont_fan_out() {
    // Regression: 3 structs each `impl SameTrait for StructX` in one file used
    // to produce 3×3 = 9 method-edge slots per method name (every struct
    // appeared to implement every same-name method) because Phase 2 resolved
    // bare target_name "run" against all 3 same-name method nodes in the
    // file. Parser now stamps {"q":"impl_method","v":"<Type>"} so the resolver
    // filters method candidates by qualified_name LIKE "<Type>.%".
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/lib.rs",
        r#"
        pub trait DoWork { fn run(&self); }
        pub struct A;
        impl DoWork for A { fn run(&self) {} }
        pub struct B<T>(T);
        impl<T: Clone> DoWork for B<T> { fn run(&self) {} }
        pub struct C<'a, U>(&'a U);
        impl<'a, U: Default> DoWork for C<'a, U> { fn run(&self) {} }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let mut stmt = db
        .conn()
        .prepare(
            "SELECT src.name, tgt.qualified_name
         FROM edges e
         JOIN nodes src ON src.id = e.source_id
         JOIN nodes tgt ON tgt.id = e.target_id
         WHERE e.relation = 'implements'
           AND tgt.name = 'run'
         ORDER BY src.name",
        )
        .unwrap();
    let pairs: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    // Each struct must implement only its own method (3 edges, not 9).
    assert_eq!(
        pairs.len(),
        3,
        "expected one implements edge per (struct, its-own-run) pair; got {:?}",
        pairs
    );
    assert!(
        pairs.contains(&("A".to_string(), "A.run".to_string())),
        "A should implement A.run; got {:?}",
        pairs
    );
    assert!(
        pairs.contains(&("B".to_string(), "B.run".to_string())),
        "B should implement B.run (bare, no <T>); got {:?}",
        pairs
    );
    assert!(
        pairs.contains(&("C".to_string(), "C.run".to_string())),
        "C should implement C.run (bare, no <'a, U>); got {:?}",
        pairs
    );
}

#[test]
fn path_qualifier_keeps_same_file_target() {
    // Regression: the Path branch of edge resolution filtered out local_ids
    // (same-file targets) before applying the path filter, contradicting the
    // spec's "same-file matches still take precedence". Net effect: a Rust
    // file with `impl Foo { fn helper() }` and a sibling caller doing
    // `Foo::helper()` produced no call edge — same-file pool was excluded,
    // and the cross-file Path filter (which scans `/Foo/` in the file path)
    // never matched in a single-file project.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/lib.rs",
        r#"
        pub struct Foo;
        impl Foo {
            pub fn helper() -> i32 { 42 }
        }
        pub fn caller() -> i32 {
            Foo::helper()
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "helper");
    assert!(
        callers.iter().any(|c| c == "caller"),
        "caller→Foo::helper() must produce a call edge even when target is in the same file; got: {:?}",
        callers
    );
}

#[test]
fn receiver_call_resolves_unique_method() {
    // `let f = Foo::new(); f.unique_method();` — the receiver `f` has no
    // statically-knowable type at the call site, so the parser stamps a
    // Receiver qualifier. There is EXACTLY ONE same-language method named
    // `unique_method` in the project, and the name is not stdlib noise, so the
    // resolver should bind the call to it (instead of dropping the edge and
    // marking `unique_method` as dead). This is the live-method-dead-code bug:
    // `file_exists` / `validate` were dropped this way.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/lib.rs",
        r#"
        pub struct Foo;
        impl Foo {
            pub fn new() -> Self { Foo }
            pub fn unique_method(&self) -> i32 { 7 }
        }
        pub fn caller() -> i32 {
            let f = Foo::new();
            f.unique_method()
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "unique_method");
    assert!(
        callers.iter().any(|c| c == "caller"),
        "caller→f.unique_method() must resolve when `unique_method` is the unique same-language method; got: {:?}",
        callers
    );
}

#[test]
fn receiver_call_resolves_method_not_free_function_same_name() {
    // The `validate` regression shape: a free function `validate(...)` AND a
    // method `Req::validate(&self)` share the name. A receiver call
    // `req.validate()` can only target a METHOD, never the free function, so
    // the resolver must treat the method as the unique candidate (the free
    // function is excluded by qualified_name having no `.`), bind the edge to
    // the method, and NOT to the free function.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/req.rs",
        r#"
        pub struct Req;
        impl Req {
            pub fn validate(&self) -> bool { true }
        }
        pub fn use_it() -> bool {
            let r = make_req();
            r.validate()
        }
        fn make_req() -> Req { Req }
    "#,
    );
    // Free function with the SAME bare name in another module — must NOT
    // receive the receiver-call edge.
    write(
        root,
        "src/install.rs",
        r#"
        pub fn validate(path: &str) -> bool { !path.is_empty() }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let method_callers = callers_of_in_file(&db, "validate", "src/req.rs");
    let free_fn_callers = callers_of_in_file(&db, "validate", "src/install.rs");
    assert!(
        method_callers.iter().any(|c| c.contains("use_it")),
        "Req::validate (method) should have `use_it` as caller via r.validate(); got: {:?}",
        method_callers
    );
    assert!(
        !free_fn_callers.iter().any(|c| c.contains("use_it")),
        "free-fn validate must NOT get the receiver-call edge (receiver targets a method); got: {:?}",
        free_fn_callers
    );
}

#[test]
fn receiver_call_with_ambiguous_method_name_stays_unresolved() {
    // NEGATIVE: two DISTINCT structs each define a method `ambiguous`. A
    // receiver call `x.ambiguous()` where `x`'s type is unknown must NOT
    // fan out to both methods — there is no unique same-language method, so
    // the edge is dropped (no false-positive impact inflation).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/a.rs",
        r#"
        pub struct A;
        impl A {
            pub fn ambiguous(&self) -> i32 { 1 }
        }
    "#,
    );
    write(
        root,
        "src/b.rs",
        r#"
        pub struct B;
        impl B {
            pub fn ambiguous(&self) -> i32 { 2 }
        }
    "#,
    );
    write(
        root,
        "src/caller.rs",
        r#"
        pub fn caller(x: SomeOpaque) -> i32 {
            x.ambiguous()
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "ambiguous");
    assert!(
        !callers.iter().any(|c| c.contains("caller")),
        "ambiguous() defined on two structs must NOT resolve a receiver call to either (no unique target); got: {:?}",
        callers
    );
}

#[test]
fn receiver_call_prefers_same_file_method_over_cross_file_ambiguity() {
    // The `same_file_methods.len() == 1 && methods.len() > 1` branch: the
    // method name `process` is defined on TWO structs in TWO files, so it is
    // NOT globally unique. But the caller lives in the SAME file as struct A's
    // method, so same-file preference resolves the receiver call to A.process
    // and NOT to the cross-file B.process — even though >1 global candidates
    // exist. (Without the same-file branch this would drop as ambiguous; with
    // it, locality breaks the tie toward the in-file method.)
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "src/a.rs",
        r#"
        pub struct A;
        impl A {
            pub fn process(&self) -> i32 { 1 }
        }
        pub fn caller(a: A) -> i32 {
            a.process()
        }
    "#,
    );
    write(
        root,
        "src/b.rs",
        r#"
        pub struct B;
        impl B {
            pub fn process(&self) -> i32 { 2 }
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let a_callers = callers_of_in_file(&db, "process", "src/a.rs");
    let b_callers = callers_of_in_file(&db, "process", "src/b.rs");
    assert!(
        a_callers.iter().any(|c| c.contains("caller")),
        "A::process (same file as caller) should get the receiver-call edge; got: {:?}",
        a_callers
    );
    assert!(
        !b_callers.iter().any(|c| c.contains("caller")),
        "B::process (cross-file) must NOT get the edge — same-file wins the tie; got: {:?}",
        b_callers
    );
}

#[test]
fn js_method_call_resolves_non_ecmascript_builtin_name() {
    // Regression: the cross-file call-noise filter (`CROSS_FILE_CALL_NOISE`) is
    // Rust-stdlib-flavored (`Vec::insert`, `HashMap::remove`, `.contains()`) but
    // was applied to EVERY language. For a JS/TS project, `db.insert(x)` and
    // `db.remove(x)` are ordinary user methods — `insert`/`remove`/`contains`
    // are NOT core ECMAScript builtins (Array uses `splice`, Map uses `has`).
    // Dropping these edges reported live methods as dead code and hid their
    // callers from impact/callers. They must resolve to the unique project method.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "db.ts",
        r#"
        export const db = {
          findOne(id: string) { return { id }; },
          insert(obj: any) { return obj; },
          remove(id: string) { return id; },
          contains(id: string) { return !!id; },
        };
    "#,
    );
    write(
        root,
        "handlers.ts",
        r#"
        import { db } from './db';
        export function createUser(body: any) { return db.insert(body); }
        export function deleteUser(id: string) { return db.remove(id); }
        export function hasUser(id: string) { return db.contains(id); }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    for (method, caller) in [
        ("insert", "createUser"),
        ("remove", "deleteUser"),
        ("contains", "hasUser"),
    ] {
        let callers = callers_of(&db, method);
        assert!(
            callers.iter().any(|c| c.contains(caller)),
            "db.{method}() must resolve to the unique project `{method}` method (not dropped as Rust-stdlib noise); got: {:?}",
            callers
        );
    }
}

#[test]
fn js_method_call_still_drops_real_ecmascript_builtin() {
    // The flip side: `arr.push(x)` / `m.get(k)` target real `Array.prototype`
    // and `Map.prototype` builtins. Even when the project defines a same-named
    // method, the receiver type is unknown and is very likely a real Array/Map,
    // so these stay in the noise set and the edge is dropped — keeping the
    // exemption narrow (only non-builtin names like `insert`/`remove`/`contains`).
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "store.ts",
        r#"
        export const store = {
          push(item: any) { return item; },
          get(key: string) { return key; },
        };
    "#,
    );
    write(
        root,
        "use.ts",
        r#"
        import { store } from './store';
        export function add(x: any) { return store.push(x); }
        export function read(k: string) { return store.get(k); }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    for method in ["push", "get"] {
        let callers = callers_of(&db, method);
        assert!(
            callers.is_empty(),
            "store.{method}() must NOT resolve — {method} is a real ECMAScript builtin and the receiver type is unknown; got: {:?}",
            callers
        );
    }
}

#[test]
fn php_method_call_resolves_collection_verb_names() {
    // PHP `$o->method()` calls have NO stdlib-builtin-method collisions: PHP's
    // array/collection ops are global FUNCTIONS (`array_push`, `count`,
    // `in_array`), never object methods. So `insert`/`remove`/`get`/`push` on a
    // `->` call are always user methods. The Rust-flavored cross-file call-noise
    // list (built for `Vec::insert` / `HashMap::remove`) wrongly dropped these,
    // reporting live PHP methods as dead code. They must resolve.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "repo.php",
        r#"<?php
        class Repo {
            public function insert($x) { return $x; }
            public function remove($x) { return $x; }
            public function get($x) { return $x; }
            public function findOne($x) { return $x; }
        }
    "#,
    );
    write(
        root,
        "service.php",
        r#"<?php
        class Service {
            private $repo;
            public function createThing($d) { return $this->repo->insert($d); }
            public function deleteThing($id) { return $this->repo->remove($id); }
            public function fetchById($id) { return $this->repo->get($id); }
            public function getOne($id) { return $this->repo->findOne($id); }
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    for (method, caller) in [
        ("insert", "createThing"),
        ("remove", "deleteThing"),
        ("get", "fetchById"),
        ("findOne", "getOne"),
    ] {
        let callers = callers_of(&db, method);
        assert!(
            callers.iter().any(|c| c.contains(caller)),
            "PHP $repo->{method}() must resolve to the unique project `{method}` method (no PHP method-builtin collision); got: {:?}",
            callers
        );
    }
}

fn routes_to_handlers(db: &Database) -> Vec<(String, String)> {
    // (handler_node_name, file_path) for every routes_to edge.
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT t.name, f.path FROM edges e
         JOIN nodes t ON t.id = e.target_id
         JOIN files f ON f.id = t.file_id
         WHERE e.relation = 'routes_to'",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    rows.filter_map(|r| r.ok()).collect()
}

#[test]
fn express_route_with_imported_handler_produces_routes_to_edge() {
    // The canonical Express layout: route registration in one file, handler
    // implementations imported from a controller file —
    //   `import { getUser } from './handlers'; app.get('/users/:id', getUser)`.
    // The routes_to relation names the handler as both source and target, but
    // the handler node lives in handlers.ts. The source-id match scanned ONLY
    // the current (route) file's nodes, found nothing, and dropped the edge —
    // so trace/impact/find_http_route saw no route at all for the most common
    // real-world Express structure. The handler must carry a routes_to edge.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "handlers.ts",
        r#"
        export function getUser(req: any, res: any) { res.json({}); }
        export function createUser(req: any, res: any) { res.json({}); }
    "#,
    );
    write(
        root,
        "server.ts",
        r#"
        import express from 'express';
        import { getUser, createUser } from './handlers';
        const app = express();
        app.get('/users/:id', getUser);
        app.post('/users', createUser);
        app.listen(3000);
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let routes = routes_to_handlers(&db);
    for handler in ["getUser", "createUser"] {
        assert!(
            routes.iter().any(|(name, path)| name == handler && path == "handlers.ts"),
            "imported Express handler `{handler}` must carry a routes_to edge in handlers.ts; got: {:?}",
            routes
        );
    }
}

/// Incoming `calls` edge count to the node whose `qualified_name` == `qn`.
fn incoming_calls_by_qualified_name(db: &Database, qn: &str) -> i64 {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM edges e JOIN nodes t ON t.id = e.target_id
         WHERE e.relation = 'calls' AND t.qualified_name = ?",
            [qn],
            |r| r.get(0),
        )
        .unwrap()
}

/// Issue #32 cause 2: a Python receiver whose type is fixed by a single local
/// constructor assignment (`writer = DataWriter()`) resolves `writer.write()` to
/// THAT class's method — even though `write` is defined on three sibling classes.
/// Before the fix the ambiguous by-name fan-out was dropped, orphaning all three.
#[test]
fn python_receiver_type_resolves_to_constructor_type() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "writers.py",
        r#"
class DataWriter:
    def write(self, id, items):
        return len(items)

class ProfileWriter:
    def write(self, id, items):
        return None

class ScenarioWriter:
    def write(self, id, items):
        return None

def save(id, conflicts):
    writer = DataWriter()
    writer.write(id, conflicts)
"#,
    );
    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    assert_eq!(
        incoming_calls_by_qualified_name(&db, "DataWriter.write"),
        1,
        "save() must resolve `writer.write()` to DataWriter.write (writer = DataWriter())"
    );
    assert_eq!(
        incoming_calls_by_qualified_name(&db, "ProfileWriter.write"),
        0,
        "ProfileWriter.write must NOT receive a false cross-type edge"
    );
    assert_eq!(
        incoming_calls_by_qualified_name(&db, "ScenarioWriter.write"),
        0,
        "ScenarioWriter.write must NOT receive a false cross-type edge"
    );
    // The precise resolution also unblocks callgraph/impact: the caller is visible.
    let callers = callers_of(&db, "write");
    assert!(
        callers.iter().any(|c| c.contains("save")),
        "save must be a caller of the resolved write method; got: {:?}",
        callers
    );
}

/// Regression guard for the cause-2 fix: when the receiver's inferred type does
/// NOT declare the method because it's INHERITED from a base class, rtype
/// filtering finds no same-type candidate and must FALL THROUGH to the default
/// bare resolution so the inherited call still resolves — the fix must add
/// precision, never drop an edge the bare path would have made.
#[test]
fn python_receiver_type_inherited_method_still_resolves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "models.py",
        r#"
class Base:
    def process(self, x):
        return x + 1

class Derived(Base):
    pass

def run(x):
    d = Derived()
    d.process(x)
"#,
    );
    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    // Derived has no own `process`; it's inherited from Base. rtype=Derived
    // filters to empty and falls through to the unique bare match Base.process.
    assert_eq!(
        incoming_calls_by_qualified_name(&db, "Base.process"),
        1,
        "inherited d.process() must fall through to Base.process, not drop"
    );
    let callers = callers_of(&db, "process");
    assert!(
        callers.iter().any(|c| c.contains("run")),
        "run must be a caller of the inherited process method; got: {:?}",
        callers
    );
}

/// Issue #32 cause 2 (parameter-annotation extension): a receiver that is a
/// function parameter with an explicit class annotation (`def save(writer:
/// DataWriter)`) resolves `writer.write()` to that class's method among sibling
/// classes sharing the method name — no local constructor assignment needed.
#[test]
fn python_receiver_type_from_parameter_annotation_resolves() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write(
        root,
        "writers.py",
        r#"
class DataWriter:
    def write(self, id, items):
        return len(items)

class ProfileWriter:
    def write(self, id, items):
        return None

class ScenarioWriter:
    def write(self, id, items):
        return None

def save(writer: DataWriter, id, conflicts):
    writer.write(id, conflicts)
"#,
    );
    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    assert_eq!(
        incoming_calls_by_qualified_name(&db, "DataWriter.write"),
        1,
        "param-annotated `writer: DataWriter` must resolve writer.write() to DataWriter.write"
    );
    assert_eq!(
        incoming_calls_by_qualified_name(&db, "ProfileWriter.write"),
        0,
        "ProfileWriter.write must NOT receive a false cross-type edge"
    );
    assert_eq!(
        incoming_calls_by_qualified_name(&db, "ScenarioWriter.write"),
        0,
        "ScenarioWriter.write must NOT receive a false cross-type edge"
    );
}

/// All resolved `calls` edges targeting a node named `persist_item`:
/// (source_name, target_qualified_name, target_file, confidence), sorted so
/// batch/iteration order never affects comparison.
fn persist_item_call_edges(db: &Database) -> Vec<(String, String, String, String)> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT src.name, COALESCE(tgt.qualified_name, tgt.name), f.path, edges.confidence
         FROM edges
         JOIN nodes src ON src.id = edges.source_id
         JOIN nodes tgt ON tgt.id = edges.target_id
         JOIN files f ON f.id = tgt.file_id
         WHERE edges.relation = 'calls' AND tgt.name = 'persist_item'",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap();
    let mut edges: Vec<(String, String, String, String)> = rows.filter_map(Result::ok).collect();
    edges.sort();
    edges
}

/// META①: the callee-qualifier filtering in Phase 2 (`index_files.rs`'s match
/// on `parse_callee_metadata`, e.g. the `CalleeMeta::RecvType` arm) and the
/// incremental pending-sweep (`resolve.rs::resolve_pending_calls`) are
/// PARALLEL, hand-maintained implementations of the same filtering rule
/// (H1/M1, v0.94.0). This locks them: a qualified call must resolve to the
/// same target, with the same confidence, whether the project was
/// full-indexed in one shot, or built incrementally where the caller was
/// indexed BEFORE its callee existed on disk at all.
///
/// The "before its callee existed" staging matters: when both files are
/// visible in the SAME low-level indexing batch (true for both full index
/// and a same-shot incremental add), Phase 2's own inline qualifier match
/// resolves the call directly and `resolve_pending_calls` never runs on it —
/// so that shape can't distinguish the two implementations. To actually
/// exercise the pending-sweep, the caller must be indexed while NO
/// same-language `persist_item` candidate exists yet (buffering the call into
/// `pending_unresolved_calls` together with its `{"q":"rtype",...}`
/// qualifier metadata); the callee is then added in a later incremental
/// pass, which drains the buffer through `resolve_pending_calls`. (The
/// target method is deliberately NOT named `write`/`get`/`insert`/etc. —
/// those are in `CROSS_FILE_CALL_NOISE`, so with zero candidates in scope the
/// default fallback in Phase 2 would drop the call as stdlib noise instead of
/// buffering it, before ever reaching the pending-sweep this test targets.)
///
/// Fixture: two same-named methods (`persist_item`) on sibling classes
/// (DataWriter, ProfileWriter) defined in ONE file; a caller whose receiver
/// type is pinned to DataWriter by a local constructor assignment (Python
/// `rtype` qualifier — same shape as
/// `python_receiver_type_resolves_to_constructor_type` above). Correct
/// resolution binds exactly one edge, to DataWriter.persist_item — not
/// ProfileWriter.persist_item, and not both.
///
/// Negative control (see task-3 report): commenting out the qualifier-filter
/// match arm (`Some(CalleeMeta::RecvType(t)) | Some(CalleeMeta::SelfType(t)) |
/// Some(CalleeMeta::SelfRecv(t))`) in `resolve_pending_calls` makes the
/// incremental side fall back to the bare same-language candidate set — both
/// DataWriter.persist_item and ProfileWriter.persist_item live in the SAME
/// file, so `refine_ambiguous_targets`'s path-prefix tiebreak can't separate
/// them and BOTH edges get bound, diverging from the full-index result
/// asserted here.
#[test]
fn qualifier_resolution_parity_full_vs_incremental() {
    const CALLER_SRC: &str = r#"
def save(id, conflicts):
    writer = DataWriter()
    writer.persist_item(id, conflicts)
"#;
    const WRITER_SRC: &str = r#"
class DataWriter:
    def persist_item(self, id, items):
        return len(items)

class ProfileWriter:
    def persist_item(self, id, items):
        return None
"#;

    // --- Full index: both files present from the start, indexed in one shot.
    let full_tmp = TempDir::new().unwrap();
    let full_root = full_tmp.path();
    write(full_root, "caller.py", CALLER_SRC);
    write(full_root, "writer.py", WRITER_SRC);
    let full_db_path = full_root.join(".code-graph/graph.db");
    fs::create_dir_all(full_db_path.parent().unwrap()).unwrap();
    let full_db = Database::open(&full_db_path).unwrap();
    run_full_index(&full_db, full_root, None, None).unwrap();
    let full_edges = persist_item_call_edges(&full_db);

    // --- Incremental: caller indexed FIRST, writer.py absent from disk — the
    // rtype-qualified call has no same-language `persist_item` candidate at
    // all yet, so Phase 2 buffers it in `pending_unresolved_calls`.
    // writer.py is added in a SECOND incremental pass, which drains the
    // buffer via `resolve_pending_calls`.
    let incr_tmp = TempDir::new().unwrap();
    let incr_root = incr_tmp.path();
    write(incr_root, "caller.py", CALLER_SRC);
    let incr_db_path = incr_root.join(".code-graph/graph.db");
    fs::create_dir_all(incr_db_path.parent().unwrap()).unwrap();
    let incr_db = Database::open(&incr_db_path).unwrap();
    run_incremental_index(&incr_db, incr_root, None, None).unwrap();

    write(incr_root, "writer.py", WRITER_SRC);
    run_incremental_index(&incr_db, incr_root, None, None).unwrap();
    let incr_edges = persist_item_call_edges(&incr_db);

    assert_eq!(
        full_edges, incr_edges,
        "qualifier resolution diverged between full index and incremental pending-sweep: full={:?} incr={:?}",
        full_edges, incr_edges
    );
    // Sanity: both sides actually resolved correctly (not vacuously equal
    // because both dropped to zero edges, or both over-resolved to two).
    assert_eq!(
        full_edges,
        vec![(
            "save".to_string(),
            "DataWriter.persist_item".to_string(),
            "writer.py".to_string(),
            "inferred".to_string(),
        )],
        "expected exactly one edge, save -> DataWriter.persist_item, confidence inferred; got {:?}",
        full_edges
    );
}

/// Regression (audit 2026-08-22 P1-1): a call qualified with the crate's OWN
/// package name — `my_crate::cli::cmd_grep()`, the standard `bin` → `lib`
/// shape — kept `my_crate` as the first Path segment. That segment names the
/// crate root, not a directory, so the chain matched no candidate path and the
/// edge dropped. In this repo that made `dead-code src/` report 23/23 false
/// positives and `impact cmd_grep` answer "0 callers" for a function main.rs
/// really calls. The crate root is now stripped like `crate::`/`super::`.
#[test]
fn path_qualifier_strips_own_crate_name() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "src/cli/commands/grep.rs",
        "pub fn cmd_grep() -> i32 { 1 }\n",
    );
    write(
        root,
        "src/cli/commands/other.rs",
        "pub fn cmd_other() -> i32 { 2 }\n",
    );
    write(
        root,
        "src/main.rs",
        r#"
        fn main() {
            // Own crate name, hyphen → underscore: must resolve.
            my_crate::cli::cmd_grep();
            // A DIFFERENT crate's path must still drop (negative control):
            // stripping must be keyed on this project's package names, not on
            // "the first segment matches nothing".
            unknown_crate::cli::cmd_other();
        }
    "#,
    );

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let grep_callers = callers_of(&db, "cmd_grep");
    assert!(
        grep_callers.iter().any(|c| c == "main"),
        "my_crate::cli::cmd_grep() must resolve after the own-crate-root strip; got: {:?}",
        grep_callers
    );
    let other_callers = callers_of(&db, "cmd_other");
    assert!(
        other_callers.is_empty(),
        "unknown_crate::cli::cmd_other() is not this crate's path and must stay dropped; got: {:?}",
        other_callers
    );
}

/// Regression (pre-tag review of the strip above): stripping the crate root can
/// leave NOTHING behind. `my_crate::run()` is the whole qualifier, so the strip
/// produced an empty chain, fell through the `segments.is_empty()` guard and
/// returned the FULL same-name candidate set unfiltered — while `q="path"`
/// exempts the edge from the ambiguous downgrade (`classify_edge_confidence`)
/// on the premise that a path qualifier binds structurally. For this branch
/// that premise is false: nothing is left to bind with. A two-way ambiguity
/// shipped as two `inferred` edges, one of them a phantom bound to a real node,
/// sitting ABOVE the default confidence floor where dead-code / impact /
/// callgraph consume it as fact.
///
/// Measured on this fixture before the fix: `demo::run()` produced edges to
/// BOTH `src/lib.rs:run` and `src/server.rs:run`, both `inferred`.
#[test]
fn bare_crate_root_qualifier_does_not_fan_out_to_same_name_siblings() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "src/lib.rs", "pub fn run() { }\npub mod server;\n");
    write(root, "src/server.rs", "pub fn run() { }\n");
    write(root, "src/main.rs", "fn main() { demo::run(); }\n");

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    // The qualifier `demo` carries no module path, so it cannot choose between
    // two same-named `run`s. No answer is the honest outcome; the one thing it
    // must not do is publish both as `inferred`.
    assert!(
        callers_of(&db, "run").is_empty(),
        "`demo::run()` cannot choose between two same-named targets — it must \
         drop rather than emit an edge to each at `inferred`; got: {:?}",
        callers_of(&db, "run")
    );
}

/// Negative control for the test above: the drop must be driven by the
/// AMBIGUITY, not by the bare crate root itself. With exactly one `run` in the
/// project there is nothing to choose between, and `demo::run()` must still
/// resolve — otherwise the fix above has over-corrected into the missing-edge
/// bug that `path_qualifier_strips_own_crate_name` exists to prevent.
#[test]
fn bare_crate_root_qualifier_still_resolves_a_unique_target() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "src/lib.rs", "pub fn run() { }\n");
    write(root, "src/main.rs", "fn main() { demo::run(); }\n");

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    let callers = callers_of(&db, "run");
    assert!(
        callers.iter().any(|c| c == "main"),
        "`demo::run()` with a single `run` in the project must still resolve; got: {:?}",
        callers
    );
}

/// Regression (pre-tag review): the strip was UNCONDITIONAL, so a package whose
/// name is also an ordinary directory name lost the qualifier's only
/// discriminating segment. `utils::helper::go()` in a package named `utils`
/// degraded from the chain `utils/helper` — which matches exactly one
/// directory — to `helper`, which matches every `helper/` in the tree.
///
/// This one is a strict REGRESSION, not merely a missed improvement: measured
/// on this fixture, the pre-strip code produced ONE correct edge, and the strip
/// turned it into two `inferred` edges whose metadata still reads
/// `v:"utils::helper"` — a path only one of the two targets has. Cargo
/// workspaces make this ordinary: `core`, `utils`, `parser` and `config` are
/// both package names and module-directory names.
#[test]
fn crate_root_strip_does_not_fire_when_the_chain_as_written_matches() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"utils\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "src/utils/helper/mod.rs", "pub fn go() { }\n");
    write(root, "src/thirdparty/helper/mod.rs", "pub fn go() { }\n");
    write(root, "src/main.rs", "fn main() { utils::helper::go(); }\n");

    let db_path = root.join(".code-graph/graph.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let db = Database::open(&db_path).unwrap();
    run_full_index(&db, root, None, None).unwrap();

    assert!(
        callers_of_in_file(&db, "go", "src/utils/helper/mod.rs")
            .iter()
            .any(|c| c == "main"),
        "the chain as written (`utils/helper`) matches this file and must win"
    );
    assert!(
        callers_of_in_file(&db, "go", "src/thirdparty/helper/mod.rs").is_empty(),
        "stripping `utils` degrades the chain to `helper`, which matches every \
         `helper/` in the tree — the strip must not fire when the qualifier as \
         written already resolves; got: {:?}",
        callers_of_in_file(&db, "go", "src/thirdparty/helper/mod.rs")
    );
}
