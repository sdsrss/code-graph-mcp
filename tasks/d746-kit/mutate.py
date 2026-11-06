import subprocess, sys, os, re
ROOT = "/home/ai/dev/code-graph-mcp"
R = "src/indexer/pipeline/resolve.rs"; I = "src/indexer/pipeline/index_files.rs"
MUTS = {
 "M2 first import target wins": (R, ".filter(|(t, _)| *t == target)", ".filter(|_| true)"),
 "M5 sweep binds every sibling": (R, "candidates = vec![target];", "candidates = siblings.clone();"),
 "M13 keyed by target name": (R, "\"SELECT COALESCE(json_extract(ie.metadata, '$.imported'), itn.name),", "\"SELECT itn.name,"),
 "M15 walked from the root": (R, "resolve_js_specifier_path(&spec, file, files)", "resolve_js_specifier_path(&spec, \"x.ts\", files)"),
 "D1 member arm drains": (R, "if pool.is_empty() {\n                    continue;\n                }\n                pool", "pool"),
 "D2 fallback drains": (R, "} else if pool.is_empty() {\n                            // Free functions only: buffered too (D#747).\n                            continue;\n                        } else {", "} else {"),
 "R1 restore by name": (I, "let family_call = relation.as_str() == REL_CALLS", "let family_call = false && relation.as_str() == REL_CALLS"),
}
sel = sys.argv[1:] or list(MUTS)
env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
for name in sel:
    path, old, new = MUTS[name]
    p = os.path.join(ROOT, path); src = open(p).read()
    assert src.count(old) == 1, (name, src.count(old))
    open(p, "w").write(src.replace(old, new))
    try:
        r = subprocess.run(["cargo", "test", "--lib", "--", "a_js_family_", "pending", "member_call"], cwd=ROOT, env=env, capture_output=True, text=True)
        out = r.stdout + r.stderr
        res = re.findall(r"test result: .*", out)
        fails = re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M)
        msgs = [l for l in out.splitlines() if l.startswith(("    got", "    want")) or ": got " in l or "the rebuild binds" in l][:6]
        print(f"== {name}: {res} {fails}"); [print("   ", m) for m in msgs]
        if "error[" in out: print("   COMPILE ERROR")
    finally:
        open(p, "w").write(src)
