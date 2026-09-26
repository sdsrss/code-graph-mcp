//! C / C++ value references: a function named by a bare `identifier` passed,
//! stored, or returned as a VALUE (a function pointer) — C's primary callback
//! mechanism. Positions: call argument (`qsort(a, n, s, compare)`), address-of
//! (`signal(2, &handler)`), designated / positional initializer (the vtable idiom
//! `struct ops o = { .read = my_read }`), init-declarator RHS (`fn_t cb = handler`),
//! assignment RHS (`ops->read = my_read`), and `return handler`.
//!
//! M2/M2.5 local exclusion is the hard part here: C declaration syntax is varied
//! (`int *x`, `void (*cb)(int)`, multi-declarator), so the declared NAME is pulled
//! from the declarator chain via `c_declared_name` — NEVER from an init `value`
//! field (that value IS the reference we want to keep). Without this a bare local
//! passed as an argument would fabricate an edge to a same-named global function.

use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use super::ParsedRelation;
use crate::domain::{REL_INHERITS, REL_REFERENCES};

/// C++ base classes → `inherits` edges. Called on a `class_specifier` /
/// `struct_specifier`. C++ has no separate interface concept, so every base is
/// an inherits parent regardless of access (`public`/`private`/`protected`
/// `access_specifier` nodes are skipped). Bases surface as `type_identifier`,
/// `qualified_identifier` (`ns::Base` → bind on the `name` tail), or
/// `template_type` (`Tmpl<int>` → the template `name`). A class/struct with no
/// `base_class_clause` — all of C, and plain C++ aggregates — yields nothing, so
/// this needs no language gate (C `struct_specifier` never carries a base clause).
pub(super) fn extract_cpp_inheritance(
    node: &tree_sitter::Node,
    source: &str,
) -> Vec<ParsedRelation> {
    let class_name = match node.child_by_field_name("name") {
        Some(n) => node_text(&n, source),
        None => return Vec::new(),
    };
    if class_name.is_empty() {
        return Vec::new();
    }
    // base_class_clause is not a named field — scan the class's named children.
    let clause = (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .find(|c| c.kind() == "base_class_clause");
    let clause = match clause {
        Some(c) => c,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..clause.named_child_count() {
        let base = match clause.named_child(i) {
            Some(b) => b,
            None => continue,
        };
        let name = match base.kind() {
            "type_identifier" => Some(node_text(&base, source)),
            // ns::Base → the `name` tail; Tmpl<int> → the template `name`.
            "qualified_identifier" | "template_type" => base
                .child_by_field_name("name")
                .map(|n| node_text(&n, source)),
            // access_specifier / virtual / anything else: not a base type name.
            _ => None,
        };
        if let Some(name) = name {
            if !name.is_empty() {
                out.push(ParsedRelation {
                    source_name: class_name.to_string(),
                    target_name: name.to_string(),
                    relation: REL_INHERITS.into(),
                    metadata: None,
                    source_language: String::new(),
                    source_line: None,
                });
            }
        }
    }
    out
}

/// Emit a `references` edge for a bare C/C++ `identifier` used as a function value.
/// Self-exclusion is structural: a call's callee is the `function` field of a
/// `call_expression` (not `argument_list`); member accesses (`obj->method`) are
/// `field_identifier`. M2/M2.5 excludes enclosing-function parameters and locals.
pub(super) fn extract_cpp_value_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    let parent = node.parent()?;
    let in_value_position = match parent.kind() {
        // Call argument.
        "argument_list" => true,
        // Designated initializer value (`{ .read = my_read }`).
        "initializer_pair" => {
            parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id())
        }
        // Positional initializer (`handler_t hs[] = { h1, h2 }`).
        "initializer_list" => true,
        // Assignment RHS (`ops->read = my_read`).
        "assignment_expression" => {
            parent.child_by_field_name("right").map(|v| v.id()) == Some(node.id())
        }
        // Init-declarator RHS (`fn_t cb = handler`).
        "init_declarator" => parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id()),
        "return_statement" => true,
        // `&fn` — only when the address-of sits in one of the value positions above.
        "pointer_expression" => parent
            .parent()
            .map(|gp| {
                matches!(
                    gp.kind(),
                    "argument_list"
                        | "initializer_pair"
                        | "initializer_list"
                        | "assignment_expression"
                        | "init_declarator"
                        | "return_statement"
                )
            })
            .unwrap_or(false),
        _ => false,
    };
    if !in_value_position {
        return None;
    }
    let name = node_text(node, source);
    if name.is_empty() || name == "NULL" {
        return None;
    }
    if c_enclosing_fn_local_names(node, source).contains(name) {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.unwrap_or("<module>").to_string(),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
    })
}

/// Collect local binding names visible to a C/C++ value-reference candidate:
/// parameters + body declarations of the nearest enclosing `function_definition`.
/// Both surface as `parameter_declaration` / `declaration` nodes; the declared NAME
/// is taken from the declarator chain (`c_declared_name`), never the init `value`.
fn c_enclosing_fn_local_names(
    node: &tree_sitter::Node,
    source: &str,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "function_definition" {
            collect_c_decls(&n, source, &mut names, 0);
            break;
        }
        cur = n.parent();
    }
    names
}

/// Walk a subtree collecting the declared names of every `parameter_declaration`
/// and `declaration` (the declarator chain, NOT init values). The enclosing
/// function's own name is a child of a `function_declarator`, not a declaration, so
/// it is naturally not collected.
fn collect_c_decls(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if matches!(node.kind(), "parameter_declaration" | "declaration") {
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if let Some(name) = c_declared_name(&child, source, 0) {
                    out.insert(name);
                }
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_c_decls(&child, source, out, depth + 1);
        }
    }
}

/// Resolve the declared identifier of a C/C++ declarator, following the declarator
/// chain (`init_declarator` → `declarator`, pointer/array/function declarators, and
/// parenthesized declarators) to the innermost `identifier`. Returns None for type
/// nodes / unnamed declarators. Never descends into an `init_declarator`'s `value`.
fn c_declared_name(node: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
    if depth > MAX_SUBTREE_DEPTH {
        return None;
    }
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(node, source).to_string()),
        "init_declarator" | "pointer_declarator" | "array_declarator" | "function_declarator" => {
            node.child_by_field_name("declarator")
                .and_then(|d| c_declared_name(&d, source, depth + 1))
        }
        "parenthesized_declarator" => (0..node.named_child_count())
            .filter_map(|i| node.named_child(i))
            .find_map(|c| c_declared_name(&c, source, depth + 1)),
        _ => None,
    }
}
