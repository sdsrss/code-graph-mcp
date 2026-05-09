//! Generic and Python-specific import extraction.
//! Generic side handles JS/TS/Java-style `import { Foo } from '...'` shapes
//! by walking import_clause/import_specifier subtrees. Python side keeps its
//! own paths because `from X import Y, Z` carries module-resolution metadata
//! that other languages don't have.

use super::ParsedRelation;
use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use crate::domain::REL_IMPORTS;

pub(super) fn extract_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    // Walk children looking for import specifiers or identifiers
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "import_clause" | "import_specifier" | "dotted_name" => {
                    // For named imports: import { Foo, Bar } from '...'
                    extract_import_specifiers(&child, source, results);
                }
                "identifier" | "namespace_import" => {
                    let name = node_text(&child, source).to_string();
                    if !name.is_empty() && name != "from" {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
                _ => {
                    extract_import_names_recursive(&child, source, results);
                }
            }
        }
    }
}

fn extract_import_specifiers(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    extract_import_specifiers_inner(node, source, results, 0);
}

fn extract_import_specifiers_inner(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>, depth: usize) {
    if depth > MAX_SUBTREE_DEPTH { return; }
    if node.kind() == "import_specifier" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = node_text(&name_node, source).to_string();
            results.push(ParsedRelation {
                source_name: "<module>".into(),
                target_name: name,
                relation: REL_IMPORTS.into(),
                metadata: None,
                source_language: String::new(),
            });
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_import_specifiers_inner(&child, source, results, depth + 1);
        }
    }
}

fn extract_import_names_recursive(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    extract_import_names_recursive_inner(node, source, results, 0);
}

fn extract_import_names_recursive_inner(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>, depth: usize) {
    if depth > MAX_SUBTREE_DEPTH { return; }
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
                metadata: None,
                source_language: String::new(),
            });
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_import_names_recursive_inner(&child, source, results, depth + 1);
        }
    }
}

/// Extract imports from Python `import X` / `import X, Y` statements.
/// AST: import_statement -> dotted_name ("os") ...
/// Adds metadata `{"python_module": "X", "is_module_import": true}` for module resolution.
pub(super) fn extract_python_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if child.kind() == "dotted_name" || child.kind() == "identifier" {
                let name = node_text(&child, source).to_string();
                if !name.is_empty() {
                    let metadata = serde_json::json!({
                        "python_module": &name,
                        "is_module_import": true
                    }).to_string();
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
                        }).to_string();
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
/// Adds metadata `{"python_module": "X"}` for module-constrained resolution.
pub(super) fn extract_python_from_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    // Prefer tree-sitter field name for module (more robust than positional heuristic)
    let mut module_path: Option<String> = node.child_by_field_name("module_name")
        .map(|m| node_text(&m, source).to_string());
    let mut is_first_dotted_name = module_path.is_none();
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "dotted_name" => {
                    if is_first_dotted_name {
                        // First dotted_name is the module name — capture it for resolution
                        module_path = Some(node_text(&child, source).to_string());
                        is_first_dotted_name = false;
                    } else {
                        // Subsequent dotted_names are imported symbols
                        let name = node_text(&child, source).to_string();
                        if !name.is_empty() {
                            let metadata = module_path.as_ref().map(|m| {
                                serde_json::json!({"python_module": m}).to_string()
                            });
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
                        let metadata = module_path.as_ref().map(|m| {
                            serde_json::json!({"python_module": m}).to_string()
                        });
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
                            let metadata = module_path.as_ref().map(|m| {
                                serde_json::json!({"python_module": m}).to_string()
                            });
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
                    let metadata = module_path.as_ref().map(|m| {
                        serde_json::json!({"python_module": m}).to_string()
                    });
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
