"""With-plugin vs no-plugin on real daagu coding tasks (replayed commits), graded by the commit's tests.

    python3 run_coding.py --out DIR --rounds 2 --max-total-usd 25 --cap 3.0

Base snapshots come from setup.sh (DIR/base/<task>, DIR/base-indexed/<task>). Each run is a real
`claude -p` session (Opus 5.5) with its own HOME and CLAUDE_CONFIG_DIR (credentials linked in,
nothing else), in a fresh copy of the snapshot outside $HOME. Both arms get daagu's local
CLAUDE.md with the claude-mem-lite block removed; the no-plugin arm also loses the code-graph block,
the plugin arm keeps it plus .claude/plugin_code_graph_mcp.md, loads the installed 0.205.0 plugin
with --plugin-dir (staged under .claude/plugins/, tests removed), has the binary in the session
HOME's ~/.cache/code-graph/bin and starts on a prebuilt --no-embed index.

The two arms of one (round, task) run concurrently, so both see the same machine load. Before a
pair starts, the driver checks spent + 2 x 1.5 x running average (the cap before one exists)
against what is left. After each session: grade.py --full (the commit's test files overlaid,
then the full backend suite), a static caller check, the diff, then the workspace is removed.
results.jsonl is appended per run and read back on start, so a restarted driver skips finished runs.
"""

import argparse
import concurrent.futures as cf
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
DAAGU = Path("/home/ai/dev/daagu")
PLUGIN_SRC = Path("/home/ai/.claude/plugins/cache/code-graph-mcp/code-graph-mcp/0.205.0")
BINARY = Path.home() / ".cache" / "code-graph" / "bin" / "code-graph-mcp"
CREDENTIALS = Path.home() / ".claude" / ".credentials.json"
MODEL = "claude-opus-5-5"
TOOLS = ["Read", "Edit", "Write", "Grep", "Glob", "Bash"]
USED_CG = re.compile(r"code-graph-mcp\s+(callgraph|impact|refs|show|search|grep|overview|map|deps|ast-search|trace|affected)"
                     r"|mcp__plugin_code-graph")

TASKS = json.loads((HERE / "private" / "tasks.json").read_text())  # commit, prompt, unawaited (git-excluded)


def strip_block(text, name):
    return re.sub(rf"<!-- {name}:begin.*?<!-- {name}:end -->\n?", "", text, flags=re.S)


def claude_md(arm):
    text = strip_block((DAAGU / "CLAUDE.md").read_text(), "claude-mem-lite")
    return text if arm == "plugin" else strip_block(text, "code-graph-mcp")


def stage_plugin(out):
    plugin = out / "stage" / ".claude" / "plugins" / "code-graph-mcp"
    if not plugin.exists():
        shutil.copytree(PLUGIN_SRC, plugin, symlinks=True)
        for t in plugin.rglob("*.test.js"):
            t.unlink()
    return plugin


def count_injections(cfg):
    """Distinct code-graph hook outputs. One output is written as consecutive attachment records
    (hook_success, then hook_additional_context or hook_system_message): count a run of equal
    (event, first line) records once."""
    n, kinds = 0, {}
    for p in cfg.glob("projects/*/*.jsonl"):
        prev = None
        for line in p.read_text(errors="replace").splitlines():
            if '"attachment"' not in line:
                continue
            a = json.loads(line).get("attachment") or {}
            body = a.get("content") or a.get("stdout") or ""
            body = "\n".join(map(str, body)) if isinstance(body, list) else str(body)
            i = body.find("[code-graph")
            if i < 0:
                prev = None
                continue
            first = re.sub(r"\\n|\s+", " ", body[i:i + 120])[:60]
            key = (a.get("hookEvent") or a.get("hookName"), a.get("toolUseID") or first)
            if key == prev:
                continue
            prev = key
            n += 1
            m = re.match(r"\[code-graph(?::([\w-]+))?\]\s*(\S+ \S+ \S+)?", first)
            k = m.group(1) or ("stale-block" if "out-of-date" in first else (m.group(2) or "plain"))
            kinds[k] = kinds.get(k, 0) + 1
    return n, kinds


def static_check(task, ws):
    """Callers the hidden tests do not reach: every non-test call to a listed name must be awaited."""
    names = TASKS[task]["unawaited"]
    if not names:
        return None
    pat = re.compile(r"\.(%s)\(" % "|".join(map(re.escape, names)))
    bad = []
    for p in (ws / "backend").rglob("*.py"):
        if "/tests/" in str(p):
            continue
        for i, line in enumerate(p.read_text(errors="replace").splitlines(), 1):
            if pat.search(line) and "await" not in line:
                bad.append(f"{p.relative_to(ws)}:{i}")
    return {"unawaited_calls": bad}


def one_run(tag, task, arm, budget, args, tools):
    root = args.out / "runs" / tag
    if root.exists():
        shutil.rmtree(root)
    ws, home, cfg, tmp = root / "ws", root / "home", root / "config", root / "tmp"
    src = args.out / ("base-indexed" if arm == "plugin" else "base") / task
    shutil.copytree(src, ws, symlinks=True)
    (ws / "CLAUDE.md").write_text(claude_md(arm))
    if arm == "plugin":
        (ws / ".claude").mkdir(exist_ok=True)
        shutil.copy2(DAAGU / ".claude" / "plugin_code_graph_mcp.md", ws / ".claude" / "plugin_code_graph_mcp.md")
    for d in (home, cfg, tmp):
        d.mkdir(parents=True)
    (cfg / ".credentials.json").symlink_to(CREDENTIALS)
    env = {"HOME": str(home), "PATH": f"{tools}:/usr/bin:/bin", "CLAUDE_CONFIG_DIR": str(cfg),
           "TMPDIR": str(tmp), "TERM": "dumb", "LANG": "C.UTF-8", "USER": os.environ.get("USER", "eval"),
           "CODE_GRAPH_NO_AUTO_UPDATE": "1", "CODE_GRAPH_DISABLE_MODEL_DOWNLOAD": "1"}
    extra = []
    if arm == "plugin":
        cg_bin = home / ".cache" / "code-graph" / "bin"
        cg_bin.mkdir(parents=True)
        shutil.copy2(BINARY, cg_bin / "code-graph-mcp")
        extra = ["--plugin-dir", str(stage_plugin(args.out))]
    for k in ("HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "ALL_PROXY",
              "https_proxy", "http_proxy", "no_proxy", "all_proxy"):
        if k in os.environ:
            env[k] = os.environ[k]
    allowed = TOOLS + (["mcp__plugin_code-graph-mcp_code-graph__*"] if arm == "plugin" else [])
    cmd = ["claude", "-p", TASKS[task]["prompt"], "--model", MODEL, "--max-budget-usd", f"{budget:.2f}", *extra,
           "--output-format", "stream-json", "--verbose",
           "--permission-mode", "dontAsk", "--tools", *TOOLS, "--allowedTools", *allowed]
    trace = root / "trace.jsonl"
    started = time.time()
    with open(trace, "w") as out:
        proc = subprocess.run(cmd, cwd=ws, env=env, stdin=subprocess.DEVNULL, stdout=out,
                              stderr=subprocess.PIPE, text=True, timeout=3600)
    secs = round(time.time() - started)
    text = trace.read_text()
    result, init = {}, {}
    out_tokens = 0
    for line in text.splitlines():
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        if ev.get("type") == "result":
            result = ev
        elif ev.get("type") == "system" and ev.get("subtype") == "init":
            init = ev
    usage = result.get("usage") or {}
    inj, inj_kinds = count_injections(cfg)
    # diff against the snapshot commit, new files included, the index excluded
    subprocess.run(["git", "-C", str(ws), "add", "-A", "--", ".", ":!.code-graph", ":!CLAUDE.md", ":!.claude"],
                   capture_output=True)
    root_sha = subprocess.run(["git", "-C", str(ws), "rev-list", "--max-parents=0", "HEAD"],
                              capture_output=True, text=True).stdout.split()[0]
    diff = subprocess.run(["git", "-C", str(ws), "diff", "--cached", root_sha, "--stat", "-p", "--",
                           ".", ":!.code-graph", ":!CLAUDE.md", ":!.claude"], capture_output=True, text=True).stdout
    (root / "diff.patch").write_text(diff)
    static = static_check(task, ws)
    g = subprocess.run(["python3", str(HERE / "grade.py"), task, str(ws), "--full"], capture_output=True, text=True)
    try:
        grade = json.loads(g.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        grade = {"error": (g.stdout + g.stderr)[-500:]}
    (root / "reply.md").write_text(result.get("result") or "")
    row = {"tag": tag, "arm": arm, "task": task, "model": MODEL, "rc": proc.returncode, "secs": secs,
           "cost": result.get("total_cost_usd", 0.0), "turns": result.get("num_turns"),
           "subtype": result.get("subtype"), "is_error": result.get("is_error", True),
           "output_tokens": usage.get("output_tokens"),
           "used_code_graph": len(USED_CG.findall(text)), "injections": inj, "injection_kinds": inj_kinds,
           "init_model": init.get("model"), "mcp_servers": [s.get("name") for s in init.get("mcp_servers", [])],
           "stderr_tail": proc.stderr[-300:] if proc.returncode else "", "static": static,
           "hidden": grade.get("hidden"), "full_failed": (grade.get("full") or {}).get("failed"),
           "full_summary": (grade.get("full") or {}).get("summary"), "full_crash": (grade.get("full") or {}).get("crash"),
           "grade_error": grade.get("error"), "diff_lines": diff.count("\n")}
    if "passed" in str(row["full_summary"]) and not row["full_crash"]:
        shutil.rmtree(ws)  # kept for a regrade when the suite crashed or did not run
    return row


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--rounds", type=int, default=2)
    ap.add_argument("--tasks", nargs="+", default=["A", "B", "C"])
    ap.add_argument("--max-total-usd", type=float, required=True)
    ap.add_argument("--cap", type=float, default=3.0)
    args = ap.parse_args()
    tools = args.out / "tools"
    tools.mkdir(exist_ok=True)
    for name in ("node", "claude"):
        link = tools / name
        if not link.exists():
            link.symlink_to(os.path.realpath(shutil.which(name)))
    results = args.out / "results.jsonl"
    done = [json.loads(l) for l in results.read_text().splitlines()] if results.exists() else []
    for r in range(1, args.rounds + 1):
        for task in args.tasks:
            pair = [(f"r{r}-{arm}-{task}", task, arm) for arm in ("none", "plugin")]
            pair = [p for p in pair if not any(d["tag"] == p[0] for d in done)]
            if not pair:
                continue
            spent = sum(d["cost"] for d in done)
            avg = sum(d["cost"] for d in done) / len(done) if done else args.cap
            left = args.max_total_usd - spent
            if 1.5 * avg * len(pair) > left:
                print(f"stop before r{r}-{task}: spent ${spent:.2f}, left ${left:.2f}, avg ${avg:.2f}", flush=True)
                continue
            budget = min(args.cap, left / len(pair))
            with cf.ThreadPoolExecutor(len(pair)) as ex:
                futs = [ex.submit(one_run, tag, t, arm, budget, args, tools) for tag, t, arm in pair]
                rows = [f.result() for f in futs]
            for row in rows:
                done.append(row)
                with open(results, "a") as f:
                    f.write(json.dumps(row, ensure_ascii=False) + "\n")
                print(json.dumps({k: row.get(k) for k in ("tag", "cost", "turns", "secs", "subtype", "used_code_graph",
                                                          "injections", "hidden", "static", "full_summary")},
                                 ensure_ascii=False)[:900], flush=True)
    print(f"total ${sum(d['cost'] for d in done):.2f} over {len(done)} run(s)", flush=True)


if __name__ == "__main__":
    main()
