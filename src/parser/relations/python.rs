//! Python-specific extraction: REFERENCES edges for edgeless type-annotation
//! usages.
//!
//! THE KEY DIFFERENCE from Rust/TS: tree-sitter-python represents a type name in
//! an annotation as a plain `identifier` — the SAME node kind as a value
//! identifier (`u`, `account`, `compute`). There is no distinct `type_identifier`
//! kind to gate on. So this extractor gates on ANNOTATION CONTEXT, not node kind:
//! an `identifier` only emits a reference when its nearest meaningful ancestor is
//! a `type` node (tree-sitter-python wraps every annotation type in a `type`
//! node). Gating on kind alone would emit a reference for every variable and
//! function name in the file.
//!
//! Probe-confirmed annotation shapes (tree-sitter-python):
//! - parameter:  `def f(x: Foo)` → `typed_parameter` → `type[field=type]` → `identifier Foo`
//! - return:     `def f() -> Bar` → `function_definition` → `type[field=return_type]` → `identifier Bar`
//! - variable / class attr: `x: Baz` → `assignment` → `type[field=type]` → `identifier Baz`
//! - generic:    `List[User]` → `type` → `generic_type` → `identifier List` (head);
//!   the arg `User` is `type_parameter [User]` → `type` → `identifier User`
//!   (its own nested `type`).
//! - dotted:     `mod.Type` → `type` → `attribute`, where `mod` is
//!   `identifier[field=object]` and `Type` is `identifier[field=attribute]`.
//!
//! Naturally excluded (NOT under a `type` node):
//! - base classes: `class Foo(Base)` → `argument_list[field=superclasses]` → `identifier Base`
//!   (already an inherits edge);
//! - value identifiers, attribute reads, call names.

use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use super::ParsedRelation;
use crate::domain::{PYTHON_TYPE_REFERENCE_NOISE, REL_REFERENCES};

/// The current Python file's `def` / `class` names ([`reset_python_file`]).
#[derive(Default)]
struct PyDefined {
    /// Every class, and every `def` outside a class body (module level, or
    /// nested in a function): what a bare name may reach anywhere.
    free: std::collections::HashSet<String>,
    /// `def`s directly in a class body: a bare name reaches one only from
    /// that body.
    methods: std::collections::HashSet<String>,
}

thread_local! {
    /// Per file: reset by [`reset_python_file`].
    static PY_DEFINED: std::cell::RefCell<PyDefined> = std::cell::RefCell::new(PyDefined::default());
}

/// Collect the file's `def` / `class` names. MUST run once per file before its
/// walk.
pub(super) fn reset_python_file(root: tree_sitter::Node, source: &str, family: &str) {
    PY_DEFINED.with(|d| {
        let mut defined = d.borrow_mut();
        defined.free.clear();
        defined.methods.clear();
        if family != "python" {
            return;
        }
        let mut cursor = root.walk();
        'walk: loop {
            let node = cursor.node();
            if let Some(name) = matches!(node.kind(), "function_definition" | "class_definition")
                .then(|| node.child_by_field_name("name"))
                .flatten()
            {
                let name = node_text(&name, source).to_string();
                if node.kind() == "function_definition" && in_class_body(node) {
                    defined.methods.insert(name);
                } else {
                    defined.free.insert(name);
                }
            }
            // A `def` / `class` is a statement: no expression holds one.
            let kind = node.kind();
            let statements = kind == "module"
                || kind == "block"
                || kind == "decorated_definition"
                || kind.ends_with("_definition")
                || kind.ends_with("_clause")
                || (kind.ends_with("_statement") && kind != "expression_statement");
            if statements && cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
    });
}

/// Whether `node` sits in a class body with no function in between.
fn in_class_body(node: tree_sitter::Node) -> bool {
    let mut at = node.parent();
    while let Some(n) = at {
        match n.kind() {
            "class_definition" => return true,
            "function_definition" | "lambda" => return false,
            _ => at = n.parent(),
        }
    }
    false
}

/// True if `node` is the type NAME of an annotation, i.e. it sits in a position
/// that tree-sitter-python wraps in a `type` node. The gate walks up a small,
/// bounded chain of *type-structural* ancestors (`type`, `generic_type`,
/// `type_parameter`) and returns true only when it reaches a `type` node:
///
/// - `identifier` directly inside `type`            → `Foo` in `x: Foo` ✓
/// - `identifier` directly inside `generic_type`    → `List` in `List[User]`
///   (the generic head); its `generic_type` parent must itself be under a `type` ✓
/// - the nested-`type` generic arg                  → `User` in `List[User]`
///   reaches `type` via its own `type` parent ✓
/// - `identifier[field=attribute]` of an `attribute` whose parent is a `type`
///   → `Type` in `mod.Type` ✓ (the dotted-annotation tail)
///
/// A value identifier (`u`, `account`, `compute`) has parent `attribute`
/// (value read), `argument_list`, `call`, `assignment[field=left/right]`,
/// `parameters`, etc. — none of which lead to a `type` node — so it returns
/// false. Base classes live under `argument_list`, also false.
fn is_annotation_type_name(node: &tree_sitter::Node) -> bool {
    // Special-case the dotted tail `mod.Type`: the tail `identifier` is the
    // `attribute` field of an `attribute` node. It is an annotation type only if
    // that `attribute` is itself directly under a `type` node (not a value read
    // like `u.account`, whose `attribute` parent is under a return/expression).
    if let Some(parent) = node.parent() {
        if parent.kind() == "attribute" {
            // Only the tail (`attribute` field) is the type name; the `object`
            // segment (`mod`) is a module path, not a project type usage.
            let is_tail =
                parent.child_by_field_name("attribute").map(|n| n.id()) == Some(node.id());
            if !is_tail {
                return false;
            }
            return parent
                .parent()
                .map(|gp| gp.kind() == "type")
                .unwrap_or(false);
        }
    }

    // Walk up through type-structural wrappers only. Stop (return false) the
    // moment we hit a non-type-structural ancestor — that means this identifier
    // is in value position, not an annotation.
    let mut current = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    let mut depth = 0;
    while depth <= MAX_SUBTREE_DEPTH {
        match current.kind() {
            "type" => return true,
            // `generic_type` head (`List` in `List[User]`) and `type_parameter`
            // (`[User]`) are the only intermediates between an annotation
            // `identifier` and its enclosing `type`. Keep climbing.
            "generic_type" | "type_parameter" => {
                current = match current.parent() {
                    Some(p) => p,
                    None => return false,
                };
                depth += 1;
            }
            // Any other ancestor (assignment, attribute value-read, argument_list,
            // call, parameters, block, ...) means non-annotation position.
            _ => return false,
        }
    }
    false
}

/// Emit a `references` edge for a Python `identifier` used as a type-annotation
/// name. Gated by `is_annotation_type_name` (annotation context, since Python
/// annotation types are plain `identifier`s). Skips:
/// - builtins / `typing` generics (`int`, `str`, `List`, `Optional`, ...) — they
///   resolve to the stdlib, not a project symbol (PYTHON_TYPE_REFERENCE_NOISE);
/// - empty / `_`.
///
/// Base classes (`class Foo(Base)`) are NOT reached here — they live under
/// `argument_list`, never a `type` node — so the existing inherits edge is not
/// double-emitted.
pub(super) fn extract_python_type_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    if !is_annotation_type_name(node) {
        return None;
    }
    let name = node_text(node, source);
    if name.is_empty() || name == "_" || PYTHON_TYPE_REFERENCE_NOISE.contains(&name) {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.unwrap_or("<module>").to_string(),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
        call_at: None,
    })
}

/// Emit a `references` edge for a Python `identifier` used as a function VALUE —
/// a callback passed/stored/returned by bare name. Value positions:
///   - call argument (`register(handler)`) — identifier under an `argument_list`
///     whose parent is a `call` (a `class_definition` superclass list is excluded —
///     that is already an inherits edge);
///   - keyword-argument value (`sorted(xs, key=my_key)`);
///   - assignment RHS (`cb = handler`) — the `right` field;
///   - `return handler`;
///   - a decorator named bare (`@setupmethod`);
///   - dict value (`{ "k": handler }`).
///
/// Self-exclusion is structural: a call's callee is the `function` field of `call`
/// (parent `call`, not `argument_list`); `attribute` reads (`obj.method`) are not
/// bare identifiers in these slots. M2/M2.5: a name equal to a parameter or local
/// binding (assignment / for target) of an enclosing function is a local, not a
/// global-fn reference — skip. Mutually exclusive with the type-annotation pass
/// (annotation context vs value position), so both can run on the same identifier.
pub(super) fn extract_python_value_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    let parent = node.parent()?;
    let in_value_position = match parent.kind() {
        "argument_list" => parent
            .parent()
            .map(|gp| gp.kind() == "call")
            .unwrap_or(false),
        "keyword_argument" => {
            parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id())
        }
        "assignment" => parent.child_by_field_name("right").map(|v| v.id()) == Some(node.id()),
        "return_statement" => true,
        // `@setupmethod` names the function it applies (C4, 2026-09-28 usage
        // evaluation: flask's 44 uses made `refs setupmethod` report none). A
        // decorator spelled as a call (`@app.route("/")`) is a call edge already.
        "decorator" => true,
        "pair" => parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id()),
        // An element of a list, tuple or set literal (`default_tags = [TagDict,
        // ...]`, `except (A, B)`, D#789), when the file defines or imports the
        // name: a builtin (`(str, bytes)`) or a module-level variable is no
        // project symbol, and bound by name it took one of another file. A
        // method's name counts only in a class body (`CHECK_METHODS =
        // [_check_action]`): elsewhere `(list, tuple)` is the builtin, not a
        // method `list`. An assignment or loop target is a `*_pattern` node,
        // never one of these.
        "list" | "tuple" | "set" => {
            let name = node_text(node, source);
            PY_DEFINED.with(|d| {
                let d = d.borrow();
                d.free.contains(name) || (d.methods.contains(name) && in_class_body(*node))
            }) || super::member::is_import_bound(name)
        }
        // Phase 3b: tuple return (`return f, g`) / tuple RHS (`a, b = f, g`) wrap the
        // values in an `expression_list` under the return / assignment-right.
        "expression_list" => match parent.parent() {
            Some(gp) => match gp.kind() {
                "return_statement" => true,
                "assignment" => {
                    gp.child_by_field_name("right").map(|r| r.id()) == Some(parent.id())
                }
                _ => false,
            },
            None => false,
        },
        _ => false,
    };
    if !in_value_position {
        return None;
    }
    let name = node_text(node, source);
    if name.is_empty() || name == "_" {
        return None;
    }
    if py_enclosing_fn_local_names(node, source).contains(name) {
        return None;
    }
    // A decorator sits before its `def`, where the walk's scope is still the
    // class body or module: attribute it to the decorated function instead,
    // named as the walk names a method (`Class.method`).
    let decorated = (parent.kind() == "decorator")
        .then(|| decorated_function_scope(&parent, source))
        .flatten();
    Some(ParsedRelation {
        source_name: decorated.unwrap_or_else(|| scope.unwrap_or("<module>").to_string()),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
        call_at: None,
    })
}

/// `Class.method` or `function` for the definition a `decorator` applies to:
/// the scope name the relations walk gives that definition, so the edge's
/// source is its node.
fn decorated_function_scope(decorator: &tree_sitter::Node, source: &str) -> Option<String> {
    let decorated = decorator
        .parent()
        .filter(|p| p.kind() == "decorated_definition")?;
    let definition = decorated.child_by_field_name("definition")?;
    let name = node_text(&definition.child_by_field_name("name")?, source).to_string();
    let class = decorated
        .parent()
        .filter(|b| b.kind() == "block")
        .and_then(|b| b.parent())
        .filter(|c| c.kind() == "class_definition")
        .and_then(|c| c.child_by_field_name("name"))
        .map(|n| node_text(&n, source).to_string());
    Some(match class {
        Some(class) if definition.kind() == "function_definition" => format!("{class}.{name}"),
        _ => name,
    })
}

/// Collect local binding names visible to a Python value-reference candidate:
/// parameters of every enclosing `function_definition` (closures capture outer
/// scope) + assignment / for targets in the nearest function body. Used for
/// M2/M2.5 exclusion. Over-collection (param type names, default-value idents) is
/// precision-safe — it only suppresses a candidate.
fn py_enclosing_fn_local_names(
    node: &tree_sitter::Node,
    source: &str,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut nearest_body_done = false;
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "function_definition" {
            if let Some(p) = n.child_by_field_name("parameters") {
                collect_py_idents(&p, source, &mut names, 0);
            }
            if !nearest_body_done {
                if let Some(body) = n.child_by_field_name("body") {
                    collect_py_local_targets(&body, source, &mut names, 0);
                }
                nearest_body_done = true;
            }
        }
        cur = n.parent();
    }
    names
}

/// Walk a function body collecting assignment / for TARGET names (the `left` field),
/// not RHS values. Recurses into nested blocks.
fn collect_py_local_targets(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if matches!(node.kind(), "assignment" | "for_statement") {
        if let Some(l) = node.child_by_field_name("left") {
            collect_py_idents(&l, source, out, 0);
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_py_local_targets(&child, source, out, depth + 1);
        }
    }
}

/// Collect all `identifier` names in a subtree.
fn collect_py_idents(
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
            collect_py_idents(&child, source, out, depth + 1);
        }
    }
}

/// Issue #32 cause 2 — bounded single-assignment receiver type propagation.
/// For a Python call `recv.method(...)` whose receiver `recv` is a plain local
/// variable, infer its type from a SINGLE unambiguous constructor assignment
/// `recv = ClassName(...)` in the nearest enclosing function body (or the module
/// for top-level code) and return `ClassName`. The mod.rs call arm stamps it as
/// `{"q":"rtype","v":"ClassName"}` so Phase-2 resolution can pick
/// `ClassName.method` out of N same-named methods (via `self_filter_candidates`),
/// instead of dropping the whole by-name fan-out as ambiguous — the exact drop
/// that reported live pydantic-style receiver methods as dead code.
///
/// Deliberately conservative — returns None (→ unchanged bare resolution) unless
/// the type is provably fixed, so it can NEVER emit a wrong-type edge. Priority:
/// a SINGLE local `recv = ClassName(...)` constructor assignment wins (a local
/// (re)assignment is the receiver's real type at the call, overriding any
/// parameter annotation); with NO local assignment, an explicit parameter
/// annotation `def f(recv: ClassName)` of the enclosing function is used; a
/// receiver with >1 assignment is possibly mixed-type and stays unknown (None).
/// Both sources require the callee shape `identifier.method()` (not `self`/`cls`,
/// not a chain / attribute receiver) and an upper-case-initial simple class name
/// (builtins like `str` and non-simple annotations fall back to bare).
///
/// A wrong guess still can't create a bad edge: a bogus type simply fails
/// `self_filter_candidates` (no `Type.method` node) and drops, exactly as today.
pub(super) fn infer_python_call_receiver_type(
    call_node: &tree_sitter::Node,
    source: &str,
) -> Option<String> {
    let function = call_node.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    if object.kind() != "identifier" {
        return None;
    }
    let recv = node_text(&object, source);
    if recv.is_empty() {
        return None;
    }
    // `self.f()` / `cls.f()`: the class whose method's first parameter it is.
    if recv == "self" || recv == "cls" {
        let (_, class) = py_method_class(call_node, Some(recv), source)?;
        return class
            .child_by_field_name("name")
            .map(|n| node_text(&n, source).to_string());
    }

    let scope = nearest_py_scope(call_node)?;
    let scan_root = if scope.kind() == "function_definition" {
        scope.child_by_field_name("body")?
    } else {
        scope
    };
    let mut rhs_nodes: Vec<tree_sitter::Node> = Vec::new();
    collect_py_recv_assignment_rhs(&scan_root, recv, source, &mut rhs_nodes, 0);
    match rhs_nodes.len() {
        // A single local `recv = ClassName(...)` fixes the type (and a local
        // reassignment correctly overrides any stale parameter annotation).
        1 => py_constructor_type_name(&rhs_nodes[0], source),
        // No local (re)assignment → fall back to an explicit parameter annotation
        // of the enclosing function. Module-level receivers have no parameters.
        0 if scope.kind() == "function_definition" => {
            py_param_annotation_type(&scope, recv, source)
        }
        // >1 assignment → possibly reassigned to different types → unknown.
        _ => None,
    }
}

/// `super().f()` / `super(C, self).f()`: the first base class of the class
/// whose method holds the call, when it is a plain name (`class A(Base)`).
pub(super) fn infer_python_super_type(
    call_node: &tree_sitter::Node,
    source: &str,
) -> Option<String> {
    let function = call_node.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    let is_super = object.kind() == "call"
        && object
            .child_by_field_name("function")
            .is_some_and(|f| node_text(&f, source) == "super");
    if !is_super {
        return None;
    }
    let (_, class) = py_method_class(call_node, None, source)?;
    // `super(B, self)` starts after `B`, which need not be this class: only the
    // two-argument form naming the enclosing class is the plain `super()`.
    let class_name = class
        .child_by_field_name("name")
        .map(|n| node_text(&n, source));
    let args = object.child_by_field_name("arguments")?;
    if let Some(named) = args.named_child(0) {
        if Some(node_text(&named, source)) != class_name {
            return None;
        }
    }
    let bases = class.child_by_field_name("superclasses")?;
    let first = bases.named_child(0)?;
    (first.kind() == "identifier").then(|| node_text(&first, source).to_string())
}

/// The method enclosing `node` and its class: the nearest `def` directly in a
/// class body, reached through nested `def`s only when none of them rebinds
/// `receiver` as a parameter. With `Some(receiver)`, the method's first parameter
/// must be that name (`self` / `cls`), so a `@staticmethod` does not qualify.
fn py_method_class<'a>(
    node: &tree_sitter::Node<'a>,
    receiver: Option<&str>,
    source: &str,
) -> Option<(tree_sitter::Node<'a>, tree_sitter::Node<'a>)> {
    let mut cur = node.parent();
    let mut depth = 0;
    while let Some(n) = cur {
        if depth > MAX_SUBTREE_DEPTH {
            return None;
        }
        if n.kind() == "class_definition" {
            return None; // class body code, not inside a method
        }
        // `lambda self: self.f()` binds its own `self`.
        if n.kind() == "lambda" {
            if let (Some(r), Some(params)) = (receiver, n.child_by_field_name("parameters")) {
                let mut ids = std::collections::HashSet::new();
                collect_py_idents(&params, source, &mut ids, 0);
                if ids.contains(r) {
                    return None;
                }
            }
        }
        if n.kind() == "function_definition" {
            let first_param = n
                .child_by_field_name("parameters")
                .and_then(|p| p.named_child(0))
                .map(|p| match p.kind() {
                    "identifier" => node_text(&p, source),
                    _ => p
                        .child_by_field_name("name")
                        .or_else(|| p.named_child(0))
                        .map(|c| node_text(&c, source))
                        .unwrap_or(""),
                });
            let mut holder = n.parent();
            if holder.is_some_and(|h| h.kind() == "decorated_definition") {
                holder = holder.and_then(|h| h.parent());
            }
            let class = holder
                .filter(|b| b.kind() == "block")
                .and_then(|b| b.parent())
                .filter(|c| c.kind() == "class_definition");
            if let Some(class) = class {
                return match receiver {
                    Some(r) if first_param != Some(r) => None,
                    _ => Some((n, class)),
                };
            }
            if let Some(r) = receiver {
                let mut ids = std::collections::HashSet::new();
                if let Some(params) = n.child_by_field_name("parameters") {
                    collect_py_idents(&params, source, &mut ids, 0);
                }
                if ids.contains(r) {
                    return None; // a nested function's own `self` parameter
                }
            }
        }
        cur = n.parent();
        depth += 1;
    }
    None
}

/// Nearest enclosing scope node — a `function_definition` or the `module` root.
/// Callers derive the statement block via the `body` field to scan assignments,
/// or read `parameters` for annotation lookup. Bounded upward walk.
fn nearest_py_scope<'a>(node: &tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    let mut cur = node.parent();
    let mut depth = 0;
    while let Some(n) = cur {
        if depth > MAX_SUBTREE_DEPTH {
            return None;
        }
        if matches!(n.kind(), "function_definition" | "module") {
            return Some(n);
        }
        cur = n.parent();
        depth += 1;
    }
    None
}

/// Resolve `recv`'s type from a parameter annotation of `func`
/// (`def f(recv: ClassName, ...)` or `def f(recv: ClassName = default)`). Returns
/// the annotated class name via `py_annotation_type_name` (upper-case-initial
/// simple identifier only). Only the nearest enclosing function's own parameters
/// are consulted — a closure-captured outer param falls back to bare resolution.
fn py_param_annotation_type(func: &tree_sitter::Node, recv: &str, source: &str) -> Option<String> {
    let params = func.child_by_field_name("parameters")?;
    for i in 0..params.named_child_count() {
        let p = match params.named_child(i) {
            Some(p) => p,
            None => continue,
        };
        // `def f(w: T)` → `typed_parameter` (name = first identifier child, no
        // field); `def f(w: T = d)` → `typed_default_parameter` (name field).
        let (name_node, type_node) = match p.kind() {
            "typed_parameter" => {
                let name = (0..p.named_child_count())
                    .filter_map(|j| p.named_child(j))
                    .find(|c| c.kind() == "identifier");
                (name, p.child_by_field_name("type"))
            }
            "typed_default_parameter" => {
                (p.child_by_field_name("name"), p.child_by_field_name("type"))
            }
            _ => continue,
        };
        if let (Some(name), Some(ty)) = (name_node, type_node) {
            if node_text(&name, source) == recv {
                return py_annotation_type_name(&ty, source);
            }
        }
    }
    None
}

/// Extract a simple upper-case-initial class name from a `type` annotation node.
/// tree-sitter-python wraps the annotation in a `type` node; only a bare
/// `identifier` annotation (`w: DataWriter`) is used — generics (`List[T]`),
/// dotted (`mod.T`), and string forward-refs are left to bare resolution.
fn py_annotation_type_name(ty: &tree_sitter::Node, source: &str) -> Option<String> {
    let inner = if ty.kind() == "type" {
        ty.named_child(0)?
    } else {
        *ty
    };
    if inner.kind() == "identifier" {
        let name = node_text(&inner, source);
        if name.chars().next()?.is_uppercase() {
            return Some(name.to_string());
        }
    }
    None
}

/// Collect the RHS node of every `recv = <rhs>` plain assignment in a scope,
/// recursing into nested blocks (if/for/while/with/try) but NOT into nested
/// `function_definition`/`class_definition` — a same-named variable there is a
/// different binding. Augmented assignments (`recv += x`) are intentionally not
/// collected (they don't (re)bind the type).
fn collect_py_recv_assignment_rhs<'a>(
    node: &tree_sitter::Node<'a>,
    recv: &str,
    source: &str,
    out: &mut Vec<tree_sitter::Node<'a>>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "assignment" {
        if let Some(left) = node.child_by_field_name("left") {
            if left.kind() == "identifier" && node_text(&left, source) == recv {
                if let Some(right) = node.child_by_field_name("right") {
                    out.push(right);
                }
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if matches!(child.kind(), "function_definition" | "class_definition") {
                continue;
            }
            collect_py_recv_assignment_rhs(&child, recv, source, out, depth + 1);
        }
    }
}

/// If `rhs` is a direct constructor call `ClassName(...)` — a `call` whose
/// `function` is a plain `identifier` with an upper-case initial — return
/// `ClassName`. Rejects `mod.Class()` (attribute callee), lower-case factory
/// functions, and non-call RHS so the inferred type is a plausible class.
fn py_constructor_type_name(rhs: &tree_sitter::Node, source: &str) -> Option<String> {
    if rhs.kind() != "call" {
        return None;
    }
    let function = rhs.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    let name = node_text(&function, source);
    if name.chars().next()?.is_uppercase() {
        Some(name.to_string())
    } else {
        None
    }
}
