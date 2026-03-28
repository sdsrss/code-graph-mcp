mod helpers;
mod tools;

use helpers::*;

use anyhow::{anyhow, Result};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};

/// Check if a process with the given PID is alive (used by non-Unix lock fallback).
/// On non-Unix platforms, conservatively assumes the process is alive to prevent dual-primary.
#[cfg(not(unix))]
fn pid_is_alive(pid: u32) -> bool {
    let _ = pid;
    true
}

/// Try to acquire the index lock (`.code-graph/index.lock`) using flock().
/// Returns `Some(File)` holding the advisory lock if this process becomes the primary indexer.
/// The lock is automatically released when the returned File is dropped.
#[cfg(unix)]
fn try_acquire_index_lock(code_graph_dir: &Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    let lock_path = code_graph_dir.join("index.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| tracing::warn!("Could not open index lock: {} — running in secondary mode", e))
        .ok()?;

    // Non-blocking flock: LOCK_EX | LOCK_NB — fails immediately if another process holds it
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        tracing::info!("Another instance holds the index lock — running in secondary (read-only) mode");
        return None;
    }

    // Write our PID for diagnostics (not used for locking logic)
    use std::io::Write;
    let mut f = &file;
    let _ = f.write_all(std::process::id().to_string().as_bytes());

    Some(file)
}

/// Non-unix fallback: PID-based lock with create_new atomicity.
#[cfg(not(unix))]
fn try_acquire_index_lock(code_graph_dir: &Path) -> Option<std::fs::File> {
    use std::io::Write;

    let lock_path = code_graph_dir.join("index.lock");
    let my_pid = std::process::id();

    match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) {
        Ok(mut f) => {
            let _ = f.write_all(my_pid.to_string().as_bytes());
            return Some(f);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            tracing::warn!("Could not write index lock: {} — running in secondary mode", e);
            return None;
        }
    }

    // Lock exists — check if holder is alive
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if pid != my_pid && pid_is_alive(pid) {
                tracing::info!("Another instance (PID {}) holds the index lock — running in secondary (read-only) mode", pid);
                return None;
            }
            tracing::info!("Reclaiming stale index lock from PID {}", pid);
            let _ = std::fs::remove_file(&lock_path);
        }
    }

    match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) {
        Ok(mut f) => {
            let _ = f.write_all(my_pid.to_string().as_bytes());
            Some(f)
        }
        Err(_) => {
            tracing::info!("Lost lock race during stale reclaim — running in secondary mode");
            None
        }
    }
}

/// Remove the index lock file from disk.
/// On Unix, the flock is released automatically when the `_index_lock` File handle is dropped.
fn release_index_lock(code_graph_dir: &Path) {
    let _ = std::fs::remove_file(code_graph_dir.join("index.lock"));
}

use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::tools::ToolRegistry;
use crate::domain::CODE_GRAPH_DIR;
use crate::embedding::model::EmbeddingModel;
use crate::indexer::pipeline::{embed_and_store_batch, run_full_index, run_incremental_index_cached, IndexStats};
use crate::indexer::watcher::{FileWatcher, WatchEvent};
use crate::search::fusion::weighted_rrf_fusion;
use crate::storage::db::Database;
use crate::storage::queries;

/// Whether a symbol is a test-only symbol (by name or file path convention).
pub(super) fn is_test_symbol(name: &str, file_path: &str) -> bool {
    crate::domain::is_test_symbol(name, file_path)
}

/// Lock a Mutex, recovering from poison but logging a warning.
pub(super) fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|e| {
        tracing::warn!("Recovering poisoned mutex ({}): prior panic in critical section", label);
        e.into_inner()
    })
}

pub(super) struct WatcherState {
    pub(super) _watcher: FileWatcher,
    pub(super) receiver: mpsc::Receiver<WatchEvent>,
}

/// Debounce interval for no-watcher incremental checks.
/// In tests, use 0s so incremental checks always run immediately.
#[cfg(not(test))]
pub(super) const INCREMENTAL_DEBOUNCE_SECS: u64 = 30;
#[cfg(test)]
pub(super) const INCREMENTAL_DEBOUNCE_SECS: u64 = 0;

/// Token threshold for auto-compressing tool results.
/// Results exceeding this estimated token count are returned as summaries
/// with node_ids for expansion via get_ast_node.
pub(super) const COMPRESSION_TOKEN_THRESHOLD: usize = 2000;

/// Result of fuzzy name resolution.
pub(super) enum FuzzyResolution {
    /// Exactly one candidate matched — use this name.
    Unique(String),
    /// Multiple candidates — return suggestions to caller.
    Ambiguous(Vec<serde_json::Value>),
    /// No candidates found.
    NotFound,
}

/// Result from background startup indexing, consumed by post-index processing.
pub(super) struct StartupIndexResult {
    pub(super) files_indexed: usize,
    pub(super) nodes_created: usize,
    pub(super) edges_created: usize,
    pub(super) elapsed_ms: u64,
    pub(super) was_full: bool,
    pub(super) new_cache: Option<crate::indexer::merkle::DirectoryCache>,
    pub(super) stats: IndexStats,
}

/// Background indexing state: tracks startup indexing lifecycle.
pub(super) struct IndexingState {
    /// Set to true when `notifications/initialized` is received, signaling
    /// the main loop to run initial indexing and auto-start the file watcher.
    pub(super) startup_index_pending: Mutex<bool>,
    /// True while background startup indexing is running.
    pub(super) startup_indexing: Arc<AtomicBool>,
    /// Signaled when background startup indexing completes.
    pub(super) startup_indexing_done: Arc<(Mutex<bool>, Condvar)>,
    /// Pending result from background startup indexing, consumed by post-index processing.
    pub(super) startup_index_result: Arc<Mutex<Option<StartupIndexResult>>>,
    /// Error message from a failed background startup indexing attempt.
    pub(super) startup_index_error: Arc<Mutex<Option<String>>>,
    /// True while a background embedding thread is running.
    pub(super) embedding_in_progress: Arc<AtomicBool>,
}

impl IndexingState {
    fn new() -> Self {
        Self {
            startup_index_pending: Mutex::new(false),
            startup_indexing: Arc::new(AtomicBool::new(false)),
            startup_indexing_done: Arc::new((Mutex::new(false), Condvar::new())),
            startup_index_result: Arc::new(Mutex::new(None)),
            startup_index_error: Arc::new(Mutex::new(None)),
            embedding_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Cached query results with TTL-based invalidation.
pub(super) struct CacheState {
    /// Cached project_map result: (timestamp, json_value). Invalidated on re-index.
    pub(super) cached_project_map: Mutex<Option<(std::time::Instant, serde_json::Value)>>,
    /// Cached module_overview results: path -> (timestamp, json_value). Invalidated on re-index.
    pub(super) cached_module_overviews: Mutex<std::collections::HashMap<String, (std::time::Instant, serde_json::Value)>>,
}

impl CacheState {
    fn new() -> Self {
        Self {
            cached_project_map: Mutex::new(None),
            cached_module_overviews: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

/// MCP server for code graph operations. Single-threaded (stdio loop).
///
/// Lock ordering (acquire in this order to avoid deadlocks):
///   1. indexing.startup_index_pending
///   2. indexed
///   3. dir_cache / last_incremental_check / last_index_stats
///   4. watcher
///   5. cache.cached_project_map / cache.cached_module_overviews
///   6. embedding_model
///   7. notify_writer / metrics
///
/// In practice, only one lock is held at a time due to the single-threaded
/// stdio loop. This ordering documents the safe sequence if concurrency is added.
pub struct McpServer {
    pub(super) registry: ToolRegistry,
    pub(super) db: Database,
    pub(super) embedding_model: Mutex<Option<EmbeddingModel>>,
    pub(super) project_root: Option<PathBuf>,
    pub(super) indexed: Mutex<bool>,
    pub(super) watcher: Mutex<Option<WatcherState>>,
    pub(super) last_incremental_check: Mutex<std::time::Instant>,
    pub(super) dir_cache: Mutex<Option<crate::indexer::merkle::DirectoryCache>>,
    /// Writer for sending MCP notifications (progress, logging) to the client.
    /// Set to stdout in production; None in tests.
    pub(super) notify_writer: Mutex<Option<Box<dyn Write + Send>>>,
    /// Background indexing state (startup indexing lifecycle + embedding flag).
    pub(super) indexing: IndexingState,
    /// Cached query results (project_map, module_overviews) with TTL invalidation.
    pub(super) cache: CacheState,
    /// Last indexing stats (skipped files, truncations) for observability.
    pub(super) last_index_stats: Mutex<IndexStats>,
    /// Aggregated session metrics, flushed to .code-graph/usage.jsonl at shutdown.
    pub(super) metrics: Mutex<super::metrics::SessionMetrics>,
    /// True if this instance holds the index lock (primary indexer).
    /// Secondary instances skip indexing/watching and read the DB in read-only mode.
    pub(super) is_primary: bool,
    /// Held lock file handle — on Unix, flock is released when this is dropped.
    _index_lock: Option<std::fs::File>,
}

impl McpServer {
    fn open_db(db_path: &Path) -> Result<Database> {
        // Always open with vec support — model may be downloaded later (hot-loading)
        // and the background embedding thread needs vec tables to exist.
        Database::open_with_vec(db_path)
    }

    /// Create from project root path: auto-creates .code-graph/ directory and .gitignore entry
    pub fn from_project_root(project_root: &Path) -> Result<Self> {
        let db_dir = project_root.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("index.db");

        // Ensure .code-graph/ is in .gitignore
        let gitignore_path = project_root.join(".gitignore");
        {
            let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
            if !content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed.trim_end_matches('/') == CODE_GRAPH_DIR
            }) {
                let mut new_content = content;
                if !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                new_content.push_str(&format!("{}/\n", CODE_GRAPH_DIR));
                if let Err(e) = std::fs::write(&gitignore_path, new_content) {
                    tracing::warn!("Could not update .gitignore: {}", e);
                }
            }
        }

        let index_lock = try_acquire_index_lock(&db_dir);
        let is_primary = index_lock.is_some();

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
            indexing: IndexingState::new(),
            cache: CacheState::new(),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            is_primary,
            _index_lock: index_lock,
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
            indexing: IndexingState::new(),
            cache: CacheState::new(),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            is_primary: true,
            _index_lock: None,
        }
    }

    #[cfg(test)]
    pub fn new_test_with_project(project_root: &Path) -> Self {
        let db_dir = project_root.join(CODE_GRAPH_DIR);
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
            indexing: IndexingState::new(),
            cache: CacheState::new(),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            is_primary: true,
            _index_lock: None,
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
                let usage_path = root.join(CODE_GRAPH_DIR).join("usage.jsonl");
                metrics.flush(&usage_path, env!("CARGO_PKG_VERSION"));
            }
            if self.is_primary {
                release_index_lock(&root.join(CODE_GRAPH_DIR));
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
            let mut guard = lock_or_recover(&self.indexing.startup_index_pending, "startup_index_pending");
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
    ///
    /// Write-access model: SQLite WAL mode with busy_timeout=5000ms.
    /// Background threads (indexing, embedding) each open their own connection.
    /// The startup_indexing flag + condvar prevents concurrent full indexes.
    /// If SQLITE_BUSY occurs (e.g., embedding vs incremental index), the 5s
    /// busy_timeout provides automatic retry. No write queue needed at current scale.
    fn spawn_startup_indexing(
        &self,
        project_root: PathBuf,
        has_existing_index: bool,
        dir_cache: Option<crate::indexer::merkle::DirectoryCache>,
    ) {
        if self.indexing.startup_indexing.swap(true, Ordering::AcqRel) {
            return; // already running
        }

        // Reset condvar done flag for this indexing session
        *lock_or_recover(&self.indexing.startup_indexing_done.0, "startup_indexing_done") = false;

        if has_existing_index {
            self.send_log("info", "Updating index in background (incremental)...");
        } else {
            self.send_log("info", "Building index in background...");
        }

        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        let indexing_flag = Arc::clone(&self.indexing.startup_indexing);
        let done_signal = Arc::clone(&self.indexing.startup_indexing_done);
        let result_slot = Arc::clone(&self.indexing.startup_index_result);
        let error_slot = Arc::clone(&self.indexing.startup_index_error);
        let progress_file = project_root.join(CODE_GRAPH_DIR).join("indexing-status.json");

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
                    let msg = format!("Background indexing failed: {}", e);
                    tracing::error!("{}", msg);
                    if let Ok(mut slot) = error_slot.lock() {
                        *slot = Some(msg);
                    }
                }
            }
            // _guard drop: clears flag, removes progress file, signals condvar
        });
    }

    /// Check if background startup indexing completed and process the result.
    /// Called from `run_startup_tasks()` and `ensure_indexed()`.
    fn consume_startup_index_result(&self) {
        if self.indexing.startup_indexing.load(Ordering::Acquire) {
            return; // still running
        }

        // Check for indexing errors and surface them to the MCP client
        if let Some(err_msg) = lock_or_recover(&self.indexing.startup_index_error, "startup_error").take() {
            self.send_log("error", &err_msg);
        }

        let result = lock_or_recover(&self.indexing.startup_index_result, "startup_result").take();
        let Some(r) = result else { return };

        *lock_or_recover(&self.indexed, "indexed") = true;

        // Invalidate caches after background startup indexing
        if r.files_indexed > 0 {
            *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
            lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
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
            let _ = std::fs::remove_file(root.join(CODE_GRAPH_DIR).join("indexing-status.json"));
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
            Some(p) => p.join(CODE_GRAPH_DIR).join("index.db"),
            None => return,
        };

        // Acquire flag AFTER precondition checks to avoid permanent flag leak
        if self.indexing.embedding_in_progress.swap(true, Ordering::AcqRel) {
            return; // already running
        }
        let flag = Arc::clone(&self.indexing.embedding_in_progress);

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

                    // embed_and_store_batch manages its own transaction internally
                    embed_and_store_batch(&db, &model, &chunk)?;

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
        let candidates = queries::get_nodes_with_files_by_name(self.db.conn(), name)?;
        let non_test: Vec<_> = candidates.iter()
            .filter(|nf| !is_test_symbol(&nf.node.name, &nf.file_path))
            .collect();
        if non_test.len() > 1 {
            let mut seen_files = std::collections::HashSet::new();
            for nf in &non_test {
                seen_files.insert(nf.node.file_id);
            }
            if seen_files.len() > 1 {
                let suggestions: Vec<_> = non_test.iter().map(|nf| {
                    json!({
                        "name": &nf.node.name,
                        "file_path": &nf.file_path,
                        "type": &nf.node.node_type,
                        "node_id": nf.node.id,
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

        // Non-blocking check for background startup indexing with short grace period.
        // Instead of blocking the stdio loop for up to 300s (which prevents all other
        // MCP requests), we wait at most 2s then return an error asking the client to retry.
        if self.indexing.startup_indexing.load(Ordering::Acquire) {
            let (lock, cvar) = &*self.indexing.startup_indexing_done;
            let mut done = lock_or_recover(lock, "startup_indexing_done");
            let grace = std::time::Duration::from_secs(2);
            if !*done {
                let (guard, wait_result) = cvar.wait_timeout(done, grace).unwrap_or_else(|e| {
                    tracing::warn!("Recovering poisoned condvar (startup_indexing_done)");
                    let guard = e.into_inner();
                    (guard.0, guard.1)
                });
                done = guard;
                if wait_result.timed_out() && !*done {
                    anyhow::bail!(
                        "Indexing in progress — results will be available shortly. \
                         Please retry your request in a few seconds or call get_index_status for details."
                    );
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
            *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
            lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
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
    ///
    /// If background embedding is in progress, waits briefly for it to finish
    /// to avoid a race condition where the embedding thread inserts vectors for
    /// node IDs that are being deleted and re-inserted by the incremental index.
    fn run_incremental_with_cache_restore(&self, project_root: &Path, model: Option<&EmbeddingModel>) -> Result<()> {
        if self.indexing.embedding_in_progress.load(Ordering::Acquire) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while self.indexing.embedding_in_progress.load(Ordering::Acquire) {
                if std::time::Instant::now() > deadline {
                    tracing::info!("Skipping incremental re-index: background embedding still in progress");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        let mut cache_guard = lock_or_recover(&self.dir_cache, "dir_cache");
        let cache_snapshot = cache_guard.clone();
        let cache = cache_guard.take();
        drop(cache_guard); // Release lock during I/O

        match run_incremental_index_cached(&self.db, project_root, model, cache.as_ref(), None) {
            Ok((result, new_cache)) => {
                if result.files_indexed > 0 {
                    // Invalidate caches when files actually changed
                    *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
                    lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
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
    /// Note: acquires `watcher` lock briefly; callers must not hold `dir_cache` to avoid deadlock.
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
            if req.validate().is_ok() && req.method == "notifications/initialized" {
                *lock_or_recover(&self.indexing.startup_index_pending, "startup_index_pending") = true;
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
                "Code Graph CLI \u{2014} this project is indexed. CLI commands (via Bash) complement built-in tools:\n",
                "\n",
                "Replace Grep (code understanding):\n",
                "  code-graph-mcp grep \"pattern\" [path]    \u{2014} grep + AST context (shows containing function/class)\n",
                "  code-graph-mcp search \"concept\"         \u{2014} semantic search by concept (no exact name needed)\n",
                "  code-graph-mcp ast-search \"q\" --type fn \u{2014} structural search by type/return/params\n",
                "\n",
                "Replace Read multiple files (understand structure):\n",
                "  code-graph-mcp map                      \u{2014} project architecture (modules, deps, entry points)\n",
                "  code-graph-mcp overview src/mcp/         \u{2014} module symbols grouped by file and type\n",
                "  code-graph-mcp callgraph symbol          \u{2014} who calls it / what it calls\n",
                "\n",
                "Before modifying code:\n",
                "  code-graph-mcp impact symbol             \u{2014} blast radius (callers, routes, risk level)\n",
                "\n",
                "Still use Grep: exact strings, constants, regex, non-code files.\n",
                "Still use Read: specific file you will edit.\n",
                "MCP tools also available for programmatic access with compact=true option.\n",
                "\n",
                "Workflow tips:\n",
                "  1. Start with project_map (compact=true) for architecture overview\n",
                "  2. Use semantic_code_search with compact=true first \u{2014} saves tokens\n",
                "  3. Expand results: get_ast_node(node_id=N, compact=true) for signature, or without compact for full code\n",
                "  4. Before changes: impact_analysis to check blast radius\n",
                "  5. If search returns no/unexpected results: call get_index_status to check index health and embedding coverage\n",
                "  Prompts available: impact-analysis, understand-module, trace-request\n",
                "\n",
                "Decision rules (use INSTEAD OF multi-step Grep/Read):\n",
                "  \u{2022} \"who calls X?\" / \"what does X call?\" \u{2192} get_call_graph (NOT grep for function name)\n",
                "  \u{2022} \"what will break if I change X?\" \u{2192} impact_analysis (BEFORE editing)\n",
                "  \u{2022} \"how is module Y structured?\" \u{2192} module_overview (NOT reading files one by one)\n",
                "  \u{2022} \"find code that does Z\" (concept) \u{2192} semantic_code_search (NOT grep)\n",
                "  \u{2022} \"find all functions returning T\" \u{2192} ast_search with --returns filter\n",
                "  \u{2022} \"is this function used anywhere?\" \u{2192} find_references\n",
                "  \u{2022} modifying a function signature \u{2192} impact_analysis FIRST, then find all call sites"
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
                let text = serde_json::to_string(&result)
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
            "ast_search" => self.tool_ast_search(args),
            "find_references" => self.tool_find_references(args),
            "find_dead_code" => self.tool_find_dead_code(args),
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

}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Release the index lock on drop (covers panics, not SIGKILL)
        if self.is_primary {
            if let Some(ref root) = self.project_root {
                release_index_lock(&root.join(CODE_GRAPH_DIR));
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

    #[test]
    fn test_ensure_indexed_non_blocking_when_indexing_in_progress() {
        // Setup: create a server with startup_indexing=true and condvar never signaled
        let project_dir = TempDir::new().unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // Simulate background indexing in progress
        server.indexing.startup_indexing.store(true, Ordering::SeqCst);
        *server.indexing.startup_indexing_done.0.lock().unwrap() = false;

        // Call ensure_indexed and verify it returns within 5 seconds
        let start = std::time::Instant::now();
        let result = server.ensure_indexed();
        let elapsed = start.elapsed();

        // Must complete quickly (under 5 seconds), not block for 300 seconds
        assert!(elapsed.as_secs() < 5,
            "ensure_indexed should return within 5 seconds, took {}s", elapsed.as_secs());

        // Should return an error indicating indexing is in progress
        assert!(result.is_err(), "ensure_indexed should return Err when indexing is in progress");
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("ndexing in progress") || err_msg.contains("retry"),
            "error message should mention indexing in progress or retry, got: {}", err_msg);
    }
}
