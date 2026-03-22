use anyhow::Result;
use super::lang_config::LanguageConfig;
use super::node_text;
use crate::domain::{REL_CALLS, REL_INHERITS, REL_IMPORTS, REL_ROUTES_TO, REL_IMPLEMENTS, REL_EXPORTS, MAX_RELATION_DEPTH};

pub struct ParsedRelation {
    pub source_name: String,
    pub target_name: String,
    pub relation: String,
    pub metadata: Option<String>,
}

pub fn extract_relations(source: &str, language: &str) -> Result<Vec<ParsedRelation>> {
    let tree = super::treesitter::parse_tree(source, language)?;
    Ok(extract_relations_from_tree(&tree, source, language))
}

/// Extract relations from a pre-parsed tree (avoids re-parsing).
pub fn extract_relations_from_tree(tree: &tree_sitter::Tree, source: &str, language: &str) -> Vec<ParsedRelation> {
    let mut relations = Vec::new();
    walk_for_relations(tree.root_node(), source, language, None, None, &mut relations, 0);
    relations
}

fn walk_for_relations(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    current_scope: Option<&str>,
    current_class: Option<&str>,
    results: &mut Vec<ParsedRelation>,
    depth: usize,
) {
    if depth > MAX_RELATION_DEPTH { return; }
    let kind = node.kind();
    let config = LanguageConfig::for_language(language);

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
            // Try to get name from parent variable_declarator: const foo = () => {}
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
                .or_else(|| Some("<anonymous>".to_string()))
        }
        // Dart: function_body is a sibling of method_signature in class_body
        // Look at previous sibling to find the method name
        "function_body" if config.function_body_has_methods => {
            node.prev_sibling()
                .filter(|s| s.kind() == "method_signature")
                .and_then(|s| {
                    // method_signature -> function_signature -> name
                    (0..s.named_child_count())
                        .filter_map(|i| s.named_child(i))
                        .find(|c| matches!(c.kind(), "function_signature" | "constructor_signature" | "getter_signature" | "setter_signature"))
                        .and_then(|sig| sig.child_by_field_name("name"))
                        .map(|n| {
                            let name = node_text(&n, source).to_string();
                            match current_class {
                                Some(cls) => format!("{}.{}", cls, name),
                                None => name,
                            }
                        })
                })
        }
        _ => None,
    };

    let active_scope = scope_name.as_deref().or(current_scope);

    match kind {
        // Call expressions
        "call_expression" => {
            // Check for HTTP route registration patterns first
            if let Some(route_rel) = extract_route_pattern(&node, source, language) {
                results.push(route_rel);
            }

            // Existing call relation extraction
            if let Some(scope) = active_scope {
                if let Some(callee) = extract_callee_name(&node, source) {
                    results.push(ParsedRelation {
                        source_name: scope.to_string(),
                        target_name: callee,
                        relation: REL_CALLS.into(),
                        metadata: None,
                    });
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
                            let mut result = None;
                            for i in 0..n.child_count() {
                                if let Some(child) = n.child(i) {
                                    if child.kind() == "name" {
                                        result = Some(node_text(&child, source).to_string());
                                    } else if child.kind() == "qualified_name" || child.kind() == "namespace_name" {
                                        if let Some(inner) = find_last_name(&child, source) {
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

        // Rust: impl Trait for Type → implements edge
        "impl_item" => {
            if let Some(impl_rel) = extract_rust_impl_trait(&node, source) {
                results.push(impl_rel);
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
        "using_directive" => {
            if config.name == "csharp" {
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
                                });
                            }
                        }
                    }
                }
            }
        }

        // C# inheritance: class Dog : Animal, IWalkable
        "base_list" => {
            if config.name == "csharp" {
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
                            });
                        }
                    }
                }
            }
        }

        // C# method/function calls: invocation_expression (Console.WriteLine(...), Baz(), etc.)
        "invocation_expression" => {
            if config.name == "csharp" {
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
                                });
                            }
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
            walk_for_relations(child, source, language, active_scope, effective_class, results, depth + 1);
        }
    }
}

/// Extract import names from Rust `use` declarations by walking the tree-sitter AST.
/// Handles simple (`use foo::Bar`), grouped (`use foo::{Bar, Baz}`),
/// nested (`use foo::{bar::{A, B}}`), aliased (`use foo::Bar as B`), and glob imports.
fn extract_rust_use_imports(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
    results: &mut Vec<ParsedRelation>,
) {
    fn collect_use_names(node: &tree_sitter::Node, source: &str, names: &mut Vec<String>) {
        match node.kind() {
            "use_as_clause" => {
                // `use foo::Bar as B` — extract the original name (first named child)
                // The first child may be a scoped_identifier, so recurse to extract leaf name
                if let Some(child) = node.named_child(0) {
                    collect_use_names(&child, source, names);
                }
            }
            "use_wildcard" => {
                // `use foo::*` — skip
            }
            "use_list" => {
                // `{HashMap, HashSet}`
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        collect_use_names(&child, source, names);
                    }
                }
            }
            "scoped_use_list" => {
                // `foo::{A, B}` — skip the path (scoped_identifier), only process the use_list
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        if child.kind() != "scoped_identifier" && child.kind() != "identifier" {
                            collect_use_names(&child, source, names);
                        }
                    }
                }
            }
            "scoped_identifier" => {
                // `foo::Bar` — extract the last segment (the name)
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
                // Recurse into children for any unhandled wrapper nodes
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        collect_use_names(&child, source, names);
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
        });
    }
}

/// Extract `impl Trait for Type` → Type implements Trait
fn extract_rust_impl_trait(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
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
    })
}

fn extract_callee_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let function = node.child_by_field_name("function")
        .or_else(|| node.named_child(0))?;

    match function.kind() {
        "identifier" | "simple_identifier" => Some(node_text(&function, source).to_string()),
        "member_expression" | "field_expression" => {
            // e.g., obj.method — extract "method" or "obj.method"
            if let Some(prop) = function.child_by_field_name("property")
                .or_else(|| function.child_by_field_name("field")) {
                Some(node_text(&prop, source).to_string())
            } else {
                Some(node_text(&function, source).to_string())
            }
        }
        "scoped_identifier" => {
            // Rust: Self::method(), Module::func(), std::collections::HashMap::new()
            // Extract the rightmost name component (the actual function being called)
            function.child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string())
        }
        "selector_expression" => {
            // Go: receiver.Method(), http.HandleFunc(), etc.
            function.child_by_field_name("field")
                .map(|n| node_text(&n, source).to_string())
        }
        "navigation_expression" => {
            // Kotlin/Swift: obj.method() — last named child is the method name
            // Swift wraps it in navigation_suffix → simple_identifier
            let count = function.named_child_count();
            if count > 0 {
                let last = function.named_child(count - 1)?;
                if last.kind() == "navigation_suffix" {
                    // Swift: navigation_suffix -> simple_identifier
                    last.named_child(0)
                        .map(|n| node_text(&n, source).to_string())
                } else {
                    Some(node_text(&last, source).to_string())
                }
            } else {
                None
            }
        }
        _ => None, // Unknown callee expression — skip to avoid noise in call graph
    }
}

fn extract_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
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
    if node.kind() == "import_specifier" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = node_text(&name_node, source).to_string();
            results.push(ParsedRelation {
                source_name: "<module>".into(),
                target_name: name,
                relation: REL_IMPORTS.into(),
                metadata: None,
            });
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_import_specifiers(&child, source, results);
        }
    }
}

fn extract_import_names_recursive(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
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
            });
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_import_names_recursive(&child, source, results);
        }
    }
}

/// Extract imports from Python `import X` / `import X, Y` statements.
/// AST: import_statement -> dotted_name ("os") ...
/// Adds metadata `{"python_module": "X", "is_module_import": true}` for module resolution.
fn extract_python_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
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
fn extract_python_from_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
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
                    });
                }
                _ => {}
            }
        }
    }
}

fn extract_superclasses(node: &tree_sitter::Node, source: &str) -> Vec<String> {
    let mut parents = Vec::new();
    // Look for "extends" clause / superclass
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "class_heritage" | "extends_clause" => {
                // TS/JS: class_heritage -> extends_clause -> type_identifier
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "extends_clause" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "identifier" || type_node.kind() == "type_identifier" {
                                        parents.push(node_text(&type_node, source).to_string());
                                    }
                                }
                            }
                        }
                        if inner.kind() == "identifier" || inner.kind() == "type_identifier" {
                            parents.push(node_text(&inner, source).to_string());
                        }
                    }
                }
            }
            "argument_list" => {
                // Python: class Dog(Animal, Pet) — extract all parent classes
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "identifier" || inner.kind() == "dotted_name" {
                            parents.push(node_text(&inner, source).to_string());
                        }
                    }
                }
            }
            "superclass" => {
                // Java: superclass -> type_identifier
                // Ruby: superclass -> constant (e.g., `< ApplicationController`)
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "type_identifier" || inner.kind() == "identifier"
                            || inner.kind() == "dotted_name" || inner.kind() == "constant" || inner.kind() == "scope_resolution" {
                            parents.push(node_text(&inner, source).to_string());
                        }
                    }
                }
                if parents.is_empty() {
                    let text = node_text(&child, source);
                    parents.push(text.trim_start_matches('(').trim_end_matches(')').trim().to_string());
                }
            }
            "delegation_specifiers" => {
                // Kotlin: class UserService : BaseService, UserRepository
                // delegation_specifiers -> delegation_specifier -> user_type -> identifier
                for k in 0..child.named_child_count() {
                    if let Some(spec) = child.named_child(k) {
                        if spec.kind() == "delegation_specifier" {
                            // Walk through user_type to find the identifier
                            if let Some(user_type) = spec.named_child(0) {
                                if let Some(ident) = user_type.named_child(0) {
                                    let name = node_text(&ident, source).to_string();
                                    if !name.is_empty() {
                                        parents.push(name);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "base_clause" => {
                // PHP: class Dog extends Animal
                // base_clause -> name (the parent class)
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "name" || inner.kind() == "qualified_name" {
                            let name = node_text(&inner, source).to_string();
                            if !name.is_empty() {
                                parents.push(name);
                            }
                        }
                    }
                }
            }
            "inheritance_specifier" => {
                // Swift: class UserService: UserRepository, Codable
                // inheritance_specifier -> user_type -> type_identifier
                if let Some(inherits_from) = child.child_by_field_name("inherits_from") {
                    // Walk into user_type to find type_identifier
                    let name = if inherits_from.kind() == "user_type" {
                        inherits_from.named_child(0)
                            .map(|n| node_text(&n, source).to_string())
                    } else {
                        Some(node_text(&inherits_from, source).to_string())
                    };
                    if let Some(name) = name {
                        if !name.is_empty() {
                            parents.push(name);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    parents
}

fn extract_implements(
    node: &tree_sitter::Node,
    source: &str,
    class_name: &str,
    results: &mut Vec<ParsedRelation>,
) {
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            // TS/JS: class_heritage contains implements_clause children
            "class_heritage" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "implements_clause" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "type_identifier" || type_node.kind() == "identifier" {
                                        results.push(ParsedRelation {
                                            source_name: class_name.to_string(),
                                            target_name: node_text(&type_node, source).to_string(),
                                            relation: REL_IMPLEMENTS.into(),
                                            metadata: None,
                                        });
                                    }
                                    // Handle generic_type: IService<T> -> extract IService
                                    if type_node.kind() == "generic_type" {
                                        if let Some(name_node) = type_node.named_child(0) {
                                            if name_node.kind() == "type_identifier" || name_node.kind() == "identifier" {
                                                results.push(ParsedRelation {
                                                    source_name: class_name.to_string(),
                                                    target_name: node_text(&name_node, source).to_string(),
                                                    relation: REL_IMPLEMENTS.into(),
                                                    metadata: None,
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // PHP: class Dog implements Walkable, Swimmable
            // class_interface_clause -> name children
            "class_interface_clause" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "name" || inner.kind() == "qualified_name" {
                            let name = node_text(&inner, source).to_string();
                            if !name.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: class_name.to_string(),
                                    target_name: name,
                                    relation: REL_IMPLEMENTS.into(),
                                    metadata: None,
                                });
                            }
                        }
                    }
                }
            }
            // Java: super_interfaces -> type_list -> type_identifier
            "super_interfaces" | "interfaces" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "type_list" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "type_identifier" || type_node.kind() == "identifier" {
                                        results.push(ParsedRelation {
                                            source_name: class_name.to_string(),
                                            target_name: node_text(&type_node, source).to_string(),
                                            relation: REL_IMPLEMENTS.into(),
                                            metadata: None,
                                        });
                                    }
                                }
                            }
                        }
                        // Fallback: direct type_identifier child
                        if inner.kind() == "type_identifier" || inner.kind() == "identifier" {
                            results.push(ParsedRelation {
                                source_name: class_name.to_string(),
                                target_name: node_text(&inner, source).to_string(),
                                relation: REL_IMPLEMENTS.into(),
                                metadata: None,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn extract_export_names(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    // Walk direct children for exported declarations
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "function_declaration" | "class_declaration" | "interface_declaration"
            | "type_alias_declaration" | "enum_declaration" | "abstract_class_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(&name_node, source).to_string();
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_EXPORTS.into(),
                            metadata: None,
                        });
                    }
                }
            }
            "lexical_declaration" => {
                // export const foo = ..., export let bar = ...
                for j in 0..child.named_child_count() {
                    if let Some(decl) = child.named_child(j) {
                        if decl.kind() == "variable_declarator" {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name = node_text(&name_node, source).to_string();
                                if !name.is_empty() {
                                    results.push(ParsedRelation {
                                        source_name: "<module>".into(),
                                        target_name: name,
                                        relation: REL_EXPORTS.into(),
                                        metadata: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// --- Route extraction functions ---

fn extract_route_pattern(node: &tree_sitter::Node, source: &str, language: &str) -> Option<ParsedRelation> {
    match language {
        "typescript" | "javascript" => extract_express_route(node, source),
        "go" => extract_go_route(node, source),
        _ => None,
    }
}

fn extract_express_route(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "member_expression" { return None; }

    let object = function.child_by_field_name("object")?;
    let property = function.child_by_field_name("property")?;

    let obj_name = node_text(&object, source);
    let method_name = node_text(&property, source);

    // Check if this looks like an HTTP route registration
    if !matches!(obj_name, "app" | "router" | "server") { return None; }
    let http_method = match method_name {
        "get" => "GET",
        "post" => "POST",
        "put" => "PUT",
        "delete" => "DELETE",
        "patch" => "PATCH",
        "use" => "USE",
        _ => return None,
    };

    let args = node.child_by_field_name("arguments")?;
    // First argument is the path (string)
    let first_arg = args.named_child(0)?;
    let path = node_text(&first_arg, source)
        .trim_matches(|c| c == '\'' || c == '"')
        .to_string();

    // Last named argument is the handler
    let handler_count = args.named_child_count();
    if handler_count < 2 { return None; }
    let handler_arg = args.named_child(handler_count - 1)?;

    if handler_arg.kind() == "identifier" {
        // Named handler reference: router.post('/path', handlerFn)
        let handler_name = node_text(&handler_arg, source).to_string();
        let metadata = serde_json::json!({"method": http_method, "path": path}).to_string();
        Some(ParsedRelation {
            source_name: handler_name.clone(),
            target_name: handler_name,
            relation: REL_ROUTES_TO.into(),
            metadata: Some(metadata),
        })
    } else if matches!(handler_arg.kind(), "arrow_function" | "function_expression" | "function") {
        // Inline handler: router.post('/path', async (req, res) => { ... })
        // Link to the <module> node so find_http_route can locate the file and handler lines
        let handler_start = handler_arg.start_position().row + 1;
        let handler_end = handler_arg.end_position().row + 1;
        let metadata = serde_json::json!({
            "method": http_method,
            "path": path,
            "inline": true,
            "handler_start_line": handler_start,
            "handler_end_line": handler_end,
        }).to_string();
        Some(ParsedRelation {
            source_name: "<module>".into(),
            target_name: "<module>".into(),
            relation: REL_ROUTES_TO.into(),
            metadata: Some(metadata),
        })
    } else {
        None
    }
}

fn extract_go_route(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "selector_expression" { return None; }

    let operand = function.child_by_field_name("operand")?;
    let field = function.child_by_field_name("field")?;

    if node_text(&operand, source) != "http" { return None; }
    let func_name = node_text(&field, source);
    if !matches!(func_name, "HandleFunc" | "Handle") { return None; }

    let args = node.child_by_field_name("arguments")?;
    let path_arg = args.named_child(0)?;
    let path = node_text(&path_arg, source).trim_matches('"').to_string();

    let handler_arg = args.named_child(1)?;
    let handler = node_text(&handler_arg, source).to_string();

    let metadata = serde_json::json!({"method": "ALL", "path": path}).to_string();

    Some(ParsedRelation {
        source_name: handler.clone(),
        target_name: handler,
        relation: REL_ROUTES_TO.into(),
        metadata: Some(metadata),
    })
}

fn extract_python_route(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    // Look for decorator that matches @app.route(...) or @app.get(...) etc.
    let mut decorator = None;
    let mut func_def = None;

    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "decorator" => decorator = Some(child),
                "function_definition" => func_def = Some(child),
                _ => {}
            }
        }
    }

    let dec = decorator?;
    let func = func_def?;
    let func_name_node = func.child_by_field_name("name")?;
    let func_name = node_text(&func_name_node, source);

    // Get the decorator expression text
    let dec_text = node_text(&dec, source);

    // Check for route-like decorator patterns (e.g., @app.route, @app.get, @bp.post)
    // Only match known framework receiver names to avoid false positives (e.g., @cache.get)
    let has_known_receiver = ["app.", "bp.", "blueprint.", "router.", "api."]
        .iter()
        .any(|prefix| dec_text.contains(prefix));
    if !has_known_receiver {
        return None;
    }
    if !dec_text.contains(".route(")
        && !dec_text.contains(".get(")
        && !dec_text.contains(".post(")
        && !dec_text.contains(".put(")
        && !dec_text.contains(".delete(")
        && !dec_text.contains(".patch(") {
        return None;
    }

    // Extract path from decorator arguments
    let path = extract_string_from_subtree(&dec, source)?;

    let method = if dec_text.contains(".get(") { "GET" }
        else if dec_text.contains(".post(") { "POST" }
        else if dec_text.contains(".put(") { "PUT" }
        else if dec_text.contains(".delete(") { "DELETE" }
        else if dec_text.contains(".patch(") { "PATCH" }
        else { "ANY" };

    let metadata = serde_json::json!({"method": method, "path": path}).to_string();

    Some(ParsedRelation {
        source_name: func_name.to_string(),
        target_name: func_name.to_string(),
        relation: REL_ROUTES_TO.into(),
        metadata: Some(metadata),
    })
}

fn extract_string_from_subtree(node: &tree_sitter::Node, source: &str) -> Option<String> {
    if node.kind() == "string" {
        let text = node_text(node, source);
        return Some(text.trim_matches(|c| c == '\'' || c == '"').to_string());
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(s) = extract_string_from_subtree(&child, source) {
                return Some(s);
            }
        }
    }
    None
}

/// Extract Dart import targets from `import_or_export` nodes.
/// AST: import_or_export -> library_import -> import_specification -> configurable_uri/uri -> string_literal
fn extract_dart_imports(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    fn find_uri_string(node: &tree_sitter::Node, source: &str) -> Option<String> {
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
                if let Some(result) = find_uri_string(&child, source) {
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
            });
        }
    }
}

/// Extract Dart function/method calls from expression_statement nodes.
/// Dart calls: identifier + selector(argument_part) = simple call
/// identifier + selector(unconditional_assignable_selector(identifier)) + selector(argument_part) = method call
fn extract_dart_calls(
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
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_call_relations() {
        let code = r#"
function handleLogin(req) {
    const user = validateToken(req.token);
    sendResponse(req, user);
}
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let calls: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(calls.contains(&"validateToken"), "got calls: {:?}", calls);
        assert!(calls.contains(&"sendResponse"), "got calls: {:?}", calls);
    }

    #[test]
    fn test_extract_import_relations() {
        let code = r#"
import { UserService } from './services/user';
import jwt from 'jsonwebtoken';
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let imports: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_IMPORTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(imports.contains(&"UserService"), "got imports: {:?}", imports);
    }

    #[test]
    fn test_extract_inherits_relations() {
        let code = r#"
class AdminService extends UserService {
    getPermissions() { return []; }
}
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let inherits: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_INHERITS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(inherits.contains(&"UserService"), "got inherits: {:?}", inherits);
    }

    #[test]
    fn test_extract_express_routes() {
        let code = r#"
app.post('/api/login', handleLogin);
app.get('/api/users/:id', getUser);
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let routes: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
            .collect();
        assert!(routes.iter().any(|(meta, target)| meta.contains("/api/login") && *target == "handleLogin"),
            "got routes: {:?}", routes);
    }

    #[test]
    fn test_extract_express_inline_arrow_routes() {
        let code = r#"
router.post('/api/login', async (req, res) => {
    const valid = validateCredentials(req.body.email);
    res.json({ token: 'ok' });
});
router.get('/api/users/:id', authMiddleware, async (req, res) => {
    res.json(user);
});
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let routes: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
            .collect();
        assert!(routes.iter().any(|(meta, _target)| meta.contains("/api/login") && meta.contains("\"inline\":true")),
            "should detect inline arrow handler route, got: {:?}", routes);
        assert!(routes.iter().any(|(meta, _target)| meta.contains("/api/users/:id")),
            "should detect multi-arg inline route, got: {:?}", routes);
    }

    #[test]
    fn test_extract_python_flask_routes() {
        let code = r#"
@app.route('/api/users', methods=['GET'])
def get_users():
    return jsonify(users)
"#;
        let relations = extract_relations(code, "python").unwrap();
        let routes: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(routes.contains(&"get_users"), "got routes: {:?}", routes);
    }

    // --- Task 2: Java inheritance ---

    #[test]
    fn test_extract_java_inheritance() {
        let code = "public class Dog extends Animal {\n    public void bark() {}\n}\n";
        let relations = extract_relations(code, "java").unwrap();
        let inherits: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_INHERITS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(inherits.contains(&"Animal"), "got: {:?}", inherits);
    }

    // --- Task 3: Python imports ---

    #[test]
    fn test_extract_python_import() {
        let code = "import os\n";
        let relations = extract_relations(code, "python").unwrap();
        let imports: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_IMPORTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(imports.contains(&"os"), "got: {:?}", imports);
    }

    #[test]
    fn test_extract_python_from_import() {
        let code = "from collections import OrderedDict, defaultdict\n";
        let relations = extract_relations(code, "python").unwrap();
        let imports: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_IMPORTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(imports.contains(&"OrderedDict"), "got: {:?}", imports);
        assert!(imports.contains(&"defaultdict"), "got: {:?}", imports);
    }

    // --- Task 4: Python class inheritance ---

    #[test]
    fn test_extract_python_inheritance() {
        let code = "class Dog(Animal):\n    def bark(self):\n        pass\n";
        let relations = extract_relations(code, "python").unwrap();
        let inherits: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_INHERITS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(inherits.contains(&"Animal"), "got: {:?}", inherits);
    }

    #[test]
    fn test_extract_rust_use_imports() {
        let source = r#"
use std::collections::HashMap;
use anyhow::Result;

fn main() {
    let m: HashMap<String, String> = HashMap::new();
}
"#;
        let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
        let relations = extract_relations_from_tree(&tree, source, "rust");
        let imports: Vec<&ParsedRelation> = relations.iter().filter(|r| r.relation == REL_IMPORTS).collect();
        assert!(imports.iter().any(|r| r.target_name == "HashMap"), "should import HashMap, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
        assert!(imports.iter().any(|r| r.target_name == "Result"), "should import Result, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
    }

    #[test]
    fn test_extract_go_import_relations() {
        let source = r#"
package main

import (
    "fmt"
    "net/http"
)

func main() {
    fmt.Println("hello")
}
"#;
        let tree = crate::parser::treesitter::parse_tree(source, "go").unwrap();
        let relations = extract_relations_from_tree(&tree, source, "go");
        let imports: Vec<&ParsedRelation> = relations.iter().filter(|r| r.relation == REL_IMPORTS).collect();
        assert!(imports.iter().any(|r| r.target_name == "fmt"), "should import fmt, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
        assert!(imports.iter().any(|r| r.target_name == "http"), "should import http, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
    }

    #[test]
    fn test_extract_rust_grouped_use_imports() {
        let source = r#"
use std::collections::{HashMap, HashSet, BTreeMap};
use std::io::Read as _;

fn main() {}
"#;
        let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
        let relations = extract_relations_from_tree(&tree, source, "rust");
        let imports: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_IMPORTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(imports.contains(&"HashMap"), "should import HashMap, got: {:?}", imports);
        assert!(imports.contains(&"HashSet"), "should import HashSet, got: {:?}", imports);
        assert!(imports.contains(&"BTreeMap"), "should import BTreeMap, got: {:?}", imports);
        assert!(imports.contains(&"Read"), "should import Read (not 'Read as _'), got: {:?}", imports);
        // Should NOT contain braces or 'as _'
        assert!(!imports.iter().any(|i| i.contains('{')), "should not have brace in import names: {:?}", imports);
    }

    #[test]
    fn test_python_route_no_false_positive_on_cache_get() {
        // @cache.get should NOT be detected as a route (cache is not a known framework receiver)
        let code = r#"
@cache.get('/dashboard')
def get_dashboard():
    return render_template('dashboard.html')
"#;
        let relations = extract_relations(code, "python").unwrap();
        let routes: Vec<&ParsedRelation> = relations.iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .collect();
        assert!(routes.is_empty(), "should not detect route from @cache.get, got: {:?}",
            routes.iter().map(|r| (&r.source_name, &r.target_name)).collect::<Vec<_>>());
    }

    #[test]
    fn test_python_route_no_false_positive_on_getter() {
        // A decorator containing "get" as substring (e.g., @target) should NOT be detected as a route
        let code = r#"
@cache_target('/dashboard')
def get_dashboard():
    return render_template('dashboard.html')
"#;
        let relations = extract_relations(code, "python").unwrap();
        let routes: Vec<&ParsedRelation> = relations.iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .collect();
        assert!(routes.is_empty(), "should not detect route from @login_required, got: {:?}", routes.iter().map(|r| (&r.source_name, &r.target_name)).collect::<Vec<_>>());
    }

    #[test]
    fn test_python_route_detects_dotted_pattern() {
        // @app.get('/path') should still be detected
        let code = r#"
@app.get('/api/items')
def list_items():
    return items
"#;
        let relations = extract_relations(code, "python").unwrap();
        let routes: Vec<&ParsedRelation> = relations.iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .collect();
        assert!(!routes.is_empty(), "should detect route from @app.get, got no routes");
        assert!(routes[0].target_name == "list_items", "target should be list_items");
    }

    #[test]
    fn test_extract_rust_impl_trait() {
        let source = r#"
struct MyStruct;
trait MyTrait { fn do_thing(&self); }
impl MyTrait for MyStruct {
    fn do_thing(&self) {}
}
"#;
        let relations = extract_relations(source, "rust").unwrap();
        let impls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_IMPLEMENTS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        assert!(impls.contains(&("MyStruct", "MyTrait")), "got implements: {:?}", impls);
    }

    #[test]
    fn test_bare_impl_no_implements_relation() {
        // `impl Type { ... }` (no trait) should produce zero REL_IMPLEMENTS relations
        let source = r#"
struct MyStruct;
impl MyStruct {
    fn new() -> Self { MyStruct }
    fn do_thing(&self) {}
}
"#;
        let relations = extract_relations(source, "rust").unwrap();
        let impls: Vec<_> = relations.iter()
            .filter(|r| r.relation == REL_IMPLEMENTS)
            .collect();
        assert!(impls.is_empty(), "bare impl should produce no implements relations, got: {:?}",
            impls.iter().map(|r| (&r.source_name, &r.target_name)).collect::<Vec<_>>());
    }

    #[test]
    fn test_extract_go_http_routes() {
        let code = r#"
package main

func main() {
    http.HandleFunc("/api/health", healthCheck)
}
"#;
        let relations = extract_relations(code, "go").unwrap();
        assert!(relations.iter().any(|r| r.relation == REL_ROUTES_TO && r.target_name == "healthCheck"),
            "got relations: {:?}", relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
    }

    #[test]
    fn test_extract_ts_implements() {
        let code = "class UserService implements IUserService {\n    getUser() { return null; }\n}\n";
        let relations = extract_relations(code, "typescript").unwrap();
        let impls: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_IMPLEMENTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(impls.contains(&"IUserService"), "got implements: {:?}", impls);
    }

    #[test]
    fn test_extract_java_implements() {
        let code = "public class ArrayList implements List, Serializable {\n}\n";
        let relations = extract_relations(code, "java").unwrap();
        let impls: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_IMPLEMENTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(impls.contains(&"List"), "got implements: {:?}", impls);
    }

    #[test]
    fn test_extract_ts_exports() {
        let code = "export function handleLogin(req: Request) {}\nexport class AuthService {}\n";
        let relations = extract_relations(code, "typescript").unwrap();
        let exports: Vec<&str> = relations.iter()
            .filter(|r| r.relation == REL_EXPORTS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(exports.contains(&"handleLogin"), "got exports: {:?}", exports);
        assert!(exports.contains(&"AuthService"), "got exports: {:?}", exports);
    }

    #[test]
    fn test_go_selector_call_relations() {
        // Go receiver.Method() calls should be extracted
        let code = r#"
package main

import "fmt"

func main() {
    fmt.Println("hello")
    http.HandleFunc("/", handler)
}
"#;
        let relations = extract_relations(code, "go").unwrap();
        let calls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        assert!(calls.contains(&("main", "Println")),
            "fmt.Println() should create call relation, got: {:?}", calls);
        assert!(calls.contains(&("main", "HandleFunc")),
            "http.HandleFunc() should create call relation, got: {:?}", calls);
    }

    #[test]
    fn test_rust_scoped_call_relations() {
        // Self::method() and Path::func() should be extracted as call relations
        let code = r#"
impl Database {
    fn open() {
        Self::open_impl(false);
    }
    fn open_impl(flag: bool) {
        HashMap::new();
    }
}
"#;
        let relations = extract_relations(code, "rust").unwrap();
        let calls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        assert!(calls.contains(&("open", "open_impl")),
            "Self::open_impl() should create call relation, got: {:?}", calls);
        assert!(calls.contains(&("open_impl", "new")),
            "HashMap::new() should create call relation, got: {:?}", calls);
    }

    #[test]
    fn test_rust_method_call_on_object() {
        // obj.method() should also be extracted as a call relation
        let code = r#"
fn test_func() {
    let server = McpServer::from_project_root(path).unwrap();
    server.handle_message(init).unwrap();
    tool_call_json("search", args);
}
"#;
        let relations = extract_relations(code, "rust").unwrap();
        eprintln!("All relations:");
        for r in &relations {
            eprintln!("  {} --[{}]--> {}", r.source_name, r.relation, r.target_name);
        }
        let calls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        eprintln!("Calls: {:?}", calls);
        assert!(calls.contains(&("test_func", "from_project_root")),
            "McpServer::from_project_root() should create call, got: {:?}", calls);
        assert!(calls.contains(&("test_func", "handle_message")),
            "server.handle_message() should create call, got: {:?}", calls);
        assert!(calls.contains(&("test_func", "tool_call_json")),
            "tool_call_json() should create call, got: {:?}", calls);
    }

    #[test]
    fn test_rust_try_expr_and_match_calls() {
        // Reproduce actual patterns from main.rs run_serve: try expressions, match scrutinee, method calls
        let code = r#"
fn run_serve() {
    let project_root = std::env::current_dir().unwrap();
    let server = code_graph_mcp::mcp::server::McpServer::from_project_root(&project_root).unwrap();
    server.set_notify_writer(Box::new(io::stdout()));
    match server.handle_message(&buf) {
        Ok(Some(response)) => {
            writeln!(stdout, "{}", response).unwrap();
            stdout.flush().unwrap();
        }
        Ok(None) => {}
        Err(e) => {}
    }
    server.run_startup_tasks();
    server.flush_metrics();
}
"#;
        let relations = extract_relations(code, "rust").unwrap();
        let calls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        assert!(calls.contains(&("run_serve", "from_project_root")),
            "McpServer::from_project_root() missing, got: {:?}", calls);
        assert!(calls.contains(&("run_serve", "set_notify_writer")),
            "server.set_notify_writer() missing, got: {:?}", calls);
        assert!(calls.contains(&("run_serve", "handle_message")),
            "server.handle_message() missing, got: {:?}", calls);
        assert!(calls.contains(&("run_serve", "run_startup_tasks")),
            "server.run_startup_tasks() missing, got: {:?}", calls);
        assert!(calls.contains(&("run_serve", "flush_metrics")),
            "server.flush_metrics() missing, got: {:?}", calls);
    }

    #[test]
    fn test_scope_qualification_class_method() {
        // Methods inside a class should have scope qualified as ClassName.method_name
        let code = r#"
class UserService {
    getUser(id) {
        return this.db.findById(id);
    }
    deleteUser(id) {
        this.getUser(id);
        this.db.remove(id);
    }
}
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let calls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        // The scope for getUser should be "UserService.getUser", not just "getUser"
        assert!(calls.iter().any(|(src, tgt)| *src == "UserService.getUser" && *tgt == "findById"),
            "getUser scope should be qualified as UserService.getUser, got calls: {:?}", calls);
        assert!(calls.iter().any(|(src, tgt)| *src == "UserService.deleteUser" && *tgt == "getUser"),
            "deleteUser scope should be qualified as UserService.deleteUser, got calls: {:?}", calls);
    }

    #[test]
    fn test_scope_standalone_function_not_qualified() {
        // Standalone functions (not inside a class) should NOT be qualified with a class prefix
        let code = r#"
function doWork() {
    process();
}
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let calls: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect();
        assert!(calls.iter().any(|(src, tgt)| *src == "doWork" && *tgt == "process"),
            "standalone function scope should remain unqualified, got calls: {:?}", calls);
    }
}
