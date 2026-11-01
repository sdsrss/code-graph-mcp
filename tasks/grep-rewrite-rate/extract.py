"""Every Bash tool_use whose command mentions grep/egrep/rg/ag, from Claude Code transcripts.

usage: python3 extract.py <since-ISO-date> <out.jsonl>

One JSON object per command: ts, cwd, sid, sub (subagent transcript), id,
cmd, ver (Claude Code version), rewritten (pre-grep-guide answered it with
updatedInput, read from the hook_success attachment of the same toolUseID).
Reads only ~/.claude/projects/*/*.jsonl and */*/subagents/agent-*.jsonl
(explicit depths, no recursive walk).
"""
import glob
import json
import os
import re
import sys

since, out_path = sys.argv[1], sys.argv[2]
pd = os.path.expanduser("~/.claude/projects")
paths = glob.glob(os.path.join(pd, "*", "*.jsonl")) + glob.glob(
    os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl"))
GREPISH = re.compile(r"\b(?:grep|egrep|rg|ag)\b")
n = 0
with open(out_path, "w") as out:
    for p in paths:
        sub = "/subagents/" in p
        rewritten, rows = set(), []
        for line in open(p, errors="replace"):
            if '"Bash"' not in line and "pre-grep-guide" not in line:
                continue
            try:
                o = json.loads(line)
            except ValueError:
                continue
            a = o.get("attachment")
            if (isinstance(a, dict) and "pre-grep-guide" in str(a.get("command", ""))
                    and "updatedInput" in (a.get("stdout") or "")):
                rewritten.add(a.get("toolUseID"))
            content = (o.get("message") or {}).get("content")
            if not isinstance(content, list):
                continue
            for b in content:
                if not (isinstance(b, dict) and b.get("type") == "tool_use" and b.get("name") == "Bash"):
                    continue
                cmd = (b.get("input") or {}).get("command", "")
                ts = o.get("timestamp") or ""
                if ts >= since and GREPISH.search(cmd):
                    rows.append({"ts": ts, "cwd": o.get("cwd"), "sid": o.get("sessionId"), "sub": sub,
                                 "id": b["id"], "cmd": cmd, "ver": o.get("version")})
        for r in rows:
            r["rewritten"] = r["id"] in rewritten
            out.write(json.dumps(r) + "\n")
            n += 1
print(n, "commands")
