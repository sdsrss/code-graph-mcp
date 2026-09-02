#!/bin/bash
set -euo pipefail
VERSION=${1:?Usage: scripts/bump-version.sh <version>}
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Self-heal the commit gate. core.hooksPath is per-clone config that nothing
# forces on a pure-Rust checkout (npm `prepare` never runs without `npm
# install`) — v0.104.0 shipped a red install-e2e §1.9 exactly this way. Wiring
# it here guarantees any machine that cuts a release has the pre-commit gate
# active before the bump commit is made. Covers all worktrees of the clone.
git config core.hooksPath scripts/githooks

# Build the dev binary WITH embed-model so the local MCP server (.mcp.json →
# target/release/code-graph-mcp) keeps producing semantic vectors. A bare bump
# would rebuild a no-embed binary, silently disabling vector search for every dev
# project sharing target/release (see feedback_build_release). Opt out by setting
# SYNC_VERSIONS_FEATURES= (empty) or SYNC_VERSIONS_SKIP_BUILD=1 before invoking.
# `-` (not `:-`) so an explicitly-empty SYNC_VERSIONS_FEATURES= is honored as
# "no features" (matching sync-versions.js, which treats empty as a no-embed
# build); only an UNSET var defaults to embed-model. Safe under `set -u`.
export SYNC_VERSIONS_FEATURES="${SYNC_VERSIONS_FEATURES-embed-model}"
node scripts/sync-versions.js "$VERSION"

# Cargo.lock. The failure is REPORTED rather than swallowed (audit 2026-08-29
# ENG-07): `2>/dev/null || true` printed "Updated Cargo.lock" whether or not
# anything happened, so a cargo that refused (offline, a vendored registry, a
# lock held by another build) looked exactly like a success. Still non-fatal —
# pre-commit compares Cargo.lock's own version against package.json and stops
# the commit — but the person running the bump now learns which one they got.
if cargo_out=$(cargo update -p code-graph-mcp 2>&1); then
  echo "Updated Cargo.lock"
else
  echo "⚠ cargo update failed — Cargo.lock still holds the OLD version." >&2
  echo "$cargo_out" | tail -3 >&2
  echo "  The pre-commit version check will refuse the commit until it is fixed." >&2
fi

echo ""
echo "All versions updated to $VERSION"
echo "Next: git add -A && git commit -m 'chore: bump to $VERSION' && git tag v$VERSION && git push && git push --tags"
