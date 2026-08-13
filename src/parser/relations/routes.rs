//! HTTP route registration extraction for Express/Connect (TS/JS/TSX),
//! Go net/http, Python Flask/FastAPI, and Rust axum. Each framework matches a
//! distinct AST shape so they keep separate per-language entry points;
//! `extract_route_pattern` is the call_expression dispatcher used by the
//! main walker, while `extract_python_route` is a decorator-driven
//! standalone path called when the walker hits `decorated_definition`.
//!
//! Returns a Vec because one registration call can register several routes:
//! axum's `.route("/u", get(a).post(b))` is ONE call carrying one edge per
//! (method, handler) pair. Express/Go/Python stay single-edge per call.

use super::super::node_text;
use super::helpers::extract_string_from_subtree;
use super::ParsedRelation;
use crate::domain::REL_ROUTES_TO;

pub(super) fn extract_route_pattern(
    node: &tree_sitter::Node,
    source: &str,
    language: &str,
) -> Vec<ParsedRelation> {
    match language {
        "typescript" | "javascript" | "tsx" => {
            extract_express_route(node, source).into_iter().collect()
        }
        "go" => extract_go_route(node, source).into_iter().collect(),
        "rust" => extract_axum_routes(node, source),
        _ => Vec::new(),
    }
}

/// axum method-router constructor names (`axum::routing::get` etc.). `trace` is
/// a real axum verb (HTTP TRACE); `any` maps to method "ALL" like Go's Handle.
const AXUM_METHOD_FNS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "trace", "any",
];

/// Rust axum builder-chain extraction (roadmap 2026-07-18 §2.1). Fires ONLY on
/// `.route(path_literal, <method-router expr>)` call links, so bare `.get(...)`
/// calls (reqwest clients, HashMap::get) can never fabricate a route. One edge
/// per (method, handler) pair found in the second argument's method-router
/// chain (`get(a).post(b)`), self-edge shape identical to Express/Go so the
/// existing same-file + cross-file routes_to resolution applies unchanged.
///
/// Scope (documented non-goals, same MVP spirit as the Express extractor):
/// - inline closures as handlers are skipped (no synthetic-node materializer
///   on the Rust side yet); named + path-qualified handlers resolve.
/// - `.nest("/prefix", <inline subtree>)` composes prefixes by ancestor walk;
///   a nest whose child router is built in ANOTHER statement/variable is not
///   composed (needs dataflow, not parse-time AST). `.merge` needs no
///   composition — merged routers keep their own paths.
/// - actix/rocket/warp are separate future arms, not silently claimed.
fn extract_axum_routes(node: &tree_sitter::Node, source: &str) -> Vec<ParsedRelation> {
    let Some(function) = node.child_by_field_name("function") else {
        return Vec::new();
    };
    if function.kind() != "field_expression" {
        return Vec::new();
    }
    let Some(field) = function.child_by_field_name("field") else {
        return Vec::new();
    };
    if node_text(&field, source) != "route" {
        return Vec::new();
    }

    let Some(args) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let Some(path_arg) = args.named_child(0) else {
        return Vec::new();
    };
    if path_arg.kind() != "string_literal" {
        return Vec::new();
    }
    let path = node_text(&path_arg, source).trim_matches('"').to_string();
    let Some(router_arg) = args.named_child(1) else {
        return Vec::new();
    };

    let full_path = format!("{}{}", compose_axum_nest_prefix(node, source), path);

    let mut pairs: Vec<(String, String)> = Vec::new();
    collect_axum_method_handlers(&router_arg, source, &mut pairs);

    pairs
        .into_iter()
        .map(|(method, handler)| {
            let metadata = serde_json::json!({"method": method, "path": full_path}).to_string();
            ParsedRelation {
                source_name: handler.clone(),
                target_name: handler,
                relation: REL_ROUTES_TO.into(),
                metadata: Some(metadata),
                source_language: String::new(),
            }
        })
        .collect()
}

/// Walk a method-router chain (`get(a)`, `axum::routing::get(a)`,
/// `get(a).post(b)`) collecting (HTTP_METHOD, handler_name) pairs. Non-method
/// chain links (`.route_layer`, `.layer`, `.with_state`) are skipped but the
/// walk continues down their receiver, so `get(a).route_layer(x)` still finds a.
fn collect_axum_method_handlers(
    expr: &tree_sitter::Node,
    source: &str,
    pairs: &mut Vec<(String, String)>,
) {
    if expr.kind() != "call_expression" {
        return;
    }
    let Some(function) = expr.child_by_field_name("function") else {
        return;
    };
    match function.kind() {
        "identifier" | "scoped_identifier" => {
            let name = node_text(&function, source);
            let last = name.rsplit("::").next().unwrap_or(name);
            if AXUM_METHOD_FNS.contains(&last) {
                if let Some(handler) = axum_handler_name(expr, source) {
                    pairs.push((axum_method_label(last), handler));
                }
            }
        }
        "field_expression" => {
            if let Some(field) = function.child_by_field_name("field") {
                let fname = node_text(&field, source);
                if AXUM_METHOD_FNS.contains(&fname) {
                    if let Some(handler) = axum_handler_name(expr, source) {
                        pairs.push((axum_method_label(fname), handler));
                    }
                }
            }
            if let Some(value) = function.child_by_field_name("value") {
                collect_axum_method_handlers(&value, source, pairs);
            }
        }
        _ => {}
    }
}

fn axum_method_label(method_fn: &str) -> String {
    if method_fn == "any" {
        "ALL".into()
    } else {
        method_fn.to_uppercase()
    }
}

/// First named argument of a method-router call, as a resolvable handler name:
/// `get(list_users)` → list_users; `get(handlers::list_users)` → list_users
/// (last path segment — matches how the handler fn node is named). Closures /
/// other expressions → None (skipped, per the extractor's documented scope).
fn axum_handler_name(call: &tree_sitter::Node, source: &str) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let arg = args.named_child(0)?;
    match arg.kind() {
        "identifier" => Some(node_text(&arg, source).to_string()),
        "scoped_identifier" => {
            let text = node_text(&arg, source);
            Some(text.rsplit("::").next().unwrap_or(text).to_string())
        }
        _ => None,
    }
}

/// Concatenated prefixes of every enclosing `.nest("<prefix>", …)` whose SECOND
/// argument subtree contains `node` (byte-range check — a `.nest` reached
/// through the receiver chain, `Router::new().route(…).nest(…)`, does NOT
/// prefix the routes registered before it). Outermost prefix first.
fn compose_axum_nest_prefix(node: &tree_sitter::Node, source: &str) -> String {
    let mut prefixes: Vec<String> = Vec::new();
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "call_expression" {
            let is_nest = p
                .child_by_field_name("function")
                .filter(|f| f.kind() == "field_expression")
                .and_then(|f| f.child_by_field_name("field"))
                .map(|f| node_text(&f, source) == "nest")
                .unwrap_or(false);
            if is_nest {
                if let Some(args) = p.child_by_field_name("arguments") {
                    let node_in_args = args.start_byte() <= node.start_byte()
                        && node.end_byte() <= args.end_byte();
                    if node_in_args {
                        if let Some(prefix_arg) = args.named_child(0) {
                            if prefix_arg.kind() == "string_literal" {
                                prefixes.push(
                                    node_text(&prefix_arg, source)
                                        .trim_matches('"')
                                        .trim_end_matches('/')
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
            }
        }
        cur = p.parent();
    }
    prefixes.reverse();
    prefixes.concat()
}

fn extract_express_route(node: &tree_sitter::Node, source: &str) -> Option<ParsedRelation> {
    // Recognize the route call + method/path via the shared helper — single
    // source of truth for the receiver objects + method map, kept in sync with
    // the handler-node materializer (crate::parser::route_handler_name).
    let (http_method, path) = crate::parser::express_route_method_path(node, source)?;

    let args = node.child_by_field_name("arguments")?;
    // Last named argument is the handler.
    let handler_count = args.named_child_count();
    if handler_count < 2 {
        return None;
    }
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
    } else if matches!(
        handler_arg.kind(),
        "arrow_function" | "function_expression" | "function"
    ) {
        // Inline handler: router.post('/path', async (req, res) => { ... }).
        // Point the edge at the synthetic handler node (materialized in
        // treesitter.rs) so trace/impact/overview resolve to the handler instead
        // of collapsing every route onto the file <module>. Keep the line
        // metadata for find_http_route. If the path isn't a concrete route the
        // name builder returns None and we fall back to the legacy <module>.
        let handler_start = handler_arg.start_position().row + 1;
        let handler_end = handler_arg.end_position().row + 1;
        let metadata = serde_json::json!({
            "method": http_method,
            "path": path,
            "inline": true,
            "handler_start_line": handler_start,
            "handler_end_line": handler_end,
        })
        .to_string();
        // Reuse route_handler_name (not synthetic_route_handler_name) so the
        // routes_to endpoint name is byte-identical to the materialized handler
        // node + its scoped calls — same node → same "METHOD path#Lstart",
        // including the per-occurrence line suffix that keeps duplicate same-route
        // handlers 1:1. Falls back to <module> when the path isn't concrete.
        let name = crate::parser::route_handler_name(&handler_arg, source)
            .unwrap_or_else(|| "<module>".into());
        Some(ParsedRelation {
            source_name: name.clone(),
            target_name: name,
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
    if function.kind() != "selector_expression" {
        return None;
    }

    let field = function.child_by_field_name("field")?;
    let func_name = node_text(&field, source);
    // Match HandleFunc/Handle on any receiver: http.HandleFunc, mux.HandleFunc, router.Handle, etc.
    if !matches!(func_name, "HandleFunc" | "Handle") {
        return None;
    }

    let args = node.child_by_field_name("arguments")?;
    let path_arg = args.named_child(0)?;
    let path = node_text(&path_arg, source).trim_matches('"').to_string();

    let handler_arg = args.named_child(1)?;
    // For selector expressions like handler.GetUser, extract just the method name
    let handler = if handler_arg.kind() == "selector_expression" {
        handler_arg
            .child_by_field_name("field")
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

pub(super) fn extract_python_route(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<ParsedRelation> {
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

    let method: String = if dec_text.contains(".get(") {
        "GET".into()
    } else if dec_text.contains(".post(") {
        "POST".into()
    } else if dec_text.contains(".put(") {
        "PUT".into()
    } else if dec_text.contains(".delete(") {
        "DELETE".into()
    } else if dec_text.contains(".patch(") {
        "PATCH".into()
    }
    // Flask `@app.route('/x', methods=['GET'])`: the decorator name is the
    // generic `.route`, so the verb lives in the `methods=` kwarg. Without
    // this, every Flask route was "ANY" and `trace 'GET /x'` (exact-method
    // filter) matched nothing.
    //
    // No `methods=` kwarg → GET, because that IS the framework default (Flask
    // and Starlette both default `methods` to `["GET"]`; HEAD and OPTIONS are
    // auto-derived from it). The earlier "ANY" wildcard fixed the false
    // negative by buying a false positive: `trace 'DELETE /x'` matched a route
    // that answers 405. HEAD/OPTIONS on a bare `@app.route` remain a knowingly
    // unmodelled approximation — the metadata schema holds one verb.
    else {
        parse_flask_methods_kwarg(dec_text).unwrap_or_else(|| "GET".into())
    };

    let metadata = serde_json::json!({"method": method, "path": path}).to_string();

    Some(ParsedRelation {
        source_name: func_name.to_string(),
        target_name: func_name.to_string(),
        relation: REL_ROUTES_TO.into(),
        metadata: Some(metadata),
        source_language: String::new(),
    })
}

/// Extract the first HTTP method from a Flask `methods=['GET', 'POST']` kwarg in
/// an `@app.route(...)` decorator. Returns None when no `methods=` kwarg is
/// present (caller falls back to Flask's own default, "GET"). The scan is
/// confined to the bracketed list literal, so a path segment like
/// `/get-methods` cannot be mistaken for a
/// verb; the failure mode is always None → "GET". The single-method metadata
/// schema stores the first listed method when several are given.
fn parse_flask_methods_kwarg(dec_text: &str) -> Option<String> {
    let after_kw = dec_text.split_once("methods").map(|(_, rest)| rest)?;
    let list_start = after_kw.find('[')?;
    let list = after_kw[list_start + 1..].split(']').next().unwrap_or("");
    const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
    let upper = list.to_uppercase();
    METHODS
        .iter()
        .filter_map(|m| upper.find(m).map(|pos| (pos, *m)))
        .min_by_key(|&(pos, _)| pos)
        .map(|(_, m)| m.to_string())
}
