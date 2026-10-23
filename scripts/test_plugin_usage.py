"""Synthetic transcripts for plugin_usage.py: python3 -m unittest scripts/test_plugin_usage.py"""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import plugin_usage as pu  # noqa: E402

_ids = iter(range(1, 10**6))


def tool(name, **inp):
    return {"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": f"t{next(_ids)}", "name": name, "input": inp}]}}


def bash(cmd):
    return tool("Bash", command=cmd)


def result(text, is_error=False):
    return {"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "x", "is_error": is_error, "content": text}]}}


def ctx(hook_name, *texts):
    return {"type": "attachment", "attachment": {
        "type": "hook_additional_context", "hookName": hook_name,
        "hookEvent": hook_name.split(":")[0], "content": list(texts)}}


def success(hook_name, stdout):
    return {"type": "attachment", "attachment": {
        "type": "hook_success", "hookName": hook_name,
        "hookEvent": hook_name.split(":")[0], "stdout": stdout}}


def header(ts, cwd="/w/app", entrypoint="cli"):
    return {"type": "user", "timestamp": ts, "cwd": cwd, "entrypoint": entrypoint,
            "message": {"content": "hi"}}


class Tree:
    """A throwaway projects dir: write(slug, name, records) → path."""

    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name

    def write(self, rel, records):
        path = os.path.join(self.root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            for r in records:
                f.write(json.dumps(r) + "\n")
        return path

    def close(self):
        self.tmp.cleanup()


class ClassifyTest(unittest.TestCase):
    def test_mem_bash(self):
        self.assertTrue(pu.is_mem_bash("claude-mem-lite search foo"))
        self.assertTrue(pu.is_mem_bash(
            "node /h/.claude/plugins/cache/sdsrss/claude-mem-lite/6.25.3/cli.mjs save --type bugfix x"))
        self.assertTrue(pu.is_mem_bash("cd /x && npx claude-mem-lite recent 5"))
        # A path, a commit message or a quoted word is not a call.
        self.assertFalse(pu.is_mem_bash("cd /home/u/dev/claude-mem-lite && git status"))
        self.assertFalse(pu.is_mem_bash('git commit -m "claude-mem-lite save now works"'))
        self.assertFalse(pu.is_mem_bash("node scripts/build.mjs"))
        self.assertFalse(pu.is_mem_bash("cat > n.sh <<'EOF'\nclaude-mem-lite save x\nEOF"))

    def test_pointer(self):
        self.assertEqual(pu.pointer_verb(
            "[code-graph] f is defined in a.py, outside the path. Its callers: code-graph-mcp callgraph f"),
            "callgraph")
        self.assertEqual(pu.pointer_verb(
            "[code-graph:hint] indexed repo — try `semantic_code_search \"<symptom>\"`"), "semantic_code_search")
        self.assertIsNone(pu.pointer_verb("[code-graph:impact] f() — Risk: LOW\n  1 direct caller"))
        # An answer that mentions a command (the rewrite notice describing what ran,
        # a truncated map's tail) is not a pointer: only a bare one- or two-line
        # suggestion asks the model to act.
        self.assertIsNone(pu.pointer_verb(
            "[code-graph] Raw `grep` on indexed source was rewritten — this call's output comes from:\n"
            "$ code-graph-mcp grep -m 0 'x' src/\nEach hit shows its containing fn/module — use these results directly."))
        self.assertIsNone(pu.pointer_verb(
            "[code-graph] 5+ Reads into src/ — module overview:\nsrc/a.rs\n  fn: f (2×)\n"
            "(truncated — `code-graph-mcp overview src/` for the full map)"))
        self.assertEqual(pu.pointer_verb(
            "[code-graph] 5+ Reads into tests/ — `code-graph-mcp overview tests/` gives symbols+callers in one call"),
            "overview")


class ScanTest(unittest.TestCase):
    def setUp(self):
        self.t = Tree()

    def tearDown(self):
        self.t.close()

    def scan(self, records, rel="p/s.jsonl"):
        return pu.scan_transcript(self.t.write(rel, records))

    def test_explicit_calls(self):
        s = self.scan([
            header("2026-10-01T00:00:00Z"),
            bash("code-graph-mcp callgraph f | head"),
            bash("cd /home/u/code-graph-mcp && grep -rn f src/"),   # path only
            tool("mcp__plugin_code-graph-mcp_code-graph__get_call_graph", symbol_name="f"),
            tool("Agent", subagent_type="code-graph-mcp:code-explorer", prompt="x"),
            tool("Skill", skill="code-graph-mcp:explore"),
            tool("mcp__plugin_claude-mem-lite_mem-lite__mem_save", title="x"),
            bash("claude-mem-lite search foo"),
            tool("Skill", skill="claude-mem-lite:lesson"),
            tool("Read", file_path="/w/app/a.py"),
        ])
        self.assertEqual(s["tools"], 9)
        self.assertEqual(s["cg_explicit"], {"bash": 1, "mcp": 1, "agent": 1, "skill": 1})
        self.assertEqual(s["mem_explicit"], {"mcp": 1, "bash": 1, "skill": 1})
        self.assertEqual((s["cwd"], s["entrypoint"], s["start"]), ("/w/app", "cli", "2026-10-01T00:00:00Z"))

    def test_injection_counted_once(self):
        js = json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse",
                                                "additionalContext": "[code-graph:impact] f() — Risk: LOW"}})
        s = self.scan([
            header("2026-10-01T00:00:00Z"),
            tool("Edit", file_path="a.py"),
            success("PreToolUse:Edit", js),                       # JSON stdout: counted via the ctx row
            ctx("PreToolUse:Edit", "[code-graph:impact] f() — Risk: LOW"),
            success("UserPromptSubmit", "[code-graph:hint] indexed repo\n"),   # plain stdout, model-visible
            success("PreToolUse:Bash", "[code-graph] plain stdout of a tool hook\n"),  # not model-visible
            ctx("PreToolUse:Read", "[mem] PreToolUse recall\n[mem] a.py"),
            ctx("PostToolUse:Bash", "[claude-mem-lite] Related memories found"),
            success("UserPromptSubmit", "[mem] FYI — related memories"),
            ctx("SessionStart:startup", "<EXTREMELY_IMPORTANT>other plugin"),
        ])
        self.assertEqual(s["cg_inject"], {"PreToolUse:Edit": 1, "UserPromptSubmit": 1})
        self.assertEqual(s["mem_inject"], {"PreToolUse:Read": 1, "PostToolUse:Bash": 1, "UserPromptSubmit": 1})

    def test_interceptions(self):
        rewrite = json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "permissionDecision": "allow",
            "permissionDecisionReason": "[code-graph] raw grep → AST-aware equivalent",
            "updatedInput": {"command": "code-graph-mcp grep x src/"}}})
        other_rewrite = json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse", "updatedInput": {"command": "ls"},
            "permissionDecisionReason": "[other] rewrote"}})
        s = self.scan([
            header("2026-10-01T00:00:00Z"),
            bash("grep -rn x src/"),
            result("[code-graph] Raw `grep` on indexed source — denied; the AST-aware equivalent ran", True),
            bash("grep -rn y src/"),
            success("PreToolUse:Bash", rewrite),
            bash("ls"),
            success("PreToolUse:Bash", other_rewrite),
            result("[code-graph] a successful answer", False),
        ])
        self.assertEqual((s["cg_deny"], s["cg_rewrite"]), (1, 1))

    def test_subagent_start_context_counts(self):
        # code-graph's SubagentStart text has no `[code-graph` prefix (review 2026-10-16).
        txt = ("This repository has a code-graph AST index of 418 files. Who calls a function: "
               "code-graph-mcp callgraph <fn>")
        js = json.dumps({"hookSpecificOutput": {"hookEventName": "SubagentStart", "additionalContext": txt}})
        s = self.scan([header("2026-10-01T00:00:00Z"), success("SubagentStart", js), ctx("SubagentStart", txt),
                       ctx("PreToolUse:Read", "This repository has a code-graph AST index (quoted elsewhere)")])
        self.assertEqual(s["cg_inject"], {"SubagentStart": 1})

    def test_failed_code_graph_call_is_not_a_deny(self):
        call = bash("code-graph-mcp grep zzz src/")
        s = self.scan([header("2026-10-01T00:00:00Z"), call,
                       {"type": "user", "message": {"content": [{
                           "type": "tool_result", "tool_use_id": call["message"]["content"][0]["id"],
                           "is_error": True, "content": "[code-graph] No matches for: zzz"}]}}])
        self.assertEqual((s["cg_deny"], s["cg_explicit"]), (0, {"bash": 1}))

    def test_non_object_records_do_not_abort(self):
        s = self.scan([header("2026-10-01T00:00:00Z"),
                       {"type": "attachment", "attachment": "text"},
                       {"type": "user", "message": "text"},
                       {"type": "assistant", "message": "text"},
                       {"type": "attachment", "attachment": {"type": "hook_additional_context",
                                                             "hookName": "SubagentStart", "hookEvent": "SubagentStart",
                                                             "content": "code-graph AST index"}},
                       bash("code-graph-mcp show f")])
        self.assertEqual((s["tools"], s["cg_explicit"]), (1, {"bash": 1}))

    def test_non_dict_hook_output_does_not_abort(self):
        s = self.scan([header("2026-10-01T00:00:00Z"),
                       success("PreToolUse:Bash", json.dumps({"hookSpecificOutput": "text"})),
                       success("PreToolUse:Bash", json.dumps({"hookSpecificOutput": None}))])
        self.assertEqual(s["cg_rewrite"], 0)

    def test_pointer_follow_through_window(self):
        ptr = "[code-graph] f is defined in a.py, outside the path your grep searched. Its callers: code-graph-mcp callgraph f"
        recs = [header("2026-10-01T00:00:00Z"), bash("grep -rn f tests/"), ctx("PostToolUse:Bash", ptr)]
        recs += [tool("Read", file_path=f"f{i}") for i in range(2)]
        recs += [bash("code-graph-mcp callgraph f")]                 # 3rd call after: inside a window of 3
        recs += [bash("grep -rn g tests/"), ctx("PostToolUse:Bash", ptr.replace(" f", " g"))]
        recs += [tool("Read", file_path=f"g{i}") for i in range(3)]
        recs += [bash("code-graph-mcp callgraph g")]                 # 4th call after: outside a window of 3
        s = pu.scan_transcript(self.t.write("p/s.jsonl", recs), window=3)
        self.assertEqual((s["pointers"], s["pointers_followed"]), (2, 1))
        self.assertEqual(s["pointer_kinds"], {"PostToolUse:Bash callgraph": 2})


def indexed_project(names):
    """A project dir whose .code-graph/index.db defines NAMES (and a heading)."""
    import sqlite3
    d = tempfile.TemporaryDirectory()
    os.makedirs(os.path.join(d.name, ".code-graph"))
    c = sqlite3.connect(os.path.join(d.name, ".code-graph", "index.db"))
    c.execute("CREATE TABLE nodes (id INTEGER PRIMARY KEY, name TEXT, qualified_name TEXT, type TEXT)")
    c.executemany("INSERT INTO nodes (name, qualified_name, type) VALUES (?, ?, ?)",
                  [(n, None, "function") for n in names] + [("Overview", None, "h2")])
    c.commit()
    c.close()
    return d


def answered(call, text, is_error=False):
    return {"type": "user", "message": {"content": [{
        "type": "tool_result", "tool_use_id": call["message"]["content"][0]["id"],
        "is_error": is_error, "content": text}]}}


class AuditCountersTest(unittest.TestCase):
    """2026-10-23 audit: lookups the index could answer, code-graph calls that
    came back empty, and edits in linked worktrees (docs/plugin-usage-audit-2026-10-23.md)."""

    def setUp(self):
        self.t = Tree()
        self.p = indexed_project(["fetch_rows", "Store", "load_all"])

    def tearDown(self):
        self.t.close()
        self.p.cleanup()

    def scan(self, records):
        return pu.scan_transcript(self.t.write("p/s.jsonl", records))

    def test_symbol_greps(self):
        cwd = self.p.name
        s = self.scan([
            header("2026-10-20T00:00:00Z", cwd=cwd),
            tool("Grep", pattern="def fetch_rows", path="src"),          # a definition lookup
            bash('grep -rn "fetch_rows(\\|Store" src/'),                  # references, alternation
            bash("rg -n '\\bload_all\\b'"),                               # rg searches the cwd
            bash("git grep -n fetch_rows -- src"),
            bash('grep -rn "TODO: fix" src/'),                            # text
            bash("grep -rn unknown_name src/"),                           # not a name the index defines
            bash("grep -rn Overview docs/"),                              # a markdown heading is no symbol
            bash("grep -rni fetch_rows src/"),                            # case-insensitive: a text search
            bash("cargo test 2>&1 | grep fetch_rows"),                    # filters piped output
            bash("grep -n fetch_rows notes.md"),                          # a non-code file
            tool("Grep", pattern="fetch_rows", glob="*.md"),
            bash("grep -rn fetch_rows ~/.claude/"),                       # outside the project
            bash("grep fetch_rows"),                                      # reads stdin
        ])
        self.assertEqual(s["symbol_greps"], 4)

    def test_symbol_greps_need_an_index(self):
        s = self.scan([header("2026-10-20T00:00:00Z", cwd="/w/app"), bash("grep -rn fetch_rows src/")])
        self.assertEqual(s["symbol_greps"], 0)

    def test_symbol_greps_from_a_subdirectory_and_a_worktree(self):
        wt = os.path.join(self.p.name, ".claude", "worktrees", "wt", "src")
        os.makedirs(wt)
        s = self.scan([header("2026-10-20T00:00:00Z", cwd=wt), bash("grep -rn fetch_rows .")])
        self.assertEqual(s["symbol_greps"], 1)

    def test_empty_code_graph_answers(self):
        a, b, c, d = (bash("code-graph-mcp show f 2>/dev/null | head -40"), bash("code-graph-mcp callgraph f"),
                      bash("code-graph-mcp show g"), bash("grep -rn x src/"))
        s = self.scan([
            header("2026-10-20T00:00:00Z"),
            a, answered(a, "(Bash completed with no output)"),
            b, answered(b, "Error: No index found at /tmp/r/.code-graph/index.db. "
                           "Run: code-graph-mcp incremental-index", True),
            c, answered(c, "fn g  src/g.rs:1-3  ()\n  fn g() {}"),
            d, answered(d, ""),                                           # not a code-graph call
        ])
        # Counted apart: since the no-index line reaches stdout, a clone's call
        # leaves `empty` for `no_index` (tasks/specs/no-index-visible.md AC5).
        self.assertEqual((s["cg_explicit"], s["cg_empty"], s["cg_no_index"]), ({"bash": 3}, 1, 1))

    def test_a_no_index_error_is_no_deny(self):
        # A dev build run as `cargo run -- show x` is no code-graph call, and its
        # failed result now opens with the no-index line on stdout.
        call = bash("cargo run -q -- show x")
        s = self.scan([header("2026-10-20T00:00:00Z"), call,
                       answered(call, "[code-graph] No index found at /w/.code-graph/index.db. Run: "
                                      "code-graph-mcp incremental-index\nError: No index found at …", True)])
        self.assertEqual(s["cg_deny"], 0)

    def test_worktree_edits_and_their_impact(self):
        wt = "/w/app/.claude/worktrees/agent-1"
        s = self.scan([
            header("2026-10-20T00:00:00Z", cwd=wt),
            tool("Edit", file_path=wt + "/a.py"),
            ctx("PreToolUse:Edit", "[code-graph:impact] f() — Risk: LOW"),
            tool("Edit", file_path=wt + "/b.py"),
            tool("Write", file_path=wt + "/c.py"),
        ])
        self.assertEqual((s["worktree"], s["edits"], s["cg_inject"]), (True, 3, {"PreToolUse:Edit": 1}))
        root = self.scan([header("2026-10-20T00:00:00Z"), tool("Edit", file_path="/w/app/a.py")])
        self.assertEqual((root["worktree"], root["edits"]), (False, 1))

    def test_aggregate(self):
        cwd = self.p.name
        self.t.write("-w-app/a.jsonl", [header("2026-10-20T00:00:00Z", cwd=cwd), bash("grep -rn fetch_rows src/"),
                                        bash("grep -rn Store src/"), bash("code-graph-mcp callgraph load_all")])
        wt = cwd + "/.claude/worktrees/wt"
        self.t.write("-w-app/b.jsonl", [header("2026-10-20T00:01:00Z", cwd=wt),
                                        tool("Edit", file_path=wt + "/a.py"),
                                        ctx("PreToolUse:Edit", "[code-graph:impact] f() — Risk: LOW"),
                                        tool("Edit", file_path=wt + "/b.py")])
        rows = pu.aggregate(pu.collect(self.t.root, projects=[cwd]))
        self.assertEqual(len(rows), 1)
        r = rows[0]
        self.assertEqual((r["symbol_greps"], r["cg_explicit"], r["structural_coverage"]), (2, 1, 0.333))
        self.assertEqual((r["tools"], r["edits"], r["worktree_edits"], r["worktree_edit_impact"]), (5, 2, 2, 1))
        self.assertEqual(r["symbol_greps_per_1k"], 400.0)


class CollectTest(unittest.TestCase):
    def setUp(self):
        self.t = Tree()
        w = self.t.write
        w("-w-app/a.jsonl", [header("2026-09-20T00:00:00Z"), bash("code-graph-mcp show f"), tool("Read", file_path="x")])
        w("-w-app/b.jsonl", [header("2026-10-02T00:00:00Z"), tool("Read", file_path="x"), tool("Read", file_path="y")])
        w("-w-app/b/subagents/agent-1.jsonl", [header("2026-10-02T00:01:00Z"), bash("code-graph-mcp grep x")])
        w("-w-app/c.jsonl", [header("2026-10-03T00:00:00Z", entrypoint="sdk-cli"), bash("code-graph-mcp show f")])
        w("-w-cg/d.jsonl", [header("2026-10-03T00:00:00Z", cwd="/w/code-graph-mcp"), bash("code-graph-mcp show f")])
        w("-w-app/deep/x/y.jsonl", [header("2026-10-03T00:00:00Z"), bash("code-graph-mcp show f")])  # not a transcript slot

    def tearDown(self):
        self.t.close()

    def test_filters_and_depth(self):
        got = pu.collect(self.t.root)
        self.assertEqual(sorted(os.path.basename(s["path"]) for s in got), ["a.jsonl", "agent-1.jsonl", "b.jsonl", "d.jsonl"])
        self.assertEqual(len(pu.collect(self.t.root, entrypoints=None)), 5)
        self.assertEqual(len(pu.collect(self.t.root, exclude=["/code-graph-mcp"])), 3)
        self.assertEqual(len(pu.collect(self.t.root, since="2026-10-01")), 3)
        self.assertEqual(len(pu.collect(self.t.root, until="2026-10-01")), 1)
        self.assertEqual(len(pu.collect(self.t.root, projects=["/w/app"])), 3)

    def test_vanished_transcript_is_skipped(self):
        # Another session's throwaway project can delete its transcript between
        # the glob and the open; a dangling link is the same shape.
        os.symlink(os.path.join(self.t.root, "gone.jsonl"), os.path.join(self.t.root, "-w-app", "e.jsonl"))
        self.assertEqual(len(pu.collect(self.t.root)), 4)

    def test_aggregate_periods(self):
        rows = pu.aggregate(pu.collect(self.t.root, exclude=["/code-graph-mcp"]),
                            splits=[("2026-09-30", "0.164")])
        by = {(r["period"], r["kind"]): r for r in rows}
        self.assertEqual(set(by), {("before 0.164", "main"), ("0.164", "main"), ("0.164", "subagent")})
        a = by[("before 0.164", "main")]
        self.assertEqual((a["sessions"], a["tools"], a["cg_explicit"], a["sessions_with_cg"]), (1, 2, 1, 1))
        self.assertEqual(a["cg_explicit_per_1k"], 500.0)
        b = by[("0.164", "main")]
        self.assertEqual((b["tools"], b["cg_explicit"], b["cg_explicit_per_1k"]), (2, 0, 0.0))

    def test_aggregate_by_project(self):
        rows = pu.aggregate(pu.collect(self.t.root), by_project=True)
        self.assertEqual(sorted({r["project"] for r in rows}), ["/w/app", "/w/code-graph-mcp"])


class MainTest(unittest.TestCase):
    def test_window_must_be_positive(self):
        import io
        from contextlib import redirect_stderr
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            pu.main(["--window", "0"])

    def test_json_and_table(self):
        t = Tree()
        try:
            t.write("-w-app/a.jsonl", [header("2026-09-20T00:00:00Z"), bash("code-graph-mcp show f")])
            import io
            from contextlib import redirect_stdout
            buf = io.StringIO()
            with redirect_stdout(buf):
                self.assertEqual(pu.main(["--projects-dir", t.root, "--json"]), 0)
            rows = json.loads(buf.getvalue())
            self.assertEqual(rows[0]["cg_explicit"], 1)
            buf = io.StringIO()
            with redirect_stdout(buf):
                self.assertEqual(pu.main(["--projects-dir", t.root, "--split", "2026-09-30=0.164", "--detail"]), 0)
            self.assertIn("before 0.164", buf.getvalue())
        finally:
            t.close()


if __name__ == "__main__":
    unittest.main()
