//! JS/TS module specifier resolution. `import { x } from '../util/helper'`
//! carries a relative path that must be resolved against the importing file's
//! directory plus extension probing to a concrete indexed file, so the indexer
//! can bind the import edge to the right definition instead of a path-proximity
//! guess (the Python analog is `python_modules::resolve_python_module_targets`).
//!
//! Also hosts `resolve_php_include_path` — PHP `require`/`include 'lib.php'`
//! is the same shape (a directory-relative file path), so it reuses the same
//! normalization to bind the include edge to the included file's `<module>`.
//!
//! Only RELATIVE specifiers (`./`, `../`) are resolved here. Bare specifiers
//! (`react`, `lodash`) and tsconfig path aliases are left to the existing
//! name-based handling / `<external>` sentinels — resolving them needs
//! node_modules / tsconfig context the indexer does not have.

use std::collections::{HashMap, HashSet};

/// Candidate file extensions probed for a relative specifier, in
/// TypeScript-first resolution order (a `.ts` next to a stale `.js` wins).
const JS_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

/// Resolve a relative ES module specifier to the indexed file it refers to.
/// Returns the normalized repo-relative file path present in `file_set`, trying
/// `<base>`, `<base>.<ext>`, then `<base>/index.<ext>`. None for bare specifiers
/// or when no candidate is indexed.
pub(super) fn resolve_js_specifier_path(
    specifier: &str,
    importer_rel_path: &str,
    file_set: &HashSet<String>,
) -> Option<String> {
    // Only relative specifiers are file-resolvable without node_modules/tsconfig.
    // A bare `.` / `..` names a directory as `./` / `../` does (express's tests
    // load the package as `require('..')`).
    let dir_spec;
    let specifier = if specifier == "." || specifier == ".." {
        dir_spec = format!("{specifier}/");
        dir_spec.as_str()
    } else {
        specifier
    };
    if !(specifier.starts_with("./") || specifier.starts_with("../")) {
        return None;
    }

    // Base = importer's directory joined with the specifier, then normalized
    // (collapse `.` and `..`). importer_rel_path is repo-relative with '/'.
    let importer_dir = match importer_rel_path.rsplit_once('/') {
        Some((dir, _)) => dir,
        None => "", // importer at repo root
    };
    let joined = if importer_dir.is_empty() {
        specifier.to_string()
    } else {
        format!("{}/{}", importer_dir, specifier)
    };
    let base = normalize_rel_path(&joined)?;

    // Specifier already names an indexed file (explicit extension).
    if file_set.contains(&base) {
        return Some(base);
    }
    // `<base>.<ext>`
    for ext in JS_EXTENSIONS {
        let cand = format!("{}.{}", base, ext);
        if file_set.contains(&cand) {
            return Some(cand);
        }
    }
    // `<base>/index.<ext>` (`index.<ext>` when the base is the repo root)
    for ext in JS_EXTENSIONS {
        let cand = if base.is_empty() {
            format!("index.{ext}")
        } else {
            format!("{}/index.{}", base, ext)
        };
        if file_set.contains(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Resolve import targets to nodes named `target_name` in the specifier-resolved
/// file. None if the specifier doesn't resolve to an indexed file or no matching
/// node exists there (caller falls back to default name-based resolution).
pub(super) fn resolve_js_module_targets(
    specifier: &str,
    importer_rel_path: &str,
    target_name: &str,
    file_set: &HashSet<String>,
    name_to_ids: &HashMap<String, Vec<i64>>,
    node_id_to_path: &HashMap<i64, String>,
) -> Option<Vec<i64>> {
    let file = resolve_js_specifier_path(specifier, importer_rel_path, file_set)?;
    let all_ids = name_to_ids.get(target_name)?;
    let targets: Vec<i64> = all_ids
        .iter()
        .filter(|id| node_id_to_path.get(id).map(|p| p == &file).unwrap_or(false))
        .copied()
        .collect();
    if targets.is_empty() {
        None
    } else {
        Some(targets)
    }
}

/// Resolve a PHP `require`/`include` path to the indexed file it refers to.
/// PHP includes are resolved against the including file's directory (the common
/// `require_once 'lib.php'`, `require 'sub/User.php'`, and
/// `require __DIR__ . '/config.php'` forms — the last yields a leading-`/`
/// path that is still importer-relative). Returns the normalized repo-relative
/// path present in `file_set`, trying the path as-given then `<path>.php`. None
/// for paths that don't resolve to an indexed file (caller leaves the edge
/// unresolved → `<external>`), biasing toward false-negatives over wrong binds.
pub(super) fn resolve_php_include_path(
    include_path: &str,
    importer_rel_path: &str,
    file_set: &HashSet<String>,
) -> Option<String> {
    // Strip a leading `./` or `/` (the latter from `__DIR__ . '/x.php'`) so the
    // join stays repo-relative. Absolute filesystem includes can't be resolved
    // against the index, so a bare `/etc/...` simply won't match file_set.
    let rel = include_path
        .trim_start_matches("./")
        .trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    let importer_dir = match importer_rel_path.rsplit_once('/') {
        Some((dir, _)) => dir,
        None => "",
    };
    let joined = if importer_dir.is_empty() {
        rel.to_string()
    } else {
        format!("{}/{}", importer_dir, rel)
    };
    let base = normalize_rel_path(&joined)?;
    if file_set.contains(&base) {
        return Some(base);
    }
    // Extensionless include (`require 'lib'`) → probe `.php`.
    let with_ext = format!("{}.php", base);
    if file_set.contains(&with_ext) {
        return Some(with_ext);
    }
    None
}

/// Resolve a C/C++ `#include "path"` to a concrete indexed file. Local includes
/// carry an explicit header extension (`.h/.hpp/.hxx/.hh`), so — unlike PHP — no
/// extensionless probing is needed: a directory-relative join + repo-root-relative
/// fallback, each an exact `file_set` membership check. System includes
/// (`<stdio.h>`, already stripped of brackets) won't be in the set → None (the
/// caller then falls back to `<external>` resolution).
pub(super) fn resolve_c_include_path(
    include_path: &str,
    importer_rel_path: &str,
    file_set: &HashSet<String>,
) -> Option<String> {
    let rel = include_path.trim_start_matches("./");
    if rel.is_empty() {
        return None;
    }
    // 1. Relative to the including file's directory (`#include "sibling.h"`).
    let importer_dir = match importer_rel_path.rsplit_once('/') {
        Some((dir, _)) => dir,
        None => "",
    };
    let joined = if importer_dir.is_empty() {
        rel.to_string()
    } else {
        format!("{}/{}", importer_dir, rel)
    };
    if let Some(base) = normalize_rel_path(&joined) {
        if file_set.contains(&base) {
            return Some(base);
        }
    }
    // 2. Relative to the repo root (`#include "src/common/widget.h"` from an
    //    include root), for projects that pass `-I.`.
    if let Some(base) = normalize_rel_path(rel) {
        if file_set.contains(&base) {
            return Some(base);
        }
    }
    None
}

/// Normalize a repo-relative path, collapsing `.` and `..` segments. Returns
/// None if it escapes the repo root (a leading `..` with nothing to pop), which
/// cannot correspond to an indexed file.
fn normalize_rel_path(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolves_parent_relative_with_ts_extension() {
        let files = fs(&[
            "src/util/helper.ts",
            "src/core/caller.ts",
            "src/core/helper.ts",
        ]);
        assert_eq!(
            resolve_js_specifier_path("../util/helper", "src/core/caller.ts", &files),
            Some("src/util/helper.ts".to_string())
        );
    }

    #[test]
    fn resolves_same_dir_relative() {
        let files = fs(&["src/core/helper.js", "src/core/caller.js"]);
        assert_eq!(
            resolve_js_specifier_path("./helper", "src/core/caller.js", &files),
            Some("src/core/helper.js".to_string())
        );
    }

    #[test]
    fn resolves_index_file() {
        let files = fs(&["src/util/index.ts", "src/core/caller.ts"]);
        assert_eq!(
            resolve_js_specifier_path("../util", "src/core/caller.ts", &files),
            Some("src/util/index.ts".to_string())
        );
    }

    #[test]
    fn bare_specifier_is_unresolved() {
        let files = fs(&["src/core/caller.ts", "node_modules/react/index.js"]);
        assert_eq!(
            resolve_js_specifier_path("react", "src/core/caller.ts", &files),
            None
        );
    }

    #[test]
    fn escaping_root_is_none() {
        let files = fs(&["caller.ts"]);
        assert_eq!(
            resolve_js_specifier_path("../../x", "caller.ts", &files),
            None
        );
    }

    #[test]
    fn ts_preferred_over_js() {
        let files = fs(&[
            "src/util/helper.js",
            "src/util/helper.ts",
            "src/core/caller.ts",
        ]);
        assert_eq!(
            resolve_js_specifier_path("../util/helper", "src/core/caller.ts", &files),
            Some("src/util/helper.ts".to_string())
        );
    }

    #[test]
    fn php_include_resolves_same_dir() {
        let files = fs(&["src/app.php", "src/lib.php"]);
        // require_once 'lib.php' from src/app.php → src/lib.php
        assert_eq!(
            resolve_php_include_path("lib.php", "src/app.php", &files),
            Some("src/lib.php".to_string())
        );
    }

    #[test]
    fn php_include_resolves_subdir_and_dir_constant() {
        let files = fs(&["app.php", "models/User.php", "config.php"]);
        // require 'models/User.php' from app.php → models/User.php
        assert_eq!(
            resolve_php_include_path("models/User.php", "app.php", &files),
            Some("models/User.php".to_string())
        );
        // require __DIR__ . '/config.php' → extract_string yields '/config.php';
        // the leading slash is stripped and resolved against the importer dir.
        assert_eq!(
            resolve_php_include_path("/config.php", "app.php", &files),
            Some("config.php".to_string())
        );
    }

    #[test]
    fn php_include_extensionless_probes_php() {
        let files = fs(&["src/app.php", "src/lib.php"]);
        assert_eq!(
            resolve_php_include_path("lib", "src/app.php", &files),
            Some("src/lib.php".to_string())
        );
    }

    #[test]
    fn php_include_unindexed_is_none() {
        // Vendored/third-party include not in the index → unresolved (→ <external>).
        let files = fs(&["src/app.php"]);
        assert_eq!(
            resolve_php_include_path("vendor/autoload.php", "src/app.php", &files),
            None
        );
    }
}
