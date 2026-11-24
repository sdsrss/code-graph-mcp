#!/usr/bin/env python3
"""Signal 3 for 0.205.0: published vs source-built `impact --json`, byte for byte.

    published_vs_source.py CORPUS PUBLISHED_BINARY SOURCE_BINARY

Same seeded 150-node sample as measure.py; each node at --depth 3 and 10.
"""
import json, os, random, sqlite3, subprocess, sys
corpus, pub, src = sys.argv[1:4]
TEST = ("/tests/", "/test/", "test_", "_test.", "conftest", ".test.", ".spec.", "__tests__")
db = sqlite3.connect(f"file:{os.path.join(corpus, '.code-graph', 'index.db')}?immutable=1", uri=True)
rows = db.execute("SELECT n.id, n.name, f.path FROM nodes n JOIN files f ON f.id = n.file_id "
    "WHERE n.type IN ('function','method') AND n.is_test = 0 AND f.path <> '<external>' "
    "AND EXISTS (SELECT 1 FROM edges e WHERE e.target_id = n.id AND e.relation = 'calls') ORDER BY n.id").fetchall()
rows = [r for r in rows if not any(t in r[2] for t in TEST)]
random.Random(20261124).shuffle(rows)
env = {**os.environ, "CODE_GRAPH_NO_AUTO_UPDATE": "1"}
same = diff = flagged = 0
for nid, _, _ in rows[:150]:
    for depth in ("3", "10"):
        a = subprocess.run([pub, "impact", "--node-id", str(nid), "--depth", depth, "--json"], cwd=corpus, capture_output=True, text=True, env=env).stdout
        b = subprocess.run([src, "impact", "--node-id", str(nid), "--depth", depth, "--json"], cwd=corpus, capture_output=True, text=True, env=env).stdout
        if a == b: same += 1
        else: diff += 1
        if depth == "3" and '"callers_truncated":true' in a.replace(" ", ""): flagged += 1
print(json.dumps({"pool": len(rows), "pairs_compared": same + diff, "byte_identical": same, "differ": diff, "flagged_at_depth3_published": flagged}))
