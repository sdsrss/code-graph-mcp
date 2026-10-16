---
status: approved
revision: 2
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
  and 6 in subagents; a code-graph call followed within 10 tool calls 0/45.
  Within those 10 calls, 23/45 read or named the defining file (2/45 as the
  very next call), e.g. `grep -n "fn resolve_python_module_targets" -B25 -A45
  src/indexer/pipeline/python_modules.rs`.
- `show <X> --file <F> --json` on the 61 real events since 2026-09-30 (54
  distinct (project, symbol, file) pairs): 58 one definition, 3 several
  (`model` ×3, `storage` ×12, test fixtures), 0 none. Signatures run to 900+
  characters over many lines (FastAPI `Query` defaults), classes have none,
  and C/C++ write the return type first (`char (int a)`, review 2026-10-16).

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

  | `show --json --compact` result | injected |
  |---|---|
  | exactly one row with `file_path == F` and name `X`, `*.X` or `*::X` | `F:start-end`, `: <type> <label>` |
  | … signature starting with `(` | label `<name><signature>` |
  | … signature ending with `param_types` (C/C++ `ret (params)`) | label `<ret> <name><params>` |
  | … any other signature, or `null` (class, Kotlin, Swift, Bash) | label `<name>` |
  | several such rows (fixtures, cfg twins, overloads) | `F`, no lines, no signature |
  | none / exit ≠ 0 / not JSON / no binary / budget gone | `F`, no lines, no signature |
  | whitespace, including U+0085 | collapsed to single spaces; `( ` → `(`, `, )` / ` )` → `)` (also inside literals) |
  | `<type> <label>` longer than 160 characters | cut to 160, ends with `…` |

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
- r2 (2026-10-16): pre-ship review round 1 (0 HIGH, 2 MEDIUM). C/C++
  return-type-first signatures; `--compact` (no `code_content` in the rows);
  U+0085 collapsed; the 160 cap is on the whole label; evidence recounted
  (61 events / 54 pairs; 23/45 touched the definition within 10 calls, not
  "the next step"). Left as LOW: a signature whose source text holds a
  `code-graph-mcp` command shows it; a quoted path puts `:lines` outside the
  quotes.
