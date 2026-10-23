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

  symG        greps for nothing but names the project's index defines, over code
              inside the project, case-sensitive: lookups the index could have
              answered (by hand, 38 of 50 were). cover = cg / (cg + symG)
  empty       code-graph calls that returned no output (an unindexed scratch
              clone under `2>/dev/null` before 0.178); with --detail, the calls
              answered "No index found" are counted apart (cg_no_index)
  wt          in sessions whose cwd is a linked worktree (.claude/worktrees/),
              edits that got a pre-edit impact summary / all edits (D#560)

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
# 2026-10-23 audit (docs/plugin-usage-audit-2026-10-23.md): lookups the index
# could have answered, code-graph calls that came back empty, worktree edits.
GREP_HEADS = {"grep", "egrep", "fgrep", "rg", "ag", "ack"}
NONCODE_EXT = {".md", ".txt", ".json", ".jsonl", ".log", ".yaml", ".yml", ".toml", ".lock", ".csv", ".html",
               ".css", ".xml", ".ini", ".cfg", ".env", ".patch", ".diff"}
DECL = re.compile(r"(?:export\s+)?(?:default\s+)?(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?"
                  r"(?:def|fn|function|class|struct|enum|trait|interface|type|const|let|var|impl|func)\s+")
IDENT = re.compile(r"[A-Za-z_$][\w$]*(?:(?:\.|::)[A-Za-z_$][\w$]*)?")
NOT_SYMBOLS = {"module", "external_module", "h1", "h2", "h3", "h4", "h5", "h6"}
CG_EMPTY_OUTPUT = ("", "(Bash completed with no output)")
WORKTREE_MARK = "/.claude/worktrees/"
EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
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


def _alternatives(pattern):
    """A pattern's `|` / `\\|` alternatives, one group around them all
    unwrapped. Parentheses are not tracked otherwise: `(` is literal in a basic
    grep pattern (`fetch_rows(\\|Store`) and a group in an extended one."""
    m = re.fullmatch(r"\\?\((.*?)\\?\)", pattern)
    if m:
        pattern = m.group(1)
    parts, cur, i = [], [], 0
    while i < len(pattern):
        if pattern[i] == "\\" and i + 1 < len(pattern):
            if pattern[i + 1] == "|":
                parts.append("".join(cur))
                cur = []
            else:
                cur.append(pattern[i:i + 2])
            i += 2
            continue
        if pattern[i] == "|":
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(pattern[i])
        i += 1
    return parts + ["".join(cur)]


def symbol_names(pattern):
    """The names a grep pattern searches for when it is nothing but names (a
    definition keyword, word boundaries and a trailing `(` allowed), else None."""
    if not pattern or len(pattern) > 200:
        return None
    alts = _alternatives(pattern)
    if len(alts) > 6:
        return None
    names = []
    for a in alts:
        a = re.sub(r"\\[bB<>]|\^|\$$", "", a.strip())
        a = re.sub(r"\s*\\?\(\s*$", "", a.replace("\\s+", " ").replace("\\s*", " ").replace("\\s", " "))
        a = re.sub(r"^\\?\.", "", a).strip()
        m = DECL.match(a)
        if m:
            a = a[m.end():].strip()
        if len(a) < 3 or not IDENT.fullmatch(a):
            return None
        names.append(a)
    return names


def _stages_after_pipe(command):
    """(read from a pipe, stage) per `|` / `&&` / `||` / `;` / newline stage."""
    out, cur, quote, piped, i = [], [], None, False, 0
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
            out.append((piped, "".join(cur)))
            cur = []
            two = command[i:i + 2] in ("&&", "||")
            piped = c == "|" and not two
            i += two
        else:
            cur.append(c)
        i += 1
    out.append((piped, "".join(cur)))
    return out


def grep_search(name, inp):
    """{pattern, paths, globs, icase} for a call that searches files with
    grep/rg (the Grep tool included), else None. A grep reading a pipe or stdin
    filters output; it searches nothing."""
    inp = inp if isinstance(inp, dict) else {}
    if name == "Grep":
        return {"pattern": str(inp.get("pattern", "")), "paths": [str(inp["path"])] if inp.get("path") else [],
                "globs": [str(inp[k]) for k in ("glob", "type") if inp.get(k)], "icase": bool(inp.get("-i"))}
    if name != "Bash":
        return None
    for piped, stage in _stages_after_pipe(without_heredoc_bodies(str(inp.get("command", "")))):
        words = command_words(stage)
        if not words:
            continue
        head = os.path.basename(words[0])
        if head == "git" and "grep" in words[1:3]:
            words = words[words.index("grep"):]
        elif head not in GREP_HEADS:
            continue
        patterns, paths, flags, globs, i, operands = [], [], set(), [], 1, False
        while i < len(words):
            w = words[i]
            if operands or not w.startswith("-") or w == "-":
                (patterns if not patterns and "-e" not in flags else paths).append(w)
            elif w == "--":
                operands = True
            elif w in ("-e", "--regexp") and i + 1 < len(words):
                flags.add("-e")
                patterns.append(words[i + 1])
                i += 1
            elif w in ("-g", "--glob", "-t", "--type", "--include") and i + 1 < len(words):
                globs.append(words[i + 1])
                i += 1
            elif w.startswith(("--include=", "--glob=", "--type=")):
                globs.append(w.split("=", 1)[1])
            elif w in ("-A", "-B", "-C", "-m", "-f", "-d", "-D", "-M", "-j", "--max-count", "--max-depth"):
                i += 1
            elif w.startswith("--"):
                flags.add(w.split("=", 1)[0])
            else:
                flags.update("-" + ch for ch in w[1:])
            i += 1
        recursive = head in ("rg", "ag", "ack", "git") or bool(flags & {"-r", "-R", "--recursive"})
        if not paths and (piped or not recursive):
            continue
        return {"pattern": "|".join(patterns), "paths": paths, "globs": globs,
                "icase": bool(flags & {"-i", "--ignore-case"})}
    return None


def _code_target(search, root, cwd):
    """Whether a search looks at code inside ROOT."""
    if any(os.path.splitext(g.rstrip("*}").split(",")[0])[1].lower() in NONCODE_EXT
           or g in ("md", "markdown", "json", "txt", "yaml", "toml") for g in search["globs"]):
        return False
    if not search["paths"]:
        return True
    for p in search["paths"]:
        if p.startswith(("~", "$")):
            continue
        ap = os.path.normpath(os.path.join(cwd, p))
        inside = ap == root or ap.startswith(root + os.sep)
        if inside and os.path.splitext(ap)[1].lower() not in NONCODE_EXT \
                and not any(x in ap.split(os.sep) for x in ("node_modules", ".git", "target", ".code-graph")):
            return True
    return False


def index_root(cwd):
    """The nearest directory at or above CWD holding `.code-graph/index.db`
    (a worktree inside the project reaches the main checkout), below home."""
    home = os.path.expanduser("~")
    d = os.path.abspath(cwd or "/")
    while True:
        if os.path.isfile(os.path.join(d, ".code-graph", "index.db")):
            return d
        parent = os.path.dirname(d)
        if parent == d or d == home:
            return None
        d = parent


_NAMES = {}


def index_names(root):
    """Names the index at ROOT defines (headings and modules excluded). Opened
    immutable: no -wal/-shm is created beside the user's index."""
    if root not in _NAMES:
        import sqlite3
        names = set()
        try:
            c = sqlite3.connect(f"file:{os.path.join(root, '.code-graph', 'index.db')}?immutable=1", uri=True)
            try:
                for name, qualified, typ in c.execute("SELECT name, qualified_name, type FROM nodes"):
                    if typ not in NOT_SYMBOLS:
                        names.update(n for n in (name, qualified) if n)
            finally:
                c.close()
        except sqlite3.Error:
            pass
        _NAMES[root] = names
    return _NAMES[root]


def scan_transcript(path, window=10):
    """Per-session counts of one transcript (see the module docstring)."""
    s = {"path": path, "kind": "subagent" if os.sep + "subagents" + os.sep in path else "main",
         "cwd": None, "start": None, "entrypoint": None, "tools": 0,
         "cg_explicit": Counter(), "mem_explicit": Counter(), "cg_inject": Counter(),
         "mem_inject": Counter(), "cg_deny": 0, "cg_rewrite": 0,
         "pointers": 0, "pointers_followed": 0, "pointer_kinds": Counter(),
         "symbol_greps": 0, "cg_empty": 0, "cg_no_index": 0, "edits": 0, "worktree": False}
    events = []  # ("ptr", kind) | ("tool", is_cg), in transcript order
    cg_calls = set()  # tool_use ids of explicit code-graph calls
    searches = []  # grep_search() of every call that greps files

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
                        if not isinstance(block, dict) or block.get("type") != "tool_result":
                            continue
                        text = _result_text(block.get("content"))
                        if block.get("tool_use_id") in cg_calls:
                            # Empty: no output at all, e.g. an unindexed scratch clone under
                            # `2>/dev/null` before the no-index line reached stdout; counted
                            # apart from a no-index answer the model did see.
                            if "No index found" in text:
                                s["cg_no_index"] += 1
                            else:
                                s["cg_empty"] += text.strip() in CG_EMPTY_OUTPUT
                        # A code-graph command that ran and failed is not a deny.
                        # Nor is the no-index line of a dev build run as `cargo run -- show x`.
                        elif block.get("is_error") and CG_PREFIX in text[:300] and "No index found" not in text:
                            s["cg_deny"] += 1
            elif kind == "assistant":
                msg = rec.get("message")
                content = msg.get("content") if isinstance(msg, dict) else None
                for block in content if isinstance(content, list) else []:
                    if not isinstance(block, dict) or block.get("type") != "tool_use":
                        continue
                    s["tools"] += 1
                    name = str(block.get("name", ""))
                    hit = _explicit(name, block.get("input"))
                    if hit:
                        s[hit[0] + "_explicit"][hit[1]] += 1
                    events.append(("tool", bool(hit and hit[0] == "cg")))
                    if hit and hit[0] == "cg":
                        cg_calls.add(block.get("id"))
                    elif not hit:
                        search = grep_search(name, block.get("input"))
                        if search:
                            searches.append(search)
                    s["edits"] += name in EDIT_TOOLS

    # A lookup the index could have answered: a case-sensitive grep over code
    # inside the project for nothing but names the project's index defines. By
    # hand, 38 of 50 such greps were symbol lookups (audit 2026-10-23).
    root = index_root(s["cwd"]) if s["cwd"] else None
    if root:
        names = index_names(root)
        for search in searches:
            found = symbol_names(search["pattern"])
            if found and not search["icase"] and _code_target(search, root, s["cwd"]) \
                    and any(n in names or re.split(r"\.|::", n)[-1] in names for n in found):
                s["symbol_greps"] += 1
    s["worktree"] = WORKTREE_MARK in (s["cwd"] or "").replace(os.sep, "/")

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
                                  "symbol_greps": 0, "cg_empty": 0, "cg_no_index": 0, "edits": 0, "worktree_edits": 0,
                                  "worktree_edit_impact": 0,
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
        for k in ("cg_deny", "cg_rewrite", "pointers", "pointers_followed", "symbol_greps", "cg_empty", "cg_no_index",
                  "edits"):
            g[k] += s[k]
        if s["worktree"]:
            g["worktree_edits"] += s["edits"]
            g["worktree_edit_impact"] += s["cg_inject"].get("PreToolUse:Edit", 0)
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
        for k in ("cg_explicit", "mem_explicit", "cg_inject", "mem_inject", "cg_deny", "cg_rewrite",
                  "symbol_greps"):
            row[k + "_per_1k"] = round(1000 * g[k] / g["tools"], 1) if g["tools"] else None
        # Of the lookups the index could answer, the share code-graph answered.
        looked_up = g["cg_explicit"] + g["symbol_greps"]
        row["structural_coverage"] = round(g["cg_explicit"] / looked_up, 3) if looked_up else None
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
    print("per 1k tool calls; cg = code-graph, mem = claude-mem-lite; symG = greps for names the index "
          "defines, cover = cg / (cg + symG), empty = cg calls with no output, wt = worktree edits with "
          "an impact summary / worktree edits")
    print(f"{'period':<16} {'kind':<8} {'sess':>5} {'tools':>7} | {'cg':>6} {'w/cg':>7} {'deny':>5} {'rewr':>5}"
          f" {'inject':>6} {'ptr fol/all':>11} {'symG':>5} {'cover':>6} {'empty':>5} {'wt':>9}"
          f" | {'mem':>5} {'w/mem':>7} {'inject':>6}"
          + ("  project" if a.by_project else ""))
    for r in rows:
        cover = "-" if r["structural_coverage"] is None else f"{100 * r['structural_coverage']:.1f}%"
        print(f"{r['period'][:16]:<16} {r['kind']:<8} {r['sessions']:>5} {r['tools']:>7} | "
              f"{_rate(r['cg_explicit_per_1k']):>6} {r['sessions_with_cg']:>3}/{r['sessions']:<3} "
              f"{_rate(r['cg_deny_per_1k']):>5} {_rate(r['cg_rewrite_per_1k']):>5} {_rate(r['cg_inject_per_1k']):>6} "
              f"{r['pointers_followed']:>5}/{r['pointers']:<5} {_rate(r['symbol_greps_per_1k']):>5} {cover:>6} "
              f"{r['cg_empty']:>5} {r['worktree_edit_impact']:>4}/{r['worktree_edits']:<4} | "
              f"{_rate(r['mem_explicit_per_1k']):>5} "
              f"{r['sessions_with_mem']:>3}/{r['sessions']:<3} {_rate(r['mem_inject_per_1k']):>6}"
              + (f"  {r['project']}" if a.by_project else ""))
        if a.detail:
            for label, k in (("cg inject", "cg_inject_by_hook"), ("mem inject", "mem_inject_by_hook"),
                             ("cg explicit", "cg_explicit_by_channel"), ("pointers", "pointer_kinds")):
                if r[k]:
                    items = ", ".join(f"{h} {n}" for h, n in sorted(r[k].items(), key=lambda kv: -kv[1]))
                    print(f"    {label}: {items}")
            if r["cg_no_index"]:
                print(f"    cg no-index answers: {r['cg_no_index']}")
    if not rows:
        print("(no sessions matched)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
