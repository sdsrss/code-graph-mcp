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
        source: "import os\nfrom pkg.mod import Helper\n",
        expect: &["os", "Helper"],
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

        for want in row.expect {
            if !got.iter().any(|g| g == want) {
                failures.push(format!(
                    "{} ({}): missing import target {want:?}; got {got:?}",
                    row.lang, row.path
                ));
            }
        }
        // A language that emits NOTHING is the exact silent-drop this table
        // exists to catch, so say it separately from a single missing target.
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
