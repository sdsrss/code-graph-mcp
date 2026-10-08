//! Cross-file call resolution helpers shared by the main `index_files` walk
//! and the post-index `pending_unresolved_calls` sweep.
//!
//! - `refine_ambiguous_targets`: disambiguator — when a call's target name
//!   matches N same-language nodes across files, prefer non-test paths and
//!   the longest common path prefix with the caller.
//! - `resolve_pending_calls`: drains buffered same-language-but-callee-not-yet-
//!   indexed rows once the callee appears (post-incremental sweep).

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::domain::REL_CALLS;
use crate::storage::db::Database;
use crate::storage::queries::{
    age_and_evict_pending_unresolved_calls, delete_pending_unresolved_call, filter_method_ids,
    get_node_paths_by_ids, get_node_qualified_names_by_ids, insert_edge_cached,
    list_pending_unresolved_calls,
};

/// Decoded form of `edges.metadata` for REL_CALLS rows. See
/// `docs/superpowers/specs/2026-05-11-bare-name-call-qualifier-design.md`
/// §"Wire protocol" for the JSON shapes this parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CalleeMeta {
    Path(Vec<String>),
    SelfType(String),
    SelfRecv(String),
    /// `recv.method()` where the source fixes `recv`'s class: a Python local
    /// constructor assignment or annotation, `self` / `this`, a C++ declared
    /// type, JS `new T()` / TS `: T` (parser `relations/receiver.rs`). Payload is
    /// the class, as a `::` path when written so. See [`recv_type_targets`].
    RecvType(String),
    /// `super().f()` (Python): the first base class, bound like `RecvType` but
    /// without its subclasses' overrides — `super()` names one implementation.
    SuperType(String),
    Receiver(String),
    Chain,
    /// `x.f()` on an object that is not `this`/`self` or a module binding
    /// (parser `relations/member.rs`): resolves like a bare call, minus the free
    /// functions, which no member call can reach.
    Member,
    /// C++ `x.f()` / `x->f()` on a field the caller's file never declares (an
    /// out-of-line member, a gtest body; parser `receiver::cpp_field_receiver`).
    /// Never resolved as such: [`CppFieldTypes::rewrite`] turns it into the
    /// `rtype` its recorded type names, else a `member` call, before the
    /// deferred pass.
    Field {
        class: String,
        field: String,
        arrow: bool,
    },
    /// C++ call on a receiver that is a chain of fields and method calls
    /// (`r->index_block.Add()`, `versions_->current()->Ref()`; parser
    /// `receiver::cpp_chain_receiver`). Rewritten like [`Self::Field`].
    Via,
    /// JS/TS `b()` through a renamed import (D#120, parser
    /// `member::js_renamed_import_call`), recorded as a call of the export.
    /// Binds only what [`js_import_targets`] finds in the file `module` names:
    /// never by name elsewhere, at batch time, in the deferred pass, or on the
    /// pending sweep.
    Import {
        module: String,
        export: String,
    },
    /// Python `m.f()` where `m` is bound by an absolute import of module `v`
    /// (`relations/member.rs`): no project code runs unless `v` is a project
    /// module ([`ProjectPythonModules`]); resolves like a bare call otherwise.
    Module(String),
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
        "via" => Some(CalleeMeta::Via),
        "field" => Some(CalleeMeta::Field {
            class: v.get("c")?.as_str()?.to_string(),
            field: v.get("v")?.as_str()?.to_string(),
            arrow: v.get("a").is_some(),
        }),
        "member" => Some(CalleeMeta::Member),
        "module" => v
            .get("v")?
            .as_str()
            .map(|m| CalleeMeta::Module(m.to_string())),
        "path" => {
            let payload = v.get("v")?.as_str()?;
            let segments: Vec<String> = payload.split("::").map(String::from).collect();
            if segments.is_empty() || segments.iter().any(|s| s.is_empty()) {
                None
            } else {
                Some(CalleeMeta::Path(segments))
            }
        }
        "self" => v
            .get("v")?
            .as_str()
            .map(|t| CalleeMeta::SelfRecv(t.to_string())),
        "stype" => v
            .get("v")?
            .as_str()
            .map(|t| CalleeMeta::SelfType(t.to_string())),
        "rtype" => v
            .get("v")?
            .as_str()
            .map(|t| CalleeMeta::RecvType(t.to_string())),
        "super" => v
            .get("v")?
            .as_str()
            .map(|t| CalleeMeta::SuperType(t.to_string())),
        "recv" => v
            .get("v")?
            .as_str()
            .map(|r| CalleeMeta::Receiver(r.to_string())),
        crate::domain::CALL_Q_IMPORT => Some(CalleeMeta::Import {
            module: v.get("js_module")?.as_str()?.to_string(),
            export: v.get("v")?.as_str()?.to_string(),
        }),
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
        p.contains(".test.")
            || p.contains("_test.")
            || p.starts_with("tests/")
            || p.contains("/tests/")
            || p.starts_with("test/")
            || p.contains("/test/")
            || p.contains(".spec.")
    };
    let caller_is_test = is_test_path(caller_rel_path);

    // Pass 1: prefer non-test candidates when the caller is non-test code.
    let pool: Vec<i64> = if caller_is_test {
        candidates.to_vec()
    } else {
        let non_test: Vec<i64> = candidates
            .iter()
            .copied()
            .filter(|id| {
                let p = node_id_to_path.get(id).map(String::as_str).unwrap_or("");
                !is_test_path(p)
            })
            .collect();
        if non_test.is_empty() {
            candidates.to_vec()
        } else {
            non_test
        }
    };

    if pool.len() == 1 {
        return pool;
    }

    // Pass 2: keep only candidates tied for the longest common path prefix
    // with the caller. Byte-wise prefix is a rough proxy for module locality
    // — e.g. `claude-plugin/scripts/session-init.js` shares 21 bytes with
    // `claude-plugin/scripts/lifecycle.js` but 0 bytes with `scripts/*`.
    let prefix_len = |p: &str| -> usize {
        caller_rel_path
            .bytes()
            .zip(p.bytes())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let max_prefix = pool
        .iter()
        .map(|id| prefix_len(node_id_to_path.get(id).map(String::as_str).unwrap_or("")))
        .max()
        .unwrap_or(0);
    let closest: Vec<i64> = pool
        .iter()
        .copied()
        .filter(|id| {
            prefix_len(node_id_to_path.get(id).map(String::as_str).unwrap_or("")) == max_prefix
        })
        .collect();

    if closest.len() == 1 {
        return closest;
    }

    // Still ambiguous — return the remaining pool rather than dropping. This
    // keeps dead-code precision high for edges we cannot confidently prune
    // (most notably Rust bare-name scoped calls) at the cost of leaving a
    // small amount of fan-out; the single-winner fast path above handles
    // the common case (unique non-test match, or unique closest path).
    if !closest.is_empty() {
        closest
    } else {
        pool
    }
}

/// The `pending_unresolved_calls.target_name` of a buffered call through a
/// renamed import (D#120). The table is unique on (caller, target name,
/// language), so the export's name alone let one caller's `m()` from './x' and
/// `v()` from './val' — both calls of `load` — keep a single row between them,
/// and whichever lost never bound. The sweep reads the call from its metadata,
/// so this name only has to be distinct per (specifier, export).
pub(super) fn js_import_pending_name(metadata: Option<&str>, target_name: &str) -> String {
    match parse_callee_metadata(metadata) {
        Some(CalleeMeta::Import { module, export }) => format!("{export}\u{1f}{module}"),
        _ => target_name.to_string(),
    }
}

/// The node a call through a renamed import binds (D#120): in the file the
/// specifier names, the top-level function its `exports` edges publish under
/// `export` — the symbol an export map renames to it
/// (`module.exports = { load: realLoad }`, `exports.load = realLoad`, stamped
/// `{"as": …}` by the parser) when the file has one, else the exported
/// function of that name. Never a method, a nested function or a function the
/// file does not export under that name, and never anything outside that file.
///
/// `None` when the specifier is a package's: it never names a file of the
/// project, so there is nothing to bind and nothing to wait for. `Some(empty)`
/// when a relative specifier names no indexed file yet, or its file does not
/// export such a function yet: the caller buffers the call, so it binds when
/// the file appears or gains the export, as a rebuild would. (An ESM import of
/// a missing file leaves an `<external>` sentinel named after the symbol, not
/// the specifier, so no file appearing later re-extracts the importer.)
///
/// Read from the database, after every batch's nodes and `exports` edges are
/// in: the batch pass always defers these calls, and a file's `exports` edges
/// to its own nodes are all bound in its own batch, so `exports` may memoize a
/// file for the whole pass.
pub(super) fn js_import_targets(
    conn: &rusqlite::Connection,
    exports: &mut JsExports,
    caller_path: &str,
    module: &str,
    export: &str,
    all_file_paths: &HashSet<String>,
) -> Result<Option<Vec<i64>>> {
    let Some(file) =
        super::js_modules::resolve_js_specifier_path(module, caller_path, all_file_paths)
    else {
        let relative = module.starts_with("./") || module.starts_with("../");
        return Ok(relative.then(Vec::new));
    };
    if !exports.by_file.contains_key(&file) {
        let loaded = JsFileExports::load(conn, &file)?;
        exports.by_file.insert(file.clone(), loaded);
    }
    Ok(Some(exports.by_file[&file].targets(export)))
}

/// Per-pass memo for [`js_import_targets`]: each file's exports, read once.
///
/// A query per call was the D#120 review's BLOCKER: it runs for every
/// renamed-import call of a full index, and the pending sweep re-runs it for
/// every buffered call on every incremental run. Besides the plan (see
/// [`js_export_map_sql`]), the bundled SQLite is built with `STAT4`, under
/// which a statement whose parameters meet an indexed column is re-prepared
/// each time they are re-bound: measured ~33 µs a call through rusqlite against
/// ~2.5 µs for the same query and plan in a `STAT4`-less build.
#[derive(Default)]
pub(super) struct JsExports {
    by_file: HashMap<String, JsFileExports>,
}

/// `(node, name, published as, is a function, start line, end line)`.
type JsExported = (i64, String, Option<String>, bool, i64, i64);

/// What one file's `<module>` exports, and the spans that make a function
/// nested.
struct JsFileExports {
    exported: Vec<JsExported>,
    /// `(node, start line, end line)` of the file's functions, methods, classes.
    spans: Vec<(i64, i64, i64)>,
}

impl JsFileExports {
    fn load(conn: &rusqlite::Connection, file: &str) -> Result<Self> {
        let exported = conn
            .prepare_cached(&js_export_map_sql())?
            .query_map([file], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let spans = conn
            .prepare_cached(
                "SELECT n.id, n.start_line, n.end_line
                 FROM files f CROSS JOIN nodes n ON n.file_id = f.id
                 WHERE f.path = ?1 AND n.type IN ('function', 'method', 'class')",
            )?
            .query_map([file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self { exported, spans })
    }

    /// The top-level functions published as `export`: through an export map
    /// (`{"as": export}`) when the file has one for that name — even one that
    /// publishes a class or a method, so a same-named function the map does
    /// not publish is never reached — else under their own name (no `as`). An
    /// `exports` edge naming a function under another key (`{ other: load }`)
    /// does not publish it as `load`.
    fn targets(&self, export: &str) -> Vec<i64> {
        let mapped = self
            .exported
            .iter()
            .any(|(_, _, key, ..)| key.as_deref() == Some(export));
        let mut ids: Vec<i64> = self
            .exported
            .iter()
            .filter(|(_, name, key, ..)| match key {
                Some(key) => key == export,
                None => !mapped && name == export,
            })
            .filter(|(id, _, _, function, start, end)| {
                *function && self.top_level(*id, *start, *end)
            })
            .map(|(id, ..)| *id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// No other function, method or class of the file spans its lines. (Two
    /// functions sharing one line both fail it: a missed edge, never a
    /// method's.)
    fn top_level(&self, id: i64, start: i64, end: i64) -> bool {
        !self
            .spans
            .iter()
            .any(|(o, s, e)| *o != id && *s <= start && *e >= end)
    }
}

/// [`JsFileExports::load`]'s query: every node the `<module>` of file `?1`
/// exports, with the key it is published under (`{"as": …}`), whether it is a
/// function, and its lines. Only exports to the file's own nodes: an export map
/// naming another file's symbol binds nothing.
///
/// `CROSS JOIN` fixes the loop order file → `<module>` → its `exports` edges.
/// Without statistics — a full index runs this before any `ANALYZE` — SQLite
/// drove the D#120 version from `idx_edges_relation`, scanning every `exports`
/// edge of the repo per call (review BLOCKER: +113% on a 5,000-file tree).
/// Pinned by `test_js_export_map_query_seeks_the_module_node_edges`.
pub(super) fn js_export_map_sql() -> String {
    format!(
        "SELECT n.id, n.name, json_extract(e.metadata, '$.as'), n.type = 'function',
                n.start_line, n.end_line
         FROM files f
         CROSS JOIN nodes m ON m.file_id = f.id AND m.name = '<module>'
         CROSS JOIN edges e ON e.source_id = m.id AND e.relation = '{}'
         CROSS JOIN nodes n ON n.id = e.target_id AND n.file_id = f.id
         WHERE f.path = ?1",
        crate::domain::REL_EXPORTS
    )
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
#[cfg(test)]
pub(super) fn resolve_pending_calls(db: &Database, crate_roots: &RustCrates) -> Result<usize> {
    resolve_pending_calls_touching(db, crate_roots, &mut std::collections::BTreeSet::new())
}

/// [`resolve_pending_calls`], adding to `touched` the file of every caller it
/// bound an edge from: those edges are cross-file by-name binds the post-pass
/// scope must classify, and their caller may be a file this run never opened.
pub(super) fn resolve_pending_calls_touching(
    db: &Database,
    crate_roots: &RustCrates,
    touched: &mut std::collections::BTreeSet<String>,
) -> Result<usize> {
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
               AND n.name IN (SELECT DISTINCT target_name FROM pending_unresolved_calls)",
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
            name_to_lang_targets
                .entry(name)
                .or_default()
                .push((id, lang));
            node_id_to_path.insert(id, path);
        }
    }

    // Map source_id → source file path so refine_ambiguous_targets gets the
    // proximity hint it needs. Dedupe first: a single source function with N
    // unresolved calls yields N pending rows sharing one source_id, so the raw
    // list can be ~2× the node count and a single unchunked `IN (...)` would
    // blow past SQLite's variable cap on large repos (issue #30). The chunked
    // helper keeps each IN-clause under MAX_IN_PARAMS.
    let mut source_ids: Vec<i64> = pending.iter().map(|p| p.source_id).collect();
    source_ids.sort_unstable();
    source_ids.dedup();
    let source_id_to_path = get_node_paths_by_ids(db.conn(), &source_ids)?;

    let mut edges_added = 0usize;
    let mut to_delete: Vec<i64> = Vec::new();
    let mut classes = ProjectClassNames::default();
    // Every file path, read at the first call a `use` anchors in a crate.
    let mut all_file_paths: Option<HashSet<String>> = None;
    let mut js_exports = JsExports::default();

    for row in &pending {
        // A call through a renamed import (D#120) binds what the file its
        // specifier names exports, and nothing by name: a requeue after the
        // export was renamed must bind what a rebuild binds, which is nothing.
        if let Some(CalleeMeta::Import { module, export }) =
            parse_callee_metadata(row.metadata.as_deref())
        {
            if all_file_paths.is_none() {
                all_file_paths = Some(
                    db.conn()
                        .prepare("SELECT path FROM files")?
                        .query_map([], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<_>>()?,
                );
            }
            let caller_path = source_id_to_path
                .get(&row.source_id)
                .map(String::as_str)
                .unwrap_or_default();
            let files = all_file_paths.as_ref().expect("loaded above");
            match js_import_targets(
                db.conn(),
                &mut js_exports,
                caller_path,
                &module,
                &export,
                files,
            )? {
                // Still waiting for the file to define it: stays buffered.
                Some(ids) if ids.is_empty() => continue,
                Some(ids) => {
                    for tgt_id in ids.iter().filter(|id| **id != row.source_id) {
                        if insert_edge_cached(
                            db.conn(),
                            row.source_id,
                            *tgt_id,
                            REL_CALLS,
                            row.metadata.as_deref(),
                        )? {
                            edges_added += 1;
                            touched.insert(caller_path.to_string());
                        }
                    }
                }
                None => {}
            }
            to_delete.push(row.id);
            continue;
        }
        let candidates: Vec<i64> = name_to_lang_targets
            .get(&row.target_name)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(_, lang)| *lang == row.source_language)
                    .map(|(id, _)| *id)
                    .filter(|id| *id != row.source_id) // self-call guard
                    .collect()
            })
            .unwrap_or_default();

        let candidates = classes.rust_call_shape_candidates(
            db,
            &row.source_language,
            row.metadata.as_deref(),
            source_id_to_path.get(&row.source_id).map(String::as_str),
            candidates,
        )?;
        if all_file_paths.is_none()
            && row
                .metadata
                .as_deref()
                .is_some_and(|m| m.contains(r#""rp":"#))
        {
            all_file_paths = Some(
                db.conn()
                    .prepare("SELECT path FROM files")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?,
            );
        }
        let no_files = HashSet::new();
        let never = classes.rust_receiver_never(
            db,
            &row.source_language,
            &row.target_name,
            row.metadata.as_deref(),
            crate_roots,
            source_id_to_path
                .get(&row.source_id)
                .map(String::as_str)
                .unwrap_or_default(),
            &candidates,
        )?;
        let candidates = classes.rust_receiver_candidates(
            db,
            &row.source_language,
            row.metadata.as_deref(),
            crate_roots,
            source_id_to_path
                .get(&row.source_id)
                .map(String::as_str)
                .unwrap_or_default(),
            &node_id_to_path,
            all_file_paths.as_ref().unwrap_or(&no_files),
            candidates,
        )?;
        let mut candidates = candidates;
        if is_package_bound(row.metadata.as_deref()) {
            let caller = source_id_to_path.get(&row.source_id);
            candidates.retain(|id| node_id_to_path.get(id) != caller);
        }
        if candidates.is_empty() {
            continue; // still unresolvable — leave buffered
        }

        // Apply the SAME callee-qualifier filtering Phase 2 does (index_files.rs
        // match on parse_callee_metadata) before binding. The sweep used to resolve
        // purely by bare name + same-language, ignoring the stored qualifier — so a
        // buffered `w.write()` (receiver type DataWriter) drained by a later pass
        // bound to EVERY same-language `write`, wiring false callers that persisted
        // until rebuild-index (H1, silent incremental graph corruption).
        //
        // Empty-filter behavior must match Phase 2 per qualifier shape, NOT a
        // blanket bare-fallback:
        //   - RecvType (receiver of known class): `recv_type_targets` — the
        //     class's own method, else the member-call set when the class is the
        //     project's (inherited method), else nothing (a library class), as
        //     the index_files.rs RecvType arm does.
        //   - SelfType / SelfRecv (Rust `Self::` / `self.`) and Path: STRUCTURAL —
        //     an empty filter means no target matches the fixed qualifier and a
        //     re-scan yields the same answer, so bind NOTHING and drain the row
        //     (index_files.rs SelfRecv/SelfType/Path arms all drop on empty). A
        //     bare fallback here would wire the very false same-name-sibling edges
        //     the qualifier exists to exclude.
        // Only bare (None), rtype, member and JS receiver can actually reach the
        // buffer today; self/stype/path handling is latent parity should a future
        // Phase-2 change route them here.
        let mut guessed: Option<String> = None;
        // A typed receiver's own-class binding is final, as in the deferred pass:
        // no proximity refinement may drop its overrides.
        let mut refine = true;
        let caller_path = source_id_to_path
            .get(&row.source_id)
            .map(String::as_str)
            .unwrap_or_default();
        // The caller itself is a same-named same-file candidate (`def f(self):
        // super().f()`), excluded above as a self-call: at resolution it filled
        // the same-file tier, which then bound nothing.
        let caller_shares_name = name_to_lang_targets
            .get(&row.target_name)
            .is_some_and(|entries| entries.iter().any(|(id, _)| *id == row.source_id));
        let resolved: Vec<i64> = match parse_callee_metadata(row.metadata.as_deref()) {
            Some(meta @ (CalleeMeta::RecvType(_) | CalleeMeta::SuperType(_))) => {
                let (t, dispatch) = match meta {
                    CalleeMeta::RecvType(t) => (t, true),
                    CalleeMeta::SuperType(t) => (t, false),
                    _ => unreachable!("matched above"),
                };
                // The row re-resolves the whole call — a requeue after its target
                // was renamed or deleted — so the call's surviving edges into
                // other files are replaced, not kept beside the new answer.
                delete_typed_call_edges(
                    db,
                    row.source_id,
                    &row.target_name,
                    row.metadata.as_deref(),
                )?;
                match recv_type_targets(
                    &t,
                    dispatch,
                    &candidates,
                    db,
                    &mut classes,
                    caller_path,
                    &node_id_to_path,
                )? {
                    RecvTypeTargets::Bind(own) => {
                        // A requeued edge may carry the `amb` mark of an earlier
                        // answer: the class now decides, so the mark goes.
                        guessed = row.metadata.as_deref().map(decided_meta);
                        refine = false;
                        own
                    }
                    RecvTypeTargets::Ambiguous(own) => {
                        guessed = row.metadata.as_deref().map(ambiguous_meta);
                        refine = false;
                        own
                    }
                    // The deferred pass's default chain over member candidates:
                    // same-file ones (the caller among them, which binds nothing
                    // to itself), else none for a noise name, else refined.
                    RecvTypeTargets::Fallback => {
                        guessed = row.metadata.as_deref().map(ambiguous_meta);
                        let pool = classes.member_call_candidates(
                            db,
                            row.metadata.as_deref(),
                            candidates,
                        )?;
                        let local: Vec<i64> = pool
                            .iter()
                            .copied()
                            .filter(|id| {
                                node_id_to_path.get(id).map(String::as_str) == Some(caller_path)
                            })
                            .collect();
                        // The deferred pass's same-file tier holds member
                        // candidates only: a free function named like the method
                        // is not in it, so it does not stop the cross-file pool.
                        let caller_in_tier = caller_shares_name
                            && !classes
                                .member_call_candidates(
                                    db,
                                    row.metadata.as_deref(),
                                    vec![row.source_id],
                                )?
                                .is_empty();
                        if !local.is_empty() || caller_in_tier {
                            refine = false;
                            local
                        } else if crate::domain::is_cross_file_call_noise(
                            &row.target_name,
                            &row.source_language,
                        ) {
                            // Stays buffered, as the deferred pass buffers it:
                            // only the class gaining the method answers it.
                            continue;
                        } else {
                            pool
                        }
                    }
                    // Not a project class yet: stay buffered (and age out), in
                    // case a later run adds it.
                    RecvTypeTargets::Drop => continue,
                }
            }
            Some(CalleeMeta::SelfType(t)) | Some(CalleeMeta::SelfRecv(t)) => {
                // Drop on empty (drain the row without binding), never bare-fall-back.
                self_filter_candidates(
                    &t,
                    &candidates,
                    &[row.source_id],
                    caller_path,
                    &node_id_to_path,
                    row.metadata.as_deref(),
                    db,
                )?
            }
            Some(CalleeMeta::Path(segments)) => {
                // A path a `use` spelled out (D#132), as the deferred pass reads it.
                match rust_use_anchor(row.metadata.as_deref(), &segments, caller_path, crate_roots)
                {
                    UseAnchor::Foreign => Vec::new(),
                    UseAnchor::At(anchor) => {
                        if all_file_paths.is_none() {
                            all_file_paths = Some(
                                db.conn()
                                    .prepare("SELECT path FROM files")?
                                    .query_map([], |r| r.get::<_, String>(0))?
                                    .collect::<rusqlite::Result<_>>()?,
                            );
                        }
                        let files = all_file_paths.as_ref().expect("loaded above");
                        match rust_anchored_targets(
                            &anchor,
                            &candidates,
                            &node_id_to_path,
                            db,
                            files,
                        )? {
                            Anchored::Named(ids) => {
                                refine = false;
                                ids
                            }
                            Anchored::Elsewhere(ids) => {
                                let ids = if ids.len() > 1 {
                                    refine_ambiguous_targets(&ids, caller_path, &node_id_to_path)
                                } else {
                                    ids
                                };
                                guessed = row
                                    .metadata
                                    .as_deref()
                                    .map(|m| reexport_meta(m, &anchor.dir, ids.len() > 1));
                                refine = false;
                                ids
                            }
                            // Stays buffered: only a new item in that crate answers it.
                            Anchored::Nothing => continue,
                        }
                    }
                    // A crate no manifest could be read for: by name, as the
                    // deferred pass's default chain binds it.
                    UseAnchor::Opaque(stripped) if stripped.is_empty() => {
                        guessed = row.metadata.as_deref().map(ambiguous_meta);
                        candidates
                    }
                    UseAnchor::Opaque(stripped) => path_filter_candidates(
                        &stripped,
                        &candidates,
                        &node_id_to_path,
                        db,
                        crate_roots,
                    )?,
                    // Drop on empty (drain the row without binding), never bare-fall-back.
                    UseAnchor::Unplaced(stripped) => path_filter_candidates(
                        &stripped,
                        &candidates,
                        &node_id_to_path,
                        db,
                        crate_roots,
                    )?,
                    UseAnchor::None => path_filter_candidates(
                        &segments,
                        &candidates,
                        &node_id_to_path,
                        db,
                        crate_roots,
                    )?,
                }
            }
            // Member call: the same free-function exclusion as Phase 2.
            Some(CalleeMeta::Member) => {
                classes.member_call_candidates(db, row.metadata.as_deref(), candidates)?
            }
            // A Rust `x.f()` whose receiver type narrowed the pool (D#112): the
            // deferred pass's unique-method rule over that pool, which a later
            // run can widen by more than one method at once.
            Some(CalleeMeta::Receiver(_) | CalleeMeta::Chain)
                if row.source_language == "rust"
                    && rust_receiver(row.metadata.as_deref(), crate_roots).is_some() =>
            {
                let methods = method_candidates(&candidates, db)?;
                let local: Vec<i64> = methods
                    .iter()
                    .copied()
                    .filter(|id| node_id_to_path.get(id).map(String::as_str) == Some(caller_path))
                    .collect();
                if local.len() == 1 {
                    local
                } else if local.is_empty() && methods.len() == 1 {
                    methods
                } else {
                    // No such method, or several: stays buffered, as the deferred
                    // pass buffers it, until the type's own method decides it.
                    continue;
                }
            }
            // Bare / chain / JS receiver: Phase 2's default chain resolves these by
            // bare name too, so the existing behavior already matches. A buffered
            // Rust `x.f()` (D#117) meets at most one method here: a second one
            // arriving in the same run is a new duplicate definition, and
            // `fan_out_to_new_duplicate_definitions` re-resolves the caller.
            _ => candidates,
        };

        let metadata = guessed.as_deref().or(row.metadata.as_deref());
        let refined = if refine && resolved.len() > 1 {
            refine_ambiguous_targets(&resolved, caller_path, &node_id_to_path)
        } else {
            resolved
        };

        // What the receiver's type rules out is dropped from the result; a call
        // left with nothing stays buffered when other files decided that.
        if never.read_elsewhere
            && !refined.is_empty()
            && refined.iter().all(|id| never.ids.contains(id))
        {
            continue;
        }
        for tgt_id in refined.iter().filter(|id| !never.ids.contains(id)) {
            if insert_edge_cached(db.conn(), row.source_id, *tgt_id, REL_CALLS, metadata)? {
                edges_added += 1;
                touched.insert(caller_path.to_string());
            }
        }
        to_delete.push(row.id);
    }

    for id in to_delete {
        delete_pending_unresolved_call(db.conn(), id)?;
    }

    // Bounded retention (SCHEMA v10): rows that survived this sweep age by one
    // failed attempt; rows reaching PENDING_CALL_MAX_ATTEMPTS are evicted.
    // Resolution wins ties — resolved rows were drained above before aging.
    let evicted = age_and_evict_pending_unresolved_calls(db.conn())?;
    if evicted > 0 {
        tracing::debug!("[pipeline] pending-call sweep evicted {evicted} rows at max attempts");
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
/// Build a TEMP table of every `imports` edge as (caller file, imported name,
/// imported target), indexed both ways.
///
/// The three global post-passes each asked this question per candidate edge, as
/// a correlated subquery. That is the whole reason a one-file refresh costs what
/// a rebuild's post-pass costs: the passes evaluate every edge in the index, and
/// each evaluation re-derives the caller file's imports. Measured on a
/// 2,385-file / 397,119-edge Python tree, the three passes were 1.75 s, 2.88 s
/// and 2.56 s of a 7.4 s query — against 5 ms when nothing is stale, because
/// `resync_stale_files` returns before any of this when no file changed.
///
/// Materializing the projection once turns each correlated re-derivation into
/// an index probe. It changes no semantics: the projection is the subquery's own
/// FROM clause verbatim. Each pass builds and drops its own copy rather than
/// sharing one, so none of them acquires an invisible precondition on having
/// been called after another.
fn build_imports_temp(conn: &rusqlite::Connection, with_name: bool) -> Result<()> {
    use crate::domain::REL_IMPORTS;
    conn.execute_batch("DROP TABLE IF EXISTS temp.cg_imports;")?;
    conn.execute(
        "CREATE TEMP TABLE cg_imports AS
         SELECT mn.file_id AS fid, itn.name AS nm, ie.target_id AS tid
         FROM edges ie
         JOIN nodes mn  ON mn.id  = ie.source_id
         JOIN nodes itn ON itn.id = ie.target_id
         WHERE ie.relation = ?1",
        rusqlite::params![REL_IMPORTS],
    )?;
    conn.execute_batch("CREATE INDEX cg_imports_fid_tid ON cg_imports(fid, tid);")?;
    if with_name {
        conn.execute_batch("CREATE INDEX cg_imports_fid_nm ON cg_imports(fid, nm);")?;
    }
    Ok(())
}

fn drop_imports_temp(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("DROP TABLE IF EXISTS temp.cg_imports;")?;
    Ok(())
}

/// Which slice of the graph the three global post-passes have to reconsider.
///
/// They are set-based passes over EVERY cross-file `calls`/`references` edge, and
/// they run on every indexing run — including the one-file refresh a read command
/// triggers when it notices a stale file. Measured on django/django (3,453 files,
/// 48,084 nodes, 262,463 edges) with a single file edited and `show` as the next
/// command: `run_global_edge_post_passes` took 674 ms of that command's 1,054 ms
/// (2d-bind 177, 2d-prune 207, 2e-confidence 290), while the whole-graph name map
/// audit item CORE-06 blamed cost 50 ms. The scan is what costs, not the writes:
/// suppressing no-op UPDATEs saved 65 ms of the 290.
///
/// `Files` bounds that scan. It is NOT an approximation — see the completeness
/// argument on [`PostPassScope::Files`].
pub(super) enum PostPassScope {
    /// Reconsider every edge. A full index, a large batch, or any run whose
    /// effect we cannot bound. Also the base case the `Files` induction rests on.
    Global,
    /// Reconsider only edges that touch `temp.cg_scope_files` on either side, or
    /// whose TARGET NAME is in `temp.cg_scope_names`.
    ///
    /// Completeness, per pass — each reads only these inputs:
    ///
    /// * 2d-bind and 2d-prune read the caller file's own nodes and import edges
    ///   plus the target node's name and id. Those change when the caller's file
    ///   or the target's file was re-indexed this run, which the two file arms
    ///   cover — but NOT only then, and an earlier version of this comment
    ///   claimed otherwise. `restore_inbound_edges` skips sources that are in
    ///   the run and requeues the rest, and the deferred pass re-binds by NAME
    ///   against the whole tree, so a run can mint an `imports` edge whose
    ///   source is a file it never opened. Neither pass has a name arm, so
    ///   `index_files` adds every deferred relation's source path to
    ///   `cg_scope_files` before the passes run (pre-ship review 2026-09-08).
    ///   Reviewers could not turn the gap into a divergence — 15 differential
    ///   rounds over a 159-file corpus and a targeted fixture both came back
    ///   byte-identical, because the requeue moves the import and the calls
    ///   through the same name pool with the same refiner, so a non-unique
    ///   import cannot make bind fire — but the scope now covers it by
    ///   construction rather than by that argument.
    /// * 2e-confidence additionally reads `COUNT(*)` of same-name, same-language
    ///   nodes — a GLOBAL input. A node appearing or vanishing anywhere flips the
    ///   confidence of edges between two files that did not change, so the name
    ///   arm carries exactly the names whose count moved. That set is DERIVED by
    ///   counting before and after (see `scope_names_from_count_drift`) rather
    ///   than accumulated as the run goes: this pipeline's bookkeeping has twice
    ///   cost real edges (INDEX_VERSION v58, v61), and a count diff cannot miss a
    ///   channel the way a hand-maintained list can. `<external>` is in the
    ///   counted paths for completeness, not because it is load-bearing:
    ///   `mint_external_sentinels` writes that files row with
    ///   `language = "external"` and `cg_namecount` is keyed (name, language),
    ///   so a sentinel's count can only ever reach edges whose TARGET is itself
    ///   a sentinel — and sentinel names are unique inside `<external>`, so that
    ///   count never exceeds 1 and those edges are always `inferred`. Including
    ///   it only widens the drift set, which is the safe direction (pre-ship
    ///   review 2026-09-08).
    ///
    /// The induction: a full index runs `Global`, so the graph starts consistent;
    /// every incremental run then repairs exactly what it disturbed.
    Files,
}

impl PostPassScope {
    /// The join-order barrier that makes a scope actually bound the work.
    ///
    /// SQLite will not drive from a temp table it has no statistics for: with a
    /// plain `JOIN` the planner opened `idx_edges_relation` and scanned all
    /// 171,981 `calls` edges, then used the scope as a bloom filter — 122 ms even
    /// when the scope was EMPTY. `CROSS JOIN` fixes the order, the plan becomes
    /// `SCAN cg_scope_files` → `idx_nodes_file` → `idx_edges_source_rel`, and the
    /// same statement takes 1.5 ms. Every arm below therefore spells its joins
    /// `CROSS JOIN`, in scope-first order, deliberately.
    fn is_global(&self) -> bool {
        matches!(self, PostPassScope::Global)
    }
}

/// Snapshot same-name/same-language node counts for the paths a run is about to
/// touch. Call before the run mutates anything; pair with
/// [`scope_names_from_count_drift`] after.
///
/// Restricted to `temp.cg_scope_paths` (this run's files, its deletions, and
/// `<external>`) because nodes outside those files are not reachable by anything
/// the run does — so their counts cannot move, and counting all 48,084 of them
/// would spend the 29 ms this is meant to save.
pub(super) fn snapshot_scope_name_counts(conn: &rusqlite::Connection) -> Result<()> {
    // Keyed (name, LANGUAGE), because that is how `cg_namecount` — the input
    // Phase 2e classifies from — is keyed. Grouping by name alone made a name
    // MOVING between languages invisible: python `helper` falling 2 -> 1 while a
    // javascript `helper` appears leaves the name-only count at 1 both sides, so
    // `helper` never entered the scope and an edge between two untouched files
    // kept an `ambiguous` a rebuild calls `inferred` (pre-ship review
    // 2026-09-08, reproduced).
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_namecount_before;
         CREATE TEMP TABLE cg_namecount_before AS
           SELECT n.name AS nm, f.language AS lang, COUNT(*) AS cnt
           FROM nodes n
           JOIN files f ON f.id = n.file_id
           JOIN cg_scope_paths p ON p.path = f.path
           GROUP BY n.name, f.language;
         CREATE INDEX cg_namecount_before_k ON cg_namecount_before(nm, lang);",
    )?;
    Ok(())
}

/// Build `temp.cg_scope_names` — the names whose node count actually moved.
///
/// A name is in the set when its count differs between the snapshot and now, in
/// EITHER direction: a name that vanished (its file was deleted, or the symbol
/// was renamed away) drops other files' edges from `ambiguous` back to
/// `inferred`, which is as much a change as gaining one.
pub(super) fn scope_names_from_count_drift(conn: &rusqlite::Connection) -> Result<usize> {
    // The pairs are compared on (nm, lang); the SCOPE is the set of NAMES those
    // pairs mention, because the classify arm joins `tgt.name` and picks the
    // language up from `cg_namecount` afterwards. `IS NOT` rather than `<>` so a
    // pair present on one side and absent on the other (NULL cnt) counts as
    // drift, and `lang IS b.lang` because `files.language` is nullable.
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_namecount_after;
         CREATE TEMP TABLE cg_namecount_after AS
           SELECT n.name AS nm, f.language AS lang, COUNT(*) AS cnt
           FROM nodes n
           JOIN files f ON f.id = n.file_id
           JOIN cg_scope_paths p ON p.path = f.path
           GROUP BY n.name, f.language;
         CREATE INDEX cg_namecount_after_k ON cg_namecount_after(nm, lang);
         DROP TABLE IF EXISTS temp.cg_scope_names;
         CREATE TEMP TABLE cg_scope_names AS
           SELECT DISTINCT nm FROM (
             SELECT b.nm AS nm FROM cg_namecount_before b
               LEFT JOIN cg_namecount_after a ON a.nm = b.nm AND a.lang IS b.lang
               WHERE a.cnt IS NOT b.cnt
             UNION ALL
             SELECT a.nm AS nm FROM cg_namecount_after a
               LEFT JOIN cg_namecount_before b ON b.nm = a.nm AND b.lang IS a.lang
               WHERE b.cnt IS NOT a.cnt
           );
         CREATE INDEX cg_scope_names_k ON cg_scope_names(nm);
         DROP TABLE IF EXISTS temp.cg_namecount_before;
         DROP TABLE IF EXISTS temp.cg_namecount_after;",
    )?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM cg_scope_names", [], |r| r.get(0))?;
    Ok(n as usize)
}

/// Resolve `temp.cg_scope_paths` to the file ids those paths hold NOW.
///
/// After the run, not before: re-indexing a file can hand it a new row, and a
/// deleted path has no row at all — which is correct, since its nodes are gone
/// and the edges that pointed into them come back through the name arm.
pub(super) fn build_scope_files(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_scope_files;
         CREATE TEMP TABLE cg_scope_files AS
           SELECT f.id AS fid FROM files f JOIN cg_scope_paths p ON p.path = f.path;
         CREATE INDEX cg_scope_files_k ON cg_scope_files(fid);",
    )?;
    Ok(())
}

/// Drop everything the scope machinery created.
pub(super) fn drop_scope_temps(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_scope_paths;
         DROP TABLE IF EXISTS temp.cg_scope_files;
         DROP TABLE IF EXISTS temp.cg_scope_names;
         DROP TABLE IF EXISTS temp.cg_namecount_before;
         DROP TABLE IF EXISTS temp.cg_namecount_after;",
    )?;
    Ok(())
}

/// Snapshot `(name, language) -> definition count` for `paths`, before an
/// `index_files` run mutates them. Pair with
/// [`bare_name_callers_of_new_duplicates`] after.
///
/// Deliberately NOT [`snapshot_scope_name_counts`], which this otherwise
/// mirrors. That one lives inside `index_files` and only on the
/// `PostPassScope::Files` branch, so a run of more than
/// `SCOPED_POST_PASS_MAX_FILES` files never takes it; its temps are also dropped
/// before `index_files` returns, so a caller cannot read them. D#24 needs the
/// signal on EVERY incremental run and needs it after the run commits, so the
/// caller owns its own pair of counts. Keyed `(name, language)` for the same
/// reason the sibling is: a name moving between languages leaves a name-only
/// count unchanged on both sides.
///
/// Unlike the sibling, `<external>` is deliberately NOT in `paths`. There it
/// widens a relabelling scope, which is the safe direction. Here it would widen
/// a set of files to RE-EXTRACT, and a sentinel is never a fan-out target: it
/// records a specifier that failed to resolve, so a bare call must not gain an
/// edge to one. Sentinels are minted and reaped mid-run, so including them would
/// buy nothing but extra rounds.
pub(super) fn snapshot_definition_counts(
    conn: &rusqlite::Connection,
    paths: &[String],
) -> Result<()> {
    drop_fanout_temps(conn)?;
    conn.execute_batch("CREATE TEMP TABLE cg_fanout_paths (path TEXT PRIMARY KEY);")?;
    {
        let mut stmt = conn.prepare("INSERT OR IGNORE INTO cg_fanout_paths (path) VALUES (?1)")?;
        for p in paths {
            stmt.execute([p])?;
        }
    }
    // `<module>` is excluded here and in the `after` half, so the two stay
    // symmetric. Every file carries one, which makes it the most duplicated name
    // in any index by a wide margin — 298 of 5,903 nodes in this repo's own,
    // where the next most duplicated real name has 7 — so ADDING any file raises
    // its count and drops it into the trigger set unconditionally. It selects no
    // caller today (measured: zero `calls` or `references` edges target a
    // `<module>` node; imports do, and imports are not in this round's relation
    // set), so the cost is an index probe per module node on every file
    // addition, paid for nothing. An ordinary EDIT never pays it, which is why
    // no one-file-edit benchmark can see it. Excluded for the same reason
    // `<external>` is left out of `paths`: a sentinel name is not a fan-out
    // target.
    conn.execute_batch(
        "CREATE TEMP TABLE cg_fanout_before AS
           SELECT n.name AS nm, f.language AS lang, COUNT(*) AS cnt
           FROM nodes n
           JOIN files f ON f.id = n.file_id
           JOIN cg_fanout_paths p ON p.path = f.path
           WHERE n.name <> '<module>'
           GROUP BY n.name, f.language;
         CREATE INDEX cg_fanout_before_k ON cg_fanout_before(nm, lang);",
    )?;
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE cg_shape_before AS {CLASS_SHAPE_SELECT};"
    ))?;
    Ok(())
}

/// The class structure of `cg_fanout_paths` that typed-call resolution reads
/// ([`recv_type_targets`]), as `(kind, path, value)` rows free of line numbers,
/// so an edit that only moves code produces the same rows:
/// `c` a class-like node (`language`, qualified name, nested), `i` an `inherits`
/// edge out of the file (`sub`, `base` names), `m` a method (qualified name).
const CLASS_SHAPE_SELECT: &str = "
    SELECT 'c' AS k, f.path AS p,
           COALESCE(f.language, '') || char(31) || COALESCE(n.qualified_name, n.name)
             || char(31) || EXISTS (
               SELECT 1 FROM nodes o
               WHERE o.file_id = n.file_id AND o.id <> n.id
                 AND o.type IN ('class', 'struct', 'interface', 'type', 'enum', 'trait', 'union')
                 AND o.start_line <= n.start_line AND o.end_line >= n.end_line) AS v
    FROM nodes n
    JOIN files f ON f.id = n.file_id
    JOIN cg_fanout_paths fp ON fp.path = f.path
    WHERE n.type IN ('class', 'struct', 'interface', 'type', 'enum', 'trait', 'union')
    UNION
    SELECT 'i', f.path, s.name || char(31) || t.name
    FROM edges e
    JOIN nodes s ON s.id = e.source_id
    JOIN files f ON f.id = s.file_id
    JOIN cg_fanout_paths fp ON fp.path = f.path
    JOIN nodes t ON t.id = e.target_id
    WHERE e.relation = 'inherits'
    UNION
    SELECT 'j', ft.path, s.name || char(31) || t.name
    FROM edges e
    JOIN nodes t ON t.id = e.target_id
    JOIN files ft ON ft.id = t.file_id
    JOIN cg_fanout_paths fp ON fp.path = ft.path
    JOIN nodes s ON s.id = e.source_id
    WHERE e.relation = 'inherits'
    UNION
    SELECT 'm', f.path, n.qualified_name || char(31) || COALESCE(n.return_type, '')
    FROM nodes n
    JOIN files f ON f.id = n.file_id
    JOIN cg_fanout_paths fp ON fp.path = f.path
    WHERE n.type IN ('function', 'method') AND n.qualified_name LIKE '%.%'
    UNION
    SELECT 'f', f.path, n.name || char(31) || cf.field || char(31)
             || COALESCE(cf.dot_type, '') || char(31) || COALESCE(cf.arrow_type, '')
    FROM cpp_fields cf
    JOIN nodes n ON n.id = cf.class_id
    JOIN files f ON f.id = n.file_id
    JOIN cg_fanout_paths fp ON fp.path = f.path";

/// The files outside this run holding a typed call (`rtype` / `super`, bound or
/// buffered) whose answer this run's change to the class structure can move —
/// D#97's answer to "who binds differently now". Pair with
/// [`snapshot_definition_counts`], and call before
/// [`bare_name_callers_of_new_duplicates`], which drops the snapshot.
///
/// A typed call reads the whole project's class structure: whether a class of
/// its receiver's name exists, whether that name is unique and top-level, which
/// classes subclass it, and which of those define the method. So the answer is
/// derived as a before/after difference over this run's paths, never kept as a
/// ledger: the class names whose rows moved, each with every ancestor (a moved
/// subclass changes its ancestors' overrides), and for a moved method only the
/// calls of that method on its class and the class's ancestors. Empty — no scan
/// of the typed calls — when the structure did not move, which is every edit
/// that leaves classes, bases and method sets alone.
pub(super) fn typed_callers_of_class_drift(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    fn rows(conn: &rusqlite::Connection, sql: &str) -> Result<HashSet<(String, String, String)>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?)))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        Ok(rows)
    }
    let last = |s: &str| class_path(s).last().map(|l| l.to_string());
    let before = rows(conn, "SELECT k, p, v FROM cg_shape_before")?;
    let after = rows(conn, &format!("SELECT k, p, v FROM ({CLASS_SHAPE_SELECT})"))?;
    // `classes`: names whose class nodes moved. `bases`-side names: both ends of a
    // moved `inherits` edge. `methods`: (owner, method) of a moved method.
    let (mut classes, mut linked, mut methods) = (HashSet::new(), HashSet::new(), HashSet::new());
    // Classes whose recorded C++ fields moved: a call through one of their
    // fields (`fc`) may now be typed differently, or at all.
    let mut field_owners: HashSet<String> = HashSet::new();
    // Subclasses whose bases moved: they and their own subclasses inherit
    // differently now.
    let mut rebased: HashSet<String> = HashSet::new();
    for (k, _, v) in before.symmetric_difference(&after) {
        let fields: Vec<&str> = v.split('\u{1f}').collect();
        match (k.as_str(), fields.as_slice()) {
            ("c", [_, name, _]) => classes.extend(last(name)),
            // `i`: out of this run's files; `j`: into them (a deleted base's
            // subclasses live in files this run never opened).
            ("i" | "j", [sub, base]) => {
                rebased.extend(last(sub));
                linked.extend(last(sub).into_iter().chain(last(base)));
            }
            ("f", [class, ..]) => field_owners.extend(last(class)),
            // The return type too: a C++ chain (`x->current()->Ref()`) goes
            // through it.
            ("m", [q, _]) => {
                if let Some((owner, m)) = q.rsplit_once('.') {
                    methods.extend(last(owner).map(|o| (o, m.to_string())));
                }
            }
            _ => {}
        }
    }
    if classes.is_empty() && linked.is_empty() && methods.is_empty() && field_owners.is_empty() {
        return Ok(Vec::new());
    }

    // A call typed `T` follows overrides only when `T` names one class (the
    // `seeds` rule in `recv_type_targets`), so a moved subclass or override
    // reaches only its UNIQUELY named ancestors. Current counts are exact for the
    // ones that did not move: a name whose count changed had a class node added
    // or removed in this run, and is in `classes`, which is not filtered.
    let mut class_count: HashMap<String, usize> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(qualified_name, name) FROM nodes
             WHERE type IN ('class', 'struct', 'interface', 'type', 'enum', 'trait', 'union')",
        )?;
        for name in stmt.query_map([], |r| r.get::<_, String>(0))? {
            if let Some(l) = last(&name?) {
                *class_count.entry(l).or_default() += 1;
            }
        }
    }
    let unique = |n: &String| class_count.get(n) == Some(&1);

    // Ancestors by name, as `recv_type_targets` reaches overrides by name.
    let mut bases: HashMap<String, Vec<String>> = HashMap::new();
    let mut subs: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT s.name, t.name FROM edges e
             JOIN nodes s ON s.id = e.source_id JOIN nodes t ON t.id = e.target_id
             WHERE e.relation = 'inherits'",
        )?;
        let pairs = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for pair in pairs {
            let (sub, base) = pair?;
            if let (Some(sub), Some(base)) = (last(&sub), last(&base)) {
                subs.entry(base.clone()).or_default().push(sub.clone());
                bases.entry(sub).or_default().push(base);
            }
        }
    }
    let walk = |edges: &HashMap<String, Vec<String>>, from: &mut dyn Iterator<Item = String>| {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = from.collect();
        while let Some(n) = stack.pop() {
            if seen.insert(n.clone()) {
                stack.extend(edges.get(&n).into_iter().flatten().cloned());
            }
        }
        seen
    };
    // Ancestors (for overrides) and descendants (which inherit the method).
    let closure = |from: &mut dyn Iterator<Item = String>| walk(&bases, from);
    let descendants = |from: &mut dyn Iterator<Item = String>| walk(&subs, from);
    // A moved class answers for its own name whatever its count: whether a class
    // of that name exists, is unique, is nested. Its ancestors, for overrides.
    let mut any: HashSet<String> = closure(&mut classes.iter().chain(linked.iter()).cloned())
        .into_iter()
        .filter(|n| unique(n))
        .collect();
    any.extend(classes.iter().cloned());
    // A class whose bases moved, and every class below it, inherits from a
    // different chain now.
    any.extend(descendants(&mut rebased.iter().cloned()));
    // A moved method answers for calls of it on its own class (own method vs
    // inherited), on the class's ancestors for overrides, and on its
    // descendants, which inherit it.
    let mut per_method: HashSet<(String, String)> = HashSet::new();
    for (owner, m) in &methods {
        for n in closure(&mut std::iter::once(owner.clone())) {
            if n == *owner || unique(&n) {
                per_method.insert((n, m.clone()));
            }
        }
        for n in descendants(&mut std::iter::once(owner.clone())) {
            per_method.insert((n, m.clone()));
        }
    }

    let in_run: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT path FROM cg_fanout_paths")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        paths
    };
    let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // A C++ call through a field (`fc`) looked its type up in `fc`'s fields,
    // else its bases': it moves with any of them, or with their bases or
    // existence. Typed (`rtype`) or not (`member`).
    let method_owners: HashSet<String> = methods.iter().map(|(o, _)| o.clone()).collect();
    let field_keys: HashSet<&String> = field_owners
        .iter()
        .chain(classes.iter())
        .chain(linked.iter())
        .collect();
    // A C++ chain (`vc`) went through fields and return types of each class it
    // names: it moves with any of them, their bases, or their methods.
    let via_keys: HashSet<&String> = field_keys
        .iter()
        .copied()
        .chain(method_owners.iter())
        .collect();
    // A Rust call typed by its receiver (`"rt"`, D#112) reads whether the file
    // of each candidate impl defines a struct of the receiver's name
    // (`foreign_receiver_owner`): it moves with a class of that name.
    let mut stmt = conn.prepare(
        "SELECT f.path, json_extract(e.metadata, '$.q'), json_extract(e.metadata, '$.v'),
                json_extract(e.metadata, '$.fc'), t.name, json_extract(e.metadata, '$.vc'),
                json_extract(e.metadata, '$.rt')
         FROM edges e
         JOIN nodes s ON s.id = e.source_id JOIN files f ON f.id = s.file_id
         JOIN nodes t ON t.id = e.target_id
         WHERE e.relation = 'calls'
           AND (json_extract(e.metadata, '$.q') IN ('rtype', 'super')
                OR json_extract(e.metadata, '$.fc') IS NOT NULL
                OR json_extract(e.metadata, '$.vc') IS NOT NULL
                OR json_extract(e.metadata, '$.rt') IS NOT NULL)
         UNION
         SELECT f.path, json_extract(p.metadata, '$.q'), json_extract(p.metadata, '$.v'),
                json_extract(p.metadata, '$.fc'), p.target_name, json_extract(p.metadata, '$.vc'),
                json_extract(p.metadata, '$.rt')
         FROM pending_unresolved_calls p
         JOIN nodes s ON s.id = p.source_id JOIN files f ON f.id = s.file_id
         WHERE json_extract(p.metadata, '$.q') IN ('rtype', 'super')
            OR json_extract(p.metadata, '$.fc') IS NOT NULL
            OR json_extract(p.metadata, '$.vc') IS NOT NULL
            OR json_extract(p.metadata, '$.rt') IS NOT NULL",
    )?;
    let calls = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    for call in calls {
        let (path, q, v, fc, target, vc, rt) = call?;
        if in_run.contains(&path) || out.contains(&path) {
            continue;
        }
        if rt.as_ref().is_some_and(|t| classes.contains(t)) {
            out.insert(path);
            continue;
        }
        // It also loses the candidates its type's methods and `Deref` rule out
        // (`ProjectClassNames::rust_receiver_never`), buffered when that left it
        // nothing: a moved method of that name, or a moved `deref`, moves it.
        if rt.as_ref().is_some_and(|t| {
            methods.iter().any(|(o, m)| {
                o == t && (*m == target || matches!(m.as_str(), "deref" | "deref_mut"))
            })
        }) {
            out.insert(path);
            continue;
        }
        let through_field = fc.as_deref().and_then(last).is_some_and(|owner| {
            closure(&mut std::iter::once(owner))
                .iter()
                .any(|n| field_keys.contains(n))
        });
        let typed = matches!(q.as_deref(), Some("rtype" | "super"));
        let by_class = typed
            && v.as_deref()
                .and_then(last)
                .is_some_and(|class| any.contains(&class) || per_method.contains(&(class, target)));
        let through_chain = vc
            .as_deref()
            .and_then(|vc| serde_json::from_str::<Vec<String>>(vc).ok())
            .is_some_and(|names| {
                names.into_iter().any(|n| {
                    closure(&mut std::iter::once(n))
                        .iter()
                        .any(|a| via_keys.contains(a))
                })
            });
        if through_field || by_class || through_chain {
            out.insert(path);
        }
    }
    // A Rust `self.m()` / `Self::m()` (`"self"` / `"stype"`, payload the impl's
    // type) binds the nearest methods of its type — own file, else crate, else
    // (from a trait impl) the workspace — so an `m` of that type appearing in or
    // leaving ANY file can move it, deleted files included (pre-tag review:
    // `a/src/y.rs` gaining `Foo::m` left the caller bound into another crate).
    if !methods.is_empty() {
        // Driven from the moved methods' names (few) through the name and
        // target indexes, not from every call edge: scanning all of them cost
        // ~100 ms on tokio's method-adding edit.
        conn.execute_batch(
            "DROP TABLE IF EXISTS temp.cg_drift_names;
             CREATE TEMP TABLE cg_drift_names (nm TEXT PRIMARY KEY);",
        )?;
        {
            let mut ins = conn.prepare("INSERT OR IGNORE INTO cg_drift_names (nm) VALUES (?1)")?;
            for (_, m) in &methods {
                ins.execute([m])?;
            }
        }
        let found = (|| -> Result<Vec<(String, String, Option<String>)>> {
            let mut stmt = conn.prepare(
                "SELECT f.path, t.name, json_extract(e.metadata, '$.v')
                 FROM cg_drift_names d
                 CROSS JOIN nodes t ON t.name = d.nm
                 CROSS JOIN edges e ON e.target_id = t.id AND e.relation = 'calls'
                 CROSS JOIN nodes s ON s.id = e.source_id
                 CROSS JOIN files f ON f.id = s.file_id
                 WHERE f.language = 'rust'
                   AND json_extract(e.metadata, '$.q') IN ('self', 'stype')
                 UNION
                 SELECT f.path, p.target_name, json_extract(p.metadata, '$.v')
                 FROM cg_drift_names d
                 CROSS JOIN pending_unresolved_calls p ON p.target_name = d.nm
                 CROSS JOIN nodes s ON s.id = p.source_id
                 CROSS JOIN files f ON f.id = s.file_id
                 WHERE f.language = 'rust'
                   AND json_extract(p.metadata, '$.q') IN ('self', 'stype')",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })();
        conn.execute_batch("DROP TABLE IF EXISTS temp.cg_drift_names;")?;
        for (path, target, v) in found? {
            if in_run.contains(&path) || out.contains(&path) {
                continue;
            }
            if v.as_deref()
                .and_then(last)
                .is_some_and(|ty| methods.contains(&(ty, target)))
            {
                out.insert(path);
            }
        }
    }
    Ok(out.into_iter().collect())
}

/// Drop everything the fan-out pair creates.
///
/// Called on entry by [`snapshot_definition_counts`] and on both exits of
/// [`bare_name_callers_of_new_duplicates`], including its error path. The
/// sibling `drop_scope_temps` guards a hazard this pair does not have — a stale
/// `cg_scope_paths` can silently scope the NEXT run, whereas these four are
/// always created fresh together, so a leak costs retention on a long-lived MCP
/// connection and cannot mis-scope anything. One case still escapes: if the
/// round-1 `index_files` returns `Err`, the fan-out round never runs and these
/// survive until the next run's entry drop. That is the documented bound, not an
/// oversight.
pub(super) fn drop_fanout_temps(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_fanout_paths;
         DROP TABLE IF EXISTS temp.cg_fanout_before;
         DROP TABLE IF EXISTS temp.cg_fanout_after;
         DROP TABLE IF EXISTS temp.cg_fanout_up;
         DROP TABLE IF EXISTS temp.cg_shape_before;",
    )?;
    Ok(())
}

/// The files a second extraction round must cover: D#24's answer to "who needs
/// to learn that this name now has another definition".
///
/// Two halves, both read off state that already exists rather than re-derived:
///
/// * **Which names.** Those whose definition count inside this run's own paths
///   ROSE — up only, unlike `scope_names_from_count_drift`'s symmetric
///   difference. A name that LOST a definition needs no new edge; the surviving
///   edges only need the relabel the post passes already do.
/// * **Which callers.** Files outside the run holding a `calls` edge to that
///   name that Phase 2e has just labelled `ambiguous`. `CONF_CASE` assigns that
///   label exactly when the target name's count is >1 AND the caller's file does
///   not import that exact target AND the edge carries no type/path qualifier —
///   which is the definition of "resolved by a bare name among same-name
///   siblings". Reusing that verdict means an import-bound or type-qualified
///   call is excluded by construction, instead of by a second predicate here
///   that would have to be kept in step with `CONF_CASE`.
///
/// Drops its own temps: unlike the scope tables, nothing downstream reads these.
pub(super) fn bare_name_callers_of_new_duplicates(
    conn: &rusqlite::Connection,
    crates: &RustCrates,
) -> Result<Vec<String>> {
    use crate::domain::{CONF_AMBIGUOUS, REL_CALLS, REL_IMPORTS, REL_REFERENCES};

    // `(nm, lang)` all the way through, never `nm` alone. The pairs are already
    // unique — `cg_fanout_after` groups by both — so no DISTINCT is needed here,
    // and carrying the language is what keeps the round from firing across
    // languages: a rise in the JavaScript count of `render` says nothing about a
    // Python caller of `render`, because `CONF_CASE` joins its name count as
    // `nc.lang IS tf.language` and so can never label that pair `ambiguous` in
    // the first place. The first version dropped the language here and joined on
    // name alone, which re-extracted callers in every language sharing a common
    // name (render / update / init / handler / run / main) for no reachable
    // edge — harmless to the graph, pure cost on a round that is deliberately
    // uncapped, and pure node-id churn.
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_fanout_after;
         CREATE TEMP TABLE cg_fanout_after AS
           SELECT n.name AS nm, f.language AS lang, COUNT(*) AS cnt
           FROM nodes n
           JOIN files f ON f.id = n.file_id
           JOIN cg_fanout_paths p ON p.path = f.path
           GROUP BY n.name, f.language;
         CREATE INDEX cg_fanout_after_k ON cg_fanout_after(nm, lang);
         DROP TABLE IF EXISTS temp.cg_fanout_up;
         CREATE TEMP TABLE cg_fanout_up AS
           SELECT a.nm AS nm, a.lang AS lang
           FROM cg_fanout_after a
           LEFT JOIN cg_fanout_before b ON b.nm = a.nm AND b.lang IS a.lang
           WHERE a.cnt > COALESCE(b.cnt, 0);
         CREATE INDEX cg_fanout_up_k ON cg_fanout_up(nm, lang);",
    )?;

    // BOTH relations `CONF_CASE` can label `ambiguous`, not just `calls`:
    // `classify_edge_confidence` binds its `CONF_WHERE` parameters as
    // `params![REL_CALLS, REL_REFERENCES, ...]`. A `references` edge — a type
    // mentioned in a signature, say — fans out to every same-name candidate
    // exactly as a call does, so leaving it out repaired one population and
    // silently left the other. Both constants come from `domain.rs` so this set
    // cannot drift from `CONF_CASE`'s again; found by pre-ship review, which
    // measured 14 of 35 ambiguous edges in this repo's own index as
    // `references`.
    //
    // `CROSS JOIN` in scope-first order for the same reason every post pass
    // spells it that way (see `PostPassScope::is_global`): SQLite has no
    // statistics for a temp table and will otherwise drive from
    // `idx_edges_relation`, scanning every `calls` edge in the repo to use a
    // handful of names as a bloom filter.
    let sql = format!(
        "SELECT DISTINCT f.path
         FROM cg_fanout_up u
         CROSS JOIN nodes tgt ON tgt.name = u.nm
         CROSS JOIN files tf ON tf.id = tgt.file_id AND tf.language IS u.lang
         CROSS JOIN edges e ON e.target_id = tgt.id
                           AND e.relation IN ('{REL_CALLS}', '{REL_REFERENCES}')
                           AND e.confidence = '{CONF_AMBIGUOUS}'
         CROSS JOIN nodes src ON src.id = e.source_id
         CROSS JOIN files f ON f.id = src.file_id
         WHERE f.path NOT IN (SELECT path FROM cg_fanout_paths)
           -- A same-file edge can be `ambiguous` too (a Rust call on an untyped
           -- receiver, D#162), but its target was chosen from the caller's own
           -- file, which a definition elsewhere does not change.
           AND src.file_id <> tgt.file_id
           -- A re-export's several targets (`\"rx\"`, D#132) are decided by
           -- the path, not the bare name: the re-export arm below judges them.
           AND (e.metadata IS NULL OR e.metadata NOT LIKE '%\"rx\"%')"
    );
    // A Rust `use crate::a::widget` bound by name to another file's `widget`,
    // because `a` had none (D#124 F9): the import edge is no `calls` edge and
    // is not `ambiguous`, so the half above misses it, and its call follows the
    // import. When `widget` gains a definition, re-extract the importer if its
    // import points outside the module file its path names — a rebuild binds
    // the named module's new item.
    let stale_use_sql = format!(
        "SELECT DISTINCT f.path, tf.path, e.metadata
         FROM cg_fanout_up u
         CROSS JOIN nodes tgt ON tgt.name = u.nm
         CROSS JOIN files tf ON tf.id = tgt.file_id AND tf.language IS u.lang
         CROSS JOIN edges e ON e.target_id = tgt.id
                           AND e.relation = '{REL_IMPORTS}'
                           AND e.metadata LIKE '%\"ru\"%'
         CROSS JOIN nodes src ON src.id = e.source_id
         CROSS JOIN files f ON f.id = src.file_id
         WHERE f.path NOT IN (SELECT path FROM cg_fanout_paths)"
    );
    // The same import bound to the `<external>` sentinel because no item of
    // that name existed when it was resolved: re-extract the importer when the
    // module file its path names now defines one. A rebuild binds that item,
    // and until the importer is re-extracted the sentinel import also prunes
    // its bare calls (`prune_import_contradicted_call_edges`).
    let sentinel_use_sql = format!(
        "SELECT DISTINCT f.path, u.nm, e.metadata
         FROM cg_fanout_up u
         CROSS JOIN nodes tgt ON tgt.name = u.nm
         CROSS JOIN files tf ON tf.id = tgt.file_id AND tf.path = '<external>'
         CROSS JOIN edges e ON e.target_id = tgt.id
                           AND e.relation = '{REL_IMPORTS}'
                           AND e.metadata LIKE '%\"ru\"%'
         CROSS JOIN nodes src ON src.id = e.source_id
         CROSS JOIN files f ON f.id = src.file_id AND f.language IS u.lang
         WHERE f.path NOT IN (SELECT path FROM cg_fanout_paths)"
    );
    // D9 (2026-09-29 usage evaluation): a Python or JS/TS import whose module
    // did not define the name — `from flask import url_for` through the
    // re-export in `flask/__init__.py`, `export { x } from './helpers'` — is
    // decided by the project-wide name pool: it took the `<external>` sentinel
    // named after the symbol, or bound another file's definition. A rebuild
    // binds the definition this run added, and the stale import went on to
    // prune the call a rebuild has (`prune_import_contradicted_call_edges`).
    // Rows whose import found its name in the module it names are dropped
    // below, in Rust, where the module paths can be resolved. Rust `use` paths
    // follow the stricter rules of the arms above.
    let module_import_sql = format!(
        "SELECT DISTINCT f.path, f.language, e.metadata, tf.path, u.lang
         FROM cg_fanout_up u
         CROSS JOIN nodes tgt ON tgt.name = u.nm AND tgt.type <> 'external_module'
         CROSS JOIN files tf ON tf.id = tgt.file_id
         CROSS JOIN edges e ON e.target_id = tgt.id AND e.relation = '{REL_IMPORTS}'
         CROSS JOIN nodes src ON src.id = e.source_id
         CROSS JOIN files f ON f.id = src.file_id
                           AND f.language IN ('python', 'javascript', 'typescript', 'tsx')
         WHERE u.nm <> '<module>'
           AND f.path NOT IN (SELECT path FROM cg_fanout_paths)"
    );
    // D10A's fallback: `from .helpers import url_for` binds `helpers.py`'s
    // `<module>` while that file defines no `url_for`, and the edge keeps no
    // name to look for. Re-extract such importers of a file of this run that
    // gained a definition; a rebuild binds the name if it is the one added.
    let module_fallback_sql = format!(
        "SELECT DISTINCT f.path
         FROM cg_fanout_paths p
         CROSS JOIN files tf ON tf.path = p.path AND tf.language = 'python'
         CROSS JOIN nodes tgt ON tgt.file_id = tf.id AND tgt.name = '<module>'
         CROSS JOIN edges e ON e.target_id = tgt.id AND e.relation = '{REL_IMPORTS}'
                           AND e.metadata LIKE '%\"python_module\":\".%'
                           AND e.metadata NOT LIKE '%\"is_module_import\":true%'
         CROSS JOIN nodes src ON src.id = e.source_id
         CROSS JOIN files f ON f.id = src.file_id
         WHERE f.path NOT IN (SELECT path FROM cg_fanout_paths)
           AND EXISTS (
               SELECT 1 FROM nodes n
               CROSS JOIN cg_fanout_up u ON u.nm = n.name AND u.lang IS tf.language
               WHERE n.file_id = tf.id AND n.name <> '<module>'
           )"
    );
    // A Rust call a `use` anchored in a crate and bound outside the module its
    // path names (`Anchored::Elsewhere`, marked `"rx"` with the crate's
    // directory, D#132): a new definition of that name in the same crate may be
    // the one the path names, or a closer re-export; a rebuild binds that one.
    let reexport_sql = format!(
        "SELECT DISTINCT f.path, e.metadata, tgt.id, tgt.name, tf.path, tgt.qualified_name
         FROM cg_fanout_up u
         CROSS JOIN nodes tgt ON tgt.name = u.nm
         CROSS JOIN files tf ON tf.id = tgt.file_id AND tf.language IS u.lang
         CROSS JOIN edges e ON e.target_id = tgt.id
                           AND e.relation = '{REL_CALLS}'
                           AND e.metadata LIKE '%\"rx\"%'
         CROSS JOIN nodes src ON src.id = e.source_id
         CROSS JOIN files f ON f.id = src.file_id
         WHERE f.path NOT IN (SELECT path FROM cg_fanout_paths)
           AND EXISTS (
               SELECT 1 FROM cg_fanout_paths p
               CROSS JOIN files nf ON nf.path = p.path
               CROSS JOIN nodes nn ON nn.file_id = nf.id AND nn.name = u.nm
               WHERE substr(p.path, 1, length(json_extract(e.metadata, '$.rx')))
                     = json_extract(e.metadata, '$.rx')
           )"
    );
    // Collected into a Result first so the temps are dropped on the error path
    // too, not only on success — `?` here would leak all four.
    let collected = (|| -> Result<Vec<String>> {
        let mut stmt = conn.prepare(&sql)?;
        let mut paths: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let reexported: Vec<ReexportRow> = conn
            .prepare(&reexport_sql)?
            .query_map([], |row| {
                Ok(ReexportRow {
                    caller: row.get(0)?,
                    metadata: row.get(1)?,
                    target: row.get(2)?,
                    name: row.get(3)?,
                    target_path: row.get(4)?,
                    target_qn: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !reexported.is_empty() {
            paths.extend(reexport_callers_to_refresh(conn, &reexported, crates)?);
            paths.sort_unstable();
            paths.dedup();
        }
        let uses: Vec<(String, String, String)> = conn
            .prepare(&stale_use_sql)?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let unbound: Vec<(String, String, String)> = conn
            .prepare(&sentinel_use_sql)?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !uses.is_empty() || !unbound.is_empty() {
            let all_file_paths: HashSet<String> = conn
                .prepare("SELECT path FROM files")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<_, _>>()?;
            let named_files = |importer: &str, metadata: &str| {
                serde_json::from_str::<serde_json::Value>(metadata)
                    .ok()
                    .and_then(|meta| rust_use_files(&meta, importer, &all_file_paths, crates))
            };
            for (importer, target_file, metadata) in uses {
                if named_files(&importer, &metadata)
                    .is_some_and(|files| !files.contains(&target_file))
                {
                    paths.push(importer);
                }
            }
            let mut defines = conn.prepare(
                "SELECT 1 FROM nodes n JOIN files f ON f.id = n.file_id
                 WHERE n.name = ?1 AND f.path = ?2 LIMIT 1",
            )?;
            for (importer, name, metadata) in unbound {
                let Some(files) = named_files(&importer, &metadata) else {
                    continue;
                };
                for file in files {
                    if defines.exists(rusqlite::params![name, file])? {
                        paths.push(importer);
                        break;
                    }
                }
            }
            paths.sort_unstable();
            paths.dedup();
        }
        let imports: Vec<ModuleImportRow> = conn
            .prepare(&module_import_sql)?
            .query_map([], |row| {
                Ok(ModuleImportRow {
                    importer: row.get(0)?,
                    language: row.get(1)?,
                    metadata: row.get(2)?,
                    target_path: row.get(3)?,
                    added_language: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let fallbacks: Vec<String> = conn
            .prepare(&module_fallback_sql)?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !imports.is_empty() || !fallbacks.is_empty() {
            paths.extend(module_importers_to_refresh(conn, &imports)?);
            paths.extend(fallbacks);
            paths.sort_unstable();
            paths.dedup();
        }
        Ok(paths)
    })();
    drop_fanout_temps(conn)?;
    collected
}

/// One Python or JS/TS `imports` edge into a name that gained a definition
/// this run (D9).
struct ModuleImportRow {
    importer: String,
    language: Option<String>,
    metadata: Option<String>,
    target_path: String,
    added_language: Option<String>,
}

/// The importers of `rows` that a rebuild would bind differently: the import
/// took the name pool — the `<external>` sentinel, or a definition outside the
/// file its module names — in a language the added definition can bind. An
/// import that found its name in its own module keeps it, whatever else defines
/// that name.
fn module_importers_to_refresh(
    conn: &rusqlite::Connection,
    rows: &[ModuleImportRow],
) -> Result<Vec<String>> {
    let all_file_paths: HashSet<String> = conn
        .prepare("SELECT path FROM files")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    let python_paths: HashSet<String> = all_file_paths
        .iter()
        .filter(|p| p.ends_with(".py"))
        .cloned()
        .collect();
    let python_module_map = super::python_modules::build_python_module_map(&python_paths);
    let mut out = Vec::new();
    for row in rows {
        let (Some(lang), Some(added)) = (row.language.as_deref(), row.added_language.as_deref())
        else {
            continue;
        };
        if !crate::utils::config::languages_compatible(lang, added) {
            continue;
        }
        if row.target_path == crate::domain::EXTERNAL_FILE_PATH {
            out.push(row.importer.clone());
            continue;
        }
        let Some(meta) = row
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        else {
            continue;
        };
        let named: Option<Vec<String>> =
            if let Some(module) = meta.get("python_module").and_then(|v| v.as_str()) {
                if meta.get("is_module_import").and_then(|v| v.as_bool()) == Some(true) {
                    continue;
                }
                super::python_modules::project_module_files_from(
                    module,
                    &row.importer,
                    &python_module_map,
                )
            } else if let Some(spec) = meta.get("js_module").and_then(|v| v.as_str()) {
                super::js_modules::resolve_js_specifier_path(spec, &row.importer, &all_file_paths)
                    .map(|f| vec![f])
            } else {
                continue;
            };
        if !named.is_some_and(|files| files.contains(&row.target_path)) {
            out.push(row.importer.clone());
        }
    }
    Ok(out)
}

/// One `calls` edge an [`Anchored::Elsewhere`] resolution produced, into a name
/// that gained a definition this run.
struct ReexportRow {
    caller: String,
    metadata: String,
    target: i64,
    name: String,
    target_path: String,
    target_qn: Option<String>,
}

/// The callers of `rows` whose resolution a definition this run added would
/// change: one the call's path admits (owner, arity, visibility) that is in the
/// module file the path names, or shares at least as much of the path as the
/// item the call bound. Re-extracting every caller of a re-exported `new`
/// whenever any `new` appeared in the crate cost 141 files on tokio.
fn reexport_callers_to_refresh(
    conn: &rusqlite::Connection,
    rows: &[ReexportRow],
    crates: &RustCrates,
) -> Result<Vec<String>> {
    // Every Rust function in this run's files, shaped as the resolver reads them.
    let fns = conn
        .prepare(
            "SELECT n.id, n.name, n.signature, n.qualified_name, f.path, n.start_line,
                    n.end_line, substr(n.code_content, 1, 64)
             FROM cg_fanout_paths p
             CROSS JOIN files f ON f.path = p.path AND f.language = 'rust'
             CROSS JOIN nodes n ON n.file_id = f.id AND n.type = 'function'
             ORDER BY f.path",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                (row.get::<_, u32>(5)?, row.get::<_, u32>(6)?),
                row.get::<_, String>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut shapes: HashMap<i64, RustFnShape> = HashMap::new();
    for file in fns.chunk_by(|a, b| a.4 == b.4) {
        let rows: Vec<RustFnRow> = file
            .iter()
            .map(|(id, _, signature, qn, _, lines, code)| RustFnRow {
                id: *id,
                signature: signature.as_deref(),
                qualified_name: qn.as_deref(),
                lines: *lines,
                code,
            })
            .collect();
        shapes.extend(rust_fn_shapes_of_file(&file[0].4, &rows));
    }
    let all_file_paths: HashSet<String> = conn
        .prepare("SELECT path FROM files")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut out = Vec::new();
    for row in rows {
        let Some(CalleeMeta::Path(segments)) = parse_callee_metadata(Some(&row.metadata)) else {
            continue;
        };
        let UseAnchor::At(anchor) =
            rust_use_anchor(Some(&row.metadata), &segments, &row.caller, crates)
        else {
            continue;
        };
        let mut files_of = HashMap::new();
        let bound = match anchor_rank(
            &anchor,
            &row.target_path,
            row.target_qn.as_deref(),
            &all_file_paths,
            &mut files_of,
        ) {
            Some(AnchorRank::Shared(n)) => n,
            _ => usize::MAX,
        };
        let changes = fns.iter().any(|(id, name, _, qn, path, _, _)| {
            *id != row.target
                && *name == row.name
                && path.starts_with(anchor.dir.as_str())
                && shapes.get(id).is_some_and(|shape| {
                    rust_call_shape_admits(Some(&row.metadata), shape)
                        && rust_crate_admits(Some(&row.caller), shape)
                })
                && match anchor_rank(&anchor, path, qn.as_deref(), &all_file_paths, &mut files_of) {
                    Some(AnchorRank::Named) => true,
                    Some(AnchorRank::Shared(n)) => n >= bound,
                    None => false,
                }
        });
        if changes {
            out.push(row.caller.clone());
        }
    }
    Ok(out)
}

pub(super) fn bind_calls_to_imported_targets(
    db: &Database,
    scope: &PostPassScope,
) -> Result<usize> {
    use crate::domain::{REL_CALLS, REL_IMPORTS};

    // Bind in ONE set-based statement. This used to SELECT the pairs and then
    // call `insert_edge_cached` per row from Rust; because the SELECT returns
    // every pair that COULD bind (not just the new ones) and the insert is
    // `INSERT OR IGNORE`, the overwhelming majority of those round trips were
    // no-ops re-proving edges that already existed. Measured on a 1,456-file
    // tree: 10,960 pairs round-tripped to create 3 edges, ~0.24 s — paid on
    // every single-file refresh, i.e. on every query that touches an edited
    // file (indexing audit 2026-08-02 P1-6). The predicate below is the
    // original SELECT verbatim; only the delivery changed, so which edges get
    // created is unaffected — `INSERT OR IGNORE` applies the same
    // `idx_edges_unique` dedup `insert_edge_cached` relied on.
    //
    // A bare call whose name is bound by a unique internal import in the
    // caller's file — and is NOT shadowed by a same-file definition of that
    // name — resolves to the imported node.
    //
    // The unique-import side is MATERIALIZED first (see `build_imports_temp` for
    // why the three post-passes all do this). As an inline derived table it was
    // re-derived while scanning every bare call edge in the index; as an indexed
    // temp table the same join is a probe. The SELECT is the original verbatim —
    // only where the rows come from changed.
    let conn = db.conn();
    conn.execute_batch("DROP TABLE IF EXISTS temp.cg_unique_imports;")?;
    conn.execute(
        "CREATE TEMP TABLE cg_unique_imports AS
         SELECT mn.file_id AS import_file, itn.name AS import_name,
                MIN(itn.id) AS import_target_id
         FROM edges ie
         JOIN nodes mn  ON mn.id  = ie.source_id
         JOIN nodes itn ON itn.id = ie.target_id
         JOIN files itf ON itf.id = itn.file_id
         WHERE ie.relation = ?1
           AND itf.path <> '<external>'
         GROUP BY mn.file_id, itn.name
         HAVING COUNT(DISTINCT itn.id) = 1",
        rusqlite::params![REL_IMPORTS],
    )?;
    conn.execute_batch(
        "CREATE INDEX cg_unique_imports_k ON cg_unique_imports(import_file, import_name);",
    )?;
    // The predicate is one string, spent by both drivers, because the two must
    // agree by construction — a scoped copy that drifted from the global one is
    // the "two surfaces, one question, two answers" shape this codebase keeps
    // paying for. Only the FROM clause ahead of it changes.
    const BIND_PREDICATE: &str = "
             WHERE e.relation = ?1
               -- bare, or bare as far as resolution goes (a D10B `ur` mark)
               AND (e.metadata IS NULL OR e.metadata = ''
                    OR json_extract(e.metadata, '$.ur') IS NOT NULL)
               AND e.source_id <> it.import_target_id
               AND NOT EXISTS (
                   SELECT 1 FROM nodes ln
                   WHERE ln.file_id = sn.file_id AND ln.name = tn.name
               )";
    let sql = if scope.is_global() {
        format!(
            "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
             SELECT DISTINCT e.source_id, it.import_target_id, ?2, NULL
             FROM edges e
             JOIN nodes sn ON sn.id = e.source_id
             JOIN nodes tn ON tn.id = e.target_id
             JOIN cg_unique_imports it
               ON it.import_file = sn.file_id
              AND it.import_name = tn.name
             {BIND_PREDICATE}"
        )
    } else {
        // Two arms: the caller moved, or the callee moved. CROSS JOIN order is
        // load-bearing (see PostPassScope::is_global).
        format!(
            "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
             SELECT DISTINCT src_id, tgt_id, ?2, NULL FROM (
               SELECT e.source_id AS src_id, it.import_target_id AS tgt_id
               FROM cg_scope_files s
               CROSS JOIN nodes sn ON sn.file_id = s.fid
               CROSS JOIN edges e ON e.source_id = sn.id
               CROSS JOIN nodes tn ON tn.id = e.target_id
               CROSS JOIN cg_unique_imports it
                 ON it.import_file = sn.file_id
                AND it.import_name = tn.name
               {BIND_PREDICATE}
               UNION
               SELECT e.source_id AS src_id, it.import_target_id AS tgt_id
               FROM cg_scope_files s
               CROSS JOIN nodes tn ON tn.file_id = s.fid
               CROSS JOIN edges e ON e.target_id = tn.id
               CROSS JOIN nodes sn ON sn.id = e.source_id
               CROSS JOIN cg_unique_imports it
                 ON it.import_file = sn.file_id
                AND it.import_name = tn.name
               {BIND_PREDICATE}
             )"
        )
    };
    let inserted = {
        let mut stmt = conn.prepare(&sql)?;
        stmt.execute(rusqlite::params![REL_CALLS, REL_CALLS])?
    };
    conn.execute_batch("DROP TABLE IF EXISTS temp.cg_unique_imports;")?;
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
pub(super) fn prune_import_contradicted_call_edges(
    db: &Database,
    scope: &PostPassScope,
) -> Result<usize> {
    use crate::domain::REL_CALLS;
    let conn = db.conn();
    build_imports_temp(conn, true)?;
    const PRUNE_PREDICATE: &str = "
            WHERE e.relation = ?1
              -- bare, or bare as far as resolution goes (a D10B `ur` mark)
              AND (e.metadata IS NULL OR e.metadata = ''
                   OR json_extract(e.metadata, '$.ur') IS NOT NULL)
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
              -- ...but only trust that instr=0 when code_content was NOT truncated.
              -- truncate_code_content (parser) caps at max_code_content_len (4096)
              -- and appends a literal three-dot sentinel; a qualified .name( call
              -- beyond the cap is sliced off, making instr a false negative that
              -- false-prunes a real edge. Truncated (ends in the sentinel) -> keep
              -- the edge (safe direction, same bias as the guard above). L12.
              AND sn.code_content NOT LIKE '%...'
              -- caller's file imports the SAME name bound to a DIFFERENT node
              AND EXISTS (
                  SELECT 1 FROM cg_imports i
                  WHERE i.fid = sn.file_id
                    AND i.nm = tn.name
                    AND i.tid <> e.target_id
              )
              -- ... and does NOT import THIS target (so it is contradicted)
              AND NOT EXISTS (
                  SELECT 1 FROM cg_imports i2
                  WHERE i2.fid = sn.file_id
                    AND i2.tid = e.target_id
              )";
    let sql = if scope.is_global() {
        format!(
            "DELETE FROM edges WHERE id IN (
            SELECT e.id FROM edges e
            JOIN nodes sn ON sn.id = e.source_id
            JOIN nodes tn ON tn.id = e.target_id
            {PRUNE_PREDICATE}
        )"
        )
    } else {
        format!(
            "DELETE FROM edges WHERE id IN (
            SELECT e.id
            FROM cg_scope_files s
            CROSS JOIN nodes sn ON sn.file_id = s.fid
            CROSS JOIN edges e ON e.source_id = sn.id
            CROSS JOIN nodes tn ON tn.id = e.target_id
            {PRUNE_PREDICATE}
            UNION
            SELECT e.id
            FROM cg_scope_files s
            CROSS JOIN nodes tn ON tn.file_id = s.fid
            CROSS JOIN edges e ON e.target_id = tn.id
            CROSS JOIN nodes sn ON sn.id = e.source_id
            {PRUNE_PREDICATE}
        )"
        )
    };
    let removed = conn.execute(&sql, rusqlite::params![REL_CALLS])?;
    drop_imports_temp(conn)?;
    Ok(removed)
}

/// Phase 2e: assign `edges.confidence` for cross-file `calls`/`references` edges
/// in one set-based pass, run after all edges exist (post Phase-2 + pending sweep
/// + prune). Purely additive metadata — no edge is added or removed.
///
/// The column defaults to `extracted`, so every precise insert (same-file
/// resolution + the structural relations imports/inherits/implements/routes_to/
/// exports) is correct without touching its insert site. This pass only
/// DOWNGRADES cross-file `calls`/`references` edges, and the same-file Rust
/// method calls on an untyped receiver (D#162) and Python calls on an untyped
/// receiver (D10B, see `CONF_WHERE`) — the by-name-resolved class:
///   - `inferred`  when the target name is unique among same-language nodes;
///   - `ambiguous` when >1 same-language node shares the target name (the
///     by-name resolution could not pick uniquely — the known false-positive
///     class: bare_name_call_qualifier / method_call_edge_drops /
///     value_reference_candidate_gen).
///
/// Idempotent: re-running recomputes from current node state, so an `inferred`
/// edge becomes `ambiguous` when a duplicate-named sibling is later added (and
/// back when it is removed). Returns the number of edges downgraded.
pub(super) fn classify_edge_confidence(db: &Database, scope: &PostPassScope) -> Result<usize> {
    use crate::domain::{CONF_AMBIGUOUS, CONF_INFERRED, REL_CALLS, REL_REFERENCES};
    let conn = db.conn();
    build_imports_temp(conn, false)?;
    // The same-name count is materialized for the same reason: as an inline
    // derived table it was rebuilt (temp B-tree GROUP BY over every node) and
    // then scanned as the driver, once per run.
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_namecount;
         CREATE TEMP TABLE cg_namecount AS
           SELECT n.name AS nm, f.language AS lang, COUNT(*) AS cnt
           FROM nodes n JOIN files f ON f.id = n.file_id
           GROUP BY n.name, f.language;
         CREATE INDEX cg_namecount_k ON cg_namecount(nm, lang);",
    )?;
    // The classification itself — one expression, spent by every driver below.
    // `nc` is the same-name/same-language count; `e`, `src`, `tgt` are the edge
    // and its two endpoints. Kept as one constant because a scoped copy that
    // drifted from the global one would label the same edge two ways depending
    // on how the index happened to be grown, which is the incremental-vs-rebuild
    // divergence class this pipeline has paid for twice already.
    const CONF_CASE: &str = "CASE
                 WHEN nc.cnt > 1
                      -- ... UNLESS the caller's file explicitly imports THIS exact
                      -- target. An import binds the bare name to one node, so the
                      -- edge is import-resolved (v0.59 bind_calls_to_imported_targets),
                      -- not a bare-name guess among same-name siblings. Without this
                      -- exception the precise binding is relabeled `ambiguous` and
                      -- hidden by the confidence floor, so callgraph/impact show NO
                      -- callee for a call the resolver bound exactly — for any name
                      -- defined in >=2 same-language files (process/handler/run/init).
                      -- Mirrors the corroboration test in prune_import_contradicted_call_edges.
                      AND NOT EXISTS (
                          SELECT 1 FROM cg_imports i
                          WHERE i.fid = src.file_id
                            AND i.tid = tgt.id
                      )
                      -- ... and UNLESS the edge was resolved by a TYPE/PATH callee
                      -- qualifier (self / stype / rtype / super / path), or through a
                      -- renamed JS import to the file its specifier names (imp,
                      -- D#120). Those bind the call
                      -- by a structural signal (the receiver's impl type, or the
                      -- module path), not by a bare-name guess among same-name
                      -- siblings — so a duplicate bare name must not relabel them
                      -- `ambiguous` and hide them under the confidence floor (M1).
                      -- `chain` / `recv` are NOT exempt: they resolve by method
                      -- uniqueness or fall back to bare, so a duplicate name there is
                      -- genuinely ambiguous. NULL metadata (bare) also stays eligible.
                      -- A typed call its class did not decide carries `amb`
                      -- (`ambiguous_meta`) and is classified like a bare one.
                      AND (json_extract(e.metadata, '$.q') IS NULL
                           OR json_extract(e.metadata, '$.q') NOT IN ('self', 'stype', 'rtype', 'super', 'path', 'imp')
                           OR json_extract(e.metadata, '$.amb') IS NOT NULL)
                 THEN ?3 ELSE ?4 END";
    // Cross-file edges, plus one same-file class (D#162): a Rust method call
    // whose receiver the source leaves untyped (`q` member / chain with no
    // `rt`). `amb` would mark a typed one its type did not decide, but only
    // `rtype` / `super` / path calls carry it today, so for member / chain that
    // arm only guards a future shape; a typed member call whose type lacks the
    // method still binds by name and keeps `extracted`. Method dispatch
    // follows the receiver's type, not the caller's file, so binding the file's
    // own `m` is a by-name guess like a cross-file one — and a skewed one, since
    // a wrapper forwards `self.inner.m()` to a field of another type while its
    // file defines `m` for the wrapper. Measured on tokio-1.41.1 by the SCIP
    // oracle: 48 of 233 such edges right when the name has another definition
    // (below the `ambiguous` tier's 50%), 49 of 54 when it has none. Rust only:
    // the same class was 75% right on hono, 91% on flask and 48% on leveldb,
    // where relabelling it hid as many correct edges as wrong ones or more. A
    // local-variable receiver (`recv`) stays out for the same reason (53 of 76).
    //
    // Python has its own untyped class (D10B, `ur`, parser `member.rs`): a
    // call on an attribute of `self` or on a name a relative import binds. It
    // carries no `q` at all, so it resolved as a bare call and bound the file's
    // own method by name. Measured by the oracle on flask + networkx: of 33
    // `self.x.f()` binds 14 right (6 of 24 where the name has another
    // definition), of 28 relative-import binds 4 credited and none right on
    // reading the code. A local receiver (`q` member) stays out: 63 of 80 right.
    const CONF_WHERE: &str = "
             WHERE e.relation IN (?1, ?2)
               AND (src.file_id <> tgt.file_id
                    OR (tf.language = 'rust'
                        AND json_extract(e.metadata, '$.q') IN ('member', 'chain')
                        AND (json_extract(e.metadata, '$.rt') IS NULL
                             OR json_extract(e.metadata, '$.amb') IS NOT NULL))
                    OR (tf.language = 'python'
                        AND json_extract(e.metadata, '$.ur') IS NOT NULL))";
    // Which edges to reconsider. The scoped form is three arms — the caller
    // moved, the callee moved, or the callee's NAME changed how many nodes
    // share it. Only the third needs `cg_scope_names`, and only this pass has
    // a globally-varying input that makes it necessary.
    let driver = if scope.is_global() {
        format!(
            "SELECT e.id AS eid, {CONF_CASE} AS conf
             FROM edges e
             JOIN nodes src ON src.id = e.source_id
             JOIN nodes tgt ON tgt.id = e.target_id
             JOIN files tf ON tf.id = tgt.file_id
             JOIN cg_namecount nc ON nc.nm = tgt.name AND nc.lang IS tf.language
             {CONF_WHERE}"
        )
    } else {
        format!(
            "SELECT eid, conf FROM (
               SELECT e.id AS eid, {CONF_CASE} AS conf
               FROM cg_scope_files s
               CROSS JOIN nodes src ON src.file_id = s.fid
               CROSS JOIN edges e ON e.source_id = src.id
               CROSS JOIN nodes tgt ON tgt.id = e.target_id
               CROSS JOIN files tf ON tf.id = tgt.file_id
               CROSS JOIN cg_namecount nc ON nc.nm = tgt.name AND nc.lang IS tf.language
               {CONF_WHERE}
               UNION
               SELECT e.id AS eid, {CONF_CASE} AS conf
               FROM cg_scope_files s
               CROSS JOIN nodes tgt ON tgt.file_id = s.fid
               CROSS JOIN edges e ON e.target_id = tgt.id
               CROSS JOIN nodes src ON src.id = e.source_id
               CROSS JOIN files tf ON tf.id = tgt.file_id
               CROSS JOIN cg_namecount nc ON nc.nm = tgt.name AND nc.lang IS tf.language
               {CONF_WHERE}
               UNION
               SELECT e.id AS eid, {CONF_CASE} AS conf
               FROM cg_scope_names sname
               CROSS JOIN nodes tgt ON tgt.name = sname.nm
               CROSS JOIN edges e ON e.target_id = tgt.id
               CROSS JOIN nodes src ON src.id = e.source_id
               CROSS JOIN files tf ON tf.id = tgt.file_id
               CROSS JOIN cg_namecount nc ON nc.nm = tgt.name AND nc.lang IS tf.language
               {CONF_WHERE}
             )"
        )
    };
    conn.execute_batch("DROP TABLE IF EXISTS temp.cg_conf;")?;
    conn.execute_batch("CREATE TEMP TABLE cg_conf (eid INTEGER PRIMARY KEY, conf TEXT);")?;
    conn.execute(
        &format!("INSERT OR REPLACE INTO cg_conf (eid, conf) {driver}"),
        rusqlite::params![REL_CALLS, REL_REFERENCES, CONF_AMBIGUOUS, CONF_INFERRED],
    )?;
    // Write only where the label actually moves. The join above is what costs;
    // suppressing the no-op writes is worth 65 ms of 290 on the django corpus,
    // and it makes the returned count mean "edges whose confidence CHANGED"
    // rather than "edges the pass looked at" — the number the log line claims.
    let downgraded = conn.execute(
        "UPDATE edges
            SET confidence = (SELECT c.conf FROM cg_conf c WHERE c.eid = edges.id)
          WHERE id IN (SELECT eid FROM cg_conf)
            AND confidence IS NOT (SELECT c.conf FROM cg_conf c WHERE c.eid = edges.id)",
        [],
    )?;
    conn.execute_batch("DROP TABLE IF EXISTS temp.cg_conf;")?;
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_namecount;
         DROP TABLE IF EXISTS temp.cg_imports;",
    )?;
    Ok(downgraded)
}

/// This project's Cargo packages (D#132): every `[package] name` found in a
/// `Cargo.toml` at or under the root, with `-` normalized to `_` (the module
/// path spelling; see [`path_filter_candidates`] for why the names are needed),
/// and each one's library directory, so a path a `use` roots at a package name
/// (`use tokio::sync::oneshot::channel` from `tests/` or another package) is
/// looked for in that package only. Scanned once per index run.
#[derive(Debug, Default, Clone)]
pub(super) struct RustCrates {
    names: HashSet<String>,
    /// Package name → `<manifest dir>/src/`, root-relative, `/`-separated.
    src_dirs: HashMap<String, String>,
    /// `src/` directory → the top-level modules its lib.rs and main.rs declare,
    /// for each package that has both (D#136).
    root_mods: std::collections::BTreeMap<String, RootMods>,
    /// Some manifest names a crate the scan could not read
    /// ([`CargoNames::unreadable`]): a path rooted at a crate name no package
    /// answers to may be that one, so it resolves by name, as before D#132.
    opaque: bool,
    /// `(key, package)` dependency renames, until the walk resolves them.
    renames: Vec<(String, String)>,
}

/// Which of a package's two crates a file is compiled into (D#136): the one
/// whose root is `src/lib.rs`, the one whose root is `src/main.rs`, or both or
/// not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RootSide {
    Lib,
    Main,
    Either,
}

/// The `mod` items at the top level of a package's lib.rs and main.rs, or
/// `Opaque` when one of them may declare a module the resolver cannot see: a
/// `mod` inside a macro call or definition, or a `#[path]` one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootMods {
    Known {
        lib: std::collections::BTreeSet<String>,
        main: std::collections::BTreeSet<String>,
    },
    Opaque,
}

impl RustCrates {
    pub(super) fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    fn src_dir(&self, name: &str) -> Option<&str> {
        self.src_dirs.get(name).map(String::as_str)
    }

    /// The crate of the package at `dir` that `path` is compiled into. lib.rs
    /// and main.rs are their own roots whatever the manifest says; any other
    /// file belongs to the root that alone declares its top-level module.
    pub(super) fn side_of(&self, dir: &str, path: &str) -> RootSide {
        let Some(rel) = path.strip_prefix(dir) else {
            return RootSide::Either;
        };
        match rel {
            "lib.rs" => return RootSide::Lib,
            "main.rs" => return RootSide::Main,
            _ => {}
        }
        let Some(RootMods::Known { lib, main }) = self.root_mods.get(dir) else {
            return RootSide::Either;
        };
        let top = rel.split('/').next().unwrap_or(rel);
        let top = top.strip_suffix(".rs").unwrap_or(top);
        match (lib.contains(top), main.contains(top)) {
            (true, false) => RootSide::Lib,
            (false, true) => RootSide::Main,
            _ => RootSide::Either,
        }
    }

    /// [`Self::root_mods`] as the JSON the index stores
    /// (`META_KEY_RUST_ROOT_MODS`), so the next incremental run can tell which
    /// files a root's `mod` edit moved to another crate.
    pub(super) fn root_mods_json(&self) -> String {
        let map: serde_json::Map<String, serde_json::Value> = self
            .root_mods
            .iter()
            .map(|(dir, mods)| (dir.clone(), root_mods_value(mods)))
            .collect();
        serde_json::Value::Object(map).to_string()
    }
}

fn root_mods_value(mods: &RootMods) -> serde_json::Value {
    match mods {
        RootMods::Known { lib, main } => serde_json::json!({ "lib": lib, "main": main }),
        RootMods::Opaque => serde_json::json!({ "opaque": true }),
    }
}

/// The files of `files` whose crate a change of root `mod` items moved between
/// the stored [`RustCrates::root_mods_json`] and `current` (D#136): under a
/// top-level module one root's set gained or lost, or anywhere in a package
/// whose record appeared, went away, or is or was `Opaque`. An absent or
/// unreadable record reads as no package recorded.
pub(super) fn rust_root_mod_moves<'a>(
    stored: Option<&str>,
    current: &RustCrates,
    files: impl Iterator<Item = &'a String>,
) -> Vec<String> {
    let stored: serde_json::Map<String, serde_json::Value> = stored
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    // Directory → the top-level modules moved, or None for the whole package.
    let mut moved: HashMap<&str, Option<HashSet<String>>> = HashMap::new();
    let dirs: std::collections::BTreeSet<&str> = stored
        .keys()
        .map(String::as_str)
        .chain(current.root_mods.keys().map(String::as_str))
        .collect();
    for dir in dirs {
        let now = current.root_mods.get(dir).map(root_mods_value);
        let was = stored.get(dir);
        if now.as_ref() == was {
            continue;
        }
        let names = |v: &serde_json::Value, side: &str| -> Option<HashSet<String>> {
            v.get(side)?
                .as_array()?
                .iter()
                .map(|s| s.as_str().map(String::from))
                .collect()
        };
        let tops = match (now.as_ref(), was) {
            (Some(now), Some(was)) => (|| {
                let mut tops = HashSet::new();
                for side in ["lib", "main"] {
                    let (a, b) = (names(now, side)?, names(was, side)?);
                    tops.extend(a.symmetric_difference(&b).cloned());
                }
                Some(tops)
            })(),
            _ => None,
        };
        moved.insert(dir, tops);
    }
    if moved.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = files
        .filter(|path| {
            moved.iter().any(|(dir, tops)| {
                let Some(rel) = path.strip_prefix(dir) else {
                    return false;
                };
                if !rel.ends_with(".rs") || matches!(rel, "lib.rs" | "main.rs") {
                    return false;
                }
                let top = rel.split('/').next().unwrap_or(rel);
                let top = top.strip_suffix(".rs").unwrap_or(top);
                tops.as_ref().is_none_or(|t| t.contains(top))
            })
        })
        .cloned()
        .collect();
    out.sort();
    out
}

/// The top-level `mod` names a crate root file declares, or None when it may
/// declare one the names do not show (see [`RootMods::Opaque`]).
///
/// Remembered by content for the process: an incremental run collects the
/// crates up to three times (its record check, `index_files`, the fan-out
/// round), and parsing a long main.rs again each time was most of what an edit
/// of it cost.
fn root_mod_names(source: &str) -> Option<std::collections::BTreeSet<String>> {
    type Memo = HashMap<[u8; 32], Option<std::collections::BTreeSet<String>>>;
    static MEMO: std::sync::Mutex<Option<Memo>> = std::sync::Mutex::new(None);
    let key = *blake3::hash(source.as_bytes()).as_bytes();
    if let Some(hit) = MEMO
        .lock()
        .ok()
        .and_then(|m| m.as_ref().and_then(|m| m.get(&key).cloned()))
    {
        return hit;
    }
    let names = parse_root_mod_names(source);
    if let Ok(mut memo) = MEMO.lock() {
        let memo = memo.get_or_insert_with(HashMap::new);
        if memo.len() >= 64 {
            memo.clear();
        }
        memo.insert(key, names.clone());
    }
    names
}

fn parse_root_mod_names(source: &str) -> Option<std::collections::BTreeSet<String>> {
    let tree = crate::parser::treesitter::parse_tree(source, "rust").ok()?;
    let root = tree.root_node();
    let mut names = std::collections::BTreeSet::new();
    let mut path_attr = false;
    let has_word = |text: &str, word: &str| {
        text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| w == word)
    };
    let mut cursor = root.walk();
    for item in root.named_children(&mut cursor) {
        let text = &source[item.byte_range()];
        match item.kind() {
            "attribute_item" => {
                path_attr |= has_word(text, "path");
                continue;
            }
            "mod_item" => {
                if path_attr {
                    return None;
                }
                let name = item.child_by_field_name("name")?;
                let name = &source[name.byte_range()];
                names.insert(name.strip_prefix("r#").unwrap_or(name).to_string());
            }
            "macro_invocation" | "macro_definition" | "expression_statement"
                if has_word(text, "mod") =>
            {
                return None;
            }
            _ => {}
        }
        path_attr = false;
    }
    Some(names)
}

/// [`RustCrates`] of the project at `root`: one walk over its manifests.
///
/// The walk is depth-limited (a crate manifest lives at the project root or one
/// or two directories down — `crates/foo/`, `scripts/poc/`) and skips build /
/// dependency directories, so it never becomes a full-tree scan.
pub(super) fn collect_rust_crates(root: &Path) -> RustCrates {
    const MAX_DEPTH: usize = 3;
    const SKIP_DIRS: &[&str] = &["node_modules", "vendor", "target", "bower_components"];

    fn walk(dir: &Path, root: &Path, depth: usize, out: &mut RustCrates) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if depth >= MAX_DEPTH || name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref())
                {
                    continue;
                }
                walk(&entry.path(), root, depth + 1, out);
            } else if name == "Cargo.toml" {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    let names = parse_cargo_names(&text);
                    out.opaque |= names.unreadable;
                    out.renames.extend(names.renames);
                    if let Some(pkg) = names.package {
                        let rel = dir
                            .strip_prefix(root)
                            .map(|r| r.to_string_lossy().replace('\\', "/"))
                            .unwrap_or_default();
                        let src = if rel.is_empty() {
                            "src/".to_string()
                        } else {
                            format!("{rel}/src/")
                        };
                        let read = |f: &str| std::fs::read_to_string(dir.join("src").join(f));
                        if let (Ok(lib), Ok(main)) = (read("lib.rs"), read("main.rs")) {
                            let mods = match (root_mod_names(&lib), root_mod_names(&main)) {
                                (Some(lib), Some(main)) => RootMods::Known { lib, main },
                                _ => RootMods::Opaque,
                            };
                            out.root_mods.insert(src.clone(), mods);
                        }
                        // The library's crate name (`[lib] name`, batch-1
                        // review H2) is what `use` paths write; the package's
                        // stays, as a dependency rename looks it up.
                        if let Some(lib) = names.lib {
                            out.src_dirs.insert(lib.clone(), src.clone());
                            out.names.insert(lib);
                        }
                        out.src_dirs.insert(pkg.clone(), src);
                        out.names.insert(pkg);
                    }
                }
            }
        }
    }

    let mut out = RustCrates::default();
    walk(root, root, 0, &mut out);
    // `engine = { package = "core-pkg" }`: `engine` is core-pkg's library in
    // the depending crate. A rename of a package the project does not hold is
    // a dependency's, and names nothing here.
    for (key, pkg) in std::mem::take(&mut out.renames) {
        if let Some(src) = out.src_dirs.get(&pkg).cloned() {
            out.src_dirs.entry(key.clone()).or_insert(src);
            out.names.insert(key);
        }
    }
    out
}

/// The crate names a `Cargo.toml` declares, module spelling (`-` → `_`).
#[derive(Debug, Default, PartialEq)]
struct CargoNames {
    /// `[package] name`.
    package: Option<String>,
    /// `[lib] name`.
    lib: Option<String>,
    /// `(dependency key, package)` of each `package = "…"` rename.
    renames: Vec<(String, String)>,
    /// A package or library name, or a rename, the line scan cannot read: a
    /// top-level dotted or inline `lib`/`package` key, a name that is no
    /// literal, a rename whose package is no literal.
    unreadable: bool,
}

/// Read [`CargoNames`] off a manifest. Deliberately a line scan rather than a
/// TOML parse: the shape that matters is a literal string on one line, and
/// `name.workspace = true` (no literal here) correctly yields nothing. What it
/// cannot read it says so, and the resolver then keeps resolving a path
/// through an unknown crate name by name, as before D#132, instead of dropping
/// it.
fn parse_cargo_names(text: &str) -> CargoNames {
    let mut out = CargoNames::default();
    // The table the line is in: None before any header.
    let mut table: Option<String> = None;
    let literal = |rest: &str| -> Option<String> {
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let value = rest[1..].split(quote).next()?;
        (!value.is_empty()).then(|| value.replace('-', "_"))
    };
    let is_deps = |t: &str| {
        let last = t.rsplit('.').next().unwrap_or(t);
        matches!(
            last,
            "dependencies" | "dev-dependencies" | "build-dependencies"
        )
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            table = Some(
                line.trim_matches(|c| c == '[' || c == ']')
                    .trim()
                    .to_string(),
            );
            continue;
        }
        let key = line
            .split(|c: char| c == '=' || c.is_whitespace())
            .next()
            .unwrap_or_default();
        match table.as_deref() {
            None => {
                if key == "lib"
                    || key == "package"
                    || key.starts_with("lib.")
                    || key.starts_with("package.")
                {
                    out.unreadable = true;
                }
            }
            Some(t @ ("package" | "lib")) => {
                if key == "name" {
                    let rest = &line["name".len()..];
                    match literal(rest) {
                        Some(v) if t == "package" => out.package = Some(v),
                        Some(v) => out.lib = Some(v),
                        None => out.unreadable = true,
                    }
                }
            }
            Some(t) if is_deps(t) => {
                // `engine = { path = "..", package = "core-pkg" }`
                let Some(brace) = line.find('{') else {
                    continue;
                };
                let table = &line[brace..];
                let Some(at) = table.match_indices("package").map(|(i, _)| i).find(|&i| {
                    table[..i].ends_with(|c: char| c == '{' || c == ',' || c.is_whitespace())
                        && table[i + "package".len()..].trim_start().starts_with('=')
                }) else {
                    continue;
                };
                match literal(&table[at + "package".len()..]) {
                    Some(pkg) => out.renames.push((key.replace('-', "_"), pkg)),
                    None => out.unreadable = true,
                }
            }
            Some(t) => {
                // `[dependencies.engine]` … `package = "core-pkg"`
                let Some((deps, dep)) = t.rsplit_once('.') else {
                    continue;
                };
                if is_deps(deps) && key == "package" {
                    match literal(&line["package".len()..]) {
                        Some(pkg) => out.renames.push((dep.trim().replace('-', "_"), pkg)),
                        None => out.unreadable = true,
                    }
                }
            }
        }
    }
    out
}

/// `name = "..."` of a `Cargo.toml`'s `[package]` table, normalized to the
/// module spelling (`-` → `_`).
#[cfg(test)]
fn parse_cargo_package_name(text: &str) -> Option<String> {
    parse_cargo_names(text).package
}

/// Filter a candidate set down to those matching the Path qualifier:
///   (1) file path contains "/seg1/seg2/" OR starts with "seg1/seg2/", OR
///   (2) qualified_name contains the segment chain joined by `.` as a
///       contiguous segment (anchored on `.` or boundary).
///
/// Storage uses `.` separator for qualified_name (treesitter.rs:582), NOT `::`.
/// Returns the filtered subset; empty result is a meaningful signal
/// (no project candidate matches → caller should drop the edge).
///
/// `crate_roots` holds this project's own Cargo package names (module
/// spelling). A leading segment equal to one of them is stripped exactly like
/// the literal `crate` / `super` / `self` prefixes the parser already removes
/// (`parser::relations::helpers::extract_rust_scoped`): `my_crate::cli::f()` is
/// the standard `bin` → `lib` call and names the crate ROOT, not a directory,
/// so leaving the segment in emptied the filter and dropped the whole class of
/// edges (audit 2026-08-22 P1-1 — `dead-code`/`impact`/`refs` then answered
/// "uncalled" for every `cmd_*` in this repo). Keyed on the real package names
/// so a genuinely foreign `other_crate::x::f()` still drops.
pub(super) fn path_filter_candidates(
    segments: &[String],
    candidates: &[i64],
    node_id_to_path: &std::collections::HashMap<i64, String>,
    db: &crate::storage::db::Database,
    crate_roots: &RustCrates,
) -> anyhow::Result<Vec<i64>> {
    if candidates.is_empty() || segments.is_empty() {
        return Ok(candidates.to_vec());
    }

    // The qualifier AS WRITTEN gets the first say. Stripping unconditionally
    // was a regression: a package whose name is also an ordinary directory name
    // (`core`, `utils`, `parser`, `config` — routine in a Cargo workspace) lost
    // the chain's only discriminating segment, so `utils::helper::go()` degraded
    // from `utils/helper`, which matches one directory, to `helper`, which
    // matches every `helper/` in the tree. Measured: one correct edge became two
    // `inferred` edges whose metadata still read `v:"utils::helper"`.
    let kept = filter_by_segment_chain(segments, candidates, node_id_to_path, db)?;
    if !kept.is_empty() {
        return Ok(kept);
    }

    // Only a chain that matches NOTHING gets the own-crate-root reading. This is
    // the `my_crate::cli::cmd_grep()` case: the leading segment names the crate
    // root rather than a directory, so `my_crate/cli` matches no path and the
    // whole class of `bin` → `lib` edges dropped (audit 2026-08-22 P1-1).
    let Some((first, rest)) = segments.split_first() else {
        return Ok(kept);
    };
    if !crate_roots.contains(first.as_str()) {
        return Ok(kept); // empty — a foreign crate path, caller drops the edge
    }
    if rest.is_empty() {
        // `my_crate::f()` — the root IS the whole qualifier, so nothing is left
        // to constrain the path with. Returning every same-name candidate here
        // published one true edge and N-1 phantoms, all at `inferred`: the
        // `path` qualifier is exempt from the ambiguous downgrade
        // (`classify_edge_confidence`) on the premise that it binds
        // structurally, and on this branch that premise is false. A single
        // candidate is still an unambiguous answer; several are not, and no
        // answer beats a wrong one above the confidence floor.
        return Ok(if candidates.len() == 1 {
            candidates.to_vec()
        } else {
            Vec::new()
        });
    }
    filter_by_segment_chain(rest, candidates, node_id_to_path, db)
}

/// Top-level modules of `std`/`core`/`alloc`. A path that opens with one
/// (`io::Error::new`, `sync::Mutex::new`) and that no `use` of the file explains
/// (a `use` spells the path out, D#132: `parser::relations::rust_use`) is
/// usually std's through a glob or a macro, so it is not split onto a project
/// module of the same name plus a type there (review of D#119: `io::Error::new(..)`
/// bound the project's `io/error.rs`). A path through `std::`/`core::`/`alloc::`
/// itself is not split either: `std::fmt::Error::new` matched `src/fmt.rs`.
const RUST_STD_MODULES: &[&str] = &[
    "alloc",
    "any",
    "arch",
    "array",
    "ascii",
    "backtrace",
    "borrow",
    "boxed",
    "cell",
    "char",
    "clone",
    "cmp",
    "collections",
    "convert",
    "default",
    "env",
    "error",
    "f32",
    "f64",
    "ffi",
    "fmt",
    "fs",
    "future",
    "hash",
    "hint",
    "io",
    "iter",
    "marker",
    "mem",
    "net",
    "num",
    "ops",
    "option",
    "os",
    "panic",
    "path",
    "pin",
    "prelude",
    "primitive",
    "process",
    "ptr",
    "rc",
    "result",
    "slice",
    "str",
    "string",
    "sync",
    "task",
    "thread",
    "time",
    "vec",
];

/// Keep the candidates whose file path or `qualified_name` carries `segments`
/// as a contiguous chain. Split out of [`path_filter_candidates`] so the
/// qualifier can be tried twice: once as written, once with an own-crate root
/// removed.
fn filter_by_segment_chain(
    segments: &[String],
    candidates: &[i64],
    node_id_to_path: &std::collections::HashMap<i64, String>,
    db: &crate::storage::db::Database,
) -> anyhow::Result<Vec<i64>> {
    // Chunked under MAX_IN_PARAMS to keep large same-name candidate sets within
    // SQLite's variable cap (issue #30).
    let id_to_qn = get_node_qualified_names_by_ids(db.conn(), candidates)?;

    // The file path carries `segs` as directories, or ends in the last one as a
    // file: Rust commonly puts single-file mods at `src/<mod>.rs` (e.g.
    // `src/domain.rs` for `crate::domain::*`), which has no `/domain/`
    // directory boundary. Without the file arm, every `crate::domain::foo()`
    // call drops on the floor and `domain::foo` looks dead.
    let path_match = |path: &str, segs: &[String]| {
        let chain = segs.join("/");
        path.contains(&format!("/{chain}/"))
            || path.starts_with(&format!("{chain}/"))
            || segs
                .last()
                .is_some_and(|last| path.ends_with(&format!("/{last}.rs")))
    };
    let qn_match = |qn: &str, segs: &[String]| {
        let chain = segs.join(".");
        qn == chain
            || qn.starts_with(&format!("{chain}."))
            || qn.contains(&format!(".{chain}."))
            || qn.ends_with(&format!(".{chain}"))
    };

    let kept: Vec<i64> = candidates
        .iter()
        .copied()
        .filter(|id| {
            let path = node_id_to_path.get(id).map(String::as_str).unwrap_or("");
            let qn = id_to_qn.get(id).map(String::as_str).unwrap_or("");
            // The whole chain names the file or the owner; or a module chain
            // names the file and the rest the method's type
            // (`runtime::Builder::new()`: `runtime/`, `Builder.new`).
            path_match(path, segments)
                || qn_match(qn, segments)
                || (!RUST_STD_MODULES.contains(&segments[0].as_str())
                    && !matches!(segments[0].as_str(), "std" | "core" | "alloc")
                    && (1..segments.len()).any(|k| {
                        path_match(path, &segments[..k])
                            && qn.starts_with(&format!("{}.", segments[k..].join(".")))
                    }))
        })
        .collect();
    Ok(kept)
}

/// Filter candidates to those whose `qualified_name` belongs to `impl_type`
/// (i.e. is a method of the named type). Storage encodes this as `Type.method`
/// with `.` separator (treesitter.rs qualified_name assignment).
///
/// Not file-restricted — Rust allows `impl Type {}` blocks to span multiple
/// files (e.g. `impl Database` is split across 3+ files in this repo), so we
/// match by `qualified_name LIKE 'Type.%'` across all files. But one name can
/// be several types: broadcast's and mpsc's `Receiver` in one crate, tokio's and
/// tokio-util's `Wheel` in two. So the nearest methods of that name win: the
/// caller's own file (which cannot define two types of one name), else its
/// crate (an inherent impl lives in its type's crate), else all of them — a
/// trait impl may sit outside its type's crate (`impl Show for a::Foo` in `b`,
/// whose `self.name()` is `a`'s). A call in an inherent impl (`"inh"`) stops at
/// its crate: the impl lives in its type's crate, and so does every impl that
/// crate can call on the type — a test / bench / example target's call also
/// looks at the files under its directory no crate layout places
/// (`tests/common/…`), which its `mod` may include. When the caller's file defines the type's method
/// only in trait impls (`"wide"`), the file is skipped: an inherent method of
/// that name in another file outranks a trait's. The callers themselves are no evidence of
/// where the method lives: `Self::poll_accept(self)` inside the trait impl's
/// own `poll_accept` means the inherent one elsewhere.
///
/// Then the call's `"xl"` lines drop, from what was chosen, the caller's-file
/// methods of impls the language keeps apart from the caller's
/// (`parser::relations::rust_impls`). Dropped AFTER the choice, never before:
/// a tier emptied by them binds nothing rather than falling through to a wider
/// one, so the rule only ever removes an edge the choice would have made.
pub(super) fn self_filter_candidates(
    impl_type: &str,
    candidates: &[i64],
    callers: &[i64],
    caller_path: &str,
    node_id_to_path: &HashMap<i64, String>,
    metadata: Option<&str>,
    db: &crate::storage::db::Database,
) -> anyhow::Result<Vec<i64>> {
    // Chunked under MAX_IN_PARAMS (issue #30).
    let of_type = filter_method_ids(db.conn(), candidates, Some(impl_type))?;
    let crate_dir = rust_crate_layout(caller_path).map(|(dir, _, _)| dir);
    let nearest = |keep: &dyn Fn(&str) -> bool| -> Vec<i64> {
        of_type
            .iter()
            .copied()
            .filter(|id| !callers.contains(id))
            .filter(|id| node_id_to_path.get(id).is_some_and(|p| keep(p)))
            .collect()
    };
    let facts: Option<serde_json::Value> = metadata
        .filter(|m| m.contains(r#""xl":"#) || m.contains(r#""inh":"#) || m.contains(r#""wide":"#))
        .and_then(|m| serde_json::from_str(m).ok());
    let inherent = facts.as_ref().is_some_and(|v| v.get("inh").is_some());
    // The file has the type's method only in trait impls: an inherent one in
    // another file outranks it, so the file is no proof and the crate decides.
    let wide = facts.as_ref().is_some_and(|v| v.get("wide").is_some());
    let mut chosen = if wide {
        Vec::new()
    } else {
        nearest(&|p| p == caller_path)
    };
    if chosen.is_empty() {
        if let Some(crate_dir) = &crate_dir {
            chosen =
                nearest(&|p| rust_crate_layout(p).is_some_and(|(dir, _, _)| dir == *crate_dir));
        }
    }
    if chosen.is_empty() && inherent {
        // A test, bench or example target's own modules (`tests/common/…` under
        // its `mod common;`) are files no crate layout names, under the
        // target's directory: the next nearest place its type keeps methods.
        // A `src/` crate owns no such file (`src/bin/…` are crates of their
        // own), so its calls never look there.
        if let Some(dir) = crate_dir.as_deref().filter(|d| !d.ends_with("src/")) {
            chosen = nearest(&|p| p.starts_with(dir) && rust_crate_layout(p).is_none());
        }
    }
    if chosen.is_empty() && !(inherent && crate_dir.is_some()) {
        chosen = of_type;
    }
    let excluded: Vec<i64> = facts
        .as_ref()
        .and_then(|v| {
            v.get("xl")?
                .as_array()
                .map(|a| a.iter().filter_map(serde_json::Value::as_i64).collect())
        })
        .unwrap_or_default();
    if excluded.is_empty() {
        return Ok(chosen);
    }
    let mut stmt = db
        .conn()
        .prepare_cached("SELECT start_line FROM nodes WHERE id = ?1")?;
    let mut kept = Vec::with_capacity(chosen.len());
    for id in chosen {
        if node_id_to_path.get(&id).is_some_and(|p| p == caller_path) {
            let line: i64 = stmt.query_row([id], |r| r.get(0))?;
            if excluded.contains(&line) {
                continue;
            }
        }
        kept.push(id);
    }
    Ok(kept)
}

/// Where a Rust file sits in its crate: the directory module files hang off, the
/// crate root file(s), and the file's own module path. `src/a/b.rs` →
/// (`src/`, [lib.rs, main.rs], [a, b]); `src/a/mod.rs` → [a]; a test/bench/
/// example target `tests/t.rs` is its own root, (`tests/`, [tests/t.rs], []).
/// None where the layout says nothing sure (`src/bin/`, `tests/common/…`).
pub(super) fn rust_crate_layout(path: &str) -> Option<(String, Vec<String>, Vec<String>)> {
    let src_at = if path.starts_with("src/") {
        Some(0)
    } else {
        path.rfind("/src/").map(|i| i + 1)
    };
    if let Some(at) = src_at {
        let dir = &path[..at + 4];
        let rel = path[at + 4..].strip_suffix(".rs")?;
        if rel.starts_with("bin/") {
            return None;
        }
        let mut module: Vec<String> = rel.split('/').map(String::from).collect();
        if matches!(rel, "lib" | "main") {
            module.clear();
        } else if module.last().is_some_and(|m| m == "mod") {
            module.pop();
        }
        return Some((
            dir.to_string(),
            vec![format!("{dir}lib.rs"), format!("{dir}main.rs")],
            module,
        ));
    }
    let (dir, file) = path.rsplit_once('/')?;
    let target_dir = dir.rsplit('/').next().unwrap_or(dir);
    if matches!(target_dir, "tests" | "benches" | "examples") && file.ends_with(".rs") {
        return Some((format!("{dir}/"), vec![path.to_string()], Vec::new()));
    }
    None
}

/// The files a project `use` path's module can be in (D#71), from the
/// `{"ru","m","up"}` import metadata (`parser::relations::rust::use_module_metadata`).
/// The longest leading part of the module path that is a file wins; the rest
/// are inline `mod` blocks inside it. None when the metadata is not a Rust
/// module path or no file matches: the caller falls back to the name.
pub(super) fn rust_use_files(
    meta: &serde_json::Value,
    importer: &str,
    all_file_paths: &HashSet<String>,
    crates: &RustCrates,
) -> Option<Vec<String>> {
    let root = meta.get("ru")?.as_str()?;
    let written: Vec<String> = meta
        .get("m")?
        .as_array()?
        .iter()
        .map(|s| s.as_str().map(String::from))
        .collect::<Option<_>>()?;
    // `{"ru":"ext","c":<crate>}`: a path rooted at a crate name, which names a
    // file only when that crate is a package of this project (D#132).
    let (dir, root_files, file_module) = if root == "ext" {
        let dir = crates.src_dir(meta.get("c")?.as_str()?)?.to_string();
        let roots = vec![format!("{dir}lib.rs"), format!("{dir}main.rs")];
        (dir, roots, Vec::new())
    } else {
        rust_crate_layout(importer)?
    };
    // A crate name is the package's library; `crate`/`self`/`super` the root
    // of the crate the importer is compiled into (D#136).
    let side = if root == "ext" {
        RootSide::Lib
    } else {
        crates.side_of(&dir, importer)
    };
    let module: Vec<String> = match root {
        "ext" => written,
        "crate" => written,
        "file" => {
            let up = meta.get("up").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let mut module = file_module[..file_module.len().checked_sub(up)?].to_vec();
            module.extend(written);
            module
        }
        _ => return None,
    };
    for len in (0..=module.len()).rev() {
        let candidates = if len == 0 {
            root_files.clone()
        } else {
            let stem = format!("{dir}{}", module[..len].join("/"));
            vec![format!("{stem}.rs"), format!("{stem}/mod.rs")]
        };
        let files: Vec<String> = candidates
            .into_iter()
            .filter(|f| all_file_paths.contains(f))
            .collect();
        if !files.is_empty() {
            return Some(if len == 0 {
                own_root(files, &dir, side)
            } else {
                files
            });
        }
    }
    None
}

/// The root file of the crate a Rust `use` (the `{"ru",…}` import metadata)
/// cannot reach (D#136): in a package with both `src/lib.rs` and `src/main.rs`,
/// main.rs for a `use` rooted at the package's name or made in lib.rs's module
/// tree, lib.rs for one made in main.rs's. None when the importer's crate is not
/// known.
pub(super) fn rust_use_other_root(
    meta: &serde_json::Value,
    importer: &str,
    crates: &RustCrates,
) -> Option<String> {
    let (dir, side) = match meta.get("ru")?.as_str()? {
        "ext" => (
            crates.src_dir(meta.get("c")?.as_str()?)?.to_string(),
            RootSide::Lib,
        ),
        "crate" | "file" => {
            let (dir, _, _) = rust_crate_layout(importer)?;
            let side = crates.side_of(&dir, importer);
            (dir, side)
        }
        _ => return None,
    };
    match side {
        RootSide::Lib => Some(format!("{dir}main.rs")),
        RootSide::Main => Some(format!("{dir}lib.rs")),
        RootSide::Either => None,
    }
}

/// The root file of `side`'s crate among the existing `roots` of the package at
/// `dir`, or all of them when the side is not known or its root is missing.
fn own_root(roots: Vec<String>, dir: &str, side: RootSide) -> Vec<String> {
    let own = match side {
        RootSide::Lib => format!("{dir}lib.rs"),
        RootSide::Main => format!("{dir}main.rs"),
        RootSide::Either => return roots,
    };
    if roots.contains(&own) {
        vec![own]
    } else {
        roots
    }
}

/// Where a Rust path call a `use` spelled out (D#132, the parser's `"u"` key,
/// `parser::relations::rust_use`) is to be looked for.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UseAnchor {
    /// Not written by a `use`, or rooted at a crate name that is neither std's
    /// nor a package of this project: the path filter as before.
    None,
    /// std's, core's, alloc's or proc_macro's item: no project code runs.
    Foreign,
    /// A module of a crate of this project.
    At(CrateModule),
    /// A path from this crate the caller's file layout cannot place (a `src/bin/`
    /// file, `super` above the crate root): the path filter over the path
    /// without its root, as the same path written in the call gets.
    Unplaced(Vec<String>),
    /// Rooted at a crate name no package answers to while some manifest names
    /// a crate the scan could not read ([`RustCrates`]'s `opaque`): it may be
    /// that one, so the path without its root, and a bare call by its name, as
    /// before D#132.
    Opaque(Vec<String>),
}

/// A module path inside one crate: its directory (`tokio/src/`), the file(s) its
/// root module is, and the path below the root (`[sync, Mutex]`).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct CrateModule {
    pub(super) dir: String,
    pub(super) roots: Vec<String>,
    pub(super) module: Vec<String>,
    /// Which of a lib.rs + main.rs package's crates the path is in (D#136).
    pub(super) side: RootSide,
}

/// Roots whose items are never the project's.
const RUST_FOREIGN_ROOTS: &[&str] = &["std", "core", "alloc", "proc_macro"];

/// Read the `"u"` of a Rust path call: `"x"` roots the path at a crate name
/// (`segments[0]`), `"c"` at this crate (`crate`, `self` = the caller's file's
/// module, `super` = one above it).
pub(super) fn rust_use_anchor(
    metadata: Option<&str>,
    segments: &[String],
    caller: &str,
    crates: &RustCrates,
) -> UseAnchor {
    let Some(tag) = metadata
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| v.get("u").and_then(|u| u.as_str()).map(String::from))
    else {
        return UseAnchor::None;
    };
    let Some((first, rest)) = segments.split_first() else {
        return UseAnchor::None;
    };
    match tag.as_str() {
        "x" => {
            if let Some(dir) = crates.src_dir(first) {
                UseAnchor::At(CrateModule {
                    dir: dir.to_string(),
                    roots: vec![format!("{dir}lib.rs"), format!("{dir}main.rs")],
                    module: rest.to_vec(),
                    side: RootSide::Lib,
                })
            } else if RUST_FOREIGN_ROOTS.contains(&first.as_str()) {
                UseAnchor::Foreign
            } else if crates.opaque {
                UseAnchor::Opaque(rest.to_vec())
            } else {
                UseAnchor::None
            }
        }
        "c" => {
            let supers = segments.iter().take_while(|s| *s == "super").count();
            let tail: Vec<String> = segments
                .iter()
                .skip_while(|s| matches!(s.as_str(), "crate" | "self" | "super"))
                .cloned()
                .collect();
            let Some((dir, roots, file_module)) = rust_crate_layout(caller) else {
                return UseAnchor::Unplaced(tail);
            };
            let mut module = match first.as_str() {
                "crate" => Vec::new(),
                "self" => file_module,
                "super" => match file_module.len().checked_sub(supers) {
                    Some(keep) => file_module[..keep].to_vec(),
                    None => return UseAnchor::Unplaced(tail),
                },
                _ => return UseAnchor::Unplaced(tail),
            };
            module.extend(tail);
            let side = crates.side_of(&dir, caller);
            UseAnchor::At(CrateModule {
                dir,
                roots,
                module,
                side,
            })
        }
        _ => UseAnchor::None,
    }
}

/// What a path call anchored in one crate ([`UseAnchor::At`]) binds.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Anchored {
    /// Items in the module file the path names (the longest leading part of it
    /// that is a file; the rest are inline `mod` blocks).
    Named(Vec<i64>),
    /// None there: a re-export (`pub use mutex::Mutex` in `sync/mod.rs`), or a
    /// `pub use` from further away. The crate's items of that name and owner
    /// whose module shares the longest leading part with the path.
    Elsewhere(Vec<i64>),
    /// No item of that name and owner in the crate (a macro-made item, or one a
    /// later edit adds).
    Nothing,
}

/// The files a module path below `anchor`'s root can be: the longest leading
/// part of `module` that is a file.
fn crate_module_files(
    anchor: &CrateModule,
    module: &[String],
    all_file_paths: &HashSet<String>,
) -> Vec<String> {
    for len in (0..=module.len()).rev() {
        let candidates = if len == 0 {
            anchor.roots.clone()
        } else {
            let stem = format!("{}{}", anchor.dir, module[..len].join("/"));
            vec![format!("{stem}.rs"), format!("{stem}/mod.rs")]
        };
        let files: Vec<String> = candidates
            .into_iter()
            .filter(|f| all_file_paths.contains(f))
            .collect();
        if !files.is_empty() {
            return if len == 0 {
                own_root(files, &anchor.dir, anchor.side)
            } else {
                files
            };
        }
    }
    Vec::new()
}

/// The module a file under `anchor`'s directory is in that crate, or None when
/// the file is a crate of its own: a `src/bin/` target, or beside a test /
/// bench / example target (`tests/other.rs`; its modules sit in
/// subdirectories, `tests/support/mpsc.rs`).
fn crate_file_module(anchor: &CrateModule, path: &str) -> Option<Vec<String>> {
    // The other crate's root: its items are not this crate's (D#136).
    let other_root = match anchor.side {
        RootSide::Lib => Some("main.rs"),
        RootSide::Main => Some("lib.rs"),
        RootSide::Either => None,
    };
    if other_root.is_some_and(|r| path.strip_prefix(anchor.dir.as_str()) == Some(r)) {
        return None;
    }
    if anchor.roots.iter().any(|r| r == path) {
        return Some(Vec::new());
    }
    let stem = path
        .strip_prefix(anchor.dir.as_str())?
        .strip_suffix(".rs")?;
    let own_root = anchor.roots.len() == 1;
    if stem.starts_with("bin/") || (own_root && !stem.contains('/')) {
        return None;
    }
    let mut module: Vec<String> = stem.split('/').map(String::from).collect();
    if module.last().is_some_and(|m| m == "mod") {
        module.pop();
    }
    Some(module)
}

/// Where an item stands for a path anchored in one crate: in the module file the
/// path names, or elsewhere in the crate with this many leading module segments
/// in common. None when the path cannot reach it: another crate, an owner type
/// the path does not end with, or a free function through a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorRank {
    Named,
    Shared(usize),
}

/// [`AnchorRank`] of the item at `path` whose qualified name is `qn`. `files_of`
/// memoizes the named module file per module-path length.
fn anchor_rank(
    anchor: &CrateModule,
    path: &str,
    qn: Option<&str>,
    all_file_paths: &HashSet<String>,
    files_of: &mut HashMap<usize, Vec<String>>,
) -> Option<AnchorRank> {
    let owner: Vec<&str> = match qn.and_then(|qn| qn.rsplit_once('.')) {
        Some((owner, _)) => owner.split('.').collect(),
        None => Vec::new(),
    };
    let module = &anchor.module;
    let modpart = if owner.is_empty() {
        if module
            .iter()
            .any(|s| s.starts_with(|c: char| c.is_uppercase()))
        {
            return None;
        }
        &module[..]
    } else {
        let keep = module.len().checked_sub(owner.len())?;
        if !module[keep..].iter().zip(&owner).all(|(a, b)| a == b) {
            return None;
        }
        &module[..keep]
    };
    let files = files_of
        .entry(modpart.len())
        .or_insert_with(|| crate_module_files(anchor, modpart, all_file_paths));
    if files.iter().any(|f| f == path) {
        return Some(AnchorRank::Named);
    }
    let file_module = crate_file_module(anchor, path)?;
    Some(AnchorRank::Shared(
        file_module
            .iter()
            .zip(modpart)
            .take_while(|(a, b)| a == b)
            .count(),
    ))
}

/// Resolve a path call anchored in one crate. A candidate must live in that
/// crate, and the path must end with the candidate's owner type (`Mutex` for
/// `Mutex.new`); a free function is reached only through a path of modules
/// (no segment naming a type). What is left of the path is the module.
pub(super) fn rust_anchored_targets(
    anchor: &CrateModule,
    candidates: &[i64],
    node_id_to_path: &HashMap<i64, String>,
    db: &Database,
    all_file_paths: &HashSet<String>,
) -> Result<Anchored> {
    let in_crate: Vec<i64> = candidates
        .iter()
        .copied()
        .filter(|id| {
            node_id_to_path
                .get(id)
                .is_some_and(|p| p.starts_with(anchor.dir.as_str()))
        })
        .collect();
    if in_crate.is_empty() {
        return Ok(Anchored::Nothing);
    }
    let id_to_qn = get_node_qualified_names_by_ids(db.conn(), &in_crate)?;
    let mut files_of: HashMap<usize, Vec<String>> = HashMap::new();
    let mut named = Vec::new();
    let mut ranked: Vec<(usize, i64)> = Vec::new();
    for id in in_crate {
        let path = node_id_to_path.get(&id).map(String::as_str).unwrap_or("");
        let qn = id_to_qn.get(&id).map(String::as_str);
        match anchor_rank(anchor, path, qn, all_file_paths, &mut files_of) {
            Some(AnchorRank::Named) => named.push(id),
            Some(AnchorRank::Shared(shared)) => ranked.push((shared, id)),
            None => {}
        }
    }
    if !named.is_empty() {
        return Ok(Anchored::Named(named));
    }
    let Some(best) = ranked.iter().map(|(n, _)| *n).max() else {
        return Ok(Anchored::Nothing);
    };
    Ok(Anchored::Elsewhere(
        ranked
            .into_iter()
            .filter(|(n, _)| *n == best)
            .map(|(_, id)| id)
            .collect(),
    ))
}

/// The metadata of an edge [`Anchored::Elsewhere`] bound: the call's own, with
/// `"rx"` = the crate directory it was looked for in, so a later definition in
/// that crate re-resolves the caller (`bare_name_callers_of_new_duplicates`),
/// and [`ambiguous_meta`]'s mark when it bound more than one.
pub(super) fn reexport_meta(metadata: &str, dir: &str, several: bool) -> String {
    let marked = match serde_json::from_str::<serde_json::Value>(metadata) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert("rx".into(), serde_json::Value::from(dir));
            serde_json::Value::Object(map).to_string()
        }
        _ => metadata.to_string(),
    };
    if several {
        ambiguous_meta(&marked)
    } else {
        marked
    }
}

/// Whether a Rust function's signature (`(&mut self, x: T) -> R`, as the parser
/// stores it) takes `self` first: `self`, `mut self`, `&self`, `&'a mut self`,
/// `self: Box<Self>`.
pub(super) fn rust_signature_takes_self(signature: &str) -> bool {
    let Some(rest) = signature.trim_start().strip_prefix('(') else {
        return false;
    };
    let mut rest = rest.trim_start();
    if let Some(r) = rest.strip_prefix('&') {
        rest = r.trim_start();
        if let Some(r) = rest.strip_prefix('\'') {
            let end = r
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(r.len());
            rest = r[end..].trim_start();
        }
    }
    if let Some(r) = rest.strip_prefix("mut") {
        if r.starts_with(char::is_whitespace) {
            rest = r.trim_start();
        }
    }
    rest.strip_prefix("self")
        .is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// How many parameters a Rust function's signature declares, `self` included:
/// commas at the top level of the parameter list, a trailing one not counted.
/// None when the count is not fixed by the text — a `#[cfg]`'d parameter, C
/// variadics (`...`), a list that does not close — or a comment in it could hide
/// or fake a comma.
pub(super) fn rust_signature_param_count(signature: &str) -> Option<usize> {
    if signature.contains("//") || signature.contains("/*") {
        return None;
    }
    let rest = signature.trim_start().strip_prefix('(')?;
    let (mut depth, mut commas, mut prev) = (0usize, 0usize, '(');
    let mut last_non_space = '(';
    for c in rest.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            // `->` in `Fn(A) -> B` closes nothing.
            '>' if prev == '-' => {}
            ')' | ']' | '}' | '>' if depth > 0 => depth -= 1,
            ')' => {
                let empty = last_non_space == '(';
                return Some(if empty {
                    0
                } else {
                    commas + usize::from(last_non_space != ',')
                });
            }
            ',' if depth == 0 => commas += 1,
            '#' | '.' if depth == 0 => return None,
            _ => {}
        }
        prev = c;
        if !c.is_whitespace() {
            last_non_space = c;
        }
    }
    None
}

/// What the resolver knows of a Rust function's parameters from its signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RustFnShape {
    pub(super) takes_self: bool,
    /// Parameters, `self` included; None when the signature does not fix it.
    pub(super) params: Option<usize>,
    /// An item of an `impl` or `trait` block, reached only through `Type::`,
    /// `Self::` or a receiver — never by a bare `f()` (D#119). Implied by
    /// `takes_self`; `Handle::spawn(me: &Arc<Self>, ..)` is one without it.
    pub(super) associated: bool,
    /// The `impl`/`trait` type a method belongs to, kept only when it starts
    /// lowercase (`impl Encode for u32`, a `non_camel_case_types` struct): the
    /// one case where a lowercase path segment names a type, not a module.
    pub(super) lowercase_owner: Option<Box<str>>,
    /// The `src/` directory of the crate that alone can call this function:
    /// set for a `pub(crate)`/`pub(super)` item and a private free function of a
    /// library, which another crate (an integration test, an example, a bench,
    /// another package) cannot reach (D#119, [`rust_crate_admits`]).
    pub(super) crate_private_dir: Option<Box<str>>,
}

/// A Rust signature split into the type parameter the parser writes before a
/// blanket impl's method's parameter list (`<T> (&self) -> String` for `impl<T:
/// Display> Shout for T`, parser `rust_blanket_impl_param`), if any, and the
/// signature after it.
pub(super) fn rust_blanket_signature(signature: &str) -> (Option<&str>, &str) {
    signature
        .strip_prefix('<')
        .and_then(|s| s.split_once("> "))
        .map_or((None, signature), |(param, rest)| (Some(param), rest))
}

/// The `impl`/`trait` type in a Rust function's qualified name (`Type.f`).
fn rust_fn_owner(qualified_name: Option<&str>) -> Option<&str> {
    qualified_name
        .and_then(|q| q.rsplit_once('.'))
        .map(|(owner, _)| owner.rsplit('.').next().unwrap_or(owner))
}

pub(super) fn rust_fn_shape(signature: Option<&str>, qualified_name: Option<&str>) -> RustFnShape {
    let signature = signature.map(|s| rust_blanket_signature(s).1);
    let takes_self = signature.is_some_and(rust_signature_takes_self);
    let owner = rust_fn_owner(qualified_name);
    RustFnShape {
        takes_self,
        params: signature.and_then(rust_signature_param_count),
        associated: takes_self || owner.is_some(),
        lowercase_owner: owner
            .filter(|owner| owner.starts_with(|c: char| c.is_ascii_lowercase()))
            .map(Box::from),
        crate_private_dir: None,
    }
}

/// The `src/` directory of the crate a Rust file belongs to (`tokio/src/`), or
/// None outside one: an integration test, example or bench target, or a
/// `src/bin/` binary, is a crate of its own.
pub(super) fn rust_lib_dir(path: &str) -> Option<&str> {
    let at = if path.starts_with("src/") {
        0
    } else {
        path.rfind("/src/")? + 1
    };
    (!path[at + 4..].starts_with("bin/")).then(|| &path[..at + 4])
}

/// Whether a Rust function is callable only inside its own crate, by the
/// visibility its source opens with. A `pub(crate)`, `pub(super)`, `pub(self)`
/// or `pub(in …)` item is; so is a free function with no `pub`. An `impl` or
/// `trait` item with no `pub` is not decided: a trait impl's method is written
/// without one and is as public as the trait.
fn rust_fn_crate_private(code: &str, associated: bool) -> bool {
    match code.trim_start().strip_prefix("pub") {
        Some(rest) if rest.starts_with(|c: char| c.is_whitespace() || c == '(') => {
            rest.trim_start().starts_with('(')
        }
        _ => !associated,
    }
}

/// Whether a Rust call from `caller_path` can reach a function, by crate
/// visibility: a crate-private function only from its own crate's `src/`.
pub(super) fn rust_crate_admits(caller_path: Option<&str>, callee: &RustFnShape) -> bool {
    match (&callee.crate_private_dir, caller_path) {
        (Some(dir), Some(caller)) => rust_lib_dir(caller) == Some(&**dir),
        _ => true,
    }
}

/// One Rust function as [`rust_fn_shapes_of_file`] reads it.
pub(super) struct RustFnRow<'a> {
    pub(super) id: i64,
    pub(super) signature: Option<&'a str>,
    pub(super) qualified_name: Option<&'a str>,
    /// 1-based (start, end) lines.
    pub(super) lines: (u32, u32),
    /// The source, of which only the leading visibility is read.
    pub(super) code: &'a str,
}

/// The shapes of one file's Rust functions. A `fn` nested in a method takes the
/// method's `Type.` prefix (`Interest.mio_add` inside `Interest::to_mio`) but is
/// a free function of that body, called bare: one strictly inside a function of
/// the same owner is not associated. (An `impl` inside a function is another
/// owner, and its functions stay associated.)
pub(super) fn rust_fn_shapes_of_file(path: &str, rows: &[RustFnRow]) -> Vec<(i64, RustFnShape)> {
    let lib_dir = rust_lib_dir(path);
    let mut by_owner: HashMap<&str, Vec<&RustFnRow>> = HashMap::new();
    for row in rows {
        if let Some(owner) = rust_fn_owner(row.qualified_name) {
            by_owner.entry(owner).or_default().push(row);
        }
    }
    let mut nested: HashSet<i64> = HashSet::new();
    for group in by_owner.values_mut().filter(|g| g.len() > 1) {
        group.sort_by_key(|r| (r.lines.0, std::cmp::Reverse(r.lines.1)));
        // Spans still open at this row's start, outermost first.
        let mut open: Vec<(u32, u32)> = Vec::new();
        for row in group.iter() {
            while open.last().is_some_and(|&(_, end)| end < row.lines.0) {
                open.pop();
            }
            if open
                .iter()
                .any(|&span| span != row.lines && span.1 >= row.lines.1)
            {
                nested.insert(row.id);
            }
            open.push(row.lines);
        }
    }
    rows.iter()
        .map(|row| {
            let mut shape = rust_fn_shape(row.signature, row.qualified_name);
            if nested.contains(&row.id) && !shape.takes_self {
                shape.associated = false;
                shape.lowercase_owner = None;
            }
            if rust_fn_crate_private(row.code, shape.associated) {
                shape.crate_private_dir = lib_dir.map(Box::from);
            }
            (row.id, shape)
        })
        .collect()
}

/// Whether a Rust call can reach a function, by the call's syntax alone.
///
/// Self-ness (D#71): a bare `f()` never calls a method: `drop(guard)` bound the
/// project's own `impl Drop::drop`. A method call `x.f()` (`recv`, `chain`,
/// `member`) only calls a function that takes `self`: `status.success()` bound
/// `JsonRpcResponse::success(id, v)`. Paths (`T::f()` reaches both) and
/// `self.`/`Self::` calls are decided elsewhere.
///
/// Arity (D#112): Rust has no overloading, default or variadic parameters, so a
/// call passing `n` arguments (the parser's `"n"`) reaches only a function
/// taking `n`, besides `self` for a method call; a path call (`T::f(x, a)`)
/// passes `self` itself. An atomic's `.load(Ordering::Acquire)` bound
/// `ProjectClassNames::load(&mut self, db, candidates)`. A bare call carries no
/// metadata, so no count.
pub(super) fn rust_call_shape_admits(metadata: Option<&str>, callee: &RustFnShape) -> bool {
    // A bare `f()` reaches no item of an `impl` or `trait` (D#119): tokio's bare
    // `spawn(fut)` bound `Handle::spawn(me: &Arc<Self>, ..)`.
    if metadata.is_none_or(str::is_empty) {
        return !callee.associated;
    }
    let meta = parse_callee_metadata(metadata);
    let method_call = matches!(
        meta,
        Some(CalleeMeta::Receiver(_) | CalleeMeta::Chain | CalleeMeta::Member)
    );
    if method_call && !callee.takes_self {
        return false;
    }
    // A path through a crate or module (`tokio::spawn(f)`, `rt::run()`) names no
    // type, so it cannot pass `self`: only `Type::f(x)` calls a method that way.
    // With the arity rule leaving `Command::spawn(&mut self)` the only
    // one-parameter `spawn`, tokio's `tokio::spawn(fut)` calls all bound it.
    // A lowercase segment still names a type when it is the method's own
    // lowercase type (D#126: `u32::encode_to(&v, buf)` through `impl Encode for
    // u32`). Nor does a module path reach an associated function without
    // `self` (D#119).
    if callee.associated
        && matches!(&meta, Some(CalleeMeta::Path(segments))
        if segments.last().is_some_and(|s| {
            s.starts_with(|c: char| c.is_ascii_lowercase())
                && callee.lowercase_owner.as_deref() != Some(s.as_str())
        }))
    {
        return false;
    }
    let receiver =
        callee.takes_self && (method_call || matches!(meta, Some(CalleeMeta::SelfRecv(_))));
    match (rust_call_arity(metadata), callee.params) {
        (Some(n), Some(params)) => n + usize::from(receiver) == params,
        _ => true,
    }
}

/// The `"n"` argument count a Rust call's metadata carries, if any.
fn rust_call_arity(metadata: Option<&str>) -> Option<usize> {
    let v: serde_json::Value = serde_json::from_str(metadata?).ok()?;
    usize::try_from(v.get("n")?.as_u64()?).ok()
}

/// Whether a call or import went through a package binding
/// ([`crate::domain::CALL_Q_PACKAGE`]): its name then means no function of the
/// caller's own file. One predicate for the batch, deferred and pending paths.
pub(super) fn is_package_bound(metadata: Option<&str>) -> bool {
    metadata
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .is_some_and(|v| v.get("q").and_then(|q| q.as_str()) == Some(crate::domain::CALL_Q_PACKAGE))
}

/// Candidates a call can reach given its metadata: a member call on an object
/// (`CalleeMeta::Member`, or `RecvType` — a receiver of known class) cannot reach
/// a free function; every other call keeps them all. One helper so the batch,
/// deferred and pending paths cannot disagree.
pub(super) fn member_call_candidates(
    metadata: Option<&str>,
    candidates: Vec<i64>,
    db: &crate::storage::db::Database,
) -> anyhow::Result<Vec<i64>> {
    if matches!(
        parse_callee_metadata(metadata),
        Some(CalleeMeta::Member | CalleeMeta::RecvType(_) | CalleeMeta::SuperType(_))
    ) {
        crate::storage::queries::filter_out_function_ids(db.conn(), &candidates)
    } else {
        Ok(candidates)
    }
}

/// Whether a dotted Python module is a project module or package: a key of the
/// module map, or a prefix of one (a namespace package without `__init__.py` is
/// no key of its own). Memoized per module name — the prefix test scans every
/// key, and a file calls into the same few modules over and over.
pub(super) struct ProjectPythonModules<'a> {
    map: &'a HashMap<String, Vec<String>>,
    seen: HashMap<String, bool>,
}

impl<'a> ProjectPythonModules<'a> {
    pub(super) fn new(map: &'a HashMap<String, Vec<String>>) -> Self {
        Self {
            map,
            seen: HashMap::new(),
        }
    }

    pub(super) fn contains(&mut self, module: &str) -> bool {
        if let Some(&known) = self.seen.get(module) {
            return known;
        }
        let known = self.map.contains_key(module)
            || self.map.keys().any(|k| {
                k.len() > module.len()
                    && k.starts_with(module)
                    && k.as_bytes()[module.len()] == b'.'
            });
        self.seen.insert(module.to_string(), known);
        known
    }
}

/// The C++ field types `cpp_fields` records, loaded once per pass, to type
/// [`CalleeMeta::Field`] calls.
pub(super) struct CppFieldTypes {
    /// (class node id, field) → (dot type, arrow type).
    fields: HashMap<(i64, String), (Option<String>, Option<String>)>,
    /// Class-like nodes by last name segment, with their class path.
    by_last: HashMap<String, Vec<(i64, Vec<String>)>>,
    /// Class node id → its last name segment (the `by_last` key).
    last_of: HashMap<i64, String>,
    /// Class node id → direct base class ids (`inherits` edges).
    parents: HashMap<i64, Vec<i64>>,
    /// (owner class last name, method) → recorded return types.
    returns: HashMap<(String, String), HashSet<String>>,
}

impl CppFieldTypes {
    pub(super) fn load(conn: &rusqlite::Connection) -> Result<Self> {
        let mut fields = HashMap::new();
        for (class_id, field, dot, arrow) in crate::storage::queries::cpp_fields(conn)? {
            fields.insert((class_id, field), (dot, arrow));
        }
        let mut by_last: HashMap<String, Vec<(i64, Vec<String>)>> = HashMap::new();
        for (id, _, spelling, _) in crate::storage::queries::class_like_names(conn)? {
            let path: Vec<String> = class_path(&spelling)
                .into_iter()
                .map(String::from)
                .collect();
            if let Some(last) = path.last() {
                by_last.entry(last.clone()).or_default().push((id, path));
            }
        }
        // Built once: `return_of` runs per chain step, and rebuilding this per
        // call made a full index quadratic in the class count.
        let last_of: HashMap<i64, String> = by_last
            .iter()
            .flat_map(|(last, nodes)| nodes.iter().map(move |(id, _)| (*id, last.clone())))
            .collect();
        let mut parents: HashMap<i64, Vec<i64>> = HashMap::new();
        for (sub, sup) in crate::storage::queries::inherits_edges(conn)? {
            parents.entry(sub).or_default().push(sup);
        }
        let mut returns: HashMap<(String, String), HashSet<String>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT n.qualified_name, n.return_type FROM nodes n JOIN files f ON f.id = n.file_id
             WHERE f.language = 'cpp' AND n.type IN ('function', 'method')
               AND n.qualified_name LIKE '%.%' AND n.return_type IS NOT NULL",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (q, ret) = row?;
            if let Some((owner, m)) = q.rsplit_once('.') {
                if let Some(o) = owner_path(&q).last() {
                    returns
                        .entry((o.to_string(), m.to_string()))
                        .or_default()
                        .insert(ret);
                }
                let _ = owner;
            }
        }
        Ok(Self {
            fields,
            by_last,
            last_of,
            parents,
            returns,
        })
    }

    /// The class nodes a class spelling names (a `::` path matched by suffix).
    fn nodes_of(&self, class: &str) -> Vec<i64> {
        let want = class_path(class);
        let Some(last) = want.last() else {
            return Vec::new();
        };
        self.by_last
            .get(*last)
            .into_iter()
            .flatten()
            .filter(|(_, path)| {
                let k = path.len().min(want.len());
                path[path.len() - k..]
                    .iter()
                    .map(String::as_str)
                    .eq(want[want.len() - k..].iter().copied())
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// The return type of `class.method`, as the class declares it or its
    /// nearest base does; None unless every class of that spelling agrees.
    fn return_of(&self, class: &str, method: &str) -> Option<String> {
        let mut found: Vec<&String> = Vec::new();
        for start in self.nodes_of(class) {
            let mut seen: HashSet<i64> = HashSet::new();
            let mut stack = vec![start];
            while let Some(c) = stack.pop() {
                if !seen.insert(c) {
                    continue;
                }
                // Returns its template's parameter (`cpp_class_fields`).
                if self.fields.contains_key(&(c, format!("{method}()"))) {
                    return None;
                }
                let key = (
                    self.last_of.get(&c).cloned().unwrap_or_default(),
                    method.to_string(),
                );
                match self.returns.get(&key) {
                    Some(types) => found.extend(types.iter()),
                    None => stack.extend(self.parents.get(&c).into_iter().flatten()),
                }
            }
        }
        let first = *found.first()?;
        found.iter().all(|t| *t == first).then(|| first.clone())
    }

    /// The class a [`CalleeMeta::Via`] chain's receiver has, and every class
    /// the walk went through (for [`typed_callers_of_class_drift`]).
    fn walk_via(&self, meta: &serde_json::Value) -> (Option<String>, Vec<String>) {
        let mut through: Vec<String> = Vec::new();
        let base = meta.get("b");
        let ba = meta.get("ba").and_then(|v| v.as_u64()) == Some(1);
        let mut ty = match (
            base.and_then(|b| b.get("t")).and_then(|t| t.as_str()),
            base.and_then(|b| b.get("c")).and_then(|c| c.as_str()),
            base.and_then(|b| b.get("v")).and_then(|v| v.as_str()),
        ) {
            (Some(t), _, _) => Some(t.to_string()),
            (None, Some(c), Some(v)) => {
                through.extend(class_path(c).last().map(|l| l.to_string()));
                self.type_of(c, v, ba)
            }
            _ => None,
        };
        for step in meta
            .get("s")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
        {
            let Some(t) = ty.take() else { break };
            through.extend(class_path(&t).last().map(|l| l.to_string()));
            let (kind, name, arrow) = (
                step.get(0).and_then(|k| k.as_str()),
                step.get(1).and_then(|n| n.as_str()),
                step.get(2).and_then(|a| a.as_u64()) == Some(1),
            );
            ty = match (kind, name) {
                (Some("f"), Some(f)) => self.type_of(&t, f, arrow),
                (Some("m"), Some(m)) => self.return_of(&t, m),
                _ => None,
            };
        }
        (ty, through)
    }

    /// The class a call through `class::field` names: the field as the class
    /// declares it, else as its nearest base does. None unless every class of
    /// that path agrees on one type.
    fn type_of(&self, class: &str, field: &str, arrow: bool) -> Option<String> {
        let want = class_path(class);
        let last = *want.last()?;
        let mut found: Vec<Option<&String>> = Vec::new();
        for (id, path) in self.by_last.get(last)? {
            let k = path.len().min(want.len());
            if path[path.len() - k..]
                .iter()
                .map(String::as_str)
                .ne(want[want.len() - k..].iter().copied())
            {
                continue;
            }
            let mut seen: HashSet<i64> = HashSet::new();
            let mut stack = vec![*id];
            while let Some(c) = stack.pop() {
                if !seen.insert(c) {
                    continue;
                }
                match self.fields.get(&(c, field.to_string())) {
                    Some((dot, via_arrow)) => {
                        found.push(if arrow { via_arrow } else { dot }.as_ref())
                    }
                    None => stack.extend(self.parents.get(&c).into_iter().flatten()),
                }
            }
        }
        let first = (*found.first()?)?;
        found
            .iter()
            .all(|t| *t == Some(first))
            .then(|| first.clone())
    }

    /// A [`CalleeMeta::Field`] call's metadata rewritten as the call it is: the
    /// `rtype` of the field's recorded type, else an untyped `member` call (a
    /// global, an untyped field). `fc`/`ff` keep the field, for
    /// [`typed_callers_of_class_drift`]. None for any other metadata.
    pub(super) fn rewrite(&self, metadata: Option<&str>) -> Option<String> {
        if let Some(CalleeMeta::Via) = parse_callee_metadata(metadata) {
            let meta: serde_json::Value = serde_json::from_str(metadata?).ok()?;
            let (ty, mut through) = self.walk_via(&meta);
            // Only a project class: a primitive or library return type keeps
            // the untyped call.
            let last = ty
                .as_deref()
                .and_then(|t| class_path(t).last().map(|l| l.to_string()));
            let known = last.as_ref().is_some_and(|l| self.by_last.contains_key(l));
            return Some(
                match ty {
                    Some(ty) if known => {
                        serde_json::json!({ "q": "rtype", "v": ty, "vc": through })
                    }
                    _ => {
                        // A final type that is no project class yet is still a
                        // class the call depends on: a class renamed to it must
                        // re-resolve this caller (`typed_callers_of_class_drift`).
                        through.extend(last);
                        serde_json::json!({ "q": "member", "vc": through })
                    }
                }
                .to_string(),
            );
        }
        let Some(CalleeMeta::Field {
            class,
            field,
            arrow,
        }) = parse_callee_metadata(metadata)
        else {
            return None;
        };
        Some(
            match self.type_of(&class, &field, arrow) {
                Some(ty) => serde_json::json!({ "q": "rtype", "v": ty, "fc": class, "ff": field }),
                None => serde_json::json!({ "q": "member", "fc": class, "ff": field }),
            }
            .to_string(),
        )
    }
}

/// `metadata` without the [`ambiguous_meta`] mark.
fn decided_meta(metadata: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(metadata) {
        Ok(serde_json::Value::Object(mut map)) if map.contains_key("amb") => {
            map.remove("amb");
            serde_json::Value::Object(map).to_string()
        }
        _ => metadata.to_string(),
    }
}

/// Delete the `calls` edges from `source_id` to nodes named `target_name` that a
/// typed call with this metadata's `q`/`v` produced (marked `amb` or not).
fn delete_typed_call_edges(
    db: &Database,
    source_id: i64,
    target_name: &str,
    metadata: Option<&str>,
) -> Result<()> {
    let Some(meta) = metadata.and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
    else {
        return Ok(());
    };
    let (Some(q), Some(v)) = (
        meta.get("q").and_then(|q| q.as_str()),
        meta.get("v").and_then(|v| v.as_str()),
    ) else {
        return Ok(());
    };
    db.conn().execute(
        "DELETE FROM edges WHERE source_id = ?1 AND relation = ?2
           AND target_id IN (SELECT id FROM nodes WHERE name = ?3)
           AND json_extract(metadata, '$.q') = ?4 AND json_extract(metadata, '$.v') = ?5",
        rusqlite::params![source_id, REL_CALLS, target_name, q, v],
    )?;
    Ok(())
}

/// The metadata a typed call's edges carry when its class did not decide the
/// target (`RecvTypeTargets::Ambiguous` / `Fallback`): the call's own
/// `rtype`/`super` metadata marked `"amb":1`. The mark makes the edge classify
/// like the untyped member call it resolved as; keeping the type lets a
/// re-resolution (a requeue after its target was renamed or deleted) resolve it
/// as the typed call a rebuild sees, not as an untyped one.
pub(super) fn ambiguous_meta(metadata: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(metadata) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert("amb".into(), serde_json::Value::from(1));
            serde_json::Value::Object(map).to_string()
        }
        _ => crate::domain::CALL_META_MEMBER.to_string(),
    }
}

/// What a call on a receiver of class `ty` (`CalleeMeta::RecvType`) binds.
#[derive(Debug, PartialEq)]
pub(super) enum RecvTypeTargets {
    /// The class's own method of that name — one, or those in the caller's
    /// file when several same-named classes define it — plus its overrides.
    Bind(Vec<i64>),
    /// Several same-named classes define the method and none is in the caller's
    /// file: bind them all, but as the untyped member call this is.
    Ambiguous(Vec<i64>),
    /// A project class without that method (inherited, or the type was
    /// mis-read): resolve like an untyped member call.
    Fallback,
    /// No class the project defines (`std::string`, `AbortController`, an
    /// imported library class): no project method can run.
    Drop,
}

/// A class-like node: its id, file and whether it is nested in another
/// class-like node of its file (or spelled `Outer::T`).
struct ClassNode {
    id: i64,
    file_id: i64,
    nested: bool,
}

/// The type an impl on a reference names: `&'a mut Foo` → `Foo`. Other types
/// come back as written.
fn rust_referent(owner: &str) -> &str {
    let mut rest = owner.trim();
    while let Some(r) = rest.strip_prefix('&') {
        rest = r.trim_start();
        if let Some(r) = rest.strip_prefix('\'') {
            rest = r
                .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_start();
        }
        if let Some(r) = rest.strip_prefix("mut ") {
            rest = r.trim_start();
        }
    }
    rest
}

/// What receiver-type resolution reads from the index, loaded once per pass and
/// then answered from memory — per call it runs no SQL beyond fetching the file
/// and qualified name of candidates it has not seen yet (django: one `__init__`
/// call has 912 candidates, and there are thousands of such calls).
#[derive(Default)]
pub(super) struct ProjectClassNames {
    /// Class-like nodes by last name segment; None until loaded.
    by_last: Option<HashMap<String, Vec<ClassNode>>>,
    /// Candidate id → (file id, qualified name).
    node_info: HashMap<i64, (i64, String)>,
    /// Superclass id → direct subclass ids, from `inherits` edges; None until
    /// loaded. The deferred pass resolves calls after every other relation, so
    /// these are complete when the first call reads them.
    children: Option<HashMap<i64, Vec<i64>>>,
    /// Subclass id → direct superclass ids, loaded with `children`.
    parents: HashMap<i64, Vec<i64>>,
    /// File path → file id, loaded with `children` (a caller's own class).
    file_ids: HashMap<String, i64>,
    /// Free functions no member call reaches (`filter_out_function_ids`'s
    /// complement); None until loaded.
    free_functions: Option<HashSet<i64>>,
    /// Every `method` node ([`Self::python_import_candidates`]); None until loaded.
    methods: Option<HashSet<i64>>,
    /// Rust function id → its parameters ([`rust_call_shape_admits`]); None
    /// until loaded.
    rust_fn_shapes: Option<HashMap<i64, RustFnShape>>,
    /// The project's Rust structs, enums and unions as (file id, name), the
    /// names of its traits (`interface` nodes), and the methods of its blanket
    /// impls with their type parameter ([`rust_blanket_signature`])
    /// ([`Self::rust_receiver_candidates`]); None until loaded.
    rust_concrete: Option<HashSet<(i64, String)>>,
    rust_traits: HashSet<String>,
    rust_blanket: HashMap<i64, Box<str>>,
    /// The same structs, enums and unions as (file path, name); loaded with
    /// `rust_concrete`.
    rust_concrete_at: HashSet<(String, String)>,
    /// Types with a `deref` / `deref_mut` method (a `Deref` impl): a call on
    /// one may run its target's methods; loaded with `rust_concrete`.
    rust_deref_owners: HashSet<String>,
}

/// What [`ProjectClassNames::rust_receiver_never`] rules out of a typed Rust
/// call's candidates.
#[derive(Debug, Default)]
pub(super) struct RecvNever {
    /// The candidates that cannot run.
    pub(super) ids: HashSet<i64>,
    /// Ruled out by what OTHER files say of the receiver's type (its methods,
    /// its `Deref`): a call this leaves with nothing to bind is buffered, not
    /// dropped, so a change there finds it (`typed_callers_of_class_drift`).
    pub(super) read_elsewhere: bool,
}

/// The receiver type a Rust method call carries (parser `rust_receiver.rs`,
/// D#112), with a crate-rooted one decided against the project's packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RustRecv {
    /// A type of the project, by name, the smart pointer it sits behind
    /// (`Arc<T>`), and the path a `use` names it by: only methods of either,
    /// when there is one; of same-named types, the one the path names.
    Project(String, Option<String>, Option<Vec<String>>),
    /// A std or dependency type (the name may be empty): only a project
    /// trait's method, or one of an impl that can run on that type
    /// ([`foreign_receiver_owner`]: on that very type unless the impl's
    /// file defines a project type of that name, a blanket impl, a reference,
    /// slice or primitive one).
    Foreign(String),
}

/// Rust's primitive types, as an impl names them (`impl NumExt for u32`).
const RUST_PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64",
];

/// How a method of an impl on `owner` (the owner its qualified name writes:
/// `u32`, `&str`, `[u8]`, `T`) stands for a receiver of the std or dependency
/// type `ty` (empty: a type with no name, such as a slice or an unsuffixed
/// literal). A project trait's own method is admitted before this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForeignOwner {
    /// Cannot run on it.
    No,
    /// An impl on that very type (`impl Weigh for AtomicU16`, `impl StrExt
    /// for &str` for a `str`): what method lookup finds first.
    Exact,
    /// Can run on it only when no exact impl answers (batch-1 review H1): one
    /// on a slice, array, tuple, pointer, `fn` or `dyn` type for a receiver
    /// with no name, a slice's for a `Vec` and `str`'s for a `String` (which
    /// deref to them), a primitive's for a receiver with no name (`7.twice()`),
    /// and a blanket impl's (`impl<T: Display> Shout for T`, `impl<T> Wait for
    /// &mut T`). tokio's `SocketAddrV4::new(..).to_socket_addrs(..)` binds the
    /// impl for `SocketAddrV4`, not the blanket one for `&T` beside it.
    ///
    /// A blanket impl binds whether or not its trait is in the caller's scope:
    /// batch-1 review round 2 withdrew the scope test, which asked whether the
    /// caller's file imports any item of the impl's file and so admitted
    /// `s.trim()` through `use crate::ext3::util`. Where rustc would call
    /// std's method, that binds a wrong edge (CHANGELOG, Not covered).
    Loose,
}

/// [`ForeignOwner`] of an impl on `owner` for a receiver of type `ty`.
/// `blanket` is the type parameter the parser found the impl to be a blanket
/// impl over (its self type is a parameter of its own `<…>` list,
/// [`rust_blanket_signature`]): the impl's header alone decides it, so no
/// other file can change the answer (batch-1 review round 2: asking whether
/// any project type was named `T` changed a rebuild, not an incremental run).
/// An impl on a named type is exact unless the impl's own file defines a
/// project struct, enum or union of that name (`own_type_here`): then it is
/// that type's. Asked of the impl's file, not of the whole project, because a
/// project `struct Duration` elsewhere does not make `impl Ext for
/// std::time::Duration` the project's, and because the answer then changes
/// only with that file, which re-resolves its callers (batch-1 review B2: the
/// project-wide reading changed a rebuild's answer when any file gained the
/// struct, and nothing re-resolved an incremental run's).
fn foreign_receiver_owner(
    ty: &str,
    owner: &str,
    blanket: Option<&str>,
    own_type_here: impl FnOnce() -> bool,
) -> ForeignOwner {
    let mut o = owner.trim();
    while let Some(rest) = o.strip_prefix('&') {
        let rest = rest.trim_start();
        let rest = match rest.strip_prefix('\'') {
            Some(lt) => lt
                .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_start(),
            None => rest,
        };
        o = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
    }
    let loose = |admits: bool| {
        if admits {
            ForeignOwner::Loose
        } else {
            ForeignOwner::No
        }
    };
    if blanket == Some(o) {
        return ForeignOwner::Loose;
    }
    if o.starts_with(['[', '(', '*']) || o.starts_with("fn(") || o.starts_with("dyn ") {
        return loose(ty.is_empty() || (ty == "Vec" && o.starts_with('[')));
    }
    if RUST_PRIMITIVES.contains(&o) {
        if o == ty {
            return ForeignOwner::Exact;
        }
        return loose(ty.is_empty() || (o == "str" && ty == "String"));
    }
    if o == ty && !own_type_here() {
        ForeignOwner::Exact
    } else {
        ForeignOwner::No
    }
}

/// The receiver type keys of a Rust call's metadata, or None.
pub(super) fn rust_receiver(metadata: Option<&str>, crate_roots: &RustCrates) -> Option<RustRecv> {
    let m = metadata?;
    if !m.contains(r#""rt":"#) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(m).ok()?;
    let name = v.get("rt")?.as_str()?.to_string();
    let via = v.get("rv").and_then(|p| p.as_str()).map(String::from);
    let path = v
        .get("rp")
        .and_then(|p| p.as_str())
        .map(|p| p.split("::").map(String::from).collect::<Vec<_>>());
    match (
        v.get("rk").and_then(|k| k.as_str()),
        v.get("rc").and_then(|c| c.as_str()),
    ) {
        (Some("p"), _) => Some(RustRecv::Project(name, via, path)),
        (Some("f"), _) => Some(RustRecv::Foreign(name)),
        (_, Some(krate)) if crate_roots.contains(krate) => Some(RustRecv::Project(name, via, path)),
        // Maybe a package whose manifest could not be read: untyped, as before.
        (_, Some(_)) if crate_roots.opaque => None,
        (_, Some(_)) => Some(RustRecv::Foreign(name)),
        _ => None,
    }
}

impl ProjectClassNames {
    /// The candidates a Rust call's syntax can reach ([`rust_call_shape_admits`]),
    /// from one read of every Rust function's signature. Other languages' calls
    /// pass through untouched.
    pub(super) fn rust_call_shape_candidates(
        &mut self,
        db: &crate::storage::db::Database,
        language: &str,
        metadata: Option<&str>,
        caller_path: Option<&str>,
        mut candidates: Vec<i64>,
    ) -> anyhow::Result<Vec<i64>> {
        if language != "rust" {
            return Ok(candidates);
        }
        if self.rust_fn_shapes.is_none() {
            let mut stmt = db.conn().prepare(
                "SELECT n.id, n.signature, n.qualified_name, f.path, n.start_line, n.end_line,
                        substr(n.code_content, 1, 64)
                 FROM nodes n JOIN files f ON f.id = n.file_id
                 WHERE f.language = 'rust' AND n.type = 'function'
                 ORDER BY n.file_id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        (row.get::<_, u32>(4)?, row.get::<_, u32>(5)?),
                        row.get::<_, String>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut map = HashMap::new();
            for file in rows.chunk_by(|a, b| a.3 == b.3) {
                let fns: Vec<RustFnRow> = file
                    .iter()
                    .map(
                        |(id, signature, qualified_name, _, lines, code)| RustFnRow {
                            id: *id,
                            signature: signature.as_deref(),
                            qualified_name: qualified_name.as_deref(),
                            lines: *lines,
                            code,
                        },
                    )
                    .collect();
                map.extend(rust_fn_shapes_of_file(&file[0].3, &fns));
            }
            self.rust_fn_shapes = Some(map);
        }
        let shapes = self.rust_fn_shapes.as_ref().expect("loaded above");
        candidates.retain(|id| {
            shapes.get(id).is_none_or(|shape| {
                rust_call_shape_admits(metadata, shape) && rust_crate_admits(caller_path, shape)
            })
        });
        Ok(candidates)
    }

    /// The candidates a Rust method call's receiver type admits (D#112; see
    /// [`RustRecv`]). A candidate's type is the owner its qualified name
    /// writes (`T.f`: an inherent or trait-impl method of `T`; `Tr.f`: a
    /// trait's). A project type with no method of that name keeps every
    /// candidate: a trait's default method or a `Deref` target runs then.
    /// Calls without a receiver type, and other languages', pass through.
    /// [`Self::rust_receiver_never`] then names, of what this keeps, what can
    /// never run.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rust_receiver_candidates(
        &mut self,
        db: &crate::storage::db::Database,
        language: &str,
        metadata: Option<&str>,
        crate_roots: &RustCrates,
        caller_path: &str,
        node_id_to_path: &HashMap<i64, String>,
        all_file_paths: &HashSet<String>,
        candidates: Vec<i64>,
    ) -> anyhow::Result<Vec<i64>> {
        if language != "rust" || candidates.is_empty() {
            return Ok(candidates);
        }
        let Some(recv) = rust_receiver(metadata, crate_roots) else {
            return Ok(candidates);
        };
        self.load_rust_types(db)?;
        self.load_node_info(db, &candidates)?;
        let owner = |id: &i64| -> Option<&str> {
            let (_, q) = self.node_info.get(id)?;
            let (owner, _) = q.rsplit_once('.')?;
            Some(owner.rsplit("::").next().unwrap_or(owner))
        };
        match recv {
            RustRecv::Project(ty, via, path) => {
                let own: Vec<i64> = candidates
                    .iter()
                    .copied()
                    .filter(|id| owner(id).is_some_and(|o| o == ty || via.as_deref() == Some(o)))
                    .collect();
                if own.is_empty() {
                    return Ok(candidates);
                }
                // Several types of that name: the one the path names, as a path
                // call through it (`Mutex::new`) is anchored (D#132).
                let Some(path) = path.filter(|_| own.len() > 1) else {
                    return Ok(own);
                };
                let tag = if matches!(path[0].as_str(), "crate" | "self" | "super") {
                    r#"{"u":"c"}"#
                } else {
                    r#"{"u":"x"}"#
                };
                let UseAnchor::At(anchor) =
                    rust_use_anchor(Some(tag), &path, caller_path, crate_roots)
                else {
                    return Ok(own);
                };
                Ok(
                    match rust_anchored_targets(&anchor, &own, node_id_to_path, db, all_file_paths)?
                    {
                        Anchored::Named(ids) | Anchored::Elsewhere(ids) if !ids.is_empty() => ids,
                        _ => own,
                    },
                )
            }
            RustRecv::Foreign(ty) => {
                let concrete = self.rust_concrete.as_ref().expect("loaded above");
                let traits = &self.rust_traits;
                let blanket = &self.rust_blanket;
                // Owners are compared by name: a type defined inside an item
                // macro (`cfg_net! { pub struct TcpStream … }`) has no node, but
                // its impls' methods do, so "not a known struct" is no proof
                // that an owner is not the project's.
                let verdicts: Vec<(i64, ForeignOwner)> = candidates
                    .iter()
                    .map(|id| {
                        let verdict = match owner(id) {
                            None => ForeignOwner::Exact,
                            Some(o) if traits.contains(o) => ForeignOwner::Exact,
                            Some(o) => {
                                let file_id = self.node_info.get(id).map(|(f, _)| *f);
                                let param = blanket.get(id).map(|b| &**b);
                                foreign_receiver_owner(&ty, o, param, || {
                                    file_id.is_some_and(|f| concrete.contains(&(f, o.to_string())))
                                })
                            }
                        };
                        (*id, verdict)
                    })
                    .collect();
                // A loose impl only where no impl on the type itself answers.
                let exact = verdicts.iter().any(|(id, v)| {
                    *v == ForeignOwner::Exact && owner(id).is_some_and(|o| !traits.contains(o))
                });
                Ok(verdicts
                    .into_iter()
                    .filter(|(_, v)| match v {
                        ForeignOwner::Exact => true,
                        ForeignOwner::Loose => !exact,
                        ForeignOwner::No => false,
                    })
                    .map(|(id, _)| id)
                    .collect())
            }
        }
    }

    /// Of a typed Rust call's candidates, those the language rules out, to be
    /// dropped from whatever the call's resolution then binds. Removed from the
    /// result, never from the pool the result is chosen from: a smaller pool
    /// can turn "several methods, bind none" into "one method, bind it", and
    /// this may only ever take an edge away.
    ///
    /// - `arc.clone()` (`Arc<T>` / `Rc<T>`): method lookup meets the pointer's
    ///   `Clone` at `&Arc<T>`, before any deref reaches `T`. The pointer is read
    ///   by name, as the parser reads these pointers everywhere
    ///   (`rust_receiver::SMART_POINTERS`), so this reads no other file.
    /// - A struct of the caller's own file with no method of that name and no
    ///   `Deref`: it cannot be an alias (the module would hold two items of one
    ///   name) and reaches no other type's methods. Its `Deref` and methods are
    ///   read from other files ([`RecvNever::read_elsewhere`]).
    ///
    /// What stays is anything that could run ([`Self::only_runnable`]).
    /// Anything less certain — an imported or unknown type — rules out nothing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rust_receiver_never(
        &mut self,
        db: &crate::storage::db::Database,
        language: &str,
        target_name: &str,
        metadata: Option<&str>,
        crate_roots: &RustCrates,
        caller_path: &str,
        candidates: &[i64],
    ) -> anyhow::Result<RecvNever> {
        if language != "rust" || candidates.is_empty() {
            return Ok(RecvNever::default());
        }
        let Some(RustRecv::Project(ty, via, _)) = rust_receiver(metadata, crate_roots) else {
            return Ok(RecvNever::default());
        };
        self.load_rust_types(db)?;
        self.load_node_info(db, candidates)?;
        let clone_of_pointer =
            target_name == "clone" && matches!(via.as_deref(), Some("Arc" | "Rc"));
        let runnable = if clone_of_pointer {
            self.only_runnable(candidates, via.as_deref().expect("matched above"), None)
        } else {
            let owns = candidates.iter().any(|id| {
                self.node_info
                    .get(id)
                    .and_then(|(_, q)| q.rsplit_once('.'))
                    .map(|(o, _)| o.rsplit("::").next().unwrap_or(o))
                    .is_some_and(|o| o == ty || via.as_deref() == Some(o))
            });
            let local = self
                .rust_concrete_at
                .contains(&(caller_path.to_string(), ty.clone()));
            if owns || !local || self.rust_deref_owners.contains(&ty) {
                return Ok(RecvNever::default());
            }
            self.only_runnable(candidates, &ty, via.as_deref())
        };
        Ok(RecvNever {
            ids: candidates
                .iter()
                .copied()
                .filter(|id| !runnable.contains(id))
                .collect(),
            read_elsewhere: !clone_of_pointer,
        })
    }

    /// File id and qualified name of every candidate not seen yet.
    fn load_node_info(
        &mut self,
        db: &crate::storage::db::Database,
        candidates: &[i64],
    ) -> anyhow::Result<()> {
        let missing: Vec<i64> = candidates
            .iter()
            .copied()
            .filter(|id| !self.node_info.contains_key(id))
            .collect();
        if !missing.is_empty() {
            self.node_info
                .extend(crate::storage::queries::get_node_files_and_qualified_names(
                    db.conn(),
                    &missing,
                )?);
        }
        Ok(())
    }

    /// The project's Rust types, traits, blanket-impl methods and `Deref`
    /// owners, read once per pass.
    fn load_rust_types(&mut self, db: &crate::storage::db::Database) -> anyhow::Result<()> {
        if self.rust_concrete.is_some() {
            return Ok(());
        }
        let mut stmt = db.conn().prepare(
            "SELECT DISTINCT n.file_id, f.path, n.name, n.type = 'interface' FROM nodes n
             JOIN files f ON f.id = n.file_id
             WHERE f.language = 'rust'
               AND n.type IN ('struct', 'enum', 'union', 'interface')",
        )?;
        let mut concrete = HashSet::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })? {
            let (file_id, path, name, is_trait) = row?;
            if is_trait {
                self.rust_traits.insert(name);
            } else {
                self.rust_concrete_at.insert((path, name.clone()));
                concrete.insert((file_id, name));
            }
        }
        self.rust_concrete = Some(concrete);
        let mut stmt = db.conn().prepare(
            "SELECT n.id, n.signature FROM nodes n
             JOIN files f ON f.id = n.file_id
             WHERE f.language = 'rust' AND n.type = 'function'
               AND n.signature LIKE '<%'",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, signature) = row?;
            if let (Some(param), _) = rust_blanket_signature(&signature) {
                self.rust_blanket.insert(id, param.into());
            }
        }
        let mut stmt = db.conn().prepare(
            "SELECT n.qualified_name FROM nodes n
             JOIN files f ON f.id = n.file_id
             WHERE f.language = 'rust' AND n.name IN ('deref', 'deref_mut')
               AND n.qualified_name LIKE '%.%'",
        )?;
        for q in stmt.query_map([], |r| r.get::<_, String>(0))? {
            if let Some((owner, _)) = q?.rsplit_once('.') {
                self.rust_deref_owners.insert(owner.to_string());
            }
        }
        Ok(())
    }

    /// The candidates that could run for a receiver of type `ty` (behind the
    /// pointer `via`, if any) that has no method of that name and no `Deref`:
    /// a method of `ty` itself — also through an impl on a reference to it,
    /// which auto-ref reaches — or of `via`, a trait's default method, a
    /// blanket impl's method. Another type's method, known or not, is not.
    fn only_runnable(&self, candidates: &[i64], ty: &str, via: Option<&str>) -> Vec<i64> {
        candidates
            .iter()
            .copied()
            .filter(|id| {
                let Some((_, q)) = self.node_info.get(id) else {
                    return true;
                };
                let Some((owner, _)) = q.rsplit_once('.') else {
                    return true;
                };
                let owner = owner.rsplit("::").next().unwrap_or(owner);
                let referent = rust_referent(owner);
                referent == ty
                    || via == Some(referent)
                    || self.rust_traits.contains(owner)
                    || self.rust_blanket.contains_key(id)
            })
            .collect()
    }

    /// A Python `from m import x` binds a module-level name, never a method:
    /// neither a class member nor a nested `def` (typed as a method too) is an
    /// attribute of its module. flask's `from flask import url_for` names a
    /// re-export of `helpers.url_for`, which the module lookup cannot follow,
    /// so the name chain took `Flask.url_for` beside it and every
    /// `url_for(...)` of the importer bound both (C4, 2026-09-28 usage
    /// evaluation). With the import unique, `prune_import_contradicted_call_edges`
    /// drops the method from the calls too. Calls are left alone: a Python call
    /// without metadata may still be `self.app.f()` or `imported_obj.f()`.
    pub(super) fn python_import_candidates(
        &mut self,
        db: &crate::storage::db::Database,
        language: &str,
        candidates: Vec<i64>,
    ) -> anyhow::Result<Vec<i64>> {
        if language != "python" {
            return Ok(candidates);
        }
        if self.methods.is_none() {
            let mut stmt = db
                .conn()
                .prepare("SELECT id FROM nodes WHERE type = 'method'")?;
            let ids = stmt
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<HashSet<i64>>>()?;
            self.methods = Some(ids);
        }
        let methods = self.methods.as_ref().expect("loaded above");
        Ok(candidates
            .into_iter()
            .filter(|id| !methods.contains(id))
            .collect())
    }

    /// [`member_call_candidates`] from memory: the deferred pass and the pending
    /// sweep run after every node of the run exists, so the set of free
    /// functions is read once instead of once per call (a query of up to
    /// hundreds of ids for each `self.x()` in a large Python tree).
    pub(super) fn member_call_candidates(
        &mut self,
        db: &crate::storage::db::Database,
        metadata: Option<&str>,
        candidates: Vec<i64>,
    ) -> anyhow::Result<Vec<i64>> {
        if !matches!(
            parse_callee_metadata(metadata),
            Some(CalleeMeta::Member | CalleeMeta::RecvType(_) | CalleeMeta::SuperType(_))
        ) {
            return Ok(candidates);
        }
        if self.free_functions.is_none() {
            let mut stmt = db.conn().prepare(
                "SELECT id FROM nodes WHERE type = 'function'
                 AND (qualified_name IS NULL OR qualified_name NOT LIKE '%.%')",
            )?;
            let ids = stmt
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<HashSet<i64>>>()?;
            self.free_functions = Some(ids);
        }
        let free = self.free_functions.as_ref().expect("loaded above");
        Ok(candidates
            .into_iter()
            .filter(|id| !free.contains(id))
            .collect())
    }

    fn load(
        &mut self,
        db: &crate::storage::db::Database,
        candidates: &[i64],
    ) -> anyhow::Result<()> {
        if self.by_last.is_none() {
            let mut map: HashMap<String, Vec<ClassNode>> = HashMap::new();
            for (id, file_id, name, nested) in crate::storage::queries::class_like_names(db.conn())?
            {
                let segs = class_path(&name);
                if let Some(last) = segs.last() {
                    map.entry(last.to_string()).or_default().push(ClassNode {
                        id,
                        file_id,
                        nested: nested || segs.len() > 1,
                    });
                }
            }
            self.by_last = Some(map);
        }
        let missing: Vec<i64> = candidates
            .iter()
            .copied()
            .filter(|id| !self.node_info.contains_key(id))
            .collect();
        if !missing.is_empty() {
            self.node_info
                .extend(crate::storage::queries::get_node_files_and_qualified_names(
                    db.conn(),
                    &missing,
                )?);
        }
        Ok(())
    }

    /// Load the class hierarchy (`inherits` edges both ways) and the file ids,
    /// once per pass.
    fn load_hierarchy(&mut self, db: &crate::storage::db::Database) -> anyhow::Result<()> {
        if self.children.is_some() {
            return Ok(());
        }
        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
        for (sub, sup) in crate::storage::queries::inherits_edges(db.conn())? {
            children.entry(sup).or_default().push(sub);
            self.parents.entry(sub).or_default().push(sup);
        }
        self.children = Some(children);
        let mut stmt = db.conn().prepare("SELECT path, id FROM files")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (path, id) = row?;
            self.file_ids.insert(path, id);
        }
        Ok(())
    }

    /// Every class inheriting, directly or not, from `seeds` (seeds excluded).
    /// [`Self::load_hierarchy`] first.
    fn subclasses(&self, seeds: &[i64]) -> HashSet<i64> {
        let Some(children) = self.children.as_ref() else {
            return HashSet::new();
        };
        let mut seen: HashSet<i64> = HashSet::new();
        let mut stack: Vec<i64> = seeds.to_vec();
        while let Some(c) = stack.pop() {
            for &sub in children.get(&c).into_iter().flatten() {
                if !seeds.contains(&sub) && seen.insert(sub) {
                    stack.push(sub);
                }
            }
        }
        seen
    }
}

/// The class path a type spelling names, template arguments dropped:
/// `SkipList<Key, Comparator>::Node` → `[SkipList, Node]`, Python's
/// `Outer.Inner` → `[Outer, Inner]`.
fn class_path(spelling: &str) -> Vec<&str> {
    let b = spelling.as_bytes();
    let (mut depth, mut start, mut i) = (0i32, 0usize, 0usize);
    let mut segs = Vec::new();
    let mut push = |from: usize, to: usize| {
        let seg = &spelling[from..to];
        let seg = seg.split('<').next().unwrap_or(seg).trim();
        if !seg.is_empty() {
            segs.push(seg);
        }
    };
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b':' if depth == 0 && b.get(i + 1) == Some(&b':') => {
                push(start, i);
                start = i + 2;
                i += 1;
            }
            b'.' if depth == 0 => {
                push(start, i);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    push(start, b.len());
    segs
}

/// The owning class path of a method's qualified name (`A::B.f` → `[A, B]`).
fn owner_path(qualified: &str) -> Vec<&str> {
    qualified
        .rsplit_once('.')
        .map(|(owner, _)| class_path(owner))
        .unwrap_or_default()
}

/// Bind a call on a receiver of class `ty` (a `::`-joined class path, e.g.
/// `Slice` or `SkipList::Iterator`) to that class's own method and, when
/// `dispatch` (not a `super()` call), to its overrides in subclasses.
///
/// A candidate's class is known by its method's qualified name, which names
/// only the last scope (`Iterator.Next` for `SkipList<K>::Iterator::Next`), so
/// the class NODE it belongs to is taken from the method's own file, else from
/// every class of that name. A nested class (`SkipList::Iterator`) answers to a
/// bare `T` only when no top-level class is named `T`; a class whose name is
/// both nested and top-level somewhere, with the method outside either's file,
/// counts as nested. A `std::` type is never the project's.
#[allow(clippy::too_many_arguments)]
pub(super) fn recv_type_targets(
    ty: &str,
    dispatch: bool,
    candidates: &[i64],
    db: &crate::storage::db::Database,
    classes: &mut ProjectClassNames,
    caller_path: &str,
    node_id_to_path: &HashMap<i64, String>,
) -> anyhow::Result<RecvTypeTargets> {
    let want = class_path(ty);
    let Some(&last) = want.last() else {
        return Ok(RecvTypeTargets::Fallback);
    };
    if want.first() == Some(&"std") {
        return Ok(RecvTypeTargets::Drop);
    }
    classes.load(db, candidates)?;
    classes.load_hierarchy(db)?;
    let classes = &*classes;
    let by_last = classes.by_last.as_ref().expect("loaded above");
    let no_classes = Vec::new();
    // The class nodes a candidate's method may belong to.
    let owners = |id: i64| -> (Vec<&str>, Vec<&ClassNode>) {
        let Some((file_id, q)) = classes.node_info.get(&id) else {
            return (Vec::new(), Vec::new());
        };
        let have = owner_path(q);
        let named = have
            .last()
            .and_then(|o| by_last.get(*o))
            .unwrap_or(&no_classes);
        let local: Vec<&ClassNode> = named.iter().filter(|c| c.file_id == *file_id).collect();
        let nodes = if local.is_empty() {
            named.iter().collect()
        } else {
            local
        };
        (have, nodes)
    };
    let (mut exact, mut nested) = (Vec::new(), Vec::new());
    for &id in candidates {
        let (have, nodes) = owners(id);
        let k = have.len().min(want.len());
        if k == 0 || have[have.len() - k..] != want[want.len() - k..] {
            continue;
        }
        let in_nested_class = nodes.iter().any(|c| c.nested);
        if have.len() > want.len() || (have.len() == want.len() && in_nested_class) {
            nested.push(id);
        } else {
            exact.push(id);
        }
    }
    let known = by_last.contains_key(last);
    let top_level = by_last
        .get(last)
        .is_some_and(|nodes| nodes.iter().any(|c| !c.nested));
    let own = if !exact.is_empty() {
        exact
    } else if !top_level {
        nested
    } else {
        Vec::new()
    };
    if own.is_empty() {
        if !known {
            return Ok(RecvTypeTargets::Drop);
        }
        return Ok(inherited_or_overridden(
            classes,
            &want,
            top_level,
            dispatch,
            candidates,
            caller_path,
            &owners,
        ));
    }
    // Several same-named classes define it: the caller's file's, else all of
    // them as an untyped member call.
    let local: Vec<i64> = own
        .iter()
        .copied()
        .filter(|id| node_id_to_path.get(id).map(String::as_str) == Some(caller_path))
        .collect();
    let (mut targets, ambiguous) = match (own.len(), local.is_empty()) {
        (1, _) => (own, false),
        (_, false) => (local, false),
        (_, true) => (own, true),
    };
    if dispatch {
        // A subclass's override runs too when the object is one (virtual
        // dispatch): binding only `T.f` left every override without a caller.
        // `inherits` edges are bound by name too, so a class whose name another
        // class shares may have another's subclasses (leveldb's `DBIter`
        // inherits `leveldb::Iterator` and was bound to `SkipList::Iterator`):
        // only a uniquely named class seeds overrides, and a candidate counts
        // only when every class node it may belong to is a subclass.
        let mut seeds = Vec::new();
        for &id in &targets {
            let (have, _) = owners(id);
            if let Some([only]) = have.last().and_then(|o| by_last.get(*o)).map(Vec::as_slice) {
                seeds.push(only.id);
            }
        }
        let overrides: Vec<(i64, Vec<i64>)> = candidates
            .iter()
            .filter(|id| !targets.contains(id))
            .map(|&id| (id, owners(id).1.iter().map(|c| c.id).collect()))
            .collect();
        if !seeds.is_empty() {
            let subclasses = classes.subclasses(&seeds);
            for (id, nodes) in overrides {
                if !nodes.is_empty() && nodes.iter().all(|c| subclasses.contains(c)) {
                    targets.push(id);
                }
            }
        }
    }
    Ok(if ambiguous {
        RecvTypeTargets::Ambiguous(targets)
    } else {
        RecvTypeTargets::Bind(targets)
    })
}

/// A call on a project class `T` that does not define the method: what runs is
/// the definition `T` inherits — the nearest ancestor's — or, dispatching, an
/// override in a subclass (a C++ pure virtual `DB::Get` has no node, its
/// overrides do). Binding the untyped member call instead reached every
/// same-named method in the caller's file (leveldb: `db->Get()` bound
/// `DBTest::Get`). `T` must name one class node: the only one of that name, else
/// the caller's file's; overrides only when the name is unique, as for a class
/// that defines the method. `Fallback` when neither side finds one (a library
/// base's method, an unbound name).
fn inherited_or_overridden<'a>(
    classes: &'a ProjectClassNames,
    want: &[&str],
    top_level: bool,
    dispatch: bool,
    candidates: &[i64],
    caller_path: &str,
    owners: &dyn Fn(i64) -> (Vec<&'a str>, Vec<&'a ClassNode>),
) -> RecvTypeTargets {
    let Some(by_last) = classes.by_last.as_ref() else {
        return RecvTypeTargets::Fallback;
    };
    let Some(named) = want.last().and_then(|l| by_last.get(*l)) else {
        return RecvTypeTargets::Fallback;
    };
    // A `::` path may be a namespace (`test::ErrorEnv`) as well as an outer
    // class, so the nesting rule is the one `recv_type_targets` applies to a
    // bare name: a top-level class answers when one exists.
    let fits: Vec<&ClassNode> = named.iter().filter(|c| !top_level || !c.nested).collect();
    let caller_file = classes.file_ids.get(caller_path);
    let start = match fits.as_slice() {
        [only] => only.id,
        _ => match fits
            .iter()
            .filter(|c| Some(&c.file_id) == caller_file)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [only] => only.id,
            _ => return RecvTypeTargets::Fallback,
        },
    };
    let cand_nodes: Vec<(i64, Vec<i64>)> = candidates
        .iter()
        .map(|&id| (id, owners(id).1.iter().map(|c| c.id).collect()))
        .collect();
    let all_in = |nodes: &[i64], set: &HashSet<i64>| {
        !nodes.is_empty() && nodes.iter().all(|c| set.contains(c))
    };
    // Nearest ancestor level defining it.
    let mut inherited: Vec<i64> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::from([start]);
    let mut level: Vec<i64> = vec![start];
    // Deep enough for any real hierarchy; `seen` already stops a cycle.
    for _ in 0..32 {
        let next: HashSet<i64> = level
            .iter()
            .flat_map(|c| classes.parents.get(c).into_iter().flatten().copied())
            .filter(|c| seen.insert(*c))
            .collect();
        if next.is_empty() {
            break;
        }
        inherited = cand_nodes
            .iter()
            .filter(|(_, nodes)| all_in(nodes, &next))
            .map(|(id, _)| *id)
            .collect();
        if !inherited.is_empty() {
            break;
        }
        level = next.into_iter().collect();
    }
    let mut targets = inherited.clone();
    if dispatch && named.len() == 1 {
        let subs = classes.subclasses(&[start]);
        for (id, nodes) in &cand_nodes {
            if all_in(nodes, &subs) && !targets.contains(id) {
                targets.push(*id);
            }
        }
    }
    if targets.is_empty() {
        RecvTypeTargets::Fallback
    } else if inherited.len() > 1 {
        // Two ancestors of one level define it (multiple bases, or a base name
        // bound to two classes): not decided by the class.
        RecvTypeTargets::Ambiguous(targets)
    } else {
        RecvTypeTargets::Bind(targets)
    }
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
    // qualified_name LIKE '%.%' — any node whose qualified_name carries a
    // `Type.` prefix. NULL qualified_name (rare) is excluded by LIKE. Chunked
    // under MAX_IN_PARAMS (issue #30).
    filter_method_ids(db.conn(), candidates, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_metadata_bare_returns_none() {
        assert!(parse_callee_metadata(None).is_none());
    }

    /// D#112: parameters are counted at the top level of the parameter list,
    /// `self` included; a list whose count can differ by build or call (a
    /// `#[cfg]` parameter, C variadics) or that does not close counts as unknown.
    #[test]
    fn rust_signature_param_count_counts_top_level_parameters() {
        let count = rust_signature_param_count;
        assert_eq!(count("() -> Result<Option<Self>>"), Some(0));
        assert_eq!(count("(&self) -> &'static str"), Some(1));
        assert_eq!(
            count("(\n    &mut self,\n    db: &Db,\n    c: &[i64],\n) -> R"),
            Some(3)
        );
        assert_eq!(
            count("(f: impl Fn(i32, i32) -> i32, m: HashMap<K, V>, t: (u8, u8))"),
            Some(3)
        );
        assert_eq!(
            count("(&self, (a, b): (i32, i32), [x, y]: [u8; 2])"),
            Some(3)
        );
        assert_eq!(count("(g: Box<dyn Fn(&str) -> Vec<u8>>)"), Some(1));
        assert_eq!(count("(a: i32, #[cfg(unix)] b: i32)"), None);
        assert_eq!(count("(fmt: *const c_char, ...)"), None);
        // A comma inside a comment is no parameter separator (review F7).
        assert_eq!(
            count("(&mut self, key: u32, // the key, as u32\n val: u32,)"),
            None
        );
        assert_eq!(count("(a: u8 /* x, y */)"), None);
        assert_eq!(count("(a: i32, b: Vec<"), None);
        assert_eq!(count("fn"), None);
    }

    /// D#112: the call's argument count must equal the callee's parameters, less
    /// `self` for a method call; a path call passes `self` itself.
    #[test]
    fn rust_call_shape_admits_checks_arity() {
        let f = |sig: &str| rust_fn_shape(Some(sig), None);
        let method = f("(&mut self, db: i32, c: &[i64])");
        let free = f("(m: Option<&str>, c: Vec<i64>, db: i32)");
        let recv1 = Some(r#"{"n":1,"q":"recv","v":"flag"}"#);
        let recv2 = Some(r#"{"n":2,"q":"recv","v":"c"}"#);
        let path3 = Some(r#"{"n":3,"q":"path","v":"resolve"}"#);
        assert!(!rust_call_shape_admits(recv1, &method));
        assert!(rust_call_shape_admits(recv2, &method));
        // UFCS through the type passes `self` itself.
        assert!(rust_call_shape_admits(
            Some(r#"{"n":3,"q":"path","v":"Classes"}"#),
            &method
        ));
        assert!(rust_call_shape_admits(path3, &free));
        assert!(!rust_call_shape_admits(
            Some(r#"{"n":3,"q":"path","v":"Names"}"#),
            &f("(&mut self, m: Option<&str>, c: Vec<i64>, db: i32)")
        ));
        assert!(rust_call_shape_admits(
            Some(r#"{"n":1,"q":"self","v":"Db"}"#),
            &f("(&self, k: u8)")
        ));
        // A module path cannot pass `self`; a type path can (F3).
        assert!(!rust_call_shape_admits(
            Some(r#"{"n":1,"q":"path","v":"tokio"}"#),
            &f("(&mut self)")
        ));
        assert!(rust_call_shape_admits(
            Some(r#"{"n":1,"q":"path","v":"process::Command"}"#),
            &f("(&mut self)")
        ));
        assert!(rust_call_shape_admits(
            Some(r#"{"n":1,"q":"path","v":"tokio"}"#),
            &f("(f: F)")
        ));
        // No count on either side: nothing to check.
        assert!(rust_call_shape_admits(
            Some(r#"{"q":"recv","v":"c"}"#),
            &method
        ));
        assert!(rust_call_shape_admits(
            recv1,
            &f("(&self, a: i32, #[cfg(x)] b: i32)")
        ));
        // A lowercase path names a type when it is the method's own lowercase
        // type (D#126), and a module otherwise.
        let enc = rust_fn_shape(Some("(&self, buf: &mut Vec<u8>)"), Some("u32.encode_to"));
        let u32_path = Some(r#"{"n":2,"q":"path","v":"u32"}"#);
        assert!(rust_call_shape_admits(u32_path, &enc));
        let close = rust_fn_shape(Some("(&mut self)"), Some("sqlite3_db.close_db"));
        assert_eq!(close.lowercase_owner.as_deref(), Some("sqlite3_db"));
        assert!(rust_call_shape_admits(
            Some(r#"{"n":1,"q":"path","v":"db::sqlite3_db"}"#),
            &close
        ));
        assert!(!rust_call_shape_admits(
            Some(r#"{"n":1,"q":"path","v":"rt"}"#),
            &close
        ));
        // An uppercase owner or a free function records none.
        assert_eq!(
            rust_fn_shape(Some("(&mut self)"), Some("Command.spawn")).lowercase_owner,
            None
        );
        let free = rust_fn_shape(Some("(x: u8)"), Some("f"));
        assert!(!free.associated && free.lowercase_owner.is_none());
        // A bare call reaches a free function, never an associated one, with or
        // without `self`; a module path neither (D#119).
        let assoc = rust_fn_shape(Some("(me: &Arc<Self>, f: F)"), Some("Handle.spawn"));
        assert!(assoc.associated && !assoc.takes_self);
        assert!(!rust_call_shape_admits(None, &assoc));
        assert!(rust_call_shape_admits(None, &free));
        assert!(!rust_call_shape_admits(
            Some(r#"{"n":2,"q":"path","v":"tokio"}"#),
            &assoc
        ));
        assert!(rust_call_shape_admits(
            Some(r#"{"n":2,"q":"path","v":"Handle"}"#),
            &assoc
        ));
        assert!(rust_call_shape_admits(
            Some(r#"{"n":1,"q":"path","v":"tokio"}"#),
            &free
        ));
    }

    /// A `fn` nested in a method of the same owner is a free function of that
    /// body; an `impl` inside a function keeps its functions associated.
    #[test]
    fn rust_fn_shapes_of_file_tells_nested_fns_from_associated_ones() {
        let row = |id, q, lines| RustFnRow {
            id,
            signature: Some("(a: u8)"),
            qualified_name: Some(q),
            lines,
            code: "fn f(a: u8) {}",
        };
        let rows = [
            row(1, "Interest.to_mio", (10, 20)),
            row(2, "Interest.mio_add", (11, 13)),
            row(3, "Interest.other", (21, 22)),
            row(4, "outer", (30, 40)),
            row(5, "Local.helper", (32, 33)),
            // Two one-line methods on the same line are siblings.
            row(6, "Pair.a", (50, 50)),
            row(7, "Pair.b", (50, 50)),
        ];
        let shapes: HashMap<i64, RustFnShape> = rust_fn_shapes_of_file("src/a.rs", &rows)
            .into_iter()
            .collect();
        let assoc = |id: i64| shapes[&id].associated;
        assert!(assoc(1) && !assoc(2) && assoc(3));
        assert!(!assoc(4) && assoc(5));
        assert!(assoc(6) && assoc(7));
    }

    mod crate_roots {
        use super::*;
        use tempfile::TempDir;

        fn write(dir: &std::path::Path, rel: &str, content: &str) {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }

        #[test]
        fn package_name_is_normalized_to_module_spelling() {
            assert_eq!(
                parse_cargo_package_name("[package]\nname = \"code-graph-mcp\"\n").as_deref(),
                Some("code_graph_mcp")
            );
            assert_eq!(
                parse_cargo_package_name("[package]\nname='single-quoted'\n").as_deref(),
                Some("single_quoted")
            );
        }

        #[test]
        fn name_outside_the_package_table_is_ignored() {
            // A `name = ...` key under any other table (a dependency, a bin
            // target, `[workspace.package]`) must not be mistaken for the
            // crate root — stripping a wrong root re-opens the phantom-edge
            // direction this fix has to stay clear of.
            assert_eq!(
                parse_cargo_package_name(
                    "[dependencies]\nname = \"not-the-crate\"\n\n[[bin]]\nname = \"also-not\"\n"
                ),
                None
            );
            assert_eq!(
                parse_cargo_package_name("[package]\n# name = \"commented-out\"\n"),
                None
            );
            // Workspace-inherited name: no literal here, so nothing to strip.
            assert_eq!(
                parse_cargo_package_name("[package]\nname.workspace = true\n"),
                None
            );
        }

        #[test]
        fn collects_root_and_nested_manifests_but_not_build_dirs() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            write(root, "Cargo.toml", "[package]\nname = \"top-level\"\n");
            write(
                root,
                "scripts/poc/Cargo.toml",
                "[package]\nname = \"nested-poc\"\n",
            );
            // Must be skipped: build output and vendored sources are not this
            // project's crates, and walking them is what turns a cheap manifest
            // scan into a full-tree scan.
            write(
                root,
                "target/debug/build/dep-1/Cargo.toml",
                "[package]\nname = \"build-artifact\"\n",
            );
            write(
                root,
                "vendor/other/Cargo.toml",
                "[package]\nname = \"vendored\"\n",
            );

            let names = collect_rust_crates(root);
            assert!(names.contains("top_level"), "got: {names:?}");
            assert!(names.contains("nested_poc"), "got: {names:?}");
            assert!(!names.contains("build_artifact"), "got: {names:?}");
            assert!(!names.contains("vendored"), "got: {names:?}");
        }
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
    fn parse_metadata_recv_type() {
        // Python receiver-type qualifier (issue #32 cause 2).
        let m = parse_callee_metadata(Some(r#"{"q":"rtype","v":"DataWriter"}"#)).unwrap();
        assert!(matches!(m, CalleeMeta::RecvType(ref t) if t == "DataWriter"));
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
        assert!(
            parse_callee_metadata(Some(r#"{"python_module":"foo","is_module_import":false}"#))
                .is_none()
        );
    }

    mod pending_qualifier {
        use super::*;
        use crate::domain::REL_CALLS;
        use crate::storage::db::Database;
        use crate::storage::queries::{
            insert_node, insert_pending_unresolved_call, list_pending_unresolved_calls,
            upsert_file, FileRecord, NodeRecord,
        };
        use tempfile::TempDir;

        fn pyfile(conn: &rusqlite::Connection, path: &str) -> i64 {
            upsert_file(
                conn,
                &FileRecord {
                    path: path.into(),
                    blake3_hash: format!("h-{path}"),
                    last_modified: 1,
                    language: Some("python".into()),
                },
            )
            .unwrap()
        }
        fn method(
            conn: &rusqlite::Connection,
            name: &str,
            qname: Option<&str>,
            file_id: i64,
        ) -> i64 {
            insert_node(
                conn,
                &NodeRecord {
                    file_id,
                    node_type: "function".into(),
                    name: name.into(),
                    qualified_name: qname.map(String::from),
                    start_line: 1,
                    end_line: 3,
                    code_content: format!("def {name}(self): pass"),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap()
        }
        fn call_targets(conn: &rusqlite::Connection, src: i64) -> Vec<i64> {
            let mut stmt = conn.prepare(
                "SELECT target_id FROM edges WHERE source_id=?1 AND relation=?2 ORDER BY target_id"
            ).unwrap();
            stmt.query_map(rusqlite::params![src, REL_CALLS], |r| r.get::<_, i64>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }

        /// H1 regression: the pending-call sweep must apply the SAME callee-qualifier
        /// filtering Phase 2 does. A buffered Python `w.write()` whose receiver type
        /// was inferred as `DataWriter` (rtype qualifier) must bind ONLY to
        /// `DataWriter.write`, never to a same-named `Profile.write` on a sibling
        /// class. The old sweep ignored the qualifier and bound by bare name → it
        /// wired the wrong same-name sibling as a false caller, persisting until
        /// rebuild-index (silent incremental graph corruption).
        #[test]
        fn pending_sweep_applies_recv_type_qualifier() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("p.db")).unwrap();
            let conn = db.conn();
            let f_app = pyfile(conn, "app.py");
            let f_writer = pyfile(conn, "writer.py");
            let f_other = pyfile(conn, "other.py");

            let run = method(conn, "run", None, f_app);
            let dw_write = method(conn, "write", Some("DataWriter.write"), f_writer);
            let pf_write = method(conn, "write", Some("Profile.write"), f_other);

            // `w = DataWriter(); w.write()` buffered while no `write` was indexed yet.
            insert_pending_unresolved_call(
                conn,
                run,
                "write",
                "python",
                Some(r#"{"q":"rtype","v":"DataWriter"}"#),
            )
            .unwrap();

            let added = resolve_pending_calls(&db, &Default::default()).unwrap();
            let targets = call_targets(conn, run);

            assert_eq!(
                added, 1,
                "exactly one edge should bind (DataWriter.write); got {added}"
            );
            assert_eq!(
                targets,
                vec![dw_write],
                "rtype qualifier must bind DataWriter.write only"
            );
            assert!(
                !targets.contains(&pf_write),
                "must NOT wire the wrong same-name sibling Profile.write"
            );
            assert_eq!(
                list_pending_unresolved_calls(conn).unwrap().len(),
                0,
                "the resolved pending row must be drained"
            );
        }

        /// A typed call whose class lacks the method resolves as the untyped member
        /// call it is. The deferred pass's same-file tier holds member candidates
        /// only, so a FREE function named like the method (`def f(): a.f()`) is not
        /// in it and the call binds cross-file. The sweep counted the caller as a
        /// same-file candidate by name alone and bound nothing (D#97).
        #[test]
        fn pending_sweep_fallback_free_function_caller_binds_cross_file() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("p.db")).unwrap();
            let conn = db.conn();
            let f_app = pyfile(conn, "app.py");
            let f_other = pyfile(conn, "other.py");
            let caller = method(conn, "f", None, f_app);
            insert_node(
                conn,
                &NodeRecord {
                    file_id: f_app,
                    node_type: "class".into(),
                    name: "A".into(),
                    qualified_name: Some("A".into()),
                    start_line: 5,
                    end_line: 6,
                    code_content: "class A: pass".into(),
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
            let b_f = method(conn, "f", Some("B.f"), f_other);
            insert_pending_unresolved_call(
                conn,
                caller,
                "f",
                "python",
                Some(r#"{"q":"rtype","v":"A"}"#),
            )
            .unwrap();

            resolve_pending_calls(&db, &Default::default()).unwrap();
            assert_eq!(
                call_targets(conn, caller),
                vec![b_f],
                "a free-function caller is no same-file member candidate"
            );
        }

        /// Three same-named classes `A` define `f`, none in the caller's file: the
        /// class does not decide, so the sweep binds all three (no proximity
        /// refinement, which would keep only `app/sub/a.py`'s) and marks each edge
        /// `amb`, as the deferred pass's `RecvTypeTargets::Ambiguous` arm does.
        #[test]
        fn pending_sweep_typed_call_on_same_named_classes_binds_all_marked_amb() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("p.db")).unwrap();
            let conn = db.conn();
            let f_app = pyfile(conn, "app/main.py");
            let caller = method(conn, "run", None, f_app);
            let mut want = Vec::new();
            for path in ["app/sub/a.py", "x/a.py", "y/z/a.py"] {
                let file = pyfile(conn, path);
                insert_node(
                    conn,
                    &NodeRecord {
                        file_id: file,
                        node_type: "class".into(),
                        name: "A".into(),
                        qualified_name: Some("A".into()),
                        start_line: 1,
                        end_line: 3,
                        code_content: "class A: pass".into(),
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
                want.push(method(conn, "f", Some("A.f"), file));
            }
            insert_pending_unresolved_call(
                conn,
                caller,
                "f",
                "python",
                Some(r#"{"q":"rtype","v":"A"}"#),
            )
            .unwrap();

            resolve_pending_calls(&db, &Default::default()).unwrap();
            want.sort_unstable();
            assert_eq!(
                call_targets(conn, caller),
                want,
                "all three `A.f`, unrefined"
            );
            let metas: Vec<String> = conn
                .prepare("SELECT metadata FROM edges WHERE source_id = ?1")
                .unwrap()
                .query_map([caller], |r| r.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(metas.len(), 3);
            for m in &metas {
                assert_eq!(
                    m, r#"{"amb":1,"q":"rtype","v":"A"}"#,
                    "undecided bind keeps its type, marked"
                );
            }
        }

        /// Bare (no-qualifier) pending calls keep the existing behavior: a unique
        /// same-language target still resolves. Negative control — proves the
        /// qualifier gate doesn't break the common bare path.
        #[test]
        fn pending_sweep_bare_call_still_resolves() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("p.db")).unwrap();
            let conn = db.conn();
            let f_app = pyfile(conn, "app.py");
            let f_util = pyfile(conn, "util.py");
            let run = method(conn, "run", None, f_app);
            let helper = method(conn, "helper", None, f_util);
            insert_pending_unresolved_call(conn, run, "helper", "python", None).unwrap();

            let added = resolve_pending_calls(&db, &Default::default()).unwrap();
            assert_eq!(added, 1, "bare unique call must still resolve");
            assert_eq!(call_targets(conn, run), vec![helper]);
        }

        /// v49 audit fix: a Self/stype-qualified buffered call whose type filter
        /// comes up empty must bind NOTHING and drain (mirroring Phase-2, which
        /// drops SelfType/SelfRecv on empty), NOT fall back to the bare candidate
        /// set. Before the fix the sweep bare-fell-back → it wired a false edge to
        /// a same-name sibling on the wrong type. (This qualifier can't reach the
        /// buffer in production today — latent parity — so the test buffers a
        /// crafted row directly.)
        #[test]
        fn pending_sweep_self_type_empty_filter_binds_nothing() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("p.db")).unwrap();
            let conn = db.conn();
            let f_app = pyfile(conn, "app.py");
            let f_other = pyfile(conn, "other.py");
            let run = method(conn, "run", None, f_app);
            // Only `Profile.write` exists — no `DataWriter.write`.
            let _pf_write = method(conn, "write", Some("Profile.write"), f_other);

            // Buffered `self.write()` whose impl type is DataWriter (stype). No
            // DataWriter.write in the project → filter empty.
            insert_pending_unresolved_call(
                conn,
                run,
                "write",
                "python",
                Some(r#"{"q":"stype","v":"DataWriter"}"#),
            )
            .unwrap();

            let added = resolve_pending_calls(&db, &Default::default()).unwrap();
            assert_eq!(added, 0,
                "empty stype filter must bind NOTHING (no bare fallback to Profile.write); got {added}");
            assert!(
                call_targets(conn, run).is_empty(),
                "no call edge may be created for an unmatched stype qualifier"
            );
            assert_eq!(
                list_pending_unresolved_calls(conn).unwrap().len(),
                0,
                "the row must be drained (dropped), never left buffered forever"
            );
        }

        /// `idx_nodes_file_name` must be a pure access path: the same edges, with
        /// and without it.
        ///
        /// A SQLite index cannot change a result set by design, but "by design"
        /// is the argument, not the evidence — and the corpus A/B that motivated
        /// this index compared two runs that both inserted ZERO edges, which is a
        /// vacuous equality (the trap the post-pass rewrite already had to dodge).
        /// So this fixture is built so the pass actually FIRES: it inserts exactly
        /// one edge, and the assertion is that both index states produce the same
        /// one.
        ///
        /// Shape: `app.py` calls a bare `helper` that currently resolves to the
        /// WRONG same-name node in `other.py`, while `app.py` carries a unique
        /// internal import binding `helper` to `util.py`. That is precisely the
        /// case `bind_calls_to_imported_targets` exists to fix, and it exercises
        /// the `NOT EXISTS (… ln.file_id = ? AND ln.name = ?)` predicate the index
        /// serves — `app.py` defines no `helper`, so the subquery must come back
        /// empty by SEARCHING, not by short-circuiting.
        #[test]
        fn composite_file_name_index_does_not_change_which_edges_bind() {
            use crate::domain::REL_IMPORTS;
            use crate::storage::queries::insert_edge;

            /// Builds the firing fixture and returns (inserted, full edge set).
            fn run_once(drop_index: bool) -> (usize, Vec<(i64, i64, String)>) {
                let tmp = TempDir::new().unwrap();
                let db = Database::open(&tmp.path().join("p.db")).unwrap();
                let conn = db.conn();

                if drop_index {
                    conn.execute_batch("DROP INDEX IF EXISTS idx_nodes_file_name;")
                        .unwrap();
                    let gone: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM sqlite_master
                             WHERE type='index' AND name='idx_nodes_file_name'",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(gone, 0, "the no-index arm must really lack the index");
                }

                let f_app = pyfile(conn, "app.py");
                let f_util = pyfile(conn, "util.py");
                let f_other = pyfile(conn, "other.py");

                let run = method(conn, "run", None, f_app);
                let imported_helper = method(conn, "helper", None, f_util);
                let decoy_helper = method(conn, "helper", None, f_other);

                // app.py imports `helper` from util.py — the unique internal
                // import. Its source must live in app.py: the pass keys the
                // import off `mn.file_id`.
                let app_mod = method(conn, "<module>", None, f_app);
                insert_edge(conn, app_mod, imported_helper, REL_IMPORTS, None).unwrap();

                // The bare call currently points at the WRONG same-name node.
                insert_edge(conn, run, decoy_helper, REL_CALLS, None).unwrap();

                let inserted = bind_calls_to_imported_targets(&db, &PostPassScope::Global).unwrap();

                let mut stmt = conn
                    .prepare(
                        "SELECT source_id, target_id, relation FROM edges
                         ORDER BY source_id, target_id, relation",
                    )
                    .unwrap();
                let edges: Vec<(i64, i64, String)> = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                (inserted, edges)
            }

            let (with_idx, edges_with) = run_once(false);
            let (without_idx, edges_without) = run_once(true);

            // Non-vacuity first: an equality between two zeros would prove nothing.
            assert_eq!(
                with_idx, 1,
                "fixture must actually FIRE — a 0 == 0 comparison is not evidence"
            );
            assert_eq!(
                without_idx, with_idx,
                "the index must not change how many edges bind"
            );
            assert_eq!(
                edges_without, edges_with,
                "the index must not change WHICH edges exist"
            );
        }

        /// v49 audit fix: same parity for the Path qualifier — empty filter binds
        /// nothing and drains, never bare-falls-back (Phase-2 drops Path on empty).
        #[test]
        fn pending_sweep_path_empty_filter_binds_nothing() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("p.db")).unwrap();
            let conn = db.conn();
            let f_app = pyfile(conn, "app.py");
            let f_other = pyfile(conn, "other.py");
            let run = method(conn, "run", None, f_app);
            // A same-name `helper` exists, but on a file/qualified-name that does
            // NOT match the `foo::bar` path segments → path filter empty.
            let _helper = method(conn, "helper", Some("Unrelated.helper"), f_other);

            insert_pending_unresolved_call(
                conn,
                run,
                "helper",
                "python",
                Some(r#"{"q":"path","v":"foo::bar"}"#),
            )
            .unwrap();

            let added = resolve_pending_calls(&db, &Default::default()).unwrap();
            assert_eq!(
                added, 0,
                "empty path filter must bind NOTHING (no bare fallback); got {added}"
            );
            assert!(
                call_targets(conn, run).is_empty(),
                "no call edge may be created for an unmatched path qualifier"
            );
            assert_eq!(
                list_pending_unresolved_calls(conn).unwrap().len(),
                0,
                "the row must be drained (dropped), never left buffered forever"
            );
        }
    }

    mod confidence {
        use super::*;
        use crate::domain::{REL_CALLS, REL_IMPORTS, REL_REFERENCES};
        use crate::storage::db::Database;
        use crate::storage::queries::{
            insert_edge, insert_node, upsert_file, FileRecord, NodeRecord,
        };
        use tempfile::TempDir;

        fn node(name: &str, file_id: i64) -> NodeRecord {
            NodeRecord {
                file_id,
                node_type: "function".into(),
                name: name.into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: format!("function {name}() {{}}"),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            }
        }
        fn file(conn: &rusqlite::Connection, path: &str, lang: &str) -> i64 {
            upsert_file(
                conn,
                &FileRecord {
                    path: path.into(),
                    blake3_hash: format!("h-{path}"),
                    last_modified: 1,
                    language: Some(lang.into()),
                },
            )
            .unwrap()
        }
        fn conf_of(conn: &rusqlite::Connection, s: i64, t: i64, rel: &str) -> String {
            conn.query_row(
                "SELECT confidence FROM edges WHERE source_id=?1 AND target_id=?2 AND relation=?3",
                rusqlite::params![s, t, rel],
                |r| r.get(0),
            )
            .unwrap()
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

            let downgraded = classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(
                downgraded, 3,
                "3 cross-file calls/refs edges downgraded (C->D, E->F, I->J)"
            );

            assert_eq!(
                conf_of(conn, a, b, REL_CALLS),
                "extracted",
                "same-file call stays extracted"
            );
            assert_eq!(
                conf_of(conn, c, d, REL_CALLS),
                "inferred",
                "cross-file unique name → inferred"
            );
            assert_eq!(
                conf_of(conn, e, f_target, REL_CALLS),
                "ambiguous",
                "cross-file duplicate name → ambiguous"
            );
            assert_eq!(
                conf_of(conn, g, h, REL_IMPORTS),
                "extracted",
                "imports stays extracted cross-file"
            );
            assert_eq!(
                conf_of(conn, i, j, REL_REFERENCES),
                "inferred",
                "cross-file unique reference → inferred"
            );
        }

        /// Import-corroboration: a cross-file call whose target NAME is duplicated
        /// (so name-count alone → `ambiguous`) but whose caller file explicitly
        /// imports THAT exact target must classify as `inferred`, not `ambiguous`.
        /// The import binds the bare name to one node, so the edge is
        /// import-resolved (v0.59), not a bare-name guess — relabeling it
        /// `ambiguous` would hide it under the confidence floor and callgraph/impact
        /// would show no callee for a precisely-bound call. Extremely common in
        /// JS/TS (any name defined in ≥2 files: process / handler / run / init).
        #[test]
        fn classify_import_corroborated_duplicate_stays_visible() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("c.db")).unwrap();
            let conn = db.conn();
            let f_main = file(conn, "src/main.ts", "typescript");
            let f_helpers = file(conn, "src/helpers.ts", "typescript");
            let f_other = file(conn, "src/other.ts", "typescript");

            // run (main) calls process, resolver-bound to helpers.process
            let run = insert_node(conn, &node("run", f_main)).unwrap();
            let proc_helpers = insert_node(conn, &node("process", f_helpers)).unwrap();
            insert_node(conn, &node("process", f_other)).unwrap(); // duplicate same-lang name
            insert_edge(conn, run, proc_helpers, REL_CALLS, None).unwrap();
            // main's module imports process FROM helpers (the disambiguating edge)
            let mod_main = insert_node(conn, &node("<module>", f_main)).unwrap();
            insert_edge(conn, mod_main, proc_helpers, REL_IMPORTS, None).unwrap();

            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(
                conf_of(conn, run, proc_helpers, REL_CALLS), "inferred",
                "import-corroborated call to a duplicate-named target must be inferred, not ambiguous",
            );

            // Control: a call to the OTHER (non-imported) duplicate stays ambiguous.
            let caller2 = insert_node(conn, &node("caller2", f_helpers)).unwrap();
            let proc_other = conn
                .query_row(
                    "SELECT id FROM nodes WHERE name='process' AND file_id=?1",
                    [f_other],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap();
            insert_edge(conn, caller2, proc_other, REL_CALLS, None).unwrap();
            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(
                conf_of(conn, caller2, proc_other, REL_CALLS),
                "ambiguous",
                "call to a duplicate-named target with no corroborating import stays ambiguous",
            );
        }

        /// M1: a cross-file call resolved by a TYPE/PATH qualifier (self / stype /
        /// rtype / path) is a precise structural binding, not a bare-name guess —
        /// so it must NOT be downgraded to `ambiguous` (and hidden by the default
        /// confidence floor) just because the bare name is duplicated among
        /// same-language nodes. Only import-corroborated edges were exempt before;
        /// qualifier-resolved edges had no equivalent, so `impl Database`
        /// cross-file `self.method()` calls (and Rust `Alpha::foo()` path calls)
        /// vanished from the default callgraph/impact. Mirrors the import exemption.
        #[test]
        fn classify_exempts_qualifier_resolved_edge_from_ambiguous() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("c.db")).unwrap();
            let conn = db.conn();
            let f1 = file(conn, "src/a.rs", "rust");
            let f2 = file(conn, "src/b.rs", "rust");
            let f3 = file(conn, "src/c.rs", "rust");

            // E calls `validate`, resolved by a self-type qualifier to b.rs::validate.
            let e = insert_node(conn, &node("E", f1)).unwrap();
            let v_target = insert_node(conn, &node("validate", f2)).unwrap();
            insert_node(conn, &node("validate", f3)).unwrap(); // same-lang duplicate name
            insert_edge(
                conn,
                e,
                v_target,
                REL_CALLS,
                Some(r#"{"q":"stype","v":"Alpha"}"#),
            )
            .unwrap();

            // Control: a BARE call to the same duplicate-named target stays ambiguous.
            let g = insert_node(conn, &node("G", f1)).unwrap();
            insert_edge(conn, g, v_target, REL_CALLS, None).unwrap();

            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(conf_of(conn, e, v_target, REL_CALLS), "inferred",
                "a stype-qualifier-resolved edge to a duplicate-named target must stay inferred, not ambiguous");
            assert_eq!(
                conf_of(conn, g, v_target, REL_CALLS),
                "ambiguous",
                "a bare call to the same duplicate-named target stays ambiguous (control)"
            );
        }

        /// A `path`-qualifier edge (Rust `crate::a::foo()`) gets the same exemption.
        #[test]
        fn classify_exempts_path_qualifier_edge() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("c.db")).unwrap();
            let conn = db.conn();
            let f1 = file(conn, "src/a.rs", "rust");
            let f2 = file(conn, "src/b.rs", "rust");
            let f3 = file(conn, "src/c.rs", "rust");
            let e = insert_node(conn, &node("E", f1)).unwrap();
            let t = insert_node(conn, &node("foo", f2)).unwrap();
            insert_node(conn, &node("foo", f3)).unwrap();
            insert_edge(conn, e, t, REL_CALLS, Some(r#"{"q":"path","v":"b"}"#)).unwrap();
            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(
                conf_of(conn, e, t, REL_CALLS),
                "inferred",
                "path-qualifier-resolved edge must stay inferred despite the duplicate name"
            );
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

            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(conf_of(conn, e, f_target, REL_CALLS), "ambiguous");
            // re-run, no change → still ambiguous (stable)
            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(conf_of(conn, e, f_target, REL_CALLS), "ambiguous");

            // remove the duplicate node → name now unique → inferred on re-run
            conn.execute("DELETE FROM nodes WHERE id=?1", [dup])
                .unwrap();
            classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
            assert_eq!(
                conf_of(conn, e, f_target, REL_CALLS),
                "inferred",
                "removing the duplicate must flip ambiguous→inferred"
            );
        }

        /// D#162: a same-file Rust method call on a receiver the source leaves
        /// untyped (`self.0.poll()`, `f().m()`) is bound by its name alone, so it
        /// is labelled by the name's count like a cross-file call. Every other
        /// same-file edge keeps `extracted`: a typed receiver, a bare call, a
        /// local-variable receiver (`recv`), and the same shape in another
        /// language.
        #[test]
        fn classify_labels_rust_untyped_same_file_member_call_by_name_count() {
            let tmp = TempDir::new().unwrap();
            let db = Database::open(&tmp.path().join("c.db")).unwrap();
            let conn = db.conn();
            let wrap = file(conn, "src/wrap.rs", "rust");
            let inner = file(conn, "src/inner.rs", "rust");
            let js = file(conn, "src/wrap.js", "javascript");
            let js2 = file(conn, "src/inner.js", "javascript");

            let get = insert_node(conn, &node("get", wrap)).unwrap();
            let poll = insert_node(conn, &node("poll", wrap)).unwrap();
            insert_node(conn, &node("poll", inner)).unwrap(); // the name is duplicated
            let spin = insert_node(conn, &node("spin", wrap)).unwrap(); // unique
            let edge = |s: i64, t: i64, meta: Option<&str>| {
                conn.execute("DELETE FROM edges", []).unwrap();
                insert_edge(conn, s, t, REL_CALLS, meta).unwrap();
                classify_edge_confidence(&db, &PostPassScope::Global).unwrap();
                conf_of(conn, s, t, REL_CALLS)
            };
            assert_eq!(
                edge(get, poll, Some(r#"{"n":0,"q":"member"}"#)),
                "ambiguous",
                "untyped member call, duplicated name: a by-name guess among several"
            );
            assert_eq!(
                edge(get, poll, Some(r#"{"n":0,"q":"chain"}"#)),
                "ambiguous",
                "untyped chain call, duplicated name"
            );
            assert_eq!(
                edge(get, spin, Some(r#"{"n":0,"q":"member"}"#)),
                "inferred",
                "untyped member call, unique name: stays above the default floor"
            );
            assert_eq!(
                edge(
                    get,
                    poll,
                    Some(r#"{"n":0,"q":"member","rk":"p","rt":"Wrap"}"#)
                ),
                "extracted",
                "a typed receiver chose the candidate by its type"
            );
            assert_eq!(
                edge(
                    get,
                    poll,
                    Some(r#"{"amb":1,"n":0,"q":"member","rk":"p","rt":"Wrap"}"#)
                ),
                "ambiguous",
                "a typed call its type did not decide is a by-name guess again"
            );
            assert_eq!(edge(get, poll, None), "extracted", "a bare same-file call");
            assert_eq!(
                edge(get, poll, Some(r#"{"n":0,"q":"recv","v":"x"}"#)),
                "extracted",
                "a local-variable receiver keeps the same-file label"
            );

            let jget = insert_node(conn, &node("get", js)).unwrap();
            let jpoll = insert_node(conn, &node("poll", js)).unwrap();
            insert_node(conn, &node("poll", js2)).unwrap();
            assert_eq!(
                edge(jget, jpoll, Some(r#"{"n":0,"q":"member"}"#)),
                "extracted",
                "only Rust: another language's same-file member call keeps its label"
            );
        }
    }
}
