//! The `imports` axis, one table row per (language, node kind), plus the
//! extractors it names.
//!
//! Generic side handles JS/TS/Java-style `import { Foo } from '...'` shapes
//! by walking import_clause/import_specifier subtrees. Python side keeps its
//! own paths because `from X import Y, Z` carries module-resolution metadata
//! that other languages don't have.
//!
//! These were arms of `walk_for_relations`'s match, the shape this crate keeps
//! getting bitten by: one arm per language per relation, where a missing arm is
//! not a compile error but a silently absent edge. `imports` is the axis where
//! every grammar spells the same idea differently — `import_declaration` (Swift,
//! Java) / `import_statement` (JS/TS, Python) / `import` (Kotlin) /
//! `import_spec` (Go) / `use_declaration` (Rust) / `using_directive` (C#) /
//! `namespace_use_declaration` plus four `*_expression` include forms (PHP) /
//! `preproc_include` (C/C++) / `import_or_export` (Dart) — so the mapping is
//! data, and as data it is enumerable: a language missing from the table is a
//! visible empty slot instead of an `if` nobody notices is gone.
//!
//! Sibling tables: `calls::CALL_PASSES`, `super::REFERENCE_PASSES`. The axes
//! still living in the match are heritage and exports; their arms are not
//! uniform in the way these are (heritage dispatches on the
//! `is_heritage_decl` PREDICATE rather than a fixed kind list, and the C#
//! `base_list` arm additionally inspects the PARENT node kind), so converting
//! them means widening the row shape, not moving bodies.

use super::super::lang_config::LanguageConfig;
use super::super::node_text;
use super::helpers::{extract_string_from_subtree, MAX_SUBTREE_DEPTH};
use super::{LangKey, ParsedRelation};
use crate::domain::REL_IMPORTS;

/// Everything an import extractor is allowed to see. Mirrors `calls::CallCtx`:
/// the resolved context arrives as data so the extractor does not have to be a
/// closure over `walk_for_relations`'s locals — which is what kept these
/// bodies inside the walk in the first place.
pub(super) struct ImportCtx<'a> {
    pub node: tree_sitter::Node<'a>,
    pub source: &'a str,
    pub language: &'a str,
    pub config: &'a LanguageConfig,
    /// The enclosing symbol, when there is one. Only Rust `use` and Go
    /// `import_spec` read it — both can appear inside a function body, and both
    /// attribute the edge to that function rather than to `<module>`.
    pub active_scope: Option<&'a str>,
}

impl ImportCtx<'_> {
    /// The `<module>`-level source name every other extractor here uses.
    fn module_import(&self, target: String, metadata: Option<String>) -> ParsedRelation {
        ParsedRelation {
            source_name: "<module>".into(),
            target_name: target,
            relation: REL_IMPORTS.into(),
            metadata,
            source_language: String::new(),
        }
    }
}

type ImportExtractor = fn(&ImportCtx, &mut Vec<ParsedRelation>);

/// `langs` for a row whose node kind is itself the guard, because no other
/// grammar spells that kind — same convention as `calls::ANY_LANG`, and the
/// opposite of `REFERENCE_PASSES`, where an empty `langs` is an inert row.
pub(super) const ANY_LANG: &[&str] = &[];

pub(super) struct ImportPass {
    /// Which language name to match on — see [`LangKey`]. Irrelevant for
    /// [`ANY_LANG`] rows.
    pub key: LangKey,
    pub langs: &'static [&'static str],
    pub kinds: &'static [&'static str],
    pub extract: ImportExtractor,
}

/// The `imports` axis. Order is the original match order; first match wins.
pub(super) const IMPORT_PASSES: &[ImportPass] = &[
    // PHP file includes: require / require_once / include / include_once.
    ImportPass {
        key: LangKey::Family,
        langs: &["php"],
        kinds: &[
            "require_expression",
            "require_once_expression",
            "include_expression",
            "include_once_expression",
        ],
        extract: extract_php_include,
    },
    ImportPass {
        key: LangKey::Family,
        langs: &["php"],
        kinds: &["namespace_use_declaration"],
        extract: extract_php_use,
    },
    // `import_declaration` is claimed by two grammars that mean different
    // shapes by it — this is exactly the collision the table makes visible.
    ImportPass {
        key: LangKey::Family,
        langs: &["swift"],
        kinds: &["import_declaration"],
        extract: extract_swift_import,
    },
    ImportPass {
        key: LangKey::Family,
        langs: &["java"],
        kinds: &["import_declaration"],
        extract: extract_java_import,
    },
    ImportPass {
        key: LangKey::Family,
        langs: &["dart"],
        kinds: &["import_or_export"],
        extract: extract_dart_import,
    },
    // JS/TS and Python share the kind; the split stays inside the extractor,
    // matching the CALL_PASSES convention for a kind no grammar disputes.
    ImportPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["import_statement"],
        extract: extract_import_statement,
    },
    ImportPass {
        key: LangKey::Family,
        langs: &["kotlin"],
        kinds: &["import"],
        extract: extract_kotlin_import,
    },
    ImportPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["import_from_statement"],
        extract: extract_python_from_import,
    },
    ImportPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["use_declaration"],
        extract: extract_rust_use,
    },
    ImportPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["import_spec"],
        extract: extract_go_import_spec,
    },
    ImportPass {
        key: LangKey::Family,
        langs: &["csharp"],
        kinds: &["using_directive"],
        extract: extract_csharp_using,
    },
    ImportPass {
        key: LangKey::Family,
        langs: &["c", "cpp"],
        kinds: &["preproc_include"],
        extract: extract_c_include,
    },
];

/// Run the first table row matching this node.
///
/// First-match-wins, not run-them-all: these rows were `match` arms, where one
/// arm excludes the others. The rows are disjoint today — and disjoint from the
/// kinds still handled by the caller's `match` — so the stop is belt and
/// braces; it is what keeps a future overlapping row from silently
/// double-emitting. Returns nothing, like `calls::run_call_passes`: an earlier
/// version handed back "did a row fire" for the caller to skip on, which the
/// caller then deliberately discarded, leaving a documented contract no code
/// honoured.
pub(super) fn run_import_passes(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    let kind = ctx.node.kind();
    for pass in IMPORT_PASSES {
        if !pass.kinds.contains(&kind) {
            continue;
        }
        if !pass.langs.is_empty() {
            let lang = match pass.key {
                LangKey::Raw => ctx.language,
                LangKey::Family => ctx.config.name,
            };
            if !pass.langs.contains(&lang) {
                continue;
            }
        }
        (pass.extract)(ctx, results);
        return;
    }
}

// ── extractors ───────────────────────────────────────────────────────────────

/// PHP `require 'path/File.php'` → IMPORTS to the bare file stem. Mirrors the
/// C/C++ `#include` and JS `require` shape: strip the directory and the `.php`
/// extension so Phase 2 can resolve the import to a concrete file node. Without
/// it PHP files got symbols + calls + `use` imports but no file-include edges,
/// so deps/cycles/affected/project_map under-reported PHP cross-file
/// dependencies. The AST node is a dedicated `*_expression`, never a
/// `function_call_expression`, so there is no double-count with the calls axis.
fn extract_php_include(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    let Some(raw) = extract_string_from_subtree(&ctx.node, ctx.source) else {
        return;
    };
    let stem = {
        let bare = raw.rsplit(['/', '\\']).next().unwrap_or(raw.as_str());
        bare.strip_suffix(".php").unwrap_or(bare).to_string()
    };
    if stem.is_empty() {
        return;
    }
    // Stamp the raw include path so Phase 2 can resolve it to the concrete
    // indexed file (require_once 'lib.php' → lib.php's <module> node),
    // mirroring the JS `js_module` specifier. target_name stays the bare stem
    // for the name-based fallback when the path doesn't resolve.
    let metadata = Some(serde_json::json!({ "php_include": &raw }).to_string());
    results.push(ctx.module_import(stem, metadata));
}

/// PHP `use App\Models\User;`
/// namespace_use_declaration → namespace_use_clause → qualified_name → name.
fn extract_php_use(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    fn find_last_name(n: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
        if depth > MAX_SUBTREE_DEPTH {
            return None;
        }
        let mut result = None;
        for i in 0..n.child_count() {
            if let Some(child) = n.child(i) {
                if child.kind() == "name" {
                    result = Some(node_text(&child, source).to_string());
                } else if matches!(child.kind(), "qualified_name" | "namespace_name") {
                    if let Some(inner) = find_last_name(&child, source, depth + 1) {
                        result = Some(inner);
                    }
                }
            }
        }
        result
    }
    for i in 0..ctx.node.named_child_count() {
        let Some(child) = ctx.node.named_child(i) else {
            continue;
        };
        if child.kind() != "namespace_use_clause" {
            continue;
        }
        if let Some(name) = find_last_name(&child, ctx.source, 0) {
            if !name.is_empty() {
                results.push(ctx.module_import(name, None));
            }
        }
    }
}

/// Swift `import Foundation`. The `identifier` may itself contain
/// simple_identifier children (dotted: `Foundation.NSObject`); the full text is
/// the import target.
fn extract_swift_import(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    for i in 0..ctx.node.named_child_count() {
        let Some(child) = ctx.node.named_child(i) else {
            continue;
        };
        if child.kind() != "identifier" {
            continue;
        }
        let name = node_text(&child, ctx.source).to_string();
        if !name.is_empty() {
            results.push(ctx.module_import(name, None));
        }
    }
}

/// Java `import p.B; import java.util.List; import static x.Y.z;`
/// AST: import_declaration → scoped_identifier(scope, name) | identifier, with
/// an optional trailing `.asterisk` for on-demand imports. Target = the LAST
/// segment (the imported type / static member), mirroring Kotlin. A wildcard
/// import names no single symbol → skip (never emit the package segment or `*`).
fn extract_java_import(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    let is_wildcard = (0..ctx.node.child_count())
        .any(|i| ctx.node.child(i).is_some_and(|c| c.kind() == "asterisk"));
    if is_wildcard {
        return;
    }
    let target = ctx
        .node
        .named_child(0)
        .and_then(|first| match first.kind() {
            "scoped_identifier" => first
                .child_by_field_name("name")
                .map(|n| node_text(&n, ctx.source).to_string()),
            "identifier" => Some(node_text(&first, ctx.source).to_string()),
            _ => None,
        });
    if let Some(name) = target {
        if !name.is_empty() {
            results.push(ctx.module_import(name, None));
        }
    }
}

/// Dart `import 'dart:async'; import 'package:foo/bar.dart';`
fn extract_dart_import(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    super::dart::extract_dart_imports(&ctx.node, ctx.source, results);
}

/// JS/TS `import … from '…'` and Python `import X`. One kind, two grammars that
/// agree on the spelling and disagree on the payload, so the split is here
/// rather than in two table rows with an open-ended "everything else" language
/// list.
fn extract_import_statement(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    if ctx.config.name == "python" {
        extract_python_import_names(&ctx.node, ctx.source, results);
    } else {
        extract_import_names(&ctx.node, ctx.source, results);
    }
}

/// Kotlin `import kotlinx.coroutines.flow.Flow`
/// AST: import → qualified_identifier → identifier*; the last segment is the
/// target.
fn extract_kotlin_import(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    for i in 0..ctx.node.named_child_count() {
        let Some(child) = ctx.node.named_child(i) else {
            continue;
        };
        if child.kind() != "qualified_identifier" {
            continue;
        }
        let count = child.named_child_count();
        if count == 0 {
            continue;
        }
        if let Some(last) = child.named_child(count - 1) {
            let name = node_text(&last, ctx.source).to_string();
            if !name.is_empty() && name != "*" {
                results.push(ctx.module_import(name, None));
            }
        }
    }
}

/// Python `from X import Y`.
fn extract_python_from_import(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    extract_python_from_import_names(&ctx.node, ctx.source, results);
}

/// Rust `use std::collections::HashMap;`, including the grouped form
/// `use std::collections::{HashMap, HashSet};`.
fn extract_rust_use(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    super::rust::extract_rust_use_imports(&ctx.node, ctx.source, ctx.active_scope, results);
}

/// Go `import "fmt"` / `import alias "fmt"`. Attributed to the enclosing scope
/// when there is one, unlike every other row here.
fn extract_go_import_spec(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    let Some(path_node) = ctx.node.child_by_field_name("path") else {
        return;
    };
    let path_text = node_text(&path_node, ctx.source)
        .trim_matches('"')
        .to_string();
    let Some(pkg_name) = path_text.rsplit('/').next() else {
        return;
    };
    if pkg_name.is_empty() {
        return;
    }
    results.push(ParsedRelation {
        source_name: ctx.active_scope.unwrap_or("<module>").to_string(),
        target_name: pkg_name.to_string(),
        relation: REL_IMPORTS.into(),
        metadata: None,
        source_language: String::new(),
    });
}

/// C# `using System; using System.Collections.Generic;`
fn extract_csharp_using(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    for i in 0..ctx.node.named_child_count() {
        let Some(child) = ctx.node.named_child(i) else {
            continue;
        };
        if !matches!(child.kind(), "qualified_name" | "identifier") {
            continue;
        }
        let name = node_text(&child, ctx.source).to_string();
        if !name.is_empty() && name != "using" {
            results.push(ctx.module_import(name, None));
        }
    }
}

/// C/C++ `#include "foo/bar.h"` / `#include <vector>`.
fn extract_c_include(ctx: &ImportCtx, results: &mut Vec<ParsedRelation>) {
    let path_node = (0..ctx.node.named_child_count())
        .filter_map(|i| ctx.node.named_child(i))
        .find(|c| matches!(c.kind(), "string_literal" | "system_lib_string"));
    let Some(p) = path_node else { return };
    let raw = node_text(&p, ctx.source);
    // string_literal text includes quotes; system_lib_string includes angle
    // brackets. Trim both forms uniformly.
    let unquoted = raw.trim_matches(|c| c == '"' || c == '<' || c == '>');
    if unquoted.is_empty() {
        return;
    }
    let last = unquoted.rsplit('/').next().unwrap_or(unquoted);
    let stem = last
        .trim_end_matches(".hpp")
        .trim_end_matches(".hxx")
        .trim_end_matches(".hh")
        .trim_end_matches(".h");
    if stem.is_empty() {
        return;
    }
    // Stamp the raw include path so Phase 2 can resolve it to the concrete
    // indexed header's <module> node (mirrors the PHP `php_include` / JS
    // `js_module` specifiers). target_name stays the bare stem for the
    // name-based fallback when the path doesn't resolve (system headers).
    let metadata = Some(serde_json::json!({ "c_include": unquoted }).to_string());
    results.push(ctx.module_import(stem.to_string(), metadata));
}

pub(super) fn extract_import_names(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    // Capture the ES module specifier (`from '../util/helper'`) so the indexer
    // can resolve a relative import to a concrete file (mirrors Python's
    // python_module metadata). The `source` field is the string literal; strip
    // its quotes. Absent (no `from` clause) → no metadata, default resolution.
    // The specifier is stamped on every binding this statement introduces.
    let js_module = node
        .child_by_field_name("source")
        .map(|s| node_text(&s, source))
        .map(|raw| {
            raw.trim_matches(|c| c == '"' || c == '\'' || c == '`')
                .to_string()
        })
        .filter(|s| !s.is_empty());
    let metadata: Option<String> = js_module
        .as_ref()
        .map(|m| serde_json::json!({ "js_module": m }).to_string());

    // Walk children looking for import specifiers or identifiers
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "import_clause" | "import_specifier" | "dotted_name" => {
                    // ESM namespace form `import * as ns from './m'`: the clause
                    // carries a namespace_import, which the specifier walk below
                    // does not know — it used to drop the whole binding
                    // (roadmap 2026-07-18 §2.3). Emit the q:"ns_import" marker
                    // (alias + specifier) that the indexer binds module-level and
                    // feeds into ns_module_map for `ns.foo()` member calls.
                    emit_namespace_import(&child, source, js_module.as_deref(), results);
                    // ESM DEFAULT binding: `import mod from './m'`, the single most
                    // common ESM form, used to emit NOTHING. Its binding is a bare
                    // `identifier` sitting directly under `import_clause` — not an
                    // `import_specifier`, which is all the specifier walk below looks
                    // for, and not a direct child of `import_statement`, which is
                    // what the `"identifier"` arm in the caller handles. So it fell
                    // between the two and every `import React from 'react'` shaped
                    // dependency was invisible to deps / cycles / affected /
                    // project_map.
                    //
                    // Bound module-level (like the namespace form), NOT as a symbol
                    // edge under the local name: the local name is arbitrary
                    // (`import anything from './m'`) and the default export's own
                    // node is usually named something else, so a name-based edge
                    // would either miss or bind a same-named symbol elsewhere. Its
                    // own marker rather than `ns_import` because a default import is
                    // NOT a namespace: `mod.foo()` is a member of the default export,
                    // not a top-level symbol of the module, so it must not feed the
                    // member-call binding map the way `import * as ns` does.
                    //
                    // `import mod, * as ns from './m'` carries BOTH bindings in
                    // one clause, and each used to emit its own module-level
                    // marker against the same module — two identical `imports`
                    // edges (measured: 2 rows, both `<module> -> <module>@tgt`,
                    // where `import mod` alone and `import * as ns` alone each
                    // produce 1). They survive because `idx_edges_unique`
                    // includes `metadata` on purpose (multiple route edges per
                    // file), so the differing `q` keeps both rows.
                    //
                    // The namespace marker is the one to keep: it also feeds
                    // ns_module_map for `ns.foo()` member calls. The default
                    // marker deliberately feeds nothing else (see above), so
                    // once a namespace binding has claimed the dependency edge
                    // there is nothing left for it to contribute.
                    let clause_has_namespace = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .any(|b| b.kind() == "namespace_import");
                    if child.kind() == "import_clause" && !clause_has_namespace {
                        if let Some(js_module) = js_module.as_deref() {
                            for j in 0..child.named_child_count() {
                                let Some(binding) = child.named_child(j) else {
                                    continue;
                                };
                                if binding.kind() != "identifier" {
                                    continue;
                                }
                                let name = node_text(&binding, source);
                                if name.is_empty() {
                                    continue;
                                }
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: name.to_string(),
                                    relation: REL_IMPORTS.into(),
                                    metadata: Some(
                                        serde_json::json!({
                                            "q": crate::domain::IMPORT_Q_DEFAULT,
                                            "js_module": js_module,
                                        })
                                        .to_string(),
                                    ),
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                    // For named imports: import { Foo, Bar } from '...'
                    extract_import_specifiers(&child, source, results, metadata.as_deref());
                }
                "namespace_import" => {
                    emit_namespace_import(&child, source, js_module.as_deref(), results);
                }
                "identifier" => {
                    let name = node_text(&child, source).to_string();
                    if !name.is_empty() && name != "from" {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_IMPORTS.into(),
                            metadata: metadata.clone(),
                            source_language: String::new(),
                        });
                    }
                }
                _ => {
                    extract_import_names_recursive(&child, source, results, metadata.as_deref());
                }
            }
        }
    }
}

/// Emit the ns_import marker for a `namespace_import` (`* as ns`) found either
/// directly or as a child of the import_clause. Marker shape mirrors the CJS
/// `q:"ns_require"` one (mod.rs) so the indexer's ns_module_map + module-level
/// binding treat ESM and CJS namespaces identically. No specifier → no marker
/// (nothing to resolve against).
fn emit_namespace_import(
    node: &tree_sitter::Node,
    source: &str,
    js_module: Option<&str>,
    results: &mut Vec<ParsedRelation>,
) {
    let ns = if node.kind() == "namespace_import" {
        Some(*node)
    } else {
        (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find(|c| c.kind() == "namespace_import")
    };
    let Some(ns) = ns else { return };
    let Some(alias) = (0..ns.named_child_count())
        .filter_map(|i| ns.named_child(i))
        .find(|c| c.kind() == "identifier")
    else {
        return;
    };
    let Some(module) = js_module else { return };
    results.push(ParsedRelation {
        source_name: "<module>".into(),
        target_name: node_text(&alias, source).to_string(),
        relation: REL_IMPORTS.into(),
        metadata: Some(
            serde_json::json!({ "q": crate::domain::IMPORT_Q_NS_IMPORT, "js_module": module })
                .to_string(),
        ),
        source_language: String::new(),
    });
}

fn extract_import_specifiers(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
    metadata: Option<&str>,
) {
    extract_import_specifiers_inner(node, source, results, metadata, 0);
}

fn extract_import_specifiers_inner(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
    metadata: Option<&str>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "import_specifier" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = node_text(&name_node, source).to_string();
            results.push(ParsedRelation {
                source_name: "<module>".into(),
                target_name: name,
                relation: REL_IMPORTS.into(),
                metadata: metadata.map(str::to_string),
                source_language: String::new(),
            });
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_import_specifiers_inner(&child, source, results, metadata, depth + 1);
        }
    }
}

fn extract_import_names_recursive(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
    metadata: Option<&str>,
) {
    extract_import_names_recursive_inner(node, source, results, metadata, 0);
}

fn extract_import_names_recursive_inner(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
    metadata: Option<&str>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "import_specifier" || node.kind() == "identifier" {
        let name = if node.kind() == "import_specifier" {
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string())
                .unwrap_or_else(|| node_text(node, source).to_string())
        } else {
            node_text(node, source).to_string()
        };
        if !name.is_empty() && name != "from" {
            results.push(ParsedRelation {
                source_name: "<module>".into(),
                target_name: name,
                relation: REL_IMPORTS.into(),
                metadata: metadata.map(str::to_string),
                source_language: String::new(),
            });
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_import_names_recursive_inner(&child, source, results, metadata, depth + 1);
        }
    }
}

/// Extract imports from Python `import X` / `import X, Y` statements.
/// AST: import_statement -> dotted_name ("os") ...
/// Adds metadata `{"python_module": "X", "is_module_import": true}` for module resolution.
pub(super) fn extract_python_import_names(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if child.kind() == "dotted_name" || child.kind() == "identifier" {
                let name = node_text(&child, source).to_string();
                if !name.is_empty() {
                    let metadata = serde_json::json!({
                        "python_module": &name,
                        "is_module_import": true
                    })
                    .to_string();
                    results.push(ParsedRelation {
                        source_name: "<module>".into(),
                        target_name: name,
                        relation: REL_IMPORTS.into(),
                        metadata: Some(metadata),
                        source_language: String::new(),
                    });
                }
            } else if child.kind() == "aliased_import" {
                // import os as operating_system — extract the original module name
                if let Some(module) = child.named_child(0) {
                    let name = node_text(&module, source).to_string();
                    if !name.is_empty() {
                        let metadata = serde_json::json!({
                            "python_module": &name,
                            "is_module_import": true
                        })
                        .to_string();
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_IMPORTS.into(),
                            metadata: Some(metadata),
                            source_language: String::new(),
                        });
                    }
                }
            }
        }
    }
}

/// Extract imports from Python `from X import Y, Z` statements.
/// AST: import_from_statement -> dotted_name ("collections"), dotted_name ("OrderedDict"), dotted_name ("defaultdict")
/// The first dotted_name is the module; the rest are imported names.
/// Adds metadata `{"python_module": "X"}` for module-constrained resolution,
/// and marks the module's own row `is_module_import: true` exactly as
/// `import os` does.
///
/// That marker used to be missing (audit 2026-08-22 P2-4, recorded in v0.123.0's
/// notes when set-equality parity rows surfaced it). The module is reached
/// through the `module_name` FIELD first, which leaves `is_first_dotted_name`
/// false, so the module's own `dotted_name` child fell through to the
/// imported-symbol branch and was emitted as a symbol named `pkg.mod`. Nothing
/// downstream could tell it from a real symbol of that name:
/// `resolve_python_module_targets` looked up `pkg.mod` in the name pool, found
/// nothing, and the module dependency `from X import Y` expresses — the
/// dominant Python import form — never reached the graph. With the marker it
/// binds to the `<module>` node of the resolved file, like `import X` always did.
///
/// Relative imports (`from . import x`, `from .rel import y`) are NOT affected:
/// their `module_name` is a `relative_import` node, which reaches neither branch
/// and emitted no module row before or after (probed against the pinned grammar,
/// not assumed).
pub(super) fn extract_python_from_import_names(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    // Prefer tree-sitter field name for module (more robust than positional heuristic)
    let module_name_node = node.child_by_field_name("module_name");
    let mut module_path: Option<String> = module_name_node
        .as_ref()
        .map(|m| node_text(m, source).to_string());
    // Identity, not text: `from pkg.mod import mod` has a symbol child whose
    // text equals part of the module path, and only the node itself is the module.
    let module_node_id = module_name_node
        .as_ref()
        .filter(|m| m.kind() == "dotted_name")
        .map(|m| m.id());
    let mut is_first_dotted_name = module_path.is_none();
    let emit_module_row = |name: &str, results: &mut Vec<ParsedRelation>| {
        if name.is_empty() {
            return;
        }
        results.push(ParsedRelation {
            source_name: "<module>".into(),
            target_name: name.to_string(),
            relation: REL_IMPORTS.into(),
            metadata: Some(
                serde_json::json!({ "python_module": name, "is_module_import": true }).to_string(),
            ),
            source_language: String::new(),
        });
    };
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if Some(child.id()) == module_node_id {
                emit_module_row(node_text(&child, source), results);
                continue;
            }
            match child.kind() {
                "dotted_name" => {
                    if is_first_dotted_name {
                        // Grammar without a `module_name` field: the first
                        // dotted_name is the module. Same row as the field path
                        // above, so the two cannot disagree.
                        let name = node_text(&child, source).to_string();
                        module_path = Some(name.clone());
                        is_first_dotted_name = false;
                        emit_module_row(&name, results);
                    } else {
                        // Subsequent dotted_names are imported symbols
                        let name = node_text(&child, source).to_string();
                        if !name.is_empty() {
                            let metadata = module_path
                                .as_ref()
                                .map(|m| serde_json::json!({"python_module": m}).to_string());
                            results.push(ParsedRelation {
                                source_name: "<module>".into(),
                                target_name: name,
                                relation: REL_IMPORTS.into(),
                                metadata,
                                source_language: String::new(),
                            });
                        }
                    }
                }
                "identifier" => {
                    // Some tree-sitter versions parse simple import names as bare identifiers
                    // (e.g., `from os import path` where `path` is an identifier, not dotted_name)
                    let name = node_text(&child, source).to_string();
                    if !name.is_empty() {
                        let metadata = module_path
                            .as_ref()
                            .map(|m| serde_json::json!({"python_module": m}).to_string());
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_IMPORTS.into(),
                            metadata,
                            source_language: String::new(),
                        });
                    }
                }
                "aliased_import" => {
                    // from X import Y as Z — extract Y (the original name)
                    if let Some(original) = child.named_child(0) {
                        let name = node_text(&original, source).to_string();
                        if !name.is_empty() {
                            let metadata = module_path
                                .as_ref()
                                .map(|m| serde_json::json!({"python_module": m}).to_string());
                            results.push(ParsedRelation {
                                source_name: "<module>".into(),
                                target_name: name,
                                relation: REL_IMPORTS.into(),
                                metadata,
                                source_language: String::new(),
                            });
                        }
                    }
                }
                "wildcard_import" => {
                    // from X import * — record as wildcard
                    let metadata = module_path
                        .as_ref()
                        .map(|m| serde_json::json!({"python_module": m}).to_string());
                    results.push(ParsedRelation {
                        source_name: "<module>".into(),
                        target_name: "*".into(),
                        relation: REL_IMPORTS.into(),
                        metadata,
                        source_language: String::new(),
                    });
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    /// These rows replaced `match` arms, where the compiler rejects a duplicate
    /// arm. A table has no such check: a second row for the same (language,
    /// kind) is simply unreachable, and the edges its extractor would have
    /// emitted are silently absent — the exact failure mode the table exists to
    /// remove. `import_declaration` is ALREADY claimed twice here (Swift and
    /// Java, by disjoint languages), so a third claim reads as the established
    /// pattern rather than as a mistake, which is precisely why this has to be
    /// checked rather than eyeballed.
    #[test]
    fn no_two_rows_claim_the_same_language_and_kind() {
        let mut claimed: Vec<(&str, &str)> = Vec::new();
        for pass in IMPORT_PASSES {
            for kind in pass.kinds {
                // An ANY_LANG row claims the kind for every language, so any
                // other row naming that kind is dead.
                if pass.langs.is_empty() {
                    let conflict = IMPORT_PASSES
                        .iter()
                        .filter(|p| p.kinds.contains(kind))
                        .count();
                    assert_eq!(
                        conflict,
                        1,
                        "kind {kind:?} is claimed by an ANY_LANG row and by {} other row(s) — \
                         those can never fire",
                        conflict - 1
                    );
                    continue;
                }
                for lang in pass.langs {
                    let slot = (*lang, *kind);
                    assert!(
                        !claimed.contains(&slot),
                        "two IMPORT_PASSES rows claim {slot:?} — the second can never fire"
                    );
                    claimed.push(slot);
                }
            }
        }
    }

    /// A row that matches nothing compiles fine and emits nothing forever.
    #[test]
    fn no_row_is_inert() {
        for pass in IMPORT_PASSES {
            assert!(
                !pass.kinds.is_empty(),
                "an IMPORT_PASSES row has no node kinds — it can never fire"
            );
            assert!(
                pass.kinds.iter().all(|k| !k.is_empty()),
                "an IMPORT_PASSES row has an empty node kind — it can never fire"
            );
            assert!(
                pass.langs.iter().all(|l| !l.is_empty()),
                "an IMPORT_PASSES row has an empty language name — that slot can never match"
            );
        }
    }
}
