#!/usr/bin/env python3
"""D#229 tables: where the unresolved-call disclosure fires against where the
zero answer is wrong (rust-analyzer finds a caller).

    analyze.py POPULATION_JSON SITES_JSONL

A definition "fires" when its name has a call with no resolved target. Per
kind (methods: qualified name with an owner; free functions), for each truth
(`ra_any`, `ra_prod`): how often it fires, the share of firings where the zero
is wrong (hit), the share of wrong zeros it fires on (coverage), and how often
the zero is wrong when it stays silent. Printed for the shipped rule (calls
with no resolved target) and for counting every call of the name.
"""
import json
import sys


def main():
    pop = json.load(open(sys.argv[1]))
    sites = {}
    for line in open(sys.argv[2]):
        r = json.loads(line)
        sites[r["name"]] = r
    missing = {p["name"] for p in pop} - set(sites)
    print(f"population {len(pop)}; names without a scan result: {len(missing)}")

    rules = {
        "listed (calls with no resolved target)": lambda s: not s["resolved"],
        "any call of the name": lambda s: True,
    }
    for rule, keep in rules.items():
        def fired(name):
            r = sites.get(name)
            return bool(r and r["calls"] is not None and any(keep(s) for s in r["sites"]))
        print(f"rule: {rule}")
        table(pop, fired)


def table(pop, fired):
    for truth in ("ra_any", "ra_prod"):
        print(f"  truth = {truth}")
        for kind in ("method", "free"):
            sub = [p for p in pop if (kind == "method") == bool(p["qn"] and "." in p["qn"])]
            tp = sum(1 for p in sub if fired(p["name"]) and p[truth] > 0)
            fp = sum(1 for p in sub if fired(p["name"]) and p[truth] == 0)
            fn = sum(1 for p in sub if not fired(p["name"]) and p[truth] > 0)
            tn = len(sub) - tp - fp - fn
            pct = lambda a, b: f"{a}/{b} = {a / b:.1%}" if b else f"{a}/0"
            print(f"    {kind:6s} fires {pct(tp + fp, len(sub))}  hit {pct(tp, tp + fp)}  "
                  f"coverage {pct(tp, tp + fn)}  wrong when silent {pct(fn, fn + tn)}")


if __name__ == "__main__":
    main()
