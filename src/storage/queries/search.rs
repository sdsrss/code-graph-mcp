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
    /// True when these rows came from the CJK substring scan rather than from
    /// FTS5 MATCH. Surfaces are expected to label them as a widened match.
    pub cjk_substring_fallback: bool,
    /// Why the result set is empty, when the reason is the QUERY rather than the
    /// index. `None` means the search really ran and found nothing.
    ///
    /// Without this a query whose every term was discarded before any SQL ran —
    /// `search x` (single characters are dropped by the `len() >= 2` filter),
    /// `search "the of"` (all stop words) — returned a result byte-identical to a
    /// genuine miss, and the surfaces then told the user their symbol does not
    /// exist. It was never looked for (2026-08-16 audit §四; the false-clean-empty
    /// class this repo has been bitten by before).
    pub empty_reason: Option<&'static str>,
}

impl FtsResult {
    /// Whether these rows are a WIDENED match — the precise query found nothing
    /// and the net was cast wider — as opposed to a direct hit.
    ///
    /// Both arms qualify for the same reason, so confidence scoring treats them
    /// alike rather than inventing a second constant: an OR fallback dropped the
    /// AND requirement, and a CJK substring rescue dropped tokenization.
    ///
    /// This flag drives exactly one thing: the widened-match confidence penalty
    /// (`CONF_OR_FALLBACK_PENALTY`). Scope it no wider when reading the call
    /// site — the "no text anchor" warning next to it keys on
    /// `fts_search.is_empty()` and never reads this flag, so it stops firing for
    /// a rescued CJK query because the scan returned ROWS, with or without the
    /// flag existing.
    ///
    /// The penalty IS observable: `match_confidence` is a response field and it
    /// multiplies every result's `relevance`, so a CJK rescue that lost this flag
    /// would ship `1.0` where it owes `0.6`.
    /// `mcp::server::tools::search` pins that end-to-end; the truth table below
    /// alone would leave the call site free to revert.
    pub fn is_widened_match(&self) -> bool {
        self.or_fallback || self.cjk_substring_fallback
    }
}

pub fn fts5_search(conn: &Connection, query: &str, limit: i64) -> Result<FtsResult> {
    fts5_search_impl(conn, query, limit, true)
}

/// FTS5 search including test symbols (for test-aware callers).
#[cfg(test)]
pub fn fts5_search_with_tests(conn: &Connection, query: &str, limit: i64) -> Result<FtsResult> {
    fts5_search_impl(conn, query, limit, false)
}

/// What counts as part of a term when splitting a raw query.
///
/// One definition, two readers: the MATCH preprocessing below and the substring
/// rescue in `cjk_substring_scan`. They must agree on where a term ends or the
/// rescue would scan for a string the MATCH path never looked up — the two
/// splitters drifting apart is exactly the class of bug that makes a filter
/// silently stop matching valid indexed nodes.
fn is_term_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Scripts written without spaces between words.
///
/// `unicode61` (the tokenizer `nodes_fts` declares) classes every one of these
/// code points as alphanumeric, so a whole phrase with no interior punctuation
/// becomes ONE token: `创建订单并扣减库存` is a single term. FTS5 matches tokens
/// and prefixes, never substrings, so `订单` — a real word sitting in the middle
/// of that token — can not be reached by MATCH at all. The text is indexed and
/// stored, and still answers "no results".
///
/// Covered, each with a fixture in this module's tests: CJK Unified Ideographs
/// and Extensions A-F, compatibility ideographs, full-width AND half-width kana,
/// precomposed Hangul syllables. NOT covered: Hangul compatibility Jamo, and
/// Thai/Lao/Khmer — they share the property, no fixture exercises them, so they
/// are left out rather than claimed.
fn is_unsegmented_script(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // Hiragana + Katakana
        | 0x3400..=0x4DBF   // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0xAC00..=0xD7AF   // Hangul syllables
        | 0xF900..=0xFAFF   // CJK Compatibility Ideographs
        | 0xFF66..=0xFF9D   // Half-width katakana (common in legacy JP source)
        | 0x20000..=0x2A6DF // CJK Unified Ideographs Extension B
        | 0x2A700..=0x2EBEF // CJK Unified Ideographs Extensions C-F
    )
}

/// How many distinct unsegmented-script terms the substring scan will AND.
///
/// Not a tuning knob — a correctness bound. `clauses.join(" AND ")` builds a
/// left-deep AND tree whose depth is the term count, and SQLite refuses to
/// prepare a statement past `SQLITE_MAX_EXPR_DEPTH` (1000), so pasting a
/// punctuated CJK document into `search` turned a query that used to answer
/// "no results" into a hard error on the CLI and a JSON-RPC error over MCP.
/// Anything past a handful of AND-ed words matches nothing anyway, so the
/// over-long query keeps the pre-rescue contract — an honest empty — rather
/// than paying for a scan that cannot succeed.
const CJK_SCAN_MAX_TERMS: usize = 32;

/// The ranking terms after the identifier-hit CASE: densest body, then id.
///
/// A const so the final tiebreak is inspectable. Pinning it behaviourally would
/// need two rows agreeing on every preceding term, and SQLite's order without a
/// tiebreak is stable-but-unspecified — such a fixture passes with the tiebreak
/// deleted, which is the vacuous shape
/// `test_fuzzy_fallback_pool_is_deterministically_bounded` already works around
/// for the fuzzy pool. Same workaround, same reason.
const CJK_SCAN_ORDER_TAIL: &str = ", LENGTH(COALESCE(n.code_content, '')), n.id";

fn fts5_search_impl(
    conn: &Connection,
    query: &str,
    limit: i64,
    exclude_tests: bool,
) -> Result<FtsResult> {
    let matched = fts5_search_match(conn, query, limit, exclude_tests)?;
    // Only a search that RAN and found nothing is worth widening. `empty_reason`
    // means no SQL ever executed (every term was dropped in preprocessing), and
    // a substring scan for a query the tokenizer rejected would answer a
    // question the user was already told was unanswerable.
    if !matched.nodes.is_empty() || matched.empty_reason.is_some() {
        return Ok(matched);
    }
    cjk_substring_scan(conn, query, limit, exclude_tests)
}

/// Substring rescue for unsegmented-script queries that MATCH could not reach.
///
/// Scoped three ways so it cannot become a general fuzzy-match: it runs only
/// after MATCH returned zero rows, only over the terms that actually contain
/// unsegmented-script characters (an ASCII term in a mixed query stays on MATCH
/// semantics — substring-matching Latin text would turn every typo into a
/// flood), and it AND-joins those terms, mirroring the AND-first strategy above.
///
/// # The scope is WHOLE-RESULT-empty, which leaves a real gap
///
/// A mixed query whose Latin half matches anything never reaches here, so the
/// CJK word in `search "payment 订单"` stays exactly as unreachable as before
/// this function existed: the AND fails, the OR fallback matches `payment`, the
/// result is non-empty, and the caller returns. The user sees rows and gets no
/// signal that half the query went unanswered. Widening the trigger to "no
/// returned row matched any CJK term" would fix it but has to union two ranked
/// sets, so it is deliberately out of scope here and documented in the README
/// rather than half-done. `test_mixed_query_leaves_the_cjk_term_unreachable`
/// pins the current behaviour so the gap cannot close by accident and go
/// unnoticed.
///
/// Rows carry no BM25 score: there is none. The `0.0` tells `weighted_rrf_fusion`
/// no raw score is available, so these rank by RRF position alone rather than by
/// a number invented for them.
fn cjk_substring_scan(
    conn: &Connection,
    query: &str,
    limit: i64,
    exclude_tests: bool,
) -> Result<FtsResult> {
    let empty = |()| FtsResult {
        nodes: vec![],
        bm25_scores: vec![],
        or_fallback: false,
        cjk_substring_fallback: false,
        empty_reason: None,
    };

    // Each MAXIMAL RUN of unsegmented-script characters is its own term. Filtering
    // the non-CJK characters out of a token instead would glue the run on either
    // side of them together and scan for a string nobody typed: `订x单` would look
    // for `%订单%` and match a document containing neither `订x单` nor `订 单`.
    let mut cjk_terms: Vec<String> = Vec::new();
    let mut run = String::new();
    for c in query.chars() {
        if is_unsegmented_script(c) {
            run.push(c);
        } else if !run.is_empty() {
            cjk_terms.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        cjk_terms.push(run);
    }
    cjk_terms.sort_unstable(); // deterministic ?1..?N binding
    cjk_terms.dedup();
    if cjk_terms.is_empty() {
        return Ok(empty(()));
    }
    // Past the bound the AND tree cannot be prepared at all — see
    // `CJK_SCAN_MAX_TERMS`. Answer the pre-rescue empty rather than an error.
    if cjk_terms.len() > CJK_SCAN_MAX_TERMS {
        return Ok(empty(()));
    }

    // The columns `nodes_fts` indexes that can hold prose. `n.name` is scanned
    // first in the ORDER BY below so an identifier hit outranks a body hit.
    const TEXT_COLS: [&str; 5] = [
        "n.name",
        "n.qualified_name",
        "n.code_content",
        "n.context_string",
        "n.doc_comment",
    ];

    let mut params: Vec<String> = Vec::with_capacity(cjk_terms.len());
    let mut clauses: Vec<String> = Vec::with_capacity(cjk_terms.len());
    let mut name_hit_ors: Vec<String> = Vec::with_capacity(cjk_terms.len() * 2);
    for (i, term) in cjk_terms.iter().enumerate() {
        // `escape_like` cannot currently fire: none of `%`, `_` or `\` is inside
        // any range `is_unsegmented_script` accepts, so every term is already
        // literal. It stays as the invariant that keeps `ESCAPE '\'` honest if
        // that predicate is ever widened — not as a guard anything exercises.
        params.push(format!("%{}%", escape_like(term)));
        let idx = i + 1;
        let ors: Vec<String> = TEXT_COLS
            .iter()
            .map(|col| format!("{} LIKE ?{} ESCAPE '\\'", col, idx))
            .collect();
        clauses.push(format!("({})", ors.join(" OR ")));
        name_hit_ors.push(format!("n.name LIKE ?{} ESCAPE '\\'", idx));
        name_hit_ors.push(format!("n.qualified_name LIKE ?{} ESCAPE '\\'", idx));
    }
    let test_filter = if exclude_tests {
        " AND n.is_test = 0"
    } else {
        ""
    };
    // An unordered LIMIT picks an arbitrary subset (the invariant
    // `test_fuzzy_fallback_pool_is_deterministically_bounded` pins for the fuzzy
    // pool). Rank: identifier hits first, then the densest match — a short body
    // containing the term is a stronger signal than a long one — then id, so the
    // order is total and the same query always answers the same way.
    //
    // The CASE spans EVERY term, not just `?1`. Hard-coding `?1` ranked on
    // whichever term happened to sort first in UTF-8 order, so for `订单 库存`
    // a node named `订单处理器` lost to an unrelated short body while
    // `库存处理器` won — "identifier hits first" held for one arbitrary half of
    // the query.
    let name_hit = format!("CASE WHEN {} THEN 0 ELSE 1 END", name_hit_ors.join(" OR "));
    let sql = format!(
        "SELECT {} FROM nodes n WHERE {}{} ORDER BY {}{} LIMIT ?{}",
        NODE_SELECT_ALIASED,
        clauses.join(" AND "),
        test_filter,
        name_hit,
        CJK_SCAN_ORDER_TAIL,
        cjk_terms.len() + 1
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut bound: Vec<&dyn rusqlite::ToSql> = params
        .iter()
        .map(|p| p as &dyn rusqlite::ToSql)
        .collect::<Vec<_>>();
    bound.push(&limit);
    let rows = stmt.query_map(bound.as_slice(), map_node_row)?;
    let nodes: Vec<NodeResult> = rows.collect::<Result<Vec<_>, _>>()?;
    if nodes.is_empty() {
        return Ok(empty(()));
    }
    Ok(FtsResult {
        bm25_scores: vec![0.0; nodes.len()],
        nodes,
        or_fallback: false,
        cjk_substring_fallback: true,
        empty_reason: None,
    })
}

fn fts5_search_match(
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
    // No usable term survived preprocessing, so no SQL will run. Say WHY: this
    // return is otherwise byte-identical to a genuine miss, and the surfaces above
    // then report the symbol as absent from a search that never happened.
    if terms.is_empty() {
        let had_input = !raw_terms.is_empty();
        let all_stop_words = had_input
            && raw_terms
                .iter()
                .all(|w| FTS_STOP_WORDS.contains(&w.to_lowercase().as_str()));
        return Ok(FtsResult {
            nodes: vec![],
            bm25_scores: vec![],
            or_fallback: false,
            cjk_substring_fallback: false,
            empty_reason: if !had_input {
                Some("the query has no searchable characters")
            } else if all_stop_words {
                Some("every word in the query is a stop word")
            } else {
                Some(
                    "every term is shorter than the 2-character minimum the index tokenizer stores",
                )
            },
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
        if pairs.len() >= crate::domain::AND_MATCH_FLOOR {
            let (nodes, bm25_scores): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
            return Ok(FtsResult {
                nodes,
                bm25_scores,
                or_fallback: false,
                cjk_substring_fallback: false,
                empty_reason: None,
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
                            cjk_substring_fallback: false,
                            empty_reason: None,
                        });
                    }
                }
                return Ok(FtsResult {
                    nodes: vec![],
                    bm25_scores: vec![],
                    or_fallback: false,
                    cjk_substring_fallback: false,
                    empty_reason: None,
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
                            cjk_substring_fallback: false,
                            empty_reason: None,
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
        cjk_substring_fallback: false,
        empty_reason: None,
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

/// The definitions named like `partial_name`, best first, at most ten: the
/// rows a "did you mean" list shows.
pub fn find_functions_by_fuzzy_name(
    conn: &Connection,
    partial_name: &str,
) -> Result<Vec<NameCandidate>> {
    find_functions_by_fuzzy_name_up_to(conn, partial_name, 10)
}

/// [`find_functions_by_fuzzy_name`] with `limit` rows at most.
pub fn find_functions_by_fuzzy_name_up_to(
    conn: &Connection,
    partial_name: &str,
    limit: usize,
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
         LIMIT ?5";
    let mut stmt = conn.prepare(sql)?;
    // ?2 is the raw name for the exact-equality bucket (`=` treats %/_ literally);
    // ?4 is the %/_-escaped form for the prefix-LIKE bucket, so a query containing
    // a wildcard char cannot mis-bucket names via the ordering LIKE (matches the
    // WHERE clause, which already escapes). Ordering-only fix; result set unchanged.
    let rows = stmt.query_map(
        rusqlite::params![pattern, partial_name, token_pattern, escaped, limit as i64],
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
    scored.truncate(limit);
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

        // 4 AND hits clear `AND_MATCH_FLOOR` (3), so the precise arm is kept.
        let fts = fts5_search(db.conn(), "parse json", 20).unwrap();
        assert!(
            !fts.or_fallback,
            "4 AND results >= AND_MATCH_FLOOR, should NOT fall back to OR"
        );
        // All 4 handler nodes match both terms
        assert_eq!(fts.nodes.len(), 4);

        // P2 (2026-08-16 audit §四): the AND→OR decision must not move with the
        // caller's fetch-pool size. It used to be `limit / 10`, and `limit` here
        // is the over-fetched POOL, whose factor is 4× for a plain search and 16×
        // (floor 100) once a `--language`/`--node-type` filter is set. So the same
        // query kept its 4 precise hits unfiltered and threw them away for OR
        // noise with a filter — a filter that can only narrow, widening the
        // answer. The three limits below are exactly the pools `search_fetch_count`
        // produces for top_k=20 in each mode, plus the direct-call value.
        for pool in [
            20,
            crate::domain::search_fetch_count(20, false),
            crate::domain::search_fetch_count(20, true),
        ] {
            let fts = fts5_search(db.conn(), "parse json", pool).unwrap();
            assert!(
                !fts.or_fallback,
                "pool size {pool} must not change the AND/OR verdict (old rule: \
                 floor {} would have discarded 4 precise hits)",
                pool / 10
            );
            assert_eq!(fts.nodes.len(), 4, "pool {pool} must return the same rows");
        }
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

    /// Seeds one node whose text carries a long unsegmented CJK run.
    ///
    /// `unicode61` classifies every CJK ideograph as alphanumeric, so a run with
    /// no interior punctuation becomes ONE token. That makes the words inside it
    /// unreachable by MATCH, which is what the substring fallback exists to fix.
    fn cjk_fixture() -> (crate::storage::db::Database, tempfile::TempDir) {
        let (db, tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "order.py".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "create_order".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "def create_order(uid):\n    pass".into(),
                signature: None,
                doc_comment: Some("创建订单并扣减库存".into()),
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "charge_card".into(),
                qualified_name: None,
                start_line: 7,
                end_line: 9,
                code_content: "def charge_card(token):\n    pass".into(),
                signature: None,
                doc_comment: Some("payment gateway wrapper".into()),
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        (db, tmp)
    }

    /// The defect: `订单` is INSIDE the stored run `创建订单并扣减库存`, so MATCH
    /// cannot reach it. Before the fallback this returned zero rows and the CLI
    /// told the user the term does not exist.
    #[test]
    fn test_cjk_substring_query_finds_node() {
        let (db, _tmp) = cjk_fixture();

        // Precondition, not decoration: if MATCH ever starts segmenting CJK this
        // test would pass for a reason that has nothing to do with the fallback.
        let via_match = fts5_search(db.conn(), "创建订单并扣减库存", 10).unwrap();
        assert_eq!(
            via_match.nodes.len(),
            1,
            "whole-run query must match via FTS5 — fixture or tokenizer changed"
        );
        assert!(
            !via_match.cjk_substring_fallback,
            "whole-run query is a real MATCH hit, the fallback must not fire"
        );

        let result = fts5_search(db.conn(), "订单", 10).unwrap();
        assert_eq!(
            result.nodes.len(),
            1,
            "a CJK word inside an unsegmented run must be reachable"
        );
        assert_eq!(result.nodes[0].name, "create_order");
        assert!(
            result.cjk_substring_fallback,
            "this hit came from the substring scan and must say so"
        );
    }

    /// The fallback is scoped to CJK. An ASCII miss must stay an honest empty —
    /// otherwise every typo turns into a substring flood.
    #[test]
    fn test_ascii_miss_does_not_trigger_substring_fallback() {
        let (db, _tmp) = cjk_fixture();

        let result = fts5_search(db.conn(), "gatewa", 10).unwrap();
        assert!(
            result.nodes.is_empty(),
            "ASCII substring of an indexed word must NOT be found by the CJK fallback"
        );
        assert!(!result.cjk_substring_fallback);
    }

    /// A CJK query that MATCH already answers must not pay for the scan, and a
    /// CJK query matching nothing at all must stay empty rather than return rows.
    #[test]
    fn test_cjk_fallback_only_fires_on_empty_and_only_for_cjk() {
        let (db, _tmp) = cjk_fixture();

        let miss = fts5_search(db.conn(), "无关词条", 10).unwrap();
        assert!(miss.nodes.is_empty(), "genuine CJK miss stays empty");

        // ASCII query that MATCH answers: fallback flag stays clear.
        let ascii = fts5_search(db.conn(), "payment gateway", 10).unwrap();
        assert!(!ascii.nodes.is_empty(), "ASCII search still works");
        assert!(!ascii.cjk_substring_fallback);
    }

    /// Both widening arms must read as widened, and a direct hit must not.
    ///
    /// This pins the truth table only. It does NOT pin the call site: replacing
    /// `fts_result.is_widened_match()` with `fts_result.or_fallback` leaves this
    /// test green, so `test_cjk_rescue_takes_the_widened_match_penalty` in
    /// `mcp::server::tools::search` is the one that kills that mutation.
    #[test]
    fn test_widened_match_covers_both_arms() {
        let mk = |or_fb: bool, cjk: bool| FtsResult {
            nodes: vec![],
            bm25_scores: vec![],
            or_fallback: or_fb,
            cjk_substring_fallback: cjk,
            empty_reason: None,
        };
        assert!(
            !mk(false, false).is_widened_match(),
            "direct hit is not widened"
        );
        assert!(mk(true, false).is_widened_match(), "OR fallback is widened");
        assert!(
            mk(false, true).is_widened_match(),
            "CJK substring rescue is widened — dropping this re-fires the no-anchor warning"
        );
        assert!(mk(true, true).is_widened_match());
    }

    /// The substring scan has no BM25 to rank by, so its ORDER BY is the only
    /// thing deciding which rows survive `LIMIT`. Fixture: three nodes all
    /// containing 库存, distinguishable only by where and how long.
    #[test]
    fn test_cjk_substring_ranking_is_identifier_then_shortest() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "w.py".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        let add = |name: &str, body: &str, doc: Option<&str>| {
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: name.into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 2,
                    code_content: body.into(),
                    signature: None,
                    doc_comment: doc.map(String::from),
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
        };
        // The identifier-hit node carries the LONGEST body on purpose. With a
        // short one, `LENGTH(code_content)` alone reproduces the expected order
        // and the identifier leg can be neutralised (`THEN 0 ELSE 1` ->
        // `THEN 1 ELSE 1`) with the whole suite still green — a fixture that
        // agrees with both legs pins neither. Here the two legs DISAGREE, so
        // only the identifier leg can produce this order.
        add("short_body", "y = 2  # 扣减库存", None);
        add(
            "mid_body",
            &format!("x = 1  # 扣减库存\n{}", "pad\n".repeat(50)),
            None,
        );
        add(
            "库存校验",
            &format!("z = 3\n{}", "pad\n".repeat(200)),
            Some("与查询词无关的说明"),
        );

        let r = fts5_search(db.conn(), "库存", 10).unwrap();
        assert!(
            r.cjk_substring_fallback,
            "must come from the substring scan"
        );
        let order: Vec<&str> = r.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            order,
            vec!["库存校验", "short_body", "mid_body"],
            "identifier hit first even with the longest body, then shortest body"
        );
    }

    /// The scan's LIMIT decides which rows survive, so its ORDER BY must end in
    /// a total tiebreak — same one-directional invariant, and the same reason,
    /// as `test_fuzzy_fallback_pool_is_deterministically_bounded`.
    #[test]
    fn test_cjk_scan_order_ends_in_a_total_tiebreak() {
        assert!(
            CJK_SCAN_ORDER_TAIL.trim_end().ends_with("n.id"),
            "without a unique final term the LIMIT keeps an arbitrary subset of \
             rows that tie on every preceding term: {CJK_SCAN_ORDER_TAIL}"
        );
        assert!(
            CJK_SCAN_ORDER_TAIL.contains("LENGTH(COALESCE(n.code_content"),
            "the density leg must precede the tiebreak"
        );
    }

    /// The identifier-hit rank must span EVERY term, not `?1` alone.
    ///
    /// `?1` is whichever term sorts first in UTF-8 order, so hard-coding it made
    /// the rank depend on an ordering the user cannot see: for `订单 库存`,
    /// 库 (U+5E93) precedes 订 (U+8BA2), and a node named `订单处理器` lost to an
    /// unrelated short body.
    #[test]
    fn test_cjk_identifier_rank_spans_every_term() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "m.py".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        let add = |name: &str, body: &str| {
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: name.into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 2,
                    code_content: body.into(),
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
        };
        // The run is UNSPACED: spelling it `订单 库存` would make each half its
        // own FTS token, MATCH would answer directly and the scan would never
        // run. Every node carries the same run, so the AND matches all three and
        // only the identifier leg can separate them; `helper` is the shortest.
        const RUN: &str = "创建订单并扣减库存";
        add("helper", &format!("a = 1  # {RUN}"));
        add(
            "订单处理器",
            &format!("b = 2  # {RUN}\n{}", "pad\n".repeat(100)),
        );
        add(
            "库存处理器",
            &format!("c = 3  # {RUN}\n{}", "pad\n".repeat(200)),
        );

        let r = fts5_search(db.conn(), "订单 库存", 10).unwrap();
        assert!(r.cjk_substring_fallback);
        let order: Vec<&str> = r.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            order,
            vec!["订单处理器", "库存处理器", "helper"],
            "both identifier hits outrank the shortest non-identifier body"
        );
    }

    /// The scan AND-joins its terms. Flipping the join to OR turns the rescue
    /// into the broad flood the module doc promises it is not, and every other
    /// CJK test uses a single term, so nothing else would notice.
    #[test]
    fn test_cjk_multi_term_requires_all_terms() {
        let (db, _tmp) = cjk_fixture();
        // 订单 is in create_order's doc; 网关 is in no node at all.
        let both = fts5_search(db.conn(), "订单 网关", 10).unwrap();
        assert!(
            both.nodes.is_empty(),
            "AND semantics: a term present in no node must empty the result"
        );
        // Control: the surviving term alone still resolves, so the emptiness
        // above is the AND and not a broken scan.
        let one = fts5_search(db.conn(), "订单", 10).unwrap();
        assert_eq!(one.nodes.len(), 1);
    }

    /// A query past `CJK_SCAN_MAX_TERMS` used to be `Err(Expression tree is too
    /// large)` from SQLite rather than an empty result — a hard CLI exit and a
    /// JSON-RPC error for pasting a punctuated CJK document into `search`.
    #[test]
    fn test_cjk_scan_term_cap_answers_empty_not_error() {
        let (db, _tmp) = cjk_fixture();
        let many: String = (0..(CJK_SCAN_MAX_TERMS + 1))
            .map(|i| char::from_u32(0x4E00 + i as u32).unwrap())
            .map(|c| format!("{} ", c))
            .collect();
        let r = fts5_search(db.conn(), &many, 10).expect("over-cap query must not error");
        assert!(r.nodes.is_empty());
        assert!(!r.cjk_substring_fallback);

        // A pathological count SQLite definitely refuses to prepare.
        let huge: String = (0..2000)
            .map(|i| char::from_u32(0x4E00 + i as u32).unwrap())
            .map(|c| format!("{} ", c))
            .collect();
        assert!(fts5_search(db.conn(), &huge, 10).is_ok());
    }

    /// Each maximal RUN of unsegmented script is its own term, AND-ed like any
    /// other pair — NOT concatenated across whatever separates them.
    ///
    /// The fixture discriminates the two readings. `create_order`'s doc is
    /// 创建订单并扣减库存, which contains 订单 and 库存 but never the contiguous
    /// string 订单库存. Concatenating would search `%订单库存%` and find nothing;
    /// splitting searches `%订单%` AND `%库存%` and finds the node. So this query
    /// resolves under the correct reading and is empty under the wrong one,
    /// which is the opposite of what a same-result fixture would prove.
    #[test]
    fn test_cjk_runs_are_not_glued_across_separators() {
        let (db, _tmp) = cjk_fixture();

        let split = fts5_search(db.conn(), "订单x库存", 10).unwrap();
        assert_eq!(
            split.nodes.len(),
            1,
            "two runs must be ANDed, not concatenated into 订单库存"
        );
        assert_eq!(split.nodes[0].name, "create_order");
        assert!(split.cjk_substring_fallback);

        // Control: the concatenation really is absent from the corpus, so the
        // assertion above distinguishes the readings instead of passing for free.
        assert!(
            fts5_search(db.conn(), "订单库存", 10)
                .unwrap()
                .nodes
                .is_empty(),
            "the glued string must match nothing, else this test proves nothing"
        );
    }

    /// The scan must not surface test symbols, matching the MATCH path's filter.
    #[test]
    fn test_cjk_scan_excludes_test_nodes() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.py".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "test_helper".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 2,
                code_content: "pass".into(),
                signature: None,
                doc_comment: Some("校验库存是否充足".into()),
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: true,
            },
        )
        .unwrap();

        assert!(
            fts5_search(db.conn(), "库存", 10).unwrap().nodes.is_empty(),
            "a test-only node must not be rescued into ordinary results"
        );
        // Control: the same node IS reachable when tests are included, so the
        // emptiness above is the filter and not a scan that found nothing.
        let with_tests = fts5_search_with_tests(db.conn(), "库存", 10).unwrap();
        assert_eq!(with_tests.nodes.len(), 1);
        assert!(with_tests.cjk_substring_fallback);
    }

    /// All five prose columns are scanned. Dropping one narrows the rescue
    /// silently; no other fixture puts CJK in `context_string` or
    /// `qualified_name`.
    #[test]
    fn test_cjk_scan_covers_every_text_column() {
        /// label, and where in the record to put the CJK run
        type ColCase = (&'static str, fn(&mut NodeRecord));
        let cols: [ColCase; 5] = [
            ("name", |n| n.name = "库存校验".into()),
            ("qualified_name", |n| {
                n.qualified_name = Some("mod::库存校验".into())
            }),
            ("code_content", |n| n.code_content = "x = '库存校验'".into()),
            ("context_string", |n| {
                n.context_string = Some("库存校验".into())
            }),
            ("doc_comment", |n| n.doc_comment = Some("库存校验".into())),
        ];
        for (label, place) in cols {
            let (db, _tmp) = test_db();
            let fid = upsert_file(
                db.conn(),
                &FileRecord {
                    path: "c.py".into(),
                    blake3_hash: "h".into(),
                    last_modified: 1,
                    language: Some("python".into()),
                },
            )
            .unwrap();
            let mut rec = NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "plain".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 2,
                code_content: "pass".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            };
            place(&mut rec);
            insert_node(db.conn(), &rec).unwrap();
            let r = fts5_search(db.conn(), "库存", 10).unwrap();
            assert_eq!(r.nodes.len(), 1, "column {label} must be scanned");
        }
    }

    /// Every script the `is_unsegmented_script` doc claims must actually resolve
    /// a needle inside a run — the comment is otherwise an unchecked promise.
    #[test]
    fn test_cjk_scan_covers_every_claimed_script() {
        // (label, stored run, needle inside it)
        let cases: [(&str, &str, &str); 5] = [
            ("CJK Unified", "创建订单并扣减库存", "订单"),
            ("kana (full-width)", "ちゅうもんをさくせいする", "もん"),
            ("kana (half-width)", "ﾁｭｳﾓﾝｻｸｾｲ", "ﾓﾝ"),
            ("Hangul syllables", "주문생성처리", "주문"),
            ("Ext B", "\u{20000}\u{20001}\u{20002}", "\u{20001}"),
        ];
        for (label, stored, needle) in cases {
            let (db, _tmp) = test_db();
            let fid = upsert_file(
                db.conn(),
                &FileRecord {
                    path: "s.py".into(),
                    blake3_hash: "h".into(),
                    last_modified: 1,
                    language: Some("python".into()),
                },
            )
            .unwrap();
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: "f".into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 2,
                    code_content: "pass".into(),
                    signature: None,
                    doc_comment: Some(stored.into()),
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
            let r = fts5_search(db.conn(), needle, 10).unwrap();
            assert_eq!(
                r.nodes.len(),
                1,
                "{label}: needle inside a run must resolve"
            );
            assert!(r.cjk_substring_fallback, "{label}");
        }
    }

    /// The documented limit of the rescue's scope, pinned so it cannot change
    /// silently in either direction.
    ///
    /// `payment` matches `charge_card` through the OR fallback, so the whole
    /// result is non-empty and the scan never runs — `create_order`, whose doc
    /// contains 订单, stays unreachable and the user gets rows with no signal
    /// that half the query went unanswered.
    #[test]
    fn test_mixed_query_leaves_the_cjk_term_unreachable() {
        let (db, _tmp) = cjk_fixture();

        let pure = fts5_search(db.conn(), "订单", 10).unwrap();
        assert_eq!(pure.nodes[0].name, "create_order", "control: rescue works");

        let mixed = fts5_search(db.conn(), "payment 订单", 10).unwrap();
        let names: Vec<&str> = mixed.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            !names.contains(&"create_order"),
            "documented gap: a non-empty Latin half suppresses the rescue"
        );
        assert!(
            !mixed.cjk_substring_fallback,
            "the scan must not have run at all"
        );
        assert!(mixed.or_fallback, "the rows came from the OR widening");
    }

    /// A single CJK character is a word, so it stays searchable — the 2-character
    /// minimum on the MATCH side comes from what the TOKENIZER stores, and the
    /// substring scan has no tokenizer. Pinned because the asymmetry with Latin
    /// (`search a` is refused) looks like an oversight and is not one: refusing
    /// single-character CJK would drop `search 猫` finding `猫咪管理`.
    #[test]
    fn test_single_cjk_character_still_searches() {
        let (db, _tmp) = cjk_fixture();
        let r = fts5_search(db.conn(), "订", 10).unwrap();
        assert_eq!(r.nodes.len(), 1, "one CJK char must still reach the scan");
        assert_eq!(r.nodes[0].name, "create_order");
        assert!(r.cjk_substring_fallback);
        assert!(
            r.empty_reason.is_none(),
            "must not be refused as 'shorter than the 2-character minimum'"
        );

        // The Latin side keeps its refusal — same query length, opposite answer.
        let latin = fts5_search(db.conn(), "a", 10).unwrap();
        assert!(latin.nodes.is_empty());
        assert!(
            latin.empty_reason.is_some(),
            "single Latin char stays refused"
        );
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
