mod helpers;
mod tools;

use helpers::*;

use anyhow::{anyhow, Result};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};

/// MCP `instructions` field (quiet variant) returned by `initialize` when
/// `CODE_GRAPH_QUIET_HOOKS=1` — a one-line pointer to the full decision table.
///
/// Module-level `pub const` (not function-local) so `tests/doc_cli_alignment.rs`
/// can scan every `` `code-graph-mcp <cmd> [--flags]` `` token in it against the
/// real clap surface. This string + the `.claude/plugin_code_graph_mcp.md` detail
/// doc are the two steering "sync faces" that (unlike the CLAUDE.md managed block)
/// have no generator↔mirror byte check — see [`project_claude_md_steering`].
// Two self-contained seams lifted out of this file (audit 2026-08-22 P2-8),
// which had grown to ~2,850 lines of production code spanning startup
// indexing, cache invalidation, lock recovery, dispatch, and these. Both are
// `impl McpServer` blocks in their own files: same type, same privacy scope,
// no API change.
mod backfill;
mod freshness;
use freshness::RESULT_REFRESH_TOOLS;

pub const INSTRUCTIONS_QUIET: &str = "code-graph-mcp ready. See CLAUDE.md \u{2192} .claude/plugin_code_graph_mcp.md for tool decision table (run `code-graph-mcp adopt` if missing). CLI: `code-graph-mcp --help`.";

/// MCP `instructions` field (default/noisy variant). v0.49: CLI form leads. In
/// Claude Code the MCP tools are deferred (a ToolSearch load must precede the
/// first call) while Bash is always live — the only conversions observed on real
/// coding nights (2026-06-12) were CLI invocations seconds after a deny. Trigger
/// phrases keep the literal questions first (routing memo). Drift-checked against
/// the live clap CLI by `tests/doc_cli_alignment.rs`.
pub const INSTRUCTIONS_NOISY: &str = concat!(
    "Code Graph MCP \u{2014} project indexed. Fastest path is the CLI via Bash (no tool loading): ",
    "\"who calls X?\" \u{2192} `code-graph-mcp callgraph X`; \"impact of X?\" or before editing a fn \u{2192} `code-graph-mcp impact X`; ",
    "module map \u{2192} `code-graph-mcp overview <dir>`; symbol source \u{2192} `code-graph-mcp show X`; text search with AST context \u{2192} `code-graph-mcp grep \"pat\" [paths]` (-i/-w/-F/-l, -c count, -t <lang>/-g <glob> scope, -A/-B/-C ctx, -M col-cap; grep exits).\n",
    "MCP tools (same data; load via ToolSearch): get_call_graph, get_ast_node include_impact=true, semantic_code_search for concept search without an exact symbol.\n",
    "Repo-wide AST index (LSP only handles open files; we don't). Replaces multi-round Grep+Read for structural queries.\n",
    "Still Grep for exact strings/regex; still Read files you will edit.\n",
    "Diagnostics: `code-graph-mcp health-check`.\n",
    "Full decision table: CLAUDE.md \u{2192} .claude/plugin_code_graph_mcp.md (run `code-graph-mcp adopt` if missing)."
);

// Compile-time guard: calibrated from observed Claude Code truncation at ~2048
// bytes; 1500 leaves ~25% margin. Future edits that blow the budget fail
// `cargo check` instead of silently getting truncated.
const _: () = assert!(
    INSTRUCTIONS_NOISY.len() <= 1500,
    "MCP noisy instructions exceed 1500-byte budget; Claude Code will truncate."
);

/// Arguments the handlers genuinely HONOR but the published schema does not
/// declare — `("*", arg)` for every tool, `(tool_name, arg)` for one.
///
/// `note_ignored_arguments` reports "the tool did nothing with this", and the
/// schema is the wrong source of truth for that claim on its own: an honored
/// argument that is merely undocumented would be reported as dropped while it is
/// in force, which inverts the whole point of the disclosure. `function_name` is
/// the live legacy alias for `get_call_graph`'s `symbol_name` (callgraph.rs) and
/// `skip_indexing` is read by every tool through `should_skip_indexing`
/// (helpers.rs) — a caller passing either gets the behaviour it asked for.
///
/// The other undeclared-but-read keys in this crate — `confirm` (rebuild_index),
/// `min_lines` / `ignore_paths` (find_dead_code) — belong to tools with no
/// published schema at all, which are skipped before this list is consulted, so
/// listing them here would be dead configuration. `test_no_new_undeclared_mcp_args`
/// in tests/hardening.rs pins that whole set: adding a handler that reads a new
/// undeclared key fails there until it is classified here.
const HONORED_UNDECLARED_ARGS: &[(&str, &str)] = &[
    ("*", "skip_indexing"),
    ("get_call_graph", "function_name"),
    // `ast_search` spells this filter `type`; its sibling `semantic_code_search`
    // (and CLI `search` / `dead-code`) spell it `node_type`, so callers carry the
    // wrong one over. The handler honors both (ast_search.rs) — without this
    // entry the very response that applied the filter also announced it had
    // ignored the argument.
    ("ast_search", "node_type"),
];

use super::metrics::ErrKind;
use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::tools::ToolRegistry;
use crate::domain::CODE_GRAPH_DIR;
use crate::embedding::model::EmbeddingModel;
use crate::indexer::lock::{release_index_lock, try_acquire_index_lock};
use crate::indexer::pipeline::{
    remove_stale_indexing_status, run_full_index, run_incremental_index_cached, IndexPhase,
    IndexStats, INDEXING_STATUS_FILE,
};
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
        tracing::warn!(
            "Recovering poisoned mutex ({}): prior panic in critical section",
            label
        );
        e.into_inner()
    })
}

pub(super) struct WatcherState {
    pub(super) _watcher: FileWatcher,
    pub(super) receiver: mpsc::Receiver<WatchEvent>,
}

/// Debounce interval for no-watcher incremental checks.
pub(super) const INCREMENTAL_DEBOUNCE_SECS: u64 = 30;

/// Upper bound on how long a session may answer from an index that no
/// incremental scan has revalidated, *while a watcher is active*.
///
/// The watcher is trusted but not infallible: `notify` reports backend errors
/// (inotify watch-limit exhaustion, network filesystems, container bind mounts)
/// through the error callback, which only logs — the `FileWatcher` object stays
/// in place, so `is_watching()` keeps reporting true while no event ever
/// arrives again. Before this backstop the no-event branch simply skipped the
/// rescan, making staleness in that state unbounded for the whole session.
/// Five minutes is far above the cost of a merkle stat pass and far below "the
/// rest of the session".
pub(super) const WATCHER_BACKSTOP_SECS: u64 = 300;

/// Minimum spacing between secondary → primary lock re-acquisition attempts.
pub(super) const PROMOTION_RETRY_SECS: u64 = 30;

/// Freshness timings, held as server state rather than read from consts at the
/// use site.
///
/// These used to be `#[cfg(test)]`-swapped constants, which made the branches
/// they gate untestable: with every interval compiled to 0 in test builds,
/// deleting a debounce branch outright left the suite green. As fields, a test
/// can set a *non-zero* interval and observe suppression, and a zero one and
/// observe the rescan — so each branch has a case that turns red when it is
/// removed.
#[derive(Debug, Clone, Copy)]
pub(super) struct TimingConfig {
    /// Debounce for the rescan taken when no watcher is active.
    pub(super) incremental_debounce: std::time::Duration,
    /// Backstop rescan interval while a watcher IS active (see [`WATCHER_BACKSTOP_SECS`]).
    pub(super) watcher_backstop: std::time::Duration,
    /// Throttle for secondary → primary re-acquisition attempts.
    pub(super) promotion_retry: std::time::Duration,
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            incremental_debounce: std::time::Duration::from_secs(INCREMENTAL_DEBOUNCE_SECS),
            watcher_backstop: std::time::Duration::from_secs(WATCHER_BACKSTOP_SECS),
            promotion_retry: std::time::Duration::from_secs(PROMOTION_RETRY_SECS),
        }
    }
}

impl TimingConfig {
    /// Timings for unit tests: preserves the pre-existing test behaviour (the
    /// no-watcher debounce was compiled to 0, and no backstop existed at all)
    /// so tests that don't care about timing keep their old semantics. Tests
    /// that DO care set the field they exercise explicitly.
    #[cfg(test)]
    fn for_tests() -> Self {
        Self {
            incremental_debounce: std::time::Duration::ZERO,
            watcher_backstop: std::time::Duration::from_secs(3600),
            promotion_retry: std::time::Duration::ZERO,
        }
    }
}

/// How long an incremental waits for an in-flight embedding backfill to release the write
/// path before skipping (and leaving the incremental owed via `pending_incremental`).
/// In tests, 0s so the skip-path test doesn't burn the full wait.
#[cfg(not(test))]
const EMBEDDING_WAIT_SECS: u64 = 2;
#[cfg(test)]
const EMBEDDING_WAIT_SECS: u64 = 0;

/// Poll interval for the no-traffic embedding backfill driver.
/// Nodes can be added to the index by a SHORT-LIVED CLI process — the PreToolUse
/// grep/read/edit hooks call `ensure_file_indexed` with `model=None` for speed — which
/// never triggers the server's tool-call-gated `ensure_indexed` backfill. With the file
/// watcher off, such a session would strand those nodes unembedded until restart. This
/// interval bounds how long the long-lived primary server takes to notice and embed them.
/// In tests, poll fast so the driver converges within the test timeout.
#[cfg(not(test))]
pub(super) const PERIODIC_BACKFILL_SECS: u64 = 60;
#[cfg(test)]
pub(super) const PERIODIC_BACKFILL_SECS: u64 = 1;

/// True if `e`'s anyhow cause chain contains a SQLite FOREIGN KEY constraint failure.
/// Matches the FULL chain (`{:#}`), not just the outer `to_string()`: the incremental
/// pipeline today surfaces rusqlite's message verbatim, but a future `.context()` wrapper
/// would hide it from a `to_string()` substring check and silently bypass the truncate+
/// rebuild recovery — regressing to the v0.11–0.14 "raw FK bubbles to the tool handler" bug.
fn is_fk_constraint_error(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("FOREIGN KEY constraint failed")
}

/// Whether an indexing error is "another connection holds the write path right
/// now", as opposed to a real failure.
///
/// The server writes from more than one connection (the startup-repair thread,
/// the embedding backfill, and — after a secondary→primary promotion — the
/// read-write handle opened at promotion). WAL absorbs most of that through
/// `busy_timeout`, but a deferred read transaction that must upgrade to a write
/// after another connection committed fails immediately with
/// `SQLITE_BUSY_SNAPSHOT`, which the busy handler never retries. Surfacing that
/// as a tool error would fail a perfectly good query for a transient condition;
/// the caller instead keeps the incremental owed and retries on the next call.
fn is_db_busy_error(e: &anyhow::Error) -> bool {
    let msg = format!("{e:#}");
    msg.contains("database is locked") || msg.contains("database table is locked")
}

/// Token threshold for auto-compressing tool results.
/// Results exceeding this estimated token count are returned as summaries
/// with node_ids for expansion via get_ast_node.
pub(super) const COMPRESSION_TOKEN_THRESHOLD: usize = 2000;

/// Result of fuzzy name resolution.
pub(super) enum FuzzyResolution {
    /// Exactly one candidate matched — use this name.
    Unique(String),
    /// Multiple candidates. Carries the CANDIDATES, not pre-rendered JSON: each
    /// consumer needs a different envelope (an error object for the by-name
    /// lookups, a "did you mean" empty-result envelope for `get_call_graph`), and
    /// rendering early is what let four hand-written shapes accumulate for one
    /// verdict (2026-08-16 audit §四/§六). Render with
    /// `crate::resolve::ambiguity_response` or `candidates_to_json`.
    Ambiguous(Vec<queries::NameCandidate>),
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
    /// True once the Phase-3 startup repair has run in this session.
    /// Used to guarantee `repair_null_context_strings` fires exactly once per process,
    /// covering the case where a prior session crashed mid-Phase-3 and left nodes
    /// with NULL context_string.
    pub(super) startup_repair_done: Arc<AtomicBool>,
    /// True once the periodic no-traffic backfill driver has been spawned this
    /// session. `start_post_index_services` runs on every tool call, so this guards
    /// the driver thread to exactly one per process.
    pub(super) periodic_backfill_started: Arc<AtomicBool>,
    /// Set when a watcher-triggered incremental was SKIPPED mid-flight (background
    /// embedding held the write path past the wait deadline). `drain_watcher_events`
    /// already consumed the signal that triggered it, so without this the change would
    /// strand until an unrelated future edit re-signals or the server restarts. The
    /// next `ensure_indexed` honors this flag and runs the owed incremental even with
    /// no fresh watcher event; cleared once an incremental actually completes.
    pub(super) pending_incremental: Arc<AtomicBool>,
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
            startup_repair_done: Arc::new(AtomicBool::new(false)),
            periodic_backfill_started: Arc::new(AtomicBool::new(false)),
            pending_incremental: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Cached query results with TTL-based invalidation.
pub(super) struct CacheState {
    /// Cached project_map result: (timestamp, json_value). Invalidated on re-index.
    pub(super) cached_project_map: Mutex<Option<(std::time::Instant, serde_json::Value)>>,
    /// Cached module_overview results: path -> (timestamp, json_value). Invalidated on re-index.
    pub(super) cached_module_overviews:
        Mutex<std::collections::HashMap<String, (std::time::Instant, serde_json::Value)>>,
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
///   7. promoted_db (innermost; `write_db()` may be called under 6 but never
///      the reverse, and it must NOT be re-entered while a `WriteDb` is alive)
///   8. notify_writer / metrics
///
/// `last_promotion_attempt` is leaf-only (taken and released inside
/// `try_promote_to_primary` before any other lock).
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
    ///
    /// Mutable state, not a construction-time constant: a secondary re-attempts
    /// the lock from `ensure_indexed` (throttled by `timing.promotion_retry`)
    /// and flips this on when the previous primary is gone. Read through
    /// [`McpServer::is_primary`].
    is_primary: AtomicBool,
    /// Held lock file handle — on Unix, flock is released when this is dropped.
    /// Behind a mutex because promotion installs it after construction.
    _index_lock: Mutex<Option<std::fs::File>>,
    /// Read-write DB handle opened at promotion time. `None` for an instance
    /// that was primary from the start (its `db` is already read-write) and for
    /// a secondary that never won the lock. See [`McpServer::write_db`].
    promoted_db: Mutex<Option<Database>>,
    /// Last secondary → primary re-acquisition attempt (throttle anchor).
    last_promotion_attempt: Mutex<std::time::Instant>,
    /// Freshness/debounce timings (see [`TimingConfig`]).
    pub(super) timing: TimingConfig,
    /// Max files re-indexed per result-set refresh (see
    /// [`crate::indexer::resync::RESYNC_BUDGET`]).
    /// Overridable via `CODE_GRAPH_RESYNC_BUDGET`, the same knob the CLI resync uses.
    pub(super) result_refresh_budget: usize,
}

/// A database handle valid for WRITES.
///
/// For an instance that was primary from the start this is just `&self.db`. An
/// instance promoted from secondary keeps `db` read-only — SQLite cannot
/// re-flag an already-open connection — and writes through the read-write
/// connection opened at promotion. Both live in this process, so the read-only
/// handle sees the writer's commits through WAL.
pub(super) enum WriteDb<'a> {
    Startup(&'a Database),
    Promoted(MutexGuard<'a, Option<Database>>),
}

impl std::ops::Deref for WriteDb<'_> {
    type Target = Database;
    fn deref(&self) -> &Database {
        match self {
            WriteDb::Startup(db) => db,
            // Only ever constructed from a `Some`; see `McpServer::write_db`.
            WriteDb::Promoted(guard) => guard
                .as_ref()
                .expect("promoted write handle is present by construction"),
        }
    }
}

impl McpServer {
    fn open_db(db_path: &Path) -> Result<Database> {
        // Always open with vec support — model may be downloaded later (hot-loading)
        // and the background embedding thread needs vec tables to exist.
        Database::open_with_vec(db_path)
    }

    /// Open the DB with the flag set appropriate for this instance's role.
    /// Primary (holds flock): full read-write, migrations enabled.
    /// Secondary (flock denied): strict read-only. We briefly wait for the
    /// primary to bootstrap the DB and then bail if it never appears, rather
    /// than fall through to read-write — a read-write open would run
    /// migrations and potentially `DELETE FROM nodes/edges/files` on the
    /// primary's DB via the INDEX_VERSION sweep at `db.rs:138-141`, defeating
    /// the whole purpose of the primary/secondary split.
    fn open_db_for_role(db_path: &Path, is_primary: bool) -> Result<Database> {
        if is_primary {
            return Self::open_db(db_path);
        }
        // Poll up to SECONDARY_DB_WAIT for primary to create the file.
        const SECONDARY_DB_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
        let deadline = std::time::Instant::now() + SECONDARY_DB_WAIT;
        while !db_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !db_path.exists() {
            anyhow::bail!(
                "Secondary instance cannot open DB at {}: the primary has not yet \
                 bootstrapped the index. Wait for the primary to finish its initial \
                 indexing, then restart this instance.",
                db_path.display()
            );
        }
        Database::open_readonly(db_path).map_err(|e| {
            anyhow::anyhow!(
                "Secondary read-only open failed at {}: {}. \
                 Primary may be mid-bootstrap — retry in a moment.",
                db_path.display(),
                e
            )
        })
    }

    /// Create from project root path: auto-creates .code-graph/ directory and .gitignore entry
    pub fn from_project_root(project_root: &Path) -> Result<Self> {
        let db_dir = project_root.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("index.db");

        // Ensure .code-graph/ is in .gitignore. Shared with the CLI index
        // commands so the two entry points cannot drift: this used to be the
        // ONLY writer, which left pure-CLI installs (hook-driven
        // `incremental-index`, MCP server never started) one `git add -A` away
        // from committing a multi-hundred-MB index (audit 2026-08-02 DB-4).
        crate::utils::gitignore::ensure_code_graph_dir_ignored(project_root);

        let index_lock = try_acquire_index_lock(&db_dir);
        let is_primary = index_lock.is_some();

        // Install snapshot BEFORE opening self.db to avoid the POSIX inode-swap problem.
        // POSIX rename(2) atomically replaces the file on disk, but any already-open
        // file descriptor keeps pointing at the OLD inode. If we opened self.db first
        // and then renamed the snapshot over index.db, self.db.conn() would silently see
        // empty schema for the rest of the session. By installing here — before open —
        // the connection we open below lands on the snapshot data directly.
        if is_primary && !db_path.exists() {
            Self::maybe_install_snapshot(project_root);
        }

        let embedding_model = EmbeddingModel::load()?;
        let db = Self::open_db_for_role(&db_path, is_primary)?;
        Ok(Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model: Mutex::new(embedding_model),
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(60),
            ),
            dir_cache: Mutex::new(None),
            notify_writer: Mutex::new(None),
            indexing: IndexingState::new(),
            cache: CacheState::new(),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            is_primary: AtomicBool::new(is_primary),
            _index_lock: Mutex::new(index_lock),
            promoted_db: Mutex::new(None),
            last_promotion_attempt: Mutex::new(std::time::Instant::now()),
            timing: TimingConfig::default(),
            result_refresh_budget: crate::indexer::resync::resync_budget(),
        })
    }

    /// Whether this instance currently holds the index lock (primary indexer).
    pub(super) fn is_primary(&self) -> bool {
        self.is_primary.load(Ordering::Acquire)
    }

    /// Handle to use for WRITES (indexing, repair, rebuild). See [`WriteDb`].
    ///
    /// Callers must bind the result to a local before using it across more than
    /// one expression, and must not call `write_db()` again while that local is
    /// alive — the promoted variant holds a non-reentrant mutex.
    pub(super) fn write_db(&self) -> WriteDb<'_> {
        let guard = lock_or_recover(&self.promoted_db, "promoted_db");
        if guard.is_some() {
            WriteDb::Promoted(guard)
        } else {
            drop(guard);
            WriteDb::Startup(&self.db)
        }
    }

    /// Re-attempt the index lock from a secondary instance, throttled.
    ///
    /// `try_acquire_index_lock` used to run exactly once, in the constructor, so
    /// a secondary stayed read-only for its entire process lifetime: once the
    /// primary exited, the session answered every query from a frozen index for
    /// the rest of the session, with no upper bound and — on the success path —
    /// no disclosure at all (only "not found" errors carry the secondary hint).
    /// Running here bounds that to `timing.promotion_retry` while keeping the
    /// common case (primary alive) to one throttled `flock` probe rather than
    /// one per tool call.
    ///
    /// Returns true when this call won the lock and the instance is now primary.
    fn try_promote_to_primary(&self) -> bool {
        let Some(root) = self.project_root.clone() else {
            return false;
        };
        {
            let mut last = lock_or_recover(&self.last_promotion_attempt, "last_promotion_attempt");
            if last.elapsed() < self.timing.promotion_retry {
                return false;
            }
            *last = std::time::Instant::now();
        }
        let db_dir = root.join(CODE_GRAPH_DIR);
        let Some(lock) = try_acquire_index_lock(&db_dir) else {
            return false;
        };
        // Writes need their own read-write connection: `self.db` was opened
        // read-only and stays the read path for the rest of the session.
        let write_db = match Database::open_with_vec(&db_dir.join("index.db")) {
            Ok(db) => db,
            Err(e) => {
                // Dropping `lock` releases the flock, so the next live primary
                // (or the next attempt) can take it.
                tracing::warn!(
                    "Won the index lock but could not open the DB read-write ({}) — staying secondary",
                    e
                );
                return false;
            }
        };
        *lock_or_recover(&self.promoted_db, "promoted_db") = Some(write_db);
        *lock_or_recover(&self._index_lock, "index_lock") = Some(lock);
        self.is_primary.store(true, Ordering::Release);
        // The index we inherited is as stale as the moment the old primary died;
        // arm the owed flag so the caller's very next step is an authoritative
        // merkle rescan rather than a debounced one.
        self.indexing
            .pending_incremental
            .store(true, Ordering::Release);
        self.send_log(
            "info",
            "Acquired the index lock — promoted from secondary to primary; \
             indexing and file watching are now active.",
        );
        self.start_post_index_services(&root);
        true
    }

    /// Attempt to install a snapshot for `project_root` without logging (no `&self`).
    /// Called during `from_project_root` before the db connection is opened.
    /// Succeeds silently, fails silently (snapshot is best-effort).
    fn maybe_install_snapshot(project_root: &Path) {
        let url = match crate::snapshot::resolve_snapshot_source(project_root) {
            Some(u) => u,
            None => {
                tracing::debug!("snapshot: no source configured, skipping pre-open install");
                return;
            }
        };
        match crate::snapshot::try_install(&url, project_root) {
            Ok(commit) => {
                tracing::info!(
                    "snapshot installed at commit {} (incremental drift-check will follow)",
                    commit
                );
            }
            Err(e) => {
                tracing::warn!("snapshot install failed ({}), will run full index", e);
            }
        }
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
            last_incremental_check: Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(60),
            ),
            dir_cache: Mutex::new(None),
            notify_writer: Mutex::new(None),
            indexing: IndexingState::new(),
            cache: CacheState::new(),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            is_primary: AtomicBool::new(true),
            _index_lock: Mutex::new(None),
            promoted_db: Mutex::new(None),
            last_promotion_attempt: Mutex::new(std::time::Instant::now()),
            timing: TimingConfig::for_tests(),
            result_refresh_budget: crate::indexer::resync::RESYNC_BUDGET,
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
            last_incremental_check: Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(60),
            ),
            dir_cache: Mutex::new(None),
            notify_writer: Mutex::new(None),
            indexing: IndexingState::new(),
            cache: CacheState::new(),
            last_index_stats: Mutex::new(IndexStats::default()),
            metrics: Mutex::new(super::metrics::SessionMetrics::new()),
            is_primary: AtomicBool::new(true),
            _index_lock: Mutex::new(None),
            promoted_db: Mutex::new(None),
            last_promotion_attempt: Mutex::new(std::time::Instant::now()),
            timing: TimingConfig::for_tests(),
            result_refresh_budget: crate::indexer::resync::RESYNC_BUDGET,
        }
    }

    /// Set the writer for sending MCP notifications to the client.
    pub fn set_notify_writer(&self, writer: Box<dyn Write + Send>) {
        *lock_or_recover(&self.notify_writer, "notify_writer") = Some(writer);
    }

    /// Flush aggregated session metrics to .code-graph/usage.jsonl.
    /// Called once at server shutdown (EOF). Skips only when the session had
    /// neither tool calls nor in-window recommendation events — a 0-tool-call
    /// session that saw deny/hint/bypass/cli-use traffic still writes, so the
    /// deny→use funnel denominator includes non-converting sessions (without
    /// this, 0% conversion was structurally unobservable).
    ///
    /// It also CALLS `release_index_lock`, which is a documented no-op on both
    /// platforms — the lock is the open handle, so the real release is this
    /// server's `File` dropping at process exit. The call and its `is_primary`
    /// guard are kept as the statement of where release belongs; see
    /// [`crate::indexer::lock::release_index_lock`] for why unlinking the lock
    /// file instead would break mutual exclusion.
    pub fn flush_metrics(&self) {
        if let Some(ref root) = self.project_root {
            let metrics = lock_or_recover(&self.metrics, "metrics");
            let cg_dir = root.join(CODE_GRAPH_DIR);
            if !metrics.is_empty() || metrics.has_recs_in_window(&cg_dir) {
                metrics.flush(&cg_dir.join("usage.jsonl"), env!("CARGO_PKG_VERSION"));
            }
            if self.is_primary() {
                release_index_lock(&cg_dir);
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
            let mut guard = lock_or_recover(
                &self.indexing.startup_index_pending,
                "startup_index_pending",
            );
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
            if !self.is_primary() {
                // Secondaries never index, so nothing on this path would ever clear
                // a progress file orphaned by a killed primary. Stale-only removal:
                // a LIVE primary's file has a fresh mtime and is left untouched.
                remove_stale_indexing_status(&project_root);
                let has_data = queries::get_index_status(self.db.conn(), false)
                    .map(|s| s.files_count > 0)
                    .unwrap_or(false);
                if has_data {
                    *lock_or_recover(&self.indexed, "indexed") = true;
                    self.send_log(
                        "info",
                        "Secondary instance: using existing index (read-only).",
                    );
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
                // The watcher and other post-index services wait for the first tool call
                // (consume_startup_index_result), but the embedding backfill driver must
                // run even in a NO-tool-call session — that's exactly when out-of-band
                // CLI/hook node additions would otherwise strand unembedded. It polls the
                // DB independently and no-ops while the startup backfill holds the flag.
                if cfg!(feature = "embed-model") {
                    self.spawn_periodic_backfill();
                }
                return;
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
        *lock_or_recover(
            &self.indexing.startup_indexing_done.0,
            "startup_indexing_done",
        ) = false;

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
        let progress_file = project_root.join(CODE_GRAPH_DIR).join(INDEXING_STATUS_FILE);
        // A progress file left by a killed predecessor (session exit / SIGKILL skips
        // IndexGuard::drop) would pin the statusline at a phantom "indexing N/M"
        // until some future run's guard removed it. We hold the primary index lock,
        // so no other process is writing this file — remove it unconditionally; our
        // own progress writes recreate it.
        let _ = std::fs::remove_file(&progress_file);
        let embedding_flag = Arc::clone(&self.indexing.embedding_in_progress);
        // Kept out of the closure so the spawn-failure path below can still clear
        // the flags the closure's IndexGuard would have cleared.
        let spawn_fail_flag = Arc::clone(&self.indexing.startup_indexing);
        let spawn_fail_done = Arc::clone(&self.indexing.startup_indexing_done);
        let spawn_fail_progress = progress_file.clone();
        let spawn_fail_error = Arc::clone(&self.indexing.startup_index_error);

        // Explicit stack size, not `thread::spawn`'s 2 MiB default: the relation
        // walker recurses to MAX_RELATION_DEPTH and a stack overflow here is an
        // abort that bypasses the serve loop's catch_unwind, killing the session.
        // See domain::INDEX_THREAD_STACK_SIZE.
        let spawned = std::thread::Builder::new()
            .name("code-graph-index".to_string())
            .stack_size(crate::domain::INDEX_THREAD_STACK_SIZE)
            .spawn(move || {
                // The index work runs under IndexGuard (clears the startup flag, removes
                // the progress file, signals the done condvar on drop). Scoped to its own
                // block so the guard drops — and `startup_indexing` reads false — BEFORE
                // the embedding backfill below, which can run for minutes; otherwise the
                // server would look "still indexing" for the whole embed.
                {
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
                    let progress_cb = move |phase: IndexPhase, current: usize, total: usize| {
                        // "finalizing" marks the post-batch full-graph phases where the
                        // file count no longer moves; each write also refreshes mtime,
                        // which is the statusline's liveness signal (stale-file gate).
                        let s = match phase {
                            IndexPhase::Files => "indexing",
                            IndexPhase::Finalizing => "finalizing",
                        };
                        let json = format!(r#"{{"s":"{}","d":{},"t":{}}}"#, s, current, total);
                        let _ = std::fs::write(&pf, json);
                    };

                    let index_start = std::time::Instant::now();
                    let result = if has_existing_index {
                        run_incremental_index_cached(
                            &db,
                            &project_root,
                            None,
                            dir_cache.as_ref(),
                            Some(&progress_cb),
                        )
                        .map(|(r, cache)| (r, Some(cache)))
                    } else {
                        run_full_index(&db, &project_root, None, Some(&progress_cb))
                            .map(|r| (r, None))
                    };

                    match result {
                        Ok((result, new_cache)) => {
                            let elapsed_ms = index_start.elapsed().as_millis() as u64;
                            tracing::info!(
                                "Background indexing complete: {} files, {} nodes in {}ms",
                                result.files_indexed,
                                result.nodes_created,
                                elapsed_ms
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
                                Err(e) => {
                                    tracing::error!(
                                        "Background indexing: result slot poisoned: {}",
                                        e
                                    )
                                }
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
                    // IndexGuard drops here: clears startup flag, removes progress file, signals condvar.
                }

                // Embed the freshly-indexed nodes right here, in the same background
                // thread. The index is committed and `startup_indexing` is now clear, so
                // a long embed won't masquerade as "still indexing". Driving the backfill
                // from the index thread — not only from consume_startup_index_result(),
                // which fires solely on an incoming MCP message — means an edit-only
                // session that issues NO code-graph tool call still gets a fully embedded
                // index instead of stranding the vectors at whatever a prior search left.
                // Guarded by embedding_in_progress so it never double-runs with a
                // search-triggered embed; no-ops when no model is available locally.
                // Skipped in no-embed builds (`default = []`): with no model to embed
                // with, spawning the backfill + its model-load attempt is pure per-session
                // waste (the message-driven spawn_background_embedding is already guarded).
                if cfg!(feature = "embed-model") {
                    let _ = Self::run_guarded_backfill(&db_path, &embedding_flag);
                }
            });

        // A failed spawn drops the closure, so the IndexGuard that normally clears
        // `startup_indexing` and signals the done condvar never exists. Without this
        // arm the flag stays set forever, `consume_startup_index_result` bails on it
        // every time, and nothing downstream of indexing — the watcher, the embedding
        // backfill — ever starts for the session. Each ensure_indexed() caller still
        // returns after its bounded grace wait, so the session is degraded rather
        // than hung.
        //
        // The error goes into the same slot a failed index run uses, so it reaches
        // the MCP client instead of living only in stderr `tracing` where no client
        // can see it (pre-tag review, Minor #2).
        if let Err(e) = spawned {
            tracing::error!("Failed to spawn background indexing thread: {}", e);
            *lock_or_recover(&spawn_fail_error, "startup_error") =
                Some(format!("could not start background indexing: {e}"));
            spawn_fail_flag.store(false, Ordering::Release);
            let (lock, cvar) = &*spawn_fail_done;
            *lock_or_recover(lock, "startup_indexing_done") = true;
            cvar.notify_all();
            let _ = std::fs::remove_file(&spawn_fail_progress);
        }
    }

    /// Check if background startup indexing completed and process the result.
    /// Called from `run_startup_tasks()` and `ensure_indexed()`.
    fn consume_startup_index_result(&self) {
        if self.indexing.startup_indexing.load(Ordering::Acquire) {
            return; // still running
        }

        // Check for indexing errors and surface them to the MCP client
        if let Some(err_msg) =
            lock_or_recover(&self.indexing.startup_index_error, "startup_error").take()
        {
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
            self.send_log(
                "info",
                &format!(
                    "Indexed {} files ({} nodes, {} edges).",
                    r.files_indexed, r.nodes_created, r.edges_created
                ),
            );
        } else {
            self.send_log("info", "Index is up to date.");
        }

        // Safety net: ensure progress file is removed (normally done by IndexGuard)
        if let Some(ref root) = self.project_root {
            let _ = std::fs::remove_file(root.join(CODE_GRAPH_DIR).join(INDEXING_STATUS_FILE));
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
            let (tx, rx) = mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
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

        // Repair Phase-3 casualties (NULL context_string from prior-session crashes)
        // BEFORE spawning embedding, so rebuilt context strings get vectorized in the
        // same run. Fires at most once per process.
        self.spawn_startup_repair(project_root);

        self.spawn_background_embedding();

        // Watcher- and traffic-independent backfill: drains nodes added out-of-band by
        // the CLI/hook freshness path (which never triggers the tool-call backfill).
        if cfg!(feature = "embed-model") {
            self.spawn_periodic_backfill();
        }

        #[cfg(feature = "embed-model")]
        self.spawn_model_download();
    }

    /// Spawn a background thread that runs `repair_null_context_strings` once per
    /// process. Covers the failure path where a prior session crashed mid-Phase-3
    /// (post-node-insert, before context_string/embedding commit). Primary-only:
    /// secondary instances can't write.
    fn spawn_startup_repair(&self, project_root: &Path) {
        if !self.is_primary() {
            return;
        }
        if self
            .indexing
            .startup_repair_done
            .swap(true, Ordering::AcqRel)
        {
            return; // already ran this session
        }
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        std::thread::spawn(move || {
            let outcome = (|| -> Result<(usize, usize, usize, usize)> {
                let db = Database::open_with_vec(&db_path)?;
                #[cfg(feature = "embed-model")]
                let model = EmbeddingModel::load().ok().flatten();
                #[cfg(not(feature = "embed-model"))]
                let model: Option<EmbeddingModel> = None;
                let repaired =
                    crate::indexer::pipeline::repair_null_context_strings(&db, model.as_ref())?;
                // Sweep orphan vectors (vectors whose node was deleted) accumulated across index
                // generations — the backfill-race residue now prevented at the insert site, plus
                // any predating that guard (daagu carried 157 at rowids past the live range). Runs
                // here because the startup index has already populated `nodes`; reap_orphan_vectors
                // additionally no-ops on an empty nodes table, so a transient mid-rebuild window
                // can never nuke live vectors. Independent of embed-model: the vec table exists in
                // all builds and reap is a cheap anti-join + point deletes.
                let reaped = crate::storage::queries::reap_orphan_vectors(db.conn())?;
                // Same-dim model-swap guard BEFORE seeding (ordering matters): if the model
                // changed, drop the stale cache + node_vectors now so the seed below cannot
                // repopulate the cache with OLD-model embeddings that a matching-but-stale
                // fingerprint would then treat as valid. No-op when unchanged / first run.
                // best-effort — a failed check must not abort the rest of the repair.
                #[cfg(feature = "embed-model")]
                if let Err(e) = crate::storage::queries::ensure_embedding_cache_valid(
                    db.conn(),
                    EmbeddingModel::MODEL_CONTENT_BLAKE3,
                ) {
                    tracing::warn!(
                        "[embed-cache] startup validity check failed (continuing): {}",
                        e
                    );
                }
                // Seed the content-hash cache from existing vectors (once) so an already-embedded
                // index reuses on its NEXT version bump instead of paying one more full re-embed.
                let seeded = crate::storage::queries::seed_embedding_cache_from_vectors(db.conn())?;
                // Bound the cache to live nodes (prune entries whose content no longer exists).
                // Same empty-nodes safety valve as the reap, so a mid-rebuild window can't wipe
                // the reuse cache.
                let pruned = crate::storage::queries::gc_embedding_cache(db.conn())?;
                Ok((repaired, reaped, seeded, pruned))
            })();
            match outcome {
                Ok((0, 0, 0, 0)) => {}
                Ok((repaired, reaped, seeded, pruned)) => tracing::info!(
                    "[startup-repair] Rebuilt {} NULL context_string rows; reaped {} orphan vectors; \
                     seeded {} cache entries; pruned {} stale",
                    repaired, reaped, seeded, pruned
                ),
                Err(e) => tracing::warn!("[startup-repair] Failed: {}", e),
            }
        });
    }

    /// Spawn a background thread to embed nodes that don't yet have vectors.
    /// The thread opens its own DB connection and model (EmbeddingModel is not Send)
    /// to avoid blocking the main stdio loop.
    pub(super) fn spawn_background_embedding(&self) {
        // Guard: only spawn if model and vec are available
        if lock_or_recover(&self.embedding_model, "embedding_model").is_none()
            || !self.db.vec_enabled()
        {
            return;
        }

        let db_path = match &self.project_root {
            Some(p) => p.join(CODE_GRAPH_DIR).join("index.db"),
            None => return,
        };

        // Cheap pre-filter to skip spawning a doomed thread; run_guarded_backfill
        // re-checks authoritatively via swap, so the small load→swap race is benign.
        if self.indexing.embedding_in_progress.load(Ordering::Acquire) {
            return; // already running
        }
        let flag = Arc::clone(&self.indexing.embedding_in_progress);
        std::thread::spawn(move || {
            let _ = Self::run_guarded_backfill(&db_path, &flag);
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

        // Test/CI escape hatch. `cargo test --features embed-model` (the embed-check
        // CI job + the integration harness) builds this bin WITH embedding and
        // spawns `serve` many times; on a cache-less runner each spawn would
        // background-download the ~90 MB model from the GitHub release — slow,
        // and flaky because cache population mid-suite flips which model-requiring
        // tests skip vs run. The flag (set by ci.yml's embed-check job and by the
        // test harness `McpClient::spawn`) disables only the AUTO-DOWNLOAD; a model
        // already cached is still loaded and used, so local embed tests with a
        // cached model are unaffected.
        if std::env::var("CODE_GRAPH_DISABLE_MODEL_DOWNLOAD")
            .ok()
            .as_deref()
            == Some("1")
        {
            EmbeddingModel::record_download_state("disabled", 0, None);
            return;
        }

        std::thread::spawn(move || {
            let cache_dir = match EmbeddingModel::cache_models_dir() {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("[model-dl] Cannot resolve cache dir: {}", e);
                    EmbeddingModel::record_download_state("failed", 0, Some(&e.to_string()));
                    return;
                }
            };

            // Version/identity-aware, not existence-only: a cached model that
            // doesn't match the weights this binary expects (older release, or
            // tampered cache) re-downloads instead of staying pinned forever —
            // same fault class as the native-binary pin fixed in v0.45.x.
            if EmbeddingModel::cached_model_is_current(&cache_dir) {
                EmbeddingModel::record_download_state("ok", 0, None);
                return; // Already downloaded and matching
            }
            if cache_dir.join("model.safetensors").exists() {
                tracing::info!("[model-dl] Cached model does not match this binary's pinned weights — re-downloading");
            }

            let url = EmbeddingModel::model_download_url();
            // Bounded retry with backoff. A single transient failure (flaky net, slow
            // mirror, brief 5xx) otherwise leaves the user silently FTS5-only for the
            // ENTIRE session until a manual restart. Retry a few times before giving
            // up; a later server start still re-attempts (cached_model_is_current).
            const MAX_ATTEMPTS: u32 = 3;
            let mut downloaded = false;
            let mut last_error = String::new();
            for attempt in 1..=MAX_ATTEMPTS {
                // Recorded BEFORE the call: an attempt that hangs or dies with
                // the process still leaves "in_flight" behind, which `doctor`
                // reports as in-flight rather than as the indistinguishable
                // "never attempted" silence issue #35 describes.
                EmbeddingModel::record_download_state("in_flight", attempt, None);
                match EmbeddingModel::download_model_to(&url, &cache_dir) {
                    Ok(trust_path) => {
                        tracing::info!(
                            "[model-dl] Model downloaded successfully (attempt {}/{}, {} trust path)",
                            attempt,
                            MAX_ATTEMPTS,
                            trust_path
                        );
                        EmbeddingModel::record_download_state_trusted(
                            "ok",
                            attempt,
                            None,
                            Some(trust_path),
                        );
                        downloaded = true;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[model-dl] Download attempt {}/{} failed: {}",
                            attempt,
                            MAX_ATTEMPTS,
                            e
                        );
                        last_error = e.to_string();
                        if attempt < MAX_ATTEMPTS {
                            // Exponential backoff: 4s, then 8s (background thread — sleeping is fine).
                            std::thread::sleep(std::time::Duration::from_secs(
                                4u64 * (1u64 << (attempt - 1)),
                            ));
                        }
                    }
                }
            }
            if !downloaded {
                tracing::warn!("[model-dl] All {} download attempts failed — staying in FTS5-only mode; will retry on next server start.", MAX_ATTEMPTS);
                EmbeddingModel::record_download_state("failed", MAX_ATTEMPTS, Some(&last_error));
            }
        });
    }

    /// Try lazy loading: if model was None but cache now has files, load it.
    /// Called at the start of semantic search / find_similar_code.
    pub(super) fn try_lazy_load_model(&self) {
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
    ///
    /// Exact-name matches take precedence over substring matches. Without this,
    /// `find_functions_by_fuzzy_name("handle_tool")` would return the exact
    /// `handle_tool` alongside substring matches like `handle_tools_list` and
    /// trigger false "ambiguous" reports in find_references/impact_analysis.
    /// Delegates to `crate::resolve::resolve_fuzzy` — the CLI runs the same code.
    /// Only the suggestion rendering differs, and that goes through
    /// `candidates_to_json`, which is what made `resolve.rs`'s "Single-sourced"
    /// promise true rather than aspirational (the two inline `json!` blocks this
    /// replaces were hand-copies of it).
    pub(super) fn resolve_fuzzy_name(&self, name: &str) -> Result<FuzzyResolution> {
        Ok(match crate::resolve::resolve_fuzzy(self.db.conn(), name)? {
            crate::resolve::FuzzyResolution::Unique(n) => FuzzyResolution::Unique(n),
            crate::resolve::FuzzyResolution::Ambiguous(cands) => FuzzyResolution::Ambiguous(cands),
            crate::resolve::FuzzyResolution::NotFound => FuzzyResolution::NotFound,
        })
    }

    /// Check if a symbol name is ambiguous (≥2 non-test definitions). Fires on
    /// both cross-file collisions AND same-file multi-definitions (e.g. two
    /// `fn new()` in one module for different impl blocks) — the latter needs
    /// `node_id`/`start_line` to disambiguate because `file_path` alone doesn't
    /// uniquely identify the target. Delegates to `crate::resolve::detect_ambiguity`
    /// so the MCP and CLI surfaces share one verdict (audit 2026-06-03 #6).
    /// Returns the candidate definitions if ambiguous, None otherwise. Use
    /// `crate::resolve::{ambiguity_message, candidates_to_json}` to render them.
    pub(super) fn disambiguate_symbol(
        &self,
        name: &str,
    ) -> Result<Option<Vec<crate::storage::queries::NameCandidate>>> {
        crate::resolve::detect_ambiguity(self.db.conn(), name)
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
    pub(super) fn send_progress(&self, token: &str, current: usize, total: usize) {
        self.send_notification(
            "notifications/progress",
            json!({
                "progressToken": token,
                "progress": current,
                "total": total,
            }),
        );
    }

    /// Send MCP log notification.
    pub(super) fn send_log(&self, level: &str, message: &str) {
        self.send_notification(
            "notifications/message",
            json!({
                "level": level,
                "logger": "code-graph",
                "data": message,
            }),
        );
    }

    /// Ensure index is up-to-date. On first call, runs full index.
    /// If background startup indexing is running, waits for it to complete.
    /// If watcher is active, checks for pending events to decide if incremental needed.
    pub(super) fn ensure_indexed(&self) -> Result<()> {
        let project_root = match &self.project_root {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        // Secondary instances: re-attempt the lock (throttled) before settling for
        // read-only. Winning it falls through to the primary path below, which the
        // promotion armed `pending_incremental` for.
        if !self.is_primary() && !self.try_promote_to_primary() {
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
            // Wait at most this grace for background indexing, then return Err so the
            // stdio loop stays responsive (vs blocking up to 300s). Loop against an
            // Instant deadline rather than trusting a single wait_timeout: a spurious
            // condvar wakeup — or wait_result.timed_out() being unreliable under load,
            // as on slow Windows CI — must NOT fall through and let us start indexing.
            // It re-waits the remaining grace and only bails once the deadline has
            // truly passed with indexing still unfinished.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !*done {
                let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
                    Some(r) if !r.is_zero() => r,
                    _ => anyhow::bail!(
                        "Indexing in progress — results will be available shortly. \
                         Please retry your request in a few seconds or run `code-graph-mcp health-check` for details."
                    ),
                };
                let (guard, _) = cvar.wait_timeout(done, remaining).unwrap_or_else(|e| {
                    tracing::warn!("Recovering poisoned condvar (startup_indexing_done)");
                    let guard = e.into_inner();
                    (guard.0, guard.1)
                });
                done = guard;
            }
        }
        // Consume result whether we waited or it completed before this call
        self.consume_startup_index_result();

        // Read the indexed flag (short lock scope to avoid holding across I/O)
        let is_indexed = *lock_or_recover(&self.indexed, "indexed");

        if !is_indexed {
            // Check whether the DB already has data (e.g. snapshot installed in
            // from_project_root before this connection was opened). If so, run
            // incremental (drift-correction) instead of a full reindex.
            let has_existing = queries::get_index_status(self.db.conn(), false)
                .map(|s| s.files_count > 0)
                .unwrap_or(false);
            let progress_cb = |_phase: IndexPhase, current: usize, total: usize| {
                self.send_progress("indexing", current, total);
            };
            let write_db = self.write_db();
            let result = if has_existing {
                self.send_log(
                    "info",
                    "Snapshot data present — running incremental drift-check...",
                );
                use crate::indexer::pipeline::run_incremental_index;
                run_incremental_index(&write_db, &project_root, None, Some(&progress_cb))?
            } else {
                self.send_log("info", "Scanning and indexing project files...");
                // Skip inline embedding for full index (too slow), background thread handles it
                run_full_index(&write_db, &project_root, None, Some(&progress_cb))?
            };
            drop(write_db);
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
            // An incremental owed from a prior skip (embedding held the write path) must
            // run even with no fresh event — `drain_watcher_events` already consumed the
            // original signal, so this sticky flag is the only record the change exists.
            let owed = self.indexing.pending_incremental.load(Ordering::Acquire);
            if has_changes || owed {
                // Skip inline embedding — background thread handles it (avoids holding model lock across I/O)
                self.run_incremental_with_cache_restore(&project_root, None)?;
                // An authoritative merkle pass just ran; restart both debounce
                // windows from here so the backstop below measures "time since
                // the index was last revalidated", not "time since the last
                // event-less tool call".
                *lock_or_recover(&self.last_incremental_check, "last_incremental_check") =
                    std::time::Instant::now();
            } else {
                // No events. Two different debounces, never a permanent skip:
                //   - no watcher: the only rescan trigger there is, so it runs often.
                //   - watcher active: a watcher can be present yet deaf (notify
                //     reports backend errors through a callback that only logs —
                //     inotify limits, network FS, container bind mounts), and its
                //     absent events are indistinguishable from "nothing changed".
                //     The longer backstop bounds that staleness instead of trusting
                //     the watcher for the rest of the session.
                let has_watcher = lock_or_recover(&self.watcher, "watcher").is_some();
                let debounce = if has_watcher {
                    self.timing.watcher_backstop
                } else {
                    self.timing.incremental_debounce
                };
                let mut last_check =
                    lock_or_recover(&self.last_incremental_check, "last_incremental_check");
                if last_check.elapsed() > debounce {
                    self.run_incremental_with_cache_restore(&project_root, None)?;
                    *last_check = std::time::Instant::now();
                }
            }
        }
        Ok(())
    }

    /// Run incremental index with cache snapshot/restore on failure.
    ///
    /// If background embedding is in progress, waits briefly for it to finish
    /// to avoid a race condition where the embedding thread inserts vectors for
    /// node IDs that are being deleted and re-inserted by the incremental index.
    fn run_incremental_with_cache_restore(
        &self,
        project_root: &Path,
        model: Option<&EmbeddingModel>,
    ) -> Result<()> {
        // Mark an incremental owed for the whole attempt, cleared only on success below.
        // The caller (`ensure_indexed`) already drained the watcher event that triggered
        // us, so this flag is the sole surviving record of the change. Arming up front (vs
        // only on the embedding-skip path) means a hard error ALSO leaves the obligation
        // recorded, so the next `ensure_indexed` retries instead of stranding the change.
        self.indexing
            .pending_incremental
            .store(true, Ordering::Release);
        if self.indexing.embedding_in_progress.load(Ordering::Acquire) {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(EMBEDDING_WAIT_SECS);
            while self.indexing.embedding_in_progress.load(Ordering::Acquire) {
                if std::time::Instant::now() > deadline {
                    // Embedding still holds the write path — skip; the incremental stays owed
                    // (flag set above) so the next ensure_indexed runs it once embedding frees up.
                    tracing::info!("Skipping incremental re-index: background embedding still in progress (incremental still owed)");
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        let mut cache_guard = lock_or_recover(&self.dir_cache, "dir_cache");
        let cache_snapshot = cache_guard.clone();
        let cache = cache_guard.take();
        drop(cache_guard); // Release lock during I/O

        let write_db = self.write_db();
        let outcome =
            run_incremental_index_cached(&write_db, project_root, model, cache.as_ref(), None);
        // Release the promoted-DB handle BEFORE any of the bookkeeping locks
        // below. `write_db()` may hold the `promoted_db` mutex (level 7 in this
        // type's lock ordering) and every lock touched from here down —
        // `dir_cache`/`last_index_stats` (3), `indexed` (2), the two caches (5) —
        // sits ABOVE it in that order. Holding 7 while taking 5 is the ordering
        // the doc comment exists to forbid; it survived because the stdio loop is
        // single-threaded today, which is precisely the assumption the ordering is
        // written to outlive (2026-08-16 audit §四). Nothing below needs the
        // handle: the FK-recovery arm re-acquires it explicitly once the
        // lower-order locks are released again.
        drop(write_db);
        match outcome {
            Ok((result, new_cache)) => {
                if result.files_indexed > 0 {
                    // Invalidate caches when files actually changed
                    *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
                    lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
                    // Refill vectors: the incremental path runs with model=None, so
                    // regenerate_context_strings invalidated (dropped) vectors for any
                    // cross-file dirty nodes, and newly-added nodes have none yet. Spawn
                    // the background embedder to backfill both. spawn_background_embedding
                    // self-guards on model-present + vec_enabled + embedding_in_progress,
                    // so it's a cheap no-op when there's nothing to embed. This single
                    // call covers both incremental callers (watcher-changes + debounce).
                    self.spawn_background_embedding();
                }
                *lock_or_recover(&self.last_index_stats, "last_index_stats") = result.stats;
                *lock_or_recover(&self.dir_cache, "dir_cache") = Some(new_cache);
                // Authoritative merkle scan completed — any owed incremental is now satisfied.
                self.indexing
                    .pending_incremental
                    .store(false, Ordering::Release);
                Ok(())
            }
            Err(e) => {
                if is_fk_constraint_error(&e) {
                    // DB state is inconsistent with in-memory caches (e.g., DB replaced
                    // externally, stale node IDs from a previous session, orphan rows
                    // left by the failed incremental, or — the cause this list used to
                    // omit — a CLI `index`/`reindex` writing the same DB concurrently,
                    // which `warn_if_index_locked` warns about but does not prevent).
                    // Naming it matters: everything else here is a rare fault, while the
                    // concurrent-CLI case is a routine user action whose price is the
                    // wipe-and-rebuild below (2026-08-16 audit §四). Recovery must
                    // truncate first —
                    // run_full_index does per-file upsert, not a global wipe, so orphan
                    // rows survive and re-trigger FK on the second attempt. Without this
                    // the fallback silently bubbles FK up to tool handlers (agent sees
                    // raw "FOREIGN KEY constraint failed" on project_map / module_overview).
                    tracing::warn!(
                        "Incremental index hit FK constraint — DB state is inconsistent. \
                         Truncating and rebuilding from scratch."
                    );
                    *lock_or_recover(&self.dir_cache, "dir_cache") = None;
                    *lock_or_recover(&self.indexed, "indexed") = false;
                    *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
                    lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
                    // CASCADE nodes→edges→node_vectors via FK ON DELETE CASCADE.
                    // The handle from above was released before the invalidations,
                    // so re-acquiring here is correct (and required — the promoted
                    // variant's mutex is not reentrant, so this must stay the only
                    // live `write_db` for the rest of the arm).
                    let write_db = self.write_db();
                    {
                        let tx = write_db.conn().unchecked_transaction()?;
                        tx.execute("DELETE FROM files", [])?;
                        tx.commit()?;
                    }

                    let progress_cb = |_phase: IndexPhase, current: usize, total: usize| {
                        self.send_progress("indexing", current, total);
                    };
                    let recovery =
                        run_full_index(&write_db, project_root, model, Some(&progress_cb));
                    drop(write_db); // same ordering rule as above
                    match recovery {
                        Ok(result) => {
                            *lock_or_recover(&self.last_index_stats, "last_index_stats") =
                                result.stats;
                            *lock_or_recover(&self.indexed, "indexed") = true;
                            self.spawn_background_embedding();
                            // Full rebuild captured current on-disk state — clear owed flag.
                            self.indexing
                                .pending_incremental
                                .store(false, Ordering::Release);
                            tracing::info!("Full re-index recovery successful");
                            Ok(())
                        }
                        Err(e2) => {
                            tracing::error!("Full re-index recovery also failed: {}", e2);
                            Err(e2)
                        }
                    }
                } else if is_db_busy_error(&e) {
                    // Transient: another connection (startup repair, embedding
                    // backfill) held the write path. The incremental stays owed
                    // via `pending_incremental`, so the next tool call retries it
                    // — same precedent as the embedding-in-progress skip above.
                    // Returning Err here would turn a healthy query into a tool
                    // error for a condition that resolves in milliseconds.
                    tracing::info!(
                        "Skipping incremental re-index: database busy ({}); incremental still owed",
                        e
                    );
                    *lock_or_recover(&self.dir_cache, "dir_cache") = cache_snapshot;
                    Ok(())
                } else {
                    tracing::error!("Incremental index failed, restoring cache: {}", e);
                    *lock_or_recover(&self.dir_cache, "dir_cache") = cache_snapshot;
                    Err(e)
                }
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
            // The payload matters: this used to discard it and treat ANY event as
            // a change, so the server's own `.code-graph/` writes (usage.jsonl,
            // recommendations.jsonl, SQLite WAL) reported changes continuously and
            // every tool call paid a full-tree merkle stat. Keep draining after the
            // first real change — leftovers would re-trigger on the next call.
            while let Ok(WatchEvent::Changed(paths)) = state.receiver.try_recv() {
                if paths
                    .iter()
                    .any(|p| !crate::indexer::watcher::is_ignored_watch_path(p))
                {
                    has_changes = true;
                }
            }
            has_changes
        } else {
            false
        }
    }

    /// Returns whether the file watcher is currently active.
    pub(super) fn is_watching(&self) -> bool {
        lock_or_recover(&self.watcher, "watcher").is_some()
    }

    pub fn handle_message(&self, line: &str) -> Result<Option<String>> {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(req) => req,
            // Deserialization failed. Fall back to a loose re-parse so we can tell
            // genuinely-invalid JSON (-32700 Parse error) apart from valid JSON
            // that just isn't a conforming Request object (-32600 Invalid Request),
            // and recover the id for response correlation. The happy path stays a
            // single allocation-free from_str; the Value re-parse only runs on error.
            Err(fast_err) => return self.reject_unparsable(line, fast_err),
        };

        // Per JSON-RPC 2.0, notifications (no id) must never receive a response
        if req.id.is_none() {
            if req.validate().is_ok() && req.method == "notifications/initialized" {
                *lock_or_recover(
                    &self.indexing.startup_index_pending,
                    "startup_index_pending",
                ) = true;
            }
            return Ok(None);
        }

        // Validate JSON-RPC version (only for requests with id)
        if let Err(msg) = req.validate() {
            let resp = JsonRpcResponse::error(
                req.id,
                super::protocol::JSONRPC_INVALID_REQUEST,
                msg.to_string(),
            );
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

    /// Build the correct JSON-RPC error reply for a `line` that failed to
    /// deserialize into a [`JsonRpcRequest`]. Distinguishes malformed JSON
    /// (`-32700` Parse error) from valid JSON that is not a conforming Request
    /// object (`-32600` Invalid Request), and echoes the request `id` when one
    /// can be recovered so a client can still correlate the failure to its call
    /// (JSON-RPC 2.0 §5). A malformed message with no `id` member is treated as
    /// a Notification and receives no reply, matching the spec rule that
    /// Notifications are never answered.
    fn reject_unparsable(&self, line: &str, fast_err: serde_json::Error) -> Result<Option<String>> {
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                // Genuinely invalid JSON: nothing to correlate, id is null.
                let resp = JsonRpcResponse::error(
                    None,
                    super::protocol::JSONRPC_PARSE_ERROR,
                    format!("Parse error: {}", fast_err),
                );
                return Ok(Some(serde_json::to_string(&resp)?));
            }
        };

        // Top-level arrays (JSON-RPC batches, which this server does not support)
        // and bare scalars can never be a single Request object. A client that
        // sent one is still waiting for a reply, so answer with Invalid Request
        // (id null) rather than hanging it or leaking a serde type error.
        if !value.is_object() {
            let detail = if value.is_array() {
                "batch requests are not supported"
            } else {
                "expected a JSON-RPC request object"
            };
            let resp = JsonRpcResponse::error(
                None,
                super::protocol::JSONRPC_INVALID_REQUEST,
                format!("Invalid Request: {}", detail),
            );
            return Ok(Some(serde_json::to_string(&resp)?));
        }

        // Valid JSON object that isn't a conforming Request (missing/mistyped
        // `method`, wrong `jsonrpc` type, …). No `id` member marks a (malformed)
        // Notification — never answered. Otherwise echo a recoverable id.
        if value.get("id").is_none() {
            return Ok(None);
        }
        let recovered_id = match value.get("id") {
            Some(id) if id.is_number() || id.is_string() => Some(id.clone()),
            _ => None,
        };
        let resp = JsonRpcResponse::error(
            recovered_id,
            super::protocol::JSONRPC_INVALID_REQUEST,
            format!("Invalid Request: {}", fast_err),
        );
        Ok(Some(serde_json::to_string(&resp)?))
    }

    fn handle_initialize(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        // CODE_GRAPH_QUIET_HOOKS=1 → ship a one-liner pointer; full decision
        // rules live in the project's .claude/plugin_code_graph_mcp.md (the
        // CLAUDE.md managed block points to it; auto-installed on plugin SessionStart).
        let quiet = std::env::var("CODE_GRAPH_QUIET_HOOKS").ok().as_deref() == Some("1");
        let instructions = if quiet {
            INSTRUCTIONS_QUIET
        } else {
            INSTRUCTIONS_NOISY
        };
        JsonRpcResponse::success(
            id,
            json!({
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
                "instructions": instructions
            }),
        )
    }

    fn handle_tools_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        let tools: Vec<serde_json::Value> = self
            .registry
            .list_tools()
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect();

        JsonRpcResponse::success(id, json!({ "tools": tools }))
    }

    fn handle_tools_call(
        &self,
        id: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
    ) -> JsonRpcResponse {
        let params = match params {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(
                    id,
                    super::protocol::JSONRPC_INVALID_PARAMS,
                    "Missing params".into(),
                )
            }
        };

        let tool_name = match params["name"].as_str() {
            Some(name) => name,
            None => {
                return JsonRpcResponse::error(
                    id,
                    super::protocol::JSONRPC_INVALID_PARAMS,
                    "Missing or invalid 'name' in tool call params".into(),
                )
            }
        };
        let arguments = &params["arguments"];

        match self.handle_tool(tool_name, arguments) {
            Ok(result) => {
                let text = serde_json::to_string(&result)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {}\"}}", e));
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": text
                        }]
                    }),
                )
            }
            Err(e) => {
                tracing::warn!("[tool-error] {}: {}", tool_name, e);
                let err_str = e.to_string();
                let mut text = format!("Error: {}", err_str);
                // Secondary (read-only) instances never reindex, so a "not found"
                // can mean the symbol exists on disk but the primary hasn't indexed
                // it yet — otherwise indistinguishable from a typo. Disambiguate it.
                if !self.is_primary() && err_str.contains("not found in") {
                    text.push_str(
                        " Note: this code-graph instance is in read-only secondary mode \
                         (another instance holds the index lock) and does not reindex on \
                         its own — if you recently edited files, the symbol may not be \
                         indexed here yet; the primary instance will pick it up shortly.",
                    );
                }
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": text
                        }],
                        "isError": true
                    }),
                )
            }
        }
    }

    fn handle_resources_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(
            id,
            json!({
                "resources": [{
                    "uri": "code-graph://project-summary",
                    "name": "Code Graph Project Summary",
                    "description": "Overview of the indexed codebase: file count, node count, edge count, languages, and index health",
                    "mimeType": "application/json",
                    "annotations": {
                        "audience": ["assistant"]
                    }
                }]
            }),
        )
    }

    fn handle_resources_read(
        &self,
        id: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
    ) -> JsonRpcResponse {
        let uri = params
            .as_ref()
            .and_then(|p| p["uri"].as_str())
            .unwrap_or("");

        match uri {
            "code-graph://project-summary" => {
                let status = match queries::get_index_status(self.db.conn(), self.is_watching()) {
                    Ok(s) => s,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            id,
                            super::protocol::JSONRPC_INTERNAL_ERROR,
                            format!("Failed to get index status: {}", e),
                        )
                    }
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

                JsonRpcResponse::success(
                    id,
                    json!({
                        "contents": [{
                            "uri": "code-graph://project-summary",
                            "mimeType": "application/json",
                            "text": serde_json::to_string_pretty(&summary).unwrap_or_default()
                        }]
                    }),
                )
            }
            _ => JsonRpcResponse::error(
                id,
                super::protocol::JSONRPC_INVALID_PARAMS,
                format!("Unknown resource URI: {}", uri),
            ),
        }
    }

    fn handle_prompts_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(
            id,
            json!({
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
            }),
        )
    }

    fn handle_prompts_get(
        &self,
        id: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
    ) -> JsonRpcResponse {
        let name = params
            .as_ref()
            .and_then(|p| p["name"].as_str())
            .unwrap_or("");
        let arguments = params.as_ref().and_then(|p| p["arguments"].as_object());

        match name {
            "impact-analysis" => {
                let symbol = arguments
                    .and_then(|a| a.get("symbol_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<symbol>");
                JsonRpcResponse::success(
                    id,
                    json!({
                        "messages": [{
                            "role": "user",
                            "content": {
                                "type": "text",
                                "text": format!(
                                    "Analyze the impact of changing the symbol '{}'. \
                                     Use get_ast_node with include_impact=true and symbol_name='{}' to get the blast radius, \
                                     then use get_call_graph to understand the full caller/callee chain. \
                                     Present: affected files, affected routes, risk level, and recommendations.",
                                    symbol, symbol
                                )
                            }
                        }]
                    }),
                )
            }
            "understand-module" => {
                let path = arguments
                    .and_then(|a| a.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<path>");
                JsonRpcResponse::success(
                    id,
                    json!({
                        "messages": [{
                            "role": "user",
                            "content": {
                                "type": "text",
                                "text": format!(
                                    "Give me a deep understanding of the module at '{}'. \
                                     Use module_overview with include_deps=true to get exports, hot paths, \
                                     and what it depends on and what depends on it. \
                                     For the top 3 most-called exports, use get_call_graph to show their caller chain. \
                                     Present: purpose, public API, dependencies, dependents, and hot paths.",
                                    path
                                )
                            }
                        }]
                    }),
                )
            }
            "trace-request" => {
                let route = arguments
                    .and_then(|a| a.get("route"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<route>");
                JsonRpcResponse::success(
                    id,
                    json!({
                        "messages": [{
                            "role": "user",
                            "content": {
                                "type": "text",
                                "text": format!(
                                    "Trace the complete HTTP request flow for route '{}'. \
                                     Use get_call_graph with route_path='{}' to get the full chain from route to data layer. \
                                     For each key node, use get_ast_node(node_id=N, context_lines=5) to show the implementation. \
                                     Map the flow: route → middleware → validation → business logic → data access → response. \
                                     Highlight error handling, auth checks, and database operations.",
                                    route, route
                                )
                            }
                        }]
                    }),
                )
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
        let result = self.dispatch_tool(name, args);
        // FRS-2: tools without a `file_path` argument get result-set freshness here.
        let result = match result {
            Ok(value) if RESULT_REFRESH_TOOLS.contains(&name) => {
                Ok(self.refresh_result_set(name, args, value))
            }
            other => other,
        };
        let elapsed = start.elapsed();
        let err_msg = result.as_ref().err().map(|e| e.to_string());
        let err_kind = err_msg.as_deref().map(ErrKind::classify);
        lock_or_recover(&self.metrics, "metrics").record_tool_call(
            name,
            elapsed.as_millis() as u64,
            err_kind,
            err_msg.as_deref(),
        );
        if elapsed.as_millis() > 100 {
            tracing::info!("[tool] {} completed in {:.1}s", name, elapsed.as_secs_f64());
        } else {
            tracing::debug!("[tool] {} completed in {}ms", name, elapsed.as_millis());
        }
        // Centralized compression: safety net for any result exceeding the token threshold.
        // Handlers with custom compression (semantic_search, call_graph, http_chain, ast_node)
        // already return results with a "mode" key when compressed — those are left unchanged.
        //
        // The ignored-argument note is attached AFTER compression on purpose: every
        // compaction path in this codebase is an explicit field allowlist, and a new
        // top-level key that forgets to enrol in one gets silently dropped — the exact
        // bug the v0.97.1 audit found for `deps`. Attaching last makes that impossible.
        result
            .map(centralized_compress)
            .map(|value| self.note_ignored_arguments(name, args, value))
    }

    /// Name back any argument the tool does not declare, as
    /// `"ignored_arguments": ["language", …]` on the result object.
    ///
    /// Undeclared members were dropped in silence, which is fine for a human
    /// reading a schema and fatal for the actual caller here: an LLM that sent
    /// `ast_search {"language": "banana"}` got the whole repo back and had no way
    /// to tell it apart from a language-filtered answer, so it reported the wrong
    /// scope downstream (QA ISSUE-015). Rejecting the call outright would be the
    /// other defensible reading, but it breaks the lenient-extra-members
    /// convention every JSON-RPC client relies on, and it would turn a mislabelled
    /// answer into no answer at all. Disclosure keeps the call working and lets
    /// the caller see what its argument did — none of it.
    ///
    /// Only tools that publish an inputSchema are covered — the 7 in the registry,
    /// plus `read_snippet`, which is a pure rename of `get_ast_node`. The hidden
    /// backends (`trace_http_chain`, `dependency_graph`, `find_similar_code`,
    /// `find_dead_code`, and the management tools) declare no properties anywhere,
    /// so there is nothing to check them against; they are skipped rather than
    /// reported against an empty allowlist, which would flag every real argument.
    ///
    /// The schema alone is NOT the honored set — see [`HONORED_UNDECLARED_ARGS`].
    fn note_ignored_arguments(
        &self,
        name: &str,
        args: &serde_json::Value,
        mut result: serde_json::Value,
    ) -> serde_json::Value {
        const MAX_REPORTED: usize = 10;
        let schema_name = if name == "read_snippet" {
            "get_ast_node"
        } else {
            name
        };
        let Some(sent) = args.as_object() else {
            return result;
        };
        let Some(declared) = self
            .registry
            .list_tools()
            .iter()
            .find(|t| t.name == schema_name)
            .and_then(|t| t.input_schema.get("properties"))
            .and_then(|p| p.as_object())
        else {
            return result;
        };
        let honored_undeclared = |key: &str| {
            HONORED_UNDECLARED_ARGS
                .iter()
                .any(|(tool, arg)| *arg == key && (*tool == "*" || *tool == schema_name))
        };
        let mut ignored: Vec<&str> = sent
            .keys()
            .filter(|k| !declared.contains_key(k.as_str()) && !honored_undeclared(k))
            .map(|k| k.as_str())
            .collect();
        if ignored.is_empty() {
            return result;
        }
        ignored.sort_unstable();
        ignored.truncate(MAX_REPORTED); // the list is caller-supplied; keep it bounded
        tracing::warn!(
            "[tool] {} received undeclared arguments: {}",
            name,
            ignored.join(", ")
        );
        if let Some(obj) = result.as_object_mut() {
            obj.insert("ignored_arguments".into(), json!(ignored));
        }
        result
    }

    fn dispatch_tool(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        match name {
            "semantic_code_search" => self.tool_semantic_search(args),
            "get_call_graph" => self.tool_get_call_graph(args),
            "find_http_route" | "trace_http_chain" => self.tool_trace_http_chain(args),
            "get_ast_node" | "read_snippet" => self.tool_get_ast_node(args),
            "start_watch" => self.tool_start_watch(),
            "stop_watch" => self.tool_stop_watch(),
            "get_index_status" => self.tool_get_index_status(),
            "rebuild_index" => self.tool_rebuild_index(args),
            "module_overview" => self.tool_module_overview(args),
            "dependency_graph" => self.tool_dependency_graph(args),
            "find_similar_code" => self.tool_find_similar_code(args),
            "project_map" => self.tool_project_map(args),
            "ast_search" => self.tool_ast_search(args),
            "find_references" => self.tool_find_references(args),
            "find_dead_code" => self.tool_find_dead_code(args),
            _ => Err(anyhow!("Unknown tool: {}", name)),
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Release the index lock on drop (covers panics, not SIGKILL)
        if self.is_primary() {
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
        })
        .to_string()
    }

    fn parse_tool_result(response: &Option<String>) -> serde_json::Value {
        let resp = response.as_ref().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn test_backfill_outcome_no_model_never_advances_floor() {
        // The bug this guards: on a fresh install the model is still downloading at the
        // first periodic tick. The drain embeds nothing (NoModel) — but if that advanced
        // the floor, every node would strand at 0% vec until restart. NoModel must leave
        // BOTH floor and retries untouched so the next tick re-attempts once the model
        // lands. `remeasured` is irrelevant here and must be ignored.
        assert_eq!(
            super::backfill::apply_backfill_outcome(
                0,
                0,
                super::backfill::BackfillOutcome::NoModel,
                500
            ),
            (0, 0)
        );
        assert_eq!(
            super::backfill::apply_backfill_outcome(
                7,
                2,
                super::backfill::BackfillOutcome::NoModel,
                500
            ),
            (7, 2)
        );
    }

    #[test]
    fn test_backfill_outcome_drained_resets() {
        // A full drain means the embeddable set emptied. Floor → 0 so any later count is
        // treated as fresh work, and the stall-retry budget resets. `remeasured` is
        // ignored (a race-added node should be picked up next tick, not skipped).
        assert_eq!(
            super::backfill::apply_backfill_outcome(
                0,
                0,
                super::backfill::BackfillOutcome::Drained,
                0
            ),
            (0, 0)
        );
        assert_eq!(
            super::backfill::apply_backfill_outcome(
                42,
                2,
                super::backfill::BackfillOutcome::Drained,
                3
            ),
            (0, 0)
        );
    }

    #[test]
    fn test_backfill_outcome_stalled_no_progress_retries_then_pins() {
        // A zero-progress stall is ambiguous (transient vs un-embeddable). Below the retry
        // budget the floor stays low (so the next tick re-attempts and a transient failure
        // self-heals) while the retry counter climbs.
        let stalled = super::backfill::BackfillOutcome::Stalled { progressed: false };
        let (f1, r1) = super::backfill::apply_backfill_outcome(0, 0, stalled, 9);
        assert_eq!(
            (f1, r1),
            (0, 1),
            "first stall: floor stays low, retry counted"
        );
        let (f2, r2) = super::backfill::apply_backfill_outcome(f1, r1, stalled, 9);
        assert_eq!((f2, r2), (0, 2), "second stall: still retrying");
        // Budget spent (MAX = 3): pin the floor to the residue and reset retries so a
        // FUTURE rise above this floor still re-triggers a drain.
        let (f3, r3) = super::backfill::apply_backfill_outcome(f2, r2, stalled, 9);
        assert_eq!(
            (f3, r3),
            (9, 0),
            "budget spent: pin floor to residue, reset retries"
        );
    }

    #[test]
    fn test_backfill_outcome_stalled_with_progress_pins_immediately() {
        // A stall AFTER embedding some nodes proves the model works, so the remainder is
        // genuine residue — pin the floor at once (no retry budget consumed) instead of
        // churning a proven-working model on un-embeddable content.
        let progressed = super::backfill::BackfillOutcome::Stalled { progressed: true };
        assert_eq!(
            super::backfill::apply_backfill_outcome(0, 0, progressed, 5),
            (5, 0)
        );
        // Even with retries already accrued, a progressed stall pins and resets them.
        assert_eq!(
            super::backfill::apply_backfill_outcome(0, 2, progressed, 5),
            (5, 0)
        );
    }

    #[test]
    fn test_is_fk_constraint_error_matches_full_chain() {
        // A bare FK error matches.
        let bare = anyhow::anyhow!("FOREIGN KEY constraint failed");
        assert!(is_fk_constraint_error(&bare));
        // A .context()-WRAPPED FK error: the outer message hides the cause, so a
        // to_string() substring check would MISS it — this is exactly why the predicate
        // matches the full chain ({:#}). Locks the W3 fix against a future .context() regression.
        let wrapped = bare.context("while running incremental index");
        assert!(
            !wrapped
                .to_string()
                .contains("FOREIGN KEY constraint failed"),
            "precondition: the outer message hides the cause from to_string()"
        );
        assert!(
            is_fk_constraint_error(&wrapped),
            "full-chain match must still detect the wrapped FK cause"
        );
        // A non-FK error must not match (no spurious truncate+rebuild).
        assert!(!is_fk_constraint_error(&anyhow::anyhow!("disk full")));
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
            upsert_file(
                server.db().conn(),
                &FileRecord {
                    path: "a.rs".into(),
                    blake3_hash: "h".into(),
                    last_modified: 1,
                    language: Some("rust".into()),
                },
            )
            .unwrap();
        }

        let req = tool_call_json("get_index_status", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["files_count"], 1);
        assert_eq!(
            result["schema_version"],
            crate::storage::schema::SCHEMA_VERSION
        );
    }

    /// Extract the results array from a `semantic_code_search` response. Every
    /// path — hybrid, FTS5-only degradation, empty, compressed — returns the same
    /// `{results, …}` envelope (the bare-array happy path was removed so the
    /// response can carry `ignored_arguments` / `freshness`; see
    /// `finalize_search_results`).
    fn search_results(v: &serde_json::Value) -> Vec<serde_json::Value> {
        v.get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default()
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
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json(
            "semantic_code_search",
            json!({"query": "validateToken", "top_k": 3}),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        // No embedding model in unit tests → FTS5-only object path; assert the
        // degradation signal is surfaced, then read results shape-agnostically.
        assert_eq!(
            result["vector_available"],
            serde_json::json!(false),
            "no-model test env must report vector_available=false, got: {}",
            result
        );
        let results = search_results(&result);
        assert!(!results.is_empty(), "search should return results");
        let names: Vec<&str> = results.iter().filter_map(|r| r["name"].as_str()).collect();
        assert!(names.contains(&"validateToken"), "got names: {:?}", names);
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
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        // Trigger indexing
        let _ = server
            .handle_message(&tool_call_json("get_index_status", json!({})))
            .unwrap();
        server.ensure_indexed().unwrap();

        let req = tool_call_json(
            "get_call_graph",
            json!({
                "function_name": "handleLogin",
                "direction": "callees",
                "depth": 2
            }),
        );
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
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json(
            "get_ast_node",
            json!({
                "file_path": "utils.ts",
                "symbol_name": "helper"
            }),
        );
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
        )
        .unwrap();

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
        assert!(result["code_content"]
            .as_str()
            .unwrap()
            .contains("return 1"));
    }

    /// An argument the tool never declared used to vanish without a trace. The
    /// caller is an LLM that cannot see the difference between "filtered by
    /// language" and "language ignored, here is the whole repo" — QA ISSUE-015
    /// reached `ast_search {"language": "banana"}` and got unfiltered results it
    /// then reported as language-scoped. The call still succeeds (JSON-RPC
    /// consumers are conventionally lenient about extra members); it now says
    /// what it dropped.
    #[test]
    fn test_undeclared_tool_argument_is_reported_not_swallowed() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function alpha() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json(
            "ast_search",
            json!({"query": "alpha", "language": "banana"}),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(
            result["ignored_arguments"],
            json!(["language"]),
            "an undeclared argument must be named back to the caller, got: {result}"
        );

        // Control: the identical call WITHOUT the stray argument must carry no
        // such key, or the field would be noise every tool call pays for.
        let req_ok = tool_call_json("ast_search", json!({"query": "alpha"}));
        let result_ok = parse_tool_result(&server.handle_message(&req_ok).unwrap());
        assert!(
            result_ok.get("ignored_arguments").is_none(),
            "a clean call must not carry the key at all, got: {result_ok}"
        );

        // Second control: a DECLARED argument must never be reported, however
        // little the handler ends up using it. `limit` is in ast_search's schema.
        let req_declared = tool_call_json("ast_search", json!({"query": "alpha", "limit": 5}));
        let result_declared = parse_tool_result(&server.handle_message(&req_declared).unwrap());
        assert!(
            result_declared.get("ignored_arguments").is_none(),
            "declared arguments must not be flagged, got: {result_declared}"
        );

        // The alias arm: `read_snippet` is a pure rename of `get_ast_node` and
        // has no registry entry of its own, so it must be checked against
        // get_ast_node's schema — not skipped, and not flagged wholesale.
        let node_id = queries::get_nodes_by_name(server.db().conn(), "alpha").unwrap()[0].id;
        let req_alias = tool_call_json("read_snippet", json!({"node_id": node_id, "bogus": 1}));
        let result_alias = parse_tool_result(&server.handle_message(&req_alias).unwrap());
        assert_eq!(
            result_alias["ignored_arguments"],
            json!(["bogus"]),
            "the alias must resolve to get_ast_node's schema: {result_alias}"
        );
        let req_alias_ok = tool_call_json("read_snippet", json!({"node_id": node_id}));
        let result_alias_ok = parse_tool_result(&server.handle_message(&req_alias_ok).unwrap());
        assert!(
            result_alias_ok.get("ignored_arguments").is_none(),
            "`node_id` IS declared on get_ast_node: {result_alias_ok}"
        );
    }

    /// `semantic_code_search` is the most-called tool and was the one whose
    /// response could not carry the disclosure: its confident-hybrid path
    /// returned a bare JSON array, and `note_ignored_arguments` /
    /// `refresh_result_set` both attach through `as_object_mut()` (audit
    /// 2026-08-16 P1-10). This drives the real dispatch and asserts the envelope
    /// AND the disclosure.
    ///
    /// Scope note, so the coverage is not overstated: with no embedding model
    /// loaded (every unit-test run, both feature sets) the tool takes the
    /// FTS-only arm, which was already an object — this test would NOT have gone
    /// red before the fix. The arm that WAS an array is unreachable without a
    /// loaded model, so it is pinned one layer down, where it is observable:
    /// `search::tests::every_response_shape_is_an_object_that_can_carry_disclosures`.
    #[test]
    fn test_semantic_search_response_is_an_envelope_that_carries_disclosures() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("a.ts"),
            "export function alphaHandler() { return 1; }\n",
        )
        .unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json(
            "semantic_code_search",
            json!({"query": "alphaHandler", "langauge": "typescript"}),
        );
        let result = parse_tool_result(&server.handle_message(&req).unwrap());
        assert!(
            result.is_object(),
            "the response must be an object on every path, got: {result}"
        );
        assert!(
            result["results"].is_array(),
            "the result list must live under `results`, got: {result}"
        );
        assert_eq!(
            result["ignored_arguments"],
            json!(["langauge"]),
            "a misspelled argument must be named back, got: {result}"
        );

        // Control: the same query without the typo carries no such key.
        let req_ok = tool_call_json("semantic_code_search", json!({"query": "alphaHandler"}));
        let result_ok = parse_tool_result(&server.handle_message(&req_ok).unwrap());
        assert!(
            result_ok.get("ignored_arguments").is_none(),
            "a clean call must not carry the key, got: {result_ok}"
        );
    }

    /// The disclosure claims "the tool did nothing with this", and the published
    /// schema is not by itself a sound source for that claim: two arguments are
    /// honored without being declared. Reporting them as ignored would invert the
    /// feature — the caller would be told the argument that actually selected its
    /// answer had been dropped. Found by the pre-tag review of this batch.
    #[test]
    fn test_honored_but_undeclared_arguments_are_not_reported_as_ignored() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("a.ts"),
            "function alpha() { return beta(); }\nfunction beta() { return 1; }\n",
        )
        .unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // `function_name` is get_call_graph's live legacy alias for symbol_name
        // (callgraph.rs) — it SELECTS the symbol being graphed.
        let req = tool_call_json("get_call_graph", json!({"function_name": "alpha"}));
        let result = parse_tool_result(&server.handle_message(&req).unwrap());
        assert!(
            result.get("ignored_arguments").is_none(),
            "function_name chose the symbol; calling it ignored is a lie: {result}"
        );

        // `skip_indexing` is read by every tool through should_skip_indexing.
        let req2 = tool_call_json(
            "ast_search",
            json!({"query": "alpha", "skip_indexing": true}),
        );
        let result2 = parse_tool_result(&server.handle_message(&req2).unwrap());
        assert!(
            result2.get("ignored_arguments").is_none(),
            "skip_indexing is honored on every tool: {result2}"
        );

        // Control: the exemption is per-argument, not a blanket amnesty — an
        // undeclared argument on the SAME calls is still reported.
        let req3 = tool_call_json(
            "get_call_graph",
            json!({"function_name": "alpha", "language": "banana"}),
        );
        let result3 = parse_tool_result(&server.handle_message(&req3).unwrap());
        assert_eq!(
            result3["ignored_arguments"],
            json!(["language"]),
            "got: {result3}"
        );

        // Control: the per-tool exemption does not leak to other tools.
        let req4 = tool_call_json(
            "ast_search",
            json!({"query": "alpha", "function_name": "alpha"}),
        );
        let result4 = parse_tool_result(&server.handle_message(&req4).unwrap());
        assert_eq!(
            result4["ignored_arguments"],
            json!(["function_name"]),
            "ast_search does not honor function_name: {result4}"
        );
    }

    #[test]
    fn test_rebuild_index_requires_confirm() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json("rebuild_index", json!({"confirm": false}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(
            parsed["result"]["isError"].as_bool().unwrap_or(false)
                || parsed["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Error")
        );
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
        // Parity with the CLI's `--json` index envelope (emit_index_json), which
        // has always carried this counter. Unlike get_index_status — whose stats
        // may be left over from no run at all — the rebuild just happened here,
        // so a 0 is EARNED and must be stated rather than omitted: silence would
        // be indistinguishable from "this surface cannot tell you".
        assert_eq!(
            result["files_with_parse_errors"], 0,
            "a clean rebuild must report the count, not omit it: {result:?}"
        );
    }

    #[test]
    fn test_rebuild_index_counts_files_with_parse_errors() {
        // Non-vacuous half of the guard above: a file whose syntax is broken but
        // still salvageable lands IN the index via tree-sitter error recovery,
        // and the count is the only signal that its symbols may be partial.
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("ok.ts"), "function a() {}").unwrap();
        std::fs::write(
            project_dir.path().join("broken.ts"),
            "function b( { const x = ;\nfunction c() {}\n",
        )
        .unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json("rebuild_index", json!({"confirm": true}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "rebuilt");
        assert!(
            result["files_with_parse_errors"].as_i64().unwrap() >= 1,
            "a syntactically broken file must be counted: {result:?}"
        );
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

        // Poll up to 8s for the watcher-triggered reindex to complete. FSEvents
        // on macOS (and a cold CI runner in general) coalesces change events
        // with 1–2s latency, so a fixed 300ms sleep flaked roughly every other
        // macOS CI run. Bounded polling is correct-on-slow-host, cheap-on-fast.
        let mut nodes = Vec::new();
        for _ in 0..40 {
            server.ensure_indexed().unwrap();
            nodes = queries::get_nodes_by_name(server.db().conn(), "changed").unwrap();
            if !nodes.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert_eq!(
            nodes.len(),
            1,
            "changed function should be indexed after watcher-triggered reindex"
        );

        // Stop watching
        let req = tool_call_json("stop_watch", json!({}));
        let _ = server.handle_message(&req).unwrap();
    }

    #[test]
    fn test_secondary_not_found_includes_stale_hint() {
        // A read-only secondary instance never reindexes, so a "not found" may mean
        // the symbol is on disk but the primary hasn't indexed it yet. The error
        // message must disambiguate that from a plain typo; the primary must not.
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function realFn() {}").unwrap();

        // Populate the shared on-disk index as a primary.
        let primary = McpServer::new_test_with_project(project_dir.path());
        primary.ensure_indexed().unwrap();

        let req = tool_call_json(
            "get_call_graph",
            json!({"symbol_name": "zzz_absent_symbol", "direction": "callees"}),
        );

        // Secondary: same DB file, is_primary flipped off → hint appended.
        // No project_root-less server here, so keep promotion from firing and
        // flipping the role back: this test is about the hint, not the role.
        let mut secondary = McpServer::new_test_with_project(project_dir.path());
        secondary.is_primary.store(false, Ordering::SeqCst);
        secondary.timing.promotion_retry = std::time::Duration::from_secs(3600);
        *lock_or_recover(&secondary.last_promotion_attempt, "last_promotion_attempt") =
            std::time::Instant::now();
        let resp = secondary.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(parsed["result"]["isError"], serde_json::json!(true));
        assert!(
            text.contains("not found"),
            "should still report not found: {text}"
        );
        assert!(
            text.contains("secondary mode"),
            "secondary not-found must carry the stale-index hint: {text}"
        );

        // Primary: identical query must NOT carry the secondary hint.
        let resp2 = primary.handle_message(&req).unwrap().unwrap();
        let parsed2: serde_json::Value = serde_json::from_str(&resp2).unwrap();
        let text2 = parsed2["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text2.contains("not found"),
            "primary should report not found: {text2}"
        );
        assert!(
            !text2.contains("secondary mode"),
            "primary must not add the secondary hint: {text2}"
        );
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
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Parse error"));
    }

    #[test]
    fn test_missing_method_is_invalid_request_not_parse_error() {
        // Valid JSON, but not a conforming Request (no `method`). Per JSON-RPC
        // 2.0 this is -32600 Invalid Request, not -32700 Parse error, and the id
        // must be echoed so the client can correlate the failure to its call.
        let server = McpServer::new_test();
        let resp = server
            .handle_message(r#"{"jsonrpc":"2.0","id":4}"#)
            .unwrap()
            .expect("a request with an id must receive a reply");
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32600);
        assert_eq!(parsed["id"], 4);
    }

    #[test]
    fn test_missing_jsonrpc_is_invalid_request_with_id() {
        let server = McpServer::new_test();
        let resp = server
            .handle_message(r#"{"id":"call-9","method":"tools/list"}"#)
            .unwrap()
            .expect("a request with an id must receive a reply");
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32600);
        assert_eq!(parsed["id"], "call-9"); // string id echoed verbatim
    }

    #[test]
    fn test_batch_request_rejected_cleanly() {
        // Batch (array) requests are unsupported; a client sending one is waiting,
        // so it must get an Invalid Request reply, not silence or a serde leak.
        let server = McpServer::new_test();
        let batch = r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"}]"#;
        let resp = server.handle_message(batch).unwrap().expect("must reply");
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32600);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("batch"));
    }

    #[test]
    fn test_malformed_notification_returns_none() {
        // No `id` member → a (malformed) Notification. Even though it fails to
        // deserialize (no `method`), the server must not reply — nobody is
        // listening for a response to a notification.
        let server = McpServer::new_test();
        let resp = server.handle_message(r#"{"jsonrpc":"2.0"}"#).unwrap();
        assert!(
            resp.is_none(),
            "malformed notifications must never receive a response"
        );
    }

    #[test]
    fn test_notification_with_invalid_version_returns_none() {
        let server = McpServer::new_test();
        // Notification (no id) with wrong JSON-RPC version — must still return None per spec
        let req = r#"{"jsonrpc":"1.0","method":"notifications/initialized"}"#;
        let resp = server.handle_message(req).unwrap();
        assert!(
            resp.is_none(),
            "malformed notifications must never receive a response"
        );
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
        assert!(
            text.contains("Error"),
            "unknown tool should return error in content"
        );
        assert!(parsed["result"]["isError"].as_bool().unwrap_or(false));
    }

    #[test]
    fn test_semantic_search_language_filter() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("app.ts"),
            "function handler() { return 1; }",
        )
        .unwrap();
        std::fs::write(
            project_dir.path().join("app.py"),
            "def handler():\n    return 1\n",
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());

        // Search with language filter for typescript
        let req = tool_call_json(
            "semantic_code_search",
            json!({
                "query": "handler",
                "language": "typescript"
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        let results = search_results(&result);
        for r in &results {
            assert!(
                r["file_path"].as_str().unwrap().ends_with(".ts"),
                "language filter should only return typescript files, got: {}",
                r["file_path"]
            );
        }
    }

    #[test]
    fn test_semantic_search_unknown_language_rejected() {
        // Parity with CLI `search` and the node_type guard: an unknown language
        // must return an error the caller can act on, not silently empty results.
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("app.ts"),
            "function handler() { return 1; }",
        )
        .unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json(
            "semantic_code_search",
            json!({
                "query": "handler",
                "language": "pyton"
            }),
        );
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            parsed["result"]["isError"].as_bool().unwrap_or(false),
            "unknown language must be an error result; got: {text}"
        );
        assert!(
            text.contains("Unknown language filter") && text.contains("pyton"),
            "error text must name the bad language and valid set; got: {text}"
        );
    }

    #[test]
    fn test_semantic_search_language_case_insensitive() {
        // Guards the load-bearing canonicalization: the MCP downstream language
        // filter is case-SENSITIVE (`nwf.language != Some(lang)`), so a mixed-case
        // language works ONLY because canonical_language normalizes the input.
        // Without it (validate then pass raw), "TypeScript" would silently return
        // no results while "typescript" works — all other tests still green.
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("app.ts"),
            "function handler() { return 1; }",
        )
        .unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());
        let ask = |lang: &str| {
            let req = tool_call_json(
                "semantic_code_search",
                json!({ "query": "handler", "language": lang }),
            );
            let resp = server.handle_message(&req).unwrap();
            search_results(&parse_tool_result(&resp)).len()
        };
        let lower = ask("typescript");
        assert!(
            lower > 0,
            "sanity: lowercase 'typescript' must return the .ts match"
        );
        assert_eq!(
            ask("TypeScript"),
            lower,
            "mixed-case must match lowercase (canonicalized)"
        );
        assert_eq!(
            ask("TYPESCRIPT"),
            lower,
            "upper-case must match lowercase (canonicalized)"
        );
    }

    #[test]
    fn test_semantic_search_node_type_filter() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("mix.ts"),
            r#"
class UserService {
    getUser() { return null; }
}
function standalone() { return 1; }
"#,
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json(
            "semantic_code_search",
            json!({
                "query": "user",
                "node_type": "class"
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        let results = search_results(&result);
        for r in &results {
            assert_eq!(
                r["type"].as_str().unwrap(),
                "class",
                "node_type filter should only return classes"
            );
        }
    }

    #[test]
    fn test_semantic_search_sandbox_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create many functions with large code to exceed 2000 token threshold.
        //
        // The names carry an ALPHABETIC suffix, not a numeric one. The fixture
        // used `func0`..`func19` and queried "func", which returns ZERO results:
        // `split_identifier` splits on case boundaries but not on the
        // letter↔digit boundary, so `func0` is one token and "func" matches
        // nothing. Measured against the real binary — `handleRequest` is findable
        // as "handle", `func0` is not findable as "func". So this test, named for
        // compression, was running the compressor over an empty result set
        // (2026-08-16 audit §四). `funcAlpha` splits into `func` + `alpha`, which
        // is what makes the query match all 20 and the payload large enough to
        // cross the threshold.
        const SUFFIXES: [&str; 20] = [
            "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta", "Theta", "Iota", "Kappa",
            "Lambda", "Mu", "Nu", "Xi", "Omicron", "Pi", "Rho", "Sigma", "Tau", "Upsilon",
        ];
        let mut code = String::new();
        for suffix in SUFFIXES {
            code.push_str(&format!(
                "function func{}() {{\n{}\n}}\n",
                suffix,
                format!("  // {}\n", "x".repeat(500)).repeat(3)
            ));
        }
        std::fs::write(project_dir.path().join("big.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json(
            "semantic_code_search",
            json!({
                "query": "func",
                "top_k": 20
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Precondition, asserted rather than assumed: without results there is
        // nothing to compress, and every assertion below is vacuous. This is
        // exactly how the test passed while measuring nothing.
        let hits = result["results"].as_array().map(|a| a.len()).unwrap_or(0);
        assert!(
            hits > 0,
            "the fixture must actually be searchable — 0 results means this test \
             compresses an empty payload: {result}"
        );

        // And the payload must be big enough to REACH the compressor, or the
        // compressed arm below is dormant.
        let mode = result["mode"].as_str().unwrap_or("");
        assert!(
            mode.starts_with("compressed_"),
            "20 functions x ~1.5KB must cross COMPRESSION_TOKEN_THRESHOLD; got mode {mode:?} \
             with {hits} result(s). If this fires, the fixture stopped exercising compression."
        );
        assert!(!result["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_find_http_route_with_downstream() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("server.ts"),
            r#"
function validateToken(token: string) { return true; }

function handleLogin(req: Request) {
    validateToken(req.token);
    return createSession(req.userId);
}

app.post('/api/login', handleLogin);
"#,
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json(
            "find_http_route",
            json!({
                "route_path": "/api/login",
                "include_middleware": true
            }),
        );
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
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        // Request absurdly large top_k — should not error, just return clamped results
        let req = tool_call_json(
            "semantic_code_search",
            json!({
                "query": "hello",
                "top_k": 999999
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        // Should succeed (`{results, …}` envelope, or a compressed mode) — not crash/OOM.
        assert!(
            result.get("results").is_some() || result["mode"].as_str() == Some("compressed"),
            "search with huge top_k should return valid results, got: {}",
            result
        );
    }

    #[test]
    fn test_trace_http_chain() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("server.ts"),
            r#"
function validateToken(token: string) { return true; }
function queryDatabase(userId: string) { return null; }

function handleLogin(req: Request) {
    validateToken(req.token);
    return queryDatabase(req.userId);
}

app.post('/api/login', handleLogin);
"#,
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json(
            "trace_http_chain",
            json!({
                "route_path": "/api/login",
                "depth": 3
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        assert_eq!(result["route"], "/api/login");
        let handlers = result["handlers"].as_array().unwrap();
        assert!(!handlers.is_empty(), "should find at least one handler");

        // First handler should have a call_chain with recursive callees
        let handler = &handlers[0];
        assert!(handler["handler_name"].as_str().is_some());
        assert!(
            handler["call_chain"].is_array(),
            "handler should have call_chain array"
        );
    }

    #[test]
    fn test_read_snippet_handles_missing_node() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("a.ts"),
            "function exists() { return 1; }\n",
        )
        .unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        // Request a non-existent node_id — should return error gracefully, not panic
        let req = tool_call_json("read_snippet", json!({"node_id": 999999}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("Error") || text.contains("not found"),
            "missing node should return error message, got: {}",
            text
        );
    }

    /// Drive the REAL `read_snippet` tool at a node whose indexed path escapes
    /// the project root, and prove the escapee's bytes never reach the response.
    ///
    /// The previous version of this test re-implemented `root.join(p)
    /// .canonicalize().starts_with(root)` inline and never called the tool, so it
    /// asserted a property of `std::path` — deleting the guard in
    /// `read_source_context` left it green (audit 2026-08-16 P1-19). False
    /// coverage on a security guard is worse than none: it stops anyone from
    /// writing the real thing.
    ///
    /// Mutation-verified: commenting out the `starts_with(&root_canonical)`
    /// early-return in `McpServer::read_source_context` turns this test RED
    /// (the secret file's contents come back in `code_content`).
    ///
    /// The traversal path is injected straight into the index because that is
    /// the only way to reach the guard: `read_source_context` is fed the path
    /// STORED for the node, never a caller argument, so a hostile/stale index
    /// row (or a symlinked tree) is the realistic vector — and a caller-supplied
    /// `../..` path fails the `get_nodes_by_file_path` lookup long before it.
    #[test]
    fn test_read_snippet_blocks_path_traversal() {
        let base = TempDir::new().unwrap();
        let project_dir = base.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let outside_dir = base.path().join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(
            outside_dir.join("secret.ts"),
            "const SECRET_TOKEN = 'do-not-leak-this';\n",
        )
        .unwrap();
        // Context line above the symbol: it exists ONLY on disk (not in the
        // stored code_content), so the positive control below can prove
        // read_source_context really ran rather than passing vacuously.
        std::fs::write(
            project_dir.join("safe.ts"),
            "// CONTEXT_MARKER_ABOVE\nexport function ok(): number { return 1; }\n",
        )
        .unwrap();

        let server = McpServer::new_test_with_project(&project_dir);
        server.ensure_indexed().unwrap();

        // Positive control FIRST: a legitimate in-root node reads from disk with
        // its context lines. If this ever stops holding, the negative assertion
        // below proves nothing.
        let safe_id = queries::get_nodes_by_name(server.db().conn(), "ok").unwrap()[0].id;
        let req = tool_call_json(
            "read_snippet",
            json!({"node_id": safe_id, "context_lines": 3}),
        );
        let safe_result = parse_tool_result(&server.handle_message(&req).unwrap());
        assert!(
            safe_result["code_content"]
                .as_str()
                .unwrap_or_default()
                .contains("CONTEXT_MARKER_ABOVE"),
            "in-root read must serve on-disk context (else the guard test is vacuous): {safe_result}"
        );

        // Now an indexed node whose path escapes the root.
        let file_id = upsert_file(
            server.db().conn(),
            &FileRecord {
                path: "../outside/secret.ts".into(),
                blake3_hash: "deadbeef".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();
        let escaped_id = queries::insert_node(
            server.db().conn(),
            &queries::NodeRecord {
                file_id,
                node_type: "const".into(),
                name: "SECRET_TOKEN".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 1,
                code_content: "<<stored placeholder>>".into(),
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

        let req = tool_call_json(
            "read_snippet",
            json!({"node_id": escaped_id, "context_lines": 3, "skip_indexing": true}),
        );
        let result = parse_tool_result(&server.handle_message(&req).unwrap());
        let body = serde_json::to_string(&result).unwrap();
        assert!(
            !body.contains("do-not-leak-this"),
            "a node indexed OUTSIDE the project root must never have its file read: {body}"
        );
        // The guard makes read_source_context return None, and the handler falls
        // back to the stored content — the call still answers, it just answers
        // from the index.
        assert_eq!(
            result["code_content"], "<<stored placeholder>>",
            "blocked read must fall back to stored code_content: {result}"
        );
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

        let req = tool_call_json(
            "get_call_graph",
            json!({
                "function_name": "chain0",
                "direction": "callees",
                "depth": 20
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Measured 2026-08-16 (audit §四): this fixture does NOT reach the
        // compressed arm — `mode` comes back absent, so the `else` branch is what
        // runs and what this test actually covers. The compressed branch is kept
        // (it is correct if the payload ever grows) but is dormant, and saying so
        // beats leaving a reader to assume coverage from the branch's existence.
        // Live compression coverage lives in
        // `test_semantic_search_sandbox_compression`, whose fixture is asserted
        // to cross the threshold.
        if result["mode"].as_str().is_some() {
            assert!(result["mode"].as_str().unwrap().starts_with("compressed_"));
            assert!(result["results"].is_array());
        } else {
            assert!(result["function"].as_str().is_some());
        }
    }

    /// Regression: when a dense call graph triggers BOTH the rollup branch
    /// (est_tokens > COMPRESSION_TOKEN_THRESHOLD) AND saturates the row limit
    /// (CALL_GRAPH_ROW_LIMIT), `attach_truncation_flags` must still fire on
    /// the rollup payload — without this the agent sees `mode="rollup_call_graph"`
    /// without any signal that more callers may exist beyond the 200-row cap.
    #[test]
    fn test_call_graph_rollup_with_truncation() {
        let project_dir = TempDir::new().unwrap();
        // 250 distinct callers of `hub` in one file → CTE produces 251 rows
        // (hub at depth 0 + 250 depth-1 callers), saturating the LIMIT 200 cap.
        // 200-row JSON serialization clears the 2000-token rollup threshold.
        let mut code = String::from("function hub() {}\n");
        for i in 0..250 {
            code.push_str(&format!("function caller_{}() {{ hub(); }}\n", i));
        }
        std::fs::write(project_dir.path().join("dense.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json(
            "get_call_graph",
            json!({
                "function_name": "hub",
                "direction": "callers",
                "depth": 1
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Rollup branch fired (dense fanout collapsed to file-level summary).
        assert_eq!(
            result["mode"], "rollup_call_graph",
            "250 callers in one file must trip rollup, got mode={:?}",
            result["mode"]
        );

        // Truncation provenance survives the rollup: row limit hit, warning present.
        assert_eq!(
            result["limit_hit"],
            json!(true),
            "200-row cap hit on 250-caller fixture must surface limit_hit=true"
        );
        assert!(
            result["truncation_warning"]
                .as_str()
                .map(|s| s.contains("row limit"))
                .unwrap_or(false),
            "truncation_warning must mention row limit; got {:?}",
            result["truncation_warning"],
        );
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

        let req = tool_call_json(
            "get_ast_node",
            json!({
                "file_path": "big.ts",
                "symbol_name": "bigFunc"
            }),
        );
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Measured 2026-08-16 (audit §四): the compressed arm is dormant here, and
        // structurally so — `truncate_code_content` caps stored `code_content` at
        // `max_code_content_len()` (4 KiB), so ONE node's payload cannot reach the
        // 2000-token threshold no matter how large the source function is. The
        // `else` branch is what this test covers. Left in place rather than
        // deleted: the branch is the correct handling if a future response shape
        // carries more per node. Live compression coverage is
        // `test_semantic_search_sandbox_compression`.
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
        assert!(
            result["embedding_status"].is_string(),
            "should have embedding_status: {:?}",
            result
        );
        assert!(
            result["embedding_progress"].is_string(),
            "should have embedding_progress: {:?}",
            result
        );
        assert!(
            result["model_available"].is_boolean(),
            "should have model_available: {:?}",
            result
        );
    }

    #[test]
    fn test_get_index_status_discloses_files_with_parse_errors() {
        // A file that fails to parse OUTRIGHT is skipped and already disclosed
        // under `skipped_files.parse_error`. This counter is the other one: the
        // file parsed WITH syntax errors, tree-sitter error recovery salvaged
        // what it could, and the partial symbols went into the index. Those
        // files look indistinguishable from clean ones at query time, so the
        // count is the only way a caller learns its results may be incomplete.
        // The CLI has disclosed it since the counter existed (index_ops.rs, both
        // the stderr summary and `--json`); the MCP surface never did.
        let server = McpServer::new_test();

        // Zero must stay SILENT, matching the `skipped_files` idiom right above
        // it: `last_index_stats` reflects the last index run IN THIS PROCESS, so
        // a server that started against an already-fresh index did no work and
        // holds zeros. Emitting `0` there would assert "no parse errors" on
        // evidence that says nothing at all.
        let resp = server
            .handle_message(&tool_call_json("get_index_status", json!({})))
            .unwrap();
        let clean = parse_tool_result(&resp);
        assert!(
            clean.get("files_with_parse_errors").is_none(),
            "a run with no parse errors must not claim a count: {clean:?}"
        );

        server
            .last_index_stats
            .lock()
            .unwrap()
            .files_with_parse_errors = 3;
        let resp = server
            .handle_message(&tool_call_json("get_index_status", json!({})))
            .unwrap();
        let degraded = parse_tool_result(&resp);
        assert_eq!(
            degraded["files_with_parse_errors"], 3,
            "salvaged-but-incomplete files must be disclosed: {degraded:?}"
        );
    }

    #[test]
    fn test_handle_tool_centralized_compression() {
        // Verify that estimate_json_tokens works as expected for compression threshold checks
        let small = json!({"name": "hello", "type": "function"});
        let small_tokens = crate::sandbox::compressor::estimate_json_tokens(&small);
        assert!(
            small_tokens < COMPRESSION_TOKEN_THRESHOLD,
            "small JSON should be under threshold: {} tokens",
            small_tokens
        );

        // Build a large JSON value that exceeds the compression threshold
        // COMPRESSION_TOKEN_THRESHOLD = 2000, and estimate is len/3
        // So we need > 6000 chars of JSON
        let large_content: String = "x".repeat(8000);
        let large = json!({"code_content": large_content, "name": "big_function"});
        let large_tokens = crate::sandbox::compressor::estimate_json_tokens(&large);
        assert!(
            large_tokens > COMPRESSION_TOKEN_THRESHOLD,
            "large JSON should exceed threshold: {} tokens vs {} threshold",
            large_tokens,
            COMPRESSION_TOKEN_THRESHOLD
        );

        // Verify the centralized compression produces a truncated result
        let compressed = centralized_compress(large.clone());
        assert_ne!(
            compressed, large,
            "compressed result should differ from original"
        );
        assert!(
            compressed.get("_truncated").is_some(),
            "centralized compression should add _truncated marker"
        );
        let compressed_tokens = crate::sandbox::compressor::estimate_json_tokens(&compressed);
        assert!(
            compressed_tokens <= COMPRESSION_TOKEN_THRESHOLD * 2,
            "compressed result should be much smaller: {} tokens",
            compressed_tokens
        );
    }

    /// W2 end-to-end: a watcher-triggered incremental skipped because embedding holds the
    /// write path must ARM `pending_incremental` (the caller already consumed the watcher
    /// event), and a later run must honor + clear it — indexing the otherwise-stranded change.
    #[test]
    fn test_incremental_skip_during_embedding_arms_then_clears_pending() {
        use std::fs;
        let project = TempDir::new().unwrap();
        fs::write(project.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        let server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap(); // initial full index → indexed=true
        assert!(!server.indexing.pending_incremental.load(Ordering::SeqCst));

        // A real on-disk change for the incremental to pick up.
        fs::write(project.path().join("beta.rs"), "fn beta_fn() {}\n").unwrap();

        // Embedding holds the write path → the incremental SKIPS (after its 2s wait) and
        // must arm pending so the consumed change isn't lost.
        server
            .indexing
            .embedding_in_progress
            .store(true, Ordering::SeqCst);
        server
            .run_incremental_with_cache_restore(project.path(), None)
            .unwrap();
        assert!(
            server.indexing.pending_incremental.load(Ordering::SeqCst),
            "a skipped incremental must arm pending_incremental"
        );
        assert!(
            crate::storage::queries::get_node_ids_by_name(server.db.conn(), "beta_fn")
                .unwrap()
                .is_empty(),
            "the skipped incremental must NOT have indexed beta yet"
        );

        // Release embedding; the owed incremental runs, clears the flag, and indexes beta.
        server
            .indexing
            .embedding_in_progress
            .store(false, Ordering::SeqCst);
        server
            .run_incremental_with_cache_restore(project.path(), None)
            .unwrap();
        assert!(
            !server.indexing.pending_incremental.load(Ordering::SeqCst),
            "a completed incremental must clear pending_incremental"
        );
        assert!(
            !crate::storage::queries::get_node_ids_by_name(server.db.conn(), "beta_fn")
                .unwrap()
                .is_empty(),
            "the previously-stranded beta.rs must be indexed once the owed incremental runs"
        );
    }

    /// W2: with a watcher ACTIVE but NO fresh events, `ensure_indexed` must still run an
    /// incremental that's owed via `pending_incremental` — `owed` is the SOLE trigger here.
    /// (A watcher being present makes the no-watcher debounce path — which would otherwise
    /// mask `owed` — unreachable, so reverting the `|| owed` wiring makes this test fail.)
    #[test]
    fn test_ensure_indexed_honors_owed_pending_without_new_events() {
        use std::fs;
        let project = TempDir::new().unwrap();
        fs::write(project.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        let server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap();

        // Make `has_watcher == true` (so ensure_indexed's no-watcher debounce branch is
        // skipped) while guaranteeing `drain_watcher_events()` returns false — so `owed` is
        // provably the only thing that can run the incremental.
        //
        // Keep a REAL watcher alive on an idle dir (for has_watcher) but route ITS events to a
        // sink we never read, and drain a SEPARATE channel nothing ever sends to. Why the
        // decoupling: macOS FSEvents can emit a coalesced bare Any/Other event when a watch
        // starts, and since 823a561 `is_content_event` treats Any/Other as content — so such a
        // startup event now reaches the channel and made this precondition flaky on macos-no-embed
        // CI. Draining an isolated channel removes that platform-specific fs-event race.
        let idle = TempDir::new().unwrap();
        let (sink_tx, _sink_rx) =
            mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
        let fw = FileWatcher::start(idle.path(), sink_tx).expect("watcher must start");
        let (_quiet_tx, quiet_rx) =
            mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
        *lock_or_recover(&server.watcher, "watcher") = Some(WatcherState {
            _watcher: fw,
            receiver: quiet_rx,
        });

        // Stranded state: a real change on disk + pending armed (as a prior embedding-skip
        // would leave it), with a watcher active and no events for it.
        fs::write(project.path().join("gamma.rs"), "fn gamma_fn() {}\n").unwrap();
        server
            .indexing
            .pending_incremental
            .store(true, Ordering::SeqCst);
        assert!(
            !server.drain_watcher_events(),
            "precondition: no watcher events queued"
        );

        server.ensure_indexed().unwrap();
        assert!(
            !server.indexing.pending_incremental.load(Ordering::SeqCst),
            "ensure_indexed must run and clear the owed incremental"
        );
        assert!(
            !crate::storage::queries::get_node_ids_by_name(server.db.conn(), "gamma_fn")
                .unwrap()
                .is_empty(),
            "owed incremental must index the stranded change (watcher active, no new events)"
        );
    }

    #[test]
    fn test_ensure_indexed_non_blocking_when_indexing_in_progress() {
        // Setup: create a server with startup_indexing=true and condvar never signaled
        let project_dir = TempDir::new().unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // Simulate background indexing in progress
        server
            .indexing
            .startup_indexing
            .store(true, Ordering::SeqCst);
        *server.indexing.startup_indexing_done.0.lock().unwrap() = false;

        // Call ensure_indexed and verify it returns within 5 seconds
        let start = std::time::Instant::now();
        let result = server.ensure_indexed();
        let elapsed = start.elapsed();

        // Must complete quickly (under 5 seconds), not block for 300 seconds
        assert!(
            elapsed.as_secs() < 5,
            "ensure_indexed should return within 5 seconds, took {}s",
            elapsed.as_secs()
        );

        // Should return an error indicating indexing is in progress
        assert!(
            result.is_err(),
            "ensure_indexed should return Err when indexing is in progress"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("ndexing in progress") || err_msg.contains("retry"),
            "error message should mention indexing in progress or retry, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_ensure_indexed_non_blocking_survives_spurious_wakeups() {
        // Regression for the windows-with-embed flake: ensure_indexed's condvar wait
        // checked wait_result.timed_out() exactly once, so a SPURIOUS wakeup (the
        // condvar returning before the grace deadline without `done` being set —
        // common on a loaded runner) made it fall through and return Ok instead of
        // staying non-blocking. Inject spurious wakeups deterministically by
        // notifying the condvar without ever setting done=true: the wait MUST loop
        // until the real deadline and still return Err.
        let project_dir = TempDir::new().unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());
        server
            .indexing
            .startup_indexing
            .store(true, Ordering::SeqCst);
        *server.indexing.startup_indexing_done.0.lock().unwrap() = false;

        let dvar = std::sync::Arc::clone(&server.indexing.startup_indexing_done);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_c = std::sync::Arc::clone(&stop);
        let notifier = std::thread::spawn(move || {
            while !stop_c.load(Ordering::Relaxed) {
                dvar.1.notify_all(); // wake the waiter WITHOUT setting done — spurious
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });

        let start = std::time::Instant::now();
        let result = server.ensure_indexed();
        let elapsed = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        notifier.join().unwrap();

        assert!(
            elapsed.as_secs() < 5,
            "must stay non-blocking even under spurious wakeups, took {}s",
            elapsed.as_secs()
        );
        assert!(
            result.is_err(),
            "ensure_indexed must return Err under spurious wakeups while indexing is unfinished"
        );
    }

    #[test]
    fn test_fk_fallback_truncate_purges_stale_state_and_rebuild_recovers() {
        // Regression for v0.11.x-v0.14.4 "FOREIGN KEY constraint failed" bubbling
        // to agents via project_map / module_overview / semantic_code_search.
        // The fix (mod.rs:987) truncates `files` before re-running run_full_index,
        // because run_full_index on its own does per-file upsert — orphan rows
        // from the failed incremental survive and re-trigger FK on retry.
        //
        // This test exercises the recovery mechanism (truncate → full index from
        // clean) with injected stale data representing the kind of dirty state
        // the FK branch exists to recover from: phantom files with no on-disk
        // counterpart, plus their nodes and edges. We cannot reproduce the
        // original in-flight FK race via black-box injection (internal JOINs in
        // get_inbound_cross_file_edges filter out orphan rows), so this covers
        // the recovery path itself — if anyone removes the `DELETE FROM files`
        // from mod.rs:987's FK branch, this test fails when the stale rows
        // survive rebuild.
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function alpha() {}").unwrap();
        std::fs::write(
            project_dir.path().join("b.ts"),
            "function beta() { alpha(); }",
        )
        .unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        // Inject a phantom file row plus its nodes and an edge. FK=OFF lets us
        // plant data unreachable via normal indexing — simulates residue from a
        // previous crashed session or external DB modification.
        server.db().conn().execute_batch(
            "PRAGMA foreign_keys = OFF;\n\
             INSERT INTO files (id, path, blake3_hash, last_modified, language, indexed_at) \
                 VALUES (9999, 'phantom.ts', 'stale', 0, 'typescript', 0);\n\
             INSERT INTO nodes (id, file_id, type, name, qualified_name, start_line, end_line, code_content) \
                 VALUES (88888, 9999, 'function', 'phantom_fn', 'phantom_fn', 1, 5, 'function phantom_fn() {}');\n\
             INSERT INTO edges (source_id, target_id, relation) VALUES (88888, 88888, 'calls');\n\
             PRAGMA foreign_keys = ON;"
        ).unwrap();
        let count =
            |sql: &str| -> i64 { server.db().conn().query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT COUNT(*) FROM nodes WHERE name = 'phantom_fn'"),
            1,
            "phantom injected"
        );

        // Step 1 of the fallback: truncate (same SQL as mod.rs:987's FK branch).
        {
            let tx = server.db().conn().unchecked_transaction().unwrap();
            tx.execute("DELETE FROM files", []).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(count("SELECT COUNT(*) FROM files"), 0, "files truncated");
        assert_eq!(
            count("SELECT COUNT(*) FROM nodes"),
            0,
            "nodes CASCADE-deleted (schema: nodes.file_id ON DELETE CASCADE)"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM edges"),
            0,
            "edges CASCADE-deleted (schema: edges.source_id/target_id ON DELETE CASCADE)"
        );

        // Step 2 of the fallback: run_full_index rebuilds from clean state.
        let result =
            crate::indexer::pipeline::run_full_index(server.db(), project_dir.path(), None, None)
                .unwrap();
        assert!(
            result.files_indexed >= 2,
            "both on-disk files re-indexed (got {})",
            result.files_indexed
        );

        // Post-recovery invariants: on-disk symbols restored, phantom gone.
        let alpha = queries::get_nodes_by_name(server.db().conn(), "alpha").unwrap();
        assert_eq!(alpha.len(), 1, "alpha re-indexed after truncate+rebuild");
        let beta = queries::get_nodes_by_name(server.db().conn(), "beta").unwrap();
        assert_eq!(beta.len(), 1, "beta re-indexed after truncate+rebuild");
        assert_eq!(
            count("SELECT COUNT(*) FROM nodes WHERE name = 'phantom_fn'"),
            0,
            "phantom purged by fallback"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM files WHERE path = 'phantom.ts'"),
            0,
            "phantom file row purged by fallback"
        );
    }

    /// Inode-safety contract: when a snapshot is installed into .code-graph/index.db
    /// BEFORE from_project_root opens self.db, the connection sees the snapshot data.
    ///
    /// Regression test for the POSIX rename(2) inode-swap problem:
    /// If snapshot were installed AFTER self.db was opened (old approach), self.db.conn()
    /// would point at the old empty inode while the snapshot data landed on a new inode.
    /// With Approach A (maybe_install_snapshot in from_project_root before open_db_for_role),
    /// the connection is opened on the post-install file, so snapshot rows are visible.
    ///
    /// Note: resolve_snapshot_source rejects file:// URLs from .code-graph.toml (https-only
    /// policy). We use crate::snapshot::try_install directly here to simulate what
    /// maybe_install_snapshot does when a valid HTTPS URL is configured, verifying the
    /// contract that snapshot data installed before open is visible via self.db.
    #[test]
    fn test_snapshot_data_visible_via_self_db_after_from_project_root() {
        use std::process::Command;

        // Build a snapshot from a git fixture with one indexable Rust file.
        let source = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(source.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(source.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(source.path())
            .status()
            .unwrap();
        std::fs::write(
            source.path().join("lib.rs"),
            "pub fn snapshot_sentinel() {}\npub fn snapshot_caller() { snapshot_sentinel(); }\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(source.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(source.path())
            .status()
            .unwrap();

        let raw_db = source.path().join("snap.db");
        crate::snapshot::create(source.path(), &raw_db, false).unwrap();
        let raw = std::fs::read(&raw_db).unwrap();
        let compressed = zstd::encode_all(&raw[..], 9).unwrap();
        let zst_path = source.path().join("snap.db.zst");
        std::fs::write(&zst_path, &compressed).unwrap();

        // Consumer project: same files, but fresh .code-graph/ directory (no prior index).
        // Install the snapshot into the consumer BEFORE calling from_project_root — this
        // is what maybe_install_snapshot does in production when index.db does not exist.
        let consumer = TempDir::new().unwrap();
        std::fs::write(
            consumer.path().join("lib.rs"),
            "pub fn snapshot_sentinel() {}\npub fn snapshot_caller() { snapshot_sentinel(); }\n",
        )
        .unwrap();
        let url = format!("file://{}", zst_path.display());
        crate::snapshot::try_install(&url, consumer.path()).unwrap();

        // Verify snapshot was installed.
        let index_db = consumer.path().join(".code-graph").join("index.db");
        assert!(
            index_db.exists(),
            "snapshot must be installed before from_project_root"
        );

        // Open the server — from_project_root skips maybe_install_snapshot because
        // index.db already exists (the guard condition is !db_path.exists()).
        // self.db is opened on the snapshot file directly.
        let server = McpServer::from_project_root(consumer.path()).unwrap();

        // KEY ASSERTION: self.db sees the snapshot nodes without calling ensure_indexed.
        // If the inode-swap bug were present (install after open), this would return empty.
        let nodes = queries::get_nodes_by_name(server.db().conn(), "snapshot_sentinel").unwrap();
        assert!(
            !nodes.is_empty(),
            "self.db must see snapshot_sentinel from the pre-installed snapshot; \
             got {} nodes — inode-swap regression if 0",
            nodes.len()
        );

        // ensure_indexed should run incrementally (not full) since has_existing = true.
        // This verifies the ensure_indexed drift-correction path works correctly.
        server.ensure_indexed().unwrap();
        let nodes_after =
            queries::get_nodes_by_name(server.db().conn(), "snapshot_sentinel").unwrap();
        assert!(
            !nodes_after.is_empty(),
            "snapshot_sentinel must still be present after incremental drift-check"
        );
        let caller_nodes =
            queries::get_nodes_by_name(server.db().conn(), "snapshot_caller").unwrap();
        assert!(
            !caller_nodes.is_empty(),
            "snapshot_caller must be present after drift-check"
        );
    }

    // ---------------------------------------------------------------------
    // P1-4: a secondary must re-evaluate its role instead of answering from a
    // frozen index for the rest of the session.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn test_secondary_promotes_to_primary_after_lock_frees() {
        use std::os::unix::io::AsRawFd;
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("alpha.rs"), "fn alpha_fn() {}\n").unwrap();

        // Bootstrap an on-disk index so the read-only secondary open succeeds.
        {
            let boot = McpServer::new_test_with_project(project.path());
            boot.ensure_indexed().unwrap();
        }

        // Stand in for a live primary: hold the flock from a separate fd. flock is
        // tied to the open file description, so this excludes us in-process too.
        let cg_dir = project.path().join(CODE_GRAPH_DIR);
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cg_dir.join("index.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        let mut server = McpServer::from_project_root(project.path()).unwrap();
        assert!(
            !server.is_primary(),
            "precondition: the held lock must force secondary mode"
        );

        // A change made while the primary is alive stays unindexed by us.
        std::fs::write(project.path().join("beta.rs"), "fn beta_fn() {}\n").unwrap();
        server.timing.promotion_retry = std::time::Duration::ZERO;
        server.ensure_indexed().unwrap();
        assert!(
            !server.is_primary(),
            "must not steal the lock while another process holds it"
        );

        // Primary exits. With the retry throttle wide open, the next tool call
        // must still NOT re-probe — this is the "one flock attempt per tool call"
        // cost the throttle exists to prevent.
        drop(holder);
        server.timing.promotion_retry = std::time::Duration::from_secs(3600);
        *lock_or_recover(&server.last_promotion_attempt, "last_promotion_attempt") =
            std::time::Instant::now();
        server.ensure_indexed().unwrap();
        assert!(
            !server.is_primary(),
            "throttled window must suppress the re-acquire attempt"
        );
        assert!(
            crate::storage::queries::get_node_ids_by_name(server.db().conn(), "beta_fn")
                .unwrap()
                .is_empty(),
            "still secondary: nothing may have been indexed yet"
        );

        // Throttle window elapsed → promote, and the promotion must leave the
        // index actually caught up (not merely flip a flag).
        //
        // Bounded poll rather than a single attempt, for the same reason the
        // `indexed` loop below has one: the bootstrap server above spawns
        // background work that owns its own handles, and `drop(holder)` frees
        // OUR stand-in lock without proving that leftover work has finished with
        // the file. When it has not, `try_promote_to_primary` correctly fails to
        // flock and stays secondary — the contract is "promotes once the lock is
        // free", which is eventual, not instantaneous. Asserting on the first
        // attempt made this test fail roughly 1 run in 4 locally (it shipped that
        // way in v0.113.0 and would have reddened the release gate at random).
        // Bounded, so a promotion that never happens still fails the test.
        let mut promoted = false;
        for _ in 0..40 {
            // Re-open the throttle each round: `ensure_indexed` stamps
            // `last_promotion_attempt` on every probe, so without this only the
            // first iteration would actually retry. This is also what opens it
            // for the FIRST iteration — the preceding stamp this replaced was
            // redundant with it.
            *lock_or_recover(&server.last_promotion_attempt, "last_promotion_attempt") =
                std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(7200))
                    .unwrap();
            server.ensure_indexed().unwrap();
            if server.is_primary() {
                promoted = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            promoted,
            "with the lock free and the throttle elapsed, the secondary must promote"
        );
        // Promotion also starts the background startup-repair thread, which writes
        // through its own connection; if it wins the write path first, the owed
        // incremental is skipped (not failed) and retried. Poll rather than assume
        // the first attempt got through — bounded, so a genuine failure to index
        // still fails the test.
        let mut indexed = false;
        for _ in 0..40 {
            server.ensure_indexed().unwrap();
            indexed = !crate::storage::queries::get_node_ids_by_name(server.db().conn(), "beta_fn")
                .unwrap()
                .is_empty();
            if indexed {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            indexed,
            "promotion must run the owed incremental — and the read-only `db` handle \
             must see the promoted write handle's commits"
        );
    }

    // ---------------------------------------------------------------------
    // FRS-1 / FRS-3 / P3-2: freshness debounces are now state, not cfg(test)
    // constants, so each branch has a case that turns red when it is removed.
    // ---------------------------------------------------------------------

    /// Install a REAL watcher on an unrelated idle dir (so `is_watching()` is true)
    /// whose events go to a sink we never read, and hand the server a receiver
    /// nothing ever sends to. That is a watcher which is present but deaf — the
    /// inotify-limit / network-FS / bind-mount failure mode, made deterministic.
    fn attach_deaf_watcher(server: &McpServer, idle: &TempDir) {
        let (sink_tx, _sink_rx) =
            mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
        let fw = FileWatcher::start(idle.path(), sink_tx).expect("watcher must start");
        let (_quiet_tx, quiet_rx) =
            mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
        std::mem::forget(_quiet_tx); // keep the channel connected, never send
        *lock_or_recover(&server.watcher, "watcher") = Some(WatcherState {
            _watcher: fw,
            receiver: quiet_rx,
        });
    }

    #[test]
    fn test_deaf_watcher_is_bounded_by_the_backstop_debounce() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        let mut server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap();

        let idle = TempDir::new().unwrap();
        attach_deaf_watcher(&server, &idle);
        assert!(
            !server.drain_watcher_events(),
            "precondition: the deaf watcher delivers no events"
        );
        std::fs::write(project.path().join("late.rs"), "fn late_fn() {}\n").unwrap();

        // Negative control — without it this test would also pass if the code
        // rescanned unconditionally, which would defeat the debounce entirely.
        server.ensure_indexed().unwrap();
        assert!(
            crate::storage::queries::get_node_ids_by_name(server.db().conn(), "late_fn")
                .unwrap()
                .is_empty(),
            "inside the backstop window a watcher-active call must NOT rescan"
        );

        // Backstop window elapsed: the session must not stay stale indefinitely
        // just because a FileWatcher object exists.
        server.timing.watcher_backstop = std::time::Duration::ZERO;
        server.ensure_indexed().unwrap();
        assert!(
            !crate::storage::queries::get_node_ids_by_name(server.db().conn(), "late_fn")
                .unwrap()
                .is_empty(),
            "once the backstop elapses, a deaf watcher must not suppress the rescan"
        );
    }

    #[test]
    fn test_no_watcher_debounce_suppresses_then_allows_rescan() {
        // P3-2: with the interval hard-compiled to 0 in test builds, deleting the
        // debounce branch left the suite green. As state, both directions bite.
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        let mut server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap();
        assert!(
            !server.is_watching(),
            "precondition: no watcher in this test"
        );

        std::fs::write(project.path().join("mid.rs"), "fn mid_fn() {}\n").unwrap();
        server.timing.incremental_debounce = std::time::Duration::from_secs(3600);
        *lock_or_recover(&server.last_incremental_check, "last_incremental_check") =
            std::time::Instant::now();
        server.ensure_indexed().unwrap();
        assert!(
            crate::storage::queries::get_node_ids_by_name(server.db().conn(), "mid_fn")
                .unwrap()
                .is_empty(),
            "inside the debounce window the no-watcher path must skip the rescan"
        );

        server.timing.incremental_debounce = std::time::Duration::ZERO;
        server.ensure_indexed().unwrap();
        assert!(
            !crate::storage::queries::get_node_ids_by_name(server.db().conn(), "mid_fn")
                .unwrap()
                .is_empty(),
            "past the debounce window the no-watcher path must rescan"
        );
    }

    #[test]
    fn test_drain_watcher_events_ignores_our_own_data_dir_writes() {
        // FRS-3: the payload used to be discarded, so `.code-graph/` WAL and
        // usage.jsonl writes reported "changes" continuously — every tool call
        // paid a full-tree merkle stat and the debounce above was unreachable.
        let project = TempDir::new().unwrap();
        let server = McpServer::new_test_with_project(project.path());
        let idle = TempDir::new().unwrap();
        let (sink_tx, _sink_rx) =
            mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
        let fw = FileWatcher::start(idle.path(), sink_tx).expect("watcher must start");
        let (tx, rx) = mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
        *lock_or_recover(&server.watcher, "watcher") = Some(WatcherState {
            _watcher: fw,
            receiver: rx,
        });

        for p in [
            ".code-graph/index.db-wal",
            ".code-graph/usage.jsonl",
            ".git/index",
        ] {
            tx.send(WatchEvent::Changed(vec![p.to_string()])).unwrap();
        }
        assert!(
            !server.drain_watcher_events(),
            "self-inflicted .code-graph/.git writes must not count as project changes"
        );

        tx.send(WatchEvent::Changed(vec!["src/a.rs".to_string()]))
            .unwrap();
        assert!(
            server.drain_watcher_events(),
            "a real source change must still register"
        );

        // Mixed batch: a real path alongside ignored ones still counts.
        tx.send(WatchEvent::Changed(vec![
            ".code-graph/usage.jsonl".to_string(),
            "src/b.rs".to_string(),
        ]))
        .unwrap();
        assert!(
            server.drain_watcher_events(),
            "an ignored path must not mask a real one in the same event"
        );
    }

    // ---------------------------------------------------------------------
    // FRS-2: the 5 tools that take no `file_path` argument get result-set
    // freshness, so a query issued right after an Edit doesn't answer with
    // pre-edit line numbers (and says so when it cannot refresh).
    // ---------------------------------------------------------------------

    /// Shut every OTHER freshness path so a test can only pass through the
    /// result-set refresh: no watcher exists, and the no-watcher debounce is
    /// closed. This is the production shape of FRS-2 — a watcher IS active there,
    /// but its event for the edit has not been delivered yet.
    fn close_other_freshness_paths(server: &mut McpServer) {
        assert!(
            !server.is_watching(),
            "precondition: no watcher in this test"
        );
        server.timing.incremental_debounce = std::time::Duration::from_secs(3600);
        *lock_or_recover(&server.last_incremental_check, "last_incremental_check") =
            std::time::Instant::now();
    }

    fn ast_search_first_line(server: &McpServer, query: &str) -> (serde_json::Value, Option<i64>) {
        let req = tool_call_json("ast_search", json!({ "query": query }));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        let payload: serde_json::Value = serde_json::from_str(text).unwrap();
        let line = payload["results"][0]["start_line"].as_i64();
        (payload, line)
    }

    #[test]
    fn test_result_set_refresh_answers_with_post_edit_line_numbers() {
        let project = TempDir::new().unwrap();
        let file = project.path().join("a.rs");
        std::fs::write(&file, "fn frs_two_target() {}\n").unwrap();
        let mut server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap();

        let (_, before) = ast_search_first_line(&server, "frs_two_target");
        assert_eq!(before, Some(1), "precondition: indexed at line 1");

        close_other_freshness_paths(&mut server);
        // Edit pushes the symbol down three lines — exactly the "Edit then ask"
        // sequence that used to be answered from the pre-edit index.
        std::fs::write(&file, "\n\n\nfn frs_two_target() {}\n").unwrap();

        let (payload, after) = ast_search_first_line(&server, "frs_two_target");
        assert_eq!(
            after,
            Some(4),
            "result-set refresh must re-index the edited file and re-run the query: {payload}"
        );
        assert!(
            payload.get("freshness").is_none(),
            "a fully refreshed result needs no staleness disclosure: {payload}"
        );
    }

    /// Audit 2026-08-22 P2-11. `get_call_graph` and `find_references` DO accept
    /// a `file_path`, so they looked covered by `ensure_file_fresh_opt` — but
    /// that argument is an optional disambiguator and the ordinary call passes a
    /// bare symbol name, which hits the helper's `None` early return. What their
    /// answer is made of is OTHER files' symbols, so what an edit invalidates is
    /// the caller / reference SET, not the named file. Absent from
    /// `RESULT_REFRESH_TOOLS`, a call added since the last index was simply
    /// missing from the tree, with nothing saying so.
    #[test]
    fn test_result_set_refresh_covers_call_graph_and_references() {
        fn callers_of(server: &McpServer, tool: &str) -> String {
            let req = tool_call_json(
                tool,
                json!({ "symbol_name": "validateToken", "direction": "callers" }),
            );
            let resp = server.handle_message(&req).unwrap().unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
            parsed["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        }

        let project = TempDir::new().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("target.ts"),
            "export function validateToken(t: string): boolean { return t.length > 0; }\n",
        )
        .unwrap();
        let caller = src.join("caller.ts");
        std::fs::write(
            &caller,
            "import { validateToken } from './target';\n             export function handleLogin(t: string) { return validateToken(t); }\n",
        )
        .unwrap();
        let mut server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap();

        for tool in ["get_call_graph", "find_references"] {
            assert!(
                callers_of(&server, tool).contains("handleLogin"),
                "precondition: {tool} sees the indexed caller"
            );
        }

        close_other_freshness_paths(&mut server);
        // A caller added AFTER the index — the "Edit then ask" sequence.
        let content = std::fs::read_to_string(&caller).unwrap();
        std::fs::write(
            &caller,
            format!(
                "{content}export function handleRefresh(t: string) {{ return validateToken(t); }}\n"
            ),
        )
        .unwrap();

        for tool in ["get_call_graph", "find_references"] {
            let payload = callers_of(&server, tool);
            assert!(
                payload.contains("handleRefresh"),
                "{tool} must refresh the files its own result names, then re-run: {payload}"
            );
        }
    }

    #[test]
    fn test_result_set_refresh_keeps_and_discloses_stale_data_over_budget() {
        let project = TempDir::new().unwrap();
        let file = project.path().join("a.rs");
        std::fs::write(&file, "fn frs_budget_target() {}\n").unwrap();
        let mut server = McpServer::new_test_with_project(project.path());
        server.ensure_indexed().unwrap();

        close_other_freshness_paths(&mut server);
        server.result_refresh_budget = 0; // every stale file is over budget
        std::fs::write(&file, "\n\n\nfn frs_budget_target() {}\n").unwrap();

        let (payload, after) = ast_search_first_line(&server, "frs_budget_target");
        assert_eq!(
            after,
            Some(1),
            "over budget the STALE row must be kept, not dropped: {payload}"
        );
        assert_eq!(
            payload["freshness"]["stale_kept"].as_u64(),
            Some(1),
            "an unrefreshed stale file must be disclosed, not silently returned: {payload}"
        );
    }

    #[test]
    fn test_db_busy_is_classified_as_transient_not_failure() {
        // The distinction decides whether a tool call returns an error or keeps
        // the incremental owed and retries. SQLITE_BUSY_SNAPSHOT is not retried
        // by SQLite's busy handler, so it reaches us as a plain error.
        assert!(is_db_busy_error(&anyhow!("database is locked")));
        assert!(is_db_busy_error(&anyhow!(
            "index sqlite failure: database table is locked: files"
        )));
        assert!(!is_db_busy_error(&anyhow!("FOREIGN KEY constraint failed")));
        assert!(!is_db_busy_error(&anyhow!("no such column: blake3_hash")));
        // The two classifiers must stay disjoint — an FK error routed into the
        // "transient, retry later" arm would silently skip the truncate+rebuild
        // recovery it needs.
        let fk = anyhow!("FOREIGN KEY constraint failed");
        assert!(is_fk_constraint_error(&fk) && !is_db_busy_error(&fk));
    }

    #[test]
    fn test_collect_result_paths_covers_every_emitted_key() {
        let value = json!({
            "results": [{"file_path": "src/a.rs", "start_line": 1}],
            "hotspots": [{"file": "src/b.rs"}],
            "modules": [{"path": "src/mod_dir"}],
            "nested": {"chain": [{"handler": {"file_path": "src/c.rs"}}]},
            "external": [{"file_path": "<external>"}, {"file_path": ""}],
        });
        let mut paths = Vec::new();
        super::freshness::collect_result_paths(&value, &mut paths);
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec!["src/a.rs", "src/b.rs", "src/c.rs", "src/mod_dir"],
            "all three key spellings, nested arbitrarily deep; `<external>` and \
             empty placeholders excluded"
        );
    }
}
