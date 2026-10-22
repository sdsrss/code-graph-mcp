import json, sys, collections
n = sys.argv[1]
def load(arm):
    d = {}
    for e in json.load(open(f'{n}/{arm}.judged.json')):
        d[(e['caller'], e['caller_at'], e['callee'], e['callee_at'])] = (e['tier'], e['verdict'])
    return d
b, c = load('base'), load('cand')
tr = collections.Counter()
lost_correct = []
new_wrong = []
for k, (t, v) in b.items():
    if k not in c:
        tr[('gone', t, v)] += 1
        if v == 'correct': lost_correct.append((k, t))
    else:
        t2, v2 = c[k]
        if t2 != t: tr[('retier', t + '->' + t2, v)] += 1
for k, (t, v) in c.items():
    if k not in b:
        tr[('new', t, v)] += 1
        if v != 'correct': new_wrong.append((k, t, v))
for k, v in sorted(tr.items()): print(k, v)
print('LOST CORRECT', len(lost_correct))
for x in lost_correct[:40]: print('  ', x)
print('NEW WRONG', len(new_wrong))
for x in sorted(new_wrong, key=lambda x: x[1])[: int(sys.argv[2]) if len(sys.argv) > 2 else 30]: print('  ', x)
