//! Rust-specific extraction: `use` declarations (simple/grouped/nested/aliased)
//! and `impl Trait for Type` blocks (emits both type-level and method-level
//! IMPLEMENTS edges so the dead-code pass sees incoming edges on trait methods).

use super::ParsedRelation;
use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use crate::domain::{REL_IMPORTS, REL_IMPLEMENTS};

/// Extract import names from Rust `use` declarations by walking the tree-sitter AST.
/// Handles simple (`use foo::Bar`), grouped (`use foo::{Bar, Baz}`),
/// nested (`use foo::{bar::{A, B}}`), aliased (`use foo::Bar as B`), and glob imports.
pub(super) fn extract_rust_use_imports(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
    results: &mut Vec<ParsedRelation>,
) {
    fn collect_use_names(node: &tree_sitter::Node, source: &str, names: &mut Vec<String>) {
        collect_use_names_inner(node, source, names, 0);
    }
    fn collect_use_names_inner(node: &tree_sitter::Node, source: &str, names: &mut Vec<String>, depth: usize) {
        if depth > MAX_SUBTREE_DEPTH { return; }
        match node.kind() {
            "use_as_clause" => {
                if let Some(child) = node.named_child(0) {
                    collect_use_names_inner(&child, source, names, depth + 1);
                }
            }
            "use_wildcard" => {}
            "use_list" => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        collect_use_names_inner(&child, source, names, depth + 1);
                    }
                }
            }
            "scoped_use_list" => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        if child.kind() != "scoped_identifier" && child.kind() != "identifier" {
                            collect_use_names_inner(&child, source, names, depth + 1);
                        }
                    }
                }
            }
            "scoped_identifier" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(&name_node, source);
                    if !name.is_empty() && name != "*" && name != "self" {
                        names.push(name.to_string());
                    }
                }
            }
            "identifier" | "type_identifier" => {
                let name = node_text(node, source);
                if !name.is_empty() && name != "self" {
                    names.push(name.to_string());
                }
            }
            _ => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        collect_use_names_inner(&child, source, names, depth + 1);
                    }
                }
            }
        }
    }

    let mut names = Vec::new();
    // The use_declaration's first named child is the argument (scoped_identifier, use_list, etc.)
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_use_names(&child, source, &mut names);
        }
    }

    let scope_name = scope.unwrap_or("<module>");
    for name in names {
        results.push(ParsedRelation {
            source_name: scope_name.to_string(),
            target_name: name,
            relation: REL_IMPORTS.into(),
            metadata: None,
            source_language: String::new(),
        });
    }
}

/// Extract `impl Trait for Type` → Type implements Trait
pub(super) fn extract_rust_impl_trait(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    // impl_item has "trait" and "type" fields when it's `impl Trait for Type`
    let trait_node = node.child_by_field_name("trait")?;
    let type_node = node.child_by_field_name("type")?;
    let trait_name = node_text(&trait_node, source).to_string();
    let type_name = node_text(&type_node, source).to_string();
    if trait_name.is_empty() || type_name.is_empty() {
        return None;
    }
    Some(ParsedRelation {
        source_name: type_name,
        target_name: trait_name,
        relation: REL_IMPLEMENTS.into(),
        metadata: None,
        source_language: String::new(),
    })
}
