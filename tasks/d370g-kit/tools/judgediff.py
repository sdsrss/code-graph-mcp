import json, sys, collections
# judgediff.py BASE_JUDGED HEAD_JUDGED: default-floor correct lost / wrong added, by pair
def load(p):
    d = {}
    for e in json.load(open(p)):
        d[(e["caller"], e["caller_at"], e["callee"], e["callee_at"])] = (e["tier"], e["verdict"])
    return d
b = load(sys.argv[1]); h = load(sys.argv[2])
vis = lambda t: t in ("extracted", "inferred")
lost = [k for k, (t, v) in b.items() if vis(t) and v == "correct" and not (k in h and vis(h[k][0]))]
added = [k for k, (t, v) in h.items() if vis(t) and v != "correct" and not (k in b and vis(b[k][0]))]
print("correct lost from the default floor:", len(lost))
for k in sorted(lost): print("  -", k[0], k[1], "->", k[2], k[3], "now", h.get(k))
print("wrong added to the default floor:", len(added))
for k in sorted(added): print("  +", k[0], k[1], "->", k[2], k[3], h[k])
