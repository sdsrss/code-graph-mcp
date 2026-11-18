"""classify_new.py <base.jsonl> <head.jsonl> <base-binary>: every seed head diverges on and base does not is
replayed on base with renamed imports stripped; 'masked' when base then diverges at the same step with the same rows."""
import json, re, sys, os, subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
a = {r['seed']: r for r in map(json.loads, open(sys.argv[1]))}
h = {r['seed']: r for r in map(json.loads, open(sys.argv[2]))}
new = sorted(s for s in h if not h[s].get('ok') and a[s].get('ok'))
pat = re.compile(r'^(\s*)from \S+ import (\w+) as (\w+)$', re.M)
tmp = os.path.join(os.path.dirname(os.path.abspath(sys.argv[2])), 'cls-variants.jsonl')
with open(tmp, 'w') as fh:
    for s in new:
        r = json.loads(json.dumps(h[s]))
        for st in r['states']:
            for p in st:
                st[p] = pat.sub(lambda m: m.group(1) + 'pass' if m.group(2) != m.group(3) else m.group(0), st[p])
        fh.write(json.dumps(r) + "\n")
out = subprocess.run([sys.executable, '-I', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'rerun.py'), tmp, sys.argv[3]], capture_output=True, text=True).stdout
blocks = {}
cur = None
for line in out.splitlines():
    m = re.match(r'seed (\d+): (OK|DIVERGED at step (\d+))', line)
    if m: cur = int(m.group(1)); blocks[cur] = {'ok': m.group(2) == 'OK', 'step': m.group(3), 'rows': set()}
    elif cur is not None: blocks[cur]['rows'].add(line.strip().split(None, 1)[1])
for s in new:
    b = blocks[s]; r = h[s]
    hr = {x.strip() for x in r['inc_only'] + r['fresh_only']}
    verdict = 'NEW' if b['ok'] else ('masked' if int(b['step']) == r['step'] and hr <= b['rows'] else 'masked?(differs)')
    print(f"seed {s}: {verdict}  head step {r['step']} rows {len(hr)}; stripped-base step {b['step']} rows {len(b['rows'])}")
