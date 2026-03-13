#!/bin/bash
set -euo pipefail
VERSION=${1:?Usage: scripts/bump-version.sh <version>}
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# 1. Cargo.toml (only match version in [package] section, before first [*] section)
sed -i '1,/^\[/s/^version = ".*"/version = "'"$VERSION"'"/' Cargo.toml
echo "Updated Cargo.toml → $VERSION"

# 2. Cargo.lock
cargo update -p code-graph-mcp 2>/dev/null || true
echo "Updated Cargo.lock"

# 3. Root package.json (version + optionalDependencies)
sed -i 's/"version": ".*"/"version": "'"$VERSION"'"/' package.json
sed -i 's/"@sdsrs\/code-graph-\([^"]*\)": "[^"]*"/"@sdsrs\/code-graph-\1": "'"$VERSION"'"/' package.json
echo "Updated package.json → $VERSION"

# 4. Platform packages
for pkg in npm/*/package.json; do
  sed -i 's/"version": ".*"/"version": "'"$VERSION"'"/' "$pkg"
done
echo "Updated npm/*/package.json → $VERSION"

# 5. Plugin manifest
sed -i 's/"version": ".*"/"version": "'"$VERSION"'"/' claude-plugin/.claude-plugin/plugin.json
echo "Updated plugin.json → $VERSION"

echo ""
echo "All versions updated to $VERSION"
echo "Next: git add -A && git commit -m 'chore: bump to $VERSION' && git tag v$VERSION && git push && git push --tags"
