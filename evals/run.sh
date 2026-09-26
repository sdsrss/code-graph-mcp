#!/usr/bin/env bash
# Run the plugin eval suite with and without the plugin, and report the delta.
#
#   evals/run.sh [claude plugin eval options...]
#   evals/run.sh --case callers-direct --runs 1        # one cheap trial
#   evals/run.sh --model claude-sonnet-5 --max-cost-usd 20
#
# Every run is a paid model call. See evals/README.md for what this measures
# and where the eval harness differs from a real session.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
binary="${CG_EVAL_BINARY:-$repo/target/release/code-graph-mcp}"
work="${CG_EVAL_WORK_DIR:-/var/tmp/code-graph-eval}"
# The agent's Bash sandbox can read the directories on PATH only when they sit
# under $HOME (/var/tmp and /tmp are denied outright), so the tools live there.
tools="${CG_EVAL_TOOLS_DIR:-$HOME/.cache/code-graph-eval/bin}"
# A real install lives under ~/.claude/plugins/, and the plugin keys behavior
# on that path: SessionStart adopts the project's CLAUDE.md only in plugin mode
# (adopt.js isPluginModeInstall). Loading claude-plugin/ from this checkout
# would also let every launcher resolve target/release as a dev build. So the
# plugin under test is a copy staged at a plugin-mode path, with the suite
# inside it where claude plugin eval looks for it.
plugin="$work/stage/.claude/plugins/code-graph-mcp"

[ -x "$binary" ] || { echo "no binary at $binary (cargo build --release, or set CG_EVAL_BINARY)" >&2; exit 1; }
want="$(node -p "require('$repo/claude-plugin/.claude-plugin/plugin.json').version")"
have="$("$binary" --version | awk '{print $2}')"
[ "$want" = "$have" ] || { echo "binary is $have, plugin is $want — rebuild or set CG_EVAL_BINARY" >&2; exit 1; }

rm -rf "${plugin:?}"
mkdir -p "$plugin" "$tools/.native"
cp -a "$repo/claude-plugin/." "$plugin/"
find "$plugin" -name '*.test.js' -delete
mkdir -p "$plugin/evals"
cp -a "$repo/evals/." "$plugin/evals/"
rm -rf "${plugin:?}/evals/results"

# The agent's Bash runs in an OS sandbox that can read only the workspace, the
# plugin directory and the PATH directories under $HOME. node on this machine
# is a symlink into a directory the sandbox cannot read, so it is copied.
cp -L "$(command -v node)" "$tools/node"
cp "$binary" "$tools/.native/code-graph-mcp"
printf 'CG_EVAL_REPO=%q\nCG_EVAL_NATIVE=%q\n' "$repo" "$tools/.native/code-graph-mcp" > "$plugin/evals/_fixture/env.sh"

# claude plugin eval does not put <plugin-root>/bin on the Bash PATH the way a
# real session does, and it inherits this shell's PATH in BOTH arms. So PATH is
# rebuilt from scratch (no installed code-graph-mcp can leak into the no-plugin
# arm) and this shim stands in for the plugin's bin/ entry. It answers only
# where the plugin's SessionStart hook has run, which is the with-plugin arm.
cat > "$tools/code-graph-mcp" <<SHIM
#!/bin/sh
if [ ! -f "\$HOME/.cache/code-graph/install-manifest.json" ]; then
  echo "sh: code-graph-mcp: command not found" >&2
  exit 127
fi
exec "$tools/node" "$plugin/bin/code-graph-mcp" "\$@"
SHIM
chmod 755 "$tools/code-graph-mcp"

results="$repo/evals/results/$(date -u +%Y-%m-%dT%H-%M-%SZ)"
tmp_base="${TMPDIR:-/tmp}"
before="$work/tmp-before.txt"
{ ls -d "$tmp_base"/claude-eval-* 2>/dev/null || true; } > "$before"
cd "$work"
# --allow-real-servers: a real session always starts the plugin's MCP server,
# and its `instructions` field is one of the plugin's steering surfaces. It
# runs as you, outside the sandbox — this is our own server.
status=0
env PATH="$tools:/usr/local/bin:/usr/bin:/bin" \
  claude plugin eval "$plugin" \
  --scaffold --trust-plugin --no-publish --allow-real-servers \
  --output-dir "$results" \
  --allow-tools Bash "mcp__plugin_code-graph-mcp_code-graph__*" \
  "$@" || status=$?

"$repo/evals/_fixture/reap.sh" "$tmp_base" "$before" "$plugin" || true
exit "$status"
