#!/usr/bin/env python3
"""mutate.py [ids...]: apply each mutation to /var/tmp/d518r2/mut/src, run the lib
suite, record failing tests, restore the file with `git checkout --` (the copy's
own git, whose only commit is f6e8c932's tree)."""
import subprocess, sys, os, re
SRC = "/var/tmp/d518r2/mut/src"
LOG = "/var/tmp/d518r2/logs/mut"
os.makedirs(LOG, exist_ok=True)
RES = "src/indexer/pipeline/resolve.rs"
IDX = "src/indexer/pipeline/index_files.rs"
DOM = "src/domain.rs"
SUR = "src/graph/surprising.rs"
M = {
 "M1_defines_in_run_never": (RES, 'WHERE p.path = ?1 AND n.name = ?2 LIMIT 1', 'WHERE p.path = ?1 AND n.name = ?2 AND 0 LIMIT 1'),
 "M2_restore_delete_never": (IDX, 'WHERE source_id = ?1 AND relation = ?2 AND metadata = ?3",', 'WHERE source_id = ?1 AND relation = ?2 AND metadata = ?3 AND 0",'),
 "M3_insert_relation_edges_counts_synthetic": (IDX, 'let counted = usize::from(!crate::domain::is_synthetic_import(relation, metadata));', 'let counted = 1 + 0 * usize::from(!crate::domain::is_synthetic_import(relation, metadata));'),
 "M4_restore_counts_synthetic": (IDX, '''edges_created += usize::from(!crate::domain::is_synthetic_import(
                            relation.as_str(),
                            metadata.as_deref(),
                        ));''', 'edges_created += 1;'),
 "M5_postpass_drops_attr_bound": (IDX, 'bound: bound + attr_bound,', 'bound: bound + 0 * attr_bound,'),
 "M6_postpass_drops_attr_pruned": (IDX, 'pruned: pruned + attr_pruned,', 'pruned: pruned + 0 * attr_pruned,'),
 "M7_scoped_arm2_callers_empty": (RES, "e.metadata LIKE '%\\\"attr\\\":%') f", "e.metadata LIKE '%\\\"attr\\\":%' AND 0) f"),
 "M8_scoped_arm1_scope_files_empty": (RES, "FROM (SELECT s.fid FROM cg_scope_files s\n", "FROM (SELECT s.fid FROM cg_scope_files s WHERE 0\n"),
 "M9_scoped_table_empty": (RES, 'WHERE {SYNTHETIC}"', 'WHERE {SYNTHETIC} AND 0"'),
 "M10_is_synthetic_import_false": (DOM, 'relation == REL_IMPORTS && metadata.is_some_and(|m| m.contains(r#""attr":"#))', '(relation == REL_IMPORTS && metadata.is_some_and(|m| m.contains(r#""attr":"#))) && false'),
 "M11_refresh_loop_includes_target_file": (RES, 'for file in files.iter().filter(|f| **f != row.target_path) {', 'for file in files.iter() {'),
 "M12_defines_anywhere_not_in_run": (RES, '''"SELECT 1 FROM cg_fanout_paths p
         CROSS JOIN files f ON f.path = p.path
         CROSS JOIN nodes n ON n.file_id = f.id
         WHERE p.path = ?1 AND n.name = ?2 LIMIT 1",''', '''"SELECT 1 FROM files f
         CROSS JOIN nodes n ON n.file_id = f.id
         WHERE f.path = ?1 AND n.name = ?2 LIMIT 1",'''),
 "M13_delete_guard_always": (IDX, '''                        if attr_import.is_some() {
                            db.conn().execute(''', '''                        if true {
                            db.conn().execute('''),
 "M14_surprising_type_alias_leg": (SUR, "nt.type IN ('interface', 'type')", "nt.type IN ('interface')"),
 "M15_surprising_relation_leg": (SUR, "e.relation = ?1 AND nt.type IN", "(NOT e.relation = ?1 OR 1) AND nt.type IN"),
 "M16_surprising_tsx_leg": (SUR, "tf.language IN ('typescript', 'tsx')", "tf.language IN ('typescript')"),
 "M17_refresh_rows_skip_in_module_branch": (RES, '''            if defines_in_run.exists(rusqlite::params![file, row.name])? {
                out.push(row.importer.clone());
                break;
            }''', '''            if defines_in_run.exists(rusqlite::params![file, row.name])? && false {
                out.push(row.importer.clone());
                break;
            }'''),
}
env = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d518r2/tgt/mut")
ids = sys.argv[1:] or list(M)
for mid in ids:
    f, old, new = M[mid]
    p = os.path.join(SRC, f)
    s = open(p).read()
    n = s.count(old)
    if n != 1:
        print(f"{mid}: anchor count {n}, SKIPPED", flush=True); continue
    open(p, "w").write(s.replace(old, new))
    try:
        r = subprocess.run(["cargo", "test", "--lib", "--no-fail-fast"], cwd=SRC, env=env, capture_output=True, text=True)
        out = r.stdout + r.stderr
        open(f"{LOG}/{mid}.log", "w").write(out)
        if "could not compile" in out or "error[E" in out:
            print(f"{mid}: COMPILE ERROR", flush=True); continue
        failed = re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M)
        summ = re.findall(r"test result: .*", out)
        print(f"{mid}: {'KILLED' if failed else 'SURVIVED'} ({len(failed)} failed) {summ[-1] if summ else ''}", flush=True)
        for t in failed: print(f"    {t}", flush=True)
    finally:
        subprocess.run(["git", "checkout", "--", f], cwd=SRC, check=True)
        assert subprocess.run(["git", "status", "--porcelain"], cwd=SRC, capture_output=True, text=True).stdout == ""
