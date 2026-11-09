"""Headroom pilot (D#736): no-plugin baseline sessions on one caller-enumeration task.

    python3 run_pilot.py --corpus DIR --out DIR [--truth private/truth-daagu.json] --runs 3 \
        --model claude-opus-5-5 --max-total-usd 1.0 --per-session-usd 0.45

Each run is a real `claude -p` session with its own HOME, CLAUDE_CONFIG_DIR
(credentials linked in, nothing else: no plugins, hooks, memory or user
CLAUDE.md) and a fresh copy of the corpus without `.code-graph/`, outside
$HOME. PATH holds only node and claude plus /usr/bin:/bin, so no code-graph
binary is reachable. Runs are sequential; before each one the driver checks
spent + 1.5 x the running average against the total cap, and each session's
own `--max-budget-usd` is the smaller of the per-session cap and what is left.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CREDENTIALS = Path.home() / ".claude" / ".credentials.json"
TOOLS = ["Read", "Grep", "Glob", "Bash"]
SITE = re.compile(r"([\w./-]+\.py):(\d+)")
# A code-graph command or MCP tool, not the string: the scratchpad path itself
# contains "code-graph-mcp", which made every run of the first pass read as used.
USED_CG = re.compile(r"code-graph-mcp\s+(callgraph|impact|refs|show|search|grep|overview|map|deps|ast-search)"
                     r"|mcp__plugin_code-graph")


def truth(path):
    t = json.loads(path.read_text())
    return t["prompt"], set(t["scip_gold"]) | set(t["adjudicated_extra"])


def answer_sites(reply):
    head = reply.rfind("CALL SITES")
    if head < 0:
        return None
    return {f"{p.lstrip('./')}:{n}" for p, n in SITE.findall(reply[head:])}


def one_run(i, args, tools):
    root = args.out / f"r{i}"
    ws, home, cfg, tmp = root / "ws", root / "home", root / "config", root / "tmp"
    shutil.copytree(args.corpus, ws, symlinks=True, ignore=shutil.ignore_patterns(".code-graph"))
    for d in (home, cfg, tmp):
        d.mkdir(parents=True)
    (cfg / ".credentials.json").symlink_to(CREDENTIALS)
    env = {
        "HOME": str(home),
        "PATH": f"{tools}:/usr/bin:/bin",
        "CLAUDE_CONFIG_DIR": str(cfg),
        "TMPDIR": str(tmp),
        "TERM": "dumb",
        "LANG": "C.UTF-8",
        "USER": os.environ.get("USER", "eval"),
    }
    for k in ("HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "ALL_PROXY",
              "https_proxy", "http_proxy", "no_proxy", "all_proxy"):
        if k in os.environ:
            env[k] = os.environ[k]
    prompt, want = truth(args.truth)
    cmd = [
        "claude", "-p", prompt, "--model", args.model,
        "--max-budget-usd", f"{args.budget_now:.2f}",
        "--output-format", "stream-json", "--verbose",
        "--permission-mode", "dontAsk", "--tools", *TOOLS, "--allowedTools", *TOOLS,
    ]
    trace = root / "trace.jsonl"
    started = time.time()
    with open(trace, "w") as out:
        proc = subprocess.run(cmd, cwd=ws, env=env, stdin=subprocess.DEVNULL, stdout=out,
                              stderr=subprocess.PIPE, text=True, timeout=1800)
    text = trace.read_text()
    result = {}
    for line in text.splitlines():
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        if ev.get("type") == "result":
            result = ev
    reply = result.get("result") or ""
    got = answer_sites(reply)
    row = {
        "run": i, "rc": proc.returncode, "secs": round(time.time() - started),
        "cost": result.get("total_cost_usd", 0.0), "turns": result.get("num_turns"),
        "subtype": result.get("subtype"), "is_error": result.get("is_error", True),
        "used_code_graph": bool(USED_CG.search(text)),
        "answer": sorted(got) if got is not None else None,
    }
    if got is not None:
        hit = got & want
        row.update(recall=round(len(hit) / len(want), 3),
                   precision=round(len(hit) / len(got), 3) if got else 0.0,
                   missed=sorted(want - got), extra=sorted(got - want))
    (root / "reply.md").write_text(reply)
    shutil.rmtree(ws)  # 569 files per run; the trace and reply are what is kept
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--truth", type=Path, default=HERE / "truth.json")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--model", required=True)
    ap.add_argument("--max-total-usd", type=float, required=True)
    ap.add_argument("--per-session-usd", type=float, required=True)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    tools = args.out / "tools"
    tools.mkdir(exist_ok=True)
    for name in ("node", "claude"):
        link = tools / name
        if not link.exists():
            link.symlink_to(os.path.realpath(shutil.which(name)))
    costs = []
    results = args.out / "results.jsonl"
    for i in range(1, args.runs + 1):
        spent = sum(costs)
        estimate = 1.5 * sum(costs) / len(costs) if costs else args.per_session_usd
        left = args.max_total_usd - spent
        if estimate > left:
            print(f"stop before run {i}: spent ${spent:.2f}, estimate ${estimate:.2f} > left ${left:.2f}")
            break
        args.budget_now = min(args.per_session_usd, left)
        row = one_run(i, args, tools)
        costs.append(row["cost"])
        with open(results, "a") as f:
            f.write(json.dumps(row) + "\n")
        print(json.dumps({k: row.get(k) for k in
                          ("run", "cost", "turns", "subtype", "recall", "precision",
                           "missed", "extra", "used_code_graph")}))
    print(f"total ${sum(costs):.2f} over {len(costs)} run(s)")


if __name__ == "__main__":
    main()
