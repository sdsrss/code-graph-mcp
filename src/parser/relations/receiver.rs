//! The class of a member call's receiver, where the source alone fixes it.
//!
//! `x.f()` names only `f`, and resolving it by name bound every same-named method
//! of every class: `snapshots_.Delete()` inside `DBImpl` bound `DBImpl::Delete`,
//! `this.getCookies()` bound three sibling classes' `getCookies`. When the
//! receiver's class is written down — the enclosing class for `this`, a declared
//! type for a C++ local, parameter or field, `new T()` or a `: T` annotation in
//! JS/TS — the call is stamped `{"q":"rtype","v":"T"}` and the resolver binds
//! `T`'s own method (see `resolve::recv_type_targets` for what happens when it has
//! none). Python has its own inference in `python.rs`.
//!
//! Conservative: any receiver whose class is not provable from the enclosing
//! function, class or (JS) enclosing scopes returns None and keeps the untyped
//! member-call resolution.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::helpers::MAX_SUBTREE_DEPTH;
use super::node_text;

/// The receiver class of `call` for C++ and JS/TS, or None.
pub(super) fn receiver_type(call: tree_sitter::Node, source: &str, family: &str) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    match family {
        "cpp" => cpp_receiver_type(function, source),
        "javascript" | "typescript" | "tsx" => js_receiver_type(function, source),
        _ => None,
    }
}

// ── C++ ──────────────────────────────────────────────────────────────────────

fn cpp_receiver_type(function: tree_sitter::Node, source: &str) -> Option<String> {
    if function.kind() != "field_expression" {
        return None;
    }
    let object = function.child_by_field_name("argument")?;
    let arrow = function
        .child_by_field_name("operator")
        .is_some_and(|o| node_text(&o, source) == "->");
    match object.kind() {
        "this" => cpp_enclosing_class(function, source),
        "identifier" => {
            let name = node_text(&object, source);
            let func = ancestor(function, &["function_definition"])?;
            let key = (func.id(), name.to_string(), arrow);
            if let Some(hit) = CPP_RECEIVERS.with(|c| c.borrow().get(&key).cloned()) {
                return hit;
            }
            let class = cpp_declared_class(func, name, arrow, source);
            CPP_RECEIVERS.with(|c| c.borrow_mut().insert(key, class.clone()));
            class
        }
        _ => None,
    }
}

/// The class a C++ receiver `name` is declared with in `func` (a local or
/// parameter), else in the class whose body holds `func` (a field).
fn cpp_declared_class(
    func: tree_sitter::Node,
    name: &str,
    arrow: bool,
    source: &str,
) -> Option<String> {
    let mut types = Vec::new();
    collect_cpp_decl_types(func, name, source, &mut types, 0);
    if types.is_empty() {
        // Not a local or parameter: a field of the class whose body
        // holds this member function.
        if let Some(class) = enclosing_class_body(func) {
            collect_cpp_field_types(class, name, source, &mut types);
        }
    }
    let (ty, pointer) = agreed(types, source)?;
    let class = cpp_type_name(ty, source, arrow && !pointer)?;
    // `template <class Iterator> … it.Next()`: a template parameter is
    // whatever type instantiates it, not a project class of that name.
    let head = class.split("::").next().unwrap_or(&class).trim();
    (!is_template_parameter(func, head, source)).then_some(class)
}

thread_local! {
    /// Per file (reset by [`reset`]): each JS scope's bindings, and each C++
    /// function's receiver classes. Collected once per scope instead of once
    /// per call — a test file's module scope holds thousands of calls, and
    /// rescanning it for each made hono's index 1.8x slower.
    static JS_SCOPES: RefCell<HashMap<usize, ScopeBindings>> = RefCell::new(HashMap::new());
    static CPP_RECEIVERS: RefCell<HashMap<(usize, String, bool), Option<String>>> =
        RefCell::new(HashMap::new());
}

/// Forget the previous file's caches. MUST run once per file before its walk.
pub(super) fn reset() {
    JS_SCOPES.with(|c| c.borrow_mut().clear());
    CPP_RECEIVERS.with(|c| c.borrow_mut().clear());
}

/// Whether `name` is a type parameter of a template enclosing `node`.
fn is_template_parameter(node: tree_sitter::Node, name: &str, source: &str) -> bool {
    let mut cur = node.parent();
    for _ in 0..MAX_SUBTREE_DEPTH * 4 {
        let Some(n) = cur else {
            return false;
        };
        if n.kind() == "template_declaration" {
            if let Some(params) = n.child_by_field_name("parameters") {
                let declares = (0..params.named_child_count())
                    .filter_map(|i| params.named_child(i))
                    .any(|p| {
                        (0..p.named_child_count())
                            .filter_map(|j| p.named_child(j))
                            .any(|c| c.kind() == "type_identifier" && node_text(&c, source) == name)
                    });
                if declares {
                    return true;
                }
            }
        }
        cur = n.parent();
    }
    false
}

/// The class a member function belongs to, as a `::` path: the classes whose
/// bodies hold its definition (`Outer::Inner`), or the scope of an out-of-line
/// `A::f` / `A<T>::B::f` declarator as written.
fn cpp_enclosing_class(node: tree_sitter::Node, source: &str) -> Option<String> {
    let func = ancestor(node, &["function_definition"])?;
    if let Some(mut body) = enclosing_class_body(func) {
        let mut path = Vec::new();
        for _ in 0..MAX_SUBTREE_DEPTH {
            let class = body.parent()?;
            path.push(node_text(&class.child_by_field_name("name")?, source).to_string());
            match class.parent().filter(|p| p.kind() == "field_declaration") {
                // `class Outer { class Inner { … } x; }` and plain nesting alike
                Some(field) => body = field.parent()?,
                None => match class.parent().and_then(|p| {
                    let p = if p.kind() == "template_declaration" {
                        p.parent()?
                    } else {
                        p
                    };
                    (p.kind() == "field_declaration_list").then_some(p)
                }) {
                    Some(outer) => body = outer,
                    None => break,
                },
            }
        }
        path.reverse();
        return Some(path.join("::"));
    }
    let mut decl = func.child_by_field_name("declarator")?;
    for _ in 0..MAX_SUBTREE_DEPTH {
        match decl.kind() {
            "function_declarator" => break,
            _ => decl = decl.child_by_field_name("declarator")?,
        }
    }
    let name = decl.child_by_field_name("declarator")?;
    if name.kind() != "qualified_identifier" {
        return None;
    }
    let scope = name.child_by_field_name("scope")?;
    Some(node_text(&scope, source).to_string())
}

/// The `field_declaration_list` directly holding `func`, when `func` is a
/// member function defined in its class body.
fn enclosing_class_body(func: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut parent = func.parent()?;
    if parent.kind() == "template_declaration" {
        parent = parent.parent()?;
    }
    (parent.kind() == "field_declaration_list"
        && parent
            .parent()
            .is_some_and(|c| matches!(c.kind(), "class_specifier" | "struct_specifier")))
    .then_some(parent)
}

/// A declaration of the receiver: its (type node, declared through a pointer),
/// or None when it binds the name in a way whose type is not written down
/// (structured binding, `auto` range loop).
type Decl<'a> = Option<(tree_sitter::Node<'a>, bool)>;

/// Every declaration of `name` among the parameters, locals and range-loop
/// variables under `node` (a whole function, lambdas included).
fn collect_cpp_decl_types<'a>(
    node: tree_sitter::Node<'a>,
    name: &str,
    source: &str,
    out: &mut Vec<Decl<'a>>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if matches!(
        node.kind(),
        "parameter_declaration"
            | "optional_parameter_declaration"
            | "declaration"
            | "for_range_loop"
    ) {
        declared_types(node, name, source, out);
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i) {
            collect_cpp_decl_types(c, name, source, out, depth + 1);
        }
    }
}

fn collect_cpp_field_types<'a>(
    body: tree_sitter::Node<'a>,
    name: &str,
    source: &str,
    out: &mut Vec<Decl<'a>>,
) {
    for i in 0..body.named_child_count() {
        if let Some(c) = body.named_child(i) {
            if c.kind() == "field_declaration" {
                declared_types(c, name, source, out);
            }
        }
    }
}

/// One entry per declarator of `decl` that declares `name`.
fn declared_types<'a>(
    decl: tree_sitter::Node<'a>,
    name: &str,
    source: &str,
    out: &mut Vec<Decl<'a>>,
) {
    let ty = decl.child_by_field_name("type");
    let mut cursor = decl.walk();
    for d in decl.children_by_field_name("declarator", &mut cursor) {
        match declares(d, name, source, false, 0) {
            Some(Some(pointer)) => out.push(ty.map(|t| (t, pointer))),
            Some(None) => out.push(None),
            None => {}
        }
    }
}

/// Whether declarator `d` declares `name`: Some(Some(through a pointer)) for a
/// plain declarator, Some(None) for one that binds it untyped (`auto [a, b]`).
fn declares(
    d: tree_sitter::Node,
    name: &str,
    source: &str,
    pointer: bool,
    depth: usize,
) -> Option<Option<bool>> {
    if depth > MAX_SUBTREE_DEPTH {
        return None;
    }
    match d.kind() {
        "identifier" | "field_identifier" => {
            (node_text(&d, source) == name).then_some(Some(pointer))
        }
        "pointer_declarator" => declares(
            d.child_by_field_name("declarator")?,
            name,
            source,
            true,
            depth + 1,
        ),
        "init_declarator" => declares(
            d.child_by_field_name("declarator")?,
            name,
            source,
            pointer,
            depth + 1,
        ),
        // `T& x` / `T&& x`: the declarator is the node's only named child.
        "reference_declarator" => declares(d.named_child(0)?, name, source, pointer, depth + 1),
        _ => mentions(d, name, source, depth).then_some(None),
    }
}

fn mentions(node: tree_sitter::Node, name: &str, source: &str, depth: usize) -> bool {
    if depth > MAX_SUBTREE_DEPTH {
        return false;
    }
    if node.kind() == "identifier" && node_text(&node, source) == name {
        return true;
    }
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .any(|c| mentions(c, name, source, depth + 1))
}

/// The one (type, pointer) every declaration agrees on, compared by text.
fn agreed<'a>(decls: Vec<Decl<'a>>, source: &str) -> Option<(tree_sitter::Node<'a>, bool)> {
    let first = (*decls.first()?)?;
    decls
        .iter()
        .all(|d| {
            d.is_some_and(|(t, p)| {
                p == first.1 && node_text(&t, source) == node_text(&first.0, source)
            })
        })
        .then_some(first)
}

/// The class a C++ type node names, as written (`Slice`, `leveldb::Slice`,
/// `SkipList<K, C>::Iterator`; the resolver drops template arguments). With
/// `through_arrow` (an `->` call on a non-pointer), the class behind a smart
/// pointer's `->`: `std::unique_ptr<T>` → `T`; any other class's `operator->` is
/// not followed.
fn cpp_type_name(ty: tree_sitter::Node, source: &str, through_arrow: bool) -> Option<String> {
    match ty.kind() {
        "type_identifier" if !through_arrow => Some(node_text(&ty, source).to_string()),
        // `struct Foo x;`
        "struct_specifier" | "class_specifier" if !through_arrow => {
            last_segment(ty.child_by_field_name("name")?, source)
        }
        "qualified_identifier" if !through_arrow => Some(node_text(&ty, source).to_string()),
        "qualified_identifier" => cpp_type_name(ty.child_by_field_name("name")?, source, true),
        "template_type" => {
            let name = node_text(&ty.child_by_field_name("name")?, source);
            if !through_arrow {
                return Some(name.to_string());
            }
            if !matches!(name, "unique_ptr" | "shared_ptr") {
                return None;
            }
            let args = ty.child_by_field_name("arguments")?;
            let arg = args.named_child(0)?;
            let inner = if arg.kind() == "type_descriptor" {
                arg.child_by_field_name("type")?
            } else {
                arg
            };
            cpp_type_name(inner, source, false)
        }
        _ => None,
    }
}

/// The name part of a (possibly qualified or templated) C++ name node.
fn last_segment(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "qualified_identifier" => last_segment(node.child_by_field_name("name")?, source),
        "template_type" => last_segment(node.child_by_field_name("name")?, source),
        "type_identifier" | "identifier" | "namespace_identifier" => {
            Some(node_text(&node, source).to_string())
        }
        _ => None,
    }
}

// ── JavaScript / TypeScript ─────────────────────────────────────────────────

const JS_FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function",
    "generator_function_declaration",
    "generator_function",
    "method_definition",
    "arrow_function",
];

fn js_receiver_type(function: tree_sitter::Node, source: &str) -> Option<String> {
    if function.kind() != "member_expression" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    match object.kind() {
        "this" => js_this_class(function, source),
        "identifier" => js_binding_type(function, node_text(&object, source), source),
        _ => None,
    }
}

/// The class `this` is an instance of: the nearest non-arrow function must be a
/// method of a class body (a field initializer's arrow counts too).
fn js_this_class(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cur = node.parent();
    for _ in 0..MAX_SUBTREE_DEPTH {
        let n = cur?;
        match n.kind() {
            "arrow_function" => {}
            "method_definition" => {
                let body = n.parent()?;
                return (body.kind() == "class_body")
                    .then(|| js_class_name(body.parent()?, source))
                    .flatten();
            }
            "class_body" => return js_class_name(n.parent()?, source),
            k if JS_FUNCTIONS.contains(&k) => return None,
            _ => {}
        }
        cur = n.parent();
    }
    None
}

fn js_class_name(class: tree_sitter::Node, source: &str) -> Option<String> {
    matches!(
        class.kind(),
        "class_declaration" | "abstract_class_declaration" | "class"
    )
    .then(|| class.child_by_field_name("name"))
    .flatten()
    .map(|n| node_text(&n, source).to_string())
}

/// The class of a variable or parameter `name`, from the innermost enclosing
/// scope that declares it: `new T(...)`, or a `: T` annotation.
fn js_binding_type(node: tree_sitter::Node, name: &str, source: &str) -> Option<String> {
    let mut cur = node.parent();
    for _ in 0..MAX_SUBTREE_DEPTH {
        let scope = cur?;
        if JS_FUNCTIONS.contains(&scope.kind()) || scope.kind() == "program" {
            let bindings = js_scope_bindings(scope, source);
            if let Some(found) = bindings.get(name) {
                let first = found.first()?.clone()?;
                return found
                    .iter()
                    .all(|t| t.as_deref() == Some(&first))
                    .then_some(first);
            }
        }
        cur = scope.parent();
    }
    None
}

type ScopeBindings = Rc<HashMap<String, Vec<Option<String>>>>;

/// Every name `scope` binds, each with one entry per binding: the class a
/// declarator or assignment gives it (`new T()` / `: T`), or None for a binding
/// whose class is not written down (destructuring, `for (x of …)`, `catch (x)`,
/// a function named `x`, or an assignment to `x` inside a nested function).
fn js_scope_bindings(scope: tree_sitter::Node, source: &str) -> ScopeBindings {
    if let Some(hit) = JS_SCOPES.with(|c| c.borrow().get(&scope.id()).cloned()) {
        return hit;
    }
    let mut out: HashMap<String, Vec<Option<String>>> = HashMap::new();
    if let Some(params) = scope.child_by_field_name("parameters") {
        for i in 0..params.named_child_count() {
            let Some(p) = params.named_child(i) else {
                continue;
            };
            let pattern = p.child_by_field_name("pattern").unwrap_or(p);
            if pattern.kind() == "identifier" {
                out.entry(node_text(&pattern, source).to_string())
                    .or_default()
                    .push(
                        p.child_by_field_name("type")
                            .and_then(|t| js_annotation(t, source)),
                    );
            } else {
                untyped(pattern, source, &mut out);
            }
        }
    }
    // `(x) => …` with a bare identifier parameter
    if let Some(p) = scope.child_by_field_name("parameter") {
        untyped(p, source, &mut out);
    }
    let body = if scope.kind() == "program" {
        Some(scope)
    } else {
        scope.child_by_field_name("body")
    };
    if let Some(body) = body {
        collect_js_bindings(body, source, &mut out, 0);
    }
    let out = Rc::new(out);
    JS_SCOPES.with(|c| c.borrow_mut().insert(scope.id(), out.clone()));
    out
}

/// Record every name `pattern` binds as a binding of unknown class.
fn untyped(
    pattern: tree_sitter::Node,
    source: &str,
    out: &mut HashMap<String, Vec<Option<String>>>,
) {
    for n in bound_names(pattern, source, 0) {
        out.entry(n).or_default().push(None);
    }
}

/// Bindings in a scope body, not descending into nested functions (see
/// [`js_scope_bindings`]).
fn collect_js_bindings(
    node: tree_sitter::Node,
    source: &str,
    out: &mut HashMap<String, Vec<Option<String>>>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    match node.kind() {
        "variable_declarator" => {
            if let Some(n) = node.child_by_field_name("name") {
                if n.kind() == "identifier" {
                    let annotated = node
                        .child_by_field_name("type")
                        .and_then(|t| js_annotation(t, source));
                    out.entry(node_text(&n, source).to_string())
                        .or_default()
                        .push(
                            annotated
                                .or_else(|| constructed(node.child_by_field_name("value"), source)),
                        );
                } else {
                    untyped(n, source, out);
                }
            }
        }
        // `q = new R()`; `[q] = [other]`, `({ q } = o)`
        "assignment_expression" => {
            if let Some(l) = node.child_by_field_name("left") {
                if l.kind() == "identifier" {
                    out.entry(node_text(&l, source).to_string())
                        .or_default()
                        .push(constructed(node.child_by_field_name("right"), source));
                } else {
                    untyped(l, source, out);
                }
            }
        }
        "for_in_statement" => {
            if let Some(l) = node.child_by_field_name("left") {
                untyped(l, source, out);
            }
        }
        "catch_clause" => {
            if let Some(p) = node.child_by_field_name("parameter") {
                untyped(p, source, out);
            }
        }
        _ => {}
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i) {
            if JS_FUNCTIONS.contains(&c.kind())
                || matches!(
                    c.kind(),
                    "class_declaration" | "abstract_class_declaration" | "class"
                )
            {
                // A nested function's own scope, except that assigning a name
                // there (`const swap = () => { q = new R(); }`) rebinds ours.
                if let Some(n) = c
                    .child_by_field_name("name")
                    .filter(|n| n.kind() == "identifier")
                {
                    untyped(n, source, out);
                }
                collect_assigned(c, source, out, 0);
                continue;
            }
            collect_js_bindings(c, source, out, depth + 1);
        }
    }
}

/// Every name an assignment under `node` assigns, nested functions included:
/// `x = new T()` as a binding of class `T` (a `beforeEach` that re-creates the
/// same class keeps it), anything else as one of unknown class.
fn collect_assigned(
    node: tree_sitter::Node,
    source: &str,
    out: &mut HashMap<String, Vec<Option<String>>>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if matches!(
        node.kind(),
        "assignment_expression" | "augmented_assignment_expression"
    ) {
        if let Some(l) = node.child_by_field_name("left") {
            if node.kind() == "assignment_expression" && l.kind() == "identifier" {
                out.entry(node_text(&l, source).to_string())
                    .or_default()
                    .push(constructed(node.child_by_field_name("right"), source));
            } else {
                untyped(l, source, out);
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i) {
            collect_assigned(c, source, out, depth + 1);
        }
    }
}

/// `new T(...)` → `T`, when `T` is a plain name the file does not import from
/// a package.
fn constructed(value: Option<tree_sitter::Node>, source: &str) -> Option<String> {
    value
        .filter(|v| v.kind() == "new_expression")
        .and_then(|v| v.child_by_field_name("constructor"))
        .filter(|c| c.kind() == "identifier")
        .and_then(|c| js_project_class(node_text(&c, source)))
}

/// The names a binding pattern binds (`x`, `{ x }`, `{ a: x }`, `[x]`).
fn bound_names(pattern: tree_sitter::Node, source: &str, depth: usize) -> Vec<String> {
    if depth > MAX_SUBTREE_DEPTH {
        return Vec::new();
    }
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            vec![node_text(&pattern, source).to_string()]
        }
        // Only the value side of `a: x` and the target of `x = default` bind.
        "pair_pattern" => pattern
            .child_by_field_name("value")
            .map(|v| bound_names(v, source, depth + 1))
            .unwrap_or_default(),
        "assignment_pattern" | "object_assignment_pattern" => pattern
            .child_by_field_name("left")
            .map(|l| bound_names(l, source, depth + 1))
            .unwrap_or_default(),
        // `q.x = v` / `q[i] = v` assign a property, not `q`.
        "type_annotation" | "member_expression" | "subscript_expression" => Vec::new(),
        _ => (0..pattern.named_child_count())
            .filter_map(|i| pattern.named_child(i))
            .flat_map(|c| bound_names(c, source, depth + 1))
            .collect(),
    }
}

/// `: T` / `: T<U>` → `T`. Unions, arrays, object and function types → None.
fn js_annotation(annotation: tree_sitter::Node, source: &str) -> Option<String> {
    let ty = if annotation.kind() == "type_annotation" {
        annotation.named_child(0)?
    } else {
        annotation
    };
    match ty.kind() {
        "type_identifier" => js_project_class(node_text(&ty, source)),
        "generic_type" => {
            let name = ty.child_by_field_name("name")?;
            if name.kind() != "type_identifier" {
                return None;
            }
            match node_text(&name, source) {
                // Wrappers whose members are the wrapped type's.
                "Readonly" | "Partial" | "Required" | "NonNullable" => {
                    let args = ty.child_by_field_name("type_arguments")?;
                    js_annotation(args.named_child(0)?, source)
                }
                other => js_project_class(other),
            }
        }
        _ => None,
    }
}

/// `name`, unless the file imports it from a package (`import { Request } from
/// 'express'`): a library class that happens to share a project class's name
/// must not claim that class's methods, so such a receiver stays untyped.
fn js_project_class(name: &str) -> Option<String> {
    (!super::member::js_imported_from_package(name)).then(|| name.to_string())
}

fn ancestor<'a>(node: tree_sitter::Node<'a>, kinds: &[&str]) -> Option<tree_sitter::Node<'a>> {
    let mut cur = node.parent();
    for _ in 0..MAX_SUBTREE_DEPTH * 4 {
        let n = cur?;
        if kinds.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}
