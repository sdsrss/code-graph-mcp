#!/usr/bin/env python3
"""D#347: which symbol does the post-grep hook pick, and is it the grepped one?

    scan.py OUT.json [--since 2026-10-17]

Reads interactive transcripts (main + subagents) and finds every post-grep
injection that names a symbol: the one-line outside-path pointer
("[code-graph] X is defined in F…") and the cross-file call graph
("Cross-file call graph for the symbol you grepped"). For each it records the
grep command, the picked symbol, where the symbol sits in the command
(shape), the definition's language vs the grep's declaration keyword, and
whether any of the next 15 tool calls mention the symbol or the definition
file (followed). Prints counts per shape, own repos vs neutral projects.
"""
import collections
import glob
import json
import os
import re
import sys

PD = os.path.expanduser("~/.claude/projects")
OWN = ("/home/ai/dev/code-graph-mcp", "/home/ai/dev/claude-mem-lite")
DECL = {"fn": ".rs", "def": ".py", "function": (".js", ".ts", ".mjs", ".cjs", ".tsx", ".jsx"),
        "func": ".go", "class": None, "struct": None, "impl": ".rs", "const": None, "let": None,
        "type": None, "interface": None, "enum": None, "trait": ".rs", "async": None, "pub": ".rs"}


def seg(text):
    i = text.find("[code-graph")
    if i < 0:
        return ""
    j = text.find("\n[mem", i + 1)
    return text[i:j if j > 0 else None]


def pick(s):
    m = re.match(r"\[code-graph\] (\S+) is defined in (\S+?)(?::\d+-\d+)?, outside", s)
    if m:
        return "defptr", m.group(1), m.group(2).strip("'")
    if s.startswith("[code-graph] Cross-file call graph for"):
        m = re.search(r"\n(\S+) \(([^()\n]+)\)", s)
        if m:
            return "xcallgraph", m.group(1), m.group(2)
    return None


def shape(cmd, sym):
    """Where the first standalone occurrence of sym sits in the command."""
    for m in re.finditer(r"(?<![A-Za-z0-9_])" + re.escape(sym) + r"(?![A-Za-z0-9_])", cmd):
        a, b = m.start(), m.end()
        prev = cmd[a - 1] if a else ""
        prev2 = cmd[a - 2] if a > 1 else ""
        nxt = cmd[b] if b < len(cmd) else ""
        nxt2 = cmd[b + 1] if b + 1 < len(cmd) else ""
        if prev == "." and prev2 != "\\":
            return "after-dot"          # db.add, foo.install
        if nxt == "." and nxt2.isalpha():
            return "before-dot"         # install.lock, mod.fn
        if prev == "-" or (nxt == "-" and nxt2.isalpha()):
            return "hyphen"             # re-record, setup-hooks
        if prev == "/" or nxt == "/":
            return "path"
        before = cmd[:a].rstrip()
        w = re.search(r"([A-Za-z]+)\s*$", before)
        if w and w.group(1) in DECL and before.endswith(w.group(1)) and a - len(before) >= 1:
            return "decl:" + w.group(1)
        return "ident"
    return "absent"


def lang_mismatch(cmd, sym, deffile):
    m = re.search(r"(?<![A-Za-z0-9_])([a-z]+)\s+" + re.escape(sym) + r"(?![A-Za-z0-9_])", cmd)
    if not m or m.group(1) not in DECL or DECL[m.group(1)] is None:
        return None
    exts = DECL[m.group(1)]
    exts = exts if isinstance(exts, tuple) else (exts,)
    return not deffile.endswith(exts)


def main():
    out = sys.argv[1]
    since = sys.argv[sys.argv.index("--since") + 1] if "--since" in sys.argv else "2026-10-17"
    paths = sorted(glob.glob(os.path.join(PD, "*", "*.jsonl"))
                   + glob.glob(os.path.join(PD, "*", "*", "subagents", "agent-*.jsonl")))
    rows = []
    for p in paths:
        try:
            recs = [json.loads(line) for line in open(p, errors="replace") if line.strip()]
        except (OSError, ValueError):
            continue
        start = next((r.get("timestamp") for r in recs if r.get("timestamp")), None)
        entry = next((r.get("entrypoint") for r in recs if r.get("entrypoint")), None)
        cwd = next((r.get("cwd") for r in recs if r.get("cwd")), "")
        if not start or start < since or entry != "cli":
            continue
        calls = []
        for r in recs:
            m = r.get("message") or {}
            if r.get("type") == "assistant" and isinstance(m.get("content"), list):
                calls.extend(c for c in m["content"] if c.get("type") == "tool_use")
        ids = {c.get("id"): n for n, c in enumerate(calls)}
        for r in recs:
            a = r.get("attachment") or {}
            if a.get("type") != "hook_additional_context" or a.get("hookEvent") != "PostToolUse":
                continue
            content = a.get("content")
            text = "\n".join(map(str, content)) if isinstance(content, list) else str(content or "")
            s = seg(text)
            got = pick(s) if s else None
            if not got:
                continue
            kind, sym, deffile = got
            idx = ids.get(a.get("toolUseID"))
            cmd = str(((calls[idx].get("input") or {}) if idx is not None else {}).get("command") or "")
            nxt = calls[idx + 1:idx + 16] if idx is not None else []
            blob = [json.dumps(c.get("input") or {}) for c in nxt]
            rows.append({
                "ts": r.get("timestamp"), "proj": cwd, "own": cwd.startswith(OWN),
                "sub": "/subagents/" in p, "kind": kind, "sym": sym, "deffile": deffile,
                "chars": len(s), "cmd": cmd[:400], "shape": shape(cmd, sym),
                "lang_mismatch": lang_mismatch(cmd, sym, deffile),
                "followed_sym": any(re.search(r"(?<![A-Za-z0-9_])" + re.escape(sym) + r"(?![A-Za-z0-9_])", b)
                                    for b in blob),
                "followed_file": any(deffile in b for b in blob),
            })
    rows.sort(key=lambda r: r["ts"] or "")
    json.dump(rows, open(out, "w"), indent=1, ensure_ascii=False)
    for own in (False, True):
        rs = [r for r in rows if r["own"] == own]
        print(f"== {'own repos' if own else 'neutral'}: {len(rs)} picks, {sum(r['chars'] for r in rs)} chars")
        by = collections.defaultdict(list)
        for r in rs:
            by[(r["kind"], r["shape"].split(":")[0])].append(r)
        for (k, sh), g in sorted(by.items()):
            print(f"  {k:10s} {sh:10s} n={len(g):3d} lang_mismatch={sum(1 for r in g if r['lang_mismatch'])}"
                  f" followed_file={sum(r['followed_file'] for r in g)} followed_sym={sum(r['followed_sym'] for r in g)}")


if __name__ == "__main__":
    main()
