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
    /// `const b = require().a`), or to a noise-named export of a relative path
    /// (`get` in `import { get } from './request'`, D#712). Only a prefilter:
    /// whether a call's `b` IS that binding is decided at the call by
    /// [`js_renamed_import_call`].
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
    /// Python names that hold one module everywhere in the file, and that
    /// module ([`py_module_names`]). Per file: reset by `reset_import_bound`.
    static PY_MODULE_NAMES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Python import-bound names a call through which may run a definition of
    /// the file itself ([`py_local_imports`]). Per file: reset by
    /// `reset_import_bound`.
    static PY_LOCAL_IMPORTS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// The file's Python classes for `C.m()` calls ([`py_class_names`]). Per
    /// file: reset by `reset_import_bound`.
    static PY_CLASSES: RefCell<PyClasses> = RefCell::new(PyClasses::default());
    /// The (module, attribute) of each `P.X()` call [`python_module_call_meta`]
    /// marked, in the order met: one synthetic import each
    /// ([`take_python_attr_imports`]).
    static PY_ATTR_IMPORTS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Whether the current file binds `name` by an import (see [`IMPORT_BOUND`]).
pub(super) fn is_import_bound(name: &str) -> bool {
    IMPORT_BOUND.with(|b| b.borrow().contains_key(name))
}

/// Collect the file's import-bound names. MUST run once per file before its walk.
/// `rel_path`: the file's project-relative path, `None` when no file is behind
/// the source ([`py_local_imports`]).
pub(super) fn reset_import_bound(
    root: tree_sitter::Node,
    source: &str,
    family: &str,
    rel_path: Option<&str>,
) {
    let mut names = HashMap::new();
    let mut targets = PyImportTargets::default();
    let mut renamed = HashSet::new();
    if matches!(family, "python" | "javascript" | "typescript" | "tsx") {
        collect(root, source, family, &mut names, &mut targets, 0);
    }
    if matches!(family, "javascript" | "typescript" | "tsx") {
        collect_renamed(root, source, &mut renamed, 0);
    }
    let (modules, local) = if family == "python" && names.values().any(Option::is_some) {
        (
            py_module_names(root, source, &names),
            py_local_imports(root, source, &names, &targets, rel_path),
        )
    } else {
        (HashMap::new(), HashSet::new())
    };
    PY_MODULE_NAMES.with(|m| *m.borrow_mut() = modules);
    PY_LOCAL_IMPORTS.with(|r| *r.borrow_mut() = local);
    let classes = if family == "python" {
        py_class_names(root, source)
    } else {
        PyClasses::default()
    };
    PY_CLASSES.with(|c| *c.borrow_mut() = classes);
    PY_ATTR_IMPORTS.with(|a| a.borrow_mut().clear());
    IMPORT_BOUND.with(|b| *b.borrow_mut() = names);
    RENAMED_IMPORTS.with(|r| *r.borrow_mut() = renamed);
    SCOPE_BINDING.with(|m| m.borrow_mut().clear());
    PACKAGE_MEMBER.with(|m| m.borrow_mut().clear());
}

/// What each Python import binding binds its name to, as a dotted path (D#700):
/// `import a.b` binds `a` to `a`, `import a.b as m` binds `m` to `a.b`, `from
/// p import n [as m]` binds `n` (`m`) to `p.n`; a relative import to `None`.
/// `from p import *` may bind any name, to `p.<name>`: kept in `stars`.
/// `truncated`: the walk stopped at its depth cap, so an import below it may
/// rebind any name.
#[derive(Default)]
struct PyImportTargets {
    by_name: HashMap<String, Vec<Option<String>>>,
    stars: Vec<Option<String>>,
    truncated: bool,
}

fn collect(
    node: tree_sitter::Node,
    source: &str,
    family: &str,
    out: &mut HashMap<String, Option<String>>,
    targets: &mut PyImportTargets,
    depth: usize,
) {
    if depth > 256 {
        targets.truncated = true;
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
                let target = |path: String| {
                    if is_from {
                        from.as_ref().map(|m| format!("{m}.{path}"))
                    } else {
                        Some(path)
                    }
                };
                match c.kind() {
                    // `import a.b` binds `a`; `from x import a` binds `a`.
                    "dotted_name" => {
                        if let Some(first) = c.named_child(0) {
                            out.insert(text(first), origin(text(first)));
                            targets
                                .by_name
                                .entry(text(first))
                                .or_default()
                                .push(target(text(first)));
                        }
                    }
                    // `import a.b as m` binds `m` to `a.b`.
                    "aliased_import" => {
                        if let Some(alias) = c.child_by_field_name("alias") {
                            let path = c.child_by_field_name("name").map(text).unwrap_or_default();
                            out.insert(text(alias), origin(path.clone()));
                            targets
                                .by_name
                                .entry(text(alias))
                                .or_default()
                                .push(target(path));
                        }
                    }
                    "wildcard_import" => targets.stars.push(from.clone()),
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
            collect(c, source, family, out, targets, depth + 1);
        }
    }
}

/// Every local name a renamed-import binding, or a noise-named import
/// ([`noise_named_specifier`]), introduces anywhere in the file.
fn collect_renamed(node: tree_sitter::Node, source: &str, out: &mut HashSet<String>, depth: usize) {
    if depth > 256 {
        return;
    }
    match node.kind() {
        "import_specifier" => {
            if let Some((local, _, _)) = import_call_specifier(node, source) {
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

/// The (local, export, specifier) a call through this import specifier is
/// recorded under: a renamed import (D#120), or a noise-named one (D#712).
fn import_call_specifier(
    spec: tree_sitter::Node,
    source: &str,
) -> Option<(String, String, String)> {
    renamed_specifier(spec, source).or_else(|| noise_named_specifier(spec, source))
}

/// `import { get } from './request'` (a value import of a relative path, not
/// renamed) whose name is on the JS cross-file noise list (`get`, `read`,
/// `join`): (`get`, `get`, `./request`) (D#712). The resolver drops a bare
/// call of such a name that has no same-file target as a stdlib method, so a
/// call through this import is recorded like a renamed one and binds the
/// export in the file it names. Other names resolve by name, and the
/// post-passes repoint them to what the import binds; a package's names bind
/// nothing either way.
fn noise_named_specifier(
    spec: tree_sitter::Node,
    source: &str,
) -> Option<(String, String, String)> {
    let name = spec.child_by_field_name("name")?;
    if name.kind() != "identifier" || has_type_keyword(spec) {
        return None;
    }
    let local = node_text(&name, source);
    if spec
        .child_by_field_name("alias")
        .is_some_and(|alias| node_text(&alias, source) != local)
        || !crate::domain::is_cross_file_call_noise(local, "javascript")
    {
        return None;
    }
    let statement = ancestor(spec, "import_statement")?;
    if has_type_keyword(statement) {
        return None;
    }
    let module = statement.child_by_field_name("source")?;
    let module = node_text(&module, source).trim_matches(['"', '\'', '`']);
    (module.starts_with("./") || module.starts_with("../"))
        .then(|| (local.to_string(), local.to_string(), module.to_string()))
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
/// import (D#120), or through a noise-named one (`get()` after `import { get }
/// from './request'`, D#712, [`noise_named_specifier`]): `b` must be bound, by
/// the NEAREST enclosing declaration of
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
    let function = super::helpers::call_function(&call)?;
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
    let Some(object) = super::helpers::call_function(&call)
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
    let Some(mut root) = super::helpers::call_function(&call)
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
    let function = super::helpers::call_function(&call)?;
    if function.kind() != "identifier" {
        return None;
    }
    js_package_binding(call, node_text(&function, source), source)
}

/// Whether a JS/TS `x.f()` call's receiver is, at the call, one of the file's
/// import bindings (`import { x }`, `import x`, `import * as x`, `const x =
/// require()`): the nearest binding of `x` is the program's, and the program
/// binds it by import. Such a call runs a member of another module's value,
/// never a definition of the caller's own file (D#789: hono's
/// `utf8Decoder.decode()` bound its file's `decode`). A parameter or local of
/// that name shadows the import.
pub(super) fn js_import_bound_receiver(
    call: tree_sitter::Node,
    source: &str,
    family: &str,
) -> bool {
    if !matches!(family, "javascript" | "typescript" | "tsx") {
        return false;
    }
    let Some(object) = super::helpers::call_function(&call)
        .filter(|f| f.kind() == "member_expression")
        .and_then(|f| f.child_by_field_name("object"))
        .filter(|o| o.kind() == "identifier")
    else {
        return false;
    };
    let name = node_text(&object, source);
    // A prefilter: a receiver the file does not import is a member call
    // (`is_member_call`), which never reaches here with a `Receiver`.
    if !IMPORT_BOUND.with(|b| b.borrow().contains_key(name)) {
        return false;
    }
    let mut child = call;
    while let Some(scope) = child.parent() {
        if binding_in(scope, child, name, source).is_some() {
            return scope.kind() == "program";
        }
        child = scope;
    }
    false
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
                                import_call_specifier(n, source)
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
    let Some(function) = super::helpers::call_function(&call) else {
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
///
/// A direct call `P.X()` on a name that holds one module everywhere in the
/// file ([`PY_MODULE_NAMES`]) also names the attribute (`"attr":"X"`), and is
/// recorded for a synthetic import of `X` from that module (D#518): the call
/// then binds what `from M import X` would, through the same import machinery.
///
/// `"loc":1` (D#700) marks a call through a name that may run one of the
/// file's own definitions ([`PY_LOCAL_IMPORTS`]): the file binds the name
/// otherwise, writes an attribute chain of it, or reaches its own module
/// through it. Only such a call keeps a same-file bind in the resolver.
pub(super) fn python_module_call_meta(call: tree_sitter::Node, source: &str) -> Option<String> {
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
    let module = IMPORT_BOUND.with(|b| b.borrow().get(name).cloned())??;
    let local = PY_LOCAL_IMPORTS.with(|r| r.borrow().contains(name));
    let attr = function
        .child_by_field_name("attribute")
        .filter(|a| object.kind() == "identifier" && a.kind() == "identifier")
        .map(|a| node_text(&a, source).to_string())
        .filter(|_| PY_MODULE_NAMES.with(|m| m.borrow().get(name) == Some(&module)));
    let mut meta = serde_json::json!({ "q": "module", "v": module });
    if local {
        meta["loc"] = 1.into();
    }
    let Some(attr) = attr else {
        return Some(meta.to_string());
    };
    meta["attr"] = attr.clone().into();
    PY_ATTR_IMPORTS.with(|a| {
        let mut a = a.borrow_mut();
        let key = (module.clone(), attr.clone());
        if !a.contains(&key) {
            a.push(key);
        }
    });
    Some(meta.to_string())
}

/// The (module, attribute) pairs of the file's `P.X()` calls, for the
/// synthetic imports `extract_relations_from_tree` adds (D#518).
pub(super) fn take_python_attr_imports() -> Vec<(String, String)> {
    PY_ATTR_IMPORTS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

/// The names of `bound` (the file's import bindings) that hold one module
/// everywhere in the file (D#518): every occurrence of the name is a plain
/// `import M` / `import a.b as P` binding of that one module, or the object
/// of an attribute (`P.X`). Anything else — an assignment, a parameter, a
/// loop, `with` or `except` target, a `def`, `global`, a from-import, `import
/// a.b` (which binds `a` while importing `a.b`), the name passed as a value —
/// leaves it out, so `pdb = Pdb(); pdb.set_trace()` stays the local's call.
/// Only identifiers are occurrences: an attribute name (`o.P`), a keyword
/// (`f(P=1)`) and a module path segment (`from P import x`) are not.
///
/// One cursor walk, no recursion: a deep expression cannot overflow it.
fn py_module_names(
    root: tree_sitter::Node,
    source: &str,
    bound: &HashMap<String, Option<String>>,
) -> HashMap<String, String> {
    let mut modules: HashMap<String, String> = HashMap::new();
    let mut rejected: HashSet<String> = HashSet::new();
    let mut ancestors: Vec<tree_sitter::Node> = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "identifier" {
            let name = node_text(&node, source);
            if bound.contains_key(name) && !rejected.contains(name) {
                match py_name_occurrence(node, &ancestors, source) {
                    Occurrence::Binding(m) => match modules.get(name) {
                        Some(known) if *known != m => {
                            rejected.insert(name.to_string());
                        }
                        _ => {
                            modules.insert(name.to_string(), m);
                        }
                    },
                    Occurrence::Neutral => {}
                    Occurrence::Other => {
                        rejected.insert(name.to_string());
                    }
                }
            }
        }
        if cursor.goto_first_child() {
            ancestors.push(node);
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                modules.retain(|n, _| !rejected.contains(n));
                return modules;
            }
            ancestors.pop();
        }
    }
}

/// The names of `bound` (the file's import bindings) a call through which may
/// run a definition of the file itself (D#700):
/// - some occurrence of the name is neither inside an import statement nor a
///   read ([`py_import_name_is_read`]): every name binding Python has — a
///   parameter, `def`, `class`, any assignment or loop target, `as`, walrus,
///   `del`, `global`, a match capture, `type` — is no read;
/// - an attribute chain rooted at the name sits where no read does (`m.f = g`,
///   `del m.f`, `m.a.f += 1`, `for m.i in …`);
/// - an occurrence may reach the file's own module: what one of the name's
///   imports binds, followed by the occurrence's attribute chain, has a
///   leading part that may be the module the file is (`import a` and
///   `a.mod.go()` in `a/mod.py`; `from c import mod` and `mod.go()` in
///   `c/mod.py`), or one of the imports is relative. Without `rel_path`, or
///   for a file no `.py`/`.pyi` name makes a module, this leg is skipped; an
///   import below `collect`'s depth cap takes every name.
///
/// A module patched any other way — through another name (`alias = m;
/// alias.f = g`), a segment of an import's path, a `__getattr__` hook,
/// `setattr`, another file — is not traced (review round 3 of D#700: the
/// cases are open-ended, and tracing one slice kept only wrong edges on
/// seven corpora).
///
/// One cursor walk, no recursion: a deep expression cannot overflow it.
fn py_local_imports(
    root: tree_sitter::Node,
    source: &str,
    bound: &HashMap<String, Option<String>>,
    targets: &PyImportTargets,
    rel_path: Option<&str>,
) -> HashSet<String> {
    // The file's module as dotted segments under the project root; any suffix
    // of them may be the module under some import root. A file no `.py` or
    // `.pyi` name gives is no module anything can import.
    let caller: Option<Vec<&str>> = rel_path
        .and_then(|p| {
            ["/__init__.py", "/__init__.pyi", ".py", ".pyi"]
                .iter()
                .find_map(|suffix| p.strip_suffix(suffix))
        })
        .map(|stem| stem.split('/').collect());
    let mut local: HashSet<String> = HashSet::new();
    // Per name, the attribute chains (as leading segments) through which an
    // occurrence reaches the file's own module; a name that reaches it with no
    // chain at all is local outright.
    let mut rests: HashMap<&str, Vec<&[&str]>> = HashMap::new();
    if let Some(caller) = &caller {
        if targets.truncated {
            return bound.keys().cloned().collect();
        }
        // `from p import *` binds a name to `p.<name>`. A leading part of `p`
        // alone that is a trailing part of `caller` reaches the file whatever
        // the name; past `p`, the name is the next link, so only a name that
        // is a segment of `caller` can go on to reach it.
        let mut starred: HashMap<&str, Vec<Option<String>>> = HashMap::new();
        for star in &targets.stars {
            let Some(module) = star else {
                return bound.keys().cloned().collect();
            };
            let segments: Vec<&str> = module.split('.').collect();
            if (1..=segments.len().min(caller.len())).any(|k| caller.ends_with(&segments[..k])) {
                return bound.keys().cloned().collect();
            }
            for segment in caller {
                if bound.contains_key(*segment) {
                    starred
                        .entry(segment)
                        .or_default()
                        .push(Some(format!("{module}.{segment}")));
                }
            }
        }
        for name in bound.keys() {
            let own = targets.by_name.get(name).into_iter().flatten();
            let starred = starred.get(name.as_str()).into_iter().flatten();
            match py_reach_of(own.chain(starred).cloned(), caller) {
                None => {
                    local.insert(name.clone());
                }
                Some(r) if !r.is_empty() => {
                    rests.insert(name, r);
                }
                Some(_) => {}
            }
        }
    }
    let no_classes: HashSet<usize> = HashSet::new();
    // `tops[i]`: the index of the nearest ancestor at or above `i` that is no
    // container, as `py_class_name_is_read` expects.
    let mut ancestors: Vec<tree_sitter::Node> = Vec::new();
    let mut tops: Vec<usize> = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "identifier" {
            let name = node_text(&node, source);
            if bound.contains_key(name)
                && !local.contains(name)
                && !py_in_import(&ancestors)
                && (!py_import_name_is_read(node, &ancestors, &tops, &no_classes)
                    || py_attribute_chain_written(node, &ancestors, &tops, &no_classes)
                    || rests.get(name).is_some_and(|rests| {
                        let longest = rests.iter().map(|r| r.len()).max().unwrap_or(0);
                        let chain = py_chain_segments(node, &ancestors, source, longest);
                        rests.iter().any(|r| chain.starts_with(r))
                    }))
            {
                local.insert(name.to_string());
            }
        }
        if cursor.goto_first_child() {
            let i = ancestors.len();
            tops.push(if py_is_container(node.kind()) && i > 0 {
                tops[i - 1]
            } else {
                i
            });
            ancestors.push(node);
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return local;
            }
            ancestors.pop();
            tops.pop();
        }
    }
}

/// The attribute names after `node` in the chain it is the object of (through
/// parentheses), at most `limit` of them: `a.mod.go` → `["mod", "go"]`.
fn py_chain_segments<'s>(
    node: tree_sitter::Node,
    ancestors: &[tree_sitter::Node],
    source: &'s str,
    limit: usize,
) -> Vec<&'s str> {
    let (mut at, mut object, mut segments) = (ancestors.len(), node, Vec::new());
    while segments.len() < limit {
        let Some(&parent) = at.checked_sub(1).map(|i| &ancestors[i]) else {
            break;
        };
        match parent.kind() {
            "parenthesized_expression" => {}
            "attribute" if is_node(parent.child_by_field_name("object"), object) => {
                match parent.child_by_field_name("attribute") {
                    Some(attr) => segments.push(node_text(&attr, source)),
                    None => break,
                }
            }
            _ => break,
        }
        at -= 1;
        object = parent;
    }
    segments
}

/// Where a name with these import `targets` reaches the file's own module
/// (D#700). `caller` is the file's dotted segments under the project root;
/// under some import root the file is the module any trailing part of them
/// names. `None`: the name reaches it whatever follows it — a target (or a
/// leading part of one) is such a trailing part, or a target is unknown (a
/// relative import). Otherwise the chains that do: a target is a run of
/// `caller` that the rest of `caller` follows (`a` in `a/mod.py` needs `.mod`).
fn py_reach_of<'c>(
    targets: impl Iterator<Item = Option<String>>,
    caller: &'c [&'c str],
) -> Option<Vec<&'c [&'c str]>> {
    let mut rests = Vec::new();
    for target in targets {
        let target = target?;
        let target: Vec<&str> = target.split('.').collect();
        let d = caller.len();
        if (1..=target.len().min(d)).any(|k| caller.ends_with(&target[..k])) {
            return None;
        }
        let m = target.len();
        for i in (0..d).take_while(|i| i + m < d) {
            if caller[i..i + m] == target[..] {
                rests.push(&caller[i + m..]);
            }
        }
    }
    // Each rest is a trailing part of `caller`: one per length.
    rests.sort_by_key(|rest| rest.len());
    rests.dedup_by_key(|rest| rest.len());
    Some(rests)
}

/// Whether the attribute chain `node` (a read name) is the object of — through
/// parentheses, `(m).a.f` — ends where no read is, as a target (D#700).
fn py_attribute_chain_written(
    node: tree_sitter::Node,
    ancestors: &[tree_sitter::Node],
    tops: &[usize],
    no_classes: &HashSet<usize>,
) -> bool {
    let (mut at, mut object, mut chained) = (ancestors.len(), node, false);
    while let Some(&parent) = at.checked_sub(1).map(|i| &ancestors[i]) {
        match parent.kind() {
            "parenthesized_expression" => {}
            "attribute" if is_node(parent.child_by_field_name("object"), object) => {
                chained = true;
            }
            _ => break,
        }
        at -= 1;
        object = parent;
    }
    chained && !py_import_name_is_read(object, &ancestors[..at], &tops[..at], no_classes)
}

/// [`py_class_name_is_read`], plus a parameter's default value (`def f(x=m.X)`,
/// evaluated where the `def` runs): only for [`py_local_imports`], so the
/// `C.m()` rule keeps its own list. The parameter's name is a binding.
fn py_import_name_is_read(
    node: tree_sitter::Node,
    ancestors: &[tree_sitter::Node],
    tops: &[usize],
    no_classes: &HashSet<usize>,
) -> bool {
    if py_class_name_is_read(node, ancestors, tops, no_classes) {
        return true;
    }
    let (mut child, mut at) = (node, ancestors.len());
    if at > 0 && py_is_container(ancestors[at - 1].kind()) {
        at = tops[at - 1] + 1;
        child = ancestors[at];
    }
    at.checked_sub(1)
        .map(|i| ancestors[i])
        .is_some_and(|parent| {
            matches!(
                parent.kind(),
                "default_parameter" | "typed_default_parameter"
            ) && is_node(parent.child_by_field_name("value"), child)
        })
}

/// What one identifier occurrence does to its name, for [`py_module_names`].
enum Occurrence {
    /// A plain import binds the name to this module.
    Binding(String),
    /// Not an occurrence of the variable, or the object of an attribute.
    Neutral,
    /// Anything else: the name is no module name.
    Other,
}

fn py_name_occurrence(
    node: tree_sitter::Node,
    ancestors: &[tree_sitter::Node],
    source: &str,
) -> Occurrence {
    let is = |field: Option<tree_sitter::Node>| field.is_some_and(|f| f.id() == node.id());
    let Some(&parent) = ancestors.last() else {
        return Occurrence::Other;
    };
    let grandparent = ancestors.len().checked_sub(2).map(|i| ancestors[i]);
    match parent.kind() {
        "attribute" if is(parent.child_by_field_name("object")) => Occurrence::Neutral,
        "attribute" if is(parent.child_by_field_name("attribute")) => Occurrence::Neutral,
        "keyword_argument" if is(parent.child_by_field_name("name")) => Occurrence::Neutral,
        "dotted_name" => {
            let first = is(parent.named_child(0));
            match grandparent.map(|g| g.kind()) {
                Some("import_statement") if first && parent.named_child_count() == 1 => {
                    Occurrence::Binding(node_text(&node, source).to_string())
                }
                Some("import_statement") if first => Occurrence::Other,
                Some("import_statement" | "aliased_import" | "relative_import") => {
                    Occurrence::Neutral
                }
                Some("import_from_statement")
                    if !first
                        || grandparent.is_some_and(|g| {
                            is_node(g.child_by_field_name("module_name"), parent)
                        }) =>
                {
                    Occurrence::Neutral
                }
                _ => Occurrence::Other,
            }
        }
        "aliased_import" if is(parent.child_by_field_name("alias")) => {
            match (
                grandparent.map(|g| g.kind()),
                parent.child_by_field_name("name"),
            ) {
                (Some("import_statement"), Some(path)) => {
                    Occurrence::Binding(node_text(&path, source).to_string())
                }
                _ => Occurrence::Other,
            }
        }
        _ => Occurrence::Other,
    }
}

fn is_node(field: Option<tree_sitter::Node>, node: tree_sitter::Node) -> bool {
    field.is_some_and(|f| f.id() == node.id())
}

/// Metadata of a Python call `C.m()` on a class the file defines at module
/// level and never rebinds (D#370). Python looks `m` up along `C`'s MRO, so the
/// call runs the first class in it that defines `m` — never another class of
/// the file and never a subclass's override. When that class is decided inside
/// this file ([`py_class_lookup`]), the call carries the `super()` qualifier
/// naming it, which binds that class's own method (`recv_type_targets` without
/// dispatch, the caller's file's among same-named classes; a class that also
/// nests a `def m` in another method, which the index names `X.m` too, is left
/// undecided), and `"own":1`: no other file can move that answer. Anything the
/// file cannot decide keeps the old untyped member call. Before, such a call
/// bound every same-named method of the file (`Widget.__init__(self)` → every
/// `__init__`).
pub(super) fn python_class_call_meta(call: tree_sitter::Node, source: &str) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    let method = function.child_by_field_name("attribute")?;
    if object.kind() != "identifier" || method.kind() != "identifier" {
        return None;
    }
    let (class, method) = (node_text(&object, source), node_text(&method, source));
    // Python mangles a `__x` name in a class body (`C.__m` is `C._A__m` inside
    // class `A`, and `def __m` in C is `_C__m`): it is not the name written,
    // and a method spelled mangled (`_C__m`) names a def the index calls `__m`.
    if [class, method]
        .iter()
        .any(|n| n.starts_with("__") && !n.ends_with("__"))
        || (method.starts_with('_') && !method.starts_with("__") && method[1..].contains("__"))
    {
        return None;
    }
    let owner = PY_CLASSES.with(|c| {
        let state = &mut *c.borrow_mut();
        let key = (class.to_string(), method.to_string());
        if let Some(hit) = state.lookups.get(&key) {
            return hit.clone();
        }
        if state.linear.is_none() && state.classes.contains_key(class) {
            state.linear = Some(py_linearize(&state.classes, &state.rebound));
        }
        let hit = py_class_lookup(state, class, method);
        state.lookups.insert(key, hit.clone());
        hit
    })?;
    // `recv_type_targets` reads a `std` path as Rust's standard library.
    if owner == "std" {
        return None;
    }
    // `own`: decided by this file alone — the class-structure drift that
    // re-reads typed calls after another file's class changes leaves it out.
    Some(serde_json::json!({ "q": "super", "v": owner, "own": 1 }).to_string())
}

/// One module-level class of the current Python file, for [`py_class_lookup`].
struct PyClass {
    /// Where its `class` statement starts: a base names it only below it.
    at: usize,
    /// Its positional bases as written.
    bases: Vec<PyBase>,
    /// `metaclass=…`.
    metaclass: Option<PyBase>,
    /// Any other class keyword (it goes to `__init_subclass__`, which may bind
    /// anything) or a `**kw` (which may hold a metaclass).
    keywords: bool,
    /// Names the body binds once, by a `def` directly in it.
    methods: HashSet<String>,
    /// Every other name the body may bind: a second `def`, a nested class, an
    /// assignment, anything under `if` / `for` / `try` / `with` in the body, a
    /// walrus in a `def`'s, a lambda's or a nested class's header
    /// ([`py_header_walrus`]), a name `__slots__` lists.
    other: HashSet<String>,
    /// The body may bind names nothing above shows: it names `locals`, `vars`,
    /// `exec` or `eval` (in a statement or a header), or sets `__slots__` to
    /// anything but string literals.
    opaque: bool,
}

/// A base (or metaclass) expression of a class statement.
#[derive(Clone)]
enum PyBase {
    /// `Base`.
    Name(String),
    /// `mod.Base`, `Generic[T]`: its root name and its whole text.
    Rooted(String, String),
    /// Anything else (`mk(A)`, `*bases`).
    Other,
}

/// The current file's Python classes for [`python_class_call_meta`]. Per file:
/// reset by `reset_import_bound`.
#[derive(Default)]
struct PyClasses {
    /// Module-level classes whose name holds the class everywhere in the file
    /// ([`py_class_names`]).
    classes: HashMap<String, PyClass>,
    /// Names a base or metaclass expression starts with that the file binds
    /// other than by one import: a class it defines but that does not qualify,
    /// an assignment, a second import, any other binding. Such a base may be
    /// (or derive from) a class of the file the rule cannot see.
    rebound: HashSet<String>,
    /// `(C, m)` where the file assigns or deletes `C.m` (`C.m = f`, `del (C).m`,
    /// `*C.m, = …`).
    patched: HashSet<(String, String)>,
    /// `(C, m)` where a `def m` sits inside C's body below its own level (in a
    /// method, a nested class): the index names it `C.m` as well.
    nested: HashSet<(String, String)>,
    /// Their MROs, computed by the first lookup on one of them.
    linear: Option<PyLinear>,
    /// [`py_class_lookup`]'s answers.
    lookups: HashMap<(String, String), Option<String>>,
}

/// Longest MRO [`py_linearize`] decides, not counting `object`; a class with a
/// longer one, or with as many bases, stays undecided.
const PY_MRO_CAP: usize = 64;

/// The C3 linearizations of a file's classes ([`py_linearize`]).
struct PyLinear {
    /// The classes in source order: a class's index is its id in an MRO. Id
    /// `names.len()` is `object`; ids past it are bases the file does not
    /// define (an import, a builtin), one per text.
    names: Vec<String>,
    /// Per class: its MRO as ids, itself first and `object` last — None where
    /// Python refuses the class (an inconsistent order, a duplicate base) or
    /// where it is past [`PY_MRO_CAP`] — whether a base in its hierarchy is one
    /// the rule cannot see, or a metaclass is one of the file's or two differ
    /// (`blind`), and the one metaclass from outside the file it has or
    /// inherits, as a leaf id (`ABCMeta`): taken to answer nothing before the
    /// class's own body does.
    of: HashMap<String, PyLinearClass>,
}

struct PyLinearClass {
    mro: Option<Vec<usize>>,
    blind: bool,
    meta: Option<usize>,
}

/// The class whose `m` a call `C.m()` runs, decided inside this file: the
/// first class of `C`'s MRO (Python's C3 linearization) whose body defines `m`
/// with one `def` and binds it no other way. None — keep the old resolution —
/// when `C` is not a class of [`py_class_names`], when Python refuses `C` or
/// its MRO is past [`PY_MRO_CAP`], when a base in its hierarchy is one the rule
/// cannot see ([`PyLinear`]), carries another class keyword or a metaclass of
/// the file, or inherits two metaclasses, when a class of its MRO has
/// `__bases__` or `__class__` assigned, when the MRO reaches `object` or a
/// base the file does not define before the `def`, or a class binding `m` some
/// other way, assigning or deleting it from outside, writing its namespace
/// opaquely, or nesting a `def m`.
fn py_class_lookup(state: &PyClasses, class: &str, method: &str) -> Option<String> {
    let linear = state.linear.as_ref()?;
    let lin = linear.of.get(class)?;
    if lin.blind {
        return None;
    }
    let mro = lin.mro.as_ref()?;
    let patched = |name: &str, attr: &str| {
        state
            .patched
            .contains(&(name.to_string(), attr.to_string()))
    };
    if mro
        .iter()
        .filter_map(|&x| linear.names.get(x))
        .any(|name| patched(name, "__bases__") || patched(name, "__class__"))
    {
        return None;
    }
    for &x in mro {
        let name = linear.names.get(x)?; // `object`, or a base the file does not define
        let k = &state.classes[name];
        let key = (name.to_string(), method.to_string());
        if k.opaque || state.patched.contains(&key) || k.other.contains(method) {
            return None;
        }
        if k.methods.contains(method) {
            return (!state.nested.contains(&key)).then(|| name.clone());
        }
    }
    None
}

/// [`PyLinear`] for the file's classes: one pass in source order, each class
/// merged once from its bases' MROs (no recursion). A base names one of the
/// classes only when that class is defined above it, which is what the name
/// holds when the `class` statement runs, so there is no cycle. A base the
/// file does not bind other than by one import (an import, a builtin, a class
/// defined further down: a star import's then), or a dotted or subscripted
/// one rooted at such a name, is a leaf: one per text, its MRO itself and
/// `object`. Any other base makes the class `blind`, as does a class keyword
/// other than `metaclass=`, and so does a blind base.
fn py_linearize(classes: &HashMap<String, PyClass>, rebound: &HashSet<String>) -> PyLinear {
    let mut order: Vec<(&String, &PyClass)> = classes.iter().collect();
    order.sort_by_key(|(_, c)| c.at);
    let names: Vec<String> = order.iter().map(|(n, _)| (*n).clone()).collect();
    let object = names.len();
    let mut leaves: HashMap<String, usize> = HashMap::new();
    let mut id: HashMap<&str, usize> = HashMap::new();
    let mut out: Vec<PyLinearClass> = Vec::with_capacity(names.len());
    for (i, (name, c)) in order.iter().enumerate() {
        let mut blind = c.keywords;
        // A base as an id: a class above, `object`, a leaf; None when blind.
        let mut resolve = |b: &PyBase| -> Option<usize> {
            let (root, text) = match b {
                PyBase::Name(n) => {
                    if let Some(&j) = id.get(n.as_str()) {
                        return Some(j);
                    }
                    (n, n)
                }
                PyBase::Rooted(root, text) => {
                    if classes.contains_key(root) {
                        return None; // `G[int]`, `C.Inner`: a class of the file
                    }
                    (root, text)
                }
                PyBase::Other => return None,
            };
            if rebound.contains(root) {
                return None;
            }
            if text == "object" && !classes.contains_key("object") {
                return Some(object);
            }
            let next = object + 1 + leaves.len();
            Some(*leaves.entry(text.clone()).or_insert(next))
        };
        let bases: Vec<usize> = c
            .bases
            .iter()
            .filter_map(|b| {
                let r = resolve(b);
                blind |= r.is_none();
                r
            })
            .collect();
        // A metaclass of the file can drop or replace `m` (`__new__`,
        // `__prepare__`, `__init__`): blind. One from outside is a leaf id,
        // kept once per class (two different ones: blind).
        let mut meta: Option<usize> = None;
        match &c.metaclass {
            None => {}
            Some(PyBase::Name(n)) if id.contains_key(n.as_str()) => blind = true,
            Some(m) => match resolve(m) {
                Some(leaf) => meta = Some(leaf),
                None => blind = true,
            },
        }
        for &b in &bases {
            if b < i {
                blind |= out[b].blind;
                match (meta, out[b].meta) {
                    (None, inherited) => meta = inherited,
                    (Some(x), Some(y)) if x != y => blind = true,
                    _ => {}
                }
            }
        }
        let mut seen = HashSet::new();
        let mro = if blind || bases.len() >= PY_MRO_CAP || !bases.iter().all(|b| seen.insert(*b)) {
            None
        } else {
            let mut lists: Vec<Vec<usize>> = Vec::with_capacity(bases.len() + 1);
            let mut known = true;
            for &b in &bases {
                match b {
                    b if b < i => match &out[b].mro {
                        Some(m) => lists.push(m.clone()),
                        None => known = false,
                    },
                    b if b == object => lists.push(vec![object]),
                    b => lists.push(vec![b, object]),
                }
            }
            lists.push(if bases.is_empty() {
                vec![object]
            } else {
                bases.clone()
            });
            if known {
                py_c3(i, lists)
            } else {
                None
            }
        };
        out.push(PyLinearClass { mro, blind, meta });
        id.insert(name.as_str(), i);
    }
    let of = names.iter().cloned().zip(out).collect();
    PyLinear { names, of }
}

/// C3 merge: `class` followed by the merge of `lists` (each base's MRO, then
/// the bases). None on an inconsistent order or past [`PY_MRO_CAP`] (plus
/// `object`). Linear: each step takes the first head no list holds in its
/// tail, from a count of tail occurrences kept as heads advance.
fn py_c3(class: usize, lists: Vec<Vec<usize>>) -> Option<Vec<usize>> {
    let mut tail: HashMap<usize, usize> = HashMap::new();
    for l in &lists {
        for &x in l.iter().skip(1) {
            *tail.entry(x).or_default() += 1;
        }
    }
    let mut at = vec![0usize; lists.len()];
    let mut out = vec![class];
    loop {
        let mut live = false;
        let mut head = None;
        for (l, &p) in lists.iter().zip(&at) {
            if p < l.len() {
                live = true;
                if tail.get(&l[p]).copied().unwrap_or(0) == 0 {
                    head = Some(l[p]);
                    break;
                }
            }
        }
        let Some(head) = head else {
            return (!live).then_some(out);
        };
        out.push(head);
        if out.len() > PY_MRO_CAP + 1 {
            return None;
        }
        for (l, p) in lists.iter().zip(at.iter_mut()) {
            if *p < l.len() && l[*p] == head {
                *p += 1;
                if let Some(next) = l.get(*p) {
                    if let Some(n) = tail.get_mut(next) {
                        *n -= 1;
                    }
                }
            }
        }
    }
}

/// The module-level classes of a Python file whose name holds that one class
/// everywhere in it, with each one's bases and body bindings, and the class
/// attributes the file assigns from outside. A class qualifies when the file
/// defines it once, as a plain statement of the module (not under `if` /
/// `try` / `with`, not decorated — a decorator may return something else),
/// no `from … import *` follows it, and every other identifier occurrence of
/// the name only reads it ([`py_class_name_is_read`]). Anything that binds the
/// name — an assignment, a parameter, a loop / `with` / `except … as` /
/// match-capture target, an import, `global`, `del`, a `def` or a class of
/// that name in another scope, a type parameter — leaves the class out, so
/// `C.m()` keeps its old resolution wherever `C` might be something else.
///
/// Cursor walks, no recursion: a deep expression cannot overflow them, and
/// each node is visited a bounded number of times.
fn py_class_names(root: tree_sitter::Node, source: &str) -> PyClasses {
    // Class definitions in module scope (the walk does not enter a def, a
    // class or a lambda) and the last wildcard import.
    let mut defs: HashMap<String, Vec<usize>> = HashMap::new();
    let mut found: HashMap<String, PyClass> = HashMap::new();
    let mut module_defs: HashSet<usize> = HashSet::new();
    let mut rejected: HashSet<String> = HashSet::new();
    let mut last_star: Option<usize> = None;
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let enter = match node.kind() {
            "class_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let name = node_text(&name, source).to_string();
                    if node.parent().is_some_and(|p| p.kind() == "module") {
                        defs.entry(name.clone())
                            .or_default()
                            .push(node.start_byte());
                        module_defs.insert(node.id());
                        found.insert(name, py_class_shape(node, source));
                    } else {
                        rejected.insert(name);
                    }
                }
                false
            }
            "function_definition" | "lambda" => false,
            "wildcard_import" => {
                last_star = last_star.max(Some(node.start_byte()));
                false
            }
            _ => true,
        };
        if enter && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break;
            }
        }
        if cursor.node().id() == root.id() {
            break;
        }
    }
    for (name, at) in &defs {
        if at.len() != 1 || last_star.is_some_and(|s| s > at[0]) {
            rejected.insert(name.clone());
        }
    }
    let mut state = PyClasses::default();
    if defs.keys().all(|n| rejected.contains(n)) {
        return state;
    }
    // The names base and metaclass expressions start with; any class the file
    // defines that does not qualify is rebound by definition.
    let mut roots: HashSet<String> = HashSet::new();
    for c in found.values() {
        for b in c.bases.iter().chain(c.metaclass.iter()) {
            if let PyBase::Name(r) | PyBase::Rooted(r, _) = b {
                roots.insert(r.clone());
            }
        }
    }
    let mut imports: HashMap<String, usize> = HashMap::new();
    // Every occurrence of a candidate or root name, `C.m = …` on a candidate,
    // and every `def` below a candidate's own level. `tops[i]`: the index of
    // the nearest ancestor at or above `i` that is no container.
    let mut ancestors: Vec<tree_sitter::Node> = Vec::new();
    let mut tops: Vec<usize> = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        match node.kind() {
            "identifier" => {
                let name = node_text(&node, source);
                let candidate = defs.contains_key(name) && !rejected.contains(name);
                if candidate || roots.contains(name) {
                    let read = py_class_name_is_read(node, &ancestors, &tops, &module_defs);
                    if !read {
                        if py_in_import(&ancestors) {
                            *imports.entry(name.to_string()).or_default() += 1;
                        } else {
                            state.rebound.insert(name.to_string());
                        }
                    }
                    if candidate && !read {
                        rejected.insert(name.to_string());
                    } else if candidate {
                        py_patch_of(
                            node,
                            name,
                            &ancestors,
                            &tops,
                            &module_defs,
                            source,
                            &mut state,
                        );
                    }
                }
            }
            "function_definition" => {
                // ancestors: module, class_definition, block[, decorated_definition].
                let direct = ancestors.len() == 3
                    || (ancestors.len() == 4 && ancestors[3].kind() == "decorated_definition");
                let class = ancestors
                    .get(1)
                    .filter(|c| module_defs.contains(&c.id()))
                    .and_then(|c| c.child_by_field_name("name"));
                if let (Some(class), Some(def), false) =
                    (class, node.child_by_field_name("name"), direct)
                {
                    state.nested.insert((
                        node_text(&class, source).to_string(),
                        node_text(&def, source).to_string(),
                    ));
                }
            }
            _ => {}
        }
        if cursor.goto_first_child() {
            let i = ancestors.len();
            tops.push(if py_is_container(node.kind()) && i > 0 {
                tops[i - 1]
            } else {
                i
            });
            ancestors.push(node);
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                for (name, n) in imports {
                    if n > 1 {
                        state.rebound.insert(name);
                    }
                }
                for name in defs.keys().chain(rejected.iter()) {
                    if !found.contains_key(name) || rejected.contains(name) {
                        state.rebound.insert(name.clone());
                    }
                }
                state.classes = found
                    .into_iter()
                    .filter(|(name, _)| !rejected.contains(name))
                    .collect();
                return state;
            }
            ancestors.pop();
            tops.pop();
        }
    }
}

/// Records `C.m = f`, `del C.m` and the like for one read occurrence of a
/// candidate class name, also through parentheses and `*` (`(C).m`, `*C.m, =`).
fn py_patch_of(
    node: tree_sitter::Node,
    name: &str,
    ancestors: &[tree_sitter::Node],
    tops: &[usize],
    module_defs: &HashSet<usize>,
    source: &str,
    state: &mut PyClasses,
) {
    let (mut at, mut object) = (ancestors.len(), node);
    while at > 0
        && matches!(
            ancestors[at - 1].kind(),
            "parenthesized_expression" | "list_splat"
        )
    {
        at -= 1;
        object = ancestors[at];
    }
    let Some(attr) = at.checked_sub(1).map(|i| ancestors[i]) else {
        return;
    };
    if attr.kind() == "attribute"
        && is_node(attr.child_by_field_name("object"), object)
        && !py_class_name_is_read(attr, &ancestors[..at - 1], &tops[..at - 1], module_defs)
    {
        if let Some(field) = attr.child_by_field_name("attribute") {
            state
                .patched
                .insert((name.to_string(), node_text(&field, source).to_string()));
        }
    }
}

/// Whether an identifier with these ancestors sits in an import statement.
fn py_in_import(ancestors: &[tree_sitter::Node]) -> bool {
    ancestors.iter().rev().take(3).any(|a| {
        matches!(
            a.kind(),
            "import_statement" | "import_from_statement" | "aliased_import"
        )
    })
}

/// Node kinds [`py_class_name_is_read`] climbs before judging an occurrence.
fn py_is_container(kind: &str) -> bool {
    matches!(
        kind,
        "expression_list"
            | "tuple"
            | "list"
            | "set"
            | "dictionary"
            | "parenthesized_expression"
            | "list_splat"
            | "dictionary_splat"
    )
}

/// A base or metaclass expression as [`PyBase`].
fn py_base(node: tree_sitter::Node, source: &str) -> PyBase {
    let mut n = node;
    loop {
        let next = match n.kind() {
            "identifier" if n.id() == node.id() => {
                return PyBase::Name(node_text(&n, source).to_string());
            }
            "identifier" => {
                return PyBase::Rooted(
                    node_text(&n, source).to_string(),
                    node_text(&node, source).to_string(),
                );
            }
            "attribute" => n.child_by_field_name("object"),
            "subscript" => n.child_by_field_name("value"),
            _ => None,
        };
        match next {
            Some(x) => n = x,
            None => return PyBase::Other,
        }
    }
}

/// A module-level class's bases and what its body binds, for [`PyClass`].
fn py_class_shape(class: tree_sitter::Node, source: &str) -> PyClass {
    let mut shape = PyClass {
        at: class.start_byte(),
        bases: Vec::new(),
        metaclass: None,
        keywords: false,
        methods: HashSet::new(),
        other: HashSet::new(),
        opaque: false,
    };
    if let Some(args) = class.child_by_field_name("superclasses") {
        for i in 0..args.named_child_count() {
            let Some(b) = args.named_child(i) else {
                continue;
            };
            match b.kind() {
                "keyword_argument" => {
                    let key = b.child_by_field_name("name").map(|k| node_text(&k, source));
                    match (key, b.child_by_field_name("value")) {
                        (Some("metaclass"), Some(v)) if shape.metaclass.is_none() => {
                            shape.metaclass = Some(py_base(v, source));
                        }
                        _ => shape.keywords = true,
                    }
                }
                "dictionary_splat" => shape.keywords = true,
                "comment" => {}
                _ => shape.bases.push(py_base(b, source)),
            }
        }
    }
    // `class A[T]` gets `typing.Generic` as its last base.
    if class.child_by_field_name("type_parameters").is_some() {
        shape.bases.push(PyBase::Name("\0Generic".to_string()));
    }
    let Some(body) = class.child_by_field_name("body") else {
        return shape;
    };
    let mut defs: HashMap<String, usize> = HashMap::new();
    for i in 0..body.named_child_count() {
        let Some(stmt) = body.named_child(i) else {
            continue;
        };
        let def = match stmt.kind() {
            "decorated_definition" => stmt.child_by_field_name("definition"),
            _ => Some(stmt),
        };
        match def.map(|d| (d.kind(), d.child_by_field_name("name"))) {
            Some(("function_definition", Some(name))) => {
                *defs
                    .entry(node_text(&name, source).to_string())
                    .or_default() += 1;
                py_header_walrus(stmt, source, &mut shape.other);
            }
            _ => {
                py_names_in(stmt, source, &mut shape.other);
                if let Some(slots) = py_slots(stmt, source) {
                    match slots {
                        Some(names) => shape.other.extend(names),
                        None => shape.opaque = true,
                    }
                }
            }
        }
    }
    shape.opaque |= PY_NAMESPACE_WRITERS
        .iter()
        .any(|n| shape.other.contains(*n));
    for (name, n) in defs {
        if n == 1 {
            shape.methods.insert(name);
        } else {
            shape.other.insert(name);
        }
    }
    shape
}

/// Builtins through which a class body can bind names its text does not show.
const PY_NAMESPACE_WRITERS: [&str; 4] = ["locals", "vars", "exec", "eval"];

/// For a class-body statement `__slots__ = …`: Some(the names it lists) when
/// the value is a string or a tuple / list / set of strings, Some(None) for
/// any other value. None for any other statement.
fn py_slots(stmt: tree_sitter::Node, source: &str) -> Option<Option<Vec<String>>> {
    let assign = (stmt.kind() == "expression_statement")
        .then(|| stmt.named_child(0))
        .flatten()
        .filter(|a| a.kind() == "assignment")?;
    let left = assign.child_by_field_name("left")?;
    if left.kind() != "identifier" || node_text(&left, source) != "__slots__" {
        return None;
    }
    let Some(value) = assign.child_by_field_name("right") else {
        return Some(None);
    };
    let items: Vec<tree_sitter::Node> = match value.kind() {
        "string" => vec![value],
        "tuple" | "list" | "set" | "parenthesized_expression" => (0..value.named_child_count())
            .filter_map(|i| value.named_child(i))
            .collect(),
        _ => return Some(None),
    };
    let mut names = Vec::new();
    for item in items {
        let content = (item.kind() == "string" && item.named_child_count() == 3)
            .then(|| item.named_child(1))
            .flatten()
            .filter(|c| c.kind() == "string_content");
        match content {
            Some(c) => names.push(node_text(&c, source).to_string()),
            None => return Some(None),
        }
    }
    Some(Some(names))
}

/// Every name a class-body statement may bind: each variable-like identifier
/// in it (not an attribute name `o.x`, not a keyword `f(x=1)`), and the name of
/// a `def` or class under it, whose body is not entered, with the walrus
/// targets of its header. An over-count only makes [`py_class_lookup`] decline.
fn py_names_in(stmt: tree_sitter::Node, source: &str, out: &mut HashSet<String>) {
    let mut cursor = stmt.walk();
    loop {
        let node = cursor.node();
        let mut enter = true;
        match node.kind() {
            "function_definition" | "class_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    out.insert(node_text(&name, source).to_string());
                }
                py_header_walrus(node, source, out);
                enter = false;
            }
            // A lambda's defaults run in the class scope; its body does not.
            "lambda" => {
                py_header_walrus(node, source, out);
                enter = false;
            }
            "identifier" => {
                let skip = node.parent().is_some_and(|p| {
                    (p.kind() == "attribute" && is_node(p.child_by_field_name("attribute"), node))
                        || (p.kind() == "keyword_argument"
                            && is_node(p.child_by_field_name("name"), node))
                });
                if !skip {
                    out.insert(node_text(&node, source).to_string());
                }
            }
            _ => {}
        }
        if enter && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node().id() == stmt.id() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// The names a walrus binds in the header of a `def`, a lambda or a class
/// (`stmt`, or one under a decorator), and the namespace writers it names
/// (`locals`, `vars`, `exec`, `eval`): its decorators, defaults, annotations
/// and bases run in the enclosing scope, so `def z(self, a=(m := f))` in a
/// class body binds `m` in the class. Its body is a scope of its own and is
/// not entered.
fn py_header_walrus(stmt: tree_sitter::Node, source: &str, out: &mut HashSet<String>) {
    let def = match stmt.kind() {
        "decorated_definition" => stmt.child_by_field_name("definition"),
        _ => Some(stmt),
    };
    let body = def
        .and_then(|d| d.child_by_field_name("body"))
        .map(|b| b.id());
    let mut cursor = stmt.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "named_expression" {
            if let Some(name) = node.child_by_field_name("name") {
                out.insert(node_text(&name, source).to_string());
            }
        }
        if node.kind() == "identifier" {
            let name = node_text(&node, source);
            if PY_NAMESPACE_WRITERS.contains(&name) {
                out.insert(name.to_string());
            }
        }
        if Some(node.id()) != body && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node().id() == stmt.id() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Whether one identifier occurrence of a class name, for [`py_class_names`],
/// leaves the name holding the class: the module-level definition itself, an
/// attribute's object (`C.m`, `C.x = 1`), a callee, an argument or base class,
/// an assignment's value, an annotation, an operand, a decorator, an
/// `except C` / `with C as x` / `case C()` class. An attribute name (`o.C`)
/// and a keyword (`f(C=1)`) are no occurrence of the variable. Tuples, lists,
/// sets, dicts, parentheses and `*` / `**` ([`py_is_container`]) are climbed
/// first, so `del a, C` and `with f() as (a, *C)` are the binding they are; in
/// one step, through `tops` (the nearest non-container ancestor of each), so a
/// name repeated deep in nested lists costs no more than once. Everything else
/// counts as a binding: an allow-list, so a shape not listed only keeps the old
/// resolution.
fn py_class_name_is_read(
    node: tree_sitter::Node,
    ancestors: &[tree_sitter::Node],
    tops: &[usize],
    module_defs: &HashSet<usize>,
) -> bool {
    let mut child = node;
    let mut at = ancestors.len();
    if at > 0 && py_is_container(ancestors[at - 1].kind()) {
        at = tops[at - 1] + 1;
        child = ancestors[at];
    }
    let Some(parent) = at.checked_sub(1).map(|i| ancestors[i]) else {
        return false;
    };
    let grandparent = at.checked_sub(2).map(|i| ancestors[i]);
    let is = |field: &str| is_node(parent.child_by_field_name(field), child);
    match parent.kind() {
        "class_definition" => is("name") && module_defs.contains(&parent.id()),
        "attribute" | "keyword_argument" => true,
        "call" => is("function"),
        "assignment" | "augmented_assignment" => is("right"),
        "named_expression" => is("value"),
        "for_statement" | "for_in_clause" => is("right"),
        "lambda" => is("body"),
        "except_clause" => is("value"),
        // `except C as e` / `with C as x` read C; `case D() as C` captures it.
        "as_pattern" => {
            is_node(parent.named_child(0), child)
                && grandparent.is_some_and(|g| matches!(g.kind(), "except_clause" | "with_item"))
        }
        "type" => grandparent.is_some_and(|g| {
            matches!(
                g.kind(),
                "assignment"
                    | "typed_parameter"
                    | "typed_default_parameter"
                    | "function_definition"
            )
        }),
        // `case C():` reads C; `case C:` (a bare name) captures it.
        "dotted_name" => grandparent.is_some_and(|g| g.kind() == "class_pattern"),
        "argument_list"
        | "subscript"
        | "comparison_operator"
        | "boolean_operator"
        | "binary_operator"
        | "unary_operator"
        | "not_operator"
        | "conditional_expression"
        | "await"
        | "return_statement"
        | "expression_statement"
        | "decorator"
        | "interpolation"
        | "pair"
        | "assert_statement"
        | "raise_statement"
        | "if_statement"
        | "elif_clause"
        | "while_statement" => true,
        _ => false,
    }
}
