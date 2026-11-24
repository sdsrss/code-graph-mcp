"""Headroom pilot round 2 (D#749): no-plugin baseline sessions over several cases and models.

    python3 run_pilot.py --corpus DIR --out DIR --cases private/truth-F.json private/truth-T1.json ... \
        --models claude-haiku-5-5 claude-sonnet-5-5 claude-opus-5-5 --rounds 2 --round2-models M ... \
        --max-total-usd 5.0 --cap claude-haiku-5-5=0.40 --cap claude-sonnet-5-5=0.70 --cap claude-opus-5-5=1.00

Round 1 runs every case x model once before round 2 repeats any (round 2 only for
--round2-models), so a budget stop still leaves one run of each. Each run is a real
`claude -p` session with its own HOME and CLAUDE_CONFIG_DIR (credentials linked in,
nothing else: no plugins, hooks, memory or user CLAUDE.md) and a fresh copy of the
corpus without `.code-graph/`, outside $HOME. PATH holds only node and claude plus
/usr/bin:/bin, so no code-graph binary is reachable. Sequential; before each run the
driver checks spent + 1.5 x that model's running average (its cap before it has
one) against what is left. results.jsonl is appended per run and read back on start,
so a restarted driver skips finished runs.

Truth file: {"case", "kind": "sites"|"callers", "prompt", "truth"}; sites are
"path:line", callers are [path, qualname]. Scoring:
  sites    an answer "path:line" matches a truth site with the same line and a path
           equal to it or one ending in the other (a dropped `backend/` prefix)
  callers  an answer "path:name" matches a truth caller with the same last name
           segment and a path that matches the same way
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path

CREDENTIALS = Path.home() / ".claude" / ".credentials.json"
TOOLS = ["Read", "Grep", "Glob", "Bash"]
SITE = re.compile(r"`?((?:[\w.-]+/)*[\w.-]+\.py)`?\s*:\s*(\d+)")
CALLER = re.compile(r"`?((?:[\w.-]+/)*[\w.-]+\.py)`?\s*(?::|::)\s*`?([A-Za-z_][\w.]*)")
# A code-graph command or MCP tool, not the string: a scratch path can contain "code-graph".
USED_CG = re.compile(r"code-graph-mcp\s+(callgraph|impact|refs|show|search|grep|overview|map|deps|ast-search)"
                     r"|mcp__plugin_code-graph")


def same_path(a, b):
    a, b = a.lstrip("./"), b.lstrip("./")
    return a == b or a.endswith("/" + b) or b.endswith("/" + a)


def section(reply, head):
    i = reply.rfind(head)
    return None if i < 0 else reply[i + len(head):]


def score(case, reply):
    if case["kind"] == "sites":
        body = section(reply, "CALL SITES")
        if body is None:
            return None
        got = {(p, int(n)) for p, n in SITE.findall(body)}
        truth = [(t.rsplit(":", 1)[0], int(t.rsplit(":", 1)[1])) for t in case["truth"]]
        match = lambda g, t: g[1] == t[1] and same_path(g[0], t[0])
    else:
        body = section(reply, "CALLERS")
        if body is None:
            return None
        got = {(p, q.rsplit(".", 1)[-1]) for p, q in CALLER.findall(body)}
        truth = [(p, q.rsplit(".", 1)[-1]) for p, q in case["truth"]]
        match = lambda g, t: g[1] == t[1] and same_path(g[0], t[0])
    hit_t = [t for t in truth if any(match(g, t) for g in got)]
    extra = sorted(f"{g[0]}:{g[1]}" for g in got if not any(match(g, t) for t in truth))
    missed = sorted(f"{t[0]}:{t[1]}" for t in truth if t not in hit_t)
    return {"recall": round(len(hit_t) / len(truth), 3),
            "precision": round((len(got) - len(extra)) / len(got), 3) if got else 0.0,
            "answered": len(got), "missed": missed, "extra": extra}


def one_run(tag, case, model, budget, args, tools):
    root = args.out / tag
    if root.exists():
        shutil.rmtree(root)
    ws, home, cfg, tmp = root / "ws", root / "home", root / "config", root / "tmp"
    shutil.copytree(args.corpus, ws, symlinks=True, ignore=shutil.ignore_patterns(".code-graph"))
    for d in (home, cfg, tmp):
        d.mkdir(parents=True)
    (cfg / ".credentials.json").symlink_to(CREDENTIALS)
    env = {"HOME": str(home), "PATH": f"{tools}:/usr/bin:/bin", "CLAUDE_CONFIG_DIR": str(cfg),
           "TMPDIR": str(tmp), "TERM": "dumb", "LANG": "C.UTF-8", "USER": os.environ.get("USER", "eval")}
    for k in ("HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "ALL_PROXY",
              "https_proxy", "http_proxy", "no_proxy", "all_proxy"):
        if k in os.environ:
            env[k] = os.environ[k]
    cmd = ["claude", "-p", case["prompt"], "--model", model, "--max-budget-usd", f"{budget:.2f}",
           "--output-format", "stream-json", "--verbose",
           "--permission-mode", "dontAsk", "--tools", *TOOLS, "--allowedTools", *TOOLS]
    trace = root / "trace.jsonl"
    started = time.time()
    with open(trace, "w") as out:
        proc = subprocess.run(cmd, cwd=ws, env=env, stdin=subprocess.DEVNULL, stdout=out,
                              stderr=subprocess.PIPE, text=True, timeout=1800)
    text = trace.read_text()
    result, init = {}, {}
    for line in text.splitlines():
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        if ev.get("type") == "result":
            result = ev
        elif ev.get("type") == "system" and ev.get("subtype") == "init":
            init = ev
    reply = result.get("result") or ""
    row = {"tag": tag, "case": case["case"], "model": model, "rc": proc.returncode,
           "secs": round(time.time() - started), "cost": result.get("total_cost_usd", 0.0),
           "turns": result.get("num_turns"), "subtype": result.get("subtype"),
           "is_error": result.get("is_error", True), "used_code_graph": bool(USED_CG.search(text)),
           "init_model": init.get("model"), "mcp_servers": [s.get("name") for s in init.get("mcp_servers", [])],
           "stderr_tail": proc.stderr[-300:] if proc.returncode else ""}
    s = score(case, reply)
    row.update(s if s else {"recall": None, "precision": None})
    (root / "reply.md").write_text(reply)
    shutil.rmtree(ws)  # ~1,900 files per run; the trace and reply are what is kept
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--cases", type=Path, nargs="+", required=True)
    ap.add_argument("--models", nargs="+", required=True)
    ap.add_argument("--rounds", type=int, default=1)
    ap.add_argument("--round2-models", nargs="*", default=None)
    ap.add_argument("--max-total-usd", type=float, required=True)
    ap.add_argument("--cap", action="append", default=[], help="MODEL=USD per-session cap")
    args = ap.parse_args()
    caps = dict((k, float(v)) for k, v in (c.split("=", 1) for c in args.cap))
    cases = [json.loads(p.read_text()) for p in args.cases]
    args.out.mkdir(parents=True, exist_ok=True)
    tools = args.out / "tools"
    tools.mkdir(exist_ok=True)
    for name in ("node", "claude"):
        link = tools / name
        if not link.exists():
            link.symlink_to(os.path.realpath(shutil.which(name)))
    results = args.out / "results.jsonl"
    done = [json.loads(l) for l in results.read_text().splitlines()] if results.exists() else []
    plan = []
    for r in range(1, args.rounds + 1):
        models = args.models if r == 1 or args.round2_models is None else args.round2_models
        plan += [(f"r{r}-{m.split('-')[1]}-{c['case']}", c, m) for m in models for c in cases]
    for tag, case, model in plan:
        if any(d["tag"] == tag for d in done):
            continue
        spent = sum(d["cost"] for d in done)
        mine = [d["cost"] for d in done if d["model"] == model]
        estimate = 1.5 * sum(mine) / len(mine) if mine else caps.get(model, 1.0)
        left = args.max_total_usd - spent
        if estimate > left:
            print(f"skip {tag}: spent ${spent:.2f}, estimate ${estimate:.2f} > left ${left:.2f}", flush=True)
            continue
        row = one_run(tag, case, model, min(caps.get(model, 1.0), left), args, tools)
        done.append(row)
        with open(results, "a") as f:
            f.write(json.dumps(row) + "\n")
        print(json.dumps({k: row.get(k) for k in ("tag", "cost", "turns", "subtype", "recall", "precision",
                                                   "answered", "used_code_graph", "init_model")}), flush=True)
    print(f"total ${sum(d['cost'] for d in done):.2f} over {len(done)} run(s)", flush=True)


if __name__ == "__main__":
    main()
