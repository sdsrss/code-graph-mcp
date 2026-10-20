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
        .filter_map(|p| p.strip_suffix("/__init__.py"))
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
        let stripped = if let Some(s) = path.strip_suffix("/__init__.py") {
            s
        } else if let Some(s) = path.strip_suffix(".py") {
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

/// [`project_module_files`] for a module path as `importer` writes it. A
/// relative path (`.globals`, `..helpers`, `.`) names a file of the importer's
/// own package: one dot is its directory, each further dot one level up
/// (PEP 328). It went to the `<external>` sentinel before, which cut every
/// module of a package off from its siblings — flask's `src/flask/` imports
/// nothing else (D10, 2026-09-29 usage evaluation). The map holds every file
/// under its dotted path from the project root, so the key is looked up there
/// and kept only for the exact file it spells, since another import root could
/// register the same key for a different file. `None` above the project root.
pub(super) fn project_module_files_from(
    python_module: &str,
    importer: &str,
    python_module_map: &HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    let Some(mut rest) = python_module.strip_prefix('.') else {
        return project_module_files(python_module, python_module_map);
    };
    let mut dir = importer.rsplit_once('/').map_or("", |(d, _)| d);
    while let Some(r) = rest.strip_prefix('.') {
        if dir.is_empty() {
            return None;
        }
        dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
        rest = r;
    }
    let path = match (dir, rest) {
        ("", "") => return None,
        ("", r) => r.replace('.', "/"),
        (d, "") => d.to_string(),
        (d, r) => format!("{d}/{}", r.replace('.', "/")),
    };
    let wanted = [format!("{path}.py"), format!("{path}/__init__.py")];
    let files: Vec<String> = python_module_map
        .get(&path.replace('/', "."))?
        .iter()
        .filter(|f| wanted.contains(f))
        .cloned()
        .collect();
    (!files.is_empty()).then_some(files)
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

/// How deep a re-export chain is followed: `pkg/__init__.py` re-exporting from
/// `pkg/sub/__init__.py`, which re-exports from `pkg/sub/impl.py`, is two.
const REEXPORT_MAX_DEPTH: usize = 8;

/// The nodes `module_files` re-export under `name` (D#370(a)): what `from
/// <module> import name` binds when the module defines no `name` node but
/// imports one itself — flask's `src/flask/__init__.py` holds only `from .app
/// import Flask as Flask`, and the name chain bound `from flask import Flask`
/// to a class local to a test function instead.
///
/// Read from the module's own `imports` edges, so it answers only once those
/// exist; the deferred pass asks again until nothing moves. An edge counts only
/// when it is verifiable: its target lies in the files its own `python_module`
/// names, or is in turn one of their re-exports. An import the name chain bound
/// is never followed, so the answer does not depend on whether that edge was
/// written yet, which is what makes an incremental run agree with a rebuild.
/// The name a module exports is its `as` when the import renamed it.
pub(super) fn python_reexport_targets(
    conn: &rusqlite::Connection,
    module_files: &[String],
    name: &str,
    python_module_map: &HashMap<String, Vec<String>>,
) -> anyhow::Result<Vec<i64>> {
    let mut on_path: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for file in module_files {
        out.extend(reexports_of(
            conn,
            file,
            name,
            python_module_map,
            &mut on_path,
        )?);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// [`python_reexport_targets`] for one file. `on_path` holds the `(file, name)`
/// pairs being answered further up, so a re-export cycle (`a` from `b`, `b`
/// from `a`) verifies nothing; answers are not cached across branches, since
/// one cut short by a cycle would be wrong where the cycle is not.
fn reexports_of(
    conn: &rusqlite::Connection,
    file: &str,
    name: &str,
    python_module_map: &HashMap<String, Vec<String>>,
    on_path: &mut HashSet<(String, String)>,
) -> anyhow::Result<Vec<i64>> {
    let key = (file.to_string(), name.to_string());
    if on_path.len() > REEXPORT_MAX_DEPTH || !on_path.insert(key.clone()) {
        return Ok(Vec::new());
    }
    let rows = conn
        .prepare_cached(
            "SELECT e.target_id, tn.name, tf.path, json_extract(e.metadata, '$.python_module')
             FROM files f
             JOIN nodes sn ON sn.file_id = f.id AND sn.name = '<module>'
             JOIN edges e ON e.source_id = sn.id AND e.relation = 'imports'
             JOIN nodes tn ON tn.id = e.target_id
             JOIN files tf ON tf.id = tn.file_id
             WHERE f.path = ?1
               AND json_extract(e.metadata, '$.python_module') IS NOT NULL
               AND json_extract(e.metadata, '$.is_module_import') IS NULL
               AND COALESCE(json_extract(e.metadata, '$.as'), tn.name) = ?2",
        )?
        .query_map(rusqlite::params![file, name], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (target, target_name, target_path, module) in rows {
        let Some(files) = project_module_files_from(&module, file, python_module_map) else {
            continue;
        };
        let mut verified = files.contains(&target_path);
        for f in &files {
            if verified {
                break;
            }
            verified =
                reexports_of(conn, f, &target_name, python_module_map, on_path)?.contains(&target);
        }
        if verified {
            out.push(target);
        }
    }
    on_path.remove(&key);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_of(paths: &[&str]) -> HashMap<String, Vec<String>> {
        build_python_module_map(&paths.iter().map(|p| p.to_string()).collect())
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
}
