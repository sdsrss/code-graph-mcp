// Shared domain constants used across modules.
// Relation constants, embedding dimensions, and other cross-cutting concerns
// live here to avoid layer violations (e.g., parser importing from storage).

// -- Data directory --
pub const CODE_GRAPH_DIR: &str = ".code-graph";

/// Opt-in per-project metrics-silence sentinel (a file under `.code-graph/`). When
/// present, the recommendations.jsonl writers — `cli::record_cli_use` (Rust) and
/// `recommendation-log.js` (JS hooks) — skip recording, so a development/dogfood
/// checkout's own CLI/hook runs (manual functionality testing, sims, ad-hoc dev)
/// don't pollute the project's adoption metrics with self-generated events. Does
/// NOT silence MCP usage.jsonl (flush_metrics), so real dev MCP tool metrics still
/// flow. Kept in sync with the literal in claude-plugin/scripts/recommendation-log.js.
pub const NO_METRICS_SENTINEL: &str = ".no-metrics";

// -- MCP tool surface --
/// Tools surfaced in `tools/list` (the live surface MCP clients see). Single
/// source of truth so `stats` can flag legacy/folded tool names recorded in
/// usage.jsonl (e.g. `read_snippet`, `trace_http_chain` from older sessions)
/// instead of commingling them with the live set. The registry's `list_tools()`
/// is asserted to match this exactly (mcp::tools tests), so they cannot drift.
pub const LIVE_MCP_TOOLS: &[&str] = &[
    "semantic_code_search",
    "get_call_graph",
    "get_ast_node",
    "project_map",
    "module_overview",
    "ast_search",
    "find_references",
];

// -- Relation types --
pub const REL_CALLS: &str = "calls";
pub const REL_INHERITS: &str = "inherits";
pub const REL_IMPORTS: &str = "imports";
pub const REL_ROUTES_TO: &str = "routes_to";
pub const REL_IMPLEMENTS: &str = "implements";
pub const REL_EXPORTS: &str = "exports";
/// A symbol *uses* another symbol without calling/importing/inheriting it:
/// a path-qualified const/static reference (`crate::a::FOO`) or a type-position
/// usage (`field: MyStruct`). Edgeless under calls/imports, so tracked separately.
pub const REL_REFERENCES: &str = "references";

/// Edge metadata marking an IMPORTS relation whose target is *statically known*
/// to live outside the project — today only Rust `use std::…` / `core` / `alloc`
/// / `proc_macro`.
///
/// Such an import must reach the `<external>` sentinel rather than binding to a
/// same-named project symbol. v52 achieved that by dropping the relation
/// outright, which killed the phantom `imports → fn fs` edges but also threw
/// away a free win: `prune_import_contradicted_call_edges` uses "this file
/// imports NAME bound to a DIFFERENT node" as its signal, so an explicit
/// `<external>` binding also cleans up the *calls* phantom that a bare
/// `use std::mem::swap; swap(&mut a, &mut b)` fabricates against a project
/// `swap`. Common risk names: swap / replace / take / min / max / read / write /
/// spawn / exit / sleep.
pub const IMPORT_EXTERNAL_META: &str = r#"{"ext":1}"#;

/// True when import metadata carries the [`IMPORT_EXTERNAL_META`] marker.
pub fn is_external_import_meta(metadata: Option<&str>) -> bool {
    metadata == Some(IMPORT_EXTERNAL_META)
}

// -- Import `q` markers --
//
// Stamped onto an import relation's metadata by the parser and read back in
// Phase 2 (`index_files.rs`). Producer and consumer sit in different modules, so
// every one of these was a bare string literal written out twice that had to
// agree by hand — the same two-copies shape this crate keeps rediscovering.
// Named here so they agree by construction.
//
// All four mean "this binding names no resolvable symbol": default name
// resolution would mint a spurious `<external>` node, so Phase 2 binds them to
// the RESOLVED file's `<module>` node instead.

/// `const m = require('./x')` — CommonJS namespace binding.
pub const IMPORT_Q_NS_REQUIRE: &str = "ns_require";
/// `import * as ns from './x'` — ESM namespace binding.
pub const IMPORT_Q_NS_IMPORT: &str = "ns_import";
/// `export * from './x'` — star re-export.
pub const IMPORT_Q_STAR_REEXPORT: &str = "star_reexport";
/// `import mod from './x'` — ESM default binding.
///
/// Binds module-level like the two namespace forms, but is deliberately NOT one
/// of them for member-call purposes: `ns.foo()` after `import * as ns` names a
/// module-level symbol, while `mod.foo()` after a default import names a member
/// of the default-exported value, which is not a top-level symbol of that file.
/// Feeding it to the namespace member-call map would bind those calls to
/// whatever same-named top-level symbol the module happens to have.
pub const IMPORT_Q_DEFAULT: &str = "default_import";

// -- Edge confidence tiers (v17+) --
// How an edge's target was resolved. Stored on `edges.confidence` and assigned
// by a single post-resolution classification pass (classify_edge_confidence),
// NOT threaded through the ~10 insert sites. Purely additive metadata: every
// edge still exists; consumers may OPT IN to filtering via --min-confidence.
//
// - extracted: same-file resolution, or a structural relation (imports / inherits
//   / implements / routes_to / exports) resolved by explicit path/parent. Precise.
// - inferred:  a cross-file `calls`/`references` edge resolved by bare name where
//   the target name is UNIQUE among same-language nodes. Likely correct.
// - ambiguous: a cross-file `calls`/`references` edge whose target name has >1
//   same-language definition — the by-name resolution could not pick uniquely.
//   The class behind the known false-positive flood (bare_name_call_qualifier,
//   method_call_edge_drops, value_reference_candidate_gen).
pub const CONF_EXTRACTED: &str = "extracted";
pub const CONF_INFERRED: &str = "inferred";
pub const CONF_AMBIGUOUS: &str = "ambiguous";

/// Rank a confidence tier high→low (extracted=2, inferred=1, ambiguous=0).
/// Unknown strings rank 0 so a corrupt/legacy value is treated as lowest, never
/// silently passing a `--min-confidence extracted` filter.
pub fn confidence_rank(c: &str) -> u8 {
    match c {
        CONF_EXTRACTED => 2,
        CONF_INFERRED => 1,
        _ => 0,
    }
}

/// Parse a user-supplied `--min-confidence` value to its canonical tier string,
/// or None if unrecognized (caller should error loudly, not silently pass-all).
pub fn normalize_confidence(input: &str) -> Option<&'static str> {
    match input.to_lowercase().as_str() {
        "extracted" | "exact" | "high" => Some(CONF_EXTRACTED),
        "inferred" | "medium" | "med" => Some(CONF_INFERRED),
        "ambiguous" | "low" | "all" => Some(CONF_AMBIGUOUS),
        _ => None,
    }
}

// Enum filters shared by the CLI and MCP surfaces. Each canonicalizes case (so
// `--direction BOTH` / MCP `direction:"Both"` are accepted like every other enum
// filter — `--node-type`, `--min-confidence`, `--language` already normalize case;
// `direction`/`relation` were the two that still matched case-sensitively inline)
// and returns None for an unknown value so callers error loudly at entry.

/// Canonicalize a call-graph `--direction` / `direction` (callers|callees|both).
pub fn normalize_call_direction(input: &str) -> Option<&'static str> {
    match input.to_lowercase().as_str() {
        "callers" => Some("callers"),
        "callees" => Some("callees"),
        "both" => Some("both"),
        _ => None,
    }
}

/// Does a stored route method satisfy a requested HTTP verb?
///
/// Route extraction stores `"ALL"` when the framework genuinely matches every
/// verb (Go `net/http` HandleFunc); it is a wildcard for matching purposes — an
/// exact-equality filter made `trace 'GET /health'` miss a Go route stored as
/// `ALL /health` while the bare-path `trace /health` found it, and the no-match
/// hint then blamed framework coverage. Comparison is case-insensitive (CLI
/// uppercases the request; MCP used exact `==`, drifting from the CLI's
/// eq_ignore case rule).
///
/// `"ANY"` is accepted as the same kind of wildcard but is no longer emitted by
/// any extractor: Flask `@app.route` without `methods=` used to store it, which
/// made `trace 'DELETE /x'` claim a route that answers 405, and since
/// INDEX_VERSION 60 that case stores the framework default `GET` instead. The
/// arm stays because the predicate is the one place both surfaces agree on
/// wildcard semantics, and a future extractor for a genuinely verb-agnostic
/// framework should reach for it rather than reinvent one.
pub fn route_method_matches(stored: &str, requested: &str) -> bool {
    stored.eq_ignore_ascii_case("ANY")
        || stored.eq_ignore_ascii_case("ALL")
        || stored.eq_ignore_ascii_case(requested)
}

/// Canonicalize a dependency `--direction` / `direction` (outgoing|incoming|both).
pub fn normalize_dep_direction(input: &str) -> Option<&'static str> {
    match input.to_lowercase().as_str() {
        "outgoing" => Some("outgoing"),
        "incoming" => Some("incoming"),
        "both" => Some("both"),
        _ => None,
    }
}

/// Every relation a `--relation` / `relation` filter accepts, `all` excluded.
///
/// This is the vocabulary the filter offers, and it must equal the set of
/// relations the graph can actually RETURN — an edge type that shows up in
/// results but is rejected by the filter is a contract the user can see and
/// cannot use. `exports` and `routes_to` were exactly that: real edge types,
/// visible under `--relation all`, refused by name (2026-08-16 audit §四). The
/// list exists as a constant, rather than as arms of the match below, so
/// `relation_filter_vocab_covers_every_edge_type` can hold it against the `REL_*`
/// constants and fail when a new edge type lands without one.
pub const RELATION_FILTER_VOCAB: &[&str] = &[
    REL_CALLS,
    REL_IMPORTS,
    REL_INHERITS,
    REL_IMPLEMENTS,
    REL_REFERENCES,
    REL_EXPORTS,
    REL_ROUTES_TO,
];

/// Human-readable form of [`RELATION_FILTER_VOCAB`] plus `all`, for help text and
/// validation errors — kept derived so the six copies the audit found drifting
/// cannot drift again.
pub fn relation_filter_vocab_list() -> String {
    format!("{}, all", RELATION_FILTER_VOCAB.join(", "))
}

/// [`relation_filter_vocab_list`] as a literal, for the places that need a `const`
/// (clap `help =`, `println!` in `print_help`). `relation_filter_help_matches_vocab`
/// pins it to the derived form, so the two cannot part ways.
pub const RELATION_FILTER_HELP: &str =
    "calls, imports, inherits, implements, references, exports, routes_to, all";

/// Parse a `--min-confidence` / `min_confidence` value into a canonical tier.
///
/// `None` and `Some("")` both mean **not given**; the caller supplies whatever
/// default it wants (a floor of `inferred` for callgraph/impact, no floor at all
/// for `refs`, which must show every usage site). Empty-as-absent is not a
/// nicety — it is how a shell spells an unset variable (`--min-confidence
/// "$TIER"`), and it used to be read one way by `callgraph`/`impact` (silently
/// the default) and the opposite way by `refs` (a hard error) for the very same
/// input (2026-08-16 audit §四 architectural-redundancy cluster).
///
/// `flag_label` keeps each surface's own spelling in the message (`--min-confidence`
/// for the CLI, `min_confidence` for MCP); everything after it is shared, which is
/// what the six verbatim copies of this block were not.
pub fn parse_min_confidence(
    raw: Option<&str>,
    flag_label: &str,
) -> anyhow::Result<Option<&'static str>> {
    match raw {
        None | Some("") => Ok(None),
        Some(c) => normalize_confidence(c).map(Some).ok_or_else(|| {
            anyhow::anyhow!(
                "{flag_label} must be one of: extracted, inferred, ambiguous (got '{c}')"
            )
        }),
    }
}

/// Default caller-traversal confidence floor for the RISK-reporting surfaces
/// (`callgraph`, `impact`, `show --impact`, MCP `get_call_graph`/`get_ast_node`).
/// Folding the ambiguous by-name fan-out out of a risk number is the point of
/// having a floor; `refs` deliberately has none.
pub const DEFAULT_RISK_CONF_FLOOR: &str = CONF_INFERRED;

/// Canonicalize a `--relation` / `relation` filter for find_references.
pub fn normalize_relation(input: &str) -> Option<&'static str> {
    let lower = input.to_lowercase();
    if lower == "all" {
        return Some("all");
    }
    RELATION_FILTER_VOCAB
        .iter()
        .copied()
        .find(|rel| *rel == lower)
}

// -- Index version --
// Bump this when parser/indexer logic changes in a way that produces different
// nodes or edges for the same source files. The server will detect a mismatch
// and automatically clear + rebuild the index.
// This is separate from SCHEMA_VERSION (which tracks table structure changes).
// Vector-only invalidation/refresh (e.g. delete_node_vectors_batch on a
// model=None incremental path) does NOT bump this — only node/edge/FTS output
// changes do; vectors regenerate via the NULL-vector background-embed convention.
pub const INDEX_VERSION: i32 = 64; // v64 (probe sweep 2026-08-19): a decorator, attribute or annotation sitting between a declaration and its doc comment blocked the whole channel, because the sibling walk broke on the first non-comment node and the wrapper climb demanded the declaration be its parent's literally-first named child. Four languages lost docs and the other seven never noticed: Java/Kotlin/Swift park annotations in the declaration's `modifiers` and C#/PHP in an `attribute_list` field, so the comment stays the immediate previous sibling there. The four that spell it as a SIBLING all broke — TS/JS `decorator` (`@Component({}) export class C {}` and `@Get() findAll() {}`, i.e. essentially every documented Angular/NestJS declaration), Rust `attribute_item` (every `#[derive]`d struct and `#[inline]`/`#[test]` fn, including this repo's own), and Dart `annotation`. Shapes measured from real parse trees, not guessed. Guarded by `test_doc_comment_parity_for_decorated_declarations` (13 rows across 11 languages, the working ones kept as controls) plus a negative control that the skip cannot cross a declaration. doc_comment values change for existing files, so indexes must rebuild. // v63 (E2E dogfood 2026-08-17): every EXPORTED TS/JS declaration was indexed with an EMPTY `doc_comment`. A JSDoc block precedes the whole `export function f(){}` statement, so it is a sibling of the `export_statement`, while `get_preceding_comment` walked the siblings of the inner `function_declaration` and found only the `export` keyword. Non-exported functions and class methods (unwrapped) kept their docs, which is why the column looked populated. In TypeScript the exported symbols are the documented ones, so the loss landed exactly where a concept query aims — and unlike a Python docstring, a JSDoc block sits OUTSIDE the node, so `code_content` did not carry it either: a phrase appearing only in a JSDoc (`search "issuer allowlist"`) was unreachable by any channel, and the embedding context string was built without it. `get_preceding_comment` now climbs `DOC_COMMENT_WRAPPERS`, bounded to 3 levels and only through a node that is its parent's first named child, so `const a = 1, b = 2` cannot hand the statement's doc to `b`. Sweeping the rest of the languages against real parse trees found three more wrappers hiding the same way — Go's `type_declaration` (the extractor sees the inner `type_spec`, whose only preceding sibling is the `type` keyword), Ruby's `body_statement` (a method's comment is a sibling of the class-body wrapper) and Dart's `method_signature` — plus Dart naming its `///` block `documentation_comment`, a spelling the three-name allowlist did not carry, so EVERY Dart symbol was undocumented. The comment test is now a `*_comment` suffix match. Python gained its own channel: it documents with a docstring, not a preceding comment, so `get_body_docstring` reads the first statement of a `function_definition`/`class_definition` body when it is a bare string (gated on those two kinds AND on the `string` literal kind, because a Rust `fn f() { "x"; }` also has a `block` body). Measured on a 95-file third-party TS/Vue checkout: documented symbols 264 → 335 (+71, +26.9%), with the edge set byte-identical at 36,671 rows — this changes `doc_comment` only. `test_doc_comment_parity_across_languages` now pins the (language, declaration form) axis that had no guard at all. Old indexes carry the empty column; only a rebuild fills it. // v62 (audit 2026-08-16 P1-3 + P1-4): the heritage axis stopped being three hard-coded node kinds. `class_declaration | class_definition | class` meant a Java `interface`/`enum`/`record`, a TypeScript `interface`, a PHP `interface`, a Kotlin `object`, a Swift `protocol` and a Dart `enum` emitted ZERO inheritance edges — nothing failed, the graph was simply incomplete, so `find_dead_code` reported an interface's implementers as unused and every heritage traversal under-reported. Declaration kinds now come from `HERITAGE_DECL_KINDS`, and three heritage-child spellings that no extractor read (`extends_interfaces`, `extends_type_clause`, Dart's `interfaces`) are handled. Two edge-set changes ride along: Go methods finally carry their receiver in `qualified_name` (`Server.Start`, so two types with a `Start` method stop being one indistinguishable symbol — P1-4), and a C# `enum E : byte` no longer emits the phantom `E inherits byte`, because C# spells an enum's underlying integral type with the same `base_list` syntax a class uses for its base. An index built before this is missing the new edges and still carries that phantom; only a rebuild fixes it. // v61 (audit 2026-08-16 P0-1): a file skipped for size or a parse failure now has its purged nodes pruned from the run's name map, so the deferred pass stops resolving onto ids the same run deleted. Any index built before this carries whatever that FK abort (787) destroyed: the aborting run had already committed the skipped file's new hash, so `compute_diff` never re-offered it and every cross-file edge the run had buffered — the caller's `imports`/`inherits`/`references` into it, plus every OTHER file's deferred relations, since one dead id rolls back the whole `idx_deferred` savepoint — was gone with no channel to rebuild it. Only a rebuild heals that, hence the bump. Also classifies confidence on a run whose only edge producer was the deferred pass (those edges used to keep the `extracted` column default). — v60 (post-v0.115.0 review NOTE-3): a Flask/Starlette `@app.route('/x')` with no `methods=` kwarg now stores the verb `GET` — the framework's own default (`methods` defaults to `["GET"]`, HEAD/OPTIONS auto-derived) — instead of the `ANY` wildcard. v25 introduced ANY to stop `trace 'GET /x'` missing the route under exact-equality matching, and v0.115.0 made ANY a matching wildcard, which fixed that false negative by buying a false positive: `trace 'DELETE /x'` claimed a handler that answers 405. Storing the real default is precise in both directions; HEAD/OPTIONS on a bare `@app.route` stay unmodelled (the metadata schema holds one verb, as it already does for `methods=['POST','PUT']` → POST). No extractor emits `ANY` any more; `route_method_matches` still accepts it as a wildcard for a future verb-agnostic framework. Old indexes carry the ANY rows and keep over-matching; bump to rebuild. — v59 (indexing audit 2026-08-02 P1-5): DELETING a file no longer silently destroys the non-`calls` edges pointing INTO it. Phase 0 buffered only `calls` before the cascade (`get_inbound_calls_for_pending` is hardcoded to that relation, because `pending_unresolved_calls` is a calls-only table), so an `imports`/`implements`/`inherits`/`references`/`exports`/`routes_to` edge from an UNCHANGED file was cascade-deleted with no recovery channel at all — and since the source file's hash still matched, no later run ever re-extracted it. A full rebuild of the same final tree kept the edge (re-resolved to an `<external>` sentinel once the target file was gone), so incremental and full diverged permanently and `deps`/`cycles`/`project_map` answered differently depending on how the index had been grown. Those edges are now requeued into the same post-batch deferred pass the edit path uses (`restore_inbound_edges`), which re-resolves them against the complete name map and mints the sentinel exactly where a rebuild would. Sources that are themselves in this run's changed set or in `delete_paths` are skipped, because their node ids are about to be invalidated and the deferred insert would abort the run on the edges FK (the failure `aaa238f` fixed on the edit path). Old incremental indexes are missing those edges; bump to rebuild. — v58 (audit 2026-08-02 P0-1/P1-2/P1-9): cross-batch resolution. On any tree larger than one batch (BATCH_SIZE 500 files), Phase 2 resolved every relation against a pool that could not contain LATER batches' nodes, so a fresh multi-batch index deterministically minted `<external>` phantoms for implements/imports whose real target sat batches ahead, and DROPPED inherits/exports/routes_to/references outright — and nothing ever healed it (only REL_CALLS had the pending buffer; rebuild reproduced the same wrong edges byte-for-byte). Measured: the same 4 files that produce 4 true edges alone produce 2 phantoms + 1 missing edge with 600 filler files. Now every relation that fails batch-time resolution is buffered in-memory and re-resolved once after the batch loop against the complete name map, mirroring the batch-time chain branch for branch; still-unresolved imports/implements mint their sentinel THERE, so a real later-batch node always beats a phantom. Also in v58: (a) a saved inbound edge whose by-name restore misses after its target file changed (symbol renamed/removed) is REQUEUED — calls into pending_unresolved_calls, everything else into the same deferred pass — instead of silently dropped, so an incremental rename now converges to the same graph a full rebuild produces; (b) `<external>` sentinel nodes with zero inbound edges are reaped at the end of every indexing run (they were never garbage-collected, lingered in the name-resolution pool, and made incremental node sets diverge from a fresh rebuild forever); (c) delete_paths are sorted+deduped at entry (HashMap-order first-wins). Old indexes carry the cross-batch phantoms/missing edges and orphaned sentinels; bump to rebuild. — v57 (audit batch 2026-07-29, part 3): `import mod, * as ns from './m'` emitted TWO identical module-level `imports` rows, one per binding, where each spelling alone emits one. They both survive because `idx_edges_unique` includes `metadata` on purpose (multiple route edges per file), so the differing `q` marker keeps both. The namespace marker wins: it also feeds ns_module_map for `ns.foo()` member calls, while the default marker deliberately feeds nothing else and is pure duplication once a namespace binding has claimed the edge. `deps` was never affected (it counts `COUNT(DISTINCT nb.id)`); edge totals and per-language relation stats were. Old indexes carry the doubled rows; bump to rebuild. NOT in v57, though an earlier draft of this batch had it: the rule that refused to guess a CONCATENATED include path (`require_once "config" . $env . ".php"` binding a real `config.php` the statement never includes). Two independent reviews each measured it as a NET LOSS of true edges on ordinary idioms — parenthesized operands, interpolated literals left of the last separator, `||` fallback chains, three-operand `__DIR__ . DIRECTORY_SEPARATOR . "file"` anchors — and both times the validating fixture happened to omit the shapes it broke. The phantom is real and still open; the extractor, which must answer with a single string, is the wrong layer to decide it. // v56 // Older entries (v56 and down) moved to CHANGELOG.md, which already carries the same per-version narrative and its rebuild notices. This one line was 33,598 bytes — 28% of src/domain.rs — and it is also a NODE in this project's own index, so every search over the repo carried it (2026-08-16 audit §四). What a reader debugging a live index needs is the last couple of bumps; the rest is release history and belongs with the releases.

// -- Pending-call buffer bound --
// A `pending_unresolved_calls` row survives this many resolution sweeps before
// being evicted. The buffer exists to bridge "caller indexed before callee"
// timing (incremental-edge-timing guarantee) — but ~99% of buffered rows are
// never-resolvable external/builtin calls (require/Some/Ok/…), structurally
// indistinguishable from a legit not-yet-added project symbol at any single
// point in time; only age tells them apart. 50 sweeps (each = one index pass
// that failed to resolve the row) is far past any realistic add-the-callee-
// later window while still bounding the table; an evicted legit row self-heals
// on the next full rebuild or caller-file touch (re-parse re-buffers it).
pub const PENDING_CALL_MAX_ATTEMPTS: i64 = 50;

// -- Betweenness centrality scale bound --
// Brandes is O(V·E) EXACT: every node is a BFS source. Past some size that stops
// being a command a person waits for, so above this many graph nodes the run
// switches to a Brandes–Pich estimate over an evenly-strided sample of sources
// (scaled by n/|pivots|) and says so on stderr. 5000 is chosen to leave real
// repositories exact — this one indexes 2,770 non-test nodes / 5,937 `calls`
// edges, and a 100K-node monorepo is where the exact form becomes minutes.
pub const BETWEENNESS_MAX_PIVOTS: usize = 5000;

// -- Schema-mismatch marker --
// Stable machine token appended to the "this DB's schema is newer than this
// binary supports" bail (storage::db). The plugin statusline matches THIS token —
// not translatable/reword-able prose — to tell the post-update window (an old
// cached binary running against a newer index, while the new binary downloads:
// "↻ updating") apart from a genuine "offline" failure. DO NOT change this string
// without updating claude-plugin/scripts/statusline.js in lockstep.
pub const SCHEMA_TOO_NEW_MARKER: &str = "code-graph:schema-too-new";

// -- Embedding --
pub const EMBEDDING_DIM: usize = 384;

// -- Semantic-search rerank tuning (search.rs) --
// Multipliers/thresholds applied AFTER RRF fusion to rerank candidates. Named
// here (audit §4/§8) so they are tunable + ablatable in one place rather than
// scattered as magic numbers. Values are the historical ones — extracting them
// is metric-neutral; change them only with a precision@5/MRR ablation.
/// RRF constant k: sharper rank sensitivity than the textbook 60 (top hits matter more).
pub const RERANK_RRF_K: u32 = 30;
/// Acronym-heavy query detection: ≤N short uppercase tokens are letter-exact identifiers.
pub const ACRONYM_MAX_TOKENS: usize = 3;
pub const ACRONYM_MAX_TOKEN_CHARS: usize = 5;
/// Fusion weights: acronym-heavy shifts toward FTS (token-exact); default favors vector.
pub const ACRONYM_FTS_WEIGHT: f64 = 2.0;
pub const ACRONYM_VEC_WEIGHT: f64 = 0.8;
pub const DEFAULT_FTS_WEIGHT: f64 = 1.0;
pub const DEFAULT_VEC_WEIGHT: f64 = 1.2;
/// match_confidence penalties. Vector-only (no FTS hit) = largely similarity noise.
pub const CONF_VEC_ONLY_PENALTY: f64 = 0.35;
/// OR-fallback fired (AND mode found no co-occurrence) → weaker match.
pub const CONF_OR_FALLBACK_PENALTY: f64 = 0.6;
/// Only judge FTS sparsity/intersection when FTS returned enough breadth (precision
/// queries with ≤4 hits legitimately have a low ratio and must not be penalized).
pub const CONF_SPARSITY_MIN_FTS: usize = 5;
/// FTS-sparsity tiers: (ratio threshold, confidence multiplier), most-sparse first.
pub const CONF_SPARSITY_R1: f64 = 0.1;
pub const CONF_SPARSITY_P1: f64 = 0.5;
pub const CONF_SPARSITY_R2: f64 = 0.25;
pub const CONF_SPARSITY_P2: f64 = 0.65;
pub const CONF_SPARSITY_R3: f64 = 0.5;
pub const CONF_SPARSITY_P3: f64 = 0.8;
/// Source-intersection: low FTS∩vec overlap in the top-k → less confidence.
pub const CONF_INTERSECTION_MIN_RATIO: f64 = 0.2;
pub const CONF_INTERSECTION_PENALTY: f64 = 0.75;
// (match_confidence is surfaced as a raw query-shape signal; the low_confidence
// warning fires on a text-anchor mechanic, not a match_confidence threshold —
// see src/mcp/server/tools/search.rs VECTOR_ONLY_WARNING. A prior
// CONF_WARNING_THRESHOLD=0.5 was removed: the calibration bench showed no signal
// separates good NL from nonsense, so a threshold warning was ~all false alarms.)
/// Name-match boost: +per-match, capped, for symbols whose name contains query terms.
pub const NAME_BOOST_PER_MATCH: f64 = 0.3;
pub const NAME_BOOST_CAP: f64 = 2.0;
/// Exact symbol-name match dominance. When the query is verbatim a node's
/// name/qualified_name, its definition must rank first: RRF already places it
/// (tier3 exact-symbol recall@10 was 0.984 RRF-only) but the `base × name_boost ×
/// size × doc` rerank buried exact matches under vector noise + size dampening,
/// dropping recall@10 to 0.806. This additive bonus dominates any non-exact
/// `adjusted` (which lies in [0, base×CAP] ⊂ [0,2]); exact matches then order
/// among themselves by `base_score`.
pub const EXACT_NAME_MATCH_BONUS: f64 = 100.0;
/// Size dampening: counter BM25/vector bias toward very large nodes (> threshold lines).
pub const SIZE_DAMPEN_LINES: f64 = 100.0;
pub const SIZE_DAMPEN_COEFF: f64 = 0.4;
/// Doc penalty: demote markdown headings for code-intent queries (unless lang=markdown).
pub const DOC_PENALTY_MARKDOWN: f64 = 0.4;

// -- Retrieval over-fetch (post-KNN filtering compensation) --
// vec0 KNN (`embedding MATCH … LIMIT k`) cannot pre-filter on joined `nodes`
// columns, so every filter — always-on test/module/external skip plus optional
// language/node_type — is applied in Rust AFTER the top-k fetch. A fetch sized to
// top_k lets a selective filter silently starve the result set (return < top_k, or
// nothing, while matches sit just past the cutoff). We over-fetch to compensate;
// when an optional language/node_type filter is active the survivors can be a small
// minority of the nearest neighbours, so the pool is widened further.
/// Base over-fetch multiplier for semantic_code_search with no language/node_type filter.
pub const SEARCH_BASE_OVERFETCH: i64 = 4;
/// Floor so a small top_k still has candidates after the always-on test/module skip.
pub const SEARCH_FETCH_FLOOR: i64 = 20;
/// Wider over-fetch when a selective language/node_type filter is active.
pub const SEARCH_FILTER_OVERFETCH: i64 = 16;
/// Floor for the filtered case.
pub const SEARCH_FILTER_FETCH_FLOOR: i64 = 100;
/// `similar` / find_similar_code over-fetch: self-exclusion + max_distance + test/module
/// skip are all post-fetch, so fetch a multiple of top_k rather than top_k+1.
pub const SIMILAR_OVERFETCH: i64 = 3;

/// Minimum AND-arm hit count for an FTS query to keep its precise results
/// instead of widening to OR.
///
/// A small ABSOLUTE floor, deliberately not a fraction of anything. The previous
/// rule, `fetch_limit / 10`, let [`search_fetch_count`]'s over-fetch factor leak
/// into a ranking decision — and the two factors differ by 4×, so at `top_k=20`
/// the floor was 8 unfiltered and **32** with `--language` set. Adding a filter,
/// an operation that can only NARROW a result set, made a query 4× more likely
/// to throw away its precise AND hits and answer with OR noise instead: same
/// query, same index, opposite precision depending on an unrelated switch
/// (2026-08-16 audit §四).
///
/// The direct-call path already used the floor's `max(3, …)` arm in practice,
/// and `test_fts5_and_threshold_no_unnecessary_or_fallback` pins the judgement
/// behind it: four precise AND hits for a `--limit 20` query are a better answer
/// than twenty loose ones. This makes that the rule everywhere. The visible
/// change is at large `top_k`, where a handful of exact matches is now kept
/// rather than widened away.
pub const AND_MATCH_FLOOR: usize = 3;

/// Candidate-pool size for semantic_code_search. `filtered` = a language or node_type
/// filter is active (widens the pool so the post-KNN filter cannot starve top_k). The
/// unfiltered value is byte-identical to the historical `(top_k*4).max(20)`, so the
/// retrieval benchmark — which passes no filter — is unchanged by the filtered branch.
pub fn search_fetch_count(top_k: i64, filtered: bool) -> i64 {
    if filtered {
        (top_k * SEARCH_FILTER_OVERFETCH).max(SEARCH_FILTER_FETCH_FLOOR)
    } else {
        (top_k * SEARCH_BASE_OVERFETCH).max(SEARCH_FETCH_FLOOR)
    }
}

/// Multiplier for the second pool when the first was consumed by post-fetch
/// filtering before `top_k` was filled.
pub const SEARCH_EXHAUSTED_RETRY_OVERFETCH: i64 = 4;
/// Hard cap on the retry pool: a query whose every hit is noise must cost a
/// bounded amount of work, not a full-index scan.
pub const SEARCH_RETRY_FETCH_CAP: i64 = 1000;

/// Pool size for a second retrieval pass, used when the first pass came back
/// FULL and the post-fetch filters still left fewer than `top_k` survivors.
///
/// The always-on module/external/test skip is applied in Rust after the fetch
/// and is not part of [`search_fetch_count`]'s widening, so a pool that happens
/// to be mostly noise starved `top_k` with no trace: the 2026-08-16 audit
/// measured top_k=3 fetching 20 candidates, dropping all of them, and reporting
/// "no results, check spelling" while real matches sat below the cut (P1-7).
/// Widening on exhaustion keeps the first pass — the one confidence is measured
/// on — byte-identical for every query that was not starved.
pub fn search_retry_fetch_count(previous: i64) -> i64 {
    previous
        .saturating_mul(SEARCH_EXHAUSTED_RETRY_OVERFETCH)
        .min(SEARCH_RETRY_FETCH_CAP)
}

/// Candidate-pool size for `similar` / find_similar_code. Over-fetches so the
/// post-fetch filters (self-exclusion, max_distance, test/module skip) do not starve
/// top_k — the old `top_k + 1` fell short on any single drop.
pub fn similar_fetch_count(top_k: i64) -> i64 {
    (top_k * SIMILAR_OVERFETCH).max(top_k + 1)
}

// -- Token estimation --
/// Approximate **bytes** per token for code content (1 token ≈ 3 bytes UTF-8).
///
/// Despite the historical name, all callers feed `s.len()` (UTF-8 byte length
/// in Rust) into this divisor — not Unicode char counts — which is why the
/// estimate stays sensible for CJK content too:
///
/// - ASCII: ~3-4 bytes/token in BPE → `bytes/3` slightly overestimates (safe).
/// - CJK: one char = 3 bytes UTF-8, ~1 token/char in BPE → `bytes/3 ≈ chars ≈ tokens` (accurate).
///
/// Conservative overestimation is the safe error direction: fires compression
/// earlier, never under-counts and overflows the downstream context window.
/// Used for token budget estimation across compression and search.
pub const CHARS_PER_TOKEN: usize = 3;

// -- Parsing limits --
pub const MAX_AST_DEPTH: usize = 64;
pub const MAX_RELATION_DEPTH: usize = 256;

/// Stack size for threads that run the index pipeline off the main thread.
///
/// `walk_for_relations` recurses once per AST level up to [`MAX_RELATION_DEPTH`],
/// so a ~800-byte source file of nested parens (`((((…f(1)…))))`) is enough to
/// drive it to the cap. Measured peak for that input: ~256–512 KiB under
/// `[profile.release]`, but ~2–4 MiB unoptimized, because debug frames carry
/// every spilled local of this function's very wide `match`.
///
/// `std::thread::spawn` gives 2 MiB by default, which the unoptimized figure
/// exceeds. That matters more than a normal panic would: a stack overflow is
/// `abort`, not unwind, so it walks straight past the serve loop's per-request
/// `catch_unwind` (see the `panic = "abort"` note in Cargo.toml) and takes the
/// whole long-lived stdio session with it. Sizing these threads explicitly
/// makes the margin independent of the build profile instead of something the
/// release optimizer happens to buy us.
pub const INDEX_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

// -- Indexing limits (env-var overridable) --

use std::sync::OnceLock;

/// Maximum file size to index. Override: CODE_GRAPH_MAX_FILE_SIZE (bytes).
/// Default: 1 MB.
pub fn max_file_size() -> u64 {
    static VAL: OnceLock<u64> = OnceLock::new();
    *VAL.get_or_init(|| {
        std::env::var("CODE_GRAPH_MAX_FILE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_048_576)
    })
}

/// Maximum code content length stored per node. Override: CODE_GRAPH_MAX_CODE_LEN (bytes).
/// Default: 4 KB.
pub fn max_code_content_len() -> usize {
    static VAL: OnceLock<usize> = OnceLock::new();
    *VAL.get_or_init(|| {
        std::env::var("CODE_GRAPH_MAX_CODE_LEN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096)
    })
}

/// Per-file parse timeout in milliseconds. Override: CODE_GRAPH_PARSE_TIMEOUT_MS.
/// Default: 5000 ms.
pub fn parse_timeout_ms() -> u64 {
    static VAL: OnceLock<u64> = OnceLock::new();
    *VAL.get_or_init(|| {
        std::env::var("CODE_GRAPH_PARSE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000)
    })
}

// -- Risk level assessment --
/// Compute impact risk level from caller/route counts.
///
/// `is_breaking` is `true` for changes that force every call site to change
/// (a removal or a signature change), which pins the result to HIGH regardless
/// of caller count; behaviour-only changes leave it `false` and scale by count.
pub fn compute_risk_level(
    prod_callers: usize,
    affected_routes: usize,
    is_breaking: bool,
) -> &'static str {
    if prod_callers > 10 || affected_routes >= 3 || is_breaking {
        "HIGH"
    } else if prod_callers > 3 || affected_routes > 0 {
        "MEDIUM"
    } else {
        "LOW"
    }
}

/// True when a node type is a function-like symbol whose usages are fully
/// captured by the `calls` call graph. False for types, constants, traits,
/// modules, etc. — whose real blast-radius includes imports / field access /
/// instantiation / type annotations that impact analysis does not track.
pub fn is_function_node_type(node_type: &str) -> bool {
    matches!(node_type, "function" | "method")
}

/// Warning surfaced by impact analysis when the target is non-function-like
/// and has zero call-graph callers. Prevents the risk level from reading as
/// a misleading `LOW` for constants / types / traits whose real users are
/// imports or type references, not calls.
pub const NON_FUNCTION_IMPACT_WARNING: &str = "Impact analysis tracks function call chains. This symbol is not a function — actual usage (imports, field access, type annotations, instantiation) may be broader than shown. Use `find_references` (MCP) or `code-graph-mcp refs <symbol>` (CLI) to find all references.";

// -- Test symbol detection --
/// Check if a symbol is a test/harness function/file based on naming conventions.
/// Used by both MCP server and CLI to separate test vs production callers.
///
/// `benches/` is classified as test/harness because criterion benchmarks are
/// macro-driven entry points (`criterion_group!`) — counting them as production
/// callers inflates impact-analysis risk and corrupts caller_count rankings.
pub fn is_test_symbol(name: &str, file_path: &str) -> bool {
    name.starts_with("test_")
        || name.ends_with("Test")
        || name.ends_with("Tests")
        || is_test_path(file_path)
}

/// Authoritative test predicate for a graph node: trust the AST-level `nodes.is_test`
/// flag first, falling back to the [`is_test_symbol`] name/path heuristic for rows
/// that don't carry it. The flag (set by the parser for `#[cfg(test)] mod tests` /
/// `#[test]` / `@Test` / ...) catches inline unit tests with descriptive snake_case
/// names in a `src/` file that the heuristic MISSES; the heuristic still catches
/// integration tests in `tests/`, `test_`-prefixed names, and any node whose
/// `is_test` projection predates a surface.
///
/// Single source so every caller-/callee-partitioning surface (callgraph, trace,
/// `show` references) classifies tests identically — mirrors `classify_impact`'s
/// rule (`graph::impact`) and prevents the is_test "sibling-hole" drift the v0.79.1
/// audit traced across impact/callgraph/trace/show.
pub fn is_test_node(is_test_flag: bool, name: &str, file_path: &str) -> bool {
    is_test_flag || is_test_symbol(name, file_path)
}

/// True for a search/similarity candidate that every result surface skips as
/// non-real output: a file-level `<module>` placeholder, an `<external>` stub,
/// or a test symbol. Single source for the triad otherwise reimplemented in
/// `cmd_search`/`tool_semantic_search` and `cmd_similar`/`tool_find_similar_code`
/// across the CLI and MCP surfaces (a recurring drift site — the CLI search/
/// similar paths historically omitted the `<external>` leg the MCP path applied).
///
/// Takes the authoritative `nodes.is_test` flag, not just the name/path
/// heuristic. Without it the two halves of one query disagreed: the SQL side
/// filters with [`is_test_node_sql`] (flag OR heuristic — `nodes.rs` calls it "a
/// superset of is_skippable_result's check"), while this post-filter saw only
/// the heuristic, so an inline `#[cfg(test)]` symbol in a `src/` file survived
/// into search results through the channel that fetched it (2026-08-16 audit §四).
pub fn is_skippable_result(
    is_test_flag: bool,
    node_type: &str,
    node_name: &str,
    file_path: &str,
) -> bool {
    (node_type == "module" && node_name == "<module>")
        || file_path == EXTERNAL_FILE_PATH
        || is_test_node(is_test_flag, node_name, file_path)
}

/// Pseudo-file holding the `<external>` sentinel nodes that unresolved imports
/// and implements edges bind to. Not a real path and never on disk.
///
/// Any surface that asks "which definitions does this NAME have?" must exclude
/// it. A sentinel is not a definition the user can open, select, or act on —
/// offering one as a disambiguation candidate is strictly worse than offering
/// nothing, because it turns a symbol that resolved into one that refuses to.
pub const EXTERNAL_FILE_PATH: &str = "<external>";

/// The synthetic per-file node's name: the scope a top-level statement belongs to
/// when it sits in no function (imports, module-level calls — see the
/// `<module>`-scope fallback in the relation extractors).
pub const MODULE_NODE_NAME: &str = "<module>";

/// A node name as a human should read it.
///
/// [`MODULE_NODE_NAME`] is an internal sentinel, and `refs` printed it verbatim:
/// `[imports] <module> (src/api/server.ts:1)` asks the reader to know that the
/// angle brackets are ours and not part of their code. The file path next to it
/// already says which file; what the sentinel adds is "top level, not inside any
/// function", so say that. Machine surfaces (`--json`, MCP) keep the raw name —
/// consumers key off it.
pub fn display_node_name(name: &str) -> &str {
    if name == MODULE_NODE_NAME {
        "(file top level)"
    } else {
        name
    }
}

/// Classify a dead-code candidate as exported-but-unused (`true`) vs a true
/// orphan (`false`). Exported = visible outside its module (public/`pub`, or an
/// uppercase Go identifier, or an explicit export edge), so even without tracked
/// callers removal is a wider decision than for an orphan.
///
/// Single source for the orphan/exported split otherwise reimplemented at three
/// sites (`cmd_dead_code` text path, `cmd_dead_code` JSON path, and
/// `tool_find_dead_code`). The CLI JSON path had drifted — it omitted the Go
/// export leg the text + MCP paths apply, so exported Go symbols were misfiled as
/// orphans in `--json` output only.
pub fn is_dead_code_exported(
    has_export_edge: bool,
    code_content: &str,
    file_path: &str,
    name: &str,
) -> bool {
    has_export_edge
        || code_content.starts_with("pub ")
        || code_content.starts_with("pub(")
        || (file_path.ends_with(".go") && name.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// File-level test classifier (path heuristics only) shared by `is_test_symbol` and
/// the `affected` command. NOT the only test-path matcher: the SQL counterparts
/// (`prod_source_filter_and` / `test_source_filter_or` below) and the local closure in
/// `indexer::pipeline::resolve::refine_ambiguous_targets` use their own, intentionally
/// divergent patterns. See the "Five sites must agree" note below and
/// feedback_test_classifier_dual_sources.md before changing any one of them.
pub fn is_test_path(file_path: &str) -> bool {
    // Case-insensitive `test`/`tests` DIRECTORY segment. xUnit/NUnit/MSTest put
    // suites under `src/Tests/<Project>/…` and Maven/Gradle under
    // `src/test/java/…`; both were invisible to the old `starts_with("tests/")`
    // leg, so `affected` reported "0 test files to re-run" for a C# change whose
    // test called the changed symbol directly (issue #36) — a silent false
    // negative in the one output a CI integration would act on.
    let lower = file_path.to_ascii_lowercase();
    if lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
    {
        return true;
    }
    // PascalCase test-class convention: `ResponsibleEntityServiceTests.cs`,
    // `AuthHandlerTest.java`, `RouteSpec.scala`. Case-SENSITIVE and pinned to a
    // known extension so `src/latest.rs` and `src/mytests.rs` stay production.
    if PASCAL_TEST_STEM_EXTS.iter().any(|(stem, exts)| {
        exts.iter()
            .any(|ext| file_path.ends_with(&format!("{}.{}", stem, ext)))
    }) {
        return true;
    }
    // `foo_test.<ext>` beyond the original Go/Rust pair, and the pytest
    // `test_*.py` / `conftest.py` filename conventions.
    if INFIX_TEST_EXTS
        .iter()
        .any(|ext| file_path.ends_with(&format!("_test.{}", ext)))
    {
        return true;
    }
    // pytest filename conventions. Case-SENSITIVE, like the PascalCase leg above
    // and unlike the directory leg: pytest matches `python_files` with
    // `_pytest.pathlib.fnmatch_ex`, which does NOT normcase, and discovers
    // conftest by the literal basename `conftest.py`. So `api/Test_Signup.py`
    // and `api/Conftest.py` are ordinary production modules pytest never
    // collects. This also keeps the leg identical to its SQL mirror
    // (`is_test_node_sql`), where GLOB is case-sensitive — lower-casing here was
    // a silent Rust-vs-SQL disagreement on exactly those two shapes.
    if file_path.ends_with(".py")
        && (file_path.starts_with("test_")
            || file_path.contains("/test_")
            || file_path.ends_with("conftest.py"))
    {
        return true;
    }
    file_path.starts_with("benches/")
        || file_path.starts_with("bench/")
        || file_path.contains("__tests__/")
        || file_path.ends_with("/tests.rs")
        || file_path.ends_with(".test.ts")
        || file_path.ends_with(".test.js")
        || file_path.ends_with(".test.tsx")
        || file_path.ends_with(".test.jsx")
        || file_path.ends_with(".spec.ts")
        || file_path.ends_with(".spec.js")
        || file_path.ends_with(".spec.tsx")
        || file_path.ends_with(".spec.jsx")
}

/// Extensions whose ecosystems name a test class `FooTests.<ext>` (PascalCase).
///
/// Enumerated rather than "any extension" so the SQL mirror ([`is_test_node_sql`])
/// can express the same predicate EXACTLY: `GLOB '*Tests.cs'` is equivalent to
/// Rust's `ends_with("Tests.cs")`, whereas a last-path-segment rule has no GLOB
/// equivalent (`*` crosses `/`). Both sides are generated from these constants,
/// so a new entry lands in both at once.
pub const PASCAL_TEST_EXTS: [&str; 8] = ["cs", "vb", "fs", "java", "kt", "scala", "swift", "php"];

/// Extensions where the `Spec` stem means TEST and not "specification".
///
/// ScalaTest (`FlatSpec`/`WordSpec`) and Kotest name suites `FooSpec`, so there
/// the stem is as reliable as `Test`. Nowhere else: `Spec` is an ordinary
/// production noun in the C#/Java/PHP/Swift world — `src/Contracts/OpenApiSpec.cs`,
/// `src/Protocol/WireSpec.java`, `src/Api/OpenApiSpec.php` are all shipped code.
/// Classifying those as tests is not a cosmetic mislabel: `is_test_symbol` feeds
/// `is_skippable_result`, so their symbols vanish from `search` entirely, and
/// `affected` reports them as "test file(s) to re-run".
pub const SPEC_TEST_EXTS: [&str; 2] = ["scala", "kt"];

/// PascalCase stem suffixes, each paired with the extension set it is a TEST
/// convention in. Per-stem rather than a flat cross-product because the stems do
/// not share an ecosystem — see [`SPEC_TEST_EXTS`].
pub const PASCAL_TEST_STEM_EXTS: [(&str, &[&str]); 3] = [
    ("Test", &PASCAL_TEST_EXTS),
    ("Tests", &PASCAL_TEST_EXTS),
    ("Spec", &SPEC_TEST_EXTS),
];

/// Extensions using the `foo_test.<ext>` file-naming convention.
pub const INFIX_TEST_EXTS: [&str; 4] = ["go", "rs", "py", "dart"];

/// Shared corpus for the cross-language parity guard over [`is_test_path`].
///
/// The predicate exists in four languages (Rust here, SQL in [`is_test_node_sql`],
/// JS in `claude-plugin/scripts/pr-impact-comment.js`, Python in
/// `scripts/embedding_benchmark/*.py`). Rust↔SQL had a real differential test;
/// the other mirrors had only hand-maintained case lists that a widening of the
/// Rust side never touched. `tests/predicate_parity.rs` runs THIS list through
/// every mirror and diffs against Rust.
///
/// One entry per leg plus the near-misses that pin each leg's case-sensitivity.
/// **Adding a leg to `is_test_path` means adding a positive AND a near-miss
/// negative here** — otherwise the guard silently stops covering it.
pub const TEST_PATH_PARITY_CORPUS: &[&str] = &[
    // Directory legs (case-INsensitive).
    "tests/foo.rs",
    "test/foo.rs",
    "src/Tests/Api.Tests/AuthTests.cs",
    "src/test/java/com/x/AuthHandler.java",
    "benches/b.rs",
    "bench/b.rs",
    "src/__tests__/x.ts",
    "src/foo/tests.rs",
    // PascalCase stem legs (case-SENSITIVE, extension-pinned).
    "app/Domain/AuthServiceTests.cs",
    "app/Domain/AuthServiceTest.java",
    "app/routes/RouteSpec.scala",
    "app/routes/RouteSpec.kt",
    // Infix `_test.<ext>`.
    "pkg/foo_test.go",
    "src/mod_test.rs",
    "pkg/util_test.py",
    "lib/widget_test.dart",
    // pytest conventions (case-SENSITIVE).
    "api/test_signup.py",
    "api/sub/test_signup.py",
    "api/conftest.py",
    // JS/TS suffix legs.
    "src/a.test.ts",
    "src/a.test.js",
    "src/a.test.tsx",
    "src/a.test.jsx",
    "src/a.spec.ts",
    "src/a.spec.js",
    "src/a.spec.tsx",
    "src/a.spec.jsx",
    // Plain production paths.
    "src/auth.ts",
    "src/main.rs",
    "src/api.py",
    // Near-misses: each pins one leg's exact boundary. A mirror that reached for
    // a looser match (lower-casing the pytest leg, dropping the extension pin,
    // treating `Spec` as universal) flips one of these and only these.
    "src/mytests.rs",
    "src/attests.py",
    "src/latest.cs",
    "src/Contest.cs",
    "src/Testimonial.cs",
    "src/protest/api.cs",
    "src/testing/api.cs",
    "src/latest_test.txt",
    "src/attest.py",
    "api/Test_Signup.py",
    "api/Conftest.py",
    "api/sub/Test_x.py",
    "src/Contracts/OpenApiSpec.cs",
    "src/Protocol/WireSpec.java",
    "src/Api/OpenApiSpec.php",
    "src/Model/FieldSpec.swift",
];

/// SQL predicate mirroring [`is_test_node`] for a node aliased `node_alias` joined to
/// its file aliased `file_alias`. Returns a parenthesized boolean (`(… OR …)`) meant
/// for `NOT (…)` in a WHERE clause, so a node-level SQL surface (dead-code,
/// surprising) classifies tests identically to the Rust query-time [`is_test_node`]
/// path: the stored `is_test` flag OR the [`is_test_symbol`] name/path heuristic.
///
/// Why this exists separately from [`test_source_filter_or`]: that one is the
/// edge-oriented (`src`/`sf` alias) variant and is intentionally NARROWER — it omits
/// the `*Test`/`*Tests` name legs and several path suffixes. Surfaces that classify a
/// *node* (not an edge source) and want full `is_test_symbol` parity — e.g. so an
/// integration test `def test_foo()` in `tests/` (whose AST `is_test` flag is 0
/// because the parser only sets it for `#[cfg(test)]`/`@Test`/... markers) is not
/// reported as dead code — must use THIS helper.
///
/// Uses `GLOB` (case-sensitive, `_` literal), not `LIKE` (case-insensitive, `_`
/// wildcard), so it matches Rust's `starts_with`/`ends_with`/`contains` EXACTLY:
/// `test_foo` matches but `Test_foo` does not, and `myTest` matches but `mytest`
/// does not — a `LIKE`-based port would wrongly flag all four. Kept in lockstep with
/// `is_test_symbol`/`is_test_path` by the `test_is_test_node_sql_matches_rust` parity
/// test (any new leg added to either must be added here and asserted there).
pub fn is_test_node_sql(node_alias: &str, file_alias: &str) -> String {
    let n = node_alias;
    format!(
        "({n}.is_test = 1 \
         OR {n}.name GLOB 'test_*' \
         OR {n}.name GLOB '*Test' \
         OR {n}.name GLOB '*Tests' \
         OR {paths})",
        paths = test_path_legs_sql(file_alias),
    )
}

/// The PATH half of the test classification — every leg of [`is_test_path`] that
/// looks at the file path, OR-joined, with no surrounding parentheses.
///
/// Extracted because two SQL surfaces need exactly this set and had drifted
/// apart: the node-level classifier ([`is_test_node_sql`]) carried all of it
/// while the edge-level source filter ([`prod_filter_and`] /
/// [`test_source_filter_or`]) had only the anchored `tests/%` prefix and the
/// infix leg. Under an xUnit (`src/Tests/Api/AuthTests.cs`), Maven
/// (`src/test/java/…`) or JS (`foo.test.js`) layout, one surface called a file a
/// test and the other counted its symbols as production callers — measured on
/// this repository, 792 `calls` edges were classified both ways at once.
///
/// Directory legs use LIKE (ASCII-case-insensitive in SQLite, matching Rust's
/// `to_ascii_lowercase` compare); none of those patterns contains `_`, so LIKE's
/// `_`-as-wildcard cannot fire. The pytest legs stay on GLOB — both for
/// `_`-as-literal AND because they are case-SENSITIVE on the Rust side too
/// (pytest's `fnmatch_ex` does not normcase; `Conftest.py` is not a conftest).
/// Mixing the two is deliberate, not an oversight: see [`is_test_path`].
pub fn test_path_legs_sql(file_alias: &str) -> String {
    let f = file_alias;
    // Generated legs — same constants the Rust predicate reads, so the two
    // cannot drift as ecosystems are added.
    let mut generated = String::new();
    for (stem, exts) in PASCAL_TEST_STEM_EXTS {
        for ext in exts {
            generated.push_str(&format!("{f}.path GLOB '*{stem}.{ext}' OR "));
        }
    }
    for ext in INFIX_TEST_EXTS {
        generated.push_str(&format!("{f}.path GLOB '*_test.{ext}' OR "));
    }
    format!(
        "{generated}{f}.path LIKE 'tests/%' \
         OR {f}.path LIKE 'test/%' \
         OR {f}.path LIKE '%/tests/%' \
         OR {f}.path LIKE '%/test/%' \
         OR {f}.path GLOB 'test_*.py' \
         OR {f}.path GLOB '*/test_*.py' \
         OR {f}.path GLOB '*conftest.py' \
         OR {f}.path GLOB 'benches/*' \
         OR {f}.path GLOB 'bench/*' \
         OR {f}.path GLOB '*__tests__/*' \
         OR {f}.path GLOB '*/tests.rs' \
         OR {f}.path GLOB '*.test.ts' \
         OR {f}.path GLOB '*.test.js' \
         OR {f}.path GLOB '*.test.tsx' \
         OR {f}.path GLOB '*.test.jsx' \
         OR {f}.path GLOB '*.spec.ts' \
         OR {f}.path GLOB '*.spec.js' \
         OR {f}.path GLOB '*.spec.tsx' \
         OR {f}.path GLOB '*.spec.jsx'"
    )
}

// -- SQL counterparts of is_test_symbol --
//
// Reused by every SQL query that counts/orders by caller_count to keep the
// classification aligned with `is_test_symbol`. Five sites must agree —
// see feedback_test_classifier_dual_sources.md for the full inventory.
//
// Convention: callers MUST alias the source node as `src` and the source file
// as `sf` and provide their own `JOIN` for the edges table. The helpers below
// emit the JOIN on `src`/`sf` and the WHERE/CASE clause body separately.

/// JOINs that attach the source node and source file to an `edges` row.
/// `edges_alias` is the alias used in the outer FROM/JOIN for the edges table.
/// Pair with [`prod_source_filter_and`] in the WHERE clause.
pub fn prod_source_join_sql(edges_alias: &str) -> String {
    format!(
        "JOIN nodes src ON src.id = {e}.source_id \
         JOIN files sf ON sf.id = src.file_id",
        e = edges_alias,
    )
}

/// AND-joined conditions that exclude test/bench source rows.
/// Combines the AST-level `src.is_test=0` flag with name and path heuristics —
/// kept in sync with `is_test_symbol`. Caller is expected to splice this
/// inside a WHERE clause already started with another condition (no leading AND
/// is added by callers — they prepend ` AND ` themselves) or inside a CASE WHEN.
///
/// This is a fifth copy of the test-classification rule, and until the
/// 2026-07-27 audit the only one with no mechanical guard. Two of its legs were
/// BROADER than `is_test_symbol`, each excluding real production rows from every
/// prod-caller count with no error raised anywhere:
///   * the infix path leg, `LIKE '%_test.%'` — `_` is a LITERAL in GLOB and a
///     single-character WILDCARD in LIKE, so it swallowed `latest.cs` (`%`=`l`,
///     `_`=`a`, then `test.`) and `attest.py`, and being a contains over any
///     extension it also took `notes_test.txt`. GLOB is anchored;
///   * the name leg, `LIKE 'test\_%'`, which is ASCII-case-INsensitive in SQLite
///     while `is_test_symbol` is `starts_with("test_")`, so `Test_Signup` was
///     excluded here and called production there. GLOB is case-sensitive.
///
/// Its PATH half is no longer narrower: it shares [`test_path_legs_sql`] with the
/// node-level classifier, because the gap was observable rather than theoretical
/// — 792 `calls` edges in this repository were counted as production callers by
/// this filter while `dead_code` / `affected` / `ast_search` called their source
/// files tests. It stays deliberately narrower on the NAME half (no
/// `*Test`/`*Tests` symbol-name suffix), an accepted recall gap asserted
/// one-directionally by `prod_source_filter_never_excludes_a_production_path`.
///
/// That gap was re-examined on 2026-07-29 and the narrower side is the correct
/// one, with a number behind it. Adding the two name legs here would have
/// matched 10 nodes in this repository's own index: 9 markdown headings
/// literally titled "Tests" (CHANGELOG section headers) and the production
/// function `formatCoveringTests`. **10 of 10 false positives, 0 true
/// positives.** The two sides also fail in opposite directions, which is why
/// they should not be symmetric: a false positive in the NODE-level classifier
/// ([`is_test_node_sql`]) only hides a dead symbol from `dead_code` — a miss.
/// A false positive HERE drops a real production caller from every prod-caller
/// count, so its callee looks colder and deader than it is, and nothing
/// anywhere raises an error. In the ecosystems where `*Tests` classes are real
/// (xUnit, JUnit) the file is named after the class, so `PASCAL_TEST_STEM_EXTS`
/// already catches it via the path legs. Widening this half needs a corpus that
/// demonstrates a true positive first; this repository cannot supply one.
pub fn prod_source_filter_and() -> String {
    prod_filter_and("src", "sf")
}

/// Same rule for an arbitrary node/file alias pair.
///
/// `project_map` carried two hand-written copies of these conditions against its
/// own `n`/`f` aliases, and the 2026-07-27 batch fixed only the `ESCAPE` on
/// their path leg — so within one `hot_functions` query the SOURCE rows were
/// judged by the anchored, extension-pinned, case-sensitive rule here while the
/// TARGET rows kept the unanchored, any-extension, case-insensitive LIKE. Any
/// symbol in `*_test.java` / `*_test.ts` / `*_test.rb` (only go/rs/py/dart are
/// in `INFIX_TEST_EXTS`), or named `Test_*`, vanished from `project_map` while
/// `callgraph` listed all its callers.
pub fn prod_filter_and(node_alias: &str, file_alias: &str) -> String {
    let n = node_alias;
    format!(
        "{n}.is_test = 0 \
         AND {n}.name NOT GLOB 'test_*' \
         AND NOT ({paths})",
        paths = test_path_legs_sql(file_alias),
    )
}

/// OR-joined inverse of [`prod_source_filter_and`] — matches test/bench sources.
/// Used by SUM/CASE constructs that count test callers separately (e.g.
/// project_map's hot_functions test_cnt CASE). Kept an exact inverse; the guard
/// test asserts the two never agree on any corpus path.
pub fn test_source_filter_or() -> String {
    format!(
        "src.is_test = 1 \
         OR src.name GLOB 'test_*' \
         OR {paths}",
        paths = test_path_legs_sql("sf"),
    )
}

// -- Dead-code ignore defaults --
/// Path-prefix defaults for `find_dead_code` ignore_paths.
///
/// Macro/harness-invoked entry points are not in the static AST call graph
/// because the references go through tokens the parser can't (or doesn't yet)
/// resolve:
/// - `claude-plugin/`: hook handlers / lifecycle scripts / auto-update hooks
///   called from `settings.json` hook definitions or shell, not JS imports.
/// - `benches/`: Criterion bench fns named inside `criterion_group!(...)`
///   tokens; macro arguments are not parsed as references.
///
/// Callers wanting the unfiltered list pass `ignore_paths: []` (CLI:
/// `--no-ignore`).
pub fn default_dead_code_ignores() -> Vec<String> {
    vec!["claude-plugin/".to_string(), "benches/".to_string()]
}

/// The `--type` / `node_type` filter vocabulary, in the spelling users type.
///
/// One source, so the help text and the parser cannot disagree (2026-08-16
/// audit §四). `module` is deliberately absent: accepting a word is not the same
/// as honoring it, and every surface taking this filter drops module
/// placeholders unconditionally — see [`type_filter_note`].
pub const TYPE_FILTER_VOCAB: &[&str] = &[
    "fn", "class", "struct", "enum", "trait", "type", "const", "var",
];

/// [`TYPE_FILTER_VOCAB`] as the literal every help/error string uses.
/// `type_filter_help_matches_vocab` pins the two together.
pub const TYPE_FILTER_HELP: &str = "fn, class, struct, enum, trait, type, const, var";

/// [`TYPE_FILTER_HELP`] phrased for a clap `help =` attribute, which needs a
/// `const` (a doc comment cannot be derived from one, and a doc comment is
/// exactly where three of the stale copies lived).
pub const TYPE_FILTER_HELP_ARG: &str =
    "Filter by node type: fn, class, struct, enum, trait, type, const, var";

/// A targeted suffix for the "unknown type filter" errors, for values a user
/// plausibly types that name nodes the search surfaces exclude by construction.
///
/// `module` used to be advertised because [`normalize_type_filter`] accepted it,
/// but no surface can ever return one: `is_skippable_result` drops
/// `<module>`-named nodes for search / ast-search / similar, and the dead-code
/// SQL pins `n.name != '<module>'`. "Accepted" read as "supported" and the user
/// got a bare zero-hit instead of a pointer to the commands that do list
/// modules. Empty string when there is nothing extra worth saying.
pub fn type_filter_note(input: &str) -> &'static str {
    match input.to_lowercase().as_str() {
        "module" | "modules" | "file" | "files" | "dir" | "directory" => {
            " Module/file placeholders are never search results — list modules with \
             `map`, `overview <path>` or `tour` instead."
        }
        _ => "",
    }
}

// -- Node type normalization --
/// Normalize shorthand type filter into canonical AST node types.
/// Shared by CLI and MCP tool implementations.
pub fn normalize_type_filter(input: &str) -> Vec<&'static str> {
    match input.to_lowercase().as_str() {
        "fn" | "func" | "function" | "method" => vec!["function", "method"],
        "class" => vec!["class"],
        "struct" => vec!["struct"],
        "enum" => vec!["enum"],
        "interface" | "iface" | "trait" => vec!["interface", "trait"],
        // The extractor emits `type` for a TS/JS `type X = …` (treesitter.rs
        // `type_alias_declaration`); the filter mapped to `type_alias`, which
        // nothing ever writes, so `--type type` was a guaranteed zero-hit on an
        // index that held the aliases. Both spellings stay listed so the mapping
        // survives a future extractor that picks the longer name.
        "type" | "type_alias" => vec!["type", "type_alias"],
        "const" | "constant" => vec!["constant"],
        // No extractor emits `variable`: a top-level `export var`/`let`/`var`
        // binding is stored as `constant` (treesitter.rs, "config constant"), so
        // `var` resolved to a type with zero rows in every index. Kept in the
        // vocabulary — the binding IS indexed — and pointed at what holds it.
        "var" | "variable" => vec!["variable", "constant"],
        _ => vec![],
    }
}

// -- Edge resolution noise filter --
// Common standard-library method/trait names that produce false-positive call edges
// when resolved cross-file by name alone (without type context).
// These are skipped for cross-file `calls` edge creation.
pub const CROSS_FILE_CALL_NOISE: &[&str] = &[
    "new",
    "default",
    "from",
    "into",
    "as_str",
    "to_string",
    "clone",
    "fmt",
    "display",
    "drop",
    "try_from",
    "try_into",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "deref",
    "deref_mut",
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "hash",
    "serialize",
    "deserialize",
    "next",
    "iter",
    "into_iter",
    "build",
    "builder",
    "len",
    "is_empty",
    "unwrap",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "expect",
    "ok",
    "err",
    "map",
    "map_err",
    "and_then",
    "or_else",
    "filter",
    "flatten",
    "push",
    "pop",
    "insert",
    "remove",
    "contains",
    "get",
    "to_owned",
    "to_vec",
    "collect",
    "join",
    "flush",
    "close",
    "read",
    "write",
];

// Names that live in CROSS_FILE_CALL_NOISE because they are Rust/collection
// stdlib methods (`Vec::insert`, `HashMap::remove`, `slice::contains`) but are
// NOT core-ECMAScript builtin instance methods — Arrays use `splice`, Maps use
// `has`, and there is no `Array/Object/String.insert`. In a JS/TS codebase these
// are ordinary user-defined methods (`db.insert(x)`, `cache.remove(k)`,
// `set.contains(v)`), so applying the Rust-flavored drop to them silently lost
// legitimate `calls` edges — reporting live methods as dead code and hiding
// their callers from impact/callers. Exempted for the JS family ONLY; genuine
// ECMAScript builtins still in the noise set (`push`/`pop`/`get`/`map`/`filter`/
// `join`/`read`/`write`...) stay dropped because the receiver type is unknown.
pub const JS_CALL_NOISE_EXEMPT: &[&str] = &["insert", "remove", "contains"];

/// Whether a cross-file `calls` target name should be dropped as stdlib noise
/// for a given source language.
///
/// [`CROSS_FILE_CALL_NOISE`] is a Rust/collection-stdlib list and fits languages
/// whose receivers expose method-style builtins under these exact (lowercase)
/// names — Rust, Python (`list.insert`/`dict.get`), Ruby (`Array#push/#insert`),
/// Java (`List.get`/`StringBuilder.insert`), Kotlin, Swift, C++ (`vector::insert`).
/// Two families diverge:
///   - **PHP**: `$o->method()` calls have NO stdlib-builtin-method collisions —
///     PHP's array/collection ops are global functions (`array_push`, `count`,
///     `in_array`), never methods, and SPL interface methods are user-implemented.
///     The list would only ever drop legitimate user-method edges, so it is not
///     applied (false-positive dead code otherwise).
///   - **JS/TS**: keeps the genuine ECMAScript builtins (`push`/`pop`/`get`/`map`
///     /`filter`...) but exempts the non-ECMAScript names in
///     [`JS_CALL_NOISE_EXEMPT`] (`insert`/`remove`/`contains`).
pub fn is_cross_file_call_noise(name: &str, language: &str) -> bool {
    match language {
        "php" => false,
        "javascript" | "typescript" | "tsx" => {
            !JS_CALL_NOISE_EXEMPT.contains(&name) && CROSS_FILE_CALL_NOISE.contains(&name)
        }
        _ => CROSS_FILE_CALL_NOISE.contains(&name),
    }
}

// -- Python type-annotation noise filter --
// Builtin types + `typing` generics that appear in annotation positions but
// resolve to the stdlib, not to a project symbol. Emitting `references` edges to
// them is pure noise (they'd inflate find_references / suppress dead-code on
// names like `List`/`Optional`). Mirrors CROSS_FILE_CALL_NOISE's role for calls,
// but is Python-type-specific. Kept case-sensitive: only the exact stdlib spellings.
pub const PYTHON_TYPE_REFERENCE_NOISE: &[&str] = &[
    // builtins
    "str",
    "int",
    "float",
    "bool",
    "bytes",
    "None",
    "object",
    "list",
    "dict",
    "set",
    "tuple",
    "frozenset",
    "complex",
    "type",
    // typing generics / special forms
    "Any",
    "List",
    "Dict",
    "Set",
    "Tuple",
    "FrozenSet",
    "Optional",
    "Union",
    "Callable",
    "Sequence",
    "Iterable",
    "Iterator",
    "Mapping",
    "MutableMapping",
    "Type",
    "ClassVar",
    "Final",
    "Literal",
    "Annotated",
    "NoReturn",
    "Self",
];

// -- Python framework-registered / attribute-accessed decorators --
// Methods/functions carrying these decorators are invoked DYNAMICALLY — the
// framework or language runtime dispatches them, so they never carry an incoming
// static `calls` edge even when fully live (pydantic validators resolve to
// `caller_count: 0`; a `@property` is read as `obj.x`, not called by name). That
// makes them edgeless by nature, the same guaranteed-false-positive class as
// constructors and dunder methods — reporting them as dead code invites deleting
// live code. `find_dead_code` excludes any Python function/method whose stored
// `code_content` contains one of these as an `@`-anchored substring. The decorator
// text is available because the parser binds Python symbols to the enclosing
// `decorated_definition` wrapper (issue #31, INDEX_VERSION 36), and decorators sit
// at the head of `code_content` (never lost to tail truncation). The `@` anchor
// prevents matching a longer identifier (`@field_validator` ⊄ `@my_field_validator`).
// Bias is deliberately toward false-negatives (a genuinely-dead decorated symbol
// may be missed) — the safe direction for an LLM-facing "candidates" tool.
pub const PYTHON_FRAMEWORK_DECORATORS: &[&str] = &[
    // pydantic v2: validators/serializers/computed fields registered on the model.
    "@field_validator",
    "@model_validator",
    "@field_serializer",
    "@model_serializer",
    "@computed_field",
    // pydantic v1
    "@validator",
    "@root_validator",
    // pytest fixtures — injected by name into test signatures, not called.
    "@pytest.fixture",
    "@fixture",
    // property-style: accessed as an attribute (`obj.x`) → no call edge emitted.
    "@property",
    "@cached_property",
    "@functools.cached_property",
    // abstract / typing.overload stubs: dispatched via a concrete override or
    // resolved at type-check time; the stub itself carries no incoming call edge.
    "@abstractmethod",
    "@overload",
    // web/UI framework handlers registered by the framework at import time.
    "@ui.refreshable",
    "@ui.page",
];

// -- Go type-position noise filter --
// UNLIKE TypeScript (where primitives are a distinct `predefined_type` kind),
// tree-sitter-go parses builtin type names (`int`, `string`, `error`, ...) as
// `type_identifier` — the SAME kind as project types. So a builtin in type
// position (`var x int`, the `string` key of `map[string]T`, `func() error`)
// would otherwise emit a `references` edge to the builtin, inflating
// find_references and suppressing dead-code on a name like `error`/`any`. This
// set lists the Go predeclared type identifiers so they can be skipped. Builtin
// FUNCTIONS (`len`, `make`, `append`, ...) and constants (`true`, `nil`) are not
// `type_identifier`, so they never reach the type-reference extractor and are
// intentionally omitted. Kept case-sensitive: only the exact predeclared
// spellings.
pub const GO_TYPE_REFERENCE_NOISE: &[&str] = &[
    "bool",
    "string",
    "error",
    "any",
    "rune",
    "byte",
    "uintptr",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "comparable",
];

// -- Java type-position noise filter --
// Java type names in type position are `type_identifier` (UNLIKE primitives —
// `int`/`long`/`double`/`boolean`/`void`/... parse as distinct
// `integral_type`/`floating_point_type`/`boolean_type`/`void_type` kinds and
// never reach the references extractor). The common JDK reference types below
// ARE `type_identifier`, so without filtering they would emit `references` edges
// to symbols that resolve to the JDK, not a project node. They drop at cross-file
// resolution anyway (no project node exists), but skipping at extraction keeps
// the edge set clean and avoids mis-binding if a project coincidentally defines a
// same-named type. This is a MODERATE set of the very common ones (java.lang
// auto-imports, common java.util collections, common annotations), NOT an attempt
// to enumerate all of java.* . Kept case-sensitive: only the exact JDK spellings.
pub const JAVA_TYPE_REFERENCE_NOISE: &[&str] = &[
    // java.lang (auto-imported)
    "String",
    "Object",
    "Integer",
    "Long",
    "Double",
    "Float",
    "Boolean",
    "Character",
    "Byte",
    "Short",
    "Number",
    "Void",
    "Class",
    "Exception",
    "RuntimeException",
    "Throwable",
    "Error",
    "Comparable",
    "Runnable",
    "Thread",
    "Iterable",
    // common annotations (java.lang / java.lang.annotation)
    "Override",
    "Deprecated",
    "SuppressWarnings",
    // common java.util collections + utilities
    "List",
    "ArrayList",
    "LinkedList",
    "Map",
    "HashMap",
    "TreeMap",
    "LinkedHashMap",
    "Set",
    "HashSet",
    "TreeSet",
    "Collection",
    "Optional",
    "Iterator",
    // java.util.stream
    "Stream",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_method_matches_wildcards_and_case() {
        // ANY (Flask no-methods) and ALL (Go net/http) satisfy every verb.
        assert!(route_method_matches("ANY", "GET"));
        assert!(route_method_matches("ANY", "DELETE"));
        assert!(route_method_matches("ALL", "POST"));
        // Exact match is case-insensitive on BOTH sides — the MCP surface had
        // drifted into case-sensitive `==` before this predicate unified them.
        assert!(route_method_matches("GET", "get"));
        assert!(route_method_matches("get", "GET"));
        assert!(route_method_matches("any", "PATCH"));
        // A real mismatch still filters.
        assert!(!route_method_matches("POST", "GET"));
        assert!(!route_method_matches("GET", "DELETE"));
    }

    #[test]
    fn test_max_file_size_default() {
        // Without env var set, should return the default 1 MB
        assert_eq!(max_file_size(), 1_048_576);
    }

    #[test]
    fn test_max_code_content_len_default() {
        assert_eq!(max_code_content_len(), 4096);
    }

    #[test]
    fn test_parse_timeout_ms_default() {
        assert_eq!(parse_timeout_ms(), 5000);
    }

    /// Pin the dead-code ignore defaults: `criterion_group!`-named bench fns
    /// and `claude-plugin/` shell-hook scripts are unreachable from the static
    /// AST graph and would otherwise dominate orphan results.
    #[test]
    fn test_default_dead_code_ignores_includes_macro_invoked_dirs() {
        let ignores = default_dead_code_ignores();
        assert!(
            ignores.iter().any(|p| p == "benches/"),
            "benches/ must be ignored — Criterion's criterion_group!() args aren't reference-tracked, so bench fns appear orphan"
        );
        assert!(
            ignores.iter().any(|p| p == "claude-plugin/"),
            "claude-plugin/ must be ignored — hook handlers are invoked from settings.json shell, not JS imports"
        );
    }

    /// The shared enum normalizers accept case variants (parity with
    /// normalize_confidence / normalize_type_filter / canonical_language) and
    /// each rejects the OTHER direction vocabulary + bogus values.
    #[test]
    fn test_enum_normalizers_case_insensitive_and_vocab_scoped() {
        // call direction: callers|callees|both
        assert_eq!(normalize_call_direction("both"), Some("both"));
        assert_eq!(normalize_call_direction("BOTH"), Some("both"));
        assert_eq!(normalize_call_direction("Callers"), Some("callers"));
        assert_eq!(
            normalize_call_direction("outgoing"),
            None,
            "deps vocab rejected"
        );
        assert_eq!(normalize_call_direction("bogus"), None);
        // dep direction: outgoing|incoming|both
        assert_eq!(normalize_dep_direction("INCOMING"), Some("incoming"));
        assert_eq!(normalize_dep_direction("both"), Some("both"));
        assert_eq!(
            normalize_dep_direction("callers"),
            None,
            "callgraph vocab rejected"
        );
        // relation: RELATION_FILTER_VOCAB + all
        assert_eq!(normalize_relation("CALLS"), Some("calls"));
        assert_eq!(normalize_relation("Implements"), Some("implements"));
        assert_eq!(normalize_relation("all"), Some("all"));
        assert_eq!(normalize_relation("bogus"), None);
        // Previously rejected despite being real edge types the graph returns.
        assert_eq!(normalize_relation("exports"), Some("exports"));
        assert_eq!(normalize_relation("routes_to"), Some("routes_to"));
    }

    /// P2 (2026-08-16 audit §四): the `--relation` / `relation` filter vocabulary
    /// must equal the set of edge types the graph can RETURN. It did not —
    /// `exports` and `routes_to` appeared in `--relation all` output and were
    /// refused by name, so a user could see an edge kind and not filter for it.
    ///
    /// Guarding it against the `REL_*` constants makes the next new edge type
    /// fail here instead of shipping a filter that silently omits it.
    #[test]
    fn relation_filter_vocab_covers_every_edge_type() {
        let every_edge_type = [
            REL_CALLS,
            REL_INHERITS,
            REL_IMPORTS,
            REL_ROUTES_TO,
            REL_IMPLEMENTS,
            REL_EXPORTS,
            REL_REFERENCES,
        ];
        for rel in every_edge_type {
            assert!(
                RELATION_FILTER_VOCAB.contains(&rel),
                "edge type '{rel}' can appear in results but cannot be filtered for"
            );
            assert_eq!(
                normalize_relation(rel),
                Some(rel),
                "'{rel}' must round-trip through the normalizer"
            );
        }
        assert_eq!(
            RELATION_FILTER_VOCAB.len(),
            every_edge_type.len(),
            "the filter must not offer a relation the graph never emits"
        );
        // `all` is the one accepted value that is NOT an edge type.
        assert!(!RELATION_FILTER_VOCAB.contains(&"all"));
    }

    /// Every node type the extractor writes into `nodes.type`.
    ///
    /// Regenerate with:
    /// `grep -rhoE '(make_simple_node|node_type:)\s*\(?\s*"[a-z_0-9]+"' src/parser/ | grep -oE '"[a-z_]+"'`
    /// plus the `Some("…") =>` arms in `treesitter.rs::classify_*`.
    /// The `--type` vocabulary must only offer words that land here — accepting a
    /// word the extractor never emits is a guaranteed zero-hit dressed up as a
    /// supported filter.
    const EXTRACTOR_NODE_TYPES: &[&str] = &[
        "function",
        "method",
        "class",
        "struct",
        "enum",
        "interface",
        "trait",
        "type",
        "constant",
        "module",
        "external_module",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
    ];

    /// P2 (2026-08-16 audit §四), reopened 2026-08-17: the first pass pinned
    /// "every advertised word PARSES", which `module`, `type` and `var` all did
    /// while resolving to node types no row ever carries (`module` is dropped by
    /// `is_skippable_result` / the dead-code SQL; nothing writes `type_alias` or
    /// `variable`). Acceptance was mistaken for support. The assertion now walks
    /// through to the extractor's own vocabulary.
    #[test]
    fn type_filter_help_matches_vocab() {
        assert_eq!(TYPE_FILTER_HELP, TYPE_FILTER_VOCAB.join(", "));
        assert!(
            TYPE_FILTER_HELP_ARG.ends_with(TYPE_FILTER_HELP),
            "the clap-attribute copy must carry the same list: {TYPE_FILTER_HELP_ARG}"
        );
        for word in TYPE_FILTER_VOCAB {
            let mapped = normalize_type_filter(word);
            assert!(
                !mapped.is_empty(),
                "'{word}' is advertised in help but rejected by the parser"
            );
            // …and at least one target must be a type the extractor emits.
            assert!(
                mapped.iter().any(|t| EXTRACTOR_NODE_TYPES.contains(t)),
                "'{word}' maps to {mapped:?}, none of which the extractor ever writes — \
                 the filter is advertised but can never match a row"
            );
        }
        // `module` is rejected on purpose: the type exists but every surface
        // taking this filter excludes the placeholder rows, so the answer is a
        // pointer to `map`/`overview`/`tour`, not a zero-hit.
        assert!(normalize_type_filter("module").is_empty());
        assert!(!TYPE_FILTER_VOCAB.contains(&"module"));
        assert!(
            type_filter_note("module").contains("overview"),
            "rejecting `module` must say where modules ARE listed"
        );
        // Negative control: the list must not be an accept-everything, and an
        // ordinary typo must not pick up the module pointer.
        assert!(normalize_type_filter("bogus").is_empty());
        assert_eq!(type_filter_note("bogus"), "");
    }

    /// The `const` copy used where an expression is not allowed (clap `help =`,
    /// `print_help`) must equal the derived list. Six hand-written copies of this
    /// vocabulary had drifted; this is what keeps the last one honest.
    #[test]
    fn relation_filter_help_matches_vocab() {
        assert_eq!(RELATION_FILTER_HELP, relation_filter_vocab_list());
    }

    /// `is_test_symbol` must classify Criterion bench files as harness so
    /// `bench_*` callers don't leak into impact-analysis production caller_count
    /// (e.g. `bench_fts5_search` was inflating `fts5_search`'s prod caller count).
    #[test]
    fn test_is_test_symbol_classifies_benches_as_harness() {
        assert!(is_test_symbol("bench_fts5_search", "benches/indexing.rs"));
        assert!(is_test_symbol("bench_call_graph", "benches/indexing.rs"));
        assert!(is_test_symbol("anything", "bench/foo.rs"));
        // Production code in src/ is unaffected
        assert!(!is_test_symbol(
            "fts5_search",
            "src/storage/queries/search.rs"
        ));
        assert!(!is_test_symbol("conn", "src/storage/db.rs"));
    }

    /// `is_test_node_sql` (the node-level SQL test filter used by dead-code /
    /// surprising) MUST agree with `is_test_symbol` for every (name, path) — the two
    /// are the "same predicate, two languages" and drift silently. Runs the emitted
    /// GLOB against in-memory SQLite so this is the real matcher, not a re-transcribed
    /// mirror. The near-miss negatives (`Test_helper`, `mytest`, `latest`,
    /// `src/mytests.rs`) specifically pin the GLOB (case-sensitive, `_` literal) vs
    /// LIKE (case-insensitive, `_` wildcard) distinction — a LIKE port flips them.
    #[test]
    fn test_is_test_node_sql_matches_rust() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!(
            "SELECT {} FROM (SELECT ?1 AS name, 0 AS is_test) n, (SELECT ?2 AS path) f",
            is_test_node_sql("n", "f")
        );
        let cases = [
            // Positives — one per leg of is_test_symbol / is_test_path.
            ("test_signup", "tests/test_api.py"),
            ("test_foo", "src/lib.rs"),    // test_ name leg
            ("MyTest", "src/lib.rs"),      // *Test
            ("SuiteTests", "src/lib.rs"),  // *Tests
            ("run", "tests/foo.rs"),       // tests/
            ("run", "test/foo.rs"),        // test/
            ("run", "benches/b.rs"),       // benches/
            ("run", "bench/b.rs"),         // bench/
            ("run", "src/__tests__/x.ts"), // __tests__/
            ("run", "src/foo/tests.rs"),   // /tests.rs
            ("run", "pkg/foo_test.go"),    // _test.go
            ("run", "src/mod_test.rs"),    // _test.rs
            ("run", "src/a.test.ts"),      // .test.ts
            ("run", "src/a.test.tsx"),     // .test.tsx
            ("run", "src/a.spec.jsx"),     // .spec.jsx
            // xUnit / JVM / pytest layouts (issue #36).
            ("run", "src/Tests/Api.Tests/AuthTests.cs"), // Tests/ segment, any case
            ("run", "src/test/java/com/x/AuthHandler.java"), // Maven src/test/
            ("run", "app/Domain/AuthServiceTests.cs"),   // *Tests.cs stem
            ("run", "app/Domain/AuthServiceTest.java"),  // *Test.java stem
            ("run", "app/routes/RouteSpec.scala"),       // *Spec.scala stem
            ("run", "app/routes/RouteSpec.kt"),          // *Spec.kt (Kotest)
            ("run", "pkg/util_test.py"),                 // _test.py
            ("run", "lib/widget_test.dart"),             // _test.dart
            ("run", "api/test_signup.py"),               // pytest test_*.py
            ("run", "api/conftest.py"),                  // pytest conftest
            // Negatives — production symbols…
            ("handle_signup", "src/api.py"),
            ("format_greeting", "src/models.py"),
            // …and near-misses a LIKE port would wrongly flag.
            ("Test_helper", "src/lib.rs"), // capital T ≠ test_ (case-sensitive)
            ("mytest", "src/lib.rs"),      // lowercase ≠ *Test
            ("latest", "src/lib.rs"),      // ends 'test' not 'Test'
            ("run", "src/mytests.rs"),     // no '/' before tests.rs
            ("run", "src/attests.py"),     // 'test' substring, no path leg
            // …and near-misses the widened legs must NOT swallow.
            ("run", "src/latest.cs"),       // lowercase stem ≠ *Test
            ("run", "src/Contest.cs"),      // 'test' inside, wrong case
            ("run", "src/Testimonial.cs"),  // starts with Test, doesn't end with it
            ("run", "src/protest/api.cs"),  // segment contains 'test', isn't 'test'
            ("run", "src/testing/api.cs"),  // 'testing' ≠ 'test'/'tests'
            ("run", "src/latest_test.txt"), // .txt not in INFIX_TEST_EXTS
            ("run", "src/attest.py"),       // no `test_` prefix / conftest
            // …and the pytest legs' case sensitivity. SQLite GLOB is
            // case-sensitive while LIKE is not, so a Rust side that lower-cased
            // these disagreed with its own SQL mirror on exactly these shapes —
            // `affected` called them tests, dead-code/search called them prod.
            // pytest agrees with the strict reading: `fnmatch_ex` does not
            // normcase and conftest is found by literal basename, so neither of
            // these is ever collected.
            ("run", "api/Test_Signup.py"), // capital T ≠ test_ prefix
            ("run", "api/Conftest.py"),    // capital C ≠ conftest.py
            ("run", "api/sub/Test_x.py"),  // same, via the `/test_` leg
            // …and the `Spec` stem OUTSIDE the ScalaTest/Kotest world, where it
            // is an ordinary production noun rather than a suite name.
            ("run", "src/Contracts/OpenApiSpec.cs"),
            ("run", "src/Protocol/WireSpec.java"),
            ("run", "src/Api/OpenApiSpec.php"),
            ("run", "src/Model/FieldSpec.swift"),
        ];
        for (name, path) in cases {
            let got: i64 = conn
                .query_row(&sql, rusqlite::params![name, path], |r| r.get(0))
                .unwrap();
            assert_eq!(
                got != 0,
                is_test_symbol(name, path),
                "is_test_node_sql disagrees with is_test_symbol for ({name:?}, {path:?})"
            );
        }
        // Stored-flag leg: is_test=1 classifies as test even when the heuristic misses.
        let flag_sql = format!(
            "SELECT {} FROM (SELECT 'plain_fn' AS name, 1 AS is_test) n, (SELECT 'src/a.rs' AS path) f",
            is_test_node_sql("n", "f")
        );
        let got: i64 = conn.query_row(&flag_sql, [], |r| r.get(0)).unwrap();
        assert!(
            got != 0,
            "is_test flag=1 must classify as test even when name/path heuristic misses"
        );
    }

    /// `prod_source_filter_and` / `test_source_filter_or` are a fifth copy of the
    /// test-path rule and were the only one with no mechanical guard: the
    /// Rust↔SQL differential above covers `is_test_node_sql`, and
    /// `tests/predicate_parity.rs` covers the JS and both Python mirrors.
    ///
    /// The invariant asserted here is ONE-DIRECTIONAL, deliberately. The filter
    /// is still narrower than `is_test_symbol` on the NAME half (no
    /// `*Test`/`*Tests` symbol suffix), so requiring agreement would assert
    /// something the code does not claim. Its PATH half no longer diverges —
    /// `path_half_agrees_with_is_test_path` below pins that direction — because
    /// the gap was measurable: 792 `calls` edges in this repo were production to
    /// this filter and test to every node-level surface. What must never happen is
    /// the other direction: a path Rust calls PRODUCTION being excluded as a
    /// test, because that drops it from every prod-caller count with no error
    /// raised anywhere. `'%_test.%'` did exactly that — LIKE's `_` is a
    /// single-character wildcard, so it swallowed `latest.cs` (`l`,`a`,`test`,`.`)
    /// and `attest.py`, two entries this corpus already carried as near-misses.
    #[test]
    fn prod_source_filter_never_excludes_a_production_path() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!(
            "SELECT ({prod}), ({test}) \
             FROM (SELECT ?1 AS name, 0 AS is_test) src, (SELECT ?2 AS path) sf",
            prod = prod_source_filter_and(),
            test = test_source_filter_or(),
        );
        let mut stmt = conn.prepare(&sql).unwrap();

        // Names cover the case boundary the LIKE spelling got wrong: SQLite's
        // LIKE is ASCII-case-insensitive, `is_test_symbol` is `starts_with`.
        let names = [
            "run",         // plainly production
            "test_signup", // genuine test_ prefix
            "Test_Signup", // production per is_test_symbol; LIKE excluded it
            "TEST_MODE",   // ditto
            "testify",     // no underscore — production on both sides
        ];

        let mut wrongly_excluded = Vec::new();
        for path in TEST_PATH_PARITY_CORPUS {
            for name in names {
                let (prod, is_test): (i64, i64) = stmt
                    .query_row(rusqlite::params![name, path], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .unwrap();
                assert_ne!(
                    prod, is_test,
                    "{name} @ {path}: the two filters must stay exact inverses; \
                     got prod={prod} test={is_test}"
                );
                if !is_test_symbol(name, path) && prod == 0 {
                    wrongly_excluded.push(format!("{name} @ {path}"));
                }
            }
        }
        assert!(
            wrongly_excluded.is_empty(),
            "these rows are PRODUCTION per `is_test_symbol` but the SQL source \
             filter excluded them as tests — they vanish from every prod-caller \
             count with nothing reported: {wrongly_excluded:?}"
        );
    }

    /// The PATH half of the edge-level filter must agree with `is_test_path`
    /// exactly, in both directions.
    ///
    /// It did not until this batch: the filter carried only the anchored
    /// `tests/%` prefix and the infix leg, so an xUnit (`src/Tests/Api/…`), Maven
    /// (`src/test/java/…`) or JS (`foo.test.js`) layout was a test to
    /// `dead_code`/`affected` and production to `hot_functions`. Both halves now
    /// build from `test_path_legs_sql`, so this test fails the moment one surface
    /// grows a leg the other lacks.
    ///
    /// Name is held constant at a production spelling so only the path decides —
    /// the NAME half is still deliberately narrower and is not asserted here.
    #[test]
    fn path_half_agrees_with_is_test_path() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!(
            "SELECT ({test}) \
             FROM (SELECT 'runQuery' AS name, 0 AS is_test) src, (SELECT ?1 AS path) sf",
            test = test_source_filter_or(),
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let mut mismatches = Vec::new();
        for path in TEST_PATH_PARITY_CORPUS {
            let sql_says: i64 = stmt.query_row([path], |r| r.get(0)).unwrap();
            let rust_says = is_test_path(path);
            if (sql_says != 0) != rust_says {
                mismatches.push(format!("{path}: sql={sql_says} rust={rust_says}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "edge-level path legs disagree with is_test_path — one surface counts \
             these files' symbols as production callers while another calls them \
             tests: {mismatches:?}"
        );
    }

    #[test]
    fn test_is_skippable_result_covers_the_triad() {
        // <module> placeholder, <external> stub, and test symbols are skipped on
        // every search/similarity surface.
        assert!(is_skippable_result(false, "module", "<module>", "src/a.rs"));
        assert!(is_skippable_result(
            false,
            "function",
            "anything",
            "<external>"
        ));
        assert!(is_skippable_result(
            false, "function", "test_foo", "src/a.rs"
        ));
        assert!(is_skippable_result(false, "function", "foo", "tests/a.rs"));
        // Real production symbols and real (named) modules are kept.
        assert!(!is_skippable_result(
            false, "function", "realFn", "src/a.rs"
        ));
        assert!(!is_skippable_result(false, "module", "my_mod", "src/a.rs"));

        // The authoritative flag alone is enough (2026-08-16 audit §四): an inline
        // `#[cfg(test)] fn compute_expected_layout()` in a `src/` file matches no
        // name or path heuristic, so before this the SQL channel filtered it and
        // the Rust post-filter did not — one query, two definitions of "test".
        assert!(
            is_skippable_result(true, "function", "compute_expected_layout", "src/a.rs"),
            "nodes.is_test must be sufficient on its own"
        );
        // …and it must not swallow production symbols when the flag is clear.
        assert!(!is_skippable_result(
            false,
            "function",
            "compute_expected_layout",
            "src/a.rs"
        ));
    }

    #[test]
    fn test_is_dead_code_exported_covers_all_legs() {
        // Explicit export edge.
        assert!(is_dead_code_exported(
            true,
            "fn hidden() {}",
            "src/a.rs",
            "hidden"
        ));
        // Rust `pub` / `pub(crate)` visibility from the code content.
        assert!(is_dead_code_exported(
            false,
            "pub fn f() {}",
            "src/a.rs",
            "f"
        ));
        assert!(is_dead_code_exported(
            false,
            "pub(crate) fn f() {}",
            "src/a.rs",
            "f"
        ));
        // Go: an uppercase identifier in a .go file is exported. This is the leg the
        // CLI JSON path used to drop — guard it on every surface now.
        assert!(is_dead_code_exported(
            false,
            "func Handler() {}",
            "pkg/h.go",
            "Handler"
        ));
        // Go lowercase = unexported → orphan; non-Go uppercase is not Go-export.
        assert!(!is_dead_code_exported(
            false,
            "func handler() {}",
            "pkg/h.go",
            "handler"
        ));
        assert!(!is_dead_code_exported(
            false,
            "fn Helper() {}",
            "src/a.rs",
            "Helper"
        ));
        // Plain private function with no callers = orphan.
        assert!(!is_dead_code_exported(
            false,
            "fn helper() {}",
            "src/a.rs",
            "helper"
        ));
    }

    /// Rust convention: `mod tests;` resolves to `<module>/tests.rs`. Functions
    /// inside (including #[test]-free helpers like `open_with_meta_table`) must
    /// classify as test callers — otherwise `find_references` / `called_by`
    /// silently treats them as production. Symptom: `get_ast_node(snapshot::create,
    /// include_references)` listed 6 src/snapshot/tests.rs entries as prod callers
    /// while `impact.test_callers_filtered` (SQL-side, AST-flag-driven) counted
    /// them as tests — the two heuristics disagreed.
    #[test]
    fn test_is_test_symbol_classifies_rust_module_tests_rs() {
        assert!(is_test_symbol(
            "create_writes_meta",
            "src/snapshot/tests.rs"
        ));
        assert!(is_test_symbol(
            "open_with_meta_table",
            "src/snapshot/tests.rs"
        ));
        assert!(is_test_symbol("anything", "src/indexer/pipeline/tests.rs"));
        // Guard against false positives: substring must be the final segment.
        assert!(!is_test_symbol("fts5_search", "src/contests.rs"));
        assert!(!is_test_symbol("normal_fn", "src/tests_helpers.rs"));
    }

    #[test]
    fn is_test_path_classifies_by_path_only() {
        // Path-based positives (no symbol name needed).
        assert!(is_test_path("tests/foo.rs"));
        assert!(is_test_path("src/auth.test.ts"));
        assert!(is_test_path("src/Button.spec.tsx"));
        assert!(is_test_path("src/Button.spec.jsx"));
        assert!(is_test_path("pkg/handler_test.go"));
        assert!(is_test_path("a/__tests__/x.js"));
        // pytest positives — the exact spellings pytest collects.
        assert!(is_test_path("api/test_signup.py"));
        assert!(is_test_path("api/sub/test_signup.py"));
        assert!(is_test_path("api/conftest.py"));
        // Negatives.
        assert!(!is_test_path("src/auth.ts"));
        assert!(!is_test_path("src/main.rs"));
        // The pytest legs are case-SENSITIVE. pytest matches `python_files` with
        // `fnmatch_ex` (no normcase) and discovers conftest by literal basename,
        // so these are production modules it never collects. Lower-casing them
        // also silently disagreed with the case-sensitive GLOB in
        // `is_test_node_sql` — see `test_is_test_node_sql_matches_rust`.
        assert!(!is_test_path("api/Test_Signup.py"));
        assert!(!is_test_path("api/sub/Test_Signup.py"));
        assert!(!is_test_path("api/Conftest.py"));
        // The DIRECTORY leg stays case-insensitive (xUnit `src/Tests/…`, issue #36).
        assert!(is_test_path("src/Tests/Api/Thing.cs"));
        assert!(is_test_path("src/Test/Api/Thing.cs"));
        // `Spec` is a suite name in ScalaTest/Kotest…
        assert!(is_test_path("app/routes/RouteSpec.scala"));
        assert!(is_test_path("app/routes/RouteSpec.kt"));
        // …and an ordinary production noun everywhere else. Classifying these as
        // tests removed their symbols from `search` (is_skippable_result) and
        // listed them as "test file(s) to re-run" in `affected`.
        assert!(!is_test_path("src/Contracts/OpenApiSpec.cs"));
        assert!(!is_test_path("src/Protocol/WireSpec.java"));
        assert!(!is_test_path("src/Api/OpenApiSpec.php"));
        assert!(!is_test_path("src/Model/FieldSpec.swift"));
        // The `Test`/`Tests` stems keep the full ecosystem list.
        assert!(is_test_path("app/Domain/AuthServiceTests.cs"));
        assert!(is_test_path("app/Domain/AuthServiceTest.java"));
        // is_test_symbol still honors the name heuristic on a non-test path.
        assert!(is_test_symbol("test_login", "src/auth.rs"));
        assert!(!is_test_symbol("login", "src/auth.rs"));
    }

    #[test]
    fn is_test_node_trusts_flag_then_heuristic() {
        // The AST flag catches the heuristic-invisible inline unit test
        // (descriptive snake_case name, src/ path) — the v0.79.1 audit case.
        assert!(is_test_node(
            true,
            "two_node_cycle_is_detected",
            "src/graph/cycles.rs"
        ));
        // Flag off + heuristic off ⇒ production.
        assert!(!is_test_node(
            false,
            "two_node_cycle_is_detected",
            "src/graph/cycles.rs"
        ));
        // Heuristic still classifies when the flag is absent (legacy / unprojected rows).
        assert!(is_test_node(false, "test_login", "src/auth.rs"));
        assert!(is_test_node(false, "anything", "tests/integration.rs"));
        // Genuine production caller stays production under both signals.
        assert!(!is_test_node(false, "real_caller", "src/lib.rs"));
    }

    #[test]
    fn test_is_function_node_type() {
        assert!(is_function_node_type("function"));
        assert!(is_function_node_type("method"));
        assert!(!is_function_node_type("constant"));
        assert!(!is_function_node_type("struct"));
        assert!(!is_function_node_type("enum"));
        assert!(!is_function_node_type("trait"));
        assert!(!is_function_node_type("interface"));
        assert!(!is_function_node_type("type_alias"));
        assert!(!is_function_node_type("module"));
        assert!(!is_function_node_type(""));
    }

    #[test]
    fn test_rel_references_constant() {
        assert_eq!(crate::domain::REL_REFERENCES, "references");
    }

    #[test]
    fn test_search_fetch_count_unfiltered_matches_historical() {
        // Unfiltered MUST stay byte-identical to the old inline `(top_k*4).max(20)`
        // so the retrieval benchmark (which passes no language/node_type filter)
        // is unchanged. Any drift here is a metric regression, not a refactor.
        assert_eq!(search_fetch_count(20, false), 80);
        assert_eq!(search_fetch_count(100, false), 400);
        assert_eq!(search_fetch_count(1, false), 20); // floor
        assert_eq!(search_fetch_count(3, false), 20); // floor
    }

    #[test]
    fn test_search_fetch_count_filtered_widens_pool() {
        // A selective language/node_type filter is applied AFTER the KNN fetch, so the
        // pool must be wider than the unfiltered case or the filter starves top_k.
        assert!(search_fetch_count(20, true) > search_fetch_count(20, false));
        assert_eq!(search_fetch_count(20, true), 320);
        assert_eq!(search_fetch_count(1, true), 100); // floor
    }

    #[test]
    fn test_search_retry_fetch_count_widens_and_is_capped() {
        // Only reached when a pool came back FULL and the post-fetch filters
        // still left top_k unfilled, so it must actually widen …
        assert_eq!(search_retry_fetch_count(20), 80);
        assert_eq!(search_retry_fetch_count(80), 320);
        // … and stay bounded: an all-noise query must not turn into a scan.
        assert_eq!(search_retry_fetch_count(400), SEARCH_RETRY_FETCH_CAP);
        assert_eq!(search_retry_fetch_count(1600), SEARCH_RETRY_FETCH_CAP);
        // At the cap the caller sees no widening and must not retry forever.
        assert_eq!(
            search_retry_fetch_count(SEARCH_RETRY_FETCH_CAP),
            SEARCH_RETRY_FETCH_CAP
        );
    }

    #[test]
    fn test_similar_fetch_count_overfetches() {
        // `similar` post-filters self + max_distance + test/module; the old `top_k + 1`
        // fell short on any single drop. Must be a multiple of top_k (MCP-twin parity).
        assert_eq!(similar_fetch_count(10), 30);
        assert_eq!(similar_fetch_count(5), 15);
        assert_eq!(similar_fetch_count(1), 3); // max(3, 2)
        assert!(similar_fetch_count(10) > 10 + 1);
    }
}
