//! Dart-specific extraction. Two distinct AST shapes:
//! - imports: `import_or_export` → drill through `library_import` →
//!   `import_specification` → `configurable_uri`/`uri` → `string_literal`,
//!   then strip `dart:` / `package:` / relative path prefix to a bare module name.
//! - calls: `expression_statement` with `identifier` head + `selector(argument_part)`
//!   tail; method-style chains (`obj.transform()`) take the last
//!   `unconditional_assignable_selector` identifier as the callee.

use super::ParsedRelation;
use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use crate::domain::{REL_CALLS, REL_IMPORTS};

/// Extract Dart import targets from `import_or_export` nodes.
/// AST: import_or_export -> library_import -> import_specification -> configurable_uri/uri -> string_literal
pub(super) fn extract_dart_imports(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    fn find_uri_string(node: &tree_sitter::Node, source: &str) -> Option<String> {
        find_uri_string_inner(node, source, 0)
    }
    fn find_uri_string_inner(node: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
        if depth > MAX_SUBTREE_DEPTH { return None; }
        if node.kind() == "string_literal" {
            let text = node_text(node, source);
            // Strip quotes: 'dart:async' -> dart:async
            let trimmed = text.trim_matches('\'').trim_matches('"');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if let Some(result) = find_uri_string_inner(&child, source, depth + 1) {
                    return Some(result);
                }
            }
        }
        None
    }

    if let Some(uri) = find_uri_string(node, source) {
        // Extract meaningful name: 'dart:async' -> 'async', 'package:foo/bar.dart' -> 'bar'
        let import_name = if let Some(rest) = uri.strip_prefix("dart:") {
            rest.to_string()
        } else if let Some(rest) = uri.strip_prefix("package:") {
            // package:foo/bar.dart -> last segment without .dart
            rest.rsplit('/').next()
                .unwrap_or(rest)
                .trim_end_matches(".dart")
                .to_string()
        } else {
            // Relative import: 'src/utils.dart' -> 'utils'
            uri.rsplit('/').next()
                .unwrap_or(&uri)
                .trim_end_matches(".dart")
                .to_string()
        };
        if !import_name.is_empty() {
            results.push(ParsedRelation {
                source_name: "<module>".into(),
                target_name: import_name,
                relation: REL_IMPORTS.into(),
                metadata: None,
                source_language: String::new(),
            });
        }
    }
}

/// Extract Dart function/method calls from expression_statement nodes.
/// Dart calls: identifier + selector(argument_part) = simple call
/// identifier + selector(unconditional_assignable_selector(identifier)) + selector(argument_part) = method call
pub(super) fn extract_dart_calls(
    node: &tree_sitter::Node,
    source: &str,
    scope: &str,
    results: &mut Vec<ParsedRelation>,
) {
    // Walk children to find the pattern: identifier followed by selectors
    let child_count = node.named_child_count();
    if child_count < 2 { return; }

    // First named child should be an identifier (the call target or receiver)
    let first = match node.named_child(0) {
        Some(c) if c.kind() == "identifier" => c,
        _ => return,
    };

    // Check if any selector has an argument_part (making this a call)
    let mut has_call = false;
    let mut last_method_name: Option<String> = None;

    for i in 1..child_count {
        if let Some(sel) = node.named_child(i) {
            if sel.kind() == "selector" {
                // Check for argument_part (indicates a function call)
                for j in 0..sel.named_child_count() {
                    if let Some(inner) = sel.named_child(j) {
                        if inner.kind() == "argument_part" {
                            has_call = true;
                        }
                        // unconditional_assignable_selector contains the method name: .transform
                        if inner.kind() == "unconditional_assignable_selector"
                            || inner.kind() == "conditional_assignable_selector"
                        {
                            for k in 0..inner.named_child_count() {
                                if let Some(id) = inner.named_child(k) {
                                    if id.kind() == "identifier" {
                                        last_method_name = Some(node_text(&id, source).to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if has_call {
        let callee = last_method_name
            .unwrap_or_else(|| node_text(&first, source).to_string());
        if !callee.is_empty() {
            results.push(ParsedRelation {
                source_name: scope.to_string(),
                target_name: callee,
                relation: REL_CALLS.into(),
                metadata: None,
                source_language: String::new(),
            });
        }
    }
}
