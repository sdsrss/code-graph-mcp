"""Dump each cross-file signature edit (sig_edits.py's population) with its Edit input, for classifying hook misses."""
import glob, json, os, sys
AUD = "/home/ai/dev/code-graph-mcp/tasks/usage-audit-2026-10-23"
sys.path.insert(0, AUD)
import deep_usage as du
import sig_edits as se
pd = os.path.expanduser("~/.claude/projects")
paths = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl")) + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl")))
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
    hooked = se.edit_hook_ids(p)
    for i, nm in sig:
        xf = se.cross_file_callers(root, nm)
        if xf <= 0:
            continue
        c = calls[i]
        inp = c["input"] if isinstance(c["input"], dict) else {}
        print(json.dumps({"proj": proj, "root": root, "name": nm, "xf": xf, "hook": c["id"] in hooked,
                          "cwd": s["cwd"], "session": os.path.basename(p), "ts": c.get("ts"),
                          "file_path": inp.get("file_path"), "old": inp.get("old_string", ""), "new": inp.get("new_string", "")}))
