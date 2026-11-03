"""Replay era-E source reads through the read-fanout rule, keyed per project
(the old rule) and per project + session (D#713), and count the overviews each
would send. Reads ~/.claude/projects at fixed depth only; writes nothing.
  replay_fanout.py [SINCE]"""
import glob, json, os, re, sys, collections
from datetime import datetime
SINCE = sys.argv[1] if len(sys.argv) > 1 else "2026-10-23T23:00"
EXCLUDE = ("/home/ai/dev/code-graph-mcp", "/home/ai/dev/claude-mem-lite")
SRC = re.compile(r"\.(rs|py|ts|tsx|js|jsx|mjs|cjs|go|java|kt|swift|rb|php|cs|cpp|cc|c|h|hpp|hxx|m|scala|clj|cljs|ex|exs|hs|ml|fs|r|lua|sh|bash|zsh|fish|sql|vue|svelte|astro|dart|elm|nim|zig)$", re.I)
SED = re.compile(r"""\bsed\s+-n\s+['"]?\d+,\d+p['"]?\s+(\S+)""")
TTL = 30 * 60
pd = os.path.expanduser("~/.claude/projects")
paths = glob.glob(os.path.join(pd, "*", "*.jsonl")) + glob.glob(os.path.join(pd, "*", "*", "subagents", "agent-*.jsonl"))

def root_of(cwd):
    d = cwd
    while d and d != "/":
        if os.path.exists(os.path.join(d, ".code-graph", "index.db")):
            return d
        d = os.path.dirname(d)
    return None

events = []  # (ts, root, session, rel, transcript)
for p in paths:
    try:
        lines = open(p, errors="replace").read().splitlines()
    except OSError:
        continue
    for l in lines:
        try:
            r = json.loads(l)
        except ValueError:
            continue
        ts = r.get("timestamp") or ""
        if ts < SINCE or r.get("type") != "assistant" or r.get("entrypoint") not in (None, "cli"):
            continue
        cwd = r.get("cwd") or ""
        root = root_of(cwd)
        if not root or root.startswith(EXCLUDE):
            continue
        for c in (r.get("message") or {}).get("content") or []:
            if not isinstance(c, dict) or c.get("type") != "tool_use":
                continue
            inp = c.get("input") or {}
            targets = []
            if c.get("name") == "Read":
                targets = [inp.get("file_path") or ""]
            elif c.get("name") == "Bash":
                targets = SED.findall(str(inp.get("command") or ""))
            for t in targets:
                if not SRC.search(t):
                    continue
                a = t if os.path.isabs(t) else os.path.normpath(os.path.join(cwd, t))
                rel = os.path.relpath(a, root)
                if rel.startswith("..") or os.path.dirname(rel) in ("", "."):
                    continue
                events.append((ts, root, r.get("sessionId") or "", rel, p))

def replay(key_of):
    state = {}  # key -> dir -> {files, last, hinted}
    fired = []
    for ts, root, sess, rel, tr in sorted(events):
        now = datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
        st = state.setdefault(key_of(root, sess, tr), {})
        for d in [d for d, e in st.items() if now - e["last"] > TTL]:
            del st[d]
        d = os.path.dirname(rel)
        e = st.setdefault(d, {"files": set(), "last": 0, "hinted": False})
        e["last"] = now
        if rel in e["files"]:
            continue
        e["files"].add(rel)
        if len(e["files"]) >= 5 and not e["hinted"]:
            e["hinted"] = True
            fired.append((ts, root, sess, d, tr))
    return fired

old = replay(lambda root, sess, tr: root)
new = replay(lambda root, sess, tr: (root, sess))
per_transcript = replay(lambda root, sess, tr: (root, tr))
own = collections.defaultdict(set)
own_tr = collections.defaultdict(set)
for ts, root, sess, rel, tr in events:
    own[(root, sess, os.path.dirname(rel))].add(rel)
    own_tr[(root, tr, os.path.dirname(rel))].add(rel)
old_self = sum(1 for ts, root, sess, d, tr in old if len(own[(root, sess, d)]) >= 5)
old_self_tr = sum(1 for ts, root, sess, d, tr in old if len(own_tr[(root, tr, d)]) >= 5)
print(f"transcripts: {len({e[4] for e in events})} ({sum(1 for t in {e[4] for e in events} if '/subagents/' in t)} subagent)")
print(f"reads replayed: {len(events)} in {len({(e[1], e[2]) for e in events})} (project, session) pairs")
print(f"per project (old rule): {len(old)} overviews; to a session with >=5 own reads of that dir in era E: {old_self}")
print(f"per session (D#713):    {len(new)} overviews")
print("old-only (would no longer fire):", len(set((r, s, d) for _, r, s, d, _t in old) - set((r, s, d) for _, r, s, d, _t in new)))
print("new-only (a fanning-out session the old rule skipped):", len(set((r, s, d) for _, r, s, d, _t in new) - set((r, s, d) for _, r, s, d, _t in old)))
print(f"old rule, overviews landing in a transcript (main or one subagent) that itself read >=5 files of the dir: {old_self_tr} of {len(old)}")
print(f"per transcript (if hooks told agents apart): {len(per_transcript)} overviews")
