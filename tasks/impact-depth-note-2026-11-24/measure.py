#!/usr/bin/env python3
"""How often 0.205.0's depth note fires, and what a deeper walk then adds.

    measure.py CORPUS BASE_BINARY HEAD_BINARY [--n 150] [--seed 20261124]

Samples production function/method nodes that have at least one incoming
`calls` edge (seeded), runs `impact --node-id N --json` with BASE and HEAD at
the default depth, and HEAD again at --depth 10. Prints how many answers carry
`callers_truncated` on each binary, how many of HEAD's flagged answers gain
production callers / covering tests at depth 10, how many still carry a note
at depth 10 and why (row limit / depth 10 is the deepest walk), and the per-call wall time of
BASE vs HEAD (median of the sample). Reads the index with immutable=1;
`impact` itself may refresh stale files in the corpus copy (use a copy).
"""
import argparse
import json
import os
import random
import sqlite3
import statistics
import subprocess
import time

TEST = ("/tests/", "/test/", "test_", "_test.", "conftest", ".test.", ".spec.", "__tests__")


def run(binary, corpus, args):
    t = time.perf_counter()
    p = subprocess.run([binary, *args], cwd=corpus, capture_output=True, text=True,
                       env={**os.environ, "CODE_GRAPH_NO_AUTO_UPDATE": "1"}, timeout=120)
    dt = time.perf_counter() - t
    try:
        return json.loads(p.stdout.strip().splitlines()[-1]), dt
    except (ValueError, IndexError):
        return None, dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus")
    ap.add_argument("base")
    ap.add_argument("head")
    ap.add_argument("--n", type=int, default=150)
    ap.add_argument("--seed", type=int, default=20261124)
    a = ap.parse_args()
    db = sqlite3.connect(f"file:{os.path.join(a.corpus, '.code-graph', 'index.db')}?immutable=1", uri=True)
    rows = db.execute(
        "SELECT n.id, n.name, f.path FROM nodes n JOIN files f ON f.id = n.file_id "
        "WHERE n.type IN ('function','method') AND n.is_test = 0 AND f.path <> '<external>' "
        "AND EXISTS (SELECT 1 FROM edges e WHERE e.target_id = n.id AND e.relation = 'calls') "
        "ORDER BY n.id").fetchall()
    rows = [r for r in rows if not any(t in r[2] for t in TEST)]
    random.Random(a.seed).shuffle(rows)
    sample = rows[:a.n]
    out = {"pool": len(rows), "n": len(sample), "base_flag": 0, "head_flag": 0, "answered": 0,
           "flag_gains_prod": 0, "flag_gains_tests_only": 0, "flag_gains_nothing": 0,
           "still_noted_at_depth10": 0, "depth10_row_limit": 0, "depth10_deepest_walk": 0,
           "base_s": [], "head_s": [], "examples": []}
    for nid, name, path in sample:
        b, tb = run(a.base, a.corpus, ["impact", "--node-id", str(nid), "--json"])
        h, th = run(a.head, a.corpus, ["impact", "--node-id", str(nid), "--json"])
        if not b or not h:
            continue
        out["answered"] += 1
        out["base_s"].append(tb)
        out["head_s"].append(th)
        out["base_flag"] += bool(b.get("callers_truncated"))
        if h.get("callers_truncated"):
            out["head_flag"] += 1
            d, _ = run(a.head, a.corpus, ["impact", "--node-id", str(nid), "--depth", "10", "--json"])
            if d is None:
                continue
            if d.get("callers_truncated"):
                out["still_noted_at_depth10"] += 1
                note = d.get("callers_truncated_note", "")
                out["depth10_row_limit" if "row traversal limit" in note else "depth10_deepest_walk"] += 1
            dp = d["total_callers"] - h["total_callers"]
            dt = d["tests_affected"] - h["tests_affected"]
            if dp > 0:
                out["flag_gains_prod"] += 1
            elif dt > 0:
                out["flag_gains_tests_only"] += 1
            else:
                out["flag_gains_nothing"] += 1
            if len(out["examples"]) < 8:
                out["examples"].append({"name": name, "path": path, "d3_prod": h["total_callers"],
                                        "d10_prod": d["total_callers"], "d3_tests": h["tests_affected"],
                                        "d10_tests": d["tests_affected"]})
    bs, hs = out.pop("base_s"), out.pop("head_s")
    out["base_median_s"] = round(statistics.median(bs), 3) if bs else None
    out["head_median_s"] = round(statistics.median(hs), 3) if hs else None
    print(json.dumps(out, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
