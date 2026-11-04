import json, os, subprocess, sys, shutil
W = os.environ["CG_WALK_WORK"]  # scratch dir; put binaries in $CG_WALK_WORK/repro/bin
BINS = {"new": f"{W}/repro/bin/cg-new", "old": f"{W}/repro/bin/cg-old", "184": os.path.expanduser("~/.cache/code-graph/bin/code-graph-mcp")}
APP = "import { f as g } from './index.js';\nexport function main() { return g(); }\nconsole.log(main());\n"

def d(name):
    return f'export function f() {{ return "{name}"; }}\n'

def star(*specs):
    return "".join(f"export * from './{s}.js';\n" for s in specs)

def named(spec, name="f"):
    return f"export {{ {name} }} from './{spec}.js';\n"

def run_node(dirp):
    p = subprocess.run(["node", "app.js"], cwd=dirp, capture_output=True, text=True, timeout=30)
    if p.returncode == 0:
        return p.stdout.strip().splitlines()[-1] if p.stdout.strip() else "?"
    err = [l for l in p.stderr.splitlines() if "Error" in l]
    return "ERR:" + (err[0][:90] if err else p.stderr[:90])

def run_cg(dirp, which, entry="app.js"):
    dd = dirp + "-" + which
    if os.path.exists(dd):
        shutil.rmtree(dd)
    shutil.copytree(dirp, dd)
    env = dict(os.environ, GIT_AUTHOR_NAME="a", GIT_AUTHOR_EMAIL="a@b", GIT_COMMITTER_NAME="a", GIT_COMMITTER_EMAIL="a@b")
    subprocess.run("git init -q && git add -A && git commit -qm i", shell=True, cwd=dd, env=env, check=True)
    subprocess.run([BINS[which], "incremental-index", "--no-embed", "--quiet"], cwd=dd, capture_output=True, timeout=120)
    db = f"{dd}/.code-graph/index.db"
    q = f"""SELECT e.relation||' '||tf.path||':'||t.name FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id
      JOIN nodes t ON t.id=e.target_id LEFT JOIN files tf ON tf.id=t.file_id
      WHERE sf.path='{entry}' AND ((e.relation='calls' AND s.name='main') OR (e.relation='imports' AND s.name='<module>'))"""
    out = subprocess.run(["sqlite3", "-readonly", f"file:{db}?mode=ro", q], capture_output=True, text=True).stdout.split()
    rows = sorted(set(subprocess.run(["sqlite3", "-readonly", f"file:{db}?mode=ro", q], capture_output=True, text=True).stdout.splitlines()))
    calls = sorted(r.split(" ",1)[1].split(":")[0] for r in rows if r.startswith("calls "))
    imps = sorted(r.split(" ",1)[1] for r in rows if r.startswith("imports "))
    return calls, imps

def main():
    spec = sys.argv[1]
    which = sys.argv[2].split(",")
    shapes = {}
    exec(open(spec).read(), {"d": d, "star": star, "named": named, "APP": APP, "shapes": shapes})
    root = f"{W}/repro/out/" + os.path.basename(spec).replace(".py", "")
    os.makedirs(root, exist_ok=True)
    for name, files in shapes.items():
        dirp = f"{root}/{name}"
        if os.path.exists(dirp):
            shutil.rmtree(dirp)
        os.makedirs(dirp)
        files = dict(files)
        files.setdefault("app.js", APP)
        files.setdefault("package.json", '{"type":"module"}\n')
        for fn, body in files.items():
            fp = f"{dirp}/{fn}"
            os.makedirs(os.path.dirname(fp), exist_ok=True)
            open(fp, "w").write(body)
        nodev = run_node(dirp)
        res = [f"{name:<40} node={nodev:<28}"]
        for w in which:
            calls, imps = run_cg(dirp, w)
            res.append(f"{w}: calls={','.join(calls) or '-'} imps={';'.join(i for i in imps if not i.startswith('?'))}")
        print("  ".join(res), flush=True)

main()
