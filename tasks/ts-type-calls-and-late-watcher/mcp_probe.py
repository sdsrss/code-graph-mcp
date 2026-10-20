import json, os, subprocess, sys, time, sqlite3, select
# usage: mcp_probe.py <binary> <corpus_dir> <home> <delay_after_index_s> <label>
binary, corpus, home, delay, label = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
env = {k: v for k, v in os.environ.items() if not k.startswith("XDG_")}
env.update(HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1", RUST_LOG="info")
errlog = open(os.path.join(os.environ.get("RV_LOGDIR", "."), f"probe-{label}.stderr"), "w")
p = subprocess.Popen([binary, "serve"], cwd=corpus, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errlog, bufsize=0)
buf = b""
def send(obj):
    p.stdin.write((json.dumps(obj) + "\n").encode()); p.stdin.flush()
def read_until_id(want, timeout=300):
    global buf
    t0 = time.time(); notes = []
    while time.time() - t0 < timeout:
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            if not line.strip(): continue
            m = json.loads(line)
            if m.get("id") == want: return m, notes
            notes.append(m)
        r, _, _ = select.select([p.stdout], [], [], 0.5)
        if r:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk: raise SystemExit("server closed stdout")
            buf += chunk
    raise SystemExit("timeout waiting for id %s" % want)
status = os.path.join(corpus, ".code-graph", "indexing-status.json")
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "rv", "version": "0"}}})
read_until_id(1)
t_init = time.time()
send({"jsonrpc": "2.0", "method": "notifications/initialized"})
# wait for the startup index to start and finish (status file appears, then disappears)
seen = False; t0 = time.time()
while time.time() - t0 < 600:
    ex = os.path.exists(status)
    if ex: seen = True
    if seen and not ex: break
    if not seen and time.time() - t0 > 5: break  # very fast index may have come and gone
    time.sleep(0.02)
t_index_done = time.time()
time.sleep(delay)
late = os.path.join(corpus, "p0", "src", "zz_late_rv.ts") if os.path.isdir(os.path.join(corpus, "p0")) else os.path.join(corpus, "src", "zz_late_rv.ts")
NOCHANGE = os.environ.get("RV_NOCHANGE") == "1"
if not NOCHANGE:
    with open(late, "w") as f: f.write("export function lateRvFn() {\n  return 1\n}\n")
t_change = time.time()
send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "get_call_graph", "arguments": {"symbol_name": "compose", "depth": 1}}})
resp, notes = read_until_id(2)
t_resp = time.time()
db = os.path.join(corpus, ".code-graph", "index.db")
c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
n_late = c.execute("SELECT COUNT(*) FROM nodes WHERE name='lateRvFn'").fetchone()[0]
c.close()
send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_call_graph", "arguments": {"symbol_name": "compose", "depth": 1}}})
t3 = time.time(); read_until_id(3); t3e = time.time()
logs = [n.get("params", {}).get("data") for n in notes if n.get("method") == "notifications/message"]
print(json.dumps({"label": label, "index_secs": round(t_index_done - t_init, 2), "first_call_ms": round((t_resp - t_change) * 1000), "second_call_ms": round((t3e - t3) * 1000), "late_fn_indexed_after_first_call": n_late, "is_error": resp.get("result", {}).get("isError"), "logs": logs}))
time.sleep(float(os.environ.get('RV_TAIL', '0')))
p.stdin.close()
try: p.wait(timeout=30)
except Exception: p.kill()
if not NOCHANGE: os.remove(late)
