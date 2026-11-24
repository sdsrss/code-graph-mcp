#!/usr/bin/env python3
"""Token spend per project, main sessions vs subagents, since SINCE (2026-11-24 audit).

Counts each assistant message once (Claude Code writes one record per content
block with the same message.id), summing usage.input_tokens +
cache_creation_input_tokens + cache_read_input_tokens as "input" and
output_tokens as "output". Interactive and non-interactive sessions both count
(this is spend, not behaviour). Tokens, not dollars: the price per token is not
read from anywhere here. Usage: token_share.py [SINCE]
"""
import collections
import glob
import json
import os
import sys

PD = os.path.expanduser("~/.claude/projects")
SINCE = sys.argv[1] if len(sys.argv) > 1 else "2026-11-04T16:53"


def main():
    tot = collections.defaultdict(collections.Counter)
    for proj_dir in sorted(glob.glob(os.path.join(PD, "*"))):
        proj = os.path.basename(proj_dir).replace("-home-ai-dev-", "")
        files = glob.glob(os.path.join(proj_dir, "*.jsonl")) + glob.glob(os.path.join(proj_dir, "*", "subagents", "agent-*.jsonl"))
        for p in files:
            kind = "sub" if "/subagents/" in p else "main"
            seen = set()
            sess_counted = False
            for line in open(p, errors="replace"):
                if '"usage"' not in line:
                    continue
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                ts = r.get("timestamp") or ""
                if ts < SINCE or r.get("type") != "assistant":
                    continue
                m = r.get("message") or {}
                key = (m.get("id"), r.get("requestId"))
                if key in seen:
                    continue
                seen.add(key)
                u = m.get("usage") or {}
                C = tot[(proj, kind)]
                C["input"] += (u.get("input_tokens") or 0) + (u.get("cache_creation_input_tokens") or 0) + (u.get("cache_read_input_tokens") or 0)
                C["output"] += u.get("output_tokens") or 0
                C["requests"] += 1
                if not sess_counted:
                    C["sessions"] += 1
                    sess_counted = True
    grand_in = sum(c["input"] for c in tot.values())
    grand_out = sum(c["output"] for c in tot.values())
    print(f"since {SINCE}; input = uncached + cache write + cache read")
    print(f"{'project':18s} {'kind':4s} {'sess':>5s} {'requests':>9s} {'input M':>9s} {'in %':>6s} {'output M':>9s} {'out %':>6s}")
    for (proj, kind), c in sorted(tot.items(), key=lambda x: -x[1]["input"]):
        print(f"{proj:18s} {kind:4s} {c['sessions']:5d} {c['requests']:9d} {c['input']/1e6:9.1f} {100*c['input']/grand_in:6.1f} "
              f"{c['output']/1e6:9.2f} {100*c['output']/grand_out:6.1f}")
    by = collections.Counter()
    for (proj, kind), c in tot.items():
        by[proj] += c["input"]
    print("\nper project, input share:", ", ".join(f"{p} {100*v/grand_in:.1f}%" for p, v in by.most_common()))
    print(f"total input {grand_in/1e9:.2f}B, output {grand_out/1e6:.1f}M")


if __name__ == "__main__":
    main()
