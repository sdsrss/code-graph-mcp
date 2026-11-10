"""MCP half of the D#753 differential: the same tool calls against a `serve`
process of each arm, results compared after the arm root is normalised."""
import concurrent.futures as cf
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(__file__))
from drive import CORPORA, ENV, W, inputs, root  # noqa: E402


def calls(c):
    out = []
    for cmd in inputs(c):
        sub, rest = cmd[0], cmd[1:]
        arg = rest[0] if rest and not rest[0].startswith("--") else None
        if sub == "callgraph" and "--json" in rest:
            out.append(("get_call_graph", {"symbol_name": arg}))
        elif sub == "refs" and "--json" in rest:
            out.append(("find_references", {"symbol_name": arg}))
            out.append(("find_references", {"symbol_name": arg, "min_confidence": "ambiguous"}))
        elif sub == "impact":
            out.append(("get_ast_node", {"symbol_name": arg, "include_impact": True, "include_references": True}))
        elif sub == "search" and "--json" in rest:
            out.append(("semantic_code_search", {"query": arg}))
            out.append(("ast_search", {"query": arg}))
        elif sub == "overview":
            out.append(("module_overview", {"path": arg, "include_deps": True, "include_dead": True}))
    out.append(("project_map", {"include_centrality": True}))
    return out


def serve(arm, c, todo):
    p = subprocess.Popen([f"{W}/bin/{arm}", "serve"], cwd=root(arm, c), env=ENV, stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
    rid = 0

    def rpc(method, params, notify=False):
        nonlocal rid
        msg = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notify:
            rid += 1
            msg["id"] = rid
        p.stdin.write(json.dumps(msg) + "\n")
        p.stdin.flush()
        if notify:
            return None
        while True:
            line = p.stdout.readline()
            if not line:
                raise SystemExit(f"{arm}/{c}: server closed stdout")
            resp = json.loads(line)
            if resp.get("id") == rid:
                return resp

    rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "d753", "version": "0"}})
    rpc("notifications/initialized", {}, notify=True)
    results = []
    for tool, args in todo:
        r = rpc("tools/call", {"name": tool, "arguments": args})
        text = json.dumps(r.get("result", r.get("error")), sort_keys=True)
        text = text.replace(root(arm, c), "<ROOT>").replace(f"{W}/run-{arm}", "<RUN>")
        results.append(text)
    p.stdin.close()
    p.wait(timeout=60)
    return results


def main():
    total = differing = 0
    for c in CORPORA:
        todo = calls(c)
        with cf.ThreadPoolExecutor(2) as ex:
            fa, fb = ex.submit(serve, "base", c, todo), ex.submit(serve, "head", c, todo)
            a, b = fa.result(), fb.result()
        diffs = [{"call": t, "base": x, "head": y} for t, x, y in zip(todo, a, b) if x != y]
        total += len(todo)
        differing += len(diffs)
        json.dump(diffs, open(f"{W}/qdiff/mcp-{c}.json", "w"), indent=1)
        print(f"{c:8} calls {len(todo):4} differing {len(diffs)}"
              + (f"  e.g. {[d['call'] for d in diffs[:3]]}" if diffs else ""), flush=True)
    print("total", total, "differing", differing)


if __name__ == "__main__":
    main()
