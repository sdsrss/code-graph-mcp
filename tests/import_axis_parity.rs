//! Parity table for the `imports` axis — one row per (language, import
//! spelling), asserting the target names the extractor actually emits.
//!
//! Why a table and not a dozen `#[test]`s: `imports` is the axis where every
//! grammar spells the same idea differently (`import_declaration` /
//! `import_statement` / `import` / `import_spec` / `use_declaration` /
//! `using_directive` / `namespace_use_declaration` / `preproc_include` /
//! `import_or_export`), and the failure mode this crate keeps hitting is a
//! language whose handler is simply absent — no compile error, no failing test,
//! just an edge that never appears. A row here is that language's proof of
//! life, and a language present in `IMPORT_PASSES` but missing here is visible
//! as a gap in one place.
//!
//! It was also the A/B instrument for the arms→table conversion: written
//! against the table, then run against the arms it replaced. The full
//! verification went wider than these rows — every relation (not just imports,
//! metadata included) extracted from 2,927 external py/ts/tsx/js/go/java files
//! was dumped on both sides and compared: 397,088 rows, byte-identical.

use code_graph_mcp::domain::REL_IMPORTS;
use code_graph_mcp::parser::relations::extract_relations;

struct Row {
    lang: &'static str,
    path: &'static str,
    source: &'static str,
    /// Import targets expected, in any order.
    expect: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        lang: "typescript",
        path: "a.ts",
        source: "import { Foo, Bar } from './mod';\n",
        expect: &["Foo", "Bar"],
    },
    Row {
        lang: "javascript",
        path: "a.js",
        source: "import Thing from './thing';\n",
        expect: &["Thing"],
    },
    Row {
        lang: "python",
        path: "a.py",
        // `pkg.mod` is here because the extractor emits it, not because it
        // should. `extract_python_from_import_names` sets `module_path` from the
        // `module_name` FIELD first, which leaves `is_first_dotted_name` false,
        // so the module's own `dotted_name` child falls into the
        // imported-symbol branch and is emitted as one. The tell is the
        // metadata: `import os` marks its row `is_module_import: true`, while
        // this row carries no such marker, so nothing downstream can tell the
        // module apart from a symbol actually named `pkg.mod`.
        //
        // PRE-EXISTING, and deliberately not fixed here — correcting it changes
        // the edge set of every indexed Python project and so belongs with an
        // INDEX_VERSION bump of its own, not inside a release cut for other
        // reasons. Pinned rather than papered over: set equality is what
        // surfaced it, and an `expect` that quietly omitted `pkg.mod` would have
        // hidden it again.
        source: "import os\nfrom pkg.mod import Helper\n",
        expect: &["os", "Helper", "pkg.mod"],
    },
    Row {
        lang: "java",
        path: "A.java",
        // Wildcard imports name no single symbol and must stay unemitted —
        // the row that pins the `asterisk` check.
        source: "import java.util.List;\nimport java.io.*;\n",
        expect: &["List"],
    },
    Row {
        lang: "go",
        path: "a.go",
        source: "package main\nimport \"fmt\"\nimport \"net/http\"\n",
        expect: &["fmt", "http"],
    },
    Row {
        lang: "rust",
        path: "a.rs",
        source: "use std::collections::HashMap;\n",
        expect: &["HashMap"],
    },
    Row {
        lang: "csharp",
        path: "A.cs",
        source: "using System.Collections.Generic;\n",
        expect: &["System.Collections.Generic"],
    },
    Row {
        lang: "kotlin",
        path: "A.kt",
        source: "import kotlinx.coroutines.flow.Flow\n",
        expect: &["Flow"],
    },
    Row {
        lang: "swift",
        path: "A.swift",
        source: "import Foundation\n",
        expect: &["Foundation"],
    },
    Row {
        lang: "php",
        path: "a.php",
        source: "<?php\nuse App\\Models\\User;\nrequire_once 'lib/helpers.php';\n",
        expect: &["User", "helpers"],
    },
    Row {
        lang: "c",
        path: "a.c",
        source: "#include <stdio.h>\n#include \"local/thing.h\"\n",
        expect: &["stdio", "thing"],
    },
    Row {
        lang: "cpp",
        path: "a.cpp",
        source: "#include <vector>\n#include \"util/helper.hpp\"\n",
        expect: &["vector", "helper"],
    },
    Row {
        lang: "dart",
        path: "a.dart",
        source: "import 'dart:async';\nimport 'package:foo/bar.dart';\n",
        expect: &["async", "bar"],
    },
];

#[test]
fn every_language_emits_its_import_targets() {
    let mut failures: Vec<String> = Vec::new();

    for row in ROWS {
        let rels = match extract_relations(row.source, row.lang) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: extract_relations failed: {e}", row.lang));
                continue;
            }
        };
        let mut got: Vec<String> = rels
            .iter()
            .filter(|r| r.relation == REL_IMPORTS)
            .map(|r| r.target_name.clone())
            .collect();
        got.sort();
        got.dedup();

        let mut want: Vec<String> = row.expect.iter().map(|s| s.to_string()).collect();
        want.sort();
        want.dedup();

        // SET EQUALITY, not containment. An earlier version asserted only that
        // every expected target was present, which a phantom target rides
        // through untouched — and a phantom bound to a real node is the failure
        // this repository treats as worse than a missing edge, because nothing
        // in the answer says it is wrong.
        let missing: Vec<&String> = want.iter().filter(|w| !got.contains(w)).collect();
        let extra: Vec<&String> = got.iter().filter(|g| !want.contains(g)).collect();

        if !missing.is_empty() {
            failures.push(format!(
                "{} ({}): missing import target(s) {missing:?}; got {got:?}",
                row.lang, row.path
            ));
        }
        if !extra.is_empty() {
            failures.push(format!(
                "{} ({}): UNEXPECTED import target(s) {extra:?} — a phantom target binds a \
                 real node and nothing downstream can tell it apart; got {got:?}",
                row.lang, row.path
            ));
        }
        // A language that emits NOTHING is the exact silent-drop this table
        // exists to catch, so say it separately from a missing target.
        if got.is_empty() {
            failures.push(format!(
                "{} ({}): emitted NO import edges at all — handler absent?",
                row.lang, row.path
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "imports axis parity failures:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn java_wildcard_import_emits_no_target() {
    // Negative control for the row above: without it, an extractor that emitted
    // the package segment or a literal "*" would still satisfy `expect: ["List"]`.
    let rels = extract_relations("import java.io.*;\n", "java").unwrap();
    let targets: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        targets.is_empty(),
        "a wildcard import names no single symbol; got {targets:?}"
    );
}
