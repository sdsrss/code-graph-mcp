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
