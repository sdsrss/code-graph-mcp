use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::domain::CODE_GRAPH_DIR;
use crate::storage::db::Database;
use crate::storage::queries;

/// Resolve the project root from an explicit `cwd`.
///
/// Priority:
/// 1. Existing `.code-graph/index.db` at `cwd` → use `cwd` (respects explicit per-dir indexes).
/// 2. Nearest ancestor containing `.git` → use that (avoids polluting subdirs).
/// 3. Fall back to `cwd`.
pub fn resolve_project_root_from(cwd: &Path) -> PathBuf {
    if cwd.join(CODE_GRAPH_DIR).join("index.db").exists() {
        return cwd.to_path_buf();
    }
    let mut cursor: Option<&Path> = Some(cwd);
    while let Some(c) = cursor {
        if c.join(".git").exists() {
            return c.to_path_buf();
        }
        cursor = c.parent();
    }
    cwd.to_path_buf()
}

/// Resolve the project root from the current working directory.
pub fn resolve_project_root() -> std::io::Result<PathBuf> {
    Ok(resolve_project_root_from(&std::env::current_dir()?))
}

/// Remove empty legacy database files left behind from past naming migrations.
/// Pre-v0.5 iterations briefly used `code-graph.db`, `code_graph.db`, `graph.db`
/// before settling on `index.db`; the renames never deleted the old 0-byte stubs.
pub fn cleanup_legacy_db_files(code_graph_dir: &Path) {
    const LEGACY: &[&str] = &["code-graph.db", "code_graph.db", "graph.db"];
    for name in LEGACY {
        let p = code_graph_dir.join(name);
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.len() == 0 {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Lightweight CLI context for subcommands called by hooks.
/// Does NOT load the embedding model (too slow for 5-10s hook timeouts).
pub struct CliContext {
    pub db: Database,
    pub project_root: PathBuf,
}

impl CliContext {
    pub fn open(project_root: &Path) -> Result<Self> {
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        if !db_path.exists() {
            anyhow::bail!(
                "No index found at {}. Run: code-graph-mcp incremental-index",
                db_path.display()
            );
        }
        cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));
        let db = Database::open(&db_path)?;
        Ok(Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Try to open, returning None if no index exists (for grep fallback).
    pub fn try_open(project_root: &Path) -> Option<Self> {
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        if !db_path.exists() {
            return None;
        }
        cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));
        Database::open(&db_path).ok().map(|db| Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }
}

// --- Argument helpers ---

/// Flags that take a value argument (not boolean).
const VALUE_FLAGS: &[&str] = &["--limit", "--type", "--returns", "--params", "--direction", "--depth", "--format", "--file", "--language", "--change-type", "--top-k", "--max-distance", "--node-type", "--node-id", "--context-lines", "--relation", "--min-lines"];

fn get_positional(args: &[String], index: usize) -> Option<&str> {
    let mut pos = 0;
    let mut i = 2; // skip binary name and subcommand
    while i < args.len() {
        if args[i].starts_with("--") {
            // Skip the flag itself
            if VALUE_FLAGS.contains(&args[i].as_str()) {
                i += 2; // skip flag + its value
            } else {
                i += 1; // boolean flag, skip just the flag
            }
            continue;
        }
        // Skip single-dash flags (e.g., -h, -V)
        if args[i].starts_with('-') && args[i].len() > 1 {
            i += 1;
            continue;
        }
        if pos == index {
            return Some(&args[i]);
        }
        pos += 1;
        i += 1;
    }
    None
}

fn get_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Collect all repeated values for a flag (e.g. `--ignore a --ignore b` → ["a","b"]).
fn collect_flag_values(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Get a flag value that represents a file path, normalizing `./` prefix.
fn get_path_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    get_flag_value(args, flag).map(|p| p.strip_prefix("./").unwrap_or(p))
}

/// Parse a numeric flag value, printing a warning on invalid input and falling back to default.
fn parse_flag_or<T: std::str::FromStr + std::fmt::Display>(args: &[String], flag: &str, default: T) -> T {
    match get_flag_value(args, flag) {
        Some(v) => match v.parse::<T>() {
            Ok(val) => val,
            Err(_) => {
                eprintln!("[code-graph] Warning: invalid value '{}' for {}, using default {}", v, flag, default);
                default
            }
        },
        None => default,
    }
}

/// Strip qualified name prefix (e.g. "McpServer.handle_message" -> "handle_message")
/// so users can copy-paste names from output and use them in lookups.
fn strip_qualified_prefix(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// CLI-side fuzzy name resolution. Mirrors MCP server's `resolve_fuzzy_name` so
/// CLI `callgraph`/`refs` auto-promote a unique fuzzy match to the exact name
/// instead of just printing "Did you mean" and bailing out.
pub(crate) enum CliFuzzyResolution {
    Unique(String),
    Ambiguous(Vec<queries::NameCandidate>),
    NotFound,
}

fn resolve_fuzzy_name_cli(conn: &rusqlite::Connection, name: &str) -> Result<CliFuzzyResolution> {
    let candidates: Vec<_> = queries::find_functions_by_fuzzy_name(conn, name)?
        .into_iter()
        .filter(|c| !crate::domain::is_test_symbol(&c.name, &c.file_path))
        .collect();
    let exact: Vec<_> = candidates.iter().filter(|c| c.name == name).cloned().collect();
    if exact.len() == 1 {
        return Ok(CliFuzzyResolution::Unique(exact[0].name.clone()));
    }
    if exact.len() > 1 {
        return Ok(CliFuzzyResolution::Ambiguous(exact));
    }
    if candidates.len() == 1 {
        return Ok(CliFuzzyResolution::Unique(candidates.into_iter().next().unwrap().name));
    }
    if !candidates.is_empty() {
        return Ok(CliFuzzyResolution::Ambiguous(candidates));
    }
    Ok(CliFuzzyResolution::NotFound)
}

/// Detect exact-name ambiguity: ≥2 non-test definitions of `name` in distinct files.
/// Returns suggestion candidates when ambiguous; None for unique-match or not-found
/// (downstream commands emit their own not-found messages). Mirrors the disambiguate
/// guard in MCP `tool_get_call_graph` so bare-name CLI queries like `impact open`
/// don't silently merge callers of two distinct `open` functions.
fn detect_exact_ambiguity(
    conn: &rusqlite::Connection,
    name: &str,
) -> Result<Option<Vec<queries::NameCandidate>>> {
    let with_files = queries::get_nodes_with_files_by_name(conn, name)?;
    let non_test: Vec<queries::NameCandidate> = with_files.iter()
        .filter(|nf| !crate::domain::is_test_symbol(&nf.node.name, &nf.file_path))
        .map(|nf| queries::NameCandidate {
            name: nf.node.name.clone(),
            file_path: nf.file_path.clone(),
            node_type: nf.node.node_type.clone(),
            node_id: nf.node.id,
            start_line: nf.node.start_line,
        })
        .collect();
    // Treat multiple definitions in the *same* file (e.g. overloads) as non-ambiguous;
    // distinct files are the user-actionable case (needs --file/--node-id).
    let distinct_files: std::collections::HashSet<&str> =
        non_test.iter().map(|c| c.file_path.as_str()).collect();
    if distinct_files.len() > 1 {
        return Ok(Some(non_test));
    }
    Ok(None)
}

/// Emit the "ambiguous symbol" error in the same shape whether the command was
/// invoked with --json (one-line JSON) or default (human-readable stderr lines),
/// then exit(1). Shared by cmd_callgraph, cmd_impact when file_filter is None
/// and detect_exact_ambiguity returned candidates.
fn emit_exact_ambiguity(symbol: &str, cands: &[queries::NameCandidate], json_mode: bool) -> ! {
    if json_mode {
        let sugg: Vec<serde_json::Value> = cands.iter().take(5).map(|c| serde_json::json!({
            "name": c.name,
            "file_path": c.file_path,
            "type": c.node_type,
            "node_id": c.node_id,
            "start_line": c.start_line,
        })).collect();
        println!("{}", serde_json::json!({
            "error": format!("Ambiguous symbol '{}': {} matches in different files. Specify --file or --node-id to disambiguate.", symbol, cands.len()),
            "suggestions": sugg,
        }));
    } else {
        eprintln!("[code-graph] Ambiguous symbol '{}': {} matches in different files. Specify --file or --node-id:", symbol, cands.len());
        for c in cands.iter().take(5) {
            eprintln!("  {} ({}) in {} [node_id {}]", c.name, c.node_type, c.file_path, c.node_id);
        }
    }
    std::process::exit(1);
}

/// Resolve a possibly-qualified symbol name (e.g. "Database.open") to a base name
/// and optional file path for disambiguation. When the user passes a qualified name,
/// we find the matching node and use its file_path as a filter so that downstream
/// queries (callgraph, impact, refs) pick the right symbol.
/// Returns (base_name, resolved_file_filter) where resolved_file_filter is Some only
/// if the qualified name resolved uniquely and no explicit --file was given.
fn resolve_qualified_symbol<'a>(
    conn: &rusqlite::Connection,
    raw_symbol: &'a str,
    explicit_file: Option<&'a str>,
) -> (&'a str, Option<String>) {
    // If user already provided --file, just strip the prefix and use their filter
    if explicit_file.is_some() {
        return (strip_qualified_prefix(raw_symbol), None);
    }
    // If the symbol contains '.', try qualified name resolution
    if raw_symbol.contains('.') {
        let base = strip_qualified_prefix(raw_symbol);
        if let Ok(nodes) = queries::get_nodes_by_name(conn, base) {
            let matched: Vec<_> = nodes
                .iter()
                .filter(|n| n.qualified_name.as_deref() == Some(raw_symbol))
                .collect();
            if matched.len() == 1 {
                if let Ok(Some(fp)) = queries::get_file_path(conn, matched[0].file_id) {
                    return (base, Some(fp));
                }
            }
        }
        return (base, None);
    }
    (raw_symbol, None)
}

// --- Output formatting ---

/// Format a node as a compact single line: `type QualifiedName  file:start-end  (params) -> return`
fn format_node_compact(node: &queries::NodeResult, file_path: &str) -> String {
    let mut out = String::with_capacity(128);
    // type prefix
    let short_type = match node.node_type.as_str() {
        "function" => "fn",
        "method" => "fn",
        "class" => "class",
        "struct" => "struct",
        "interface" => "iface",
        "trait" => "trait",
        "enum" => "enum",
        "type_alias" => "type",
        "constant" => "const",
        "variable" => "var",
        other => other,
    };
    out.push_str(short_type);
    out.push(' ');

    // name (prefer qualified)
    if let Some(ref qn) = node.qualified_name {
        out.push_str(qn);
    } else {
        out.push_str(&node.name);
    }

    // location
    out.push_str("  ");
    out.push_str(file_path);
    out.push(':');
    out.push_str(&node.start_line.to_string());
    out.push('-');
    out.push_str(&node.end_line.to_string());

    // signature parts
    if let Some(ref params) = node.param_types {
        if !params.is_empty() {
            out.push_str("  (");
            out.push_str(params);
            out.push(')');
        }
    }
    if let Some(ref ret) = node.return_type {
        if !ret.is_empty() {
            out.push_str(" -> ");
            out.push_str(ret);
        }
    }
    out
}

// --- Subcommands ---

/// Run incremental index update.
/// If `quiet` is true, suppress non-error output.
/// Auto-creates the database and runs a full index if no index exists.
pub fn cmd_incremental_index(project_root: &Path, quiet: bool) -> Result<()> {
    let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
    let is_new = !db_path.exists();

    if is_new {
        // Ensure .code-graph/ directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !quiet {
            eprintln!("No index found, creating full index...");
        }
    }
    cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));

    // Open with vec support so embeddings can be stored
    let db = Database::open_with_vec(&db_path)?;

    if is_new {
        // Full index for new databases
        use crate::indexer::pipeline::run_full_index;
        let result = run_full_index(&db, project_root, None, None)?;
        if !quiet {
            eprintln!(
                "Full index: {} files, {} nodes, {} edges",
                result.files_indexed, result.nodes_created, result.edges_created
            );
        }
    } else {
        // Incremental index for existing databases
        use crate::indexer::pipeline::run_incremental_index;
        let stats = run_incremental_index(&db, project_root, None, None)?;
        if !quiet {
            eprintln!(
                "Incremental index: {} files updated, {} nodes created",
                stats.files_indexed, stats.nodes_created
            );
        }
    }

    // Embed any nodes missing vectors (runs synchronously, unlike server background thread)
    if db.vec_enabled() {
        use crate::embedding::model::EmbeddingModel;
        use crate::indexer::pipeline::embed_and_store_batch;
        if let Some(model) = EmbeddingModel::load()? {
            let mut total = 0usize;
            loop {
                let chunk = queries::get_unembedded_nodes(db.conn(), 64)?;
                if chunk.is_empty() { break; }
                embed_and_store_batch(&db, &model, &chunk)?;
                total += chunk.len();
            }
            if total > 0 && !quiet {
                let (embedded, embeddable) = queries::count_nodes_with_vectors(db.conn())?;
                eprintln!("Embedded {} nodes ({}/{})", total, embedded, embeddable);
            }
        }
    }

    Ok(())
}

/// Drop the existing index.db (plus WAL/SHM) and trigger a full rebuild via
/// `cmd_incremental_index` (which auto-detects the missing DB and does a full
/// index). Mirrors MCP `rebuild_index` tool semantics.
pub fn cmd_rebuild_index(project_root: &Path, args: &[String]) -> Result<()> {
    let confirm = args.iter().any(|a| a == "--confirm");
    let quiet = args.iter().any(|a| a == "--quiet");
    if !confirm {
        anyhow::bail!(
            "rebuild-index drops the existing index and re-parses every file. \
             Pass --confirm to proceed. Use `incremental-index` for incremental updates."
        );
    }
    // Destructive-op sanity: refuse to operate on degenerate roots. Guards against
    // a resolve_project_root regression that could return `/` or `""`.
    if project_root.as_os_str().is_empty() || project_root == Path::new("/") {
        anyhow::bail!(
            "refusing to rebuild-index with degenerate project_root ({}). \
             Run from within a git-tracked project directory.",
            project_root.display()
        );
    }
    let code_graph_dir = project_root.join(CODE_GRAPH_DIR);
    let db_path = code_graph_dir.join("index.db");
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }
    let wal = db_path.with_extension("db-wal");
    let shm = db_path.with_extension("db-shm");
    if wal.exists() { std::fs::remove_file(&wal)?; }
    if shm.exists() { std::fs::remove_file(&shm)?; }
    cmd_incremental_index(project_root, quiet)
}

/// Run health check and print status, including index freshness.
pub fn cmd_health_check(project_root: &Path, format: &str) -> Result<()> {
    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();
    let status = queries::get_index_status(conn, false)?;

    let expected_schema = crate::storage::schema::SCHEMA_VERSION;
    let schema_ok = status.schema_version == expected_schema;
    let has_data = status.nodes_count > 0 && status.files_count > 0;
    let healthy = schema_ok && has_data;

    // Compute index age from last_indexed_at (unix timestamp in seconds)
    let age_str = status.last_indexed_at.map(|ts| {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64 - ts)
            .unwrap_or(0);
        if elapsed < 60 { format!("{}s ago", elapsed) }
        else if elapsed < 3600 { format!("{}m ago", elapsed / 60) }
        else if elapsed < 86400 { format!("{}h ago", elapsed / 3600) }
        else { format!("{}d ago", elapsed / 86400) }
    });

    // Embedding coverage (works without sqlite-vec loaded)
    let (vectors_done, vectors_total) = queries::count_nodes_with_vectors(conn).unwrap_or((0, 0));
    let coverage_pct: i64 = if vectors_total > 0 {
        (vectors_done as f64 / vectors_total as f64 * 100.0).round() as i64
    } else {
        0
    };
    // Embedding model availability: compile-time feature flag proxy (runtime-cheap,
    // avoids loading weights which would violate CLI's hook-fast contract).
    // NOTE: This diverges from MCP `get_index_status` (which checks runtime
    // `embedding_model.is_some()` — true only after weights load). CLI reports
    // `model_available=true` whenever the binary was built with --features
    // embed-model, even if model weights are missing locally. Cross-check
    // `embedding_progress`/`embedding_status` to tell apart "compiled but not
    // loaded yet" from "compiled and embedding in progress".
    let model_available: bool = cfg!(feature = "embed-model");
    let search_mode = if model_available && vectors_done > 0 { "hybrid" } else { "fts_only" };
    let embedding_status = if !model_available {
        "unavailable"
    } else if vectors_done == 0 {
        "pending"
    } else if vectors_done >= vectors_total && vectors_total > 0 {
        "complete"
    } else {
        "partial"
    };

    match format {
        "json" => {
            let mut json = serde_json::json!({
                "healthy": healthy,
                "nodes": status.nodes_count,
                "edges": status.edges_count,
                "files": status.files_count,
                "watching": false,
                "schema_version": status.schema_version,
                "db_size_bytes": status.db_size_bytes,
                "search_mode": search_mode,
                "embedding_progress": format!("{}/{}", vectors_done, vectors_total),
                "embedding_coverage_pct": coverage_pct,
                "embedding_status": embedding_status,
                "model_available": model_available,
            });
            if let Some(ts) = status.last_indexed_at {
                json["last_indexed_at"] = serde_json::json!(ts);
            }
            if let Some(ref age) = age_str {
                json["index_age"] = serde_json::json!(age);
            }
            if !schema_ok {
                json["issue"] = serde_json::json!(format!(
                    "schema version mismatch: got {}, expected {}",
                    status.schema_version, expected_schema
                ));
            } else if !has_data {
                json["issue"] = serde_json::json!("index is empty");
            }
            println!("{}", json);
            if !healthy {
                std::process::exit(1);
            }
        }
        _ => {
            if healthy {
                let age_info = age_str.map(|a| format!(" (updated {})", a)).unwrap_or_default();
                println!(
                    "OK: {} nodes, {} edges, {} files{}",
                    status.nodes_count, status.edges_count, status.files_count, age_info
                );
            } else if !schema_ok {
                eprintln!(
                    "UNHEALTHY: schema version mismatch (got {}, expected {})",
                    status.schema_version, expected_schema
                );
                std::process::exit(1);
            } else {
                eprintln!("UNHEALTHY: index is empty");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// Aggregated per-tool counts across sessions.
pub struct ToolAgg {
    pub n: u64,
    pub total_ms: u64,
    pub err: u64,
    pub max_ms: u64,
}

/// Summary produced by `aggregate_usage_jsonl` — drives both human + JSON output.
pub struct UsageSummary {
    pub sessions: u64,
    pub parse_errors: u64,
    pub tools: HashMap<String, ToolAgg>,
    pub search_queries: u64,
    pub search_zero: u64,
    pub search_quality_weighted_sum: f64,
    pub search_fts_only: u64,
    pub search_hybrid: u64,
    pub full_index_count: u64,
    pub full_index_ms_sum: u64,
    pub incr_count: u64,
    pub files_indexed: u64,
    pub versions: std::collections::BTreeSet<String>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
}

impl UsageSummary {
    pub fn total_tool_calls(&self) -> u64 {
        self.tools.values().map(|a| a.n).sum()
    }
}

/// Parse and aggregate `.code-graph/usage.jsonl` content.
/// Pure function: no IO, no panics — malformed lines are counted, not fatal.
/// `last_n`: if Some, keep only the last N records before aggregating.
pub fn aggregate_usage_jsonl(content: &str, last_n: Option<usize>) -> UsageSummary {
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut parse_errors: u64 = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(v) => records.push(v),
            Err(_) => parse_errors += 1,
        }
    }
    if let Some(n) = last_n {
        if records.len() > n {
            let drop = records.len() - n;
            records.drain(..drop);
        }
    }

    let mut summary = UsageSummary {
        sessions: records.len() as u64,
        parse_errors,
        tools: HashMap::new(),
        search_queries: 0,
        search_zero: 0,
        search_quality_weighted_sum: 0.0,
        search_fts_only: 0,
        search_hybrid: 0,
        full_index_count: 0,
        full_index_ms_sum: 0,
        incr_count: 0,
        files_indexed: 0,
        versions: std::collections::BTreeSet::new(),
        first_ts: None,
        last_ts: None,
    };

    for rec in &records {
        if let Some(v) = rec.get("v").and_then(|v| v.as_str()) {
            summary.versions.insert(v.to_string());
        }
        if let Some(ts) = rec.get("ts").and_then(|v| v.as_str()) {
            if summary.first_ts.is_none() { summary.first_ts = Some(ts.to_string()); }
            summary.last_ts = Some(ts.to_string());
        }
        if let Some(tools_obj) = rec.get("tools").and_then(|v| v.as_object()) {
            for (name, s) in tools_obj {
                let agg = summary.tools.entry(name.clone()).or_insert(ToolAgg {
                    n: 0, total_ms: 0, err: 0, max_ms: 0,
                });
                agg.n += s.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.total_ms += s.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.err += s.get("err").and_then(|v| v.as_u64()).unwrap_or(0);
                let m = s.get("max_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                if m > agg.max_ms { agg.max_ms = m; }
            }
        }
        if let Some(s) = rec.get("search") {
            let q = s.get("queries").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_queries += q;
            summary.search_zero += s.get("zero").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_fts_only += s.get("fts_only").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_hybrid += s.get("hybrid").and_then(|v| v.as_u64()).unwrap_or(0);
            // Per-session avg_quality → re-weight by query count to merge.
            let avg = s.get("avg_quality").and_then(|v| v.as_f64()).unwrap_or(0.0);
            summary.search_quality_weighted_sum += avg * q as f64;
        }
        if let Some(idx) = rec.get("index") {
            if let Some(ms) = idx.get("full_ms").and_then(|v| v.as_u64()) {
                summary.full_index_count += 1;
                summary.full_index_ms_sum += ms;
            }
            summary.incr_count += idx.get("incr").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.files_indexed += idx.get("files").and_then(|v| v.as_u64()).unwrap_or(0);
        }
    }
    summary
}

/// Print aggregated session metrics from `.code-graph/usage.jsonl`.
/// Diagnostic: shows which tools you actually use + search/index activity.
/// `--last N` limits to the most recent N sessions. `--json` emits structured output.
pub fn cmd_stats(project_root: &Path, args: &[String]) -> Result<()> {
    let json_mode = has_flag(args, "--json");
    let last_n = get_flag_value(args, "--last").and_then(|s| s.parse::<usize>().ok());

    let usage_path = project_root.join(CODE_GRAPH_DIR).join("usage.jsonl");
    if !usage_path.exists() {
        if json_mode {
            println!("{}", serde_json::json!({
                "sessions": 0,
                "tools": {},
                "note": format!("no usage data at {}", usage_path.display()),
            }));
        } else {
            eprintln!("No usage data yet at {}", usage_path.display());
            eprintln!("Run an MCP session first (sessions flush metrics on EOF).");
        }
        return Ok(());
    }

    let content = std::fs::read_to_string(&usage_path)?;
    let summary = aggregate_usage_jsonl(&content, last_n);

    if summary.sessions == 0 {
        if json_mode {
            println!("{}", serde_json::json!({"sessions": 0, "tools": {}}));
        } else {
            eprintln!("No sessions recorded.");
        }
        return Ok(());
    }

    if json_mode {
        let tools_json: serde_json::Map<String, serde_json::Value> = summary.tools.iter().map(|(name, a)| {
            let avg = a.total_ms.checked_div(a.n).unwrap_or(0);
            (name.clone(), serde_json::json!({
                "n": a.n, "total_ms": a.total_ms, "avg_ms": avg, "err": a.err, "max_ms": a.max_ms,
            }))
        }).collect();
        let avg_q = if summary.search_queries > 0 {
            summary.search_quality_weighted_sum / summary.search_queries as f64
        } else { 0.0 };
        let full_avg = summary.full_index_ms_sum.checked_div(summary.full_index_count).unwrap_or(0);
        println!("{}", serde_json::json!({
            "sessions": summary.sessions,
            "parse_errors": summary.parse_errors,
            "versions": summary.versions.iter().cloned().collect::<Vec<_>>(),
            "first_ts": summary.first_ts,
            "last_ts": summary.last_ts,
            "total_tool_calls": summary.total_tool_calls(),
            "tools": tools_json,
            "search": {
                "queries": summary.search_queries,
                "zero": summary.search_zero,
                "avg_quality": (avg_q * 100.0).round() / 100.0,
                "fts_only": summary.search_fts_only,
                "hybrid": summary.search_hybrid,
            },
            "index": {
                "full_count": summary.full_index_count,
                "full_avg_ms": full_avg,
                "incr_count": summary.incr_count,
                "files_indexed": summary.files_indexed,
            },
        }));
    } else {
        let versions: Vec<&str> = summary.versions.iter().map(|s| s.as_str()).collect();
        println!("Sessions: {}   versions: {}   {} → {}",
            summary.sessions,
            if versions.is_empty() { "-".into() } else { versions.join(",") },
            summary.first_ts.as_deref().unwrap_or("-"),
            summary.last_ts.as_deref().unwrap_or("-"),
        );
        println!("Total tool calls: {}", summary.total_tool_calls());
        if summary.parse_errors > 0 {
            println!("(warning: {} malformed line(s) skipped)", summary.parse_errors);
        }
        println!();

        let mut sorted: Vec<(&String, &ToolAgg)> = summary.tools.iter().collect();
        sorted.sort_by_key(|(_, a)| std::cmp::Reverse(a.n));

        if sorted.is_empty() {
            println!("(no tool calls recorded)");
        } else {
            println!("{:<28} {:>6} {:>10} {:>6} {:>8}", "Tool", "n", "avg_ms", "err", "max_ms");
            println!("{}", "-".repeat(62));
            for (name, agg) in &sorted {
                let avg = agg.total_ms.checked_div(agg.n).unwrap_or(0);
                println!("{:<28} {:>6} {:>10} {:>6} {:>8}", name, agg.n, avg, agg.err, agg.max_ms);
            }
        }

        if summary.search_queries > 0 {
            let zero_pct = (summary.search_zero as f64 / summary.search_queries as f64 * 100.0).round() as u64;
            let avg_q = summary.search_quality_weighted_sum / summary.search_queries as f64;
            println!();
            println!("Search: {} queries, {} zero-result ({}%), hybrid/fts {}/{}, avg quality {:.2}",
                summary.search_queries, summary.search_zero, zero_pct,
                summary.search_hybrid, summary.search_fts_only, avg_q);
        }

        if summary.full_index_count > 0 || summary.incr_count > 0 {
            let full_part = match summary.full_index_ms_sum.checked_div(summary.full_index_count) {
                Some(avg) if summary.full_index_count > 0 => format!(" (avg {}ms)", avg),
                _ => String::new(),
            };
            println!("Index:  {} full{}, {} incremental, {} files indexed",
                summary.full_index_count, full_part, summary.incr_count, summary.files_indexed);
        }
    }

    Ok(())
}

/// AST-context grep: ripgrep + AST context from index.
///
/// Output format:
/// ```text
/// src/mcp/server.rs:142  let result = handle_request(params);
///   → fn McpServer::process_message (lines 130-180)
/// ```
pub fn cmd_grep(project_root: &Path, args: &[String]) -> Result<()> {
    let pattern = get_positional(args, 0)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Usage: code-graph-mcp grep <pattern> [path] [--json]"))?;

    let search_path = get_positional(args, 1);
    let json_mode = has_flag(args, "--json");

    // Run ripgrep with JSON output for structured parsing
    let mut rg_cmd = Command::new("rg");
    rg_cmd
        .arg("--json")
        .arg("-n")
        .arg("--max-count=100")
        .arg(pattern);

    if let Some(path) = search_path {
        // Validate search_path is within project root to prevent path traversal
        let resolved = project_root.join(path);
        let canonical = resolved.canonicalize().unwrap_or(resolved);
        let root_canonical = project_root.canonicalize().unwrap_or(project_root.to_path_buf());
        if !canonical.starts_with(&root_canonical) {
            anyhow::bail!("search path must be within project root");
        }
        rg_cmd.arg(canonical);
    } else {
        rg_cmd.arg(project_root);
    }

    let rg_output = rg_cmd.output();
    let rg_output = match rg_output {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("ripgrep (rg) not found. Install: https://github.com/BurntSushi/ripgrep");
        }
        Err(e) => return Err(e.into()),
    };

    // Parse rg JSON output into matches
    let matches = parse_rg_json(&rg_output.stdout, project_root);
    if matches.is_empty() {
        if json_mode {
            println!("[]");
        }
        // Surface ripgrep errors (e.g., path not found) instead of silent exit
        let stderr = String::from_utf8_lossy(&rg_output.stderr);
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            eprintln!("[code-graph] {}", stderr);
        } else {
            eprintln!("[code-graph] No matches for: {}", pattern);
        }
        return Ok(());
    }

    // Try to open index for AST context
    let ctx = CliContext::try_open(project_root);
    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let mut json_results = Vec::new();
        for m in &matches {
            let mut entry = serde_json::json!({
                "file": m.file,
                "line": m.line,
                "text": m.text,
            });
            if let Some(ref ctx) = ctx {
                if let Some(container) = find_containing_node(ctx, &m.file, m.line) {
                    entry["container"] = serde_json::json!({
                        "type": container.0,
                        "name": container.1,
                        "lines": format!("{}-{}", container.2, container.3),
                    });
                }
            }
            json_results.push(entry);
        }
        writeln!(stdout, "{}", serde_json::to_string(&json_results)?)?;
        return Ok(());
    }

    // Text output — cache nodes per file to avoid redundant DB queries
    let mut node_cache: std::collections::HashMap<&str, Vec<queries::NodeResult>> =
        std::collections::HashMap::new();
    for m in &matches {
        write!(stdout, "{}:{}  {}", m.file, m.line, m.text)?;
        if !m.text.ends_with('\n') {
            writeln!(stdout)?;
        }
        if let Some(ref ctx) = ctx {
            let nodes = node_cache.entry(&m.file).or_insert_with(|| {
                queries::get_nodes_by_file_path(ctx.db.conn(), &m.file).unwrap_or_default()
            });
            if let Some((node_type, name, start, end)) = find_containing_node_in(nodes, m.line) {
                writeln!(stdout, "  → {} {} (lines {}-{})", node_type, name, start, end)?;
            }
        }
    }

    if ctx.is_none() {
        eprintln!("[code-graph] No index found. Run: code-graph-mcp incremental-index");
        eprintln!("[code-graph] Showing plain grep results (no AST context).");
    }

    Ok(())
}

struct GrepMatch {
    file: String,
    line: u64,
    text: String,
}

/// Parse ripgrep JSON output into structured matches.
fn parse_rg_json(stdout: &[u8], project_root: &Path) -> Vec<GrepMatch> {
    let root_str = project_root.to_string_lossy().into_owned();
    let mut matches = Vec::new();
    for line in stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v["type"].as_str() != Some("match") {
            continue;
        }
        let data = &v["data"];
        let Some(path_str) = data["path"]["text"].as_str() else {
            continue;
        };
        let Some(line_number) = data["line_number"].as_u64() else {
            continue;
        };
        let text = data["lines"]["text"].as_str().unwrap_or("").to_string();

        // Make path relative to project root
        let root_prefix = root_str.trim_end_matches('/');
        let relative_path = path_str
            .strip_prefix(root_prefix)
            .or_else(|| path_str.strip_prefix(&root_str))
            .unwrap_or(path_str)
            .trim_start_matches('/');

        matches.push(GrepMatch {
            file: relative_path.to_string(),
            line: line_number,
            text,
        });
    }
    matches
}

/// Find the innermost AST node containing the given line (with DB lookup).
fn find_containing_node(
    ctx: &CliContext,
    file_path: &str,
    line: u64,
) -> Option<(String, String, i64, i64)> {
    let nodes = queries::get_nodes_by_file_path(ctx.db.conn(), file_path).ok()?;
    find_containing_node_in(&nodes, line)
}

/// Find the innermost AST node containing the given line (from pre-loaded nodes).
fn find_containing_node_in(
    nodes: &[queries::NodeResult],
    line: u64,
) -> Option<(String, String, i64, i64)> {
    let mut best: Option<&queries::NodeResult> = None;
    for node in nodes {
        if node.start_line as u64 <= line && line <= node.end_line as u64 {
            match best {
                None => best = Some(node),
                Some(prev) => {
                    let prev_span = prev.end_line - prev.start_line;
                    let cur_span = node.end_line - node.start_line;
                    if cur_span < prev_span {
                        best = Some(node);
                    }
                }
            }
        }
    }

    best.map(|n| {
        let short_type = match n.node_type.as_str() {
            "function" | "method" => "fn",
            other => other,
        };
        let name = n
            .qualified_name
            .as_deref()
            .unwrap_or(&n.name)
            .to_string();
        (short_type.to_string(), name, n.start_line, n.end_line)
    })
}

/// FTS5 semantic search.
///
/// Output format:
/// ```text
/// fn McpServer::handle_tool_call  src/mcp/server.rs:350-420  (name: &str, params: Value) -> Result<Value>
/// ```
pub fn cmd_search(project_root: &Path, args: &[String]) -> Result<()> {
    let query = get_positional(args, 0)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Usage: code-graph-mcp search <query> [--json] [--limit N] [--top-k N] [--language <lang>] [--compact]"))?;

    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");
    let language_filter = get_flag_value(args, "--language");
    let node_type_filter = get_flag_value(args, "--node-type");
    let limit: i64 = if get_flag_value(args, "--limit").is_some() {
        parse_flag_or(args, "--limit", 20_i64)
    } else {
        parse_flag_or(args, "--top-k", 20_i64)
    }.clamp(1, 100);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Fetch more results if filtering, to ensure enough after filtering
    let has_filter = language_filter.is_some() || node_type_filter.is_some();
    let fetch_limit = if has_filter { limit * 4 } else { limit };
    let fts_result = queries::fts5_search(conn, query, fetch_limit)?;
    if fts_result.nodes.is_empty() {
        if json_mode {
            println!("[]");
        }
        eprintln!("[code-graph] No results for: {}", query);
        // Hint: if query looks like code syntax, suggest ast-search
        if query.contains('(') || query.contains(')') || query.contains("->") || query.contains("::") || query.contains('<') {
            // Replace non-word chars with spaces, collapse multiple spaces, extract clean keywords
            let clean: String = query.chars()
                .map(|c| if c.is_alphanumeric() || c == '_' { c } else { ' ' })
                .collect();
            let keywords: Vec<&str> = clean.split_whitespace().collect();
            if !keywords.is_empty() {
                eprintln!("  Tip: For structural queries, try: code-graph-mcp ast-search --type fn --returns \"{}\"",
                    keywords.join(" "));
            }
        }
        return Ok(());
    }

    let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
    let nodes_with_files = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;

    // Build id->NodeWithFile map preserving FTS rank order
    let nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf))
        .collect();

    // Normalize node_type filter for matching
    let normalized_node_types: Vec<&'static str> = node_type_filter
        .map(normalize_type_filter)
        .unwrap_or_default();

    // Filter by language, node_type, and skip test/module nodes (align with MCP behavior)
    let filtered_nodes: Vec<&queries::NodeResult> = fts_result.nodes.iter()
        .filter(|n| {
            // Skip <module> nodes and test symbols (consistent with MCP semantic_code_search)
            if n.node_type == "module" && n.name == "<module>" { return false; }
            if let Some(nwf) = nwf_map.get(&n.id) {
                if crate::domain::is_test_symbol(&n.name, &nwf.file_path) { return false; }
            }
            if let Some(lang) = language_filter {
                let lang_ok = nwf_map.get(&n.id)
                    .and_then(|nwf| nwf.language.as_deref())
                    .map(|l| l.eq_ignore_ascii_case(lang))
                    .unwrap_or(false);
                if !lang_ok { return false; }
            }
            if !normalized_node_types.is_empty()
                && !normalized_node_types.iter().any(|t| n.node_type == *t)
            {
                return false;
            }
            true
        })
        .take(limit as usize)
        .collect();

    if filtered_nodes.is_empty() {
        if json_mode {
            println!("[]");
        }
        eprintln!("[code-graph] No results for: {} (language: {})", query, language_filter.unwrap_or("any"));
        return Ok(());
    }

    // Build file_path map from filtered results
    let file_map: std::collections::HashMap<i64, &str> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf.file_path.as_str()))
        .collect();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = filtered_nodes
            .iter()
            .map(|n| {
                let fp = file_map.get(&n.id).copied().unwrap_or("?");
                serde_json::json!({
                    "node_id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file_path": fp,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "signature": n.signature,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for node in &filtered_nodes {
        let fp = file_map.get(&node.id).copied().unwrap_or("?");
        if compact {
            let name = node.qualified_name.as_deref().unwrap_or(&node.name);
            writeln!(stdout, "{}  {}:{}-{}", name, fp, node.start_line, node.end_line)?;
        } else {
            writeln!(stdout, "{}", format_node_compact(node, fp))?;
        }
    }

    if fts_result.or_fallback {
        eprintln!("[code-graph] Note: AND match insufficient, showing OR results (broader match).");
    }
    if !json_mode {
        eprintln!("[code-graph] Tip: CLI search is FTS5-only. For vector+RRF hybrid recall use MCP semantic_code_search.");
    }

    Ok(())
}

/// Structured AST search: FTS5 + column filtering.
///
/// Flags: --type <type>, --returns <type>, --params <text>
pub fn cmd_ast_search(project_root: &Path, args: &[String]) -> Result<()> {
    let query = get_positional(args, 0).filter(|q| !q.is_empty());

    let type_filter = get_flag_value(args, "--type");
    let returns_filter = get_flag_value(args, "--returns");
    let params_filter = get_flag_value(args, "--params");
    let json_mode = has_flag(args, "--json");
    let limit: usize = parse_flag_or(args, "--limit", 20_usize).clamp(1, 100);

    // Require either a query or at least one structural filter
    let has_filters = type_filter.is_some() || returns_filter.is_some() || params_filter.is_some();
    if query.is_none() && !has_filters {
        anyhow::bail!(
            "Usage: code-graph-mcp ast-search <query> [--type fn|class|...] [--returns type] [--params text] [--json]\n\
             Either a query or at least one filter (--type, --returns, --params) is required."
        );
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Two paths: filter-only (direct SQL) vs query+filter (FTS5 then filter)
    let results_with_files: Vec<queries::NodeWithFile> = if let Some(query) = query {
        // FTS5 search then filter in Rust
        let fts_result = queries::fts5_search(conn, query, (limit * 4) as i64)?;
        if fts_result.nodes.is_empty() {
            if json_mode {
                println!("{}", serde_json::json!({"results": [], "count": 0}));
            }
            eprintln!("[code-graph] No results for: {}", query);
            return Ok(());
        }

        let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
        let all = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;

        // Preserve FTS5 rank order, then apply filters
        let id_order: std::collections::HashMap<i64, usize> = node_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut sorted = all;
        sorted.sort_by_key(|nwf| id_order.get(&nwf.node.id).copied().unwrap_or(usize::MAX));

        sorted
            .into_iter()
            .filter(|nwf| {
                let n = &nwf.node;
                if let Some(tf) = type_filter {
                    let normalized = normalize_type_filter(tf);
                    if !normalized.iter().any(|t| n.node_type == *t) {
                        return false;
                    }
                }
                if let Some(rf) = returns_filter {
                    match &n.return_type {
                        Some(rt) => {
                            if !rt.to_lowercase().contains(&rf.to_lowercase()) {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
                if let Some(pf) = params_filter {
                    match &n.param_types {
                        Some(pt) => {
                            if !pt.to_lowercase().contains(&pf.to_lowercase()) {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
                true
            })
            .take(limit)
            .collect()
    } else {
        // Filter-only: direct SQL query
        let normalized_types: Vec<&str>;
        let type_refs = if let Some(tf) = type_filter {
            normalized_types = normalize_type_filter(tf).into_iter().collect();
            Some(normalized_types.as_slice())
        } else {
            None
        };
        queries::get_nodes_with_files_by_filters(
            conn, type_refs, returns_filter, params_filter, limit,
        )?
    };

    if results_with_files.is_empty() {
        if json_mode {
            println!("{}", serde_json::json!({"results": [], "count": 0}));
        }
        eprintln!("[code-graph] No results matching filters.");
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = results_with_files
            .iter()
            .map(|nwf| {
                let n = &nwf.node;
                serde_json::json!({
                    "node_id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file_path": &nwf.file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        // Envelope matches MCP ast_search: {results, count}
        let envelope = serde_json::json!({
            "results": results,
            "count": results_with_files.len(),
        });
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        return Ok(());
    }

    for nwf in &results_with_files {
        writeln!(stdout, "{}", format_node_compact(&nwf.node, &nwf.file_path))?;
    }
    Ok(())
}

/// Normalize type filter shorthand: fn → function/method, class → class/struct, etc.
fn normalize_type_filter(input: &str) -> Vec<&'static str> {
    let result = crate::domain::normalize_type_filter(input);
    if result.is_empty() {
        eprintln!(
            "[code-graph] Unknown type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
            input
        );
    }
    result
}

/// Call graph display.
///
/// Output format:
/// ```text
/// handle_tool_call (src/mcp/server.rs:350)
///   ← called by: process_message (src/mcp/server.rs:130)
///   → calls: tool_semantic_search (src/mcp/server.rs:1360)
/// ```
pub fn cmd_callgraph(project_root: &Path, args: &[String]) -> Result<()> {
    let raw_symbol = get_positional(args, 0)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp callgraph <symbol> [--direction callers|callees|both] [--depth N] [--file <path>] [--json]"
        ))?;

    let direction = get_flag_value(args, "--direction").unwrap_or("both");
    let depth: i32 = parse_flag_or(args, "--depth", 3_i32).clamp(1, 20);
    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");
    let include_tests = has_flag(args, "--include-tests");
    let explicit_file = get_path_flag(args, "--file");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (symbol, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
    let file_filter = explicit_file.or(resolved_file.as_deref());

    // Exact-name ambiguity guard: bare name with ≥2 non-test definitions in different
    // files would silently merge call graphs. Prompt for --file before running query.
    if file_filter.is_none() {
        if let Some(cands) = detect_exact_ambiguity(conn, symbol)? {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
    }

    let mut nodes = crate::graph::query::get_call_graph(conn, symbol, direction, depth, file_filter)?;
    // Fuzzy auto-resolve: if exact-name lookup returned nothing (or only the seed
    // node with no edges) and no --file was specified, promote a unique fuzzy
    // match. Matches MCP get_call_graph behavior.
    let has_edges = nodes.iter().any(|n| n.depth > 0);
    let has_seed = nodes.iter().any(|n| n.depth == 0);
    let mut resolved_symbol: String = symbol.to_string();
    if !(has_edges || (has_seed && file_filter.is_some())) {
        match resolve_fuzzy_name_cli(conn, symbol)? {
            CliFuzzyResolution::Unique(resolved) => {
                resolved_symbol = resolved.clone();
                nodes = crate::graph::query::get_call_graph(conn, &resolved, direction, depth, file_filter)?;
                eprintln!("[code-graph] Resolved '{}' → '{}'", symbol, resolved);
            }
            CliFuzzyResolution::Ambiguous(cands) => {
                if json_mode {
                    let sugg: Vec<serde_json::Value> = cands.iter().take(5).map(|c| serde_json::json!({
                        "name": c.name, "file_path": c.file_path, "type": c.node_type,
                        "node_id": c.node_id, "start_line": c.start_line,
                    })).collect();
                    println!("{}", serde_json::json!({
                        "results": [],
                        "error": format!("Ambiguous symbol '{}': {} matches", symbol, cands.len()),
                        "candidates": sugg,
                    }));
                } else {
                    eprintln!("[code-graph] Ambiguous symbol '{}': {} matches. Did you mean:", symbol, cands.len());
                    for c in cands.iter().take(5) {
                        eprintln!("  {} ({}) in {} [node_id {}]", c.name, c.node_type, c.file_path, c.node_id);
                    }
                }
                std::process::exit(1);
            }
            CliFuzzyResolution::NotFound => { /* fall through to empty-nodes branch */ }
        }
    }
    // Intentional shadow: if fuzzy promoted, `resolved_symbol` holds the resolved
    // name; otherwise it still equals the original input (initialized at
    // `symbol.to_string()` above). Either way, `symbol` below is the correct
    // identifier to print in the "No call graph results" eprintln.
    let symbol = resolved_symbol.as_str();
    if nodes.is_empty() {
        if json_mode {
            println!("{{\"results\":[]}}");
        }
        eprintln!("[code-graph] No call graph results for: {}", symbol);
        std::process::exit(1);
    }

    // Filter test callers unless --include-tests is set.
    // The seed (depth=0) is kept here because the human-readable renderer
    // below uses it as the tree root. The JSON path filters it separately
    // for parity with MCP `get_call_graph` (which excludes the seed).
    let (display_nodes, test_count) = if include_tests {
        (nodes.iter().collect::<Vec<_>>(), 0usize)
    } else {
        let mut display = Vec::new();
        let mut tests = 0usize;
        for n in &nodes {
            if n.depth > 0
                && matches!(n.direction, crate::graph::query::Direction::Callers)
                && crate::domain::is_test_symbol(&n.name, &n.file_path)
            {
                tests += 1;
            } else {
                display.push(n);
            }
        }
        (display, tests)
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Drop the seed (depth=0) — parity with MCP `get_call_graph`
        // (`format_call_graph_response` filters `n.depth > 0`). With
        // `direction=both` the seed appears twice (once per direction),
        // inflating result counts.
        let results: Vec<serde_json::Value> = display_nodes
            .iter()
            .filter(|n| n.depth > 0)
            .map(|n| {
                serde_json::json!({
                    "node_id": n.node_id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": n.file_path,
                    "depth": n.depth,
                    "direction": n.direction.as_str(),
                    "parent_id": n.parent_id,
                })
            })
            .collect();
        let mut output = serde_json::json!({ "results": results });
        if test_count > 0 {
            output["test_callers_hidden"] = serde_json::json!(test_count);
        }
        writeln!(stdout, "{}", serde_json::to_string(&output)?)?;
        return Ok(());
    }

    // Find root node (depth 0)
    let root = display_nodes.iter().find(|n| n.depth == 0);
    if let Some(root) = root {
        writeln!(stdout, "{} ({})", root.name, root.file_path)?;
    } else {
        return Ok(());
    }
    let root_id = root.unwrap().node_id;

    // Build parent_id → children map per direction, so depth-N nodes nest under
    // their *actual* depth-(N-1) parent rather than visually clumping under the
    // last sibling. Same direction filter so callers/callees subtrees stay
    // separate when --direction=both.
    use std::collections::HashMap;
    let mut children: HashMap<(i64, &'static str), Vec<&crate::graph::query::CallGraphNode>> =
        HashMap::new();
    let mut dedup = std::collections::HashSet::new();
    for n in &display_nodes {
        if n.depth == 0 {
            continue;
        }
        // Dedup cfg-gated duplicates (same name+file+direction+depth, different node_id).
        if !dedup.insert((&n.name, &n.file_path, n.direction.as_str(), n.depth)) {
            continue;
        }
        let parent = n.parent_id.unwrap_or(root_id);
        children
            .entry((parent, n.direction.as_str()))
            .or_default()
            .push(n);
    }

    fn render_subtree<W: std::io::Write>(
        out: &mut W,
        children: &HashMap<(i64, &'static str), Vec<&crate::graph::query::CallGraphNode>>,
        parent_id: i64,
        direction: &'static str,
        compact: bool,
    ) -> std::io::Result<()> {
        let arrow = match direction {
            "callers" => "←",
            _ => "→",
        };
        let arrow_text = match direction {
            "callers" => "← called by",
            _ => "→ calls",
        };
        if let Some(kids) = children.get(&(parent_id, direction)) {
            for n in kids {
                let indent = "  ".repeat(n.depth as usize);
                if compact {
                    writeln!(out, "{}{} {} ({})", indent, arrow, n.name, n.file_path)?;
                } else {
                    writeln!(
                        out,
                        "{}{}: {} ({}) [{}]",
                        indent, arrow_text, n.name, n.file_path, n.node_type
                    )?;
                }
                render_subtree(out, children, n.node_id, direction, compact)?;
            }
        }
        Ok(())
    }

    render_subtree(&mut stdout, &children, root_id, "callers", compact)?;
    render_subtree(&mut stdout, &children, root_id, "callees", compact)?;

    if test_count > 0 {
        writeln!(stdout, "  ({} test callers hidden, use --include-tests to show)", test_count)?;
    }

    Ok(())
}

/// Impact analysis.
///
/// Shows callers with route info and risk level.
pub fn cmd_impact(project_root: &Path, args: &[String]) -> Result<()> {
    let raw_symbol = get_positional(args, 0)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp impact <symbol> [--depth N] [--file <path>] [--change-type signature|behavior|remove] [--json]"
        ))?;

    let depth: i32 = parse_flag_or(args, "--depth", 3_i32).clamp(1, 20);
    let json_mode = has_flag(args, "--json");
    let explicit_file = get_path_flag(args, "--file");
    let change_type = get_flag_value(args, "--change-type").unwrap_or("behavior");
    if !matches!(change_type, "signature" | "behavior" | "remove") {
        anyhow::bail!("--change-type must be one of: signature, behavior, remove");
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (symbol, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
    let file_filter = explicit_file.or(resolved_file.as_deref());

    // Verify symbol exists before running impact analysis
    let symbol_nodes = queries::get_nodes_by_name(conn, symbol)?;
    if symbol_nodes.is_empty() {
        if json_mode {
            println!("{}", serde_json::json!({"error": "Symbol not found", "symbol": symbol}));
        }
        eprintln!("[code-graph] Symbol not found: {}", symbol);
        let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
        if !candidates.is_empty() {
            eprintln!("[code-graph] Did you mean:");
            for c in candidates.iter().take(5) {
                eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
            }
        }
        std::process::exit(1);
    }

    // Exact-name ambiguity guard: bare name with ≥2 non-test definitions in different
    // files would silently merge callers across both, misreporting risk/blast radius.
    if file_filter.is_none() {
        if let Some(cands) = detect_exact_ambiguity(conn, symbol)? {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
    }

    let callers = queries::get_callers_with_route_info(conn, symbol, file_filter, depth)?;

    // Exclude root node (depth 0) — it's the queried symbol itself
    let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();

    // Separate production callers from test callers, deduplicate by (name, file, depth)
    let mut seen = std::collections::HashSet::new();
    let prod_callers: Vec<_> = callers.iter()
        .filter(|c| !crate::domain::is_test_symbol(&c.name, &c.file_path))
        .filter(|c| seen.insert((&c.name, &c.file_path, c.depth)))
        .collect();
    let test_count = callers.iter()
        .filter(|c| crate::domain::is_test_symbol(&c.name, &c.file_path))
        .count();

    // Count unique files and routes from production callers only
    let files: std::collections::HashSet<&str> = prod_callers.iter().map(|c| c.file_path.as_str()).collect();
    let routes: Vec<&&queries::CallerWithRouteInfo> = prod_callers.iter().filter(|c| c.route_info.is_some()).collect();
    let direct_callers = prod_callers.iter().filter(|c| c.depth == 1).count();

    // Call-graph-based impact only tracks function call chains. For non-function
    // symbols (constant/struct/class/enum/interface/type_alias/trait/module) with
    // zero callers the real usage (imports, field access, instantiation, type
    // annotations) is broader than the call graph. Flag risk_level=UNKNOWN so
    // downstream consumers (LLMs) don't act on a misleading LOW.
    let type_warning: Option<&'static str> = if prod_callers.is_empty() {
        let is_function_like = symbol_nodes.iter()
            .any(|n| crate::domain::is_function_node_type(n.node_type.as_str()));
        if !is_function_like {
            Some(crate::domain::NON_FUNCTION_IMPACT_WARNING)
        } else {
            None
        }
    } else {
        None
    };

    let risk: &'static str = if type_warning.is_some() {
        "UNKNOWN"
    } else {
        crate::domain::compute_risk_level(prod_callers.len(), routes.len(), change_type == "remove")
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let mut result = serde_json::json!({
            "symbol": symbol,
            "risk": risk,
            "direct_callers": direct_callers,
            "total_callers": prod_callers.len(),
            "tests_affected": test_count,
            "affected_files": files.len(),
            "affected_routes": routes.len(),
            "callers": prod_callers.iter().map(|c| serde_json::json!({
                "name": c.name,
                "type": c.node_type,
                "file": c.file_path,
                "depth": c.depth,
                "route": c.route_info,
            })).collect::<Vec<_>>(),
        });
        if let Some(warning) = type_warning {
            result["warning"] = serde_json::json!(warning);
        }
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "Impact: {} — Risk: {}", symbol, risk)?;
    if let Some(warning) = type_warning {
        writeln!(stdout, "  (warning: {})", warning)?;
    }
    writeln!(
        stdout,
        "  {} direct callers, {} total, {} files, {} routes ({} tests affected)",
        direct_callers,
        prod_callers.len(),
        files.len(),
        routes.len(),
        test_count
    )?;

    if !routes.is_empty() {
        writeln!(stdout, "Routes:")?;
        for r in &routes {
            let route_str = r.route_info.as_deref().unwrap_or("?");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(route_str) {
                let method = v["method"].as_str().unwrap_or("?");
                let path = v["path"].as_str().unwrap_or("?");
                writeln!(stdout, "  {} {} → {} ({})", method, path, r.name, r.file_path)?;
            } else {
                writeln!(stdout, "  {} → {} ({})", route_str, r.name, r.file_path)?;
            }
        }
    }

    if !prod_callers.is_empty() {
        writeln!(stdout, "Callers:")?;
        for c in &prod_callers {
            let indent = "  ".repeat(c.depth as usize);
            writeln!(stdout, "{}{}  ({}) {}", indent, c.name, c.node_type, c.file_path)?;
        }
    }

    Ok(())
}

/// Project map — aider repo-map style.
///
/// Output format:
/// ```text
/// src/mcp/server.rs (158KB, 98 symbols)
///   McpServer: handle_tool_call, process_message, flush_metrics
/// ```
pub fn cmd_map(project_root: &Path, args: &[String]) -> Result<()> {
    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (modules, deps, entry_points, hot_functions) = queries::get_project_map(conn)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Field names (`caller_count` / `test_caller_count`) and `--compact`
        // cap (top-10) match MCP `project_map`. CLI default returns top-15
        // (the DB LIMIT in get_project_map).
        let hot_cap = if compact { 10 } else { hot_functions.len() };
        let hot_json: Vec<serde_json::Value> = hot_functions.iter().take(hot_cap).map(|h| {
            let mut obj = serde_json::json!({
                "name": h.name,
                "type": h.node_type,
                "file": h.file,
                "caller_count": h.caller_count,
            });
            if h.test_caller_count > 0 {
                obj["test_caller_count"] = serde_json::json!(h.test_caller_count);
            }
            obj
        }).collect();

        let result = serde_json::json!({
            "modules": modules.iter().map(|m| serde_json::json!({
                "path": m.path,
                "files": m.files,
                "functions": m.functions,
                "classes": m.classes,
                "interfaces_traits": m.interfaces_traits,
                "languages": m.languages,
                "key_symbols": m.key_symbols,
            })).collect::<Vec<_>>(),
            "module_dependencies": deps.iter().map(|d| serde_json::json!({
                "from": d.from,
                "to": d.to,
                "imports": d.import_count,
            })).collect::<Vec<_>>(),
            "entry_points": entry_points.iter().map(|e| serde_json::json!({
                "route": e.route,
                "handler": e.handler,
                "file": e.file,
                "kind": e.kind,
            })).collect::<Vec<_>>(),
            "hot_functions": hot_json,
        });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    // Entry points
    if !entry_points.is_empty() {
        writeln!(stdout, "Entry Points:")?;
        for ep in &entry_points {
            writeln!(stdout, "  {} → {} ({})", ep.route, ep.handler, ep.file)?;
        }
        writeln!(stdout)?;
    }

    // Modules
    writeln!(stdout, "Modules:")?;
    let max_modules = if compact { 15 } else { modules.len() };
    for m in modules.iter().take(max_modules) {
        let total_symbols = m.functions + m.classes + m.interfaces_traits;
        write!(
            stdout,
            "{} ({} files, {} symbols",
            m.path, m.files, total_symbols
        )?;
        if !m.languages.is_empty() {
            write!(stdout, ", {}", m.languages.join("/"))?;
        }
        writeln!(stdout, ")")?;
        if !m.key_symbols.is_empty() {
            writeln!(stdout, "  {}", m.key_symbols.join(", "))?;
        }
    }
    if compact && modules.len() > max_modules {
        writeln!(stdout, "  ... and {} more modules", modules.len() - max_modules)?;
    }

    // Dependencies (compact: top 10)
    if !deps.is_empty() {
        writeln!(stdout)?;
        writeln!(stdout, "Dependencies:")?;
        let max_deps = if compact { 10 } else { deps.len().min(30) };
        for d in deps.iter().take(max_deps) {
            writeln!(stdout, "  {} → {} ({} imports)", d.from, d.to, d.import_count)?;
        }
    }

    // Hot functions (compact: top 5)
    if !hot_functions.is_empty() {
        writeln!(stdout)?;
        writeln!(stdout, "Hot Functions:")?;
        let max_hot = if compact { 5 } else { hot_functions.len() };
        for h in hot_functions.iter().take(max_hot) {
            if h.test_caller_count > 0 {
                writeln!(
                    stdout,
                    "  {} ({}) — {} callers + {} test ({})",
                    h.name, h.node_type, h.caller_count, h.test_caller_count, h.file
                )?;
            } else {
                writeln!(
                    stdout,
                    "  {} ({}) — {} callers ({})",
                    h.name, h.node_type, h.caller_count, h.file
                )?;
            }
        }
    }

    Ok(())
}

/// Module overview: all symbols in files under a path prefix.
pub fn cmd_overview(project_root: &Path, args: &[String]) -> Result<()> {
    let raw_path = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!("Usage: code-graph-mcp overview <path> [--json] [--compact]"))?;
    let path_prefix = raw_path.strip_prefix("./").unwrap_or(raw_path);

    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let exports = queries::get_module_exports(conn, path_prefix)?;

    // Filter out test symbols (align with MCP module_overview behavior)
    let exports: Vec<_> = exports.into_iter()
        .filter(|e| !crate::domain::is_test_symbol(&e.name, &e.file_path))
        .collect();

    if exports.is_empty() {
        if json_mode { println!("[]"); }
        anyhow::bail!("[code-graph] No symbols found under: {}", path_prefix);
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // `caller_count` matches MCP `module_overview.active_exports[].caller_count`.
        let results: Vec<serde_json::Value> = exports
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "type": e.node_type,
                    "file": e.file_path,
                    "signature": e.signature,
                    "caller_count": e.caller_count,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<&str, Vec<&queries::ModuleExport>> =
        std::collections::BTreeMap::new();
    for e in &exports {
        by_file.entry(&e.file_path).or_default().push(e);
    }

    for (file, symbols) in &by_file {
        writeln!(stdout, "{}", file)?;
        // Group by type within file
        let mut by_type: std::collections::BTreeMap<&str, Vec<&&queries::ModuleExport>> =
            std::collections::BTreeMap::new();
        for s in symbols {
            by_type.entry(&s.node_type).or_default().push(s);
        }
        for (typ, syms) in &by_type {
            let names: Vec<String> = syms
                .iter()
                .map(|s| {
                    if compact {
                        s.name.clone()
                    } else if s.caller_count > 0 {
                        format!("{} ({}×)", s.name, s.caller_count)
                    } else {
                        s.name.clone()
                    }
                })
                .collect();
            writeln!(stdout, "  {}: {}", typ, names.join(", "))?;
        }
    }

    Ok(())
}

/// Show symbol details (code, type, signature).
/// CLI equivalent of MCP `get_ast_node`.
pub fn cmd_show(project_root: &Path, args: &[String]) -> Result<()> {
    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");
    let include_refs = has_flag(args, "--include-refs") || has_flag(args, "--include-references") || has_flag(args, "--refs");
    let include_impact = has_flag(args, "--include-impact") || has_flag(args, "--impact");
    let file_filter = get_path_flag(args, "--file");
    let context_lines_explicit: Option<usize> = if get_flag_value(args, "--context-lines").is_some() {
        Some(parse_flag_or(args, "--context-lines", 0_usize))
    } else {
        None
    };
    let node_id_arg: Option<i64> = if get_flag_value(args, "--node-id").is_some() {
        Some(parse_flag_or(args, "--node-id", 0_i64))
    } else {
        None
    };
    // Default context_lines=3 when using --node-id (align with MCP behavior), 0 otherwise
    let context_lines: usize = context_lines_explicit
        .unwrap_or(if node_id_arg.is_some() { 3 } else { 0 });

    // If positional arg points at a real file on disk (has a recognized code
    // extension), nudge the user toward `overview` — `show` takes symbol names.
    if node_id_arg.is_none() {
        if let Some(arg) = get_positional(args, 0) {
            if !arg.is_empty()
                && crate::utils::config::detect_language(arg).is_some()
                && project_root.join(arg).is_file()
            {
                eprintln!(
                    "[code-graph] `{}` looks like a file path. `show` takes a symbol name (function/struct/const).",
                    arg
                );
                eprintln!(
                    "            File-level symbols: code-graph-mcp overview {}",
                    arg
                );
                eprintln!(
                    "            Full file content:  Read the file directly."
                );
                std::process::exit(1);
            }
        }
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve node(s): by --node-id, or by positional symbol name
    let nodes_with_paths: Vec<(queries::NodeResult, String)> = if let Some(nid) = node_id_arg {
        match queries::get_node_with_file_by_id(conn, nid)? {
            Some(nwf) => vec![(nwf.node, nwf.file_path)],
            None => {
                eprintln!("[code-graph] Node ID {} not found.", nid);
                std::process::exit(1);
            }
        }
    } else {
        let symbol = get_positional(args, 0)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp show <symbol> [--node-id N] [--file <path>] [--refs] [--impact] [--context-lines N] [--compact] [--json]"
            ))?;

        let nodes = if let Some(fp) = file_filter {
            queries::get_nodes_by_file_path(conn, fp)?
                .into_iter()
                .filter(|n| n.name == symbol || n.qualified_name.as_deref() == Some(symbol))
                .collect::<Vec<_>>()
        } else {
            let mut found = queries::get_nodes_by_name(conn, symbol)?;
            // Fallback: try as qualified name (e.g. "McpServer.handle_message")
            if found.is_empty() && symbol.contains('.') {
                if let Some(base_name) = symbol.rsplit('.').next() {
                    found = queries::get_nodes_by_name(conn, base_name)?
                        .into_iter()
                        .filter(|n| n.qualified_name.as_deref() == Some(symbol))
                        .collect();
                }
            }
            found
        };

        if nodes.is_empty() {
            if json_mode { println!("[]"); }
            eprintln!("[code-graph] Symbol not found: {}", symbol);
            let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
            if !candidates.is_empty() {
                eprintln!("[code-graph] Did you mean:");
                for c in candidates.iter().take(5) {
                    eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
                }
            }
            std::process::exit(1);
        }

        nodes.into_iter().map(|n| {
            let fp = queries::get_file_path(conn, n.file_id)
                .ok().flatten().unwrap_or_else(|| "?".to_string());
            (n, fp)
        }).collect()
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = nodes_with_paths.iter().map(|(node, fp)| {
            let mut obj = serde_json::json!({
                "node_id": node.id,
                "type": node.node_type,
                "name": node.qualified_name.as_deref().unwrap_or(&node.name),
                "file_path": fp,
                "start_line": node.start_line,
                "end_line": node.end_line,
                "signature": node.signature,
                "return_type": node.return_type,
                "param_types": node.param_types,
            });
            if !compact {
                if context_lines > 0 {
                    if let Some(code) = read_source_context(project_root, fp, node.start_line, node.end_line, context_lines) {
                        obj["code_content"] = serde_json::json!(code);
                    } else {
                        obj["code_content"] = serde_json::json!(node.code_content);
                    }
                } else {
                    obj["code_content"] = serde_json::json!(node.code_content);
                }
            }
            if include_refs {
                use crate::domain::REL_CALLS;
                let include_tests = has_flag(args, "--include-tests");
                let callees = queries::get_edge_targets_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
                let callers = queries::get_edge_sources_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
                obj["calls"] = serde_json::json!(callees.iter().map(|(n, f)| serde_json::json!({"name": n, "file": f})).collect::<Vec<_>>());
                let filtered_callers: Vec<_> = if include_tests {
                    callers.iter().collect()
                } else {
                    callers.iter().filter(|(n, f)| !crate::domain::is_test_symbol(n, f)).collect()
                };
                obj["called_by"] = serde_json::json!(filtered_callers.iter().map(|(n, f)| serde_json::json!({"name": n, "file": f})).collect::<Vec<_>>());
                if !include_tests {
                    let test_count = callers.len() - filtered_callers.len();
                    if test_count > 0 {
                        obj["test_callers_hidden"] = serde_json::json!(test_count);
                    }
                }
            }
            if include_impact {
                let callers = queries::get_callers_with_route_info(conn, &node.name, Some(fp.as_str()), 3).unwrap_or_default();
                let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();
                let prod: Vec<_> = callers.iter().filter(|c| !crate::domain::is_test_symbol(&c.name, &c.file_path)).collect();
                let routes = callers.iter().filter(|c| c.route_info.is_some()).count();
                let files: std::collections::HashSet<&str> = prod.iter().map(|c| c.file_path.as_str()).collect();
                let risk = crate::domain::compute_risk_level(prod.len(), routes, false);
                obj["impact"] = serde_json::json!({
                    "risk_level": risk,
                    "direct_callers": prod.iter().filter(|c| c.depth == 1).count(),
                    "transitive_callers": prod.iter().filter(|c| c.depth > 1).count(),
                    "affected_files": files.len(),
                    "affected_routes": routes,
                });
            }
            obj
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for (node, fp) in &nodes_with_paths {
        writeln!(stdout, "{}", format_node_compact(node, fp))?;
        if !compact {
            if context_lines > 0 {
                if let Some(code) = read_source_context(project_root, fp, node.start_line, node.end_line, context_lines) {
                    for line in code.lines() {
                        writeln!(stdout, "  {}", line)?;
                    }
                } else if !node.code_content.is_empty() {
                    for line in node.code_content.lines() {
                        writeln!(stdout, "  {}", line)?;
                    }
                }
            } else if !node.code_content.is_empty() {
                for line in node.code_content.lines() {
                    writeln!(stdout, "  {}", line)?;
                }
            }
        }
        if include_refs {
            use crate::domain::REL_CALLS;
            let include_tests = has_flag(args, "--include-tests");
            let callees = queries::get_edge_targets_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
            let callers = queries::get_edge_sources_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
            if !callees.is_empty() {
                writeln!(stdout, "  Calls:")?;
                for (name, file) in &callees {
                    writeln!(stdout, "    → {} ({})", name, file)?;
                }
            }
            if !callers.is_empty() {
                let mut test_count = 0usize;
                writeln!(stdout, "  Called by:")?;
                for (name, file) in &callers {
                    if !include_tests && crate::domain::is_test_symbol(name, file) {
                        test_count += 1;
                    } else {
                        writeln!(stdout, "    ← {} ({})", name, file)?;
                    }
                }
                if test_count > 0 {
                    writeln!(stdout, "    ({} test callers hidden, use --include-tests to show)", test_count)?;
                }
            }
        }
        if include_impact {
            let callers = queries::get_callers_with_route_info(conn, &node.name, Some(fp.as_str()), 3).unwrap_or_default();
            let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();
            let prod: Vec<_> = callers.iter().filter(|c| !crate::domain::is_test_symbol(&c.name, &c.file_path)).collect();
            let routes = callers.iter().filter(|c| c.route_info.is_some()).count();
            let files: std::collections::HashSet<&str> = prod.iter().map(|c| c.file_path.as_str()).collect();
            let risk = crate::domain::compute_risk_level(prod.len(), routes, false);
            writeln!(stdout, "  Impact: {} — {} direct, {} transitive, {} files, {} routes",
                risk, prod.iter().filter(|c| c.depth == 1).count(),
                prod.iter().filter(|c| c.depth > 1).count(), files.len(), routes)?;
        }
    }

    Ok(())
}

/// Read source code with context lines from the project file system.
fn read_source_context(project_root: &Path, file_path: &str, start_line: i64, end_line: i64, context_lines: usize) -> Option<String> {
    use std::io::BufRead;
    let abs_path = project_root.join(file_path);
    let canonical = abs_path.canonicalize().ok()?;
    let root_canonical = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&root_canonical) {
        return None;
    }
    let file = std::fs::File::open(&canonical).ok()?;
    let reader = std::io::BufReader::new(file);
    let start = (start_line as usize).saturating_sub(1 + context_lines);
    let end = (end_line as usize) + context_lines;
    let mut collected = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        if i >= end { break; }
        if i >= start { collected.push(line.ok()?); }
    }
    if collected.is_empty() { return None; }
    Some(collected.join("\n"))
}

/// Trace HTTP route → handler → downstream calls.
/// CLI equivalent of MCP `trace_http_chain`.
pub fn cmd_trace(project_root: &Path, args: &[String]) -> Result<()> {
    let route_path = get_positional(args, 0)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp trace <route> [--depth N] [--include-middleware] [--json]"
        ))?;

    let depth: i32 = parse_flag_or(args, "--depth", 3_i32).clamp(1, 20);
    let json_mode = has_flag(args, "--json");
    let include_middleware = !has_flag(args, "--no-middleware");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Parse method filter (e.g., "POST /api/login" → method=POST, path=/api/login)
    let (method_filter, path) = if let Some(idx) = route_path.find(' ') {
        (Some(route_path[..idx].to_uppercase()), &route_path[idx + 1..])
    } else {
        (None, route_path)
    };

    use crate::domain::REL_ROUTES_TO;
    let mut rows = queries::find_routes_by_path(conn, path, REL_ROUTES_TO)?;

    // Filter by HTTP method if specified (parse metadata JSON for accurate matching)
    if let Some(ref method) = method_filter {
        rows.retain(|r| {
            r.metadata.as_ref().is_some_and(|m| {
                serde_json::from_str::<serde_json::Value>(m).ok()
                    .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|s| s.to_string()))
                    .is_some_and(|rm| rm.eq_ignore_ascii_case(method))
            })
        });
    }

    if rows.is_empty() {
        if json_mode {
            println!("{}", serde_json::json!({"handlers": [], "message": format!("No routes matching: {}", route_path)}));
        }
        anyhow::bail!("[code-graph] No routes matching: {}", route_path);
    }

    let mut stdout = std::io::stdout().lock();

    // Batch-fetch downstream calls if middleware included
    use crate::domain::REL_CALLS;
    let downstream_map = if include_middleware {
        let node_ids: Vec<i64> = rows.iter().map(|rm| rm.node_id).collect();
        queries::get_edge_target_names_batch(conn, &node_ids, REL_CALLS)?
    } else {
        std::collections::HashMap::new()
    };

    if json_mode {
        // Single JSON object envelope matching MCP trace_http_chain shape
        let mut handlers = Vec::with_capacity(rows.len());
        for rm in &rows {
            let chain = crate::graph::query::get_call_graph(
                conn, &rm.handler_name, "callees", depth, Some(&rm.file_path),
            )?;
            let chain_nodes: Vec<serde_json::Value> = chain.iter()
                .filter(|n| n.depth > 0)
                .map(|n| serde_json::json!({
                    "name": n.name, "file_path": n.file_path, "depth": n.depth,
                }))
                .collect();
            let mut entry = serde_json::json!({
                "handler_name": rm.handler_name,
                "file_path": rm.file_path,
                "start_line": rm.start_line,
                "end_line": rm.end_line,
                "metadata": rm.metadata,
                "call_chain": chain_nodes,
            });
            if include_middleware {
                let downstream = downstream_map.get(&rm.node_id)
                    .cloned()
                    .unwrap_or_default();
                entry["downstream_calls"] = serde_json::json!(downstream);
            }
            handlers.push(entry);
        }
        let envelope = serde_json::json!({
            "route": path,
            "handlers": handlers,
        });
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        return Ok(());
    }

    for rm in &rows {
        writeln!(stdout, "{} → {} ({}:{})",
            rm.metadata.as_deref().unwrap_or(path),
            rm.handler_name, rm.file_path, rm.start_line)?;

        if include_middleware {
            if let Some(downstream) = downstream_map.get(&rm.node_id) {
                if !downstream.is_empty() {
                    writeln!(stdout, "  downstream: {}", downstream.join(", "))?;
                }
            }
        }

        // Show call chain
        let chain = crate::graph::query::get_call_graph(
            conn, &rm.handler_name, "callees", depth, Some(&rm.file_path),
        )?;
        for n in &chain {
            if n.depth == 0 { continue; }
            let indent = "  ".repeat(n.depth as usize);
            writeln!(stdout, "{}→ {} ({})", indent, n.name, n.file_path)?;
        }
    }

    Ok(())
}

/// File-level dependency graph.
/// CLI equivalent of MCP `dependency_graph`.
/// Scan a file for language-appropriate barrel / re-export / import patterns.
/// Used by `cmd_deps` as a fallback when the graph has no tracked edges for
/// a file (e.g. Rust `mod.rs` barrels that only contain `pub mod X;`).
fn scan_barrel_patterns(project_root: &Path, file_path: &str) -> Option<Vec<(usize, String)>> {
    let full = project_root.join(file_path);
    let content = std::fs::read_to_string(&full).ok()?;
    let lang = crate::utils::config::detect_language(file_path);
    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate().take(1000) {
        let t = line.trim_start();
        let matched = match lang {
            Some("rust") => {
                t.starts_with("pub mod ")
                    || t.starts_with("mod ")
                    || t.starts_with("pub use ")
                    || t.starts_with("use ")
            }
            Some("typescript") | Some("tsx") | Some("javascript") => {
                t.starts_with("import ")
                    || (t.starts_with("export ") && t.contains(" from "))
            }
            Some("python") => {
                (t.starts_with("from ") && t.contains(" import "))
                    || t.starts_with("import ")
            }
            Some("go") | Some("java") | Some("csharp") | Some("kotlin") => {
                t.starts_with("import ")
            }
            Some("ruby") => t.starts_with("require ") || t.starts_with("require_relative "),
            Some("php") => {
                t.starts_with("use ")
                    || t.starts_with("require ")
                    || t.starts_with("include ")
            }
            _ => false,
        };
        if matched {
            hits.push((idx + 1, line.to_string()));
        }
    }
    if hits.is_empty() { None } else { Some(hits) }
}

pub fn cmd_deps(project_root: &Path, args: &[String]) -> Result<()> {
    let raw_file_path = get_positional(args, 0)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp deps <file> [--direction outgoing|incoming|both] [--depth N] [--json]"
        ))?;
    let file_path = raw_file_path.strip_prefix("./").unwrap_or(raw_file_path);

    let direction = get_flag_value(args, "--direction").unwrap_or("both");
    if !matches!(direction, "outgoing" | "incoming" | "both") {
        anyhow::bail!("--direction must be one of: outgoing, incoming, both");
    }
    let depth: i32 = parse_flag_or(args, "--depth", 2_i32).clamp(1, 10);
    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let deps = queries::get_import_tree(conn, file_path, direction, depth)?;
    if deps.is_empty() {
        // Barrel / index-file fallback — scan source for re-export / import lines.
        // Rust `mod.rs` with only `pub mod X;` has no tracked edges in the graph.
        if let Some(lines) = scan_barrel_patterns(project_root, file_path) {
            let mut stdout = std::io::stdout().lock();
            if json_mode {
                let result = serde_json::json!({
                    "file": file_path,
                    "depends_on": [],
                    "depended_by": [],
                    "barrel_scan": lines.iter().map(|(ln, t)| {
                        serde_json::json!({"line": ln, "text": t.trim()})
                    }).collect::<Vec<_>>(),
                    "note": "no tracked dep edges; barrel_scan is raw re-export/import lines from file scan",
                });
                writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
            } else {
                writeln!(stdout, "{}", file_path)?;
                writeln!(stdout, "  (no tracked dep edges \u{2014} raw re-export/import lines from file scan:)")?;
                for (ln, text) in lines {
                    writeln!(stdout, "    {}: {}", ln, text.trim())?;
                }
            }
            return Ok(());
        }
        if json_mode {
            let result = serde_json::json!({
                "file": file_path,
                "depends_on": [],
                "depended_by": [],
                "error": "No tracked dependencies (not a barrel/import file)",
            });
            println!("{}", serde_json::to_string(&result)?);
        }
        anyhow::bail!(
            "[code-graph] No tracked dependencies for: {} (not a barrel/import file \u{2014} try `code-graph-mcp overview {}` or Read directly)",
            file_path,
            file_path
        );
    }

    // Filter out cross-language false edges (name-based resolution artifacts)
    // and the synthetic `<external>` bucket (unresolved imports, not a real file).
    let root_lang = crate::utils::config::detect_language(file_path);
    let is_compatible_lang = |dep_path: &str| -> bool {
        if dep_path == "<external>" { return false; }
        let dep_lang = crate::utils::config::detect_language(dep_path);
        match (root_lang, dep_lang) {
            (None, _) | (_, None) => true,
            (Some(a), Some(b)) if a == b => true,
            (Some(a), Some(b)) if matches!((a, b),
                ("javascript" | "typescript" | "tsx", "javascript" | "typescript" | "tsx")
            ) => true,
            (Some(a), Some(b)) if matches!((a, b),
                ("c" | "cpp", "c" | "cpp")
            ) => true,
            _ => false,
        }
    };

    let outgoing: Vec<&_> = deps.iter().filter(|d| d.direction == "outgoing" && is_compatible_lang(&d.file_path)).collect();
    let incoming: Vec<&_> = deps.iter().filter(|d| d.direction == "incoming" && is_compatible_lang(&d.file_path)).collect();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let result = serde_json::json!({
            "file": file_path,
            "depends_on": outgoing.iter().map(|d| {
                let mut obj = serde_json::json!({"file": d.file_path, "depth": d.depth});
                if !compact && d.depth == 1 { obj["symbols"] = serde_json::json!(d.symbol_count); }
                obj
            }).collect::<Vec<_>>(),
            "depended_by": incoming.iter().map(|d| {
                let mut obj = serde_json::json!({"file": d.file_path, "depth": d.depth});
                if !compact && d.depth == 1 { obj["symbols"] = serde_json::json!(d.symbol_count); }
                obj
            }).collect::<Vec<_>>(),
        });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "{}", file_path)?;
    if !outgoing.is_empty() {
        writeln!(stdout, "  Depends on:")?;
        for d in &outgoing {
            if compact {
                writeln!(stdout, "    {}", d.file_path)?;
            } else if d.depth == 1 {
                writeln!(stdout, "    {} ({} symbols)", d.file_path, d.symbol_count)?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }
    if !incoming.is_empty() {
        writeln!(stdout, "  Depended by:")?;
        for d in &incoming {
            if compact {
                writeln!(stdout, "    {}", d.file_path)?;
            } else if d.depth == 1 {
                writeln!(stdout, "    {} ({} symbols)", d.file_path, d.symbol_count)?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }

    Ok(())
}

/// Find semantically similar code.
/// CLI equivalent of MCP `find_similar_code`.
pub fn cmd_similar(project_root: &Path, args: &[String]) -> Result<()> {
    let top_k: i64 = parse_flag_or(args, "--top-k", 5_i64).clamp(1, 100);
    let max_distance: f64 = parse_flag_or(args, "--max-distance", 0.8_f64);
    let json_mode = has_flag(args, "--json");
    let node_id_arg: Option<i64> = if get_flag_value(args, "--node-id").is_some() {
        Some(parse_flag_or(args, "--node-id", 0_i64))
    } else {
        None
    };

    // Open with vec support for vector search
    let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
    if !db_path.exists() {
        anyhow::bail!("No index found. Run the MCP server first to create the index.");
    }
    let db = Database::open_with_vec(&db_path)?;
    let conn = db.conn();

    if !db.vec_enabled() {
        eprintln!("[code-graph] Vector search not available (sqlite-vec extension not loaded).");
        eprintln!("  To enable: build with `cargo build --release --features embed-model`.");
        eprintln!("  Alternative: use `code-graph-mcp search <query>` for text-based similarity.");
        return Ok(());
    }

    // Resolve to node_id: by --node-id or by positional symbol name. `target_label`
    // is what we display in error messages — symbol name when resolved by name,
    // "node_id N" when resolved by --node-id.
    let (node_id, target_label) = if let Some(nid) = node_id_arg {
        (nid, format!("node_id {}", nid))
    } else {
        let symbol = get_positional(args, 0)
            .filter(|s| !s.is_empty())
            .map(strip_qualified_prefix)
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp similar <symbol> [--node-id N] [--top-k N] [--max-distance N] [--json]"
            ))?;
        match queries::get_first_node_id_by_name(conn, symbol)? {
            Some(id) => (id, symbol.to_string()),
            None => {
                if json_mode { println!("[]"); }
                // All-digit positional is almost certainly a node_id mistakenly passed
                // without the flag — guide the user instead of "Symbol not found: 1010".
                if !symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_digit()) {
                    eprintln!(
                        "[code-graph] Symbol not found: {} \u{2014} did you mean `code-graph-mcp similar --node-id {}`?",
                        symbol, symbol
                    );
                } else {
                    eprintln!("[code-graph] Symbol not found: {}", symbol);
                }
                std::process::exit(1);
            }
        }
    };

    // Check embedding exists
    let (embedded_count, total_nodes) = queries::count_nodes_with_vectors(conn)?;
    if embedded_count == 0 {
        eprintln!("[code-graph] No embeddings found ({}/{} nodes embedded).", embedded_count, total_nodes);
        eprintln!("  To enable: build with `cargo build --release --features embed-model`,");
        eprintln!("  then restart the MCP server to generate embeddings.");
        eprintln!("  Alternative: use `code-graph-mcp search <query>` for text-based similarity.");
        std::process::exit(1);
    }

    let embedding: Vec<f32> = {
        let bytes = queries::get_node_embedding(conn, node_id)
            .map_err(|_| anyhow::anyhow!(
                "No embedding for {} ({}/{} nodes embedded \u{2014} embeddings still generating; try again shortly or pick a node with `--node-id` from `show {}`)",
                target_label, embedded_count, total_nodes, target_label
            ))?;
        bytemuck::cast_slice(&bytes).to_vec()
    };

    let raw_results = queries::vector_search(conn, &embedding, top_k + 1)?;

    // Collect filtered results
    let mut similar: Vec<(queries::NodeResult, String, f64)> = Vec::new();
    for (id, distance) in &raw_results {
        if *id == node_id || *distance > max_distance { continue; }
        let Some(node) = queries::get_node_by_id(conn, *id)? else { continue; };
        if node.node_type == "module" && node.name == "<module>" { continue; }
        let fp = queries::get_file_path(conn, node.file_id)?.unwrap_or_default();
        if crate::domain::is_test_symbol(&node.name, &fp) { continue; }
        similar.push((node, fp, *distance));
        if similar.len() >= top_k as usize { break; }
    }

    let mut stdout = std::io::stdout().lock();

    if similar.is_empty() {
        if json_mode {
            writeln!(stdout, "[]")?;
        }
        eprintln!("[code-graph] No similar code found for node_id: {}", node_id);
        return Ok(());
    }

    if json_mode {
        let json_results: Vec<serde_json::Value> = similar.iter().map(|(node, fp, distance)| {
            let similarity = 1.0 / (1.0 + distance);
            serde_json::json!({
                "node_id": node.id, "name": node.name, "type": node.node_type, "file_path": fp,
                "start_line": node.start_line, "similarity": (similarity * 10000.0).round() / 10000.0,
                "distance": (distance * 10000.0).round() / 10000.0,
            })
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&json_results)?)?;
        return Ok(());
    }

    for (node, fp, distance) in &similar {
        let similarity = 1.0 / (1.0 + distance);
        writeln!(stdout, "{:.1}%  {} {}  {}:{}-{}",
            similarity * 100.0,
            node.node_type, node.qualified_name.as_deref().unwrap_or(&node.name),
            fp, node.start_line, node.end_line)?;
    }

    Ok(())
}

pub fn cmd_refs(project_root: &Path, args: &[String]) -> Result<()> {
    let explicit_file = get_path_flag(args, "--file");
    let relation = get_flag_value(args, "--relation");
    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");
    let node_id_arg: Option<i64> = if get_flag_value(args, "--node-id").is_some() {
        Some(parse_flag_or(args, "--node-id", 0_i64))
    } else {
        None
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve to (target_ids, symbol_name) — prefer --node-id for same-file multi-def disambiguation.
    // When --node-id is given, it is authoritative: --file is ignored (matches MCP find_references).
    if node_id_arg.is_some() && explicit_file.is_some() {
        eprintln!("[code-graph] Note: --file is ignored when --node-id is given (node_id is authoritative).");
    }
    let (target_ids, symbol): (Vec<i64>, String) = if let Some(nid) = node_id_arg {
        let node = queries::get_node_by_id(conn, nid)?
            .ok_or_else(|| anyhow::anyhow!("node_id {} not found in index", nid))?;
        (vec![nid], node.name)
    } else {
        let raw_symbol = get_positional(args, 0)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp refs <symbol> [--node-id N] [--file path] [--relation calls|imports|inherits|implements] [--compact] [--json]"
            ))?;
        let (base, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
        let file_path = explicit_file.or(resolved_file.as_deref());

        if let Some(fp) = file_path {
            let nodes = queries::get_nodes_by_file_path(conn, fp)?;
            let matched: Vec<i64> = nodes.iter().filter(|n| n.name == base).map(|n| n.id).collect();
            if matched.is_empty() {
                anyhow::bail!("Symbol '{}' not found in file '{}'.", base, fp);
            }
            (matched, base.to_string())
        } else {
            let ids = queries::get_node_ids_by_name(conn, base)?;
            if ids.is_empty() {
                // Fuzzy auto-resolve: unique match → promote; multi → suggest; none → bail
                match resolve_fuzzy_name_cli(conn, base)? {
                    CliFuzzyResolution::Unique(resolved) => {
                        let resolved_ids = queries::get_node_ids_by_name(conn, &resolved)?;
                        (resolved_ids.into_iter().map(|(id, _)| id).collect(), resolved)
                    }
                    CliFuzzyResolution::Ambiguous(cands) => {
                        if json_mode {
                            let sugg: Vec<serde_json::Value> = cands.iter().take(5).map(|c| serde_json::json!({
                                "name": c.name, "file_path": c.file_path,
                                "type": c.node_type, "node_id": c.node_id, "start_line": c.start_line,
                            })).collect();
                            println!("{}", serde_json::json!({
                                "error": format!("Ambiguous symbol '{}': {} matches. Specify --file or --node-id to disambiguate.", base, cands.len()),
                                "suggestions": sugg,
                            }));
                        } else {
                            eprintln!("[code-graph] Ambiguous symbol '{}': {} matches. Specify --file or --node-id.", base, cands.len());
                            for c in cands.iter().take(5) {
                                eprintln!("  {} ({}) in {} [node_id {}]", c.name, c.node_type, c.file_path, c.node_id);
                            }
                        }
                        std::process::exit(1);
                    }
                    CliFuzzyResolution::NotFound => {
                        if json_mode { println!("[]"); }
                        eprintln!("[code-graph] Symbol not found: {}", base);
                        std::process::exit(1);
                    }
                }
            } else {
                (ids.into_iter().map(|(id, _)| id).collect(), base.to_string())
            }
        }
    };
    // Intentional shadow: downstream paths want &str. Do NOT "simplify" into a
    // single binding — the tuple above must own the String so `get_node_by_id`'s
    // return doesn't get dropped across the .as_str() borrow.
    let symbol = symbol.as_str();

    use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_IMPLEMENTS};
    let relation_filter = match relation {
        Some("calls") => Some(REL_CALLS),
        Some("imports") => Some(REL_IMPORTS),
        Some("inherits") => Some(REL_INHERITS),
        Some("implements") => Some(REL_IMPLEMENTS),
        Some("all") | None => None,
        Some(other) => anyhow::bail!("Unknown relation '{}'. Valid: calls, imports, inherits, implements, all", other),
    };

    let mut all_refs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for target_id in &target_ids {
        let refs = queries::get_incoming_references(conn, *target_id, relation_filter)?;
        for r in refs {
            let key = (r.name.clone(), r.file_path.clone(), r.relation.clone());
            if seen.insert(key) {
                all_refs.push(r);
            }
        }
    }

    if json_mode {
        let items: Vec<serde_json::Value> = all_refs.iter().map(|r| {
            if compact {
                serde_json::json!({
                    "name": r.name,
                    "file_path": r.file_path,
                    "start_line": r.start_line,
                    "relation": r.relation,
                    "node_id": r.node_id,
                })
            } else {
                serde_json::json!({
                    "node_id": r.node_id,
                    "name": r.name,
                    "type": r.node_type,
                    "file_path": r.file_path,
                    "start_line": r.start_line,
                    "relation": r.relation,
                })
            }
        }).collect();
        // Group counts by relation, mirroring MCP find_references envelope
        let mut by_relation: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for r in &all_refs {
            *by_relation.entry(r.relation.clone()).or_insert(0) += 1;
        }
        let envelope = serde_json::json!({
            "symbol": symbol,
            "total_references": items.len(),
            "by_relation": by_relation,
            "references": items,
        });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        let mut stdout = std::io::stdout().lock();
        if all_refs.is_empty() {
            writeln!(stdout, "No references found for '{}'.", symbol)?;
        } else {
            writeln!(stdout, "{} references to '{}':", all_refs.len(), symbol)?;
            for r in &all_refs {
                if compact {
                    writeln!(stdout, "  [{}] {} {}", r.relation, r.name, r.file_path)?;
                } else {
                    writeln!(stdout, "  [{}] {} ({}:{})", r.relation, r.name, r.file_path, r.start_line)?;
                }
            }
        }
    }

    Ok(())
}

/// Find dead code: orphans and exported-unused symbols.
/// CLI equivalent of MCP `find_dead_code`.
pub fn cmd_dead_code(project_root: &Path, args: &[String]) -> Result<()> {
    let path_filter = get_positional(args, 0).map(|p| p.strip_prefix("./").unwrap_or(p));
    // Accept both --node-type (preferred, matches `search` CLI + MCP param) and --type (legacy).
    let type_filter = get_flag_value(args, "--node-type")
        .or_else(|| get_flag_value(args, "--type"));
    let include_tests = has_flag(args, "--include-tests");
    let min_lines: u32 = parse_flag_or(args, "--min-lines", 3_u32);
    let compact = !has_flag(args, "--no-compact");
    let json_mode = has_flag(args, "--json");

    // --ignore <pref>: repeatable, prefix-match exclusion. --no-ignore disables defaults.
    // Defaults are owned by `domain::default_dead_code_ignores()` (claude-plugin/, benches/).
    let ignore_prefixes: Vec<String> = if has_flag(args, "--no-ignore") {
        Vec::new()
    } else {
        let explicit: Vec<String> = collect_flag_values(args, "--ignore");
        if explicit.is_empty() {
            crate::domain::default_dead_code_ignores()
        } else {
            explicit
        }
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let raw = queries::find_dead_code(conn, path_filter, type_filter, include_tests, min_lines, 200)?;
    let pre_count = raw.len();
    let results: Vec<_> = raw.into_iter()
        .filter(|r| !ignore_prefixes.iter().any(|p| r.file_path.starts_with(p)))
        .collect();
    let ignored = pre_count - results.len();

    if results.is_empty() {
        if json_mode {
            writeln!(std::io::stdout().lock(), "[]")?;
        }
        if ignored > 0 {
            eprintln!(
                "[code-graph] No dead code found after filtering; {} suppressed by --ignore (use --no-ignore to see them).",
                ignored,
            );
        } else {
            eprintln!("[code-graph] No dead code found.");
        }
        return Ok(());
    }

    // Classify into orphans and exported-unused
    let mut orphans: Vec<&queries::DeadCodeResult> = Vec::new();
    let mut exported_unused: Vec<&queries::DeadCodeResult> = Vec::new();

    for r in &results {
        let is_exported = r.has_export_edge
            || r.code_content.starts_with("pub ")
            || r.code_content.starts_with("pub(")
            || (r.file_path.ends_with(".go")
                && r.name.chars().next().is_some_and(|c| c.is_uppercase()));
        if is_exported {
            exported_unused.push(r);
        } else {
            orphans.push(r);
        }
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let items: Vec<serde_json::Value> = results.iter().map(|r| {
            let is_exported = r.has_export_edge
                || r.code_content.starts_with("pub ")
                || r.code_content.starts_with("pub(");
            let mut obj = serde_json::json!({
                "name": r.name,
                "type": r.node_type,
                "file_path": r.file_path,
                "start_line": r.start_line,
                "end_line": r.end_line,
                "category": if is_exported { "exported_unused" } else { "orphan" },
                "lines": r.end_line - r.start_line + 1,
            });
            if !compact {
                obj["code"] = serde_json::json!(r.code_content);
            }
            obj
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    writeln!(stdout, "Dead code: {} results ({} orphan, {} exported-unused)\n",
        results.len(), orphans.len(), exported_unused.len())?;

    if !orphans.is_empty() {
        writeln!(stdout, "ORPHAN ({}) — no references, not exported", orphans.len())?;
        for r in &orphans {
            let lines = r.end_line - r.start_line + 1;
            writeln!(stdout, "  {} {} {}:{} ({} lines)",
                r.node_type, r.name, r.file_path, r.start_line, lines)?;
            if !compact {
                for line in r.code_content.lines().take(5) {
                    writeln!(stdout, "    {}", line)?;
                }
                if r.code_content.lines().count() > 5 {
                    writeln!(stdout, "    ...")?;
                }
            }
        }
    }

    if !exported_unused.is_empty() {
        if !orphans.is_empty() { writeln!(stdout)?; }
        writeln!(stdout, "EXPORTED-UNUSED ({}) — exported/public but never called", exported_unused.len())?;
        for r in &exported_unused {
            let lines = r.end_line - r.start_line + 1;
            writeln!(stdout, "  {} {} {}:{} ({} lines)",
                r.node_type, r.name, r.file_path, r.start_line, lines)?;
            if !compact {
                for line in r.code_content.lines().take(5) {
                    writeln!(stdout, "    {}", line)?;
                }
                if r.code_content.lines().count() > 5 {
                    writeln!(stdout, "    ...")?;
                }
            }
        }
    }

    Ok(())
}

/// Run benchmark: full index, incremental index, query latency, DB size, token savings.
pub fn cmd_benchmark(project_root: &Path, args: &[String]) -> Result<()> {
    use crate::domain::CODE_GRAPH_DIR;
    use crate::indexer::pipeline::{run_full_index, run_incremental_index};
    use std::time::Instant;

    let json_mode = has_flag(args, "--json");

    // Create a temporary database for benchmarking
    let data_dir = project_root.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&data_dir)?;
    let bench_db_path = data_dir.join("benchmark-temp.db");
    if bench_db_path.exists() {
        std::fs::remove_file(&bench_db_path)?;
    }

    eprintln!("[benchmark] Indexing {}...", project_root.display());

    // 1. Full index timing
    let bench_db = Database::open(&bench_db_path)?;
    let t_full = Instant::now();
    let result = run_full_index(&bench_db, project_root, None, None)?;
    let full_index_ms = t_full.elapsed().as_millis() as u64;

    let files_indexed = result.files_indexed;
    let nodes_created = result.nodes_created;
    let edges_created = result.edges_created;

    eprintln!("[benchmark] Full index: {}ms ({} files, {} nodes, {} edges)",
        full_index_ms, files_indexed, nodes_created, edges_created);

    // 2. Incremental index (no-change detection — should be fast)
    let t_incr = Instant::now();
    let _ = run_incremental_index(&bench_db, project_root, None, None)?;
    let incr_index_ms = t_incr.elapsed().as_millis() as u64;

    eprintln!("[benchmark] Incremental (no-change): {}ms", incr_index_ms);

    // 3. Query latency: run 5 FTS searches, compute P50/P99
    let test_queries = ["function", "error", "config", "parse", "index"];
    let mut query_times_us: Vec<u64> = Vec::with_capacity(test_queries.len());
    let conn = bench_db.conn();

    for q in &test_queries {
        let t_q = Instant::now();
        let _ = queries::fts5_search(conn, q, 10)?;
        query_times_us.push(t_q.elapsed().as_micros() as u64);
    }

    query_times_us.sort();
    let p50_us = query_times_us[query_times_us.len() / 2];
    let p99_us = query_times_us[query_times_us.len() - 1]; // with 5 samples, P99 ≈ max

    eprintln!("[benchmark] Query latency P50: {}us, P99: {}us", p50_us, p99_us);

    // 4. DB size
    let db_size_bytes = std::fs::metadata(&bench_db_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let db_size_mb = db_size_bytes as f64 / (1024.0 * 1024.0);

    // 5. Token savings estimate: avg code_content length / 3.0 tokens per char
    let avg_content_len: f64 = conn
        .query_row(
            "SELECT COALESCE(AVG(LENGTH(code_content)), 0) FROM nodes WHERE code_content IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0.0);
    let avg_tokens = avg_content_len / 3.0;

    // Clean up: drop connection before deleting file
    drop(bench_db);
    if bench_db_path.exists() {
        std::fs::remove_file(&bench_db_path)?;
    }
    // Also clean up WAL/SHM files that SQLite may leave behind
    let wal_path = bench_db_path.with_extension("db-wal");
    let shm_path = bench_db_path.with_extension("db-shm");
    if wal_path.exists() { let _ = std::fs::remove_file(&wal_path); }
    if shm_path.exists() { let _ = std::fs::remove_file(&shm_path); }

    if json_mode {
        let json = serde_json::json!({
            "full_index_ms": full_index_ms,
            "incremental_index_ms": incr_index_ms,
            "files_indexed": files_indexed,
            "nodes_created": nodes_created,
            "edges_created": edges_created,
            "query_p50_us": p50_us,
            "query_p99_us": p99_us,
            "db_size_mb": (db_size_mb * 100.0).round() / 100.0,
            "avg_tokens_per_node": (avg_tokens * 10.0).round() / 10.0,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "Benchmark Results")?;
        writeln!(stdout, "=================")?;
        writeln!(stdout)?;
        writeln!(stdout, "Full index:          {:>8}ms  ({} files, {} nodes, {} edges)",
            full_index_ms, files_indexed, nodes_created, edges_created)?;
        writeln!(stdout, "Incremental (noop):  {:>8}ms", incr_index_ms)?;
        writeln!(stdout, "Query latency P50:   {:>8}us", p50_us)?;
        writeln!(stdout, "Query latency P99:   {:>8}us", p99_us)?;
        writeln!(stdout, "DB size:             {:>8.2}MB", db_size_mb)?;
        writeln!(stdout, "Avg tokens/node:     {:>8.1}", avg_tokens)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_project_root_prefers_existing_index_at_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let idx_dir = cwd.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&idx_dir).unwrap();
        std::fs::write(idx_dir.join("index.db"), b"").unwrap();
        assert_eq!(resolve_project_root_from(cwd), cwd);
    }

    #[test]
    fn resolve_project_root_climbs_to_git_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let subdir = root.join("sub").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();
        assert_eq!(resolve_project_root_from(&subdir), root);
    }

    #[test]
    fn resolve_project_root_falls_back_to_cwd_when_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // canonicalize both sides: on macOS `/tmp` ↔ `/private/tmp` symlinking;
        // on Linux they match directly, so this is a no-op but keeps the test portable.
        assert_eq!(resolve_project_root_from(cwd), cwd);
    }

    #[test]
    fn cleanup_legacy_db_files_removes_empty_legacy_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Empty legacy files — should be removed
        std::fs::write(dir.join("code-graph.db"), b"").unwrap();
        std::fs::write(dir.join("code_graph.db"), b"").unwrap();
        std::fs::write(dir.join("graph.db"), b"").unwrap();
        // Non-empty legacy file — must NOT be removed (guard against deleting real data)
        std::fs::write(dir.join("index.db"), b"real data").unwrap();
        // Unrelated file — must NOT be touched
        std::fs::write(dir.join("usage.jsonl"), b"").unwrap();

        cleanup_legacy_db_files(dir);

        assert!(!dir.join("code-graph.db").exists());
        assert!(!dir.join("code_graph.db").exists());
        assert!(!dir.join("graph.db").exists());
        assert!(dir.join("index.db").exists(), "non-empty index.db must survive");
        assert!(dir.join("usage.jsonl").exists(), "unrelated file must survive");
    }

    #[test]
    fn cleanup_legacy_db_files_keeps_non_empty_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // If a legacy file has content, it might be a real backup — don't delete.
        std::fs::write(dir.join("graph.db"), b"some content").unwrap();
        cleanup_legacy_db_files(dir);
        assert!(dir.join("graph.db").exists());
    }

    #[test]
    fn resolve_project_root_prefers_cwd_index_over_git_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let subdir = root.join("sub");
        let sub_idx = subdir.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&sub_idx).unwrap();
        std::fs::write(sub_idx.join("index.db"), b"").unwrap();
        assert_eq!(resolve_project_root_from(&subdir), subdir);
    }

    #[test]
    fn test_get_positional() {
        let args: Vec<String> = vec![
            "code-graph-mcp".into(),
            "grep".into(),
            "pattern".into(),
            "src/".into(),
        ];
        assert_eq!(get_positional(&args, 0), Some("pattern"));
        assert_eq!(get_positional(&args, 1), Some("src/"));
        assert_eq!(get_positional(&args, 2), None);
    }

    #[test]
    fn test_get_positional_with_flags() {
        let args: Vec<String> = vec![
            "code-graph-mcp".into(),
            "search".into(),
            "--json".into(),
            "query".into(),
            "--limit".into(),
            "10".into(),
        ];
        // --json is a flag without value (next arg doesn't start with --), so "query" is consumed as its value
        // Let's fix the logic to handle boolean flags properly
        // For now, positional extraction with flags interleaved
        assert_eq!(get_positional(&args, 0), Some("query"));
    }

    #[test]
    fn test_has_flag() {
        let args: Vec<String> = vec![
            "code-graph-mcp".into(),
            "search".into(),
            "--json".into(),
            "query".into(),
        ];
        assert!(has_flag(&args, "--json"));
        assert!(!has_flag(&args, "--compact"));
    }

    #[test]
    fn test_get_flag_value() {
        let args: Vec<String> = vec![
            "code-graph-mcp".into(),
            "search".into(),
            "--limit".into(),
            "10".into(),
            "query".into(),
        ];
        assert_eq!(get_flag_value(&args, "--limit"), Some("10"));
        assert_eq!(get_flag_value(&args, "--json"), None);
    }

    #[test]
    fn test_normalize_type_filter() {
        assert_eq!(normalize_type_filter("fn"), vec!["function", "method"]);
        assert_eq!(normalize_type_filter("class"), vec!["class"]);
        assert_eq!(normalize_type_filter("trait"), vec!["interface", "trait"]);
        assert!(normalize_type_filter("unknown").is_empty());
    }

    #[test]
    fn test_format_node_compact() {
        let node = queries::NodeResult {
            id: 1,
            file_id: 1,
            node_type: "function".into(),
            name: "foo".into(),
            qualified_name: Some("MyClass::foo".into()),
            start_line: 10,
            end_line: 20,
            code_content: String::new(),
            signature: None,
            doc_comment: None,
            context_string: None,
            name_tokens: None,
            return_type: Some("Result<Value>".into()),
            param_types: Some("name: &str, value: i64".into()),
            is_test: false,
        };
        let formatted = format_node_compact(&node, "src/lib.rs");
        assert!(formatted.contains("fn MyClass::foo"));
        assert!(formatted.contains("src/lib.rs:10-20"));
        assert!(formatted.contains("(name: &str, value: i64)"));
        assert!(formatted.contains("-> Result<Value>"));
    }

    #[test]
    fn test_parse_rg_json_empty() {
        let root = Path::new("/project");
        assert!(parse_rg_json(b"", root).is_empty());
    }

    #[test]
    fn test_parse_rg_json_match() {
        let root = Path::new("/project");
        let json_line = br#"{"type":"match","data":{"path":{"text":"/project/src/main.rs"},"line_number":42,"lines":{"text":"fn main() {\n"}}}"#;
        let matches = parse_rg_json(json_line, root);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, "src/main.rs");
        assert_eq!(matches[0].line, 42);
    }

    #[test]
    fn test_aggregate_usage_empty() {
        let s = aggregate_usage_jsonl("", None);
        assert_eq!(s.sessions, 0);
        assert_eq!(s.parse_errors, 0);
        assert!(s.tools.is_empty());
        assert_eq!(s.total_tool_calls(), 0);
    }

    #[test]
    fn test_aggregate_usage_skips_malformed_and_blank() {
        let content = "\n\nnot-json\n{\"ts\":\"2026-04-20T00:00:00Z\",\"v\":\"0.12.1\",\"tools\":{}}\n";
        let s = aggregate_usage_jsonl(content, None);
        assert_eq!(s.sessions, 1);
        assert_eq!(s.parse_errors, 1);
    }

    #[test]
    fn test_aggregate_usage_merges_tool_counts_across_sessions() {
        let line1 = r#"{"ts":"2026-04-19T10:00:00Z","v":"0.12.0","tools":{"get_call_graph":{"n":2,"ms":200,"err":0,"max_ms":150},"project_map":{"n":1,"ms":1000,"err":0,"max_ms":1000}}}"#;
        let line2 = r#"{"ts":"2026-04-20T10:00:00Z","v":"0.12.1","tools":{"get_call_graph":{"n":3,"ms":900,"err":1,"max_ms":500}}}"#;
        let content = format!("{}\n{}\n", line1, line2);
        let s = aggregate_usage_jsonl(&content, None);
        assert_eq!(s.sessions, 2);
        assert_eq!(s.total_tool_calls(), 6);

        let cg = s.tools.get("get_call_graph").unwrap();
        assert_eq!(cg.n, 5);
        assert_eq!(cg.total_ms, 1100);
        assert_eq!(cg.err, 1);
        assert_eq!(cg.max_ms, 500); // max across sessions

        let pm = s.tools.get("project_map").unwrap();
        assert_eq!(pm.n, 1);
        assert_eq!(pm.max_ms, 1000);

        assert_eq!(s.versions.len(), 2);
        assert!(s.versions.contains("0.12.0") && s.versions.contains("0.12.1"));
        assert_eq!(s.first_ts.as_deref(), Some("2026-04-19T10:00:00Z"));
        assert_eq!(s.last_ts.as_deref(), Some("2026-04-20T10:00:00Z"));
    }

    #[test]
    fn test_aggregate_usage_last_n_keeps_tail() {
        let lines: Vec<String> = (0..5).map(|i|
            format!(r#"{{"ts":"2026-04-2{}T00:00:00Z","v":"0.12.1","tools":{{"t":{{"n":1,"ms":{},"err":0,"max_ms":{}}}}}}}"#, i, (i + 1) * 10, (i + 1) * 10)
        ).collect();
        let content = lines.join("\n");
        let s = aggregate_usage_jsonl(&content, Some(2));
        assert_eq!(s.sessions, 2);
        let t = s.tools.get("t").unwrap();
        // Last 2 sessions: ms 40 + 50 = 90
        assert_eq!(t.total_ms, 90);
        assert_eq!(t.max_ms, 50);
    }

    #[test]
    fn test_aggregate_usage_search_and_index_merged() {
        let l1 = r#"{"ts":"t1","v":"0.12.1","tools":{"t":{"n":1,"ms":1,"err":0,"max_ms":1}},"search":{"queries":10,"zero":2,"avg_quality":0.8,"fts_only":3,"hybrid":7},"index":{"full_ms":2000,"incr":5,"files":50,"nodes":100}}"#;
        let l2 = r#"{"ts":"t2","v":"0.12.1","tools":{"t":{"n":1,"ms":1,"err":0,"max_ms":1}},"search":{"queries":5,"zero":0,"avg_quality":0.6,"fts_only":1,"hybrid":4},"index":{"full_ms":null,"incr":3,"files":10,"nodes":20}}"#;
        let s = aggregate_usage_jsonl(&format!("{}\n{}", l1, l2), None);
        assert_eq!(s.search_queries, 15);
        assert_eq!(s.search_zero, 2);
        assert_eq!(s.search_fts_only, 4);
        assert_eq!(s.search_hybrid, 11);
        // Weighted quality: (0.8 * 10 + 0.6 * 5) / 15 = 11.0 / 15 ≈ 0.7333
        let weighted_avg = s.search_quality_weighted_sum / s.search_queries as f64;
        assert!((weighted_avg - 0.7333).abs() < 0.01, "got {}", weighted_avg);
        assert_eq!(s.full_index_count, 1);
        assert_eq!(s.full_index_ms_sum, 2000);
        assert_eq!(s.incr_count, 8);
        assert_eq!(s.files_indexed, 60);
    }
}
