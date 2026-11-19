#!/usr/bin/env bash
# deadab.sh <base> <head> <corpus> <work>: dead-code --json --min-lines 1 on fresh indexes of both arms, diff of reported names
set -euo pipefail
B="$1"; H="$2"; C="$3"; W="${4:?}"
n="$(basename "$C")"
for arm in base head; do
  bin="$B"; [ "$arm" = head ] && bin="$H"
  d="${W:?}/$n-$arm"; rm -rf "${d:?}"; cp -a "$C" "$d"; rm -rf "${d:?}/.code-graph"
  (cd "$d" && CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1 "$bin" incremental-index --no-embed --quiet && CODE_GRAPH_NO_AUTO_UPDATE=1 "$bin" dead-code --json --min-lines 1 --no-ignore > "$W/$n-$arm.dead.json" 2>/dev/null) || true
  rm -rf "${d:?}"
done
python3 -I - "$W/$n-base.dead.json" "$W/$n-head.dead.json" "$n" <<'PY'
import json, sys
def load(p):
    try: d = json.load(open(p))
    except Exception as e: return None, str(e)
    items = d if isinstance(d, list) else d.get("results") or d.get("dead_code") or d.get("items") or []
    out = set()
    def walk(x):
        if isinstance(x, dict):
            if "name" in x and ("file" in x or "file_path" in x or "path" in x):
                out.add((x.get("file") or x.get("file_path") or x.get("path"), x.get("qualified_name") or x["name"], x.get("type") or x.get("node_type"), x.get("category") or x.get("kind")))
            for v in x.values(): walk(v)
        elif isinstance(x, list):
            for v in x: walk(v)
    walk(d)
    return out, None
b, eb = load(sys.argv[1]); h, eh = load(sys.argv[2])
if b is None or h is None: print(sys.argv[3], "parse error", eb, eh); sys.exit()
print(f"{sys.argv[3]}: dead-code {len(b)} -> {len(h)}; head-only {len(h-b)}, base-only {len(b-h)}")
for x in sorted(h - b, key=str)[:25]: print("   + ", x)
for x in sorted(b - h, key=str)[:10]: print("   - ", x)
PY
