#!/usr/bin/env python3
"""Was each code-graph hook injection worth its characters? (2026-11-24 audit)

For every code-graph injection in interactive transcripts since SINCE, split
by kind and check what the model did next:

  rewrite   PreToolUse grep -> `code-graph-mcp grep` rewrite: was the next
            Bash call the same grep again (a duplicate the rewrite caused)?
  grepview  PostToolUse "AST-aware view of your grep": hits whose container is
            only the file's module (no information beyond grep) vs hits with a
            fn/class container
  overview  "5+ Reads into DIR": did one of the next 10 calls touch a file the
            overview lists? same text injected before in this session?
  impact    pre-edit impact: callers in another file than the one edited; was
            any of those files opened in the next 15 calls?
  ups       UserPromptSubmit search/impact: did any of the next 15 tool calls
            touch a file the injection names?
  subagent  SubagentStart facts: did the subagent call code-graph at all?

Reads transcripts only. Usage: inject_value.py OUT.json
"""
import collections
import glob
import json
import os
import re
import sys

PD = os.path.expanduser("~/.claude/projects")
SINCE = "2026-11-04T16:53"
OWN = ("/home/ai/dev/code-graph-mcp", "/home/ai/dev/claude-mem-lite")
PATH_RE = re.compile(r"(?<![\w/.-])((?:[\w.-]+/)+[\w.-]+\.\w+)")


def seg(text):
    """The code-graph part of an attachment that may hold several hooks' text."""
    i = text.find("[code-graph")
    if i < 0:
        return text if "code-graph AST index" in text else ""
    j = text.find("\n[mem", i + 1)
    return text[i:j if j > 0 else None]


def kind(s, event):
    if event == "SubagentStart":
        return "subagent"
    if "rewritten to its AST-aware equivalent" in s:
        return "rewrite"
    if "AST-aware view of your grep" in s:
        return "grepview"
    if "5+ Reads into" in s:
        return "overview"
    if event == "UserPromptSubmit":
        return "ups"
    if re.match(r"\[code-graph\] \S+ is defined in ", s):
        return "defptr"
    if s.startswith("[code-graph] Cross-file call graph for"):
        return "xcallgraph"
    if s.startswith("[code-graph:impact]") and event == "PreToolUse":
        return "impact"
    return "other:" + s[:40]


def touches(call, files):
    inp = call.get("input") or {}
    blob = json.dumps(inp)
    return any(f in blob for f in files)


def is_cg(call):
    inp = call.get("input") or {}
    if call.get("name", "").startswith("mcp__plugin_code-graph"):
        return True
    return "code-graph-mcp " in str(inp.get("command") or "")


def main():
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
        if not start or start < SINCE or entry != "cli":
            continue
        calls = []  # (record index, tool_use)
        for i, r in enumerate(recs):
            m = r.get("message") or {}
            if r.get("type") == "assistant" and isinstance(m.get("content"), list):
                calls.extend((i, c) for c in m["content"] if c.get("type") == "tool_use")
        sub = "/subagents/" in p
        seen_text = set()
        for i, r in enumerate(recs):
            a = r.get("attachment") or {}
            t, ev = a.get("type"), a.get("hookEvent")
            if t != "hook_additional_context" and not (t == "hook_success" and ev == "UserPromptSubmit"):
                continue
            content = a.get("content")
            text = "\n".join(map(str, content)) if isinstance(content, list) else str(content or a.get("stdout") or "")
            s = seg(text)
            if not s.strip():
                continue
            k = kind(s, ev)
            tid = a.get("toolUseID")
            idx = next((n for n, (_, c) in enumerate(calls) if c.get("id") == tid), None)
            if idx is None:  # prompt / subagent start: calls after this record
                idx = next((n for n, (j, _) in enumerate(calls) if j > i), len(calls))
                nxt = calls[idx:idx + 15]
            else:
                nxt = calls[idx + 1:idx + 16]  # after the call the hook ran on
            row = {"proj": cwd, "own": cwd.startswith(OWN), "sub": sub, "kind": k, "chars": len(s),
                   "ts": r.get("timestamp"), "path": p}
            files = sorted(set(PATH_RE.findall(s)))
            if k == "rewrite":
                cur = calls[idx][1] if idx < len(calls) else {}
                cmd = str((cur.get("input") or {}).get("command") or "")
                nb = next((c for _, c in nxt if c.get("name") == "Bash"), None)
                ncmd = str((nb.get("input") or {}).get("command") or "") if nb else ""
                m = re.search(r"'([^']+)'", s)
                pat = m.group(1) if m else ""
                row["regrep_same"] = bool(pat) and ("grep" in ncmd and pat.split("|")[0] in ncmd)
            elif k == "grepview":
                hits = re.findall(r"^\s+→ (\w+) ", s, re.M)
                row["hits"] = len(hits)
                row["module_only"] = sum(1 for h in hits if h == "module")
            elif k == "overview":
                row["dup_in_session"] = s in seen_text
                row["followed"] = any(touches(c, files) for _, c in nxt[:10])
            elif k == "impact":
                edited = str(((calls[idx][1].get("input") or {}) if idx < len(calls) else {}).get("file_path") or "")
                xfiles = [f for f in files if not edited.endswith(f)]
                row["xfile_callers"] = len(xfiles)
                row["xfile_opened"] = any(touches(c, xfiles) for _, c in nxt) if xfiles else None
            elif k in ("ups", "defptr", "xcallgraph"):
                row["followed"] = any(touches(c, files) for _, c in nxt) if files else None
                row["text"] = s[:600]
            elif k == "subagent":
                row["cg_used"] = any(is_cg(c) for _, c in calls)
            seen_text.add(s)
            rows.append(row)
    json.dump(rows, open(sys.argv[1], "w"), indent=1, ensure_ascii=False)
    for grp in (False, True):
        print("== own" if grp else "== neutral")
        by = collections.defaultdict(list)
        for r in rows:
            if r["own"] == grp:
                by[(r["kind"] if not r["kind"].startswith("other") else "other", r["sub"])].append(r)
        for (k, sub), rs in sorted(by.items()):
            line = f"  {k:9s} {'sub ' if sub else 'main'} n={len(rs):4d} chars={sum(r['chars'] for r in rs):7d}"
            if k == "rewrite":
                line += f" regrep_same={sum(r['regrep_same'] for r in rs)}"
            if k == "grepview":
                line += (f" hits={sum(r['hits'] for r in rs)} module_only={sum(r['module_only'] for r in rs)}"
                         f" all_module={sum(1 for r in rs if r['hits'] and r['hits'] == r['module_only'])}")
            if k == "overview":
                line += f" dup={sum(r['dup_in_session'] for r in rs)} followed={sum(r['followed'] for r in rs)}"
            if k == "impact":
                xs = [r for r in rs if r["xfile_callers"]]
                line += f" with_xfile={len(xs)} xfile_opened={sum(1 for r in xs if r['xfile_opened'])}"
            if k in ("ups", "defptr", "xcallgraph"):
                line += f" followed={sum(1 for r in rs if r['followed'])} named_files={sum(1 for r in rs if r['followed'] is not None)}"
            if k == "subagent":
                line += f" cg_used={sum(r['cg_used'] for r in rs)}"
            print(line)


if __name__ == "__main__":
    main()
