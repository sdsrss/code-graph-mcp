use anyhow::Result;
use rusqlite::Connection;

use super::helpers::escape_like;
use super::nodes::{map_node_row, NodeResult, NODE_SELECT_ALIASED};

/// Stopwords filtered from FTS5 queries to reduce noise.
const FTS_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "the", "or", "in", "of", "for", "to", "with", "is", "it", "this", "that",
    "by", "from", "on", "at", "as", "be", "are", "was", "were", "been", "all", "each", "how",
    "what", "when",
];

/// FTS5 search result with quality metadata.
pub struct FtsResult {
    pub nodes: Vec<NodeResult>,
    /// Raw BM25 scores (negated so higher = better match), parallel to `nodes`.
    pub bm25_scores: Vec<f64>,
    /// True if AND mode failed and OR fallback was used (weaker match).
    pub or_fallback: bool,
}

pub fn fts5_search(conn: &Connection, query: &str, limit: i64) -> Result<FtsResult> {
    fts5_search_impl(conn, query, limit, true)
}

/// FTS5 search including test symbols (for test-aware callers).
#[cfg(test)]
pub fn fts5_search_with_tests(conn: &Connection, query: &str, limit: i64) -> Result<FtsResult> {
    fts5_search_impl(conn, query, limit, false)
}

fn fts5_search_impl(
    conn: &Connection,
    query: &str,
    limit: i64,
    exclude_tests: bool,
) -> Result<FtsResult> {
    // Preprocess query: split on term boundaries, filter stopwords, split
    // identifiers (camelCase/snake_case), expand domain acronyms (RRF →
    // reciprocal rank fusion, etc.), then sanitize for FTS5. Porter stemming is
    // handled by the FTS5 tokenizer.
    //
    // Term boundary = anything outside [alphanumeric _]: whitespace AND
    // punctuation. Punctuation used to be DELETED from each whitespace-word
    // instead, which glued `db.execute` into the token `dbexecute` — a string
    // that exists in no index, so every qualified-name query (`db.execute`,
    // `domain::search_fetch_count`, `name:fts5_search`, `path/to/file`) was a
    // hard zero, and the empty response blamed the user's spelling (audit
    // 2026-08-16 P1-6). `_` stays a term character so snake_case identifiers
    // survive as one token.
    let is_term_char = |c: char| c.is_alphanumeric() || c == '_';
    let raw_terms: Vec<&str> = query
        .split(|c: char| !is_term_char(c))
        .filter(|w| !w.is_empty())
        .collect();
    let terms: Vec<String> = raw_terms
        .iter()
        .copied()
        .filter(|w| !FTS_STOP_WORDS.contains(&w.to_lowercase().as_str()))
        .flat_map(|word| {
            // Split camelCase/snake_case identifiers into constituent words
            let split = crate::utils::tokenizer::split_identifier(word);
            let mut out: Vec<String> = split.split_whitespace().map(String::from).collect();
            // Acronym expansion: append full-form terms alongside the original token.
            // BTreeSet below handles dedup if original already expanded form.
            for token in split.split_whitespace() {
                for exp in crate::utils::acronyms::expand_acronym(token) {
                    out.push((*exp).to_string());
                }
            }
            out
        })
        .collect::<std::collections::BTreeSet<_>>() // deduplicate (sorted for deterministic queries)
        .into_iter()
        .map(|word| {
            // Defense in depth: the split above already dropped every FTS5
            // metacharacter (* ^ : + - ~ ( ) { } " alter FTS5 semantics), but
            // identifier splitting and acronym expansion run in between, so
            // re-assert the invariant the quoting at the MATCH site depends on:
            // a term contains only [alphanumeric _].
            let sanitized: String = word.chars().filter(|c| is_term_char(*c)).collect();
            sanitized
        })
        .filter(|w| w.len() >= 2)
        .collect();
    // Empty/whitespace-only queries would cause FTS5 MATCH error
    if terms.is_empty() {
        return Ok(FtsResult {
            nodes: vec![],
            bm25_scores: vec![],
            or_fallback: false,
        });
    }

    let test_filter = if exclude_tests {
        " AND n.is_test = 0"
    } else {
        ""
    };
    // Include BM25 score in SELECT for raw score blending in RRF fusion
    let bm25_expr = "bm25(nodes_fts, 5.0, 3.0, 2.0, 2.0, 1.0, 5.0, 1.0, 1.0)";
    let sql = format!(
        "SELECT {}, {} FROM nodes_fts fts JOIN nodes n ON n.id = fts.rowid WHERE nodes_fts MATCH ?1{}
         ORDER BY {} LIMIT ?2",
        NODE_SELECT_ALIASED, bm25_expr, test_filter, bm25_expr
    );

    // Row mapper: map_node_row for columns 0..14 (including is_test), BM25 score at column 15
    let map_row_with_bm25 = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(NodeResult, f64)> {
        let node = map_node_row(row)?;
        // BM25 returns negative values (more negative = better); negate for positive scores
        let bm25: f64 = row.get(15)?;
        Ok((node, -bm25))
    };

    // Wrap each sanitized term in double quotes so a bare token like "NOT"
    // (FTS5 keyword) parses as a phrase query for that token instead of as the
    // unary NOT operator. After sanitization tokens contain only [A-Za-z0-9_]
    // so `"<token>"` is always a well-formed FTS5 phrase. Same protection for
    // AND, OR, NEAR — covers user queries that happen to contain reserved words.
    let quoted: Vec<String> = terms.iter().map(|t| format!("\"{}\"", t)).collect();

    // Strategy: AND-first for multi-term queries (higher precision), fallback to OR
    if terms.len() > 1 {
        let and_query = quoted.join(" AND ");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![and_query, limit], map_row_with_bm25)?;
        let pairs: Vec<(NodeResult, f64)> = rows.collect::<Result<Vec<_>, _>>()?;
        if pairs.len() >= std::cmp::max(3, limit as usize / 10) {
            let (nodes, bm25_scores): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
            return Ok(FtsResult {
                nodes,
                bm25_scores,
                or_fallback: false,
            });
        }

        // Garbage-query guard: when the user typed a single word, AND found
        // nothing, AND that word doesn't appear as a token anywhere in the
        // index, OR-fallback would just match camelCase fragments — Rust's
        // `match` keyword, `--no-default-features`, etc. — turning a typo or
        // bogus identifier into noise. Acronym queries like "RRF" still get
        // OR-fallback because RRF *is* in the index (so OR widens a known-good
        // search). Multi-word queries always get OR-fallback (user explicitly
        // listed terms; widening is the documented recall behavior).
        if pairs.is_empty() {
            // "One word" is counted on the USER's basis — whitespace tokens —
            // not on the punctuation-split term basis: the user typed
            // `--no-default-features` or `name:run_migration` as ONE token,
            // and OR-widening over the fragments OUR splitter produced is
            // noise by construction, exactly the flood this guard exists to
            // stop (batch review of audit 2026-08-16 P1-6). Instead of the OR
            // pass, a single-token multi-fragment query gets one RELAXED AND
            // retry: fragments that exist nowhere in the index (`name` in a
            // fixture without it, the `no` of a flag) are dropped and the AND
            // re-runs over the survivors — still a co-occurrence query, so
            // `name:run_migration` finds run_migration without `--flag`-shaped
            // queries flooding unrelated hits. If nothing can be dropped or
            // the relaxed AND still finds nothing, the answer is an honest
            // empty. Multi-token queries keep the documented OR-fallback (the
            // user listed the terms themselves).
            let user_words = query
                .split_whitespace()
                .filter(|w| {
                    let s: String = w.chars().filter(|c| is_term_char(*c)).collect();
                    !s.is_empty() && !FTS_STOP_WORDS.contains(&s.to_lowercase().as_str())
                })
                .count();
            let original_terms: Vec<&&str> = raw_terms
                .iter()
                .filter(|w| !FTS_STOP_WORDS.contains(&w.to_lowercase().as_str()))
                .collect();
            if user_words <= 1 && original_terms.len() > 1 {
                let probe_sql = format!(
                    "SELECT 1 FROM nodes_fts fts JOIN nodes n ON n.id = fts.rowid \
                     WHERE nodes_fts MATCH ?1{} LIMIT 1",
                    test_filter
                );
                let mut probe = conn.prepare(&probe_sql)?;
                let surviving: Vec<&String> = terms
                    .iter()
                    .filter(|t| {
                        probe
                            .exists(rusqlite::params![format!("\"{}\"", t)])
                            .unwrap_or(false)
                    })
                    .collect();
                if !surviving.is_empty() && surviving.len() < terms.len() {
                    let relaxed_query = surviving
                        .iter()
                        .map(|t| format!("\"{}\"", t))
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    let mut stmt = conn.prepare(&sql)?;
                    let rows =
                        stmt.query_map(rusqlite::params![relaxed_query, limit], map_row_with_bm25)?;
                    let relaxed: Vec<(NodeResult, f64)> = rows.collect::<Result<Vec<_>, _>>()?;
                    if !relaxed.is_empty() {
                        let (nodes, bm25_scores): (Vec<_>, Vec<_>) = relaxed.into_iter().unzip();
                        return Ok(FtsResult {
                            nodes,
                            bm25_scores,
                            // Reported as a widened match, because it IS one: part
                            // of what the user typed was dropped to get here. That
                            // applies CONF_OR_FALLBACK_PENALTY and prints the "AND
                            // match insufficient" note, so a `db.migratoin` typo
                            // reads as broad-and-uncertain instead of as a precise
                            // hit (pre-tag review). A query whose fragments all
                            // co-occur never reaches this branch — it returns from
                            // the AND above with the penalty correctly absent.
                            or_fallback: true,
                        });
                    }
                }
                return Ok(FtsResult {
                    nodes: vec![],
                    bm25_scores: vec![],
                    or_fallback: false,
                });
            }
            if user_words <= 1 && original_terms.len() == 1 {
                let sanitized_original: String = original_terms[0]
                    .chars()
                    .filter(|c| is_term_char(*c))
                    .collect();
                if sanitized_original.len() >= 2 {
                    let probe_sql = format!(
                        "SELECT 1 FROM nodes_fts fts JOIN nodes n ON n.id = fts.rowid \
                         WHERE nodes_fts MATCH ?1{} LIMIT 1",
                        test_filter
                    );
                    let mut probe = conn.prepare(&probe_sql)?;
                    let probe_query = format!("\"{}\"", sanitized_original);
                    let exists: bool = probe.exists(rusqlite::params![probe_query])?;
                    if !exists {
                        return Ok(FtsResult {
                            nodes: vec![],
                            bm25_scores: vec![],
                            or_fallback: false,
                        });
                    }
                }
            }
        }
        // Fallback: OR gives broader recall
    }

    let or_query = quoted.join(" OR ");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![or_query, limit], map_row_with_bm25)?;
    let pairs: Vec<(NodeResult, f64)> = rows.collect::<Result<Vec<_>, _>>()?;
    let (nodes, bm25_scores): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    Ok(FtsResult {
        nodes,
        bm25_scores,
        or_fallback: terms.len() > 1,
    })
}

// --- Fuzzy name resolution ---

/// Candidate result from fuzzy function name matching.
#[derive(Debug, Clone)]
pub struct NameCandidate {
    pub name: String,
    pub file_path: String,
    pub node_type: String,
    pub node_id: i64,
    pub start_line: i64,
}

/// Find symbol names that match the given input.
/// Uses substring matching first, then falls back to edit-distance matching.
/// Matches all node types except modules.
/// Candidate pool for the phase-2 edit-distance fallback.
///
/// The `LIMIT` is a cost bound, not a filter: on a repo with more than 5000
/// eligible nodes it decides WHICH names get a typo-correction chance. Without an
/// ORDER BY that choice is whatever the query planner happens to emit — stable in
/// practice today (rowid order), not stable across an index addition, a schema
/// change or a SQLite upgrade, and untraceable when it shifts because the symptom
/// is only "that suggestion used to appear". `ORDER BY n.id` pins it to insertion
/// order, which is at least a rule that can be stated and reproduced.
///
/// Exclusions match phase 1 exactly — a candidate the LIKE pass refuses to return
/// must not reappear through the typo fallback.
const FUZZY_EDIT_DISTANCE_SQL: &str = "SELECT DISTINCT n.name, f.path, n.type, n.id, n.start_line
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE n.type != 'module'
           AND f.path <> '<external>'
         ORDER BY n.id
         LIMIT 5000";

pub fn find_functions_by_fuzzy_name(
    conn: &Connection,
    partial_name: &str,
) -> Result<Vec<NameCandidate>> {
    // Phase 1: LIKE-based substring + token matching (fast path)
    let escaped = escape_like(partial_name);
    let pattern = format!("%{}%", escaped);

    let tokens_only = crate::utils::tokenizer::split_identifier_tokens(partial_name);
    let token_escaped = escape_like(&tokens_only);
    let token_pattern = format!("%{}%", token_escaped);

    let sql = "SELECT DISTINCT n.name, f.path, n.type, n.id, n.start_line
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.name_tokens LIKE ?3 ESCAPE '\\')
           AND n.type != 'module'
           -- `<external>` holds sentinel nodes for imports binding outside
           -- the project. `n.type != 'module'` already drops the import
           -- sentinels, but IMPLEMENTS sentinels are typed `trait` and slip
           -- through — so a `use std::fmt::Debug; impl Debug for S {}` made
           -- `Debug` a fuzzy candidate the caller cannot open or select.
           AND f.path <> '<external>'
         ORDER BY
           CASE WHEN n.name = ?2 THEN 0
                WHEN n.name LIKE ?4 || '%' ESCAPE '\\' THEN 1
                ELSE 2
           END,
           LENGTH(n.name)
         LIMIT 10";
    let mut stmt = conn.prepare(sql)?;
    // ?2 is the raw name for the exact-equality bucket (`=` treats %/_ literally);
    // ?4 is the %/_-escaped form for the prefix-LIKE bucket, so a query containing
    // a wildcard char cannot mis-bucket names via the ordering LIKE (matches the
    // WHERE clause, which already escapes). Ordering-only fix; result set unchanged.
    let rows = stmt.query_map(
        rusqlite::params![pattern, partial_name, token_pattern, escaped],
        |row| {
            Ok(NameCandidate {
                name: row.get(0)?,
                file_path: row.get(1)?,
                node_type: row.get(2)?,
                node_id: row.get(3)?,
                start_line: row.get(4)?,
            })
        },
    )?;
    let results: Vec<NameCandidate> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if !results.is_empty() {
        return Ok(results);
    }

    // Phase 2: Edit-distance fallback for typos (e.g., "handle_mesage" → "handle_message")
    let query_lower = partial_name.to_lowercase();
    let max_dist = match query_lower.len() {
        0..=3 => 1,
        4..=7 => 2,
        _ => 3,
    };

    let mut stmt2 = conn.prepare(FUZZY_EDIT_DISTANCE_SQL)?;
    let rows2 = stmt2.query_map([], |row| {
        Ok(NameCandidate {
            name: row.get(0)?,
            file_path: row.get(1)?,
            node_type: row.get(2)?,
            node_id: row.get(3)?,
            start_line: row.get(4)?,
        })
    })?;

    let mut scored: Vec<(usize, NameCandidate)> = Vec::new();
    for row in rows2 {
        let candidate = row?;
        let dist = levenshtein(&query_lower, &candidate.name.to_lowercase());
        if dist <= max_dist {
            scored.push((dist, candidate));
        }
    }
    scored.sort_by_key(|(dist, c)| (*dist, c.name.len()));
    scored.truncate(10);
    Ok(scored.into_iter().map(|(_, c)| c).collect())
}

/// Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (m, n) = (a_chars.len(), b_chars.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    // Single-row optimization: O(min(m,n)) space
    let mut prev: Vec<usize> = (0..=n).collect();

    for i in 1..=m {
        let mut curr = vec![0usize; n + 1];
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = curr;
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};
    use super::*;

    #[test]
    fn test_fts5_search() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "validateToken".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "function validateToken(token) { jwt.verify(token); }".into(),
                signature: None,
                doc_comment: None,
                context_string: Some("validates JWT authentication token".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let results = fts5_search(db.conn(), "authentication token", 5)
            .unwrap()
            .nodes;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "validateToken");
    }

    /// A `LIMIT` that decides WHICH rows survive needs an ORDER BY above it.
    ///
    /// Behavioural coverage would need a >5000-node fixture to make the cut
    /// bite, so this asserts the one-directional invariant on the statement
    /// itself: the candidate pool is bounded, and the bound is ordered. Deleting
    /// the ORDER BY (the exact regression) turns it red; rewording the query
    /// around it does not.
    #[test]
    fn test_fuzzy_fallback_pool_is_deterministically_bounded() {
        let sql = FUZZY_EDIT_DISTANCE_SQL;
        let order_at = sql.find("ORDER BY").expect(
            "edit-distance pool must be ordered — an unordered LIMIT picks an arbitrary subset",
        );
        let limit_at = sql.find("LIMIT").expect("pool must stay bounded");
        assert!(
            order_at < limit_at,
            "ORDER BY must precede LIMIT to constrain which rows the cut keeps"
        );
    }

    // L8: the ORDER BY prefix-LIKE bucket must escape %/_ so a wildcard in the query
    // cannot promote a name that only coincidentally matches under wildcard semantics
    // above a name that is a genuine literal prefix. Ordering-only; the result set is
    // identical either way (both names contain the literal query substring "a_c").
    #[test]
    fn test_fuzzy_name_order_by_escapes_wildcards() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        // "abca_c": NOT a literal "a_c" prefix, but the raw (unescaped) ORDER BY LIKE
        // 'a_c%' matches it because `_` acts as a wildcard over the leading "abc".
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "abca_c".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 2,
                code_content: "x".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // "a_cLongerName": a genuine literal "a_c" prefix, deliberately longer so that
        // if both land in the same bucket the length tiebreak puts it LAST.
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "a_cLongerName".into(),
                qualified_name: None,
                start_line: 3,
                end_line: 4,
                code_content: "x".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let results = find_functions_by_fuzzy_name(db.conn(), "a_c").unwrap();
        let names: Vec<&str> = results.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"abca_c") && names.contains(&"a_cLongerName"),
            "both literal-substring matches must be returned, got {names:?}"
        );
        // Escaped: the genuine literal prefix ranks first (bucket 1 vs bucket 2).
        // Pre-fix (raw ?2): both land in bucket 1, length tiebreak surfaces "abca_c".
        assert_eq!(
            results[0].name, "a_cLongerName",
            "genuine literal prefix must outrank a wildcard-coincidental match, got {names:?}"
        );
    }

    /// Insert a production function node with the given name/code into `fid`.
    fn insert_fn(conn: &rusqlite::Connection, fid: i64, name: &str, line: i64, code: &str) {
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: name.into(),
                qualified_name: None,
                start_line: line,
                end_line: line + 4,
                code_content: code.into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: Some(crate::utils::tokenizer::split_identifier(name)),
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
    }

    fn qualified_name_fixture() -> (crate::storage::db::Database, tempfile::TempDir) {
        let (db, tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "runner.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        insert_fn(
            db.conn(),
            fid,
            "run_migration",
            1,
            "fn run_migration(db: &Db) { db.execute(\"PRAGMA foreign_keys=ON\"); }",
        );
        insert_fn(
            db.conn(),
            fid,
            "search_fetch_count",
            20,
            "pub fn search_fetch_count(top_k: i64) -> i64 { top_k * 4 }",
        );
        insert_fn(
            db.conn(),
            fid,
            "widen_pool",
            40,
            "fn widen_pool() { let n = domain::search_fetch_count(top_k); }",
        );
        // A decoy so a plain OR over the split tokens cannot be mistaken for a
        // precise hit: it contains "execute" but never "db".
        insert_fn(
            db.conn(),
            fid,
            "execute_plan",
            60,
            "fn execute_plan() { plan.execute(); }",
        );
        (db, tmp)
    }

    /// Punctuation between word runs must SPLIT terms, not be deleted.
    ///
    /// Pre-fix the sanitizer kept only `[alnum_]` per whitespace-word, so
    /// `db.execute` collapsed to the token `dbexecute`, which exists nowhere in
    /// any index — a hard zero on the single most natural way to search for a
    /// method call. Same for `::` and `:` separated queries (audit 2026-08-16
    /// P1-6).
    #[test]
    fn test_fts5_qualified_name_query_splits_on_punctuation() {
        let (db, _tmp) = qualified_name_fixture();

        for query in [
            "db.execute",
            "domain::search_fetch_count",
            "name:run_migration",
        ] {
            let hits = fts5_search(db.conn(), query, 10).unwrap().nodes;
            assert!(
                !hits.is_empty(),
                "query {query:?} must not be a hard zero — punctuation is a term separator, not a character to delete"
            );
        }

        // The concrete symbols each query names must actually come back.
        let db_execute: Vec<String> = fts5_search(db.conn(), "db.execute", 10)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.name.clone())
            .collect();
        assert!(
            db_execute.contains(&"run_migration".to_string()),
            "db.execute must find the function whose body calls db.execute(), got {db_execute:?}"
        );

        let qualified: Vec<String> = fts5_search(db.conn(), "domain::search_fetch_count", 10)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.name.clone())
            .collect();
        assert!(
            qualified.contains(&"search_fetch_count".to_string()),
            "a `mod::symbol` query must find `symbol`, got {qualified:?}"
        );
    }

    /// The sanitizer's security property survives the split: after term
    /// extraction no token may carry an FTS5 operator/quote, so the `"token"`
    /// quoting at the MATCH site stays well-formed. Every hostile input must
    /// return Ok — an Err here means a crafted query reached the FTS5 parser.
    #[test]
    fn test_fts5_hostile_queries_stay_well_formed() {
        let (db, _tmp) = qualified_name_fixture();
        let hostile = [
            "\" OR nodes_fts MATCH \"a",
            "a\" AND \"b",
            "NEAR(a b, 2)",
            "a* OR b*",
            "^start",
            "col:value",
            "a AND NOT b",
            "(a OR b) AND c",
            "{a b}",
            "a-b-c",
            "\"\"\"\"",
            "'; DROP TABLE nodes; --",
            "a+b~c",
            "run_migration\" OR \"1",
        ];
        for q in hostile {
            let out = fts5_search(db.conn(), q, 10);
            assert!(
                out.is_ok(),
                "hostile query {q:?} must not reach the FTS5 parser as syntax: {:?}",
                out.err()
            );
        }
        // The injection attempt must not widen the result set beyond what the
        // literal tokens justify: `"1` is not a term, so this is just the
        // run_migration query.
        let injected: Vec<String> = fts5_search(db.conn(), "run_migration\" OR \"1", 10)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.name.clone())
            .collect();
        assert!(
            injected.iter().all(|n| n == "run_migration"),
            "quote-injection must not pull in unrelated rows, got {injected:?}"
        );
    }

    /// A single flag-shaped user token must not flood OR-fallback noise when
    /// its punctuation-split fragments never co-occur (batch review of audit
    /// 2026-08-16 P1-6: `--no-default-features` regressed from a clean empty
    /// to a wall of unrelated hits). The relaxed-AND retry may drop fragments
    /// absent from the index, but fragments that exist individually without
    /// co-occurring stay an honest empty. The SAME fragments typed as separate
    /// words are the user's own term list and keep the OR-fallback.
    #[test]
    fn test_single_flag_token_does_not_or_flood() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "flags.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        // "prune" and "vector" each exist, in different nodes; they never co-occur.
        insert_fn(db.conn(), fid, "prune_edges", 1, "fn prune_edges() {}");
        insert_fn(db.conn(), fid, "vector_scan", 20, "fn vector_scan() {}");

        // One user token: relaxed AND over [prune, vector] finds no
        // co-occurrence, and OR must NOT kick in.
        let flag = fts5_search(db.conn(), "--prune-vector", 10).unwrap();
        assert!(
            flag.nodes.is_empty() && !flag.or_fallback,
            "a single flag-shaped token whose fragments never co-occur must stay empty, got {:?}",
            flag.nodes
                .iter()
                .map(|n| n.name.clone())
                .collect::<Vec<_>>()
        );

        // Two user words: documented OR-fallback recall behavior.
        let listed = fts5_search(db.conn(), "prune vector", 10).unwrap();
        assert!(
            listed.or_fallback && listed.nodes.len() == 2,
            "user-listed words must keep OR-fallback, got or_fallback={} nodes={:?}",
            listed.or_fallback,
            listed
                .nodes
                .iter()
                .map(|n| n.name.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fts5_search_excludes_test_nodes() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        // Production function
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "validateToken".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "function validateToken(token) { jwt.verify(token); }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // Test function (should be excluded by default)
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "test_validateToken".into(),
                qualified_name: None,
                start_line: 10,
                end_line: 15,
                code_content: "function test_validateToken() { assert(validateToken('x')); }"
                    .into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: true,
            },
        )
        .unwrap();

        // Default search excludes test nodes
        let results = fts5_search(db.conn(), "validateToken", 10).unwrap().nodes;
        assert_eq!(results.len(), 1, "should exclude test node");
        assert_eq!(results[0].name, "validateToken");

        // With tests included
        let results_all = fts5_search_with_tests(db.conn(), "validateToken", 10)
            .unwrap()
            .nodes;
        assert_eq!(results_all.len(), 2, "should include test node");
    }

    #[test]
    fn test_fts5_and_then_or_strategy() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        // Node with both "validate" and "token" in content
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "validateToken".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "function validateToken(token) { return true; }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // Node with only "validate" (not "token")
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "validateEmail".into(),
                qualified_name: None,
                start_line: 10,
                end_line: 15,
                code_content: "function validateEmail(email) { return true; }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Multi-term query: AND should match validateToken; if not enough results, OR adds validateEmail
        let fts = fts5_search(db.conn(), "validate token", 10).unwrap();
        assert!(!fts.nodes.is_empty(), "should find results");
        // validateToken matches both terms so should rank first
        assert_eq!(fts.nodes[0].name, "validateToken");
    }

    #[test]
    fn test_fts5_and_threshold_no_unnecessary_or_fallback() {
        // Verify that a small number of high-quality AND results don't trigger OR fallback.
        // With limit=20: new threshold = max(3, 20/10) = 3
        // So 4 AND results >= 3 means no fallback.
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        // Create 4 nodes that match BOTH "parse" and "json" as separate tokens
        for i in 0..4 {
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: format!("handler{}", i),
                    qualified_name: None,
                    start_line: i * 10 + 1,
                    end_line: i * 10 + 5,
                    code_content: format!("function handler{}() {{ parse json data }}", i),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
        }
        // Create a node that only matches "parse" (not "json")
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "parseXml".into(),
                qualified_name: None,
                start_line: 50,
                end_line: 55,
                code_content: "function parseXml(xml) { parse xml data }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // With limit=20: old threshold was 20/2=10 (4 < 10 => fallback to OR)
        // New threshold: max(3, 20/10)=3, so 4 >= 3 => no OR fallback
        let fts = fts5_search(db.conn(), "parse json", 20).unwrap();
        assert!(
            !fts.or_fallback,
            "4 AND results >= threshold 3, should NOT fall back to OR"
        );
        // All 4 handler nodes match both terms
        assert_eq!(fts.nodes.len(), 4);
    }

    #[test]
    fn test_fts5_single_word_garbage_does_not_or_fallback() {
        // Regression: split_identifier("ZzzzNoMatchXyzzz") yields tokens
        // ["Match", "No", "Xyzzz", "Zzzz", "ZzzzNoMatchXyzzz"]. Real code often
        // contains "match" or "no" as standalone tokens (e.g. Rust `match`
        // keyword, `--no-default-features` flag). Without guarding, the OR
        // fallback turns a clearly-non-existent identifier into a wall of
        // unrelated hits — actively misleading the user.
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        // A real node whose name_tokens include the bare word "Match" — would
        // be reached by OR fallback if the guard were missing.
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "tryMatchSomething".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn tryMatchSomething() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: Some("try Match Something tryMatchSomething".into()),
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // And another with the bare token "No" in code_content.
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "buildScript".into(),
                qualified_name: None,
                start_line: 10,
                end_line: 14,
                code_content: "fn buildScript() { run(\"--no-default-features\"); }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: Some("build Script buildScript".into()),
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let result = fts5_search(db.conn(), "ZzzzNoMatchXyzzz", 20).unwrap();
        assert!(
            result.nodes.is_empty(),
            "single-word garbage query must not OR-fallback to camelCase noise; got {:?}",
            result.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fts5_single_word_real_identifier_still_matches() {
        // Verify the garbage-query guard doesn't suppress real single-word
        // matches whose camelCase parts happen to AND-fail.
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "validateToken".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn validateToken() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: Some("validate Token validateToken".into()),
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let result = fts5_search(db.conn(), "validateToken", 10).unwrap();
        assert!(!result.nodes.is_empty(), "real identifier must still match");
        assert_eq!(result.nodes[0].name, "validateToken");
    }

    #[test]
    fn test_fts5_multiword_garbage_still_or_fallbacks() {
        // OR fallback for multi-word queries is unchanged — the user explicitly
        // listed terms, and OR-widening is the documented recall behavior.
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "doMatchOnly".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn doMatchOnly() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: Some("do Match Only doMatchOnly".into()),
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Multi-word with one-real-one-fake — AND fails, OR finds the Match-only node.
        let result = fts5_search(db.conn(), "Match XyzNotReal", 10).unwrap();
        assert!(
            !result.nodes.is_empty(),
            "multi-word query keeps OR-fallback"
        );
        assert!(result.or_fallback, "expected or_fallback flag to be true");
    }

    #[test]
    fn test_levenshtein() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("handle_message", "handle_mesage"), 1);
        assert_eq!(levenshtein("database", "databas"), 1);
        assert_eq!(levenshtein("foo", "bar"), 3);
    }
}
