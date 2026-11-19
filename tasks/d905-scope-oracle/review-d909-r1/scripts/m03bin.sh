#!/bin/bash
# Build a debug binary of head with mutant M03 (class lexical scope for every language), restore the source.
set -euo pipefail
SRC=/var/tmp/d909-review/mut-src
cd "$SRC"; pwd
python3 -I - <<'PY'
p = "/var/tmp/d909-review/mut-src/src/parser/treesitter.rs"
s = open(p).read()
old = 'lexical_scope: (language == "python")\n                        .then('
assert s.count(old) == 1
open(p, "w").write(s.replace(old, 'lexical_scope: (language == "python" || true)\n                        .then('))
PY
CARGO_TARGET_DIR=/var/tmp/d909-review/target-m03 cargo build --locked --release --bin code-graph-mcp 2>&1 | tail -2
cp /var/tmp/d909-review/target-m03/release/code-graph-mcp /var/tmp/d909-review/bin/m03
git checkout -- src/parser/treesitter.rs
touch src/parser/treesitter.rs
git status --porcelain | wc -l
