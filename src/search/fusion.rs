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
/// Blending: when raw scores are available (score > 0), a small fraction (SCORE_BLEND_FACTOR)
/// of the normalized raw score is added to break ties between same-ranked results.
///
/// Higher `k` values dampen the impact of rank differences (typically k=60).
pub fn weighted_rrf_fusion(
    fts_results: &[SearchResult],
    vec_results: &[SearchResult],
    k: u32,
    top_k: usize,
    fts_weight: f64,
    vec_weight: f64,
) -> Vec<SearchResult> {
    // Fraction of normalized raw score blended into RRF score.
    // Small enough to not override rank ordering, but enough to break ties.
    const SCORE_BLEND_FACTOR: f64 = 0.1;

    let mut scores: HashMap<i64, f64> = HashMap::new();

    // Normalize raw scores to [0, 1] for blending
    let fts_max = fts_results.iter().map(|r| r.score).fold(0.0_f64, f64::max);
    let vec_max = vec_results.iter().map(|r| r.score).fold(0.0_f64, f64::max);

    for (rank, r) in fts_results.iter().enumerate() {
        let rrf = fts_weight / (k as f64 + rank as f64 + 1.0);
        let blend = if fts_max > 0.0 {
            SCORE_BLEND_FACTOR * fts_weight * (r.score / fts_max)
        } else {
            0.0
        };
        *scores.entry(r.node_id).or_default() += rrf + blend;
    }
    for (rank, r) in vec_results.iter().enumerate() {
        let rrf = vec_weight / (k as f64 + rank as f64 + 1.0);
        let blend = if vec_max > 0.0 {
            SCORE_BLEND_FACTOR * vec_weight * (r.score / vec_max)
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
}
