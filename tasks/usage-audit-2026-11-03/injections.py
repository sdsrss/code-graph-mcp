import glob, json, os, sys, collections
pd = os.path.expanduser("~/.claude/projects")
paths = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl")) + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl")))
SINCE = "2026-10-23T23:00"
out = []
for p in paths:
    try: lines = open(p, errors="replace").read().splitlines()
    except OSError: continue
    recs = []
    for l in lines:
        try: recs.append(json.loads(l))
        except ValueError: pass
    start = next((r.get("timestamp") for r in recs if r.get("timestamp")), None)
    entry = next((r.get("entrypoint") for r in recs if r.get("entrypoint")), None)
    cwd = next((r.get("cwd") for r in recs if r.get("cwd")), "")
    if not start or start < SINCE or entry != "cli": continue
    calls = []  # ordered tool_use
    for i, r in enumerate(recs):
        m = r.get("message") or {}
        if r.get("type") == "assistant" and isinstance(m.get("content"), list):
            for c in m["content"]:
                if c.get("type") == "tool_use": calls.append((i, c))
    for i, r in enumerate(recs):
        a = r.get("attachment") or {}
        t = a.get("type")
        if t not in ("hook_additional_context", "hook_success", "hook_error", "hook_non_blocking_error", "hook_blocking_error", "hook_cancelled"): continue
        content = a.get("content")
        text = "\n".join(map(str, content)) if isinstance(content, list) else str(content or a.get("stdout") or a.get("stderr") or "")
        hn = str(a.get("hookName") or "") + str(a.get("command") or "")
        if "[code-graph" not in text and "code-graph AST index" not in text and "code-graph" not in hn: continue
        if t == "hook_success" and not text.strip(): continue
        tid = a.get("toolUseID")
        idx = next((k for k, (j, c) in enumerate(calls) if c.get("id") == tid), None)
        nxt = []
        if idx is not None:
            for j, c in calls[idx:idx + 11]:
                inp = c.get("input") or {}
                nxt.append(c["name"] + ":" + str(inp.get("command") or inp.get("file_path") or inp.get("pattern") or "")[:120])
        out.append(dict(path=p, cwd=cwd, sub="/subagents/" in p, ts=r.get("timestamp"), type=t, event=a.get("hookEvent"), text=text[:1500], next=nxt))
json.dump(out, open(sys.argv[1], "w"), indent=1, ensure_ascii=False)
c = collections.Counter((o["type"], o["event"], o["sub"]) for o in out)
for k, v in sorted(c.items(), key=str): print(k, v)
