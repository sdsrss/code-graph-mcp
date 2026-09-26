//! TypeScript/TSX-specific extraction: REFERENCES edges for edgeless
//! type-position usages.
//!
//! A `type_identifier` in type position (type annotation `x: Foo`, return type
//! `(): Foo`, generic arg `Array<Foo>`, field type `size: Foo`) is a usage of a
//! type that produces no `calls`/`imports`/`inherits` edge on its own, so
//! `find_dead_code` would flag `Foo` dead and `find_references` would miss it.
//! This mirrors `rust::extract_rust_type_reference` for TS/TSX.
//!
//! Tree-sitter-typescript represents every type-position name as a
//! `type_identifier`; primitive types (`string`, `number`, `boolean`, `any`,
//! `void`, `unknown`, `never`) parse as `predefined_type`, so they are
//! naturally excluded. JavaScript has no type annotations, so this is a no-op
//! there — the gate is `language == "typescript" || language == "tsx"`.

use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use super::ParsedRelation;
use crate::domain::REL_REFERENCES;

/// Emit a `references` edge for a `type_identifier` used in type position.
/// Skips the cases already covered by another edge (or that aren't usages) so
/// the references edge stays a pure "edgeless usage" signal:
/// - the type's OWN definition name — the `name` field of
///   `interface_declaration` / `class_declaration` / `type_alias_declaration` /
///   `enum_declaration` (declaration, not a usage);
/// - heritage types — a `type_identifier` anywhere inside `extends_clause` /
///   `implements_clause` / `class_heritage` already yields an inherits/implements
///   edge, so a references edge there would be a double edge (and would defeat
///   dead-code detection for the supertype).
///
/// Note: the `name` field of `generic_type` (`Array` in `Array<Foo>`, `Map` in
/// `Map<K, V>`) IS a real usage and is intentionally NOT skipped — only the
/// declaration-name skip is keyed off the four declaration parent kinds, not off
/// "is some parent's name field".
pub(super) fn extract_ts_type_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    if let Some(parent) = node.parent() {
        // The `name` field of a type/interface/class/enum declaration is the
        // declaration, not a usage. (class/enum names are `type_identifier` /
        // `identifier`; only `type_identifier` reaches this fn, but interface
        // and type-alias names ARE `type_identifier`, so this matters.)
        if matches!(
            parent.kind(),
            "interface_declaration"
                | "class_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
        ) && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
        {
            return None;
        }
        // Heritage types (`extends Bar` / `implements Iface`) already produce
        // inherits/implements edges — don't double-emit a reference.
        if matches!(
            parent.kind(),
            "extends_clause" | "implements_clause" | "class_heritage"
        ) {
            return None;
        }
    }
    let name = node_text(node, source);
    if name.is_empty() {
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

/// Emit a `references` edge for a BARE `identifier` used as a function VALUE —
/// a callback passed in a call-argument position (`arr.map(myFunc)`,
/// `addEventListener('click', handler)`). JS/TS/TSX. No address-of in JS, so the
/// only shape is a direct `arguments` child.
///
/// Self-exclusion is structural: a call's *callee* identifier has parent
/// `call_expression` (or sits under a `member_expression`), never `arguments`, so it
/// never fires here. `member_expression` selectors (`obj.method`) are
/// `property_identifier`, not `identifier`, so `foo(obj.method)` does not emit a
/// reference to `method` (Phase 2 scope).
///
/// M2 (param exclusion): a bare id equal to a parameter of ANY enclosing function
/// is a local binding, not a global-fn reference — skip. UNLIKE Rust, JS closures
/// capture outer-function params, so we collect params from every enclosing
/// function up to the root (no break at the nearest one).
pub(super) fn extract_js_value_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    let parent = node.parent()?;
    let in_value_position = match parent.kind() {
        // Phase 1: call argument.
        "arguments" => true,
        // Phase 2: binding RHS (`const cb = handler`) — the `value` field only, never
        // the `name` (a local binding, handled by M2.5).
        "variable_declarator" => {
            parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id())
        }
        // Phase 2: object property value (`{ onClick: handler }`) — `value` field
        // only, never the property key.
        "pair" => parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id()),
        // Phase 2: explicit `return handler;`.
        "return_statement" => true,
        // Phase 2: arrow implicit-return body (`() => handler`) — the `body` field.
        "arrow_function" => parent.child_by_field_name("body").map(|b| b.id()) == Some(node.id()),
        // Phase 3b: JSX attribute callback (`onClick={handleClick}`) — a bare
        // identifier inside a `jsx_expression` container whose parent is a
        // `jsx_attribute`. JSX children expressions (`<div>{x}</div>`) are excluded
        // (their `jsx_expression` parent is a jsx element, not an attribute).
        "jsx_expression" => parent
            .parent()
            .map(|gp| gp.kind() == "jsx_attribute")
            .unwrap_or(false),
        _ => false,
    };
    if !in_value_position {
        return None;
    }
    let name = node_text(node, source);
    if name.is_empty() {
        return None;
    }
    if js_enclosing_fn_local_names(node, source).contains(name) {
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

/// Collect LOCAL binding names visible to a value-reference candidate (M2 + M2.5):
///   - parameters of EVERY enclosing function (closures capture outer params) —
///     `formal_parameters` and the single-param `parameter` field (`x => ...`);
///   - `var`/`let`/`const` binding names from the NEAREST enclosing function body
///     (M2.5) — `variable_declarator` `name` field only, not the RHS `value`.
///
/// The nearest-function bound on var collection caps cost (a candidate inside a big
/// module IIFE would otherwise re-scan the whole file); outer-function *locals*
/// captured by an inner closure are a documented Phase-1 residual, outer *params*
/// are still covered. Default-value expressions over-collect, which only suppresses
/// a candidate (precision-safe).
fn js_enclosing_fn_local_names(
    node: &tree_sitter::Node,
    source: &str,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut nearest_fn_vars_done = false;
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
                | "generator_function"
                | "function"
        ) {
            if let Some(p) = n.child_by_field_name("parameters") {
                collect_js_param_idents(&p, source, &mut names, 0);
            }
            if let Some(p) = n.child_by_field_name("parameter") {
                collect_js_param_idents(&p, source, &mut names, 0);
            }
            if !nearest_fn_vars_done {
                if let Some(body) = n.child_by_field_name("body") {
                    collect_js_var_names(&body, source, &mut names, 0);
                }
                nearest_fn_vars_done = true;
            }
        }
        cur = n.parent();
    }
    names
}

/// Walk a function body collecting `variable_declarator` binding names from the
/// `name` field (not the RHS `value`). Recurses to reach declarations in nested
/// blocks. Used by `js_enclosing_fn_local_names` for M2.5.
fn collect_js_var_names(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "variable_declarator" {
        if let Some(name) = node.child_by_field_name("name") {
            collect_js_param_idents(&name, source, out, 0);
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_js_var_names(&child, source, out, depth + 1);
        }
    }
}

fn collect_js_param_idents(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "identifier" {
        out.insert(node_text(node, source).to_string());
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_js_param_idents(&child, source, out, depth + 1);
        }
    }
}
