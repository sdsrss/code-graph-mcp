//! `semantic_code_search` — hybrid BM25 + vector search with RRF fusion.
//!
//! Confidence scoring (FTS sparsity / OR-fallback / source intersection),
//! acronym-heavy query detection, doc-penalty for markdown matches, and
//! token-aware compression sit here. Adjusted score combines RRF rank,
//! query quality, name match boost, and size dampening.

use super::super::*;

/// Per-result code_content cap used both in estimation and the actual result
/// payload, so compression triggers reflect real output size. At module scope
/// because the extracted result-building and compression helpers both need it
/// and must not each carry their own copy (audit 2026-08-22 P2-15).
const MAX_SEARCH_CODE_LEN: usize = 500;

/// One scored search hit, between fusion and rendering.
struct Candidate {
    node: queries::NodeResult,
    file_path: String,
    adjusted_score: f64,
}

/// Cut an uncapped fused pool to `cap` rows, keeping every row past the cut
/// whose node is named exactly `query` (name or qualified name, trimmed and
/// case-insensitive: the rerank's `is_exact_name` rule).
///
/// The cut used to happen inside the fusion, before the exact-name rerank
/// could see the row: a symbol the vector channel outranked fell off a small
/// pool, so on flask `semantic_code_search "send_file"` missed `send_file`
/// at top_k 1–10 and put it first at top_k 12. Fusion scores depend only on
/// each row's channel ranks, so the uncapped list's first `cap` rows ARE the
/// old capped pool; only exact-name rows are added after them.
fn cap_keeping_exact_names(
    conn: &rusqlite::Connection,
    mut fused_all: Vec<crate::search::fusion::SearchResult>,
    cap: usize,
    query: &str,
) -> Result<Vec<crate::search::fusion::SearchResult>> {
    if fused_all.len() <= cap {
        return Ok(fused_all);
    }
    let tail = fused_all.split_off(cap);
    let wanted = query.trim().to_lowercase();
    let tail_ids: Vec<i64> = tail.iter().map(|r| r.node_id).collect();
    let exact: std::collections::HashSet<i64> =
        queries::get_nodes_with_files_by_ids(conn, &tail_ids)?
            .into_iter()
            .filter(|nwf| {
                nwf.node.name.to_lowercase() == wanted
                    || nwf
                        .node
                        .qualified_name
                        .as_deref()
                        .is_some_and(|q| q.to_lowercase() == wanted)
            })
            .map(|nwf| nwf.node.id)
            .collect();
    fused_all.extend(tail.into_iter().filter(|r| exact.contains(&r.node_id)));
    Ok(fused_all)
}

impl McpServer {
    pub(in crate::mcp::server) fn tool_semantic_search(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let query = required_str(args, "query")?;
        // `limit` is the documented alias for `top_k`; whichever the caller sent is
        // type-checked (CON-15), and sending neither still means 20.
        let top_k = match &args["top_k"] {
            serde_json::Value::Null => arg_clamped(args, "limit", "semantic_code_search", 20)?,
            _ => arg_clamped(args, "top_k", "semantic_code_search", 20)?,
        } as i64;
        let node_type_filter = args["node_type"].as_str();
        let compact = arg_bool(args, "compact", false)?;

        // Validate node_type up-front: unknown aliases normalize to empty and
        // would silently filter every result away (see tool_ast_search parity).
        if let Some(nt) = node_type_filter {
            if crate::domain::normalize_type_filter(nt).is_empty() {
                return Err(anyhow!(
                    "Unknown node_type filter: '{}'. Valid: {}.{}",
                    nt,
                    crate::domain::TYPE_FILTER_HELP,
                    crate::domain::type_filter_note(nt)
                ));
            }
        }

        // Validate `language` up-front and normalize to canonical case: an unknown
        // language matches no stored `language` field and would silently return an
        // empty result. Canonicalizing also accepts mixed-case input, since the
        // downstream filter is an exact match. Parity with node_type above and CLI.
        let language_filter = match args["language"].as_str() {
            Some(lf) => Some(crate::utils::config::canonical_language(lf).ok_or_else(|| {
                anyhow!(
                    "Unknown language filter: '{}'. Valid: {}",
                    lf,
                    crate::utils::config::SUPPORTED_LANGUAGES.join(", ")
                )
            })?),
            None => None,
        };

        // Query quality factor: penalize vague/short queries so relevance scores
        // reflect actual match quality, not just relative rank position.
        let meaningful_tokens: Vec<&str> = query
            .split_whitespace()
            .filter(|w| {
                let has_alnum = w.chars().any(|c| c.is_alphanumeric());
                let char_count = w.chars().count();
                has_alnum && (char_count > 1 || w.chars().all(|c| c.is_uppercase()))
            })
            .collect();
        let query_quality = match meaningful_tokens.len() {
            0 => 0.3,
            1 if meaningful_tokens[0].len() <= 2 => 0.4,
            1 => 0.7,
            2 => 0.85,
            _ => 1.0,
        };

        // Lazy model loading: pick up model if downloaded in background
        self.try_lazy_load_model();

        // Ensure index is up to date (unless caller requested read-only mode)
        if !should_skip_indexing(args)? {
            self.ensure_indexed()?;
        }

        // vec0 KNN can't pre-filter on joined `nodes` columns, so language/node_type
        // filtering happens after the fetch (Phase 1 below). Widen the candidate pool
        // when a filter is active so a selective filter can't silently starve top_k.
        // The unfiltered fetch is byte-identical to the historical (top_k*4).max(20),
        // so the retrieval benchmark (which passes no filter) is unaffected.
        let filtered = language_filter.is_some() || node_type_filter.is_some();
        let fetch_count = crate::domain::search_fetch_count(top_k, filtered);
        // FTS sparsity ratio uses the base (unfiltered) pool size so a widened filtered
        // fetch doesn't spuriously depress match_confidence for filtered queries.
        let conf_fetch = crate::domain::search_fetch_count(top_k, false);
        // Whether the vector channel was actually available for this query (model
        // loaded AND sqlite-vec enabled). When false, every result is FTS5-only with
        // reduced semantic recall — surfaced in the output below so the caller is not
        // silently degraded (the model auto-downloads in the background on first use).
        //
        // The query is embedded ONCE here even though retrieval can run twice
        // (the pool-exhaustion retry below): embedding is the expensive half.
        let model_guard = lock_or_recover(&self.embedding_model, "embedding_model");
        // A loaded model over an index with no vectors yet is the FTS5 channel
        // alone (B7); a failed probe keeps the prior reading.
        let channel = vector_channel(model_guard.is_some() && self.db.vec_enabled(), || {
            queries::any_node_vector(self.db.conn()).unwrap_or(true)
        });
        let vector_available = channel == VectorChannel::Ready;
        let query_embedding: Option<Vec<f32>> = match *model_guard {
            Some(ref model) if vector_available => model.embed(query).ok(),
            _ => None,
        };
        drop(model_guard);

        // One retrieval pass at a given pool size: FTS5 + KNN, both sized by the
        // SAME count (they share domain::search_fetch_count by design — a KNN
        // pool wider than the FTS pool re-opens the post-filter starvation this
        // whole mechanism exists to prevent).
        type Fused = Vec<crate::search::fusion::SearchResult>;
        // The trailing `Option<&str>` is the text channel's "I never ran" reason.
        type NotSearched = Option<&'static str>;
        let retrieve = |fetch: i64| -> Result<(Fused, Fused, bool, NotSearched)> {
            let fts_result = queries::fts5_search(self.db.conn(), query, fetch)?;
            // Both widening arms feed one flag; `FtsResult::is_widened_match`
            // carries the rationale and the two surface consequences.
            let or_fallback = fts_result.is_widened_match();
            let fts_not_searched = fts_result.empty_reason;
            // Carry raw BM25 scores for score blending in RRF fusion.
            let fts_search: Fused = fts_result
                .nodes
                .iter()
                .enumerate()
                .map(|(i, r)| crate::search::fusion::SearchResult {
                    node_id: r.id,
                    score: fts_result.bm25_scores.get(i).copied().unwrap_or(0.0),
                })
                .collect();
            let vec_search: Fused = match &query_embedding {
                Some(embedding) => queries::vector_search(self.db.conn(), embedding, fetch)?
                    .iter()
                    // Convert distance to similarity: 1.0 - distance (L2-normalized vectors)
                    .map(|(node_id, distance)| crate::search::fusion::SearchResult {
                        node_id: *node_id,
                        score: 1.0 - distance,
                    })
                    .collect(),
                None => vec![],
            };
            Ok((fts_search, vec_search, or_fallback, fts_not_searched))
        };
        let (fts_search, vec_search, fts_or_fallback, fts_not_searched) = retrieve(fetch_count)?;

        // Track search source IDs for confidence scoring
        let fts_node_ids: std::collections::HashSet<i64> =
            fts_search.iter().map(|r| r.node_id).collect();
        let vec_node_ids: std::collections::HashSet<i64> =
            vec_search.iter().map(|r| r.node_id).collect();

        // RRF fusion (FTS + Vec when available, FTS-only otherwise)
        // k=30: sharper rank sensitivity than default 60 (top results matter more)
        // Default fts=1.0, vec=1.2: slightly favor vector similarity since FTS is now stronger
        // with name_tokens and type columns in v2 schema.
        //
        // Acronym-heavy override: queries that are entirely short uppercase tokens
        // (≤3 tokens, each ≤5 chars, all [A-Z0-9]) are letter-exact identifiers —
        // embeddings handle them poorly (training corpora rarely teach "RRF" ≈
        // "reciprocal rank fusion"), while FTS5's token-exact match is reliable.
        // Shift the weight toward FTS to let the precise channel dominate.
        let is_acronym_heavy = !meaningful_tokens.is_empty()
            && meaningful_tokens.len() <= crate::domain::ACRONYM_MAX_TOKENS
            && meaningful_tokens.iter().all(|t| {
                let len_ok = t.chars().count() <= crate::domain::ACRONYM_MAX_TOKEN_CHARS;
                let shape_ok = t
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
                len_ok && shape_ok
            });
        let (fts_weight, vec_weight) = if is_acronym_heavy {
            (
                crate::domain::ACRONYM_FTS_WEIGHT,
                crate::domain::ACRONYM_VEC_WEIGHT,
            )
        } else {
            (
                crate::domain::DEFAULT_FTS_WEIGHT,
                crate::domain::DEFAULT_VEC_WEIGHT,
            )
        };
        let fuse = |fts: &[crate::search::fusion::SearchResult],
                    vecs: &[crate::search::fusion::SearchResult],
                    cap: usize| {
            weighted_rrf_fusion(
                fts,
                vecs,
                crate::domain::RERANK_RRF_K,
                cap,
                fts_weight,
                vec_weight,
            )
        };
        let fused = cap_keeping_exact_names(
            self.db.conn(),
            fuse(&fts_search, &vec_search, usize::MAX),
            fetch_count as usize,
            query,
        )?;

        // Match confidence: penalize when search signals are weak
        let match_confidence = {
            let mut c = 1.0_f64;
            // FTS-empty penalty: no text match → results are purely vector similarity (often noise)
            if fts_search.is_empty() && !vec_search.is_empty() {
                c *= crate::domain::CONF_VEC_ONLY_PENALTY;
            } else if !fts_search.is_empty() {
                // OR-fallback penalty: AND mode failed → query terms don't co-occur (weaker match)
                if fts_or_fallback {
                    c *= crate::domain::CONF_OR_FALLBACK_PENALTY;
                }
                // FTS sparsity: fewer results relative to fetch_count → weaker text match.
                // Skip the ratio check for precision queries (fts returns ≤4 hits): a
                // unique-identifier search legitimately has a low ratio but is a strong
                // signal, not a weak one. Only apply when we have enough FTS breadth to
                // judge "sparse vs. broad".
                if fts_search.len() >= crate::domain::CONF_SPARSITY_MIN_FTS {
                    let fts_ratio = fts_search.len() as f64 / conf_fetch as f64;
                    if fts_ratio < crate::domain::CONF_SPARSITY_R1 {
                        c *= crate::domain::CONF_SPARSITY_P1;
                    } else if fts_ratio < crate::domain::CONF_SPARSITY_R2 {
                        c *= crate::domain::CONF_SPARSITY_P2;
                    } else if fts_ratio < crate::domain::CONF_SPARSITY_R3 {
                        c *= crate::domain::CONF_SPARSITY_P3;
                    }
                }
            }
            // Source intersection: when both sources available, low overlap → less confidence.
            // Only meaningful when FTS returned enough breadth to judge overlap; for
            // precision queries (≤4 FTS hits) the intersection is naturally tiny and
            // should not count against confidence.
            if fts_search.len() >= crate::domain::CONF_SPARSITY_MIN_FTS && !vec_search.is_empty() {
                let top_ids: Vec<i64> = fused
                    .iter()
                    .take(top_k as usize)
                    .map(|r| r.node_id)
                    .collect();
                let in_both = top_ids
                    .iter()
                    .filter(|id| fts_node_ids.contains(id) && vec_node_ids.contains(id))
                    .count();
                let ratio = in_both as f64 / top_ids.len().max(1) as f64;
                if ratio < crate::domain::CONF_INTERSECTION_MIN_RATIO {
                    c *= crate::domain::CONF_INTERSECTION_PENALTY;
                }
            }
            c
        };

        // Measurement seam (env-gated, stderr-only — NO response-contract change): emit
        // the raw top-1 vector similarity alongside the final match_confidence so the
        // confidence-calibration bench can test whether it separates good-NL from
        // nonsense queries (the RRF `relevance` score does not — it is rank-fused and
        // discards similarity magnitude). Default behavior is untouched: nothing is
        // emitted unless CODE_GRAPH_EMIT_CONFIDENCE is set. vec_search is KNN-ordered
        // (nearest first), so its head carries the top raw similarity `1.0 - distance`.
        // NOTE: node_vectors is a plain vec0 table (no `distance=` metric) → sqlite-vec
        // uses L2 distance, so this is `1.0 - L2_distance`, NOT cosine similarity. For
        // L2-normalized embeddings it is order-equivalent to cosine but not equal to it.
        // See scripts/embedding_benchmark/eval_confidence.py.
        if std::env::var_os("CODE_GRAPH_EMIT_CONFIDENCE").is_some() {
            let top_vec_score = vec_search.first().map(|r| r.score).unwrap_or(f64::NAN);
            eprintln!(
                "[CONF_PROBE] q={:?} match_confidence={:.4} top_vec_score={:.4} fts_hits={} vec_hits={} or_fallback={}",
                query, match_confidence, top_vec_score, fts_search.len(), vec_search.len(), fts_or_fallback
            );
        }

        // Low-confidence warning trigger (consumed by the compressed path and
        // finalize_search_results below). Fires ONLY when the result set has no text
        // anchor at all — FTS returned nothing, so the ranking is vector similarity
        // alone, the one case where "vector-similarity only" is literally true.
        //
        // It deliberately does NOT use the match_confidence<0.5 threshold: the
        // confidence-calibration bench (scripts/embedding_benchmark/eval_confidence.py)
        // measured that match_confidence pins ~0.45 for essentially every multi-word
        // natural-language query, good and nonsense alike (OR-fallback 0.6 ×
        // intersection 0.75), and that neither match_confidence, RRF relevance, nor raw
        // top-1 vector similarity separates a good NL query from nonsense on this index.
        // The old threshold therefore warned on 100% of good NL queries (which retrieve
        // relevant results 82% of the time) — a false alarm that pushed callers to
        // distrust correct results. fts-empty is the honest, mechanically-trustworthy
        // trigger. (FTS-only degradation — vector channel down — is surfaced separately
        // as a `note` in finalize_search_results.)
        let vector_only_no_anchor = fts_search.is_empty() && !vec_search.is_empty();

        // Phase 1: Collect all valid candidates with adjusted scores
        // Name match boost + size dampening counter BM25/vector bias toward large nodes
        let query_terms_lower: Vec<String> =
            meaningful_tokens.iter().map(|t| t.to_lowercase()).collect();
        // Verbatim identifier query (e.g. "run_serve") — used for exact-name rerank
        // dominance below and the confidence exemption further down (single source).
        let query_trimmed = query.trim().to_lowercase();

        // Hoisted out of the per-candidate loop below: the filter string is
        // invariant for the whole call, while `normalize_type_filter_mcp`
        // allocates a fresh `Vec<String>` on every invocation — once per
        // candidate, over a pool of up to 1000, and the closure runs TWICE when
        // the pool-exhaustion retry fires (SURF-07, audit 2026-09-05).
        let normalized_type_filter: Option<Vec<String>> =
            node_type_filter.map(normalize_type_filter_mcp);

        // Scoring/filtering for one fused pool. Returns the candidates plus BOTH
        // drop counts: the optional language/node_type filter AND the always-on
        // module/external/test skip. The latter used to be a bare `continue` —
        // invisible to the pool sizing and to the empty-result message, so a pool
        // eaten by noise looked identical to a query that matched nothing
        // (audit 2026-08-16 P1-7).
        let build_candidates = |fused: &[crate::search::fusion::SearchResult]| -> Result<(Vec<Candidate>, usize, usize)> {
            // Batch-fetch all candidate nodes with file info (single query instead of N+1)
            let candidate_ids: Vec<i64> = fused.iter().map(|r| r.node_id).collect();
            let nodes_with_files =
                queries::get_nodes_with_files_by_ids(self.db.conn(), &candidate_ids)?;
            // Lookup by node_id; the fused order drives iteration below.
            let mut nwf_map: std::collections::HashMap<i64, queries::NodeWithFile> =
                nodes_with_files
                    .into_iter()
                    .map(|nwf| (nwf.node.id, nwf))
                    .collect();
            let max_rrf = fused.first().map(|f| f.score).unwrap_or(0.0);
            let mut candidates: Vec<Candidate> = Vec::new();
            let mut dropped_by_filter = 0usize;
            let mut skipped_noise = 0usize;
            for r in fused {
                let Some(nwf) = nwf_map.remove(&r.node_id) else {
                    continue;
                };
                {
                    let node = &nwf.node;
                    if crate::domain::is_skippable_result(
                        node.is_test,
                        &node.node_type,
                        &node.name,
                        &nwf.file_path,
                    ) {
                        skipped_noise += 1;
                        continue;
                    }
                    if let Some(normalized) = normalized_type_filter.as_deref() {
                        if !normalized.iter().any(|t| t == &node.node_type) {
                            dropped_by_filter += 1;
                            continue;
                        }
                    }
                    if let Some(lang) = language_filter {
                        if nwf.language.as_deref() != Some(lang) {
                            dropped_by_filter += 1;
                            continue;
                        }
                    }
                }

                let node = &nwf.node;
                let base_score = if max_rrf > 0.0 {
                    (r.score / max_rrf * query_quality * match_confidence * 100.0).round() / 100.0
                } else {
                    0.0
                };

                // Name match boost: symbols whose name contains query terms are more likely relevant
                let name_lower = node.name.to_lowercase();
                // Exact symbol-name match dominates the rerank: RRF already ranks an
                // exact match (tier3 recall@10 0.984 RRF-only), but base×name_boost×size
                // could bury it under vector noise + size dampening (→ 0.806). Same
                // semantics as `has_exact_name_match` (confidence exemption) below.
                let is_exact_name = name_lower == query_trimmed
                    || node
                        .qualified_name
                        .as_deref()
                        .map(|q| q.to_lowercase() == query_trimmed)
                        .unwrap_or(false);
                let name_match_count = query_terms_lower
                    .iter()
                    .filter(|t| name_lower.contains(t.as_str()))
                    .count();
                let name_boost = (1.0
                    + name_match_count as f64 * crate::domain::NAME_BOOST_PER_MATCH)
                    .min(crate::domain::NAME_BOOST_CAP);

                // Size dampening: counter BM25/vector bias toward very large nodes (>100 lines)
                let node_lines = (node.end_line.saturating_sub(node.start_line) + 1) as f64;
                let size_factor = if node_lines > crate::domain::SIZE_DAMPEN_LINES {
                    1.0 / (1.0
                        + (node_lines / crate::domain::SIZE_DAMPEN_LINES).ln()
                            * crate::domain::SIZE_DAMPEN_COEFF)
                } else {
                    1.0
                };

                // Doc penalty: markdown headings can match loosely via vector similarity
                // for code-intent queries (the tool is `semantic_code_search`). When the
                // caller has not explicitly requested markdown via `language="markdown"`,
                // demote them so README/heading prose cannot outrank real code matches.
                let doc_penalty = if nwf.language.as_deref() == Some("markdown")
                    && language_filter != Some("markdown")
                {
                    crate::domain::DOC_PENALTY_MARKDOWN
                } else {
                    1.0
                };

                let adjusted = crate::search::fusion::final_adjusted_score(
                    base_score,
                    name_boost,
                    size_factor,
                    doc_penalty,
                    is_exact_name,
                );
                candidates.push(Candidate {
                    node: nwf.node,
                    file_path: nwf.file_path,
                    adjusted_score: adjusted,
                });
            }
            Ok((candidates, dropped_by_filter, skipped_noise))
        };

        let (mut candidates, mut dropped_by_filter, mut skipped_noise_count) =
            build_candidates(&fused)?;

        // Which pool the returned candidates actually came from. The retry below
        // REPLACES the candidate set wholesale when it wins, so "did the pool come
        // back full" has to be asked of the pool that produced the answer: reading
        // `fused.len()` after an adopted retry tests a pool no result came from,
        // and reports a truncation that did not happen (a first pool is full far
        // more often than the 4× wider retry pool).
        let mut final_pool_len = fused.len();

        // Pool-exhaustion retry: when the first pool came back FULL and the
        // post-fetch filters still left top_k unfilled, matches may sit just
        // below the cut — widen once and re-rank. Confidence (measured above on
        // the first pass) is deliberately NOT recomputed: this widens recall, it
        // does not change how well the query matched. A pool that was not
        // exhausted, or that lost nothing to filtering, retrieves exactly as
        // before — the retrieval benchmark path is untouched.
        let retry_fetch = crate::domain::search_retry_fetch_count(fetch_count);
        // Did the WIDEST fetch we performed come back full? That, not the pool the
        // answer happened to come from, is the question "were there rows nobody
        // looked at". Tracked separately so the reported numbers can stay coupled
        // to one pool while the flag still knows about the other fetch.
        let mut widest_fetch_was_full = fused.len() >= fetch_count as usize;
        if candidates.len() < top_k as usize
            && fused.len() >= fetch_count as usize
            && (skipped_noise_count + dropped_by_filter) > 0
            && retry_fetch > fetch_count
        {
            let (fts_retry, vec_retry, _, _) = retrieve(retry_fetch)?;
            let fused_retry = cap_keeping_exact_names(
                self.db.conn(),
                fuse(&fts_retry, &vec_retry, usize::MAX),
                retry_fetch as usize,
                query,
            )?;
            let (retry_candidates, retry_dropped, retry_skipped) = build_candidates(&fused_retry)?;
            widest_fetch_was_full = fused_retry.len() >= retry_fetch as usize;
            // Adoption stays on a STRICT improvement, and all five values move
            // together. Three review rounds converged here, two of them on my own
            // wrong fixes:
            //
            //   - recording the pool size only on adoption while judging fullness
            //     on it announced "the candidate pool (320 rows) came back full"
            //     at top_k=20 on a 421-row index the retry had just read out;
            //   - moving the size alone split the envelope, so a 400-row pool
            //     reported 99 rows dropped and the numbers did not add up;
            //   - relaxing adoption to `>=` to re-couple them changed RESULT
            //     SELECTION. `weighted_rrf_fusion` at a wider cap is neither a
            //     superset nor order-preserving once the vector channel is
            //     non-empty: a row in both channels just past the first cut
            //     outscores a single-channel row and displaces it, and `max_rrf`
            //     — the divisor of every `base_score` — changes with the pool.
            //     Refuted by probe tests against the real fusion function
            //     (round 4).
            //
            // No test in this module can see that class of change, and REBUILDING
            // WITH `--features embed-model` DOES NOT HELP — round 5 measured
            // `vector_available=false` on that leg. The blindness is the harness,
            // not the build: `McpServer::new_test_with_project` hard-codes
            // `embedding_model: None` and `indexed_server` indexes with
            // `model: None`, so `vec_search` is empty here on every leg. Covering
            // the vector path needs a fixture with real weights loaded AND
            // embeddings written to the index. Round 4's "18/18 on the shipped
            // feature leg" was a compile-and-still-pass check offered as evidence
            // about a path it never reached — the same mis-citation one level
            // down from the one it was correcting.
            //
            // So the answer's pool owns every number reported beside it, and
            // `widest_fetch_was_full` above carries what the other fetch learned.
            if retry_candidates.len() > candidates.len() {
                candidates = retry_candidates;
                dropped_by_filter = retry_dropped;
                skipped_noise_count = retry_skipped;
                final_pool_len = fused_retry.len();
            }
        }

        // Everything a short `results` array cannot say about itself, computed
        // once here (the counts and both pool numbers are only in scope at this
        // point) and attached to whichever of the three response shapes returns.
        //
        // Two wrong attempts at this are worth recording, because both looked
        // reasonable and both were refuted by measurement.
        //
        // The first suppressed the flag whenever the widening added no survivor.
        // That silenced the EMPTY case, where a match provably does exist below
        // the cut — the widened pool held 400 of 422 rows and still missed it.
        //
        // The second kept the flag but branched the ADVICE on the same signal,
        // telling the caller that raising top_k was unlikely to help. Also false:
        // `fetch_count` is `top_k * 16` while the retry widens by a fixed 4x, so a
        // bigger top_k reaches PAST what the retry saw. Measured on that fixture,
        // top_k=27 returns the match the retry never reached — the advice steered
        // the caller away from the remedy that works.
        //
        // What was actually broken is which FETCH the flag asks about.
        // `widest_fetch_was_full` answers that directly, and it is the ONLY
        // suppressor: a widening that read the index to its end leaves it false
        // and no flag is raised. An earlier version also required
        // `final_pool_len >= final_fetch_count`, which round 5 proved dead in
        // every reachable state — the retry guard has already established that the
        // first pool was full, so on the non-adopted path that clause is
        // unconditionally true. It read as load-bearing and was not; the variable
        // it was the last reader of is gone with it.
        let shortfall = PoolShortfall {
            dropped_by_filter,
            skipped_noise: skipped_noise_count,
            pool_len: final_pool_len,
            saturated: candidates.len() < top_k as usize
                && widest_fetch_was_full
                && (dropped_by_filter + skipped_noise_count) > 0,
            top_k,
        };

        // Phase 2: Re-rank by adjusted score (name relevance + size normalization)
        candidates.sort_by(|a, b| b.adjusted_score.total_cmp(&a.adjusted_score));
        candidates.truncate(top_k as usize);

        // Phase 3: Build results
        let results = build_search_results(&candidates, compact);
        // Record search metrics (before potential compression return)
        lock_or_recover(&self.metrics, "metrics").record_search(
            results.len(),
            query_quality,
            vec_search.is_empty(),
        );

        // Exact-identifier exemption for the low-confidence warning: when the query
        // is a single identifier that appears verbatim as a candidate symbol name,
        // retrieval is precise regardless of the FTS breadth heuristics. Computed
        // once here so BOTH the compressed and the bare-array return paths gate the
        // noise warning identically (previously only the compressed path had it).
        let has_exact_name_match = candidates.iter().take(5).any(|c| {
            c.node.name.to_lowercase() == query_trimmed
                || c.node
                    .qualified_name
                    .as_deref()
                    .map(|q| q.to_lowercase() == query_trimmed)
                    .unwrap_or(false)
        });

        // Context Sandbox: compress only if results likely exceed token threshold.
        if let Some(mut compressed) = try_compress_results(
            &candidates,
            compact,
            match_confidence,
            vector_available,
            vector_only_no_anchor,
            has_exact_name_match,
        )? {
            // A compressed answer is under-returned in the same way and by the
            // same mechanism; the envelope contract is what makes one attach
            // point cover all three shapes.
            shortfall.attach(&mut compressed);
            note_vector_channel(&mut compressed, channel);
            return Ok(compressed);
        }

        if results.is_empty() {
            let mut out = explain_empty_results(
                query,
                filtered,
                &shortfall,
                fts_not_searched,
                vector_available,
            );
            shortfall.attach(&mut out);
            note_vector_channel(&mut out, channel);
            return Ok(out);
        }

        // Shape the response: one object envelope on every path ({results, …}),
        // carrying the degradation (FTS-only) / no-text-anchor (vector-only)
        // signals when they apply. Mirrors the compressed path above AND keeps
        // the response writable by the server-level disclosures that run after
        // every tool call (see `finalize_search_results`).
        let mut out = finalize_search_results(
            results,
            match_confidence,
            vector_only_no_anchor,
            has_exact_name_match,
            vector_available,
        );
        shortfall.attach(&mut out);
        note_vector_channel(&mut out, channel);
        Ok(out)
    }
}

/// Phase 3 of `tool_semantic_search`: scored candidates into result rows.
/// Extracted so the surrounding function's control flow — retry, compress,
/// explain-empty, shape — is readable at all (audit 2026-08-22 P2-15). Pure.
fn build_search_results(candidates: &[Candidate], compact: bool) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    for c in candidates {
        let node = &c.node;
        let score = c.adjusted_score;

        if compact {
            results.push(json!({
                "node_id": node.id,
                "name": node.name,
                "type": node.node_type,
                "file_path": c.file_path,
                "line": format!("{}-{}", node.start_line, node.end_line),
                "signature": node.signature,
                "relevance": score,
            }));
        } else {
            let code = if node.code_content.len() > MAX_SEARCH_CODE_LEN {
                let safe_end = node.code_content.floor_char_boundary(MAX_SEARCH_CODE_LEN);
                let truncated = &node.code_content[..node.code_content[..safe_end]
                    .rfind('\n')
                    .unwrap_or(safe_end)];
                format!(
                    "{}\n// ... truncated ({} lines total, use get_ast_node for full code)",
                    truncated,
                    node.end_line - node.start_line + 1
                )
            } else {
                node.code_content.clone()
            };
            results.push(json!({
                "node_id": node.id,
                "name": node.name,
                "type": node.node_type,
                "file_path": c.file_path,
                "start_line": node.start_line,
                "end_line": node.end_line,
                "code_content": code,
                "signature": node.signature,
                "relevance": score,
            }));
        }
    }

    results
}

/// The Context Sandbox tail: when the payload would blow the token budget,
/// return the compressed envelope instead. `None` means "small enough, keep the
/// full results" (audit 2026-08-22 P2-15).
#[allow(clippy::too_many_arguments)]
fn try_compress_results(
    candidates: &[Candidate],
    compact: bool,
    match_confidence: f64,
    vector_available: bool,
    vector_only_no_anchor: bool,
    has_exact_name_match: bool,
) -> Result<Option<serde_json::Value>> {
    // Context Sandbox: compress only if results likely exceed token threshold.
    // Skip compression when compact=true — compact results are already token-efficient
    // (~85% smaller than full results) and contain fields (relevance, signature)
    // that would be lost by compression.
    //
    // Estimation must mirror the actual result payload: code_content is capped at
    // MAX_SEARCH_CODE_LEN per result, and context_string is NOT included in
    // the output. Estimating from raw context_string massively overestimates and
    // fires compression even for small top_k (e.g. 3) responses that would fit
    // comfortably under the token budget.
    //
    // The formula lives in `compressor::estimate_result_tokens` so this gate
    // and the compression LEVEL selector cannot drift apart — they did, and
    // the selector was reading context_string until the 2026-07-27 audit.
    use crate::sandbox::compressor::CompressedOutput;
    let estimated_tokens: usize = if compact {
        0
    } else {
        candidates
            .iter()
            .map(|c| {
                crate::sandbox::compressor::estimate_result_tokens(
                    &c.node.code_content,
                    MAX_SEARCH_CODE_LEN,
                    c.node.signature.as_deref(),
                    &c.node.name,
                    &c.file_path,
                )
            })
            .sum()
    };
    if estimated_tokens > COMPRESSION_TOKEN_THRESHOLD {
        // Build node_results and file_paths only when compression is needed
        // NodeResult is not Clone; rebuild the rows the compressor needs.
        let node_results: Vec<queries::NodeResult> = candidates
            .iter()
            .map(|c| {
                let node = &c.node;
                queries::NodeResult {
                    id: node.id,
                    file_id: node.file_id,
                    node_type: node.node_type.clone(),
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                    code_content: node.code_content.clone(),
                    signature: node.signature.clone(),
                    doc_comment: node.doc_comment.clone(),
                    context_string: node.context_string.clone(),
                    name_tokens: node.name_tokens.clone(),
                    return_type: node.return_type.clone(),
                    param_types: node.param_types.clone(),
                    is_test: node.is_test,
                }
            })
            .collect();
        let file_paths: Vec<String> = candidates.iter().map(|c| c.file_path.clone()).collect();
        if let Some(compressed) = crate::sandbox::compressor::compress_if_needed(
            &node_results,
            &file_paths,
            COMPRESSION_TOKEN_THRESHOLD,
            // Same number that opened this branch: the level selector used
            // to re-derive its own from context_string, which is not part of
            // the payload (audit 2026-07-27).
            estimated_tokens,
        )? {
            let (mode, compact) = match compressed {
                CompressedOutput::Nodes(nodes) => {
                    let items: Vec<serde_json::Value> = nodes
                        .iter()
                        .map(|c| {
                            json!({
                                "node_id": c.node_id,
                                "file_path": c.file_path,
                                "summary": c.summary,
                            })
                        })
                        .collect();
                    ("compressed_nodes", items)
                }
                CompressedOutput::Files(groups) => {
                    let items: Vec<serde_json::Value> = groups
                        .iter()
                        .map(|g| {
                            json!({
                                "file_path": g.file_path,
                                "summary": g.summary,
                                "node_ids": g.node_ids,
                            })
                        })
                        .collect();
                    ("compressed_files", items)
                }
                CompressedOutput::Directories(groups) => {
                    let items: Vec<serde_json::Value> = groups
                        .iter()
                        .map(|g| {
                            json!({
                                "file_path": g.file_path,
                                "summary": g.summary,
                                "node_ids": g.node_ids,
                            })
                        })
                        .collect();
                    ("compressed_directories", items)
                }
            };
            // match_confidence (FTS/vector agreement + coverage) is always surfaced as a
            // rough query-shape signal. The warning is separate and fires only when the
            // ranking has no text anchor (see vector_only_no_anchor); `has_exact_name_match`
            // (hoisted above) exempts precise single-identifier queries.
            let mut out = json!({
                "mode": mode,
                "message": "Results exceeded token limit. Use get_ast_node(node_id) to expand individual symbols.",
                "match_confidence": (match_confidence * 100.0).round() / 100.0,
                "search_mode": if vector_available { "hybrid" } else { "fts_only" },
                "vector_available": vector_available,
                "results": compact
            });
            if vector_only_no_anchor && !has_exact_name_match {
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("low_confidence_warning".into(), json!(VECTOR_ONLY_WARNING));
                }
            }
            return Ok(Some(out));
        }
    } // end estimated_tokens check
    Ok(None)
}

/// What a short `results` array cannot say about itself.
///
/// A truncated answer is byte-identical to a complete one: the caller sees three
/// results and cannot tell whether the repo holds three matches or three hundred
/// with the pool consumed before `top_k` was filled. The CLI twin says so on
/// stderr (`src/cli/commands/search.rs:457`); an MCP client has no stderr, so
/// here the finding has to ride in the envelope or reach nobody — which is what
/// it did (audit 2026-09-05 §15).
///
/// `saturated` is deliberately a conjunction rather than `dropped > 0`: a pool
/// that did NOT come back full was read to its end, so a short answer from it is
/// COMPLETE, and flagging that would teach callers to ignore the field.
#[derive(Debug, Default, Clone, Copy)]
struct PoolShortfall {
    /// Rows the caller's language/node_type filter removed after the fetch.
    dropped_by_filter: usize,
    /// Rows the always-on module/external/test filter removed.
    skipped_noise: usize,
    /// Size of the pool the RETURNED candidates came from — the retry's only when
    /// the retry was adopted. On a retry that ran and was not adopted this names
    /// a pool up to 4x smaller than the one actually read; that is deliberate, so
    /// this number and the two counts beside it describe one fetch and add up.
    /// Whether the wider fetch came back full is carried separately, by
    /// `widest_fetch_was_full` at the call site.
    pool_len: usize,
    /// That pool came back FULL and was still consumed before `top_k` was
    /// filled, so rows below the fetch cut were never examined.
    saturated: bool,
    /// The `top_k` this answer was built for, so the note can tell whether
    /// raising it is still an instruction the caller can follow.
    top_k: i64,
}

impl PoolShortfall {
    /// One sentence naming the pool that ran out. Appended to the empty-result
    /// arms whose standing advice — broaden the filter — cannot help when the
    /// filter is not what removed the match. Empty when nothing was cut off, so
    /// the unaffected arms keep their wording verbatim.
    ///
    /// Kept short on purpose: `truncate_large_strings` (see `server/helpers.rs`'s
    /// `TRUNCATE_MIN_LEN = 200`) cuts longer strings mid-sentence on a large
    /// compact response, and an advisory that ends in "…" advises nothing.
    /// Measured after `trim()` at the largest reachable pool size (1600, the
    /// filtered fetch at the top_k ceiling): 129 bytes on the ordinary arm and
    /// 133 on the at-ceiling arm. This line has now been wrong twice — it claimed
    /// 168 uncounted, then 161, which was the length of a clause deleted in the
    /// same commit that kept the number. Re-count it when the wording changes.
    ///
    /// That margin protects the standalone `pool_saturated_note` only. On the
    /// two empty-result arms this sentence is APPENDED into `hint`, and the
    /// composed hint runs past 200 bytes, so it remains eligible for truncation
    /// there. Left as is rather than shortened further: those arms carry the same
    /// text verbatim in `pool_saturated_note`, which is not composed and not
    /// truncated, so the finding survives the cut either way.
    fn exhaustion_note(&self) -> String {
        if !self.saturated {
            return String::new();
        }
        // At the clamp ceiling "raise top_k" is an instruction the caller cannot
        // follow — `top_k: 200` is silently clamped to 100, so the same answer
        // comes back with the same advice. Same defect `HintStyle::limit_remedy`
        // exists to avoid in `ast_search`, and the ceiling is READ from
        // `COUNT_RANGES` rather than restated, because the comment on that table
        // records a release where two independent literal 100s drifted apart.
        let ceiling = count_range("semantic_code_search", "top_k").map(|(_, hi)| hi);
        if ceiling.is_some_and(|hi| self.top_k as u64 >= hi) {
            return format!(
                " The candidate pool ({} rows) came back full at the maximum top_k, so matches may sit below the cut. Narrow the query to reach them.",
                self.pool_len
            );
        }
        // Speaks only about the POOL. An earlier version ended "…; broadening the
        // filter will not", which is false on the arm that appends it most often:
        // that arm fires on `filtered && dropped_by_filter > 0`, i.e. exactly when
        // the filter DID remove candidates the query matched, so the sentence
        // contradicted the one before it (pre-ship review round 3). Each arm now
        // states its own remedy and this states the pool's.
        format!(
            " The candidate pool ({} rows) came back full before top_k was filled, so matches may sit below the cut. Raise top_k to widen it.",
            self.pool_len
        )
    }

    /// Attach the disclosure to any of this tool's response envelopes.
    ///
    /// Silent unless something was actually cut off, so the common complete
    /// answer keeps its current shape byte for byte.
    ///
    /// Plain `insert` for all four keys. An earlier version used `entry` for the
    /// two counts and justified it as deferring to the empty-result arms' "more
    /// specific" values — a mechanism that does not exist: those arms now read
    /// the SAME `PoolShortfall`, so the values are identical, and the two
    /// producers that could differ (`finalize_search_results`,
    /// `try_compress_results`) write neither key.
    fn attach(&self, out: &mut serde_json::Value) {
        if !self.saturated {
            return;
        }
        let Some(obj) = out.as_object_mut() else {
            return;
        };
        obj.insert("pool_saturated".into(), json!(true));
        obj.insert(
            "pool_saturated_note".into(),
            json!(self.exhaustion_note().trim()),
        );
        if self.dropped_by_filter > 0 {
            obj.insert("dropped_by_filter".into(), json!(self.dropped_by_filter));
        }
        if self.skipped_noise > 0 {
            obj.insert("skipped_noise".into(), json!(self.skipped_noise));
        }
    }
}

/// Every "no results" answer this tool can give, and why each is a different
/// sentence: an explicit filter dropped real matches, the always-on noise
/// filter did, the text channel never ran, or the query genuinely missed.
/// Extracted intact (audit 2026-08-22 P2-15) — the distinctions ARE the
/// content, each having been a false diagnosis at some past release.
fn explain_empty_results(
    query: &str,
    filtered: bool,
    shortfall: &PoolShortfall,
    fts_not_searched: Option<&str>,
    vector_available: bool,
) -> serde_json::Value {
    let dropped_by_filter = shortfall.dropped_by_filter;
    let skipped_noise_count = shortfall.skipped_noise;
    // Filter-aware: if a language/node_type filter removed candidates that DID
    // match the query, say so — the index has matches, just not of this
    // language/type. (vec0 can't pre-filter, so this is a post-fetch drop.)
    if filtered && dropped_by_filter > 0 {
        return json!({
            "results": [],
            "message": "No matching symbols after filtering.",
            "dropped_by_filter": dropped_by_filter,
            // BOTH remedies, because on this arm both are real: the guard above is
            // `filtered && dropped_by_filter > 0`, so the filter did remove
            // candidates the query matched, AND the pool ran out. An earlier
            // version replaced the filter remedy with the pool's and left the
            // caller told, in consecutive sentences, that the filter removed 100
            // matches and that broadening it would not help (round 3).
            "hint": if shortfall.saturated {
                format!(
                    "{} candidate(s) matched the query but were removed by the active language/node_type filter — broadening or clearing it recovers those.{}",
                    dropped_by_filter,
                    shortfall.exhaustion_note()
                )
            } else {
                format!(
                    "{} candidate(s) matched the query but were removed by the active language/node_type filter. Broaden or clear the filter, or raise top_k.",
                    dropped_by_filter
                )
            },
            // The envelope contract ("ONE envelope on every path", see
            // `finalize_search_results`) — this was the only branch that omitted
            // both fields, so a caller reading `search_mode` had to special-case
            // the filter-emptied answer. No `note` is built here; the caller
            // adds the vector-channel note (`note_vector_channel`) to this
            // answer as to every other.
            "search_mode": if vector_available { "hybrid" } else { "fts_only" },
            "vector_available": vector_available
        });
    }
    // Same disclosure duty for the always-on filter: the query DID match,
    // and every match was a `<module>`/`<external>` placeholder or a test
    // symbol. Saying "check spelling / the index may need rebuilding"
    // there is a false diagnosis — the index is fine and the spelling
    // found rows (audit 2026-08-16 P1-7).
    if skipped_noise_count > 0 {
        return json!({
            "results": [],
            "message": format!(
                "No matching symbols — {} candidate(s) matched the query but are module/external placeholders or test symbols, which this tool always excludes.",
                skipped_noise_count
            ),
            "skipped_noise": skipped_noise_count,
            // Appended here rather than substituted: this arm's advice (reach the
            // test symbols another way) stays correct on a consumed pool, it is
            // just no longer the whole story.
            "hint": format!(
                "Spelling and index freshness are not the problem. To reach test symbols use `find_references` with include_tests, or `code-graph-mcp grep`; for structural enumeration use `ast_search`.{}",
                shortfall.exhaustion_note()
            ),
            "search_mode": if vector_available { "hybrid" } else { "fts_only" },
            "vector_available": vector_available
        });
    }
    // The text channel never ran (single characters, stop words), so
    // "check spelling / the index may need rebuilding" below would be a
    // false diagnosis of a search that did not happen (2026-08-16 audit
    // §四). With no vector channel either, nothing was searched at all.
    if let Some(reason) = fts_not_searched {
        return json!({
            "results": [],
            "message": format!("Text search did not run: {reason}."),
            "not_searched": reason,
            "hint": if vector_available {
                "Only the vector channel ran, and it found nothing above threshold. Spelling and index freshness are not the problem — use a longer or more specific term."
            } else {
                "Nothing was searched for. Spelling and index freshness are not the problem — use a longer or more specific term."
            },
            "search_mode": if vector_available { "hybrid" } else { "fts_only" },
            "vector_available": vector_available
        });
    }
    let has_code_syntax = query.contains('(')
        || query.contains(')')
        || query.contains("->")
        || query.contains("::")
        || query.contains('<');
    let has_non_ascii = !query.is_ascii();
    let hint = if has_code_syntax {
        "Query looks like code syntax. For structural queries, use ast_search with type/returns/params filters instead of text search."
    } else if has_non_ascii {
        "Try using English keywords — the search index is English-optimized. Also try broader terms or check spelling."
    } else {
        "Try broader terms, check spelling, or use different keywords. The index may need rebuilding if the codebase changed significantly."
    };
    let mut out = json!({
        "results": [],
        "message": "No matching symbols found.",
        "hint": hint,
        "search_mode": if vector_available { "hybrid" } else { "fts_only" },
        "vector_available": vector_available
    });
    // The non-empty answer carries the degradation note; the EMPTY one did not,
    // and empty is where it changes the reader's conclusion. On an FTS-only
    // install this tool is keyword matching wearing the name `semantic_code_search`,
    // so "check spelling / the index may need rebuilding" sends a caller to fix an
    // index that is fine when the real cause is that the semantic channel never ran
    // — the false-diagnosis class the branches above exist to avoid.
    if !vector_available {
        out["note"] = json!(fts_only_note());
    }
    out
}

/// Notice attached when a semantic-search result set has NO text anchor — FTS
/// returned nothing, so the ranking is vector similarity alone, the one condition
/// where "vector-similarity only" is literally true. Shared by the compressed
/// (large-result) and bare-array (small-result) returns so the two never drift.
///
/// It is deliberately NOT keyed on a match_confidence threshold: the calibration
/// bench (scripts/embedding_benchmark/eval_confidence.py) refuted match_confidence,
/// RRF relevance, AND raw top-1 vector similarity as separators of good-NL from nonsense, so
/// the old `<0.5` trigger warned on ~every natural-language query (100% of good NL
/// in the corpus) while they returned relevant results. The message states the
/// mechanic and explicitly does not claim the results are wrong.
const VECTOR_ONLY_WARNING: &str = "No exact text matches — results are ranked by vector similarity alone (no keyword anchor). Vague or natural-language queries often land here yet still return relevant symbols, so judge by the results; if they miss, add a concrete identifier or use ast_search with type/returns/params filters.";

/// What the vector half of a semantic search has to work with (B7,
/// 2026-09-29 usage evaluation). `search_mode` said `hybrid` whenever the model
/// was loaded — also before the backfill had embedded anything, when the KNN
/// half returns nothing and every result is FTS5's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VectorChannel {
    Ready,
    /// No model loaded, or no vector storage: [`fts_only_note`].
    NoModel,
    /// A loaded model and an empty vector table: the backfill has not stored
    /// its first batch yet.
    NotEmbedded,
}

/// The channel, probing the vector table only when a model is loaded.
fn vector_channel(model_loaded: bool, has_vectors: impl FnOnce() -> bool) -> VectorChannel {
    if !model_loaded {
        VectorChannel::NoModel
    } else if has_vectors() {
        VectorChannel::Ready
    } else {
        VectorChannel::NotEmbedded
    }
}

/// Replace the no-model note with the right cause when the model IS loaded
/// but nothing is embedded yet. The answer's `search_mode` / `vector_available`
/// already read `fts_only` / `false` from `vector_available`.
fn note_vector_channel(out: &mut serde_json::Value, channel: VectorChannel) {
    if channel == VectorChannel::NotEmbedded {
        out["note"] = json!(
            "The embedding model is loaded, but this index is not embedded yet (the background \
             backfill has not stored its first batch) — results are FTS5-only until it does. \
             `code-graph-mcp health-check` shows embedding progress."
        );
    }
}

/// The one wording for "this answer had no vector channel", shared by every
/// branch that owes it.
///
/// Two histories meet here. "retry shortly" was printed unconditionally, so a
/// machine whose download can never succeed got a wait-and-see message forever
/// (issue #35) — hence the recorded last outcome. And `default = []`, so every
/// `cargo install code-graph-mcp` runs a binary with no downloader AT ALL and was
/// told, on every query, to wait for a background download that cannot start —
/// hence the compile-time arm, which names the same cause `health-check` ("binary
/// built without embed-model feature") and `similar` already name on that build.
fn fts_only_note() -> String {
    #[cfg(feature = "embed-model")]
    let last = crate::embedding::model::EmbeddingModel::download_state_summary();
    #[cfg(not(feature = "embed-model"))]
    let last: Option<String> = None;
    if !cfg!(feature = "embed-model") {
        return "Embedding model not compiled in — results are FTS5-only (reduced semantic \
                recall). This binary was built without the `embed-model` feature; no model \
                download will happen. To enable vector search, reinstall with \
                `cargo install code-graph-mcp --features embed-model` (npm/npx builds ship it), \
                then restart the MCP server to backfill embeddings."
            .to_string();
    }
    match last {
        Some(s) => format!(
            "Embedding model not loaded — results are FTS5-only (reduced semantic recall). \
             Last model download: {}. Run `code-graph-mcp doctor` for detail.",
            s
        ),
        None => "Embedding model not loaded — results are FTS5-only (reduced semantic recall). \
                 The model auto-downloads in the background on first use; retry shortly, or \
                 run `code-graph-mcp doctor` to check status."
            .to_string(),
    }
}

/// Build the response for an uncompressed semantic-search result set.
///
/// ONE envelope on every path: `{"results": [...], "search_mode", "vector_available",
/// …}`, matching the compressed and empty branches of [`McpServer::tool_semantic_search`].
/// The confident-hybrid path used to return a BARE ARRAY, which cost the tool both
/// server-level disclosures — `note_ignored_arguments` and `refresh_result_set`
/// attach through `as_object_mut()` and silently no-op on an array, so a misspelled
/// argument and a stale-file warning both evaporated on the most common response of
/// the most-called tool (audit 2026-08-16 P1-10).
///
/// Arm-specific fields on top of the envelope:
/// - vector unavailable → `search_mode: "fts_only"` + the degradation `note`.
/// - vector-only (no FTS anchor, and not an exact-identifier hit) →
///   `low_confidence_warning`, so a query whose ranking rests on vector similarity
///   alone carries the signal. Low `match_confidence` WITH a text anchor does not
///   warn: those are overwhelmingly good natural-language queries (see
///   [`VECTOR_ONLY_WARNING`]).
fn finalize_search_results(
    results: Vec<serde_json::Value>,
    match_confidence: f64,
    vector_only: bool,
    has_exact_name_match: bool,
    vector_available: bool,
) -> serde_json::Value {
    if !vector_available {
        let note = fts_only_note();
        return json!({
            "results": results,
            "search_mode": "fts_only",
            "vector_available": false,
            "note": note
        });
    }
    let mut out = json!({
        "results": results,
        "search_mode": "hybrid",
        "vector_available": vector_available,
        "match_confidence": (match_confidence * 100.0).round() / 100.0,
    });
    if vector_only && !has_exact_name_match {
        out["low_confidence_warning"] = json!(VECTOR_ONLY_WARNING);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_results() -> Vec<serde_json::Value> {
        vec![json!({"node_id": 1, "name": "foo", "relevance": 0.4})]
    }

    /// Index where the FTS pool for "widget" is dominated by triad noise, built
    /// on the mechanism that produces it in production: symbols under `tests/`
    /// pass the FTS `is_test = 0` column filter (only `#[test]`-shaped symbols
    /// set that column) but `domain::is_test_symbol` rejects them on the PATH,
    /// so they are fetched and then dropped in Rust. The dual-classifier gap is
    /// the same one the retrieval benchmark documents.
    ///
    /// 30 short `widget_helper_*` functions under `tests/` outrank the 5 real
    /// matches in `src/real.py`, which are long and mention the term once.
    /// "widgetonly" appears in the `tests/` helpers alone, so every candidate
    /// for that query is noise.
    fn noise_dominated_project() -> tempfile::TempDir {
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        let tests = project.path().join("tests");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&tests).unwrap();
        for i in 0..30 {
            std::fs::write(
                tests.join(format!("helpers_{i}.py")),
                format!(
                    "def widget_helper_{i}(widget):\n    return widget + widget + {i}\n\ndef widgetonly_helper_{i}(widgetonly):\n    return widgetonly\n"
                ),
            )
            .unwrap();
        }
        let mut real = String::new();
        for name in [
            "alpha_one",
            "alpha_two",
            "alpha_three",
            "alpha_four",
            "alpha_five",
        ] {
            real.push_str(&format!("def {name}(value):\n"));
            for line in 0..40 {
                real.push_str(&format!("    step_{line} = value + {line}\n"));
            }
            real.push_str("    return widget(value)\n\n");
        }
        std::fs::write(src.join("real.py"), real).unwrap();
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"fixture_lib\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        project
    }

    fn indexed_server(project: &tempfile::TempDir) -> McpServer {
        let server = McpServer::new_test_with_project(project.path());
        crate::indexer::pipeline::run_full_index(&server.db, project.path(), None, None).unwrap();
        server
    }

    /// A row named exactly like the query survives the fused-pool cut.
    ///
    /// The cut runs before the exact-name rerank, so a match the vector
    /// channel outranked fell off the pool before the bonus could lift it:
    /// on flask, `send_file` was absent at top_k 1–10 (pool 20) and first at
    /// top_k 12 (pool 48). This harness has no vector channel, so the pool
    /// order is built by hand; the fusion is order-stable under a wider cap
    /// (scores depend on channel ranks only), which is what lets the caller
    /// fuse once uncapped and cut here.
    #[test]
    fn test_cap_keeps_exact_name_rows_past_the_cut() {
        use crate::search::fusion::SearchResult;
        let project = tempfile::TempDir::new().unwrap();
        std::fs::write(
            project.path().join("lib.py"),
            "def send_file(p):\n    return p\n\ndef other_a():\n    return 1\n\n\
             def other_b():\n    return 2\n\ndef other_c():\n    return 3\n",
        )
        .unwrap();
        let server = indexed_server(&project);
        let conn = server.db.conn();
        let id_of = |name: &str| -> i64 {
            conn.query_row("SELECT id FROM nodes WHERE name = ?1", [name], |r| r.get(0))
                .unwrap()
        };
        let pool: Vec<SearchResult> = [
            ("other_a", 0.9),
            ("other_b", 0.8),
            ("other_c", 0.7),
            ("send_file", 0.6),
        ]
        .iter()
        .map(|(n, s)| SearchResult {
            node_id: id_of(n),
            score: *s,
        })
        .collect();
        let ids = |q: &str| -> Vec<i64> {
            cap_keeping_exact_names(conn, pool.clone(), 2, q)
                .unwrap()
                .iter()
                .map(|r| r.node_id)
                .collect()
        };

        // The capped prefix keeps its order; the exact match past the cut is
        // appended; a non-matching row past the cut stays cut.
        let expected = vec![id_of("other_a"), id_of("other_b"), id_of("send_file")];
        assert_eq!(ids("send_file"), expected);
        // Same case and whitespace rule as the rerank's `is_exact_name`.
        assert_eq!(ids("  SEND_FILE "), expected);
        // A query that names nothing past the cut is the plain cut.
        assert_eq!(ids("send"), vec![id_of("other_a"), id_of("other_b")]);
    }

    /// A CJK substring rescue must reach the response carrying the widened-match
    /// penalty.
    ///
    /// `FtsResult::is_widened_match()` is read at exactly one place, and the
    /// unit test beside it pins the truth table rather than the use — replacing
    /// the call with `fts_result.or_fallback` leaves the whole suite green while
    /// every rescued CJK query silently ships full confidence.
    ///
    /// `match_confidence` itself is only emitted when a vector channel exists,
    /// and this harness hard-codes `embedding_model: None`, so it is asserted
    /// through `relevance`, which carries it on every build:
    ///
    ///   relevance = score/max_rrf * query_quality * match_confidence * 100
    ///              * name_boost * size_dampening
    ///
    /// Both queries resolve the SAME single node, so score/max_rrf is 1.0,
    /// name_boost and size_dampening are identical, and both are one token
    /// longer than two bytes so query_quality is 0.7 for each. Everything
    /// cancels except the factor under test, leaving the ratio at exactly
    /// `CONF_OR_FALLBACK_PENALTY`. Under the mutation it is 1.0.
    #[test]
    fn test_cjk_rescue_takes_the_widened_match_penalty() {
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // `gateway` is its own FTS token; 订单 sits INSIDE the unspaced run
        // 创建订单并扣减库存, so only the substring scan can reach it.
        std::fs::write(
            src.join("handler.py"),
            "def handler(req):\n    \"\"\"gateway 创建订单并扣减库存\"\"\"\n    return req\n",
        )
        .unwrap();
        let server = indexed_server(&project);

        let top_relevance = |q: &str| -> f64 {
            let out = server
                .tool_semantic_search(&json!({"query": q, "top_k": 5, "skip_indexing": true}))
                .unwrap();
            let results = out["results"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                results.len(),
                1,
                "fixture must resolve exactly one node for {q:?}, else the ratio \
                 below compares different rows: {out}"
            );
            assert_eq!(results[0]["name"].as_str(), Some("handler"), "{out}");
            results[0]["relevance"].as_f64().unwrap()
        };

        let direct = top_relevance("gateway");
        let rescued = top_relevance("订单");
        assert!(direct > 0.0, "control: the direct hit must score");

        let ratio = rescued / direct;
        let expected = crate::domain::CONF_OR_FALLBACK_PENALTY;
        assert!(
            (ratio - expected).abs() < 0.02,
            "a rescued CJK query must ship the widened-match penalty: \
             rescued {rescued} / direct {direct} = {ratio}, expected {expected} \
             (1.0 means the call site stopped reading is_widened_match)"
        );
    }

    /// The always-on `<module>`/`<external>`/test filter must not silently eat
    /// the candidate pool: when it consumes the fetch before `top_k` is filled,
    /// the pool has to widen, exactly as it does for language/node_type filters.
    ///
    /// Measured pre-fix (audit 2026-08-16 P1-7): top_k=3 fetched 20, every one
    /// of them was dropped by the bare `continue`, and the caller got zero
    /// results while 5 real matches sat just below the cut.
    #[test]
    fn triad_drops_widen_the_pool_instead_of_starving_top_k() {
        let project = noise_dominated_project();
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({"query": "widget", "top_k": 3, "skip_indexing": true}))
            .unwrap();
        let results = out["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            results.len(),
            3,
            "5 real `alpha_*` matches exist below the noise; top_k=3 must be filled. Got: {out}"
        );
        assert!(
            results
                .iter()
                .all(|r| r["name"].as_str().unwrap_or("").starts_with("alpha_")),
            "every result must be a real symbol, got: {out}"
        );
    }

    /// When every candidate was dropped as module/external/test noise, the
    /// response must say THAT — not blame the user's spelling or suggest a
    /// rebuild. The index is fine and the query matched; the matches were noise.
    #[test]
    fn empty_after_noise_drops_names_the_noise_not_the_speller() {
        let project = noise_dominated_project();
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(
                &json!({"query": "widgetonly", "top_k": 5, "skip_indexing": true}),
            )
            .unwrap();
        assert_eq!(
            out["results"].as_array().map(|a| a.len()),
            Some(0),
            "fixture must produce an empty result for this test to mean anything: {out}"
        );
        let text = format!("{} {}", out["message"], out["hint"]);
        assert!(
            text.contains("test symbols") && text.contains("placeholder"),
            "the empty response must name the always-on filter that consumed the candidates; got: {out}"
        );
        assert!(
            text.contains("20"),
            "and how many candidates it consumed; got: {out}"
        );
        assert!(
            !text.contains("check spelling"),
            "the query matched — blaming spelling is a false diagnosis; got: {out}"
        );
        assert!(
            out["skipped_noise"].as_u64().unwrap_or(0) > 0,
            "the drop count must be reported, like dropped_by_filter is; got: {out}"
        );
    }

    #[test]
    fn vector_only_result_carries_the_warning() {
        // The one honest trigger: no text anchor (fts empty → vector-only ranking).
        let out = finalize_search_results(dummy_results(), 0.30, true, false, true);
        assert!(out.is_object(), "vector-only result must wrap in an object");
        assert_eq!(out["match_confidence"], 0.3);
        assert!(out["low_confidence_warning"]
            .as_str()
            .unwrap()
            .contains("vector similarity alone"));
        assert!(out["results"].is_array());
    }

    /// ONE envelope on every path. The confident-hybrid arm used to return a
    /// BARE ARRAY, and the two server-level disclosures that run after every
    /// tool call — `note_ignored_arguments` (`ignored_arguments`) and
    /// `refresh_result_set` (`freshness`) — both write through
    /// `Value::as_object_mut()`, so on the most frequent response of the most
    /// frequent tool they silently no-opped: a misspelled argument vanished and
    /// a stale-file warning was dropped (audit 2026-08-16 P1-10). Shape, not
    /// content, is the fix — this asserts it for all four arms at once so a
    /// future arm cannot reintroduce the array.
    #[test]
    fn every_response_shape_is_an_object_that_can_carry_disclosures() {
        let arms = [
            (
                "confident hybrid",
                finalize_search_results(dummy_results(), 0.85, false, false, true),
            ),
            (
                "low confidence with anchor",
                finalize_search_results(dummy_results(), 0.45, false, false, true),
            ),
            (
                "vector-only",
                finalize_search_results(dummy_results(), 0.30, true, false, true),
            ),
            (
                "exact-name match",
                finalize_search_results(dummy_results(), 0.20, true, true, true),
            ),
            (
                "vector unavailable",
                finalize_search_results(dummy_results(), 0.90, false, false, false),
            ),
        ];
        for (arm, mut out) in arms {
            assert!(
                out.is_object(),
                "{arm}: response must be an object (a bare array cannot carry ignored_arguments/freshness), got: {out}"
            );
            assert!(
                out["results"].is_array(),
                "{arm}: the result list must live under `results`, got: {out}"
            );
            assert_eq!(
                out["results"].as_array().unwrap().len(),
                1,
                "{arm}: results must survive the wrap"
            );
            // The exact operation both server-level disclosures perform.
            out.as_object_mut()
                .expect("checked above")
                .insert("ignored_arguments".into(), json!(["bogus"]));
            assert_eq!(out["ignored_arguments"], json!(["bogus"]), "{arm}");
        }
    }

    #[test]
    fn low_confidence_with_text_anchor_no_longer_warns() {
        // A low match_confidence (0.45 — the pin for good NL queries) that HAS a text
        // anchor (vector_only=false) carries NO warning. The old match_confidence<0.5
        // trigger warned here — on 100% of good NL queries — even though they retrieve
        // relevant results (bench: eval_confidence.py).
        let out = finalize_search_results(dummy_results(), 0.45, false, false, true);
        assert!(
            out.get("low_confidence_warning").is_none(),
            "low confidence WITH a text anchor must not warn, got: {out}"
        );
        assert_eq!(out["search_mode"], "hybrid");
    }

    #[test]
    fn confident_hybrid_carries_no_warning() {
        // Confident results: the envelope, no warning, no degradation note.
        let out = finalize_search_results(dummy_results(), 0.85, false, false, true);
        assert_eq!(out["match_confidence"], 0.85);
        assert_eq!(out["vector_available"], true);
        assert!(
            out.get("low_confidence_warning").is_none() && out.get("note").is_none(),
            "confident hybrid must carry no caveat, got: {out}"
        );
    }

    #[test]
    fn exact_name_match_is_exempt_from_the_warning() {
        // A precise single-identifier hit is trustworthy even with no FTS breadth —
        // no warning despite being vector-only.
        let out = finalize_search_results(dummy_results(), 0.20, true, true, true);
        assert!(
            out.get("low_confidence_warning").is_none(),
            "exact-name match is warning-exempt, got: {out}"
        );
    }

    #[test]
    fn vector_unavailable_reports_fts_only_degradation() {
        let out = finalize_search_results(dummy_results(), 0.90, false, false, false);
        assert_eq!(out["search_mode"], "fts_only");
        assert_eq!(out["vector_available"], false);
        assert!(
            out.get("low_confidence_warning").is_none(),
            "FTS-only degradation is a separate signal from the vector-only warning"
        );
    }

    /// `default = []`, so `cargo install code-graph-mcp` produces a binary with
    /// no downloader at all. The degradation note used to tell that build, on
    /// every single query, that "the model auto-downloads in the background on
    /// first use; retry shortly" — an instruction that can never come true, and
    /// the opposite of what `health-check` ("binary built without embed-model
    /// feature") and `similar` print on the same binary.
    #[cfg(not(feature = "embed-model"))]
    #[test]
    fn fts_only_note_does_not_promise_a_download_that_cannot_happen() {
        let out = finalize_search_results(dummy_results(), 0.90, false, false, false);
        let note = out["note"].as_str().unwrap_or_default();
        // The empty answer owes the same note: "no matching symbols" on an
        // FTS-only index reads as "this code does not exist" unless the response
        // says the semantic channel never ran.
        let empty = explain_empty_results(
            "handle user login",
            false,
            &PoolShortfall::default(),
            None,
            false,
        );
        assert_eq!(
            empty["note"].as_str(),
            Some(note),
            "empty and non-empty answers must carry the same degradation note, got: {empty}"
        );
        assert_eq!(empty["search_mode"], "fts_only");

        // The filter-emptied arm returns EARLY, so the assertions above never
        // reach it — it needs its own call (`dropped_by_filter > 0`). It was the
        // one path with no `search_mode` / `vector_available` at all, against a
        // doc-comment promising "ONE envelope on every path".
        let filtered = explain_empty_results(
            "widget",
            true,
            &PoolShortfall {
                dropped_by_filter: 7,
                ..Default::default()
            },
            None,
            false,
        );
        assert_eq!(filtered["search_mode"], "fts_only");
        assert_eq!(filtered["vector_available"], false);
        assert_eq!(filtered["dropped_by_filter"], 7);
        // No degradation note here: the cause is known and named, and it is the
        // filter — not the missing vector channel.
        assert!(
            filtered.get("note").is_none(),
            "the filter-emptied answer explains itself; got: {filtered}"
        );
        assert!(
            !note.contains("auto-downloads") && !note.contains("retry shortly"),
            "no download can start in a build without `embed-model`, got: {note}"
        );
        assert!(
            note.contains("embed-model"),
            "the note must name the missing feature — it is the only remedy, got: {note}"
        );
    }

    /// B7: `search_mode` said `hybrid` whenever the model was loaded — also
    /// over an index with no vectors yet, where KNN returns nothing and every
    /// result is FTS5's. The channel is ready only with a model AND vectors.
    #[test]
    fn the_vector_channel_needs_a_model_and_at_least_one_vector() {
        assert_eq!(vector_channel(false, || true), VectorChannel::NoModel);
        assert_eq!(vector_channel(true, || false), VectorChannel::NotEmbedded);
        assert_eq!(vector_channel(true, || true), VectorChannel::Ready);
        // No model: the vector table is not even asked.
        assert_eq!(
            vector_channel(false, || panic!("probed without a model")),
            VectorChannel::NoModel
        );
    }

    #[test]
    fn a_loaded_model_with_nothing_embedded_says_so_not_model_not_loaded() {
        let mut out = finalize_search_results(dummy_results(), 0.90, false, false, false);
        note_vector_channel(&mut out, VectorChannel::NotEmbedded);
        let note = out["note"].as_str().unwrap_or_default();
        assert!(note.contains("not embedded yet"), "{note}");
        assert!(!note.contains("not loaded"), "{note}");
        assert_eq!(out["search_mode"], "fts_only");

        let mut ready = finalize_search_results(dummy_results(), 0.90, false, false, true);
        note_vector_channel(&mut ready, VectorChannel::Ready);
        assert!(ready.get("note").is_none(), "{ready}");
        let mut none = finalize_search_results(dummy_results(), 0.90, false, false, false);
        let before = none["note"].clone();
        note_vector_channel(&mut none, VectorChannel::NoModel);
        assert_eq!(none["note"], before, "the no-model note is left as it was");
    }

    /// End to end with the real model: loaded, over an index whose vector
    /// table is empty. Needs the model on disk; without it there is nothing to
    /// check.
    #[cfg(feature = "embed-model")]
    #[test]
    fn a_search_before_any_vector_exists_reports_fts_only() {
        let project = tempfile::TempDir::new().unwrap();
        std::fs::write(
            project.path().join("a.rs"),
            "fn widget_alpha() {}\nfn widget_beta() { widget_alpha(); }\n",
        )
        .unwrap();
        let mut server = McpServer::new_test_with_project(project.path());
        // The test server opens without vector storage; a real one has it.
        server.db = crate::storage::db::Database::open_with_vec(
            &project.path().join(CODE_GRAPH_DIR).join("index.db"),
        )
        .unwrap();
        server.ensure_indexed().unwrap();
        let Some(model) = crate::embedding::model::EmbeddingModel::load().unwrap() else {
            return;
        };
        *lock_or_recover(&server.embedding_model, "embedding_model") = Some(model);
        // An index the backfill has not reached yet. (Indexing here can already
        // store vectors, copied from the embedding cache.)
        server
            .db
            .conn()
            .execute("DELETE FROM node_vectors", [])
            .unwrap();
        assert!(
            !crate::storage::queries::any_node_vector(server.db.conn()).unwrap_or(true),
            "precondition: nothing embedded"
        );
        let out = server
            .tool_semantic_search(&json!({ "query": "widget" }))
            .unwrap();
        assert_eq!(out["search_mode"], "fts_only", "{out}");
        assert_eq!(out["vector_available"], false, "{out}");
        assert!(
            out["note"]
                .as_str()
                .unwrap_or_default()
                .contains("not embedded yet"),
            "{out}"
        );
    }

    /// The feature-compiled leg keeps the wait-and-see wording: there really is
    /// a background downloader, so "retry shortly" is actionable there.
    #[cfg(feature = "embed-model")]
    #[test]
    fn fts_only_note_still_points_at_the_download_when_one_can_run() {
        let out = finalize_search_results(dummy_results(), 0.90, false, false, false);
        let note = out["note"].as_str().unwrap_or_default();
        assert!(
            !note.contains("built without the `embed-model` feature"),
            "this build HAS the feature; the not-compiled arm must not fire, got: {note}"
        );
    }

    /// A pool that comes back FULL of rows the caller's filter then removes.
    /// Ported from `setup_saturating_pool_project` (tests/cli_e2e.rs:1136) — the
    /// recipe is identical because the mechanism is: neither vec0 KNN nor FTS5
    /// can pre-filter on `language`, so a selective filter eats the fetch after
    /// it lands.
    ///
    /// `distractors` TypeScript `widgetHandler*` functions saturate the pool.
    /// Each carrier is `(name, filler_lines)`: filler decides where BM25 ranks
    /// it, so `("widget", 0)` lands inside the FIRST pool, a middling one is
    /// reachable only through the retry, and a long one stays below every cut.
    /// The tests assert which of those actually happened rather than trusting
    /// the tuning.
    fn saturating_pool_project(
        distractors: usize,
        carriers: &[(&str, usize)],
    ) -> tempfile::TempDir {
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();

        let mut pool = String::new();
        for i in 0..distractors {
            pool.push_str(&format!("function widgetHandler{i}() {{ return {i}; }}\n"));
        }
        std::fs::write(src.join("pool.ts"), &pool).unwrap();

        let mut py = String::new();
        for (name, filler) in carriers {
            py.push_str(&format!("def {name}():\n"));
            for i in 0..*filler {
                py.push_str(&format!("    filler_{i} = {i}\n"));
            }
            py.push_str("    return widget\n\n");
        }
        std::fs::write(src.join("carrier.py"), py).unwrap();
        project
    }

    /// A SHORT `results` array is byte-identical to a complete one. The CLI twin
    /// discloses the shortfall on stderr (`src/cli/commands/search.rs:457`); an
    /// MCP client has no stderr, so the envelope is the only channel the finding
    /// can ride — and it carried nothing, `finalize_search_results` not even
    /// taking the two drop counts (audit 2026-09-05 §15).
    #[test]
    fn short_answer_discloses_the_pool_it_could_not_fill() {
        // Rank is controlled by DOCUMENT LENGTH in three tiers, not by tuning a
        // filler count until it lands: 20 short test helpers outrank everything,
        // the two production symbols sit next, and 70 long test helpers fill the
        // tail. Unfiltered, so `fetch_count` is 20 and the retry is 80 — small
        // enough that the tiers decide the pools outright.
        //
        //   first pool (20 rows) = 20 short helpers    -> 0 survivors, all noise
        //   retry pool (80 rows) = those + 2 prod + …  -> 2 survivors, ADOPTED
        //
        // So the retry recovers matches and the answer is STILL short of top_k=3:
        // the one shape whose remedy really is "raise top_k". Two earlier attempts
        // tuned a Python carrier's filler count instead, and both landed outside
        // the retry pool entirely — length beats name-match against a field of
        // short name-matching distractors.
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        let tests_dir = project.path().join("tests");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&tests_dir).unwrap();
        for i in 0..20 {
            std::fs::write(
                tests_dir.join(format!("short_{i}.py")),
                format!("def widget_short_{i}():\n    return widget\n"),
            )
            .unwrap();
        }
        let mut prod = String::new();
        for name in ["widget_prod_a", "widget_prod_b"] {
            prod.push_str(&format!("def {name}():\n"));
            for j in 0..6 {
                prod.push_str(&format!("    step_{j} = {j}\n"));
            }
            prod.push_str("    return widget\n\n");
        }
        std::fs::write(src.join("prod.py"), prod).unwrap();
        for i in 0..70 {
            let mut long = format!("def widget_long_{i}():\n");
            for j in 0..25 {
                long.push_str(&format!("    pad_{j} = {j}\n"));
            }
            long.push_str("    return widget\n");
            std::fs::write(tests_dir.join(format!("long_{i}.py")), long).unwrap();
        }
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "top_k": 3, "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["results"].as_array().map(|a| a.len()),
            Some(2),
            "fixture precondition: the retry must RECOVER the second match and the \
             answer must still be short of top_k=3 — otherwise this is testing the \
             widening-found-nothing case below, not a truncation; got {out}"
        );
        assert_eq!(
            out["pool_saturated"],
            json!(true),
            "the pool came back full and was consumed before top_k=3 was filled — \
             the one fact a 2-element array cannot state about itself; got {out}"
        );
        assert!(
            out["skipped_noise"].as_u64().unwrap_or(0) > 0,
            "and the count that explains it — here the always-on noise filter, not \
             a caller filter; got {out}"
        );
        let note = out["pool_saturated_note"].as_str().unwrap_or_default();
        assert!(
            note.contains("Raise top_k to widen it"),
            "the retry recovered matches here, so widening is the remedy that WOULD \
             help and the note must say so; got {out}"
        );
        // The note speaks only about the pool now; the filter remedy lives on the
        // arm that owns it. What must never appear here is the instruction to
        // broaden a filter this note knows nothing about.
        assert!(
            !note.contains("Broaden or clear the filter"),
            "broadening the filter is the WRONG fix here — the filter is not what \
             removed the match; got {out}"
        );
    }

    /// The empty twin. `explain_empty_results`' filter arm sends the caller to
    /// "Broaden or clear the filter, or raise top_k", and on a consumed pool the
    /// first half of that sentence is the wrong instruction — the same reason
    /// the CLI spells out at `src/cli/commands/search.rs:365`.
    #[test]
    fn empty_after_a_consumed_pool_does_not_only_blame_the_filter() {
        let project = saturating_pool_project(420, &[("carrier_box", 80)]);
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "language": "python", "top_k": 1, "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["results"].as_array().map(|a| a.len()),
            Some(0),
            "fixture precondition: the match must stay below the cut even after the \
             retry, or this is the short-answer case; got {out}"
        );
        assert_eq!(out["pool_saturated"], json!(true), "got {out}");
        assert!(
            out["hint"]
                .as_str()
                .unwrap_or_default()
                .contains("came back full"),
            "the hint must say the pool ran out, not only that the filter dropped \
             rows; got {out}"
        );
    }

    /// Regression, pre-ship review round 2: a retry that read the index to its
    /// END was reported as a full pool, because the pool numbers were updated
    /// only when the retry's candidates were ADOPTED.
    ///
    /// One Python match among ~421 fusable rows. At `top_k=20` the retry fetches
    /// up to 1000 and comes back with all 421 — nothing sits below any cut, so
    /// the answer is complete and must carry no flag. Measured pre-fix: it
    /// announced "the candidate pool (320 rows) came back full", the superseded
    /// FIRST pool, and advised raising top_k on an answer that was already whole.
    ///
    /// `top_k=2` is the contrast in the same fixture: there the retry stops at
    /// 400 of 421, rows really are unread, and the flag belongs.
    #[test]
    fn a_retry_that_read_the_whole_index_reports_no_truncation() {
        let project = saturating_pool_project(420, &[("widget", 0)]);
        let server = indexed_server(&project);
        let ask = |top_k: i64| {
            server
                .tool_semantic_search(&json!({
                    "query": "widget", "language": "python",
                    "top_k": top_k, "skip_indexing": true
                }))
                .unwrap()
        };

        let wide = ask(20);
        assert_eq!(
            wide["results"].as_array().map(|a| a.len()),
            Some(1),
            "precondition: exactly one Python match exists in the index; got {wide}"
        );
        assert!(
            wide.get("pool_saturated").is_none(),
            "the retry fetched up to 1000 rows and the index holds ~421, so it was \
             read to its end — nothing was cut off and nothing may be claimed; got \
             {wide}"
        );

        let narrow = ask(2);
        assert_eq!(
            narrow["pool_saturated"],
            json!(true),
            "the contrast: at top_k=2 the retry stops at 400 of ~421, so rows below \
             the cut really were unread; got {narrow}"
        );
        assert!(
            narrow["pool_saturated_note"]
                .as_str()
                .unwrap_or_default()
                .contains("Raise top_k to widen it"),
            "and raising top_k IS the remedy there — `fetch_count` is top_k*16 \
             while the retry widens by a fixed 4x, so a bigger top_k reaches past \
             what the retry saw; got {narrow}"
        );
    }

    /// The compressed envelope is the third response shape, and removing its
    /// `attach` call left every test in this module green (pre-ship review
    /// 2026-09-07) — the one-envelope contract was asserted in a comment and
    /// covered on two paths out of three.
    ///
    /// `top_k=63` makes `fetch_count` 1008, so `search_retry_fetch_count` caps at
    /// 1000 and the retry cannot run at all: saturation here is the no-widening-
    /// available kind, which keeps the fixture to one pool.
    #[test]
    fn the_compressed_envelope_carries_the_pool_disclosure_too() {
        // Bespoke rather than `saturating_pool_project`: this one needs the
        // distractors to rank BELOW the carriers, so that filling the pool cuts
        // distractors instead of the matches. The shared fixture's one-line
        // distractors outrank everything, and its first build of this test
        // returned 0 results for exactly that reason.
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut pool = String::new();
        for i in 0..1000 {
            pool.push_str(&format!("function drainer{i}(a) {{\n"));
            for j in 0..30 {
                pool.push_str(&format!("  const s{j} = a + {j};\n"));
            }
            pool.push_str(&format!("  return widget + {i};\n}}\n"));
        }
        std::fs::write(src.join("pool.ts"), &pool).unwrap();
        // 24 short, name-matching Python functions: each is ~200 bytes of code, so
        // 24 of them clear the 2000-token compression threshold together.
        let mut py = String::new();
        for i in 0..24 {
            py.push_str(&format!("def widget_{i:02}():\n"));
            for j in 0..10 {
                py.push_str(&format!("    filler_{j} = {j}\n"));
            }
            py.push_str("    return widget\n\n");
        }
        std::fs::write(src.join("carrier.py"), py).unwrap();
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "language": "python", "top_k": 63, "skip_indexing": true
            }))
            .unwrap();
        assert!(
            out["mode"]
                .as_str()
                .is_some_and(|m| m.starts_with("compressed")),
            "fixture precondition: the answer must exceed the compression \
             threshold, or this is testing the plain envelope again; got mode={:?} \
             with {:?} results",
            out["mode"],
            out["results"].as_array().map(|a| a.len())
        );
        assert_eq!(
            out["pool_saturated"],
            json!(true),
            "a compressed answer is under-returned by the same mechanism and owes \
             the same disclosure; got {out}"
        );
    }

    /// Regression, pre-ship review round 3: the reported numbers came from two
    /// different pools and did not add up.
    ///
    /// Round 2 moved `pool_len` to the retry's pool while leaving the drop counts
    /// on the first pool's, so a 400-row pool reported 99 rows dropped by the
    /// filter — a caller could not tell where the other 300 went.
    ///
    /// On a saturated PLAIN answer the identity below holds, because every
    /// fetched row is either returned or removed by one of the two filters:
    ///
    ///     pool_len == results + dropped_by_filter + skipped_noise
    ///
    /// It is NOT exact on the compressed arms, where `results` can be one entry
    /// per file or per directory rather than per node, nor in the presence of an
    /// orphan row, which `build_candidates` drops into neither counter. Asserted
    /// on the plain arm only, and said here rather than left for a maintainer to
    /// discover (round 4).
    ///
    /// Asserting the identity rather than three literals is deliberate: it stays
    /// true if the fixture's rank order shifts, and fails for the right reason if
    /// any one number is ever sourced from a different fetch.
    #[test]
    fn every_disclosed_number_describes_the_same_pool() {
        let project = saturating_pool_project(420, &[("widget", 0)]);
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "language": "python", "top_k": 2, "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["pool_saturated"],
            json!(true),
            "precondition: this arm only claims a pool size when saturated; got {out}"
        );
        let results = out["results"].as_array().map(|a| a.len()).unwrap_or(0) as u64;
        let dropped = out["dropped_by_filter"].as_u64().unwrap_or(0);
        let noise = out["skipped_noise"].as_u64().unwrap_or(0);
        let pool: u64 = out["pool_saturated_note"]
            .as_str()
            .and_then(|n| n.split_once('(').and_then(|(_, r)| r.split_once(" rows")))
            .and_then(|(n, _)| n.parse().ok())
            .unwrap_or_else(|| panic!("the note must name the pool size; got {out}"));
        assert_eq!(
            results + dropped + noise,
            pool,
            "every fetched row is returned or dropped, so the disclosed numbers \
             must close over the pool they are reported beside: {results} + \
             {dropped} + {noise} != {pool}; got {out}"
        );
    }

    /// At the `top_k` ceiling, "raise top_k" is an instruction the caller cannot
    /// follow — `top_k: 200` is clamped to 100 and returns the same answer with
    /// the same advice (pre-ship review round 3). Exercised on the note directly
    /// rather than end to end: saturating at top_k=100 needs a 1600-row pool, and
    /// the branch under test is a pure function of the struct.
    #[test]
    fn at_the_top_k_ceiling_the_note_stops_advising_a_bigger_top_k() {
        let ceiling = count_range("semantic_code_search", "top_k")
            .map(|(_, hi)| hi as i64)
            .expect("semantic_code_search.top_k must have a COUNT_RANGES row");
        let at_ceiling = PoolShortfall {
            dropped_by_filter: 1599,
            skipped_noise: 0,
            pool_len: 1600,
            saturated: true,
            top_k: ceiling,
        };
        let note = at_ceiling.exhaustion_note();
        assert!(
            !note.contains("Raise top_k"),
            "top_k is already at its maximum; got {note:?}"
        );
        assert!(
            note.contains("Narrow the query"),
            "and the remedy that DOES remain must be named; got {note:?}"
        );

        let below = PoolShortfall {
            top_k: ceiling - 1,
            ..at_ceiling
        };
        assert!(
            below.exhaustion_note().contains("Raise top_k to widen it"),
            "one below the ceiling raising it still works, so the ordinary wording \
             must survive — otherwise this test would pass on a note that never \
             advises anything; got {:?}",
            below.exhaustion_note()
        );
    }

    /// The WIRING for the test above. Building `PoolShortfall` by hand leaves the
    /// field's provenance untested: replacing `top_k` with a literal `1` in the
    /// struct expression disabled the ceiling arm for every real caller and the
    /// entire suite stayed green (round 4). This drives the tool.
    ///
    /// `top_k=100` is the clamp ceiling, so `fetch_count` is 1600 and
    /// `search_retry_fetch_count` caps below it — no retry, and the pool is full
    /// only because the fixture has 1601 matching rows.
    #[test]
    fn at_the_ceiling_the_tool_itself_stops_advising_a_bigger_top_k() {
        let project = saturating_pool_project(1600, &[("widget", 0)]);
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "language": "python", "top_k": 100, "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["results"].as_array().map(|a| a.len()),
            Some(1),
            "fixture precondition: the Python row must survive INSIDE the 1600-row \
             cut. Without this the flag also holds via the empty-result arm, and \
             the short-answer ceiling wiring goes uncovered silently; got {out}"
        );
        assert_eq!(
            out["pool_saturated"],
            json!(true),
            "fixture precondition: 1601 matching rows against a 1600-row fetch must \
             leave the pool full and the answer short; got {out}"
        );
        let note = out["pool_saturated_note"].as_str().unwrap_or_default();
        assert!(
            !note.contains("Raise top_k"),
            "top_k=100 IS the maximum — `top_k: 200` is clamped back to it, so this \
             advice returns the same answer forever; got {out}"
        );
        assert!(
            note.contains("Narrow the query"),
            "and the remedy that still exists must be named; got {out}"
        );
    }

    /// Negative control: short is not truncated. One match in a four-row repo is
    /// a COMPLETE answer, and flagging it would train the caller to ignore the
    /// field. Only the pool-came-back-full leg separates the two cases, so this
    /// reddens if `saturated` is ever weakened to "results < top_k".
    #[test]
    fn a_short_but_complete_answer_carries_no_pool_disclosure() {
        let project = saturating_pool_project(3, &[("widget", 0)]);
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "language": "python", "top_k": 5, "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["results"].as_array().map(|a| a.len()),
            Some(1),
            "precondition: short (1 of top_k=5) WITH rows dropped by the filter, so \
             only pool fullness can decide; got {out}"
        );
        assert!(
            out.get("pool_saturated").is_none(),
            "a 4-row pool cannot have come back full at fetch 100 — this answer is \
             complete, not truncated; got {out}"
        );
    }

    /// The retry REPLACES the candidate set wholesale, so "did the pool come
    /// back full" must be asked of the pool that produced the answer. Here the
    /// FIRST pool is full (100 of 100) and the retry pool is not (121 of 400),
    /// and the surviving match came from the retry — so measuring `fused.len()`
    /// against `fetch_count` reports a truncation that did not happen. This is
    /// the accounting risk the audit named before the fix existed (§15).
    #[test]
    fn saturation_is_measured_against_the_pool_that_produced_the_answer() {
        let project = saturating_pool_project(120, &[("carrier_box", 80)]);
        let server = indexed_server(&project);
        let out = server
            .tool_semantic_search(&json!({
                "query": "widget", "language": "python", "top_k": 2, "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["results"].as_array().map(|a| a.len()),
            Some(1),
            "precondition: the match is reachable ONLY through the retry (120 \
             distractors fill the first pool of 100, not the retry pool of 400); \
             got {out}"
        );
        assert!(
            out.get("pool_saturated").is_none(),
            "the retry pool held 121 rows of a possible 400 — nothing was cut off. \
             Reading the FIRST pool here calls a complete answer truncated; got {out}"
        );
    }
}
