#!/usr/bin/env python3
"""Full edge-set diff of two index.db files, by node identity (name, path, line).
edgediff.py A.db B.db [--show N] [--relation calls]"""
import argparse, collections, sqlite3


def edges(p, rel):
    db = sqlite3.connect(p)
    q = """SELECT sn.name, sf.path, sn.start_line, tn.name, tf.path, tn.start_line, e.relation, e.confidence, COALESCE(e.metadata,'')
           FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
    if rel:
        q += f" WHERE e.relation = '{rel}'"
    return collections.Counter(db.execute(q).fetchall())


ap = argparse.ArgumentParser()
ap.add_argument("a"); ap.add_argument("b"); ap.add_argument("--show", type=int, default=20); ap.add_argument("--relation")
x = ap.parse_args()
A, B = edges(x.a, x.relation), edges(x.b, x.relation)
only_a, only_b = A - B, B - A
print(f"A {sum(A.values())} edges, B {sum(B.values())}; only-A {sum(only_a.values())}, only-B {sum(only_b.values())}")
# pair up label-only moves (same endpoints+relation, different confidence/metadata)
key = lambda e: e[:7]
ka = collections.defaultdict(list); kb = collections.defaultdict(list)
for e in only_a.elements(): ka[key(e)].append(e)
for e in only_b.elements(): kb[key(e)].append(e)
moves = collections.Counter()
for k in set(ka) & set(kb):
    for ea, eb in zip(ka[k], kb[k]):
        moves[(ea[7], eb[7], ea[8] != eb[8])] += 1
print("label/metadata moves (conf_a, conf_b, metadata_changed):", dict(moves))
gone = [e for k, v in ka.items() if k not in kb for e in v]
new = [e for k, v in kb.items() if k not in ka for e in v]
print(f"edges removed {len(gone)}, added {len(new)}")
for e in gone[:x.show]: print("  -", e)
for e in new[:x.show]: print("  +", e)
