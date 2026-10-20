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
use std::collections::{HashMap, HashSet};

use super::node_text;

pub(super) const MEMBER_META: &str = crate::domain::CALL_META_MEMBER;

thread_local! {
    /// Names the current file binds by import / require. A member call on one is
    /// a module-function call (`helpers.run()`, `m.f()`), which may well target a
    /// free function, so it is not marked. Per file: reset by `reset_import_bound`.
    /// The value is the absolute module a Python import binds the name from
    /// (`import click` → `click`, `from a.b import c` → `a.b`), or the specifier
    /// a JS import / `require` names (`'express'`, `'./x'`); None for a Python
    /// relative import.
    static IMPORT_BOUND: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
    /// JS/TS local names the current file binds, somewhere, to another name's
    /// export (`b` in `import { a as b }`, `const { a: b } = require()`,
    /// `const b = require().a`). Only a prefilter: whether a call's `b` IS that
    /// binding is decided at the call by [`js_renamed_import_call`].
    static RENAMED_IMPORTS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// [`binding_in`]'s answer per scope node id and name, all but the
    /// `for (… of …)` head, which depends on where the name is read: a scope is
    /// scanned once per name, not once per call under it. Every bare JS call
    /// walks its scopes up to the binding, most of them to the program, whose
    /// top-level statements were re-read for each call (D#193: hono's full
    /// index +42.6% CPU). Per file (node ids are unique only within a tree):
    /// reset by `reset_import_bound`.
    #[allow(clippy::type_complexity)]
    static SCOPE_BINDING: RefCell<HashMap<usize, HashMap<String, Option<Option<(String, String)>>>>> =
        RefCell::new(HashMap::new());
    /// [`js_package_binding`]'s program-level answer per name: the package a
    /// top-level `var resolve = path.resolve` stands for. Per file, as above.
    static PACKAGE_MEMBER: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
}

/// Collect the file's import-bound names. MUST run once per file before its walk.
pub(super) fn reset_import_bound(root: tree_sitter::Node, source: &str, family: &str) {
    let mut names = HashMap::new();
    let mut renamed = HashSet::new();
    if matches!(family, "python" | "javascript" | "typescript" | "tsx") {
        collect(root, source, family, &mut names, 0);
    }
    if matches!(family, "javascript" | "typescript" | "tsx") {
        collect_renamed(root, source, &mut renamed, 0);
    }
    IMPORT_BOUND.with(|b| *b.borrow_mut() = names);
    RENAMED_IMPORTS.with(|r| *r.borrow_mut() = renamed);
    SCOPE_BINDING.with(|m| m.borrow_mut().clear());
    PACKAGE_MEMBER.with(|m| m.borrow_mut().clear());
}

fn collect(
    node: tree_sitter::Node,
    source: &str,
    family: &str,
    out: &mut HashMap<String, Option<String>>,
    depth: usize,
) {
    if depth > 256 {
        return;
    }
    let text = |n: tree_sitter::Node| node_text(&n, source).to_string();
    match (family, node.kind()) {
        ("python", "import_statement") | ("python", "import_from_statement") => {
            let module = node.child_by_field_name("module_name");
            // `from x import y`: y comes from x; relative (`from . import y`): None.
            let from = module.filter(|m| m.kind() == "dotted_name").map(text);
            let is_from = node.kind() == "import_from_statement";
            for i in 0..node.named_child_count() {
                let Some(c) = node.named_child(i) else {
                    continue;
                };
                if Some(c.id()) == module.map(|m| m.id()) {
                    continue; // `from x import y` binds y, not x
                }
                let origin = |path: String| if is_from { from.clone() } else { Some(path) };
                match c.kind() {
                    // `import a.b` binds `a`; `from x import a` binds `a`.
                    "dotted_name" => {
                        if let Some(first) = c.named_child(0) {
                            out.insert(text(first), origin(text(first)));
                        }
                    }
                    // `import a.b as m` binds `m` to `a.b`.
                    "aliased_import" => {
                        if let Some(alias) = c.child_by_field_name("alias") {
                            let path = c.child_by_field_name("name").map(text).unwrap_or_default();
                            out.insert(text(alias), origin(path));
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
                        out.insert(text(c), js_import_specifier(node, source));
                    }
                }
            }
        }
        (_, "import_specifier") => {
            if let Some(n) = node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
            {
                out.insert(text(n), js_import_specifier(node, source));
            }
            return;
        }
        (_, "variable_declarator") if family != "python" => {
            let value = node.child_by_field_name("value");
            let value = match value {
                Some(v) if v.kind() == "await_expression" => v.named_child(0),
                v => v,
            };
            if let Some(load) = value.filter(|v| is_module_load(*v, source)) {
                if let Some(name) = node.child_by_field_name("name") {
                    let spec = load
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                        .map(|a| {
                            node_text(&a, source)
                                .trim_matches(['"', '\'', '`'])
                                .to_string()
                        });
                    bind_pattern(name, source, &spec, out);
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

/// Every local name a renamed-import binding introduces anywhere in the file.
fn collect_renamed(node: tree_sitter::Node, source: &str, out: &mut HashSet<String>, depth: usize) {
    if depth > 256 {
        return;
    }
    match node.kind() {
        "import_specifier" => {
            if let Some((local, _, _)) = renamed_specifier(node, source) {
                out.insert(local);
            }
            return;
        }
        "variable_declarator" => {
            if let Some(name) = node.child_by_field_name("name") {
                match name.kind() {
                    "identifier" => {
                        let local = node_text(&name, source);
                        if renamed_require_binding(node, local, source).is_some() {
                            out.insert(local.to_string());
                        }
                    }
                    "object_pattern" => {
                        for i in 0..name.named_child_count() {
                            let Some(value) = name
                                .named_child(i)
                                .filter(|c| c.kind() == "pair_pattern")
                                .and_then(|p| p.child_by_field_name("value"))
                                .filter(|v| v.kind() == "identifier")
                            else {
                                continue;
                            };
                            let local = node_text(&value, source);
                            if renamed_require_binding(node, local, source).is_some() {
                                out.insert(local.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i) {
            collect_renamed(c, source, out, depth + 1);
        }
    }
}

/// `import { a as b } from 's'` (a value import, not `import type`): (`b`,
/// `a`, `s`). None when the name is not renamed.
fn renamed_specifier(spec: tree_sitter::Node, source: &str) -> Option<(String, String, String)> {
    let name = spec.child_by_field_name("name")?;
    let alias = spec.child_by_field_name("alias")?;
    if name.kind() != "identifier" || alias.kind() != "identifier" {
        return None;
    }
    let (export, local) = (node_text(&name, source), node_text(&alias, source));
    if export == local || has_type_keyword(spec) {
        return None;
    }
    let statement = ancestor(spec, "import_statement")?;
    if has_type_keyword(statement) {
        return None;
    }
    let module = statement.child_by_field_name("source")?;
    let module = node_text(&module, source).trim_matches(['"', '\'', '`']);
    (!module.is_empty()).then(|| (local.to_string(), export.to_string(), module.to_string()))
}

/// A TS `import type { … }` / `import { type A as B }`: binds no value.
fn has_type_keyword(node: tree_sitter::Node) -> bool {
    (0..node.child_count())
        .filter_map(|i| node.child(i))
        .any(|c| matches!(c.kind(), "type" | "typeof"))
}

fn ancestor<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == kind {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// The `require('s')` call a declarator's value is, with its specifier.
fn require_call(value: tree_sitter::Node, source: &str) -> Option<String> {
    if value.kind() != "call_expression"
        || value
            .child_by_field_name("function")
            .is_none_or(|f| f.kind() != "identifier" || node_text(&f, source) != "require")
    {
        return None;
    }
    let arg = value.child_by_field_name("arguments")?.named_child(0)?;
    if arg.kind() != "string" {
        return None;
    }
    let spec = node_text(&arg, source).trim_matches(['"', '\'', '`']);
    (!spec.is_empty()).then(|| spec.to_string())
}

/// The export and specifier `local` is a renamed require of in `declarator`:
/// `const { a: local } = require('s')` or `const local = require('s').a`, the
/// export `a` a different name. A nested or defaulted pattern, an awaited or
/// dynamic `import()`, and a same-named binding are not.
fn renamed_require_binding(
    declarator: tree_sitter::Node,
    local: &str,
    source: &str,
) -> Option<(String, String)> {
    let name = declarator.child_by_field_name("name")?;
    let value = declarator.child_by_field_name("value")?;
    let (export, spec) = match name.kind() {
        "identifier" if node_text(&name, source) == local => {
            if value.kind() != "member_expression" {
                return None;
            }
            let property = value.child_by_field_name("property")?;
            let spec = require_call(value.child_by_field_name("object")?, source)?;
            (node_text(&property, source).to_string(), spec)
        }
        "object_pattern" => {
            let spec = require_call(value, source)?;
            let export = (0..name.named_child_count())
                .filter_map(|i| name.named_child(i))
                .filter(|c| c.kind() == "pair_pattern")
                .find_map(|pair| {
                    let key = pair.child_by_field_name("key")?;
                    let value = pair.child_by_field_name("value")?;
                    (key.kind() == "property_identifier"
                        && value.kind() == "identifier"
                        && node_text(&value, source) == local)
                        .then(|| node_text(&key, source).to_string())
                })?;
            (export, spec)
        }
        _ => return None,
    };
    (export != local).then_some((export, spec))
}

/// The export and specifier a JS/TS bare call `b()` reaches through a renamed
/// import (D#120): `b` must be bound, by the NEAREST enclosing declaration of
/// it, to `import { a as b } from 's'`, `const { a: b } = require('s')` or
/// `const b = require('s').a` — so a parameter, a local, a function, class,
/// catch or loop variable, or a hoisted `var` named `b` in between shadows it,
/// and a rename inside one function is invisible to a sibling.
pub(super) fn js_renamed_import_call(
    call: tree_sitter::Node,
    source: &str,
    family: &str,
) -> Option<(String, String)> {
    if !matches!(family, "javascript" | "typescript" | "tsx") {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    let name = node_text(&function, source);
    if !RENAMED_IMPORTS.with(|r| r.borrow().contains(name)) {
        return None;
    }
    let mut child = call;
    while let Some(scope) = child.parent() {
        if let Some(binding) = binding_in(scope, child, name, source) {
            return binding;
        }
        child = scope;
    }
    None
}

/// JS/Node built-in globals: a member call on one (`Object.create`, `JSON.parse`)
/// runs no project code.
const JS_BUILTIN_GLOBALS: &[&str] = &[
    "Array",
    "Atomics",
    "BigInt",
    "Boolean",
    "Buffer",
    "Date",
    "Error",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "Reflect",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "WeakMap",
    "WeakSet",
    "console",
    "globalThis",
    "process",
];

/// Whether a JS/TS call is a member call on a built-in global the file does not
/// rebind (a parameter, local or import of that name makes it the file's own).
pub(super) fn js_builtin_global_call(call: tree_sitter::Node, source: &str, family: &str) -> bool {
    if !matches!(family, "javascript" | "typescript" | "tsx") {
        return false;
    }
    let Some(object) = call
        .child_by_field_name("function")
        .filter(|f| f.kind() == "member_expression")
        .and_then(|f| f.child_by_field_name("object"))
        .filter(|o| o.kind() == "identifier")
    else {
        return false;
    };
    let name = node_text(&object, source);
    if !JS_BUILTIN_GLOBALS.contains(&name) {
        return false;
    }
    let mut child = call;
    while let Some(scope) = child.parent() {
        if binding_in(scope, child, name, source).is_some() {
            return false;
        }
        child = scope;
    }
    true
}

/// Node's own modules: never a project's workspace package, so a call through
/// one (`resolve(p)` with `var resolve = require('path').resolve`) runs no
/// project code — express's `res.download` bound `View.prototype.resolve`.
const NODE_BUILTIN_MODULES: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// Whether a bare JS/TS call goes through a binding of one of Node's own
/// modules (`node:path`, `path`, `fs/promises`): no project code runs.
/// A renamed import (`const { resolve } = require('path')`) keeps its own path:
/// it is recorded as a call of the export, which the resolver binds nothing for
/// a package (`js_renamed_import_call`, D#120).
pub(super) fn js_node_builtin_call(call: tree_sitter::Node, source: &str, family: &str) -> bool {
    if js_renamed_import_call(call, source, family).is_some() {
        return false;
    }
    js_package_bound_call(call, source, family).is_some_and(|spec| is_node_builtin(&spec))
}

/// `node:path`, `path`, `fs/promises`: one of Node's own modules.
fn is_node_builtin(spec: &str) -> bool {
    spec.starts_with("node:")
        || NODE_BUILTIN_MODULES.contains(&spec.split('/').next().unwrap_or(spec))
}

/// Whether a JS/TS member call's receiver is one of Node's own modules (B9):
/// `path.resolve(p)`, `fs.promises.readFile(p)` or `require('path').join(p)`,
/// the root name's nearest binding a built-in module or a member of one. No
/// project code runs, however many project functions share the method's name.
pub(super) fn js_node_builtin_member_call(
    call: tree_sitter::Node,
    source: &str,
    family: &str,
) -> bool {
    if !matches!(family, "javascript" | "typescript" | "tsx") {
        return false;
    }
    let Some(mut root) = call
        .child_by_field_name("function")
        .filter(|f| f.kind() == "member_expression")
        .and_then(|f| f.child_by_field_name("object"))
    else {
        return false;
    };
    while root.kind() == "member_expression" {
        match root.child_by_field_name("object") {
            Some(inner) => root = inner,
            None => return false,
        }
    }
    let spec = match root.kind() {
        "identifier" => js_package_binding(call, node_text(&root, source), source),
        "call_expression" => require_call(root, source),
        _ => None,
    };
    spec.is_some_and(|s| is_node_builtin(&s))
}

/// The package a bare JS/TS call goes through (`crate::domain::CALL_Q_PACKAGE`):
/// `send(req)` whose nearest binding of `send` is the file's own `var send =
/// require('send')` / `import send from 'send'`. A parameter or local of that
/// name shadows the import, and a relative specifier is a project file, not a
/// package: neither is one.
pub(super) fn js_package_bound_call(
    call: tree_sitter::Node,
    source: &str,
    family: &str,
) -> Option<String> {
    if !matches!(family, "javascript" | "typescript" | "tsx") {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    js_package_binding(call, node_text(&function, source), source)
}

/// The package `name` stands for at `from`: its nearest binding is the file's
/// own `var send = require('send')` / `import send from 'send'`, or a member of
/// one (`var resolve = path.resolve`). None when a parameter or local shadows
/// it, or when the specifier is relative (a project file, not a package).
fn js_package_binding(from: tree_sitter::Node, name: &str, source: &str) -> Option<String> {
    let mut child = from;
    let program = loop {
        let scope = child.parent()?;
        if binding_in(scope, child, name, source).is_some() {
            break Some(scope).filter(|s| s.kind() == "program")?;
        }
        child = scope;
    };
    let package = |n: &str| {
        IMPORT_BOUND
            .with(|b| b.borrow().get(n).cloned().flatten())
            .filter(|spec| !spec.starts_with('.') && !spec.starts_with('/'))
    };
    // `var send = require('send')`, `import send from 'send'`, or a member of one:
    // `var resolve = path.resolve` with `path` a package (express's view.js
    // bound `resolve(root, name)` to its own `View.prototype.resolve`). One
    // answer per name and file: the binding is the program's.
    if let Some(hit) = PACKAGE_MEMBER.with(|m| m.borrow().get(name).cloned()) {
        return hit;
    }
    let found = package(name).or_else(|| {
        (0..program.named_child_count())
            .filter_map(|i| program.named_child(i))
            .filter(|c| matches!(c.kind(), "lexical_declaration" | "variable_declaration"))
            .flat_map(|decl| (0..decl.named_child_count()).filter_map(move |i| decl.named_child(i)))
            .filter(|d| d.kind() == "variable_declarator")
            .find(|d| {
                d.child_by_field_name("name")
                    .is_some_and(|n| n.kind() == "identifier" && node_text(&n, source) == name)
            })
            .and_then(|d| d.child_by_field_name("value"))
            .filter(|v| v.kind() == "member_expression")
            .and_then(|v| {
                let mut root = v;
                while let Some(obj) = root.child_by_field_name("object") {
                    root = obj;
                }
                // `path.resolve` through a package binding, or
                // `require('node:path').resolve` directly.
                match root.kind() {
                    "identifier" => package(node_text(&root, source)),
                    _ => require_call(root, source)
                        .filter(|spec| !spec.starts_with('.') && !spec.starts_with('/')),
                }
            })
    });
    PACKAGE_MEMBER.with(|m| m.borrow_mut().insert(name.to_string(), found.clone()));
    found
}

/// Function-like nodes: a scope whose parameters bind names and whose `var`s
/// hoist to it.
fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "generator_function"
            | "generator_function_declaration"
    )
}

/// What `scope` binds `name` to, seen from its child `from`: Some(Some(…)) a
/// renamed import, Some(None) anything else, None no binding here.
#[allow(clippy::option_option)]
fn binding_in(
    scope: tree_sitter::Node,
    from: tree_sitter::Node,
    name: &str,
    source: &str,
) -> Option<Option<(String, String)>> {
    binding_in_memo(scope, from, name, source, true)
}

/// Runtime globals beyond [`JS_BUILTIN_GLOBALS`] that a file assigns onto
/// only to stub or patch the host (`global.fetch = …` in a test).
const JS_HOST_GLOBALS: &[&str] = &[
    "Bun",
    "Deno",
    "document",
    "global",
    "location",
    "navigator",
    "self",
    "window",
];

/// Whether a function assigned onto `name.…`, seen from `from`, is API a
/// module hands out: `name` is bound at the file's top level, or bound nowhere
/// in the file and not a host global (a global another script defines, as in
/// `jQuery.fn.plugin = …`). A parameter or local of an enclosing function
/// (a test's mock) and a host global (`global`, `console`) are not (pre-tag
/// review 2026-09-29). For the node pass, which runs outside the per-file state
/// `reset_import_bound` keeps, so it takes no memo: node ids repeat across
/// trees and a memo from another file could answer for this one.
pub(crate) fn js_member_root_is_api(from: tree_sitter::Node, name: &str, source: &str) -> bool {
    let mut child = from;
    while let Some(scope) = child.parent() {
        if binding_in_memo(scope, child, name, source, false).is_some() {
            return scope.kind() == "program";
        }
        child = scope;
    }
    !JS_BUILTIN_GLOBALS.contains(&name) && !JS_HOST_GLOBALS.contains(&name)
}

#[allow(clippy::option_option)]
fn binding_in_memo(
    scope: tree_sitter::Node,
    from: tree_sitter::Node,
    name: &str,
    source: &str,
    memo: bool,
) -> Option<Option<(String, String)>> {
    // `for (const m of xs)` binds `m` in its body, not in `xs`: the one answer
    // that depends on `from`, so it stays out of the memo.
    if scope.kind() == "for_in_statement"
        && scope
            .child_by_field_name("left")
            .is_some_and(|l| l.id() != from.id() && binds(l, name, source))
    {
        return Some(None);
    }
    if !memo {
        return scope_binding(scope, name, source);
    }
    let id = scope.id();
    if let Some(hit) = SCOPE_BINDING.with(|m| {
        m.borrow()
            .get(&id)
            .and_then(|by_name| by_name.get(name))
            .cloned()
    }) {
        return hit;
    }
    let found = scope_binding(scope, name, source);
    SCOPE_BINDING.with(|m| {
        m.borrow_mut()
            .entry(id)
            .or_default()
            .insert(name.to_string(), found.clone())
    });
    found
}

/// [`binding_in_memo`] without the `for (… of …)` head: what `scope` binds
/// `name` to wherever in it the name is read.
#[allow(clippy::option_option)]
fn scope_binding(
    scope: tree_sitter::Node,
    name: &str,
    source: &str,
) -> Option<Option<(String, String)>> {
    let kind = scope.kind();
    if is_function_like(kind) {
        let params = scope
            .child_by_field_name("parameters")
            .or_else(|| scope.child_by_field_name("parameter"));
        if params.is_some_and(|p| binds(p, name, source)) {
            return Some(None);
        }
        // `function m() { m() }` as an expression names itself inside.
        if matches!(
            kind,
            "function_expression" | "function" | "generator_function"
        ) && scope
            .child_by_field_name("name")
            .is_some_and(|n| node_text(&n, source) == name)
        {
            return Some(None);
        }
    }
    // `catch (m)`.
    if kind == "catch_clause"
        && scope
            .child_by_field_name("parameter")
            .is_some_and(|p| binds(p, name, source))
    {
        return Some(None);
    }
    for i in 0..scope.named_child_count() {
        let Some(c) = scope.named_child(i) else {
            continue;
        };
        if let Some(found) = declares(c, name, source) {
            return Some(found);
        }
    }
    // A `var` anywhere in a function body or the program is hoisted to it.
    let body = if is_function_like(kind) {
        scope.child_by_field_name("body")
    } else if kind == "program" {
        Some(scope)
    } else {
        None
    };
    hoisted_var(body?, name, source, 0)
}

/// What a statement directly in a scope declares `name` as, if it does.
#[allow(clippy::option_option)]
fn declares(stmt: tree_sitter::Node, name: &str, source: &str) -> Option<Option<(String, String)>> {
    match stmt.kind() {
        "lexical_declaration" | "variable_declaration" => declarators(stmt, name, source),
        // No `function_signature`: a TS overload signature is valid only
        // beside its implementation, a `function_declaration` of the same
        // name in the same scope, or in an ambient context, where no call runs.
        "function_declaration"
        | "generator_function_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "enum_declaration" => stmt
            .child_by_field_name("name")
            .filter(|n| node_text(n, source) == name)
            .map(|_| None),
        "export_statement" => stmt
            .child_by_field_name("declaration")
            .and_then(|d| declares(d, name, source)),
        "import_statement" => {
            let mut found = None;
            let mut stack = vec![stmt];
            while let Some(n) = stack.pop() {
                match n.kind() {
                    "import_specifier" => {
                        let local = n
                            .child_by_field_name("alias")
                            .or_else(|| n.child_by_field_name("name"));
                        if local.is_some_and(|l| node_text(&l, source) == name) {
                            found = Some(
                                renamed_specifier(n, source)
                                    .map(|(_, export, module)| (export, module)),
                            );
                        }
                    }
                    "identifier" if node_text(&n, source) == name => found = Some(None),
                    "string" => {}
                    _ => {
                        for i in 0..n.named_child_count() {
                            if let Some(c) = n.named_child(i) {
                                stack.push(c);
                            }
                        }
                    }
                }
            }
            found
        }
        _ => None,
    }
}

#[allow(clippy::option_option)]
fn declarators(
    decl: tree_sitter::Node,
    name: &str,
    source: &str,
) -> Option<Option<(String, String)>> {
    for i in 0..decl.named_child_count() {
        let Some(d) = decl
            .named_child(i)
            .filter(|d| d.kind() == "variable_declarator")
        else {
            continue;
        };
        if d.child_by_field_name("name")
            .is_some_and(|n| binds(n, name, source))
        {
            return Some(renamed_require_binding(d, name, source));
        }
    }
    None
}

/// A `var` declaring `name` inside `node`, not crossing into a nested function.
#[allow(clippy::option_option)]
fn hoisted_var(
    node: tree_sitter::Node,
    name: &str,
    source: &str,
    depth: usize,
) -> Option<Option<(String, String)>> {
    if depth > 256 {
        return None;
    }
    for i in 0..node.named_child_count() {
        let Some(c) = node.named_child(i) else {
            continue;
        };
        if is_function_like(c.kind()) || matches!(c.kind(), "class_declaration" | "class") {
            continue;
        }
        if c.kind() == "variable_declaration" {
            if let Some(found) = declarators(c, name, source) {
                return Some(found);
            }
        }
        if let Some(found) = hoisted_var(c, name, source, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// Whether a binding pattern (a declarator name, a parameter list, a catch
/// parameter) binds `name`. Over-inclusive on purpose: a default value that
/// mentions `name` counts, which only leaves a call bare.
fn binds(pattern: tree_sitter::Node, name: &str, source: &str) -> bool {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            node_text(&pattern, source) == name
        }
        // `{ key: value }`: the key names a property, not a binding.
        "pair_pattern" => pattern
            .child_by_field_name("value")
            .is_some_and(|v| binds(v, name, source)),
        // TS type annotations bind nothing.
        "type_annotation" => false,
        _ => (0..pattern.named_child_count())
            .filter_map(|i| pattern.named_child(i))
            .any(|c| binds(c, name, source)),
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
fn bind_pattern(
    node: tree_sitter::Node,
    source: &str,
    spec: &Option<String>,
    out: &mut HashMap<String, Option<String>>,
) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            out.insert(node_text(&node, source).to_string(), spec.clone());
        }
        "pair_pattern" => {
            if let Some(v) = node.child_by_field_name("value") {
                bind_pattern(v, source, spec, out);
            }
        }
        _ => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    bind_pattern(c, source, spec, out);
                }
            }
        }
    }
}

/// The module specifier of the `import` statement holding `node`, unquoted.
fn js_import_specifier(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "import_statement" {
            let spec = n.child_by_field_name("source")?;
            return Some(
                node_text(&spec, source)
                    .trim_matches(['"', '\'', '`'])
                    .to_string(),
            );
        }
        cur = n.parent();
    }
    None
}

/// Whether the current JS/TS file imports `name` from a package — a specifier
/// that is not a relative or absolute path (`'express'`, `'node:fs'`).
pub(super) fn js_imported_from_package(name: &str) -> bool {
    IMPORT_BOUND.with(|b| {
        b.borrow()
            .get(name)
            .and_then(|spec| spec.as_deref())
            .is_some_and(|spec| !spec.starts_with('.') && !spec.starts_with('/'))
    })
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
        ) || IMPORT_BOUND.with(|b| b.borrow().contains_key(name))
        {
            return false;
        }
    }
    true
}

/// `{"ur":…}` metadata (`crate::domain::CALL_KEY_UNTYPED_RECEIVER`) of a Python
/// member call no other qualifier covers: `"attr"` when the receiver is an
/// attribute of `self` / `cls` (`self.serializer.tag()` — the instance's field,
/// not the instance), `"rel"` when its root is a name a relative import binds
/// (`_cv_app.get()`, `helpers.load()`). A call on `self` itself, a local, or an
/// absolute import already carries `rtype` / `member` / `module`.
pub(super) fn python_untyped_receiver_meta(
    call: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    let mut root = object;
    while root.kind() == "attribute" {
        root = root.child_by_field_name("object")?;
    }
    if root.kind() != "identifier" {
        return None;
    }
    let name = node_text(&root, source);
    let kind = if matches!(name, "self" | "cls") {
        // `self.f()` is the instance's own method; `self.x.f()` is not.
        (object.kind() == "attribute").then_some("attr")?
    } else if IMPORT_BOUND.with(|b| matches!(b.borrow().get(name), Some(None))) {
        "rel"
    } else {
        return None;
    };
    Some(serde_json::json!({ crate::domain::CALL_KEY_UNTYPED_RECEIVER: kind }).to_string())
}

/// Metadata of a Python call through a name an absolute import binds
/// (`click.echo()` → `{"q":"module","v":"click"}`): the resolver drops it when
/// that module is not the project's, since no project code can run then.
pub(super) fn python_module_call_meta(call: tree_sitter::Node, source: &str) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let mut root = function.child_by_field_name("object")?;
    while root.kind() == "attribute" {
        root = root.child_by_field_name("object")?;
    }
    if root.kind() != "identifier" {
        return None;
    }
    let module = IMPORT_BOUND.with(|b| b.borrow().get(node_text(&root, source)).cloned())??;
    Some(serde_json::json!({ "q": "module", "v": module }).to_string())
}
