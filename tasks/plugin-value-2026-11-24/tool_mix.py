"""Tool mix per project since SINCE: LSP vs code-graph vs grep-ish, main and subagents.
Fixed-depth globs only (projects/*/*.jsonl, projects/*/*/subagents/agent-*.jsonl)."""
import collections, glob, json, os, re, sys
PD = os.path.expanduser("~/.claude/projects")
SINCE = sys.argv[1] if len(sys.argv) > 1 else "2026-11-04T16:53"
CG = re.compile(r"code-graph-mcp\s+(callgraph|impact|refs|show|search|grep|overview|map|deps|ast-search|trace|affected)")
GREP = re.compile(r"(^|[|;&\s])(grep|rg|ag)\s")
c = collections.defaultdict(collections.Counter)
lsp_ops = collections.Counter()
lsp_err = 0
for proj_dir in sorted(glob.glob(os.path.join(PD, "*"))):
    proj = os.path.basename(proj_dir).replace("-home-ai-dev-", "")
    for p in glob.glob(os.path.join(proj_dir, "*.jsonl")) + glob.glob(os.path.join(proj_dir, "*", "subagents", "agent-*.jsonl")):
        kind = "sub" if "/subagents/" in p else "main"
        k = (proj, kind)
        ids = {}
        for line in open(p, errors="replace"):
            try: ev = json.loads(line)
            except ValueError: continue
            if (ev.get("timestamp") or "") < SINCE: continue
            msg = ev.get("message") or {}
            cont = msg.get("content")
            if not isinstance(cont, list): continue
            for b in cont:
                if not isinstance(b, dict): continue
                if b.get("type") == "tool_use":
                    n = b.get("name", ""); inp = b.get("input") or {}
                    c[k]["tools"] += 1
                    if n == "LSP":
                        c[k]["lsp"] += 1; lsp_ops[inp.get("operation")] += 1; ids[b.get("id")] = 1
                    elif n.startswith("mcp__plugin_code-graph"):
                        c[k]["cg"] += 1
                    elif n == "Grep":
                        c[k]["grep"] += 1
                    elif n == "Bash":
                        cmd = inp.get("command", "")
                        if CG.search(cmd): c[k]["cg"] += 1
                        elif GREP.search(cmd): c[k]["grep"] += 1
                elif b.get("type") == "tool_result" and b.get("tool_use_id") in ids:
                    if b.get("is_error"): lsp_err += 1
print(f"since {SINCE}")
print(f"{'project':18} {'kind':5} {'tools':>7} {'LSP':>5} {'cg':>5} {'grep':>6}")
for (proj, kind), v in sorted(c.items(), key=lambda x: -x[1]['tools']):
    if v["tools"] < 50: continue
    print(f"{proj:18} {kind:5} {v['tools']:7} {v['lsp']:5} {v['cg']:5} {v['grep']:6}")
print("LSP ops:", dict(lsp_ops), "LSP errors:", lsp_err)
