# D#347: does the post-grep hook pick the grepped symbol? (2026-11-24)

`scan.py OUT.json [--since 2026-10-17]` lists every post-grep injection that
names a symbol (the one-line outside-path pointer and the cross-file call
graph) in interactive transcripts on this machine, with the grep command, the
picked symbol, where it sits in the command, and whether any of the next 15
tool calls mention the symbol or open the definition file. The JSON stays in
the scratchpad: it carries identifiers from a private project.

The pick logic (`extractCallgraphSymbols`) is unchanged since ce20553f
(2026-07-03), so every event in the window ran the same rule.

## Result, 2026-10-17 .. 2026-11-24 (530 sessions)

Each of the 70 picks was judged by hand against its grep command.

| | neutral projects | own repos | all |
|---|---|---|---|
| picks | 43 | 27 | 70 |
| right symbol, right definition | 37 | 21 | 58 |
| wrong symbol (a word the grep did not look for as a symbol) | 3 | 2 | 5 |
| right symbol, a namesake's definition in another file | 3 | 4 | 7 |
| wrong picks followed by opening the named file | 0 of 6 | 0 of 6 | 0 of 12 |
| right picks followed by opening the named file | 17 of 37 | 14 of 21 | 31 of 58 |

- Acted on, checked a second way: for each of the 12 wrong picks, up to the
  next human prompt (at most 15 tool calls), no tool call names the pointed
  file by path or file name, and no assistant text names the picked symbol
  or that file (the one text hit, after `module`, is the model's own
  `module.exports` edit). An influence that leaves neither trace is outside
  what a transcript shows.
- Wrong symbol: `module` from `module.exports`, `describe` from
  `describe('…` (a test-framework global with a project namesake), a word
  from a URL path in an alternation, a generic lower-case word in an
  alternation, a pandas method name.
- Namesake definition: `grep "fn setup\|…" src/graph/query.rs` pointed at a
  Python `setup` in a review script; `fn fixture`, `def render`, a test
  helper, a private method of another service, an MCP tool name pointed at
  the query function of the same name.
- Characters: wrong picks 3,701 (5 wrong-symbol 2,424 + 7 namesake 1,277) of
  22,313 injected by these two kinds.
- Only 2 of the 12 (`module.exports`, the URL path) have a shape a token rule
  could reject (a `.` or `/` next to the token); the other 10 need to know
  which names are symbols.
- Several right picks were the useful case the pointer exists for: the grep
  looked for a definition in a file that does not hold it (`fn index_files`
  in `src/indexer/pipeline.rs`, `fn get_language` in
  `src/parser/treesitter.rs`), and the pointer named the file that does.

Decision: `tasks/specs/maintenance-mode-2026-11-24.md`, section
"0.205.0 之后的遗留".
