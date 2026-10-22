#!/usr/bin/env python3
"""Perf measurements for the 0.175.0 release-note claims.
usage:
  perf.py full SRC NAME PAIRS            alternating full-index CPU time, head vs base174
  perf.py delete SRC NAME REL TRIALS      delete REL after a full index, time the incremental run
  perf.py edit SRC NAME REL TRIALS        append a comment line to REL repeatedly, time each run
  perf.py add SRC NAME REL TRIALS         add an empty REL after a full index, time the run
"""
import os, sys, subprocess, time, json, resource, shutil, statistics, re

BIN = "/var/tmp/d175-r2/bin"
CORP = "/var/tmp/d175-r2/corp"
ENV = dict(os.environ, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
if os.environ.get("PERF_LOG"):
    ENV["RUST_LOG"] = "info"
else:
    ENV.pop("RUST_LOG", None)
ARMS = os.environ.get("PERF_ARMS", "head,base174").split(",")

def cp(src, dst):
    subprocess.run(["/var/tmp/d175-r2/harness/cpcorp.sh", src, dst], check=True)

def index(arm, d):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    p = subprocess.run([f"{BIN}/{arm}", "incremental-index", "--no-embed", "--json"], cwd=d, env=ENV,
                       capture_output=True, text=True)
    wall = time.perf_counter() - t0
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    if p.returncode:
        raise SystemExit(f"{arm} failed in {d}: {p.stderr[-1500:]}")
    js = {}
    for line in reversed(p.stdout.strip().splitlines()):
        try:
            js = json.loads(line); break
        except Exception:
            pass
    pulls = re.findall(r"(Phase 0-pre: re-extracting \d+ file\(s\)|fan-out round: re-extracting \d+ (?:caller file\(s\)|importer\(s\))[^\n]*)", p.stderr)
    return wall, cpu, js, pulls

def med(xs):
    return statistics.median(xs)

def full(src, name, pairs):
    res = {a: [] for a in ARMS}
    for i in range(pairs):
        order = ARMS[i % len(ARMS):] + ARMS[:i % len(ARMS)]
        if i % 2:
            order = order[::-1]
        for a in order:
            d = f"{CORP}/perf-{name}-{a}"
            cp(src, d)
            try:
                wall, cpu, js, _ = index(a, d)
            except SystemExit as e:
                print(f"round {i+1} {a}: INDEX FAILED: {str(e)[-200:]}", flush=True)
                shutil.copytree(d + "/.code-graph", f"/var/tmp/d175-r2/ki/fail-{name}-{a}-{i+1}")
                shutil.rmtree(d)
                continue
            res[a].append(cpu)
            print(f"pair {i+1} {a}: cpu {cpu:.2f}s wall {wall:.2f}s edges {js.get('edges_created')}", flush=True)
            shutil.rmtree(d)
    for a in ARMS:
        if a == "base174":
            continue
        more = sum(1 for h, b in zip(res[a], res["base174"]) if h > b)
        pairs = min(len(res[a]), len(res["base174"]))
        print(f"{name} full CPU median: {a} {med(res[a]):.2f}s base174 {med(res['base174']):.2f}s "
              f"({(med(res[a])/med(res['base174'])-1)*100:+.1f}%), {a} more in {more}/{pairs} rounds")
    for a in ARMS:
        print(f" {a}:", [round(x, 2) for x in res[a]])

def delete(src, name, rel, trials):
    res = {a: [] for a in ARMS}
    for i in range(trials):
        order = ARMS if i % 2 == 0 else ARMS[::-1]
        for a in order:
            d = f"{CORP}/perf-{name}-{a}"
            if i == 0:
                cp(src, d)
                index(a, d)
            body = open(os.path.join(d, rel), "rb").read() if i == 0 else body_saved[a]
            body_saved[a] = body
            os.remove(os.path.join(d, rel))
            wall, cpu, js, pulls = index(a, d)
            res[a].append((wall, cpu, js.get("elapsed_ms"), js.get("files_indexed")))
            print(f"trial {i+1} {a}: wall {wall*1000:.0f} ms elapsed_ms {js.get('elapsed_ms')} cpu {cpu:.2f}s "
                  f"files_indexed {js.get('files_indexed')} deleted {js.get('files_deleted')} pulls {pulls}", flush=True)
            # restore the file and re-index for the next trial
            with open(os.path.join(d, rel), "wb") as f:
                f.write(body)
            index(a, d)
    for a in ARMS:
        print(f"{name} delete {rel} {a}: median wall {med([x[0] for x in res[a]])*1000:.0f} ms, "
              f"median elapsed_ms {med([x[2] for x in res[a]])}, median cpu {med([x[1] for x in res[a]]):.2f}s")
    for a in ARMS:
        shutil.rmtree(f"{CORP}/perf-{name}-{a}")

body_saved = {}

def edit(src, name, rel, trials):
    res = {a: [] for a in ARMS}
    for a in ARMS:
        d = f"{CORP}/perf-{name}-{a}"
        cp(src, d)
        index(a, d)
    for i in range(trials):
        order = ARMS if i % 2 == 0 else ARMS[::-1]
        for a in order:
            d = f"{CORP}/perf-{name}-{a}"
            with open(os.path.join(d, rel), "a") as f:
                f.write(f"\n# edit {i}\n")
            wall, cpu, js, pulls = index(a, d)
            res[a].append((wall, cpu, js.get("elapsed_ms")))
            print(f"edit {i+1} {a}: wall {wall*1000:.0f} ms elapsed_ms {js.get('elapsed_ms')} files {js.get('files_indexed')} {pulls}", flush=True)
    for a in ARMS:
        w = [x[0] * 1000 for x in res[a]]
        print(f"{name} edit {rel} {a}: median wall {med(w):.0f} ms (min {min(w):.0f}, max {max(w):.0f}), "
              f"median elapsed_ms {med([x[2] for x in res[a]])}")
    for a in ARMS:
        shutil.rmtree(f"{CORP}/perf-{name}-{a}")

def add(src, name, rel, trials):
    res = {a: [] for a in ARMS}
    for i in range(trials):
        order = ARMS if i % 2 == 0 else ARMS[::-1]
        for a in order:
            d = f"{CORP}/perf-{name}-{a}"
            if i == 0:
                cp(src, d)
                index(a, d)
            open(os.path.join(d, rel), "w").close()
            wall, cpu, js, pulls = index(a, d)
            res[a].append((wall, cpu, js.get("elapsed_ms")))
            print(f"trial {i+1} {a}: wall {wall*1000:.0f} ms elapsed_ms {js.get('elapsed_ms')} files {js.get('files_indexed')} {pulls}", flush=True)
            os.remove(os.path.join(d, rel))
            index(a, d)
    for a in ARMS:
        w = [x[0] * 1000 for x in res[a]]
        print(f"{name} add {rel} {a}: wall min {min(w):.0f} max {max(w):.0f} median {med(w):.0f} ms; "
              f"elapsed_ms {sorted(x[2] for x in res[a])}")
    for a in ARMS:
        shutil.rmtree(f"{CORP}/perf-{name}-{a}")

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "full":
        full(sys.argv[2], sys.argv[3], int(sys.argv[4]))
    elif cmd == "delete":
        delete(sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5]))
    elif cmd == "edit":
        edit(sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5]))
    elif cmd == "add":
        add(sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5]))
