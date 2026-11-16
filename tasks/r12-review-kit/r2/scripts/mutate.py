"""mutate.py <id>...: apply mutation, build, run lib tests, report failing tests, restore."""
import subprocess, sys, os, shutil, time, re
W='/var/tmp/agent-tmp-r12-review2'
F={'resolve':'src/indexer/pipeline/resolve.rs','index_files':'src/indexer/pipeline/index_files.rs','go':'src/parser/relations/go.rs'}
M={
 'C1_rule_off': ('go', "None if !decl.params.contains(name)\n                && GO_IMPORTS.with(|m| m.borrow().contains_key(name)) =>", "None if false && !decl.params.contains(name)\n                && GO_IMPORTS.with(|m| m.borrow().contains_key(name)) =>"),
 'C2_dirfilter_off': ('resolve', 'if language == "go" {\n        let dir', 'if language == "never" {\n        let dir'),
 'C3_params_ignored': ('go', "None if !decl.params.contains(name)\n", "None if { let _ = &decl.params; true }\n"),
 'C4_params_empty': ('go', "params.insert(node_text(&name, source).to_string());", "if false { params.insert(node_text(&name, source).to_string()); }"),
 'C5_pkg_ignores_dir': ('resolve', ".is_some_and(|p| dir(p) == caller_dir)", ".is_some_and(|p| { let _ = (dir(p), &caller_dir); true })"),
 'C6_deferred_lang': ('index_files', "dispatch,\n                            &d.language,", "dispatch,\n                            \"\","),
 'C7_sweep_lang': ('resolve', "dispatch,\n                    &row.source_language,\n                    &candidates,", "dispatch,\n                    \"\",\n                    &candidates,"),
 'N1_results_as_params': ('go', "        for field in [\"parameters\", \"result\"] {\n            if let Some(p) = decl.child_by_field_name(field) {\n                collect_go_idents(&p, source, &mut declared, 0);", "        if let Some(r) = decl.child_by_field_name(\"result\") { if r.kind() == \"parameter_list\" { let mut c = r.walk(); for p in r.named_children(&mut c) { let mut n = p.walk(); for name in p.children_by_field_name(\"name\", &mut n) { params.insert(node_text(&name, source).to_string()); } } } }\n        for field in [\"parameters\", \"result\"] {\n            if let Some(p) = decl.child_by_field_name(field) {\n                collect_go_idents(&p, source, &mut declared, 0);"),
 'N2_same_file_not_dir': ('resolve', ".is_some_and(|p| dir(p) == caller_dir)", ".is_some_and(|p| { let _ = &caller_dir; p == caller_path })"),
 'N3_top_dir_only': ('resolve', "let dir = |p: &str| p.rsplit_once('/')", "let dir = |p: &str| p.split_once('/')"),
 'N4_nested_unfiltered': ('resolve', "        exact.retain(in_package);\n        nested.retain(in_package);", "        exact.retain(in_package);"),
 'N5_first_name_only': ('go', "for name in p.children_by_field_name(\"name\", &mut names) {", "for name in p.children_by_field_name(\"name\", &mut names).take(1) {"),
 'N6_no_variadic': ('go', "\"parameter_declaration\" | \"variadic_parameter_declaration\"\n", "\"parameter_declaration\"\n"),
 'N7_any_local_unmarked': ('go', "None if !decl.params.contains(name)\n                && GO_IMPORTS.with(|m| m.borrow().contains_key(name)) =>", "None if !decl.params.contains(name) =>"),
 'N8_params_include_literal': ('go', "            collect_go_body_names(&body, source, &mut declared, &mut assigned, 0);", "            collect_go_body_names(&body, source, &mut declared, &mut assigned, 0);\n            params.extend(declared.iter().cloned());"),
 'N9_filter_before_own_only_if_multi': ('resolve', "        exact.retain(in_package);\n        nested.retain(in_package);", "        if exact.len() > 1 { exact.retain(in_package); }\n        nested.retain(in_package);"),
}
def run(mid):
    key, old, new = M[mid]
    path = os.path.join(W, 'src', F[key]); pristine = os.path.join(W, 'pristine', os.path.basename(path))
    s = open(pristine).read()
    n = s.count(old)
    if n != 1: print(mid, 'PATTERN COUNT', n); return
    open(path, 'w').write(s.replace(old, new))
    try:
        env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(W, 'target'))
        r = subprocess.run(['cargo', 'test', '--locked', '--lib'], cwd=os.path.join(W, 'src'), env=env, capture_output=True, text=True)
        out = r.stdout + r.stderr
        res = [l for l in out.splitlines() if l.startswith('test result')]
        fails = sorted(set(re.findall(r'^    (\S+)$', out, re.M)))
        if 'error[' in out or not res:
            print(mid, 'BUILD/RUN ERROR', [l for l in out.splitlines() if 'error' in l][:5]); return
        print(mid, res[-1], 'KILLED' if fails else 'SURVIVED', fails, flush=True)
    finally:
        shutil.copyfile(pristine, path); t = time.time(); os.utime(path, (t, t))
for mid in sys.argv[1:]: run(mid)
