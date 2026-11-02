import json, sys, collections
def load(p):
    return json.load(open(p))
for corp in sys.argv[1:]:
    A = load(f"/var/tmp/d370/out/base/{corp}.judged.json"); B = load(f"/var/tmp/d370/out/head2/{corp}.judged.json")
    def stats(J):
        c = collections.Counter()
        for r in J:
            c[(r['tier'], r['verdict'])] += 1
        return c
    for name, J in (("base", A), ("head2", B)):
        c = stats(J)
        dflt = [r for r in J if r['tier'] in ('extracted', 'inferred')]
        v = collections.Counter(r['verdict'] for r in dflt)
        right = v['correct']; tot = len(dflt); wrong = tot - right
        print(f"{corp} {name}: default-floor judged={tot} correct={right} other={wrong} verdicts={dict(v)} prec={right/tot*100:.1f}%")
    key = lambda r: (r['caller_at'], r['caller'], r['callee'], r['callee_at'])
    da = {key(r): r for r in A if r['tier'] in ('extracted','inferred')}
    db = {key(r): r for r in B if r['tier'] in ('extracted','inferred')}
    lost = [da[k] for k in da.keys() - db.keys()]; added = [db[k] for k in db.keys() - da.keys()]
    print(f"  default-floor lost={len(lost)} {dict(collections.Counter(r['verdict'] for r in lost))}; added={len(added)} {dict(collections.Counter(r['verdict'] for r in added))}")
    # tier moves
    ka = {key(r): r['tier'] for r in A}; kb = {key(r): r['tier'] for r in B}
    moves = collections.Counter((ka[k], kb[k]) for k in ka.keys() & kb.keys() if ka[k] != kb[k])
    print(f"  all-tier lost={len(ka.keys()-kb.keys())} added={len(kb.keys()-ka.keys())} tier-moves={dict(moves)}")
    if len(sys.argv) > 2 or corp != 'std':
        for r in added[:20]: print("   ADDED", r)
