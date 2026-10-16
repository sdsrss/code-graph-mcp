#!/usr/bin/env python3
"""How code-graph is used in real Claude Code sessions, per 1,000 tool calls.

Hook and steering changes are judged on UX and side effects; whether the model
still reaches for code-graph afterwards is only in the transcripts Claude Code
keeps under <config dir>/projects/. Around 0.154.0 (deny -> rewrite) and
0.164.0 (hook de-noise), explicit use in `daagu` fell from 68.0 to 1.5 calls
per 1k tool calls, and no eval saw it (tasks/specs/plugin-usage-audit.md;
compare within a project: a total over projects mixes their shares). Per
session this counts:

  explicit    code-graph calls the model made: a Bash stage whose command word
              is code-graph-mcp (subagent_share.classify_bash), an MCP tool, a
              code-graph subagent or skill
  inject      hook context the model was given, by hook: `[code-graph...` text
              (or code-graph's SubagentStart index facts) in a
              hook_additional_context attachment, or the plain stdout of a
              UserPromptSubmit / SessionStart hook (JSON stdout is recorded as
              both attachments, so only the context row counts)
  deny        an error tool result whose text is a `[code-graph` reason, unless
              the call was itself a code-graph command that failed
  rewrite     a PreToolUse `updatedInput` whose reason or context is code-graph's
  pointers    one- or two-line injections that name a code-graph command or
              MCP tool (the SubagentStart facts among them), and how many were
              followed by a code-graph call within the next --window tool calls

and the same explicit / inject counts for claude-mem-lite as a reference arm.
Counts are not outcomes: a session can use code-graph more and finish no
better. Pair a change in these numbers with an outcome eval (evals/).

    scripts/plugin_usage.py [--projects-dir DIR] [--since T] [--until T]
        [--split T[=LABEL]]... [--project PATH]... [--exclude SUBSTR]...
        [--all-entrypoints] [--by-project] [--detail] [--window N] [--json]

Sessions are interactive ones (entrypoint "cli") unless --all-entrypoints:
`claude -p` eval runs would otherwise swamp the numbers. Reads transcripts
only; writes nothing. Globs are fixed-depth, never a recursive walk of the
config dir. Tests: python3 -m unittest scripts/test_plugin_usage.py
"""
import argparse
import glob
import json
import os
import re
import sys
from collections import Counter, defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from subagent_share import classify_bash, command_words, stages, without_heredoc_bodies  # noqa: E402

CG_PREFIX = "[code-graph"
# code-graph's SubagentStart context opens with prose, not the prefix
# (subagent_share.HOOK_MARK).
CG_SUBAGENT_MARK = "code-graph AST index"
MEM_PREFIXES = ("[mem", "[claude-mem-lite")
CG_MCP = "mcp__plugin_code-graph"
MEM_MCP = "mcp__plugin_claude-mem"
MEM_CLI_SCRIPTS = {"cli.mjs", "mem-cli.mjs"}
# Hook events whose plain stdout Claude Code adds to the model's context.
PLAIN_STDOUT_EVENTS = {"UserPromptSubmit", "SessionStart"}
MCP_TOOL_NAMES = ("semantic_code_search", "module_overview", "get_call_graph", "find_references",
                  "get_ast_node", "project_map", "ast_search")
POINTER = re.compile(r"code-graph-mcp\s+([a-z][a-z-]*)|\b(" + "|".join(MCP_TOOL_NAMES) + r")\b")


def default_projects_dir():
    base = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.join(os.path.expanduser("~"), ".claude")
    return os.path.join(base, "projects")


def is_mem_bash(command):
    """True when a stage runs claude-mem-lite: the command word itself, or
    `node <...claude-mem-lite...>/cli.mjs`. A path or message naming the
    plugin is not a call."""
    for stage in stages(without_heredoc_bodies(command)):
        words = command_words(stage)
        if not words:
            continue
        head = os.path.basename(words[0])
        if head == "claude-mem-lite":
            return True
        if head == "node" and len(words) > 1 and "claude-mem-lite" in words[1] \
                and os.path.basename(words[1]) in MEM_CLI_SCRIPTS:
            return True
    return False


def pointer_verb(text):
    """The code-graph command or MCP tool a bare suggestion tells the model to
    run, or None. Only a one- or two-line injection counts: a longer one is an
    answer that may mention a command (a truncated map's tail, the rewrite
    notice describing the command that already ran), not a request to act."""
    if sum(1 for line in text.splitlines() if line.strip()) > 2:
        return None
    m = POINTER.search(text)
    return (m.group(1) or m.group(2)) if m else None


def _source(text, event=None):
    s = text.lstrip()
    if s.startswith(CG_PREFIX) or (event == "SubagentStart" and CG_SUBAGENT_MARK in s):
        return "cg"
    if s.startswith(MEM_PREFIXES):
        return "mem"
    return None


def _json(text):
    try:
        v = json.loads(text)
    except (TypeError, ValueError):
        return None
    return v if isinstance(v, dict) else None


def _result_text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(c.get("text", "") for c in content if isinstance(c, dict))
    return ""


def _explicit(name, inp):
    """('cg' | 'mem', channel) for a tool call that uses a plugin, else None."""
    inp = inp if isinstance(inp, dict) else {}
    if name.startswith(CG_MCP):
        return "cg", "mcp"
    if name.startswith(MEM_MCP):
        return "mem", "mcp"
    if name == "Bash":
        cmd = str(inp.get("command", ""))
        if classify_bash(cmd) == "cg":
            return "cg", "bash"
        if is_mem_bash(cmd):
            return "mem", "bash"
    elif name in ("Agent", "Task") and str(inp.get("subagent_type", "")).startswith("code-graph"):
        return "cg", "agent"
    elif name == "Skill":
        skill = str(inp.get("skill", ""))
        if skill.startswith("code-graph"):
            return "cg", "skill"
        if skill.startswith("claude-mem-lite"):
            return "mem", "skill"
    return None


def scan_transcript(path, window=10):
    """Per-session counts of one transcript (see the module docstring)."""
    s = {"path": path, "kind": "subagent" if os.sep + "subagents" + os.sep in path else "main",
         "cwd": None, "start": None, "entrypoint": None, "tools": 0,
         "cg_explicit": Counter(), "mem_explicit": Counter(), "cg_inject": Counter(),
         "mem_inject": Counter(), "cg_deny": 0, "cg_rewrite": 0,
         "pointers": 0, "pointers_followed": 0, "pointer_kinds": Counter()}
    events = []  # ("ptr", kind) | ("tool", is_cg), in transcript order
    cg_calls = set()  # tool_use ids of explicit code-graph calls

    def inject(src, hook):
        s["cg_inject" if src == "cg" else "mem_inject"][hook or "?"] += 1

    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if not isinstance(rec, dict):
                continue
            s["start"] = s["start"] or rec.get("timestamp")
            s["cwd"] = s["cwd"] or rec.get("cwd")
            s["entrypoint"] = s["entrypoint"] or rec.get("entrypoint")
            kind = rec.get("type")
            if kind == "attachment":
                att = rec.get("attachment")
                if not isinstance(att, dict):
                    continue
                hook = att.get("hookName")
                if att.get("type") == "hook_additional_context":
                    texts = att.get("content") or []
                    for text in [texts] if isinstance(texts, str) else texts:
                        src = _source(str(text), att.get("hookEvent"))
                        if src:
                            inject(src, hook)
                        verb = pointer_verb(str(text)) if src == "cg" else None
                        if verb:
                            events.append(("ptr", f"{hook} {verb}"))
                elif att.get("type") == "hook_success":
                    out = str(att.get("stdout") or "")
                    doc = _json(out.strip())
                    if doc is None:
                        src = _source(out)
                        if src and att.get("hookEvent") in PLAIN_STDOUT_EVENTS:
                            inject(src, hook)
                            verb = pointer_verb(out) if src == "cg" else None
                            if verb:
                                events.append(("ptr", f"{hook} {verb}"))
                    elif att.get("hookEvent") == "PreToolUse":
                        hso = doc.get("hookSpecificOutput")
                        if not isinstance(hso, dict):
                            continue
                        why = str(hso.get("permissionDecisionReason") or hso.get("additionalContext") or "")
                        if hso.get("updatedInput") and _source(why) == "cg":
                            s["cg_rewrite"] += 1
            elif kind == "user":
                msg = rec.get("message")
                content = msg.get("content") if isinstance(msg, dict) else None
                if isinstance(content, list):
                    for block in content:
                        # A code-graph command that ran and failed is not a deny.
                        if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("is_error") \
                                and block.get("tool_use_id") not in cg_calls \
                                and CG_PREFIX in _result_text(block.get("content"))[:300]:
                            s["cg_deny"] += 1
            elif kind == "assistant":
                msg = rec.get("message")
                content = msg.get("content") if isinstance(msg, dict) else None
                for block in content if isinstance(content, list) else []:
                    if not isinstance(block, dict) or block.get("type") != "tool_use":
                        continue
                    s["tools"] += 1
                    hit = _explicit(str(block.get("name", "")), block.get("input"))
                    if hit:
                        s[hit[0] + "_explicit"][hit[1]] += 1
                    events.append(("tool", bool(hit and hit[0] == "cg")))
                    if hit and hit[0] == "cg":
                        cg_calls.add(block.get("id"))

    for i, (what, val) in enumerate(events):
        if what != "ptr":
            continue
        s["pointers"] += 1
        s["pointer_kinds"][val] += 1
        nxt = [v for w, v in events[i + 1:] if w == "tool"][:window]
        s["pointers_followed"] += any(nxt)
    for k in ("cg_explicit", "mem_explicit", "cg_inject", "mem_inject", "pointer_kinds"):
        s[k] = dict(s[k])
    return s


def _inside(cwd, root):
    cwd, root = os.path.realpath(cwd), os.path.realpath(root)
    return cwd == root or cwd.startswith(root + os.sep)


def collect(projects_dir, since=None, until=None, entrypoints=("cli",), projects=None,
            exclude=None, window=10):
    """Scanned sessions under PROJECTS_DIR (main transcripts and their
    subagents), filtered by start time [since, until), entrypoint, project
    (cwd inside one of PROJECTS) and cwd substrings to EXCLUDE."""
    paths = sorted(glob.glob(os.path.join(projects_dir, "*", "*.jsonl"))
                   + glob.glob(os.path.join(projects_dir, "*", "*", "subagents", "agent-*.jsonl")))
    out = []
    for p in paths:
        try:
            s = scan_transcript(p, window)
        except OSError:
            continue  # deleted since the glob (another session's throwaway project)
        if not s["start"] or not s["cwd"]:
            continue
        if since and s["start"] < since:
            continue
        if until and s["start"] >= until:
            continue
        if entrypoints and s["entrypoint"] not in entrypoints:
            continue
        if projects and not any(_inside(s["cwd"], p) for p in projects):
            continue
        if exclude and any(x in s["cwd"] for x in exclude):
            continue
        out.append(s)
    return out


def _period(start, splits):
    label = f"before {splits[0][1]}" if splits else "all"
    for ts, name in splits:
        if start >= ts:
            label = name
    return label


def aggregate(sessions, splits=(), by_project=False):
    """One row per (period, main|subagent[, project]) with totals and rates
    per 1,000 tool calls. SPLITS: [(timestamp, label)], sorted here."""
    splits = sorted(splits)
    groups = defaultdict(lambda: {"sessions": 0, "tools": 0, "cg_explicit": 0, "sessions_with_cg": 0,
                                  "mem_explicit": 0, "sessions_with_mem": 0, "cg_inject": 0, "mem_inject": 0,
                                  "cg_deny": 0, "cg_rewrite": 0, "pointers": 0, "pointers_followed": 0,
                                  "cg_inject_by_hook": Counter(), "mem_inject_by_hook": Counter(),
                                  "cg_explicit_by_channel": Counter(), "pointer_kinds": Counter()})
    order = {name: i for i, (_, name) in enumerate(splits)}
    for s in sessions:
        key = (_period(s["start"], splits), s["kind"]) + ((os.path.realpath(s["cwd"]),) if by_project else ())
        g = groups[key]
        g["sessions"] += 1
        g["tools"] += s["tools"]
        cg, mem = sum(s["cg_explicit"].values()), sum(s["mem_explicit"].values())
        g["cg_explicit"] += cg
        g["sessions_with_cg"] += cg > 0
        g["mem_explicit"] += mem
        g["sessions_with_mem"] += mem > 0
        g["cg_inject"] += sum(s["cg_inject"].values())
        g["mem_inject"] += sum(s["mem_inject"].values())
        for k in ("cg_deny", "cg_rewrite", "pointers", "pointers_followed"):
            g[k] += s[k]
        g["cg_inject_by_hook"].update(s["cg_inject"])
        g["mem_inject_by_hook"].update(s["mem_inject"])
        g["cg_explicit_by_channel"].update(s["cg_explicit"])
        g["pointer_kinds"].update(s["pointer_kinds"])
    rows = []
    for key in sorted(groups, key=lambda k: (-1 if k[0].startswith("before ") else order.get(k[0], 0),) + k):
        g = groups[key]
        row = {"period": key[0], "kind": key[1]}
        if by_project:
            row["project"] = key[2]
        row.update({k: (dict(v) if isinstance(v, Counter) else v) for k, v in g.items()})
        for k in ("cg_explicit", "mem_explicit", "cg_inject", "mem_inject", "cg_deny", "cg_rewrite"):
            row[k + "_per_1k"] = round(1000 * g[k] / g["tools"], 1) if g["tools"] else None
        rows.append(row)
    return rows


def _positive_int(text):
    n = int(text)
    if n < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return n


def _split_arg(text):
    ts, _, label = text.partition("=")
    return ts, label or ts


def _rate(v):
    return "-" if v is None else f"{v:.1f}"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--projects-dir", default=default_projects_dir())
    ap.add_argument("--since", help="ISO timestamp; sessions started earlier are skipped")
    ap.add_argument("--until", help="ISO timestamp; sessions started at or after it are skipped")
    ap.add_argument("--split", action="append", default=[], type=_split_arg, metavar="T[=LABEL]",
                    help="start a new period at T (repeatable), e.g. 2026-09-25T11:00=0.154")
    ap.add_argument("--project", action="append", default=[], metavar="PATH",
                    help="only sessions whose cwd is inside PATH (repeatable)")
    ap.add_argument("--exclude", action="append", default=[], metavar="SUBSTR",
                    help="skip sessions whose cwd contains SUBSTR (repeatable)")
    ap.add_argument("--all-entrypoints", action="store_true", help="include claude -p / SDK sessions")
    ap.add_argument("--by-project", action="store_true")
    ap.add_argument("--detail", action="store_true", help="per-hook injections and pointer kinds")
    ap.add_argument("--window", type=_positive_int, default=10, help="tool calls a pointer is followed within (10)")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    sessions = collect(a.projects_dir, a.since, a.until, None if a.all_entrypoints else ("cli",),
                       a.project or None, a.exclude or None, a.window)
    rows = aggregate(sessions, a.split, a.by_project)
    if a.json:
        json.dump(rows, sys.stdout, indent=1, ensure_ascii=False)
        print()
        return 0
    print("per 1k tool calls; cg = code-graph, mem = claude-mem-lite")
    print(f"{'period':<16} {'kind':<8} {'sess':>5} {'tools':>7} | {'cg':>6} {'w/cg':>7} {'deny':>5} {'rewr':>5}"
          f" {'inject':>6} {'ptr fol/all':>11} | {'mem':>5} {'w/mem':>7} {'inject':>6}"
          + ("  project" if a.by_project else ""))
    for r in rows:
        print(f"{r['period'][:16]:<16} {r['kind']:<8} {r['sessions']:>5} {r['tools']:>7} | "
              f"{_rate(r['cg_explicit_per_1k']):>6} {r['sessions_with_cg']:>3}/{r['sessions']:<3} "
              f"{_rate(r['cg_deny_per_1k']):>5} {_rate(r['cg_rewrite_per_1k']):>5} {_rate(r['cg_inject_per_1k']):>6} "
              f"{r['pointers_followed']:>5}/{r['pointers']:<5} | {_rate(r['mem_explicit_per_1k']):>5} "
              f"{r['sessions_with_mem']:>3}/{r['sessions']:<3} {_rate(r['mem_inject_per_1k']):>6}"
              + (f"  {r['project']}" if a.by_project else ""))
        if a.detail:
            for label, k in (("cg inject", "cg_inject_by_hook"), ("mem inject", "mem_inject_by_hook"),
                             ("cg explicit", "cg_explicit_by_channel"), ("pointers", "pointer_kinds")):
                if r[k]:
                    items = ", ".join(f"{h} {n}" for h, n in sorted(r[k].items(), key=lambda kv: -kv[1]))
                    print(f"    {label}: {items}")
    if not rows:
        print("(no sessions matched)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
