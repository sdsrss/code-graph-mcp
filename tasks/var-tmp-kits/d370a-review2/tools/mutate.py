#!/usr/bin/env python3
"""mutate.py [ids...]: apply each mutation to /var/tmp/d370a-review2/src/mut (restored from the
pristine src/head copy before and after), run `cargo test --lib -- python`, report killed/survived."""
import os, re, shutil, subprocess, sys
R = "/var/tmp/d370a-review2"
PRISTINE = f"{R}/src/head"; MUT = f"{R}/src/mut"
M = {
 # parser: collect_py_module_names arms (member.rs)
 "MP1_for_in_clause": ("src/parser/relations/member.rs",
    '"assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" =>',
    '"assignment" | "augmented_assignment" | "for_statement" =>'),
 "MP2_named_expression": ("src/parser/relations/member.rs",
    '"named_expression"\n        | "function_definition"', '"function_definition"'),
 "MP3_class_definition": ("src/parser/relations/member.rs",
    '| "class_definition"\n', ''),
 "MP4_default_parameter": ("src/parser/relations/member.rs",
    '        | "default_parameter"\n        | "typed_default_parameter" => {', ' => {'),
 "MP5_lambda_parameters": ("src/parser/relations/member.rs",
    '"parameters" | "lambda_parameters" =>', '"parameters" =>'),
 "MP6_typed_splat_params": ("src/parser/relations/member.rs",
    '"typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {', '"__never__" => {'),
 "MP7_attr_subscript_not_binding": ("src/parser/relations/member.rs",
    '        "attribute" | "subscript" => {}\n', ''),
 "MP8_augmented_assignment": ("src/parser/relations/member.rs",
    '"assignment" | "augmented_assignment" | "for_statement"', '"assignment" | "for_statement"'),
 "MP9_function_definition": ("src/parser/relations/member.rs",
    '        | "function_definition"\n', '\n'),
 # resolver
 "MR2_seen_counts_edges": ("src/indexer/pipeline/python_modules.rs",
    '.any(|e| e.sources.is_some_and(|n| (seen.len() as i64) < n))',
    '.any(|e| e.sources.is_some_and(|n| (matched.len() as i64) < n && !seen.is_empty()))'),
 "MR3_nested_def_in_function": ("src/indexer/pipeline/python_modules.rs",
    "AND o.type IN ('function', 'method', 'class')", "AND o.type IN ('method', 'class')"),
 "MR6_decline_keeps_on_path": ("src/indexer/pipeline/python_modules.rs",
    "    {\n        on_path.remove(&key);\n        return Ok(Vec::new());\n    }", "    {\n        return Ok(Vec::new());\n    }"),
 # index_files
 "MI1_delete_path_pending": ("src/indexer/pipeline/index_files.rs",
    "if metadata.as_deref().is_some_and(is_module_attr_call) {", "if false && metadata.as_deref().is_some_and(is_module_attr_call) {"),
 "MI2_no_relative_d10": ("src/indexer/pipeline/index_files.rs",
    "            if relative {\n                relative_bound.push(d);\n            }", "            if relative && false {\n                relative_bound.push(d);\n            }"),
 # resolve.rs fan-out
 "MD1_snapshot_keeps_renamed": ("src/indexer/pipeline/resolve.rs",
    "      AND json_extract(e.metadata, '$.is_module_import') IS NULL\n      AND json_extract(e.metadata, '$.as') IS NULL\";",
    "      AND json_extract(e.metadata, '$.is_module_import') IS NULL\";"),
 "MD3_attr_arm_any_module": ("src/indexer/pipeline/resolve.rs",
    "                    if defines.exists(rusqlite::params![file, name])? {\n                        paths.push(caller);",
    "                    if true || defines.exists(rusqlite::params![file, name])? {\n                        paths.push(caller);"),
 # imports.rs count
 "MC1_count_by_original_name": ("src/parser/relations/imports.rs",
    '        let name = meta\n            .get("as")\n            .and_then(|a| a.as_str())\n            .map(str::to_string)\n            .unwrap_or_else(|| r.target_name.clone());',
    '        let name = r.target_name.clone();'),
}
ids = sys.argv[1:] or list(M)
env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TARGET_DIR=f"{R}/target-mut")
def restore(f):
    shutil.copyfile(os.path.join(PRISTINE, f), os.path.join(MUT, f))
for mid in ids:
    f, old, new = M[mid]
    restore(f)
    p = os.path.join(MUT, f); s = open(p).read()
    n = s.count(old)
    if n != 1:
        print(f"{mid}: pattern count {n} != 1, skipped", flush=True); continue
    open(p, "w").write(s.replace(old, new))
    log = f"{R}/mut-{mid}.log"
    with open(log, "w") as lf:
        rc = subprocess.run(["cargo", "test", "--lib", "--", "python"], cwd=MUT, env=env, stdout=lf, stderr=subprocess.STDOUT).returncode
    restore(f)
    out = open(log).read()
    res = re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed", out)
    failed = re.findall(r"^    ([\w:]+)$", out, re.M)
    comp_err = "error[E" in out or "could not compile" in out
    verdict = "COMPILE-ERROR" if comp_err else ("SURVIVED" if rc == 0 else "KILLED")
    print(f"{mid}: rc={rc} {verdict} {res} failing={failed[:6]}", flush=True)
# final check: mutant copy equals pristine
for f in {m[0] for m in M.values()}:
    assert open(os.path.join(MUT, f)).read() == open(os.path.join(PRISTINE, f)).read(), f
print("restored OK")
