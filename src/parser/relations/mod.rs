//! Relation extraction from parsed tree-sitter trees.
//!
//! Public surface:
//! - `ParsedRelation` (intermediate struct consumed by the indexer's edge resolver)
//! - `extract_relations` (parses + walks)
//! - `extract_relations_from_tree` (walks a pre-parsed tree)
//!
//! Internals are split per concern:
//! - `helpers`: shared callee/string utilities used by every language arm
//! - `imports`: generic JS/TS/Java + Python imports
//! - `inherits`: superclass + implements across class-style languages
//! - `exports`: TS/JS export statements
//! - `routes`: Express/Go/Python HTTP route registrations
//! - `rust`: Rust-specific `use` and `impl Trait for Type`
//! - `dart`: Dart-specific imports and call expressions
//!
//! `walk_for_relations` is the single recursive dispatcher that maps tree-sitter
//! node kinds to the appropriate extractor. It must keep all language arms in
//! one match because they share `current_scope` / `current_class` propagation
//! (splitting it per-language would either duplicate the recursion or lose
//! scope context across language-specific arms).

use anyhow::Result;
use super::lang_config::LanguageConfig;
use super::node_text;
use crate::domain::{REL_CALLS, REL_INHERITS, REL_IMPORTS, REL_IMPLEMENTS, MAX_RELATION_DEPTH};

mod helpers;
mod imports;
mod inherits;
mod exports;
mod routes;
mod rust;
mod dart;

#[cfg(test)]
mod tests;

use helpers::{extract_callee_name, extract_string_from_subtree, MAX_SUBTREE_DEPTH};
use imports::{extract_import_names, extract_python_import_names, extract_python_from_import_names};
use inherits::{extract_superclasses, extract_implements};
use exports::extract_export_names;
use routes::{extract_route_pattern, extract_python_route};
use rust::{extract_rust_use_imports, extract_rust_impl_trait};
use dart::{extract_dart_imports, extract_dart_calls};

pub struct ParsedRelation {
    pub source_name: String,
    pub target_name: String,
    pub relation: String,
    pub metadata: Option<String>,
    /// Language of the file that produced this relation. Stamped by
    /// `extract_relations_from_tree`; used by the edge resolver in pipeline.rs
    /// to enforce same-language hard equality on cross-file `calls` edges
    /// (prevents false positives like Python `foo()` matching a C `foo()`).
    pub source_language: String,
}

pub fn extract_relations(source: &str, language: &str) -> Result<Vec<ParsedRelation>> {
    let tree = super::treesitter::parse_tree(source, language)?;
    Ok(extract_relations_from_tree(&tree, source, language))
}

/// Extract relations from a pre-parsed tree (avoids re-parsing).
pub fn extract_relations_from_tree(tree: &tree_sitter::Tree, source: &str, language: &str) -> Vec<ParsedRelation> {
    let mut relations = Vec::new();
    let config = LanguageConfig::for_language(language);
    walk_for_relations(tree.root_node(), source, language, &config, None, None, &mut relations, 0);
    // Stamp source_language on every relation. walk_for_relations constructs
    // ParsedRelation with source_language: String::new(), and we fill it in
    // here so every call site inside walk doesn't need to propagate language.
    for r in &mut relations {
        r.source_language = language.to_string();
    }
    relations
}

#[allow(clippy::too_many_arguments)]
fn walk_for_relations(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    config: &LanguageConfig,
    current_scope: Option<&str>,
    current_class: Option<&str>,
    results: &mut Vec<ParsedRelation>,
    depth: usize,
) {
    if depth > MAX_RELATION_DEPTH { return; }
    let kind = node.kind();

    // Determine if this node creates a new scope
    let scope_name = match kind {
        "function_declaration" | "function_definition" | "function_item"
        | "method_definition" | "method_declaration" | "constructor_declaration"
        | "async_function_definition"
        | "method" | "singleton_method" => {
            node.child_by_field_name("name")
                .map(|n| {
                    let name = node_text(&n, source).to_string();
                    match current_class {
                        Some(cls) => format!("{}.{}", cls, name),
                        None => name,
                    }
                })
        }
        "arrow_function" => {
            // `const foo = () => {}` → scope name is the binding.
            // Other anonymous arrows (e.g. `test(() => {...})` callbacks,
            // `.map(x => x)` inline lambdas) inherit the parent scope so
            // calls inside them attribute to the enclosing named function
            // or fall through to `<module>` at file top level. Returning
            // `Some("<anonymous>")` here used to emit unresolvable edges
            // (no node is named "<anonymous>"), silently dropping test
            // callback calls and causing false-positive orphans.
            node.parent()
                .filter(|p| p.kind() == "variable_declarator")
                .and_then(|p| p.child_by_field_name("name"))
                .map(|n| {
                    let name = node_text(&n, source).to_string();
                    match current_class {
                        Some(cls) => format!("{}.{}", cls, name),
                        None => name,
                    }
                })
        }
        // Dart: function_body is a sibling of either method_signature
        // (in class_body) or function_signature (top-level declaration).
        // Look at previous sibling to find the function/method name.
        "function_body" if config.function_body_has_methods => {
            node.prev_sibling()
                .and_then(|s| match s.kind() {
                    // Top-level Dart function: declaration > function_signature + function_body
                    "function_signature" => s.child_by_field_name("name")
                        .map(|n| node_text(&n, source).to_string()),
                    // Class method: method_signature wraps function_signature
                    "method_signature" => (0..s.named_child_count())
                        .filter_map(|i| s.named_child(i))
                        .find(|c| matches!(c.kind(),
                            "function_signature" | "constructor_signature"
                            | "getter_signature" | "setter_signature"))
                        .and_then(|sig| sig.child_by_field_name("name"))
                        .map(|n| node_text(&n, source).to_string()),
                    _ => None,
                })
                .map(|name| match current_class {
                    Some(cls) => format!("{}.{}", cls, name),
                    None => name,
                })
        }
        _ => None,
    };

    let active_scope = scope_name.as_deref().or(current_scope);

    match kind {
        // Call expressions
        "call_expression" => {
            // JS/TS CommonJS: require('./foo') / require('pkg') → IMPORTS edge.
            // Mirrors the Ruby `require` handling above; target is the last path
            // segment so node_modules imports become `<external>` sentinels and
            // relative imports can match a file module node by name.
            if matches!(config.name, "javascript" | "typescript" | "tsx")
                && node.child_by_field_name("function")
                    .map(|f| node_text(&f, source) == "require")
                    .unwrap_or(false)
            {
                if let Some(args) = node.child_by_field_name("arguments") {
                    if let Some(first) = args.named_child(0) {
                        if let Some(path) = extract_string_from_subtree(&first, source) {
                            // Normalize `node:fs` → `fs`; strip trailing JS extensions.
                            let normalized = path.strip_prefix("node:").unwrap_or(&path);
                            let segment = normalized.trim_end_matches(".js")
                                .trim_end_matches(".ts")
                                .trim_end_matches(".mjs")
                                .trim_end_matches(".cjs")
                                .rsplit('/')
                                .next()
                                .unwrap_or(normalized)
                                .to_string();
                            if !segment.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: segment,
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                }
            }

            // Check for HTTP route registration patterns first
            if let Some(route_rel) = extract_route_pattern(&node, source, language) {
                results.push(route_rel);
            }

            // Call relation extraction. For JS/TS/TSX, fall back to `<module>`
            // when the call sits at file top level (imports, init code) or
            // inside an anonymous callback (test/describe/it, Array.map, etc.)
            // so same-file edges can still resolve. Other languages keep the
            // named-scope-only rule to avoid polluting their callgraphs.
            let call_scope: Option<String> = match active_scope {
                Some(s) => Some(s.to_string()),
                None if matches!(config.name, "javascript" | "typescript" | "tsx") => {
                    Some("<module>".to_string())
                }
                None => None,
            };
            if let Some(scope) = call_scope {
                if let Some(callee) = extract_callee_name(&node, source) {
                    results.push(ParsedRelation {
                        source_name: scope,
                        target_name: callee,
                        relation: REL_CALLS.into(),
                        metadata: None,
                        source_language: String::new(),
                    });
                }
            }
        }

        // Rust/Go: struct instantiation → calls edge (enables cross-file dead code tracking)
        // e.g., `MyStruct { field: value }` or `MyStruct::new()` (calls already handled above)
        "struct_expression" => {
            if let Some(scope) = active_scope {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let struct_name = node_text(&name_node, source);
                    // Strip path prefix: path::MyStruct → MyStruct
                    let short_name = struct_name.rsplit("::").next().unwrap_or(struct_name);
                    if !short_name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: short_name.to_string(),
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Ruby: `call` node kind for method calls (require, require_relative, and regular calls)
        "call" if config.name == "ruby" => {
            // Extract method name from the "method" field
            if let Some(method_node) = node.child_by_field_name("method") {
                let method_name = node_text(&method_node, source);
                // require 'json' / require_relative 'helper'
                if method_name == "require" || method_name == "require_relative" {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        if let Some(first_arg) = args.named_child(0) {
                            if let Some(string_val) = extract_string_from_subtree(&first_arg, source) {
                                results.push(ParsedRelation {
                                    source_name: active_scope.unwrap_or("<module>").to_string(),
                                    target_name: string_val,
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                } else if let Some(scope) = active_scope {
                    // Regular method call
                    results.push(ParsedRelation {
                        source_name: scope.to_string(),
                        target_name: method_name.to_string(),
                        relation: REL_CALLS.into(),
                        metadata: None,
                        source_language: String::new(),
                    });
                }
            }
        }

        // PHP: function_call_expression (doSomething()), member_call_expression ($this->move()),
        // scoped_call_expression (User::all())
        "function_call_expression" | "member_call_expression" | "scoped_call_expression"
            if config.name == "php" =>
        {
            if let Some(scope) = active_scope {
                // All three PHP call types have a `name` child for the method/function name
                // For scoped_call_expression, there are multiple `name` children; the second is the method
                let callee = if kind == "scoped_call_expression" {
                    // User::all() -> children: name("User"), "::", name("all"), arguments
                    // The method name is the second `name` child
                    let mut names = Vec::new();
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "name" {
                                names.push(node_text(&child, source).to_string());
                            }
                        }
                    }
                    names.pop() // Last name is the method
                } else {
                    // function_call_expression: name("doSomething"), arguments
                    // member_call_expression: variable_name("$this"), "->", name("move"), arguments
                    node.child_by_field_name("name")
                        .or_else(|| {
                            // Fallback: find the `name` node among children
                            (0..node.child_count())
                                .filter_map(|i| node.child(i))
                                .find(|c| c.kind() == "name")
                        })
                        .map(|n| node_text(&n, source).to_string())
                };
                if let Some(name) = callee {
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: name,
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // PHP: use App\Models\User;
        // namespace_use_declaration -> namespace_use_clause -> qualified_name -> name (last segment)
        "namespace_use_declaration" if config.name == "php" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "namespace_use_clause" {
                        // Get the last `name` segment from the qualified_name
                        fn find_last_name(n: &tree_sitter::Node, source: &str) -> Option<String> {
                            find_last_name_inner(n, source, 0)
                        }
                        fn find_last_name_inner(n: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
                            if depth > MAX_SUBTREE_DEPTH { return None; }
                            let mut result = None;
                            for i in 0..n.child_count() {
                                if let Some(child) = n.child(i) {
                                    if child.kind() == "name" {
                                        result = Some(node_text(&child, source).to_string());
                                    } else if child.kind() == "qualified_name" || child.kind() == "namespace_name" {
                                        if let Some(inner) = find_last_name_inner(&child, source, depth + 1) {
                                            result = Some(inner);
                                        }
                                    }
                                }
                            }
                            result
                        }
                        if let Some(name) = find_last_name(&child, source) {
                            if !name.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: name,
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Swift: import Foundation, import UIKit
        // AST: import_declaration -> identifier -> simple_identifier
        "import_declaration" if config.name == "swift" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "identifier" {
                        // identifier may contain simple_identifier children (dotted: Foundation.NSObject)
                        // Use the full text as the import target
                        let name = node_text(&child, source).to_string();
                        if !name.is_empty() {
                            results.push(ParsedRelation {
                                source_name: "<module>".into(),
                                target_name: name,
                                relation: REL_IMPORTS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
        }

        // Dart: import 'dart:async'; import 'package:foo/bar.dart';
        "import_or_export" if config.name == "dart" => {
            extract_dart_imports(&node, source, results);
        }

        // Import statements
        "import_statement" => {
            if config.name == "python" {
                extract_python_import_names(&node, source, results);
            } else {
                extract_import_names(&node, source, results);
            }
        }

        // Kotlin: import kotlinx.coroutines.flow.Flow
        // AST: import -> qualified_identifier -> identifier*
        // Extract the last identifier segment as the import target
        "import" if config.name == "kotlin" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "qualified_identifier" {
                        let count = child.named_child_count();
                        if count > 0 {
                            if let Some(last) = child.named_child(count - 1) {
                                let name = node_text(&last, source).to_string();
                                if !name.is_empty() && name != "*" {
                                    results.push(ParsedRelation {
                                        source_name: "<module>".into(),
                                        target_name: name,
                                        relation: REL_IMPORTS.into(),
                                        metadata: None,
                                        source_language: String::new(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Python: from X import Y
        "import_from_statement" => {
            extract_python_from_import_names(&node, source, results);
        }

        // Class inheritance
        "class_declaration" | "class_definition" | "class" => {
            let class_name = node.child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string());

            if let Some(ref cls) = class_name {
                // Check for extends/superclass (supports multiple inheritance)
                for parent in extract_superclasses(&node, source) {
                    results.push(ParsedRelation {
                        source_name: cls.clone(),
                        target_name: parent,
                        relation: REL_INHERITS.into(),
                        metadata: None,
                        source_language: String::new(),
                    });
                }

                // Check for implements (TS/JS/Java)
                extract_implements(&node, source, cls, results);
            }
        }

        // Export statements (TS/JS)
        "export_statement" => {
            extract_export_names(&node, source, results);
        }

        // Rust: impl Trait for Type → implements edge (type-level + method-level)
        "impl_item" => {
            if let Some(impl_rel) = extract_rust_impl_trait(&node, source) {
                let type_name = impl_rel.source_name.clone();
                results.push(impl_rel);
                // For each method in the trait impl block, emit a method-level
                // implements edge: TypeName → method_name. This ensures dead code
                // detection sees incoming implements edges on trait methods.
                if let Some(body) = node.child_by_field_name("body") {
                    for i in 0..body.named_child_count() {
                        if let Some(child) = body.named_child(i) {
                            if child.kind() == "function_item" {
                                if let Some(name_node) = child.child_by_field_name("name") {
                                    let method_name = node_text(&name_node, source);
                                    if !method_name.is_empty() {
                                        results.push(ParsedRelation {
                                            source_name: type_name.clone(),
                                            target_name: method_name.to_string(),
                                            relation: REL_IMPLEMENTS.into(),
                                            metadata: None,
                                            source_language: String::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Rust: use std::collections::HashMap;
        // Also handles grouped imports: use std::collections::{HashMap, HashSet};
        "use_declaration" => {
            extract_rust_use_imports(&node, source, active_scope, results);
        }

        // Go: import "fmt" or import alias "fmt"
        "import_spec" => {
            if let Some(path_node) = node.child_by_field_name("path") {
                let path_text = node_text(&path_node, source).trim_matches('"').to_string();
                if let Some(pkg_name) = path_text.rsplit('/').next() {
                    if !pkg_name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: active_scope.unwrap_or("<module>").to_string(),
                            target_name: pkg_name.to_string(),
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Python decorated definitions (for Flask/FastAPI route decorators)
        "decorated_definition" => {
            if let Some(route_rel) = extract_python_route(&node, source) {
                results.push(route_rel);
            }
        }

        // C# using directives: using System; using System.Collections.Generic;
        "using_directive" if config.name == "csharp" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "qualified_name" || child.kind() == "identifier" {
                        let name = node_text(&child, source).to_string();
                        if !name.is_empty() && name != "using" {
                            results.push(ParsedRelation {
                                source_name: "<module>".into(),
                                target_name: name,
                                relation: REL_IMPORTS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
        }

        // C# inheritance: class Dog : Animal, IWalkable
        "base_list" if config.name == "csharp" => {
            // Get the class/struct name from the parent node
            let owner_name = node.parent()
                .and_then(|p| p.child_by_field_name("name"))
                .map(|n| node_text(&n, source).to_string());
            let owner = owner_name.as_deref().or(active_scope).unwrap_or("<module>");
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    let base_name = node_text(&child, source).to_string();
                    if !base_name.is_empty() {
                        let rel = if config.interface_by_prefix
                            && base_name.starts_with('I') && base_name.len() > 1
                            && base_name.chars().nth(1).map(|c| c.is_uppercase()).unwrap_or(false) {
                            REL_IMPLEMENTS
                        } else {
                            REL_INHERITS
                        };
                        results.push(ParsedRelation {
                            source_name: owner.to_string(),
                            target_name: base_name,
                            relation: rel.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // C# method/function calls: invocation_expression (Console.WriteLine(...), Baz(), etc.)
        "invocation_expression" if config.name == "csharp" => {
            if let Some(scope) = active_scope {
                if let Some(func) = node.named_child(0) {
                    let callee = match func.kind() {
                        "identifier" => Some(node_text(&func, source).to_string()),
                        "member_access_expression" => {
                            // e.g. Console.WriteLine — extract "WriteLine"
                            func.child_by_field_name("name")
                                .map(|n| node_text(&n, source).to_string())
                        }
                        _ => None,
                    };
                    if let Some(name) = callee {
                        if !name.is_empty() {
                            results.push(ParsedRelation {
                                source_name: scope.to_string(),
                                target_name: name,
                                relation: REL_CALLS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
        }

        // Dart: expression_statement with identifier + selector(argument_part) = function call
        // e.g. fetchData() or result.transform() or print(result)
        "expression_statement" if config.name == "dart" => {
            if let Some(scope) = active_scope {
                extract_dart_calls(&node, source, scope, results);
            }
        }

        // C/C++: `#include "foo.h"` → IMPORTS to "foo"
        //         `#include <stdio.h>` → IMPORTS to "stdio"
        // Header extension stripped so cross-file resolution can match the
        // bare module name (mirrors JS require pattern).
        "preproc_include" if matches!(config.name, "c" | "cpp") => {
            let path_node = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| matches!(c.kind(), "string_literal" | "system_lib_string"));
            if let Some(p) = path_node {
                let raw = node_text(&p, source);
                // string_literal text includes quotes; system_lib_string
                // includes angle brackets. Trim both forms uniformly.
                let unquoted = raw.trim_matches(|c| c == '"' || c == '<' || c == '>');
                if !unquoted.is_empty() {
                    let last = unquoted.rsplit('/').next().unwrap_or(unquoted);
                    let stem = last.trim_end_matches(".hpp")
                        .trim_end_matches(".hxx")
                        .trim_end_matches(".hh")
                        .trim_end_matches(".h");
                    if !stem.is_empty() {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: stem.to_string(),
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Bash command invocation:
        //   `source <file>` / `. <file>` → IMPORTS edge (mirrors JS require).
        //   Otherwise: CALLS edge to command_name.
        // External commands (cat, grep, ...) without a function_definition
        // in any indexed shell file get dropped at Phase 2 same-language
        // edge resolution (see feedback_edge_resolution_same_language).
        "command" if config.name == "bash" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let raw = node_text(&name_node, source).trim();

                if raw == "source" || raw == "." {
                    // First non-command_name word/string sibling = the file path arg.
                    let arg = (0..node.named_child_count())
                        .filter_map(|i| node.named_child(i))
                        .find(|n| matches!(n.kind(), "word" | "string" | "raw_string"));
                    if let Some(arg_node) = arg {
                        let text = node_text(&arg_node, source);
                        let unquoted = text.trim_matches(|c| c == '"' || c == '\'');
                        // Skip dynamic paths ($VAR, $(...), ${...}).
                        if !unquoted.is_empty() && !unquoted.contains('$') {
                            let last = unquoted.rsplit('/').next().unwrap_or(unquoted);
                            let stem = last.trim_end_matches(".sh").trim_end_matches(".bash");
                            if !stem.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: stem.to_string(),
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                } else if let Some(scope) = active_scope {
                    // Strip path prefix: ./foo, /usr/bin/foo, path/to/foo → foo
                    let short = raw.rsplit('/').next().unwrap_or(raw);
                    // Reject variable expansions ($VAR, ${VAR}), substitutions
                    // ($(...), `...`), and concatenations (foo$VAR) — not statically
                    // resolvable. Allow [a-zA-Z_.][a-zA-Z0-9_.-]* (covers `cat`,
                    // `_helper`, `Backup_Files`, `script.sh`, `.bashrc`).
                    let first_ok = short.chars().next()
                        .map(|c| c == '_' || c == '.' || c.is_ascii_alphabetic())
                        .unwrap_or(false);
                    let all_ok = short.chars().all(|c|
                        c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
                    );
                    if first_ok && all_ok {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: short.to_string(),
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        _ => {}
    }

    // Determine class context for children: when entering a class body,
    // pass the class name so methods can build qualified scope names.
    let child_class = match kind {
        "class_declaration" | "class_definition" | "class" => {
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string())
        }
        _ => None,
    };
    let effective_class = child_class.as_deref().or(current_class);

    // Recurse into children
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_for_relations(child, source, language, config, active_scope, effective_class, results, depth + 1);
        }
    }
}
