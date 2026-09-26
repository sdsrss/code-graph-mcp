#!/usr/bin/env bash
# Shared workspace for every case: this repo's src/ at a pinned commit, as a
# one-commit git repo. Pinned so the answers the graders check never drift.
# Sourced by each case's scaffold.sh; runs in the empty workspace, as you,
# outside the agent's sandbox, in BOTH arms (claude plugin eval --scaffold).
# evals/run.sh writes env.sh (CG_EVAL_REPO, CG_EVAL_NATIVE) into the staged copy:
# the eval passes a scaffold only an allowlisted environment.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/env.sh"
FIXTURE_COMMIT=6f0f6a2
git -C "${CG_EVAL_REPO:?run through evals/run.sh}" archive "$FIXTURE_COMMIT" src | tar -x
git init -q
git add -A
git -c user.email=eval@example.invalid -c user.name=eval commit -qm fixture

# Where a real install keeps the binary (the plugin's hooks, MCP launcher and
# CLI launcher all find it there). Present in the no-plugin arm too, where
# nothing on PATH or in context points at it.
mkdir -p "$HOME/.cache/code-graph/bin"
cp "${CG_EVAL_NATIVE:?run through evals/run.sh}" "$HOME/.cache/code-graph/bin/code-graph-mcp"
