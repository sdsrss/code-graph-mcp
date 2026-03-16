#!/bin/bash
set -euo pipefail
VERSION=${1:?Usage: scripts/bump-version.sh <version>}
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

node scripts/sync-versions.js "$VERSION"

# Cargo.lock
cargo update -p code-graph-mcp 2>/dev/null || true
echo "Updated Cargo.lock"

echo ""
echo "All versions updated to $VERSION"
echo "Next: git add -A && git commit -m 'chore: bump to $VERSION' && git tag v$VERSION && git push && git push --tags"
