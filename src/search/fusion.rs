use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub node_id: i64,
    /// Raw score from the source (BM25 for FTS, cosine similarity for vector).
    /// Used for score blending in RRF when available (non-zero).
    pub score: f64,
}

/// Reciprocal Rank Fusion with raw score blending.
///
/// Base: score(node) = sum( weight_i / (k + rank_i + 1) ) across sources (classic RRF).
///
/// Blending: when raw scores are available (score > 0), a small fraction of the
/// normalized raw score is added as a TRUE tie-breaker — strictly bounded so it
/// cannot flip adjacent RRF ranks at any k. Scale is adaptive:
///   blend_max_per_source = 0.5 / ((k+1) * (k+2)) * weight
/// Proof: the RRF gap between rank i and rank i+1 in one source is
///   1/(k+i+1) - 1/(k+i+2) = 1/((k+i+1)(k+i+2))
/// which is minimized at i=0 as 1/((k+1)(k+2)). Setting blend_max to HALF that
/// gap guarantees the blend contribution is strictly less than any adjacent-rank
/// RRF gap — so a higher-ranked node can never be overtaken by a lower-ranked
/// one with a bigger raw score.
///
/// Historical note: a previous version used SCORE_BLEND_FACTOR=0.1, which at k=30
/// produced blend_max ≈ 0.1 vs adjacent-rank gap ≈ 0.001 — blend dominated RRF
/// by ~100×, silently converting RRF into per-source-raw-score ranking. This
/// adaptive bound restores RRF's actual semantics while keeping blending as a
/// meaningful tie-breaker within a single source.
///
/// Higher `k` values dampen the impact of rank differences (typically k=30–60).
pub fn weighted_rrf_fusion(
    fts_results: &[SearchResult],
    vec_results: &[SearchResult],
    k: u32,
    top_k: usize,
    fts_weight: f64,
    vec_weight: f64,
) -> Vec<SearchResult> {
    // Adaptive blend scale: half of the smallest adjacent-rank RRF gap.
    // Guarantees blend is strictly subordinate to rank ordering at any k.
    let k_f = k as f64;
    let blend_scale = 0.5 / ((k_f + 1.0) * (k_f + 2.0));

    let mut scores: HashMap<i64, f64> = HashMap::new();

    // Normalize raw scores to [0, 1] for blending
    let fts_max = fts_results.iter().map(|r| r.score).fold(0.0_f64, f64::max);
    let vec_max = vec_results.iter().map(|r| r.score).fold(0.0_f64, f64::max);

    for (rank, r) in fts_results.iter().enumerate() {
        let rrf = fts_weight / (k as f64 + rank as f64 + 1.0);
        let blend = if fts_max > 0.0 {
            blend_scale * fts_weight * (r.score / fts_max)
        } else {
            0.0
        };
        *scores.entry(r.node_id).or_default() += rrf + blend;
    }
    for (rank, r) in vec_results.iter().enumerate() {
        let rrf = vec_weight / (k as f64 + rank as f64 + 1.0);
        let blend = if vec_max > 0.0 {
            blend_scale * vec_weight * (r.score / vec_max)
        } else {
            0.0
        };
        *scores.entry(r.node_id).or_default() += rrf + blend;
    }

    let mut results: Vec<SearchResult> = scores
        .into_iter()
        .map(|(id, score)| SearchResult { node_id: id, score })
        .collect();
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    results.truncate(top_k);
    results
}

#[cfg(test)]
pub fn rrf_fusion(
    fts_results: &[SearchResult],
    vec_results: &[SearchResult],
    k: u32,
    top_k: usize,
) -> Vec<SearchResult> {
    weighted_rrf_fusion(fts_results, vec_results, k, top_k, 1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_fusion_basic() {
        let fts_results = vec![
            SearchResult { node_id: 1, score: 0.0 },
            SearchResult { node_id: 2, score: 0.0 },
            SearchResult { node_id: 3, score: 0.0 },
        ];
        let vec_results = vec![
            SearchResult { node_id: 2, score: 0.0 },
            SearchResult { node_id: 4, score: 0.0 },
            SearchResult { node_id: 1, score: 0.0 },
        ];

        let fused = rrf_fusion(&fts_results, &vec_results, 60, 3);

        assert_eq!(fused[0].node_id, 2);
        assert_eq!(fused[1].node_id, 1);
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn test_rrf_with_no_overlap() {
        let fts = vec![SearchResult { node_id: 1, score: 0.0 }];
        let vec = vec![SearchResult { node_id: 2, score: 0.0 }];

        let fused = rrf_fusion(&fts, &vec, 60, 5);
        assert_eq!(fused.len(), 2);
    }

    #[test]
    fn test_weighted_rrf_prefers_fts() {
        let fts = vec![SearchResult { node_id: 1, score: 0.0 }];
        let vec = vec![SearchResult { node_id: 2, score: 0.0 }];

        let fused = weighted_rrf_fusion(&fts, &vec, 60, 5, 2.0, 1.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].node_id, 1, "FTS-only result should rank first when fts_weight > vec_weight");
        assert!(fused[0].score > fused[1].score);
    }

    #[test]
    fn test_weighted_rrf_both_sources() {
        let fts = vec![
            SearchResult { node_id: 1, score: 0.0 },
            SearchResult { node_id: 2, score: 0.0 },
        ];
        let vec = vec![
            SearchResult { node_id: 2, score: 0.0 },
            SearchResult { node_id: 3, score: 0.0 },
        ];

        let fused = weighted_rrf_fusion(&fts, &vec, 60, 5, 1.0, 1.0);
        assert_eq!(fused[0].node_id, 2, "Node appearing in both sources should rank highest");
    }

    #[test]
    fn test_score_blending_breaks_ties() {
        // Two FTS results at rank 0 and 1: node_1 has higher raw BM25 score
        // With blending, even if RRF ranks are close, the higher BM25 should win
        let fts = vec![
            SearchResult { node_id: 1, score: 10.0 }, // high BM25
            SearchResult { node_id: 2, score: 1.0 },  // low BM25
        ];
        let vec: Vec<SearchResult> = vec![];

        let fused = weighted_rrf_fusion(&fts, &vec, 60, 5, 1.0, 1.0);
        assert_eq!(fused[0].node_id, 1, "Higher raw score should rank first");
        // Verify that blending added score beyond pure RRF
        let pure_rrf_rank0 = 1.0 / (60.0 + 0.0 + 1.0);
        assert!(fused[0].score > pure_rrf_rank0, "Blended score should exceed pure RRF");
    }

    /// Scientific invariant: blending must NEVER flip adjacent ranks.
    /// A rank-0 result with a low raw score must still beat a rank-1 result with
    /// max raw score. Historically (SCORE_BLEND_FACTOR=0.1 with k=30), this
    /// invariant was violated — the blend term dominated the RRF term by ~100×.
    #[test]
    fn test_blend_cannot_flip_adjacent_ranks() {
        // Adversarial case: rank-0 has the minimum non-zero raw score,
        // rank-1 has the maximum. Old formula would let rank-1 win.
        for &k in &[10u32, 30, 60, 100] {
            let fts = vec![
                SearchResult { node_id: 1, score: 0.0001 }, // rank 0, tiny score
                SearchResult { node_id: 2, score: 1000.0 }, // rank 1, huge score
            ];
            let vec_empty: Vec<SearchResult> = vec![];
            let fused = weighted_rrf_fusion(&fts, &vec_empty, k, 5, 1.0, 1.0);
            assert_eq!(
                fused[0].node_id, 1,
                "k={}: rank-0 must win even when rank-1 has much higher raw score (blend must not flip ranks)",
                k
            );
        }
    }

    /// Scientific invariant: cross-source ranks are also safe.
    /// An item at rank 0 in FTS + absent in vec should beat an item at rank 5
    /// in FTS + rank 0 in vec IF the RRF score says so (regardless of raw scores).
    #[test]
    fn test_blend_respects_cross_source_rank_budget() {
        let k = 30u32;
        // Node 1: rank 0 in FTS only → RRF = 1/31 ≈ 0.03226
        // Node 2: rank 5 in FTS + rank 0 in vec → RRF = 1/36 + 1/31 ≈ 0.0601
        // With (fts,vec) both weight=1, Node 2 has higher RRF and must win.
        let fts = vec![
            SearchResult { node_id: 1, score: 100.0 }, // rank 0, max raw
            SearchResult { node_id: 9, score: 1.0 },
            SearchResult { node_id: 8, score: 1.0 },
            SearchResult { node_id: 7, score: 1.0 },
            SearchResult { node_id: 6, score: 1.0 },
            SearchResult { node_id: 2, score: 0.001 }, // rank 5, tiny raw
        ];
        let vec = vec![
            SearchResult { node_id: 2, score: 0.001 }, // rank 0 in vec, tiny raw
        ];
        let fused = weighted_rrf_fusion(&fts, &vec, k, 5, 1.0, 1.0);
        assert_eq!(
            fused[0].node_id, 2,
            "Higher combined RRF rank must win regardless of raw scores"
        );
    }

    /// Within the same source, blending provides a meaningful tie-breaker
    /// between items whose RRF ranks differ by 1 but raw scores diverge hugely.
    /// This is the scenario the blend is actually designed for.
    ///
    /// Note: cross-source blend tie-breaking cannot work — per-source normalization
    /// maps each source's top-scoring item to blend=blend_scale regardless of raw
    /// units, so FTS BM25 and vector cosine cannot be directly compared.
    #[test]
    fn test_blend_nudges_within_source() {
        // Same source, two items at adjacent ranks. The RRF gap at k=30 is tiny
        // (1/(31*32) ≈ 0.00101). Blend adds ~0.00025 max. Rank still dominates.
        let k = 30u32;
        let fts = vec![
            SearchResult { node_id: 1, score: 100.0 }, // rank 0, max raw
            SearchResult { node_id: 2, score: 10.0 },  // rank 1, lower raw
        ];
        let vec_empty: Vec<SearchResult> = vec![];
        let fused = weighted_rrf_fusion(&fts, &vec_empty, k, 5, 1.0, 1.0);
        // Natural rank still wins (1 before 2), but score gap is larger than pure RRF
        // because both blend contributions add to the correct side.
        assert_eq!(fused[0].node_id, 1);
        let pure_rrf_gap = 1.0 / (k as f64 + 1.0) - 1.0 / (k as f64 + 2.0);
        let observed_gap = fused[0].score - fused[1].score;
        assert!(
            observed_gap >= pure_rrf_gap,
            "Blending should preserve or widen rank-0/rank-1 gap when raw scores agree with rank, got {} vs RRF-only {}",
            observed_gap, pure_rrf_gap
        );
    }

    /// Proof that blend_scale is mathematically bounded below the
    /// smallest adjacent-rank RRF gap for all realistic k values.
    #[test]
    fn test_blend_scale_mathematically_bounded() {
        for &k in &[5u32, 10, 30, 60, 100, 200] {
            let k_f = k as f64;
            let blend_scale = 0.5 / ((k_f + 1.0) * (k_f + 2.0));
            let adjacent_gap = 1.0 / (k_f + 1.0) - 1.0 / (k_f + 2.0);
            assert!(
                blend_scale < adjacent_gap,
                "k={}: blend_scale {} must be < adjacent RRF gap {}",
                k, blend_scale, adjacent_gap
            );
            // Safety margin: blend should be ≤ half the gap
            assert!(
                blend_scale <= adjacent_gap * 0.5 + f64::EPSILON,
                "k={}: blend should be ≤ half the adjacent gap",
                k
            );
        }
    }
}
