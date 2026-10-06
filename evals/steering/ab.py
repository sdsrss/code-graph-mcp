"""Compare steering channels in real `claude -p` sessions.

    python3 evals/steering/ab.py --variants none,rules --tags structural,control,coding \
        --runs 3 -j 3 --model claude-opus-5-5 --max-cost-usd 35

`claude plugin eval` loads no project instruction file (see evals/README.md,
"What an eval run cannot measure"), so a steering file can only be compared in
real sessions. Each run gets its own HOME, CLAUDE_CONFIG_DIR (credentials
linked in, nothing else: a default user with only this plugin) and workspace
outside $HOME, so no ancestor CLAUDE.md loads. The cases, prompts and graders
are the plugin-eval ones under evals/.

Variants place the block `code-graph-mcp adopt` writes, plus its detail doc,
before the session starts, git-excluded:
  none      nothing (0.164.0: MCP instructions + hooks)
  rules     .claude/rules/code-graph.md
  local     CLAUDE.local.md
  claudemd  CLAUDE.md

Every run is a paid session on your login. Starts are staggered and the run
refuses to begin unless the OAuth access token outlives it by a margin: two
processes refreshing one token at once can leave the stored login invalid.
Results: evals/results/<timestamp>-steering-ab/results.jsonl (+ traces).
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
EVALS = HERE.parent
REPO = EVALS.parent
sys.path.insert(0, str(EVALS / "_coding"))
import grade  # noqa: E402  (hidden-test runner and transcript reader)

FIXTURE_COMMIT = "6f0f6a2"  # evals/_fixture/scaffold.sh
CODING_CACHE = Path("/var/tmp/code-graph-eval/coding/fixtures")
WORK = Path(os.environ.get("CG_STEER_WORK_DIR", "/var/tmp/cg-steer"))
# Cases with `workspace: tokio` copy this (evals/steering/tokio/template.sh).
TOKIO = Path(os.environ.get("CG_STEER_TOKIO", WORK / "tokio" / "tokio"))
CREDENTIALS = Path.home() / ".claude" / ".credentials.json"
TARGETS = {
    "rules": ".claude/rules/code-graph.md",
    "local": "CLAUDE.local.md",
    "claudemd": "CLAUDE.md",
}
USED_CG = re.compile(
    r'"command":"[^"]*code-graph-mcp\s+(callgraph|impact|refs|show|search|grep|overview|map|tour|deps|dead-code|affected|ast-search)'
    r'|"name":"mcp__plugin_code-graph-mcp_code-graph__\w+"'
)


def frontmatter(path):
    """The `key: value` lines between the leading `---` fences, and the body."""
    text = path.read_text()
    _, head, body = text.split("---", 2)
    meta = {}
    for line in head.strip().splitlines():
        key, _, value = line.partition(":")
        value = value.strip()
        if value.startswith("'") and value.endswith("'"):
            value = value[1:-1].replace("''", "'")
        elif value.startswith('"') and value.endswith('"'):
            value = json.loads(value)
        elif value.startswith("[") and value.endswith("]"):
            value = [v.strip() for v in value[1:-1].split(",") if v.strip()]
        meta[key.strip()] = value
    return meta, body.strip()


def load_cases(tags, names, suite=EVALS):
    cases = []
    for prompt in sorted(suite.glob("*/prompt.md")):
        meta, body = frontmatter(prompt)
        case_tags = set(meta.get("tags") or [])
        if names and prompt.parent.name not in names:
            continue
        if not names and not case_tags & tags:
            continue
        graders = []
        for g in sorted((prompt.parent / "graders").glob("*.md")):
            gm, _ = frontmatter(g)
            if gm.get("type") != "regex":
                continue
            graders.append({
                "name": g.stem,
                "pattern": re.compile(gm["pattern"]),
                "target": gm.get("target", "last_message"),
                "scored": gm.get("arm") != "with-only",
                "negate": gm.get("match") == "not_contains",
            })
        cases.append({
            "name": prompt.parent.name,
            "prompt": body,
            "max_turns": int(meta.get("max_turns", 40)),
            "timeout": int(meta.get("timeout_seconds", 600)),
            "tools": meta.get("allowed_tools") or ["Read", "Glob", "Grep", "Bash"],
            "coding": "coding" in case_tags,
            "workspace": meta.get("workspace"),
            "graders": graders,
        })
    return cases


# --probe: does each variant's file reach the model, and is the plugin's CLI on
# PATH? Run it before a comparison; `sees-block` must pass exactly where a
# file is placed.
PROBE = {
    "name": "probe",
    "prompt": (
        "First, answer from the instructions you were given at the start of this session, "
        "before using any tool: is there a section headed \"Code Graph (repo-wide AST index)\" "
        "in them? Your reply starts with exactly one line `STEERING=yes` or `STEERING=no`; if yes, "
        "the next line quotes the first row of that section's table after the header row. Then run "
        "`ls -a . .claude .claude/rules; code-graph-mcp --version` with Bash and end your reply "
        "with its output under the heading FILES:."
    ),
    "max_turns": 4,
    "timeout": 180,
    "tools": ["Bash"],
    "coding": False,
    "graders": [
        {"name": "sees-block", "pattern": re.compile(r"STEERING=yes[\s\S]*Who calls X"),
         "target": "last_message", "scored": True, "negate": False},
        {"name": "cli-runs", "pattern": re.compile(r"code-graph-mcp \d+\.\d+\.\d+"),
         "target": "last_message", "scored": True, "negate": False},
    ],
}


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def stage(binary):
    """A plugin-mode copy of the plugin (as evals/run.sh stages one), and a tools
    dir holding only node and claude, so no installed code-graph-mcp leaks in."""
    plugin = WORK / "stage" / ".claude" / "plugins" / "code-graph-mcp"
    shutil.rmtree(plugin, ignore_errors=True)
    shutil.copytree(REPO / "claude-plugin", plugin, symlinks=True)
    for t in plugin.rglob("*.test.js"):
        t.unlink()
    tools = WORK / "tools"
    shutil.rmtree(tools, ignore_errors=True)
    tools.mkdir(parents=True)
    for name in ("node", "claude"):
        (tools / name).symlink_to(os.path.realpath(shutil.which(name)))
    return plugin, tools


def build_workspace(case, variant, root, plugin, binary):
    ws, home = root / "ws", root / "home"
    ws.mkdir(parents=True)
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin"}
    if case["coding"]:
        run(["tar", "-xf", str(CODING_CACHE / f"{case['name']}.tar"), "-C", str(ws)])
    elif case.get("workspace") == "tokio":
        # Already a one-commit repo, indexed as a user's project would be.
        if not (TOKIO / ".code-graph" / "index.db").exists():
            raise RuntimeError(f"no tokio template at {TOKIO}: run evals/steering/tokio/template.sh")
        shutil.copytree(TOKIO, ws, symlinks=True, dirs_exist_ok=True)
    else:
        archive = subprocess.run(["git", "-C", str(REPO), "archive", FIXTURE_COMMIT, "src"],
                                 check=True, capture_output=True).stdout
        run(["tar", "-x", "-C", str(ws)], input=archive)
    git = ["git", "-C", str(ws), "-c", "user.email=eval@example.invalid", "-c", "user.name=eval"]
    home.mkdir()
    if not (ws / ".git").exists():
        run(git[:3] + ["init", "-q"], env=env)
        run(git[:3] + ["add", "-A"], env=env)
        run(git + ["commit", "-qm", "fixture"], env=env)
    cg_bin = home / ".cache" / "code-graph" / "bin"
    cg_bin.mkdir(parents=True)
    shutil.copy2(binary, cg_bin / "code-graph-mcp")
    if variant != "none":
        target = TARGETS[variant]
        run([shutil.which("node"), "-e", """
const fs = require('fs'), path = require('path');
const a = require(process.argv[1]);
const [target] = process.argv.slice(2);
const detail = path.join('.claude', a.TARGET_NAME);
fs.mkdirSync(path.dirname(target), { recursive: true });
fs.writeFileSync(target, a.buildBlock(a.detectProjectType(process.cwd())) + '\\n');
fs.mkdirSync(path.dirname(detail), { recursive: true });
fs.writeFileSync(detail, Buffer.concat([Buffer.from(a.MANAGED_BY + '\\n'), fs.readFileSync(a.TEMPLATE_PATH)]));
fs.appendFileSync(path.join('.git', 'info', 'exclude'), `/${target}\\n/${detail}\\n`);
""", str(plugin / "scripts" / "adopt.js"), target], cwd=ws)
    cfg = root / "config"
    cfg.mkdir()
    (cfg / ".credentials.json").symlink_to(CREDENTIALS)
    (root / "tmp").mkdir()
    return ws, home, cfg


def session(case, variant, root, plugin, tools, model, ws, home, cfg):
    env = {
        "HOME": str(home),
        "PATH": f"{tools}:/usr/local/bin:/usr/bin:/bin",
        "CLAUDE_CONFIG_DIR": str(cfg),
        "TMPDIR": str(root / "tmp"),
        "TERM": "dumb",
        "LANG": "C.UTF-8",
        "USER": os.environ.get("USER", "eval"),
        # Both arms: no background download into the throwaway HOME.
        "CODE_GRAPH_NO_AUTO_UPDATE": "1",
    }
    # A shell that reaches the network through a proxy (a sandboxed one does)
    # has no DNS of its own: without these the session retries for minutes.
    for k in ("HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "ALL_PROXY",
              "https_proxy", "http_proxy", "no_proxy", "all_proxy"):
        if k in os.environ:
            env[k] = os.environ[k]
    cmd = [
        "claude", "-p", case["prompt"],
        "--plugin-dir", str(plugin),
        "--model", model,
        "--max-turns", str(case["max_turns"]),
        "--output-format", "stream-json", "--verbose",
        "--permission-mode", "dontAsk",
        "--tools", *case["tools"],
        "--allowedTools", *case["tools"], "mcp__plugin_code-graph-mcp_code-graph__*",
    ]
    trace = root / "trace.jsonl"
    started = time.time()
    with open(trace, "w") as out:
        try:
            # stdin closed: `claude -p` otherwise waits for an inherited pipe's EOF.
            proc = subprocess.run(cmd, cwd=ws, env=env, stdin=subprocess.DEVNULL, stdout=out,
                                  stderr=subprocess.PIPE, text=True, timeout=case["timeout"] + 120)
            rc, err = proc.returncode, proc.stderr[-2000:]
        except subprocess.TimeoutExpired:
            rc, err = "timeout", ""
    return trace, rc, err, time.time() - started


def grade_run(case, trace, ws, cfg):
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
    out = {
        "cost": result.get("total_cost_usd", 0.0),
        "turns": result.get("num_turns"),
        "is_error": result.get("is_error", True),
        "used_cg": bool(USED_CG.search(text)),
        "cg_calls": len(USED_CG.findall(text)),
    }
    if case["coding"]:
        if case["name"] == "code-dead-helpers":
            out["score"] = None  # graded below with the fixture, as grade.py does
            import tempfile
            with tempfile.TemporaryDirectory() as fx:
                run(["tar", "-xf", str(CODING_CACHE / f"{case['name']}.tar"), "-C", fx])
                proc = subprocess.run(
                    [sys.executable, str(grade.HERE / "hidden" / case["name"] / "dead_helpers.py"),
                     "grade", fx, str(ws)], capture_output=True, text=True, check=True)
            g = json.loads(proc.stdout)
            out["hidden"] = g
            out["score"] = g["recall"] if not g["false_positives"] else 0.0
        else:
            h = grade.run_pytest(ws, grade.HERE / "hidden" / case["name"], 600)
            total = sum(h.get(k, 0) for k in ("passed", "failed", "error", "errors"))
            out["hidden"] = h
            out["score"] = round(h.get("passed", 0) / total, 3) if total else 0.0
    else:
        graded = []
        for g in case["graders"]:
            hay = text if g["target"] == "trace" else reply
            ok = bool(g["pattern"].search(hay)) != g["negate"]
            if g["scored"]:
                graded.append(ok)
            out.setdefault("graders", {})[g["name"]] = ok
        out["score"] = round(sum(graded) / len(graded), 3) if graded else None
    tr = grade.main_transcript(cfg.parent)  # it looks under <run>/config/projects
    if tr:
        info = grade.read_transcript(tr, grade.case_prompts())
        out["main_cg_calls"] = info.get("cg_calls")  # main thread only
        out["hook_chars"] = info.get("cg_hook_chars")
    return out


def token_hours():
    oauth = json.loads(CREDENTIALS.read_text()).get("claudeAiOauth") or {}
    return (oauth.get("expiresAt", 0) / 1000 - time.time()) / 3600


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--variants", default="none,rules")
    ap.add_argument("--tags", default="structural")
    ap.add_argument("--case", action="append", default=[])
    ap.add_argument("--suite", type=Path, default=EVALS,
                    help="directory of case dirs (default evals/; the tokio cases: evals/steering/tokio/cases)")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("-j", type=int, default=3)
    ap.add_argument("--model", required=True)
    ap.add_argument("--max-cost-usd", type=float, required=True)
    ap.add_argument("--stagger", type=float, default=15.0)
    ap.add_argument("--min-token-hours", type=float, default=2.5)
    ap.add_argument("--keep", action="store_true", help="keep each run's directory")
    ap.add_argument("--probe", action="store_true", help="run only the delivery probe")
    args = ap.parse_args()

    variants = args.variants.split(",")
    for v in variants:
        if v != "none" and v not in TARGETS:
            sys.exit(f"unknown variant {v}")
    left = token_hours()
    if left < args.min_token_hours:
        sys.exit(f"OAuth access token expires in {left:.1f} h (< {args.min_token_hours}); "
                 "start a session to refresh it, then re-run")
    binary = Path(os.environ.get("CG_EVAL_BINARY", REPO / "target" / "release" / "code-graph-mcp"))
    want = json.loads((REPO / "claude-plugin" / ".claude-plugin" / "plugin.json").read_text())["version"]
    have = subprocess.run([str(binary), "--version"], capture_output=True, text=True).stdout.split()[1]
    if want != have:
        sys.exit(f"binary is {have}, plugin is {want}")
    cases = [PROBE] if args.probe else load_cases(set(args.tags.split(",")), set(args.case), args.suite)
    if not cases:
        sys.exit("no cases")
    plugin, tools = stage(binary)
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H-%M-%SZ")
    out_dir = EVALS / "results" / f"{stamp}-steering-ab"
    out_dir.mkdir(parents=True)
    # Interleaved: each case's variants run side by side, so drift in the model
    # or the service over the hour lands on every variant alike.
    jobs = [(r, c, v) for r in range(1, args.runs + 1) for c in cases for v in variants]
    print(f"{len(jobs)} sessions, {len(cases)} cases x {variants} x {args.runs}; token {left:.1f} h; -> {out_dir}",
          flush=True)
    spent = [0.0]
    lock = threading.Lock()
    last_start = [0.0]

    def one(idx_job):
        idx, (r, case, variant) = idx_job
        with lock:
            if spent[0] >= args.max_cost_usd:
                return None
            wait = last_start[0] + args.stagger - time.time()
            if wait > 0:
                time.sleep(wait)
            last_start[0] = time.time()
        root = WORK / "runs" / f"{stamp}-{idx:03d}-{case['name']}-{variant}-r{r}"
        rec = {"case": case["name"], "variant": variant, "run": r}
        try:
            ws, home, cfg = build_workspace(case, variant, root, plugin, binary)
            trace, rc, err, secs = session(case, variant, root, plugin, tools, args.model, ws, home, cfg)
            rec.update(rc=rc, seconds=round(secs, 1), stderr=err.strip()[-500:] if err else "")
            rec.update(grade_run(case, trace, ws, cfg))
            shutil.copy2(trace, out_dir / f"{idx:03d}-{case['name']}-{variant}-r{r}.jsonl")
        except Exception as e:  # recorded, not fatal to the other runs
            rec["error"] = repr(e)
        finally:
            if not args.keep:
                shutil.rmtree(root, ignore_errors=True)
        with lock:
            spent[0] += rec.get("cost") or 0.0
            with open(out_dir / "results.jsonl", "a") as f:
                f.write(json.dumps(rec) + "\n")
            print(f"[{idx + 1}/{len(jobs)}] {case['name']} {variant} r{r}: score={rec.get('score')} "
                  f"cg={rec.get('used_cg')} turns={rec.get('turns')} ${rec.get('cost') or 0:.3f} "
                  f"err={rec.get('error') or rec.get('is_error')} total=${spent[0]:.2f}", flush=True)
        return rec

    with ThreadPoolExecutor(max_workers=args.j) as pool:
        list(pool.map(one, enumerate(jobs)))
    print(f"done: ${spent[0]:.2f} -> {out_dir}/results.jsonl")


if __name__ == "__main__":
    main()
