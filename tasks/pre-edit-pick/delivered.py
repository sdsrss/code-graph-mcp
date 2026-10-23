"""toolUseID -> symbol of every delivered pre-edit impact summary in the transcripts."""
import glob, json, os, re, sys
pd = os.path.expanduser("~/.claude/projects")
paths = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl")) + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl")))
out = {}
for p in paths:
    for line in open(p, errors="replace"):
        if "[code-graph:impact]" not in line:
            continue
        try:
            a = json.loads(line).get("attachment") or {}
        except ValueError:
            continue
        if a.get("hookEvent") != "PreToolUse":
            continue
        for t in a.get("content") or []:
            m = re.match(r"\s*\[code-graph:impact\] (\S+?)\(\) — Risk: (\w+)\n\s+(\d+) direct", str(t))
            if m:
                out[a.get("toolUseID")] = {"symbol": m.group(1), "risk": m.group(2), "direct": int(m.group(3))}
json.dump(out, open(sys.argv[1], "w"))
print(len(out), "delivered impact summaries")
