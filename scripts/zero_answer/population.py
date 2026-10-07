#!/usr/bin/env python3
"""Zero-answer population for the D#229 measurement.

Every function definition under --prefix (not a test node, not under a tests/
directory) with no incoming `calls` edge at any tier, with how many callers
rust-analyzer finds for it (scripts/scip_oracle/oracle.py --dump-gold):
`ra_any` counts every caller, `ra_prod` only callers that are not test nodes
and not in tests/ or benches/. Writes a JSON list, and the names one per line
for tests/zero_answer_bench.rs.
"""
import argparse
import json
import sqlite3


def test_path(p):
    return any(s in f"/{p}" for s in ("/tests/", "/benches/")) or p.endswith("_test.rs")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--db", required=True, help="the corpus's .code-graph/index.db")
    ap.add_argument("--gold", required=True, help="oracle.py --dump-gold output")
    ap.add_argument("--prefix", default="tokio/src/")
    ap.add_argument("--out", required=True)
    ap.add_argument("--names-out", required=True)
    a = ap.parse_args()
    con = sqlite3.connect(f"file:{a.db}?mode=ro", uri=True)
    nodes = {}
    for nid, name, typ, path, line, is_test, qn in con.execute(
            "SELECT n.id, n.name, n.type, f.path, n.start_line, n.is_test, n.qualified_name "
            "FROM nodes n JOIN files f ON f.id = n.file_id"):
        nodes[(path, line, name)] = (nid, typ, is_test, qn)
    incoming = {t for (t,) in con.execute("SELECT DISTINCT target_id FROM edges WHERE relation = 'calls'")}
    callers = {}
    for g in json.load(open(a.gold)):
        path, line = g["callee_at"].rsplit(":", 1)
        cpath, cline = g["caller_at"].rsplit(":", 1)
        c = nodes.get((cpath, int(cline), g["caller"]))
        prod = c is not None and not c[2] and not test_path(cpath)
        callers.setdefault((path, int(line), g["callee"]), []).append(prod)
    pop = []
    for (path, line, name), (nid, typ, is_test, qn) in nodes.items():
        if not path.startswith(a.prefix) or is_test or "/tests/" in path:
            continue
        if typ not in ("function", "method") or nid in incoming:
            continue
        cs = callers.get((path, line, name), [])
        pop.append({"id": nid, "name": name, "at": f"{path}:{line}", "qn": qn,
                    "ra_any": len(cs), "ra_prod": sum(cs)})
    json.dump(pop, open(a.out, "w"))
    open(a.names_out, "w").write("".join(f"{n}\n" for n in sorted({p["name"] for p in pop})))
    print(f"{len(pop)} zero-answer definitions, {len({p['name'] for p in pop})} names")


if __name__ == "__main__":
    main()
