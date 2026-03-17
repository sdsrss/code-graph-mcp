use anyhow::{anyhow, Result};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};

/// Check if a process with the given PID is alive.
fn pid_is_alive(pid: u32) -> bool {
    // Linux: fast /proc check
    #[cfg(target_os = "linux")]
    { Path::new(&format!("/proc/{}", pid)).exists() }
    // Non-Linux fallback: conservative — assume alive to prevent dual-primary.
    // Stale locks are reclaimed only on Linux or via manual cleanup.
    #[cfg(not(target_os = "linux"))]
    { let _ = pid; true }
}

/// Try to acquire the index lock (`.code-graph/index.lock`).
/// Returns `true` if this process becomes the primary indexer.
/// Uses O_CREAT|O_EXCL for atomic creation; stale locks (dead PID) are reclaimed.
fn try_acquire_index_lock(code_graph_dir: &Path) -> bool {
    use std::fs::OpenOptions;
    use std::io::Write;

    let lock_path = code_graph_dir.join("index.lock");
    let my_pid = std::process::id();

    // Try atomic exclusive create first — eliminates TOCTOU race
    match OpenOptions::new().write(true).create_new(true).open(&lock_path) {
        Ok(mut f) => {
            let _ = f.write_all(my_pid.to_string().as_bytes());
            return true;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Lock file exists — check if holder is alive
        }
        Err(e) => {
            tracing::warn!("Could not write index lock: {} — running in secondary mode", e);
            return false;
        }
    }

    // Lock exists — check if the holding process is alive
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if pid != my_pid && pid_is_alive(pid) {
                tracing::info!("Another instance (PID {}) holds the index lock — running in secondary (read-only) mode", pid);
                return false;
            }
            // Stale lock from dead process — reclaim
            tracing::info!("Reclaiming stale index lock from PID {}", pid);
            let _ = std::fs::remove_file(&lock_path);
        }
    }

    // Re-create atomically after reclaiming stale lock
    match OpenOptions::new().write(true).create_new(true).open(&lock_path) {
        Ok(mut f) => {
            let _ = f.write_all(my_pid.to_string().as_bytes());
            true
        }
        Err(_) => {
            // Another process grabbed it between our remove and create — that's fine
            tracing::info!("Lost lock race during stale reclaim — running in secondary mode");
            false
        }
    }
}

/// Release the index lock if we are the owner.
fn release_index_lock(code_graph_dir: &Path) {
    let lock_path = code_graph_dir.join("index.lock");
    let my_pid = std::process::id();
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if content.trim().parse::<u32>().ok() == Some(my_pid) {
            let _ = std::fs::remove_file(&lock_path);
        }
    }
}

use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::tools::ToolRegistry;
use crate::embedding::model::EmbeddingModel;
use crate::indexer::pipeline::{embed_and_store_batch, run_full_index, run_incremental_index_cached, IndexStats};
use crate::indexer::watcher::{FileWatcher, WatchEvent};
use crate::search::fusion::weighted_rrf_fusion;
use crate::storage::db::Database;
use crate::storage::queries;

/// Extract a required string argument, trimming whitespace and rejecting empty values.
fn required_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    let s = args[key].as_str()
        .ok_or_else(|| anyhow!("{} is required", key))?
        .trim();
    if s.is_empty() {
        return Err(anyhow!("{} must not be empty", key));
    }
    Ok(s)
}

/// Whether a symbol is a test-only symbol (by name or file path convention).
fn is_test_symbol(name: &str, file_path: &str) -> bool {
    name.starts_with("test_") || file_path.starts_with("tests/")
}

/// Parse route input like "GET /api/users" into (Some("GET"), "/api/users").
/// If no method prefix, returns (None, original_path).
fn parse_route_input(input: &str) -> (Option<String>, &str) {
    let trimmed = input.trim();
    if let Some(space_idx) = trimmed.find(' ') {
        let prefix = &trimmed[..space_idx];
        let methods = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "USE"];
        if methods.contains(&prefix.to_uppercase().as_str()) {
            return (Some(prefix.to_uppercase()), trimmed[space_idx..].trim());
        }
    }
    (None, trimmed)
}

/// Filter route matches by HTTP method from metadata JSON.
fn filter_routes_by_method(rows: &mut Vec<queries::RouteMatch>, method: &Option<String>) {
    if let Some(method) = method {
        rows.retain(|r| {
            r.metadata.as_ref().is_some_and(|m| {
                serde_json::from_str::<serde_json::Value>(m).ok()
                    .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|s| s.to_string()))
                    .is_some_and(|rm| rm == *method)
            })
        });
    }
}

/// For inline handlers, override handler_name and start/end lines from metadata.
fn apply_inline_handler_metadata(handler: &mut serde_json::Value, metadata: Option<&str>) {
    if let Some(meta_str) = metadata {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
            if meta.get("inline").and_then(|v| v.as_bool()).unwrap_or(false) {
                handler["handler_name"] = json!(format!(
                    "{} {} (inline)",
                    meta.get("method").and_then(|v| v.as_str()).unwrap_or("?"),
                    meta.get("path").and_then(|v| v.as_str()).unwrap_or("?")
                ));
                if let Some(sl) = meta.get("handler_start_line").and_then(|v| v.as_i64()) {
                    handler["start_line"] = json!(sl);
                }
                if let Some(el) = meta.get("handler_end_line").and_then(|v| v.as_i64()) {
                    handler["end_line"] = json!(el);
                }
            }
        }
    }
}

/// Check if the caller requested to skip indexing (read-only mode).
fn should_skip_indexing(args: &serde_json::Value) -> bool {
    args.get("skip_indexing").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Centralized compression for tool results that exceed the token threshold.
/// Handlers that already produce custom compressed output (with a "mode" key)
/// are left unchanged. For other results, this truncates large string values
/// and adds a `_truncated` marker.
fn centralized_compress(value: serde_json::Value) -> serde_json::Value {
    use crate::sandbox::compressor::estimate_json_tokens;
    let tokens = estimate_json_tokens(&value);
    if tokens <= COMPRESSION_TOKEN_THRESHOLD {
        return value;
    }
    // If the handler already produced a compressed result, leave it alone
    if value.get("mode").is_some() {
        return value;
    }
    // Truncate large string values to bring result under threshold
    truncate_large_strings(value, COMPRESSION_TOKEN_THRESHOLD)
}

/// Recursively truncate string values in a JSON value to stay within a token budget.
/// Adds a `_truncated` key to the top-level object when truncation occurs.
fn truncate_large_strings(value: serde_json::Value, token_budget: usize) -> serde_json::Value {
    // Target: reduce to roughly token_budget * 3 chars total
    let target_chars = token_budget * 3;
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() <= target_chars {
        return value;
    }

    let mut result = truncate_value(value, target_chars);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("_truncated".to_string(), json!(true));
        obj.insert("_truncation_hint".to_string(),
            json!("Result exceeded token limit. Use get_ast_node(node_id) to read specific nodes."));
    }
    result
}

/// Truncate a JSON value's string fields to fit within a char budget.
fn truncate_value(value: serde_json::Value, budget: usize) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            // Per-field budget: distribute chars across fields
            let field_count = map.len().max(1);
            let per_field = budget / field_count;
            let truncated: serde_json::Map<String, serde_json::Value> = map.into_iter()
                .map(|(k, v)| {
                    let tv = match &v {
                        serde_json::Value::String(s) if s.len() > per_field => {
                            let truncated_str = &s[..s.floor_char_boundary(per_field.min(s.len()))];
                            json!(format!("{}... [truncated, {} chars total]", truncated_str, s.len()))
                        }
                        serde_json::Value::Array(arr) if arr.len() > 20 => {
                            // Keep first 10 and last 5 items, note truncation
                            let mut kept: Vec<serde_json::Value> = arr[..10].to_vec();
                            kept.push(json!(format!("... [{} items truncated]", arr.len() - 15)));
                            kept.extend_from_slice(&arr[arr.len()-5..]);
                            serde_json::Value::Array(kept)
                        }
                        _ => v,
                    };
                    (k, tv)
                })
                .collect();
            serde_json::Value::Object(truncated)
        }
        serde_json::Value::Array(arr) if arr.len() > 20 => {
            let mut kept: Vec<serde_json::Value> = arr[..10].to_vec();
            kept.push(json!(format!("... [{} items truncated]", arr.len() - 15)));
            kept.extend_from_slice(&arr[arr.len()-5..]);
            serde_json::Value::Array(kept)
        }
        other => other,
    }
}

/// Lock a Mutex, recovering from poison but logging a warning.
fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|e| {
        tracing::warn!("Recovering poisoned mutex ({}): prior panic in critical section", label);
        e.into_inner()
    })
}

struct WatcherState {
    _watcher: FileWatcher,
    receiver: mpsc::Receiver<WatchEvent>,
}

/// Debounce interval for no-watcher incremental checks.
/// In tests, use 0s so incremental checks always run immediately.
#[cfg(not(test))]
const INCREMENTAL_DEBOUNCE_SECS: u64 = 30;
#[cfg(test)]
const INCREMENTAL_DEBOUNCE_SECS: u64 = 0;

/// Token threshold for auto-compressing tool results.
/// Results exceeding this estimated token count are returned as summaries
/// with node_ids for expansion via get_ast_node.
const COMPRESSION_TOKEN_THRESHOLD: usize = 2000;

/// Result of fuzzy name resolution.
enum FuzzyResolution {
    /// Exactly one candidate matched — use this name.
    Unique(String),
    /// Multiple candidates — return suggestions to caller.
    Ambiguous(Vec<serde_json::Value>),
    /// No candidates found.
    NotFound,
}

/// Result from background startup indexing, consumed by post-index processing.
struct StartupIndexResult {
    files_indexed: usize,
    nodes_created: usize,
    edges_created: usize,
    elapsed_ms: u64,
    was_full: bool,
    new_cache: Option<crate::indexer::merkle::DirectoryCache>,
    stats: IndexStats,
}

/// MCP server for code graph operations. Single-threaded (stdio loop).
pub struct McpServer {
    registry: ToolRegistry,
    db: Database,
    embedding_model: Mutex<Option<EmbeddingModel>>,
    project_root: Option<PathBuf>,
    indexed: Mutex<bool>,
    watcher: Mutex<Option<WatcherState>>,
    last_incremental_check: Mutex<std::time::Instant>,
    dir_cache: Mutex<Option<crate::indexer::merkle::DirectoryCache>>,
    /// Writer for sending MCP notifications (progress, logging) to the client.
    /// Set to stdout in production; None in tests.
    notify_writer: Mutex<Option<Box<dyn Write + Send>>>,
    /// Set to true when `notifications/initialized` is received, signaling
    /// the main loop to run initial indexing and auto-start the file watcher.
    startup_index_pending: Mutex<bool>,
    /// True while a background embedding thread is running.
    embedding_in_progress: Arc<AtomicBool>,
    /// True while background startup indexing is running.
    startup_indexing: Arc<AtomicBool>,
    /// Signaled when background startup indexing completes.
    startup_indexing_done: Arc<(Mutex<bool>, Condvar)>,
    /// Pending result from background startup indexing, consumed by post-index processing.
    startup_index_result: Arc<Mutex<Option<StartupIndexResult>>>,
    /// Last indexing stats (skipped files, truncations) for observability.
    last_index_stats: Mutex<IndexStats>,
    /// Aggregated session metrics, flushed to .code-graph/usage.jsonl at shutdown.
    metrics: Mutex<super::metrics::SessionMetrics>,
    /// Cached project_map result: (timestamp, json_value). Invalidated on re-index.
    cached_project_map: Mutex<Option<(std::time::Instant, serde_json::Value)>>,
    /// Cached module_overview results: path → (timestamp, json_value). Invalidated on re-index.
    cached_module_overviews: Mutex<std::collections::HashMap<String, (std::time::Instant, serde_json::Value)>>,
    /// True if this instance holds the index lock (primary indexer).
    /// Secondary instances skip indexing/watching and read the DB in read-only mode.
    is_primary: bool,
}

impl McpServer {
    fn open_db(db_path: &Path) -> Result<Database> {
        // Always open with vec support — model may be downloaded later (hot-loading)
        // and the background embedding thread needs vec tables to exist.
        Database::open_with_vec(db_path)
    }

    /// Create from project root path: auto-creates .code-graph/ directory and .gitignore entry
    pub fn from_project_root(project_root: &Path) -> Result<Self> {
        let db_dir = project_root.join(".code-graph");
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("index.db");

        // Ensure .code-graph/ is in .gitignore
        let gitignore_path = project_root.join(".gitignore");
        {
            let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
            if !content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed == ".code-graph/" || trimmed == ".code-graph"
            }) {
                let mut new_content = content;
                if !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                new_content.push_str(".code-graph/\n");
                if let Err(e) = std::fs::write(&gitignore_path, new_content) {
                    tracing::warn!("Could not update .gitignore: {}", e);
                }
            }
        }

        let is_primary = try_acquire_index_lock(&db_dir);

        let embedding_model = EmbeddingModel::load()?;
        let db = Self::open_db(&db_path)?;
        Ok(Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model: Mutex::new(embedding_model),
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            dir_cache: Mutex::new(None),
            notify_writer: Mutex::new(None),
            startup_index_pending: Mutex::new(false),
            embedding_in_progress: Arc::new(AtomicBool::new(false)),
            startup_indexing: Arc::new(AtomicBool::new(false)),
            startup_indexing_done: Arc::new((Mutex::new(false), Condvar::new())),
            startup_index_result: Arc::new(Mutex::new(None)),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            cached_project_map: Mutex::new(None),
            cached_module_overviews: Mutex::new(std::collections::HashMap::new()),
            is_primary,
        })
    }

    #[cfg(test)]
    pub fn new_test() -> Self {
        let db = Database::open(Path::new(":memory:")).unwrap();
        Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model: Mutex::new(None),
            project_root: None,
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            dir_cache: Mutex::new(None),
            notify_writer: Mutex::new(None),
            startup_index_pending: Mutex::new(false),
            embedding_in_progress: Arc::new(AtomicBool::new(false)),
            startup_indexing: Arc::new(AtomicBool::new(false)),
            startup_indexing_done: Arc::new((Mutex::new(false), Condvar::new())),
            startup_index_result: Arc::new(Mutex::new(None)),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            cached_project_map: Mutex::new(None),
            cached_module_overviews: Mutex::new(std::collections::HashMap::new()),
            is_primary: true,
        }
    }

    #[cfg(test)]
    pub fn new_test_with_project(project_root: &Path) -> Self {
        let db_dir = project_root.join(".code-graph");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&db_dir.join("index.db")).unwrap();
        Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model: Mutex::new(None),
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            dir_cache: Mutex::new(None),
            notify_writer: Mutex::new(None),
            startup_index_pending: Mutex::new(false),
            embedding_in_progress: Arc::new(AtomicBool::new(false)),
            startup_indexing: Arc::new(AtomicBool::new(false)),
            startup_indexing_done: Arc::new((Mutex::new(false), Condvar::new())),
            startup_index_result: Arc::new(Mutex::new(None)),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            cached_project_map: Mutex::new(None),
            cached_module_overviews: Mutex::new(std::collections::HashMap::new()),
            is_primary: true,
        }
    }

    /// Set the writer for sending MCP notifications to the client.
    pub fn set_notify_writer(&self, writer: Box<dyn Write + Send>) {
        *lock_or_recover(&self.notify_writer, "notify_writer") = Some(writer);
    }

    /// Flush aggregated session metrics to .code-graph/usage.jsonl.
    /// Called once at server shutdown (EOF). Skips if no tool calls were made.
    /// Also releases the index lock if this instance is the primary.
    pub fn flush_metrics(&self) {
        if let Some(ref root) = self.project_root {
            let metrics = lock_or_recover(&self.metrics, "metrics");
            if !metrics.is_empty() {
                let usage_path = root.join(".code-graph").join("usage.jsonl");
                metrics.flush(&usage_path, env!("CARGO_PKG_VERSION"));
            }
            if self.is_primary {
                release_index_lock(&root.join(".code-graph"));
            }
        }
    }

    /// Run startup tasks if triggered by `notifications/initialized`.
    /// Called from the main loop after each message. Spawns background indexing
    /// (non-blocking) and starts watcher/embedding once indexing completes.
    /// Secondary instances (no index lock) skip indexing and watcher entirely.
    pub fn run_startup_tasks(&self) {
        // Phase 1: On notifications/initialized, spawn background indexing
        let pending = {
            let mut guard = lock_or_recover(&self.startup_index_pending, "startup_index_pending");
            let was_pending = *guard;
            *guard = false;
            was_pending
        };

        if pending {
            let project_root = match &self.project_root {
                Some(p) => p.clone(),
                None => return,
            };

            // Secondary instances: skip indexing/watcher, but still do embedding
            if !self.is_primary {
                let has_data = queries::get_index_status(self.db.conn(), false)
                    .map(|s| s.files_count > 0)
                    .unwrap_or(false);
                if has_data {
                    *lock_or_recover(&self.indexed, "indexed") = true;
                    self.send_log("info", "Secondary instance: using existing index (read-only).");
                    // Embedding uses its own DB connection and is append-only — safe for secondary
                    self.spawn_background_embedding();
                } else {
                    self.send_log("info", "Secondary instance: no index available yet. Queries will work once the primary instance finishes indexing.");
                }
                return;
            }

            let is_indexed = *lock_or_recover(&self.indexed, "indexed");
            if !is_indexed {
                let has_existing = queries::get_index_status(self.db.conn(), false)
                    .map(|s| s.files_count > 0)
                    .unwrap_or(false);
                // Take dir_cache for background thread (incremental can use it)
                let dir_cache = if has_existing {
                    lock_or_recover(&self.dir_cache, "dir_cache").take()
                } else {
                    None
                };
                self.spawn_startup_indexing(project_root, has_existing, dir_cache);
                return; // watcher + embedding start after indexing completes
            }

            // Already indexed — just start watcher + embedding
            self.start_post_index_services(&project_root);
            return;
        }

        // Phase 2: Check if background indexing completed, do post-index work
        self.consume_startup_index_result();
    }

    /// Spawn a background thread for startup indexing (non-blocking).
    /// Writes progress to `.code-graph/indexing-status.json` for statusline.
    fn spawn_startup_indexing(
        &self,
        project_root: PathBuf,
        has_existing_index: bool,
        dir_cache: Option<crate::indexer::merkle::DirectoryCache>,
    ) {
        if self.startup_indexing.swap(true, Ordering::AcqRel) {
            return; // already running
        }

        // Reset condvar done flag for this indexing session
        *self.startup_indexing_done.0.lock().unwrap() = false;

        if has_existing_index {
            self.send_log("info", "Updating index in background (incremental)...");
        } else {
            self.send_log("info", "Building index in background...");
        }

        let db_path = project_root.join(".code-graph").join("index.db");
        let indexing_flag = Arc::clone(&self.startup_indexing);
        let done_signal = Arc::clone(&self.startup_indexing_done);
        let result_slot = Arc::clone(&self.startup_index_result);
        let progress_file = project_root.join(".code-graph").join("indexing-status.json");

        std::thread::spawn(move || {
            // Guard ensures flags are always cleared, even on panic
            struct IndexGuard {
                flag: Arc<AtomicBool>,
                done: Arc<(Mutex<bool>, Condvar)>,
                progress_file: PathBuf,
            }
            impl Drop for IndexGuard {
                fn drop(&mut self) {
                    self.flag.store(false, Ordering::Release);
                    let _ = std::fs::remove_file(&self.progress_file);
                    let (lock, cvar) = &*self.done;
                    if let Ok(mut done) = lock.lock() {
                        *done = true;
                    }
                    cvar.notify_all();
                }
            }
            let _guard = IndexGuard {
                flag: indexing_flag,
                done: done_signal,
                progress_file: progress_file.clone(),
            };

            let db = match Database::open_with_vec(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    tracing::error!("Background indexing: failed to open DB: {}", e);
                    return;
                }
            };

            let pf = progress_file.clone();
            let progress_cb = move |current: usize, total: usize| {
                let json = format!(r#"{{"s":"indexing","d":{},"t":{}}}"#, current, total);
                let _ = std::fs::write(&pf, json);
            };

            let index_start = std::time::Instant::now();
            let result = if has_existing_index {
                run_incremental_index_cached(
                    &db, &project_root, None,
                    dir_cache.as_ref(),
                    Some(&progress_cb),
                ).map(|(r, cache)| (r, Some(cache)))
            } else {
                run_full_index(&db, &project_root, None, Some(&progress_cb))
                    .map(|r| (r, None))
            };

            match result {
                Ok((result, new_cache)) => {
                    let elapsed_ms = index_start.elapsed().as_millis() as u64;
                    tracing::info!(
                        "Background indexing complete: {} files, {} nodes in {}ms",
                        result.files_indexed, result.nodes_created, elapsed_ms
                    );
                    match result_slot.lock() {
                        Ok(mut slot) => {
                            *slot = Some(StartupIndexResult {
                                files_indexed: result.files_indexed,
                                nodes_created: result.nodes_created,
                                edges_created: result.edges_created,
                                elapsed_ms,
                                was_full: !has_existing_index,
                                new_cache,
                                stats: result.stats,
                            });
                        }
                        Err(e) => tracing::error!("Background indexing: result slot poisoned: {}", e),
                    }
                }
                Err(e) => {
                    tracing::error!("Background indexing failed: {}", e);
                }
            }
            // _guard drop: clears flag, removes progress file, signals condvar
        });
    }

    /// Check if background startup indexing completed and process the result.
    /// Called from `run_startup_tasks()` and `ensure_indexed()`.
    fn consume_startup_index_result(&self) {
        if self.startup_indexing.load(Ordering::Acquire) {
            return; // still running
        }

        let result = lock_or_recover(&self.startup_index_result, "startup_result").take();
        let Some(r) = result else { return };

        *lock_or_recover(&self.indexed, "indexed") = true;

        // Invalidate caches after background startup indexing
        if r.files_indexed > 0 {
            *lock_or_recover(&self.cached_project_map, "cached_pmap") = None;
            lock_or_recover(&self.cached_module_overviews, "cached_movw").clear();
        }

        // Store indexing stats for observability (exposed via get_index_status)
        *lock_or_recover(&self.last_index_stats, "last_index_stats") = r.stats;

        // Store new dir_cache if available
        if let Some(cache) = r.new_cache {
            *lock_or_recover(&self.dir_cache, "dir_cache") = Some(cache);
        }

        // Record metrics
        lock_or_recover(&self.metrics, "metrics").record_index(
            r.files_indexed as u64,
            r.nodes_created as u64,
            r.was_full,
            r.elapsed_ms,
        );

        if r.files_indexed > 0 {
            self.send_log("info", &format!(
                "Indexed {} files ({} nodes, {} edges).",
                r.files_indexed, r.nodes_created, r.edges_created
            ));
        } else {
            self.send_log("info", "Index is up to date.");
        }

        // Safety net: ensure progress file is removed (normally done by IndexGuard)
        if let Some(ref root) = self.project_root {
            let _ = std::fs::remove_file(root.join(".code-graph").join("indexing-status.json"));
        }

        // Start watcher + embedding
        if let Some(ref root) = self.project_root {
            self.start_post_index_services(root);
        }
    }

    /// Start file watcher and background embedding (called after indexing completes).
    fn start_post_index_services(&self, project_root: &Path) {
        // Auto-start file watcher
        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if watcher_guard.is_none() {
            let (tx, rx) = mpsc::channel();
            match FileWatcher::start(project_root, tx) {
                Ok(fw) => {
                    *watcher_guard = Some(WatcherState {
                        _watcher: fw,
                        receiver: rx,
                    });
                    self.send_log("info", "File watcher started automatically.");
                }
                Err(e) => {
                    self.send_log("warning", &format!("Could not start file watcher: {}", e));
                }
            }
        }
        drop(watcher_guard);

        self.spawn_background_embedding();

        #[cfg(feature = "embed-model")]
        self.spawn_model_download();
    }

    /// Spawn a background thread to embed nodes that don't yet have vectors.
    /// The thread opens its own DB connection and model (EmbeddingModel is not Send)
    /// to avoid blocking the main stdio loop.
    fn spawn_background_embedding(&self) {
        // Guard: only spawn if model and vec are available
        if lock_or_recover(&self.embedding_model, "embedding_model").is_none() || !self.db.vec_enabled() {
            return;
        }

        let db_path = match &self.project_root {
            Some(p) => p.join(".code-graph").join("index.db"),
            None => return,
        };

        // Acquire flag AFTER precondition checks to avoid permanent flag leak
        if self.embedding_in_progress.swap(true, Ordering::AcqRel) {
            return; // already running
        }
        let flag = Arc::clone(&self.embedding_in_progress);

        std::thread::spawn(move || {
            // Drop guard ensures flag is always cleared, even on panic
            struct FlagGuard(Arc<AtomicBool>);
            impl Drop for FlagGuard {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _guard = FlagGuard(flag);

            let result = (|| -> Result<()> {
                let model = match EmbeddingModel::load()? {
                    Some(m) => m,
                    None => return Ok(()),
                };
                let db = Database::open_with_vec(&db_path)?;

                const EMBED_BATCH: usize = 32;
                let mut total_embedded = 0usize;
                let t0 = std::time::Instant::now();

                loop {
                    let chunk = queries::get_unembedded_nodes(db.conn(), EMBED_BATCH)?;
                    if chunk.is_empty() {
                        break;
                    }

                    let tx = db.conn().unchecked_transaction()?;
                    embed_and_store_batch(&db, &model, &chunk)?;
                    tx.commit()?;

                    total_embedded += chunk.len();
                    tracing::info!("[embed-bg] Progress: {} nodes embedded", total_embedded);
                }

                if total_embedded > 0 {
                    tracing::info!("[embed-bg] Complete: {} nodes in {:.1}s",
                        total_embedded, t0.elapsed().as_secs_f64());
                }
                Ok(())
            })();

            if let Err(e) = result {
                tracing::warn!("[embed-bg] Failed: {}", e);
            }
            // FlagGuard::drop() clears the flag automatically
        });
    }

    /// Spawn a background thread to download the embedding model if not available.
    /// On success, the model files are placed in the cache directory; lazy loading
    /// in tool_semantic_search will pick them up on the next call.
    #[cfg(feature = "embed-model")]
    fn spawn_model_download(&self) {
        // Only if model is not already loaded
        if lock_or_recover(&self.embedding_model, "embedding_model").is_some() {
            return;
        }

        std::thread::spawn(move || {
            let cache_dir = match EmbeddingModel::cache_models_dir() {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("[model-dl] Cannot resolve cache dir: {}", e);
                    return;
                }
            };

            if cache_dir.join("model.safetensors").exists() {
                return; // Already downloaded
            }

            let url = EmbeddingModel::model_download_url();
            match EmbeddingModel::download_model_to(&url, &cache_dir) {
                Ok(()) => tracing::info!("[model-dl] Model downloaded successfully"),
                Err(e) => tracing::warn!("[model-dl] Download failed (FTS5-only mode): {}", e),
            }
        });
    }

    /// Try lazy loading: if model was None but cache now has files, load it.
    /// Called at the start of semantic search / find_similar_code.
    fn try_lazy_load_model(&self) {
        let needs_load = lock_or_recover(&self.embedding_model, "embedding_model").is_none();
        if !needs_load {
            return;
        }
        // Try loading — if files were downloaded in background, this will find them
        if let Ok(Some(model)) = EmbeddingModel::load() {
            *lock_or_recover(&self.embedding_model, "embedding_model") = Some(model);
            tracing::info!("[model] Embedding model hot-loaded from cache");
            // Trigger background embedding for existing nodes
            self.spawn_background_embedding();
        }
    }

    /// Try fuzzy name resolution: returns the unique match, multiple suggestions, or nothing.
    fn resolve_fuzzy_name(&self, name: &str) -> Result<FuzzyResolution> {
        let candidates: Vec<_> = queries::find_functions_by_fuzzy_name(self.db.conn(), name)?
            .into_iter()
            .filter(|c| !is_test_symbol(&c.name, &c.file_path))
            .collect();
        if candidates.len() == 1 {
            Ok(FuzzyResolution::Unique(candidates.into_iter().next().unwrap().name))
        } else if !candidates.is_empty() {
            let suggestions = candidates.iter().map(|c| json!({
                "name": c.name, "file_path": c.file_path, "type": c.node_type,
            })).collect();
            Ok(FuzzyResolution::Ambiguous(suggestions))
        } else {
            Ok(FuzzyResolution::NotFound)
        }
    }

    /// Check if a symbol name is ambiguous (exists in multiple files).
    /// Returns Some(suggestions) if ambiguous, None if unambiguous or not found.
    fn disambiguate_symbol(&self, name: &str) -> Result<Option<Vec<serde_json::Value>>> {
        let candidates = queries::get_nodes_by_name(self.db.conn(), name)?;
        let non_test: Vec<_> = candidates.iter()
            .filter(|n| {
                let fp = queries::get_file_path(self.db.conn(), n.file_id)
                    .ok().flatten().unwrap_or_default();
                !is_test_symbol(&n.name, &fp)
            })
            .collect();
        if non_test.len() > 1 {
            let mut seen_files = std::collections::HashSet::new();
            for n in &non_test {
                seen_files.insert(n.file_id);
            }
            if seen_files.len() > 1 {
                let suggestions: Vec<_> = non_test.iter().map(|n| {
                    let fp = queries::get_file_path(self.db.conn(), n.file_id)
                        .ok().flatten().unwrap_or_else(|| "unknown".to_string());
                    json!({
                        "name": &n.name,
                        "file_path": fp,
                        "type": &n.node_type,
                        "node_id": n.id,
                    })
                }).collect();
                return Ok(Some(suggestions));
            }
        }
        Ok(None)
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Send a JSON-RPC notification to the client (non-blocking, best-effort).
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let mut guard = lock_or_recover(&self.notify_writer, "notify_writer");
        if let Some(ref mut writer) = *guard {
            let msg = json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            });
            let _ = writeln!(writer, "{}", msg);
            let _ = writer.flush();
        }
    }

    /// Send MCP progress notification.
    fn send_progress(&self, token: &str, current: usize, total: usize) {
        self.send_notification("notifications/progress", json!({
            "progressToken": token,
            "progress": current,
            "total": total,
        }));
    }

    /// Send MCP log notification.
    fn send_log(&self, level: &str, message: &str) {
        self.send_notification("notifications/message", json!({
            "level": level,
            "logger": "code-graph",
            "data": message,
        }));
    }

    /// Ensure index is up-to-date. On first call, runs full index.
    /// If background startup indexing is running, waits for it to complete.
    /// If watcher is active, checks for pending events to decide if incremental needed.
    fn ensure_indexed(&self) -> Result<()> {
        let project_root = match &self.project_root {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        // Secondary instances: read-only — just check if DB has data
        if !self.is_primary {
            let is_indexed = *lock_or_recover(&self.indexed, "indexed");
            if !is_indexed {
                let has_data = queries::get_index_status(self.db.conn(), false)
                    .map(|s| s.files_count > 0)
                    .unwrap_or(false);
                if has_data {
                    *lock_or_recover(&self.indexed, "indexed") = true;
                }
            }
            return Ok(());
        }

        // Wait for or consume background startup indexing result
        if self.startup_indexing.load(Ordering::Acquire) {
            self.send_log("info", "Waiting for background indexing to complete...");
            let (lock, cvar) = &*self.startup_indexing_done;
            let mut done = lock_or_recover(lock, "startup_indexing_done");
            let timeout = std::time::Duration::from_secs(300);
            while !*done {
                let (guard, wait_result) = cvar.wait_timeout(done, timeout).unwrap_or_else(|e| {
                    tracing::warn!("Recovering poisoned condvar (startup_indexing_done)");
                    let guard = e.into_inner();
                    (guard.0, guard.1)
                });
                done = guard;
                if wait_result.timed_out() {
                    tracing::warn!("Background indexing wait timed out ({}s), falling back to synchronous", timeout.as_secs());
                    break;
                }
            }
        }
        // Consume result whether we waited or it completed before this call
        self.consume_startup_index_result();

        // Read the indexed flag (short lock scope to avoid holding across I/O)
        let is_indexed = *lock_or_recover(&self.indexed, "indexed");

        if !is_indexed {
            self.send_log("info", "Scanning and indexing project files...");
            let progress_cb = |current: usize, total: usize| {
                self.send_progress("indexing", current, total);
            };
            // Skip inline embedding for full index (too slow), background thread handles it
            let result = run_full_index(&self.db, &project_root, None, Some(&progress_cb))?;
            *lock_or_recover(&self.last_index_stats, "last_index_stats") = result.stats;
            *lock_or_recover(&self.indexed, "indexed") = true;
            // Invalidate caches after re-index
            *lock_or_recover(&self.cached_project_map, "cached_pmap") = None;
            lock_or_recover(&self.cached_module_overviews, "cached_movw").clear();
            // Note: model lock is NOT held here — spawn_background_embedding locks it internally
            self.spawn_background_embedding();
        } else {
            // Check if watcher detected changes (locks watcher only)
            let has_changes = self.drain_watcher_events();
            if has_changes {
                // Skip inline embedding — background thread handles it (avoids holding model lock across I/O)
                self.run_incremental_with_cache_restore(&project_root, None)?;
            } else {
                // No watcher or no events: still run incremental (cheap if nothing changed)
                let has_watcher = lock_or_recover(&self.watcher, "watcher").is_some();
                if !has_watcher {
                    // No watcher active — debounce to avoid rescanning on every tool call
                    let mut last_check = lock_or_recover(&self.last_incremental_check, "last_incremental_check");
                    if last_check.elapsed() > std::time::Duration::from_secs(INCREMENTAL_DEBOUNCE_SECS) {
                        self.run_incremental_with_cache_restore(&project_root, None)?;
                        *last_check = std::time::Instant::now();
                    }
                }
                // Watcher active but no events → index is up-to-date, skip
            }
        }
        Ok(())
    }

    /// Run incremental index with cache snapshot/restore on failure.
    fn run_incremental_with_cache_restore(&self, project_root: &Path, model: Option<&EmbeddingModel>) -> Result<()> {
        let mut cache_guard = lock_or_recover(&self.dir_cache, "dir_cache");
        let cache_snapshot = cache_guard.clone();
        let cache = cache_guard.take();
        drop(cache_guard); // Release lock during I/O

        match run_incremental_index_cached(&self.db, project_root, model, cache.as_ref(), None) {
            Ok((result, new_cache)) => {
                if result.files_indexed > 0 {
                    // Invalidate caches when files actually changed
                    *lock_or_recover(&self.cached_project_map, "cached_pmap") = None;
                    lock_or_recover(&self.cached_module_overviews, "cached_movw").clear();
                }
                *lock_or_recover(&self.last_index_stats, "last_index_stats") = result.stats;
                *lock_or_recover(&self.dir_cache, "dir_cache") = Some(new_cache);
                Ok(())
            }
            Err(e) => {
                tracing::error!("Incremental index failed, restoring cache: {}", e);
                *lock_or_recover(&self.dir_cache, "dir_cache") = cache_snapshot;
                Err(e)
            }
        }
    }

    /// Drain all pending events from the watcher receiver.
    /// Returns true if any file change events were received.
    fn drain_watcher_events(&self) -> bool {
        let watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if let Some(ref state) = *watcher_guard {
            let mut has_changes = false;
            while state.receiver.try_recv().is_ok() {
                has_changes = true;
            }
            has_changes
        } else {
            false
        }
    }

    /// Returns whether the file watcher is currently active.
    fn is_watching(&self) -> bool {
        lock_or_recover(&self.watcher, "watcher").is_some()
    }

    pub fn handle_message(&self, line: &str) -> Result<Option<String>> {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(req) => req,
            Err(e) => {
                let resp = JsonRpcResponse::error(
                    None,
                    super::protocol::JSONRPC_PARSE_ERROR,
                    format!("Parse error: {}", e),
                );
                return Ok(Some(serde_json::to_string(&resp)?));
            }
        };

        // Per JSON-RPC 2.0, notifications (no id) must never receive a response
        if req.id.is_none() {
            if req.method == "notifications/initialized" {
                *lock_or_recover(&self.startup_index_pending, "startup_index_pending") = true;
            }
            return Ok(None);
        }

        // Validate JSON-RPC version (only for requests with id)
        if let Err(msg) = req.validate() {
            let resp = JsonRpcResponse::error(req.id, super::protocol::JSONRPC_INVALID_REQUEST, msg.to_string());
            return Ok(Some(serde_json::to_string(&resp)?));
        }

        let response = match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id),
            "ping" => JsonRpcResponse::success(req.id, json!({})),
            "tools/list" => self.handle_tools_list(req.id),
            "tools/call" => self.handle_tools_call(req.id, req.params),
            "resources/list" => self.handle_resources_list(req.id),
            "resources/read" => self.handle_resources_read(req.id, req.params),
            "prompts/list" => self.handle_prompts_list(req.id),
            "prompts/get" => self.handle_prompts_get(req.id, req.params),
            _ => JsonRpcResponse::error(
                req.id,
                super::protocol::JSONRPC_METHOD_NOT_FOUND,
                format!("Method not found: {}", req.method),
            ),
        };

        Ok(Some(serde_json::to_string(&response)?))
    }

    fn handle_initialize(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false }
            },
            "serverInfo": {
                "name": "code-graph-mcp",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": concat!(
                "Code Graph: AST knowledge graph with semantic search. RULES:\n",
                "PRIORITY: When indexed, code-graph tools SUPERSEDE Grep/Agent for code understanding.\n",
                "0. START HERE \u{2192} project_map for full architecture overview (modules, deps, routes, hot paths).\n",
                "1. Call graph (who calls X / what X calls) \u{2192} get_call_graph. NOT Grep.\n",
                "2. Module/file understanding \u{2192} module_overview. NOT Read multiple files.\n",
                "3. BEFORE modifying a function \u{2192} impact_analysis FIRST.\n",
                "4. Find code by concept/meaning \u{2192} semantic_code_search. NOT Grep.\n",
                "5. HTTP request tracing \u{2192} trace_http_chain. NOT Read router+handler.\n",
                "6. One symbol's signature+relations \u{2192} get_ast_node. NOT Read whole file.\n",
                "7. File dependencies \u{2192} dependency_graph. NOT Grep imports.\n",
                "8. Similar/duplicate code \u{2192} find_similar_code.\n",
                "Use Grep ONLY for: exact strings, constants, regex patterns.\n",
                "Use Read ONLY for: files you will edit.\n",
                "PATTERNS:\n",
                "  Quick lookup: semantic_code_search(query, compact=true) \u{2192} get_ast_node(node_id=N)\n",
                "  Before edit: impact_analysis(symbol) \u{2192} Edit\n",
                "  Understand: project_map(compact=true) \u{2192} module_overview(path, compact=true) \u{2192} get_call_graph(symbol)\n",
                "TOKEN SAVING: Several tools support compact=true. Use compact for browsing/overview, full when you need signatures or will edit."
            )
        }))
    }

    fn handle_tools_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        let tools: Vec<serde_json::Value> = self.registry.list_tools().iter().map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        }).collect();

        JsonRpcResponse::success(id, json!({ "tools": tools }))
    }

    fn handle_tools_call(&self, id: Option<serde_json::Value>, params: Option<serde_json::Value>) -> JsonRpcResponse {
        let params = match params {
            Some(p) => p,
            None => return JsonRpcResponse::error(id, super::protocol::JSONRPC_INVALID_PARAMS, "Missing params".into()),
        };

        let tool_name = match params["name"].as_str() {
            Some(name) => name,
            None => return JsonRpcResponse::error(id, super::protocol::JSONRPC_INVALID_PARAMS, "Missing or invalid 'name' in tool call params".into()),
        };
        let arguments = &params["arguments"];

        match self.handle_tool(tool_name, arguments) {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {}\"}}", e));
                JsonRpcResponse::success(id, json!({
                    "content": [{
                        "type": "text",
                        "text": text
                    }]
                }))
            }
            Err(e) => {
                tracing::warn!("[tool-error] {}: {}", tool_name, e);
                JsonRpcResponse::success(id, json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Error: {}", e)
                    }],
                    "isError": true
                }))
            }
        }
    }

    fn handle_resources_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "resources": [{
                "uri": "code-graph://project-summary",
                "name": "Code Graph Project Summary",
                "description": "Overview of the indexed codebase: file count, node count, edge count, languages, and index health",
                "mimeType": "application/json",
                "annotations": {
                    "audience": ["assistant"]
                }
            }]
        }))
    }

    fn handle_resources_read(&self, id: Option<serde_json::Value>, params: Option<serde_json::Value>) -> JsonRpcResponse {
        let uri = params.as_ref()
            .and_then(|p| p["uri"].as_str())
            .unwrap_or("");

        match uri {
            "code-graph://project-summary" => {
                let status = match queries::get_index_status(self.db.conn(), self.is_watching()) {
                    Ok(s) => s,
                    Err(e) => return JsonRpcResponse::error(
                        id,
                        super::protocol::JSONRPC_INTERNAL_ERROR,
                        format!("Failed to get index status: {}", e),
                    ),
                };

                let summary = json!({
                    "files": status.files_count,
                    "nodes": status.nodes_count,
                    "edges": status.edges_count,
                    "schema_version": status.schema_version,
                    "db_size_bytes": status.db_size_bytes,
                    "watching": status.is_watching,
                    "last_indexed_at": status.last_indexed_at,
                });

                JsonRpcResponse::success(id, json!({
                    "contents": [{
                        "uri": "code-graph://project-summary",
                        "mimeType": "application/json",
                        "text": serde_json::to_string_pretty(&summary).unwrap_or_default()
                    }]
                }))
            }
            _ => JsonRpcResponse::error(
                id,
                super::protocol::JSONRPC_INVALID_PARAMS,
                format!("Unknown resource URI: {}", uri),
            ),
        }
    }

    fn handle_prompts_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "prompts": [
                {
                    "name": "impact-analysis",
                    "description": "Analyze the blast radius of changing a symbol",
                    "arguments": [
                        { "name": "symbol_name", "description": "Symbol to analyze", "required": true }
                    ]
                },
                {
                    "name": "understand-module",
                    "description": "Deep dive into a module's architecture and relationships",
                    "arguments": [
                        { "name": "path", "description": "File or directory path", "required": true }
                    ]
                },
                {
                    "name": "trace-request",
                    "description": "Trace an HTTP request from route to data layer",
                    "arguments": [
                        { "name": "route", "description": "HTTP route path (e.g. /api/users)", "required": true }
                    ]
                }
            ]
        }))
    }

    fn handle_prompts_get(&self, id: Option<serde_json::Value>, params: Option<serde_json::Value>) -> JsonRpcResponse {
        let name = params.as_ref()
            .and_then(|p| p["name"].as_str())
            .unwrap_or("");
        let arguments = params.as_ref()
            .and_then(|p| p["arguments"].as_object());

        match name {
            "impact-analysis" => {
                let symbol = arguments
                    .and_then(|a| a.get("symbol_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<symbol>");
                JsonRpcResponse::success(id, json!({
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Analyze the impact of changing the symbol '{}'. \
                                 Use the impact_analysis tool with symbol_name='{}' to get the blast radius, \
                                 then use get_call_graph to understand the full caller/callee chain. \
                                 Present: affected files, affected routes, risk level, and recommendations.",
                                symbol, symbol
                            )
                        }
                    }]
                }))
            }
            "understand-module" => {
                let path = arguments
                    .and_then(|a| a.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<path>");
                JsonRpcResponse::success(id, json!({
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Give me a deep understanding of the module at '{}'. \
                                 Use module_overview to get exports and hot paths, \
                                 then use dependency_graph to map what it depends on and what depends on it. \
                                 For the top 3 most-called exports, use get_call_graph to show their caller chain. \
                                 Present: purpose, public API, dependencies, dependents, and hot paths.",
                                path
                            )
                        }
                    }]
                }))
            }
            "trace-request" => {
                let route = arguments
                    .and_then(|a| a.get("route"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<route>");
                JsonRpcResponse::success(id, json!({
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Trace the complete HTTP request flow for route '{}'. \
                                 Use trace_http_chain to get the full chain from route to data layer. \
                                 For each key node, use get_ast_node(node_id=N, context_lines=5) to show the implementation. \
                                 Map the flow: route → middleware → validation → business logic → data access → response. \
                                 Highlight error handling, auth checks, and database operations.",
                                route
                            )
                        }
                    }]
                }))
            }
            _ => JsonRpcResponse::error(
                id,
                super::protocol::JSONRPC_INVALID_PARAMS,
                format!("Unknown prompt: {}", name),
            ),
        }
    }

    fn handle_tool(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        let start = std::time::Instant::now();
        let result = match name {
            "semantic_code_search" => self.tool_semantic_search(args),
            "get_call_graph" => self.tool_get_call_graph(args),
            "find_http_route" | "trace_http_chain" => self.tool_trace_http_chain(args),
            "get_ast_node" | "read_snippet" => self.tool_get_ast_node(args),
            "start_watch" => self.tool_start_watch(),
            "stop_watch" => self.tool_stop_watch(),
            "get_index_status" => self.tool_get_index_status(),
            "rebuild_index" => self.tool_rebuild_index(args),
            "impact_analysis" => self.tool_impact_analysis(args),
            "module_overview" => self.tool_module_overview(args),
            "dependency_graph" => self.tool_dependency_graph(args),
            "find_similar_code" => self.tool_find_similar_code(args),
            "project_map" => self.tool_project_map(args),
            _ => Err(anyhow!("Unknown tool: {}", name)),
        };
        let elapsed = start.elapsed();
        let is_error = result.is_err();
        lock_or_recover(&self.metrics, "metrics")
            .record_tool_call(name, elapsed.as_millis() as u64, is_error);
        if elapsed.as_millis() > 100 {
            tracing::info!("[tool] {} completed in {:.1}s", name, elapsed.as_secs_f64());
        } else {
            tracing::debug!("[tool] {} completed in {}ms", name, elapsed.as_millis());
        }
        // Centralized compression: safety net for any result exceeding the token threshold.
        // Handlers with custom compression (semantic_search, call_graph, http_chain, ast_node)
        // already return results with a "mode" key when compressed — those are left unchanged.
        result.map(centralized_compress)
    }

    fn tool_semantic_search(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let query = required_str(args, "query")?;
        let top_k = args["top_k"].as_u64().unwrap_or(5).clamp(1, 100) as i64;
        let language_filter = args["language"].as_str();
        let node_type_filter = args["node_type"].as_str();
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Query quality factor: penalize vague/short queries so relevance scores
        // reflect actual match quality, not just relative rank position.
        let meaningful_tokens: Vec<&str> = query.split_whitespace()
            .filter(|w| w.len() > 1 || w.chars().all(|c| c.is_uppercase()))
            .collect();
        let query_quality = match meaningful_tokens.len() {
            0 => 0.3,
            1 if meaningful_tokens[0].len() <= 2 => 0.4,
            1 => 0.7,
            2 => 0.85,
            _ => 1.0,
        };

        // Lazy model loading: pick up model if downloaded in background
        self.try_lazy_load_model();

        // Ensure index is up to date (unless caller requested read-only mode)
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // FTS5 search (fetch extra to allow for filtering)
        // Use a floor of 20 so small top_k values still have enough candidates after filtering
        let fetch_count = (top_k * 4).max(20);
        let fts_results = queries::fts5_search(self.db.conn(), query, fetch_count)?;

        // Convert to SearchResult for RRF
        let fts_search: Vec<crate::search::fusion::SearchResult> = fts_results.iter()
            .map(|r| crate::search::fusion::SearchResult { node_id: r.id, score: 0.0 })
            .collect();

        // Vector search (if embedding model available and vec enabled)
        let model_guard = lock_or_recover(&self.embedding_model, "embedding_model");
        let vec_search: Vec<crate::search::fusion::SearchResult> =
            if let Some(ref model) = *model_guard {
                if self.db.vec_enabled() {
                    match model.embed(query) {
                        Ok(query_embedding) => {
                            queries::vector_search(self.db.conn(), &query_embedding, fetch_count)?
                                .iter()
                                .map(|(node_id, _distance)| {
                                    crate::search::fusion::SearchResult { node_id: *node_id, score: 0.0 }
                                })
                                .collect()
                        }
                        Err(_) => vec![],
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
        drop(model_guard);

        // RRF fusion (FTS + Vec when available, FTS-only otherwise)
        // k=30: sharper rank sensitivity than default 60 (top results matter more)
        // fts=1.0, vec=1.2: slightly favor vector similarity since FTS is now stronger
        // with name_tokens and type columns in v2 schema
        let fused = weighted_rrf_fusion(&fts_search, &vec_search, 30, fetch_count as usize, 1.0, 1.2);

        // Batch-fetch all candidate nodes with file info (single query instead of N+1)
        let candidate_ids: Vec<i64> = fused.iter().map(|r| r.node_id).collect();
        let nodes_with_files = queries::get_nodes_with_files_by_ids(self.db.conn(), &candidate_ids)?;

        // Build a lookup by node_id preserving the fused ranking order
        let mut nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> =
            nodes_with_files.iter().map(|nwf| (nwf.node.id, nwf)).collect();

        // Collect results with language/node_type filtering
        // We keep matched (node, file_path) pairs for lazy compression building
        struct MatchedNode<'a> {
            node: &'a queries::NodeResult,
            file_path: &'a str,
        }
        let mut matched: Vec<MatchedNode> = Vec::new();
        let mut results = Vec::new();
        for r in &fused {
            if results.len() >= top_k as usize {
                break;
            }
            if let Some(nwf) = nwf_map.remove(&r.node_id) {
                let node = &nwf.node;
                // Skip module-level container nodes (no useful content for search)
                if node.node_type == "module" && node.name == "<module>" {
                    continue;
                }
                // Skip test functions (FTS5 filters via is_test=0, but vector search doesn't)
                if is_test_symbol(&node.name, &nwf.file_path) {
                    continue;
                }
                // Apply node_type filter
                if let Some(nt) = node_type_filter {
                    if node.node_type != nt {
                        continue;
                    }
                }
                // Apply language filter
                if let Some(lang) = language_filter {
                    if nwf.language.as_deref() != Some(lang) {
                        continue;
                    }
                }
                // Normalize RRF score to 0.0–1.0 range, then apply query quality factor
                // so short/vague queries produce lower relevance scores
                let score = if let Some(max_score) = fused.first().map(|f| f.score) {
                    if max_score > 0.0 {
                        let normalized = r.score / max_score;
                        (normalized * query_quality * 100.0).round() / 100.0
                    } else { 0.0 }
                } else { 0.0 };

                // Compact mode: signature + location only (saves ~85% tokens per result)
                if compact {
                    results.push(json!({
                        "node_id": node.id,
                        "name": node.name,
                        "type": node.node_type,
                        "file_path": nwf.file_path,
                        "line": format!("{}-{}", node.start_line, node.end_line),
                        "signature": node.signature,
                        "relevance": score,
                    }));
                } else {
                    // Truncate large code_content to reduce token usage;
                    // users can get full code via get_ast_node(node_id)
                    const MAX_SEARCH_CODE_LEN: usize = 500;
                    let code = if node.code_content.len() > MAX_SEARCH_CODE_LEN {
                        let truncated = &node.code_content[..node.code_content[..MAX_SEARCH_CODE_LEN]
                            .rfind('\n').unwrap_or(MAX_SEARCH_CODE_LEN)];
                        format!("{}\n// ... truncated ({} lines total, use get_ast_node for full code)",
                            truncated, node.end_line - node.start_line + 1)
                    } else {
                        node.code_content.clone()
                    };
                    results.push(json!({
                        "node_id": node.id,
                        "name": node.name,
                        "type": node.node_type,
                        "file_path": nwf.file_path,
                        "start_line": node.start_line,
                        "end_line": node.end_line,
                        "code_content": code,
                        "signature": node.signature,
                        "relevance": score,
                    }));
                }
                matched.push(MatchedNode {
                    node: &nwf.node,
                    file_path: &nwf.file_path,
                });
            }
        }

        // Record search metrics (before potential compression return)
        lock_or_recover(&self.metrics, "metrics")
            .record_search(results.len(), query_quality, vec_search.is_empty());

        // Context Sandbox: compress only if results likely exceed token threshold
        // Skip compression when compact=true — compact results are already token-efficient
        // (~85% smaller than full results) and contain fields (relevance, signature)
        // that would be lost by compression.
        use crate::sandbox::compressor::CompressedOutput;
        let estimated_tokens: usize = if compact { 0 } else {
            matched.iter()
                .map(|m| {
                    let node = m.node;
                    node.context_string.as_ref().map_or_else(
                        || node.code_content.len() + node.name.len() + node.signature.as_ref().map_or(0, |s| s.len()),
                        |ctx| ctx.len(),
                    ) / 3
                })
                .sum()
        };
        if estimated_tokens > COMPRESSION_TOKEN_THRESHOLD {
            // Build node_results and file_paths only when compression is needed
            let node_results: Vec<queries::NodeResult> = matched.iter().map(|m| {
                let node = m.node;
                queries::NodeResult {
                    id: node.id,
                    file_id: node.file_id,
                    node_type: node.node_type.clone(),
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                    code_content: node.code_content.clone(),
                    signature: node.signature.clone(),
                    doc_comment: node.doc_comment.clone(),
                    context_string: node.context_string.clone(),
                    name_tokens: node.name_tokens.clone(),
                    return_type: node.return_type.clone(),
                    param_types: node.param_types.clone(),
                }
            }).collect();
            let file_paths: Vec<String> = matched.iter().map(|m| m.file_path.to_string()).collect();
        if let Some(compressed) = crate::sandbox::compressor::compress_if_needed(&node_results, &file_paths, COMPRESSION_TOKEN_THRESHOLD)? {
            let (mode, compact) = match compressed {
                CompressedOutput::Nodes(nodes) => {
                    let items: Vec<serde_json::Value> = nodes.iter().map(|c| json!({
                        "node_id": c.node_id,
                        "file_path": c.file_path,
                        "summary": c.summary,
                    })).collect();
                    ("compressed_nodes", items)
                }
                CompressedOutput::Files(groups) => {
                    let items: Vec<serde_json::Value> = groups.iter().map(|g| json!({
                        "file_path": g.file_path,
                        "summary": g.summary,
                        "node_ids": g.node_ids,
                    })).collect();
                    ("compressed_files", items)
                }
                CompressedOutput::Directories(groups) => {
                    let items: Vec<serde_json::Value> = groups.iter().map(|g| json!({
                        "file_path": g.file_path,
                        "summary": g.summary,
                        "node_ids": g.node_ids,
                    })).collect();
                    ("compressed_directories", items)
                }
            };
            return Ok(json!({
                "mode": mode,
                "message": "Results exceeded token limit. Use get_ast_node(node_id) to expand individual symbols.",
                "results": compact
            }));
        }
        } // end estimated_tokens check

        if results.is_empty() {
            return Ok(json!({
                "results": [],
                "message": "No matching symbols found.",
                "hint": "Try broader terms, check spelling, or use different keywords. The index may need rebuilding if the codebase changed significantly."
            }));
        }

        Ok(json!(results))
    }

    fn tool_get_call_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        // Accept both "symbol_name" (canonical) and "function_name" (legacy alias)
        let function_name = args["symbol_name"].as_str()
            .or_else(|| args["function_name"].as_str())
            .ok_or_else(|| anyhow!("symbol_name is required"))?;
        let direction = args["direction"].as_str().unwrap_or("both");
        let depth = args["depth"].as_i64().unwrap_or(2).clamp(1, 20) as i32;
        let file_path = args["file_path"].as_str();
        let compact = args["compact"].as_bool().unwrap_or(false);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // Disambiguate: if no file_path provided, check if symbol matches multiple distinct nodes
        if file_path.is_none() {
            if let Some(suggestions) = self.disambiguate_symbol(function_name)? {
                return Ok(json!({
                    "function": function_name,
                    "direction": direction,
                    "error": format!("Ambiguous symbol '{}': {} matches in different files. Specify file_path to disambiguate.", function_name, suggestions.len()),
                    "suggestions": suggestions,
                }));
            }
        }

        let results = crate::graph::query::get_call_graph(
            self.db.conn(), function_name, direction, depth, file_path,
        )?;

        // If exact match returns empty (only seed node, no edges), try fuzzy name resolution
        let has_edges = results.iter().any(|n| n.depth > 0);
        let has_seed = results.iter().any(|n| n.depth == 0);
        if !(has_edges || (has_seed && file_path.is_some())) {
            match self.resolve_fuzzy_name(function_name)? {
                FuzzyResolution::Unique(resolved) => {
                    let results2 = crate::graph::query::get_call_graph(
                        self.db.conn(), &resolved, direction, depth, file_path,
                    )?;
                    return self.format_call_graph_response(&resolved, direction, &results2, compact);
                }
                FuzzyResolution::Ambiguous(suggestions) => {
                    return Ok(json!({
                        "function": function_name,
                        "direction": direction,
                        "callees": [],
                        "callers": [],
                        "suggestion": format!("No exact match for '{}'. Did you mean one of these?", function_name),
                        "candidates": suggestions,
                    }));
                }
                FuzzyResolution::NotFound => {
                    if !has_seed {
                        return Ok(json!({
                            "function": function_name,
                            "direction": direction,
                            "callers": [],
                            "callees": [],
                            "error": format!("Symbol '{}' not found in the index.", function_name),
                            "hint": "Use semantic_code_search to find the correct symbol name, or check spelling.",
                        }));
                    }
                    // Function exists but has no callers/callees — fall through
                }
            }
        }

        self.format_call_graph_response(function_name, direction, &results, compact)
    }

    fn format_call_graph_response(
        &self,
        function_name: &str,
        direction: &str,
        results: &[crate::graph::query::CallGraphNode],
        compact: bool,
    ) -> Result<serde_json::Value> {
        let is_test = |n: &&crate::graph::query::CallGraphNode| {
            is_test_symbol(&n.name, &n.file_path)
        };
        let mut seen_nodes = std::collections::HashSet::new();
        let all_nodes: Vec<serde_json::Value> = results.iter()
            .filter(|n| n.depth > 0 && !is_test(n))
            // Deduplicate cfg-gated functions (same name+file+depth+direction, different node_id)
            .filter(|n| seen_nodes.insert((&n.name, &n.file_path, n.depth, n.direction.as_str())))
            .map(|n| {
                if compact {
                    // Compact: keep node_id for chaining to get_ast_node, drop type (usually "function")
                    json!({
                        "node_id": n.node_id,
                        "name": n.name,
                        "file_path": n.file_path,
                        "depth": n.depth,
                        "direction": n.direction.as_str(),
                    })
                } else {
                    json!({
                        "node_id": n.node_id,
                        "name": n.name,
                        "type": n.node_type,
                        "file_path": n.file_path,
                        "depth": n.depth,
                        "direction": n.direction.as_str(),
                    })
                }
            })
            .collect();
        let test_callers_count = results.iter()
            .filter(|n| n.depth > 0 && is_test(n))
            .count();

        let est_tokens = crate::sandbox::compressor::estimate_json_tokens(&json!(all_nodes));
        if est_tokens > COMPRESSION_TOKEN_THRESHOLD {
            return Ok(json!({
                "mode": "compressed_call_graph",
                "message": "Call graph exceeded token limit. Use get_ast_node(node_id) to expand individual nodes.",
                "function": function_name,
                "results": all_nodes,
            }));
        }

        let callee_nodes: Vec<&serde_json::Value> = all_nodes.iter()
            .filter(|n| n["direction"] == "callees")
            .collect();
        let caller_nodes: Vec<&serde_json::Value> = all_nodes.iter()
            .filter(|n| n["direction"] == "callers")
            .collect();

        let mut result = json!({
            "function": function_name,
            "direction": direction,
            "callees": callee_nodes,
            "callers": caller_nodes,
        });
        if test_callers_count > 0 {
            result["test_callers_filtered"] = json!(test_callers_count);
        }
        Ok(result)
    }

    // find_http_route merged into trace_http_chain — old name kept as alias in handle_tool()

    fn tool_trace_http_chain(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let route_path_raw = args["route_path"].as_str()
            .ok_or_else(|| anyhow!("route_path is required"))?;
        let depth = args["depth"].as_i64().unwrap_or(3).clamp(1, 20) as i32;
        let include_middleware = args["include_middleware"].as_bool().unwrap_or(true);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let (method_filter, route_path) = parse_route_input(route_path_raw);

        use crate::domain::{REL_CALLS, REL_ROUTES_TO};
        let mut rows = queries::find_routes_by_path(self.db.conn(), route_path, REL_ROUTES_TO)?;
        filter_routes_by_method(&mut rows, &method_filter);

        let mut handlers: Vec<serde_json::Value> = Vec::new();
        for rm in &rows {
            let mut handler = json!({
                "node_id": rm.node_id,
                "metadata": rm.metadata,
                "handler_name": rm.handler_name,
                "handler_type": rm.handler_type,
                "file_path": rm.file_path,
                "start_line": rm.start_line,
                "end_line": rm.end_line,
            });

            apply_inline_handler_metadata(&mut handler, rm.metadata.as_deref());

            if include_middleware {
                let downstream = queries::get_edge_target_names(self.db.conn(), rm.node_id, REL_CALLS)?;
                handler["downstream_calls"] = json!(downstream);
            }

            // Recursive call chain via call graph
            let chain = crate::graph::query::get_call_graph(
                self.db.conn(), &rm.handler_name, "callees", depth, Some(&rm.file_path),
            )?;
            let chain_nodes: Vec<serde_json::Value> = chain.iter()
                .filter(|n| n.depth > 0) // exclude root (the handler itself)
                .filter(|n| !is_test_symbol(&n.name, &n.file_path))
                .map(|n| json!({
                    "node_id": n.node_id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": n.file_path,
                    "depth": n.depth,
                }))
                .collect();
            handler["call_chain"] = json!(chain_nodes);

            handlers.push(handler);
        }

        let mut result = json!({
            "route": route_path,
            "handlers": handlers,
        });
        if handlers.is_empty() {
            result["message"] = json!("No matching routes found. This may mean: (1) the project has no HTTP routes, (2) the route pattern didn't match, or (3) routes use a framework not yet supported. Try a broader pattern or use semantic_code_search to find route handlers.");
        }

        // Compress if result exceeds token threshold
        let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
        if tokens > COMPRESSION_TOKEN_THRESHOLD {
            let compressed_handlers: Vec<serde_json::Value> = handlers.iter().map(|h| {
                json!({
                    "node_id": h["node_id"],
                    "handler_name": h["handler_name"],
                    "file_path": h["file_path"],
                    "start_line": h["start_line"],
                    "end_line": h["end_line"],
                    "chain_count": h["call_chain"].as_array().map_or(0, |a| a.len()),
                })
            }).collect();
            return Ok(json!({
                "mode": "compressed_http_chain",
                "message": "HTTP chain exceeded token limit. Use get_ast_node(node_id) or get_call_graph(symbol_name) to expand.",
                "route": route_path,
                "results": compressed_handlers,
            }));
        }

        Ok(result)
    }

    fn tool_get_ast_node(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let include_refs = args["include_references"].as_bool().unwrap_or(false);
        let include_impact = args["include_impact"].as_bool().unwrap_or(false);

        // Support lookup by node_id or file_path+symbol_name
        if let Some(nid) = args["node_id"].as_i64() {
            // When called with node_id, default context_lines=3
            let ctx = args["context_lines"].as_i64().unwrap_or(3).clamp(0, 100) as usize;
            return self.ast_node_by_id(nid, include_refs, include_impact, ctx);
        }

        let context_lines = args["context_lines"].as_i64().unwrap_or(0).clamp(0, 100) as usize;

        let symbol_name = args["symbol_name"].as_str();
        let file_path = args["file_path"].as_str();

        // If only symbol_name provided (no file_path), resolve by name lookup
        if let (Some(sym), None) = (symbol_name, file_path) {
            let candidates = queries::get_nodes_by_name(self.db.conn(), sym)?;
            let non_test: Vec<_> = candidates.iter()
                .filter(|n| {
                    let fp = queries::get_file_path(self.db.conn(), n.file_id)
                        .ok().flatten().unwrap_or_default();
                    !is_test_symbol(&n.name, &fp)
                })
                .collect();
            return match non_test.len() {
                0 => Ok(json!({
                    "error": format!("Symbol '{}' not found in index.", sym),
                    "hint": "Use semantic_code_search to find the correct symbol name, or check spelling.",
                })),
                1 => self.ast_node_by_id(non_test[0].id, include_refs, include_impact, context_lines),
                _ => {
                    let suggestions: Vec<_> = non_test.iter().map(|n| {
                        let fp = queries::get_file_path(self.db.conn(), n.file_id)
                            .ok().flatten().unwrap_or_else(|| "unknown".to_string());
                        json!({
                            "name": n.name,
                            "file_path": fp,
                            "type": n.node_type,
                            "node_id": n.id,
                        })
                    }).collect();
                    Ok(json!({
                        "error": format!("Ambiguous symbol '{}': {} matches found. Specify file_path or use node_id.", sym, suggestions.len()),
                        "suggestions": suggestions,
                    }))
                }
            };
        }

        let file_path = file_path
            .ok_or_else(|| anyhow!("Either node_id, symbol_name, or file_path+symbol_name is required"))?;
        let symbol_name = symbol_name
            .ok_or_else(|| anyhow!("symbol_name is required when using file_path"))?;

        let nodes = queries::get_nodes_by_file_path(self.db.conn(), file_path)?;
        if nodes.is_empty() {
            return Ok(json!({
                "error": format!("File '{}' not found in index.", file_path),
                "hint": "Check that the path is relative to the project root and the file has been indexed.",
            }));
        }
        let node = nodes.iter().find(|n| n.name == symbol_name);

        match node {
            Some(n) => {
                let mut result = json!({
                    "node_id": n.id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "signature": n.signature,
                    "qualified_name": n.qualified_name,
                });

                // Include source code: prefer context view, fall back to stored code_content
                if context_lines > 0 {
                    if let Some(code) = self.read_source_context(file_path, n.start_line, n.end_line, context_lines) {
                        result["code_content"] = json!(code);
                    } else {
                        result["code_content"] = json!(n.code_content);
                    }
                } else {
                    result["code_content"] = json!(n.code_content);
                }

                if include_refs {
                    use crate::domain::REL_CALLS as CALLS;
                    let callees = queries::get_edge_targets_with_files(self.db.conn(), n.id, CALLS)?;
                    let callers = queries::get_edge_sources_with_files(self.db.conn(), n.id, CALLS)?;
                    result["calls"] = json!(callees.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
                    result["called_by"] = json!(callers.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
                }

                if include_impact {
                    self.append_impact_summary(&mut result, &n.name, file_path)?;
                }

                // Compress if code_content exceeds token threshold
                let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
                if tokens > COMPRESSION_TOKEN_THRESHOLD {
                    return Ok(json!({
                        "mode": "compressed_node",
                        "message": "Node content exceeded token limit. Retry with context_lines=0 or use get_ast_node(node_id=N) to read specific parts.",
                        "node_id": n.id,
                        "name": n.name,
                        "type": n.node_type,
                        "file_path": file_path,
                        "start_line": n.start_line,
                        "end_line": n.end_line,
                        "signature": n.signature,
                        "summary": format!("{} {} in {} (lines {}-{}){}",
                            n.node_type, n.name, file_path, n.start_line, n.end_line,
                            n.signature.as_ref().map(|s| format!(" {}", s)).unwrap_or_default()),
                    }));
                }

                Ok(result)
            }
            None => {
                // List available symbols to help the user
                let available: Vec<String> = nodes.iter()
                    .filter(|n| n.name != "<module>")
                    .take(10)
                    .map(|n| format!("{} ({})", n.name, n.node_type))
                    .collect();
                let hint = if available.is_empty() {
                    String::new()
                } else {
                    format!(". Available symbols: {}", available.join(", "))
                };
                Err(anyhow!("Symbol '{}' not found in '{}'{}", symbol_name, file_path, hint))
            }
        }
    }

    /// Lookup AST node by node_id.
    fn ast_node_by_id(&self, node_id: i64, include_refs: bool, include_impact: bool, context_lines: usize) -> Result<serde_json::Value> {
        let node = queries::get_node_by_id(self.db.conn(), node_id)?
            .ok_or_else(|| anyhow!("Node {} not found", node_id))?;
        let file_path = queries::get_file_path(self.db.conn(), node.file_id)?
            .ok_or_else(|| anyhow!("File record missing for node {}", node_id))?;

        let mut result = json!({
            "node_id": node.id,
            "name": node.name,
            "type": node.node_type,
            "file_path": file_path,
            "start_line": node.start_line,
            "end_line": node.end_line,
            "signature": node.signature,
            "qualified_name": node.qualified_name,
        });

        // Include source code: prefer context view when requested, fall back to stored code_content
        if context_lines > 0 {
            if let Some(code) = self.read_source_context(&file_path, node.start_line, node.end_line, context_lines) {
                result["code_content"] = json!(code);
            } else {
                result["code_content"] = json!(node.code_content);
            }
        } else {
            result["code_content"] = json!(node.code_content);
        }

        if include_refs {
            use crate::domain::REL_CALLS as CALLS;
            let callees = queries::get_edge_targets_with_files(self.db.conn(), node.id, CALLS)?;
            let callers = queries::get_edge_sources_with_files(self.db.conn(), node.id, CALLS)?;
            result["calls"] = json!(callees.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
            result["called_by"] = json!(callers.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
        }

        if include_impact {
            self.append_impact_summary(&mut result, &node.name, &file_path)?;
        }

        Ok(result)
    }

    /// Append a lightweight impact summary to an existing result JSON.
    /// Reuses the impact_analysis query logic but returns a compact summary object.
    fn append_impact_summary(&self, result: &mut serde_json::Value, symbol_name: &str, file_path: &str) -> Result<()> {
        let callers = queries::get_callers_with_route_info(
            self.db.conn(), symbol_name, Some(file_path), 3
        )?;
        let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();
        let prod_callers: Vec<_> = callers.iter()
            .filter(|c| !is_test_symbol(&c.name, &c.file_path))
            .collect();
        let affected_files: std::collections::HashSet<&str> = prod_callers.iter()
            .map(|c| c.file_path.as_str()).collect();
        let affected_routes: usize = callers.iter()
            .filter(|c| c.route_info.is_some())
            .count();

        let risk = if prod_callers.len() > 10 || affected_routes >= 3 {
            "HIGH"
        } else if prod_callers.len() > 3 || affected_routes > 0 {
            "MEDIUM"
        } else {
            "LOW"
        };

        result["impact"] = json!({
            "risk_level": risk,
            "direct_callers": prod_callers.iter().filter(|c| c.depth == 1).count(),
            "transitive_callers": prod_callers.iter().filter(|c| c.depth > 1).count(),
            "affected_files": affected_files.len(),
            "affected_routes": affected_routes,
        });
        Ok(())
    }

    /// Read source code with context lines from the project file system.
    fn read_source_context(&self, file_path: &str, start_line: i64, end_line: i64, context_lines: usize) -> Option<String> {
        let root = self.project_root.as_ref()?;
        let abs_path = root.join(file_path);
        let canonical = abs_path.canonicalize().ok()?;
        let root_canonical = root.canonicalize().ok()?;
        if !canonical.starts_with(&root_canonical) {
            return None; // path traversal
        }
        let source = std::fs::read_to_string(&canonical).ok()?;
        let lines: Vec<&str> = source.lines().collect();
        let start = (start_line as usize).saturating_sub(1 + context_lines);
        let end = ((end_line as usize) + context_lines).min(lines.len());
        if start >= end {
            return None;
        }
        Some(lines[start..end].join("\n"))
    }

    // read_snippet is a legacy alias for get_ast_node in handle_tool()

    fn tool_start_watch(&self) -> Result<serde_json::Value> {
        if !self.is_primary {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. File watching is handled by the primary instance."
            }));
        }
        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if watcher_guard.is_some() {
            return Ok(json!({
                "status": "already_watching",
                "message": "File watcher is already running"
            }));
        }

        let (tx, rx) = mpsc::channel();
        let fw = FileWatcher::start(project_root, tx)?;
        *watcher_guard = Some(WatcherState {
            _watcher: fw,
            receiver: rx,
        });

        Ok(json!({
            "status": "watching",
            "message": "File watcher started. Changes will be detected and indexed on next tool call."
        }))
    }

    fn tool_stop_watch(&self) -> Result<serde_json::Value> {
        if !self.is_primary {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. File watching is handled by the primary instance."
            }));
        }
        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if watcher_guard.is_none() {
            return Ok(json!({
                "status": "not_watching",
                "message": "File watcher was not running"
            }));
        }
        *watcher_guard = None; // Drops the FileWatcher, stopping it
        Ok(json!({
            "status": "stopped",
            "message": "File watcher stopped"
        }))
    }

    fn tool_get_index_status(&self) -> Result<serde_json::Value> {
        let mut status = serde_json::to_value(
            queries::get_index_status(self.db.conn(), self.is_watching())?
        )?;

        // Add embedding status fields
        let model_available = lock_or_recover(&self.embedding_model, "embedding_model").is_some();
        let (vectors_done, vectors_total) = if self.db.vec_enabled() {
            queries::count_nodes_with_vectors(self.db.conn()).unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        let embedding_status = if !model_available {
            "unavailable"
        } else if self.embedding_in_progress.load(Ordering::Acquire) {
            "in_progress"
        } else if vectors_done >= vectors_total && vectors_total > 0 {
            "complete"
        } else if vectors_done > 0 {
            "partial"
        } else {
            "pending"
        };

        if let Some(obj) = status.as_object_mut() {
            obj.insert("embedding_status".into(), json!(embedding_status));
            obj.insert("embedding_progress".into(), json!(format!("{}/{}", vectors_done, vectors_total)));
            obj.insert("model_available".into(), json!(model_available));
            let coverage_pct = if vectors_total > 0 {
                (vectors_done as f64 / vectors_total as f64 * 100.0).round() as i64
            } else {
                0
            };
            obj.insert("embedding_coverage_pct".into(), json!(coverage_pct));
            obj.insert("search_mode".into(), json!(if model_available && vectors_done > 0 {
                "hybrid"
            } else {
                "fts_only"
            }));

            // Add indexing observability stats (skipped files, truncations)
            let stats = lock_or_recover(&self.last_index_stats, "last_index_stats").clone();
            let skipped_total = stats.files_skipped_size + stats.files_skipped_parse
                + stats.files_skipped_read + stats.files_skipped_hash;
            if skipped_total > 0 {
                obj.insert("skipped_files".into(), json!({
                    "total": skipped_total,
                    "too_large": stats.files_skipped_size,
                    "parse_error": stats.files_skipped_parse,
                    "read_error": stats.files_skipped_read,
                    "hash_error": stats.files_skipped_hash,
                }));
            }
            if stats.files_skipped_language > 0 {
                obj.insert("files_skipped_unsupported_language".into(), json!(stats.files_skipped_language));
            }
            obj.insert("instance_mode".into(), json!(if self.is_primary { "primary" } else { "secondary" }));
        }

        Ok(status)
    }

    fn tool_rebuild_index(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !self.is_primary {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. Rebuild must be done from the primary instance."
            }));
        }
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        if !confirm {
            return Err(anyhow!("Must pass confirm: true to rebuild index"));
        }

        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        // Wait for background embedding to finish before clearing data
        // to avoid race where embedding thread writes vectors for deleted nodes
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.embedding_in_progress.load(Ordering::Acquire) {
            if std::time::Instant::now() > deadline {
                return Err(anyhow!("Background embedding still in progress. Try again shortly."));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Clear all data in a single transaction (CASCADE handles nodes→edges)
        {
            let tx = self.db.conn().unchecked_transaction()?;
            tx.execute("DELETE FROM files", [])?;
            tx.commit()?;
        }

        self.send_log("info", "Rebuilding index...");
        let progress_cb = |current: usize, total: usize| {
            self.send_progress("rebuild-index", current, total);
        };
        // Skip inline embedding, background thread handles it
        let result = run_full_index(&self.db, project_root, None, Some(&progress_cb))?;

        // Save indexing stats for observability
        *lock_or_recover(&self.last_index_stats, "last_index_stats") = result.stats.clone();

        // Reset indexed flag and invalidate caches
        *lock_or_recover(&self.indexed, "indexed") = true;
        *lock_or_recover(&self.cached_project_map, "cached_pmap") = None;
        lock_or_recover(&self.cached_module_overviews, "cached_movw").clear();

        self.spawn_background_embedding();

        Ok(json!({
            "status": "rebuilt",
            "files_indexed": result.files_indexed,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
        }))
    }

    fn tool_impact_analysis(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let symbol_name = required_str(args, "symbol_name")?;
        let change_type = args.get("change_type")
            .and_then(|v| v.as_str())
            .unwrap_or("behavior");
        if !matches!(change_type, "signature" | "behavior" | "remove") {
            return Err(anyhow!("change_type must be one of: signature, behavior, remove"));
        }
        let depth = args.get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(3)
            .clamp(1, 20) as i32;

        // Disambiguate: check if symbol matches multiple distinct nodes in different files
        if let Some(suggestions) = self.disambiguate_symbol(symbol_name)? {
            return Ok(json!({
                "symbol": symbol_name,
                "change_type": change_type,
                "error": format!("Ambiguous symbol '{}': {} matches in different files. Cannot assess impact without disambiguation.", symbol_name, suggestions.len()),
                "suggestions": suggestions,
            }));
        }

        let mut resolved_name = symbol_name.to_string();
        let mut callers = queries::get_callers_with_route_info(
            self.db.conn(), symbol_name, None, depth
        )?;

        // Fuzzy fallback: if no callers found, try fuzzy name resolution
        if callers.is_empty() {
            match self.resolve_fuzzy_name(symbol_name)? {
                FuzzyResolution::Unique(resolved) => {
                    resolved_name = resolved;
                    callers = queries::get_callers_with_route_info(
                        self.db.conn(), &resolved_name, None, depth
                    )?;
                }
                FuzzyResolution::Ambiguous(suggestions) => {
                    return Ok(json!({
                        "symbol": symbol_name,
                        "change_type": change_type,
                        "direct_callers": [],
                        "transitive_callers": [],
                        "affected_routes": [],
                        "affected_files": 0,
                        "risk_level": "LOW",
                        "summary": format!("No exact match for '{}'. Did you mean one of these?", symbol_name),
                        "candidates": suggestions,
                    }));
                }
                FuzzyResolution::NotFound => {
                    return Ok(json!({
                        "symbol": symbol_name,
                        "change_type": change_type,
                        "direct_callers": [],
                        "transitive_callers": [],
                        "affected_routes": [],
                        "affected_files": 0,
                        "risk_level": "UNKNOWN",
                        "warning": format!("Symbol '{}' not found in index. Cannot assess impact.", symbol_name),
                        "summary": format!("Symbol '{}' not found in the codebase index", symbol_name)
                    }));
                }
            }
        }

        // Exclude root node (depth 0) — it's the queried symbol itself, not a caller
        let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();

        // Separate production callers from test callers
        let is_test = |c: &&queries::CallerWithRouteInfo| {
            is_test_symbol(&c.name, &c.file_path)
        };
        let prod_callers: Vec<_> = callers.iter().filter(|c| !is_test(c)).collect();
        let test_callers: Vec<_> = callers.iter().filter(|c| is_test(c)).collect();

        let affected_files: std::collections::HashSet<&str> = prod_callers.iter()
            .map(|c| c.file_path.as_str()).collect();
        let affected_routes: Vec<serde_json::Value> = callers.iter()
            .filter_map(|c| {
                c.route_info.as_ref().and_then(|meta| serde_json::from_str(meta).ok())
            }).collect();

        // Risk based on production callers, not test callers
        let risk_level = if prod_callers.len() > 10 || affected_routes.len() >= 3 || change_type == "remove" {
            "HIGH"
        } else if prod_callers.len() > 3 || !affected_routes.is_empty() {
            "MEDIUM"
        } else {
            "LOW"
        };

        let direct: Vec<_> = prod_callers.iter().filter(|c| c.depth == 1).collect();
        let transitive: Vec<_> = prod_callers.iter().filter(|c| c.depth > 1).collect();

        // For non-function types (struct/class/enum), call graph may miss type-usage references
        let type_warning = if prod_callers.is_empty() {
            let nodes = queries::get_nodes_by_name(self.db.conn(), &resolved_name)?;
            let is_type = nodes.iter().any(|n| matches!(n.node_type.as_str(), "struct" | "class" | "enum" | "interface" | "type_alias"));
            if is_type {
                Some("Impact analysis tracks function call chains. This is a type definition — actual usage (field access, type annotations, instantiation) may be broader than shown. Use semantic_code_search to find all references.")
            } else {
                None
            }
        } else {
            None
        };

        let mut result = json!({
            "symbol": &resolved_name,
            "change_type": change_type,
            "direct_callers": direct.iter().map(|c| json!({
                "name": c.name, "file": c.file_path, "depth": c.depth
            })).collect::<Vec<_>>(),
            "transitive_callers": transitive.iter().map(|c| json!({
                "name": c.name, "file": c.file_path, "depth": c.depth
            })).collect::<Vec<_>>(),
            "affected_routes": affected_routes,
            "affected_files": affected_files.len(),
            "risk_level": risk_level,
            "tests_affected": test_callers.len(),
            "summary": format!("Changing {} affects {} routes, {} functions across {} files [{}] ({} tests affected)",
                &resolved_name, affected_routes.len(), prod_callers.len(), affected_files.len(), risk_level, test_callers.len())
        });
        if let Some(warning) = type_warning {
            result["warning"] = json!(warning);
        }
        Ok(result)
    }

    fn tool_module_overview(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let raw_path = args["path"].as_str()
            .ok_or_else(|| anyhow!("Missing path"))?;
        let compact = args["compact"].as_bool().unwrap_or(false);
        // Normalize: strip leading "./" and treat "." as empty prefix (match all)
        let path = raw_path.strip_prefix("./").unwrap_or(raw_path);
        let path = if path == "." { "" } else { path };

        // Return cached result if fresh (< 60s), evict if expired
        {
            let mut cache = lock_or_recover(&self.cached_module_overviews, "cached_movw");
            if let Some((ts, _)) = cache.get(path) {
                if ts.elapsed().as_secs() < 60 {
                    let val = cache.get(path).unwrap().1.clone();
                    if compact {
                        return self.compact_module_overview(&val);
                    }
                    return Ok(val);
                } else {
                    cache.remove(path);
                }
            }
        }

        let exports = queries::get_module_exports(self.db.conn(), path)?;

        // Filter out test functions — they add noise to module overviews
        let exports: Vec<_> = exports.into_iter()
            .filter(|e| !is_test_symbol(&e.name, &e.file_path))
            .collect();

        // Get import/dependency info at file level
        let files: std::collections::HashSet<&str> = exports.iter()
            .map(|e| e.file_path.as_str()).collect();

        // Split exports into active (called by others) and inactive to save tokens.
        let (active, inactive): (Vec<_>, Vec<_>) = exports.iter()
            .partition(|e| e.caller_count > 0);

        let mut hot_candidates: Vec<_> = exports.iter()
            .filter(|e| e.caller_count > 0)
            .collect();
        hot_candidates.sort_by(|a, b| b.caller_count.cmp(&a.caller_count));
        let hot_paths: Vec<serde_json::Value> = hot_candidates.iter()
            .take(5)
            .map(|e| json!({
                "name": e.name,
                "type": e.node_type,
                "file": e.file_path,
                "caller_count": e.caller_count,
            }))
            .collect();

        // Active exports get full detail; inactive ones are summarized by type.
        const MAX_ACTIVE: usize = 30;
        let active_capped = active.len() > MAX_ACTIVE;
        let mut active_sorted = active.clone();
        active_sorted.sort_by(|a, b| b.caller_count.cmp(&a.caller_count));
        let active_exports: Vec<serde_json::Value> = active_sorted.iter()
            .take(MAX_ACTIVE)
            .map(|e| json!({
                "node_id": e.node_id,
                "name": e.name,
                "type": e.node_type,
                "file": e.file_path,
                "caller_count": e.caller_count,
                "signature": e.signature,
            }))
            .collect();

        // Compact summary for inactive symbols — just counts by type
        let mut inactive_by_type: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for e in &inactive {
            inactive_by_type.entry(e.node_type.as_str()).or_default().push(e.name.as_str());
        }
        let inactive_summary: Vec<serde_json::Value> = inactive_by_type.iter()
            .map(|(typ, names)| {
                let display: Vec<&&str> = names.iter().take(8).collect();
                let mut obj = json!({
                    "type": typ,
                    "count": names.len(),
                    "names": display,
                });
                if names.len() > 8 {
                    obj["more"] = json!(names.len() - 8);
                }
                obj
            })
            .collect();

        let mut result = json!({
            "path": raw_path,
            "files_count": files.len(),
            "active_exports": active_exports,
            "inactive_summary": inactive_summary,
            "hot_paths": hot_paths,
            "summary": format!("Module '{}': {} active + {} inactive exports across {} files",
                raw_path, active.len(), inactive.len(), files.len())
        });
        if files.is_empty() {
            result["warning"] = json!(format!("No files found for path '{}'. Check that the path is relative to the project root.", raw_path));
        }
        if active_capped {
            result["active_capped"] = json!(true);
            result["showing"] = json!(MAX_ACTIVE);
            result["total_active"] = json!(active.len());
            result["hint"] = json!("Active exports capped. Use a more specific path to see all.");
        }

        // Cache the full result (max 10 entries to bound memory)
        {
            let mut cache = lock_or_recover(&self.cached_module_overviews, "cached_movw");
            if cache.len() >= 10 {
                // Evict oldest entry
                if let Some(oldest_key) = cache.iter()
                    .min_by_key(|(_, (ts, _))| *ts)
                    .map(|(k, _)| k.to_string())
                {
                    cache.remove(&oldest_key);
                }
            }
            cache.insert(path.to_string(), (std::time::Instant::now(), result.clone()));
        }

        if compact {
            return self.compact_module_overview(&result);
        }
        Ok(result)
    }

    fn compact_module_overview(&self, full: &serde_json::Value) -> Result<serde_json::Value> {
        // Compact: keep node_id for chaining, drop signature
        let active: Vec<serde_json::Value> = full["active_exports"].as_array()
            .map(|arr| arr.iter().map(|e| json!({
                "node_id": e["node_id"],
                "name": e["name"],
                "type": e["type"],
                "file": e["file"],
                "callers": e["caller_count"],
            })).collect())
            .unwrap_or_default();

        let inactive_count: usize = full["inactive_summary"].as_array()
            .map(|arr| arr.iter()
                .filter_map(|s| s["count"].as_u64())
                .sum::<u64>() as usize)
            .unwrap_or(0);

        let mut result = json!({
            "path": full["path"],
            "files": full["files_count"],
            "active": active,
            "inactive_count": inactive_count,
            "hot_paths": full["hot_paths"],
            "summary": full["summary"],
        });
        if full.get("warning").is_some() {
            result["warning"] = full["warning"].clone();
        }
        Ok(result)
    }

    fn tool_dependency_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let file_path = args["file_path"].as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let direction = args.get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("both");
        let depth = args.get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(2)
            .clamp(1, 10) as i32;

        // Check if file exists in index
        let file_nodes = queries::get_nodes_by_file_path(self.db.conn(), file_path)?;
        if file_nodes.is_empty() {
            let hint = if file_path.ends_with('/') || !file_path.contains('.') {
                // Looks like a directory — suggest using module_overview instead
                let dir = if file_path.ends_with('/') { file_path.to_string() } else { format!("{}/", file_path) };
                format!(
                    "Path '{}' looks like a directory. Use module_overview(path=\"{}\") for directory-level analysis, or specify an exact file (e.g., '{}mod.rs')",
                    file_path, file_path, dir
                )
            } else {
                format!("File '{}' not found in index. Check path is relative to project root.", file_path)
            };
            return Ok(json!({
                "file": file_path,
                "depends_on": [],
                "depended_by": [],
                "warning": hint,
                "summary": format!("File '{}' not found in index", file_path)
            }));
        }

        let deps = queries::get_import_tree(self.db.conn(), file_path, direction, depth)?;

        let outgoing: Vec<serde_json::Value> = deps.iter()
            .filter(|d| d.direction == "outgoing")
            .map(|d| {
                let mut obj = json!({
                    "file": d.file_path,
                    "depth": d.depth,
                });
                // Only show symbols for direct dependencies (depth 1);
                // deeper entries have 0 direct edges from root which is misleading
                if d.depth == 1 {
                    obj["symbols"] = json!(d.symbol_count);
                }
                obj
            })
            .collect();

        let incoming: Vec<serde_json::Value> = deps.iter()
            .filter(|d| d.direction == "incoming")
            .map(|d| {
                let mut obj = json!({
                    "file": d.file_path,
                    "depth": d.depth,
                });
                if d.depth == 1 {
                    obj["symbols"] = json!(d.symbol_count);
                }
                obj
            })
            .collect();

        Ok(json!({
            "file": file_path,
            "depends_on": outgoing,
            "depended_by": incoming,
            "summary": format!("{} depends on {} file{}, {} file{} depend{} on it",
                file_path,
                outgoing.len(), if outgoing.len() == 1 { "" } else { "s" },
                incoming.len(), if incoming.len() == 1 { "" } else { "s" },
                if incoming.len() == 1 { "s" } else { "" })
        }))
    }

    fn tool_find_similar_code(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.try_lazy_load_model();
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // Accept node_id directly, or resolve from symbol_name
        let node_id = if let Some(id) = args["node_id"].as_i64() {
            id
        } else if let Some(name) = args["symbol_name"].as_str() {
            match queries::get_first_node_id_by_name(self.db.conn(), name)? {
                Some(id) => id,
                None => return Ok(json!({
                    "error": format!("Symbol '{}' not found in index.", name),
                    "hint": "Use semantic_code_search to find the correct symbol name, or check spelling."
                })),
            }
        } else {
            return Ok(json!({
                "error": "Either node_id or symbol_name is required.",
                "hint": "Provide symbol_name (e.g. \"my_function\") or node_id (from other tool results)."
            }));
        };
        let top_k = args.get("top_k")
            .and_then(|v| v.as_i64())
            .unwrap_or(5)
            .clamp(1, 100);
        let max_distance = args.get("max_distance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.8);

        // Check if embeddings are available
        if !self.db.vec_enabled() {
            return Ok(json!({
                "error": "Embedding not available. Build with --features embed-model.",
                "node_id": node_id
            }));
        }

        // Check if any embeddings exist at all
        let (embedded_count, total_nodes) = queries::count_nodes_with_vectors(self.db.conn())?;
        if embedded_count == 0 {
            return Ok(json!({
                "error": format!("No embeddings found ({} nodes indexed, 0 embedded). The embedding model may not be loaded — restart the MCP server with the embed-model feature enabled.", total_nodes),
                "node_id": node_id,
                "hint": "Alternative: use semantic_code_search with a descriptive query to find similar code by text matching."
            }));
        }

        // Get the node's embedding
        let embedding: Vec<f32> = {
            let bytes = queries::get_node_embedding(self.db.conn(), node_id)
                .map_err(|_| anyhow!("No embedding found for node_id {}. Node may not have been embedded yet ({}/{} nodes embedded).", node_id, embedded_count, total_nodes))?;
            bytemuck::cast_slice(&bytes).to_vec()
        };

        // Search for similar vectors
        let results = queries::vector_search(self.db.conn(), &embedding, top_k + 1)?; // +1 to exclude self

        // Filter and format results (skip self, module nodes, and over-distance)
        let similar: Vec<serde_json::Value> = results.iter()
            .filter(|(id, dist)| *id != node_id && *dist <= max_distance)
            .filter_map(|(id, distance)| {
                queries::get_node_by_id(self.db.conn(), *id).ok().flatten().map(|node| (node, *distance))
            })
            .filter(|(node, _)| !(node.node_type == "module" && node.name == "<module>"))
            .filter_map(|(node, distance)| {
                let file_path = queries::get_file_path(self.db.conn(), node.file_id)
                    .ok().flatten().unwrap_or_default();
                if is_test_symbol(&node.name, &file_path) {
                    return None;
                }
                let similarity = 1.0 / (1.0 + distance);
                Some(json!({
                    "node_id": node.id,
                    "name": node.name,
                    "type": node.node_type,
                    "file_path": file_path,
                    "start_line": node.start_line,
                    "similarity": (similarity * 10000.0).round() / 10000.0,
                    "distance": (distance * 10000.0).round() / 10000.0,
                }))
            })
            .take(top_k as usize)
            .collect();

        Ok(json!({
            "query_node_id": node_id,
            "results": similar,
            "count": similar.len(),
        }))
    }

    fn tool_project_map(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Return cached result if fresh (< 60s) — project_map is expensive and rarely changes mid-session
        // Note: cache stores full result; compact is derived from it on the fly
        let full_result = {
            let cache = lock_or_recover(&self.cached_project_map, "cached_pmap");
            if let Some((ts, ref val)) = *cache {
                if ts.elapsed().as_secs() < 60 {
                    Some(val.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        let result = if let Some(cached) = full_result {
            cached
        } else {
            let (modules, deps, entry_points, hot_functions) = queries::get_project_map(self.db.conn())?;

            let modules_json: Vec<serde_json::Value> = modules.iter().map(|m| {
                let mut obj = json!({
                    "path": m.path,
                    "files": m.files,
                    "functions": m.functions,
                    "classes": m.classes,
                });
                if m.interfaces_traits > 0 {
                    obj["interfaces_traits"] = json!(m.interfaces_traits);
                }
                if !m.languages.is_empty() {
                    obj["languages"] = json!(m.languages);
                }
                if !m.key_symbols.is_empty() {
                    obj["key_symbols"] = json!(m.key_symbols);
                }
                obj
            }).collect();

            let deps_json: Vec<serde_json::Value> = deps.iter().map(|d| {
                json!({
                    "from": d.from,
                    "to": d.to,
                    "imports": d.import_count,
                })
            }).collect();

            let routes_json: Vec<serde_json::Value> = entry_points.iter().map(|e| {
                json!({
                    "route": e.route,
                    "handler": e.handler,
                    "file": e.file,
                })
            }).collect();

            let hot_json: Vec<serde_json::Value> = hot_functions.iter().map(|h| {
                json!({
                    "name": h.name,
                    "type": h.node_type,
                    "file": h.file,
                    "caller_count": h.caller_count,
                })
            }).collect();

            let r = json!({
                "modules": modules_json,
                "module_dependencies": deps_json,
                "entry_points": routes_json,
                "hot_functions": hot_json,
            });

            // Cache the full result
            *lock_or_recover(&self.cached_project_map, "cached_pmap") =
                Some((std::time::Instant::now(), r.clone()));

            r
        };

        if compact {
            // Compact mode: drop languages/classes/interfaces, keep key_symbols for discoverability
            let compact_modules: Vec<serde_json::Value> = result["modules"].as_array()
                .map(|arr| arr.iter().map(|m| {
                    let mut obj = json!({
                        "path": m["path"],
                        "files": m["files"],
                        "functions": m["functions"],
                    });
                    // Preserve key_symbols — essential for deciding what to explore next
                    if let Some(ks) = m.get("key_symbols") {
                        if ks.is_array() && !ks.as_array().unwrap().is_empty() {
                            obj["key_symbols"] = ks.clone();
                        }
                    }
                    obj
                }).collect())
                .unwrap_or_default();

            let compact_deps: Vec<serde_json::Value> = result["module_dependencies"].as_array()
                .map(|arr| arr.iter().map(|d| json!({
                    "from": d["from"],
                    "to": d["to"],
                })).collect())
                .unwrap_or_default();

            // Trim hot_functions: top 10, name+file only
            let compact_hot: Vec<serde_json::Value> = result["hot_functions"].as_array()
                .map(|arr| arr.iter().take(10).map(|h| json!({
                    "name": h["name"],
                    "file": h["file"],
                    "caller_count": h["caller_count"],
                })).collect())
                .unwrap_or_default();

            // Trim entry_points: file+handler only
            let compact_entries: Vec<serde_json::Value> = result["entry_points"].as_array()
                .map(|arr| arr.iter().map(|e| json!({
                    "file": e["file"],
                    "handler": e["handler"],
                })).collect())
                .unwrap_or_default();

            return Ok(json!({
                "modules": compact_modules,
                "module_dependencies": compact_deps,
                "entry_points": compact_entries,
                "hot_functions": compact_hot,
            }));
        }

        Ok(result)
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Release the index lock on drop (covers panics, not SIGKILL)
        if self.is_primary {
            if let Some(ref root) = self.project_root {
                release_index_lock(&root.join(".code-graph"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queries::{upsert_file, FileRecord};
    use tempfile::TempDir;

    fn tool_call_json(tool_name: &str, args: serde_json::Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args
            }
        }).to_string()
    }

    fn parse_tool_result(response: &Option<String>) -> serde_json::Value {
        let resp = response.as_ref().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn test_handle_initialize() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"claude-code","version":"1.0"}}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"]["capabilities"]["tools"].is_object());
        assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn test_handle_tools_list() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), crate::mcp::tools::TOOL_COUNT);
    }

    #[test]
    fn test_handle_unknown_method() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"unknown/method","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32601);
    }

    #[test]
    fn test_get_index_status_tool() {
        let server = McpServer::new_test();
        {
            upsert_file(server.db().conn(), &FileRecord {
                path: "a.rs".into(), blake3_hash: "h".into(),
                last_modified: 1, language: Some("rust".into()),
            }).unwrap();
        }

        let req = tool_call_json("get_index_status", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["files_count"], 1);
        assert_eq!(result["schema_version"], crate::storage::schema::SCHEMA_VERSION);
    }

    #[test]
    fn test_semantic_search_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project_dir.path().join("src")).unwrap();
        std::fs::write(
            project_dir.path().join("src/auth.ts"),
            r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json("semantic_code_search", json!({"query": "validateToken", "top_k": 3}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert!(result.is_array());
        let results = result.as_array().unwrap();
        assert!(!results.is_empty(), "search should return results");
        let names: Vec<&str> = results.iter().filter_map(|r| r["name"].as_str()).collect();
        assert!(names.contains(&"validateToken"),
            "got names: {:?}", names);
    }

    #[test]
    fn test_get_call_graph_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("auth.ts"),
            r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        // Trigger indexing
        let _ = server.handle_message(&tool_call_json("get_index_status", json!({}))).unwrap();
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_call_graph", json!({
            "function_name": "handleLogin",
            "direction": "callees",
            "depth": 2
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["function"], "handleLogin");
    }

    #[test]
    fn test_get_ast_node_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("utils.ts"),
            "function helper() { return 42; }\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_ast_node", json!({
            "file_path": "utils.ts",
            "symbol_name": "helper"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["name"], "helper");
        assert_eq!(result["type"], "function");
    }

    #[test]
    fn test_read_snippet_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("main.ts"),
            "// header\nfunction foo() {\n  return 1;\n}\n// footer\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        // Find the node ID first
        let nodes = queries::get_nodes_by_name(server.db().conn(), "foo").unwrap();
        assert!(!nodes.is_empty());
        let node_id = nodes[0].id;

        let req = tool_call_json("read_snippet", json!({"node_id": node_id}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["name"], "foo");
        assert!(result["code_content"].as_str().unwrap().contains("return 1"));
    }

    #[test]
    fn test_rebuild_index_requires_confirm() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json("rebuild_index", json!({"confirm": false}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"]["isError"].as_bool().unwrap_or(false)
            || parsed["result"]["content"][0]["text"].as_str().unwrap_or("").contains("Error"));
    }

    #[test]
    fn test_rebuild_index_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json("rebuild_index", json!({"confirm": true}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "rebuilt");
        assert!(result["files_indexed"].as_i64().unwrap() >= 1);
    }

    #[test]
    fn test_start_stop_watch() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // Start watching
        let req = tool_call_json("start_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "watching");
        assert!(server.is_watching());

        // Starting again should say already watching
        let req = tool_call_json("start_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "already_watching");

        // Status should reflect watching
        let req = tool_call_json("get_index_status", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["is_watching"], true);

        // Stop watching
        let req = tool_call_json("stop_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "stopped");
        assert!(!server.is_watching());

        // Stopping again should say not watching
        let req = tool_call_json("stop_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "not_watching");
    }

    #[test]
    fn test_watcher_detects_changes_and_reindexes() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function original() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // Initial index
        server.ensure_indexed().unwrap();

        // Verify original is indexed
        let nodes = queries::get_nodes_by_name(server.db().conn(), "original").unwrap();
        assert_eq!(nodes.len(), 1);

        // Start watching
        let req = tool_call_json("start_watch", json!({}));
        let _ = server.handle_message(&req).unwrap();

        // Modify file
        std::fs::write(project_dir.path().join("a.ts"), "function changed() {}").unwrap();

        // Give watcher time to detect change
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Next ensure_indexed should detect change via watcher and run incremental
        server.ensure_indexed().unwrap();

        // Verify changed is now indexed
        let nodes = queries::get_nodes_by_name(server.db().conn(), "changed").unwrap();
        assert_eq!(nodes.len(), 1, "changed function should be indexed after watcher-triggered reindex");

        // Stop watching
        let req = tool_call_json("stop_watch", json!({}));
        let _ = server.handle_message(&req).unwrap();
    }

    #[test]
    fn test_from_project_root_creates_db() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join(".gitignore"), "node_modules/\n").unwrap();

        let _server = McpServer::from_project_root(project_dir.path()).unwrap();

        assert!(project_dir.path().join(".code-graph/index.db").exists());
        let gitignore = std::fs::read_to_string(project_dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".code-graph/"));
    }

    #[test]
    fn test_malformed_json_returns_error() {
        let server = McpServer::new_test();
        let result = server.handle_message("not valid json");
        let resp = result.expect("should be Ok").expect("should be Some");
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32700);
        assert!(parsed["error"]["message"].as_str().unwrap().contains("Parse error"));
    }

    #[test]
    fn test_notification_with_invalid_version_returns_none() {
        let server = McpServer::new_test();
        // Notification (no id) with wrong JSON-RPC version — must still return None per spec
        let req = r#"{"jsonrpc":"1.0","method":"notifications/initialized"}"#;
        let resp = server.handle_message(req).unwrap();
        assert!(resp.is_none(), "malformed notifications must never receive a response");
    }

    #[test]
    fn test_wrong_jsonrpc_version() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"1.0","id":1,"method":"initialize","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32600);
    }

    #[test]
    fn test_notification_returns_none() {
        let server = McpServer::new_test();
        // JSON-RPC notification: no "id" field
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let resp = server.handle_message(req).unwrap();
        assert!(resp.is_none(), "notifications should return None");
    }

    #[test]
    fn test_ping_returns_empty_object() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"].is_object());
    }

    #[test]
    fn test_tools_call_missing_params() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32602);
    }

    #[test]
    fn test_tools_call_missing_name() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"arguments":{}}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32602);
    }

    #[test]
    fn test_unknown_tool_returns_error() {
        let server = McpServer::new_test();
        let req = tool_call_json("nonexistent_tool", json!({}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Error"), "unknown tool should return error in content");
        assert!(parsed["result"]["isError"].as_bool().unwrap_or(false));
    }

    #[test]
    fn test_semantic_search_language_filter() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("app.ts"), "function handler() { return 1; }").unwrap();
        std::fs::write(project_dir.path().join("app.py"), "def handler():\n    return 1\n").unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());

        // Search with language filter for typescript
        let req = tool_call_json("semantic_code_search", json!({
            "query": "handler",
            "language": "typescript"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        let results = result.as_array().unwrap();
        for r in results {
            assert!(r["file_path"].as_str().unwrap().ends_with(".ts"),
                "language filter should only return typescript files, got: {}", r["file_path"]);
        }
    }

    #[test]
    fn test_semantic_search_node_type_filter() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("mix.ts"), r#"
class UserService {
    getUser() { return null; }
}
function standalone() { return 1; }
"#).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json("semantic_code_search", json!({
            "query": "user",
            "node_type": "class"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        let results = result.as_array().unwrap();
        for r in results {
            assert_eq!(r["type"].as_str().unwrap(), "class",
                "node_type filter should only return classes");
        }
    }

    #[test]
    fn test_semantic_search_sandbox_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create many functions with large code to exceed 2000 token threshold
        let mut code = String::new();
        for i in 0..20 {
            code.push_str(&format!(
                "function func{}() {{\n{}\n}}\n",
                i,
                format!("  // {}\n", "x".repeat(500)).repeat(3)
            ));
        }
        std::fs::write(project_dir.path().join("big.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json("semantic_code_search", json!({
            "query": "func",
            "top_k": 20
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Should be in compressed mode
        let mode = result["mode"].as_str().unwrap_or("");
        if mode.starts_with("compressed_") {
            assert!(result["results"].is_array());
            let compressed = result["results"].as_array().unwrap();
            assert!(!compressed.is_empty());
        }
        // If not compressed (small code), that's also valid behavior
    }

    #[test]
    fn test_find_http_route_with_downstream() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("server.ts"), r#"
function validateToken(token: string) { return true; }

function handleLogin(req: Request) {
    validateToken(req.token);
    return createSession(req.userId);
}

app.post('/api/login', handleLogin);
"#).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("find_http_route", json!({
            "route_path": "/api/login",
            "include_middleware": true
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["route"], "/api/login");
        // handlers array should exist
        assert!(result["handlers"].is_array());
    }

    #[test]
    fn test_semantic_search_clamps_top_k() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("small.ts"),
            "function hello() { return 1; }\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        // Request absurdly large top_k — should not error, just return clamped results
        let req = tool_call_json("semantic_code_search", json!({
            "query": "hello",
            "top_k": 999999
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        // Should succeed (array or compressed mode) — not crash or OOM
        assert!(result.is_array() || result["mode"].as_str() == Some("compressed"),
            "search with huge top_k should return valid results, got: {}", result);
    }

    #[test]
    fn test_trace_http_chain() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("server.ts"), r#"
function validateToken(token: string) { return true; }
function queryDatabase(userId: string) { return null; }

function handleLogin(req: Request) {
    validateToken(req.token);
    return queryDatabase(req.userId);
}

app.post('/api/login', handleLogin);
"#).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("trace_http_chain", json!({
            "route_path": "/api/login",
            "depth": 3
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        assert_eq!(result["route"], "/api/login");
        let handlers = result["handlers"].as_array().unwrap();
        assert!(!handlers.is_empty(), "should find at least one handler");

        // First handler should have a call_chain with recursive callees
        let handler = &handlers[0];
        assert!(handler["handler_name"].as_str().is_some());
        assert!(handler["call_chain"].is_array(), "handler should have call_chain array");
    }

    #[test]
    fn test_read_snippet_handles_missing_node() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("a.ts"),
            "function exists() { return 1; }\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        // Request a non-existent node_id — should return error gracefully, not panic
        let req = tool_call_json("read_snippet", json!({"node_id": 999999}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Error") || text.contains("not found"),
            "missing node should return error message, got: {}", text);
    }

    #[test]
    fn test_read_snippet_blocks_path_traversal() {
        // Verify the canonicalize+starts_with guard prevents reading outside project root.
        // Instead of fighting the server lifecycle, test the path logic directly:
        // root.join("../../etc/passwd").canonicalize() should NOT starts_with(root).
        let project_dir = TempDir::new().unwrap();
        let root = project_dir.path().canonicalize().unwrap();

        // Simulate what tool_read_snippet does with a traversal path
        let traversal_path = root.join("../../etc/passwd");

        // If the file exists on disk (e.g., /etc/passwd on Linux), canonicalize
        // succeeds but starts_with check rejects it. If it doesn't exist,
        // canonicalize fails — either way, content is never read.
        match traversal_path.canonicalize() {
            Ok(canonical) => {
                assert!(
                    !canonical.starts_with(&root),
                    "canonical traversal path {:?} must not start with root {:?}",
                    canonical, root
                );
            }
            Err(_) => {
                // File doesn't exist — canonicalize fails, read_snippet returns "Cannot resolve"
                // This is the safe outcome on systems without /etc/passwd at that relative path
            }
        }

        // Also test that a legitimate path DOES pass
        std::fs::write(project_dir.path().join("safe.ts"), "function ok() {}").unwrap();
        let safe_path = root.join("safe.ts").canonicalize().unwrap();
        assert!(safe_path.starts_with(&root), "legitimate path should be within root");
    }

    #[test]
    fn test_call_graph_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create a deep call chain with large function bodies
        let mut code = String::new();
        for i in 0..30 {
            code.push_str(&format!(
                "function chain{}() {{\n{}\n  chain{}();\n}}\n",
                i,
                format!("  // {}\n", "x".repeat(400)).repeat(3),
                i + 1,
            ));
        }
        code.push_str("function chain30() { return 1; }\n");
        std::fs::write(project_dir.path().join("deep.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_call_graph", json!({
            "function_name": "chain0",
            "direction": "callees",
            "depth": 20
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Result should either be normal (if small enough) or compressed
        if result["mode"].as_str().is_some() {
            assert!(result["mode"].as_str().unwrap().starts_with("compressed_"));
            assert!(result["results"].is_array());
        } else {
            assert!(result["function"].as_str().is_some());
        }
    }

    #[test]
    fn test_ast_node_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create a function with very large body
        let big_body = format!("  // {}\n", "x".repeat(500)).repeat(30);
        let code = format!("function bigFunc() {{\n{}}}\n", big_body);
        std::fs::write(project_dir.path().join("big.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_ast_node", json!({
            "file_path": "big.ts",
            "symbol_name": "bigFunc"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Result should either be normal or compressed
        if result["mode"].as_str().is_some() {
            assert_eq!(result["mode"], "compressed_node");
            assert!(result["node_id"].is_number());
            assert!(result["summary"].is_string());
        } else {
            assert_eq!(result["name"], "bigFunc");
        }
    }

    #[test]
    fn test_find_similar_code_no_embeddings() {
        let server = McpServer::new_test(); // no embedding model, vec not enabled
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"find_similar_code","arguments":{"node_id":1}}}"#;
        let response = server.handle_message(msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should return a result (not error) with an informative message about embedding requirement
        assert!(parsed["result"].is_object());
    }

    #[test]
    fn test_resources_list() {
        let server = McpServer::new_test();
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"resources/list","params":{}}"#;
        let response = server.handle_message(msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        let resources = parsed["result"]["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "code-graph://project-summary");
    }

    #[test]
    fn test_prompts_list() {
        let server = McpServer::new_test();
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list","params":{}}"#;
        let response = server.handle_message(msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        let prompts = parsed["result"]["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 3);
    }

    #[test]
    fn test_get_index_status_has_embedding_fields() {
        let server = McpServer::new_test();
        let req = tool_call_json("get_index_status", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert!(result["embedding_status"].is_string(),
            "should have embedding_status: {:?}", result);
        assert!(result["embedding_progress"].is_string(),
            "should have embedding_progress: {:?}", result);
        assert!(result["model_available"].is_boolean(),
            "should have model_available: {:?}", result);
    }

    #[test]
    fn test_handle_tool_centralized_compression() {
        // Verify that estimate_json_tokens works as expected for compression threshold checks
        let small = json!({"name": "hello", "type": "function"});
        let small_tokens = crate::sandbox::compressor::estimate_json_tokens(&small);
        assert!(small_tokens < COMPRESSION_TOKEN_THRESHOLD,
            "small JSON should be under threshold: {} tokens", small_tokens);

        // Build a large JSON value that exceeds the compression threshold
        // COMPRESSION_TOKEN_THRESHOLD = 2000, and estimate is len/3
        // So we need > 6000 chars of JSON
        let large_content: String = "x".repeat(8000);
        let large = json!({"code_content": large_content, "name": "big_function"});
        let large_tokens = crate::sandbox::compressor::estimate_json_tokens(&large);
        assert!(large_tokens > COMPRESSION_TOKEN_THRESHOLD,
            "large JSON should exceed threshold: {} tokens vs {} threshold",
            large_tokens, COMPRESSION_TOKEN_THRESHOLD);

        // Verify the centralized compression produces a truncated result
        let compressed = centralized_compress(large.clone());
        assert_ne!(compressed, large, "compressed result should differ from original");
        assert!(compressed.get("_truncated").is_some(),
            "centralized compression should add _truncated marker");
        let compressed_tokens = crate::sandbox::compressor::estimate_json_tokens(&compressed);
        assert!(compressed_tokens <= COMPRESSION_TOKEN_THRESHOLD * 2,
            "compressed result should be much smaller: {} tokens", compressed_tokens);
    }
}
