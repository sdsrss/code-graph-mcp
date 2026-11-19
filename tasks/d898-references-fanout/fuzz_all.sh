#!/usr/bin/env bash
# fuzz_all.sh <binary> <out-dir>: every random-sequence generator of D#893/D#901
# against one binary, compared with a rebuild after every step: the default
# generator 4,500 seeds (3 steps), gen_review 1,500 (4 steps), each language
# generator in tasks/d893-fuzz/gens/ 1,500 (3 steps). Compare two out-dirs with
# `python3 -I tasks/d893-fuzz/fuzzcmp.py base/<g>.jsonl head/<g>.jsonl`.
# ONLY="php ruby" runs only those language generators.
set -euo pipefail
BIN="$(realpath "${1:?binary}")"
OUT="${2:?out-dir}"
R="$(cd "$(dirname "$0")/../d893-fuzz" && pwd)"
mkdir -p "$OUT"
export FUZZ_WORK="${FUZZ_WORK:?set FUZZ_WORK to an agent-tmp work dir}"
mkdir -p "$FUZZ_WORK"
if [ -z "${ONLY:-}" ]; then
  python3 -I "$R/fuzz_seq.py" "$BIN" 0 4500 "$OUT/own.jsonl" --jobs 12
  python3 -I "$R/fuzz_seq.py" "$BIN" 0 1500 "$OUT/review.jsonl" --jobs 12 --steps 4 --gen "$R/gen_review.py"
fi
for g in "$R"/gens/gen_*.py; do
  n="$(basename "$g" .py)"
  # gen_multi.py is the template the per-language copies come from (its
  # language is its file name's tail); it runs as none of them.
  [ "$n" = gen_multi ] && continue
  [ -n "${ONLY:-}" ] && ! grep -qw "${n#gen_}" <<<"$ONLY" && continue
  python3 -I "$R/fuzz_seq.py" "$BIN" 0 1500 "$OUT/${n#gen_}.jsonl" --jobs 12 --gen "$g"
done
