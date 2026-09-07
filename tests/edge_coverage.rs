//! Edge-resolution coverage baseline. A per-language edge-count drop here flags a
//! silent edge-resolution regression (the class of bug that has historically shipped
//! undetected: method→sibling-method drops, value-reference floods, qualifier loss).
//! Update the baselines deliberately when a change is a real improvement.
use std::collections::BTreeMap;
use tempfile::TempDir;

use code_graph_mcp::graph::routes::get_callers_with_route_info;
use code_graph_mcp::storage::db::Database;
use code_graph_mcp::storage::queries;

/// Index a fixed multi-language fixture, returning the project dir and an open DB.
/// Keep both bound in tests: dropping the TempDir wipes the index.
fn index_fixture() -> (TempDir, Database) {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // TypeScript: class with two methods, one calling the sibling (intra-class call).
    std::fs::write(
        src.join("svc.ts"),
        r#"
export class Svc {
    handle(x: number): number { return this.helper(x); }
    helper(x: number): number { return x + 1; }
}
"#,
    )
    .unwrap();

    // Python: same intra-class sibling call.
    std::fs::write(
        src.join("svc.py"),
        r#"
class Svc:
    def handle(self, x):
        return self.helper(x)
    def helper(self, x):
        return x + 1
"#,
    )
    .unwrap();

    // Rust: same-file function call.
    std::fs::write(
        src.join("lib.rs"),
        r#"
pub fn helper(x: i32) -> i32 { x + 1 }
pub fn handle(x: i32) -> i32 { helper(x) }
"#,
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    (project, db)
}

fn edge_counts(db: &Database) -> BTreeMap<String, BTreeMap<String, i64>> {
    queries::resolution_stats(db.conn())
        .unwrap()
        .edges_by_language
}

#[test]
fn edge_coverage_per_language_baseline() {
    let (_p, db) = index_fixture();
    let by_lang = edge_counts(&db);
    // Lower-bound baselines: each language must produce at least these call edges.
    // Raise deliberately when extraction genuinely improves.
    let calls = |lang: &str| {
        by_lang
            .get(lang)
            .and_then(|m| m.get("calls"))
            .copied()
            .unwrap_or(0)
    };
    assert!(calls("typescript") >= 1, "TS calls regressed: {by_lang:?}");
    assert!(calls("python") >= 1, "Python calls regressed: {by_lang:?}");
    assert!(calls("rust") >= 1, "Rust calls regressed: {by_lang:?}");
}

/// Every language whose extractor claims `calls` must produce one, in one table.
///
/// The baseline above covers three languages, and the import parity test covers
/// six — so of the twenty languages the README lists, fourteen had NO numeric
/// guard on the call axis at all (audit P2-27; the "~11/19" in the 07-24 report
/// counted the two tests' union under a loose reading). A silently dropped arm
/// in `walk_for_relations` is exactly the failure this repo keeps rediscovering,
/// and it has been invisible for every language outside that overlap.
///
/// Excluded on purpose, with the reason stated so the gap stays legible:
///   * `markdown` extracts headings, `html`/`css`/`json` are FTS-only — none has
///     a call axis to regress.
///   * `tsx` shares the TypeScript extractor path (same arm, different grammar).
///
/// Each fixture is the smallest two-function file where one calls the other, so
/// a failure points at the language's arm rather than at resolution subtleties.
#[test]
fn call_extraction_parity_across_every_call_capable_language() {
    let cases: &[(&str, &str, &str)] = &[
        ("javascript", "a.js", "function helper(x){return x+1;}\nfunction handle(x){return helper(x);}\n"),
        ("go", "a.go", "package main\nfunc helper(x int) int { return x + 1 }\nfunc handle(x int) int { return helper(x) }\n"),
        ("java", "A.java", "class A {\n  int helper(int x){ return x+1; }\n  int handle(int x){ return helper(x); }\n}\n"),
        ("csharp", "A.cs", "class A {\n  int Helper(int x){ return x+1; }\n  int Handle(int x){ return Helper(x); }\n}\n"),
        ("kotlin", "a.kt", "fun helper(x: Int): Int = x + 1\nfun handle(x: Int): Int = helper(x)\n"),
        ("ruby", "a.rb", "def helper(x)\n  x + 1\nend\ndef handle(x)\n  helper(x)\nend\n"),
        ("php", "a.php", "<?php\nfunction helper($x){ return $x+1; }\nfunction handle($x){ return helper($x); }\n"),
        ("swift", "a.swift", "func helper(_ x: Int) -> Int { return x + 1 }\nfunc handle(_ x: Int) -> Int { return helper(x) }\n"),
        ("dart", "a.dart", "int helper(int x) => x + 1;\nint handle(int x) => helper(x);\n"),
        ("c", "a.c", "int helper(int x){ return x+1; }\nint handle(int x){ return helper(x); }\n"),
        ("cpp", "a.cpp", "int helper(int x){ return x+1; }\nint handle(int x){ return helper(x); }\n"),
        ("bash", "a.sh", "helper() { echo 1; }\nhandle() { helper; }\n"),
    ];

    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (_, file, source) in cases {
        std::fs::write(src.join(file), source).unwrap();
    }

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let by_lang = edge_counts(&db);
    let missing: Vec<&str> = cases
        .iter()
        .filter(|(lang, _, _)| {
            by_lang
                .get(*lang)
                .and_then(|m| m.get("calls"))
                .copied()
                .unwrap_or(0)
                < 1
        })
        .map(|(lang, _, _)| *lang)
        .collect();
    assert!(
        missing.is_empty(),
        "these languages produced no `calls` edge for a plain handle→helper call \
         — their extractor arm is missing or regressed: {missing:?}; \
         edges_by_language={by_lang:?}"
    );
}

#[test]
fn c_include_resolves_to_indexed_header_module() {
    // A C/C++ `#include "widget.h"` must resolve to the indexed header's <module>
    // node (an IMPORTS edge), mirroring PHP require / JS require. Before the fix
    // the include emitted only a bare stem with NO path metadata, so it fell to
    // `<external>/widget` and deps/cycles/affected/project_map under-reported the
    // local header dependency (M6). INDEX_VERSION 45→46.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("widget.h"), "int widget_add(int a, int b);\n").unwrap();
    std::fs::write(
        src.join("widget.cpp"),
        "#include \"widget.h\"\nint widget_add(int a, int b) { return a + b; }\n",
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let conn = db.conn();
    let module_id = |path: &str| -> i64 {
        conn.query_row(
            "SELECT n.id FROM nodes n JOIN files f ON f.id = n.file_id
             WHERE n.name = '<module>' AND f.path = ?1",
            [path],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(-1)
    };
    let cpp_mod = module_id("src/widget.cpp");
    let h_mod = module_id("src/widget.h");
    assert!(
        cpp_mod > 0 && h_mod > 0,
        "both <module> nodes must exist (cpp={cpp_mod}, h={h_mod})"
    );

    let has_edge: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM edges WHERE source_id=?1 AND target_id=?2 AND relation='imports')",
        [cpp_mod, h_mod], |r| r.get(0),
    ).unwrap();
    assert!(
        has_edge,
        "widget.cpp #include \"widget.h\" must produce an IMPORTS edge to widget.h's <module>",
    );
}

#[test]
fn import_extraction_parity_across_full_languages() {
    // META②: every full-extraction language must emit a REL_IMPORTS edge for its
    // canonical import form. walk_for_relations (src/parser/relations/mod.rs) has
    // one arm per (language, relation); a silently missing arm is the H2/M5/M6
    // sibling-hole class this test locks against. All six full-extraction
    // languages are checked in one table so a future per-language addition (or
    // regression) can't slip through unnoticed.
    //
    // None of the import targets below need a real importee file indexed: an
    // import that doesn't resolve to a local file/symbol still produces a
    // REL_IMPORTS edge to an `<external>/<name>` sentinel node (Phase 2b/2b-ext
    // in src/indexer/pipeline/index_files.rs) — the same fallback the
    // `c_include_resolves_to_indexed_header_module` test's C++ case would hit for
    // a system header. So the plain canonical form is enough to prove the arm
    // fires; no per-language importee scaffolding is required.
    //
    // Negative control: deleting a language's import arm in
    // src/parser/relations/mod.rs (e.g. the Java `import_declaration` arm added
    // by H2) makes exactly that row's assertion fail while the others stay green.
    let cases: &[(&str, &str, &str)] = &[
        ("typescript", "a.ts", "import { x } from './b';\n"),
        ("javascript", "a.js", "const x = require('./b');\n"),
        ("go", "a.go", "package main\nimport \"fmt\"\n"),
        ("python", "a.py", "import os\n"),
        // Non-std crate root: `use std::…` is skipped whole as of IDX v52
        // (statically-external root; its bare tail used to bind same-named
        // project symbols). An unknown crate root still exercises the Rust
        // use-arm and lands on the `<external>` sentinel like the others.
        ("rust", "a.rs", "use anyhow::fmt;\n"),
        ("java", "A.java", "import java.util.List;\n"),
    ];

    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (_, file, source) in cases {
        std::fs::write(src.join(file), source).unwrap();
    }

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let by_lang = edge_counts(&db);
    for (lang, file, source) in cases {
        let imports = by_lang
            .get(*lang)
            .and_then(|m| m.get("imports"))
            .copied()
            .unwrap_or(0);
        assert!(
            imports >= 1,
            "{lang}: expected a REL_IMPORTS edge from {file} (`{source:?}`) but found none; edges_by_language={by_lang:?}"
        );
    }
}

#[test]
fn edge_coverage_intra_class_method_call_resolves() {
    // Guards the method→sibling-method drop class (method_call_edge_drops, fixed v16).
    // Scope per-file so a Rust same-file call cannot mask a TS/Python OO regression.
    let (_p, db) = index_fixture();
    for file in ["src/svc.ts", "src/svc.py"] {
        let callers = get_callers_with_route_info(db.conn(), "helper", Some(file), 3, 0).unwrap();
        assert!(
            callers.callers.iter().any(|c| c.name == "handle"),
            "intra-class call handle→helper must resolve in {file}; got {:?}",
            callers
                .callers
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
        );
    }
}

/// Index one file per fixture and return the `imports` edge count per source file.
///
/// Asserts the fixtures PARSED. tree-sitter recovers from a syntax error by
/// returning a damaged tree, so extraction still yields *some* symbols and a
/// missing edge would be ambiguous between "the extractor arm is gone" and
/// "this fixture never parsed". That is not hypothetical: a single-line Kotlin
/// class body (`class C { fun f(): Int = 1 }`) errors under the pinned grammar
/// while the identical code across three lines does not, so a parity fixture
/// written the first way reports Kotlin as having lost its arm.
fn index_parity_fixture(files: &[(&str, &str)]) -> (TempDir, Database) {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (name, body) in files {
        std::fs::write(src.join(name), body).unwrap();
    }
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    let result =
        code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    assert_eq!(
        result.stats.files_with_parse_errors, 0,
        "a parity fixture failed to parse — fix the fixture before reading anything into a \
         missing edge, because error recovery makes 'arm removed' and 'fixture invalid' \
         look identical"
    );
    (project, db)
}

/// `imports` edge count per source file, for the spelling table below.
fn imports_per_file(files: &[(&str, &str)]) -> (TempDir, BTreeMap<String, i64>) {
    let (project, db) = index_parity_fixture(files);
    let mut counts = BTreeMap::new();
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT f.path, count(*) FROM edges e \
             JOIN nodes s ON e.source_id = s.id JOIN files f ON s.file_id = f.id \
             WHERE e.relation = 'imports' GROUP BY 1",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap();
    for row in rows {
        let (path, n) = row.unwrap();
        counts.insert(path, n);
    }
    (project, counts)
}

/// Per-language, per-FORM import parity.
///
/// `import_extraction_parity_across_full_languages` above covers six languages
/// with one canonical form each. That left two gaps this table closes, both of
/// which were live defects when it was written:
///
///   * nine import-capable languages had no import guard at all (C#, Kotlin,
///     Ruby, PHP, Swift, Dart, C, C++, Bash);
///   * within a covered language, only ONE spelling was ever exercised — and
///     the spelling decides. PHP `require_once "b.php"` emitted nothing while
///     `require_once 'b.php'` worked, because tree-sitter-php gives a
///     double-quoted string the kind `encapsed_string` (it can interpolate) and
///     the extractor only knew `string`. All four PHP include keywords were
///     affected. Likewise `import f from './b'` — the single most common ESM
///     form — emitted nothing, because the default binding is a bare
///     `identifier` under `import_clause`, which is neither an
///     `import_specifier` nor a direct child of the statement.
///
/// So each row is (label, file, source, min_edges): a language may legitimately
/// need more than one row.
#[test]
fn import_parity_across_languages_and_spellings() {
    let cases: &[(&str, &str, &str, i64)] = &[
        // --- the spelling axis, where the two defects lived ---
        (
            "php require_once double-quoted",
            "a.php",
            "<?php\nrequire_once \"b.php\";\n",
            1,
        ),
        (
            "php require_once single-quoted",
            "c.php",
            "<?php\nrequire_once 'b.php';\n",
            1,
        ),
        (
            "php include double-quoted",
            "d.php",
            "<?php\ninclude \"b.php\";\n",
            1,
        ),
        (
            "php include_once double-quoted",
            "e.php",
            "<?php\ninclude_once \"b.php\";\n",
            1,
        ),
        (
            "php require double-quoted",
            "f.php",
            "<?php\nrequire \"b.php\";\n",
            1,
        ),
        (
            "esm default import",
            "def.ts",
            "import mod from './tgt';\n",
            1,
        ),
        (
            "esm default + named",
            "both.ts",
            "import mod, { y } from './tgt';\n",
            2,
        ),
        (
            "esm named import",
            "named.ts",
            "import { y } from './tgt';\n",
            1,
        ),
        (
            "esm namespace import",
            "ns.ts",
            "import * as everything from './tgt';\n",
            1,
        ),
        (
            "cjs require double-quoted",
            "r1.js",
            "const x = require(\"./tgtjs\");\n",
            1,
        ),
        (
            "cjs require single-quoted",
            "r2.js",
            "const x = require('./tgtjs');\n",
            1,
        ),
        // --- the nine languages that had no import guard at all ---
        ("csharp using", "A.cs", "using System;\n", 1),
        ("kotlin import", "a.kt", "import kotlin.math.abs\n", 1),
        ("ruby require", "a.rb", "require 'json'\n", 1),
        ("swift import", "a.swift", "import Foundation\n", 1),
        ("dart import", "a.dart", "import 'dart:math';\n", 1),
        (
            "c include angle-bracketed",
            "a.c",
            "#include <stdio.h>\n",
            1,
        ),
        ("cpp include quoted", "a.cpp", "#include \"hdr.h\"\n", 1),
        ("bash source", "a.sh", "source ./b.sh\n", 1),
    ];

    let mut files: Vec<(&str, &str)> = cases.iter().map(|(_, f, s, _)| (*f, *s)).collect();
    // Import TARGETS. Not required for an edge to exist — an unresolved
    // specifier still reaches an `<external>` sentinel — but a resolvable one
    // proves the edge points somewhere real for the forms that bind file-level.
    files.push(("b.php", "<?php\nfunction phpHelper() { return 1; }\n"));
    files.push((
        "tgt.ts",
        "export const y = 1;\nexport default function f() {}\n",
    ));
    files.push(("tgtjs.js", "module.exports = { y: 1 };\n"));
    files.push(("hdr.h", "int hdr_fn(int a);\n"));
    files.push(("b.sh", "other() { echo 2; }\n"));

    let (_p, counts) = imports_per_file(&files);

    let shortfalls: Vec<String> = cases
        .iter()
        .filter_map(|(label, file, source, min)| {
            let got = counts.get(&format!("src/{file}")).copied().unwrap_or(0);
            (got < *min).then(|| format!("{label} ({file}, {source:?}): {got} < {min}"))
        })
        .collect();

    assert!(
        shortfalls.is_empty(),
        "these import spellings produced too few `imports` edges — the extractor arm or \
         branch for each is missing or regressed:\n  {}\nall counts: {counts:?}",
        shortfalls.join("\n  ")
    );
}

/// The EXACT-count half of the import axis: one module, one edge.
///
/// The spelling table above asserts "at least N", which is the right shape for
/// "did the arm survive" and cannot see the opposite failure.
/// `import mod, * as ns from './m'` emitted two identical
/// `<module> -> <module>@./m` rows, one per binding, because each marker claimed
/// the module-level dependency separately and `idx_edges_unique` includes
/// `metadata` (so the differing `q` kept both). A floor of 1 passes on 2. `deps`
/// happens to survive it — its symbol counts are `COUNT(DISTINCT nb.id)` — but
/// the edge totals and the per-language relation stats do not.
///
/// A note on what is NOT here. This table briefly also asserted that dynamically
/// built include paths produce no edge (`require_once "config" . $env . ".php"`
/// must not bind `config.php`). That rule was reverted before release: two
/// independent reviews each measured it as a NET LOSS of true edges on ordinary
/// idioms, and both times the fixture used to validate it happened not to
/// contain the shapes it broke. Any future attempt needs a fixture with a FLAT
/// layout — targets that can actually bind — and rows for the parenthesized
/// operand, the interpolated literal left of the last separator, and the route
/// whose path ends in a variable. See CHANGELOG "Known gap".
#[test]
fn import_forms_resolve_to_exactly_one_module_or_none() {
    // (label, file, source, exact expected `imports` edge count)
    let cases: &[(&str, &str, &str, i64)] = &[
        (
            "esm default + namespace (one module, one edge)",
            "dup.ts",
            "import mod, * as ns from './tgt';\n",
            1,
        ),
        (
            "esm default alone",
            "d1.ts",
            "import mod from './tgt';\n",
            1,
        ),
        (
            "esm namespace alone",
            "d2.ts",
            "import * as ns from './tgt';\n",
            1,
        ),
        (
            // The guard must not over-fire: a clause with no namespace binding
            // keeps its default marker.
            "esm default alone under a second module",
            "d3.ts",
            "import only from './tgt2';\n",
            1,
        ),
    ];

    let mut files: Vec<(&str, &str)> = cases.iter().map(|(_, f, s, _)| (*f, *s)).collect();
    files.push((
        "tgt.ts",
        "export const y = 1;\nexport default function f() {}\n",
    ));
    files.push((
        "tgt2.ts",
        "export const z = 1;\nexport default function g() {}\n",
    ));

    let (_p, counts) = imports_per_file(&files);

    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|(label, file, source, want)| {
            let got = counts.get(&format!("src/{file}")).copied().unwrap_or(0);
            (got != *want).then(|| format!("{label} ({file}, {source:?}): {got} != {want}"))
        })
        .collect();

    assert!(
        wrong.is_empty(),
        "these import forms produced the wrong NUMBER of `imports` edges — more than \
         one means a single module was counted twice, once per binding:\n  {}\n\
         all counts: {counts:?}",
        wrong.join("\n  ")
    );
}

/// Per-language inheritance-axis parity.
///
/// The `calls` axis has `call_extraction_parity_across_every_call_capable_language`
/// and imports has the table above; the inheritance axis had NO parity table at
/// all — only scattered single-language tests for Java, Ruby, PHP, C++ and Dart.
/// Six languages that emit these edges today (C#, Kotlin, Swift, Python,
/// TypeScript, JavaScript) could lose their arm without a single test going red.
///
/// The expectations are per (language, relation) because the MODELING is not
/// uniform, and that non-uniformity is the point of writing it down:
///
///   * separate `inherits` + `implements`: TS, Java, C#, PHP, Dart
///   * `inherits` only, no interface syntax in the language: JS, Python, Ruby, C++
///   * conformance folded INTO `inherits`: Kotlin (`: Base(), Iface`) and Swift
///     (`: Base, Protocol`) — both emit TWO `inherits` edges and no `implements`
///   * `implements` only: Rust `impl Trait for Type` (no inheritance to model).
///     Rust also emits one `implements` per method in the impl block, on purpose,
///     so dead-code sees incoming edges on trait methods.
///   * Go: `inherits` for struct EMBEDDING only. Interface satisfaction is
///     structural — there is no declaration to extract, so no edge, ever.
///   * C: no inheritance at all.
///
/// Type names are language-distinct so a cross-language name collision cannot
/// manufacture an edge and mask a missing arm.
#[test]
fn inheritance_parity_across_every_inheritance_capable_language() {
    let files: &[(&str, &str)] = &[
        (
            "svc.ts",
            "export interface TsGreeter {\n    greet(): string;\n}\nexport class TsBase {\n    helper(): number {\n        return 1;\n    }\n}\nexport class TsSvc extends TsBase implements TsGreeter {\n    greet(): string {\n        return \"hi\";\n    }\n}\n",
        ),
        (
            "svc.js",
            "class JsBase {\n    helper() {\n        return 1;\n    }\n}\nclass JsSvc extends JsBase {\n    handle() {\n        return this.helper();\n    }\n}\nmodule.exports = { JsSvc };\n",
        ),
        (
            "svc.py",
            "class PyBase:\n    def helper(self):\n        return 1\n\nclass PySvc(PyBase):\n    def handle(self):\n        return self.helper()\n",
        ),
        (
            "a.rb",
            "class RbBase\n  def helper\n    1\n  end\nend\n\nclass RbSvc < RbBase\n  def greet\n    helper\n  end\nend\n",
        ),
        (
            "JvSvc.java",
            "interface JvGreeter {\n    int greet();\n}\n\nclass JvBase {\n    int helper() {\n        return 1;\n    }\n}\n\npublic class JvSvc extends JvBase implements JvGreeter {\n    public int greet() {\n        return helper();\n    }\n}\n",
        ),
        (
            "CsSvc.cs",
            "interface ICsGreeter {\n    int Greet();\n}\n\nclass CsBase {\n    public int Helper() {\n        return 1;\n    }\n}\n\nclass CsSvc : CsBase, ICsGreeter {\n    public int Greet() {\n        return Helper();\n    }\n}\n",
        ),
        (
            "a.php",
            "<?php\ninterface PhpGreet {\n    public function greet();\n}\n\nclass PhpBase {\n    public function helper() {\n        return 1;\n    }\n}\n\nclass PhpSvc extends PhpBase implements PhpGreet {\n    public function greet() {\n        return $this->helper();\n    }\n}\n",
        ),
        (
            "a.dart",
            "abstract class DtGreet {\n  int greet();\n}\n\nclass DtBase {\n  int helper() => 1;\n}\n\nclass DtSvc extends DtBase implements DtGreet {\n  int greet() => helper();\n}\n",
        ),
        (
            "a.kt",
            "interface KtGreet {\n    fun greet(): Int\n}\n\nopen class KtBase {\n    open fun helper(): Int {\n        return 1\n    }\n}\n\nclass KtSvc : KtBase(), KtGreet {\n    override fun greet(): Int {\n        return helper()\n    }\n}\n",
        ),
        (
            "a.swift",
            "protocol SwGreet {\n    func greet() -> Int\n}\n\nclass SwBase {\n    func helper() -> Int {\n        return 1\n    }\n}\n\nclass SwSvc: SwBase, SwGreet {\n    func greet() -> Int {\n        return helper()\n    }\n}\n",
        ),
        (
            "cppbase.hpp",
            "class CppBase {\npublic:\n    int helper();\n};\n",
        ),
        (
            "a.cpp",
            "#include \"cppbase.hpp\"\n\nclass CppSvc : public CppBase {\npublic:\n    int greet();\n};\n",
        ),
        (
            "lib.rs",
            "pub trait RsGreet {\n    fn greet(&self) -> i32;\n}\n\npub struct RsSvc;\n\nimpl RsGreet for RsSvc {\n    fn greet(&self) -> i32 {\n        1\n    }\n}\n",
        ),
        (
            "a.go",
            "package main\n\ntype GoBase struct {\n\tId int\n}\n\ntype GoSvc struct {\n\tGoBase\n}\n",
        ),
        (
            "a.c",
            "struct CBase {\n    int id;\n};\n\nstruct CSvc {\n    struct CBase b;\n};\n",
        ),
    ];

    // (language, relation, minimum). A language absent from a relation here is
    // asserted to emit ZERO of it below, so both directions are pinned.
    let expected: &[(&str, &str, i64)] = &[
        ("typescript", "inherits", 1),
        ("typescript", "implements", 1),
        ("javascript", "inherits", 1),
        ("python", "inherits", 1),
        ("ruby", "inherits", 1),
        ("java", "inherits", 1),
        ("java", "implements", 1),
        ("csharp", "inherits", 1),
        ("csharp", "implements", 1),
        ("php", "inherits", 1),
        ("php", "implements", 1),
        ("dart", "inherits", 1),
        ("dart", "implements", 1),
        // Conformance folded into `inherits`: base class AND interface/protocol.
        ("kotlin", "inherits", 2),
        ("swift", "inherits", 2),
        ("cpp", "inherits", 1),
        ("rust", "implements", 1),
        // Struct embedding, the only inheritance-shaped Go syntax.
        ("go", "inherits", 1),
    ];

    let (_p, db) = index_parity_fixture(files);
    let by_lang = edge_counts(&db);
    let count = |lang: &str, rel: &str| -> i64 {
        by_lang
            .get(lang)
            .and_then(|m| m.get(rel))
            .copied()
            .unwrap_or(0)
    };

    let shortfalls: Vec<String> = expected
        .iter()
        .filter_map(|(lang, rel, min)| {
            let got = count(lang, rel);
            (got < *min).then(|| format!("{lang} {rel}: {got} < {min}"))
        })
        .collect();
    assert!(
        shortfalls.is_empty(),
        "these languages stopped emitting an inheritance edge — the arm is missing or \
         regressed:\n  {}\nall counts: {by_lang:?}",
        shortfalls.join("\n  ")
    );

    // Negative half. Without it, a change that starts emitting `implements` for
    // every Kotlin supertype (or an `inherits` edge for Go's structural
    // interfaces) would silently double-count and no test would notice.
    let unexpected: Vec<String> = [
        "typescript",
        "javascript",
        "python",
        "ruby",
        "java",
        "csharp",
        "php",
        "dart",
        "kotlin",
        "swift",
        "cpp",
        "rust",
        "go",
        "c",
    ]
    .iter()
    .flat_map(|lang| {
        ["inherits", "implements"]
            .iter()
            .map(move |rel| (*lang, *rel))
    })
    .filter(|(lang, rel)| {
        !expected.iter().any(|(l, r, _)| l == lang && r == rel) && count(lang, rel) > 0
    })
    .map(|(lang, rel)| format!("{lang} {rel}: {} (expected none)", count(lang, rel)))
    .collect();
    assert!(
        unexpected.is_empty(),
        "these languages emitted an inheritance edge the table does not model — either the \
         extractor widened (update the table) or it is manufacturing edges:\n  {}\nall counts: \
         {by_lang:?}",
        unexpected.join("\n  ")
    );
}

/// The METHOD-call spelling, per OO language.
///
/// `call_extraction_parity_across_every_call_capable_language` exercises exactly
/// one spelling — a free function calling a free function. The import table
/// above exists because both defects it found lived in the *spelling* dimension
/// rather than the language dimension, so the same question was put to the call
/// axis: 46 spellings across 15 languages (receiver calls, qualified/static
/// calls, chained calls, optional chaining, `Self::assoc`, `super()`, Kotlin
/// extension functions, C++ out-of-class definitions) were measured and every
/// one resolved. Nothing to fix — but the receiver path is the one that has
/// actually shipped broken (intra-class method→sibling-method edges vanished
/// once already), so the second spelling is pinned here rather than left to the
/// next audit.
///
/// Callee and caller share a file on purpose: a cross-file fixture would make a
/// zero ambiguous between "the arm is gone" and a resolution limitation. That
/// is not hypothetical either — a C++ call to an INHERITED method extracts fine
/// and then sits unresolved in `pending_unresolved_calls`, because binding it
/// needs class-hierarchy awareness the resolver does not have.
#[test]
fn method_call_parity_across_every_oo_language() {
    let cases: &[(&str, &str, &str)] = &[
        ("typescript", "m.ts", "class MTs {\n    helper(): number { return 1; }\n    handle(): number { return this.helper(); }\n}\n"),
        ("javascript", "m.js", "class MJs {\n    helper() { return 1; }\n    handle() { return this.helper(); }\n}\n"),
        ("python", "m.py", "class MPy:\n    def helper(self):\n        return 1\n\n    def handle(self):\n        return self.helper()\n"),
        ("rust", "m.rs", "pub struct MRs;\nimpl MRs {\n    pub fn helper(&self) -> i32 { 1 }\n    pub fn handle(&self) -> i32 { self.helper() }\n}\n"),
        ("go", "m.go", "package main\n\ntype MGo struct{}\n\nfunc (g MGo) helper() int { return 1 }\nfunc (g MGo) handle() int { return g.helper() }\n"),
        ("java", "MJv.java", "class MJv {\n    int helper() { return 1; }\n    int handle() { return this.helper(); }\n}\n"),
        ("csharp", "MCs.cs", "class MCs {\n    int Helper() { return 1; }\n    int Handle() { return this.Helper(); }\n}\n"),
        ("kotlin", "m.kt", "class MKt {\n    fun helper(): Int {\n        return 1\n    }\n\n    fun handle(): Int {\n        return this.helper()\n    }\n}\n"),
        ("ruby", "m.rb", "class MRb\n  def helper\n    1\n  end\n\n  def handle\n    self.helper\n  end\nend\n"),
        ("php", "m.php", "<?php\nclass MPhp {\n    public function helper() { return 1; }\n    public function handle() { return $this->helper(); }\n}\n"),
        ("swift", "m.swift", "class MSw {\n    func helper() -> Int {\n        return 1\n    }\n\n    func handle() -> Int {\n        return self.helper()\n    }\n}\n"),
        ("dart", "m.dart", "class MDt {\n  int helper() => 1;\n  int handle() => this.helper();\n}\n"),
        ("cpp", "m.cpp", "class MCpp {\npublic:\n    int helper() { return 1; }\n    int handle() { return this->helper(); }\n};\n"),
    ];

    let files: Vec<(&str, &str)> = cases.iter().map(|(_, f, s)| (*f, *s)).collect();
    let (_p, db) = index_parity_fixture(&files);

    // Per FILE, not per language: a language with two fixtures would otherwise
    // let the free-function one satisfy the method-call assertion.
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT count(*) FROM edges e JOIN nodes s ON e.source_id = s.id \
             JOIN files f ON s.file_id = f.id WHERE e.relation = 'calls' AND f.path = ?1",
        )
        .unwrap();
    let missing: Vec<&str> = cases
        .iter()
        .filter(|(_, file, _)| {
            stmt.query_row([format!("src/{file}")], |r| r.get::<_, i64>(0))
                .unwrap_or(0)
                < 1
        })
        .map(|(lang, _, _)| *lang)
        .collect();

    assert!(
        missing.is_empty(),
        "these languages lost the receiver/method call spelling — a sibling-method call \
         inside one class produced no `calls` edge: {missing:?}"
    );
}

/// `exports` edges for CommonJS, not just ESM — and the dead-code verdict that
/// rides on them.
///
/// An incoming `exports` edge is what makes `find_dead_code` report an unused
/// symbol as EXPORTED_UNUSED ("public surface, something outside may use it")
/// rather than ORPHAN ("nothing references this"). Only the ESM `export`
/// keyword produced one, so identical dead code got opposite verdicts by module
/// system — and CommonJS got the stronger, more dangerous one, inviting deletion
/// of a module's public API. Every JS file in this repo's own plugin is
/// CommonJS.
///
/// The category is asserted, not just the edge, because the category is the
/// thing a user acts on. `min_lines` is why the fixtures have bodies.
#[test]
fn exports_parity_across_module_systems() {
    let files: &[(&str, &str)] = &[
        (
            "esm.ts",
            "export function esmUnused(a: number, b: number): number {\n  const x = a + b;\n  const y = x * 2;\n  return y;\n}\n",
        ),
        (
            "shorthand.js",
            "function cjsShorthand(a, b) {\n  const x = a + b;\n  const y = x * 2;\n  return y;\n}\nmodule.exports = { cjsShorthand };\n",
        ),
        (
            "pair.js",
            "function cjsPair(a, b) {\n  const x = a + b;\n  const y = x * 2;\n  return y;\n}\nmodule.exports = { alias: cjsPair };\n",
        ),
        (
            "single.js",
            "function cjsSingle(a, b) {\n  const x = a + b;\n  const y = x * 2;\n  return y;\n}\nmodule.exports = cjsSingle;\n",
        ),
        (
            "prop.js",
            "function cjsProp(a, b) {\n  const x = a + b;\n  const y = x * 2;\n  return y;\n}\nexports.cjsProp = cjsProp;\n",
        ),
    ];
    let expected_exported = [
        "esmUnused",
        "cjsShorthand",
        "cjsPair",
        "cjsSingle",
        "cjsProp",
    ];

    let (_p, db) = index_parity_fixture(files);

    let mut stmt = db
        .conn()
        .prepare(
            "SELECT count(*) FROM edges e JOIN nodes t ON e.target_id = t.id \
             WHERE e.relation = 'exports' AND t.name = ?1",
        )
        .unwrap();
    let no_edge: Vec<&str> = expected_exported
        .iter()
        .copied()
        .filter(|name| stmt.query_row([name], |r| r.get::<_, i64>(0)).unwrap_or(0) < 1)
        .collect();
    assert!(
        no_edge.is_empty(),
        "these exported symbols got no `exports` edge, so dead-code will call them orphans: \
         {no_edge:?}"
    );

    // The consequence, end to end.
    let dead =
        code_graph_mcp::storage::queries::find_dead_code(db.conn(), None, None, false, 3, 50)
            .unwrap();
    let orphaned: Vec<&str> = dead
        .iter()
        .filter(|d| {
            expected_exported.contains(&d.name.as_str())
                && !code_graph_mcp::domain::is_dead_code_exported(
                    d.has_export_edge,
                    &d.code_content,
                    &d.file_path,
                    &d.name,
                )
        })
        .map(|d| d.name.as_str())
        .collect();
    assert!(
        orphaned.is_empty(),
        "dead-code called these exported symbols ORPHANS — the verdict that reads as \
         'safe to delete': {orphaned:?}"
    );
}

/// The `references` axis, one fixture per PASS.
///
/// `tests/reference_pass_wiring.rs` asserts every `extract_*_reference` appears
/// in `REFERENCE_PASSES` — that the wiring exists. It cannot see an extractor
/// gutted behind live wiring, so this runs the axis end to end.
///
/// Per pass, not per language, and that distinction was found the hard way: a
/// first version asserted "each reference-capable language emits ≥ 1" and
/// survived deleting Go's `type_identifier` row, because Go's OTHER pass
/// (`identifier` → value reference) kept the count above zero. A language with
/// two passes made the guard vacuous for both. Each row below exercises exactly
/// one pass in its own file, so deleting any single row reddens exactly one row
/// here.
#[test]
fn reference_parity_one_fixture_per_pass() {
    let cases: &[(&str, &str, &str)] = &[
        // rust: scoped_identifier → path reference
        ("rust path (crate::LIMIT)", "rp.rs", "pub const LIMIT: i32 = 5;\npub fn read_limit() -> i32 {\n    crate::LIMIT\n}\n"),
        // rust: type_identifier → type reference
        ("rust type (&Widget)", "rt.rs", "pub struct Widget {\n    pub id: i32,\n}\npub fn take(w: &Widget) -> i32 {\n    w.id\n}\n"),
        // rust: identifier → value reference (fn passed as a value)
        ("rust value (use_cb(cb))", "rv.rs", "pub fn cb() -> i32 {\n    1\n}\npub fn use_cb(f: fn() -> i32) -> i32 {\n    f()\n}\npub fn run() -> i32 {\n    use_cb(cb)\n}\n"),
        // ts: type_identifier → type reference
        ("ts type (s: Shape)", "t.ts", "export interface Shape {\n    side: number;\n}\nexport function area(s: Shape): number {\n    return s.side;\n}\n"),
        // js: identifier → value reference
        ("js value (register(handler))", "v.js", "function handler() {\n    return 1;\n}\nfunction register(fn) {\n    return fn();\n}\nfunction boot() {\n    return register(handler);\n}\nmodule.exports = { boot };\n"),
        // python: identifier in ANNOTATION context → type reference
        ("py type (s: Shape)", "pt.py", "class Shape:\n    pass\n\ndef area(s: Shape) -> int:\n    return 1\n"),
        // python: identifier in VALUE position → value reference
        ("py value (register(handler))", "pv.py", "def handler():\n    return 2\n\ndef register(fn):\n    return fn()\n\ndef boot():\n    return register(handler)\n"),
        // go: type_identifier → type reference
        ("go type (s Shape)", "gt.go", "package main\n\ntype Shape struct{ Side int }\n\nfunc area(s Shape) int { return s.Side }\n"),
        // go: identifier → value reference
        ("go value (register(handler))", "gv.go", "package main\n\nfunc handler() int { return 1 }\n\nfunc register(fn func() int) int { return fn() }\n\nfunc boot() int { return register(handler) }\n"),
        // java: type_identifier → type reference
        ("java type (Shape s)", "Jt.java", "class Shape {\n    int side;\n}\n\nclass Jt {\n    int area(Shape s) {\n        return s.side;\n    }\n}\n"),
        // c/cpp: identifier → value reference (function pointer)
        ("c value (register_cb(handler))", "cv.c", "int handler(int a) { return a; }\nint register_cb(int (*fn)(int)) { return fn(1); }\nint boot(void) { return register_cb(handler); }\n"),
    ];

    // Languages with NO reference passes at all. Indexed so the zero half is
    // exercised against real files rather than absent ones.
    let no_pass_files: &[(&str, &str)] = &[
        ("z.rb", "class RbShape\n  def side\n    1\n  end\nend\n\ndef rb_area(s)\n  s.side\nend\n"),
        ("z.php", "<?php\nclass PhpShape {\n    public function side() { return 1; }\n}\n\nfunction php_area(PhpShape $s) { return $s->side(); }\n"),
        ("z.kt", "class KtShape {\n    fun side(): Int {\n        return 1\n    }\n}\n\nfun ktArea(s: KtShape): Int {\n    return s.side()\n}\n"),
    ];

    let mut files: Vec<(&str, &str)> = cases.iter().map(|(_, f, s)| (*f, *s)).collect();
    files.extend_from_slice(no_pass_files);
    let (_p, db) = index_parity_fixture(&files);

    let mut stmt = db
        .conn()
        .prepare(
            "SELECT count(*) FROM edges e JOIN nodes s ON e.source_id = s.id \
             JOIN files f ON s.file_id = f.id \
             WHERE e.relation = 'references' AND f.path = ?1",
        )
        .unwrap();
    let mut count = |file: &str| -> i64 {
        stmt.query_row([format!("src/{file}")], |r| r.get::<_, i64>(0))
            .unwrap_or(0)
    };

    let silent: Vec<&str> = cases
        .iter()
        .filter(|(_, file, _)| count(file) < 1)
        .map(|(label, _, _)| *label)
        .collect();
    assert!(
        silent.is_empty(),
        "these REFERENCE_PASSES rows produced no edge — the pass is wired but not working: \
         {silent:?}"
    );

    let unexpected: Vec<&str> = no_pass_files
        .iter()
        .filter(|(file, _)| count(file) > 0)
        .map(|(file, _)| *file)
        .collect();
    assert!(
        unexpected.is_empty(),
        "these languages have no REFERENCE_PASSES row yet emitted `references` edges — either a \
         row was added (update this list) or something is manufacturing them: {unexpected:?}"
    );
}

/// The two false-edge shapes the pre-release review found in CommonJS export
/// extraction. Both understate rather than invite deletion, so neither is
/// urgent — but both bind the WRONG node, and a wrong edge is harder to notice
/// than a missing one.
#[test]
fn cjs_exports_do_not_bind_the_wrong_node() {
    let files: &[(&str, &str)] = &[
        // The UMD / webpack wrapper. `exports` here is the loader's object,
        // passed in as a parameter — assigning through it exports nothing from
        // THIS file. Matching the object by text alone treated it as a module
        // export and marked the real `wrapped` function exported.
        (
            "umd.js",
            "function wrapped(a, b) {\n  const x = a + b;\n  return x * 2;\n}\n(function (module, exports) {\n  exports.wrapped = wrapped;\n})(module, exports);\n",
        ),
        // A pair whose VALUE is not a symbol. Falling back to the KEY bound the
        // same-named real function, claiming a number export marks it public.
        (
            "pairlit.js",
            "function keyed(a, b) {\n  const x = a + b;\n  return x * 2;\n}\nmodule.exports = { keyed: 42 };\n",
        ),
        // Control: the real forms must still emit, or this test would pass by
        // breaking the feature.
        (
            "real.js",
            "function realExport(a, b) {\n  const x = a + b;\n  return x * 2;\n}\nmodule.exports = { realExport };\n",
        ),
    ];

    let (_p, db) = index_parity_fixture(files);
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT count(*) FROM edges e JOIN nodes t ON e.target_id = t.id \
             WHERE e.relation = 'exports' AND t.name = ?1",
        )
        .unwrap();
    let mut exported =
        |name: &str| -> i64 { stmt.query_row([name], |r| r.get::<_, i64>(0)).unwrap_or(0) };

    assert_eq!(
        exported("wrapped"),
        0,
        "a UMD wrapper's `exports` is a function parameter, not this module's exports"
    );
    assert_eq!(
        exported("keyed"), 0,
        "`module.exports = {{ keyed: 42 }}` exports a number; it must not mark the same-named function exported"
    );
    assert_eq!(
        exported("realExport"), 1,
        "control: a genuine shorthand export must still emit — without this the test passes by breaking the feature"
    );
}
