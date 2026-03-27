#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"

# ── 1. Version consistency check ─────────────────────────────
# If any version-bearing file is staged, verify all 4 locations match.
VERSION_FILES=(
  "package.json"
  "Cargo.toml"
  "claude-plugin/.claude-plugin/plugin.json"
  ".claude-plugin/marketplace.json"
)

staged_files=$(git diff --cached --name-only)
version_staged=false
for vf in "${VERSION_FILES[@]}"; do
  if echo "$staged_files" | grep -qF "$vf"; then
    version_staged=true
    break
  fi
done

if $version_staged; then
  # Extract versions from staged content (not working tree)
  v_pkg=$(git show :package.json | node -e "process.stdout.write(JSON.parse(require('fs').readFileSync('/dev/stdin','utf8')).version)")
  v_cargo=$(git show :Cargo.toml | grep -m1 '^version' | sed 's/version = "\(.*\)"/\1/')
  v_plugin=$(git show :"claude-plugin/.claude-plugin/plugin.json" | node -e "process.stdout.write(JSON.parse(require('fs').readFileSync('/dev/stdin','utf8')).version)")
  v_market=$(git show :".claude-plugin/marketplace.json" | node -e "
    const d=JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));
    process.stdout.write(d.metadata.version)
  ")

  mismatch=false
  if [ "$v_pkg" != "$v_cargo" ]; then
    echo "❌ Version mismatch: package.json=$v_pkg vs Cargo.toml=$v_cargo"
    mismatch=true
  fi
  if [ "$v_pkg" != "$v_plugin" ]; then
    echo "❌ Version mismatch: package.json=$v_pkg vs plugin.json=$v_plugin"
    mismatch=true
  fi
  if [ "$v_pkg" != "$v_market" ]; then
    echo "❌ Version mismatch: package.json=$v_pkg vs marketplace.json=$v_market"
    mismatch=true
  fi

  if $mismatch; then
    echo ""
    echo "Fix: node scripts/sync-versions.js $v_pkg"
    echo "Then: git add the updated files"
    exit 1
  fi

  echo "✓ Version consistency: $v_pkg"
fi

# ── 2. Plugin JS tests ───────────────────────────────────────
# Run plugin tests if any JS file under claude-plugin/ is staged.
js_staged=$(echo "$staged_files" | grep -c '^claude-plugin/.*\.js$' || true)
if [ "$js_staged" -gt 0 ]; then
  echo "Running plugin JS tests..."
  for t in "$ROOT"/claude-plugin/scripts/*.test.js; do
    [ -f "$t" ] || continue
    if ! node --test "$t" > /dev/null 2>&1; then
      echo "❌ Test failed: $(basename "$t")"
      echo "Run: node --test $t"
      exit 1
    fi
  done
  echo "✓ Plugin JS tests passed"
fi

# ── 3. Rust checks ───────────────────────────────────────────
# Run cargo check + test if any Rust source is staged.
rs_staged=$(echo "$staged_files" | grep -c '\.rs$' || true)
cargo_staged=$(echo "$staged_files" | grep -c 'Cargo\.\(toml\|lock\)' || true)
if [ "$rs_staged" -gt 0 ] || [ "$cargo_staged" -gt 0 ]; then
  echo "Running cargo check..."
  if ! cargo check --quiet 2>&1; then
    echo "❌ cargo check failed"
    exit 1
  fi
  echo "✓ cargo check passed"

  echo "Running cargo test..."
  if ! cargo test --quiet 2>&1; then
    echo "❌ cargo test failed"
    exit 1
  fi
  echo "✓ cargo test passed"
fi

echo "Pre-commit checks passed."
