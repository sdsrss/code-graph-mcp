#!/usr/bin/env python3
"""Score code-graph's Rust `calls` edges against a SCIP index (rust-analyzer).

The oracle's gold set is every *call-shaped* reference in the SCIP index whose
callee is a function defined in this repo, attributed to the innermost function
definition that encloses it. Both ends are then aligned to index.db nodes by
(file, name, definition line inside the node's line span). Our edges are judged
only when both endpoints align, so every loss is counted in `funnel` instead of
silently shrinking the denominators.

    precision(tier)  = judged edges of that tier that are in gold / judged edges of that tier
    recall(floor)    = gold pairs we have at that tier or better / gold pairs

Usage: see README.md (run.sh builds the SCIP index and calls this).
"""

import argparse
import json
import os
import re
import sqlite3
import sys
from collections import Counter, defaultdict

import scip_decode

TIERS = ("extracted", "inferred", "ambiguous")
TIER_RANK = {t: i for i, t in enumerate(TIERS)}
CALLABLE_NODE_TYPES = ("function", "method")
UTF8_OFFSETS = 1  # scip.proto PositionEncoding.UTF8CodeUnitOffsetFromLineStart

_CALLABLE_DESCRIPTOR = re.compile(r"\([^()]*\)\.$")


def is_callable_symbol(symbol):
    """A SCIP method descriptor ends in `(<disambiguator>).`; locals never do."""
    return not symbol.startswith("local ") and bool(_CALLABLE_DESCRIPTOR.search(symbol))


def is_call_site(line, end):
    """True when the name ending at byte `end` is followed by `(`, or `::<...>(`."""
    rest = line[end:].lstrip()
    if rest.startswith(b"::<"):
        depth, i = 0, 2
        while i < len(rest):
            c = rest[i:i + 1]
            if c == b"<":
                depth += 1
            elif c == b">":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        else:
            return False
        rest = rest[i + 1:].lstrip()
    return rest.startswith(b"(")


def _shape(line, start):
    before = line[:start].rstrip()
    if before.endswith(b"."):
        return "method"
    if before.endswith(b"::"):
        return "path"
    return "bare"


def _contains(rng, line, col):
    return (rng[0], rng[1]) <= (line, col) <= (rng[2], rng[3])


def _read_lines(root, rel):
    with open(os.path.join(root, rel), "rb") as f:
        return f.read().split(b"\n")


def evaluate(docs, db_path, root, samples=15):
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    rust_files = {p for (p,) in con.execute("SELECT path FROM files WHERE language = 'rust'")}
    scip_paths = {d.relative_path for d in docs}
    docs = [d for d in docs if d.relative_path in rust_files]

    node_meta = {}
    nodes_by_file = defaultdict(list)
    q = (
        "SELECT n.id, n.name, n.start_line, n.end_line, f.path, n.type FROM nodes n "
        "JOIN files f ON f.id = n.file_id WHERE f.language = 'rust'"
    )
    for nid, name, start, end, path, ntype in con.execute(q):
        node_meta[nid] = (name, path, start)
        if ntype in CALLABLE_NODE_TYPES:
            nodes_by_file[path].append((nid, name, start, end))

    funnel = Counter()
    funnel["rust_files_in_index_not_in_scip"] = len(rust_files - scip_paths)
    lines = {}
    for d in docs:
        if d.position_encoding not in (0, UTF8_OFFSETS):
            raise SystemExit(f"{d.relative_path}: unsupported SCIP position encoding {d.position_encoding}")
        lines[d.relative_path] = _read_lines(root, d.relative_path)

    def text(path, rng):
        return lines[path][rng[0]][rng[1]:rng[3]].decode("utf-8", "replace")

    # Pass 1: callable definitions -> our nodes.
    defs = defaultdict(list)
    for d in docs:
        for o in d.occurrences:
            if o.is_definition and is_callable_symbol(o.symbol):
                defs[o.symbol].append((d.relative_path, o.range, o.enclosing_range))

    def key_of(sym, path):
        """Definition key a reference in `path` resolves to, or None if ambiguous.

        rust-analyzer names every tests/*.rs crate with the same package prefix,
        so a helper defined in two test files is ONE symbol. A crate can only call
        its own copy, so a reference binds to the definition in its own file."""
        sites = defs[sym]
        if len(sites) == 1:
            return sym
        here = sum(1 for p, _r, _e in sites if p == path)
        return (path, sym) if here == 1 else None

    sym_to_node = {}
    unmapped = []
    enclosers = defaultdict(list)  # path -> [(enclosing_range, key)]
    for sym, sites in defs.items():
        if len(sites) > 1:
            funnel["colliding_definitions"] += 1
        for path, rng, enc in sites:
            key = key_of(sym, path)
            if key is None:
                funnel["ambiguous_definitions_in_one_file"] += 1
                continue
            if enc:
                enclosers[path].append((enc, key))
            name, line1 = text(path, rng), rng[0] + 1
            cands = [n for n in nodes_by_file[path] if n[1] == name and n[2] <= line1 <= n[3]]
            if cands:
                sym_to_node[key] = min(cands, key=lambda n: n[3] - n[2])[0]
            else:
                funnel["unmapped_definitions"] += 1
                unmapped.append(f"{path}:{line1} {name}")

    # Pass 2: call-shaped references -> gold (caller node, callee node) pairs.
    gold = {}
    called_names = defaultdict(set)  # caller node -> every name it calls, in-repo or not
    for d in docs:
        path = d.relative_path
        for o in d.occurrences:
            if o.is_definition or not is_callable_symbol(o.symbol):
                continue
            sl, sc, el, ec = o.range
            if sl != el or not is_call_site(lines[path][sl], ec):
                funnel["non_call_references"] += 1
                continue
            funnel["call_sites"] += 1
            inner = [(enc, s) for enc, s in enclosers[path] if _contains(enc, sl, sc)]
            caller_sym = max(inner, key=lambda e: (e[0][0], e[0][1]))[1] if inner else None
            caller = sym_to_node.get(caller_sym)
            if caller is not None:
                called_names[caller].add(text(path, o.range))
            callee = sym_to_node.get(key_of(o.symbol, path)) if o.symbol in defs else None
            if o.symbol not in defs:
                funnel["call_sites_to_external"] += 1
            elif callee is None:
                funnel["call_sites_to_unmapped_callee"] += 1
            elif caller_sym is None:
                funnel["call_sites_without_enclosing_fn"] += 1
            elif caller is None:
                funnel["call_sites_in_unmapped_caller"] += 1
            elif caller == callee:
                funnel["self_calls_excluded"] += 1  # index_files.rs drops self-edges by design
            else:
                pair = (caller, callee)
                gold.setdefault(pair, (f"{path}:{sl + 1}", _shape(lines[path][sl], sc)))

    # Our edges, one per (source, target) pair at its highest tier.
    ours = {}
    for s, t, conf in con.execute("SELECT source_id, target_id, confidence FROM edges WHERE relation = 'calls'"):
        if s not in node_meta:
            continue  # caller outside the Rust scope
        if t not in node_meta:
            funnel["edges_to_outside_rust_scope"] += 1
            continue
        if (s, t) in ours:
            funnel["duplicate_pair_edges"] += 1
            if TIER_RANK[conf] >= TIER_RANK[ours[(s, t)]]:
                continue
        ours[(s, t)] = conf
    con.close()

    mapped = set(sym_to_node.values())
    callable_nodes = {n[0] for ns in nodes_by_file.values() for n in ns}
    tiers = {t: {"judged": 0, "correct": 0} for t in TIERS}
    wrong = []
    unjudged = []
    for (s, t), conf in ours.items():
        if s not in mapped or t not in mapped:
            if s in callable_nodes and t in callable_nodes:
                funnel["unjudged_edges_unmapped_function"] += 1
                unjudged.append((conf, s, t))
            else:
                funnel["unjudged_edges_non_function_endpoint"] += 1
            continue
        tiers[conf]["judged"] += 1
        if (s, t) in gold:
            tiers[conf]["correct"] += 1
        else:
            kind = "wrong_target" if node_meta[t][0] in called_names[s] else "no_call_by_that_name"
            funnel[f"wrong_{conf}_{kind}"] += 1
            wrong.append((conf, s, t, kind))

    recall = {}
    for floor in TIERS:
        found = sum(1 for p in gold if p in ours and TIER_RANK[ours[p]] <= TIER_RANK[floor])
        recall[floor] = {"found": found, "gold": len(gold)}
    by_shape = defaultdict(lambda: {"found": 0, "gold": 0})
    for p, (_site, shape) in gold.items():
        by_shape[shape]["gold"] += 1
        by_shape[shape]["found"] += p in ours

    def loc(n):
        name, path, line = node_meta[n]
        return name, f"{path}:{line}"

    missed = sorted((site, shape, c, e) for (c, e), (site, shape) in gold.items() if (c, e) not in ours)
    return {
        "gold_pairs": len(gold),
        "tiers": tiers,
        "recall_at_floor": recall,
        "recall_by_call_shape": dict(sorted(by_shape.items())),
        "funnel": dict(sorted(funnel.items())),
        "samples": {
            "missed": [
                {"caller": loc(c)[0], "callee": loc(e)[0], "site": site, "shape": shape}
                for site, shape, c, e in missed[:samples]
            ],
            "wrong": [
                {"tier": conf, "caller": loc(s)[0], "caller_at": loc(s)[1],
                 "callee": loc(t)[0], "callee_at": loc(t)[1], "kind": kind}
                for conf, s, t, kind in sorted(wrong, key=lambda w: (TIER_RANK[w[0]], loc(w[1])[1], loc(w[2])[1]))[:samples]
            ],
            "unmapped_definitions": sorted(unmapped)[:samples],
            "unjudged_function_edges": [
                {"tier": conf, "caller": loc(s)[0], "caller_at": loc(s)[1],
                 "callee": loc(t)[0], "callee_at": loc(t)[1]}
                for conf, s, t in sorted(unjudged, key=lambda u: (loc(u[1])[1], loc(u[2])[1]))[:samples]
            ],
        },
    }


def _pct(num, den):
    return f"{num / den:.1%}" if den else "n/a"


def render(r):
    out = [f"gold call pairs: {r['gold_pairs']}", "", "precision by tier (judged edges only):"]
    for t, v in r["tiers"].items():
        out.append(f"  {t:<10} {v['correct']:>5}/{v['judged']:<5} {_pct(v['correct'], v['judged'])}")
    out.append("recall at --min-confidence floor:")
    for t, v in r["recall_at_floor"].items():
        out.append(f"  {t:<10} {v['found']:>5}/{v['gold']:<5} {_pct(v['found'], v['gold'])}")
    out.append("recall by call shape (any tier):")
    for s, v in r["recall_by_call_shape"].items():
        out.append(f"  {s:<10} {v['found']:>5}/{v['gold']:<5} {_pct(v['found'], v['gold'])}")
    out.append("funnel (every loss, so no denominator shrinks silently):")
    out += [f"  {k}: {v}" for k, v in r["funnel"].items()]
    for kind, rows in r["samples"].items():
        out.append(f"samples: {kind}")
        out += [f"  {row if isinstance(row, str) else json.dumps(row)}" for row in rows]
    return "\n".join(out)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--scip", required=True, help="index.scip from `rust-analyzer scip`")
    ap.add_argument("--root", default=".", help="repo root the SCIP paths are relative to")
    ap.add_argument("--db", help="code-graph index (default: <root>/.code-graph/index.db)")
    ap.add_argument("--samples", type=int, default=15)
    ap.add_argument("--json-out", help="also write the full report as JSON here")
    a = ap.parse_args(argv)
    db = a.db or os.path.join(a.root, ".code-graph", "index.db")
    r = evaluate(scip_decode.read_index(a.scip), db, a.root, a.samples)
    print(render(r))
    if a.json_out:
        with open(a.json_out, "w") as f:
            json.dump(r, f, indent=2, sort_keys=True)
            f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
