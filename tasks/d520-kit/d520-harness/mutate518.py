import subprocess, sys, os, json
T = "/var/tmp/d520/mut"
env = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d520/mut-target", PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
IF = "src/indexer/pipeline/index_files.rs"; RS = "src/indexer/pipeline/resolve.rs"; PM = "src/indexer/pipeline/python_modules.rs"
MB = "src/parser/relations/member.rs"; IM = "src/parser/relations/imports.rs"; DM = "src/domain.rs"
ED = "src/storage/queries/edges.rs"; IQ = "src/storage/queries/imports.rs"; PJ = "src/storage/queries/project_map.rs"; DC = "src/storage/queries/dead_code.rs"
FILTER = ["python", "module_attr", "synthetic", "big_run", "attr"]
BATCH_FILTER = open(os.path.join(T, IF)).read().split("let found = match found {\n",1)[1].split("found => found,",1)[0]
M = {
 "P4_dotted_import_binds": (MB, '                Some("import_statement") if first => Occurrence::Other,\n', ''),
 "R3_no_module_drop": (IF, 'if module_files.is_none() && meta.get("attr").is_some() {', 'if false {'),
 "R4_restore_module_by_name": (IF, 'let attr_import = (relation.as_str() == REL_IMPORTS && target_name == "<module>")', 'let attr_import = (false)'),
 "R5_restore_every_attr_requeued": (IF, 'let attr_import = (relation.as_str() == REL_IMPORTS && target_name == "<module>")', 'let attr_import = (relation.as_str() == REL_IMPORTS)'),
 "R6_attr_calls_pending": (IF, "        if super::resolve::is_module_attr_call(metadata.as_deref()) {\n            let source_path", "        if false {\n            let source_path"),
 "R7_restore_attr_call_by_name": (IF, "|| super::resolve::is_module_attr_call(metadata.as_deref()));", ");"),
 "R9_methods_kept_direct_batch": (IF, BATCH_FILTER, BATCH_FILTER.replace('if meta.get("attr").is_some()', 'if false')),
 "S1_reexport_reads_attr": (PM, "                       AND json_extract(e.metadata, '$.attr') IS NULL\",", "\","),
 "S2_drift_reads_attr": (RS, "      AND json_extract(e.metadata, '$.as') IS NULL\n      AND json_extract(e.metadata, '$.attr') IS NULL\";", "      AND json_extract(e.metadata, '$.as') IS NULL\";"),
 "S3_unique_imports_attr": (RS, "           AND json_extract(ie.metadata, '$.attr') IS NULL\n", ""),
 "S4_prune_reads_attr": (RS, "                    AND i.attr IS NULL\n              )", "              )"),
 "S5_prune_this_target_attr": (RS, "                    AND i2.attr IS NULL\n", ""),
 "S6_exemption_any_call": (RS, "AND (i.attr IS NULL\n                                 OR (i.attr = json_extract(e.metadata, '$.attr')\n                                     AND i.module = json_extract(e.metadata, '$.v')))", ""),
 "S7_exemption_no_module_check": (RS, "\n                                     AND i.module = json_extract(e.metadata, '$.v')))", "))"),
 "F1_refs": (ED, "WHERE e.target_id = ?1 AND NOT {synthetic}\n", "WHERE e.target_id = ?1 AND (NOT {synthetic} OR 1)\n"),
 "F1b_refs_filtered": (ED, "WHERE e.target_id = ?1 AND e.relation = ?2 AND NOT {synthetic}", "WHERE e.target_id = ?1 AND e.relation = ?2 AND (NOT {synthetic} OR 1)"),
 "F2_context_out": (ED, "WHERE e.source_id IN ({}) AND NOT {} ORDER BY", "WHERE e.source_id IN ({}) AND (NOT {} OR 1) ORDER BY"),
 "F2b_context_in": (ED, "WHERE e.target_id IN ({}) AND NOT {} ORDER BY", "WHERE e.target_id IN ({}) AND (NOT {} OR 1) ORDER BY"),
 "F3_stats": (ED, "         WHERE NOT {}\n         GROUP BY lang, e.relation", "         WHERE (NOT {} OR 1)\n         GROUP BY lang, e.relation"),
 "F4_count": (ED, "    Ok(all - synthetic)", "    Ok(all - synthetic * 0)"),
 "F5_closure": (IQ, "                   AND NOT {synthetic}\n", "                   AND (NOT {synthetic} OR 1)\n"),
 "F6_symbol_counts": (IQ, "           AND NOT {synthetic}\n         JOIN nodes nb", "           AND (NOT {synthetic} OR 1)\n         JOIN nodes nb"),
 "F7_cycles": (IQ, "           AND NOT {} \\\n", "           AND (NOT {} OR 1) \\\n"),
 "F8_map": (PJ, "AND sf.path != tf.path AND NOT {} \\", "AND sf.path != tf.path AND (NOT {} OR 1) \\"),
 "F9_dead": (DC, "                 AND NOT {synthetic}\n", "                 AND (NOT {synthetic} OR 1)\n"),
}
only = sys.argv[1:]
for name, (f, a, b) in M.items():
    if only and name not in only: continue
    p = os.path.join(T, f); orig = open(p).read()
    if orig.count(a) != 1: print(name, "PATTERN COUNT", orig.count(a), flush=True); continue
    open(p, "w").write(orig.replace(a, b))
    try:
        r = subprocess.run(["cargo", "test", "--lib", "--", *FILTER], cwd=T, env=env, capture_output=True, text=True)
        out = r.stdout + r.stderr
        failed = [l.split()[1].split("::")[-1] for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
        if not failed and ("error[" in out or "\nerror:" in out and "test result" not in out):
            print(name, "BUILD ERROR", [l for l in out.splitlines() if l.startswith("error")][:2], flush=True)
        else:
            print(name, "KILLED by" if failed else "SURVIVED", failed[:4], flush=True)
    finally:
        open(p, "w").write(orig)
