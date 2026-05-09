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
    delete_pending_unresolved_call, insert_edge_cached, list_pending_unresolved_calls,
};
use crate::domain::REL_CALLS;

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
    // proximity hint it needs.
    let source_ids: Vec<i64> = pending.iter().map(|p| p.source_id).collect();
    let mut source_id_to_path: HashMap<i64, String> = HashMap::new();
    if !source_ids.is_empty() {
        let placeholders = std::iter::repeat_n("?", source_ids.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id WHERE n.id IN ({})",
            placeholders
        );
        let mut stmt = db.conn().prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = source_ids.iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, path) = row?;
            source_id_to_path.insert(id, path);
        }
    }

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
