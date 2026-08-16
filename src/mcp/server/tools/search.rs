//! `semantic_code_search` — hybrid BM25 + vector search with RRF fusion.
//!
//! Confidence scoring (FTS sparsity / OR-fallback / source intersection),
//! acronym-heavy query detection, doc-penalty for markdown matches, and
//! token-aware compression sit here. Adjusted score combines RRF rank,
//! query quality, name match boost, and size dampening.

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_semantic_search(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Per-result code_content cap used both in estimation (below) and the
        // actual result payload so compression triggers reflect real output size.
        const MAX_SEARCH_CODE_LEN: usize = 500;
        let query = required_str(args, "query")?;
        let top_k = args["top_k"]
            .as_u64()
            .or_else(|| args["limit"].as_u64())
            .unwrap_or(20)
            .clamp(1, 100) as i64;
        let node_type_filter = args["node_type"].as_str();
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Validate node_type up-front: unknown aliases normalize to empty and
        // would silently filter every result away (see tool_ast_search parity).
        if let Some(nt) = node_type_filter {
            if crate::domain::normalize_type_filter(nt).is_empty() {
                return Err(anyhow!(
                    "Unknown node_type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                    nt
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
        if !should_skip_indexing(args) {
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
        let vector_available = model_guard.is_some() && self.db.vec_enabled();
        let query_embedding: Option<Vec<f32>> = match *model_guard {
            Some(ref model) if self.db.vec_enabled() => model.embed(query).ok(),
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
            let or_fallback = fts_result.or_fallback;
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
        let fused = fuse(&fts_search, &vec_search, fetch_count as usize);

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
        struct Candidate {
            node: queries::NodeResult,
            file_path: String,
            adjusted_score: f64,
        }
        let query_terms_lower: Vec<String> =
            meaningful_tokens.iter().map(|t| t.to_lowercase()).collect();
        // Verbatim identifier query (e.g. "run_serve") — used for exact-name rerank
        // dominance below and the confidence exemption further down (single source).
        let query_trimmed = query.trim().to_lowercase();

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
                    if let Some(nt) = node_type_filter {
                        let normalized = normalize_type_filter_mcp(nt);
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

        // Pool-exhaustion retry: when the first pool came back FULL and the
        // post-fetch filters still left top_k unfilled, matches may sit just
        // below the cut — widen once and re-rank. Confidence (measured above on
        // the first pass) is deliberately NOT recomputed: this widens recall, it
        // does not change how well the query matched. A pool that was not
        // exhausted, or that lost nothing to filtering, retrieves exactly as
        // before — the retrieval benchmark path is untouched.
        let retry_fetch = crate::domain::search_retry_fetch_count(fetch_count);
        if candidates.len() < top_k as usize
            && fused.len() >= fetch_count as usize
            && (skipped_noise_count + dropped_by_filter) > 0
            && retry_fetch > fetch_count
        {
            let (fts_retry, vec_retry, _, _) = retrieve(retry_fetch)?;
            let fused_retry = fuse(&fts_retry, &vec_retry, retry_fetch as usize);
            let (retry_candidates, retry_dropped, retry_skipped) = build_candidates(&fused_retry)?;
            if retry_candidates.len() > candidates.len() {
                candidates = retry_candidates;
                dropped_by_filter = retry_dropped;
                skipped_noise_count = retry_skipped;
            }
        }

        // Phase 2: Re-rank by adjusted score (name relevance + size normalization)
        candidates.sort_by(|a, b| b.adjusted_score.total_cmp(&a.adjusted_score));
        candidates.truncate(top_k as usize);

        // Phase 3: Build results
        let mut results = Vec::new();
        for c in &candidates {
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
                return Ok(out);
            }
        } // end estimated_tokens check

        if results.is_empty() {
            // Filter-aware: if a language/node_type filter removed candidates that DID
            // match the query, say so — the index has matches, just not of this
            // language/type. (vec0 can't pre-filter, so this is a post-fetch drop.)
            if filtered && dropped_by_filter > 0 {
                return Ok(json!({
                    "results": [],
                    "message": "No matching symbols after filtering.",
                    "dropped_by_filter": dropped_by_filter,
                    "hint": format!(
                        "{} candidate(s) matched the query but were removed by the active language/node_type filter. Broaden or clear the filter, or raise top_k.",
                        dropped_by_filter
                    )
                }));
            }
            // Same disclosure duty for the always-on filter: the query DID match,
            // and every match was a `<module>`/`<external>` placeholder or a test
            // symbol. Saying "check spelling / the index may need rebuilding"
            // there is a false diagnosis — the index is fine and the spelling
            // found rows (audit 2026-08-16 P1-7).
            if skipped_noise_count > 0 {
                return Ok(json!({
                    "results": [],
                    "message": format!(
                        "No matching symbols — {} candidate(s) matched the query but are module/external placeholders or test symbols, which this tool always excludes.",
                        skipped_noise_count
                    ),
                    "skipped_noise": skipped_noise_count,
                    "hint": "Spelling and index freshness are not the problem. To reach test symbols use `find_references` with include_tests, or `code-graph-mcp grep`; for structural enumeration use `ast_search`.",
                    "search_mode": if vector_available { "hybrid" } else { "fts_only" },
                    "vector_available": vector_available
                }));
            }
            // The text channel never ran (single characters, stop words), so
            // "check spelling / the index may need rebuilding" below would be a
            // false diagnosis of a search that did not happen (2026-08-16 audit
            // §四). With no vector channel either, nothing was searched at all.
            if let Some(reason) = fts_not_searched {
                return Ok(json!({
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
                }));
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
            return Ok(json!({
                "results": [],
                "message": "No matching symbols found.",
                "hint": hint,
                "search_mode": if vector_available { "hybrid" } else { "fts_only" },
                "vector_available": vector_available
            }));
        }

        // Shape the response: one object envelope on every path ({results, …}),
        // carrying the degradation (FTS-only) / no-text-anchor (vector-only)
        // signals when they apply. Mirrors the compressed path above AND keeps
        // the response writable by the server-level disclosures that run after
        // every tool call (see `finalize_search_results`).
        Ok(finalize_search_results(
            results,
            match_confidence,
            vector_only_no_anchor,
            has_exact_name_match,
            vector_available,
        ))
    }
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
        // "retry shortly" was printed unconditionally, so a machine whose
        // download can never succeed got a wait-and-see message forever
        // (issue #35). Name the actual last outcome when one was recorded.
        #[cfg(feature = "embed-model")]
        let last = crate::embedding::model::EmbeddingModel::download_state_summary();
        #[cfg(not(feature = "embed-model"))]
        let last: Option<String> = None;
        let note = match last {
            Some(s) => format!(
                "Embedding model not loaded — results are FTS5-only (reduced semantic recall). \
                 Last model download: {}. Run `code-graph-mcp doctor` for detail.",
                s
            ),
            None => {
                "Embedding model not loaded — results are FTS5-only (reduced semantic recall). \
                     The model auto-downloads in the background on first use; retry shortly, or \
                     run `code-graph-mcp doctor` to check status."
                    .to_string()
            }
        };
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
}
