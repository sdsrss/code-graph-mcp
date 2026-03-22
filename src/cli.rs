use anyhow::Result;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::domain::CODE_GRAPH_DIR;
use crate::storage::db::Database;
use crate::storage::queries;

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
                "No index found at {}. Run the MCP server first to create the index.",
                db_path.display()
            );
        }
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
        Database::open(&db_path).ok().map(|db| Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }
}

// --- Argument helpers ---

/// Flags that take a value argument (not boolean).
const VALUE_FLAGS: &[&str] = &["--limit", "--type", "--returns", "--params", "--direction", "--depth", "--format", "--file", "--language", "--change-type", "--top-k", "--max-distance"];

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
pub fn cmd_incremental_index(project_root: &Path, quiet: bool) -> Result<()> {
    let ctx = CliContext::open(project_root)?;

    // Use run_incremental_index without a model (no embedding for short-lived CLI)
    use crate::indexer::pipeline::run_incremental_index;
    let stats = run_incremental_index(&ctx.db, &ctx.project_root, None, None)?;

    if !quiet {
        eprintln!(
            "Incremental index: {} files updated, {} nodes created",
            stats.files_indexed, stats.nodes_created
        );
    }
    Ok(())
}

/// Run health check and print status.
pub fn cmd_health_check(project_root: &Path, format: &str) -> Result<()> {
    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();
    let status = queries::get_index_status(conn, false)?;

    let expected_schema = crate::storage::schema::SCHEMA_VERSION;
    let schema_ok = status.schema_version == expected_schema;
    let has_data = status.nodes_count > 0 && status.files_count > 0;
    let healthy = schema_ok && has_data;

    match format {
        "json" => {
            let mut json = serde_json::json!({
                "healthy": healthy,
                "nodes": status.nodes_count,
                "edges": status.edges_count,
                "files": status.files_count,
                "watching": false,
                "schema_version": status.schema_version,
            });
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
                println!(
                    "OK: {} nodes, {} edges, {} files",
                    status.nodes_count, status.edges_count, status.files_count
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
        rg_cmd.arg(path);
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
        let relative_path = path_str
            .strip_prefix(root_str.as_str())
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
        .ok_or_else(|| anyhow::anyhow!("Usage: code-graph-mcp search <query> [--json] [--limit N] [--language <lang>] [--compact]"))?;

    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");
    let language_filter = get_flag_value(args, "--language");
    let limit: i64 = get_flag_value(args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Fetch more results if filtering, to ensure enough after filtering
    let fetch_limit = if language_filter.is_some() { limit * 4 } else { limit };
    let fts_result = queries::fts5_search(conn, query, fetch_limit)?;
    if fts_result.nodes.is_empty() {
        eprintln!("[code-graph] No results for: {}", query);
        return Ok(());
    }

    let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
    let nodes_with_files = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;

    // Build id->NodeWithFile map preserving FTS rank order
    let nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf))
        .collect();

    // Filter by language if requested, preserving FTS rank order
    let filtered_nodes: Vec<&queries::NodeResult> = fts_result.nodes.iter()
        .filter(|n| {
            if let Some(lang) = language_filter {
                nwf_map.get(&n.id)
                    .and_then(|nwf| nwf.language.as_deref())
                    .map(|l| l.eq_ignore_ascii_case(lang))
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .take(limit as usize)
        .collect();

    if filtered_nodes.is_empty() {
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
                    "id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file": fp,
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
    let limit: usize = get_flag_value(args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

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
                    "id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file": &nwf.file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for nwf in &results_with_files {
        writeln!(stdout, "{}", format_node_compact(&nwf.node, &nwf.file_path))?;
    }
    Ok(())
}

/// Normalize type filter shorthand: fn → function/method, class → class/struct, etc.
fn normalize_type_filter(input: &str) -> Vec<&'static str> {
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
        _ => {
            eprintln!(
                "[code-graph] Unknown type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                input
            );
            vec![]
        }
    }
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
    let symbol = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp callgraph <symbol> [--direction callers|callees|both] [--depth N] [--file <path>] [--json]"
        ))?;

    let direction = get_flag_value(args, "--direction").unwrap_or("both");
    let depth: i32 = get_flag_value(args, "--depth")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .clamp(1, 20);
    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");
    let include_tests = has_flag(args, "--include-tests");
    let file_filter = get_flag_value(args, "--file");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let nodes = crate::graph::query::get_call_graph(conn, symbol, direction, depth, file_filter)?;
    if nodes.is_empty() {
        eprintln!("[code-graph] No call graph results for: {}", symbol);
        // Try fuzzy match
        let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
        if !candidates.is_empty() {
            eprintln!("[code-graph] Did you mean:");
            for c in candidates.iter().take(5) {
                eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
            }
        }
        return Ok(());
    }

    // Filter test callers unless --include-tests is set
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
        let results: Vec<serde_json::Value> = display_nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "name": n.name,
                    "type": n.node_type,
                    "file": n.file_path,
                    "depth": n.depth,
                    "direction": n.direction.as_str(),
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
    }

    // Group by direction, deduplicate same (name, file, direction, depth)
    // e.g. cfg-gated conditional compilation variants
    let mut seen = std::collections::HashSet::new();
    for node in &display_nodes {
        if node.depth == 0 {
            continue;
        }
        let key = (&node.name, &node.file_path, node.direction.as_str(), node.depth);
        if !seen.insert(key) {
            continue; // skip duplicate (e.g. #[cfg] conditional compilation variants)
        }
        let arrow = match node.direction {
            crate::graph::query::Direction::Callers => "←",
            crate::graph::query::Direction::Callees => "→",
        };
        let indent = "  ".repeat(node.depth as usize);
        if compact {
            writeln!(stdout, "{}{} {} ({})", indent, arrow, node.name, node.file_path)?;
        } else {
            let arrow_text = match node.direction {
                crate::graph::query::Direction::Callers => "← called by",
                crate::graph::query::Direction::Callees => "→ calls",
            };
            writeln!(
                stdout,
                "{}{}: {} ({}) [{}]",
                indent, arrow_text, node.name, node.file_path, node.node_type
            )?;
        }
    }

    if test_count > 0 {
        writeln!(stdout, "  ({} test callers hidden, use --include-tests to show)", test_count)?;
    }

    Ok(())
}

/// Impact analysis.
///
/// Shows callers with route info and risk level.
pub fn cmd_impact(project_root: &Path, args: &[String]) -> Result<()> {
    let symbol = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp impact <symbol> [--depth N] [--file <path>] [--change-type signature|behavior|remove] [--json]"
        ))?;

    let depth: i32 = get_flag_value(args, "--depth")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .clamp(1, 20);
    let json_mode = has_flag(args, "--json");
    let file_filter = get_flag_value(args, "--file");
    let change_type = get_flag_value(args, "--change-type").unwrap_or("behavior");
    if !matches!(change_type, "signature" | "behavior" | "remove") {
        anyhow::bail!("--change-type must be one of: signature, behavior, remove");
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Verify symbol exists before running impact analysis
    let symbol_nodes = queries::get_nodes_by_name(conn, symbol)?;
    if symbol_nodes.is_empty() {
        eprintln!("[code-graph] Symbol not found: {}", symbol);
        let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
        if !candidates.is_empty() {
            eprintln!("[code-graph] Did you mean:");
            for c in candidates.iter().take(5) {
                eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
            }
        }
        return Ok(());
    }

    let callers = queries::get_callers_with_route_info(conn, symbol, file_filter, depth)?;

    // Exclude root node (depth 0) — it's the queried symbol itself
    let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();

    // Separate production callers from test callers
    let prod_callers: Vec<_> = callers.iter()
        .filter(|c| !crate::domain::is_test_symbol(&c.name, &c.file_path))
        .collect();
    let test_count = callers.len() - prod_callers.len();

    // Count unique files and routes from production callers only
    let files: std::collections::HashSet<&str> = prod_callers.iter().map(|c| c.file_path.as_str()).collect();
    let routes: Vec<&&queries::CallerWithRouteInfo> = prod_callers.iter().filter(|c| c.route_info.is_some()).collect();
    let direct_callers = prod_callers.iter().filter(|c| c.depth == 1).count();

    let risk = crate::domain::compute_risk_level(prod_callers.len(), routes.len(), change_type == "remove");

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let result = serde_json::json!({
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
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "Impact: {} — Risk: {}", symbol, risk)?;
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
        let result = serde_json::json!({
            "modules": modules.iter().map(|m| serde_json::json!({
                "path": m.path,
                "files": m.files,
                "functions": m.functions,
                "classes": m.classes,
                "interfaces": m.interfaces_traits,
                "languages": m.languages,
                "key_symbols": m.key_symbols,
            })).collect::<Vec<_>>(),
            "dependencies": deps.iter().map(|d| serde_json::json!({
                "from": d.from,
                "to": d.to,
                "imports": d.import_count,
            })).collect::<Vec<_>>(),
            "entry_points": entry_points.iter().map(|e| serde_json::json!({
                "route": e.route,
                "handler": e.handler,
                "file": e.file,
            })).collect::<Vec<_>>(),
            "hot_functions": hot_functions.iter().map(|h| serde_json::json!({
                "name": h.name,
                "type": h.node_type,
                "file": h.file,
                "callers": h.caller_count,
            })).collect::<Vec<_>>(),
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
            writeln!(
                stdout,
                "  {} ({}) — {} callers ({})",
                h.name, h.node_type, h.caller_count, h.file
            )?;
        }
    }

    Ok(())
}

/// Module overview: all symbols in files under a path prefix.
pub fn cmd_overview(project_root: &Path, args: &[String]) -> Result<()> {
    let path_prefix = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!("Usage: code-graph-mcp overview <path> [--json] [--compact]"))?;

    let json_mode = has_flag(args, "--json");
    let compact = has_flag(args, "--compact");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let exports = queries::get_module_exports(conn, path_prefix)?;
    if exports.is_empty() {
        eprintln!("[code-graph] No symbols found under: {}", path_prefix);
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = exports
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "type": e.node_type,
                    "file": e.file_path,
                    "signature": e.signature,
                    "callers": e.caller_count,
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
    let symbol = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp show <symbol> [--file <path>] [--json]"
        ))?;

    let json_mode = has_flag(args, "--json");
    let file_filter = get_flag_value(args, "--file");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve symbol: prefer file_path+name, fallback to name-only
    let nodes = if let Some(fp) = file_filter {
        queries::get_nodes_by_file_path(conn, fp)?
            .into_iter()
            .filter(|n| n.name == symbol || n.qualified_name.as_deref() == Some(symbol))
            .collect::<Vec<_>>()
    } else {
        queries::get_nodes_by_name(conn, symbol)?
    };

    if nodes.is_empty() {
        eprintln!("[code-graph] Symbol not found: {}", symbol);
        let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
        if !candidates.is_empty() {
            eprintln!("[code-graph] Did you mean:");
            for c in candidates.iter().take(5) {
                eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
            }
        }
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = nodes.iter().map(|node| {
            let fp = queries::get_file_path(conn, node.file_id)
                .ok().flatten().unwrap_or_else(|| "?".to_string());
            serde_json::json!({
                "id": node.id,
                "type": node.node_type,
                "name": node.qualified_name.as_deref().unwrap_or(&node.name),
                "file": fp,
                "start_line": node.start_line,
                "end_line": node.end_line,
                "signature": node.signature,
                "return_type": node.return_type,
                "param_types": node.param_types,
                "code": node.code_content,
            })
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for node in &nodes {
        let fp = queries::get_file_path(conn, node.file_id)?
            .unwrap_or_else(|| "?".to_string());
        writeln!(stdout, "{}", format_node_compact(node, &fp))?;
        if !node.code_content.is_empty() {
            for line in node.code_content.lines() {
                writeln!(stdout, "  {}", line)?;
            }
        }
    }

    Ok(())
}

/// Trace HTTP route → handler → downstream calls.
/// CLI equivalent of MCP `trace_http_chain`.
pub fn cmd_trace(project_root: &Path, args: &[String]) -> Result<()> {
    let route_path = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp trace <route> [--depth N] [--json]"
        ))?;

    let depth: i32 = get_flag_value(args, "--depth")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3)
        .clamp(1, 20);
    let json_mode = has_flag(args, "--json");

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
        eprintln!("[code-graph] No routes matching: {}", route_path);
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    for rm in &rows {
        if json_mode {
            let chain = crate::graph::query::get_call_graph(
                conn, &rm.handler_name, "callees", depth, Some(&rm.file_path),
            )?;
            let chain_nodes: Vec<serde_json::Value> = chain.iter()
                .filter(|n| n.depth > 0)
                .map(|n| serde_json::json!({
                    "name": n.name, "file": n.file_path, "depth": n.depth,
                }))
                .collect();
            writeln!(stdout, "{}", serde_json::to_string(&serde_json::json!({
                "route": path,
                "handler": rm.handler_name,
                "file": rm.file_path,
                "call_chain": chain_nodes,
            }))?)?;
        } else {
            writeln!(stdout, "{} → {} ({}:{})",
                rm.metadata.as_deref().unwrap_or(path),
                rm.handler_name, rm.file_path, rm.start_line)?;

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
    }

    Ok(())
}

/// File-level dependency graph.
/// CLI equivalent of MCP `dependency_graph`.
pub fn cmd_deps(project_root: &Path, args: &[String]) -> Result<()> {
    let file_path = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp deps <file> [--direction outgoing|incoming|both] [--depth N] [--json]"
        ))?;

    let direction = get_flag_value(args, "--direction").unwrap_or("both");
    if !matches!(direction, "outgoing" | "incoming" | "both") {
        anyhow::bail!("--direction must be one of: outgoing, incoming, both");
    }
    let depth: i32 = get_flag_value(args, "--depth")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
        .clamp(1, 10);
    let json_mode = has_flag(args, "--json");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let deps = queries::get_import_tree(conn, file_path, direction, depth)?;
    if deps.is_empty() {
        eprintln!("[code-graph] No dependencies found for: {}", file_path);
        return Ok(());
    }

    // Filter out cross-language false edges (name-based resolution artifacts)
    let root_lang = crate::utils::config::detect_language(file_path);
    let is_compatible_lang = |dep_path: &str| -> bool {
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
            "depends_on": outgoing.iter().map(|d| serde_json::json!({
                "file": d.file_path, "depth": d.depth, "symbols": d.symbol_count,
            })).collect::<Vec<_>>(),
            "depended_by": incoming.iter().map(|d| serde_json::json!({
                "file": d.file_path, "depth": d.depth, "symbols": d.symbol_count,
            })).collect::<Vec<_>>(),
        });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "{}", file_path)?;
    if !outgoing.is_empty() {
        writeln!(stdout, "  Depends on:")?;
        for d in &outgoing {
            if d.depth == 1 {
                writeln!(stdout, "    {} ({} symbols)", d.file_path, d.symbol_count)?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }
    if !incoming.is_empty() {
        writeln!(stdout, "  Depended by:")?;
        for d in &incoming {
            if d.depth == 1 {
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
    let symbol = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp similar <symbol> [--top-k N] [--max-distance N] [--json]"
        ))?;

    let top_k: i64 = get_flag_value(args, "--top-k")
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
        .clamp(1, 100);
    let max_distance: f64 = get_flag_value(args, "--max-distance")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.8);
    let json_mode = has_flag(args, "--json");

    // Open with vec support for vector search
    let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
    if !db_path.exists() {
        anyhow::bail!("No index found. Run the MCP server first to create the index.");
    }
    let db = Database::open_with_vec(&db_path)?;
    let conn = db.conn();

    if !db.vec_enabled() {
        eprintln!("[code-graph] Embedding not available. Build with --features embed-model.");
        return Ok(());
    }

    // Resolve symbol to node_id
    let node_id = match queries::get_first_node_id_by_name(conn, symbol)? {
        Some(id) => id,
        None => {
            eprintln!("[code-graph] Symbol not found: {}", symbol);
            return Ok(());
        }
    };

    // Check embedding exists
    let (embedded_count, total_nodes) = queries::count_nodes_with_vectors(conn)?;
    if embedded_count == 0 {
        eprintln!("[code-graph] No embeddings found ({}/{} nodes embedded). Run MCP server with embed-model feature.", embedded_count, total_nodes);
        return Ok(());
    }

    let embedding: Vec<f32> = {
        let bytes = queries::get_node_embedding(conn, node_id)
            .map_err(|_| anyhow::anyhow!("No embedding for node_id {} ({}/{} nodes embedded)", node_id, embedded_count, total_nodes))?;
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

    if similar.is_empty() {
        eprintln!("[code-graph] No similar code found for: {}", symbol);
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let json_results: Vec<serde_json::Value> = similar.iter().map(|(node, fp, distance)| {
            let similarity = 1.0 / (1.0 + distance);
            serde_json::json!({
                "name": node.name, "type": node.node_type, "file": fp,
                "start_line": node.start_line, "similarity": (similarity * 10000.0).round() / 10000.0,
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
    let symbol = get_positional(args, 0)
        .ok_or_else(|| anyhow::anyhow!(
            "Usage: code-graph-mcp refs <symbol> [--file path] [--relation calls|imports|inherits|implements] [--json]"
        ))?;
    let file_path = get_flag_value(args, "--file");
    let relation = get_flag_value(args, "--relation");
    let json_mode = has_flag(args, "--json");

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve symbol to node_id(s)
    let target_ids: Vec<i64> = if let Some(fp) = file_path {
        let nodes = queries::get_nodes_by_file_path(conn, fp)?;
        let ids: Vec<i64> = nodes.iter().filter(|n| n.name == symbol).map(|n| n.id).collect();
        if ids.is_empty() {
            anyhow::bail!("Symbol '{}' not found in file '{}'.", symbol, fp);
        }
        ids
    } else {
        let ids = queries::get_node_ids_by_name(conn, symbol)?;
        if ids.is_empty() {
            anyhow::bail!("Symbol '{}' not found in index.", symbol);
        }
        ids.into_iter().map(|(id, _)| id).collect()
    };

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
            serde_json::json!({
                "node_id": r.node_id,
                "name": r.name,
                "type": r.node_type,
                "file": r.file_path,
                "start_line": r.start_line,
                "relation": r.relation,
            })
        }).collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        let mut stdout = std::io::stdout().lock();
        if all_refs.is_empty() {
            writeln!(stdout, "No references found for '{}'.", symbol)?;
        } else {
            writeln!(stdout, "{} references to '{}':", all_refs.len(), symbol)?;
            for r in &all_refs {
                writeln!(stdout, "  [{}] {} ({}:{})", r.relation, r.name, r.file_path, r.start_line)?;
            }
        }
    }

    Ok(())
}

/// Run benchmark: full index, incremental index, query latency, DB size, token savings.
pub fn cmd_benchmark(project_root: &Path, args: &[String]) -> Result<()> {
    use crate::domain::CODE_GRAPH_DIR;
    use crate::indexer::pipeline::run_full_index;
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
    let _ = run_full_index(&bench_db, project_root, None, None)?;
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
}
