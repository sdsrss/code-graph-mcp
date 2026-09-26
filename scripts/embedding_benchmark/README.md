# Embedding Retrieval Benchmark

Offline A/B of code embeddings for `semantic_code_search`. Vector-only ranking
(isolates the embedding variable). Gate for the spec's Phase 2 Rust work:
`docs/superpowers/specs/2026-06-21-tier1-static-embeddings-design.md`.

## Prerequisites

- One or more repos indexed with `code-graph-mcp rebuild-index` (gives `.code-graph/index.db`).
- For TS coverage, index a TS-heavy project and pass an extra `--db`.
  TS project used: `/mnt/data_ssd/dev/projects/sgc` (ts+js).
- Install Python deps: `pip install -r requirements.txt`

## Run

```bash
# Regenerate labeled query set (stdlib-only — system python3 works)
python3 build_query_set.py \
  --db .code-graph/index.db \
  --db /path/to/ts-project/.code-graph/index.db \
  --real real_queries.jsonl \
  --out query_set.jsonl

# Evaluate one cell of the matrix (venv python required — ML deps)
.venv/bin/python eval_retrieval.py \
  --backend {minilm,potion} \
  --field {context_string_nodoc,code_content} \
  --db .code-graph/index.db \
  [--db extra.db ...] \
  --queries query_set.jsonl \
  --out results/<backend>_<field>.json
```

Short-query slices (keyword + partial identifier, see
[Short-query slices](#short-query-slices-2026-09-26)) come from their own
stdlib builder; concatenate them with the query set to score all shapes on one
candidate pool. `--backend bm25` is a lexical reference arm (`lexical.py`), no
model:

```bash
python3 build_short_slices.py --db .code-graph/index.db --out short_slices.jsonl
cat query_set.jsonl short_slices.jsonl > all_queries.jsonl
.venv/bin/python eval_retrieval.py --backend bm25 --field context_string_nodoc \
  --db .code-graph/index.db --queries all_queries.jsonl --out results/bm25_nodoc.json
```

The result's `by_source` splits every metric per slice.

Every run first prints how many doc-derived queries (bootstrap, keyword) have
their doc in the gold's text on the chosen field, and fails above `--max-leak` (default 5%) —
see [Leakage](#leakage-2026-09-25). `context_string_nodoc` is the production
`context_string` with its `doc:` part removed from every candidate; plain
`context_string` leaks 100% and needs `--max-leak 1` to run at all.

To run the full matrix:

```bash
Q=query_set.jsonl
D="--db .code-graph/index.db --db /path/to/ts-project/.code-graph/index.db"
for backend in minilm potion; do
  for field in context_string_nodoc code_content; do
    .venv/bin/python eval_retrieval.py --backend $backend --field $field $D \
      --queries $Q --out results/${backend}_${field}.json
  done
done
```

## Leakage (2026-09-25)

> **CORRECTION.** The 2026-06-21 table below scored bootstrap queries on
> `context_string`, and every one of them is in its gold's `context_string`
> verbatim: a bootstrap query IS the symbol's doc comment
> (`build_query_set.py`), and `src/embedding/context.rs` writes that doc into
> the embedded text as `doc: {doc}`. Measured on this repo's index today, the
> leak check (`leakage.py`: the query's first 12 words, contiguous, in the gold)
> fires on **1536/1536** bootstrap queries on `context_string`, **1/1508** on
> `context_string_nodoc`, **0/1508** on `code_content`. Python docstrings sit
> inside `code_content`, so `build_query_set.py` now drops the 28 queries whose
> doc is in the body (27 Python, 1 JS) — no field can strip those.

Re-measured 2026-09-25 on the same query set for every cell (n=1513: rust 1183,
javascript 323, python 4, bash 3; 3974 candidates). **One DB only** —
`code-graph-mcp` at v0.155.0 HEAD, snapshotted with `sqlite3 .backup`; the `sgc`
TS index the 06-21 run also used is not on this machine, so these numbers are
comparable with each other, not with the table below.

| backend | field | leak | NDCG@10 | rust | javascript | recall@1 | recall@10 | MRR |
|---|---|---|---|---|---|---|---|---|
| minilm | context_string (`--max-leak 1`) | 100.0% | 0.9107 | 0.9064 | 0.9372 | 0.8553 | 0.9636 | 0.8950 |
| minilm | **context_string_nodoc** | 0.1% | **0.4082** | 0.4393 | 0.2970 | 0.2307 | 0.6021 | 0.3574 |
| minilm | code_content | 0.0% | 0.3743 | 0.3944 | 0.3023 | 0.2062 | 0.5644 | 0.3261 |
| potion | context_string (`--max-leak 1`) | 100.0% | 0.8692 | 0.8797 | 0.8372 | 0.7964 | 0.9385 | 0.8495 |
| potion | **context_string_nodoc** | 0.1% | **0.3706** | 0.3921 | 0.2940 | 0.2168 | 0.5479 | 0.3262 |
| potion | code_content | 0.0% | 0.3232 | 0.3353 | 0.2828 | 0.1818 | 0.4865 | 0.2831 |

What survives, what does not:

- **The minilm-over-potion NO-GO holds.** Leak-free, minilm leads by 3.76pp
  overall (0.4082 vs 0.3706) and 4.72pp on rust (0.4393 vs 0.3921) — still past
  the 0.02 threshold. The leak inflated both arms by roughly the same amount.
- **"context_string dominates code_content" does not.** The leaked gap was
  53.64pp here (0.9107 vs 0.3743); leak-free it is 3.39pp overall, and on
  javascript `code_content` is ahead (0.3023 vs 0.2970). What `context_string`
  adds beyond the code — signature, relations, path — is worth a few points, not
  the doubling the table below shows.
- **The honest baseline for doc -> code retrieval is NDCG@10 ≈ 0.41** (minilm),
  not 0.87. Any model or context-string change must be judged against the
  `context_string_nodoc` row.
- `eval_ranking.py` (end-to-end) and `eval_rrf_ab.py` read the real index, whose
  FTS columns and vectors both carry the doc. `eval_rrf_ab.py` now refuses a
  leaked query set the same way; `eval_ranking.py`'s NL numbers below remain
  leaked (see "Separate future threads" 2) — use the tier3 slice or real
  queries for anything decided on it.

## Short-query slices (2026-09-26)

Two query shapes the doc-comment set does not cover, built by
`build_short_slices.py` (accepted shapes and their tests:
`tasks/specs/retrieval-short-query-slices.md`,
`tests/embedding_benchmark/test_short_slices.py`):

- **partial_identifier** — two contiguous subtokens of a symbol name
  (`parse_config_file` -> `parse config`); gold = every symbol whose name holds
  that run (at most 3). Examples: `extract function`, `adopted projects`.
- **keyword** — the 3 highest-IDF plain words of the doc's first sentence
  (`scoped tmp dir`, `prerequisites kahn topological`); gold = the symbol.
  Synthetic, and a share read as word salad (`channel branch owes`).

One DB: `code-graph-mcp` at `2043f3b`, `sqlite3 .backup` snapshot, 4145
candidates; all slices scored in one run per cell, so rows are comparable with
each other. The bootstrap row is the same set as the [Leakage](#leakage-2026-09-25)
table re-run on today's index (n=1520 vs 1513, 4145 vs 3974 candidates: minilm
0.4075 vs 0.4082). `bm25` is BM25 over identifier subtokens (`lexical.py`) — a
reference for term overlap, **not** the production FTS (`porter unicode61`,
which stems and does not split camelCase).

NDCG@10 / recall@1 / recall@10 / MRR:

| slice | backend | field | NDCG@10 | recall@1 | recall@10 | MRR |
|---|---|---|---|---|---|---|
| partial_identifier (n=867) | bm25 | context_string_nodoc | **0.6724** | 0.4225 | 0.8929 | 0.6123 |
| | bm25 | code_content | 0.6055 | 0.3799 | 0.8110 | 0.5553 |
| | minilm | context_string_nodoc | 0.4015 | 0.2197 | 0.5844 | 0.3623 |
| | minilm | code_content | 0.3732 | 0.2095 | 0.5488 | 0.3378 |
| | potion | context_string_nodoc | 0.2140 | 0.1176 | 0.3101 | 0.1964 |
| | potion | code_content | 0.1696 | 0.0807 | 0.2668 | 0.1535 |
| keyword (n=999) | bm25 | context_string_nodoc | 0.0909 | 0.0330 | 0.1722 | 0.0758 |
| | bm25 | code_content | 0.0895 | 0.0290 | 0.1682 | 0.0738 |
| | minilm | context_string_nodoc | 0.0668 | 0.0210 | 0.1301 | 0.0568 |
| | minilm | code_content | 0.0715 | 0.0260 | 0.1281 | 0.0636 |
| | potion | context_string_nodoc | 0.0717 | 0.0270 | 0.1381 | 0.0624 |
| | potion | code_content | 0.0734 | 0.0260 | 0.1331 | 0.0652 |
| bootstrap (n=1520) | bm25 | context_string_nodoc | 0.4108 | 0.2559 | 0.5855 | 0.3649 |
| | bm25 | code_content | 0.3679 | 0.2178 | 0.5303 | 0.3259 |
| | minilm | context_string_nodoc | 0.4075 | 0.2309 | 0.6013 | 0.3566 |
| | minilm | code_content | 0.3726 | 0.2053 | 0.5625 | 0.3247 |
| | potion | context_string_nodoc | 0.3695 | 0.2171 | 0.5447 | 0.3254 |
| | potion | code_content | 0.3229 | 0.1822 | 0.4849 | 0.2832 |

What the numbers say:

- **Partial identifiers are a lexical job.** Term overlap beats minilm by
  27.09pp (0.6724 vs 0.4015) and potion by 45.84pp on `context_string_nodoc`.
  Every partial_identifier query's words are in its gold's text (867/867), so
  this is ranking, not vocabulary: the dense arm does not reward an exact
  subtoken match the way BM25 does. potion loses more here than anywhere
  (-18.75pp to minilm), which adds to the NO-GO.
- **The keyword slice does not test "dense collapses on keywords".** BM25 is at
  the floor too (0.0909). Measured: all query words appear in the gold's
  `context_string_nodoc` for 26/999 queries (2.6%), none of them for 509/999
  (51.0%) — the words come from the doc, and the doc is exactly what the field
  removes. The slice measures doc-vocabulary vs code-vocabulary mismatch; a real
  keyword slice needs queries written by a person or an LLM (the other half of
  the #1(a) proposal), not picked out of the doc.
- **On doc -> code queries, BM25 ties minilm** (0.4108 vs 0.4075 NDCG@10;
  minilm leads recall@10 0.6013 vs 0.5855). The vector arm alone adds nothing
  over term overlap on this set; whether their fusion adds is an
  `eval_rrf_ab.py`-type question that needs a doc-free FTS to answer.
- Not measured: the production pipeline on these slices. `eval_ranking.py`
  reads the real index, whose FTS and vectors carry the doc, so the keyword
  slice is leaked there (`eval_rrf_ab.py` refuses it); partial_identifier is not
  doc-derived and can be run end-to-end. No significance test was run (the
  #1(a) proposal names ranx); the gaps called out above are 27-46pp on n=867.

## Results (2026-06-21, query_set n=648, candidates=5879)

> Leaked — see [Leakage](#leakage-2026-09-25). Kept for history.

DBs: `code-graph-mcp` (rust+js) + `sgc` (ts+js).  
By-language query counts: rust=429, javascript=160, typescript=58 (n=58 — limited statistical power; treat TS numbers as directional), python=1 (n=1; included in the overall mean but too small to interpret — negligible weight, ~0.0001 effect).

| backend | field          | NDCG@10 (overall) | rust   | typescript | javascript | recall@1 | recall@10 |
|---------|----------------|-------------------|--------|------------|------------|----------|-----------|
| minilm  | context_string | 0.8655            | 0.9355 | 0.8486     | 0.6890     | 0.8025   | 0.9306    |
| minilm  | code_content   | 0.4394            | 0.5458 | 0.2936     | 0.2097     | 0.2685   | 0.6235    |
| potion  | context_string | 0.7898            | 0.8782 | 0.8456     | 0.5312     | 0.7099   | 0.8673    |
| potion  | code_content   | 0.3804            | 0.4622 | 0.2796     | 0.1980     | 0.2500   | 0.5247    |

**Baseline**: minilm / context_string (NDCG@10 = 0.8655).  
**Potion's best field**: context_string (NDCG@10 = 0.7898 vs 0.3804 for code_content).

## Key findings

1. **[Refuted 2026-09-25 — the gap was the leak; leak-free it is 3.39pp, see [Leakage](#leakage-2026-09-25).]** **context_string dominates code_content for both backends** — the gap is large (minilm: 0.8655 vs 0.4394; potion: 0.7898 vs 0.3804). The spec §0.4 field choice is answered: use `context_string`.

2. **minilm beats potion overall** (0.8655 vs 0.7898, −7.6pp). The gap is consistent across rust (0.9355 vs 0.8782) and javascript (0.6890 vs 0.5312).

3. **Rust > TS > JS by NDCG@10 (despite JS's heavy presence in web-crawl pre-training)** — the rust gap over JS is large for both backends on context_string (minilm: 0.9355 vs 0.6890; potion: 0.8782 vs 0.5312). TS lands between rust and JS despite n=58 being a small sample. (None of the three is fine-tuned for this task; the surprise is that JS, the most pre-training-abundant language, ranks lowest.)

4. **JS underperforms despite likely being in minilm's training data** — JavaScript is extremely well-represented in web-crawl pre-training yet scores substantially lower than rust. This suggests the JS query+context construction or the mixed-type JS corpus (loose scripts + generated code) is harder to rank, not a data-domain coverage issue. **Caveat:** some bootstrap queries (notably JS) are section-header doc-comments mislabeled to a neighboring symbol (a banner comment naming a sibling), which the `name not in doc` filter cannot catch — so the absolute JS numbers carry label noise and likely understate true quality. This is symmetric across both backends (identical query set), so it does not affect the minilm-vs-potion comparison, only the absolute per-language reading.

5. **TS n=58 caveat** — numbers directionally consistent with rust's strong performance on context_string but insufficient for statistical significance. Add more TS-heavy repos to the `--db` list before drawing hard conclusions about TS.

## Go/no-go gate

**Decision: NO-GO** (re-confirmed leak-free 2026-09-25: minilm +3.76pp overall, +4.72pp rust — see [Leakage](#leakage-2026-09-25); the margins quoted in this paragraph are the leaked ones) — `potion-code-16M` does not replace `all-MiniLM-L6-v2`. potion's best config (context_string) trails minilm overall (0.7898 vs 0.8655, −7.6pp) and on rust (−5.7pp, beyond the 0.02 regression threshold); it only ties on TS (n=58, directional). Phase 2 (Rust static inference + 384→256 migration) is not authorized. Full rationale: `docs/superpowers/specs/2026-06-21-tier1-static-embeddings-decision.md`.

Best config measured: **minilm / context_string** (NDCG@10 = 0.8655, recall@10 = 0.9306) — the current production embedding remains the strongest, so no change is made.

## Ranking benchmark (end-to-end)

`eval_ranking.py` drives the **real** `semantic_code_search` pipeline (FTS5 + vector + RRF + adjusted-score re-rank) over MCP stdio, unlike `eval_retrieval.py` which is vector-only. Spawns `code-graph-mcp serve` for each project root against a frozen `sqlite3.backup()` copy of the index; never sends `initialized` (so background startup indexing is never triggered) and passes `skip_indexing=true` on every search call (so the copied index is never reindexed or wiped during a run).

### Invariants

- **Isolated root**: each benchmark root is a `/tmp/cg-bench-*` copy backed up via `sqlite3.backup()` — WAL + vec0 shadow tables included so vector search works against the copy.
- **No `initialized`**: omitting the `notifications/initialized` message prevents the server from launching its background index-watcher, keeping the index snapshot stable.
- **`skip_indexing=true`**: every `semantic_code_search` call carries this flag to skip per-query freshness checks, ensuring the server reads the snapshot, not a live reindex.
- **`CODE_GRAPH_INTERNAL=1`**: suppresses usage.jsonl writes so benchmark runs never pollute real adoption metrics.

### Run commands

```bash
# Step 1 — generate the tier3 slice (exact-symbol queries, gold = defining node)
python3 scripts/embedding_benchmark/build_tier3_slice.py \
  --db .code-graph/index.db \
  --db /mnt/data_ssd/dev/projects/sgc/.code-graph/index.db \
  --out scripts/embedding_benchmark/tier3_slice.jsonl --limit-per-db 250

# Step 2 — NL baseline (regression guard; --min-ndcg 0.5 catches a missing embed-model build)
python3 scripts/embedding_benchmark/eval_ranking.py \
  --queries scripts/embedding_benchmark/query_set.jsonl \
  --root . --root /mnt/data_ssd/dev/projects/sgc \
  --min-ndcg 0.5 \
  --out scripts/embedding_benchmark/results/ranking_nl_baseline.json

# Step 3 — tier3 slice baseline (improvement measure; no --min-ndcg floor)
python3 scripts/embedding_benchmark/eval_ranking.py \
  --queries scripts/embedding_benchmark/tier3_slice.jsonl \
  --root . --root /mnt/data_ssd/dev/projects/sgc \
  --out scripts/embedding_benchmark/results/ranking_tier3_baseline.json
```

### Results (2026-06-21, end-to-end RRF pipeline)

Binary: `target/release/code-graph-mcp` (built with `embed-model` feature, minilm active).  
DBs: `code-graph-mcp` (rust+js) + `sgc` (ts+js). `--top-k 20`.

**NL set** (`query_set.jsonl`, n=648, bootstrap doc-comment queries — regression guard):

| metric     | overall | rust   | typescript | javascript |
|------------|---------|--------|------------|------------|
| NDCG@10    | 0.6698  | 0.7453 | 0.5714     | 0.5040     |
| recall@1   | 0.5448  | 0.6084 | 0.4655     | 0.4062     |
| recall@10  | 0.7870  | 0.8695 | 0.6897     | 0.6000     |
| MRR        | 0.6320  | 0.7050 | 0.5339     | 0.4737     |

Note: NL overall NDCG@10 = 0.6698, which is below the vector-only baseline of 0.8655. The vector-only benchmark (`eval_retrieval.py`) measures pure embedding similarity without BM25; the end-to-end pipeline adds FTS5 + RRF fusion which can dilute pure vector signal when BM25 and vector disagree. The run did NOT abort (above the 0.5 `--min-ndcg` floor), confirming vector search is active. The gap signals that BM25 and vector are not always aligned on NL queries — a known property of RRF fusion when the FTS ranking is noisy relative to the embedding.

**Tier3 slice** (`tier3_slice.jsonl`, n=500, exact-symbol-name queries, gold = defining node — improvement measure):

| metric    | overall | rust   | typescript | javascript | python |
|-----------|---------|--------|------------|------------|--------|
| NDCG@10   | 0.8453  | 0.8869 | 0.7823     | 0.8701     | 1.0000 |
| recall@1  | 0.8360  | 0.8696 | 0.7789     | 0.8701     | 1.0000 |
| recall@10 | 0.8540  | 0.9043 | 0.7842     | 0.8701     | 1.0000 |
| MRR       | 0.8424  | 0.8813 | 0.7816     | 0.8701     | 1.0000 |

### Phase B go/no-go

> **CORRECTION (2026-06-21, post-fix `00160d4` + test-aware re-measurement).** The *"retrieval miss"* framing in this section is **RETRACTED**. The miss was a **benchmark artifact**, not unretrieved golds. `build_tier3_slice.py` selected gold on the `is_test = 0` column alone, but the binary's candidate loop *also* excludes via `is_test_symbol(name, path)` (path/name) — so ~16–22% of slice "symbols" were test-path helpers (e.g. `build_oracle` in `tests/routing_bench.rs`, which FTS even ranks **#1**) that the binary **correctly** drops, registering as false `absent`. This is the [[feedback_test_classifier_dual_sources]] dual-classifier gap. Step-1 `build_tier3_slice.py` has been hardened to match the binary's exclusion. Re-measured against only genuinely-retrievable symbols on the post-fix binary (00160d4 exact-name dominance), exact-symbol **rank1 = 0.987, absent = 0.008** (residual = degenerate minified-JS names like `V$1`/`Schema$1`, not real code). `rank 11–100 = 0.000` everywhere (no rerank burial) and the FTS garbage-guard (`0.001`) are both confirmed **non-factors** — overturning both deferred hypotheses. The shipped fix `00160d4` lifts genuinely-retrievable rank1 from **0.944 → 0.987 (+4.3pp)**, driving rust/python/ts `absent` to **zero** — its slice-measured `+2.0pp` was diluted by the ~19% test pollution it (correctly) cannot help. Evidence + the id-drift-immune, test-aware gate: `diag_retrieval_drop.py` → `results/diag_retrieval_drop_postfix.txt`. The NO-GO on definition-boost still stands — there is no retrieval headroom to chase — but for this reason, not "unretrieved golds". The pre-fix, test-polluted tables/numbers below are kept only for history.

**Decision: NO-GO** — the Tier 3 ranking changes (single-identifier weighting + definition-node boost) have a measured ceiling of **+1.6pp** on exact-symbol recall@1 (**+0.0pp** for JavaScript), because the recall@1 miss is overwhelmingly a *retrieval* failure, not a *ranking* one.

The `recall@1 = 0.836` "miss" of 16.4% is NOT re-rankable headroom (the original `0.836 < 0.97 → PROCEED, ~13pp` reading was wrong — it assumed every rank-1 miss is re-rankable). A gold-rank distribution — the tier3 slice re-run at `--top-k 100` (pool-depth probe, fetch_count = top_k×4 = 400) — splits it cleanly:

| band | overall | rust | typescript | javascript |
|------|---------|------|------------|------------|
| rank 1 | 0.838 | 0.874 | 0.779 | 0.870 |
| rank 2–10 (re-rankable) | 0.016 | 0.030 | 0.005 | 0.000 |
| **rank 11–100** | **0.000** | 0.000 | 0.000 | 0.000 |
| absent from top-100 (retrieval miss) | 0.146 | 0.096 | 0.216 | 0.130 |

The defining node is either in the top-10 or absent from the top-100 entirely — the rank 11–100 band is empty (`recall@100 == recall@10 == 0.854`). (Rank-1 here is 0.838 from this `--top-k 100` pool-depth probe; the `--top-k 20` baseline JSON above reports recall@1 = 0.836 — the 0.2pp delta is pool-depth re-ranking of a single query, not an inconsistency. Raw probe output: `results/tier3_rank_distribution.txt`.) Therefore:

- **Definition boost** can only promote the rank-2–10 golds → ceiling **+1.6pp** overall (rust +3.0pp, ts +0.5pp, **js +0.0pp**). Marginal-to-negligible, and on the wrong side of "worth a `search.rs` change."
- **Adaptive query weighting** reshuffles only candidates already in the fused pool; the empty rank 11–100 band shows the missing golds are not fused-but-low, they are *unretrieved* — re-weighting cannot surface them.

**Outcome:** Phase B (the Tier 3 `search.rs` ranking changes) is NOT authorized — same data-driven discipline as the potion NO-GO. `eval_ranking.py` + `build_tier3_slice.py` are kept as a permanent end-to-end retrieval-quality gate.

**Separate future threads (NOT Phase B):**
1. ~~The **14.6% exact-symbol retrieval miss** — defining nodes never fetched.~~ **RESOLVED / not a real miss** (see correction above): it was test-symbol benchmark pollution + the pre-fix size-dampening rerank burying large exact-match functions (fixed by `00160d4`). True post-fix miss on genuinely-retrievable symbols = **0.8%**, all degenerate minified-JS names. No retrieval/indexing lever to chase here.
2. **NL overall NDCG@10 = 0.6698 vs vector-only 0.8655.** Caveat: the NL query set was built for the vector-only eval (query = a symbol's doc-comment; the gold's `context_string` contains that doc), so it structurally favors pure-embedding cosine — not a clean "pipeline is worse" claim. Needs an isolated look before reading it as an RRF-tuning regression.
