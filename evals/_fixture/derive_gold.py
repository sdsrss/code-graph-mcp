#!/usr/bin/env python3
"""Compiler-grade call edges for the eval fixture, from rust-analyzer's SCIP index.

The answers of the set-valued cases (transitive closures, never-called sets) come
from here, not from code-graph, so a case cannot reward the plugin for sharing
its own blind spots. Gold-pair rules are the SCIP oracle's (scripts/scip_oracle):
call-shaped references only, caller = innermost enclosing function, both ends
in-repo definitions, self-calls dropped.

    rust-analyzer scip . --output /tmp/index.scip
    python3 evals/_fixture/derive_gold.py --scip /tmp/index.scip --out /tmp/gold.json

Run on a tree whose src/ matches the fixture commit (evals/_fixture/scaffold.sh).
--db is any code-graph index of that tree; it supplies node spans and is_test.
Only edges whose caller AND callee are under src/ are kept: the fixture holds
src/ alone.
"""
import argparse
import json
import sqlite3
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts" / "scip_oracle"))
from oracle import CALLABLE_NODE_TYPES, _contains, _read_lines, is_call_site, is_callable_symbol  # noqa: E402
from scip_decode import read_index  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--scip", required=True)
    ap.add_argument("--root", default=".")
    ap.add_argument("--db", help="default: <root>/.code-graph/index.db")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    root = Path(a.root)
    db = a.db or str(root / ".code-graph" / "index.db")

    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    nodes_by_file = defaultdict(list)
    meta = {}
    q = ("SELECT n.id, n.name, n.start_line, n.end_line, f.path, n.type, n.is_test FROM nodes n "
         "JOIN files f ON f.id = n.file_id WHERE f.language = 'rust' AND f.path LIKE 'src/%'")
    for nid, name, start, end, path, ntype, is_test in con.execute(q):
        if ntype in CALLABLE_NODE_TYPES:
            nodes_by_file[path].append((nid, name, start, end))
            meta[nid] = {"name": name, "file": path, "line": start, "is_test": bool(is_test)}
    con.close()

    docs = [d for d in read_index(a.scip) if d.relative_path in nodes_by_file]
    lines = {d.relative_path: _read_lines(root, d.relative_path) for d in docs}

    def text(path, rng):
        return lines[path][rng[0]][rng[1]:rng[3]].decode("utf-8", "replace")

    defs = defaultdict(list)
    for d in docs:
        for o in d.occurrences:
            if o.is_definition and is_callable_symbol(o.symbol):
                defs[o.symbol].append((d.relative_path, o.range, o.enclosing_range))

    sym_to_node, enclosers = {}, defaultdict(list)
    for sym, sites in defs.items():
        if len(sites) != 1:
            continue  # src/ has no test-crate collisions; skip rather than guess
        path, rng, enc = sites[0]
        if enc:
            enclosers[path].append((enc, sym))
        name, line1 = text(path, rng), rng[0] + 1
        cands = [n for n in nodes_by_file[path] if n[1] == name and n[2] <= line1 <= n[3]]
        if cands:
            sym_to_node[sym] = min(cands, key=lambda n: n[3] - n[2])[0]

    edges = {}
    for d in docs:
        path = d.relative_path
        for o in d.occurrences:
            if o.is_definition or not is_callable_symbol(o.symbol):
                continue
            sl, sc, el, ec = o.range
            if sl != el or not is_call_site(lines[path][sl], ec):
                continue
            inner = [(enc, s) for enc, s in enclosers[path] if _contains(enc, sl, sc)]
            if not inner:
                continue
            caller = sym_to_node.get(max(inner, key=lambda e: (e[0][0], e[0][1]))[1])
            callee = sym_to_node.get(o.symbol)
            if caller is None or callee is None or caller == callee:
                continue
            edges.setdefault((caller, callee), f"{path}:{sl + 1}")

    out = {
        "nodes": {str(k): v for k, v in meta.items()},
        "edges": [{"caller": c, "callee": t, "site": s} for (c, t), s in sorted(edges.items())],
    }
    Path(a.out).write_text(json.dumps(out, indent=1))
    print(f"{len(edges)} gold call edges among {len(meta)} callable src/ nodes -> {a.out}")


if __name__ == "__main__":
    main()
