"""mutate.py <id>: apply mutation <id> to /var/tmp/d175-review/mut (reversible via restore)."""
import sys, os, shutil
M = "/var/tmp/d175-review/mut/"
P = "src/indexer/pipeline/"
MUT = {
 "M1_restore_no_module_level": (P+"index_files.rs", 'src_lang == "python" && found.iter().any(|(_, _, ty)| *ty != "method");', 'false && found.iter().any(|(_, _, ty)| *ty != "method");'),
 "M2_resolve_no_method_filter": (P+"python_modules.rs", "if !is_module_import && targets.len() > 1 {", "if false && !is_module_import && targets.len() > 1 {"),
 "M3_resolve_drop_methods_always": (P+"python_modules.rs", "if methods.len() < targets.len() {", "if true {"),
 "M4_root_change_off": (P+"index_files.rs", "    let mut package_changed = delete_paths.iter().any(|p| is_init(p));", "    let mut package_changed = delete_paths.iter().any(|p| is_init(p));\n    if true { return Ok(Vec::new()); }"),
 "M5_root_change_no_sentinels": (P+"index_files.rs", "if importer.ends_with(\".py\") && changed.contains(&module) {", "if false && importer.ends_with(\".py\") && changed.contains(&module) {"),
 "M6_changed_paths_new_keys_only": (P+"python_modules.rs", "    old.keys()\n        .chain(new.keys())", "    new.keys()\n        .chain(new.keys())"),
 "M7_drift_no_rebind_gate": (P+"resolve.rs", "if (names_moved && !module_row && rebinds)", "if (names_moved)"),
 "M7b_drift_no_rebind_gate": (P+"resolve.rs", "if (names_moved && !module_row && rebinds)", "if (names_moved && (module_row || !module_row) && (rebinds || true))"),
 "M8b_drift_no_module_branch": (P+"resolve.rs", "drifted.contains(target_path) || names.contains(&stem(target_path))", "false && (drifted.contains(target_path) || names.contains(&stem(target_path)))"),
 "M21_drift_sentinel_off": (P+"resolve.rs", "|| (target_path == crate::domain::EXTERNAL_FILE_PATH && names.contains(target_name))", "|| (false && target_path == crate::domain::EXTERNAL_FILE_PATH && names.contains(target_name))"),
 "M22_restore_filter_imports_only": (P+"index_files.rs", 'src_lang == "python" && found.iter().any(|(_, _, ty)| *ty != "method");', 'src_lang == "python" && relation.as_str() == crate::domain::REL_IMPORTS && found.iter().any(|(_, _, ty)| *ty != "method");'),
 "M13b_no_verify": (P+"python_modules.rs", "let mut verified = files.contains(&target_path);", "let mut verified = files.contains(&target_path) || true;"),
 "M14b_no_relative_bound": (P+"index_files.rs", "                relative_bound.push(d);", "                if false { relative_bound.push(d); }"),
 "M15b_no_forget": (P+"index_files.rs", "            reexports.forget(&d.rel_path);", "            if false { reexports.forget(&d.rel_path); }"),
 "M7c_drift_no_rebind_gate": (P+"resolve.rs", "if (names_moved && !module_row && rebinds)", "if (names_moved && (*module_row || !*module_row) && (rebinds || true))"),
 "M8_drift_no_module_branch": (P+"resolve.rs", "drifted.contains(target_path) || names.contains(&stem(target_path))", "false"),
 "M9_fanout_rounds_zero": (P+"mod.rs", "const REEXPORT_FANOUT_ROUNDS: usize = 8;", "const REEXPORT_FANOUT_ROUNDS: usize = 0;"),
 "M10_ignore_n": (P+"python_modules.rs", "if matched.iter().any(|e| e.sources.is_some()) {", "if false && matched.iter().any(|e| e.sources.is_some()) {"),
 "M11_count_lt3": ("src/parser/relations/imports.rs", "if count < 2 {", "if count < 3 {"),
 "M12_follow_renamed": (P+"python_modules.rs", "                       AND json_extract(e.metadata, '$.as') IS NULL\",", "\","),
 "M13_no_verify": (P+"python_modules.rs", "let mut verified = files.contains(&target_path);", "let mut verified = true;"),
 "M14_no_relative_bound": (P+"index_files.rs", "                relative_bound.push(d);", "                let _ = d;"),
 "M15_no_forget": (P+"index_files.rs", "            reexports.forget(&d.rel_path);", ""),
 "M16_ts_no_same_file": (P+"resolve.rs", "                    OR {TS_TYPE_CALL})\"", "                    OR (0 AND {TS_TYPE_CALL}))\""),
 "M17_ts_interface_only": (P+"resolve.rs", "AND tgt.type IN ('interface', 'type')", "AND tgt.type IN ('interface')"),
 "M18_snapshot_no_deletes": (P+"index_files.rs", "            files.iter().chain(delete_paths.iter()),", "            files.iter(),"),
 "M19_restore_module_level_any_lang": (P+"index_files.rs", 'src_lang == "python" && found.iter().any(|(_, _, ty)| *ty != "method");', 'found.iter().any(|(_, _, ty)| *ty != "method");'),
 "M20_depth_1": (P+"python_modules.rs", "const REEXPORT_MAX_DEPTH: usize = 8;", "const REEXPORT_MAX_DEPTH: usize = 0;"),
}
cmd, mid = sys.argv[1], sys.argv[2]
f, old, new = MUT[mid]
p = M + f
if cmd == "apply":
    s = open(p).read()
    assert s.count(old) == 1, (mid, s.count(old))
    shutil.copy(p, p + ".orig")
    open(p, "w").write(s.replace(old, new))
elif cmd == "restore":
    shutil.move(p + ".orig", p)
