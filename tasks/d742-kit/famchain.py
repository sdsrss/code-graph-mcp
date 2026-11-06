# famchain.py BIN CORPUS N SEED: on a corpus copy, delete then restore N files that are
# targets of JS-family import calls (D#742); after each step compare the incremental
# index (edges with metadata + pending rows) with a fresh index of the same tree.
import os, random, re, shutil, subprocess, sys, sqlite3
bin_, corpus, n, seed = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
H = "/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness"
W = os.path.join(os.environ["CG_WALK_WORK"], f"famchain-{os.path.basename(corpus)}-{seed}")
F = W + "-full"
for d in (W, F):
    if os.path.exists(d): shutil.rmtree(d)
shutil.copytree(corpus, W, symlinks=True); shutil.rmtree(f"{W}/.code-graph", ignore_errors=True)
idx = lambda d: subprocess.run([bin_, "incremental-index", "--no-embed", "--quiet"], cwd=d, capture_output=True, timeout=1200)
def dump(d):
    e = subprocess.run([f"{H}/edgesm.sh", d], capture_output=True, text=True).stdout
    db = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    p = sorted("PENDING %s:%s => %s %s" % r for r in db.execute("SELECT COALESCE(f.path,'?'), n.name, p.target_name, COALESCE(p.metadata,'') FROM pending_unresolved_calls p JOIN nodes n ON n.id=p.source_id LEFT JOIN files f ON f.id=n.file_id"))
    return e + "\n".join(p)
idx(W)
db = sqlite3.connect(f"file:{W}/.code-graph/index.db?mode=ro", uri=True)
fam = {"javascript", "typescript", "tsx"}
targets = sorted({tf for (tf, tl, sl) in db.execute("""SELECT DISTINCT ft.path, ft.language, fs.language FROM edges e
    JOIN nodes s ON s.id=e.source_id JOIN files fs ON fs.id=s.file_id JOIN nodes t ON t.id=e.target_id JOIN files ft ON ft.id=t.file_id
    WHERE e.relation='calls' AND fs.language<>ft.language""") if tl in fam and sl in fam})
db.close()
rng = random.Random(seed); pick = rng.sample(targets, min(n, len(targets)))
diffs = 0
def commit(d):
    subprocess.run("git add -A && git -c user.email=a@b -c user.name=a commit -qm e", shell=True, cwd=d, capture_output=True)
for f in pick:
    body = open(f"{W}/{f}", "rb").read()
    for step in ("delete", "restore"):
        if step == "delete": os.remove(f"{W}/{f}")
        else: open(f"{W}/{f}", "wb").write(body)
        commit(W); idx(W)
        if os.path.exists(F): shutil.rmtree(F)
        shutil.copytree(W, F, symlinks=True, ignore=shutil.ignore_patterns(".code-graph")); idx(F)
        a, b = dump(W), dump(F)
        if a != b:
            diffs += 1
            sa, sb = set(a.splitlines()), set(b.splitlines())
            print(f"{step} {f}: incremental-only {len(sa-sb)} rebuild-only {len(sb-sa)}")
            for l in sorted(sa - sb)[:3]: print("   inc:", l[:180])
            for l in sorted(sb - sa)[:3]: print("   full:", l[:180])
print(f"{os.path.basename(corpus)}: family targets {len(targets)}, picked {len(pick)}, steps {2*len(pick)}, diff steps {diffs}")
shutil.rmtree(W); shutil.rmtree(F, ignore_errors=True)
