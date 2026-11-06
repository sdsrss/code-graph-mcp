#!/bin/bash
# par.sh BIN DIR OUT : ivchain each chain in DIR in parallel
B="$(realpath "$1")"; D="$(realpath "$2")"; O="$3"
ls "$D" | xargs -P 8 -I{} sh -c 'echo "{} $(/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness/ivchain.sh '"$B"' '"$D"'/{} 2>&1 | head -1)"' | sort > "$O"
echo "$(grep -c DIFF "$O") DIFF / $(wc -l < "$O")"
