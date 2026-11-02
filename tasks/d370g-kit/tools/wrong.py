import json, sys, collections
# wrong.py JUDGED [tiers]: list wrong edges at the given tiers (default extracted,inferred)
d = json.load(open(sys.argv[1]))
tiers = (sys.argv[2] if len(sys.argv) > 2 else "extracted,inferred").split(",")
w = [e for e in d if e["tier"] in tiers and e["verdict"] != "correct"]
c = collections.Counter(e["verdict"] for e in d if e["tier"] in tiers)
print("verdicts:", dict(c))
for e in sorted(w, key=lambda e: (e["tier"], e["callee"])):
    print(f'{e["tier"][:3]} {e["verdict"][:6]} {e["caller"]}@{e["caller_at"]} -> {e["callee"]}@{e["callee_at"]}')
