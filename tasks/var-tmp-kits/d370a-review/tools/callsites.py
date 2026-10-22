#!/usr/bin/env python3
"""callsites.py <dirA> <dirB> [rel] [--show cat N]: group edges by (src file, src name, src line, target name)
and classify how each group's target set moved A -> B."""
import sqlite3, sys, collections
A, B = sys.argv[1], sys.argv[2]
rel = sys.argv[3] if len(sys.argv) > 3 and not sys.argv[3].startswith('--') else 'calls'
Q = """select coalesce(sf.path,'?'), sn.name, sn.start_line, tn.name, coalesce(tf.path,'?') || '@' || tn.start_line || '(' || tn.type || ')'
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id where e.relation=?"""
def load(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    g = collections.defaultdict(set)
    for r in con.execute(Q, (rel,)): g[r[:4]].add(r[4])
    return g
ga, gb = load(A), load(B)
cats = collections.defaultdict(list)
for k in set(ga) | set(gb):
    a, b = ga.get(k, set()), gb.get(k, set())
    if a == b: continue
    if not b: c = "lost-all"
    elif not a: c = "new"
    elif b < a: c = "narrowed"
    elif a < b: c = "widened"
    elif not (a & b): c = "retargeted"
    else: c = "mixed"
    cats[c].append((k, sorted(a), sorted(b)))
for c, v in sorted(cats.items()): print(c, len(v))
show = sys.argv[sys.argv.index('--show')+1:] if '--show' in sys.argv else []
for i in range(0, len(show), 2):
    c, n = show[i], int(show[i+1])
    print("####", c)
    for k, a, b in sorted(cats[c])[:n]:
        print(f"  {k[0]}:{k[1]}@{k[2]} calls {k[3]}\n      A={a}\n      B={b}")
