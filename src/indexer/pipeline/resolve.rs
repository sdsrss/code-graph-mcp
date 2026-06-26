//! Cross-file call resolution helpers shared by the main `index_files` walk
//! and the post-index `pending_unresolved_calls` sweep.
//!
//! - `refine_ambiguous_targets`: disambiguator — when a call's target name
//!   matches N same-language nodes across files, prefer non-test paths and
//!   the longest common path prefix with the caller.
//! - `resolve_pending_calls`: drains buffered same-language-but-callee-not-yet-
//!   indexed rows once the callee appears (post-incremental sweep).

use anyhow::Result;
use std::collections::HashMap;

use crate::storage::db::Database;
use crate::storage::queries::{
    delete_pending_unresolved_call, get_node_paths_by_ids, insert_edge_cached,
    list_pending_unresolved_calls,
};
use crate::domain::REL_CALLS;

/// Decoded form of `edges.metadata` for REL_CALLS rows. See
/// `docs/superpowers/specs/2026-05-11-bare-name-call-qualifier-design.md`
/// §"Wire protocol" for the JSON shapes this parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CalleeMeta {
    Path(Vec<String>),
    SelfType(String),
    SelfRecv(String),
    Receiver(String),
    Chain,
}

/// Parse a `{"q":"...", "v":"..."}` JSON metadata blob. Returns None for
/// metadata produced by other relations (routes, python imports), absent
/// metadata, or unrecognized `q` values.
pub(super) fn parse_callee_metadata(s: Option<&str>) -> Option<CalleeMeta> {
    let raw = s?;
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let q = v.get("q")?.as_str()?;
    match q {
        "chain" => Some(CalleeMeta::Chain),
        "path" => {
            let payload = v.get("v")?.as_str()?;
            let segments: Vec<String> = payload.split("::").map(String::from).collect();
            if segments.is_empty() || segments.iter().any(|s| s.is_empty()) {
                None
            } else {
                Some(CalleeMeta::Path(segments))
            }
        }
        "self" => v.get("v")?.as_str().map(|t| CalleeMeta::SelfRecv(t.to_string())),
        "stype" => v.get("v")?.as_str().map(|t| CalleeMeta::SelfType(t.to_string())),
        "recv" => v.get("v")?.as_str().map(|r| CalleeMeta::Receiver(r.to_string())),
        _ => None,
    }
}

/// Disambiguate N same-language cross-file candidates for a single call/import
/// target. Returns a subset. A single-element result is the authoritative
/// winner; ties fall back to the full input so the caller does not
/// inadvertently drop legitimate edges.
///
/// Heuristic: (1) prefer non-test-file candidates when the caller is not
/// itself a test file; (2) among the preferred pool, keep only those tied
/// for the longest byte-common path prefix with the caller. Previous
/// versions dropped on ambiguity, which regressed dead-code detection for
/// bare-name Rust calls like `crate::domain::foo()` where scoped_identifier
/// extraction keeps only `foo` and two `foo` definitions under `src/` tie
/// on prefix — better to keep both edges than to report `foo` as dead.
pub(super) fn refine_ambiguous_targets(
    candidates: &[i64],
    caller_rel_path: &str,
    node_id_to_path: &HashMap<i64, String>,
) -> Vec<i64> {
    if candidates.len() <= 1 {
        return candidates.to_vec();
    }

    // NOTE: deliberately divergent local copy — biases ambiguous-target resolution by
    // substring (.test. / .spec. / _test. / mid-path /tests/), broader than the
    // suffix/prefix rules in `domain::is_test_path`. Kept separate on purpose; see the
    // "Five sites must agree" note in domain.rs (feedback_test_classifier_dual_sources.md).
    let is_test_path = |p: &str| {
        p.contains(".test.") || p.contains("_test.")
            || p.starts_with("tests/") || p.contains("/tests/")
            || p.starts_with("test/") || p.contains("/test/")
            || p.contains(".spec.")
    };
    let caller_is_test = is_test_path(caller_rel_path);

    // Pass 1: prefer non-test candidates when the caller is non-test code.
    let pool: Vec<i64> = if caller_is_test {
        candidates.to_vec()
    } else {
        let non_test: Vec<i64> = candidates.iter().copied()
            .filter(|id| {
                let p = node_id_to_path.get(id).map(String::as_str).unwrap_or("");
                !is_test_path(p)
            })
            .collect();
        if non_test.is_empty() { candidates.to_vec() } else { non_test }
    };

    if pool.len() == 1 { return pool; }

    // Pass 2: keep only candidates tied for the longest common path prefix
    // with the caller. Byte-wise prefix is a rough proxy for module locality
    // — e.g. `claude-plugin/scripts/session-init.js` shares 21 bytes with
    // `claude-plugin/scripts/lifecycle.js` but 0 bytes with `scripts/*`.
    let prefix_len = |p: &str| -> usize {
        caller_rel_path.bytes().zip(p.bytes())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let max_prefix = pool.iter()
        .map(|id| prefix_len(node_id_to_path.get(id).map(String::as_str).unwrap_or("")))
        .max()
        .unwrap_or(0);
    let closest: Vec<i64> = pool.iter().copied()
        .filter(|id| prefix_len(node_id_to_path.get(id).map(String::as_str).unwrap_or("")) == max_prefix)
        .collect();

    if closest.len() == 1 { return closest; }

    // Still ambiguous — return the remaining pool rather than dropping. This
    // keeps dead-code precision high for edges we cannot confidently prune
    // (most notably Rust bare-name scoped calls) at the cost of leaving a
    // small amount of fan-out; the single-winner fast path above handles
    // the common case (unique non-test match, or unique closest path).
    if !closest.is_empty() { closest } else { pool }
}

/// Sweep `pending_unresolved_calls` against the current node state. Rows whose
/// `(target_name, source_language)` now match a real node become a `calls`
/// edge and the pending row is dropped; rows that still don't resolve stay
/// buffered for the next index pass.
///
/// Resolution priority mirrors Phase 2: same-language candidates only (no
/// cross-language promotion — memory `feedback_edge_resolution_same_language.md`
/// flags that as the canonical false-positive class), with
/// `refine_ambiguous_targets` applied when multiple candidates share the name.
///
/// Returns the number of edges inserted by this sweep.
pub(super) fn resolve_pending_calls(db: &Database) -> Result<usize> {
    let pending = list_pending_unresolved_calls(db.conn())?;
    if pending.is_empty() {
        return Ok(0);
    }

    // Build name → [(node_id, language)] map ONCE, then iterate pending rows
    // in memory. Narrowed by `n.name IN (SELECT DISTINCT target_name ...)` so
    // even a 1-row pending table doesn't trigger a full nodes-table scan on
    // every incremental pass — for a 100K-node project the unfiltered SELECT
    // was 100K rows × every index call, even with no work to do.
    let mut name_to_lang_targets: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mut node_id_to_path: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = db.conn().prepare(
            "SELECT n.id, n.name, COALESCE(f.language, ''), f.path
             FROM nodes n JOIN files f ON f.id = n.file_id
             WHERE f.language IS NOT NULL
               AND n.name IN (SELECT DISTINCT target_name FROM pending_unresolved_calls)"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, name, lang, path) = row?;
            if lang.is_empty() {
                continue;
            }
            name_to_lang_targets.entry(name).or_default().push((id, lang));
            node_id_to_path.insert(id, path);
        }
    }

    // Map source_id → source file path so refine_ambiguous_targets gets the
    // proximity hint it needs. Dedupe first — pending rows repeat source_ids;
    // batch via get_node_paths_by_ids to stay under SQLite's bind limit.
    let mut source_ids: Vec<i64> = pending.iter().map(|p| p.source_id).collect();
    source_ids.sort_unstable();
    source_ids.dedup();
    let source_id_to_path = get_node_paths_by_ids(db.conn(), &source_ids)?;

    let mut edges_added = 0usize;
    let mut to_delete: Vec<i64> = Vec::new();

    for row in &pending {
        let candidates: Vec<i64> = name_to_lang_targets.get(&row.target_name)
            .map(|entries| entries.iter()
                .filter(|(_, lang)| *lang == row.source_language)
                .map(|(id, _)| *id)
                .filter(|id| *id != row.source_id) // self-call guard
                .collect())
            .unwrap_or_default();

        if candidates.is_empty() {
            continue; // still unresolvable — leave buffered
        }

        let refined = if candidates.len() > 1 {
            let source_path = source_id_to_path.get(&row.source_id).cloned().unwrap_or_default();
            refine_ambiguous_targets(&candidates, &source_path, &node_id_to_path)
        } else {
            candidates
        };

        for tgt_id in &refined {
            if insert_edge_cached(
                db.conn(),
                row.source_id,
                *tgt_id,
                REL_CALLS,
                row.metadata.as_deref(),
            )? {
                edges_added += 1;
            }
        }
        to_delete.push(row.id);
    }

    for id in to_delete {
        delete_pending_unresolved_call(db.conn(), id)?;
    }

    Ok(edges_added)
}

/// Positively resolve bare-name `calls` edges to the node an explicit import in
/// the caller's file binds them to. Runs once after all call edges exist (post
/// Phase-2 + pending sweep), immediately before
/// `prune_import_contradicted_call_edges`.
///
/// Motivation: `refine_ambiguous_targets` resolves a bare call to the
/// path-closest same-name node, which can be the WRONG file when the caller
/// explicitly `from X import name`s a farther one. The prune then deletes that
/// wrong edge — but the language's scoping rules say the call resolves to the
/// IMPORTED node, and path-proximity never produced that edge, so the call was
/// left with no edge at all (`feedback_bare_name_call_qualifier`,
/// `feedback_edge_resolution_same_language`). This inserts the import-bound edge
/// so bind + prune together repoint the call: insert correct, drop wrong.
///
/// Conservative by construction (mirrors the prune's guards):
/// - only bare-name call edges (NULL/empty metadata) are eligible — qualified
///   calls (`cache.save()`, `crate::x::foo()`) carry their own resolution;
/// - the import must bind the name to exactly ONE internal node in the caller's
///   file (ambiguous suffix-imports / `<external>` targets are skipped — never
///   creates an edge into the external sentinel);
/// - a file-local definition of the name shadows the import — skip, the
///   same-file tier already resolved it;
/// - idempotent: `insert_edge_cached` is INSERT OR IGNORE, so a call that
///   already resolved to the imported node is a no-op (e.g. JS, whose import
///   edges resolve by proximity, agrees with the call and gains nothing here).
///
/// Returns the number of edges inserted.
pub(super) fn bind_calls_to_imported_targets(db: &Database) -> Result<usize> {
    use crate::domain::{REL_CALLS, REL_IMPORTS};

    // Collect (caller_fn_id, imported_target_id) pairs to bind. A bare call
    // whose name is bound by a unique internal import in the caller's file —
    // and is NOT shadowed by a same-file definition of that name — resolves to
    // the imported node.
    let pairs: Vec<(i64, i64)> = {
        let mut stmt = db.conn().prepare(
            "SELECT DISTINCT c.source_id, it.import_target_id
             FROM (
                 SELECT e.source_id AS source_id, tn.name AS call_name,
                        sn.file_id AS caller_file
                 FROM edges e
                 JOIN nodes sn ON sn.id = e.source_id
                 JOIN nodes tn ON tn.id = e.target_id
                 WHERE e.relation = ?1
                   AND (e.metadata IS NULL OR e.metadata = '')
             ) c
             JOIN (
                 SELECT mn.file_id AS import_file, itn.name AS import_name,
                        MIN(itn.id) AS import_target_id
                 FROM edges ie
                 JOIN nodes mn  ON mn.id  = ie.source_id
                 JOIN nodes itn ON itn.id = ie.target_id
                 JOIN files itf ON itf.id = itn.file_id
                 WHERE ie.relation = ?2
                   AND itf.path <> '<external>'
                 GROUP BY mn.file_id, itn.name
                 HAVING COUNT(DISTINCT itn.id) = 1
             ) it
               ON it.import_file = c.caller_file
              AND it.import_name = c.call_name
             WHERE c.source_id <> it.import_target_id
               AND NOT EXISTS (
                   SELECT 1 FROM nodes ln
                   WHERE ln.file_id = c.caller_file AND ln.name = c.call_name
               )",
        )?;
        let rows = stmt.query_map(rusqlite::params![REL_CALLS, REL_IMPORTS], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.filter_map(std::result::Result::ok).collect()
    };

    let mut inserted = 0usize;
    for (src, tgt) in pairs {
        if insert_edge_cached(db.conn(), src, tgt, REL_CALLS, None)? {
            inserted += 1;
        }
    }
    Ok(inserted)
}

/// Remove bare-name `calls` edges that an explicit import in the caller's file
/// contradicts. Runs once after all call edges exist (post Phase-2 + pending
/// sweep).
///
/// Motivation: when a caller makes a bare call `save()` whose name matches
/// several same-language nodes across files, `refine_ambiguous_targets`
/// deliberately keeps every tied candidate rather than drop (so Rust
/// `crate::domain::foo()` scoped calls with no disambiguating info don't get
/// reported as dead). But when the caller's file carries an `imports` edge that
/// binds that exact name to ONE specific node, the language's scoping rules say
/// the bare call resolves to the imported node — every other same-name target is
/// a false caller that inflates impact/call-graph and pollutes dead-code
/// (`feedback_edge_resolution_same_language.md`). This prunes only those
/// import-contradicted edges, which removes false positives while keeping the
/// correct edge — so it never regresses dead-code (the imported target stays
/// linked) and never fires for the no-import tie case the tie-keeping protects.
///
/// Conservative by construction:
/// - only bare-name edges (NULL/empty metadata) are eligible — qualified calls
///   (`cache.save()`, `crate::x::foo()`) carry receiver/path metadata and are
///   left alone, so an explicit cross-module call is never pruned;
/// - same-file targets are never pruned (a local `def save` shadows an import
///   and is authoritative — resolved by the same-file tier);
/// - an edge is pruned only when the caller's file imports the SAME NAME bound
///   to a DIFFERENT node AND does not import this target.
///
/// Returns the number of edges removed.
pub(super) fn prune_import_contradicted_call_edges(db: &Database) -> Result<usize> {
    use crate::domain::{REL_CALLS, REL_IMPORTS};
    let removed = db.conn().execute(
        "DELETE FROM edges WHERE id IN (
            SELECT e.id FROM edges e
            JOIN nodes sn ON sn.id = e.source_id
            JOIN nodes tn ON tn.id = e.target_id
            WHERE e.relation = ?1
              AND (e.metadata IS NULL OR e.metadata = '')
              AND tn.file_id <> sn.file_id
              -- Don't false-prune a real qualified call. A `module.func()` /
              -- `obj.method()` call can be extracted WITHOUT receiver metadata
              -- (e.g. Python `cache.save()` lands as a bare NULL-metadata row) and
              -- then dedups into the same edge as the bare import-resolved call.
              -- If the caller's source contains a `.<name>(` qualified call, this
              -- edge may legitimately bind to a different same-name target, so keep
              -- it — biasing toward keeping edges (the safe direction). Verified by
              -- `test_cli_callgraph_prune_keeps_qualified_call_to_same_name`.
              AND instr(sn.code_content, '.' || tn.name || '(') = 0
              -- caller's file imports the SAME name bound to a DIFFERENT node
              AND EXISTS (
                  SELECT 1 FROM edges ie
                  JOIN nodes mn  ON mn.id  = ie.source_id
                  JOIN nodes itn ON itn.id = ie.target_id
                  WHERE ie.relation = ?2
                    AND mn.file_id = sn.file_id
                    AND itn.name = tn.name
                    AND ie.target_id <> e.target_id
              )
              -- ... and does NOT import THIS target (so it is contradicted)
              AND NOT EXISTS (
                  SELECT 1 FROM edges ie2
                  JOIN nodes mn2 ON mn2.id = ie2.source_id
                  WHERE ie2.relation = ?2
                    AND mn2.file_id = sn.file_id
                    AND ie2.target_id = e.target_id
              )
        )",
        rusqlite::params![REL_CALLS, REL_IMPORTS],
    )?;
    Ok(removed)
}

/// Phase 2e: assign `edges.confidence` for cross-file `calls`/`references` edges
/// in one set-based pass, run after all edges exist (post Phase-2 + pending sweep
/// + prune). Purely additive metadata — no edge is added or removed.
///
/// The column defaults to `extracted`, so every precise insert (same-file
/// resolution + the structural relations imports/inherits/implements/routes_to/
/// exports) is correct without touching its insert site. This pass only
/// DOWNGRADES cross-file `calls`/`references` edges — the by-name-resolved class:
///   - `inferred`  when the target name is unique among same-language nodes;
///   - `ambiguous` when >1 same-language node shares the target name (the
///     by-name resolution could not pick uniquely — the known false-positive
///     class: bare_name_call_qualifier / method_call_edge_drops /
///     value_reference_candidate_gen).
///
/// Idempotent: re-running recomputes from current node state, so an `inferred`
/// edge becomes `ambiguous` when a duplicate-named sibling is later added (and
/// back when it is removed). Returns the number of edges downgraded.
pub(super) fn classify_edge_confidence(db: &Database) -> Result<usize> {
    use crate::domain::{CONF_AMBIGUOUS, CONF_INFERRED, REL_CALLS, REL_REFERENCES};
    let downgraded = db.conn().execute(
        "UPDATE edges
         SET confidence = CASE WHEN namecount.cnt > 1 THEN ?3 ELSE ?4 END
         FROM nodes AS src, nodes AS tgt, files AS tf,
              (SELECT n.name AS nm, f.language AS lang, COUNT(*) AS cnt
               FROM nodes n JOIN files f ON f.id = n.file_id
               GROUP BY n.name, f.language) AS namecount
         WHERE edges.source_id = src.id
           AND edges.target_id = tgt.id
           AND tf.id = tgt.file_id
           AND src.file_id <> tgt.file_id
           AND edges.relation IN (?1, ?2)
           AND namecount.nm = tgt.name
           AND namecount.lang IS tf.language",
        rusqlite::params![REL_CALLS, REL_REFERENCES, CONF_AMBIGUOUS, CONF_INFERRED],
    )?;
    Ok(downgraded)
}

/// Filter a candidate set down to those matching the Path qualifier:
///   (1) file path contains "/seg1/seg2/" OR starts with "seg1/seg2/", OR
///   (2) qualified_name contains the segment chain joined by `.` as a
///       contiguous segment (anchored on `.` or boundary).
///
/// Storage uses `.` separator for qualified_name (treesitter.rs:582), NOT `::`.
/// Returns the filtered subset; empty result is a meaningful signal
/// (no project candidate matches → caller should drop the edge).
pub(super) fn path_filter_candidates(
    segments: &[String],
    candidates: &[i64],
    node_id_to_path: &std::collections::HashMap<i64, String>,
    db: &crate::storage::db::Database,
) -> anyhow::Result<Vec<i64>> {
    if candidates.is_empty() || segments.is_empty() {
        return Ok(candidates.to_vec());
    }
    let path_chain = segments.join("/");
    let qn_chain = segments.join(".");

    let placeholders: String = std::iter::repeat_n("?", candidates.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, COALESCE(qualified_name, '') FROM nodes WHERE id IN ({})",
        placeholders
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = candidates.iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut id_to_qn: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    for r in rows {
        let (id, qn) = r?;
        id_to_qn.insert(id, qn);
    }

    // For the LAST segment, also accept the path ending in `<seg>.rs` —
    // Rust commonly puts single-file mods at `src/<mod>.rs` (e.g. `src/domain.rs`
    // for `crate::domain::*`), which has no `/domain/` directory boundary the
    // directory-style check below would catch. Without this, every
    // `crate::domain::foo()` call drops on the floor and `domain::foo` looks dead.
    let last_seg = segments.last().cloned().unwrap_or_default();
    let single_file_suffix = if !last_seg.is_empty() {
        Some(format!("/{}.rs", last_seg))
    } else {
        None
    };

    let kept: Vec<i64> = candidates.iter().copied().filter(|id| {
        let path = node_id_to_path.get(id).map(String::as_str).unwrap_or("");
        let qn = id_to_qn.get(id).map(String::as_str).unwrap_or("");

        let path_match = path.contains(&format!("/{}/", path_chain))
            || path.starts_with(&format!("{}/", path_chain))
            || single_file_suffix.as_deref().is_some_and(|sfx| path.ends_with(sfx));

        let qn_match = qn == qn_chain
            || qn.starts_with(&format!("{}.", qn_chain))
            || qn.contains(&format!(".{}.", qn_chain))
            || qn.ends_with(&format!(".{}", qn_chain));

        path_match || qn_match
    }).collect();
    Ok(kept)
}

/// Filter candidates to those whose `qualified_name` belongs to `impl_type`
/// (i.e. is a method of the named type). Storage encodes this as `Type.method`
/// with `.` separator (treesitter.rs qualified_name assignment).
///
/// Not file-restricted — Rust allows `impl Type {}` blocks to span multiple
/// files (e.g. `impl Database` is split across 3+ files in this repo), so we
/// match by `qualified_name LIKE 'Type.%'` across all files.
pub(super) fn self_filter_candidates(
    impl_type: &str,
    candidates: &[i64],
    db: &crate::storage::db::Database,
) -> anyhow::Result<Vec<i64>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = std::iter::repeat_n("?", candidates.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id FROM nodes
         WHERE id IN ({})
           AND qualified_name LIKE ? || '.%'",
        placeholders
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = candidates
        .iter()
        .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
        .collect();
    params.push(Box::new(impl_type.to_string()));
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
    let kept: Vec<i64> = rows.filter_map(|r| r.ok()).collect();
    Ok(kept)
}

/// Filter candidates to those whose `qualified_name` denotes a METHOD — i.e.
/// contains a `.` separator (`Type.method`), as opposed to a free function
/// whose `qualified_name` equals its bare name. A receiver call `obj.method()`
/// can only bind to a method, never a free function, so this is the gate the
/// receiver-resolution arm uses to exclude same-named free functions before
/// deciding whether a unique target exists.
///
/// Storage encodes methods as `Type.method` (treesitter.rs qualified_name
/// assignment) and free functions as just `name`.
pub(super) fn method_candidates(
    candidates: &[i64],
    db: &crate::storage::db::Database,
) -> anyhow::Result<Vec<i64>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = std::iter::repeat_n("?", candidates.len())
        .collect::<Vec<_>>()
        .join(",");
    // qualified_name LIKE '%.%' — any node whose qualified_name carries a
    // `Type.` prefix. NULL qualified_name (rare) is excluded by LIKE.
    let sql = format!(
        "SELECT id FROM nodes WHERE id IN ({}) AND qualified_name LIKE '%.%'",
        placeholders
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let params: Vec<&dyn rusqlite::ToSql> = candidates
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, i64>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_metadata_bare_returns_none() {
        assert!(parse_callee_metadata(None).is_none());
    }

    #[test]
    fn parse_metadata_path() {
        let m = parse_callee_metadata(Some(r#"{"q":"path","v":"snapshot"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::Path(ref segs) if segs == &["snapshot"]));
    }

    #[test]
    fn parse_metadata_path_multi_segment() {
        let m = parse_callee_metadata(Some(r#"{"q":"path","v":"a::b::c"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::Path(ref segs) if segs == &["a", "b", "c"]));
    }

    #[test]
    fn parse_metadata_self_recv() {
        let m = parse_callee_metadata(Some(r#"{"q":"self","v":"Db"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::SelfRecv(ref t) if t == "Db"));
    }

    #[test]
    fn parse_metadata_self_type() {
        let m = parse_callee_metadata(Some(r#"{"q":"stype","v":"Db"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::SelfType(ref t) if t == "Db"));
    }

    #[test]
    fn parse_metadata_recv() {
        let m = parse_callee_metadata(Some(r#"{"q":"recv","v":"path"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::Receiver(ref r) if r == "path"));
    }

    #[test]
    fn parse_metadata_chain() {
        let m = parse_callee_metadata(Some(r#"{"q":"chain"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::Chain));
    }

    #[test]
    fn parse_metadata_routes_or_python_imports_returns_none() {
        // Other relations also use metadata; resolver should skip non-call shapes.
        assert!(parse_callee_metadata(Some(r#"{"method":"GET","path":"/api"}"#)).is_none());
        assert!(parse_callee_metadata(Some(r#"{"python_module":"foo","is_module_import":false}"#)).is_none());
    }

    mod confidence {
        use super::*;
        use crate::domain::{REL_CALLS, REL_IMPORTS, REL_REFERENCES};
        use crate::storage::db::Database;
        use crate::storage::queries::{insert_edge, insert_node, upsert_file, FileRecord, NodeRecord};
        use tempfile::TempDir;

        fn node(name: &str, file_id: i64) -> NodeRecord {
            NodeRecord {
                file_id, node_type: "function".into(), name: name.into(),
                qualified_name: None, start_line: 1, end_line: 5,
                code_content: format!("function {name}() {{}}"), signature: None,
                doc_comment: None, context_string: None, name_tokens: None,
                return_type: None, param_types: None, is_test: false,
            }
        }
        fn file(conn: &rusqlite::Connection, path: &str, lang: &str) -> i64 {
            upsert_file(conn, &FileRecord {
                path: path.into(), blake3_hash: format!("h-{path}"),
                last_modified: 1, language: Some(lang.into()),
            }).unwrap()
        }
        fn conf_of(conn: &rusqlite::Connection, s: i64, t: i64, rel: &str) -> String {
            conn.query_row(
                "SELECT confidence FROM edges WHERE source_id=?1 AND target_id=?2 AND relation=?3",
                rusqlite::params![s, t, rel], |r| r.get(0),
            ).unwrap()
        }

        /// Same-file → extracted; cross-file unique by-name → inferred;
        /// cross-file with a same-language duplicate name → ambiguous; non-calls
        /// relation (imports) stays extracted even cross-file; references behave
        /// like calls.
        #[test]
        fn classify_splits_extracted_inferred_ambiguous() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("c.db")).unwrap();
            let conn = db.conn();
            let f1 = file(conn, "src/a.ts", "typescript");
            let f2 = file(conn, "src/b.ts", "typescript");
            let f3 = file(conn, "src/c.ts", "typescript");

            // same-file call A->B
            let a = insert_node(conn, &node("A", f1)).unwrap();
            let b = insert_node(conn, &node("B", f1)).unwrap();
            insert_edge(conn, a, b, REL_CALLS, None).unwrap();

            // cross-file unique call C(f1)->D(f2)
            let c = insert_node(conn, &node("C", f1)).unwrap();
            let d = insert_node(conn, &node("D", f2)).unwrap();
            insert_edge(conn, c, d, REL_CALLS, None).unwrap();

            // cross-file ambiguous call E(f1)->F(f2), with a duplicate F in f3 (same lang)
            let e = insert_node(conn, &node("E", f1)).unwrap();
            let f_target = insert_node(conn, &node("F", f2)).unwrap();
            insert_node(conn, &node("F", f3)).unwrap(); // duplicate same-language name
            insert_edge(conn, e, f_target, REL_CALLS, None).unwrap();

            // cross-file imports G(f1)->H(f2) — must stay extracted (structural)
            let g = insert_node(conn, &node("G", f1)).unwrap();
            let h = insert_node(conn, &node("H", f2)).unwrap();
            insert_edge(conn, g, h, REL_IMPORTS, None).unwrap();

            // cross-file unique references I(f1)->J(f2) — inferred like calls
            let i = insert_node(conn, &node("I", f1)).unwrap();
            let j = insert_node(conn, &node("J", f2)).unwrap();
            insert_edge(conn, i, j, REL_REFERENCES, None).unwrap();

            let downgraded = classify_edge_confidence(&db).unwrap();
            assert_eq!(downgraded, 3, "3 cross-file calls/refs edges downgraded (C->D, E->F, I->J)");

            assert_eq!(conf_of(conn, a, b, REL_CALLS), "extracted", "same-file call stays extracted");
            assert_eq!(conf_of(conn, c, d, REL_CALLS), "inferred", "cross-file unique name → inferred");
            assert_eq!(conf_of(conn, e, f_target, REL_CALLS), "ambiguous", "cross-file duplicate name → ambiguous");
            assert_eq!(conf_of(conn, g, h, REL_IMPORTS), "extracted", "imports stays extracted cross-file");
            assert_eq!(conf_of(conn, i, j, REL_REFERENCES), "inferred", "cross-file unique reference → inferred");
        }

        /// Idempotency: removing the duplicate flips ambiguous→inferred on re-run;
        /// re-running without change is stable.
        #[test]
        fn classify_is_idempotent_and_recomputes() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("c.db")).unwrap();
            let conn = db.conn();
            let f1 = file(conn, "src/a.ts", "typescript");
            let f2 = file(conn, "src/b.ts", "typescript");
            let f3 = file(conn, "src/c.ts", "typescript");

            let e = insert_node(conn, &node("E", f1)).unwrap();
            let f_target = insert_node(conn, &node("F", f2)).unwrap();
            let dup = insert_node(conn, &node("F", f3)).unwrap();
            insert_edge(conn, e, f_target, REL_CALLS, None).unwrap();

            classify_edge_confidence(&db).unwrap();
            assert_eq!(conf_of(conn, e, f_target, REL_CALLS), "ambiguous");
            // re-run, no change → still ambiguous (stable)
            classify_edge_confidence(&db).unwrap();
            assert_eq!(conf_of(conn, e, f_target, REL_CALLS), "ambiguous");

            // remove the duplicate node → name now unique → inferred on re-run
            conn.execute("DELETE FROM nodes WHERE id=?1", [dup]).unwrap();
            classify_edge_confidence(&db).unwrap();
            assert_eq!(conf_of(conn, e, f_target, REL_CALLS), "inferred",
                "removing the duplicate must flip ambiguous→inferred");
        }
    }
}
