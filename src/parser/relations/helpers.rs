//! Generic AST helpers shared by language-specific extractors:
//! callee-name extraction across multiple call-expression shapes,
//! depth-bounded string-literal lookup inside an arbitrary subtree.

use super::super::node_text;

pub(super) const MAX_SUBTREE_DEPTH: usize = 32;

pub(super) fn extract_callee_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
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

pub(super) fn extract_string_from_subtree(node: &tree_sitter::Node, source: &str) -> Option<String> {
    extract_string_from_subtree_inner(node, source, 0)
}

fn extract_string_from_subtree_inner(node: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
    if depth > MAX_SUBTREE_DEPTH { return None; }
    if node.kind() == "string" {
        let text = node_text(node, source);
        let text = text.trim_start_matches(['f', 'r', 'b', 'u', 'F', 'R', 'B', 'U']);
        return Some(text.trim_matches(|c| c == '\'' || c == '"').to_string());
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(s) = extract_string_from_subtree_inner(&child, source, depth + 1) {
                return Some(s);
            }
        }
    }
    None
}
