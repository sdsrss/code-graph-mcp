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
    "invariance_check",
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

// -- Index version --
// Bump this when parser/indexer logic changes in a way that produces different
// nodes or edges for the same source files. The server will detect a mismatch
// and automatically clear + rebuild the index.
// This is separate from SCHEMA_VERSION (which tracks table structure changes).
// Vector-only invalidation/refresh (e.g. delete_node_vectors_batch on a
// model=None incremental path) does NOT bump this — only node/edge/FTS output
// changes do; vectors regenerate via the NULL-vector background-embed convention.
pub const INDEX_VERSION: i32 = 30; // v30: Dart fixes — (a) top-level functions (`int helper() {}`) are now extracted as symbols (parsed as a bare function_signature sibling under `program`, never matched before so callgraph/impact/dead-code were blind to them); (b) calls now dispatch on the `selector(argument_part)` node (callee = preceding sibling) instead of only `expression_statement`, so calls in return / assignment / argument / binary-expression positions resolve (were silently dropped — only bare `foo();` statements worked); v29 also: Express routes_to with an IMPORTED named handler (`import {getUser} from './ctrl'; app.get('/x', getUser)`) now resolves the handler cross-file (was matched only against the route file's own nodes → route silently dropped for the most common Express layout; inline + same-file handlers already worked); v29: cross-file call-noise filter is now language-aware — JS/TS `obj.insert()`/`remove()`/`contains()` resolve (not ECMAScript builtins) while genuine builtins (push/pop/get/map/filter...) still drop; PHP `$o->method()` calls are fully exempt (PHP array ops are global functions, not methods, so the Rust-collection list only produced false-positive dead code). Was reporting live JS/TS/PHP methods as dead code + hiding callers; v28: Ruby bare (parens-less) method calls in statement position now produce calls edges via a scope-aware pass that excludes local variables (Ruby's own assigned-vs-call rule), closing a recall gap where `helper` (no parens) was dropped; v27: Python + Ruby top-level (module/class-body) calls now attribute to `<module>` too (same fix as bash v26) so an entry-point function called only at top level isn't reported dead; v26: bash top-level command invocations now attribute to `<module>` (were dropped) so an entry-point function called only at script top level (`run_app "$@"`) is no longer reported dead; external commands still drop at Phase-2 resolution; v25: Flask @app.route(..., methods=['GET']) now derives the HTTP verb from the methods= kwarg (was always "ANY", breaking method-scoped trace); v24: PHP file-include imports (require/require_once/include/include_once → REL_IMPORTS to the bare file stem)

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
/// Below this match_confidence, surface a "results are largely vector noise" warning.
pub const CONF_WARNING_THRESHOLD: f64 = 0.5;
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
pub fn compute_risk_level(prod_callers: usize, affected_routes: usize, is_breaking: bool) -> &'static str {
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
        || name.ends_with("Test") || name.ends_with("Tests")
        || is_test_path(file_path)
}

/// True for a search/similarity candidate that every result surface skips as
/// non-real output: a file-level `<module>` placeholder, an `<external>` stub,
/// or a test symbol. Single source for the triad otherwise reimplemented in
/// `cmd_search`/`tool_semantic_search` and `cmd_similar`/`tool_find_similar_code`
/// across the CLI and MCP surfaces (a recurring drift site — the CLI search/
/// similar paths historically omitted the `<external>` leg the MCP path applied).
pub fn is_skippable_result(node_type: &str, node_name: &str, file_path: &str) -> bool {
    (node_type == "module" && node_name == "<module>")
        || file_path == "<external>"
        || is_test_symbol(node_name, file_path)
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
        || (file_path.ends_with(".go")
            && name.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// File-level test classifier (path heuristics only) shared by `is_test_symbol` and
/// the `affected` command. NOT the only test-path matcher: the SQL counterparts
/// (`PROD_SOURCE_FILTER_AND` / `TEST_SOURCE_FILTER_OR` below) and the local closure in
/// `indexer::pipeline::resolve::refine_ambiguous_targets` use their own, intentionally
/// divergent patterns. See the "Five sites must agree" note below and
/// feedback_test_classifier_dual_sources.md before changing any one of them.
pub fn is_test_path(file_path: &str) -> bool {
    file_path.starts_with("tests/") || file_path.starts_with("test/")
        || file_path.starts_with("benches/") || file_path.starts_with("bench/")
        || file_path.contains("__tests__/")
        || file_path.ends_with("/tests.rs")
        || file_path.ends_with("_test.go") || file_path.ends_with("_test.rs")
        || file_path.ends_with(".test.ts") || file_path.ends_with(".test.js")
        || file_path.ends_with(".test.tsx") || file_path.ends_with(".test.jsx")
        || file_path.ends_with(".spec.ts") || file_path.ends_with(".spec.js")
        || file_path.ends_with(".spec.tsx") || file_path.ends_with(".spec.jsx")
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
/// Pair with [`PROD_SOURCE_FILTER_AND`] in the WHERE clause.
pub fn prod_source_join_sql(edges_alias: &str) -> String {
    format!(
        "JOIN nodes src ON src.id = {e}.source_id \
         JOIN files sf ON sf.id = src.file_id",
        e = edges_alias,
    )
}

/// AND-joined conditions that exclude test/bench source rows.
/// Combines the AST-level `src.is_test=0` flag with name and path heuristics —
/// kept in sync with `is_test_symbol`. Caller is expected to splice these
/// inside a WHERE clause already started with another condition (no leading AND
/// is added by callers — they prepend ` AND ` themselves) or inside a CASE WHEN.
pub const PROD_SOURCE_FILTER_AND: &str =
    "src.is_test = 0 \
     AND src.name NOT LIKE 'test\\_%' ESCAPE '\\' \
     AND sf.path NOT LIKE 'tests/%' \
     AND sf.path NOT LIKE 'benches/%' \
     AND sf.path NOT LIKE '%_test.%' \
     AND sf.path NOT LIKE '%/tests.rs'";

/// OR-joined inverse of [`PROD_SOURCE_FILTER_AND`] — matches test/bench sources.
/// Used by SUM/CASE constructs that count test callers separately (e.g.
/// project_map's hot_functions test_cnt CASE).
pub const TEST_SOURCE_FILTER_OR: &str =
    "src.is_test = 1 \
     OR src.name LIKE 'test\\_%' ESCAPE '\\' \
     OR sf.path LIKE 'tests/%' \
     OR sf.path LIKE 'benches/%' \
     OR sf.path LIKE '%_test.%' \
     OR sf.path LIKE '%/tests.rs'";

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
        "type" | "type_alias" => vec!["type_alias"],
        "const" | "constant" => vec!["constant"],
        "var" | "variable" => vec!["variable"],
        "module" => vec!["module"],
        _ => vec![],
    }
}

// -- Edge resolution noise filter --
// Common standard-library method/trait names that produce false-positive call edges
// when resolved cross-file by name alone (without type context).
// These are skipped for cross-file `calls` edge creation.
pub const CROSS_FILE_CALL_NOISE: &[&str] = &[
    "new", "default", "from", "into", "as_str", "to_string", "clone",
    "fmt", "display", "drop", "try_from", "try_into",
    "as_ref", "as_mut", "borrow", "borrow_mut", "deref", "deref_mut",
    "eq", "ne", "cmp", "partial_cmp", "hash",
    "serialize", "deserialize",
    "next", "iter", "into_iter",
    "build", "builder",
    "len", "is_empty",
    "unwrap", "unwrap_or", "unwrap_or_else", "unwrap_or_default",
    "expect", "ok", "err", "map", "map_err", "and_then",
    "or_else", "filter", "flatten",
    "push", "pop", "insert", "remove", "contains", "get",
    "to_owned", "to_vec", "collect", "join",
    "flush", "close", "read", "write",
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
    "str", "int", "float", "bool", "bytes", "None", "object",
    "list", "dict", "set", "tuple", "frozenset", "complex", "type",
    // typing generics / special forms
    "Any", "List", "Dict", "Set", "Tuple", "FrozenSet", "Optional", "Union",
    "Callable", "Sequence", "Iterable", "Iterator", "Mapping", "MutableMapping",
    "Type", "ClassVar", "Final", "Literal", "Annotated", "NoReturn", "Self",
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
    "bool", "string", "error", "any", "rune", "byte", "uintptr",
    "int", "int8", "int16", "int32", "int64",
    "uint", "uint8", "uint16", "uint32", "uint64",
    "float32", "float64", "complex64", "complex128",
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
    "String", "Object", "Integer", "Long", "Double", "Float", "Boolean",
    "Character", "Byte", "Short", "Number", "Void", "Class",
    "Exception", "RuntimeException", "Throwable", "Error",
    "Comparable", "Runnable", "Thread", "Iterable",
    // common annotations (java.lang / java.lang.annotation)
    "Override", "Deprecated", "SuppressWarnings",
    // common java.util collections + utilities
    "List", "ArrayList", "LinkedList",
    "Map", "HashMap", "TreeMap", "LinkedHashMap",
    "Set", "HashSet", "TreeSet",
    "Collection", "Optional", "Iterator",
    // java.util.stream
    "Stream",
];

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `is_test_symbol` must classify Criterion bench files as harness so
    /// `bench_*` callers don't leak into impact-analysis production caller_count
    /// (e.g. `bench_fts5_search` was inflating `fts5_search`'s prod caller count).
    #[test]
    fn test_is_test_symbol_classifies_benches_as_harness() {
        assert!(is_test_symbol("bench_fts5_search", "benches/indexing.rs"));
        assert!(is_test_symbol("bench_call_graph", "benches/indexing.rs"));
        assert!(is_test_symbol("anything", "bench/foo.rs"));
        // Production code in src/ is unaffected
        assert!(!is_test_symbol("fts5_search", "src/storage/queries/search.rs"));
        assert!(!is_test_symbol("conn", "src/storage/db.rs"));
    }

    #[test]
    fn test_is_skippable_result_covers_the_triad() {
        // <module> placeholder, <external> stub, and test symbols are skipped on
        // every search/similarity surface.
        assert!(is_skippable_result("module", "<module>", "src/a.rs"));
        assert!(is_skippable_result("function", "anything", "<external>"));
        assert!(is_skippable_result("function", "test_foo", "src/a.rs"));
        assert!(is_skippable_result("function", "foo", "tests/a.rs"));
        // Real production symbols and real (named) modules are kept.
        assert!(!is_skippable_result("function", "realFn", "src/a.rs"));
        assert!(!is_skippable_result("module", "my_mod", "src/a.rs"));
    }

    #[test]
    fn test_is_dead_code_exported_covers_all_legs() {
        // Explicit export edge.
        assert!(is_dead_code_exported(true, "fn hidden() {}", "src/a.rs", "hidden"));
        // Rust `pub` / `pub(crate)` visibility from the code content.
        assert!(is_dead_code_exported(false, "pub fn f() {}", "src/a.rs", "f"));
        assert!(is_dead_code_exported(false, "pub(crate) fn f() {}", "src/a.rs", "f"));
        // Go: an uppercase identifier in a .go file is exported. This is the leg the
        // CLI JSON path used to drop — guard it on every surface now.
        assert!(is_dead_code_exported(false, "func Handler() {}", "pkg/h.go", "Handler"));
        // Go lowercase = unexported → orphan; non-Go uppercase is not Go-export.
        assert!(!is_dead_code_exported(false, "func handler() {}", "pkg/h.go", "handler"));
        assert!(!is_dead_code_exported(false, "fn Helper() {}", "src/a.rs", "Helper"));
        // Plain private function with no callers = orphan.
        assert!(!is_dead_code_exported(false, "fn helper() {}", "src/a.rs", "helper"));
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
        assert!(is_test_symbol("create_writes_meta", "src/snapshot/tests.rs"));
        assert!(is_test_symbol("open_with_meta_table", "src/snapshot/tests.rs"));
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
        // Negatives.
        assert!(!is_test_path("src/auth.ts"));
        assert!(!is_test_path("src/main.rs"));
        // is_test_symbol still honors the name heuristic on a non-test path.
        assert!(is_test_symbol("test_login", "src/auth.rs"));
        assert!(!is_test_symbol("login", "src/auth.rs"));
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
    fn test_similar_fetch_count_overfetches() {
        // `similar` post-filters self + max_distance + test/module; the old `top_k + 1`
        // fell short on any single drop. Must be a multiple of top_k (MCP-twin parity).
        assert_eq!(similar_fetch_count(10), 30);
        assert_eq!(similar_fetch_count(5), 15);
        assert_eq!(similar_fetch_count(1), 3); // max(3, 2)
        assert!(similar_fetch_count(10) > 10 + 1);
    }
}
