use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub node_id: i64,
    pub score: f64,
}

pub fn weighted_rrf_fusion(
    fts_results: &[SearchResult],
    vec_results: &[SearchResult],
    k: u32,
    top_k: usize,
    fts_weight: f64,
    vec_weight: f64,
) -> Vec<SearchResult> {
    let mut scores: HashMap<i64, f64> = HashMap::new();

    for (rank, r) in fts_results.iter().enumerate() {
        *scores.entry(r.node_id).or_default() += fts_weight / (k as f64 + rank as f64 + 1.0);
    }
    for (rank, r) in vec_results.iter().enumerate() {
        *scores.entry(r.node_id).or_default() += vec_weight / (k as f64 + rank as f64 + 1.0);
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
        // Node 1 is FTS-only (rank 0), Node 2 is Vec-only (rank 0)
        // With fts_weight=2.0 > vec_weight=1.0, FTS-only result should rank higher
        let fts = vec![SearchResult { node_id: 1, score: 0.0 }];
        let vec = vec![SearchResult { node_id: 2, score: 0.0 }];

        let fused = weighted_rrf_fusion(&fts, &vec, 60, 5, 2.0, 1.0);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].node_id, 1, "FTS-only result should rank first when fts_weight > vec_weight");
        assert!(fused[0].score > fused[1].score);
    }

    #[test]
    fn test_weighted_rrf_both_sources() {
        // Node 2 appears in both FTS and Vec; Nodes 1 and 3 appear in only one
        // With equal weights, the node in both sources should rank highest
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
}
