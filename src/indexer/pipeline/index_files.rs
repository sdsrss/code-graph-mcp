//! Single-pass batched indexer. Phases share local state (transaction,
//! atomics, batch_parsed, name_to_ids, global_name_map) so the function
//! itself stays whole — the *helpers* that feed it (context, embedding,
//! Python module map, ambiguity refinement, pending-call sweep) live in
//! sibling modules.
//!
//! Phase outline:
//! - 0: delete files; pre-cascade-buffer inbound calls into pending so
//!   B → A.foo doesn't silently vanish when only A is in `delete_paths`.
//! - 1a: parallel CPU work (read + parse + extract nodes) via rayon.
//! - 1b: sequential DB inserts (file row, node rows; cascades old nodes).
//! - 2: extract relations, resolve to edges with same-file → same-language
//!   → drop/global tier order; buffer unresolved bare-name same-language
//!   calls into pending instead of dropping; track external imports/symbols.
//! - 2b / 2b-ext: virtual `<external>` nodes for unresolved imports/traits.
//! - 2c: restore cross-file inbound edges that cascade-delete just stripped.
//! - 3: build context strings (parallel), batch-update, then embed outside tx.
//! - 2c sweep: drain `pending_unresolved_calls` against the new node state.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use crate::domain::{
    is_cross_file_call_noise, max_file_size, REL_CALLS, REL_IMPLEMENTS, REL_IMPORTS, REL_INHERITS,
    REL_REFERENCES, REL_ROUTES_TO,
};
use crate::embedding::context::{build_context_string, NodeContext};
use crate::embedding::model::EmbeddingModel;
use crate::indexer::merkle::hash_file;
use crate::parser::relations::extract_relations_from_tree;
use crate::parser::treesitter::{extract_nodes_from_tree, parse_tree};
use crate::storage::db::Database;
use crate::storage::queries::{
    delete_files_by_paths, delete_nodes_by_file, get_all_node_names_with_ids, get_edges_batch,
    get_inbound_cross_file_edges, get_nodes_by_file_path, get_nodes_with_files_by_ids,
    insert_edge_cached, insert_node_cached, update_context_strings_batch, upsert_file, FileRecord,
    NodeRecord, NodeResult,
};
use crate::utils::config::detect_language;
use crate::utils::tokenizer::split_identifier;

use super::context::{categorize_edges, format_route_from_metadata};
use super::embed::embed_and_store_batch;
use super::js_modules::{
    resolve_c_include_path, resolve_js_module_targets, resolve_js_specifier_path,
    resolve_php_include_path,
};
use super::python_modules::{
    build_python_module_map, project_module_files, resolve_python_module_targets,
};
use super::resolve::{
    bind_calls_to_imported_targets, classify_edge_confidence, prune_import_contradicted_call_edges,
    refine_ambiguous_targets, resolve_pending_calls,
};
use super::{IndexPhase, IndexResult, IndexStats, ProgressFn};

/// Heuristic: does a `.h` header contain C++-specific constructs? `.h` is C-vs-C++
/// ambiguous by extension (detect_language maps it to C), and the C grammar cannot
/// extract `class`/`namespace` symbols — so a C++ class in a `.h` header is silently
/// dropped. When any of these markers is present the header is parsed as C++ instead.
/// The markers are C++-only (`::`, access specifiers, `class`/`namespace`/`template`)
/// so a pure-C header stays C; a false positive is low-harm because the C++ grammar
/// is a near-superset of C and still extracts C functions/structs/#includes.
fn looks_like_cpp_header(source: &str) -> bool {
    source.contains("::")
        || source.contains("public:")
        || source.contains("private:")
        || source.contains("protected:")
        || source.contains("class ")
        || source.contains("namespace ")
        || source.contains("template<")
        || source.contains("template <")
}

/// Batch size for streaming indexing. Each batch processes Phase 1+2
/// then drops heavyweight data (ASTs, source strings) before the next batch.
pub(super) const BATCH_SIZE: usize = 500;

/// Files touched in one run before it refreshes query-planner statistics.
///
/// Set from the two measured ends rather than picked round: at 1 file the
/// ANALYZE is pure overhead on the `ensure_file_indexed` query path (+10 ms of
/// ~70 ms), and by a couple of hundred files it has already paid for itself
/// (this repo's 232-file full index went 1.45 s -> 1.35 s, and a real
/// 2 052-file repo 13.16 s -> 8.52 s). Anything in between is cheap either way,
/// so the exact value is not load-bearing — only that a single-file refresh
/// falls below it and a real indexing run does not.
const STATS_REFRESH_MIN_FILES: usize = 50;

// CPU-bound parse result — produced in parallel, consumed sequentially for DB insert
struct FilePreParsed {
    rel_path: String,
    source: String,
    language: String,
    tree: tree_sitter::Tree,
    hash: String,
    last_modified: i64,
    parsed_nodes: Vec<crate::parser::treesitter::ParsedNode>,
}

// Heavyweight per-file data used during Phase 1+2, dropped after each batch
#[allow(dead_code)]
struct FileParsed {
    rel_path: String,
    source: String,
    language: String,
    tree: tree_sitter::Tree,
    file_id: i64,
    node_ids: Vec<i64>,
    node_names: Vec<String>,
    // Qualified names parallel to node_ids/node_names (None for <module>).
    // Needed so Phase-2 source resolution can match a relation's
    // qualified scope_name (`Class.method`) against class-based-language
    // method nodes, whose bare `name` is just `method`.
    node_qualified_names: Vec<Option<String>>,
    // Node types parallel to node_ids/node_names. Needed so inherits/implements
    // source resolution can reject a same-named function/method (a C++ inline
    // constructor shares its class's name) — only a type node can be a supertype.
    node_types: Vec<String>,
}

/// The counters Phase 1a bumps from rayon worker threads, so they are atomics
/// rather than the plain `usize`s the sequential phases use. `parse_errors` is
/// deliberately NOT a skip: tree-sitter's error recovery still returns a tree,
/// so extraction proceeds best-effort — the count is what makes that
/// partial-extraction risk observable without any schema change.
#[derive(Default)]
struct SkipCounters {
    size: AtomicUsize,
    parse: AtomicUsize,
    read: AtomicUsize,
    hash: AtomicUsize,
    language: AtomicUsize,
    parse_errors: AtomicUsize,
}

/// A file Phase 1a could not turn into symbols, but whose CONTENT IDENTITY it
/// nonetheless established (oversize, or readable-but-unparsable).
///
/// Phase 1b records these with their current hash and purges whatever nodes the
/// file had before. Without that, the old nodes stayed in the graph forever —
/// `upsert_file` and `delete_nodes_by_file` both live on the parsed path, so a
/// file that grew past `max_file_size` (or stopped parsing) kept answering
/// `show` / `callgraph` / `dead-code` with symbols that no longer exist. The
/// stored hash never advanced either, so `compute_diff` re-reported the file as
/// changed on every single run and `ensure_file_indexed` re-ran the whole
/// pipeline on every query touching it (indexing audit 2026-08-02 IDX-1).
///
/// Read and hash failures deliberately do NOT land here: those are the
/// transient, environmental failures (a permission blip, a file being rewritten
/// under us), and purging a file's symbols because one read failed is the same
/// destructive-on-transient-state mistake the `<external>` exemption exists for.
struct SkippedFile {
    rel_path: String,
    hash: String,
    last_modified: i64,
    language: String,
}

/// Phase 1a output: files that produced symbols, plus files whose identity is
/// known but whose symbols are not (see [`SkippedFile`]).
#[derive(Default)]
struct PreParsed {
    parsed: Vec<FilePreParsed>,
    skipped: Vec<SkippedFile>,
}

/// One file's Phase 1a verdict. `Nothing` covers the skips we must not act on
/// (unknown language, read error, hash error) — the file keeps whatever the
/// index already knows about it.
enum PreParseOutcome {
    Parsed(Box<FilePreParsed>),
    Skipped(SkippedFile),
    Nothing,
}

/// Phase 1a: the parallel, CPU-bound half of indexing one batch — read, parse,
/// extract nodes. Touches no DB state (Phase 1b does the inserts sequentially),
/// which is what makes it safe to fan out over rayon. Files it cannot handle
/// are counted in `counters`; those whose hash is nonetheless known come back
/// as [`SkippedFile`] so Phase 1b can record them instead of leaving stale
/// nodes and a stale hash behind.
fn pre_parse_batch(
    batch: &[String],
    root: &Path,
    hashes: &HashMap<String, String>,
    counters: &SkipCounters,
) -> PreParsed {
    let outcomes: Vec<PreParseOutcome> = batch
        .par_iter()
        .map(|rel_path| {
            let mut language = match detect_language(rel_path) {
                Some(l) => l,
                None => {
                    counters.language.fetch_add(1, AtomicOrdering::Relaxed);
                    return PreParseOutcome::Nothing;
                }
            };
            let abs_path = root.join(rel_path);

            let file_meta = std::fs::metadata(&abs_path).ok();
            let last_modified = file_meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // Resolve the hash up front so an oversize file can still be
            // RECORDED (we know exactly which bytes we are declining to parse).
            // Falls back to hashing only when the caller did not supply one.
            // A caller with hashes already in hand (the incremental paths, the
            // query-time refresh) supplies them; a full index does not, and for
            // those the hash comes from the bytes read below rather than from a
            // second full read of the same file (audit 2026-08-22 P2-16).
            let provided_hash = hashes.get(rel_path.as_str()).cloned();
            if let Some(ref meta) = file_meta {
                if meta.len() > max_file_size() {
                    tracing::debug!("Skipping large file ({} bytes): {}", meta.len(), rel_path);
                    counters.size.fetch_add(1, AtomicOrdering::Relaxed);
                    // This branch never reads the file, so an unsupplied hash
                    // still costs one read — unchanged from before, and it
                    // applies only to files we refuse to parse.
                    let known_hash = provided_hash.or_else(|| hash_file(&abs_path).ok());
                    return match known_hash {
                        Some(hash) => PreParseOutcome::Skipped(SkippedFile {
                            rel_path: rel_path.clone(),
                            hash,
                            last_modified,
                            language: language.to_string(),
                        }),
                        None => PreParseOutcome::Nothing,
                    };
                }
            }

            let source = match std::fs::read_to_string(&abs_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Skipping file {}: {}", rel_path, e);
                    counters.read.fetch_add(1, AtomicOrdering::Relaxed);
                    return PreParseOutcome::Nothing;
                }
            };

            // `.h` is C-vs-C++ ambiguous by extension, so detect_language maps it
            // to C. But the C grammar can't parse `class`/`namespace`, so C++ classes
            // declared in a `.h` header (the MOST common C++ layout) — and their
            // base-class `inherits` edges — were silently dropped. When the header's
            // content actually contains C++ constructs, parse it as C++ so those
            // symbols are captured. Gated on markers so a pure-C header stays C;
            // false positives are low-harm (the C++ grammar is a near-superset of C).
            if language == "c" && rel_path.ends_with(".h") && looks_like_cpp_header(&source) {
                language = "cpp";
            }

            // `read_to_string` succeeded, so these bytes ARE the file's bytes —
            // `hash_file` streams the same content through the same hasher.
            let hash = provided_hash
                .unwrap_or_else(|| blake3::hash(source.as_bytes()).to_hex().to_string());

            let tree = match parse_tree(&source, language) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Parse failed for {}: {}", rel_path, e);
                    counters.parse.fetch_add(1, AtomicOrdering::Relaxed);
                    // Readable and hashed, just not parseable by this grammar:
                    // record the identity so its stale symbols go away and the
                    // file stops re-diffing on every run.
                    return PreParseOutcome::Skipped(SkippedFile {
                        rel_path: rel_path.clone(),
                        hash,
                        last_modified,
                        language: language.to_string(),
                    });
                }
            };

            // Tree-sitter recovers from syntax errors by inserting ERROR/MISSING
            // nodes and still returning a tree, so parse "succeeds" but symbol
            // extraction below runs over a damaged parse and can silently drop
            // symbols. Surface it: warn once per file and count the pass total.
            if tree.root_node().has_error() {
                tracing::warn!(
                    "Syntax errors in {} — symbols may be incomplete (parsed with tree-sitter error recovery)",
                    rel_path
                );
                counters.parse_errors.fetch_add(1, AtomicOrdering::Relaxed);
            }

            let parsed_nodes = extract_nodes_from_tree(&tree, &source, language);

            PreParseOutcome::Parsed(Box::new(FilePreParsed {
                rel_path: rel_path.clone(),
                source,
                language: language.to_string(),
                tree,
                hash,
                last_modified,
                parsed_nodes,
            }))
        })
        .collect();

    let mut out = PreParsed::default();
    for outcome in outcomes {
        match outcome {
            PreParseOutcome::Parsed(p) => out.parsed.push(*p),
            PreParseOutcome::Skipped(s) => out.skipped.push(s),
            PreParseOutcome::Nothing => {}
        }
    }
    out
}

/// What Phase 1b hands to Phase 2 for one batch.
struct BatchInserted {
    parsed: Vec<FileParsed>,
    /// Inbound cross-file edges captured BEFORE the cascade delete that
    /// `delete_nodes_by_file` triggers, for [`restore_inbound_edges`] to re-bind.
    /// Tuple: (source_id, source_file_id, target_file_id, target_name, relation,
    /// metadata). `target_file_id` is the re-indexed file the edge pointed INTO;
    /// the restore re-binds ONLY to the new same-name node in THAT file, not
    /// every same-name node in the batch (which fanned out cross-file /
    /// cross-language).
    #[allow(clippy::type_complexity)]
    saved_inbound_edges: Vec<(i64, i64, i64, String, String, Option<String>)>,
    /// File ids in this batch, so Phase 2c can skip intra-batch edges.
    file_ids: HashSet<i64>,
    nodes_created: usize,
}

/// Phase 1b: the sequential, DB-bound half of indexing one batch. Upserts each
/// file row, replaces its nodes (a `<module>` node plus one per extracted
/// symbol), and returns the per-file handles Phase 2 resolves relations against.
///
/// Sequential on purpose: it shares one connection with the enclosing savepoint,
/// and node ids must be minted in a stable order.
fn insert_batch_nodes(db: &Database, pre_parsed: Vec<FilePreParsed>) -> Result<BatchInserted> {
    let mut parsed: Vec<FileParsed> = Vec::new();
    let mut nodes_created = 0usize;
    // Saved inbound edges from other files → batch files (to restore after cascade delete)
    // Tuple: (source_id, source_file_id, target_file_id, target_name, relation, metadata).
    // target_file_id is the re-indexed file the edge pointed INTO; the restore
    // re-binds ONLY to the new same-name node in THAT file, not every same-name
    // node in the batch (which fanned out cross-file / cross-language).
    #[allow(clippy::type_complexity)]
    let mut saved_inbound_edges: Vec<(i64, i64, i64, String, String, Option<String>)> = Vec::new();
    // Track file_ids in this batch to filter intra-batch edges in Phase 2c
    let mut batch_file_ids: HashSet<i64> = HashSet::new();

    for pp in pre_parsed {
        let file_id = upsert_file(
            db.conn(),
            &FileRecord {
                path: pp.rel_path.clone(),
                blake3_hash: pp.hash,
                last_modified: pp.last_modified,
                language: Some(pp.language.clone()),
            },
        )?;

        // Save cross-file inbound edges before cascade delete destroys them.
        // file_id IS the target file these edges point into — attach it so the
        // Phase 2c restore can re-bind to the same-name node in THIS file only.
        saved_inbound_edges.extend(
            get_inbound_cross_file_edges(db.conn(), file_id)?
                .into_iter()
                .map(|(src, src_file, tname, rel, meta)| {
                    (src, src_file, file_id, tname, rel, meta)
                }),
        );
        batch_file_ids.insert(file_id);

        delete_nodes_by_file(db.conn(), file_id)?;

        let mut node_ids = Vec::new();
        let mut node_names = Vec::new();
        let mut node_qualified_names: Vec<Option<String>> = Vec::new();
        let mut node_types: Vec<String> = Vec::new();

        let module_node_id = insert_node_cached(
            db.conn(),
            &NodeRecord {
                file_id,
                node_type: "module".into(),
                name: "<module>".into(),
                qualified_name: Some(pp.rel_path.clone()),
                start_line: 1,
                end_line: pp.source.lines().count() as i64,
                code_content: String::new(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )?;
        node_ids.push(module_node_id);
        node_names.push("<module>".into());
        // <module> resolves by its bare name; no qualified form.
        node_qualified_names.push(None);
        node_types.push("module".into());
        nodes_created += 1;

        for pn in &pp.parsed_nodes {
            let name_tokens = split_identifier(&pn.name);
            let node_id = insert_node_cached(
                db.conn(),
                &NodeRecord {
                    file_id,
                    node_type: pn.node_type.clone(),
                    name: pn.name.clone(),
                    qualified_name: pn.qualified_name.clone(),
                    start_line: pn.start_line as i64,
                    end_line: pn.end_line as i64,
                    code_content: pn.code_content.clone(),
                    signature: pn.signature.clone(),
                    doc_comment: pn.doc_comment.clone(),
                    context_string: None,
                    name_tokens: Some(name_tokens),
                    return_type: pn.return_type.clone(),
                    param_types: pn.param_types.clone(),
                    is_test: pn.is_test,
                },
            )?;
            node_ids.push(node_id);
            node_names.push(pn.name.clone());
            node_qualified_names.push(pn.qualified_name.clone());
            node_types.push(pn.node_type.clone());
            nodes_created += 1;
        }

        parsed.push(FileParsed {
            rel_path: pp.rel_path,
            source: pp.source,
            language: pp.language,
            tree: pp.tree,
            file_id,
            node_ids,
            node_names,
            node_qualified_names,
            node_types,
        });
    }

    Ok(BatchInserted {
        parsed,
        saved_inbound_edges,
        file_ids: batch_file_ids,
        nodes_created,
    })
}

/// The `<module>` node of one file, as a target list.
///
/// Three import forms bind to a whole file rather than a symbol — JS namespace
/// and star re-export, PHP `require`, C `#include` — and each had its own copy
/// of this lookup. Returns empty when the file is not indexed, which every
/// caller reads as "unresolved specifier, fall through".
fn module_node_of(
    name_to_ids: &HashMap<String, Vec<i64>>,
    node_id_to_path: &HashMap<i64, String>,
    file: &str,
) -> Vec<i64> {
    name_to_ids
        .get("<module>")
        .map(|ids| {
            ids.iter()
                .copied()
                .filter(|id| node_id_to_path.get(id).map(|p| p == file).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default()
}

/// Insert the `sources` × `targets` cross-product for one relation.
///
/// Every Phase-2 resolution branch ends this way, and each used to carry its
/// own copy of the double loop — twelve of them, which is exactly the shape
/// where a later fix lands in one copy and not the other eleven.
///
/// `allow_self` exists for `routes_to` alone: a route's handler IS its target,
/// and that self-edge is what carries the method/path metadata that trace and
/// impact read. Every other branch drops self-edges. It is a parameter rather
/// than a `relation == REL_ROUTES_TO` test inside because only the fallthrough
/// branch ever sees a `routes_to` relation — deciding it here would silently
/// widen the other eleven if that ever stopped being true.
///
/// Returns the number of rows actually created; `insert_edge_cached` dedups,
/// so a repeat of an existing edge counts zero.
fn insert_relation_edges(
    db: &Database,
    sources: &[i64],
    targets: &[i64],
    relation: &str,
    metadata: Option<&str>,
    allow_self: bool,
) -> Result<usize> {
    let mut created = 0usize;
    for &src_id in sources {
        for &tgt_id in targets {
            if (src_id != tgt_id || allow_self)
                && insert_edge_cached(db.conn(), src_id, tgt_id, relation, metadata)?
            {
                created += 1;
            }
        }
    }
    Ok(created)
}

/// Phase 0: cascade-delete `delete_paths` in a transaction of its own, after
/// buffering the inbound edges that cascade is about to strip.
///
/// Without the buffering, deleting file A wipes B's edge to A.foo while B is
/// not in `delete_paths` (so Phase 2 never re-extracts it), leaving B with
/// neither an edge nor a pending row — the same staleness window the
/// "callee added later" buffering closes, just from the deletion side. Both
/// directions need to round-trip through pending or the v0.18.2 fix is only
/// half-complete.
///
/// Two channels, because there are two kinds of edge:
/// - `calls` → the persistent `pending_unresolved_calls` table (survives across
///   invocations, since the callee's file may come back in a much later run).
/// - everything else → `deferred`, re-resolved after this run's batch loop
///   against the complete name map. That half was missing entirely: the calls
///   buffer is hardcoded to `relation = 'calls'`, so imports/implements/
///   inherits/references/exports/routes_to just disappeared and incremental
///   diverged from a full rebuild of the same tree forever (indexing audit
///   2026-08-02 P1-5 — the sibling face of the edit-path requeue in
///   `restore_inbound_edges`).
///
/// Two source files are deliberately NOT requeued, both to keep the deferred
/// pass's `source_ids` insertable:
/// - a source in `run_file_paths`: its own batch re-extracts every relation
///   with fresh node ids, so requeuing would duplicate that work AND capture a
///   node id that its own cascade-delete is about to invalidate;
/// - a source in `delete_paths`: its nodes are going away in this very
///   function, so the captured id would dangle and abort the deferred insert on
///   the edges FK (the failure mode `aaa238f` fixed on the edit path).
fn buffer_then_delete_files(
    db: &Database,
    delete_paths: &[String],
    run_file_paths: &HashSet<&str>,
    deferred: &mut Vec<DeferredRelation>,
) -> Result<()> {
    let tx = db.savepoint("idx_delete")?;
    let delete_set: HashSet<&str> = delete_paths.iter().map(|s| s.as_str()).collect();

    // Resolve file IDs once (delete_files_by_paths drops them) so we can
    // query inbound calls before cascade fires.
    let mut deleted_file_ids: Vec<i64> = Vec::with_capacity(delete_paths.len());
    for path in delete_paths {
        if let Ok(Some(fid)) =
            db.conn()
                .query_row("SELECT id FROM files WHERE path = ?1", [path], |row| {
                    row.get::<_, Option<i64>>(0)
                })
        {
            deleted_file_ids.push(fid);
        }
    }

    let mut buffered = 0usize;
    let mut requeued = 0usize;
    for fid in &deleted_file_ids {
        let (b, r) =
            buffer_inbound_before_node_purge(db, *fid, run_file_paths, &delete_set, deferred)?;
        buffered += b;
        requeued += r;
    }
    if buffered > 0 || requeued > 0 {
        tracing::info!(
            "[index] Phase 0: buffered {} inbound calls, requeued {} other inbound relations before cascade-deleting {} file(s)",
            buffered,
            requeued,
            deleted_file_ids.len()
        );
    }

    delete_files_by_paths(db.conn(), delete_paths)?;
    tx.commit()?;
    Ok(())
}

/// Buffer every inbound cross-file edge into `file_id` before its nodes are
/// purged, so the cascade cannot destroy an edge whose SOURCE file is not being
/// re-extracted this run.
///
/// Two channels because there are two kinds of edge: `calls` go to the
/// persistent `pending_unresolved_calls` table (they must survive across
/// invocations — the callee's file may only come back in a much later run),
/// everything else goes to this run's `deferred` list and is re-resolved against
/// the complete name map after the batch loop.
///
/// Sources in `run_file_paths` or `delete_set` are skipped: their node ids are
/// about to be invalidated (their own batch re-extracts them, or they are being
/// deleted outright), and a deferred insert against a dangling `source_id`
/// aborts the whole run on the edges FK.
///
/// Returns `(calls_buffered, other_relations_requeued)`.
fn buffer_inbound_before_node_purge(
    db: &Database,
    file_id: i64,
    run_file_paths: &HashSet<&str>,
    delete_set: &HashSet<&str>,
    deferred: &mut Vec<DeferredRelation>,
) -> Result<(usize, usize)> {
    let mut buffered = 0usize;
    let mut requeued = 0usize;

    let inbound = crate::storage::queries::get_inbound_calls_for_pending(db.conn(), file_id)?;
    for (source_id, target_name, source_language, metadata) in inbound {
        crate::storage::queries::insert_pending_unresolved_call(
            db.conn(),
            source_id,
            &target_name,
            &source_language,
            metadata.as_deref(),
        )?;
        buffered += 1;
    }

    let inbound_rest =
        crate::storage::queries::get_inbound_relations_for_requeue(db.conn(), file_id)?;
    for (source_id, source_path, source_language, target_name, relation, metadata) in inbound_rest {
        if run_file_paths.contains(source_path.as_str())
            || delete_set.contains(source_path.as_str())
        {
            continue;
        }
        deferred.push(DeferredRelation {
            source_ids: vec![source_id],
            source_name: String::new(),
            target_name,
            relation,
            metadata,
            rel_path: source_path,
            language: source_language,
            ns_file: None,
        });
        requeued += 1;
    }

    Ok((buffered, requeued))
}

/// Lightweight post-batch record — no Tree or source string.
pub(super) struct FileIndexed {
    pub rel_path: String,
    pub node_ids: Vec<i64>,
    pub node_names: Vec<String>,
}

/// A relation whose target (or, for `routes_to`, whose handler source) failed
/// batch-time resolution.
///
/// On a fresh multi-batch index the per-batch pool cannot contain any LATER
/// batch's nodes, so batch-time "unresolved" proves nothing (audit 2026-08-02
/// P0-1: implements/imports minted `<external>` phantoms and inherits/exports/
/// routes_to/references dropped outright whenever source and target landed in
/// different batches — deterministically, and rebuild never healed it). Only
/// REL_CALLS had a recovery channel (`pending_unresolved_calls`). Everything
/// else is buffered here and re-run once after the batch loop, when
/// `global_name_map` finally holds the whole tree
/// (`resolve_deferred_relations`).
struct DeferredRelation {
    /// Source node ids resolved at batch time (the source side is same-file,
    /// so it is complete then). Empty only for the `routes_to` imported-handler
    /// case, whose source recovery itself needs the full pool.
    source_ids: Vec<i64>,
    source_name: String,
    target_name: String,
    relation: String,
    metadata: Option<String>,
    rel_path: String,
    language: String,
    /// For a JS `m.foo()` whose receiver `m` is a require/import-namespace
    /// binding: the RESOLVED module file the call must bind into. The per-file
    /// `ns_module_map` is gone by the time the deferred pass runs, so the
    /// resolved constraint is captured here instead.
    ns_file: Option<String>,
}

impl DeferredRelation {
    fn of(
        source_ids: &[i64],
        rel: &crate::parser::relations::ParsedRelation,
        rel_path: &str,
        language: &str,
    ) -> Self {
        DeferredRelation {
            source_ids: source_ids.to_vec(),
            source_name: rel.source_name.clone(),
            target_name: rel.target_name.clone(),
            relation: rel.relation.clone(),
            metadata: rel.metadata.clone(),
            rel_path: rel_path.to_string(),
            language: language.to_string(),
            ns_file: None,
        }
    }
}

pub(super) fn index_files(
    db: &Database,
    root: &Path,
    files: &[String],
    hashes: &HashMap<String, String>,
    model: Option<&EmbeddingModel>,
    delete_paths: &[String],
    progress: Option<ProgressFn>,
) -> Result<IndexResult> {
    // Phase transactions use `db.savepoint(...)`, NOT `conn().unchecked_transaction()`,
    // so this pipeline is atomic whether run standalone (CLI / incremental — a
    // top-level SAVEPOINT auto-starts a transaction, RELEASE commits it) OR nested
    // inside an enclosing transaction. The MCP `rebuild_index` tool wraps the whole
    // DELETE-then-reindex in one outer transaction so external fresh-connection
    // readers never observe the empty/partial mid-rebuild window and a failed rebuild
    // rolls back to the old index; `unchecked_transaction` can't be used there because
    // it always issues BEGIN, which errors inside an already-open transaction.
    //
    // Safety of the shared `&Connection` (savepoint borrows &Connection, we still read
    // via db.conn() on the same handle): (1) db.conn() and the savepoint act on the
    // same Connection; (2) concurrent access (e.g. background embedding thread) uses
    // separate DB connections — safety relies on SQLite WAL mode + busy_timeout(5000),
    // not single-threadedness.

    let counters = SkipCounters::default();

    // This project's own Rust package names, in module spelling. Read once per
    // run (a handful of `Cargo.toml` reads) and handed to every Path-qualifier
    // filter so `my_crate::module::f()` strips its crate root the way
    // `crate::module::f()` already does. See `resolve::path_filter_candidates`.
    let crate_roots = super::resolve::collect_crate_root_names(root);

    // Every caller derives `files` from HashMap iteration — `run_full_index`
    // from `scan_directory`'s hash map keys, both incremental entries from
    // `compute_diff`, which walks the current/old hash maps. So the order
    // varies run to run, and with it the batch each file lands in. Several
    // bindings below are first-wins *within* a batch (the `<external>`
    // sentinel's node type, same-batch callee resolution, the pending-call
    // drain), so two indexes of an unchanged tree could disagree. Sorting
    // here — the one choke point all four entry points funnel through — makes
    // a given tree index the same way every time. Dedup because a duplicated
    // path would otherwise insert, then cascade-delete, its own file's nodes.
    let files: Vec<String> = {
        let mut v = files.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };

    let mut total_nodes_created = 0usize;
    let mut total_edges_created = 0usize;
    let mut all_indexed: Vec<FileIndexed> = Vec::new();

    // Relations that failed batch-time resolution; re-run after the batch loop
    // against the complete pool (see `DeferredRelation`).
    //
    // This is the one structure that grows for the whole run rather than per
    // batch, so it looks like the place to put a size cap. It is not, and the
    // measurement says why. A generated 10 000-file / 90 000-edge TypeScript
    // corpus deferred 32 314 relations — about a third of all edges — and the
    // whole indexing process peaked at 172 MB RSS; re-running with every import
    // pointing BACKWARD instead of forward changed the deferral count by 7%
    // (30 073), so the ratio is a property of batch-time resolution, not of how
    // the corpus happens to be ordered. Growth is therefore proportional to
    // edges, not unbounded in the runaway sense, and the persistent
    // `pending_unresolved_calls` side's `attempts` limit is not the precedent it
    // looks like: that evicts rows that can NEVER resolve, whereas every entry
    // here resolves a few lines below. Capping this list would silently drop
    // real edges at exactly the scale where a full rebuild is least likely to be
    // re-run — the failure this pipeline has already been bitten by twice.
    let mut deferred: Vec<DeferredRelation> = Vec::new();

    // Same sort+dedup rationale as `files` above: `delete_paths` arrives from
    // `compute_diff`'s HashMap walk, and the pending-buffer rows Phase 0 writes
    // reach a first-wins unique index — unsorted input made their insertion
    // order (and thus which duplicate wins) vary run to run.
    let delete_paths: Vec<String> = {
        let mut v = delete_paths.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };
    let delete_paths = delete_paths.as_slice();

    // Every file THIS RUN will (re)index — `restore_inbound_edges` and Phase 0
    // both consult it: a requeue whose source file is in here must NOT happen,
    // because that file's own batch re-extracts its relations with fresh node
    // ids. The captured source_id would otherwise dangle once a LATER batch
    // cascade-deletes the source file's old nodes, and the deferred pass's
    // insert then aborts the whole run on the FK (pre-tag review, Critical-1).
    // Computed BEFORE Phase 0 because the delete path needs the same guard.
    let run_file_paths: HashSet<&str> = files.iter().map(|s| s.as_str()).collect();

    // Run-completion marker (audit 2026-08-16 P1-2). Every cross-file relation
    // this run cannot resolve at batch time lives in the in-memory `deferred`
    // vector until the single savepoint after the batch loop. The per-batch
    // savepoints, meanwhile, commit each file's NEW HASH as they go — so a run
    // killed between the first batch commit and the deferred commit leaves the
    // index claiming those files are indexed while their cross-file edges were
    // never written, and `compute_diff` never offers them again. Nothing in the
    // pipeline could observe that afterwards.
    //
    // Written BEFORE Phase 0 (which commits its deletions in its own savepoint)
    // so no committed change of this run precedes the marker. Cleared right
    // after the deferred commit, which is the point where the edges are durable;
    // everything past it — context strings, the pending sweep, the global post
    // passes — is recomputed by any later run that indexes anything, so an
    // interruption there is not worth a full re-index.
    //
    // A no-op run writes nothing: watcher flushes reach here with an empty diff
    // constantly, and two meta writes per tick would be pure churn.
    //
    // Known amplification, accepted deliberately: any `?` between the marker
    // write and the post-deferred clear — a crash, but also an ordinary error
    // like SQLITE_BUSY from a concurrent CLI writer during a one-file
    // query-time refresh — leaves the marker durable, and the NEXT run
    // escalates to a full re-index (plus a re-embed under `embed-model`). That
    // is the conservative trade: an error mid-run means we cannot prove which
    // cross-file edges were durably committed, and a wrongly-cheap answer here
    // is the silent edge-loss class this marker exists to close. The escalated
    // run clears the marker, so the cost is one full pass, not a loop.
    let has_work = !files.is_empty() || !delete_paths.is_empty();
    if has_work {
        crate::storage::queries::set_meta(
            db.conn(),
            crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT,
            "1",
        )?;
    }

    // Phase 0: Delete removed files in own transaction.
    if !delete_paths.is_empty() {
        buffer_then_delete_files(db, delete_paths, &run_file_paths, &mut deferred)?;
    }

    // Pre-build Python module map once (used in all batches for import resolution)
    let mut all_python_paths: HashSet<String> = files
        .iter()
        .filter(|f| f.ends_with(".py"))
        .cloned()
        .collect();
    {
        let mut stmt = db
            .conn()
            .prepare("SELECT path FROM files WHERE path LIKE '%.py'")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            all_python_paths.insert(row?);
        }
    }
    let python_module_map = build_python_module_map(&all_python_paths);

    // All indexed file paths (this run's `files` plus everything already in the
    // DB), used to resolve JS/TS relative import specifiers to a concrete file.
    // Includes pseudo-files like `<external>`; the resolver only matches real
    // relative paths so they never collide.
    let mut all_file_paths: HashSet<String> = files.iter().cloned().collect();
    {
        let mut stmt = db.conn().prepare("SELECT path FROM files")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            all_file_paths.insert(row?);
        }
    }

    // Pre-load global name->[(id, path, language)] map once before the batch loop.
    // This avoids a full table scan per batch in Phase 2 relation resolution.
    // The map is updated incrementally as each batch commits new nodes.
    // `language` drives same-language-preferred resolution to avoid cross-language
    // bare-name collisions (e.g. Rust `hasher.update()` resolving to JS `function update`).
    let mut global_name_map: HashMap<String, Vec<crate::storage::queries::NameEntry>> =
        get_all_node_names_with_ids(db.conn())?;

    // Process files in batches — each batch does Phase 1 + Phase 2
    for batch in files.chunks(BATCH_SIZE) {
        let tx = db.savepoint("idx_batch")?;

        // --- Phase 1a: Parallel CPU-bound work (read + parse + extract nodes) ---
        let pre_parsed = pre_parse_batch(batch, root, hashes, &counters);

        // The paths this batch purges WITHOUT reinserting anything. They have to
        // join `batch_file_paths` below, which is the set that both excludes a
        // file's old ids from the per-batch resolution pool and prunes them out
        // of `global_name_map` after the commit. Built from `batch_parsed` alone
        // it covered only the files that PARSED, so a skipped file's ids stayed
        // in the map pointing at rows that no longer exist — and the deferred
        // pass resolved a requeued relation onto one, aborting the whole run on
        // the edges FK (787) after the batch savepoint had already committed the
        // file's new hash, which put the file permanently out of reach of
        // `compute_diff` (audit 2026-08-16 P0-1).
        let skipped_paths: Vec<String> = pre_parsed
            .skipped
            .iter()
            .map(|sk| sk.rel_path.clone())
            .collect();

        // Files we know the identity of but not the symbols (oversize /
        // unparsable): record the hash so they stop re-diffing forever, and
        // purge whatever symbols the index still claims for them — those are
        // provably stale, since the content hash just changed (audit IDX-1).
        // Their inbound edges go through the same buffer the delete path uses,
        // so an unchanged caller's edge is re-resolved instead of cascaded away.
        for sk in &pre_parsed.skipped {
            let file_id = upsert_file(
                db.conn(),
                &FileRecord {
                    path: sk.rel_path.clone(),
                    blake3_hash: sk.hash.clone(),
                    last_modified: sk.last_modified,
                    language: Some(sk.language.clone()),
                },
            )?;
            let (b, r) = buffer_inbound_before_node_purge(
                db,
                file_id,
                &run_file_paths,
                &HashSet::new(),
                &mut deferred,
            )?;
            if b > 0 || r > 0 {
                tracing::debug!(
                    "[index] unparsable/oversize {}: buffered {} calls, requeued {} relations before purge",
                    sk.rel_path, b, r
                );
            }
            delete_nodes_by_file(db.conn(), file_id)?;
        }

        // --- Phase 1b: Sequential DB inserts ---
        let inserted = insert_batch_nodes(db, pre_parsed.parsed)?;
        let batch_parsed = inserted.parsed;
        let saved_inbound_edges = inserted.saved_inbound_edges;
        let batch_file_ids = inserted.file_ids;
        total_nodes_created += inserted.nodes_created;

        // --- Phase 2: Extract relations + insert edges ---
        // Build per-batch name_to_ids and node_id_to_path from the pre-loaded global map,
        // excluding files in the current batch (their old nodes were deleted in Phase 1b).
        let mut batch_file_paths: HashSet<&str> =
            batch_parsed.iter().map(|pf| pf.rel_path.as_str()).collect();
        // Purged-but-not-reinserted files (see `skipped_paths`): their old ids
        // must be excluded from this batch's pool and pruned from the global map
        // exactly like a reindexed file's, or they resolve onto deleted rows.
        batch_file_paths.extend(skipped_paths.iter().map(|p| p.as_str()));

        // These three pools are rebuilt from `global_name_map` on EVERY batch —
        // O(nodes x batches) where the map itself is maintained incrementally.
        // Carried as an open performance item across two audits (2026-08-16,
        // 2026-08-22 P2-5) on the theory that it is a hotspot above ~500 files,
        // which this repository's own 278-file single-batch tree cannot show.
        //
        // Measured, 2026-08-22, on 1,763 files of third-party Python (25,041
        // nodes, four batches at BATCH_SIZE 500), timing this block alone:
        //
        //   batch 1  2.4ms   batch 2  5.4ms   batch 3  6.8ms   batch 4  8.7ms
        //   total   23.2ms   of an 8,899ms full index  =  0.26%
        //
        // Linear per batch, exactly as the O() says, at ~0.35us per node. The
        // shape is real; the constant is not worth incremental maintenance of
        // three more structures in a pipeline whose bookkeeping misses have
        // twice cost real edges. Deliberately left alone — reopen it with a
        // measurement, not with the complexity argument.
        let mut name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
        let mut node_id_to_path: HashMap<i64, String> = HashMap::new();
        // Per-node language for same-language-preferred edge resolution (§ cross-lang collision).
        let mut node_id_to_language: HashMap<i64, Option<String>> = HashMap::new();

        // Add current batch's newly inserted nodes
        for pf in &batch_parsed {
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                name_to_ids.entry(name.clone()).or_default().push(*id);
                node_id_to_path.insert(*id, pf.rel_path.clone());
                node_id_to_language.insert(*id, Some(pf.language.clone()));
            }
        }

        // Add nodes from the global map, excluding those in current batch's files
        // (their old nodes were deleted and replaced by new ones above)
        for (name, entries) in &global_name_map {
            for (id, path, language) in entries {
                if !batch_file_paths.contains(path.as_str()) {
                    name_to_ids.entry(name.clone()).or_default().push(*id);
                    node_id_to_path.insert(*id, path.clone());
                    node_id_to_language.insert(*id, language.clone());
                }
            }
        }

        for ids in name_to_ids.values_mut() {
            ids.sort();
            ids.dedup();
        }

        // Track unresolved external Python imports: (source_module_node_id, module_name)
        let mut external_python_imports: Vec<(i64, String)> = Vec::new();
        // Track unresolved external symbols for sentinel node creation:
        // (source_id, target_name, relation) — e.g., implements edges to external traits
        let mut unresolved_externals: Vec<(i64, String, String)> = Vec::new();

        for pf in &batch_parsed {
            let relations = extract_relations_from_tree(&pf.tree, &pf.source, &pf.language);
            let local_ids: HashSet<i64> = pf.node_ids.iter().copied().collect();

            // Pre-scan this file's require-namespace bindings
            // (`const m = require('./x')`, stamped `{"q":"ns_require",...}`) →
            // resolved file path, so `m.foo()` member calls (CalleeMeta::Receiver)
            // bind to the required module in the call-resolution pass below.
            let mut ns_module_map: HashMap<String, String> = HashMap::new();
            for rel in &relations {
                if rel.relation != REL_IMPORTS {
                    continue;
                }
                if let Some(meta_str) = rel.metadata.as_deref() {
                    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
                        // ESM `import * as ns` (q:"ns_import", v51) binds member
                        // calls exactly like the CJS require-namespace form.
                        if matches!(
                            meta.get("q").and_then(|v| v.as_str()),
                            Some(crate::domain::IMPORT_Q_NS_REQUIRE)
                                | Some(crate::domain::IMPORT_Q_NS_IMPORT)
                        ) {
                            if let Some(spec) = meta.get("js_module").and_then(|v| v.as_str()) {
                                if let Some(file) =
                                    resolve_js_specifier_path(spec, &pf.rel_path, &all_file_paths)
                                {
                                    ns_module_map.insert(rel.target_name.clone(), file);
                                }
                            }
                        }
                    }
                }
            }

            for rel in &relations {
                // Contract: extract_relations_from_tree stamps every relation with
                // source_language equal to the language argument. The
                // same-language resolution at line 811+ depends on it. Hard
                // error instead of debug_assert so a parser regression fails
                // loudly in release builds too (one string compare per
                // relation is negligible against the SQL writes below).
                if rel.source_language != pf.language {
                    anyhow::bail!(
                        "ParsedRelation.source_language ({}) does not match file language ({}); \
                         parser regressed the source_language contract",
                        rel.source_language,
                        pf.language
                    );
                }

                // Match the relation's enclosing scope (source_name) to a node.
                // Class-based languages (Python/TS/JS/Java/Ruby) qualify a
                // method's scope as `Class.method`, but the node's bare `name`
                // is just `method` — so match qualified_name too, else every
                // intra-class method-to-method edge is silently dropped.
                // Bare-scope sources (Rust impl, Go receivers, free functions)
                // still match on `name`.
                // inherits/implements describe a TYPE's supertype, so their source
                // must be a class/struct/interface/enum/trait — never a function or
                // method that merely shares the type's name. A C++ inline constructor
                // (`Widget(int){}`) produces a `method Widget` node alongside `class
                // Widget`; without this both matched `source_name == "Widget"` and the
                // constructor got a bogus `inherits` edge. Blacklist fn/method (rather
                // than whitelist type kinds) so no language's type node is missed.
                let type_source_only =
                    rel.relation == REL_INHERITS || rel.relation == REL_IMPLEMENTS;
                let mut source_ids = (0..pf.node_ids.len())
                    .filter(|&i| {
                        (pf.node_names[i] == rel.source_name
                            || pf.node_qualified_names[i].as_deref()
                                == Some(rel.source_name.as_str()))
                            && (!type_source_only
                                || !matches!(pf.node_types[i].as_str(), "function" | "method"))
                    })
                    .map(|i| pf.node_ids[i])
                    .collect::<Vec<_>>();

                // Route handlers are commonly imported from a controller file —
                // the canonical Express layout `import { getUser } from './ctrl';
                // app.get('/x', getUser)`. The routes_to relation names the handler
                // (== source == target), but the handler node lives in another
                // file, so the same-file scan above finds nothing and the route
                // edge (the handler self-edge carrying method/path) is silently
                // dropped — trace/impact/find_http_route then see no route at all.
                // Recover by resolving the handler name cross-file, same-language,
                // exactly like a call target below (refine breaks any ambiguity by
                // path locality). Only fires for routes_to with an unresolved
                // same-file source; inline + same-file named handlers already match.
                if rel.relation == REL_ROUTES_TO && source_ids.is_empty() {
                    let same_lang: Vec<i64> = name_to_ids
                        .get(&rel.source_name)
                        .map(|ids| {
                            ids.iter()
                                .copied()
                                .filter(|id| {
                                    matches!(
                                        node_id_to_language.get(id).and_then(|l| l.as_deref()),
                                        Some(l) if l == pf.language.as_str()
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    source_ids =
                        refine_ambiguous_targets(&same_lang, &pf.rel_path, &node_id_to_path);
                    if source_ids.is_empty() {
                        // Imported handler may sit in a later batch — the recovery
                        // above can only see the pool visible NOW. Defer with empty
                        // source_ids; the post-loop pass re-runs this recovery
                        // against the whole tree.
                        deferred.push(DeferredRelation::of(&[], rel, &pf.rel_path, &pf.language));
                        continue;
                    }
                }

                // The five metadata-driven import forms below all key off the same
                // JSON blob, and each used to parse it again — five `from_str`
                // calls per import relation, four of them thrown away. Parse once.
                // Order still decides: namespace/star, then Python module, then
                // JS specifier, then PHP include, then C include, then the
                // default name-based chain.
                let import_meta: Option<serde_json::Value> = if rel.relation == REL_IMPORTS {
                    rel.metadata
                        .as_deref()
                        .and_then(|m| serde_json::from_str(m).ok())
                } else {
                    None
                };

                // Module-level import markers (v51, roadmap §2.3): namespace
                // bindings (`const m = require('./x')` q:"ns_require", `import *
                // as ns from './x'` q:"ns_import") and star re-exports (`export *
                // from './x'` q:"star_reexport") name no resolvable symbol, so
                // default name resolution would mint a spurious `<external>` node
                // (or, for star's `<module>` target, cross-link a random file).
                // Instead bind them to the RESOLVED file's `<module>` node — the
                // PHP-include/C-include pattern — so a namespace-only or
                // star-barrel dependency is finally visible to deps/affected/
                // cycles/map. Unresolvable specifier (external package) → no
                // edge, same as before. Always `continue`: never fall through.
                if let Some(meta) = import_meta.as_ref() {
                    if matches!(
                        meta.get("q").and_then(|v| v.as_str()),
                        Some(crate::domain::IMPORT_Q_NS_REQUIRE)
                            | Some(crate::domain::IMPORT_Q_NS_IMPORT)
                            | Some(crate::domain::IMPORT_Q_STAR_REEXPORT)
                            | Some(crate::domain::IMPORT_Q_DEFAULT)
                    ) {
                        let mut resolved = false;
                        if let Some(spec) = meta.get("js_module").and_then(|v| v.as_str()) {
                            if let Some(file) =
                                resolve_js_specifier_path(spec, &pf.rel_path, &all_file_paths)
                            {
                                let module_targets =
                                    module_node_of(&name_to_ids, &node_id_to_path, &file);
                                if !module_targets.is_empty() {
                                    resolved = true;
                                    total_edges_created += insert_relation_edges(
                                        db,
                                        &source_ids,
                                        &module_targets,
                                        &rel.relation,
                                        rel.metadata.as_deref(),
                                        false,
                                    )?;
                                }
                            }
                        }
                        if !resolved {
                            // The specifier's file (or its <module> node) may sit
                            // in a later batch — retry after the loop. A genuinely
                            // external specifier fails there too and drops, same
                            // as before.
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                        }
                        continue;
                    }
                }

                // Try Python module-constrained resolution for import edges
                if let Some(meta) = import_meta.as_ref() {
                    if let Some(python_module) = meta.get("python_module").and_then(|v| v.as_str())
                    {
                        let is_module_import = meta
                            .get("is_module_import")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Some(module_files) =
                            project_module_files(python_module, &python_module_map)
                        {
                            // Internal module — try constrained resolution
                            if let Some(module_targets) = resolve_python_module_targets(
                                &module_files,
                                is_module_import,
                                &rel.target_name,
                                &node_id_to_path,
                                &name_to_ids,
                            ) {
                                total_edges_created += insert_relation_edges(
                                    db,
                                    &source_ids,
                                    &module_targets,
                                    &rel.relation,
                                    rel.metadata.as_deref(),
                                    false,
                                )?;
                                continue;
                            }
                            // Module found but symbol not visible IN THIS BATCH —
                            // the module file may sit batches ahead. Defer; the
                            // post-loop pass retries constrained resolution and
                            // only then falls back exactly as this chain would.
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        } else {
                            // External module — track for virtual node creation.
                            // For `from X import Y`, we track the module-level dependency (X),
                            // not the individual symbol (Y), since we can't index external code.
                            for &src_id in &source_ids {
                                external_python_imports.push((src_id, python_module.to_string()));
                            }
                            continue; // No point in default resolution for external imports
                        }
                    }
                }

                // Try JS/TS relative-specifier resolution for import edges. The
                // parser stamps `{"js_module":"<specifier>"}` (imports.rs);
                // resolve the specifier against the importer's path + extension
                // probing to a concrete file so the import binds there instead
                // of a path-proximity same-name guess. Combined with Phase
                // 2d-bind, this also repoints the matching bare calls. Bare/
                // external/unindexed specifiers return None → fall through to
                // default name-based / `<external>` resolution (unchanged).
                if let Some(meta) = import_meta.as_ref() {
                    if let Some(js_module) = meta.get("js_module").and_then(|v| v.as_str()) {
                        if let Some(targets) = resolve_js_module_targets(
                            js_module,
                            &pf.rel_path,
                            &rel.target_name,
                            &all_file_paths,
                            &name_to_ids,
                            &node_id_to_path,
                        ) {
                            total_edges_created += insert_relation_edges(
                                db,
                                &source_ids,
                                &targets,
                                &rel.relation,
                                rel.metadata.as_deref(),
                                false,
                            )?;
                            continue;
                        }
                        // The specifier resolves to an INDEXED file whose nodes
                        // are just not visible to this batch — falling through
                        // to bare-name resolution here bound the WRONG same-name
                        // symbol from another file cross-batch. Defer; the
                        // post-loop pass retries the constrained resolution and
                        // only then falls back exactly as this chain would.
                        if resolve_js_specifier_path(js_module, &pf.rel_path, &all_file_paths)
                            .is_some()
                        {
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        }
                        // Genuinely unresolved (bare pkg / unindexed) —
                        // fall through to default resolution below.
                    }
                }

                // PHP file includes: the parser stamps `{"php_include":"<path>"}`
                // on the import edge (require/require_once/include 'lib.php').
                // Resolve the path against the importer's directory + `.php`
                // probing to a concrete file, then bind to that file's <module>
                // node so deps/cycles/affected/project_map see the cross-file
                // include dependency. Unindexed/vendored paths return None →
                // fall through to default (`<external>`) resolution.
                if let Some(meta) = import_meta.as_ref() {
                    if let Some(inc) = meta.get("php_include").and_then(|v| v.as_str()) {
                        if let Some(file) =
                            resolve_php_include_path(inc, &pf.rel_path, &all_file_paths)
                        {
                            // Bind to the resolved file's <module> node.
                            let module_targets =
                                module_node_of(&name_to_ids, &node_id_to_path, &file);
                            if !module_targets.is_empty() {
                                total_edges_created += insert_relation_edges(
                                    db,
                                    &source_ids,
                                    &module_targets,
                                    &rel.relation,
                                    rel.metadata.as_deref(),
                                    false,
                                )?;
                                continue;
                            }
                            // File resolved but its <module> node sits in a later
                            // batch — defer rather than fall to bare-name guessing.
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        }
                        // Unindexed include → fall through to default.
                    }
                }

                // C/C++ file includes: the parser stamps `{"c_include":"<path>"}`
                // on the import edge (`#include "widget.h"`). Resolve the path
                // against the importer's directory (and repo root) to a concrete
                // header, then bind to that file's <module> node so deps/cycles/
                // affected/project_map see the local header dependency. System
                // headers (`<stdio.h>`) / unindexed paths return None → fall
                // through to default (`<external>`) resolution (M6).
                if let Some(meta) = import_meta.as_ref() {
                    if let Some(inc) = meta.get("c_include").and_then(|v| v.as_str()) {
                        if let Some(file) =
                            resolve_c_include_path(inc, &pf.rel_path, &all_file_paths)
                        {
                            let module_targets =
                                module_node_of(&name_to_ids, &node_id_to_path, &file);
                            if !module_targets.is_empty() {
                                total_edges_created += insert_relation_edges(
                                    db,
                                    &source_ids,
                                    &module_targets,
                                    &rel.relation,
                                    rel.metadata.as_deref(),
                                    false,
                                )?;
                                continue;
                            }
                            // Header resolved but its <module> node sits in a later
                            // batch — defer rather than fall to bare-name guessing.
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        }
                        // Unindexed include → fall through to default.
                    }
                }

                // Rust trait impl method-level edges: parser stamps
                // `{"q":"impl_method","v":"<TypeName>"}` so we can restrict
                // candidate target methods to those that actually belong to
                // this impl block (qualified_name LIKE "<TypeName>.%"). Without
                // this, N structs implementing the same trait in one file all
                // fan their method edges onto every same-name method node.
                if rel.relation == REL_IMPLEMENTS {
                    if let Some(ref meta_str) = rel.metadata {
                        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
                            if meta.get("q").and_then(|v| v.as_str()) == Some("impl_method")
                                && meta.get("v").and_then(|v| v.as_str()).is_some()
                            {
                                // The impl type's methods can span batches, so
                                // both a non-empty filtered set and an empty one
                                // are partial views here. Defer unconditionally;
                                // the post-loop pass runs the identical
                                // self_filter against the complete pool (a
                                // genuinely external trait method still drops).
                                deferred.push(DeferredRelation::of(
                                    &source_ids,
                                    rel,
                                    &pf.rel_path,
                                    &pf.language,
                                ));
                                continue;
                            }
                        }
                    }
                }

                // Bare-name call qualifier (Rust): inspect metadata to
                // skip / restrict candidate set before the existing fallback
                // chain. See spec
                // docs/superpowers/specs/2026-05-11-bare-name-call-qualifier-design.md.
                if rel.relation == REL_CALLS {
                    use super::resolve::{method_candidates, parse_callee_metadata, CalleeMeta};
                    match parse_callee_metadata(rel.metadata.as_deref()) {
                        Some(CalleeMeta::Receiver(recv))
                            if matches!(
                                pf.language.as_str(),
                                "javascript" | "typescript" | "tsx"
                            ) =>
                        {
                            // Cycle 4: `m.foo()` where `const m = require('./x')` —
                            // bind the method to the required module file. Only JS
                            // produces a Receiver here (extract_callee captures a
                            // simple-identifier receiver for the JS family). When recv
                            // is NOT a require-namespace binding (`arr.map()`,
                            // `res.send()`) or the method isn't in that file, fall
                            // through to the default resolution below — identical to
                            // the pre-Cycle-4 Bare path — by NOT continuing.
                            if let Some(module_file) = ns_module_map.get(&recv) {
                                let targets: Vec<i64> = name_to_ids
                                    .get(&rel.target_name)
                                    .map(|ids| {
                                        ids.iter()
                                            .copied()
                                            .filter(|id| {
                                                node_id_to_path
                                                    .get(id)
                                                    .map(|p| p == module_file)
                                                    .unwrap_or(false)
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                if !targets.is_empty() {
                                    total_edges_created += insert_relation_edges(
                                        db,
                                        &source_ids,
                                        &targets,
                                        &rel.relation,
                                        rel.metadata.as_deref(),
                                        false,
                                    )?;
                                    continue;
                                }
                                // The namespace binding names a concrete file, but
                                // that file's nodes may sit in a later batch —
                                // falling through to bare-name resolution here
                                // bound the WRONG same-name symbol cross-batch.
                                // Defer with the resolved file as a constraint.
                                let mut d = DeferredRelation::of(
                                    &source_ids,
                                    rel,
                                    &pf.rel_path,
                                    &pf.language,
                                );
                                d.ns_file = Some(module_file.clone());
                                deferred.push(d);
                                continue;
                            }
                            // Not a namespace binding → fall through to default.
                        }
                        Some(CalleeMeta::Chain) | Some(CalleeMeta::Receiver(_)) => {
                            // Receiver type is not statically inferable (`obj.method()`
                            // where `obj`'s type is unknown). The blanket drop here
                            // marked uniquely-named live methods (`file_exists`,
                            // `validate`) as dead and hid their callers from
                            // impact/callers. Recover ONLY the unambiguous case: a
                            // single same-language METHOD with that name, not a
                            // stdlib-noise name. A unique non-noise method cannot
                            // fan out across unrelated modules — the exact inflation
                            // the drop guarded against — so binding it is safe.
                            // Anything ambiguous (0 or >1 method candidates) or a
                            // noise name stays dropped (no buffer; re-scan won't help).
                            if is_cross_file_call_noise(&rel.target_name, pf.language.as_str()) {
                                continue;
                            }
                            let all = name_to_ids
                                .get(&rel.target_name)
                                .cloned()
                                .unwrap_or_default();
                            let same_lang: Vec<i64> = all
                                .iter()
                                .filter(|id| {
                                    matches!(
                                        node_id_to_language.get(id).and_then(|l| l.as_deref()),
                                        Some(l) if l == pf.language.as_str()
                                    )
                                })
                                .copied()
                                .collect();
                            // A receiver call can only target a method, never a
                            // same-named free function — filter those out first.
                            let methods = method_candidates(&same_lang, db)?;
                            // Prefer a same-file method if present (strongest
                            // locality signal); otherwise require a globally
                            // unique method.
                            let same_file_methods: Vec<i64> = methods
                                .iter()
                                .copied()
                                .filter(|id| local_ids.contains(id))
                                .collect();
                            if same_file_methods.len() == 1 {
                                // Strongest locality signal, and the same-file pool
                                // is COMPLETE at batch time — safe to decide here.
                                total_edges_created += insert_relation_edges(
                                    db,
                                    &source_ids,
                                    &[same_file_methods[0]],
                                    &rel.relation,
                                    rel.metadata.as_deref(),
                                    false,
                                )?;
                            } else if same_file_methods.len() > 1 {
                                // >1 same-file methods (two structs in one file each
                                // defining this name) — intentionally ambiguous, the
                                // receiver's type still can't pick between them.
                                // Same-file pool is complete, so drop is final.
                            } else {
                                // No same-file method. The "globally unique method"
                                // decision depends on the WHOLE pool, and this batch
                                // cannot see later batches (a unique-in-view method
                                // may have cross-batch twins, and a zero-in-view
                                // name may exist one batch ahead — the old comment
                                // "re-scan won't change it" was false there). Defer;
                                // the post-loop pass applies the identical rule
                                // against the complete pool.
                                deferred.push(DeferredRelation::of(
                                    &source_ids,
                                    rel,
                                    &pf.rel_path,
                                    &pf.language,
                                ));
                            }
                            continue;
                        }
                        Some(CalleeMeta::SelfRecv(_)) | Some(CalleeMeta::SelfType(_)) => {
                            // The impl type's methods can span batches, so even a
                            // non-empty filtered set here is a PARTIAL view (and an
                            // empty one proves nothing — the old "a re-scan will
                            // yield the same answer" held only within one batch).
                            // Defer unconditionally; the post-loop pass applies the
                            // identical self_filter against the complete pool.
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        }
                        Some(CalleeMeta::RecvType(_)) => {
                            // Same partial-view argument as SelfRecv/SelfType
                            // above; additionally this arm's EMPTY case falls
                            // through to bare default resolution rather than
                            // dropping, and that decision too needs the whole
                            // pool. Defer unconditionally; the post-loop pass
                            // replicates bind-precisely-or-fall-through.
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        }
                        Some(CalleeMeta::Path(_)) => {
                            // Path-qualified call (`Alpha::foo()`). The path filter
                            // scans candidate FILE PATHS, which span batches, so
                            // both its match set and its empty verdict are partial
                            // views. Defer unconditionally; the post-loop pass
                            // runs the identical filter + refine on the whole pool
                            // (an external crate path still resolves to nothing
                            // there and drops, as before).
                            deferred.push(DeferredRelation::of(
                                &source_ids,
                                rel,
                                &pf.rel_path,
                                &pf.language,
                            ));
                            continue;
                        }
                        _ => {} // None (Bare) or unrecognized q → falls through to default chain below.
                    }
                }

                // Statically-external import (Rust `use std::…`): skip the whole
                // name-based chain and bind to the `<external>` sentinel. The
                // trailing segment of a std path is a bare name like `swap` or
                // `fs`, and letting it into the global pool is how it bound to a
                // same-named project symbol. See `domain::IMPORT_EXTERNAL_META`
                // for why an explicit sentinel edge beats emitting nothing.
                if rel.relation == REL_IMPORTS
                    && crate::domain::is_external_import_meta(rel.metadata.as_deref())
                {
                    for &src_id in &source_ids {
                        unresolved_externals.push((
                            src_id,
                            rel.target_name.clone(),
                            rel.relation.clone(),
                        ));
                    }
                    continue;
                }

                // Default resolution: global name-based lookup with language-aware layering.
                // Tier order: same-file → same-language → (calls: drop) / (other: global).
                // Dropping calls without a same-language match prevents Rust `hasher.update()`
                // binding to an unrelated JS `function update()` via bare-name collision.
                let all_target_ids = name_to_ids
                    .get(&rel.target_name)
                    .cloned()
                    .unwrap_or_default();

                let same_file_targets: Vec<i64> = all_target_ids
                    .iter()
                    .filter(|id| local_ids.contains(id))
                    .copied()
                    .collect();

                let source_lang = pf.language.as_str();

                // Same-file binds are decided HERE (a file's nodes are atomic
                // within its batch, so that pool is complete); the cross-file
                // noise drop is pool-independent. EVERY other cross-file
                // name-based decision is deferred to the post-loop pass: a
                // batch's view of same-language candidates is a PREFIX of the
                // tree, and refining/uniqueness/fan-out decisions on a partial
                // pool bound the wrong same-name symbol whenever the right one
                // sat batches ahead (audit 2026-08-02 P0-1; measured on this
                // repo at BATCH_SIZE 25: `test_db` bound three src/graph/*
                // twins instead of the path-closest helpers.rs one).
                let target_ids = if !same_file_targets.is_empty() {
                    same_file_targets
                } else if rel.relation == REL_CALLS
                    && is_cross_file_call_noise(&rel.target_name, source_lang)
                {
                    // Stdlib method names (new/default/from) — drop. Language-aware:
                    // the JS/TS family exempts non-ECMAScript names (insert/remove/
                    // contains) so user methods resolve; all else drops regardless
                    // of language (a Rust `hasher.update()` must not bind a JS fn).
                    continue;
                } else if rel.relation == REL_CALLS {
                    // No same-file, no same-language candidate VISIBLE TO THIS
                    // BATCH. Defer — the target may sit in a later batch, and
                    // resolving it there uses the same rules as here (the
                    // pending sweep's binding rules are deliberately narrower,
                    // so buffering directly at this point made a multi-batch
                    // tree resolve differently from a single-batch one). The
                    // deferred pass buffers into pending_unresolved_calls only
                    // what is STILL unresolved after seeing the whole tree —
                    // preserving the cross-invocation channel that memory
                    // `feedback_incremental_edge_timing.md` documents (B's call
                    // to a `foo` that A only adds in a later indexing run).
                    deferred.push(DeferredRelation::of(
                        &source_ids,
                        rel,
                        &pf.rel_path,
                        &pf.language,
                    ));
                    continue;
                } else if rel.relation == REL_REFERENCES {
                    // Bare-name value references (callbacks / fn pointers) share the
                    // cross-language collision risk of bare-name calls: short common
                    // names like `process` / `handler` / `run` exist in many
                    // languages. Without a same-file or same-language target, do NOT
                    // fall through to the global pool — a Rust `references → process`
                    // must never bind a JS `function process()`
                    // (feedback_edge_resolution_same_language). Precision over
                    // recall. Defer rather than drop: the same-language target may
                    // sit in a later batch (the old "full rebuild resolves" comment
                    // here was false above one batch — audit 2026-08-02 P0-1).
                    deferred.push(DeferredRelation::of(
                        &source_ids,
                        rel,
                        &pf.rel_path,
                        &pf.language,
                    ));
                    continue;
                } else {
                    // Structural relations (imports / inherits / implements /
                    // exports / routes_to) with no same-file target: the
                    // same-language / language-FAMILY pool this arm used to
                    // bind against is a PARTIAL view (this batch + earlier
                    // ones), so decide in the deferred pass instead, where the
                    // identical family rules (see `resolve_deferred_relations`
                    // branch 7 — cross-LANGUAGE phantom protection unchanged)
                    // run against the complete pool. Still-unresolved
                    // implements/imports mint their `<external>` sentinel
                    // there; the rest drop.
                    deferred.push(DeferredRelation::of(
                        &source_ids,
                        rel,
                        &pf.rel_path,
                        &pf.language,
                    ));
                    continue;
                };

                {
                    total_edges_created += insert_relation_edges(
                        db,
                        &source_ids,
                        &target_ids,
                        &rel.relation,
                        rel.metadata.as_deref(),
                        rel.relation == REL_ROUTES_TO,
                    )?;
                }
            }
        }

        // Phases 2b / 2b-ext: mint the `<external>` sentinel nodes for this
        // batch's unresolved imports and trait targets, plus their edges.
        let (ext_nodes, ext_edges) =
            mint_external_sentinels(db, &external_python_imports, &unresolved_externals)?;
        total_nodes_created += ext_nodes;
        total_edges_created += ext_edges;

        // Phase 2c: restore cross-file inbound edges that cascade-delete stripped.
        total_edges_created += restore_inbound_edges(
            db,
            &batch_parsed,
            &batch_file_ids,
            &saved_inbound_edges,
            &run_file_paths,
            &mut deferred,
        )?;

        tx.commit()?;

        let batch_file_count = batch_parsed.len();

        // Update global_name_map: remove old entries for batch files, add new ones
        for (_, entries) in global_name_map.iter_mut() {
            entries.retain(|(_id, path, _lang)| !batch_file_paths.contains(path.as_str()));
        }
        global_name_map.retain(|_, entries| !entries.is_empty());

        // Convert to lightweight records — drops Tree and source string
        for pf in batch_parsed {
            // Add newly committed nodes to the global map
            let pf_lang = Some(pf.language.clone());
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                global_name_map.entry(name.clone()).or_default().push((
                    *id,
                    pf.rel_path.clone(),
                    pf_lang.clone(),
                ));
            }
            all_indexed.push(FileIndexed {
                rel_path: pf.rel_path,
                node_ids: pf.node_ids,
                node_names: pf.node_names,
            });
            // pf.tree and pf.source are dropped here — memory freed
        }

        // Report progress after each batch
        if let Some(cb) = progress {
            cb(IndexPhase::Files, all_indexed.len(), files.len());
        }

        if files.len() > BATCH_SIZE {
            tracing::info!(
                "[index] batch {}/{}: {} files ({} nodes, {} edges)",
                all_indexed.len(),
                files.len(),
                batch_file_count,
                total_nodes_created,
                total_edges_created
            );
        }
    }

    // Phase 2b-final: re-run resolution for relations deferred at batch time
    // (cross-batch targets — audit 2026-08-02 P0-1). Must run BEFORE Phase 3 so
    // context strings see the recovered edges.
    let mut deferred_edges = 0usize;
    if !deferred.is_empty() {
        let deferred_count = deferred.len();
        let tx = db.savepoint("idx_deferred")?;
        let (d_edges, d_nodes) = resolve_deferred_relations(
            db,
            &deferred,
            &global_name_map,
            &all_file_paths,
            &python_module_map,
            &crate_roots,
        )?;
        tx.commit()?;
        deferred_edges = d_edges;
        total_edges_created += d_edges;
        total_nodes_created += d_nodes;
        tracing::debug!(
            "[index] Phase 2b-final: {} deferred relation(s) → {} edge(s), {} sentinel node(s)",
            deferred_count,
            d_edges,
            d_nodes
        );
    }

    // Cross-file edges are durable from here on — clear the marker set above.
    if has_work {
        crate::storage::queries::delete_meta(
            db.conn(),
            crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT,
        )?;
    }

    // Finalizing heartbeat: every phase below is a full-graph pass with no
    // per-file progress, so the last `Files` tick would sit frozen for the whole
    // tail (minutes on 10k-file repos). Ticking between phases keeps the progress
    // consumer's mtime fresh — a stale-file gate can then distinguish "long tail
    // phase" from "indexer was killed". No-op when this run changed nothing, so a
    // no-diff incremental never (re)creates a progress file it never wrote to.
    let finalize_tick = || {
        if all_indexed.is_empty() && delete_paths.is_empty() {
            return;
        }
        if let Some(cb) = progress {
            cb(IndexPhase::Finalizing, all_indexed.len(), files.len());
        }
    };

    // Phase 3: Build context strings + embeddings (single transaction, lightweight)
    if !all_indexed.is_empty() {
        finalize_tick();
        build_context_strings_and_embed(db, &all_indexed, model, &finalize_tick)?;
    }

    // Phase 2c: sweep pending_unresolved_calls — promote any rows whose
    // target_name now resolves against a same-language node. Cheap when the
    // table is empty (typical after a full index of a self-contained codebase).
    //
    // Gated on this batch having PARSED something, because the sweep does two
    // things and the second one is not free. Resolution: a buffered row can only
    // start resolving once its target node exists, and nodes appear only from
    // parsed files — so on a batch that parsed nothing the sweep provably finds
    // nothing new. Retention: the same call ages every survivor by one attempt.
    // Ungated, that spent the row's 50-attempt budget on ambient watcher and
    // periodic-rescan ticks (measured on this repo: attempts = 4 after 26h /
    // 4 scans, ~2 weeks to the ceiling with the code untouched), and an evicted
    // row only comes back if the CALLER file is re-indexed — so a forward
    // reference could go permanently unresolved for no reason but elapsed time.
    // Attempts now count resolution OPPORTUNITIES.
    //
    // Deletions do not qualify: removing nodes can only shrink the candidate set.
    let pending_resolved = if all_indexed.is_empty() {
        0
    } else {
        resolve_pending_calls(db, &crate_roots)?
    };
    total_edges_created += pending_resolved;
    if pending_resolved > 0 {
        tracing::info!(
            "[index] Phase 2c: resolved {} pending unresolved calls",
            pending_resolved
        );
    }

    // Phases 2d-bind, 2d-prune, and 2e are full-graph set-based passes (a JOIN over
    // all edges, a DELETE with correlated subqueries, and a GROUP-BY over all nodes).
    // Their result is a guaranteed no-op when this invocation indexed AND deleted
    // nothing: the edge set is unchanged, so the import-bind finds nothing new to
    // bind, the import-contradiction prune finds nothing to drop, and the confidence
    // reclassification recomputes identical counts. Gate the whole block on a real
    // change so no-diff incremental ticks (e.g. a file-watcher flush whose diff is
    // empty) don't pay for three full-graph scans on the hot path. When anything DID
    // change it must run GLOBALLY, not just over the changed files — adding/removing
    // a duplicate-named node in ONE file flips bind/prune eligibility and the
    // ambiguity of cross-file edges in OTHER, unchanged files.
    //
    // `pending_resolved > 0` is part of the condition because Phase 2c INSERTS
    // edges, and every edge it inserts is a cross-file by-name bind — precisely
    // the shape Phase 2e downgrades off the `extracted` column default. Phase 2c
    // is now itself gated on `!all_indexed.is_empty()`, so the term cannot fire
    // alone — it is kept because it states the actual invariant (Phase 2c wrote
    // edges ⇒ reclassify) rather than a chain of reasoning about which batches
    // can produce them. Give the sweep a per-tick cap for large backlogs and the
    // spilled remainder would drain on a batch whose own files are unchanged;
    // this term is what keeps those edges from holding a confidence they never
    // earned. Gating on the observable keeps the invariant local.
    //
    // `deferred_edges > 0` states the same invariant for Phase 2b-final, which
    // also inserts cross-file by-name binds. It is not implied by the other three:
    // a run whose ONLY changed file was skipped for size or a parse failure
    // indexes nothing (`all_indexed` empty), deletes nothing, and never reaches
    // the pending sweep — yet its purge requeues that file's inbound relations,
    // and the deferred pass re-binds them. Without this term those edges kept the
    // `extracted` column default, the top confidence tier, having never been
    // classified (audit 2026-08-16, the P2 riding with P0-1).
    //
    // Bound once, into a name, because the `<external>` reaper below needs the
    // SAME predicate and used to carry a narrower copy (`all_indexed` /
    // `delete_paths` only). That asymmetry was a live hole: the prune inside
    // these post-passes is one of the two things that orphans a sentinel, and on
    // a deferred-only or pending-only run it ran while the reaper did not — the
    // orphan then stayed in the name-resolution pool, which is the exact
    // incremental-vs-rebuild divergence audit 2026-08-02 P1-9 introduced the
    // reaper to end (2026-08-16 audit §四, sibling of the P0-1 rider above).
    let graph_changed = !all_indexed.is_empty()
        || !delete_paths.is_empty()
        || pending_resolved > 0
        || deferred_edges > 0;
    if graph_changed {
        finalize_tick();
        // The post-passes below are the first big correlated-subquery joins over
        // the graph this run just wrote, and on a fresh index there is no
        // `sqlite_stat1` for the planner to use — `run_optimize()` only runs at
        // the very END of the run, after they are done. Measured on a real
        // 2,052-file TypeScript repo: prune took 5.14 s of a 13.5 s full index
        // without statistics and 0.187 s with them. Paying ~30 ms here to make
        // them available is the whole difference (13.16 s -> 8.52 s end to end, as
        // shipped; 8.58 s was the ungated variant this gate replaced).
        //
        // Gated by size because `ensure_file_indexed` reaches this same block on
        // every query that touches an edited file: an unconditional ANALYZE cost
        // that path ~10 ms of its ~70 ms (measured on this repo), for no gain —
        // a one-file refresh inherits perfectly good statistics from whatever
        // run last crossed the threshold, and its own edge delta is far too
        // small to shift them.
        if all_indexed.len() + delete_paths.len() >= STATS_REFRESH_MIN_FILES {
            db.refresh_query_stats();
        }
        let post = run_global_edge_post_passes(db)?;
        total_edges_created += post.bound;
        total_edges_created = total_edges_created.saturating_sub(post.pruned);
    }

    // Reap `<external>` sentinel nodes that no edge points at any more. Pruning
    // and deferred re-resolution can orphan them, and nothing else ever deleted
    // them — a lingering orphan stays in the name-resolution pool and makes an
    // incrementally-grown node set diverge from a fresh rebuild forever (audit
    // 2026-08-02 P1-9). Shares `graph_changed` with the post-passes above: the
    // prune that runs there is one of the two orphan sources, so anything that
    // lets the prune run must also let the reaper run.
    if graph_changed {
        finalize_tick();
        let reaped = crate::storage::queries::reap_orphan_external_nodes(db.conn())?;
        if reaped > 0 {
            tracing::debug!(
                "[index] reaped {} orphan <external> sentinel node(s)",
                reaped
            );
        }
    }

    // Optimize query planner statistics after bulk writes
    if !all_indexed.is_empty() {
        finalize_tick();
        let _ = db.run_optimize();
    }

    let stats = IndexStats {
        files_skipped_size: counters.size.load(AtomicOrdering::Relaxed),
        files_skipped_parse: counters.parse.load(AtomicOrdering::Relaxed),
        files_skipped_read: counters.read.load(AtomicOrdering::Relaxed),
        files_skipped_hash: counters.hash.load(AtomicOrdering::Relaxed),
        files_skipped_language: counters.language.load(AtomicOrdering::Relaxed),
        files_with_parse_errors: counters.parse_errors.load(AtomicOrdering::Relaxed),
    };

    Ok(IndexResult {
        files_indexed: all_indexed.len(),
        files_deleted: delete_paths.len(),
        nodes_created: total_nodes_created,
        edges_created: total_edges_created,
        stats,
    })
}

/// Phases 2b / 2b-ext: mint the `<external>` pseudo-file's sentinel nodes.
///
/// Two channels feed it. Python `import flask` with no project file behind it
/// arrives as `external_python_imports` (module → `external_module`); every
/// other unresolved import or `implements` target — Rust `use std::io::Write`,
/// JS `require('fs')`, `impl Write for X` — arrives as `unresolved_externals`.
/// Both mint into the same namespace, so a name may already exist from the
/// other channel or from an earlier batch; whoever gets there first owns the
/// node and later arrivals only add edges to it.
///
/// Returns `(nodes_created, edges_created)`.
fn mint_external_sentinels(
    db: &Database,
    external_python_imports: &[(i64, String)],
    unresolved_externals: &[(i64, String, String)],
) -> Result<(usize, usize)> {
    let mut nodes_created = 0usize;
    let mut edges_created = 0usize;

    // Phase 2b: Create virtual nodes for external Python imports
    if !external_python_imports.is_empty() {
        let ext_file_id = upsert_file(
            db.conn(),
            &FileRecord {
                path: crate::domain::EXTERNAL_FILE_PATH.into(),
                blake3_hash: "external".into(),
                last_modified: 0,
                language: Some("external".into()),
            },
        )?;

        // Load existing external module nodes to avoid duplicates
        let existing_ext_nodes: HashMap<String, i64> =
            get_nodes_by_file_path(db.conn(), "<external>")?
                .into_iter()
                .map(|n| (n.name.clone(), n.id))
                .collect();

        // Sorted, not raw set order: these nodes are minted here, so set
        // iteration order would hand the same module a different node id
        // on every run.
        let unique_modules: Vec<String> = {
            let mut v: Vec<String> = external_python_imports
                .iter()
                .map(|(_, m)| m.clone())
                .collect::<HashSet<String>>()
                .into_iter()
                .collect();
            v.sort_unstable();
            v
        };

        let mut ext_node_ids: HashMap<String, i64> = existing_ext_nodes;
        for module_name in &unique_modules {
            if !ext_node_ids.contains_key(module_name) {
                let node_id = insert_node_cached(
                    db.conn(),
                    &NodeRecord {
                        file_id: ext_file_id,
                        node_type: "external_module".into(),
                        name: module_name.clone(),
                        qualified_name: Some(format!("<external>/{}", module_name)),
                        start_line: 0,
                        end_line: 0,
                        code_content: String::new(),
                        signature: None,
                        doc_comment: None,
                        context_string: None,
                        name_tokens: None,
                        return_type: None,
                        param_types: None,
                        is_test: false,
                    },
                )?;
                ext_node_ids.insert(module_name.clone(), node_id);
                nodes_created += 1;
            }
        }

        for (source_id, module_name) in external_python_imports {
            if let Some(&ext_id) = ext_node_ids.get(module_name) {
                if insert_edge_cached(db.conn(), *source_id, ext_id, REL_IMPORTS, None)? {
                    edges_created += 1;
                }
            }
        }
    }

    // Phase 2b-ext: Create sentinel nodes for unresolved external symbols
    // (e.g., Rust `impl Write for SharedStdout` where Write is from std::io)
    if !unresolved_externals.is_empty() {
        let ext_file_id = upsert_file(
            db.conn(),
            &FileRecord {
                path: crate::domain::EXTERNAL_FILE_PATH.into(),
                blake3_hash: "external".into(),
                last_modified: 0,
                language: Some("external".into()),
            },
        )?;

        let existing_ext: Vec<crate::storage::queries::NodeResult> =
            get_nodes_by_file_path(db.conn(), "<external>")?;
        let existing_types: HashMap<String, String> = existing_ext
            .iter()
            .map(|n| (n.name.clone(), n.node_type.clone()))
            .collect();
        let mut ext_node_ids: HashMap<String, i64> =
            existing_ext.into_iter().map(|n| (n.name, n.id)).collect();

        // Collect unique targets with inferred type.
        //
        // One name can reach both channels in the same batch — `impl Write
        // for X` alongside an unresolved `use std::io::Write`. Collecting
        // straight into a map let the LAST push decide, so the sentinel was
        // minted `trait` or `module` depending on relation order. Give the
        // channels a fixed precedence instead: `implements` is the specific
        // claim (this name IS a trait), an import only says "some external
        // module-ish name", so implements wins regardless of arrival order.
        // Sorted before insert so the ids are stable too.
        let unique_targets: Vec<(&str, &str)> = {
            let mut by_name: HashMap<&str, &str> = HashMap::new();
            for (_, name, rel) in unresolved_externals {
                let node_type = if rel == REL_IMPLEMENTS {
                    "trait"
                } else {
                    "module"
                };
                let slot = by_name.entry(name.as_str()).or_insert(node_type);
                if node_type == "trait" {
                    *slot = "trait";
                }
            }
            let mut v: Vec<(&str, &str)> = by_name.into_iter().collect();
            v.sort_unstable();
            v
        };

        for &(name, node_type) in &unique_targets {
            if let Some(&existing_id) = ext_node_ids.get(name) {
                // The precedence above only decided ties WITHIN this call. The
                // node may pre-exist from an earlier mint (another batch, the
                // deferred pass, or a previous incremental run) that only had
                // the weaker import claim — apply the same precedence across
                // calls: an implements claim upgrades `module` → `trait`.
                // Never the reverse (audit 2026-08-02, claims P2-8: a node
                // first minted `module` was never upgraded, so full and
                // incremental indexes disagreed on its type).
                if node_type == "trait"
                    && existing_types.get(name).map(String::as_str) == Some("module")
                {
                    db.conn().execute(
                        "UPDATE nodes SET type = 'trait' WHERE id = ?1 AND type = 'module'",
                        [existing_id],
                    )?;
                }
                continue;
            }
            {
                let node_id = insert_node_cached(
                    db.conn(),
                    &NodeRecord {
                        file_id: ext_file_id,
                        node_type: node_type.into(),
                        name: name.into(),
                        qualified_name: Some(format!("<external>/{}", name)),
                        start_line: 0,
                        end_line: 0,
                        code_content: String::new(),
                        signature: None,
                        doc_comment: None,
                        context_string: None,
                        name_tokens: None,
                        return_type: None,
                        param_types: None,
                        is_test: false,
                    },
                )?;
                ext_node_ids.insert(name.into(), node_id);
                nodes_created += 1;
            }
        }

        for (source_id, target_name, relation) in unresolved_externals {
            if let Some(&ext_id) = ext_node_ids.get(target_name.as_str()) {
                if insert_edge_cached(db.conn(), *source_id, ext_id, relation, None)? {
                    edges_created += 1;
                }
            }
        }
    }

    Ok((nodes_created, edges_created))
}

/// Phase 2c: restore the cross-file inbound edges that cascade-delete stripped.
///
/// Re-indexing a file deletes its old nodes, and the cascade takes every edge
/// pointing INTO them with it — including edges whose source file is not in
/// this batch and so never gets re-extracted. Those were saved before the
/// delete; here they are re-bound to the new node ids.
///
/// Returns the number of edges actually inserted.
#[allow(clippy::type_complexity)]
fn restore_inbound_edges(
    db: &Database,
    batch_parsed: &[FileParsed],
    batch_file_ids: &HashSet<i64>,
    saved_inbound_edges: &[(i64, i64, i64, String, String, Option<String>)],
    run_file_paths: &HashSet<&str>,
    deferred: &mut Vec<DeferredRelation>,
) -> Result<usize> {
    let mut edges_created = 0usize;
    // Phase 2c: Restore cross-file inbound edges lost to cascade delete.
    // When a file is re-indexed, its old nodes are deleted (cascade-deleting edges).
    // Edges from OTHER files into the re-indexed file must be rebuilt using new node IDs.
    if !saved_inbound_edges.is_empty() {
        // Build (target_file_id, name) → new_node_id map for batch files. Keying
        // on the file the edge pointed INTO — not just the bare name — pins the
        // restore to the same-name node in THAT file, so a re-indexed sibling
        // file sharing the symbol name (or a cross-language same-name node in the
        // batch) can no longer steal the edge. A genuinely-removed symbol yields
        // no match → the edge drops, exactly as a full rebuild would.
        let mut batch_name_to_ids: HashMap<(i64, &str), Vec<i64>> = HashMap::new();
        for pf in batch_parsed {
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                batch_name_to_ids
                    .entry((pf.file_id, name.as_str()))
                    .or_default()
                    .push(*id);
            }
        }

        // Memoized source-file lookup for the requeue path below.
        let mut src_file_info: HashMap<i64, (String, String)> = HashMap::new();

        let mut restored = 0usize;
        let mut skipped_intra_batch = 0usize;
        let mut requeued = 0usize;
        for (source_id, source_file_id, target_file_id, target_name, relation, metadata) in
            saved_inbound_edges
        {
            // Source file is also in this batch — source_id is stale (deleted + re-created).
            // Phase 2 already resolves cross-file edges for intra-batch files.
            if batch_file_ids.contains(source_file_id) {
                skipped_intra_batch += 1;
                continue;
            }
            if let Some(new_target_ids) =
                batch_name_to_ids.get(&(*target_file_id, target_name.as_str()))
            {
                for &new_tgt_id in new_target_ids {
                    if *source_id != new_tgt_id
                        && insert_edge_cached(
                            db.conn(),
                            *source_id,
                            new_tgt_id,
                            relation,
                            metadata.as_deref(),
                        )?
                    {
                        edges_created += 1;
                        restored += 1;
                    }
                }
            } else {
                // The symbol this edge pointed at no longer exists in the changed
                // file (renamed or removed). A full rebuild would RE-RESOLVE the
                // surviving caller's relation against the whole tree — possibly
                // binding a same-name candidate elsewhere — so dropping here made
                // incremental diverge permanently (audit 2026-08-02 P1-2: the
                // delete-path got this buffering in v0.18.2, the far more common
                // edit path never did). Requeue instead: calls through the
                // persistent pending buffer, everything else through the deferred
                // pass, both of which apply the normal resolution rules.
                let (src_path, src_lang) = src_file_info
                    .entry(*source_file_id)
                    .or_insert_with(|| {
                        db.conn()
                            .query_row(
                                "SELECT path, COALESCE(language, '') FROM files WHERE id = ?1",
                                [*source_file_id],
                                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                            )
                            .unwrap_or_default()
                    })
                    .clone();
                if src_path.is_empty() {
                    continue; // source file row gone — nothing to requeue for
                }
                if run_file_paths.contains(src_path.as_str()) {
                    // The source file is in THIS RUN's changed set (an earlier
                    // OR a later batch): its own batch (re)extracts every
                    // relation with fresh node ids, so there is nothing to
                    // requeue — and requeuing would capture a source_id that a
                    // LATER batch's cascade-delete turns dangling, aborting the
                    // whole run on the edges FK when the deferred pass inserts
                    // (pre-tag review Critical-1: >500 changed files + a
                    // rename reproduced `FOREIGN KEY constraint failed`, with
                    // the index left missing every deferred edge and no
                    // self-heal).
                    skipped_intra_batch += 1;
                    continue;
                }
                if relation.as_str() == REL_CALLS {
                    crate::storage::queries::insert_pending_unresolved_call(
                        db.conn(),
                        *source_id,
                        target_name,
                        &src_lang,
                        metadata.as_deref(),
                    )?;
                } else {
                    deferred.push(DeferredRelation {
                        source_ids: vec![*source_id],
                        source_name: String::new(),
                        target_name: target_name.clone(),
                        relation: relation.clone(),
                        metadata: metadata.clone(),
                        rel_path: src_path,
                        language: src_lang,
                        ns_file: None,
                    });
                }
                requeued += 1;
            }
        }
        if restored > 0 || skipped_intra_batch > 0 || requeued > 0 {
            tracing::debug!(
                "[index] Restored {} cross-file inbound edges, skipped {} intra-batch, requeued {} misses",
                restored,
                skipped_intra_batch,
                requeued
            );
        }
    }

    Ok(edges_created)
}

/// Phase 2b-final: post-loop resolution for `DeferredRelation`s (audit
/// 2026-08-02 P0-1).
///
/// Mirrors the batch-time Phase-2 chain BRANCH FOR BRANCH, in the same order,
/// against the complete `global_name_map`. Kept in lockstep by the multi-batch
/// parity tests in `pipeline/tests.rs` (a multi-batch fixture must produce the
/// same edge set as its single-batch control) — if you change one chain, change
/// the other. Still-unresolved imports/implements mint their `<external>`
/// sentinels HERE (the batch loop no longer mints for empty-resolution cases —
/// a later batch's real node must beat a phantom); everything else drops,
/// exactly as the batch-time chain would.
///
/// Returns `(edges_created, nodes_created)`.
fn resolve_deferred_relations(
    db: &Database,
    deferred: &[DeferredRelation],
    global_name_map: &HashMap<String, Vec<crate::storage::queries::NameEntry>>,
    all_file_paths: &HashSet<String>,
    python_module_map: &HashMap<String, Vec<String>>,
    crate_roots: &HashSet<String>,
) -> Result<(usize, usize)> {
    use super::resolve::{
        method_candidates, parse_callee_metadata, path_filter_candidates, self_filter_candidates,
        CalleeMeta,
    };

    // Containment layer for dead ids (audit 2026-08-16 P0-1). Both id sources
    // this pass inserts from are SNAPSHOTS taken earlier in the run: the name map
    // was loaded before the batch loop and pruned per batch, and every
    // `source_ids` was captured when its relation was buffered. Any bookkeeping
    // miss on either side puts a deleted id in front of `insert_edge_cached`,
    // where the edges FK aborts the savepoint and destroys the WHOLE run's
    // cross-file edges — one bad id costing every good one. Screening both sides
    // against the live node set turns that into a skipped edge plus a warning.
    //
    // This is containment, not recovery: the id is dropped, not re-queued. It is
    // inert whenever the bookkeeping is right, which is what the warning is for —
    // a nonzero count means a purge site is still missing its map removal, and
    // that is a bug to fix at the purge site, not here.
    //
    // The scan is proportional to work already being done: the map walk below is
    // already O(all nodes), and this is one index-only pass over the same rows.
    let live_ids: HashSet<i64> = {
        let mut stmt = db.conn().prepare("SELECT id FROM nodes")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<HashSet<i64>, _>>()?
    };
    let mut dead_map_ids = 0usize;

    let mut name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
    let mut node_id_to_path: HashMap<i64, String> = HashMap::new();
    let mut node_id_to_language: HashMap<i64, Option<String>> = HashMap::new();
    for (name, entries) in global_name_map {
        for (id, path, language) in entries {
            if !live_ids.contains(id) {
                dead_map_ids += 1;
                continue;
            }
            name_to_ids.entry(name.clone()).or_default().push(*id);
            node_id_to_path.insert(*id, path.clone());
            node_id_to_language.insert(*id, language.clone());
        }
    }
    if dead_map_ids > 0 {
        tracing::warn!(
            "[index] Phase 2b-final: {} name-map entr(ies) referenced deleted nodes and were \
             skipped — a node purge did not prune the run's name map",
            dead_map_ids
        );
    }
    for ids in name_to_ids.values_mut() {
        ids.sort();
        ids.dedup();
    }
    let same_lang_of = |ids: &[i64], lang: &str, exclude: &[i64]| -> Vec<i64> {
        ids.iter()
            .filter(|id| !exclude.contains(id))
            .filter(|id| {
                matches!(
                    node_id_to_language.get(id).and_then(|l| l.as_deref()),
                    Some(l) if l == lang
                )
            })
            .copied()
            .collect()
    };

    let mut edges_created = 0usize;
    let mut unresolved_externals: Vec<(i64, String, String)> = Vec::new();

    for d in deferred {
        // routes_to whose imported-handler source never resolved at batch time.
        let source_ids: Vec<i64> = if d.relation == REL_ROUTES_TO && d.source_ids.is_empty() {
            let all = name_to_ids.get(&d.source_name).cloned().unwrap_or_default();
            let same_lang = same_lang_of(&all, &d.language, &[]);
            refine_ambiguous_targets(&same_lang, &d.rel_path, &node_id_to_path)
        } else {
            // Source ids were captured when the relation was buffered, which for
            // a requeue is before its holder's own purge. `restore_inbound_edges`
            // and Phase 0 both guard against buffering an about-to-die id; this
            // screen is what keeps a miss in either guard from aborting the run
            // (see the `live_ids` note above).
            let live: Vec<i64> = d
                .source_ids
                .iter()
                .copied()
                .filter(|id| live_ids.contains(id))
                .collect();
            if live.len() != d.source_ids.len() {
                tracing::warn!(
                    "[index] Phase 2b-final: dropped {} deleted source id(s) for {} {} → {} in {}",
                    d.source_ids.len() - live.len(),
                    d.language,
                    d.relation,
                    d.target_name,
                    d.rel_path
                );
            }
            live
        };
        if source_ids.is_empty() {
            continue;
        }

        let import_meta: Option<serde_json::Value> = if d.relation == REL_IMPORTS {
            d.metadata
                .as_deref()
                .and_then(|m| serde_json::from_str(m).ok())
        } else {
            None
        };

        // 1. Namespace / star / default module-level import markers → module node.
        if let Some(meta) = import_meta.as_ref() {
            if matches!(
                meta.get("q").and_then(|v| v.as_str()),
                Some(crate::domain::IMPORT_Q_NS_REQUIRE)
                    | Some(crate::domain::IMPORT_Q_NS_IMPORT)
                    | Some(crate::domain::IMPORT_Q_STAR_REEXPORT)
                    | Some(crate::domain::IMPORT_Q_DEFAULT)
            ) {
                if let Some(spec) = meta.get("js_module").and_then(|v| v.as_str()) {
                    if let Some(file) = resolve_js_specifier_path(spec, &d.rel_path, all_file_paths)
                    {
                        let module_targets = module_node_of(&name_to_ids, &node_id_to_path, &file);
                        edges_created += insert_relation_edges(
                            db,
                            &source_ids,
                            &module_targets,
                            &d.relation,
                            d.metadata.as_deref(),
                            false,
                        )?;
                    }
                }
                continue; // still-unresolved marker imports drop (external package)
            }
        }

        // 2. Python module-constrained resolution (external modules were
        //    sentinel'd at batch time; only internal-module symbol misses defer).
        if let Some(meta) = import_meta.as_ref() {
            if let Some(python_module) = meta.get("python_module").and_then(|v| v.as_str()) {
                let is_module_import = meta
                    .get("is_module_import")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if let Some(module_files) = project_module_files(python_module, python_module_map) {
                    if let Some(module_targets) = resolve_python_module_targets(
                        &module_files,
                        is_module_import,
                        &d.target_name,
                        &node_id_to_path,
                        &name_to_ids,
                    ) {
                        edges_created += insert_relation_edges(
                            db,
                            &source_ids,
                            &module_targets,
                            &d.relation,
                            d.metadata.as_deref(),
                            false,
                        )?;
                        continue;
                    }
                }
            }
        }

        // 3. JS/TS relative-specifier resolution.
        if let Some(meta) = import_meta.as_ref() {
            if let Some(js_module) = meta.get("js_module").and_then(|v| v.as_str()) {
                if let Some(targets) = resolve_js_module_targets(
                    js_module,
                    &d.rel_path,
                    &d.target_name,
                    all_file_paths,
                    &name_to_ids,
                    &node_id_to_path,
                ) {
                    edges_created += insert_relation_edges(
                        db,
                        &source_ids,
                        &targets,
                        &d.relation,
                        d.metadata.as_deref(),
                        false,
                    )?;
                    continue;
                }
            }
        }

        // 4. PHP / C file includes → resolved file's <module> node.
        if let Some(meta) = import_meta.as_ref() {
            let inc_file = meta
                .get("php_include")
                .and_then(|v| v.as_str())
                .and_then(|inc| resolve_php_include_path(inc, &d.rel_path, all_file_paths))
                .or_else(|| {
                    meta.get("c_include")
                        .and_then(|v| v.as_str())
                        .and_then(|inc| resolve_c_include_path(inc, &d.rel_path, all_file_paths))
                });
            if let Some(file) = inc_file {
                let module_targets = module_node_of(&name_to_ids, &node_id_to_path, &file);
                if !module_targets.is_empty() {
                    edges_created += insert_relation_edges(
                        db,
                        &source_ids,
                        &module_targets,
                        &d.relation,
                        d.metadata.as_deref(),
                        false,
                    )?;
                    continue;
                }
            }
        }

        // 5. Rust trait-impl method edges (q:"impl_method").
        if d.relation == REL_IMPLEMENTS {
            if let Some(ref meta_str) = d.metadata {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
                    if meta.get("q").and_then(|v| v.as_str()) == Some("impl_method") {
                        if let Some(impl_type) = meta.get("v").and_then(|v| v.as_str()) {
                            let all = name_to_ids.get(&d.target_name).cloned().unwrap_or_default();
                            let filtered = self_filter_candidates(impl_type, &all, db)?;
                            if !filtered.is_empty() {
                                edges_created += insert_relation_edges(
                                    db,
                                    &source_ids,
                                    &filtered,
                                    &d.relation,
                                    d.metadata.as_deref(),
                                    false,
                                )?;
                            }
                            continue; // external trait method → drop, as at batch time
                        }
                    }
                }
            }
        }

        // 6. Calls — full qualifier dispatch mirroring the batch-time arms.
        if d.relation == REL_CALLS {
            let all = name_to_ids.get(&d.target_name).cloned().unwrap_or_default();

            // 6a. JS namespace-receiver constraint captured at batch time
            //     (`m.foo()` where `m` is a require/import-namespace binding).
            if let Some(ns_file) = d.ns_file.as_deref() {
                let targets: Vec<i64> = all
                    .iter()
                    .copied()
                    .filter(|id| {
                        node_id_to_path
                            .get(id)
                            .map(|p| p == ns_file)
                            .unwrap_or(false)
                    })
                    .collect();
                if !targets.is_empty() {
                    edges_created += insert_relation_edges(
                        db,
                        &source_ids,
                        &targets,
                        &d.relation,
                        d.metadata.as_deref(),
                        false,
                    )?;
                    continue;
                }
                // Method not in the bound file even with the full pool —
                // fall through to the default chain, as the batch arm would.
            }

            let mut handled = true;
            match parse_callee_metadata(d.metadata.as_deref()) {
                Some(CalleeMeta::Receiver(_))
                    if matches!(d.language.as_str(), "javascript" | "typescript" | "tsx") =>
                {
                    // ns miss (or no ns binding) → default chain below.
                    handled = false;
                }
                Some(CalleeMeta::Chain) | Some(CalleeMeta::Receiver(_)) => {
                    // Unique-method rule, now against the complete pool.
                    if !is_cross_file_call_noise(&d.target_name, &d.language) {
                        let same_lang = same_lang_of(&all, &d.language, &[]);
                        let methods = method_candidates(&same_lang, db)?;
                        let same_file_methods: Vec<i64> = methods
                            .iter()
                            .copied()
                            .filter(|id| {
                                node_id_to_path.get(id).map(|p| p.as_str())
                                    == Some(d.rel_path.as_str())
                            })
                            .collect();
                        let target = if same_file_methods.len() == 1 {
                            Some(same_file_methods[0])
                        } else if same_file_methods.is_empty() && methods.len() == 1 {
                            Some(methods[0])
                        } else {
                            None // ambiguous either way → drop, as at batch time
                        };
                        if let Some(tgt_id) = target {
                            edges_created += insert_relation_edges(
                                db,
                                &source_ids,
                                &[tgt_id],
                                &d.relation,
                                d.metadata.as_deref(),
                                false,
                            )?;
                        }
                    }
                }
                Some(CalleeMeta::SelfRecv(impl_type)) | Some(CalleeMeta::SelfType(impl_type)) => {
                    let same_lang = same_lang_of(&all, &d.language, &[]);
                    let filtered = self_filter_candidates(&impl_type, &same_lang, db)?;
                    if !filtered.is_empty() {
                        edges_created += insert_relation_edges(
                            db,
                            &source_ids,
                            &filtered,
                            &d.relation,
                            d.metadata.as_deref(),
                            false,
                        )?;
                    }
                    // empty → drop: qualifier is fixed and the pool is now complete.
                }
                Some(CalleeMeta::RecvType(recv_type)) => {
                    // Bind precisely to the inferred type's own methods; an EMPTY
                    // filter (inherited method / mis-inferred type) falls through
                    // to the bare default chain below — rtype is strictly additive
                    // precision and must never drop an edge the bare path would
                    // have resolved (mirrors the batch-time arm).
                    let same_lang = same_lang_of(&all, &d.language, &[]);
                    let filtered = self_filter_candidates(&recv_type, &same_lang, db)?;
                    if !filtered.is_empty() {
                        edges_created += insert_relation_edges(
                            db,
                            &source_ids,
                            &filtered,
                            &d.relation,
                            d.metadata.as_deref(),
                            false,
                        )?;
                    } else {
                        handled = false;
                    }
                }
                Some(CalleeMeta::Path(segments)) => {
                    let same_lang = same_lang_of(&all, &d.language, &[]);
                    let filtered = path_filter_candidates(
                        &segments,
                        &same_lang,
                        &node_id_to_path,
                        db,
                        crate_roots,
                    )?;
                    if !filtered.is_empty() {
                        let final_targets = if filtered.len() > 1 {
                            refine_ambiguous_targets(&filtered, &d.rel_path, &node_id_to_path)
                        } else {
                            filtered
                        };
                        edges_created += insert_relation_edges(
                            db,
                            &source_ids,
                            &final_targets,
                            &d.relation,
                            d.metadata.as_deref(),
                            false,
                        )?;
                    }
                    // empty → drop: external crate path, as at batch time.
                }
                _ => {
                    handled = false; // bare call → default chain below
                }
            }
            if handled {
                continue;
            }

            // 6b. Default chain for bare calls (and JS receiver ns-misses):
            //     same-file → noise → same-language(refined) → pending buffer.
            let same_file_targets: Vec<i64> = all
                .iter()
                .copied()
                .filter(|id| {
                    node_id_to_path.get(id).map(|p| p.as_str()) == Some(d.rel_path.as_str())
                })
                .collect();
            if !same_file_targets.is_empty() {
                edges_created += insert_relation_edges(
                    db,
                    &source_ids,
                    &same_file_targets,
                    &d.relation,
                    d.metadata.as_deref(),
                    false,
                )?;
                continue;
            }
            if is_cross_file_call_noise(&d.target_name, &d.language) {
                continue;
            }
            // Cross-file pool: batch-time exclusion is BY SOURCE FILE (local_ids),
            // not by source node ids — mirror that (a routes_to self-target from
            // another file must stay in the pool; insert_relation_edges handles
            // self-pairs via allow_self).
            let cross_file: Vec<i64> = all
                .iter()
                .copied()
                .filter(|id| {
                    node_id_to_path.get(id).map(|p| p.as_str()) != Some(d.rel_path.as_str())
                })
                .collect();
            let same_language_targets = same_lang_of(&cross_file, &d.language, &[]);
            if !same_language_targets.is_empty() {
                let final_targets =
                    refine_ambiguous_targets(&same_language_targets, &d.rel_path, &node_id_to_path);
                edges_created += insert_relation_edges(
                    db,
                    &source_ids,
                    &final_targets,
                    &d.relation,
                    d.metadata.as_deref(),
                    false,
                )?;
                continue;
            }
            // Still unresolved after seeing the WHOLE tree — this is what the
            // persistent pending buffer is for (cross-INVOCATION forward
            // references: the callee's file arrives in a later indexing run).
            for &src_id in &source_ids {
                crate::storage::queries::insert_pending_unresolved_call(
                    db.conn(),
                    src_id,
                    &d.target_name,
                    &d.language,
                    d.metadata.as_deref(),
                )?;
            }
            continue;
        }

        // 7. Default name chain: same-file → same-language (refined) →
        //    (references: drop) / (structural: family pool → sentinel/drop).
        let all_target_ids = name_to_ids.get(&d.target_name).cloned().unwrap_or_default();
        let same_file_targets: Vec<i64> = all_target_ids
            .iter()
            .filter(|id| node_id_to_path.get(id).map(|p| p.as_str()) == Some(d.rel_path.as_str()))
            .copied()
            .collect();
        // Batch-time exclusion is BY SOURCE FILE, not by source node ids — a
        // routes_to whose target IS its (cross-file) source must stay in the
        // pool; insert_relation_edges handles self-pairs via allow_self.
        let cross_file: Vec<i64> = all_target_ids
            .iter()
            .copied()
            .filter(|id| node_id_to_path.get(id).map(|p| p.as_str()) != Some(d.rel_path.as_str()))
            .collect();
        let same_language_targets = same_lang_of(&cross_file, &d.language, &[]);

        let target_ids: Vec<i64> = if !same_file_targets.is_empty() {
            same_file_targets
        } else if !same_language_targets.is_empty() {
            refine_ambiguous_targets(&same_language_targets, &d.rel_path, &node_id_to_path)
        } else if d.relation == REL_REFERENCES {
            continue; // precision over recall, as at batch time
        } else {
            cross_file
                .iter()
                .filter(|id| {
                    matches!(
                        node_id_to_language.get(id).and_then(|l| l.as_deref()),
                        Some(l) if crate::utils::config::languages_compatible(l, &d.language)
                    )
                })
                .copied()
                .collect()
        };

        if target_ids.is_empty() && (d.relation == REL_IMPLEMENTS || d.relation == REL_IMPORTS) {
            for &src_id in &source_ids {
                unresolved_externals.push((src_id, d.target_name.clone(), d.relation.clone()));
            }
        } else {
            edges_created += insert_relation_edges(
                db,
                &source_ids,
                &target_ids,
                &d.relation,
                d.metadata.as_deref(),
                d.relation == REL_ROUTES_TO,
            )?;
        }
    }

    let (nodes_created, sentinel_edges) = mint_external_sentinels(db, &[], &unresolved_externals)?;
    Ok((edges_created + sentinel_edges, nodes_created))
}

/// Phase 3: build every indexed node's context string, store it, then embed.
///
/// Extracted from `index_files` (audit P1-9: 12 phases, 6 accumulators and a
/// brace depth of 13 in one 1,686-line function). This phase is a clean seam —
/// it reads `all_indexed` and writes only the `context_string` column plus the
/// vector table, touching none of the caller's accumulators — so the extraction
/// is behaviour-preserving by construction rather than by inspection.
///
/// `tick` is the caller's finalizing heartbeat: 3a/3b run inside one savepoint
/// and 3c can take minutes on a cold embed, so the progress consumer's mtime has
/// to move between them or a stale-file gate reads the run as killed.
fn build_context_strings_and_embed(
    db: &Database,
    all_indexed: &[FileIndexed],
    model: Option<&EmbeddingModel>,
    tick: &dyn Fn(),
) -> Result<()> {
    {
        let tx = db.savepoint("idx_context")?;
        let all_node_ids: Vec<i64> = all_indexed
            .iter()
            .flat_map(|fi| fi.node_ids.iter().copied())
            .collect();
        let all_edges = get_edges_batch(db.conn(), &all_node_ids)?;
        let all_node_details: HashMap<i64, (NodeResult, Option<String>)> = {
            let nodes = get_nodes_with_files_by_ids(db.conn(), &all_node_ids)?;
            nodes
                .into_iter()
                .map(|nwf| (nwf.node.id, (nwf.node, nwf.language)))
                .collect()
        };

        // Phase 3a: Build all context strings (CPU-bound, parallelized with rayon)
        // Flatten to (node_id, node_name, file_path) tuples for parallel iteration
        let node_tasks: Vec<(i64, &str, &str)> = all_indexed
            .iter()
            .flat_map(|fi| {
                fi.node_ids.iter().enumerate().map(move |(idx, &node_id)| {
                    (node_id, fi.node_names[idx].as_str(), fi.rel_path.as_str())
                })
            })
            .collect();

        let context_updates: Vec<(i64, String)> = node_tasks
            .par_iter()
            .map(|&(node_id, node_name, file_path)| {
                let edges = all_edges.get(&node_id);
                let cat = categorize_edges(edges, format_route_from_metadata);
                let node_detail = all_node_details.get(&node_id);

                let ctx = build_context_string(&NodeContext {
                    node_type: node_detail
                        .map(|(n, _)| n.node_type.clone())
                        .unwrap_or_default(),
                    name: node_name.to_string(),
                    qualified_name: node_detail.and_then(|(n, _)| n.qualified_name.clone()),
                    file_path: file_path.to_string(),
                    language: node_detail.and_then(|(_, lang)| lang.clone()),
                    signature: node_detail.and_then(|(n, _)| n.signature.clone()),
                    return_type: node_detail.and_then(|(n, _)| n.return_type.clone()),
                    param_types: node_detail.and_then(|(n, _)| n.param_types.clone()),
                    code_content: node_detail.map(|(n, _)| n.code_content.clone()),
                    routes: cat.routes,
                    callees: cat.callees,
                    callers: cat.callers,
                    inherits: cat.inherits,
                    imports: cat.imports,
                    implements: cat.implements,
                    exports: cat.exports,
                    doc_comment: node_detail.and_then(|(n, _)| n.doc_comment.clone()),
                });

                (node_id, ctx)
            })
            .collect();

        // Phase 3b: Batch update context strings in DB
        update_context_strings_batch(db.conn(), &context_updates)?;
        tx.commit()?;

        tracing::info!(
            "[index] Phase 3: context strings built for {} nodes",
            all_node_ids.len()
        );

        // Phase 3c: Embed outside the committed tx — recoverable on failure via repair_null_context_strings
        tick();
        if let Some(m) = model {
            if db.vec_enabled() {
                embed_and_store_batch(db, m, &context_updates)?;
            }
        }
    }
    Ok(())
}

/// Result of the three global edge post-passes, in the caller's accounting terms.
struct GlobalPostPassCounts {
    /// Bare calls newly bound to the target an explicit import names.
    bound: usize,
    /// Proximity-picked call edges dropped as contradicted by that import.
    pruned: usize,
}

/// Phases 2d-bind, 2d-prune and 2e — the three passes that run over the WHOLE
/// graph rather than over this batch.
///
/// Extracted from `index_files` (audit P1-9). They belong together: the bind
/// inserts the import-named edge, the prune removes the proximity-picked one it
/// contradicts, and only the pair repoints the call — running either alone
/// leaves the call either double-edged or edgeless. The confidence pass follows
/// because it reads the edge set the first two just settled.
///
/// The caller decides WHETHER to run them (they are a guaranteed no-op when the
/// batch changed nothing); this decides what they do.
fn run_global_edge_post_passes(db: &Database) -> Result<GlobalPostPassCounts> {
    // Phase 2d-bind: positively resolve bare-name calls to the node an explicit
    // import in the caller's file binds them to. `refine_ambiguous_targets`
    // picks the path-closest same-name node, which can be the wrong file when
    // the caller `from X import name`s a farther one; that wrong edge is dropped
    // by the prune below, so without this bind the call would be left with no
    // edge at all. Insert the import-bound edge first, then let the prune remove
    // the contradicted proximity edge — together they repoint the call.
    let bound = bind_calls_to_imported_targets(db)?;
    if bound > 0 {
        tracing::info!(
            "[index] Phase 2d-bind: bound {} bare call(s) to their imported target",
            bound
        );
    }

    // Phase 2d: drop bare-name call edges contradicted by an explicit import in
    // the caller's file. `refine_ambiguous_targets` keeps every tied same-name
    // candidate when it has no disambiguating info; an import edge IS that info,
    // so a bare `save()` in a file that does `from db import save` must bind to
    // db.save only — the fanned-out edge to a sibling `save` elsewhere is a false
    // caller. Removes those false positives without touching the correct edge.
    let pruned = prune_import_contradicted_call_edges(db)?;
    if pruned > 0 {
        tracing::info!(
            "[index] Phase 2d: pruned {} import-contradicted call edges",
            pruned
        );
    }

    // Phase 2e: classify edge confidence. Downgrades cross-file by-name
    // `calls`/`references` edges to inferred/ambiguous; every precise edge keeps
    // the column default `extracted`. Purely additive metadata — no edge
    // added or removed.
    let downgraded = classify_edge_confidence(db)?;
    if downgraded > 0 {
        tracing::info!(
            "[index] Phase 2e: classified {} cross-file by-name edge(s) as inferred/ambiguous",
            downgraded
        );
    }

    Ok(GlobalPostPassCounts { bound, pruned })
}

#[cfg(test)]
mod tests {
    use super::looks_like_cpp_header;

    #[test]
    fn cpp_header_detection_upgrades_only_real_cpp() {
        // C++ markers → parse the `.h` as C++ (so class symbols aren't dropped).
        assert!(looks_like_cpp_header(
            "class Shape {\npublic:\n  void f();\n};"
        ));
        assert!(looks_like_cpp_header(
            "struct S { int x; };\nnamespace ns { int g(); }"
        ));
        assert!(looks_like_cpp_header("template<typename T> T id(T x);"));
        assert!(looks_like_cpp_header("template <class T> struct Box {};"));
        assert!(looks_like_cpp_header("int Foo::bar() { return 1; }")); // scope resolution
        assert!(looks_like_cpp_header(
            "class Widget {\nprivate:\n  int id;\n};"
        ));
        assert!(looks_like_cpp_header(
            "class Base {\nprotected:\n  int n;\n};"
        ));

        // Pure C headers have none of these → stay C (no over-eager upgrade).
        assert!(!looks_like_cpp_header(
            "#ifndef FOO_H\n#define FOO_H\nint add(int a, int b);\nstruct Point { int x; int y; };\n#endif"
        ));
        assert!(!looks_like_cpp_header(
            "typedef struct { int fd; } handle_t;\nvoid close_handle(handle_t*);"
        ));
        assert!(!looks_like_cpp_header(
            "#define MAX(a,b) ((a)>(b)?(a):(b))\nextern int errno;"
        ));
    }
}
