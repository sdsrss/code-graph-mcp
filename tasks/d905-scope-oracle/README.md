# D#905 scope oracle

Checks every Python `calls` / `references` edge of an index against Python's
own scoping (`ast`), per occurrence of the target's name in the source node.
Written for D#905 (0.203.0); see `tasks/specs/d905-python-scope-bare-names-2026-11-19.md`.

- `reforacle.py <corpus> [--rel references|calls] [--dump N]` — verdicts over
  `<corpus>/.code-graph/index.db`: `ok`, `wrong-local`, `wrong-var`,
  `wrong-builtin`, `wrong-ext`, `wrong-other` (another definition, a method by
  bare name, a same-file nested namesake), `unknown` (star import from
  outside, an alias `X = m.X`, a `global` name), `unmatched` (no occurrence
  found in the source node: decorators other than a bare name, parameter
  defaults, which code-graph attributes differently). `--rel calls` reads bare
  calls `f(...)` only; `x.f()` edges come out `unmatched`.
- `oracle_by_type.py <corpus> <rel>` — the same verdicts split by target kind
  (method or not) and tier (`ambiguous` or default-visible).
- `abdiff.py <base-bin> <head-bin> <corpora-dir> <work> [names]` — fresh index
  per arm from one copy, every edge compared, the oracle's verdict for each
  base-only edge, Python definitions left with no incoming edge.
- `baseonly.py <base-bin> <head-bin> <corpus> <work> <verdicts...>` — base-only
  edges of the given verdicts, grouped by name, for reading by hand.
- `parity905.py <bin> <work>` — D#905 shapes as edit sequences, incremental vs
  fresh index after every step.
- `varshape.py <corpus>` — shapes of the module-level bindings behind
  `wrong-var` edges (aliases, calls, other).
- `deadab.sh <base> <head> <corpus> <work>` — `dead-code --json` on both arms
  (the scan returns at most 200 rows).

Corpora: `tasks/d898-references-fanout/corpora.sh <dir>`.
