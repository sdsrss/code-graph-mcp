#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"

# Strip the GIT_* vars git exports into this hook before running ANY subprocess
# that shells out to git for tempdir fixtures — both the JS tests (§2) and the
# Rust tests (§3). Without this, a partial `git commit` leaks GIT_DIR /
# GIT_INDEX_FILE / GIT_WORK_TREE into the child, which then operates on THIS
# repo's index instead of its tempdir: `git add .` records a fixture file into
# the real index and the fixture commit comes back empty. v0.80.3 fixed this for
# the cargo path only; the JS section was the sibling hole (H4), so the stripper
# is hoisted here and applied to both sections.
git_clean=(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY -u GIT_COMMON_DIR -u GIT_NAMESPACE -u GIT_PREFIX)

# ── 1. Version consistency check ─────────────────────────────
# If any version-bearing file is staged, verify EVERY location sync-versions.js
# writes matches the staged package.json: Cargo.toml, plugin.json, marketplace.json
# (metadata.version AND plugins[0].version — the field the marketplace actually
# reads), the 5 npm/<plat>/package.json, and package.json's 5 optionalDependencies
# pins. The old guard checked only 4 — a lagging linux-arm64 pin or plugins[0].version
# (no CI runner for either) shipped a release npm/marketplace couldn't resolve.
VERSION_FILES=(
  "package.json"
  "Cargo.toml"
  "claude-plugin/.claude-plugin/plugin.json"
  ".claude-plugin/marketplace.json"
  "npm/linux-x64/package.json"
  "npm/linux-arm64/package.json"
  "npm/darwin-x64/package.json"
  "npm/darwin-arm64/package.json"
  "npm/win32-x64/package.json"
  # The shipped snapshot workflow pins the npm package it runs. sync-versions.js
  # rewrites that pin like every other site; this guard did not check it, so a
  # hand-edited template committed on its own passed here and was only caught by
  # release.yml re-running sync-versions at tag time (audit 2026-08-29 ENG-07).
  # Kept in step with sync-versions.js by `pre_commit_checks_every_version_site`
  # in tests/hardening.rs, so site #11 cannot be added to one list alone.
  "claude-plugin/templates/code-graph-snapshot.yml"
)

staged_files=$(git diff --cached --name-only)
version_staged=false
for vf in "${VERSION_FILES[@]}"; do
  # `grep -F` without `-q`, output discarded: `-q` exits on the first match, and
  # once "$staged_files" outgrows the pipe buffer `echo` takes SIGPIPE, which
  # `set -o pipefail` turns into a failed pipeline — inverting the test exactly
  # when it matched. Same shape as the two probes in §3/§4 below.
  #
  # Onset is far lower than it looks: measured green at 500 staged paths (22 KB)
  # and INVERTED (exit 141) at 1,000 (44 KB) — an ordinary vendored-directory or
  # generated-code commit, so this gate was plausibly already dark in practice.
  # And the MATCH POSITION decides it: with the match on the last line `grep -q`
  # must read to EOF and stays correct at any size, which is why a casual
  # reproduction at multi-MB can show nothing and conclude there is no bug.
  if echo "$staged_files" | grep -F "$vf" >/dev/null; then
    version_staged=true
    break
  fi
done

if $version_staged; then
  # Read versions from STAGED content (git show :<path>), so a partial bump is
  # caught before it lands. $2 is a JS accessor (.version / .metadata.version / ...).
  read_json_ver() { git show ":$1" | node -e "process.stdout.write(String(JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'))$2))"; }
  ref=$(read_json_ver package.json .version)
  mismatch=false
  report() { if [ "$2" != "$ref" ]; then echo "❌ Version mismatch: $1=$2 vs package.json=$ref"; mismatch=true; fi; }

  report Cargo.toml "$(git show :Cargo.toml | grep -m1 '^version' | sed 's/version = "\(.*\)"/\1/')"
  report plugin.json "$(read_json_ver 'claude-plugin/.claude-plugin/plugin.json' .version)"
  report marketplace.metadata "$(read_json_ver '.claude-plugin/marketplace.json' .metadata.version)"
  report 'marketplace.plugins[0]' "$(read_json_ver '.claude-plugin/marketplace.json' '.plugins[0].version')"
  for plat in linux-x64 linux-arm64 darwin-x64 darwin-arm64 win32-x64; do
    report "npm/$plat" "$(read_json_ver "npm/$plat/package.json" .version)"
  done
  # package.json optionalDependencies pins (npm install resolves the platform binary via these)
  while IFS=$'\t' read -r dep ver; do
    [ -n "$dep" ] && report "optionalDependencies[$dep]" "$ver"
  done < <(git show :package.json | node -e "const d=JSON.parse(require('fs').readFileSync('/dev/stdin','utf8')).optionalDependencies||{};for(const[k,v]of Object.entries(d))process.stdout.write(k+'\t'+v+'\n')")

  # Cargo.lock's own code-graph-mcp package version. A `SYNC_VERSIONS_SKIP_BUILD=1`
  # bump never re-runs cargo, so the lockfile can stay stale even when Cargo.toml is
  # bumped — grab the `version =` line that follows the `name = "code-graph-mcp"`
  # package entry from staged content and compare it against the reference.
  report Cargo.lock "$(git show :Cargo.lock | awk '/^name = "code-graph-mcp"$/{f=1;next} f&&/^version = /{gsub(/version = "|"/,"");print;exit}')"

  # The snapshot template's npm pin. Extracted with the SCOPED name in the
  # pattern on purpose: the unscoped `code-graph-mcp` on npm belongs to someone
  # else, so a rewrite that lands there must not look like a matching version.
  report snapshot-template "$(git show :claude-plugin/templates/code-graph-snapshot.yml | grep -oE '@sdsrs/code-graph@[0-9]+\.[0-9]+\.[0-9]+' | head -1 | sed 's/.*code-graph@//')"

  if $mismatch; then
    echo ""
    echo "Fix: node scripts/sync-versions.js $ref"
    echo "Then: git add the updated files"
    exit 1
  fi

  echo "✓ Version consistency: $ref (Cargo + plugin + marketplace×2 + 5 npm + optionalDeps + snapshot template)"
fi

# ── 2. Plugin JS tests ───────────────────────────────────────
# Run all JS tests if any JS file under claude-plugin/ or scripts/ is staged.
# scripts/*.test.js included since v0.49.1: install-e2e.test.js asserts
# lifecycle.js behavior but lived outside this gate (and outside CI), so the
# v0.32.0 hooks-to-settings.json inversion silently stranded it for 3 months.
js_staged=$(echo "$staged_files" | grep -c '^\(claude-plugin\|scripts\)/.*\.js$' || true)
if [ "$js_staged" -gt 0 ]; then
  echo "Running plugin JS tests..."
  # `find`, not a `*.test.js` glob: the glob is NON-RECURSIVE and would skip the
  # first test file anyone puts in a subdirectory, in silence. Note the staged-file
  # detection above is already recursive (`.*\.js$`), so the asymmetry meant a
  # nested test could TRIGGER this gate without ever being RUN by it.
  # Fed by process substitution, NOT a pipe: a pipe would put the loop in a
  # subshell where `exit 1` only leaves the subshell and the hook would pass a
  # failing test.
  while IFS= read -r t; do
    [ -f "$t" ] || continue
    if ! "${git_clean[@]}" node --test "$t" > /dev/null 2>&1; then
      echo "❌ Test failed: $(basename "$t")"
      echo "Run: node --test $t"
      exit 1
    fi
  done < <(find "$ROOT/claude-plugin/scripts" "$ROOT/scripts" -type f -name '*.test.js' | sort)
  echo "✓ Plugin JS tests passed"
fi

# ── 3. Rust checks ───────────────────────────────────────────
# Run cargo check + test if any Rust source is staged.
rs_staged=$(echo "$staged_files" | grep -c '\.rs$' || true)
cargo_staged=$(echo "$staged_files" | grep -c 'Cargo\.\(toml\|lock\)' || true)
if [ "$rs_staged" -gt 0 ] || [ "$cargo_staged" -gt 0 ]; then
  # git_clean (defined at the top) strips GIT_* so cargo tests that shell out to
  # git for fixtures (src/snapshot/tests.rs init_git_fixture) run against their
  # tempdir, not this repo's index.
  echo "Running cargo check..."
  if ! "${git_clean[@]}" cargo check --quiet 2>&1; then
    echo "❌ cargo check failed"
    exit 1
  fi
  echo "✓ cargo check passed"

  echo "Running cargo test..."
  if ! "${git_clean[@]}" cargo test --quiet 2>&1; then
    echo "❌ cargo test failed"
    exit 1
  fi
  echo "✓ cargo test passed"

  # fmt + clippy. Both are CI gates, and neither ran here — the asymmetry has
  # cost this repo a re-tagged release once already, because a commit that
  # passes locally and fails in CI is only discovered after the push (audit
  # 2026-08-16 §四). `--all-targets` matches ci.yml: without it clippy lints
  # lib+bins only and a test-only warning still lands.
  #
  # Ordered after check/test on purpose: they share the compilation the slow
  # step already did, so this adds seconds rather than a second build.
  echo "Running cargo fmt --check..."
  if ! "${git_clean[@]}" cargo fmt --all -- --check; then
    echo "❌ cargo fmt --check failed — run: cargo fmt --all"
    exit 1
  fi
  echo "✓ cargo fmt passed"

  echo "Running cargo clippy..."
  if ! "${git_clean[@]}" cargo clippy --all-targets --quiet -- -D warnings 2>&1; then
    echo "❌ cargo clippy failed"
    exit 1
  fi
  echo "✓ cargo clippy passed"

  # The embed-model leg. Everything above runs with `default = []`, so anything
  # inside `#[cfg(feature = "embed-model")]` is not even COMPILED here — while
  # ci.yml and the release gate build both legs. A warning or type error in that
  # code is therefore locally green and remotely red, which for a solo dev
  # pushing straight to main means "main is red" (v0.128.0 instance; audit
  # 2026-09-02 P2-7).
  #
  # Conditional, but the condition is "could this commit touch embed-gated code",
  # NOT "is it in src/embedding/". The first cut used only the path prefix plus a
  # `cfg(feature` scan of a `-U0` diff, and pre-tag review showed that misses the
  # majority of the gated code: 17 files outside src/embedding/ carry
  # `embed-model` gating (src/mcp/server/mod.rs alone has 11), and an edit to a
  # line INSIDE such a block never puts the attribute line in a -U0 diff. Both
  # Rust commits of the batch that added this gate evaluated false.
  #
  # So leg 3 asks the question directly: does any staged .rs file CONTAIN
  # `embed-model` gating? The original objection to a broad trigger was that
  # "candle is a multi-minute build" — true cold, but measured warm on this repo
  # at 7.6s for the whole `clippy --features embed-model --all-targets` leg. A
  # 7.6s gate does not get disabled; a gate that misses most of what it guards is
  # already disabled.
  embed_touched=false
  if echo "$staged_files" | grep -E '^(src/embedding/|Cargo\.(toml|lock))' >/dev/null; then
    embed_touched=true
  else
    # Captured, then matched in-shell — the strictest form of the same fix the
    # `grep` probes above use. `git diff | grep -q` would be the same pipefail
    # trap: `-q` exits on the first match, git upstream takes SIGPIPE once the
    # diff outgrows the pipe buffer, and pipefail reports the pipeline as failed,
    # flipping this flag OFF exactly when it fired. Matching the captured string
    # in-shell has no pipe and no early exit. (`[[ == *lit* ]]` is linear —
    # measured ~7 ms/MB, 432 ms on a 60 MB diff — so this is not a cost.)
    #
    # `|| true` is deliberately NOT used: a `git diff` that fails would yield an
    # empty string, no match, and a silently un-gated commit — the exact shape
    # this section condemns. A failure here fails the hook instead.
    staged_diff=$("${git_clean[@]}" git diff --cached -U0)
    if [[ $staged_diff == *"cfg(feature"* ]]; then
      embed_touched=true
    else
      # Leg 3: staged .rs files whose CONTENT is embed-gated, read from the
      # staged blob so an unstaged edit cannot change the verdict.
      while IFS= read -r f; do
        case "$f" in
          *.rs) ;;
          *) continue ;;
        esac
        if git show ":$f" 2>/dev/null | grep -F 'embed-model' >/dev/null; then
          embed_touched=true
          break
        fi
      done <<< "$staged_files"
    fi
  fi
  if $embed_touched; then
    echo "Running cargo clippy --features embed-model..."
    if ! "${git_clean[@]}" cargo clippy --features embed-model --all-targets --quiet -- -D warnings 2>&1; then
      echo "❌ cargo clippy (embed-model leg) failed"
      exit 1
    fi
    echo "✓ cargo clippy (embed-model leg) passed"
  fi
fi

# ── 4. Rust guards over NON-Rust files ───────────────────────
# `tests/hardening.rs` and `tests/doc_cli_alignment.rs` read — and in three cases
# EXECUTE under node — files that are not Rust: README.md, .github/workflows/*,
# this very hook, the plugin templates/skills/agents, and every `*.js` /
# `*.test.js` under `claude-plugin/scripts/` and `scripts/`. Section 3 only fires
# on staged `.rs`/`Cargo.*`, so a yml-only or README-only commit ran NONE of them
# locally — the guards that exist precisely to catch edits to those files were
# the ones that did not run (audit 2026-09-02 P2-8). Skipped when section 3
# already ran: its `cargo test` is a superset.
#
# The first cut of this list named only `scripts/sync-versions.js` and the
# plugin templates, which left the two script trees uncovered — and pre-tag
# review replayed it against this gate's OWN batch: the security commit
# (`claude-plugin/scripts/recommendation-log.js`) did not trigger section 4. The
# guard sites that were uncovered: hardening.rs:625 runs `project-root.js` under
# node, doc_cli_alignment.rs:285 runs `adopt.js` to render the shipped CLAUDE.md
# block, hardening.rs:1545 walks both trees for `*.test.js`, and
# doc_cli_alignment.rs:789 walks both for every `*.js` (the README env-var
# table). Hence `(claude-plugin/)?scripts/` rather than two named files.
#
# `CHANGELOG.md` is deliberately NOT here: `grep -rn CHANGELOG` over both guard
# files returns nothing, so listing it only bought a full `cargo test` build on
# every CHANGELOG-only commit. It was in the first cut, with a comment claiming
# the guards read it.
if [ "$rs_staged" -eq 0 ] && [ "$cargo_staged" -eq 0 ] && \
   echo "$staged_files" | grep -E '^(\.github/|README\.md|(claude-plugin/)?scripts/|claude-plugin/(templates|skills|agents)/|\.claude-plugin/)' >/dev/null; then
  echo "Running Rust guards over non-Rust files..."
  if ! "${git_clean[@]}" cargo test --quiet --test hardening --test doc_cli_alignment 2>&1; then
    echo "❌ hardening / doc_cli_alignment guards failed"
    exit 1
  fi
  echo "✓ Rust guards passed"
fi

echo "Pre-commit checks passed."
