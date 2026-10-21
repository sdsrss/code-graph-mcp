//! Surprising connections: unexpected cross-file couplings ranked by a composite
//! surprise score, for review/audit ("why does this reach that?").
//!
//! Candidates are cross-file `calls`/`references` edges (structural relations like
//! `imports`/`inherits` are expected, not surprising). Each is scored by:
//!   - **confidence** — how the resolver matched the target (`edges.confidence`):
//!     `ambiguous` (bare name, >1 candidate) = 3, `inferred` (bare name, unique) = 2,
//!     `extracted` (direct) = 1. Lower confidence = more noteworthy.
//!   - **cross-module** (+2) — endpoints in different top-level directories.
//!   - **sole bridge** (+2) — the *only* edge between those two modules (a single
//!     unexpected thread between otherwise-separate parts of the codebase).
//!
//! Deferred (documented follow-ups): peripheral→hub weighting (needs node degree)
//! and a cross-community bonus (needs community detection, roadmap item ③).
//! Deterministic for a fixed input. CLI-only; not exposed as an MCP tool.

use anyhow::Result;
use rusqlite::Connection;

use crate::domain::{CONF_AMBIGUOUS, CONF_INFERRED};

const CROSS_DIR_BONUS: i32 = 2;
const SOLE_BRIDGE_BONUS: i32 = 2;

/// One cross-file edge fed to the scorer.
#[derive(Debug, Clone)]
pub struct SurpriseInput {
    pub source: String,
    pub source_file: String,
    pub target: String,
    pub target_file: String,
    pub relation: String,
    /// `edges.confidence`: extracted | inferred | ambiguous.
    pub confidence: String,
    /// A call into a TypeScript `interface` or `type` alias, which the
    /// resolver labels `ambiguous` whatever the candidates
    /// (`classify_edge_confidence`).
    pub type_call: bool,
}

/// A scored surprising connection with a human explanation of the score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurprisingConnection {
    pub source: String,
    pub source_file: String,
    pub target: String,
    pub target_file: String,
    pub relation: String,
    pub confidence: String,
    pub score: i32,
    /// Why this edge is surprising — one phrase per scoring component that fired.
    pub reasons: Vec<String>,
}

/// Directory of a path (everything before the last '/'), matching the project-map
/// module convention; top-level files map to "<root>".
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "<root>",
    }
}

/// Score and rank surprising connections, returning the top `top_n` by score
/// (descending; ties broken lexically by source file/name then target file/name
/// for determinism).
pub fn find_surprising(edges: &[SurpriseInput], top_n: usize) -> Vec<SurprisingConnection> {
    use std::collections::HashMap;

    // Count edges per unordered cross-directory module pair, to spot sole bridges.
    let mut pair_count: HashMap<(&str, &str), usize> = HashMap::new();
    for e in edges {
        let (ds, dt) = (dir_of(&e.source_file), dir_of(&e.target_file));
        if ds != dt {
            *pair_count.entry(module_pair(ds, dt)).or_insert(0) += 1;
        }
    }

    let mut out: Vec<SurprisingConnection> = edges
        .iter()
        .map(|e| {
            let (ds, dt) = (dir_of(&e.source_file), dir_of(&e.target_file));
            let mut score = confidence_surprise(&e.confidence);
            let mut reasons = vec![confidence_reason(&e.confidence, e.type_call)];

            if ds != dt {
                score += CROSS_DIR_BONUS;
                reasons.push(format!("crosses modules ({ds} → {dt})"));
                if pair_count.get(&module_pair(ds, dt)).copied().unwrap_or(0) <= 1 {
                    score += SOLE_BRIDGE_BONUS;
                    reasons.push("sole connection between these modules".to_string());
                }
            }

            SurprisingConnection {
                source: e.source.clone(),
                source_file: e.source_file.clone(),
                target: e.target.clone(),
                target_file: e.target_file.clone(),
                relation: e.relation.clone(),
                confidence: e.confidence.clone(),
                score,
                reasons,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.source_file.cmp(&b.source_file))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.target_file.cmp(&b.target_file))
            .then_with(|| a.target.cmp(&b.target))
    });
    out.truncate(top_n);
    out
}

/// Unordered (dir_a, dir_b) key so A→B and B→A count toward the same module pair.
fn module_pair<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn confidence_surprise(c: &str) -> i32 {
    if c == CONF_AMBIGUOUS {
        3
    } else if c == CONF_INFERRED {
        2
    } else {
        1
    }
}

fn confidence_reason(c: &str, type_call: bool) -> String {
    if c == CONF_AMBIGUOUS && type_call {
        "ambiguous: a call into a TypeScript type, which exists only at compile time".to_string()
    } else if c == CONF_AMBIGUOUS {
        "ambiguous: bare-name target not decided (several candidates)".to_string()
    } else if c == CONF_INFERRED {
        "inferred: cross-file bare-name resolution".to_string()
    } else {
        "extracted: directly resolved".to_string()
    }
}

/// Query cross-file `calls`/`references` edges (with resolution confidence) and
/// rank them by surprise. Test symbols are excluded unless `include_tests`.
/// Returns the top `top_n`. CLI-only wrapper around [`find_surprising`].
pub fn surprising_connections(
    conn: &Connection,
    include_tests: bool,
    top_n: usize,
) -> Result<Vec<SurprisingConnection>> {
    use crate::domain::{REL_CALLS, REL_REFERENCES};
    // No user input in `test_filter` (helper emits a fixed GLOB literal) → no
    // injection. Uses the full is_test_node predicate, not the raw `is_test` flag:
    // an integration test `def test_foo()` in `tests/` has is_test=0 (the parser
    // only flags AST-level markers) yet a `test_foo → foo` call is the *expected*
    // coupling, not a surprising one. Applied symmetrically to source and target
    // (mirrors the existing `<external>`/`<module>` symmetric guards on ns/nt).
    let test_filter = if include_tests {
        String::new()
    } else {
        format!(
            " AND NOT {} AND NOT {}",
            crate::domain::is_test_node_sql("ns", "sf"),
            crate::domain::is_test_node_sql("nt", "tf")
        )
    };
    let sql = format!(
        "SELECT ns.name, sf.path, nt.name, tf.path, e.relation, e.confidence, \
                e.relation = ?1 AND nt.type IN ('interface', 'type') \
                AND tf.language IN ('typescript', 'tsx') \
         FROM edges e \
         JOIN nodes ns ON ns.id = e.source_id \
         JOIN files sf ON sf.id = ns.file_id \
         JOIN nodes nt ON nt.id = e.target_id \
         JOIN files tf ON tf.id = nt.file_id \
         WHERE e.relation IN (?1, ?2) \
           AND sf.id != tf.id \
           AND sf.path != '<external>' AND tf.path != '<external>' \
           AND ns.name != '<module>' AND nt.name != '<module>'{test_filter}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![REL_CALLS, REL_REFERENCES], |row| {
        Ok(SurpriseInput {
            source: row.get(0)?,
            source_file: row.get(1)?,
            target: row.get(2)?,
            target_file: row.get(3)?,
            relation: row.get(4)?,
            confidence: row.get(5)?,
            type_call: row.get(6)?,
        })
    })?;
    let mut inputs = Vec::new();
    for r in rows {
        inputs.push(r?);
    }
    Ok(find_surprising(&inputs, top_n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        (db, tmp)
    }

    #[test]
    fn surprising_connections_selects_cross_file_calls_refs_only() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.ts','h1',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('lib/b.ts','h2',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (1,'function','callerA',1,2,'',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (2,'function','targetB',1,2,'',0)", []).unwrap();
        // Cross-file ambiguous call src/a.ts → lib/b.ts (a surprising candidate).
        conn.execute("INSERT INTO edges (source_id,target_id,relation,confidence) VALUES (1,2,'calls','ambiguous')", []).unwrap();
        // A cross-file IMPORT (structural) — must be excluded from candidates.
        conn.execute("INSERT INTO edges (source_id,target_id,relation,confidence) VALUES (1,2,'imports','extracted')", []).unwrap();

        let r = surprising_connections(conn, false, 10).unwrap();
        assert_eq!(
            r.len(),
            1,
            "only the cross-file call is a candidate (import excluded); got {r:?}"
        );
        assert_eq!(
            (
                r[0].source.as_str(),
                r[0].target.as_str(),
                r[0].confidence.as_str()
            ),
            ("callerA", "targetB", "ambiguous"),
        );
        assert_eq!(
            r[0].score, 7,
            "ambiguous(3) + cross-module(2) + sole bridge(2)"
        );
    }

    #[test]
    fn surprising_connections_excludes_tests_by_default() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.ts','h1',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('lib/b.ts','h2',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (1,'function','callerA',1,2,'',0)", []).unwrap();
        // Target is a test symbol → excluded by default, included with include_tests.
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (2,'function','testB',1,2,'',1)", []).unwrap();
        conn.execute("INSERT INTO edges (source_id,target_id,relation,confidence) VALUES (1,2,'calls','inferred')", []).unwrap();

        assert!(
            surprising_connections(conn, false, 10).unwrap().is_empty(),
            "edge to a test symbol is excluded by default"
        );
        assert_eq!(
            surprising_connections(conn, true, 10).unwrap().len(),
            1,
            "included with include_tests"
        );
    }

    /// Sibling-hole guard: the parser sets `is_test=1` only for AST-level markers
    /// (`#[cfg(test)]`, `@Test`, …), so the MOST COMMON test shape — an integration
    /// test `def test_foo()` in a `tests/` file — carries is_test=0. The raw
    /// `ns.is_test = 0 AND nt.is_test = 0` filter let that `test_foo → foo` edge leak
    /// in as a "surprising" coupling (it is the expected coupling). Now excluded via
    /// the name/path heuristic. Asserts both the `test_`-name leg and the `tests/`-path
    /// leg, source-side and target-side.
    #[test]
    fn surprising_connections_excludes_name_path_tests_without_flag() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('tests/test_api.py','h1',0,'python',0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/api.py','h2',0,'python',0)", []).unwrap();
        // Source: test_-named, is_test flag NOT set (0) — the sibling-hole case.
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (1,'function','test_signup',1,2,'',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (2,'function','handle_signup',1,2,'',0)", []).unwrap();
        conn.execute("INSERT INTO edges (source_id,target_id,relation,confidence) VALUES (1,2,'calls','inferred')", []).unwrap();
        assert!(
            surprising_connections(conn, false, 10).unwrap().is_empty(),
            "test_-named source (is_test=0) in tests/ must be excluded by default"
        );
        assert_eq!(
            surprising_connections(conn, true, 10).unwrap().len(),
            1,
            "included with include_tests"
        );
    }

    #[test]
    fn surprising_connections_excludes_module_scope_nodes() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.py','h1',0,'python',0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('lib/b.py','h2',0,'python',0)", []).unwrap();
        // A top-level call is attributed to the synthetic `<module>` scope node
        // (feedback_top_level_call_scope). It is not an actionable symbol, and
        // project_map / dead_code / module_exports all filter `<module>` — surprising
        // must too, or `<module> → target` leaks into the coupling-review output.
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (1,'module','<module>',0,0,'',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (2,'function','targetB',1,2,'',0)", []).unwrap();
        conn.execute("INSERT INTO edges (source_id,target_id,relation,confidence) VALUES (1,2,'calls','inferred')", []).unwrap();

        assert!(
            surprising_connections(conn, false, 10).unwrap().is_empty(),
            "a coupling whose source is the synthetic <module> scope node must be excluded"
        );
    }

    fn inp(src: &str, sf: &str, tgt: &str, tf: &str, conf: &str) -> SurpriseInput {
        SurpriseInput {
            source: src.into(),
            source_file: sf.into(),
            target: tgt.into(),
            target_file: tf.into(),
            relation: "calls".into(),
            confidence: conf.into(),
            type_call: false,
        }
    }

    fn pairs(r: &[SurprisingConnection]) -> Vec<(&str, &str)> {
        r.iter()
            .map(|c| (c.source.as_str(), c.target.as_str()))
            .collect()
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(find_surprising(&[], 10).is_empty());
    }

    #[test]
    fn confidence_drives_ranking_within_same_structure() {
        // All same-dir (src/) cross-file edges → only confidence differs.
        let edges = [
            inp("a", "src/a.ts", "b", "src/b.ts", "inferred"),
            inp("c", "src/c.ts", "d", "src/d.ts", "ambiguous"),
            inp("e", "src/e.ts", "f", "src/f.ts", "extracted"),
        ];
        let r = find_surprising(&edges, 10);
        assert_eq!(
            pairs(&r),
            [("c", "d"), ("a", "b"), ("e", "f")],
            "ambiguous > inferred > extracted"
        );
    }

    #[test]
    fn cross_directory_outranks_same_directory() {
        // Two cross-dir edges in the SAME module-pair (not sole) vs one same-dir edge.
        let edges = [
            inp("s", "src/a.ts", "t", "src/b.ts", "inferred"), // same dir → 2
            inp("u", "src/a.ts", "v", "lib/x.ts", "inferred"), // src|lib
            inp("w", "src/c.ts", "x", "lib/y.ts", "inferred"), // src|lib (pair count 2, not sole)
        ];
        let r = find_surprising(&edges, 10);
        assert_eq!(r[0].score, 4, "cross-dir non-sole = inferred(2) + cross(2)");
        assert_eq!(r[1].score, 4);
        assert_eq!(
            (r[2].source.as_str(), r[2].score),
            ("s", 2),
            "same-dir edge ranks last"
        );
    }

    #[test]
    fn sole_bridge_between_modules_scores_highest() {
        let edges = [
            inp("a", "src/a.ts", "b", "lib/x.ts", "inferred"), // sole src|lib → 2+2+2 = 6
            inp("c", "src/c.ts", "d", "util/p.ts", "inferred"), // src|util #1
            inp("e", "src/e.ts", "f", "util/q.ts", "inferred"), // src|util #2 (count 2) → 4
        ];
        let r = find_surprising(&edges, 10);
        assert_eq!(
            (r[0].source.as_str(), r[0].score),
            ("a", 6),
            "sole bridge ranks first"
        );
    }

    #[test]
    fn reasons_explain_each_component() {
        // ambiguous + cross-module + sole bridge = 3 + 2 + 2 = 7.
        let edges = [inp("a", "src/a.ts", "b", "lib/x.ts", "ambiguous")];
        let r = find_surprising(&edges, 10);
        assert_eq!(r[0].score, 7);
        let reasons = r[0].reasons.join(" | ").to_lowercase();
        assert!(
            reasons.contains("ambiguous"),
            "explains confidence; got: {reasons}"
        );
        assert!(
            reasons.contains("module"),
            "explains cross-module; got: {reasons}"
        );
        assert!(
            reasons.contains("sole"),
            "explains sole bridge; got: {reasons}"
        );
    }

    // D#520 (review of 0.175.0, LOW 11): only a call into a TypeScript
    // `interface` or `type` alias is said to be one; a Python call with several
    // candidates, or a call into a Java interface, is not.
    #[test]
    fn the_ambiguous_reason_names_a_typescript_type_only_for_a_call_into_one() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        for (path, lang) in [
            ("src/a.py", "python"),
            ("lib/b.py", "python"),
            ("src/c.ts", "typescript"),
            ("lib/types.ts", "typescript"),
            ("src/D.java", "java"),
            ("lib/Svc.java", "java"),
        ] {
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES (?1,'h',0,?2,0)",
                [path, lang],
            )
            .unwrap();
        }
        for (file, ty, name) in [
            (1, "function", "go"),
            (2, "function", "run"),
            (3, "function", "make"),
            (4, "interface", "Response"),
            (5, "method", "call"),
            (6, "interface", "Svc"),
            (4, "function", "build"),
        ] {
            conn.execute(
                "INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content,is_test) VALUES (?1,?2,?3,1,2,'',0)",
                rusqlite::params![file, ty, name],
            )
            .unwrap();
        }
        for (s, t) in [(1, 2), (3, 4), (5, 6), (3, 7)] {
            conn.execute(
                "INSERT INTO edges (source_id,target_id,relation,confidence) VALUES (?1,?2,'calls','ambiguous')",
                [s, t],
            )
            .unwrap();
        }
        let r = surprising_connections(conn, false, 10).unwrap();
        let reason = |target: &str| {
            r.iter()
                .find(|c| c.target == target)
                .map(|c| c.reasons[0].clone())
                .unwrap()
        };
        assert_eq!(
            reason("run"),
            "ambiguous: bare-name target not decided (several candidates)"
        );
        assert_eq!(
            reason("Svc"),
            "ambiguous: bare-name target not decided (several candidates)"
        );
        assert_eq!(
            reason("build"),
            "ambiguous: bare-name target not decided (several candidates)"
        );
        assert_eq!(
            reason("Response"),
            "ambiguous: a call into a TypeScript type, which exists only at compile time"
        );
    }

    #[test]
    fn top_n_truncates() {
        let edges = [
            inp("a", "src/a.ts", "b", "lib/x.ts", "ambiguous"),
            inp("c", "src/c.ts", "d", "src/d.ts", "inferred"),
            inp("e", "src/e.ts", "f", "src/f.ts", "extracted"),
        ];
        assert_eq!(find_surprising(&edges, 2).len(), 2);
    }

    #[test]
    fn output_is_deterministic() {
        let edges = [
            inp("a", "src/a.ts", "b", "src/b.ts", "inferred"),
            inp("c", "src/c.ts", "d", "src/d.ts", "inferred"),
        ];
        assert_eq!(find_surprising(&edges, 10), find_surprising(&edges, 10));
    }
}
