//! Python module path resolution. `import myapp.utils` and `from myapp.utils
//! import helper` carry dotted module paths that don't directly map to file
//! names, so the indexer pre-builds a `dotted_path → file_paths` map and
//! consults it during Phase 2 import-edge resolution.
//!
//! The map is keyed by IMPORT ROOT, not by path suffix. A dotted path is
//! resolved relative to the directories Python would actually import from —
//! the project root plus every directory that is not itself a package (no
//! `__init__.py`), which is what a `src/` layout, a `tests/` tree or a plain
//! script directory looks like. Inside a package, `import logging` means the
//! standard library, not the sibling `logging.py`, and PEP 328 has made that
//! the only reading since Python 3.
//!
//! It used to register every suffix instead: `src/myapp/utils.py` was reachable
//! as `src.myapp.utils`, `myapp.utils` AND `utils`, on the argument that
//! over-connecting is the safer failure without `sys.path` context. Measured
//! against 1,763 files of third-party Python (audit 2026-08-22 P2-4), that
//! argument does not survive contact: `import logging` bound to
//! `accelerate/logging.py`, `import json` to `rich/json.py`, `import math` to
//! `pygments/lexers/math.py` — 886 of 1,451 module bindings pointed at a real
//! node that the import does not name, and each one fed `deps`, `cycles` and
//! `map` as fact. A phantom bound to a real node is this repository's worst
//! failure mode precisely because nothing in the answer says it is wrong.

use std::collections::{HashMap, HashSet};

use crate::domain::REL_IMPORTS;
use crate::parser::relations::ParsedRelation;

#[derive(Debug, Clone, PartialEq, Eq)]
/// The semantic target and lexical spelling of one Python import binding.
///
/// `is_explicit_alias` distinguishes `import pkg.sub as alias`, where the
/// alias replaces the full module path, from plain `import pkg.sub`, where a
/// valid attribute reference must still spell the complete dotted path.
pub(super) struct PythonImportBinding {
    pub module: String,
    pub imported_name: String,
    pub is_module_import: bool,
    pub is_explicit_alias: bool,
}

/// Visible imports keyed by lexical scope and the local name they bind.
pub(super) type PythonImportBindings = HashMap<(String, String), Vec<PythonImportBinding>>;
pub(super) type PythonLocalBindings = HashMap<String, HashSet<String>>;

fn node_text<'a>(node: &tree_sitter::Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// Collect names bound in function scopes (parameters, local assignments,
/// exception aliases, and nested definitions) to prevent module-level imports
/// from binding to shadowed names.
pub(super) fn collect_python_local_bindings(
    tree: &tree_sitter::Tree,
    source: &str,
) -> PythonLocalBindings {
    let mut out: PythonLocalBindings = HashMap::new();
    walk_python_scopes(&tree.root_node(), source, None, &mut out);
    out
}

fn walk_python_scopes(
    node: &tree_sitter::Node,
    source: &str,
    current_class: Option<&str>,
    out: &mut PythonLocalBindings,
) {
    match node.kind() {
        "class_definition" => {
            let class_name = node
                .child_by_field_name("name")
                .map(|n| node_text(&n, source));
            if let Some(body) = node.child_by_field_name("body") {
                for i in 0..body.named_child_count() {
                    if let Some(child) = body.named_child(i) {
                        walk_python_scopes(&child, source, class_name, out);
                    }
                }
            }
        }
        "function_definition" | "async_function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let fn_name = node_text(&name_node, source);
                let qualified_name = current_class
                    .map(|cls| format!("{}.{}", cls, fn_name))
                    .unwrap_or_else(|| fn_name.to_string());

                let mut locals = HashSet::new();
                if let Some(params) = node.child_by_field_name("parameters") {
                    collect_py_param_idents(&params, source, &mut locals);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    let mut globals = HashSet::new();
                    collect_py_body_bindings(&body, source, &mut locals, &mut globals, 0);
                    locals.retain(|name| !globals.contains(name));
                }

                // A method's relation source is qualified (`Class.method`).
                // A second bare-method bucket would let an unrelated module
                // function with the same name inherit the method's locals.
                out.entry(qualified_name).or_default().extend(locals);

                // Recurse into body to collect nested functions/classes
                if let Some(body) = node.child_by_field_name("body") {
                    for i in 0..body.named_child_count() {
                        if let Some(child) = body.named_child(i) {
                            walk_python_scopes(&child, source, None, out);
                        }
                    }
                }
            }
        }
        _ => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    walk_python_scopes(&child, source, current_class, out);
                }
            }
        }
    }
}

fn collect_py_param_idents(node: &tree_sitter::Node, source: &str, out: &mut HashSet<String>) {
    collect_binding_pattern(node, source, out);
}

fn collect_py_body_bindings(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut HashSet<String>,
    globals: &mut HashSet<String>,
    depth: usize,
) {
    if depth > 50 {
        return;
    }
    match node.kind() {
        "function_definition" | "async_function_definition" | "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                out.insert(node_text(&name, source).to_string());
            }
            return;
        }
        "assignment" | "augmented_assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_binding_pattern(&left, source, out);
            }
        }
        "for_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_binding_pattern(&left, source, out);
            }
        }
        // A comprehension's `for_in_clause` owns a nested Python 3 scope. Its
        // loop target must not shadow an import in the enclosing function.
        "for_in_clause" => {}
        "with_item" | "as_clause" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                collect_binding_pattern(&alias, source, out);
            } else if let Some(target) = node.child_by_field_name("target") {
                collect_binding_pattern(&target, source, out);
            }
        }
        "as_pattern" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                collect_binding_pattern(&alias, source, out);
            }
        }
        "except_clause" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                collect_binding_pattern(&alias, source, out);
            }
        }
        "named_expression" => {
            if let Some(name) = node.child_by_field_name("name") {
                collect_binding_pattern(&name, source, out);
            }
        }
        "global_statement" => {
            for i in 0..node.named_child_count() {
                if let Some(name) = node.named_child(i) {
                    if name.kind() == "identifier" {
                        globals.insert(node_text(&name, source).to_string());
                    }
                }
            }
        }
        // A nonlocal name is not local to this function, but it still blocks a
        // module import because lookup is directed to an enclosing function.
        "nonlocal_statement" => {
            for i in 0..node.named_child_count() {
                if let Some(name) = node.named_child(i) {
                    if name.kind() == "identifier" {
                        out.insert(node_text(&name, source).to_string());
                    }
                }
            }
        }
        _ => {}
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_py_body_bindings(&child, source, out, globals, depth + 1);
        }
    }
}

/// Collect identifiers that a Python binding pattern actually assigns.
///
/// The walker enters only grammar nodes that represent parameters or
/// destructuring. It deliberately stops at attributes and subscripts:
/// `obj.value = x` binds no local named `value`, and `items[key] = x` binds
/// neither `items` nor `key`.
fn collect_binding_pattern(node: &tree_sitter::Node, source: &str, out: &mut HashSet<String>) {
    match node.kind() {
        "identifier" => {
            out.insert(node_text(node, source).to_string());
        }
        "attribute" | "subscript" => {}
        "default_parameter" | "typed_default_parameter" => {
            if let Some(name) = node.child_by_field_name("name") {
                collect_binding_pattern(&name, source, out);
            }
        }
        // tree-sitter-python gives the annotation a `type` field but leaves
        // the bound parameter as the first named child without a `name` field.
        "typed_parameter" => {
            if let Some(name) = node.named_child(0) {
                collect_binding_pattern(&name, source, out);
            }
        }
        "parameters"
        | "lambda_parameters"
        | "pattern_list"
        | "tuple_pattern"
        | "list_pattern"
        | "list_splat_pattern"
        | "dictionary_splat_pattern"
        | "parenthesized_expression"
        | "as_pattern_target" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    collect_binding_pattern(&child, source, out);
                }
            }
        }
        _ => {}
    }
}

/// Map `(lexical_scope, local_name)` to the imported Python symbol. Module-level
/// bindings are fallback-visible from function scopes; function-local imports
/// stay scoped to the function that contains them.
pub(super) fn build_python_import_bindings(relations: &[ParsedRelation]) -> PythonImportBindings {
    let mut bindings: PythonImportBindings = HashMap::new();
    for rel in relations.iter().filter(|rel| rel.relation == REL_IMPORTS) {
        let Some(metadata) = rel
            .metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        else {
            continue;
        };
        let Some(module) = metadata.get("python_module").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(local_name) = metadata.get("python_local").and_then(|v| v.as_str()) else {
            continue;
        };
        let scope = metadata
            .get("python_scope")
            .and_then(|v| v.as_str())
            .unwrap_or("<module>");
        let binding = PythonImportBinding {
            module: module.to_string(),
            imported_name: rel.target_name.clone(),
            is_module_import: metadata
                .get("is_module_import")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            is_explicit_alias: metadata
                .get("python_explicit_alias")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        let coexist = binding.is_module_import && !binding.is_explicit_alias;
        let entry = bindings
            .entry((scope.to_string(), local_name.to_string()))
            .or_default();
        if coexist
            && entry
                .iter()
                .all(|existing| existing.is_module_import && !existing.is_explicit_alias)
        {
            if !entry.contains(&binding) {
                entry.push(binding);
            }
        } else {
            // Imported symbols and explicit aliases rebind their local name.
            // A plain import after either form also replaces that binding, but
            // subsequent plain dotted imports with the same root can coexist.
            entry.clear();
            entry.push(binding);
        }
    }
    bindings
}

/// Find every import binding visible for one local name.
///
/// The slice normally has one entry. It contains multiple entries only for
/// plain dotted module imports that bind the same leading component, such as
/// `import pkg.models` followed by `import pkg.views`.
pub(super) fn find_python_import_bindings<'a>(
    bindings: &'a PythonImportBindings,
    local_bindings: &PythonLocalBindings,
    scope: &str,
    local_name: &str,
) -> Option<&'a [PythonImportBinding]> {
    if let Some(binding) = bindings.get(&(scope.to_string(), local_name.to_string())) {
        return Some(binding.as_slice());
    }
    if python_import_is_shadowed(bindings, local_bindings, scope, local_name) {
        return None;
    }
    bindings
        .get(&("<module>".to_string(), local_name.to_string()))
        .map(Vec::as_slice)
}

/// Return whether a function-local binding blocks a module-level import.
///
/// A function-scoped import is itself authoritative and therefore wins over
/// the conservative local-binding set. Otherwise any parameter, assignment,
/// exception alias, or nested definition with this name shadows the module
/// import throughout the function under Python's lexical scoping rules.
pub(super) fn python_import_is_shadowed(
    bindings: &PythonImportBindings,
    local_bindings: &PythonLocalBindings,
    scope: &str,
    local_name: &str,
) -> bool {
    scope != "<module>"
        && !bindings.contains_key(&(scope.to_string(), local_name.to_string()))
        && bindings.contains_key(&("<module>".to_string(), local_name.to_string()))
        && local_bindings
            .get(scope)
            .is_some_and(|locals| locals.contains(local_name))
}

/// Translate a written Python attribute receiver through an import binding.
///
/// The returned module is guaranteed to exist in `python_module_map`. The
/// optional owner is the original imported class or nested attribute prefix
/// that must precede the called method's bare name; no owner means a module-level
/// call. An outer `None` means the binding is external or the written receiver
/// does not name the bound target. Callers must not reinterpret that receiver
/// through filename or bare-name guessing.
pub(super) fn python_bound_call_target(
    bindings: &[PythonImportBinding],
    written_segments: &[String],
    python_module_map: &HashMap<String, Vec<String>>,
) -> Option<(String, Option<String>)> {
    if written_segments.is_empty() {
        return None;
    }

    bindings
        .iter()
        .filter_map(|binding| {
            python_bound_call_target_one(binding, written_segments, python_module_map)
        })
        .max_by_key(|(_, _, specificity)| *specificity)
        .map(|(module, owner, _)| (module, owner))
}

/// Translate one Python import binding and rank how specifically it matched.
///
/// Exact plain dotted imports outrank a package-root fallback. The rank lets a
/// shared-root binding set select `pkg.models` for `pkg.models.load()` without
/// depending on import order.
fn python_bound_call_target_one(
    binding: &PythonImportBinding,
    written_segments: &[String],
    python_module_map: &HashMap<String, Vec<String>>,
) -> Option<(String, Option<String>, usize)> {
    let promote_submodules = |base: &str, consumed: usize, specificity: usize| {
        if !python_module_map.contains_key(base) {
            return None;
        }
        let remaining = &written_segments[consumed..];
        let mut promoted = 0usize;
        for count in 1..=remaining.len() {
            let module = format!("{}.{}", base, remaining[..count].join("."));
            if python_module_map.contains_key(&module) {
                promoted = count;
            }
        }
        let module = if promoted == 0 {
            base.to_string()
        } else {
            format!("{}.{}", base, remaining[..promoted].join("."))
        };
        let owner = remaining
            .get(promoted..)
            .filter(|segments| !segments.is_empty())
            .map(|segments| segments.join("."));
        Some((module, owner, specificity + promoted))
    };

    if binding.is_module_import {
        let module_segments: Vec<&str> = binding.module.split('.').collect();
        if binding.is_explicit_alias {
            // An explicit alias replaces the complete module path, including
            // the valid but unusual `import pkg.sub as pkg` spelling.
            return promote_submodules(&binding.module, 1, module_segments.len() * 2);
        } else if written_segments.len() >= module_segments.len()
            && written_segments
                .iter()
                .zip(&module_segments)
                .all(|(written, module)| written == module)
        {
            return promote_submodules(
                &binding.module,
                module_segments.len(),
                module_segments.len() * 2,
            );
        } else if written_segments.len() == 1
            && module_segments.len() > 1
            && written_segments[0].as_str() == module_segments[0]
        {
            // `import pkg.sub` also binds `pkg`; a direct `pkg.helper()` call
            // can therefore target the package module, never `pkg.sub`.
            return promote_submodules(module_segments[0], 1, 1);
        } else if module_segments.len() == 1 && written_segments[0].as_str() == module_segments[0] {
            // `import pkg` permits `pkg.sub.func()`. Promote the longest
            // receiver prefix that is an indexed module and leave any tail as
            // a class or nested owner.
            return promote_submodules(&binding.module, 1, 2);
        }
        return None;
    }

    // `from pkg import sub as s` can bind a real submodule. Prefer that module
    // identity when it exists so this form deduplicates with `import pkg.sub`.
    let submodule = if binding.module.is_empty() {
        binding.imported_name.clone()
    } else {
        format!("{}.{}", binding.module, binding.imported_name)
    };
    if python_module_map.contains_key(&submodule) {
        return promote_submodules(&submodule, 1, 2);
    }
    if !python_module_map.contains_key(&binding.module) {
        return None;
    }
    let mut owner_segments = vec![binding.imported_name.clone()];
    owner_segments.extend(written_segments.iter().skip(1).cloned());
    Some((binding.module.clone(), Some(owner_segments.join(".")), 1))
}

/// Directories Python would import from: the project root, plus every
/// directory that is neither a package nor inside one. A package directory is
/// deliberately NOT a root — that is the whole difference between `src/db.py`
/// (importable as `db` when `src/` is a plain directory) and
/// `accelerate/logging.py` (never importable as `logging`, because
/// `accelerate/` is a package).
///
/// "Inside one" carries the same weight as "is one". `__init__.py` has been
/// optional since PEP 420, so packages routinely hold subdirectories without it
/// — vendored trees, asset dirs, plugin folders. Testing only the directory
/// itself made every one of those a top-level root and rebuilt the exact
/// phantom the package rule removed (`import logging` →
/// `mypkg/vendored/logging.py`). Inside a package tree you are reached by
/// dotted path, never by sitting on `sys.path`.
fn import_roots(python_paths: &HashSet<String>) -> HashSet<String> {
    let packages: HashSet<&str> = python_paths
        .iter()
        .filter_map(|p| {
            p.strip_suffix("/__init__.py")
                .or_else(|| p.strip_suffix("/__init__.pyi"))
        })
        .collect();
    let mut roots: HashSet<String> = HashSet::new();
    roots.insert(String::new()); // the project root is always importable-from
    let mut chain: Vec<&str> = Vec::new();
    for path in python_paths {
        // Every ancestor directory of every module file is a candidate, but the
        // chain has to be walked ROOT-FIRST: the first package on it ends the
        // roots, and a package's own parent (`src/` above `src/myapp/`) is
        // still a root, so an upward walk cannot decide `d` before its
        // ancestors are known.
        chain.clear();
        let mut dir = path.rsplit_once('/').map(|(d, _)| d);
        while let Some(d) = dir {
            chain.push(d);
            dir = d.rsplit_once('/').map(|(parent, _)| parent);
        }
        for d in chain.iter().rev() {
            if packages.contains(d) {
                break;
            }
            roots.insert((*d).to_string());
        }
    }
    roots
}

/// Build mapping from Python dotted module paths to file paths.
/// Each file is registered under the dotted path it has RELATIVE TO each import
/// root above it — so `src/myapp/utils.py` is `src.myapp.utils` from the project
/// root and `myapp.utils` from `src/`, but is `utils` only if `src/myapp/` is
/// itself a plain directory rather than a package.
pub(super) fn build_python_module_map(
    python_paths: &HashSet<String>,
) -> HashMap<String, Vec<String>> {
    let roots = import_roots(python_paths);
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for path in python_paths {
        let stripped = if let Some(s) = path
            .strip_suffix("/__init__.py")
            .or_else(|| path.strip_suffix("/__init__.pyi"))
        {
            s
        } else if let Some(s) = path
            .strip_suffix(".pyi")
            .or_else(|| path.strip_suffix(".py"))
        {
            s
        } else {
            continue;
        };
        for root in &roots {
            let rel = if root.is_empty() {
                Some(stripped)
            } else {
                stripped
                    .strip_prefix(root.as_str())
                    .and_then(|r| r.strip_prefix('/'))
            };
            let Some(rel) = rel else { continue };
            if rel.is_empty() {
                continue;
            }
            map.entry(rel.replace('/', "."))
                .or_default()
                .push(path.clone());
        }
    }
    // Deduplicate
    for paths in map.values_mut() {
        paths.sort();
        paths.dedup();
    }
    map
}

/// The project files a dotted Python module path may legitimately name, or
/// `None` when the path is not a project module at all (so the caller binds it
/// to the `<external>` sentinel).
///
/// The map is already root-relative, so this is a lookup — but it stays a named
/// function because "is this a project module?" is a decision the caller makes
/// twice (bind vs. `<external>`) and the two must not drift.
pub(super) fn project_module_files(
    python_module: &str,
    python_module_map: &HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    python_module_map.get(python_module).cloned()
}

/// Return indexed Python files in the same import package as `caller_path`.
///
/// The module map can contain more than one spelling for a file (for example
/// `src.pkg.mod` and `pkg.mod`). Package comparison therefore uses every map
/// key that names the caller and accepts files sharing any corresponding
/// dotted package prefix. Root modules share the empty package.
pub(super) fn python_same_package_files(
    caller_path: &str,
    python_module_map: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    fn package_name(module: &str, path: &str) -> String {
        if path.ends_with("/__init__.py") || path.ends_with("/__init__.pyi") {
            module.to_string()
        } else {
            module
                .rsplit_once('.')
                .map(|(package, _)| package.to_string())
                .unwrap_or_default()
        }
    }

    let caller_packages: HashSet<String> = python_module_map
        .iter()
        .filter(|(_, paths)| paths.iter().any(|path| path == caller_path))
        .map(|(module, _)| package_name(module, caller_path))
        .collect();
    if caller_packages.is_empty() {
        return HashSet::new();
    }

    python_module_map
        .iter()
        .filter(|(module, paths)| {
            paths
                .iter()
                .any(|path| caller_packages.contains(&package_name(module, path)))
        })
        .flat_map(|(_, paths)| paths.iter().cloned())
        .collect()
}

/// Resolve Python import targets within the files [`project_module_files`]
/// resolved the module to.
/// For `import X` (is_module_import): finds `<module>` nodes in those files.
/// For `from X import Y`: finds nodes named Y only in those files.
/// Returns None if no matching node exists yet.
pub(super) fn resolve_python_module_targets(
    module_files: &[String],
    is_module_import: bool,
    target_name: &str,
    node_id_to_path: &HashMap<i64, String>,
    name_to_ids: &HashMap<String, Vec<i64>>,
) -> Option<Vec<i64>> {
    let lookup_name = if is_module_import {
        "<module>"
    } else {
        target_name
    };
    let all_ids = name_to_ids.get(lookup_name)?;
    let targets: Vec<i64> = all_ids
        .iter()
        .filter(|nid| {
            node_id_to_path
                .get(nid)
                .map(|p| module_files.iter().any(|f| f == p))
                .unwrap_or(false)
        })
        .copied()
        .collect();
    if targets.is_empty() {
        None
    } else {
        Some(targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_parameters_stay_in_the_qualified_scope() {
        let source = "class Scope:\n    def invoke(self, run):\n        run()\n\ndef invoke():\n    return run()\n\ndef with_bound():\n    with context() as run:\n        run()\n";
        let tree = crate::parser::treesitter::parse_tree(source, "python").unwrap();
        let bindings = collect_python_local_bindings(&tree, source);
        assert!(
            bindings
                .get("Scope.invoke")
                .is_some_and(|names| names.contains("run")),
            "{bindings:?}"
        );
        assert!(
            !bindings
                .get("invoke")
                .is_some_and(|names| names.contains("run")),
            "{bindings:?}"
        );
        assert!(
            bindings
                .get("with_bound")
                .is_some_and(|names| names.contains("run")),
            "{bindings:?}; {}",
            tree.root_node().to_sexp()
        );
    }

    #[test]
    fn except_clause_alias_is_a_local_binding() {
        let source = "def invoke():\n    try:\n        operation()\n    except RuntimeError as api:\n        api.send()\n";
        let tree = crate::parser::treesitter::parse_tree(source, "python").unwrap();
        let bindings = collect_python_local_bindings(&tree, source);

        assert!(
            bindings
                .get("invoke")
                .is_some_and(|names| names.contains("api")),
            "{bindings:?}; {}",
            tree.root_node().to_sexp()
        );
    }

    #[test]
    fn global_and_comprehension_bindings_follow_python_scope_rules() {
        let source = "def invoke(items):\n    global api\n    api = None\n    values = [run for run in items]\n    return api(), run()\n\ndef outer():\n    value = None\n    def inner():\n        nonlocal value\n        return value()\n";
        let tree = crate::parser::treesitter::parse_tree(source, "python").unwrap();
        let bindings = collect_python_local_bindings(&tree, source);
        let invoke = bindings.get("invoke").expect("invoke scope");
        assert!(
            !invoke.contains("api"),
            "global escaped local set: {invoke:?}"
        );
        assert!(
            !invoke.contains("run"),
            "comprehension target leaked into function: {invoke:?}"
        );
        assert!(
            bindings
                .get("inner")
                .is_some_and(|names| names.contains("value")),
            "nonlocal must block a module import: {bindings:?}"
        );
    }

    fn map_of(paths: &[&str]) -> HashMap<String, Vec<String>> {
        build_python_module_map(&paths.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn plain_dotted_module_import_requires_its_full_path() {
        let module_map = map_of(&["pkg/__init__.py", "pkg/sub.py"]);
        let plain = PythonImportBinding {
            module: "pkg.sub".into(),
            imported_name: "pkg.sub".into(),
            is_module_import: true,
            is_explicit_alias: false,
        };

        assert_eq!(
            python_bound_call_target(
                std::slice::from_ref(&plain),
                &["pkg".into(), "sub".into()],
                &module_map,
            ),
            Some(("pkg.sub".into(), None))
        );
        assert_eq!(
            python_bound_call_target(std::slice::from_ref(&plain), &["pkg".into()], &module_map,),
            Some(("pkg".into(), None)),
            "`import pkg.sub` binds pkg, but must not reinterpret pkg.helper() as pkg.sub.helper()"
        );

        let alias = PythonImportBinding {
            is_explicit_alias: true,
            ..plain
        };
        assert_eq!(
            python_bound_call_target(std::slice::from_ref(&alias), &["alias".into()], &module_map,),
            Some(("pkg.sub".into(), None))
        );
        assert_eq!(
            python_bound_call_target(std::slice::from_ref(&alias), &["pkg".into()], &module_map,),
            Some(("pkg.sub".into(), None)),
            "an explicit alias remains authoritative even when it equals the root component"
        );
    }

    #[test]
    fn package_import_promotes_indexed_receiver_prefix_to_submodule() {
        let module_map = map_of(&["pkg/__init__.py", "pkg/sub.py", "pkg/sub/deep.py"]);
        let binding = PythonImportBinding {
            module: "pkg".into(),
            imported_name: "pkg".into(),
            is_module_import: true,
            is_explicit_alias: false,
        };

        assert_eq!(
            python_bound_call_target(
                std::slice::from_ref(&binding),
                &["pkg".into(), "sub".into()],
                &module_map,
            ),
            Some(("pkg.sub".into(), None))
        );
        assert_eq!(
            python_bound_call_target(
                std::slice::from_ref(&binding),
                &["pkg".into(), "sub".into(), "Owner".into()],
                &module_map,
            ),
            Some(("pkg.sub".into(), Some("Owner".into())))
        );
    }

    #[test]
    fn a_package_directory_is_not_an_import_root() {
        // `accelerate/` has `__init__.py`, so `import logging` inside it names
        // the standard library — never the sibling. This single rule is what
        // removed 864 phantom bindings from a 1,763-file third-party corpus
        // (audit 2026-08-22 P2-4).
        let m = map_of(&[
            "accelerate/__init__.py",
            "accelerate/logging.py",
            "huggingface_hub/__init__.py",
            "huggingface_hub/utils/__init__.py",
            "huggingface_hub/utils/logging.py",
        ]);
        assert_eq!(project_module_files("logging", &m), None);
        assert_eq!(
            project_module_files("accelerate.logging", &m),
            Some(vec!["accelerate/logging.py".to_string()])
        );
    }

    #[test]
    fn a_plain_directory_inside_a_package_is_not_an_import_root() {
        // PEP 420 made `__init__.py` optional, so a package routinely contains
        // subdirectories without one — vendored trees, data/asset dirs, plugin
        // folders. Checking only the directory ITSELF for `__init__.py` made
        // every such subdirectory a top-level import root, which is the same
        // phantom class the package rule above removed: `import logging` bound
        // to `mypkg/vendored/logging.py`.
        //
        // A directory is importable-from only when NO ancestor of it is a
        // package either — inside a package tree you are reached by dotted
        // path, never by being on `sys.path`.
        let m = map_of(&[
            "mypkg/__init__.py",
            "mypkg/app.py",
            "mypkg/vendored/logging.py",
            "mypkg/vendored/deep/json.py",
        ]);
        assert_eq!(project_module_files("logging", &m), None);
        assert_eq!(project_module_files("json", &m), None);
        assert_eq!(project_module_files("deep.json", &m), None);
        assert_eq!(project_module_files("vendored.logging", &m), None);
        // The dotted path from the project root still resolves — that is the
        // spelling an actual `sys.path` entry at the project root would use.
        assert_eq!(
            project_module_files("mypkg.vendored.logging", &m),
            Some(vec!["mypkg/vendored/logging.py".to_string()])
        );
    }

    #[test]
    fn a_plain_directory_is_an_import_root() {
        // The `src/` layout: `src/` carries no `__init__.py`, so it IS on the
        // path and `from db import save` in `src/app.py` names `src/db.py`.
        // Dropping this was measured as a real regression before the rule was
        // stated in terms of packages rather than path depth.
        let m = map_of(&["src/app.py", "src/db.py", "src/cache.py"]);
        assert_eq!(
            project_module_files("db", &m),
            Some(vec!["src/db.py".to_string()])
        );
        // …and it is still reachable by its full path from the project root.
        assert_eq!(
            project_module_files("src.db", &m),
            Some(vec!["src/db.py".to_string()])
        );
    }

    #[test]
    fn a_root_anchored_file_beats_a_basename_coincidence() {
        // `packaging/version.py` IS `packaging.version`; the vendored copy is
        // only reachable as `packaging.version` if its own parent is a plain
        // directory, and here it is not.
        let m = map_of(&[
            "packaging/__init__.py",
            "packaging/version.py",
            "setuptools/__init__.py",
            "setuptools/_vendor/__init__.py",
            "setuptools/_vendor/packaging/__init__.py",
            "setuptools/_vendor/packaging/version.py",
        ]);
        assert_eq!(
            project_module_files("packaging.version", &m),
            Some(vec!["packaging/version.py".to_string()])
        );
    }

    #[test]
    fn a_src_layout_package_keeps_its_dotted_path() {
        let m = map_of(&["src/myapp/__init__.py", "src/myapp/utils.py"]);
        assert_eq!(
            project_module_files("myapp.utils", &m),
            Some(vec!["src/myapp/utils.py".to_string()])
        );
        // `utils` alone is NOT importable: `src/myapp/` is a package.
        assert_eq!(project_module_files("utils", &m), None);
    }

    #[test]
    fn genuine_ambiguity_is_still_preserved() {
        // Two plain directories both on the path: the module name really is
        // ambiguous and both files stay candidates, as before.
        let m = map_of(&["a/utils.py", "b/utils.py"]);
        let got = project_module_files("utils", &m).unwrap();
        assert_eq!(got.len(), 2, "got {got:?}");
    }

    #[test]
    fn a_module_the_tree_never_mentions_stays_unknown() {
        let m = map_of(&["a/b.py"]);
        assert_eq!(project_module_files("numpy.linalg", &m), None);
    }

    #[test]
    fn stub_packages_follow_the_same_module_rules() {
        let m = map_of(&["stubs/pkg/__init__.pyi", "stubs/pkg/api.pyi"]);
        assert_eq!(
            project_module_files("pkg.api", &m),
            Some(vec!["stubs/pkg/api.pyi".to_string()])
        );
        assert_eq!(project_module_files("api", &m), None);
    }
}
