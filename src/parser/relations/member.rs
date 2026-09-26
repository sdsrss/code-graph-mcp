//! Member calls on an object: `x.f()` / `x->f()` whose receiver is neither the
//! enclosing instance (`this` / `self` / `cls` / `super`) nor a module binding.
//!
//! Such a call can only run a METHOD — a free function is never reachable as an
//! object's member — but the non-Rust extractors recorded it as a bare `f()`, so
//! the resolver bound `words.push(x)` to a nested `const push = () => …`,
//! `JSON.stringify(v)` to a project `function stringify`, and `ctx.get(None)` to a
//! module-level `def get`. Measured on four external corpora plus this repo
//! (scripts/scip_oracle), member-call → free-function edges were 0 right and 131
//! wrong. The call is stamped `{"q":"member"}` and the resolver drops `function`
//! candidates for it; everything else about its resolution is unchanged.
//!
//! Languages: C++, Python, JS/TS. Not C, whose struct fields commonly hold a
//! free function of the same name (`ops->read` → `read`), and not Rust, which has
//! its own receiver qualifiers.

use std::cell::RefCell;
use std::collections::HashSet;

use super::node_text;

pub(super) const MEMBER_META: &str = r#"{"q":"member"}"#;

thread_local! {
    /// Names the current file binds by import / require. A member call on one is
    /// a module-function call (`helpers.run()`, `m.f()`), which may well target a
    /// free function, so it is not marked. Per file: reset by `reset_import_bound`.
    static IMPORT_BOUND: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Collect the file's import-bound names. MUST run once per file before its walk.
pub(super) fn reset_import_bound(root: tree_sitter::Node, source: &str, family: &str) {
    let mut names = HashSet::new();
    if matches!(family, "python" | "javascript" | "typescript" | "tsx") {
        collect(root, source, family, &mut names, 0);
    }
    IMPORT_BOUND.with(|b| *b.borrow_mut() = names);
}

fn collect(
    node: tree_sitter::Node,
    source: &str,
    family: &str,
    out: &mut HashSet<String>,
    depth: usize,
) {
    if depth > 256 {
        return;
    }
    let text = |n: tree_sitter::Node| node_text(&n, source).to_string();
    match (family, node.kind()) {
        ("python", "import_statement") | ("python", "import_from_statement") => {
            let module = node.child_by_field_name("module_name").map(|m| m.id());
            for i in 0..node.named_child_count() {
                let Some(c) = node.named_child(i) else {
                    continue;
                };
                if Some(c.id()) == module {
                    continue; // `from x import y` binds y, not x
                }
                match c.kind() {
                    // `import a.b` binds `a`; `from x import a` binds `a`.
                    "dotted_name" => {
                        if let Some(first) = c.named_child(0) {
                            out.insert(text(first));
                        }
                    }
                    "aliased_import" => {
                        if let Some(alias) = c.child_by_field_name("alias") {
                            out.insert(text(alias));
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        (_, "import_clause") | (_, "namespace_import") => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    if c.kind() == "identifier" {
                        out.insert(text(c));
                    }
                }
            }
        }
        (_, "import_specifier") => {
            if let Some(n) = node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
            {
                out.insert(text(n));
            }
            return;
        }
        (_, "variable_declarator") if family != "python" => {
            let value = node.child_by_field_name("value");
            let value = match value {
                Some(v) if v.kind() == "await_expression" => v.named_child(0),
                v => v,
            };
            if value.is_some_and(|v| is_module_load(v, source)) {
                if let Some(name) = node.child_by_field_name("name") {
                    bind_pattern(name, source, out);
                }
            }
        }
        _ => {}
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i) {
            collect(c, source, family, out, depth + 1);
        }
    }
}

/// `require('x')` or `import('x')`.
fn is_module_load(node: tree_sitter::Node, source: &str) -> bool {
    node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|f| matches!(node_text(&f, source), "require" | "import"))
}

/// Every identifier a declarator's name binds: `m`, `{ a, b: c }`.
fn bind_pattern(node: tree_sitter::Node, source: &str, out: &mut HashSet<String>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            out.insert(node_text(&node, source).to_string());
        }
        "pair_pattern" => {
            if let Some(v) = node.child_by_field_name("value") {
                bind_pattern(v, source, out);
            }
        }
        _ => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    bind_pattern(c, source, out);
                }
            }
        }
    }
}

/// Whether the call node is a member call on an object (see module docs).
pub(super) fn is_member_call(call: tree_sitter::Node, source: &str, family: &str) -> bool {
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    let (member_kind, object_field) = match family {
        "cpp" => ("field_expression", "argument"),
        "python" => ("attribute", "object"),
        "javascript" | "typescript" | "tsx" => ("member_expression", "object"),
        _ => return false,
    };
    if function.kind() != member_kind {
        return false;
    }
    let Some(object) = function.child_by_field_name(object_field) else {
        return false;
    };
    match object.kind() {
        "this" | "super" => return false,
        "call" | "call_expression" => {
            // `super().close()` (Python), `require('./x').f()` (JS): not an object.
            let callee = object
                .child_by_field_name("function")
                .map(|f| node_text(&f, source));
            if matches!(callee, Some("super" | "require" | "import")) {
                return false;
            }
        }
        _ => {}
    }
    // The receiver's root name: `a` in `a.b.c.f()`.
    let mut root = object;
    while matches!(
        root.kind(),
        "attribute" | "member_expression" | "field_expression"
    ) {
        match root.child_by_field_name(object_field) {
            Some(inner) => root = inner,
            None => break,
        }
    }
    if matches!(root.kind(), "identifier" | "this") {
        let name = node_text(&root, source);
        if matches!(
            name,
            "this" | "self" | "cls" | "globalThis" | "window" | "module" | "exports"
        ) || IMPORT_BOUND.with(|b| b.borrow().contains(name))
        {
            return false;
        }
    }
    true
}
