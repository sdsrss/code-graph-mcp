#!/usr/bin/env python3
"""Deep usage audit of code-graph in real Claude Code sessions (2026-10-23).

scripts/plugin_usage.py counts calls and injections. This goes one step
further, per tool call, to answer four questions:

  1. trend        explicit code-graph calls per 1k tool calls, by era and week
  2. opportunity  "structural lookups": a grep whose pattern is only symbol
                  names the project's index defines, over code inside the
                  project. Coverage = cg structural calls / (those + greps).
                  Plus signature edits: was the name searched before / after?
  3. efficiency   proxies only (no outcome data): navigation-run length after a
                  lookup, result size, latency, cg failures and fallbacks
  4. interference injected characters per plugin, pointer follow-through,
                  hook latency (only hooks that printed something are logged)

Reads transcripts and each project's .code-graph/index.db (immutable=1, so no
-wal/-shm is created). Writes one JSON file with --out. Interactive sessions
only (entrypoint "cli").
"""
import argparse
import collections
import glob
import json
import os
import re
import shlex
import sqlite3
import statistics
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(REPO, "scripts"))
from plugin_usage import _explicit, _json, pointer_verb  # noqa: E402
from subagent_share import command_words, without_heredoc_bodies  # noqa: E402

ROOTS = {
    "/home/ai/dev/daagu": "daagu",
    "/home/ai/dev/gsd-lite": "gsd-lite",
    "/home/ai/dev/claudemd": "claudemd",
    "/home/ai/dev/loop-eng": "loop-eng",
    "/home/ai/dev/code-graph-mcp": "code-graph-mcp",
    "/home/ai/dev/claude-mem-lite": "claude-mem-lite",
}
OWN = {"code-graph-mcp", "claude-mem-lite"}
ERAS = [("A", ""), ("B", "2026-09-25T11:10"), ("C", "2026-09-29T23:32"), ("D", "2026-10-16T12:01")]
CODE_EXT = {".py", ".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx", ".vue", ".rs", ".go", ".java", ".c", ".h",
            ".cc", ".cpp", ".hpp", ".rb", ".php", ".sh", ".bash", ".kt", ".swift", ".cs", ".scala", ".sql"}
NONCODE_EXT = {".md", ".txt", ".json", ".jsonl", ".log", ".yaml", ".yml", ".toml", ".lock", ".csv", ".html",
               ".css", ".xml", ".ini", ".cfg", ".env", ".patch", ".diff"}
GREP_HEADS = {"grep", "egrep", "fgrep", "rg", "ag", "ack"}
NAV_BASH = {"cat", "head", "tail", "sed", "awk", "ls", "find", "fd", "tree", "wc", "nl", "less"}
DECL = re.compile(
    r"(?:(?:export\s+)?(?:default\s+)?(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?"
    r"(?:def|fn|function|class|struct|enum|trait|interface|type|const|let|var|impl|func)\s+)")
IDENT = re.compile(r"[A-Za-z_$][\w$]*(?:(?:\.|::)[A-Za-z_$][\w$]*)?")
# grep/rg options that take a separate argument
OPT_ARG = {"-e", "-f", "-A", "-B", "-C", "-m", "-d", "-D", "-g", "-t", "-T", "-M", "-j", "--type", "--glob",
           "--max-count", "--max-depth", "--include", "--exclude", "--exclude-dir", "--type-not", "--regexp",
           "--context", "--after-context", "--before-context", "--file"}
DEF_TYPES_SKIP = {"module", "external_module", "h1", "h2", "h3", "h4", "h5", "h6"}
CG_STRUCT = {"callgraph", "refs", "impact", "show", "overview", "map", "affected", "deps", "trace", "search",
             "grep", "ast-search", "dead-code", "similar", "centrality", "get_call_graph", "find_references",
             "get_ast_node", "module_overview", "project_map", "semantic_code_search", "ast_search"}
EMPTY_HINTS = re.compile(r"no (?:callers|references|matches|results|symbol|definition|node)|not found|"
                         r"0 (?:results|matches|callers|references)|no call graph|did you mean|"
                         r"index is (?:building|being built)|not indexed", re.I)


def era_of(ts):
    e = "A"
    for name, start in ERAS[1:]:
        if ts >= start:
            e = name
    return e


def week_of(ts):
    import datetime
    d = datetime.date.fromisoformat(ts[:10])
    iso = d.isocalendar()
    return f"{iso[0]}-W{iso[1]:02d}"


def root_of(cwd):
    for r, name in ROOTS.items():
        if cwd == r or cwd.startswith(r + "/"):
            return r, name
    return None, None


_NAMES = {}


def index_names(root):
    if root in _NAMES:
        return _NAMES[root]
    names = set()
    db = os.path.join(root, ".code-graph", "index.db")
    if os.path.exists(db):
        c = sqlite3.connect(f"file:{db}?immutable=1", uri=True)
        for name, qn, typ in c.execute("SELECT name, qualified_name, type FROM nodes"):
            if typ in DEF_TYPES_SKIP:
                continue
            names.add(name)
            if qn:
                names.add(qn)
        c.close()
    _NAMES[root] = names
    return names


def split_stages(command):
    """(separator before the stage, stage text), split outside quotes."""
    out, cur, quote, i, sep = [], [], None, 0, ""
    while i < len(command):
        ch = command[i]
        if quote:
            if ch == quote:
                quote = None
            elif ch == "\\" and quote == '"' and i + 1 < len(command):
                cur.append(ch)
                i += 1
                ch = command[i]
            cur.append(ch)
        elif ch in "'\"":
            quote = ch
            cur.append(ch)
        elif ch in "|;&\n":
            out.append((sep, "".join(cur)))
            cur = []
            two = command[i:i + 2]
            if two in ("&&", "||"):
                sep = two
                i += 1
            else:
                sep = ch
        else:
            cur.append(ch)
        i += 1
    out.append((sep, "".join(cur)))
    return [(s, t) for s, t in out if t.strip()]


def split_alternation(pat):
    parts, depth, cur, i = [], 0, [], 0
    while i < len(pat):
        ch = pat[i]
        if ch == "\\" and i + 1 < len(pat):
            if pat[i + 1] == "|" and depth == 0:
                parts.append("".join(cur))
                cur = []
                i += 2
                continue
            cur.append(pat[i:i + 2])
            i += 2
            continue
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        if ch == "|" and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
        i += 1
    parts.append("".join(cur))
    return parts


def symbol_idents(pat):
    """(identifiers, is_declaration_lookup) when the pattern is nothing but
    symbol names (optionally with a declaration keyword, word boundaries, a
    trailing `(`), else None."""
    if not pat or len(pat) > 200:
        return None
    alts = split_alternation(pat)
    if len(alts) > 6:
        return None
    ids, decl = [], False
    for a in alts:
        a = a.strip()
        # a parenthesised alternation group of plain names: (foo|bar)
        m = re.fullmatch(r"\\?\(([\w$|]+)\\?\)", a)
        if m and "|" in m.group(1):
            for sub in m.group(1).split("|"):
                if IDENT.fullmatch(sub):
                    ids.append(sub)
                else:
                    return None
            continue
        a = re.sub(r"\\[bB<>]|\^|\$$", "", a)
        a = a.replace("\\s+", " ").replace("\\s*", " ").replace("\\s", " ").replace("[[:space:]]+", " ")
        a = re.sub(r"\s*\\?\(\s*$", "", a)
        a = re.sub(r"^\\?\.", "", a).strip()
        m = DECL.match(a)
        if m:
            decl = True
            a = a[m.end():].strip()
        if not IDENT.fullmatch(a) or len(a) < 3:
            return None
        ids.append(a)
    return (ids, decl) if ids else None


def parse_grep(words):
    """(pattern, paths, flags) for a grep/rg word list (command word first)."""
    head = os.path.basename(words[0])
    args = words[1:]
    if head == "git":
        args = args[args.index("grep") + 1:] if "grep" in args else []
    pats, paths, flags, i, positional_only = [], [], set(), 0, False
    while i < len(args):
        w = args[i]
        if not positional_only and w == "--":
            positional_only = True
        elif not positional_only and w.startswith("--"):
            k, _, v = w.partition("=")
            if k in ("--regexp",):
                pats.append(v or (args[i + 1] if i + 1 < len(args) else ""))
                i += 0 if v else 1
            elif k in ("--include", "--glob", "--type") and (v or i + 1 < len(args)):
                flags.add((k, v or args[i + 1]))
                i += 0 if v else 1
            elif k in OPT_ARG and not v:
                i += 1
            else:
                flags.add((k, v))
        elif not positional_only and w.startswith("-") and len(w) > 1 and not w[1].isdigit():
            if w in ("-e",):
                if i + 1 < len(args):
                    pats.append(args[i + 1])
                i += 1
            elif w in ("-g", "-t") and i + 1 < len(args):
                flags.add((w, args[i + 1]))
                i += 1
            elif w in OPT_ARG:
                i += 1
            else:
                for ch in w[1:]:
                    flags.add(("-" + ch, ""))
                # attached arg forms like -A3, -m1 are covered by the loop
        else:
            if not pats:
                pats.append(w)
            else:
                paths.append(w)
        i += 1
    if len(pats) > 1:
        pat = "|".join(pats)
    else:
        pat = pats[0] if pats else None
    return pat, paths, {f[0] for f in flags}, flags


def target_class(paths, flagset, flags, root, cwd):
    """'code' | 'noncode' | 'outside'."""
    for k, v in flags:
        if k in ("--include", "-g", "--glob") and v:
            ext = os.path.splitext(v.rstrip("*}").split(",")[0])[1].lower()
            if ext in NONCODE_EXT:
                return "noncode"
        if k in ("-t", "--type") and v in ("md", "markdown", "json", "txt", "yaml", "toml"):
            return "noncode"
    if not paths:
        return "code"
    classes = []
    for p in paths:
        if p.startswith("~") or p.startswith("$"):
            classes.append("outside")
            continue
        ap = os.path.normpath(p if os.path.isabs(p) else os.path.join(cwd or root, p))
        if not (ap == root or ap.startswith(root + "/")) or "/node_modules" in ap or "/.git/" in ap \
                or "/target/" in ap or "/.code-graph" in ap:
            classes.append("outside")
            continue
        ext = os.path.splitext(ap)[1].lower()
        if ext in NONCODE_EXT:
            classes.append("noncode")
        else:
            classes.append("code")
    if "code" in classes:
        return "code"
    return "noncode" if "noncode" in classes else "outside"


def classify_call(name, inp, root, cwd):
    """Feature dict for one tool call."""
    inp = inp if isinstance(inp, dict) else {}
    f = {"kind": "other"}
    hit = _explicit(name, inp)
    if hit and hit[0] == "cg":
        f["kind"] = "cg"
        if hit[1] == "mcp":
            f["cg_cmd"] = name.split("__")[-1]
            f["cg_arg"] = str(inp.get("symbol_name") or inp.get("symbol") or inp.get("query") or inp.get("name") or "")
        elif hit[1] == "bash":
            cmd = str(inp.get("command", ""))
            for _, st in split_stages(without_heredoc_bodies(cmd)):
                w = command_words(st)
                if w and os.path.basename(w[0]) == "code-graph-mcp":
                    rest = [x for x in w[1:] if not x.startswith("-")]
                    f["cg_cmd"] = rest[0] if rest else "?"
                    f["cg_arg"] = rest[1] if len(rest) > 1 else ""
                    f["cg_devbuild"] = "/target/" in w[0]
                    break
        else:
            f["cg_cmd"] = hit[1]
        return f
    if hit and hit[0] == "mem":
        f["kind"] = "mem"
        return f
    if name == "Grep":
        pat = str(inp.get("pattern", ""))
        path = inp.get("path")
        flags = set()
        if inp.get("glob"):
            flags.add(("-g", str(inp["glob"])))
        if inp.get("type"):
            flags.add(("-t", str(inp["type"])))
        f.update(kind="grep", via="Grep", pattern=pat,
                 target=target_class([path] if path else [], set(), flags, root, cwd),
                 icase=bool(inp.get("-i")))
    elif name == "Bash":
        cmd = str(inp.get("command", ""))
        nav = False
        for sep, st in split_stages(without_heredoc_bodies(cmd)):
            w = command_words(st)
            if not w:
                continue
            head = os.path.basename(w[0])
            is_grep = head in GREP_HEADS or (head == "git" and "grep" in w[1:3])
            if is_grep:
                pat, paths, fset, flags = parse_grep(w)
                recursive = bool(fset & {"-r", "-R", "--recursive"}) or head in ("rg", "ag", "ack") \
                    or head == "git"
                if sep == "|" and not paths and not recursive:
                    continue  # filters piped output
                if not paths and not recursive:
                    continue
                f.update(kind="grep", via=head, pattern=pat or "",
                         target=target_class(paths, fset, flags, root, cwd),
                         icase="-i" in fset or "--ignore-case" in fset)
                break
            if head in NAV_BASH:
                nav = True
        if f["kind"] == "other" and nav:
            f["kind"] = "nav"
    elif name in ("Read",):
        f["kind"] = "read"
        f["file"] = str(inp.get("file_path", ""))
    elif name in ("Glob",):
        f["kind"] = "nav"
    elif name in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
        f["kind"] = "edit"
    elif name in ("Agent", "Task"):
        f["kind"] = "agent"
        f["agent_type"] = str(inp.get("subagent_type", ""))
    if f["kind"] == "grep":
        si = symbol_idents(f["pattern"])
        if si:
            ids, decl = si
            names = index_names(root)
            known = [x for x in ids if x in names or x.split(".")[-1].split("::")[-1] in names]
            f.update(symbol=True, ids=ids, decl=decl, known=bool(known))
        else:
            f["symbol"] = False
    return f


def plugin_of_text(text, event):
    s = text.lstrip()
    if s.startswith("[code-graph") or (event == "SubagentStart" and "code-graph AST index" in s):
        return "cg"
    if s.startswith(("[mem", "[claude-mem-lite", "<claude-mem-context>")):
        return "mem"
    if s.startswith("[claudemd"):
        return "claudemd"
    return "other"


def plugin_of_cmd(cmd):
    c = str(cmd)
    if "code-graph" in c or re.search(r"(pre-grep-guide|post-grep-inject|pre-read-guide|pre-edit-guide|"
                                      r"session-init|subagent-start|user-prompt-context|incremental-index|"
                                      r"stop-impact)\.js", c):
        return "cg"
    if re.search(r"hook-launcher\.mjs|pre-tool-recall|post-tool-use\.sh|setup\.sh|user-prompt-search", c):
        return "mem"
    if re.search(r"/hooks/[a-z-]+\.sh|run-hook\.cmd", c):
        return "claudemd"
    return "other"


def result_text(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(c.get("text", "") for c in content if isinstance(c, dict))
    return ""


def parse(path):
    sess = {"path": path, "kind": "subagent" if "/subagents/" in path else "main", "cwd": None, "start": None,
            "entry": None, "calls": [], "inject": collections.Counter(), "inject_n": collections.Counter(),
            "hook_ms": collections.defaultdict(list), "pointers": [], "texts_mentioning_cg": 0, "texts": 0,
            "rewrites": []}
    results = {}
    order = []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if not isinstance(rec, dict):
                continue
            sess["start"] = sess["start"] or rec.get("timestamp")
            sess["cwd"] = sess["cwd"] or rec.get("cwd")
            sess["entry"] = sess["entry"] or rec.get("entrypoint")
            t = rec.get("type")
            if t == "attachment":
                a = rec.get("attachment") or {}
                at = a.get("type")
                ev = a.get("hookEvent")
                if at == "hook_additional_context":
                    texts = a.get("content") or []
                    for tx in [texts] if isinstance(texts, str) else texts:
                        tx = str(tx)
                        p = plugin_of_text(tx, ev)
                        sess["inject"][(p, a.get("hookName"))] += len(tx)
                        sess["inject_n"][(p, a.get("hookName"))] += 1
                        if p == "cg" and pointer_verb(tx):
                            sess["pointers"].append((len(order), pointer_verb(tx)))
                elif at == "hook_success":
                    p = plugin_of_cmd(a.get("command"))
                    if a.get("durationMs") is not None:
                        sess["hook_ms"][(p, a.get("hookName"))].append(a["durationMs"])
                    out = str(a.get("stdout") or "")
                    doc = _json(out.strip())
                    if doc is None and out.strip() and ev in ("UserPromptSubmit", "SessionStart"):
                        pp = plugin_of_text(out, ev)
                        if pp == "other":
                            pp = p
                        sess["inject"][(pp, a.get("hookName"))] += len(out)
                        sess["inject_n"][(pp, a.get("hookName"))] += 1
                    elif doc and ev == "PreToolUse":
                        hso = doc.get("hookSpecificOutput") or {}
                        if isinstance(hso, dict) and hso.get("updatedInput") and p == "cg":
                            sess["rewrites"].append(a.get("toolUseID"))
            elif t == "assistant":
                msg = rec.get("message") or {}
                for b in msg.get("content") or [] if isinstance(msg.get("content"), list) else []:
                    if not isinstance(b, dict):
                        continue
                    if b.get("type") == "tool_use":
                        order.append({"id": b.get("id"), "name": str(b.get("name", "")),
                                      "input": b.get("input"), "ts": rec.get("timestamp")})
                    elif b.get("type") == "text":
                        sess["texts"] += 1
                        if re.search(r"code-graph|\bcg\b|callgraph|hook", str(b.get("text", "")), re.I):
                            sess["texts_mentioning_cg"] += 1
            elif t == "user":
                msg = rec.get("message") or {}
                c = msg.get("content")
                if isinstance(c, list):
                    for b in c:
                        if isinstance(b, dict) and b.get("type") == "tool_result":
                            results[b.get("tool_use_id")] = (result_text(b.get("content")), bool(b.get("is_error")),
                                                             rec.get("timestamp"))
    sess["order"] = order
    sess["results"] = results
    return sess


def secs(a, b):
    import datetime
    try:
        ta = datetime.datetime.fromisoformat(a.replace("Z", "+00:00"))
        tb = datetime.datetime.fromisoformat(b.replace("Z", "+00:00"))
        return (tb - ta).total_seconds()
    except Exception:
        return None


def analyse(sess, root):
    calls = []
    for c in sess["order"]:
        f = classify_call(c["name"], c["input"], root, sess["cwd"])
        r = sess["results"].get(c["id"])
        f["name"] = c["name"]
        f["id"] = c["id"]
        f["res_len"] = len(r[0]) if r else None
        f["err"] = r[1] if r else None
        f["lat"] = secs(c["ts"], r[2]) if r and c["ts"] and r[2] else None
        f["res_head"] = r[0][:400] if r else ""
        f["rewritten"] = c["id"] in sess["rewrites"]
        f["input"] = c["input"]
        calls.append(f)
    return calls


NAVKINDS = {"grep", "read", "nav", "cg"}


def nav_run(calls, i):
    """Navigation calls from i (inclusive) until the first non-navigation call."""
    n = 0
    for c in calls[i:]:
        if c["kind"] in NAVKINDS:
            n += 1
        else:
            break
    return n


def sig_edits(calls):
    """Edits whose old_string holds a declaration header that the new_string
    changes or drops: (index, name)."""
    out = []
    hdr = re.compile(r"^\s*(?:export\s+)?(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:def|fn|function)\s+([A-Za-z_]\w*)\s*\(([^)]*)",
                     re.M)
    for i, c in enumerate(calls):
        if c["kind"] != "edit" or c["name"] not in ("Edit", "MultiEdit"):
            continue
        inp = c["input"] if isinstance(c["input"], dict) else {}
        fp = str(inp.get("file_path", ""))
        if os.path.splitext(fp)[1].lower() not in CODE_EXT:
            continue
        edits = inp.get("edits") or [{"old_string": inp.get("old_string", ""), "new_string": inp.get("new_string", "")}]
        for e in edits:
            old, new = str(e.get("old_string", "")), str(e.get("new_string", ""))
            olds = {m.group(1): m.group(2).strip() for m in hdr.finditer(old)}
            news = {m.group(1): m.group(2).strip() for m in hdr.finditer(new)}
            for nm, params in olds.items():
                if nm.startswith("test") or nm.startswith("_test"):
                    continue
                if nm not in news or re.sub(r"\s+", "", news[nm]) != re.sub(r"\s+", "", params):
                    # A body-only edit keeps the header; this changes params or drops/renames the def.
                    if params.count(",") + 1 > 0 and (nm not in news or news[nm] != params):
                        out.append((i, nm))
    return out


def mentions(c, name):
    if c["kind"] == "grep":
        return name in str(c.get("pattern", ""))
    if c["kind"] == "cg":
        return name in str(c.get("cg_arg", "")) or name in json.dumps(c.get("input"))
    return False


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--projects-dir", default=os.path.expanduser("~/.claude/projects"))
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    paths = sorted(glob.glob(os.path.join(a.projects_dir, "*", "*.jsonl"))
                   + glob.glob(os.path.join(a.projects_dir, "*", "*", "subagents", "agent-*.jsonl")))
    agg = collections.defaultdict(lambda: collections.Counter())
    lists = collections.defaultdict(list)
    examples = collections.defaultdict(list)
    weekly = collections.defaultdict(collections.Counter)
    per_session = []
    for p in paths:
        try:
            s = parse(p)
        except OSError:
            continue
        if not s["start"] or not s["cwd"] or s["entry"] != "cli":
            continue
        root, proj = root_of(os.path.realpath(s["cwd"]) if os.path.exists(s["cwd"]) else s["cwd"])
        if not root:
            continue
        calls = analyse(s, root)
        era = era_of(s["start"])
        grp = "own" if proj in OWN else "neutral"
        keys = [(era, s["kind"], grp), (era, s["kind"], proj), ("ALL", s["kind"], grp)]
        wk = week_of(s["start"])
        n = len(calls)
        ncg = sum(1 for c in calls if c["kind"] == "cg" and not c.get("cg_devbuild"))
        weekly[(wk, s["kind"], grp)].update(tools=n, cg=ncg, sessions=1, cg_sessions=int(ncg > 0))
        sym = [c for c in calls if c["kind"] == "grep" and c.get("symbol") and c.get("known")
               and c["target"] == "code" and not c.get("icase")]
        cgs = [c for c in calls if c["kind"] == "cg" and not c.get("cg_devbuild") and c.get("cg_cmd") in CG_STRUCT]
        per_session.append({"path": p, "proj": proj, "era": era, "kind": s["kind"], "tools": n,
                            "sym_greps": len(sym), "cg": len(cgs), "start": s["start"]})
        for k in keys:
            C = agg[k]
            C["sessions"] += 1
            C["tools"] += n
            C["grep_all"] += sum(1 for c in calls if c["kind"] == "grep")
            C["grep_symbol_any"] += sum(1 for c in calls if c["kind"] == "grep" and c.get("symbol"))
            C["grep_symbol_known_code"] += len(sym)
            C["grep_symbol_decl"] += sum(1 for c in sym if c.get("decl"))
            C["grep_symbol_ref"] += sum(1 for c in sym if not c.get("decl"))
            C["grep_via_tool"] += sum(1 for c in sym if c.get("via") == "Grep")
            C["cg_struct"] += len(cgs)
            C["cg_devbuild"] += sum(1 for c in calls if c["kind"] == "cg" and c.get("cg_devbuild"))
            C["sessions_with_sym"] += int(len(sym) > 0)
            C["sessions_with_cg"] += int(len(cgs) > 0)
            C["reads"] += sum(1 for c in calls if c["kind"] == "read")
            for c in cgs:
                C["cgcmd:" + str(c.get("cg_cmd"))] += 1
                C["cg_err"] += int(bool(c.get("err")))
                C["cg_empty"] += int(bool(EMPTY_HINTS.search(c.get("res_head", ""))))
            for (pl, hk), chars in s["inject"].items():
                C[f"inject_chars:{pl}"] += chars
                C[f"inject_n:{pl}"] += s["inject_n"][(pl, hk)]
            C["pointers"] += len(s["pointers"])
            C["texts"] += s["texts"]
            C["texts_mentioning_cg"] += s["texts_mentioning_cg"]
            C["rewrites"] += len(s["rewrites"])
        # per-call lists (era/kind/grp key only, to keep it small)
        k0 = (era, s["kind"], grp)
        for i, c in enumerate(calls):
            if c in sym:
                lists[k0 + ("navrun_after_symgrep",)].append(nav_run(calls, i))
                if c["res_len"] is not None:
                    lists[k0 + ("reslen_symgrep",)].append(c["res_len"])
                if c["lat"] is not None:
                    lists[k0 + ("lat_symgrep",)].append(c["lat"])
                if len(examples[k0 + ("symgrep",)]) < 400:
                    examples[k0 + ("symgrep",)].append({"proj": proj, "via": c.get("via"), "pattern": c["pattern"][:120],
                                                       "decl": c.get("decl"), "res_len": c["res_len"]})
            if c["kind"] == "cg" and not c.get("cg_devbuild"):
                lists[k0 + ("navrun_after_cg",)].append(nav_run(calls, i))
                if c["res_len"] is not None:
                    lists[k0 + ("reslen_cg",)].append(c["res_len"])
                if c["lat"] is not None:
                    lists[k0 + ("lat_cg",)].append(c["lat"])
                arg = str(c.get("cg_arg") or "")
                fb = arg and len(arg) >= 3 and any(mentions(d, arg) for d in calls[i + 1:i + 6] if d["kind"] == "grep")
                agg[k0]["cg_followed_by_grep_same"] += int(bool(fb))
                if len(examples[k0 + ("cg",)]) < 300:
                    examples[k0 + ("cg",)].append({"proj": proj, "cmd": c.get("cg_cmd"), "arg": arg[:80],
                                                  "err": c.get("err"), "res_len": c["res_len"],
                                                  "head": c.get("res_head", "")[:160], "grep_after": bool(fb)})
            if c.get("rewritten"):
                pat = str(c.get("pattern") or "")
                again = pat and any(d["kind"] == "grep" and d.get("pattern") == pat for d in calls[i + 1:i + 4])
                agg[k0]["rewrite_regrep_same"] += int(bool(again))
        # signature edits
        for i, nm in sig_edits(calls):
            before = calls[max(0, i - 25):i]
            after = calls[i + 1:i + 16]
            b_any = any(mentions(d, nm) for d in before)
            b_cg = any(d["kind"] == "cg" and mentions(d, nm) for d in before)
            a_any = any(mentions(d, nm) for d in after)
            C = agg[k0]
            C["sig_edits"] += 1
            C["sig_checked_before"] += int(b_any)
            C["sig_checked_before_cg"] += int(b_cg)
            C["sig_checked_after"] += int(a_any)
            C["sig_unchecked"] += int(not b_any and not a_any)
            if len(examples[k0 + ("sig",)]) < 200:
                examples[k0 + ("sig",)].append({"proj": proj, "name": nm, "before": b_any, "before_cg": b_cg,
                                               "after": a_any})
        for (pl, hk), ms in s["hook_ms"].items():
            lists[("hook_ms", era, pl, hk)].extend(ms)
        # pointer follow-through
        for idx, verb in s["pointers"]:
            nxt = calls[idx:idx + 10]
            agg[k0]["ptr_followed"] += int(any(d["kind"] == "cg" for d in nxt))

    def summ(v):
        if not v:
            return None
        v = sorted(v)
        return {"n": len(v), "p50": v[len(v) // 2], "p90": v[int(len(v) * 0.9)], "mean": round(statistics.mean(v), 2)}

    out = {"agg": {"|".join(k): dict(v) for k, v in agg.items()},
           "lists": {"|".join(map(str, k)): summ(v) for k, v in lists.items()},
           "weekly": {"|".join(k): dict(v) for k, v in sorted(weekly.items())},
           "examples": {"|".join(k): v for k, v in examples.items()},
           "sessions": per_session}
    with open(a.out, "w") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1, default=str)
    print("wrote", a.out, len(per_session), "sessions")


if __name__ == "__main__":
    main()
