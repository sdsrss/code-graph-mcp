//! Domain-acronym expansion for FTS queries.
//!
//! When a query token matches a known acronym (e.g. `RRF`, `BM25`, `FTS`), this
//! module returns the expanded full-form word list. The FTS preprocessing pipeline
//! keeps the original acronym *and* adds the expansion terms (deduped via BTreeSet),
//! so exact matches still win while recall broadens onto code/docs that spell out
//! the full form.
//!
//! Scope: CS / IR / DB terms common in this codebase and typical developer queries.
//! Keep entries that are (1) clearly unambiguous and (2) likely to appear in either
//! acronym or full form in real code — skip polysemous fringe acronyms.

/// Static dictionary. Keys are lowercase ASCII; expansion terms are lowercase
/// individual words (each ≥ 2 chars to survive the FTS length filter).
const ACRONYMS: &[(&str, &[&str])] = &[
    ("rrf",  &["reciprocal", "rank", "fusion"]),
    ("bm25", &["best", "match"]),
    ("fts",  &["full", "text", "search"]),
    ("fts5", &["full", "text", "search"]),
    ("ast",  &["abstract", "syntax", "tree"]),
    ("cst",  &["concrete", "syntax", "tree"]),
    ("lsp",  &["language", "server", "protocol"]),
    ("mcp",  &["model", "context", "protocol"]),
    ("rpc",  &["remote", "procedure", "call"]),
    ("sql",  &["structured", "query", "language"]),
    ("orm",  &["object", "relational", "mapping"]),
    ("cte",  &["common", "table", "expression"]),
    ("jwt",  &["json", "web", "token"]),
    ("ttl",  &["time", "live"]),
    ("dag",  &["directed", "acyclic", "graph"]),
    ("rbac", &["role", "based", "access", "control"]),
    ("crud", &["create", "read", "update", "delete"]),
    ("cors", &["cross", "origin", "resource", "sharing"]),
];

/// Return the expansion term list for `token`, or an empty slice if unknown.
/// Matching is ASCII case-insensitive.
pub fn expand_acronym(token: &str) -> &'static [&'static str] {
    for &(key, expansion) in ACRONYMS {
        if token.eq_ignore_ascii_case(key) {
            return expansion;
        }
    }
    &[]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_case_insensitive() {
        assert_eq!(expand_acronym("RRF"), ["reciprocal", "rank", "fusion"]);
        assert_eq!(expand_acronym("rrf"), ["reciprocal", "rank", "fusion"]);
        assert_eq!(expand_acronym("Rrf"), ["reciprocal", "rank", "fusion"]);
    }

    #[test]
    fn unknown_returns_empty() {
        assert!(expand_acronym("xyz").is_empty());
        assert!(expand_acronym("").is_empty());
        assert!(expand_acronym("fusion").is_empty(), "non-acronym common words skipped");
    }

    #[test]
    fn numeric_acronym_supported() {
        assert_eq!(expand_acronym("BM25"), ["best", "match"]);
        assert_eq!(expand_acronym("bm25"), ["best", "match"]);
    }

    #[test]
    fn all_expansion_terms_survive_fts_length_filter() {
        // FTS preprocessor drops tokens shorter than 2 chars. Guard against
        // accidentally adding entries that get silently filtered.
        for &(key, expansion) in ACRONYMS {
            for term in expansion {
                assert!(term.len() >= 2, "acronym '{}' has sub-2-char term '{}'", key, term);
            }
        }
    }
}
