#!/usr/bin/env python3
"""Minimal MCP stdio client: run tool calls against one server process.

usage: mcpc.py <binary> <project_dir> < calls.jsonl > results.jsonl
Each input line: {"id": <any>, "tool": <name>, "args": {...}}
Each output line: {"id": ..., "result": <tool result text parsed as JSON if possible>, "is_error": bool}
"""
import json, subprocess, sys, time

def main():
    binary, proj = sys.argv[1], sys.argv[2]
    p = subprocess.Popen([binary, "serve"], cwd=proj, stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
    rid = 0
    def rpc(method, params, notify=False):
        nonlocal rid
        msg = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notify:
            rid += 1
            msg["id"] = rid
        p.stdin.write(json.dumps(msg) + "\n"); p.stdin.flush()
        if notify:
            return None
        while True:
            line = p.stdout.readline()
            if not line:
                raise SystemExit("server closed stdout")
            resp = json.loads(line)
            if resp.get("id") == rid:
                return resp
    rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "d435", "version": "0"}})
    rpc("notifications/initialized", {}, notify=True)
    # Wait until the startup index is usable.
    for _ in range(600):
        r = rpc("tools/call", {"name": "get_index_status", "arguments": {}})
        txt = r.get("result", {}).get("content", [{}])[0].get("text", "")
        if "Indexing in progress" not in txt and not r.get("result", {}).get("isError"):
            break
        time.sleep(0.5)
    for line in sys.stdin:
        if not line.strip():
            continue
        call = json.loads(line)
        r = rpc("tools/call", {"name": call["tool"], "arguments": call["args"]})
        res = r.get("result", {})
        txt = "".join(c.get("text", "") for c in res.get("content", []))
        try:
            body = json.loads(txt)
        except Exception:
            body = txt
        if "error" in r:
            body, err = r["error"], True
        else:
            err = bool(res.get("isError"))
        sys.stdout.write(json.dumps({"id": call["id"], "is_error": err, "result": body}, sort_keys=True) + "\n")
        sys.stdout.flush()
    p.stdin.close(); p.wait(timeout=30)

main()
