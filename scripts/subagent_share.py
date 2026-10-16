#!/usr/bin/env python3
"""How much of a subagent's code search went through code-graph (roadmap P1 #3a).

Explore and Plan subagents skip CLAUDE.md, so the SubagentStart hook hands them
the index facts instead. Whether they then USE the index is only in their own
transcripts, which Claude Code keeps beside the parent session's:

    <projects dir>/<project slug>/<session id>/subagents/agent-<id>.jsonl
    <projects dir>/<project slug>/<session id>/subagents/agent-<id>.meta.json  ({"agentType": ...})

For each subagent whose working directory is inside PROJECT this counts its
code-search tool calls and splits them into code-graph calls (a Bash command
running `code-graph-mcp`, or a code-graph MCP tool) and the rest (Grep, Glob,
Read, and Bash commands running grep / rg / find / cat / head / tail / sed / ls).
Subagents are grouped by agent type and by whether a SubagentStart hook
delivered the index facts to them (a `hook_success` attachment whose output
carries "code-graph AST index").

    scripts/subagent_share.py [PROJECT] [--projects-dir DIR] [--since 2026-09-28T16:00] [--json]

Reads transcripts only; writes nothing. The glob is fixed-depth, never a
recursive walk of the projects dir.
"""
import argparse
import glob
import json
import os
import re
import shlex
import sys
from collections import defaultdict

SEARCH_TOOLS = {"Grep", "Glob", "Read"}
# Command word of a Bash stage (`|` / `&&` / `;` separated) that reads or
# searches code.
SEARCH_COMMANDS = {"grep", "rg", "egrep", "fgrep", "find", "fd", "cat", "head", "tail", "sed", "awk", "ls", "tree", "ag"}
# Words that run the next word as the command.
WRAPPERS = {"timeout", "time", "env", "npx", "nice", "command", "exec"}
# Words that open or continue a compound command; the command follows them.
KEYWORDS = {"if", "then", "elif", "else", "while", "until", "do", "!", "{", "eval"}
# `$(...)` without nesting: its body runs as a command of its own.
SUBSTITUTION = re.compile(r"\$\(([^()]*)\)")
HOOK_MARK = "code-graph AST index"


# `<<<` is a here-string (one word on stdin), not a heredoc.
HEREDOC = re.compile(r"(?<!<)<<(?!<)-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")


def without_heredoc_bodies(command):
    """The command with every heredoc body removed: those lines are data (a
    script being written, text piped to python), not commands."""
    out, end = [], None
    for line in command.split("\n"):
        if end is not None:
            if line.strip() == end:
                end = None
            continue
        out.append(line)
        m = HEREDOC.search(line)
        if m:
            end = m.group(2)
    return "\n".join(out)


def stages(command):
    """`|` / `&&` / `||` / `;` / newline separated stages, split outside quotes."""
    out, cur, quote, i = [], [], None, 0
    while i < len(command):
        c = command[i]
        if quote:
            if c == quote:
                quote = None
            elif c == "\\" and quote == '"' and i + 1 < len(command):
                cur.append(c)
                i += 1
                c = command[i]
            cur.append(c)
        elif c in "'\"":
            quote = c
            cur.append(c)
        elif c in "|;&\n":
            out.append("".join(cur))
            cur = []
            if command[i:i + 2] in ("&&", "||"):
                i += 1
        else:
            cur.append(c)
        i += 1
    out.append("".join(cur))
    return out


def command_words(stage):
    """A stage's words from its command word on: environment assignments and
    wrappers (with their options and a timeout's duration) are skipped.
    `command -v X` only looks X up, so it runs nothing."""
    try:
        words = shlex.split(stage)
    except ValueError:
        words = stage.strip().split()
    i = 0
    while i < len(words):
        if words[i].startswith("(") and not words[i].startswith("$("):
            # A subshell: `(cmd args)`.
            words[i] = words[i].lstrip("(")
            if not words[i]:
                i += 1
            continue
        if words[i] in KEYWORDS:
            i += 1
        elif re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[i]):
            i += 1
        elif os.path.basename(words[i]) in WRAPPERS:
            if words[i] == "command" and i + 1 < len(words) and words[i + 1] in ("-v", "-V"):
                return []
            i += 1
            while i < len(words) and (words[i].startswith("-") or re.match(r"^[0-9.]+[smhd]?$", words[i])):
                i += 1
        else:
            break
    return words[i:]


def git_subcommand(words):
    i = 1
    while i < len(words) and words[i].startswith("-"):
        i += 2 if words[i] in ("-C", "-c") else 1
    return words[i] if i < len(words) else None


def classify_bash(command):
    """'cg' when a stage runs code-graph-mcp, else 'search' when a stage runs a
    search command, else None. Only the command word counts: a path or a file
    named code-graph-mcp (`cd ~/code-graph-mcp`, `cargo run --bin
    code-graph-mcp`) is not a code-graph call."""
    kinds = set()
    command = without_heredoc_bodies(command)
    bodies = SUBSTITUTION.findall(command)
    for stage in stages(command) + [st for b in bodies for st in stages(b)]:
        words = command_words(stage)
        if not words:
            continue
        head = os.path.basename(words[0])
        if head == "code-graph-mcp" or words[0] == "@sdsrs/code-graph":
            return "cg"
        if head in SEARCH_COMMANDS or (head == "git" and git_subcommand(words) == "grep"):
            kinds.add("search")
    return "search" if kinds else None


def classify_tool(name, tool_input):
    if name == "Bash":
        return classify_bash(str((tool_input or {}).get("command", "")))
    if "code-graph" in name or "code_graph" in name:
        return "cg"
    if name in SEARCH_TOOLS:
        return "search"
    return None


def read_subagent(path):
    """(cwd, first timestamp, delivered, {'cg': n, 'search': n}) of one transcript."""
    cwd = first_ts = None
    delivered = False
    counts = {"cg": 0, "search": 0}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            cwd = cwd or rec.get("cwd")
            first_ts = first_ts or rec.get("timestamp")
            att = rec.get("attachment") or {}
            if att.get("hookEvent") == "SubagentStart" and HOOK_MARK in str(att.get("stdout", "")):
                delivered = True
            msg = rec.get("message") or {}
            if rec.get("type") != "assistant" or not isinstance(msg.get("content"), list):
                continue
            for block in msg["content"]:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    kind = classify_tool(block.get("name", ""), block.get("input"))
                    if kind:
                        counts[kind] += 1
    return cwd, first_ts, delivered, counts


def agent_type(jsonl_path):
    """The subagent's type from its .meta.json. A named teammate's `agentType`
    holds its NAME; its type is `customAgentType` when recorded, and is
    otherwise unknown, so all of them share one row."""
    meta = jsonl_path[: -len(".jsonl")] + ".meta.json"
    try:
        with open(meta, encoding="utf-8") as f:
            m = json.load(f)
    except (OSError, ValueError):
        return "unknown"
    if m.get("customAgentType"):
        return m["customAgentType"]
    if m.get("name") and m.get("agentType") == m.get("name"):
        return "(named teammate)"
    return m.get("agentType") or "unknown"


def collect(project, projects_dir, since=None):
    project = os.path.realpath(project)
    groups = defaultdict(lambda: {"subagents": 0, "used_cg": 0, "cg": 0, "search": 0})
    for path in sorted(glob.glob(os.path.join(projects_dir, "*", "*", "subagents", "agent-*.jsonl"))):
        cwd, ts, delivered, counts = read_subagent(path)
        if not cwd:
            continue
        cwd = os.path.realpath(cwd)
        if cwd != project and not cwd.startswith(project + os.sep):
            continue
        if since and (ts or "") < since:
            continue
        g = groups[(agent_type(path), delivered)]
        g["subagents"] += 1
        g["used_cg"] += counts["cg"] > 0
        g["cg"] += counts["cg"]
        g["search"] += counts["search"]
    return groups


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("project", nargs="?", default=".")
    ap.add_argument("--projects-dir", default=os.path.join(os.path.expanduser("~"), ".claude", "projects"))
    ap.add_argument("--since", help="ISO timestamp; subagents started earlier are skipped")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    groups = collect(a.project, a.projects_dir, a.since)
    rows = []
    for (atype, delivered), g in sorted(groups.items()):
        calls = g["cg"] + g["search"]
        rows.append({"agent_type": atype, "hook_delivered": delivered, **g,
                     "cg_share": round(g["cg"] / calls, 3) if calls else None})
    if a.json:
        json.dump(rows, sys.stdout, indent=1)
        print()
        return 0
    print(f"{'agent type':<24} {'hook':<5} {'subagents':>9} {'used cg':>8} {'cg calls':>9} {'search':>7} {'cg share':>9}")
    for r in rows:
        share = "-" if r["cg_share"] is None else f"{r['cg_share']:.1%}"
        print(f"{r['agent_type']:<24} {'yes' if r['hook_delivered'] else 'no':<5} {r['subagents']:>9} "
              f"{r['used_cg']:>8} {r['cg']:>9} {r['search']:>7} {share:>9}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
