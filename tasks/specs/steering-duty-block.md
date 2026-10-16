---
status: deprecated
revision: 1
---

# Steering block as moment-bound duties (not pursued)

## Goal

Rewrite the CLAUDE.md block from a menu ("who calls X → `callgraph X`") into
duties tied to checkable moments: before changing a signature, renaming or
deleting → `impact` / `refs`; after editing, before tests → `affected`. The
idea came from claude-mem-lite, whose explicit calls are mostly duties its
block assigns (save, defer). Draft: `tasks/steering-duty-block/block-duty.md`.

## Non-goals

- Changing the shipped template without an outcome gain.

## Constraints

- Gate (agreed 2026-10-16): a baseline-only pilot on tasks with headroom
  first; the A/B (~50 sessions per arm) only if the baseline is below ceiling.
  The 2026-10-06 A/B moved usage (19/30 → 26/30) with no outcome change, so
  usage alone does not justify a template change.

## Success criteria

- Baseline below ~0.85 on the pilot cases for at least one model; then the
  duty arm beats the current block on score with a paired test.

## Result (2026-10-16): stopped at the gate

Baseline = the 0.169.0 block in CLAUDE.md (`ab.py --variants claudemd --tags
coding --runs 1`), the five networkx bug-fix cases with hidden tests:

| model | score | used code-graph | turns | cost |
|---|---|---|---|---|
| claude-haiku-5-5 | 5/5 = 1.0 | 0/5 | 10–29 | $0.11 |
| claude-sonnet-5-5 | 5/5 = 1.0 | 0/5 | 5–17 | $0.75 |

Raw rows: `evals/results/2026-10-16T08-40-21Z-steering-ab/` (Haiku; the
`model` field was added to rows after this run) and
`…T08-47-24Z-steering-ab/` (Sonnet), gitignored. No headroom, so no A/B; the
block text stays as shipped. `ab.py` now accepts `claudemd@<file>`, so the A/B
is one command once cases with headroom exist.

## Open questions

- Which edit tasks leave headroom on Haiku/Sonnet? Likely a signature change
  in a dynamic language whose callers grep cannot list (aliases, attribute
  calls, callbacks) — and code-graph must have those edges, or the duty only
  amplifies its gaps (the tokio pilot: Haiku's misses were exactly where the
  graph has no edge).

# Change log

- r1 (2026-10-16): pilot run, stopped at the gate.
