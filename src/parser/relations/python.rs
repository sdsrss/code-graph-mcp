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
    /// Per file: [`scope_scan`], filled by [`reset_python_file`].
    static PY_LOCAL_VALUE_CALLS: std::cell::RefCell<std::collections::HashSet<usize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// Per file, keyed by `function_definition` (or `class_definition`) node
    /// id: what each function's own scope (or class body) binds
    /// ([`function_bindings`]). Reset by [`reset_python_file`],
    /// since node ids are only unique within one tree.
    static PY_FN_BINDINGS: std::cell::RefCell<
        std::collections::HashMap<usize, std::rc::Rc<std::collections::HashMap<String, PyBinding>>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    /// Per file: [`renamed_import_calls`], filled by [`reset_python_file`].
    static PY_RENAMED_IMPORT_CALLS: std::cell::RefCell<
        std::collections::HashMap<usize, (String, String)>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    /// Per file: [`builtin_calls`], filled by [`reset_python_file`].
    static PY_BUILTIN_CALLS: std::cell::RefCell<std::collections::HashSet<usize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// Per file: what the module scope binds, filled by [`reset_python_file`].
    static PY_MODULE: std::cell::RefCell<PyModuleScope> =
        std::cell::RefCell::new(PyModuleScope::default());
    /// Per file: its scopes ([`scope_scan`]), filled by [`reset_python_file`].
    static PY_SCOPES: std::cell::RefCell<PyScopes> = std::cell::RefCell::new(PyScopes::default());
    /// Per file: every `def`'s header ([`ScopeScan::headers`]), filled by
    /// [`reset_python_file`].
    static PY_DEF_HEADERS: std::cell::RefCell<Vec<(usize, usize)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// What the module scope binds ([`collect_module_bindings`]), and the names a
/// `global` statement anywhere in the file declares.
#[derive(Default)]
struct PyModuleScope {
    /// `from m import a as b` with `a` ≠ `b`: `b` → every (`a`, `m`).
    renamed: std::collections::HashMap<String, std::collections::HashSet<(String, String)>>,
    /// A `def`, a `class`, any other import, or an alias `X = m.X`.
    definitions: std::collections::HashSet<String>,
    /// An assignment, `for`, `with … as`, `except … as`, `case … as` or `:=`
    /// target.
    values: std::collections::HashSet<String>,
    /// `from m import *`: any name may come from `m`.
    star: bool,
    globals: std::collections::HashSet<String>,
}

/// What a name read at module level names, as far as the file shows (D#905).
#[derive(PartialEq, Eq, Debug)]
enum PyModuleRead {
    /// Possibly a project definition: the module defines or imports it
    /// (`X = m.X` counts), a `def` / `class` of the name sits anywhere in the
    /// file, a star import may bring it, a function declares it `global`, or
    /// nothing binds it (left to the by-name resolution).
    MaybeDefinition,
    /// A value the module binds and nothing else: an assignment or other
    /// target. No node holds it.
    Value,
    /// A built-in nothing in the file shadows.
    Builtin,
}

impl PyModuleScope {
    fn read(&self, name: &str, defined: &PyDefined) -> PyModuleRead {
        // A `global` name is bound by a function, which may assign it the
        // definition of that name (`concurrent.futures.__getattr__`:
        // `global ProcessPoolExecutor` then the lazily imported class).
        if self.definitions.contains(name)
            || self.renamed.contains_key(name)
            || defined.free.contains(name)
            || self.star
            || self.globals.contains(name)
        {
            PyModuleRead::MaybeDefinition
        } else if self.values.contains(name) {
            PyModuleRead::Value
        } else if crate::domain::PYTHON_BUILTINS.contains(&name) {
            PyModuleRead::Builtin
        } else {
            PyModuleRead::MaybeDefinition
        }
    }
}

/// Collect the file's `def` / `class` names. MUST run once per file before its
/// walk.
pub(super) fn reset_python_file(root: tree_sitter::Node, source: &str, family: &str) {
    PY_FN_BINDINGS.with(|b| b.borrow_mut().clear());
    collect_defined(root, source, family);
    let (local_calls, renamed_calls, builtin, module, scopes, headers) = if family == "python" {
        let scan = scope_scan(root, source);
        let mut module = PyModuleScope::default();
        collect_module_bindings(root, source, &mut module, 0);
        module.globals = scan.globals.iter().map(|g| g.to_string()).collect();
        let renamed = renamed_import_calls(&scan, &module);
        let builtin = PY_DEFINED.with(|d| builtin_calls(&scan, &module, &d.borrow()));
        (
            scan.local_values,
            renamed,
            builtin,
            module,
            scan.scopes,
            scan.headers,
        )
    } else {
        Default::default()
    };
    PY_LOCAL_VALUE_CALLS.with(|c| *c.borrow_mut() = local_calls);
    PY_RENAMED_IMPORT_CALLS.with(|c| *c.borrow_mut() = renamed_calls);
    PY_BUILTIN_CALLS.with(|c| *c.borrow_mut() = builtin);
    PY_MODULE.with(|m| *m.borrow_mut() = module);
    PY_SCOPES.with(|s| *s.borrow_mut() = scopes);
    PY_DEF_HEADERS.with(|h| *h.borrow_mut() = headers);
}

/// [`PY_DEFINED`] for the file.
fn collect_defined(root: tree_sitter::Node, source: &str, family: &str) {
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

/// How a function's own scope binds a name, as Python's symbol table ranks
/// the ways (`global` / `nonlocal` first, then a parameter, then an import or
/// a nested `def` / `class`, then any other binding).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PyBinding {
    /// A value only a run of the code knows: a parameter, or the target of an
    /// assignment, `for`, `with … as`, `except … as` or `:=`.
    Value,
    /// A name that keeps naming a definition: an import, a nested `def` /
    /// `class`, or a parameter whose default is the name itself (`f=f`,
    /// `heappush=heapq.heappush`).
    Definition,
    Global,
    Nonlocal,
}

/// `{"ur":"local"}` (`crate::domain::CALL_KEY_UNTYPED_RECEIVER`) for a bare
/// call `f(...)` whose name the caller's own scope, or an enclosing
/// function's, binds to a value ([`PyBinding::Value`]): `conv(x)` on a
/// parameter `conv` runs whatever the caller passed, not a `def conv` that
/// happens to share the name in another file. [`reset_python_file`] decided
/// it for every call of the file ([`scope_scan`]).
pub(super) fn python_local_value_call_meta(call: tree_sitter::Node) -> Option<String> {
    PY_LOCAL_VALUE_CALLS
        .with(|c| c.borrow().contains(&call.id()))
        .then(|| crate::domain::CALL_META_LOCAL_VALUE.to_string())
}

/// (`a`, `m`) for a bare call `b(...)` whose nearest binding is a
/// module-level `from m import a as b` (D#726): the call runs `a` of module
/// `m`, which no node is named after `b`. [`reset_python_file`] decided it for
/// every call of the file ([`renamed_import_calls`]).
pub(super) fn python_renamed_import_call(call: tree_sitter::Node) -> Option<(String, String)> {
    PY_RENAMED_IMPORT_CALLS.with(|c| c.borrow().get(&call.id()).cloned())
}

/// Whether a bare call `f(...)` runs the built-in `f` (D#905): no scope
/// around it binds `f`, nor the module, nor a `def` / `class` anywhere in
/// the file, and no star import may bring it. Bound by name it took any
/// namesake: daagu's `set()` bound `TTLCache.set` 313 times, `inferred`.
/// [`reset_python_file`] decided it for every call of the file
/// ([`builtin_calls`]).
pub(super) fn python_builtin_call(call: tree_sitter::Node) -> bool {
    PY_BUILTIN_CALLS.with(|c| c.borrow().contains(&call.id()))
}

/// What one sweep of a file's scopes finds ([`scope_scan`]).
#[derive(Default)]
struct ScopeScan<'s> {
    /// Bare calls whose name the caller's scope, or an enclosing function's,
    /// binds to a value ([`python_local_value_call_meta`]).
    local_values: std::collections::HashSet<usize>,
    /// Bare calls no function, lambda or comprehension around them binds the
    /// name of (or that a `global` sends to module level), with that name,
    /// and whether the call is written directly in a class body that binds
    /// the name (Python reads that body first).
    module_level: Vec<(usize, &'s str, bool)>,
    /// Every name a `global` statement anywhere in the file declares.
    globals: std::collections::HashSet<&'s str>,
    scopes: PyScopes,
    /// Every `def` from its start to its body's start (name, parameters,
    /// return annotation), in start order. No `def` is written inside
    /// another's header, so the ranges are disjoint.
    headers: Vec<(usize, usize)>,
}

/// One sweep of the file's scopes ([`ScopeScan`]): which bare calls are on
/// a local value, which reach module level, and the file's `global` names.
/// The local-value answer, by node id, follows Python's scope
/// chain for a call: a lambda's parameters and a comprehension's `for`
/// targets bind in their body (not in the first clause's iterable, which runs
/// outside), a function's bindings in its body (not in its defaults or
/// annotations), a class body binds nothing its methods see, `global` sends a
/// name to module level and `nonlocal` to the enclosing function.
///
/// One sweep collects every scope as a byte range and every bare call, then
/// each call reads the scopes around it from that list ([`PyScopes::read`]:
/// a binary search and a climb through parents). Asking each call's
/// ancestors instead costs the depth squared per call (`Node::parent`
/// descends from the root): `test_python_class_body_marking_stays_linear`
/// timed out at 60 s.
fn scope_scan<'s>(root: tree_sitter::Node, source: &'s str) -> ScopeScan<'s> {
    let mut scopes: Vec<PyScope> = Vec::new();
    let mut calls: Vec<(usize, usize, &str)> = Vec::new();
    let scope = |start: usize, end: usize, names, class| PyScope {
        start,
        end,
        parent: None,
        names,
        class,
    };
    // Every node passes this match: kind ids, not kind strings.
    let language = root.language();
    let id = |kind: &str| language.id_for_node_kind(kind, true);
    let wanted = [
        id("call"),
        id("function_definition"),
        id("class_definition"),
        id("lambda"),
        id("list_comprehension"),
        id("set_comprehension"),
        id("dictionary_comprehension"),
        id("generator_expression"),
        id("global_statement"),
    ];
    let mut globals = std::collections::HashSet::new();
    let mut headers = Vec::new();
    let mut cursor = root.walk();
    'walk: loop {
        let n = cursor.node();
        let kind = if wanted.contains(&n.kind_id()) {
            n.kind()
        } else {
            ""
        };
        match kind {
            "call" => {
                if let Some(f) = n
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "identifier")
                {
                    calls.push((n.start_byte(), n.id(), node_text(&f, source)));
                }
            }
            // `function_bindings` reads a class body as it reads a
            // function's: a class has no `parameters` field.
            "function_definition" | "class_definition" => {
                if let Some(body) = n.child_by_field_name("body") {
                    if kind == "function_definition" {
                        headers.push((n.start_byte(), body.start_byte()));
                    }
                    scopes.push(scope(
                        body.start_byte(),
                        body.end_byte(),
                        function_bindings(n, source),
                        kind == "class_definition",
                    ));
                }
            }
            "global_statement" => {
                for i in 0..n.named_child_count() {
                    if let Some(c) = n.named_child(i).filter(|c| c.kind() == "identifier") {
                        globals.insert(node_text(&c, source));
                    }
                }
            }
            "lambda" => {
                if let Some(body) = n.child_by_field_name("body") {
                    let params = n
                        .child_by_field_name("parameters")
                        .map(|p| parameter_bindings(p, source))
                        .unwrap_or_default();
                    scopes.push(scope(
                        body.start_byte(),
                        body.end_byte(),
                        std::rc::Rc::new(params),
                        false,
                    ));
                }
            }
            "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => {
                let mut names = std::collections::HashMap::new();
                let mut first_iterable = None;
                let mut c = n.walk();
                for clause in n
                    .named_children(&mut c)
                    .filter(|c| c.kind() == "for_in_clause")
                {
                    if first_iterable.is_none() {
                        first_iterable = clause.child_by_field_name("right");
                    }
                    if let Some(left) = clause.child_by_field_name("left") {
                        let mut bound = Vec::new();
                        pattern_names(left, source, &mut bound);
                        names.extend(bound.into_iter().map(|n| (n, PyBinding::Value)));
                    }
                }
                let names = std::rc::Rc::new(names);
                // Around the first iterable: before it and after it.
                let (gap_start, gap_end) = first_iterable
                    .map_or((n.end_byte(), n.end_byte()), |r| {
                        (r.start_byte(), r.end_byte())
                    });
                scopes.push(scope(n.start_byte(), gap_start, names.clone(), false));
                scopes.push(scope(gap_end, n.end_byte(), names, false));
            }
            _ => {}
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    let scopes = PyScopes::new(scopes);
    let mut out = std::collections::HashSet::new();
    let mut module_level = Vec::new();
    for (at, id, name) in calls {
        match scopes.read(at, name) {
            (PyScopeRead::Value, _) => {
                out.insert(id);
            }
            (PyScopeRead::Definition, _) => {}
            (PyScopeRead::Module, class_bound) => module_level.push((id, name, class_bound)),
        }
    }
    ScopeScan {
        local_values: out,
        module_level,
        globals,
        scopes,
        headers,
    }
}

/// One scope of a file as a byte range ([`scope_scan`]): a function's or
/// lambda's body, a class body, or a comprehension around its first
/// iterable (which runs outside it).
struct PyScope {
    start: usize,
    end: usize,
    /// The innermost scope around this one.
    parent: Option<usize>,
    names: std::rc::Rc<std::collections::HashMap<String, PyBinding>>,
    /// A class body: read only by code written directly in it, and then
    /// only as [`PyScopes::read`]'s flag.
    class: bool,
}

/// How the scopes around a read bind its name ([`PyScopes::read`]).
#[derive(PartialEq, Eq, Debug)]
enum PyScopeRead {
    /// A function, lambda or comprehension binds it to a value.
    Value,
    /// A function binds it to a definition: a nested `def` / `class`, an
    /// import, a parameter defaulting to the name.
    Definition,
    /// It reaches the module scope (or a `global` sends it there).
    Module,
}

/// A file's scopes, sorted by start (the outer first on a tie), each with
/// its parent: the scopes around any byte are one parent chain, found by a
/// binary search and a climb, so a read costs the scope nesting, never the
/// syntax depth (`Node::parent` descends from the root).
#[derive(Default)]
struct PyScopes(Vec<PyScope>);

impl PyScopes {
    fn new(mut scopes: Vec<PyScope>) -> Self {
        scopes.sort_unstable_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        // Scopes are syntax ranges: nested or disjoint.
        let mut open: Vec<usize> = Vec::new();
        for i in 0..scopes.len() {
            while open
                .last()
                .is_some_and(|&top| scopes[top].end <= scopes[i].start)
            {
                open.pop();
            }
            scopes[i].parent = open.last().copied();
            open.push(i);
        }
        PyScopes(scopes)
    }

    /// The innermost scope holding byte `at`.
    fn innermost(&self, at: usize) -> Option<usize> {
        let mut i = self.0.partition_point(|s| s.start <= at).checked_sub(1)?;
        while self.0[i].end <= at {
            i = self.0[i].parent?;
        }
        Some(i)
    }

    /// How the scopes around byte `at` bind `name`, innermost first, as
    /// Python's chain reads them: a class body only for code written
    /// directly in it (reported as the flag, the chain goes on past it),
    /// `global` to module level, `nonlocal` outward.
    fn read(&self, at: usize, name: &str) -> (PyScopeRead, bool) {
        let mut class_bound = false;
        let mut at_scope = self.innermost(at);
        let mut innermost = true;
        while let Some(i) = at_scope {
            let scope = &self.0[i];
            if scope.class {
                class_bound |= innermost && scope.names.contains_key(name);
            } else {
                match scope.names.get(name) {
                    Some(PyBinding::Value) => return (PyScopeRead::Value, class_bound),
                    Some(PyBinding::Definition) => return (PyScopeRead::Definition, class_bound),
                    Some(PyBinding::Global) => break,
                    Some(PyBinding::Nonlocal) | None => {}
                }
            }
            innermost = false;
            at_scope = scope.parent;
        }
        (PyScopeRead::Module, class_bound)
    }
}

/// The bare calls of `scan` whose name the module binds only through
/// `from m import a as b` with one (`a`, `m`), by node id (D#726). Any other
/// module-level binding of `b` — a `def` or `class`, an assignment, a `for` or
/// `with` target, another import, a renamed import of something else (an
/// `ImportError` fallback), or a `global b` in any function — leaves the call
/// to the by-name resolution it had. A call directly in a class body is
/// judged at module level, as [`scope_scan`] judges it; a star import
/// is not read.
fn renamed_import_calls(
    scan: &ScopeScan,
    module: &PyModuleScope,
) -> std::collections::HashMap<usize, (String, String)> {
    if module.renamed.is_empty() {
        return std::collections::HashMap::new();
    }
    scan.module_level
        .iter()
        .filter(|(_, name, _)| {
            !module.definitions.contains(*name)
                && !module.values.contains(*name)
                && !scan.globals.contains(name)
        })
        .filter_map(|&(id, name, _)| {
            let imports = module.renamed.get(name)?;
            let (export, module) = imports.iter().next().filter(|_| imports.len() == 1)?;
            Some((id, (export.clone(), module.clone())))
        })
        .collect()
}

/// The bare calls of `scan` that run a built-in ([`python_builtin_call`]),
/// by node id.
fn builtin_calls(
    scan: &ScopeScan,
    module: &PyModuleScope,
    defined: &PyDefined,
) -> std::collections::HashSet<usize> {
    scan.module_level
        .iter()
        .filter(|&&(_, name, class_bound)| {
            !class_bound && module.read(name, defined) == PyModuleRead::Builtin
        })
        .map(|&(id, _, _)| id)
        .collect()
}

/// The names the module scope binds, as [`collect_scope_bindings`] reads a
/// function's: `from m import a as b` (`a` ≠ `b`) into `renamed` as `b` →
/// (`a`, `m`), a `def`, `class` or other import into `definitions`, any
/// other binding into `values`, and whether a star import is among them.
fn collect_module_bindings(
    node: tree_sitter::Node,
    source: &str,
    module: &mut PyModuleScope,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    let mut targets = |field: &str| {
        if let Some(t) = node.child_by_field_name(field) {
            let mut names = Vec::new();
            pattern_names(t, source, &mut names);
            module.values.extend(names);
        }
    };
    match node.kind() {
        "function_definition" | "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                module
                    .definitions
                    .insert(node_text(&name, source).to_string());
            }
            return;
        }
        "lambda"
        | "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => return,
        "import_statement" | "import_from_statement" => {
            let from = (node.kind() == "import_from_statement")
                .then(|| node.child_by_field_name("module_name"))
                .flatten()
                .map(|m| node_text(&m, source));
            let mut cursor = node.walk();
            module.star |= node
                .named_children(&mut cursor)
                .any(|c| c.kind() == "wildcard_import");
            for name in node.children_by_field_name("name", &mut cursor) {
                match name.kind() {
                    "aliased_import" => {
                        let (Some(imported), Some(alias)) = (
                            name.child_by_field_name("name"),
                            name.child_by_field_name("alias"),
                        ) else {
                            continue;
                        };
                        let (imported, alias) =
                            (node_text(&imported, source), node_text(&alias, source));
                        match from {
                            Some(m) if imported != alias => {
                                module
                                    .renamed
                                    .entry(alias.to_string())
                                    .or_default()
                                    .insert((imported.to_string(), m.to_string()));
                            }
                            _ => {
                                module.definitions.insert(alias.to_string());
                            }
                        }
                    }
                    // `import a.b` binds `a`; `from m import a` binds `a`.
                    "dotted_name" => {
                        if let Some(first) = name.named_child(0) {
                            module
                                .definitions
                                .insert(node_text(&first, source).to_string());
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        // `Command = commands.Command`, `TimeoutError = TimeoutError`: the
        // value is the definition of that name, as for a parameter default
        // (`heappush=heapq.heappush`, [`parameter_bindings`]).
        "assignment" if module_alias(node, source).is_some() => {
            if let Some(alias) = module_alias(node, source) {
                module.definitions.insert(alias.to_string());
            }
        }
        "assignment" | "augmented_assignment" | "for_statement" => targets("left"),
        "named_expression" => targets("name"),
        "as_pattern" => targets("alias"),
        _ => {}
    }
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        collect_module_bindings(c, source, module, depth + 1);
    }
}

/// `X` of an assignment `X = X` or `X = a.b.X`.
fn module_alias<'s>(assignment: tree_sitter::Node, source: &'s str) -> Option<&'s str> {
    let left = assignment
        .child_by_field_name("left")
        .filter(|l| l.kind() == "identifier")?;
    let right = assignment.child_by_field_name("right")?;
    let spelled = match right.kind() {
        "identifier" => right,
        "attribute" => right.child_by_field_name("attribute")?,
        _ => return None,
    };
    let name = node_text(&left, source);
    (node_text(&spelled, source) == name).then_some(name)
}

/// What `function`'s own scope binds, computed once per function.
fn function_bindings(
    function: tree_sitter::Node,
    source: &str,
) -> std::rc::Rc<std::collections::HashMap<String, PyBinding>> {
    if let Some(hit) = PY_FN_BINDINGS.with(|b| b.borrow().get(&function.id()).cloned()) {
        return hit;
    }
    let mut declared = std::collections::HashMap::new();
    let mut definitions = std::collections::HashSet::new();
    let mut values = std::collections::HashSet::new();
    if let Some(body) = function.child_by_field_name("body") {
        collect_scope_bindings(
            body,
            source,
            &mut declared,
            &mut definitions,
            &mut values,
            0,
        );
    }
    let params = function
        .child_by_field_name("parameters")
        .map(|p| parameter_bindings(p, source))
        .unwrap_or_default();
    let mut out: std::collections::HashMap<String, PyBinding> =
        values.into_iter().map(|n| (n, PyBinding::Value)).collect();
    out.extend(definitions.into_iter().map(|n| (n, PyBinding::Definition)));
    out.extend(params);
    out.extend(declared);
    let out = std::rc::Rc::new(out);
    PY_FN_BINDINGS.with(|b| b.borrow_mut().insert(function.id(), out.clone()));
    out
}

/// A function's or lambda's parameter names: [`PyBinding::Definition`] when
/// the default is the name itself or an attribute ending in it, so the call
/// runs what the name names (`def f(x, len=len)`, `heappush=heapq.heappush`).
fn parameter_bindings(
    parameters: tree_sitter::Node,
    source: &str,
) -> std::collections::HashMap<String, PyBinding> {
    let mut out = std::collections::HashMap::new();
    for i in 0..parameters.named_child_count() {
        let Some(p) = parameters.named_child(i) else {
            continue;
        };
        let (name, default) = match p.kind() {
            "default_parameter" | "typed_default_parameter" => (
                p.child_by_field_name("name"),
                p.child_by_field_name("value"),
            ),
            // `x: T` keeps its name as the first named child (`*a: T` too).
            "typed_parameter" => (p.named_child(0), None),
            _ => (Some(p), None),
        };
        let Some(name) = name else { continue };
        let mut names = Vec::new();
        pattern_names(name, source, &mut names);
        for n in names {
            let alias = default.is_some_and(|d| {
                let spelled = match d.kind() {
                    "identifier" => Some(d),
                    "attribute" => d.child_by_field_name("attribute"),
                    _ => None,
                };
                spelled.is_some_and(|s| node_text(&s, source) == n)
            });
            let binding = if alias {
                PyBinding::Definition
            } else {
                PyBinding::Value
            };
            out.insert(n, binding);
        }
    }
    out
}

/// The names a binding target binds: `a`, `a, b`, `(a, [b, *c])`, `**kw` —
/// not `self.x` or `a[i]`, which store into an object.
fn pattern_names(node: tree_sitter::Node, source: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => out.push(node_text(&node, source).to_string()),
        "pattern_list"
        | "tuple_pattern"
        | "list_pattern"
        | "list_splat_pattern"
        | "dictionary_splat_pattern"
        | "as_pattern_target" => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    pattern_names(c, source, out);
                }
            }
        }
        _ => {}
    }
}

/// Walk a function body for what its own scope binds, not entering a nested
/// function, class, lambda or comprehension (each a scope of its own; a
/// nested `def` / `class` binds its name here).
fn collect_scope_bindings(
    node: tree_sitter::Node,
    source: &str,
    declared: &mut std::collections::HashMap<String, PyBinding>,
    definitions: &mut std::collections::HashSet<String>,
    values: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    let targets = |field: &str, values: &mut std::collections::HashSet<String>| {
        if let Some(t) = node.child_by_field_name(field) {
            let mut names = Vec::new();
            pattern_names(t, source, &mut names);
            values.extend(names);
        }
    };
    match node.kind() {
        "function_definition" | "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                definitions.insert(node_text(&name, source).to_string());
            }
            return;
        }
        "lambda"
        | "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => return,
        "global_statement" | "nonlocal_statement" => {
            let binding = if node.kind() == "global_statement" {
                PyBinding::Global
            } else {
                PyBinding::Nonlocal
            };
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i).filter(|c| c.kind() == "identifier") {
                    declared.insert(node_text(&c, source).to_string(), binding);
                }
            }
            return;
        }
        "import_statement" | "import_from_statement" => {
            let mut cursor = node.walk();
            for name in node.children_by_field_name("name", &mut cursor) {
                let bound = match name.kind() {
                    "aliased_import" => name.child_by_field_name("alias"),
                    // `import a.b` binds `a`.
                    "dotted_name" => name.named_child(0),
                    _ => None,
                };
                if let Some(b) = bound {
                    definitions.insert(node_text(&b, source).to_string());
                }
            }
            return;
        }
        "assignment" | "augmented_assignment" | "for_statement" => targets("left", values),
        "named_expression" => targets("name", values),
        // `with C as x`, `except C as e` and `case D() as c` all parse so.
        "as_pattern" => targets("alias", values),
        _ => {}
    }
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        collect_scope_bindings(c, source, declared, definitions, values, depth + 1);
    }
}

/// Whether `node` is the `attribute` field of an `attribute` (`b` of `a.b`).
fn is_attribute_tail(node: &tree_sitter::Node) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "attribute"
            && p.child_by_field_name("attribute")
                .is_some_and(|a| a.id() == node.id())
    })
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
/// D#909: where a bare annotation name is evaluated, for picking among the
/// file's same-named definitions by scope. An annotation in a `def`'s header
/// (a parameter's, the return's) runs around that `def`, so it reads from the
/// byte just before it, which no range of the function's own nested
/// definitions holds (`usize::MAX`, held by no function, for a `def` at byte
/// 0). Any other annotation reads where it stands. A binary search over the
/// file's headers ([`ScopeScan::headers`]): climbing to the `def` through
/// `Node::parent` cost 1.05 -> 3.77 s on 480 KB of annotations nested 150
/// generics deep, since each `parent` descends from the root.
fn py_annotation_at(node: &tree_sitter::Node) -> usize {
    let at = node.start_byte();
    PY_DEF_HEADERS.with(|h| {
        let h = h.borrow();
        match h.partition_point(|&(start, _)| start <= at).checked_sub(1) {
            Some(i) if at < h[i].1 => h[i].0.checked_sub(1).unwrap_or(usize::MAX),
            _ => at,
        }
    })
}

pub(super) fn extract_python_type_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    if !is_annotation_type_name(node) {
        return None;
    }
    let name = node_text(node, source);
    // `Type` of `mod.Type` is an attribute of `mod`, not a bare read.
    let bare = !is_attribute_tail(node);
    if name.is_empty()
        || name == "_"
        || PYTHON_TYPE_REFERENCE_NOISE.contains(&name)
        || (bare && py_read_names_no_definition(*node, name))
    {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.unwrap_or("<module>").to_string(),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
        // A bare name picks among the file's definitions by scope, as a bare
        // call does (D#909, D#714), from where Python evaluates it.
        call_at: bare.then(|| crate::parser::treesitter::BareCall {
            at: py_annotation_at(node),
            reaches_members: false,
        }),
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
    if py_enclosing_fn_local_names(node, source).contains(name)
        || py_read_names_no_definition(*node, name)
    {
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
        // Every slot above holds a bare identifier: it picks among the file's
        // definitions by scope, as a bare call does (D#909, D#714). A
        // decorator's name is where it is evaluated, around its `def`.
        call_at: crate::parser::treesitter::bare_call_at(node, "python"),
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

/// Whether the bare `name` read at `node` names no project definition by
/// Python's scope chain (D#905): a value some scope around it binds (a
/// parameter, an assignment, `for`, `with … as`, `except … as`, `:=` or
/// comprehension target, a lambda's parameter), or at module level a value
/// the module binds or a built-in nothing in the file shadows
/// ([`PyModuleRead`]). Bound by name such a read took every namesake:
/// `sum(len(m) for m in marks)` bound 283 fixture methods `C.m`,
/// `isinstance(x, int)` and `@property` the standard library's namesakes.
///
/// The chain is the one bare calls are read by ([`PyScopes::read`]). A
/// nested `def` / `class`, an import, or a binding of the class body the
/// read is written in leaves the name to the by-name resolution it had.
fn py_read_names_no_definition(node: tree_sitter::Node, name: &str) -> bool {
    match PY_SCOPES.with(|s| s.borrow().read(node.start_byte(), name)) {
        (_, true) | (PyScopeRead::Definition, _) => false,
        (PyScopeRead::Value, _) => true,
        (PyScopeRead::Module, _) => PY_MODULE.with(|m| {
            PY_DEFINED.with(|d| m.borrow().read(name, &d.borrow()) != PyModuleRead::MaybeDefinition)
        }),
    }
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
