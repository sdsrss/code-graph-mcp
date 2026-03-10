use anyhow::{anyhow, Result};
use super::languages::get_language;

pub struct ParsedRelation {
    pub source_name: String,
    pub target_name: String,
    pub relation: String,
    pub metadata: Option<String>,
}

pub fn extract_relations(source: &str, language: &str) -> Result<Vec<ParsedRelation>> {
    let lang = get_language(language)
        .ok_or_else(|| anyhow!("unsupported language: {}", language))?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang)?;
    let tree = parser.parse(source, None)
        .ok_or_else(|| anyhow!("parse failed"))?;

    let mut relations = Vec::new();
    walk_for_relations(tree.root_node(), source, None, &mut relations);
    Ok(relations)
}

fn walk_for_relations(
    node: tree_sitter::Node,
    source: &str,
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
        _ => None,
    };

    let active_scope = scope_name.as_deref().or(current_scope);

    match kind {
        // Call expressions
        "call_expression" => {
            if let Some(scope) = active_scope {
                if let Some(callee) = extract_callee_name(&node, source) {
                    results.push(ParsedRelation {
                        source_name: scope.to_string(),
                        target_name: callee,
                        relation: "calls".into(),
                        metadata: None,
                    });
                }
            }
        }

        // Import statements (TypeScript/JavaScript)
        "import_statement" => {
            extract_import_names(&node, source, results);
        }

        // Python imports
        "import_from_statement" => {
            extract_import_names(&node, source, results);
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
                        relation: "inherits".into(),
                        metadata: None,
                    });
                }
            }
        }

        _ => {}
    }

    // Recurse into children
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_for_relations(child, source, active_scope, results);
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
        _ => Some(node_text(&function, source).to_string()),
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
                            relation: "imports".into(),
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
                relation: "imports".into(),
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
                relation: "imports".into(),
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

fn extract_superclass(node: &tree_sitter::Node, source: &str) -> Option<String> {
    // Look for "extends" clause / superclass
    for i in 0..node.named_child_count() {
        let child = node.named_child(i)?;
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
            "superclass" => {
                // Python: class Foo(Bar)
                return Some(node_text(&child, source).to_string());
            }
            _ => {}
        }
    }
    None
}

fn node_text<'a>(node: &tree_sitter::Node, source: &'a str) -> &'a str {
    &source[node.byte_range()]
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
            .filter(|r| r.relation == "calls")
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
            .filter(|r| r.relation == "imports")
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
            .filter(|r| r.relation == "inherits")
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(inherits.contains(&"UserService"), "got inherits: {:?}", inherits);
    }
}
