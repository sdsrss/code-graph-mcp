#!/usr/bin/env python3
"""Headroom in the wild: how often does real work hit an error that a stale
call site produces (a caller not updated after a definition changed)?

Scans tool results in interactive transcripts since SINCE for error shapes a
missed caller produces, then, per hit, how many tool calls passed until the
model next edited a file (the repair) and whether the name the error carries
was defined in the project index (so code-graph could have listed its callers).

Shapes:
  py-args     TypeError: ... unexpected keyword argument / missing N required
              positional argument / takes N positional arguments but M were given
  py-import   ImportError: cannot import name 'X'
  py-attr     AttributeError: ... has no attribute 'X'   (noisy: also None/typos)
  ts-args     TS2554 Expected N arguments / TS2305 has no exported member /
              TS2339 Property 'X' does not exist
  rs-args     error[E0061] / error[E0425] / error[E0599]

A hit is only an opportunity bound: the error may come from a third-party
library, an intentional red test, or a typo. Usage: missed_callers.py OUT.json
"""
import collections
import glob
import json
import os
import re
import sqlite3
import sys

PD = os.path.expanduser("~/.claude/projects")
SINCE = os.environ.get("SINCE", "2026-10-24T00:00")
OWN = ("/home/ai/dev/code-graph-mcp", "/home/ai/dev/claude-mem-lite")
ROOTS = ["/home/ai/dev/daagu", "/home/ai/dev/gsd-lite", "/home/ai/dev/claudemd", "/home/ai/dev/sgc",
         "/home/ai/dev/sec", "/home/ai/dev/code-graph-mcp", "/home/ai/dev/claude-mem-lite"]
SHAPES = [
    ("py-args", re.compile(r"TypeError: ([\w.]+)\(\) (?:got an unexpected keyword argument|missing \d+ required"
                           r"|takes \d+ positional arguments? but)")),
    ("py-import", re.compile(r"ImportError: cannot import name '(\w+)'")),
    ("py-attr", re.compile(r"AttributeError: (?:module )?'[\w.]+' (?:object )?has no attribute '(\w+)'")),
    ("ts-args", re.compile(r"TS(?:2554|2305|2339|2345)\b[^\n]*?'(\w+)'|TS2554: Expected \d+ arguments")),
    ("rs-args", re.compile(r"error\[E0(?:061|425|599)\][^\n]*?`(\w+)`")),
]
_names = {}


def index_names(root):
    if root not in _names:
        names = set()
        db = os.path.join(root, ".code-graph", "index.db")
        if os.path.exists(db):
            c = sqlite3.connect(f"file:{db}?immutable=1", uri=True)
            names = {n for (n,) in c.execute("SELECT name FROM nodes WHERE type NOT IN ('module','external_module')")}
            c.close()
        _names[root] = names
    return _names[root]


def root_of(cwd):
    return next((r for r in ROOTS if cwd == r or cwd.startswith(r + "/")), None)


def result_text(c):
    content = c.get("content")
    if isinstance(content, list):
        return "\n".join(str(x.get("text", "")) for x in content if isinstance(x, dict))
    return str(content or "")


def main():
    paths = sorted(glob.glob(os.path.join(PD, "*", "*.jsonl"))
                   + glob.glob(os.path.join(PD, "*", "*", "subagents", "agent-*.jsonl")))
    hits = []
    totals = collections.Counter()
    for p in paths:
        try:
            recs = [json.loads(line) for line in open(p, errors="replace") if line.strip()]
        except (OSError, ValueError):
            continue
        start = next((r.get("timestamp") for r in recs if r.get("timestamp")), None)
        entry = next((r.get("entrypoint") for r in recs if r.get("entrypoint")), None)
        cwd = next((r.get("cwd") for r in recs if r.get("cwd")), "")
        root = root_of(cwd)
        if not start or start < SINCE or entry != "cli" or not root:
            continue
        own = cwd.startswith(OWN)
        sub = "/subagents/" in p
        seq = []  # ordered ("use", name, input) / ("result", text)
        for r in recs:
            m = r.get("message") or {}
            if not isinstance(m.get("content"), list):
                continue
            for c in m["content"]:
                if c.get("type") == "tool_use":
                    seq.append(("use", c.get("name"), c.get("input") or {}))
                elif c.get("type") == "tool_result":
                    seq.append(("result", None, result_text(c)))
        totals[("own" if own else "neutral", "sub" if sub else "main", "tools")] += sum(1 for s in seq if s[0] == "use")
        seen = set()
        for i, (t, _, x) in enumerate(seq):
            if t != "result":
                continue
            for shape, rx in SHAPES:
                for m in rx.finditer(x):
                    name = next((g for g in m.groups() if g), "") if m.groups() else ""
                    name = name.split(".")[-1]
                    key = (shape, name)
                    if key in seen:
                        continue
                    seen.add(key)
                    later = [s for s in seq[i + 1:] if s[0] == "use"]
                    edit_at = next((k for k, s in enumerate(later)
                                    if s[1] in ("Edit", "Write", "MultiEdit")), None)
                    hits.append({"proj": root.rsplit("/", 1)[1], "own": own, "sub": sub, "shape": shape,
                                 "name": name, "indexed": bool(name) and name in index_names(root),
                                 "calls_to_edit": edit_at, "line": x[max(0, m.start() - 80):m.end() + 80],
                                 "path": p})
    json.dump(hits, open(sys.argv[1], "w"), indent=1, ensure_ascii=False)
    for k, v in sorted(totals.items()):
        print("tools", k, v)
    by = collections.defaultdict(list)
    for h in hits:
        by[("own" if h["own"] else "neutral", "sub" if h["sub"] else "main", h["shape"])].append(h)
    for k, hs in sorted(by.items()):
        idx = [h for h in hs if h["indexed"]]
        print(k, "hits", len(hs), "name in index", len(idx),
              "calls-to-edit p50", sorted(h["calls_to_edit"] for h in idx if h["calls_to_edit"] is not None)[len(idx) // 2:len(idx) // 2 + 1])


if __name__ == "__main__":
    main()
