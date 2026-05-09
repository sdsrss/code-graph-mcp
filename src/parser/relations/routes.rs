//! HTTP route registration extraction for Express/Connect (TS/JS/TSX),
//! Go net/http, and Python Flask/FastAPI. Each framework matches a
//! distinct AST shape so they keep separate per-language entry points;
//! `extract_route_pattern` is the call_expression dispatcher used by the
//! main walker, while `extract_python_route` is a decorator-driven
//! standalone path called when the walker hits `decorated_definition`.

use super::ParsedRelation;
use super::super::node_text;
use super::helpers::extract_string_from_subtree;
use crate::domain::REL_ROUTES_TO;

pub(super) fn extract_route_pattern(node: &tree_sitter::Node, source: &str, language: &str) -> Option<ParsedRelation> {
    match language {
        "typescript" | "javascript" | "tsx" => extract_express_route(node, source),
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
            source_language: String::new(),
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
            source_language: String::new(),
        })
    } else {
        None
    }
}

fn extract_go_route(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "selector_expression" { return None; }

    let field = function.child_by_field_name("field")?;
    let func_name = node_text(&field, source);
    // Match HandleFunc/Handle on any receiver: http.HandleFunc, mux.HandleFunc, router.Handle, etc.
    if !matches!(func_name, "HandleFunc" | "Handle") { return None; }

    let args = node.child_by_field_name("arguments")?;
    let path_arg = args.named_child(0)?;
    let path = node_text(&path_arg, source).trim_matches('"').to_string();

    let handler_arg = args.named_child(1)?;
    // For selector expressions like handler.GetUser, extract just the method name
    let handler = if handler_arg.kind() == "selector_expression" {
        handler_arg.child_by_field_name("field")
            .map(|f| node_text(&f, source).to_string())
            .unwrap_or_else(|| node_text(&handler_arg, source).to_string())
    } else {
        node_text(&handler_arg, source).to_string()
    };

    let metadata = serde_json::json!({"method": "ALL", "path": path}).to_string();

    Some(ParsedRelation {
        source_name: handler.clone(),
        target_name: handler,
        relation: REL_ROUTES_TO.into(),
        metadata: Some(metadata),
        source_language: String::new(),
    })
}

pub(super) fn extract_python_route(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    // Look for decorator that matches @app.route(...) or @app.get(...) etc.
    // Iterate all decorators and match the first route-like one (not the last),
    // since route decorators may appear before auth/middleware decorators.
    let mut matched_decorator = None;
    let mut func_def = None;

    let known_receivers = ["app.", "bp.", "blueprint.", "router.", "api."];
    let route_methods = [".route(", ".get(", ".post(", ".put(", ".delete(", ".patch("];

    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "decorator" if matched_decorator.is_none() => {
                    let dec_text = node_text(&child, source);
                    let has_receiver = known_receivers.iter().any(|p| dec_text.contains(p));
                    let has_method = route_methods.iter().any(|m| dec_text.contains(m));
                    if has_receiver && has_method {
                        matched_decorator = Some(child);
                    }
                }
                "function_definition" => func_def = Some(child),
                _ => {}
            }
        }
    }

    let dec = matched_decorator?;
    let func = func_def?;
    let func_name_node = func.child_by_field_name("name")?;
    let func_name = node_text(&func_name_node, source);

    // Get the decorator expression text
    let dec_text = node_text(&dec, source);

    // Check for route-like decorator patterns (e.g., @app.route, @app.get, @bp.post)
    // Only match known framework receiver names to avoid false positives (e.g., @cache.get)
    // Route validation already done during decorator selection above

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
        source_language: String::new(),
    })
}
