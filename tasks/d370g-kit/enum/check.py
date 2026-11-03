#!/usr/bin/env python3
"""check.py TRUTH_JSON BASE_DIR HEAD_DIR: judge HEAD's edges from `t_call`.

BASE_DIR and HEAD_DIR are the same generated files indexed by the BASE and
HEAD binaries. For each file:
- HEAD emits an `"own"` edge: it must be exactly one edge, to the function
  CPython ran first in `t_call()` (its TRACE tag). Anything else is WRONG
  (own on a module CPython refuses to import counts as REFUSED).
- HEAD emits no `"own"` edge: its `t_call` edges and pending rows must equal
  BASE's byte for byte, else DRIFT.
Prints counts per verdict and the first examples of each failing kind.
"""
import collections
import json
import os
import sqlite3
import sys

Q = """SELECT coalesce(t.qualified_name, t.name), t.start_line, t.end_line,
              coalesce(e.metadata, ''), e.confidence
       FROM edges e JOIN nodes s ON s.id = e.source_id JOIN files f ON f.id = s.file_id
       JOIN nodes t ON t.id = e.target_id JOIN files tf ON tf.id = t.file_id
       WHERE e.relation = 'calls' AND s.name = 't_call' AND f.path = ? AND tf.path = ?"""
QP = """SELECT p.target_name, coalesce(p.metadata, '') FROM pending_unresolved_calls p
        JOIN nodes s ON s.id = p.source_id JOIN files f ON f.id = s.file_id
        WHERE s.name = 't_call' AND f.path = ?"""


def edges(db, name):
    return sorted(db.execute(Q, (name, name)).fetchall()), sorted(db.execute(QP, (name,)).fetchall())


def tag_of(lines, start, end):
    for line in lines[start - 1:end]:
        i = line.find('TRACE.append("')
        if i >= 0:
            return line[i + 14:line.index('"', i + 14)]
    return None


def main():
    truth = json.load(open(sys.argv[1]))
    base = sqlite3.connect(f"file:{sys.argv[2]}/.code-graph/index.db?mode=ro", uri=True)
    head = sqlite3.connect(f"file:{sys.argv[3]}/.code-graph/index.db?mode=ro", uri=True)
    verdicts = collections.Counter()
    examples = collections.defaultdict(list)
    for name, t in sorted(truth.items()):
        lines = open(os.path.join(sys.argv[3], name)).read().split("\n")
        he, hp = edges(head, name)
        own = [e for e in he if '"own"' in e[3]]
        if own:
            ran = t["ran"][0] if t["ran"] else None
            tags = [tag_of(lines, e[1], e[2]) for e in own]
            if t["import_error"]:
                v = "REFUSED"
            elif len(own) == 1 and ran is not None and tags[0] == ran:
                v = "own-right"
            else:
                v = "WRONG"
            detail = f"ran={t['ran'][:2]} exc={t['exc']} import={t['import_error']} own={tags}"
        else:
            v = "declined-same" if (he, hp) == edges(base, name) else "DRIFT"
            detail = f"base={edges(base, name)} head={(he, hp)}"
        verdicts[v] += 1
        if v in ("WRONG", "REFUSED", "DRIFT") and len(examples[v]) < 8:
            examples[v].append(f"{name}: {detail}")
    print(dict(sorted(verdicts.items())))
    for v, ex in examples.items():
        print(f"--- {v}")
        for e in ex:
            print("  ", e[:400])


if __name__ == "__main__" and "--attribute" not in sys.argv:
    main()


FEATURES = {
    "patch_assign": r"^K\d\.m = ", "patch_paren": r"^\(K\d\)\.m = ", "patch_del": r"del K\d\.m",
    "patch_starred": r"^\*K\d\.m, =", "patch_setattr": r"^setattr\(", "patch_alias": r"^_V\.m = ",
    "slots": r"__slots__", "locals": r"locals\(\)", "lambda_walrus": r"lambda a=\(m :=",
    "walrus_default": r"def z\(self, a=\(m :=", "deco_class": r"^@keep", "if_class": r"^if True:\n    class",
    "try_class": r"^try:\n    class", "subscript": r"G\d+\[int\]", "call_base": r"mk\(K\d\)|mk\(E1\)",
    "meta_MP": r"metaclass=MP", "meta_MQ": r"metaclass=MQ", "meta_MG": r"metaclass=MG",
    "meta_abc": r"metaclass=ABCMeta", "meta_kw": r"\*\*\{'metaclass'", "nested": r"def other\(self\)",
    "two_defs": None, "attr": r"m = staticmethod\(_free\)\n", "cond_def": r"    if True:\n        def m",
    "static": r"@staticmethod", "object_base": r"\bobject\b", "dup": None,
}


def features(text):
    import re
    out = set()
    for k, pat in FEATURES.items():
        if pat and re.search(pat, text, re.M):
            out.add(k)
    return out


def attribute(truth_path, base_dir, head_dir):
    """Per feature: how many WRONG / REFUSED / own-right files carry it."""
    truth = json.load(open(truth_path))
    head = sqlite3.connect(f"file:{head_dir}/.code-graph/index.db?mode=ro", uri=True)
    table = collections.defaultdict(collections.Counter)
    for name, t in sorted(truth.items()):
        text = open(os.path.join(head_dir, name)).read()
        lines = text.split("\n")
        he, _ = edges(head, name)
        own = [e for e in he if '"own"' in e[3]]
        if not own:
            continue
        ran = t["ran"][0] if t["ran"] else None
        tags = [tag_of(lines, e[1], e[2]) for e in own]
        v = "REFUSED" if t["import_error"] else ("own-right" if len(own) == 1 and tags[0] == ran else "WRONG")
        fs = features(text) or {"(none)"}
        for f in fs:
            table[f][v] += 1
    for f, c in sorted(table.items(), key=lambda kv: -kv[1]["WRONG"]):
        print(f"{f:16} WRONG {c['WRONG']:4}  REFUSED {c['REFUSED']:4}  right {c['own-right']:4}")


if __name__ == "__main__" and len(sys.argv) > 4 and sys.argv[4] == "--attribute":
    attribute(*sys.argv[1:4])
