use anyhow::Result;
use super::node_text;
use crate::storage::schema::{REL_CALLS, REL_INHERITS, REL_IMPORTS, REL_ROUTES_TO, REL_IMPLEMENTS, REL_EXPORTS};

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
    walk_for_relations(tree.root_node(), source, language, None, &mut relations);
    relations
}

fn walk_for_relations(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    current_scope: Option<&str>,
    results: &mut Vec<ParsedRelation>,
) {
    let kind = node.kind();

    // Determine if this node creates a new scope
    let scope_name = match kind {
        "function_declaration" | "function_definition" | "function_item"
        | "method_definition" | "method_declaration" => {
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string())
        }
        "arrow_function" => {
            // Try to get name from parent variable_declarator: const foo = () => {}
            node.parent()
                .filter(|p| p.kind() == "variable_declarator")
                .and_then(|p| p.child_by_field_name("name"))
                .map(|n| node_text(&n, source).to_string())
                .or_else(|| Some("<anonymous>".to_string()))
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

        // Import statements
        "import_statement" => {
            if language == "python" {
                extract_python_import_names(&node, source, results);
            } else {
                extract_import_names(&node, source, results);
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
                // Check for extends/superclass
                if let Some(parent) = extract_superclass(&node, source) {
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

        // Rust: use std::collections::HashMap;
        // Also handles grouped imports: use std::collections::{HashMap, HashSet};
        "use_declaration" => {
            let text = node_text(&node, source).trim().trim_end_matches(';').to_string();
            if let Some(name) = text.rsplit("::").next() {
                // Skip glob imports like `use foo::*`
                if name != "*" && !name.is_empty() {
                    let clean_name = name.trim();
                    // Handle grouped imports: {HashMap, HashSet}
                    if clean_name.starts_with('{') && clean_name.ends_with('}') {
                        let inner = &clean_name[1..clean_name.len() - 1];
                        for item in inner.split(',') {
                            let item = item.trim();
                            // Handle aliased imports: HashMap as HM -> extract HashMap
                            let actual_name = item.split_whitespace().next().unwrap_or("");
                            if !actual_name.is_empty() && actual_name != "*" {
                                results.push(ParsedRelation {
                                    source_name: active_scope.unwrap_or("<module>").to_string(),
                                    target_name: actual_name.to_string(),
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                });
                            }
                        }
                    } else {
                        // Handle aliased imports: Read as _ -> extract Read
                        let actual_name = clean_name.split_whitespace().next().unwrap_or(clean_name);
                        if !actual_name.is_empty() {
                            results.push(ParsedRelation {
                                source_name: active_scope.unwrap_or("<module>").to_string(),
                                target_name: actual_name.to_string(),
                                relation: REL_IMPORTS.into(),
                                metadata: None,
                            });
                        }
                    }
                }
            }
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

        _ => {}
    }

    // Recurse into children
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_for_relations(child, source, language, active_scope, results);
        }
    }
}

fn extract_callee_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let function = node.child_by_field_name("function")
        .or_else(|| node.named_child(0))?;

    match function.kind() {
        "identifier" => Some(node_text(&function, source).to_string()),
        "member_expression" | "field_expression" => {
            // e.g., obj.method — extract "method" or "obj.method"
            if let Some(prop) = function.child_by_field_name("property")
                .or_else(|| function.child_by_field_name("field")) {
                Some(node_text(&prop, source).to_string())
            } else {
                Some(node_text(&function, source).to_string())
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
fn extract_python_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if child.kind() == "dotted_name" || child.kind() == "identifier" {
                let name = node_text(&child, source).to_string();
                if !name.is_empty() {
                    results.push(ParsedRelation {
                        source_name: "<module>".into(),
                        target_name: name,
                        relation: REL_IMPORTS.into(),
                        metadata: None,
                    });
                }
            } else if child.kind() == "aliased_import" {
                // import os as operating_system — extract the original module name
                if let Some(module) = child.named_child(0) {
                    let name = node_text(&module, source).to_string();
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

/// Extract imports from Python `from X import Y, Z` statements.
/// AST: import_from_statement -> dotted_name ("collections"), dotted_name ("OrderedDict"), dotted_name ("defaultdict")
/// The first dotted_name is the module; the rest are imported names.
fn extract_python_from_import_names(node: &tree_sitter::Node, source: &str, results: &mut Vec<ParsedRelation>) {
    let mut is_first_dotted_name = true;
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "dotted_name" => {
                    if is_first_dotted_name {
                        // First dotted_name is the module name (e.g., "collections") — skip it
                        is_first_dotted_name = false;
                    } else {
                        // Subsequent dotted_names are imported symbols
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
                "aliased_import" => {
                    // from X import Y as Z — extract Y (the original name)
                    if let Some(original) = child.named_child(0) {
                        let name = node_text(&original, source).to_string();
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
                "wildcard_import" => {
                    // from X import * — record as wildcard
                    results.push(ParsedRelation {
                        source_name: "<module>".into(),
                        target_name: "*".into(),
                        relation: REL_IMPORTS.into(),
                        metadata: None,
                    });
                }
                _ => {}
            }
        }
    }
}

fn extract_superclass(node: &tree_sitter::Node, source: &str) -> Option<String> {
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
                                        return Some(node_text(&type_node, source).to_string());
                                    }
                                }
                            }
                        }
                        if inner.kind() == "identifier" || inner.kind() == "type_identifier" {
                            return Some(node_text(&inner, source).to_string());
                        }
                    }
                }
            }
            "argument_list" => {
                // Python: class Dog(Animal) — the parent class list is an argument_list node,
                // containing identifier or dotted_name children for each superclass.
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "identifier" || inner.kind() == "dotted_name" {
                            return Some(node_text(&inner, source).to_string());
                        }
                    }
                }
            }
            "superclass" => {
                // Java: superclass -> type_identifier
                // Also check identifier and dotted_name as fallbacks.
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "type_identifier" || inner.kind() == "identifier" || inner.kind() == "dotted_name" {
                            return Some(node_text(&inner, source).to_string());
                        }
                    }
                }
                let text = node_text(&child, source);
                return Some(text.trim_start_matches('(').trim_end_matches(')').trim().to_string());
            }
            _ => {}
        }
    }
    None
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

    // Last named argument is the handler — only use if it's an identifier
    let handler_count = args.named_child_count();
    if handler_count < 2 { return None; }
    let handler_arg = args.named_child(handler_count - 1)?;
    // Skip inline functions (arrow_function, function) — they don't resolve to graph nodes
    if handler_arg.kind() != "identifier" { return None; }
    let handler_name = node_text(&handler_arg, source).to_string();

    let metadata = serde_json::json!({"method": http_method, "path": path}).to_string();

    Some(ParsedRelation {
        source_name: handler_name.clone(),
        target_name: handler_name,
        relation: REL_ROUTES_TO.into(),
        metadata: Some(metadata),
    })
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
}
