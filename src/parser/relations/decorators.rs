//! TypeScript/JavaScript decorator extraction.
//!
//! Emits `references` edges from the DECORATED symbol to the DECORATOR name,
//! making `@Tool(...)`, `@Route(...)`, `@Injectable()` etc. visible in the
//! call graph, find_references, and dead-code analysis.
//!
//! Tree-sitter-typescript parses decorators as:
//! ```text
//! decorator
//!   └─ call_expression
//!        ├─ identifier "Tool"                  ← bare decorator
//!        └─ arguments (...)
//! decorator
//!   └─ call_expression
//!        ├─ member_expression                  ← dotted decorator
//!        │    ├─ identifier "app"
//!        │    └─ property_identifier "get"
//!        └─ arguments (...)
//! decorator
//!   └─ identifier "sealed"                     ← no-call decorator
//! ```
//!
//! We extract the decorator name and emit a `references` edge from the
//! decorated symbol (class/method/function) to the decorator name. This
//! ensures that:
//! - `find_references Tool` shows every `@Tool(...)` site
//! - `find_dead_code` doesn't flag `Tool` as unused
//! - `get_call_graph` for a class shows its decorator dependencies

use super::ParsedRelation;
use super::super::node_text;
use crate::domain::REL_REFERENCES;

/// Metadata key for decorator argument extraction.
const DECORATOR_META_KEY: &str = "decorator";

/// Extract a `references` edge from a `decorator` node.
///
/// `scope` is the class/function name that the decorator is applied to (the
/// decorated symbol). When the decorated symbol isn't known yet (decorator
/// appears before the class/function in the AST), caller should pass None
/// and we'll use `<module>`.
///
/// Returns the decorator name as the target and optionally extracts the first
/// string argument (for `@Route('GET', '/api/users')` → metadata `GET /api/users`).
pub(super) fn extract_ts_decorator(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Vec<ParsedRelation> {
    let mut results = Vec::new();

    // The decorator node's first named child is either:
    // - call_expression (decorator with args: @Tool(...))
    // - identifier (decorator without args: @sealed)
    // - member_expression (dotted without args: @app.route — rare without parens)
    let first_child = match node.named_child(0) {
        Some(c) => c,
        None => return results,
    };

    let (decorator_name, metadata) = match first_child.kind() {
        "call_expression" => {
            // @Tool({ name: 'read_file' }) or @Route('GET', '/api/users')
            let callee = first_child.child_by_field_name("function")
                .or_else(|| first_child.named_child(0));
            let name = match callee {
                Some(c) => extract_decorator_name(&c, source),
                None => return results,
            };
            let meta = extract_decorator_args(&first_child, source);
            (name, meta)
        }
        "identifier" => {
            // @sealed (no call expression)
            let name = node_text(&first_child, source).to_string();
            (name, None)
        }
        "member_expression" => {
            // @app.route (no parens — unusual but valid)
            let name = extract_member_expression_name(&first_child, source);
            (name, None)
        }
        _ => return results,
    };

    if decorator_name.is_empty() {
        return results;
    }

    let source_name = scope.unwrap_or("<module>").to_string();

    results.push(ParsedRelation {
        source_name,
        target_name: decorator_name,
        relation: REL_REFERENCES.into(),
        metadata: metadata.map(|m| format!(r#"{{"{}":"{}"}}"#, DECORATOR_META_KEY, m)),
        source_language: String::new(),
    });

    results
}

/// Extract the decorator name from the callee node.
/// For `identifier "Tool"` → "Tool".
/// For `member_expression` → "app.route" (dotted form).
fn extract_decorator_name(node: &tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "member_expression" => extract_member_expression_name(node, source),
        _ => String::new(),
    }
}

/// Extract a dotted name from a member_expression: `app.route` → "app.route".
fn extract_member_expression_name(node: &tree_sitter::Node, source: &str) -> String {
    let object = node.child_by_field_name("object");
    let property = node.child_by_field_name("property");
    match (object, property) {
        (Some(obj), Some(prop)) => {
            let obj_text = node_text(&obj, source);
            let prop_text = node_text(&prop, source);
            format!("{}.{}", obj_text, prop_text)
        }
        _ => node_text(node, source).to_string(),
    }
}

/// Extract decorator arguments as a metadata string.
/// For `@Route('GET', '/api/users')` → Some("GET /api/users").
/// For `@Tool({ name: 'read_file' })` → Some("name=read_file").
/// For `@Injectable()` → None (empty args).
fn extract_decorator_args(call_expr: &tree_sitter::Node, source: &str) -> Option<String> {
    let args = call_expr.child_by_field_name("arguments")?;

    // Collect string arguments
    let mut strings = Vec::new();
    // Collect key-value pairs from object literals
    let mut pairs = Vec::new();

    for i in 0..args.named_child_count() {
        let child = match args.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "string" => {
                // Extract the string content (skip quotes)
                if let Some(frag) = child.named_child(0) {
                    strings.push(node_text(&frag, source).to_string());
                }
            }
            "object" => {
                // Extract key-value pairs from { name: 'read_file', ... }
                for j in 0..child.named_child_count() {
                    if let Some(pair) = child.named_child(j) {
                        if pair.kind() == "pair" {
                            let key = pair.child_by_field_name("key")
                                .map(|k| node_text(&k, source).to_string());
                            let value = pair.child_by_field_name("value")
                                .and_then(|v| {
                                    if v.kind() == "string" {
                                        v.named_child(0).map(|f| node_text(&f, source).to_string())
                                    } else {
                                        Some(node_text(&v, source).to_string())
                                    }
                                });
                            if let (Some(k), Some(v)) = (key, value) {
                                pairs.push(format!("{}={}", k, v));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if strings.is_empty() && pairs.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if !strings.is_empty() {
        parts.push(strings.join(" "));
    }
    if !pairs.is_empty() {
        parts.push(pairs.join(","));
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_and_find_decorators(code: &str) -> Vec<ParsedRelation> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();
        let mut results = Vec::new();
        collect_decorators(&root, code, None, &mut results);
        results
    }

    fn collect_decorators(
        node: &tree_sitter::Node,
        source: &str,
        scope: Option<&str>,
        results: &mut Vec<ParsedRelation>,
    ) {
        if node.kind() == "decorator" {
            results.extend(extract_ts_decorator(node, source, scope));
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                collect_decorators(&child, source, scope, results);
            }
        }
    }

    #[test]
    fn test_bare_decorator_with_call() {
        let code = r#"
@Injectable()
class MyService {}
"#;
        let rels = parse_and_find_decorators(code);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].target_name, "Injectable");
        assert_eq!(rels[0].relation, REL_REFERENCES);
        assert!(rels[0].metadata.is_none()); // empty args
    }

    #[test]
    fn test_decorator_with_object_arg() {
        let code = r#"
@Tool({ name: 'read_file', description: 'Reads a file' })
async function readFile() {}
"#;
        let rels = parse_and_find_decorators(code);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].target_name, "Tool");
        assert!(rels[0].metadata.as_ref().unwrap().contains("name=read_file"));
    }

    #[test]
    fn test_decorator_with_string_args() {
        let code = r#"
@Route('GET', '/api/users')
function getUsers() {}
"#;
        let rels = parse_and_find_decorators(code);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].target_name, "Route");
        let meta = rels[0].metadata.as_ref().unwrap();
        assert!(meta.contains("GET /api/users"));
    }

    #[test]
    fn test_dotted_decorator() {
        let code = r#"
@app.get('/users')
function getUsers() {}
"#;
        let rels = parse_and_find_decorators(code);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].target_name, "app.get");
    }

    #[test]
    fn test_method_decorator() {
        let code = r#"
class Controller {
  @Get('/items')
  getItems() {}

  @Post('/items')
  createItem() {}
}
"#;
        let rels = parse_and_find_decorators(code);
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].target_name, "Get");
        assert_eq!(rels[1].target_name, "Post");
    }

    #[test]
    fn test_no_call_decorator() {
        let code = r#"
@sealed
class MyClass {}
"#;
        let rels = parse_and_find_decorators(code);
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].target_name, "sealed");
        assert!(rels[0].metadata.is_none());
    }
}
