import subprocess, sys, os
T = "/var/tmp/d520/mut"
env = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d520/mut-target", PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
IF = "src/indexer/pipeline/index_files.rs"; SU = "src/graph/surprising.rs"
FILTER = ["a_new_python_module_on_a_module_path", "a_package_init_that_appears", "a_python_package_change", "the_ambiguous_reason", "surprising"]
M = {
 "M1_no_new_module_gate": (IF, "            new_module = true;\n", "            new_module = false;\n"),
 "M2_names_package_false": (IF, ".is_some_and(|p| packages.contains(p.as_str()))", ".is_some_and(|_p| false)"),
 "M3_bound_arm_no_relative": (IF, "if changed.contains(&module) || names_package(&module, &importer) {", "if changed.contains(&module) {"),
 "M4_sentinel_arm_no_relative": (IF, "&& (changed.contains(&module) || names_package(&module, &importer))", "&& changed.contains(&module)"),
  "M6_no_language_cond": (SU, "AND tf.language IN ('typescript', 'tsx') \\\\", "\\\\"),
 "M7_no_type_cond": (SU, "e.relation = ?1 AND nt.type IN ('interface', 'type') \\\\", "e.relation = ?1 \\\\"),
 "M8_reason_ignores_type_call": (SU, "if c == CONF_AMBIGUOUS && type_call {", "if c == CONF_AMBIGUOUS && type_call && false {"),
}
only = sys.argv[1:]
for name, (f, a, b) in M.items():
    if only and name not in only: continue
    p = os.path.join(T, f); orig = open(p).read()
    a2, b2 = a.encode().decode("unicode_escape") if "\\\\" in a else a, b.encode().decode("unicode_escape") if "\\\\" in b else b
    if orig.count(a2) != 1: print(name, "PATTERN COUNT", orig.count(a2)); continue
    open(p, "w").write(orig.replace(a2, b2))
    try:
        r = subprocess.run(["cargo", "test", "--lib", "--", *FILTER], cwd=T, env=env, capture_output=True, text=True)
        out = r.stdout + r.stderr
        failed = [l.split()[1] for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
        if "error[" in out or "error:" in out and not failed: print(name, "BUILD/OTHER ERROR", [l for l in out.splitlines() if l.startswith("error")][:3])
        else: print(name, "KILLED by" if failed else "SURVIVED", [x.split("::")[-1] for x in failed])
    finally:
        open(p, "w").write(orig)
