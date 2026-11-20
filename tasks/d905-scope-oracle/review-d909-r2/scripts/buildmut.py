import importlib.util, os, shutil, subprocess, sys
SRC = "/var/tmp/d909-review2/mut-src"
ENV = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d909-review2/target-mut")
spec = importlib.util.spec_from_file_location("m", sys.argv[1]); mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
for name, (f, old, new) in mod.MUTANTS.items():
    tag = name.split()[0]
    if tag not in sys.argv[2:]:
        continue
    path = os.path.join(SRC, f); orig = open(path).read(); assert orig.count(old) == 1
    open(path, "w").write(orig.replace(old, new))
    try:
        r = subprocess.run(["cargo", "build", "--locked", "--bin", "code-graph-mcp"], cwd=SRC, env=ENV, capture_output=True, text=True)
    finally:
        open(path, "w").write(orig); os.utime(path, None)
    print(tag, "build exit", r.returncode, r.stderr[-300:] if r.returncode else "")
    if r.returncode == 0:
        shutil.copy("/var/tmp/d909-review2/target-mut/debug/code-graph-mcp", f"/var/tmp/d909-review2/bin/{tag}")
