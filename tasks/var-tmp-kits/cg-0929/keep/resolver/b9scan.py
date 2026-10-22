#!/usr/bin/env python3
"""B9 census: cross-file JS/TS call edges whose call site's receiver root is bound,
at file top level, to a package (non-relative, non-Node-builtin specifier).
b9scan.py ARM CORPUS"""
import collections, json, os, re, sqlite3, sys

arm, corpus = sys.argv[1], sys.argv[2]
root = f"{arm}/{corpus}"
db = sqlite3.connect(f"{root}/.code-graph/index.db")
BUILTIN = set("assert async_hooks buffer child_process cluster console constants crypto dgram diagnostics_channel dns domain events fs http http2 https inspector module net os path perf_hooks process punycode querystring readline repl stream string_decoder sys timers tls trace_events tty url util v8 vm wasi worker_threads zlib".split())
# package names this repo defines (workspace packages)
own = set()
for dp, dn, fn in os.walk(root):
    dn[:] = [d for d in dn if d not in ("node_modules", ".git", ".code-graph")]
    if "package.json" in fn:
        try:
            n = json.load(open(os.path.join(dp, "package.json"))).get("name")
            if n: own.add(n)
        except Exception:
            pass
REQ = re.compile(r"""^(?:var|let|const)\s+(\w+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)(\.\w+)*\s*[;,]?\s*$""")
IMP_DEF = re.compile(r"""^import\s+(?:(\w+)\s*,?\s*)?(?:\*\s+as\s+(\w+)|\{([^}]*)\})?\s*from\s*['"]([^'"]+)['"]""")
def bindings(path):
    b = {}
    try:
        lines = open(f"{root}/{path}", encoding="utf-8", errors="replace").read().split("\n")
    except Exception:
        return b, []
    for ln in lines:
        m = REQ.match(ln.strip()) if not ln.startswith((" ", "\t")) else None
        if m:
            b[m.group(1)] = m.group(2)
        m = IMP_DEF.match(ln.strip()) if not ln.startswith((" ", "\t")) else None
        if m:
            spec = m.group(4)
            for g in (m.group(1), m.group(2)):
                if g: b[g] = spec
            if m.group(3):
                for part in m.group(3).split(","):
                    part = part.strip().replace("type ", "")
                    if not part: continue
                    local = part.split(" as ")[-1].strip()
                    b[local] = spec
    return b, lines
def pkg_of(spec):
    if spec.startswith((".", "/")): return None
    parts = spec.split("/")
    return "/".join(parts[:2]) if spec.startswith("@") else parts[0]
cache = {}
rows = collections.Counter()
examples = []
q = """SELECT sf.path, sn.name, sn.start_line, sn.end_line, tf.path, tn.name, tn.type, e.confidence, e.metadata, sf.language
       FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
       JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id
       WHERE e.relation='calls' AND sf.language IN ('javascript','typescript','tsx') AND sf.id<>tf.id"""
for sp, sname, sl, el, tp, tname, ttype, conf, meta, lang in db.execute(q):
    if sp not in cache: cache[sp] = bindings(sp)
    b, lines = cache[sp]
    short = tname.split(".")[-1]
    pat = re.compile(r"([A-Za-z_$][\w$]*)((?:\s*\.\s*[\w$]+)*)\s*\.\s*" + re.escape(short) + r"\s*\(")
    roots = set()
    for i in range(sl - 1, min(el, len(lines))):
        for m in pat.finditer(lines[i]):
            roots.add(m.group(1))
    bare = any(re.search(r"(?<![\w$.])" + re.escape(short) + r"\s*\(", lines[i]) for i in range(sl - 1, min(el, len(lines))))
    pk = {r: b[r] for r in roots if r in b and pkg_of(b[r])}
    if not pk or bare:
        continue
    kinds = {("builtin" if pkg_of(s).removeprefix("node:") in BUILTIN or s.startswith("node:") else "own" if pkg_of(s) in own else "pkg") for s in pk.values()}
    key = "|".join(sorted(kinds))
    rows[(key, conf)] += 1
    examples.append((key, conf, f"{sp}:{sname}", f"{tp}:{tname}({ttype})", pk, meta))
print(f"{corpus}: own packages {sorted(own)[:8]}{'...' if len(own) > 8 else ''} ({len(own)})")
for k, v in sorted(rows.items()): print(f"  {k}: {v}")
for e in examples[:60]: print("   ", e)
