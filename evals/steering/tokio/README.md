# tokio caller cases

Six "who calls this?" cases on tokio 1.41.1 (720 Rust files), for
`evals/steering/ab.py`. They exist because the fixture cases leave no headroom:
in the steering A/B both arms scored 0.997 and 1.000, so no steering change can
show an effect on answers there.

```bash
evals/steering/tokio/template.sh                 # once: workspace + index, ~5 s after the clone
python3 evals/steering/ab.py --suite evals/steering/tokio/cases --tags tokio \
    --variants none --runs 2 -j 3 --model claude-opus-5-5 --max-cost-usd 5
```

`template.sh` builds the workspace each run copies: tokio at `bb7ca75` as a
one-commit git repo, indexed by the binary under test with no embedding model
(as in every eval session). An index copied to another path stays fresh
(`incremental-index`: 0 files updated), so the session does not index anything.
Rebuild the template after changing the binary.

## Answers

Each case asks for the direct callers of one definition in library code
(`tokio/src/`, test nodes excluded). The answer is rust-analyzer's, through the
SCIP oracle's gold pairs (`scripts/scip_oracle/oracle.py --dump-gold`), not
code-graph's edges, so a case cannot reward the plugin for sharing its blind
spots. `gen_cases.py` writes the cases; its docstring has the full recipe.

The reply must end with one `Type::method @ path` line per caller. Each caller
has its own grader, which needs the name and the file on one line, so the score
is recall. A name qualified by another type does not count, and a bare name
counts only when no other caller in that file has the same name (one bare
`new @ sync/broadcast.rs` line used to satisfy both `Waiter::new` and
`Recv::new`). Listing extra functions costs nothing.

| case | callers | files | code-graph default floor finds |
|---|---|---|---|
| `pointers-new` | 9 | 7 | 9 |
| `poll-evented-new` | 12 | 7 | 12 |
| `poll-evented-into-inner` | 9 | 8 | 0 |
| `linked-list-remove` | 12 | 10 | 0 |
| `spawn-blocking` | 6 | 3 | 3 |
| `registration-poll-read-ready` | 10 | 8 | 0 |

The selection mixes call shapes code-graph resolves at the default floor (path
calls) with ones it does not (method calls on common names), and names grep
cannot tell apart (`new` has 284 definitions in the index, `remove` 14).
Checked before any paid run: a perfect reply in three spellings
(`Type::method`, backticked, `<Type as Trait>::method`) scores 1.0 on every
case; in bare names it scores 1.0 except on `pointers-new` and
`poll-evented-into-inner` (7/9 each), whose two same-named callers in one file
need their types.

First run (2026-10-06, Opus, Sonnet and Haiku, $4.82): `evals/README.md`,
"Headroom pilot on tokio". Opus scored 1.000 on all 12 sessions; the room is
in the smaller models, on callers code-graph has no edge for or
`find_references` drops.
