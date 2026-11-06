# D#742 / 0.188.0 measurement kit

Scripts from the 0.188.0 work (D#745, D#743, and the withdrawn D#742 JS-family
import binding, redo filed as D#746). Spec and findings:
`tasks/specs/d742-js-family-import-calls.md`. Most scripts hard-code
`/var/tmp/agent-tmp-d745` (corpus copies under `corpora/` and `js/`, binaries
under `bin/`) or `/var/tmp/agent-tmp-review-0188*`; point them at a new work
directory first. Corpus commits: `tasks/var-tmp-kits/README.md`; the four JS
projects (claudemd, gsd-lite, llm_wiki, sgc) and claude-mem-lite, daagu were
`git archive HEAD` copies of `~/dev/<name>`.

- `d745_gen.py ROOT N` — the six D#745 shapes (N classes with method `h`, N
  calls of `h`: Python def/class body/other file, JS bare/other file/member).
- `diff2.sh W ARM_A ARM_B DIR...` — full index of each corpus with two binaries,
  edges with metadata plus pending rows compared.
- `timing.sh` — alternating base/new full-index timings (the CHANGELOG table).
- `gap.py` — JS/TS bare calls left pending although the caller imports the name
  from a relative path, classified by cause (`classify(dir)`): found hono's 182
  `.tsx -> .ts` calls.
- `famchain.py BIN CORPUS N SEED` — delete/restore files that are targets of
  cross-language calls, comparing incremental with a rebuild.
- `delone.sh BIN CORPUS FILE` — one deletion, incremental vs rebuild (the hono
  `src/jsx/streaming.ts` member-row repro of D#747).
- `runwalk.sh ARM` — the walk-harness chains (seeds 21, 22) and kits.
- `famgen.py ROOT N SEED STEPS` + `par.sh BIN DIR OUT` (pre-tag reviewer, round
  1) — edit chains whose files move between `.ts`/`.tsx`/`.js`. Picks from a
  set, so trees depend on Python's hash seed: keep a generated tree to compare
  arms.
- `perfgen.py` (round 1) — the D#745 shapes plus `bigjs`: 4,400 TS/TSX files,
  100,000 imports. `bigjsg.py` (round 2) adds a `gfn()` call defined only in a
  `.js` file to every file (cost of the sweep's import reads).
- `manual2.py` (round 2) — the MEDIUM that ended D#742: a callee file edited to
  add a same-named node (class method, nested function, ...) after the
  cross-language call was bound.

Archives:

- `review-keep.tar.gz` — what the two pre-tag reviewers kept (`.code-graph`
  dirs left out): round 1 under `agent-tmp-review-0188/keep/` (`scripts/` incl.
  `mutate.py`, `movechain.py`, `xkit.py`; `cases/` x1–x13 repro trees;
  `chains/` fc1_92, fc2_89, fc2_117; `results/` incl. `mutations.txt`), round 2
  under `agent-tmp-review-0188b/keep/` (`split.py`, `mutation-detail.txt`).
  Starting material for D#746.
- `results.tar.gz` — raw outputs behind the spec and CHANGELOG numbers:
  `timing-w1.txt` (the CHANGELOG table), `*-diff.txt` (corpus differentials per
  build), `fam-*.txt` / `*.ids` (mixed-extension chains per build and seed),
  `walk-*.out` (walk-harness runs).
