#!/usr/bin/env python3
"""Signature edits (a def/fn/function header whose parameters change, or which
disappears) in interactive sessions: was the name searched (grep or
code-graph) within 25 calls before / 15 after, did code-graph's pre-edit hook
inject callers on that Edit, and does the current index show callers in OTHER
files (the case where an unchecked change can break something unseen)."""
import collections
import glob
import json
import os
import sqlite3
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import deep_usage as du  # noqa: E402


def cross_file_callers(root, name):
    db = os.path.join(root, ".code-graph", "index.db")
    c = sqlite3.connect(f"file:{db}?immutable=1", uri=True)
    q = """SELECT COUNT(DISTINCT sf.path) FROM edges e
           JOIN nodes t ON t.id = e.target_id JOIN files tf ON tf.id = t.file_id
           JOIN nodes s ON s.id = e.source_id JOIN files sf ON sf.id = s.file_id
           WHERE e.relation = 'calls' AND t.name = ? AND sf.path <> tf.path"""
    n = c.execute(q, (name,)).fetchone()[0]
    c.close()
    return n


def edit_hook_ids(path):
    ids = set()
    for line in open(path, errors="replace"):
        if "hook_additional_context" not in line:
            continue
        try:
            a = json.loads(line).get("attachment") or {}
        except ValueError:
            continue
        if a.get("hookEvent") == "PreToolUse" and any(str(t).lstrip().startswith("[code-graph")
                                                      for t in (a.get("content") or [])):
            ids.add(a.get("toolUseID"))
    return ids


def main():
    pd = os.path.expanduser("~/.claude/projects")
    paths = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl"))
                   + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl")))
    tot = collections.Counter()
    rows = []
    for p in paths:
        try:
            s = du.parse(p)
        except OSError:
            continue
        if s["entry"] != "cli" or not s["cwd"] or not s["start"]:
            continue
        root, proj = du.root_of(s["cwd"])
        if not root:
            continue
        calls = du.analyse(s, root)
        sig = du.sig_edits(calls)
        if not sig:
            continue
        hooked = edit_hook_ids(p)
        for i, nm in sig:
            before = any(du.mentions(d, nm) for d in calls[max(0, i - 25):i])
            after = any(du.mentions(d, nm) for d in calls[i + 1:i + 16])
            hook = calls[i]["id"] in hooked
            xf = cross_file_callers(root, nm)
            grp = "own" if proj in du.OWN else "neutral"
            tot[(grp, "n")] += 1
            tot[(grp, "xfile")] += int(xf > 0)
            tot[(grp, "xfile_searched")] += int(xf > 0 and (before or after))
            tot[(grp, "xfile_hook")] += int(xf > 0 and hook)
            tot[(grp, "xfile_neither")] += int(xf > 0 and not (before or after or hook))
            tot[(grp, "hook")] += int(hook)
            rows.append((grp, proj, nm, xf, before, after, hook))
    for k in sorted(tot):
        print(k, tot[k])
    for r in rows:
        if r[3] > 0:
            print(r)


if __name__ == "__main__":
    main()
