#!/usr/bin/env python3
"""Outcome of every explicit code-graph call in interactive transcripts:
ok / empty output / no index / error / stale-index notice / no match,
by era and project group, plus where the call ran (indexed root or a copy)."""
import collections
import glob
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import deep_usage as du  # noqa: E402

NOINDEX = re.compile(r"No index found|not indexed|no \.code-graph", re.I)
STALE = re.compile(r"built by an older code-graph")
NOMATCH = re.compile(r"No (?:symbol|node|definition|callers|references|matches)|not found in the index|"
                     r"did you mean|has no call graph|0 results", re.I)


def main():
    pd = os.path.expanduser("~/.claude/projects")
    paths = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl"))
                   + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl")))
    out = collections.defaultdict(collections.Counter)
    ex = collections.defaultdict(list)
    for p in paths:
        try:
            s = du.parse(p)
        except OSError:
            continue
        if s["entry"] != "cli" or not s["cwd"]:
            continue
        root, proj = du.root_of(s["cwd"])
        if not root:
            continue
        era = du.era_of(s["start"])
        grp = "own" if proj in du.OWN else "neutral"
        for c in s["order"]:
            hit = du._explicit(c["name"], c["input"])
            if not hit or hit[0] != "cg":
                continue
            cmd = str((c["input"] or {}).get("command", "")) if hit[1] == "bash" else c["name"]
            if hit[1] == "bash" and re.search(r"(^|[\s;&|(])(\./)?target/(release|debug)/code-graph-mcp", cmd):
                continue  # dev build under test
            r = s["results"].get(c["id"])
            text, err = (r[0], r[1]) if r else ("", None)
            where = "copy" if re.search(r"(^|\s)cd\s+\S*(scratchpad|/tmp/|/var/tmp/|worktrees)", cmd) else "root"
            suppressed = "2>/dev/null" in cmd or "2>&1 >/dev/null" in cmd
            if r is None:
                k = "no-result"
            elif NOINDEX.search(text):
                k = "no-index"
            elif text.strip() in ("", "(Bash completed with no output)"):
                k = "empty"
            elif err:
                k = "error"
            elif STALE.search(text):
                k = "ok+stale-notice"
            elif NOMATCH.search(text[:600]):
                k = "no-match"
            else:
                k = "ok"
            key = (era, s["kind"], grp)
            out[key][k] += 1
            out[key]["where:" + where] += 1
            out[key][f"{where}:{k}"] += 1
            if suppressed:
                out[key]["stderr-suppressed"] += 1
            if k in ("empty", "no-index", "error", "no-match") and len(ex[key]) < 12:
                ex[key].append((proj, k, cmd[:200].replace("\n", " ")))
    for key in sorted(out):
        print("|".join(key), dict(sorted(out[key].items())))
    print()
    for key in sorted(ex):
        print("==", "|".join(key))
        for e in ex[key]:
            print("  ", e)


if __name__ == "__main__":
    main()
