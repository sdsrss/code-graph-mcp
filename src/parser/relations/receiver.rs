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
            cpp_type_name(ty, source, arrow && !pointer)
        }
        _ => None,
    }
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
            let mut found: Vec<Option<String>> = Vec::new();
            if let Some(params) = scope.child_by_field_name("parameters") {
                for i in 0..params.named_child_count() {
                    let Some(p) = params.named_child(i) else {
                        continue;
                    };
                    let pattern = p.child_by_field_name("pattern").unwrap_or(p);
                    if pattern.kind() == "identifier" && node_text(&pattern, source) == name {
                        found.push(
                            p.child_by_field_name("type")
                                .and_then(|t| js_annotation(t, source)),
                        );
                    } else if binds(pattern, name, source, 0) {
                        found.push(None);
                    }
                }
            }
            // `(x) => …` with a bare identifier parameter
            if let Some(p) = scope.child_by_field_name("parameter") {
                if node_text(&p, source) == name {
                    found.push(None);
                }
            }
            let body = if scope.kind() == "program" {
                Some(scope)
            } else {
                scope.child_by_field_name("body")
            };
            if let Some(body) = body {
                collect_js_declarators(body, name, source, &mut found, 0);
            }
            if !found.is_empty() {
                let first = found[0].clone()?;
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

/// Bindings of `name` in a scope body, not descending into nested functions:
/// a declarator or assignment gives its `new T()` / `: T` class; any other
/// binding (destructuring, `for (x of …)`, `catch (x)`, a function named `x`)
/// gives None, which the caller reads as "untyped".
fn collect_js_declarators(
    node: tree_sitter::Node,
    name: &str,
    source: &str,
    out: &mut Vec<Option<String>>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    let is_name = |n: tree_sitter::Node| n.kind() == "identifier" && node_text(&n, source) == name;
    let constructed = |v: Option<tree_sitter::Node>| {
        v.filter(|v| v.kind() == "new_expression")
            .and_then(|v| v.child_by_field_name("constructor"))
            .filter(|c| c.kind() == "identifier")
            .map(|c| node_text(&c, source).to_string())
    };
    match node.kind() {
        "variable_declarator" => {
            if let Some(n) = node.child_by_field_name("name") {
                if is_name(n) {
                    let annotated = node
                        .child_by_field_name("type")
                        .and_then(|t| js_annotation(t, source));
                    out.push(annotated.or_else(|| constructed(node.child_by_field_name("value"))));
                } else if binds(n, name, source, 0) {
                    out.push(None);
                }
            }
        }
        "assignment_expression" if node.child_by_field_name("left").is_some_and(is_name) => {
            out.push(constructed(node.child_by_field_name("right")));
        }
        "for_in_statement"
            if node
                .child_by_field_name("left")
                .is_some_and(|l| binds(l, name, source, 0)) =>
        {
            out.push(None);
        }
        "catch_clause"
            if node
                .child_by_field_name("parameter")
                .is_some_and(|p| binds(p, name, source, 0)) =>
        {
            out.push(None);
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
                if c.child_by_field_name("name").is_some_and(is_name) {
                    out.push(None);
                }
                continue;
            }
            collect_js_declarators(c, name, source, out, depth + 1);
        }
    }
}

/// Whether a binding pattern binds `name` (`x`, `{ x }`, `{ a: x }`, `[x]`).
fn binds(pattern: tree_sitter::Node, name: &str, source: &str, depth: usize) -> bool {
    if depth > MAX_SUBTREE_DEPTH {
        return false;
    }
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            node_text(&pattern, source) == name
        }
        // Only the value side of `a: x` and the target of `x = default` bind.
        "pair_pattern" => pattern
            .child_by_field_name("value")
            .is_some_and(|v| binds(v, name, source, depth + 1)),
        "assignment_pattern" | "object_assignment_pattern" => pattern
            .child_by_field_name("left")
            .is_some_and(|l| binds(l, name, source, depth + 1)),
        "type_annotation" => false,
        _ => (0..pattern.named_child_count())
            .filter_map(|i| pattern.named_child(i))
            .any(|c| binds(c, name, source, depth + 1)),
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
        "type_identifier" => Some(node_text(&ty, source).to_string()),
        "generic_type" => {
            let name = ty.child_by_field_name("name")?;
            (name.kind() == "type_identifier").then(|| node_text(&name, source).to_string())
        }
        _ => None,
    }
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
