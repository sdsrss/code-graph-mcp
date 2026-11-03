"""Does the startup repair (orphan reap + vector compaction) run in an MCP session
that sends only the handshake Claude Code sends, and no tools/call?
Arm A: initialize, notifications/initialized, tools/list, then idle.
Arm B: the same, then one tools/call after the startup index is done.
CG_BIN overrides the binary (default: the installed release). Each stderr line is
logged with seconds since spawn, so the repair's duration can be read off."""
import json, os, sqlite3, subprocess, sys, threading, time
arm, proj, home = sys.argv[1], sys.argv[2], sys.argv[3]
BIN = os.environ.get("CG_BIN") or os.path.expanduser("~/.cache/code-graph/bin/code-graph-mcp")
db = os.path.join(proj, ".code-graph", "index.db")
def slots():
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    r = (c.execute("select count(*) from node_vectors_rowids").fetchone()[0], c.execute("select coalesce(sum(size),0) from node_vectors_chunks").fetchone()[0], c.execute("select count(*) from nodes").fetchone()[0])
    c.close(); return r
print(arm, "before (live vectors, slots, nodes):", slots(), "db MB", os.path.getsize(db) // 1048576, flush=True)
env = dict(os.environ, HOME=home, CODE_GRAPH_MODEL_DIR=os.path.expanduser("~/.cache/code-graph/models"), RUST_LOG="info")
err = open(os.path.join(home, "stderr.log"), "w")
t_spawn = time.time()
p = subprocess.Popen([BIN, "serve"], cwd=proj, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
def pump():
    for line in p.stderr:
        err.write(f"{time.time() - t_spawn:8.2f} {line}"); err.flush()
pumper = threading.Thread(target=pump); pumper.start()
def send(o): p.stdin.write(json.dumps(o) + "\n"); p.stdin.flush()
def recv():
    return json.loads(p.stdout.readline())
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "probe", "version": "0"}}}); recv()
send({"jsonrpc": "2.0", "method": "notifications/initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}); recv()
# wait for the startup index to finish (progress file gone + db quiet)
t0 = time.time()
while time.time() - t0 < 240:
    time.sleep(5)
    if not os.path.exists(os.path.join(proj, ".code-graph", "indexing-status.json")) and time.time() - t0 > 30: break
print(arm, "startup index done after", round(time.time() - t0), "s", flush=True)
if arm == "B":
    send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "project_map", "arguments": {}}})
    import select
    r, _, _ = select.select([p.stdout], [], [], 120)
    if r: p.stdout.readline()
time.sleep(60)
p.stdin.close(); p.wait(timeout=120); pumper.join(); err.close()
print(arm, "after (live vectors, slots, nodes):", slots(), "db MB", os.path.getsize(db) // 1048576)
log = open(os.path.join(home, "stderr.log")).read()
for k in ("startup-repair", "vec-compact", "Index version", "Failed"):
    print(arm, k, "lines:", sum(1 for l in log.splitlines() if k in l))
for l in log.splitlines():
    if any(k in l for k in ("Background indexing complete", "startup-repair", "vec-compact", "[embed-bg]")):
        print(arm, "|", l[:220])
