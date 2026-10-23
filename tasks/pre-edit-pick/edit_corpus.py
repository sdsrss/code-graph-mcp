"""All Edit/MultiEdit hunks in this machine's transcripts (fixed-depth globs), one JSON line each."""
import glob, json, os, sys
pd = os.path.expanduser("~/.claude/projects")
paths = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl")) + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl")))
n = 0
out = open(sys.argv[1], "w")
for p in paths:
    entry = None
    for line in open(p, errors="replace"):
        if '"tool_use"' not in line and '"entrypoint"' not in line:
            continue
        try:
            o = json.loads(line)
        except ValueError:
            continue
        entry = entry or o.get("entrypoint")
        m = o.get("message") or {}
        c = m.get("content")
        if not isinstance(c, list):
            continue
        for b in c:
            if not isinstance(b, dict) or b.get("type") != "tool_use" or b.get("name") not in ("Edit", "MultiEdit"):
                continue
            inp = b.get("input") or {}
            fp = str(inp.get("file_path", ""))
            edits = inp.get("edits") or [{"old_string": inp.get("old_string", ""), "new_string": inp.get("new_string", "")}]
            for k, e in enumerate(edits):
                out.write(json.dumps({"id": b.get("id"), "k": k, "entry": entry, "proj": os.path.basename(os.path.dirname(p)) if "/subagents/" not in p else os.path.basename(os.path.dirname(os.path.dirname(os.path.dirname(p)))),
                                      "sub": "/subagents/" in p, "ts": o.get("timestamp"), "file_path": fp,
                                      "old": str(e.get("old_string", "")), "new": str(e.get("new_string", ""))}) + "\n")
                n += 1
print(n, "hunks from", len(paths), "transcripts")
