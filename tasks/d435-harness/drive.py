#!/usr/bin/env python3
"""Run every generated input through MCP and CLI surfaces of one binary.

usage: drive.py <binary> <arm> [--corpora flask,leveldb] [--sample N] [--phase mcp|cli|both]
                [--workers W] [--root /var/tmp/d435]
Writes <root>/results/<arm>/<corpus>/<surface>.jsonl, one line per input, sorted by id.
MCP surfaces run on one server per corpus (mcpc.py); the CLI phase runs after it
(never concurrently on one corpus). --sample takes a seeded sample (seed 435).
"""
import argparse, concurrent.futures as cf, json, os, random, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
IMPACT_CLASSES = {"Q", "QF", "D1", "D2", "D3", "DF", "X", "DC", "E", "HD", "HDF", "DCW", "EF", "FH"}
DIRS = ("callers", "callees", "both")
CLASSES = set()
SURFACES = set()
# CLI impact runs only for these classes and for every input of the fx2 corpus.
IMPACT_CLI_CLASSES = {"HD", "HDF", "E", "EF", "FH"}
IMPACT_ALL = False  # --impact-all: CLI impact for every input

def load_inputs(root, corpus, sample):
    recs = [json.loads(l) for l in open(f"{root}/results/inputs-{corpus}.jsonl", encoding="utf-8")]
    if sample:
        recs = random.Random(435).sample(recs, min(sample, len(recs)))
    if CLASSES:
        recs = [r for r in recs if r["class"] in CLASSES]
    return recs

def mcp_calls(recs):
    calls = []
    for r in recs:
        base = {"symbol_name": r["sym"]}
        if "file" in r:
            base["file_path"] = r["file"]
        for d in DIRS:
            calls.append(("gcg_" + d, {"id": r["id"], "tool": "get_call_graph", "args": dict(base, direction=d)}))
        calls.append(("fr", {"id": r["id"], "tool": "find_references", "args": dict(base)}))
        calls.append(("gan", {"id": r["id"], "tool": "get_ast_node", "args": dict(base, compact=True)}))
        if r["class"] in IMPACT_CLASSES:
            calls.append(("gani", {"id": r["id"], "tool": "get_ast_node",
                                   "args": dict(base, compact=True, include_impact=True)}))
    return [c for c in calls if not SURFACES or "mcp_" + c[0] in SURFACES]

def run_mcp(binary, corpus_dir, outdir, recs):
    calls = mcp_calls(recs)
    # Tag each call with its surface in the id so one server run covers all.
    payload = "".join(json.dumps(dict(c, id=f"{s}|{c['id']}"), ensure_ascii=False) + "\n" for s, c in calls)
    p = subprocess.run([sys.executable, "-I", f"{HERE}/mcpc.py", binary, corpus_dir],
                       input=payload, capture_output=True, text=True, encoding="utf-8")
    if p.returncode != 0:
        raise SystemExit(f"mcpc failed rc={p.returncode}: {p.stderr[-2000:]}")
    by_surface = {}
    for line in p.stdout.splitlines():
        rec = json.loads(line)
        s, rid = rec["id"].split("|", 1)
        rec["id"] = rid
        by_surface.setdefault(s, []).append(rec)
    n = 0
    for s, rows in by_surface.items():
        rows.sort(key=lambda x: x["id"])
        with open(f"{outdir}/mcp_{s}.jsonl", "w", encoding="utf-8") as f:
            for rec in rows:
                f.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")
                n += 1
    if n != len(calls):
        raise SystemExit(f"mcp: expected {len(calls)} results, got {n}")
    return n

def cli_jobs(recs):
    jobs = []
    for r in recs:
        fileargs = ["--file", r["file"]] if "file" in r else []
        for d in DIRS:
            jobs.append(("cli_cg_" + d, r["id"], ["callgraph", r["sym"], "--direction", d, "--json"] + fileargs))
        jobs.append(("cli_refs", r["id"], ["refs", r["sym"], "--json"] + fileargs))
        if IMPACT_ALL or r["class"] in IMPACT_CLI_CLASSES or r["id"].startswith("fx2:"):
            jobs.append(("cli_impact", r["id"], ["impact", r["sym"], "--json"] + fileargs))
    return [j for j in jobs if not SURFACES or j[0] in SURFACES]

def run_cli(binary, corpus_dir, outdir, recs, workers):
    jobs = cli_jobs(recs)
    env = dict(os.environ)
    def one(job):
        s, rid, argv = job
        p = subprocess.run([binary] + argv, cwd=corpus_dir, capture_output=True, env=env, timeout=300)
        return s, {"id": rid, "rc": p.returncode,
                   "stdout": p.stdout.decode("utf-8", "replace"),
                   "stderr": p.stderr.decode("utf-8", "replace")}
    by_surface = {}
    with cf.ThreadPoolExecutor(max_workers=workers) as ex:
        for s, rec in ex.map(one, jobs):
            by_surface.setdefault(s, []).append(rec)
    n = 0
    for s, rows in by_surface.items():
        rows.sort(key=lambda x: x["id"])
        with open(f"{outdir}/{s}.jsonl", "w", encoding="utf-8") as f:
            for rec in rows:
                f.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")
                n += 1
    return n

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("binary"); ap.add_argument("arm")
    ap.add_argument("--corpora", default="flask,leveldb")
    ap.add_argument("--sample", type=int, default=0)
    ap.add_argument("--phase", default="both")
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--root", default="/var/tmp/d435")
    ap.add_argument("--classes", default="")
    ap.add_argument("--surfaces", default="")
    ap.add_argument("--impact-all", action="store_true")
    a = ap.parse_args()
    global CLASSES, SURFACES, IMPACT_ALL
    IMPACT_ALL = a.impact_all
    CLASSES = set(filter(None, a.classes.split(",")))
    SURFACES = set(filter(None, a.surfaces.split(",")))
    for corpus in a.corpora.split(","):
        recs = load_inputs(a.root, corpus, a.sample)
        outdir = f"{a.root}/results/{a.arm}/{corpus}"
        os.makedirs(outdir, exist_ok=True)
        cdir = f"{a.root}/corpora/{corpus}"
        if a.phase in ("mcp", "both") and mcp_calls(recs):
            n = run_mcp(a.binary, cdir, outdir, recs)
            print(f"{corpus}: mcp {n} runs", flush=True)
        if a.phase in ("cli", "both") and cli_jobs(recs):
            n = run_cli(a.binary, cdir, outdir, recs, a.workers)
            print(f"{corpus}: cli {n} runs", flush=True)

main()
