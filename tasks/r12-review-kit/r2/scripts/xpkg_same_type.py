import sqlite3, sys, os, collections
def rows(db):
    c = sqlite3.connect(db)
    q = """SELECT fs.path, ns.qualified_name, ft.path, nt.qualified_name, e.confidence, e.metadata FROM edges e
      JOIN nodes ns ON ns.id=e.source_id JOIN files fs ON fs.id=ns.file_id
      JOIN nodes nt ON nt.id=e.target_id JOIN files ft ON ft.id=nt.file_id
      WHERE e.relation='calls' AND fs.language='go' AND ns.type='method' AND nt.type='method'"""
    out = []
    for sp, sq, tp, tq, conf, meta in c.execute(q):
        if '.' not in sq or '.' not in tq: continue
        if sq.rsplit('.',1)[0] != tq.rsplit('.',1)[0]: continue
        if os.path.dirname(sp) == os.path.dirname(tp): continue
        out.append((sp, sq, tp, tq, conf, meta))
    return out
for db in sys.argv[1:]:
    r = rows(db)
    print(db, len(r), dict(collections.Counter(x[4] for x in r)), dict(collections.Counter('rtype' in (x[5] or '') for x in r)))
    for x in r[:int(os.environ.get('N','0'))]: print('  ', *x)
