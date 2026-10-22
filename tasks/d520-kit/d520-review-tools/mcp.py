#!/usr/bin/env python3
"""mcp.py <dir> <calls.json|-list>: drive `cgm-head serve` over stdio; print each tool result."""
import json, os, subprocess, sys, select, time
R = "/var/tmp/d520-review"
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_RESYNC_BUDGET="0")
p = subprocess.Popen([f"{R}/bin/{os.environ.get('BIN','cgm-head')}", "serve"], cwd=sys.argv[1], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
def rpc(i, method, params=None):
    msg = {"jsonrpc": "2.0", "id": i, "method": method}
    if params is not None: msg["params"] = params
    p.stdin.write(json.dumps(msg) + "\n"); p.stdin.flush()
    while True:
        line = p.stdout.readline()
        if not line: return None
        try: r = json.loads(line)
        except Exception: continue
        if r.get("id") == i: return r
rpc(1, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "1.0"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"); p.stdin.flush()
if sys.argv[2] == "-list":
    r = rpc(2, "tools/list", {})
    for t in r["result"]["tools"]: print(t["name"], json.dumps(t.get("inputSchema", {}).get("properties", {}))[:300])
else:
    calls = json.load(open(sys.argv[2]))
    for k, (name, args) in enumerate(calls, 10):
        r = rpc(k, "tools/call", {"name": name, "arguments": args})
        txt = json.dumps(r.get("result", r.get("error")), sort_keys=True) if r else "NONE"
        print(f"### {name} {json.dumps(args)}\n{txt}")
p.stdin.close(); p.terminate()
