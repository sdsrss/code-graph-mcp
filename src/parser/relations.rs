use anyhow::{anyhow, Result};
use super::languages::get_language;
use super::node_text;
use crate::storage::schema::{REL_CALLS, REL_INHERITS, REL_IMPORTS, REL_ROUTES_TO};

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
    walk_for_relations(tree.root_node(), source, language, None, &mut relations);
    Ok(relations)
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
                        relation: REL_INHERITS.into(),
                        metadata: None,
                    });
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
                // Python: class Foo(Bar) — navigate into child identifier if possible,
                // otherwise strip parentheses from the raw text.
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "identifier" || inner.kind() == "dotted_name" {
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

    // Match patterns like @app.route('/path') or @app.get('/path')
    if !dec_text.contains("route") && !dec_text.contains("get") && !dec_text.contains("post")
       && !dec_text.contains("put") && !dec_text.contains("delete") {
        return None;
    }

    // Extract path from decorator arguments
    let path = extract_string_from_subtree(&dec, source)?;

    let method = if dec_text.contains(".get(") || dec_text.contains(".get\"") { "GET" }
        else if dec_text.contains(".post(") { "POST" }
        else if dec_text.contains(".put(") { "PUT" }
        else if dec_text.contains(".delete(") { "DELETE" }
        else { "ALL" };

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

    #[test]
    fn test_extract_express_routes() {
        let code = r#"
app.post('/api/login', handleLogin);
app.get('/api/users/:id', getUser);
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let routes: Vec<(&str, &str)> = relations.iter()
            .filter(|r| r.relation == "routes_to")
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
            .filter(|r| r.relation == "routes_to")
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(routes.contains(&"get_users"), "got routes: {:?}", routes);
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
        assert!(relations.iter().any(|r| r.relation == "routes_to" && r.target_name == "healthCheck"),
            "got relations: {:?}", relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
    }
}
