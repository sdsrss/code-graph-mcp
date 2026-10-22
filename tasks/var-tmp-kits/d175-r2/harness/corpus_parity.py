#!/usr/bin/env python3
"""Corpus-level incremental-vs-fresh parity.
usage: corpus_parity.py ARM SRC NAME EDITSPEC...
EDITSPEC: write:REL:TEXTFILE | append:REL:TEXT | delete:REL ; several edits form one step.
Steps separated by '--'."""
import os, sys, subprocess, shutil, time, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run as R

def cp(src, dst):
    subprocess.run(["/var/tmp/d175-r2/harness/cpcorp.sh", src, dst], check=True)

def apply(d, spec):
    kind, rest = spec.split(":", 1)
    if kind == "delete":
        os.remove(os.path.join(d, rest))
    elif kind == "append":
        rel, text = rest.split(":", 1)
        with open(os.path.join(d, rel), "a") as f:
            f.write(text.replace("\\n", "\n"))
        t = time.time() + 5; os.utime(os.path.join(d, rel), (t, t))
    elif kind == "write":
        rel, text = rest.split(":", 1)
        p = os.path.join(d, rel); os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(text.replace("\\n", "\n"))
    elif kind == "sub":
        rel, text = rest.split(":", 1)
        old, new = text.split("=>", 1)
        p = os.path.join(d, rel); body = open(p).read()
        assert body.count(old) >= 1, (rel, old)
        open(p, "w").write(body.replace(old, new))
        t = time.time() + 5; os.utime(p, (t, t))
    else:
        raise SystemExit("bad spec " + spec)

def timed_index(arm, d):
    t0 = time.time()
    p = subprocess.run([f"{R.BIN}/{arm}", "incremental-index", "--no-embed", "--json"], cwd=d, env=R.ENV,
                       capture_output=True, text=True)
    dt = time.time() - t0
    if p.returncode:
        raise SystemExit(p.stderr[-2000:])
    try:
        js = json.loads(p.stdout.strip().splitlines()[-1])
    except Exception:
        js = {}
    return dt, js

def main():
    arm, src, name = sys.argv[1:4]
    steps = [[]]
    for a in sys.argv[4:]:
        if a == "--":
            steps.append([])
        else:
            steps[-1].append(a)
    base = f"/var/tmp/d175-r2/corp/par-{name}-{arm}"
    inc, fr = base + "-inc", base + "-fresh"
    cp(src, inc)
    timed_index(arm, inc)
    for st in steps:
        for e in st:
            apply(inc, e)
        dt, js = timed_index(arm, inc)
        print(f"[{arm}] {name} step: {dt*1000:.0f} ms wall, json={js}")
    cp(src, fr)
    for st in steps:
        for e in st:
            apply(fr, e)
    timed_index(arm, fr)
    a, b = R.dump(inc), R.dump(fr)
    extra, missing = a - b, b - a
    print(f"[{arm}] {name}: incremental has {sum(extra.values())} edges fresh lacks, lacks {sum(missing.values())} fresh has")
    for r, n in sorted(extra.items())[:8]:
        print(f"      + {n}x {R.fmt(r)}")
    for r, n in sorted(missing.items())[:8]:
        print(f"      - {n}x {R.fmt(r)}")
    if "--keep" not in os.environ.get("CP_KEEP", ""):
        shutil.rmtree(inc); shutil.rmtree(fr)

if __name__ == "__main__":
    main()
