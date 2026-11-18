"""strip_variant.py <in.jsonl> <seeds,...> <out.jsonl>: renamed `from X import Y as Z` lines -> `pass` (same indent)."""
import json, re, sys
rows = {r['seed']: r for r in map(json.loads, open(sys.argv[1]))}
pat = re.compile(r'^(\s*)from \S+ import (\w+) as (\w+)$', re.M)
with open(sys.argv[3], 'w') as fh:
    for s in map(int, sys.argv[2].split(',')):
        r = rows[s]
        for st in r['states']:
            for p in st:
                st[p] = pat.sub(lambda m: m.group(1) + 'pass' if m.group(2) != m.group(3) else m.group(0), st[p])
        r['seed'] = 900000 + s
        fh.write(json.dumps(r) + "\n")
