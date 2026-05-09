//! Python module path resolution. `import myapp.utils` and `from myapp.utils
//! import helper` carry dotted module paths that don't directly map to file
//! names, so the indexer pre-builds a `dotted_path → file_paths` map and
//! consults it during Phase 2 import-edge resolution.
//!
//! Suffix matching deliberately fans out: `utils` matches every `*/utils.py`
//! we know about. Over-connecting is the safer failure mode for dependency
//! analysis without `sys.path` context — a missed dependency is harder to
//! debug than an extra one.

use std::collections::{HashMap, HashSet};

/// Build mapping from Python dotted module paths to file paths.
/// Registers both full paths and suffix paths for flexible matching.
/// e.g., "src/myapp/utils.py" matches "src.myapp.utils", "myapp.utils", and "utils".
pub(super) fn build_python_module_map(python_paths: &HashSet<String>) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for path in python_paths {
        let stripped = if let Some(s) = path.strip_suffix("/__init__.py") {
            s
        } else if let Some(s) = path.strip_suffix(".py") {
            s
        } else {
            continue;
        };

        // Register all suffix module paths for flexible matching
        // e.g., "src/myapp/utils" -> "src.myapp.utils", "myapp.utils", "utils"
        let parts: Vec<&str> = stripped.split('/').collect();
        for i in 0..parts.len() {
            let dotted = parts[i..].join(".");
            map.entry(dotted).or_default().push(path.clone());
        }
    }
    // Deduplicate
    for paths in map.values_mut() {
        paths.sort();
        paths.dedup();
    }
    map
}

/// Resolve Python import targets using pre-parsed module metadata.
/// For `import X` (is_module_import): finds `<module>` nodes in resolved files.
/// For `from X import Y`: finds nodes named Y only in resolved files.
/// Returns None if module can't be resolved or no matching nodes found.
pub(super) fn resolve_python_module_targets(
    python_module: &str,
    is_module_import: bool,
    target_name: &str,
    python_module_map: &HashMap<String, Vec<String>>,
    node_id_to_path: &HashMap<i64, String>,
    name_to_ids: &HashMap<String, Vec<i64>>,
) -> Option<Vec<i64>> {
    // Resolve module path to file path(s).
    // Note: suffix matching in python_module_map means `import utils` may match
    // multiple files (e.g., "myapp/utils.py" and "other/utils.py"). This is an
    // inherent ambiguity without sys.path context; over-connecting is safer for
    // dependency analysis than missing real dependencies.
    let module_files = python_module_map.get(python_module)?;

    let lookup_name = if is_module_import { "<module>" } else { target_name };
    let all_ids = name_to_ids.get(lookup_name)?;
    let targets: Vec<i64> = all_ids.iter()
        .filter(|nid| {
            node_id_to_path.get(nid)
                .map(|p| module_files.contains(p))
                .unwrap_or(false)
        })
        .copied()
        .collect();
    if targets.is_empty() { None } else { Some(targets) }
}
