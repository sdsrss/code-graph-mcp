---
status: approved
revision: 1
---

# Outside-path grep inject: the definition, not a command to run

## Goal

When a grep misses and the symbol it looked for is defined outside the
grep's path, PostToolUse injects one line (B11, 0.164.0):

    [code-graph] X is defined in F, outside the path your grep searched (P). Its callers and callees: code-graph-mcp callgraph X

Replace the command with the answer the model goes after next: the
definition's line range and signature.

    [code-graph] X is defined in F:120-145, outside the path your grep searched (P): method Cls.X(self, a: int) -> str

## Evidence (2026-10-16, `scripts/plugin_usage.py` + transcript replay)

- Since 0.164.0 the pointer fired 39 times in neutral-project main sessions
  and 6 in subagents; a code-graph call followed within 10 tool calls 0/39
  and 0/6. Across all projects from 2026-09-30: 60 events, 4 followed by any
  code-graph call, 26 by a read of the defining file, e.g. `grep -n "fn
  resolve_python_module_targets" -B25 -A45 src/indexer/pipeline/python_modules.rs`.
- `show <X> --file <F> --json` on all 60 real (project, symbol, file) pairs:
  57 one definition, 3 several (`model` ×3, `storage` ×12, test fixtures),
  0 none. Signatures run to 900+ characters over many lines (FastAPI `Query`
  defaults), classes have none.

## Non-goals

- The in-path call-graph inject, the grep/show echo, the record fields and
  the funnel buckets (`mode: callgraph`, `scope: outside`) stay as they are.
- Symbol choice (`extractCallgraphSymbols`) is not touched. It picks a
  fragment of a file name or hyphenated word (`install` from `install.lock`,
  `record` from `re-record`) in 4 of 60 events; deferred, its own change.
- No caller list: 4 of 60 events led to any code-graph call; the 0.164.0
  B11 review already judged the caller tree noise on a missed grep.

## Constraints

- One extra binary call, only on the outside branch, through `runCg`, so it
  shares the hook's 5 s budget; out of budget or any failure → the location
  sentence without lines and without a command (never an error, never a
  delay past the budget).
- Accepted shapes for the lookup (table-driven test before the code):

  | `show --json` result | injected |
  |---|---|
  | exactly one row with `file_path == F` and name `X`, `*.X` or `*::X` | `F:start-end`, `: <type> <name><signature>` |
  | several such rows (fixtures, cfg twins, overloads) | `F`, no lines, no signature |
  | none / exit ≠ 0 / not JSON / no binary / budget gone | `F`, no lines, no signature |
  | signature `null` (class) | `: class Name` |
  | signature with newlines / runs of spaces | collapsed to single spaces |
  | signature + name longer than 160 characters | cut to 160, ends with `…` |

- The line stays one line; file and search path keep `shellQuoteArg`.

## Success criteria

- Table-driven unit test over the shapes above fails before the change and
  passes after; existing B11 tests updated (no `code-graph-mcp callgraph` in
  the pointer); in-path tree test unchanged and green.
- e2e (stub binary answering `callgraph` and `show --json`): outside grep →
  `F:N-M` and the signature; `show` exit 1 → location sentence only.
- Full JS suite and `cargo test --test hardening --test doc_cli_alignment`
  green; the replayed 60 real pairs produce 57 line-range texts and 3
  location-only texts.
- After release, `plugin_usage.py` shows the outside-path pointer gone from
  the pointer table, and a follow-up read of the defining file is measured
  as before/after share (monitor, not a gate).

## Open questions

None.

## Plan

1. `cg-answer.js`: `runDefinitionAnswer({cwd, symbol, file})` → `{status:
   'hits', start, end, label}` | `{status: 'several' | 'no-hits' |
   'unavailable' | 'no-binary'}`; exported. Shape test first.
2. `post-grep-inject.js`: `buildPointerText(symbol, file, searchPath, def)`
   renders the table; the outside branch calls `runDefinitionAnswer` once.
3. Tests: update the two B11 tests, add the shape table and the e2e pair;
   run the JS suite and the hardening guards.
4. CHANGELOG `## Unreleased`; pre-ship review by a fresh subagent with the
   shape table and this evidence; stop line: a HIGH in round 2 inside round
   1's fix → withdraw, do not patch.

# Change log

- r1 (2026-10-16): initial, from the usage audit.
