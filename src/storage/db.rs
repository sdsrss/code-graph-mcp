use super::schema;
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Once;

// FFI declaration for sqlite-vec init function (compiled via build.rs)
extern "C" {
    fn sqlite3_vec_init(
        db: *mut rusqlite::ffi::sqlite3,
        pz_err_msg: *mut *mut std::os::raw::c_char,
        p_api: *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;
}

static VEC_INIT: Once = Once::new();

/// Size of the SQLite database header per the on-disk format spec.
/// A file shorter than this cannot be a database.
/// <https://www.sqlite.org/fileformat.html#magic_header_string>
const SQLITE_HEADER_SIZE: u64 = 100;

/// Marker in the reader-side corruption error. Matched by
/// [`Database::is_corrupt_index_error`] so `health-check` can tell a corrupt
/// index apart from every other open failure without a bespoke error type
/// threaded through `CliContext`.
const CORRUPT_INDEX_PREFIX: &str = "index database is corrupt";

fn register_sqlite_vec() {
    VEC_INIT.call_once(|| {
        // SAFETY: sqlite3_vec_init has the exact C ABI signature expected by
        // sqlite3_auto_extension in rusqlite's FFI bindings. No transmute needed.
        // The Once guard ensures single registration. SQLite is compiled with
        // SQLITE_THREADSAFE=1 (bundled default), making global extension registration safe.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(sqlite3_vec_init));
        }
    });
}

pub struct Database {
    conn: Connection,
    vec_enabled: bool,
    /// `Some(old_version)` when this handle opened a DB built by a *different*
    /// `INDEX_VERSION` in non-destructive (reader) mode — the data was left
    /// intact and a rebuild is owed. `None` when fresh, current, or already
    /// revalidated (wiped) by an indexer open. See [`Database::index_version_stale`].
    index_version_stale: Option<i32>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_impl(path, false, true)
    }

    pub fn open_with_vec(path: &Path) -> Result<Self> {
        Self::open_impl(path, true, true)
    }

    /// Open for READ / observability (health-check, grep, show, callgraph …).
    /// Forward schema migration still runs, but an `INDEX_VERSION` mismatch does
    /// NOT wipe data — it sets [`Database::index_version_stale`] so the caller can
    /// report "rebuild pending" instead of a passive reader destroying the index.
    ///
    /// Rationale: the destructive version sweep is only safe when a rebuild
    /// follows in the same context (an indexer). A status poll (statusline →
    /// `health-check`) or a one-off `grep` opened writably and wiped the index to
    /// 0 nodes; in a project where no MCP server is running, nothing rebuilt it,
    /// so the index stayed empty. Readers must never trigger the wipe.
    pub fn open_nondestructive(path: &Path) -> Result<Self> {
        Self::open_impl(path, false, false)
    }

    /// READER open that additionally brings up the sqlite-vec tables.
    ///
    /// `similar` (CLI) is a passive consumer that happens to need vector search.
    /// Before this existed it reached for [`Database::open_with_vec`], the
    /// *indexer* constructor — so a single `code-graph-mcp similar foo` against a
    /// version-lagging index wiped it to 0 nodes with no rebuild following, the
    /// exact daagu failure [`Database::open_nondestructive`] documents. Vector
    /// support and destructive revalidation are orthogonal; keep them so.
    pub fn open_nondestructive_with_vec(path: &Path) -> Result<Self> {
        Self::open_impl(path, true, false)
    }

    /// Open an existing database in strict read-only mode. Used by secondary
    /// MCP instances (those that failed to acquire the index flock) so the
    /// SQLite driver hard-refuses any write, eliminating race conditions
    /// against the primary instance's indexing transactions.
    ///
    /// Requires the file to already exist (returns Err if not). No schema
    /// migrations or table creation happens — secondary relies entirely on
    /// the primary's bootstrap.
    pub fn open_readonly(path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        // PRAGMA query_only is belt-and-suspenders on top of the flag —
        // any accidental write attempt errors out at the SQL layer.
        conn.execute_batch(
            "
            PRAGMA query_only = ON;
            PRAGMA busy_timeout = 5000;
            -- Keep mmap disabled here too (mirrors open_impl_inner). This is the
            -- secondary / flock-denied reader: it holds a mapping while the primary
            -- VACUUMs or checkpoints, the exact truncation-under-mmap SIGBUS hazard.
            -- SQLite's bundled compile default is already 0, so this enforces the
            -- invariant in code instead of relying on the build flag staying unset.
            PRAGMA mmap_size = 0;
        ",
        )?;
        // Future-schema refusal, same verdict the primary path gives. A secondary
        // never migrates (the primary owns bootstrap), so without this check a
        // mixed-version session — an updated binary indexing while the old one is
        // still attached as a reader — surfaced whatever bare `no such column`
        // the first query happened to hit. That is unactionable, and it is also
        // invisible to the plugin statusline, which keys on the marker below to
        // render "↻ updating" instead of "offline" (2026-08-16 audit §四).
        let existing_version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap_or(0);
        if existing_version > schema::SCHEMA_VERSION {
            anyhow::bail!(
                "Database schema version v{} is newer than supported v{}. Please update code-graph-mcp. [{}]",
                existing_version,
                schema::SCHEMA_VERSION,
                crate::domain::SCHEMA_TOO_NEW_MARKER
            );
        }

        // Detect vec tables via sqlite_master so consumers know if vector
        // search is available without needing a separate probe.
        let vec_enabled: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_vectors'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        // Secondary read-only instances never migrate or revalidate (the primary
        // owns bootstrap), so they don't compute a staleness verdict.
        Ok(Self {
            conn,
            vec_enabled,
            index_version_stale: None,
        })
    }

    /// `revalidate` doubles as "this caller is an INDEXER". Only an indexer may
    /// destroy the file, because only an indexer rebuilds it in the same breath.
    ///
    /// [`Database::open_nondestructive`] already carried that invariant in its
    /// doc comment ("Readers must never trigger the wipe") but only enforced it
    /// for the `INDEX_VERSION` sweep — the two wipes below ran for readers too.
    /// Reproduced: clobbering the header and running a plain `health-check`
    /// (a status poll, run by the statusline on every render) deleted a
    /// 151 552-byte index holding real symbols and left a 4 096-byte empty one.
    /// Nothing rebuilt it, and the integrity probes added for the same command
    /// then reported `quick_check: ok` — on the replacement. The user loses the
    /// index and is told everything is fine.
    ///
    /// For readers the corruption is now REPORTED instead: the error names the
    /// one-line remedy, `health-check` turns it into its usual corrupt-index
    /// verdict, and doctor's `index-corrupt` repair does the rebuild under a
    /// caller that actually rebuilds.
    fn open_impl(path: &Path, enable_vec: bool, revalidate: bool) -> Result<Self> {
        // Proactive sub-header size guard: any pre-existing main file smaller
        // than the 100-byte SQLite database header cannot be a valid database.
        // Without this, post-crash residue (0-byte main + stale .wal/.shm,
        // partial-write truncated mid-page) lands in SQLite-version-dependent
        // territory — sometimes Connection::open silently treats the file as
        // fresh, sometimes wal frames are replayed against the empty main,
        // sometimes is_corruption_error fires. The guard collapses every
        // sub-header state to one canonical recovery path: wipe the whole
        // triple (main + wal + shm) so the retry starts blank.
        if revalidate {
            Self::sub_header_size_guard(path);
        } else if Self::is_sub_header(path) {
            return Err(Self::corrupt_index_error(
                path,
                &format!(
                    "file is {} bytes, shorter than the 100-byte SQLite header",
                    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
                ),
            ));
        }

        match Self::open_impl_inner(path, enable_vec, revalidate) {
            Ok(db) => Ok(db),
            Err(e) if Self::is_corruption_error(&e) && path.exists() => {
                if !revalidate {
                    return Err(Self::corrupt_index_error(path, &e.to_string()));
                }
                tracing::warn!(
                    "[db] Database corrupt ({}), deleting for rebuild: {}",
                    path.display(),
                    e
                );
                // Remove DB + WAL + SHM files — the index is a pure cache
                std::fs::remove_file(path).ok();
                let wal_path = path.with_extension("db-wal");
                let shm_path = path.with_extension("db-shm");
                if wal_path.exists() {
                    std::fs::remove_file(&wal_path).ok();
                }
                if shm_path.exists() {
                    std::fs::remove_file(&shm_path).ok();
                }
                // Retry once with a fresh database
                Self::open_impl_inner(path, enable_vec, revalidate)
            }
            Err(e) => Err(e),
        }
    }

    /// The reader-side corruption verdict. Carries the remedy in the message
    /// because every read command surfaces this string verbatim, and "file is
    /// not a database" on its own tells the user nothing they can act on.
    fn corrupt_index_error(path: &Path, detail: &str) -> anyhow::Error {
        anyhow::anyhow!(
            "{}: {} ({}). The index is a rebuildable cache — run: \
             code-graph-mcp rebuild-index --confirm",
            CORRUPT_INDEX_PREFIX,
            detail,
            path.display()
        )
    }

    /// True when a file exists but is shorter than the SQLite header, i.e. it
    /// cannot be a database. Read-only counterpart of [`Self::sub_header_size_guard`].
    fn is_sub_header(path: &Path) -> bool {
        matches!(std::fs::metadata(path), Ok(m) if m.is_file() && m.len() < SQLITE_HEADER_SIZE)
    }

    /// Does this error come from the reader-side corruption path above?
    /// Used by `health-check` to render a corrupt index as its normal
    /// corrupt-index verdict rather than an opaque open failure.
    pub(crate) fn is_corrupt_index_error(e: &anyhow::Error) -> bool {
        e.to_string().contains(CORRUPT_INDEX_PREFIX)
    }

    /// Wipe an existing main DB file plus any sibling .wal/.shm if the main
    /// file is shorter than the SQLite header (100 bytes). No-op when the
    /// main file is absent (fresh install) or already a proper size.
    ///
    /// INDEXER-ONLY (see [`Self::open_impl`]): the caller must rebuild.
    fn sub_header_size_guard(path: &Path) {
        let size = match std::fs::metadata(path) {
            Ok(m) if m.is_file() => m.len(),
            _ => return,
        };
        if size >= SQLITE_HEADER_SIZE {
            return;
        }
        tracing::warn!(
            "[db] index.db is {} bytes (< {} SQLite header); wiping main+wal+shm for clean recovery",
            size, SQLITE_HEADER_SIZE
        );
        std::fs::remove_file(path).ok();
        let wal_path = path.with_extension("db-wal");
        let shm_path = path.with_extension("db-shm");
        if wal_path.exists() {
            std::fs::remove_file(&wal_path).ok();
        }
        if shm_path.exists() {
            std::fs::remove_file(&shm_path).ok();
        }
    }

    fn open_impl_inner(path: &Path, enable_vec: bool, revalidate: bool) -> Result<Self> {
        // Always register sqlite-vec extension (it's process-global anyway via auto_extension)
        register_sqlite_vec();

        let conn = Connection::open(path)?;

        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -64000;
            -- mmap DISABLED (was 256MB). SQLite memory-mapped reads raise SIGBUS if
            -- the mapped DB file is truncated underneath the mapping
            -- (https://www.sqlite.org/mmap.html §5). We DO truncate the main file —
            -- the post-version-sweep VACUUM below, plus sqlite's own checkpoints —
            -- and a long-lived reader (the MCP server) can hold the mapping while
            -- the watcher writes. Under the embed build's memory pressure the kernel
            -- reclaims the mapped pages, so the next read re-faults from the now
            -- shorter file and SIGBUSes: this is the snapshot_integration CI flake
            -- (with-embed only; an x86 SIGBUS can ONLY be an mmap-beyond-EOF fault,
            -- which pins it to this mapping). The OS page cache keeps plain pread
            -- fast at these index sizes, so disabling mmap costs ~nothing.
            PRAGMA mmap_size = 0;
            PRAGMA temp_store = MEMORY;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            -- Bound the WAL: a checkpoint truncates it back to <= this size, so an
            -- idle DB doesn't carry a multi-MB resident WAL (audit §8 saw ~4MB WAL
            -- on a 7.8MB main DB). run_optimize() additionally TRUNCATEs after bulk writes.
            PRAGMA journal_size_limit = 6291456;
        ",
        )?;

        // Check existing schema version — migrate if needed, bail only on future versions
        let existing_version: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if existing_version > schema::SCHEMA_VERSION {
            // The trailing [marker] is a stable token the plugin statusline keys on
            // to render "↻ updating" (post-update window) vs "offline". See
            // domain::SCHEMA_TOO_NEW_MARKER — keep it in the message.
            anyhow::bail!(
                "Database schema version v{} is newer than supported v{}. Please update code-graph-mcp. [{}]",
                existing_version,
                schema::SCHEMA_VERSION,
                crate::domain::SCHEMA_TOO_NEW_MARKER
            );
        }

        if existing_version > 0 && existing_version < schema::SCHEMA_VERSION {
            // Run migrations sequentially
            if existing_version < 2 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v1_to_v2(&conn)?;
                tx.commit()?;
            }
            if existing_version < 3 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v2_to_v3(&conn)?;
                tx.commit()?;
            }
            if existing_version < 4 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v3_to_v4(&conn)?;
                tx.commit()?;
            }
            if existing_version < 5 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v4_to_v5(&conn)?;
                tx.commit()?;
            }
            if existing_version < 6 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v5_to_v6(&conn)?;
                tx.commit()?;
            }
            if existing_version < 7 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v6_to_v7(&conn)?;
                tx.commit()?;
            }
            if existing_version < 8 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v7_to_v8(&conn)?;
                tx.commit()?;
            }
            if existing_version < 9 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v8_to_v9(&conn)?;
                tx.commit()?;
            }
            if existing_version < 10 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v9_to_v10(&conn)?;
                tx.commit()?;
            }
        }

        conn.execute_batch(&schema::create_tables_sql())?;

        if enable_vec {
            conn.execute_batch(&schema::create_vec_tables_sql())?;
            // Enforce that the on-disk vec0 table dimension matches the
            // compile-time EMBEDDING_DIM. On mismatch (e.g., user upgrades
            // the embedding model) we drop the vec table and rebuild at the
            // new dim rather than silently producing corrupt similarity scores.
            Self::ensure_embedding_dim_consistency(&conn)?;
        }

        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;

        // Check INDEX_VERSION (stored in application_id pragma).
        // When parser/indexer logic changes, INDEX_VERSION is bumped. An INDEXER
        // open (`revalidate = true`) clears the stale data so the rebuild it is
        // about to run starts clean. A READER open (`revalidate = false`:
        // health-check, grep, show, …) must NOT clear — a passive consumer that
        // destroys the index is the daagu failure (a status poll wiped it to 0
        // nodes and, with no MCP server running in that project, nothing rebuilt
        // it). Readers instead flag staleness and leave the data intact.
        // INDEX_VERSION lives in the application_id pragma. The comparison is
        // DIRECTIONAL, not symmetric (the bare `stored != INDEX_VERSION` it replaced
        // wiped in both directions, which let an older binary clobber a newer index):
        //
        //   stored < current  → UPGRADE: the stored index was built by an OLDER
        //     binary and is genuinely stale. An INDEXER open (revalidate=true) clears
        //     it so the rebuild it is about to run starts clean; a READER open
        //     (revalidate=false: health-check, grep, …) must NOT clear — a passive
        //     consumer that destroys the index is the daagu failure (a status poll
        //     wiped it to 0 nodes and, with no MCP server running, nothing rebuilt
        //     it). Readers flag staleness and leave the data intact.
        //
        //   stored > current  → DOWNGRADE: the stored index was built by a NEWER
        //     binary. An older binary must NEVER wipe it — that is the destructive
        //     half of the version ping-pong (a stale old server clobbering the index
        //     a current binary just built; the two then wipe each other on every
        //     open → 0 nodes, never stable). Leave the data AND application_id intact
        //     so the newer binary stays the owner; flag staleness and, on an
        //     indexer/server-startup open, warn on stderr. A genuine permanent
        //     downgrade still rebuilds by deleting .code-graph/index.db* first (fresh
        //     DB → application_id 0 → this binary stamps its own version cleanly).
        let stored_index_version: i32 =
            conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let current_index_version = crate::domain::INDEX_VERSION;
        let is_downgrade = stored_index_version > current_index_version;
        let mut index_version_stale = None;
        let mut wiped = false;
        if stored_index_version != 0 && stored_index_version != current_index_version {
            if is_downgrade {
                // Older binary, newer index: refuse to clobber it (reader OR indexer).
                index_version_stale = Some(stored_index_version);
                tracing::warn!(
                    "[index] index built by a newer code-graph (v{} > binary v{}); leaving it intact, not rebuilding",
                    stored_index_version, current_index_version
                );
                if revalidate {
                    // Only the indexer/server-startup path warns on stderr — readers
                    // (the statusline health-check polls every few seconds) would
                    // otherwise spam it. CLI/MCP install no tracing subscriber
                    // (feedback_tracing_invisible_in_cli.md), so double-write here.
                    eprintln!(
                        "[code-graph] Index was built by a NEWER code-graph (v{} > this binary v{}): \
not rebuilding — an older binary must not clobber a newer index. Restart all code-graph servers on one \
version, or update this binary. To force a rebuild at this older version, delete .code-graph/index.db* first.",
                        stored_index_version, current_index_version
                    );
                }
            } else if revalidate {
                tracing::info!(
                    "[index] Index version changed ({} → {}), clearing stale data for rebuild",
                    stored_index_version,
                    current_index_version
                );
                // Double-write to stderr: the CLI/MCP startup paths install no tracing
                // subscriber (feedback_tracing_invisible_in_cli.md), so the tracing line
                // above is invisible to users. Without this, a version-mismatch wipe
                // surfaces only as a confusing "index is empty" — most often when two
                // code-graph binaries of different INDEX_VERSION (e.g. a stale server +
                // a freshly-built one) share one .code-graph/index.db and clear each
                // other's data on every open. Name the cause so the fix (restart all
                // servers / rebuild to one version) is obvious.
                eprintln!(
                    "[code-graph] Index version mismatch (stored v{} ≠ binary v{}): clearing + rebuilding. \
If you see this repeatedly, another code-graph server of a different version is sharing this index — restart all servers so they run one version.",
                    stored_index_version, current_index_version
                );
                conn.execute_batch(
                    "BEGIN; DELETE FROM edges; DELETE FROM nodes; DELETE FROM files; COMMIT;",
                )?;
                // Reclaim the pages freed by the sweep so a version bump that shrinks
                // the index (fewer nodes under the new INDEX_VERSION, or a shrunk
                // codebase) doesn't carry the old high-water-mark of free pages into
                // the rebuild. Benefit is bounded — the immediate rebuild reuses free
                // pages when the new index is >= the old size — so this is hygiene on
                // the rare version-mismatch open, not a hot path. Best-effort: another
                // code-graph binary sharing this DB can make VACUUM fail with
                // "database is locked", which must never block opening the DB.
                if let Err(e) = conn.execute_batch("VACUUM;") {
                    tracing::warn!("[index] post-sweep VACUUM skipped: {}", e);
                }
                wiped = true;
            } else {
                // Reader/observability open on an OLDER (upgrade-pending) index: leave
                // the data alone, just surface the mismatch so callers can report
                // "rebuild pending". Crucially do NOT bump application_id below —
                // stamping current would mask the staleness from the next indexer
                // open that should rebuild.
                index_version_stale = Some(stored_index_version);
            }
        }
        // Stamp current version only on a fresh DB (application_id == 0) or after an
        // upgrade-wipe. NEVER on a downgrade (the index belongs to the newer binary)
        // and never on a passive reader (would mask the owed rebuild).
        if stored_index_version == 0 || wiped {
            conn.pragma_update(None, "application_id", current_index_version)?;
        }

        Ok(Self {
            conn,
            vec_enabled: enable_vec,
            index_version_stale,
        })
    }

    /// `Some(old_version)` when this handle opened (non-destructively) a DB built
    /// by a different `INDEX_VERSION` — the structural data is intact but a full
    /// rebuild is owed. `None` for a fresh, current, or indexer-revalidated DB.
    /// Lets readers (e.g. `health-check`) report a stale index instead of having
    /// silently wiped it. See [`Database::open_nondestructive`].
    pub fn index_version_stale(&self) -> Option<i32> {
        self.index_version_stale
    }

    /// Check if an error indicates SQLite database corruption.
    /// Used to decide whether to auto-delete and rebuild the index cache.
    fn is_corruption_error(e: &anyhow::Error) -> bool {
        let msg = e.to_string();
        if msg.contains("malformed") || msg.contains("corrupt") || msg.contains("not a database") {
            return true;
        }
        if let Some(sqlite_err) = e.downcast_ref::<rusqlite::Error>() {
            return matches!(
                sqlite_err,
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ffi::ErrorCode::DatabaseCorrupt,
                        ..
                    },
                    _
                ) | rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ffi::ErrorCode::NotADatabase,
                        ..
                    },
                    _
                )
            );
        }
        false
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Open a nestable [`UncheckedSavepoint`] on this connection.
    ///
    /// `name` MUST be a caller-controlled static SQL identifier (never user
    /// input) — it is interpolated into the SAVEPOINT/RELEASE/ROLLBACK
    /// statements verbatim. Use this instead of `conn().unchecked_transaction()`
    /// in code that may run either standalone (autocommit) or wrapped inside an
    /// enclosing transaction: `unchecked_transaction` always issues `BEGIN`,
    /// which errors when a transaction is already open, whereas a SAVEPOINT
    /// auto-starts a transaction when standalone and nests when not. This lets
    /// the full-index pipeline be wrapped atomically by `rebuild_index` without
    /// changing its behavior when run directly.
    pub fn savepoint(&self, name: &'static str) -> Result<UncheckedSavepoint<'_>> {
        self.conn.execute_batch(&format!("SAVEPOINT {name}"))?;
        Ok(UncheckedSavepoint {
            conn: &self.conn,
            name,
            committed: false,
        })
    }

    pub fn vec_enabled(&self) -> bool {
        self.vec_enabled
    }

    /// Run PRAGMA optimize to rebuild query planner statistics after bulk writes,
    /// then checkpoint + TRUNCATE the WAL so the post-index WAL doesn't stay
    /// resident on disk (audit §8). Best-effort: a concurrent reader can defer the
    /// truncation, but it never blocks indefinitely or risks the data.
    pub fn run_optimize(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA optimize;")?;
        // TRUNCATE reclaims the WAL file; tolerate a transient BUSY (best-effort).
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    }

    /// Refresh query-planner statistics MID-RUN, before the global edge
    /// post-passes read the tables the batch loop just filled.
    ///
    /// Those passes are the first thing to run big correlated-subquery joins
    /// over a freshly written graph, and on a brand-new index there is no
    /// `sqlite_stat1` yet, so SQLite plans them by its built-in guesses. On a
    /// real 2,052-file TypeScript repo that costs
    /// `prune_import_contradicted_call_edges` **5.14 s of a 13.5 s full index**.
    /// The same DELETE against the same database with statistics present takes
    /// **0.187 s**; dropping `sqlite_stat1` from that copy reproduces 5.17 s, so
    /// the cost is the query plan, not the predicate.
    ///
    /// `ANALYZE`, not `PRAGMA optimize`: optimize decides what to analyze from
    /// per-connection change counters and is a no-op on a connection that has
    /// not yet accumulated them, which is exactly the fresh-index case this
    /// exists for. Measured at 30 ms on that repo — it buys back ~5 s.
    ///
    /// KNOWN COUNTER-CASE, measured, not hypothetical. Statistics change which
    /// plan SQLite picks, and for `prune_import_contradicted_call_edges` that is
    /// not always the better one. Without statistics its first `EXISTS` leads
    /// with `SEARCH ie USING idx_edges_relation (relation=?)`, and in a
    /// repository with almost no `imports` edges that driver matches ~0 rows and
    /// exits immediately. With statistics the planner sees `idx_edges_relation`
    /// spread over very few distinct relation values (on the corpus below,
    /// `33315 11105` — an estimate of ~11,105 rows for `relation=?`) and
    /// reorders to lead with `idx_nodes_file`, reaching `idx_nodes_name` second
    /// at an estimated 3 rows per name — except the fanned-out name really has
    /// 60. It then pays that per candidate edge.
    ///
    /// The trigger is import DENSITY, not repository size. Measured on a
    /// 605-file synthetic with 60 files exporting one name, varying only how
    /// many of the 540 callers import what they call:
    ///
    /// | callers importing | `imports` edges | with ANALYZE | without |
    /// |---|---|---|---|
    /// | 0 %  | 10  | 0.76 s | **0.36 s** |
    /// | 10 % | 64  | **0.72 s** | 0.84 s |
    /// | 50 % | 280 | **0.60 s** | 2.33 s |
    /// | 100 %| 550 | **0.44 s** | 2.95 s |
    ///
    /// So the loss needs a tree whose files essentially never import anything —
    /// 10 import edges across 605 TypeScript files — and 10 % density already
    /// flips it to a win. Real repositories measured land far on the winning
    /// side (a 2,052-file TypeScript repo: 13.16 s -> 8.52 s). Deliberately NOT
    /// gated on an import-density heuristic: the threshold would be tuned on one
    /// synthetic corpus, and misjudging it forfeits a ~4.6 s win to avoid a
    /// ~0.4 s loss.
    ///
    /// Deliberately NOT `PRAGMA analysis_limit`. SQLite documents that knob for
    /// bounding ANALYZE on large tables, and it looks like the obvious insurance
    /// here because the caller gates on files-touched rather than index size —
    /// but measured, it is the worst of the three options. On a 605-file corpus
    /// built for same-name fan-out, full index time was **0.49 s** with a
    /// complete ANALYZE, **2.92 s** with no ANALYZE at all, and **6.52 s** with
    /// `analysis_limit=400`. Partial statistics produced a plan worse than
    /// having none, i.e. adding the limit would be a 13x regression against what
    /// ships. If you are here to bound this scan, measure that shape first.
    ///
    /// Scope that precisely: on the 0 %-import corpus above the limit measures
    /// 0.56 s — between full ANALYZE (0.76 s) and none (0.36 s), not worst. The
    /// 13x is real and reproduced on the high-density corpus; it is not a claim
    /// that partial statistics are always the worst option.
    ///
    /// Untried lead, if someone wants both ends: `ANALYZE nodes;` alone, leaving
    /// `idx_edges_relation` unanalyzed so the cheap relation-led driver survives
    /// while the name-multiplicity estimate improves. Note that no single join
    /// order wins both shapes — at 100 % density the relation-led plan is the
    /// SLOW one (3.03 s vs 0.44 s) — so pinning the plan is not a way out.
    ///
    /// Best-effort: statistics are an optimization, never correctness, so a
    /// failure here must not fail the index run.
    pub fn refresh_query_stats(&self) {
        if let Err(e) = self.conn.execute_batch("ANALYZE;") {
            tracing::debug!("[db] ANALYZE before edge post-passes failed (continuing): {e}");
        }
    }

    /// Compare the stored embedding dimension (meta table) against the
    /// compile-time EMBEDDING_DIM. Mismatch → drop node_vectors and recreate
    /// at the new dim so the next indexing run re-embeds cleanly.
    ///
    /// Three cases:
    ///   1. meta.embedding_dim == current: no-op.
    ///   2. meta.embedding_dim exists but != current: drop + rebuild.
    ///   3. meta.embedding_dim absent (fresh install OR first post-v7 open on a
    ///      v6 DB): introspect actual vec0 dim from sqlite_master.sql. If it
    ///      exists at a different dim than current, drop + rebuild; otherwise
    ///      just record the current dim. This catches the scenario where a
    ///      user built the binary at one EMBEDDING_DIM, generated a v6 DB,
    ///      then rebuilt the binary at a different EMBEDDING_DIM — without
    ///      this introspection, v6→v7 would silently stamp the wrong dim and
    ///      every subsequent INSERT into node_vectors would crash.
    fn ensure_embedding_dim_consistency(conn: &rusqlite::Connection) -> Result<()> {
        let current: i64 = crate::domain::EMBEDDING_DIM as i64;

        let stored: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| {
                    let v: String = row.get(0)?;
                    Ok(v.parse::<i64>().ok())
                },
            )
            .unwrap_or(None);

        let effective = stored.or_else(|| Self::vec0_dim_from_sqlite_master(conn));

        match effective {
            Some(dim) if dim == current => {} // match, nothing to do
            Some(dim) => {
                tracing::warn!(
                    "[vec] Embedding dim changed: on-disk={} current={}. \
                     Dropping node_vectors and rebuilding at the new dim. \
                     Existing vectors were invalid for the new model.",
                    dim,
                    current
                );
                // Atomically drop + recreate so a mid-statement failure can't
                // leave the DB with no vec0 table at all. embedding_cache is dropped
                // alongside: its cached vectors were computed at the OLD dim and are
                // invalid for the new model, exactly like node_vectors. create_vec_tables_sql
                // recreates both.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch(
                    "DROP TABLE IF EXISTS node_vectors; DROP TABLE IF EXISTS embedding_cache;",
                )?;
                tx.execute_batch(&schema::create_vec_tables_sql())?;
                tx.commit()?;
            }
            None => {
                tracing::debug!(
                    "[vec] No prior vec0 table found; recording embedding_dim={}",
                    current
                );
            }
        }

        // Upsert current dim (idempotent — same value on match).
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [schema::META_KEY_EMBEDDING_DIM, &current.to_string()],
        )?;
        Ok(())
    }

    /// Parse `float[N]` from the node_vectors DDL stored in sqlite_master.sql.
    /// Returns None when the table doesn't exist or the DDL shape is unexpected.
    fn vec0_dim_from_sqlite_master(conn: &rusqlite::Connection) -> Option<i64> {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='node_vectors'",
                [],
                |r| r.get(0),
            )
            .ok()?;
        let start = sql.find("float[")?;
        let remainder = &sql[start + 6..];
        let end = remainder.find(']')?;
        remainder[..end].trim().parse::<i64>().ok()
    }
}

/// RAII SAVEPOINT guard usable through a shared `&Connection` (rusqlite's
/// [`rusqlite::Connection::savepoint`] needs `&mut`). Rolls back to — and
/// releases — the savepoint on drop unless [`UncheckedSavepoint::commit`]
/// released it first, mirroring `Transaction`'s rollback-on-drop semantics.
/// See [`Database::savepoint`] for why SAVEPOINT rather than BEGIN.
#[must_use = "a savepoint rolls back unless committed"]
pub struct UncheckedSavepoint<'c> {
    conn: &'c Connection,
    name: &'static str,
    committed: bool,
}

impl UncheckedSavepoint<'_> {
    /// Release the savepoint. Standalone, releasing the outermost savepoint
    /// commits the transaction SQLite auto-started for it; nested, it merges the
    /// savepoint's work into the enclosing transaction.
    pub fn commit(mut self) -> Result<()> {
        self.conn.execute_batch(&format!("RELEASE {}", self.name))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for UncheckedSavepoint<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Best-effort, like Transaction's drop. ROLLBACK TO undoes to the
            // savepoint but leaves the (possibly auto-started) transaction open;
            // the RELEASE then closes it, so a standalone savepoint that rolled
            // back commits an empty transaction (net: nothing persisted).
            let _ = self
                .conn
                .execute_batch(&format!("ROLLBACK TO {n}; RELEASE {n}", n = self.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_savepoint_standalone_commit_and_rollback() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("index.db")).unwrap();
        db.conn()
            .execute_batch("CREATE TABLE sp_t (x INTEGER);")
            .unwrap();

        // Standalone: a top-level SAVEPOINT auto-starts a transaction; RELEASE
        // (commit) persists the row.
        let sp = db.savepoint("s1").unwrap();
        db.conn()
            .execute("INSERT INTO sp_t VALUES (1)", [])
            .unwrap();
        sp.commit().unwrap();

        // Drop without commit → ROLLBACK TO + RELEASE discards the row.
        let sp = db.savepoint("s2").unwrap();
        db.conn()
            .execute("INSERT INTO sp_t VALUES (2)", [])
            .unwrap();
        drop(sp);

        let rows: Vec<i64> = db
            .conn()
            .prepare("SELECT x FROM sp_t ORDER BY x")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec![1], "committed row kept, rolled-back row dropped");
    }

    #[test]
    fn test_savepoint_nested_in_outer_transaction_discarded_on_rollback() {
        // The rebuild_index atomicity guarantee at the mechanism level: a RELEASE'd
        // inner savepoint whose enclosing transaction later ROLLS BACK leaves nothing
        // behind — a rebuild that clears the index then fails restores the old index.
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("index.db")).unwrap();
        db.conn()
            .execute_batch("CREATE TABLE sp_t (x INTEGER);")
            .unwrap();
        db.conn()
            .execute("INSERT INTO sp_t VALUES (100)", [])
            .unwrap(); // "old index"

        {
            let outer = db.conn().unchecked_transaction().unwrap();
            db.conn().execute("DELETE FROM sp_t", []).unwrap(); // clear (part of outer)
            let sp = db.savepoint("inner").unwrap();
            db.conn()
                .execute("INSERT INTO sp_t VALUES (200)", [])
                .unwrap();
            sp.commit().unwrap(); // released into the outer transaction
            drop(outer); // no commit → ROLLBACK everything, incl. the released savepoint
        }

        let rows: Vec<i64> = db
            .conn()
            .prepare("SELECT x FROM sp_t")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![100],
            "old data restored; both the DELETE and the released savepoint rolled back"
        );
    }

    #[test]
    fn test_v7_records_embedding_dim_on_fresh_db() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open_with_vec(&db_path).unwrap();

        let stored: String = db
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, crate::domain::EMBEDDING_DIM.to_string());
    }

    #[test]
    fn test_index_version_sweep_vacuums_freed_pages() {
        // A version-mismatch open wipes edges/nodes/files; the post-sweep VACUUM
        // must reclaim the freed pages AND must not error on a DB carrying the
        // vec0 virtual table. Without the VACUUM, freelist_count stays > 0.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        {
            let db = Database::open_with_vec(&db_path).unwrap();
            // ~1.2MB of file rows so the sweep frees a meaningful page count.
            let pad = "h".repeat(2048);
            let tx = db.conn().unchecked_transaction().unwrap();
            for i in 0..600 {
                tx.execute(
                    "INSERT INTO files (path, blake3_hash, last_modified, indexed_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![format!("f/{i}.rs"), pad, 0_i64, 0_i64],
                )
                .unwrap();
            }
            tx.commit().unwrap();
            // Stamp an older index generation so the next open triggers the sweep.
            db.conn()
                .pragma_update(None, "application_id", crate::domain::INDEX_VERSION - 1)
                .unwrap();
        }

        // Reopen → INDEX_VERSION mismatch → sweep DELETE + VACUUM.
        let db = Database::open_with_vec(&db_path).unwrap();
        let files: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 0, "version-mismatch sweep must clear files");
        let freelist: i64 = db
            .conn()
            .pragma_query_value(None, "freelist_count", |r| r.get(0))
            .unwrap();
        assert_eq!(
            freelist, 0,
            "post-sweep VACUUM must reclaim freed pages (freelist_count)"
        );
    }

    #[test]
    fn version_bump_wipe_reaps_all_vectors_via_trigger() {
        // Core self-healing invariant behind the "从 1% 重建" analysis: on an INDEX_VERSION
        // bump the indexer open wipes nodes/edges/files, and the `nodes_vectors_ad` AFTER
        // DELETE trigger must reap EVERY wiped node's vector. SQLite disables the truncate
        // optimization when a table carries triggers, so the no-WHERE `DELETE FROM nodes`
        // fires per-row — proving the wipe itself creates NO orphans (daagu's 157 came from
        // the async backfill race, now guarded in insert_node_vectors_batch, not from here).
        use crate::storage::queries::{
            insert_node, insert_node_vector, upsert_file, FileRecord, NodeRecord,
        };
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        {
            let db = Database::open_with_vec(&db_path).unwrap();
            let conn = db.conn();
            let fid = upsert_file(
                conn,
                &FileRecord {
                    path: "a.rs".into(),
                    blake3_hash: "h".into(),
                    last_modified: 0,
                    language: None,
                },
            )
            .unwrap();
            let nid = insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: "f".into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 2,
                    code_content: String::new(),
                    signature: None,
                    doc_comment: None,
                    context_string: Some("ctx".into()),
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
            insert_node_vector(conn, nid, &vec![0.0f32; crate::domain::EMBEDDING_DIM]).unwrap();
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            // Stamp an older generation so the next indexer open triggers the wipe.
            conn.pragma_update(None, "application_id", crate::domain::INDEX_VERSION - 1)
                .unwrap();
        }
        // Reopen (indexer, revalidate=true) → version mismatch → wipe.
        let db = Database::open_with_vec(&db_path).unwrap();
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
            "version-mismatch sweep must clear nodes"
        );
        assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r.get::<_, i64>(0)).unwrap(),
            0, "AFTER DELETE trigger must reap the wiped node's vector — no orphan survives a version bump");
    }

    #[test]
    fn version_bump_preserves_embedding_cache_for_reuse() {
        // C's core invariant: the content-hash embedding_cache SURVIVES the INDEX_VERSION-bump
        // wipe (which only DELETEs nodes/edges/files), so a rebuild reuses embeddings for
        // unchanged content by content hash instead of re-running the model — turning the
        // "从 1% 重建" full re-embed into a byte copy.
        use crate::storage::queries::{
            cache_key, cache_put_embeddings, insert_node, insert_node_vector, partition_by_cache,
            upsert_file, FileRecord, NodeRecord,
        };
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let emb: Vec<f32> = vec![0.25; crate::domain::EMBEDDING_DIM];
        let mk = |conn: &rusqlite::Connection, hash: &str| -> i64 {
            let fid = upsert_file(
                conn,
                &FileRecord {
                    path: "a.rs".into(),
                    blake3_hash: hash.into(),
                    last_modified: 0,
                    language: None,
                },
            )
            .unwrap();
            insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: "f".into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 2,
                    code_content: String::new(),
                    signature: None,
                    doc_comment: None,
                    context_string: Some("ctx-A".into()),
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap()
        };
        {
            let db = Database::open_with_vec(&db_path).unwrap();
            let conn = db.conn();
            let nid = mk(conn, "h1");
            insert_node_vector(conn, nid, &emb).unwrap();
            cache_put_embeddings(conn, &[(cache_key("ctx-A"), emb.clone())]).unwrap();
            conn.pragma_update(None, "application_id", crate::domain::INDEX_VERSION - 1)
                .unwrap();
        }
        // Reopen (indexer) → version mismatch → wipe.
        let db = Database::open_with_vec(&db_path).unwrap();
        let conn = db.conn();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
            "wipe clears nodes"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "trigger reaped the wiped node's vector"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM embedding_cache", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "embedding_cache SURVIVES the version-bump wipe (that is what enables reuse)"
        );
        // Rebuild: a new node (new id) with the same content is a cache HIT — reused, no model.
        let new_nid = mk(conn, "h2");
        let (hits, misses) = partition_by_cache(conn, &[(new_nid, "ctx-A".into())]).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "unchanged content reuses the cached embedding across the bump"
        );
        assert_eq!(hits[0].1, emb, "reused embedding is byte-identical");
        assert!(misses.is_empty(), "nothing left to re-embed");
    }

    #[test]
    fn test_nondestructive_open_preserves_data_on_version_mismatch() {
        // A READER open (health-check, grep, …) must NOT wipe an index built by a
        // different INDEX_VERSION — that was the daagu failure: a statusline poll
        // opened writably and cleared the index to 0 nodes, and with no indexer
        // running nothing rebuilt it. The reader must keep the data and flag stale.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        {
            let db = Database::open_with_vec(&db_path).unwrap();
            let tx = db.conn().unchecked_transaction().unwrap();
            for i in 0..10 {
                tx.execute(
                    "INSERT INTO files (path, blake3_hash, last_modified, indexed_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![format!("f/{i}.rs"), "h", 0_i64, 0_i64],
                )
                .unwrap();
            }
            tx.commit().unwrap();
            // Stamp an older generation so the next open sees a version mismatch.
            db.conn()
                .pragma_update(None, "application_id", crate::domain::INDEX_VERSION - 1)
                .unwrap();
        }

        // Reader open: data intact, staleness flagged, application_id left at OLD
        // so the rebuild is still owed (a later indexer open will revalidate).
        let reader = Database::open_nondestructive(&db_path).unwrap();
        let files: i64 = reader
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 10, "non-destructive reader must NOT clear files");
        assert_eq!(
            reader.index_version_stale(),
            Some(crate::domain::INDEX_VERSION - 1),
            "reader must flag the stale generation it observed"
        );
        let stamped: i32 = reader
            .conn()
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap();
        assert_eq!(
            stamped,
            crate::domain::INDEX_VERSION - 1,
            "reader must NOT bump application_id (would mask the owed rebuild)"
        );

        // A subsequent INDEXER open revalidates: wipes + stamps current + clears stale.
        let indexer = Database::open_with_vec(&db_path).unwrap();
        let files_after: i64 = indexer
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            files_after, 0,
            "indexer open must perform the deferred wipe"
        );
        assert_eq!(
            indexer.index_version_stale(),
            None,
            "post-revalidate handle is current"
        );
    }

    #[test]
    fn test_downgrade_open_never_wipes_newer_index() {
        // The destructive half of the version ping-pong: an OLDER binary opening an
        // index built by a NEWER INDEX_VERSION must NOT wipe it. Before the
        // directional guard, the symmetric `stored != INDEX_VERSION` check made an
        // INDEXER open (revalidate=true) DELETE the newer index and stamp DOWN to
        // this binary's version — so a stale v30 server clobbered the v31 index a
        // current binary had just built, and the two wiped each other on every open
        // (0 nodes, never stable). An older binary must instead leave the newer
        // index (data AND application_id) intact and flag it stale. A genuine
        // permanent downgrade still rebuilds by deleting .code-graph/index.db* first
        // (fresh DB → application_id 0 → this binary stamps its own version cleanly).
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        let newer = crate::domain::INDEX_VERSION + 1;
        {
            let db = Database::open_with_vec(&db_path).unwrap();
            let tx = db.conn().unchecked_transaction().unwrap();
            for i in 0..10 {
                tx.execute(
                    "INSERT INTO files (path, blake3_hash, last_modified, indexed_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![format!("f/{i}.rs"), "h", 0_i64, 0_i64],
                )
                .unwrap();
            }
            tx.commit().unwrap();
            // Stamp a NEWER generation than this binary → a downgrade from its POV.
            db.conn()
                .pragma_update(None, "application_id", newer)
                .unwrap();
        }

        // INDEXER open (revalidate=true) — the exact path a stale older server's
        // startup / a downgrade `incremental-index` takes. Must NOT wipe.
        let indexer = Database::open_with_vec(&db_path).unwrap();
        let files: i64 = indexer
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 10, "downgrade open must NOT wipe a newer index");
        let stamped: i32 = indexer
            .conn()
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap();
        assert_eq!(
            stamped, newer,
            "downgrade open must leave the newer application_id intact (index belongs to the newer binary)"
        );
        assert_eq!(
            indexer.index_version_stale(),
            Some(newer),
            "downgrade must be flagged stale so the caller can warn"
        );
    }

    #[test]
    fn open_rejects_future_schema_with_stable_marker() {
        // A DB whose SCHEMA is newer than this binary supports must be REFUSED
        // (not wiped), carrying the stable statusline marker. Closes the audit's
        // "future-schema refusal untested" gap AND pins domain::SCHEMA_TOO_NEW_MARKER
        // so a reworded message can't silently break the statusline's
        // "↻ updating" vs "offline" discrimination (statusline.js keys on it).
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("future.db");
        {
            let db = Database::open(&db_path).unwrap();
            db.conn()
                .pragma_update(None, "user_version", schema::SCHEMA_VERSION + 1)
                .unwrap();
        }
        // .map(|_| ()) — Database isn't Debug, so unwrap_err needs the Ok type erased.
        let err = Database::open(&db_path).map(|_| ()).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains(crate::domain::SCHEMA_TOO_NEW_MARKER),
            "future-schema bail must carry the stable marker; got: {msg}"
        );

        // Sibling surface (2026-08-16 audit §四): the read-only open is what a
        // SECONDARY server instance uses, i.e. exactly the process that meets a
        // future schema during an update, and it used to sail past this check and
        // fail later with a bare `no such column`. Same refusal, same marker.
        let ro_err = Database::open_readonly(&db_path).map(|_| ()).unwrap_err();
        let ro_msg = format!("{ro_err}");
        assert!(
            ro_msg.contains(crate::domain::SCHEMA_TOO_NEW_MARKER),
            "open_readonly must refuse a future schema with the same marker; got: {ro_msg}"
        );

        // Negative control: the same reader on a CURRENT-schema DB must open.
        // Without this, deleting the version check entirely would still leave the
        // assertion above green if `open_readonly` merely failed for some other
        // reason.
        let ok_path = tmp.path().join("current.db");
        drop(Database::open(&ok_path).unwrap());
        assert!(
            Database::open_readonly(&ok_path).is_ok(),
            "a current-schema DB must still open read-only"
        );
    }

    #[test]
    fn test_embedding_dim_mismatch_rebuilds_vec_table() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let current_dim = crate::domain::EMBEDDING_DIM as i64;

        // First open: normal path records dim
        drop(Database::open_with_vec(&db_path).unwrap());

        // Simulate a model swap by poisoning meta with a wrong dim.
        let fake_dim = current_dim + 1;
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute(
                "UPDATE meta SET value = ?1 WHERE key = ?2",
                [&fake_dim.to_string(), schema::META_KEY_EMBEDDING_DIM],
            )
            .unwrap();
        }

        // Reopen: guard should detect mismatch, drop + recreate node_vectors,
        // and upsert current dim.
        let db = Database::open_with_vec(&db_path).unwrap();
        let stored: i64 = db
            .conn()
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored, current_dim,
            "stored dim must be upserted back to current EMBEDDING_DIM"
        );
        // node_vectors must exist and be empty (rebuilt)
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM node_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_v6_upgrade_rebuilds_vec_table_when_dim_differs() {
        // Reproduces the adversarial I1 scenario: a v6 DB already has a
        // node_vectors vec0 table at a dim different from the current
        // EMBEDDING_DIM (e.g., user rebuilt the binary with a new model).
        // Without sqlite_master introspection, the v6→v7 migration would
        // silently stamp the current dim into meta while leaving the old
        // vec0 in place — every subsequent INSERT would fail at runtime.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let current_dim = crate::domain::EMBEDDING_DIM as i64;
        let fake_dim = if current_dim == 128 { 256 } else { 128 };

        // Hand-craft a v6 DB with a wrong-dim vec0 table.
        {
            register_sqlite_vec();
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            c.execute_batch(&format!(
                "CREATE VIRTUAL TABLE node_vectors USING vec0(
                    node_id INTEGER PRIMARY KEY,
                    embedding float[{}]
                );",
                fake_dim
            ))
            .unwrap();
            c.pragma_update(None, "user_version", 6).unwrap();
        }

        // Reopen: guard must introspect actual vec0 dim, detect mismatch,
        // drop + rebuild at current dim, and stamp meta.
        let db = Database::open_with_vec(&db_path).unwrap();
        let actual = Database::vec0_dim_from_sqlite_master(db.conn()).unwrap();
        assert_eq!(
            actual, current_dim,
            "node_vectors must be rebuilt at current EMBEDDING_DIM after v6→v7 upgrade"
        );
        let stored: String = db
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, current_dim.to_string());
    }

    #[test]
    fn test_v6_to_v7_migration_adds_meta_table() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Build a v6 database by hand, then verify open upgrades it to v7.
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            // Minimal v6 shape: we don't need full tables for this test — just
            // set user_version=6 so the migration path fires on reopen.
            c.pragma_update(None, "user_version", 6).unwrap();
        }

        let db = Database::open_with_vec(&db_path).unwrap();
        // Meta table exists and has our dim recorded
        let stored: String = db
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, crate::domain::EMBEDDING_DIM.to_string());

        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
    }

    /// v7 → v8 adds the `pending_unresolved_calls` table that buffers REL_CALLS
    /// edges Phase 2 couldn't resolve, plus the unique + lookup indexes that
    /// make insert/sweep O(log N). Mirrors the pattern of every prior migration
    /// test — bypassing it would silently drop the only safety net we have for
    /// catching schema drift between create_tables_sql and migrate_v7_to_v8.
    #[test]
    fn test_v7_to_v8_migration_adds_pending_table() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Build a v7 database by hand. Construct the v7 shape (files+nodes+edges
        // tables) so the migration runs against realistic schema state.
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            c.execute_batch(&schema::create_tables_sql()).unwrap();
            c.pragma_update(None, "user_version", 7).unwrap();
        }

        // Open via Database::open — the v7→v8 migration must run.
        let db = Database::open(&db_path).unwrap();

        // (a) Pending table exists and is empty (fresh migration → no rows).
        let pending_count =
            crate::storage::queries::count_pending_unresolved_calls(db.conn()).unwrap();
        assert_eq!(
            pending_count, 0,
            "fresh migration must leave pending_unresolved_calls empty"
        );

        // (b) The unique index (source_id, target_name, source_language) exists —
        // without it, repeated Phase 2 invocations on the same file would
        // grow the table unbounded.
        let unique_idx_exists: bool = db
            .conn()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_pending_unique'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(unique_idx_exists,
            "idx_pending_unique must exist after v7→v8 migration (insert idempotency depends on it)");

        // (c) The (target_name, source_language) lookup index exists — the sweep
        // depends on this for sub-O(N) name lookup.
        let lookup_idx_exists: bool = db
            .conn()
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_pending_target_lang'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(
            lookup_idx_exists,
            "idx_pending_target_lang must exist after v7→v8 migration"
        );

        // (d) user_version pragma actually advanced.
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
        assert!(version >= 8, "version must have advanced to at least 8");
    }

    /// M3: a crash between a mid-sequence migration commit and the final
    /// user_version stamp leaves user_version=2 while `edges` already carries the
    /// extra columns a later migration added (v9's `confidence` → 6 columns). The
    /// v2→v3 migration used `INSERT INTO edges_new SELECT *`, which then failed
    /// with "5 columns but 6 values" — and because is_corruption_error didn't
    /// match, the DB was PERMANENTLY unopenable (no self-heal). Explicit column
    /// names make the re-run forward-compatible; the sequence must converge to
    /// the current schema instead of bricking.
    #[test]
    fn v2_migration_survives_schema_ahead_of_stamped_version() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        {
            // Full v9 schema (6-column edges incl. `confidence`) but user_version
            // stamped at 2 — exactly the partial-crash state that bricked the DB.
            // The v2→v3 `INSERT ... SELECT *` arity mismatch (5-col target vs 6-col
            // source) fires on the empty table too, so no fixture rows are needed.
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            c.execute_batch(&schema::create_tables_sql()).unwrap();
            c.pragma_update(None, "user_version", 2).unwrap();
        }

        // Must open cleanly (re-run migrations from v2), not brick.
        let db = Database::open(&db_path).unwrap();
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION,
            "a v2-stamped DB whose edges already has extra columns must migrate to current, not brick");
    }

    /// v8 → v9 adds `edges.confidence`. Critical: a real upgrade keeps the
    /// existing `edges` table, so `CREATE TABLE IF NOT EXISTS` is a no-op and the
    /// column must arrive via ALTER. Without the migration an upgraded user's DB
    /// crashed with `no such column: confidence` on the next index pass / `refs`
    /// query. We hand-build a COLUMN-LESS edges table (the pre-v9 shape) — using
    /// create_tables_sql() would wrongly include the new column and mask the bug.
    #[test]
    fn test_v8_to_v9_migration_adds_confidence_column() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            // Pre-v9 edges shape: no `confidence` column.
            c.execute_batch(
                "CREATE TABLE edges (
                    id          INTEGER PRIMARY KEY,
                    source_id   INTEGER NOT NULL,
                    target_id   INTEGER NOT NULL,
                    relation    TEXT NOT NULL,
                    metadata    TEXT
                );",
            )
            .unwrap();
            c.pragma_update(None, "user_version", 8).unwrap();
        }

        // Open via Database::open — the v8→v9 migration must run.
        let db = Database::open(&db_path).unwrap();

        // (a) The column now exists with the backfill default.
        let has_col: bool = db
            .conn()
            .query_row(
                "SELECT 1 FROM pragma_table_info('edges') WHERE name = 'confidence'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(has_col, "edges.confidence must exist after v8→v9 migration");

        // (b) The exact query that crashed pre-fix now succeeds (no rows is fine).
        let _: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE confidence = 'extracted'",
                [],
                |r| r.get(0),
            )
            .expect("SELECT on edges.confidence must not error after migration");

        // (c) user_version advanced.
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
        assert!(version >= 9, "version must have advanced to at least 9");
    }

    /// v9→v10 seam (D#77 bounded pending retention): upgraded DBs keep their
    /// existing `pending_unresolved_calls` table, where `CREATE TABLE IF NOT
    /// EXISTS` is a no-op — without migrate_v9_to_v10 the `attempts` column
    /// never appears and the sweep's aging UPDATE crashes with
    /// `no such column: attempts`. Hand-build the COLUMN-LESS pre-v10 shape
    /// (create_tables_sql() would include the column and mask the bug).
    #[test]
    fn test_v9_to_v10_migration_adds_attempts_column() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            // Pre-v10 pending shape: no `attempts` column.
            c.execute_batch(
                "CREATE TABLE pending_unresolved_calls (
                    id              INTEGER PRIMARY KEY,
                    source_id       INTEGER NOT NULL,
                    target_name     TEXT NOT NULL,
                    source_language TEXT NOT NULL,
                    metadata        TEXT
                );",
            )
            .unwrap();
            c.execute(
                "INSERT INTO pending_unresolved_calls (source_id, target_name, source_language)
                 VALUES (1, 'foo', 'typescript')",
                [],
            )
            .unwrap();
            c.pragma_update(None, "user_version", 9).unwrap();
        }

        let db = Database::open(&db_path).unwrap();

        // (a) The column now exists, existing rows backfilled to 0.
        let attempts: i64 = db
            .conn()
            .query_row(
                "SELECT attempts FROM pending_unresolved_calls WHERE target_name = 'foo'",
                [],
                |r| r.get(0),
            )
            .expect("attempts column must exist after v9→v10 migration");
        assert_eq!(attempts, 0, "pre-existing rows must backfill attempts = 0");

        // (b) The exact statements the sweep runs now succeed.
        let evicted = crate::storage::queries::age_and_evict_pending_unresolved_calls(db.conn())
            .expect("age/evict must not error after migration");
        assert_eq!(
            evicted, 0,
            "a once-aged row is far below the eviction threshold"
        );

        // (c) user_version advanced.
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
    }

    #[test]
    fn test_open_readonly_rejects_writes() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        // Primary bootstrap: create the DB normally.
        drop(Database::open(&db_path).unwrap());

        // Secondary opens read-only.
        let ro = Database::open_readonly(&db_path).unwrap();

        // Reads work.
        let count: i64 = ro
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Writes must fail at the SQLite layer — not bubble up as silent no-ops.
        let err = ro
            .conn()
            .execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) \
                 VALUES ('a', 'b', 0, 'rust', 0)",
                [],
            )
            .unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("readonly") || msg.contains("read-only") || msg.contains("read only"),
            "Expected read-only error, got: {}",
            msg
        );
    }

    #[test]
    fn test_open_readonly_missing_file_errors() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("no-such.db");
        assert!(Database::open_readonly(&missing).is_err());
    }

    #[test]
    fn test_init_creates_db_and_tables() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"files".to_string()));
        assert!(tables.contains(&"nodes".to_string()));
        assert!(tables.contains(&"edges".to_string()));
        assert!(!tables.contains(&"context_sandbox".to_string()));
    }

    #[test]
    fn test_schema_version_is_set() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
    }

    #[test]
    fn test_wal_mode_enabled() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let mode: String = db
            .conn()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn test_v1_to_v2_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v1 database manually (without the 3 new columns)
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT, UNIQUE(source_id, target_id, relation)
                );
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();
            // Insert test data to verify preservation
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'hello', 'hello', 1, 5, 'function hello() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }

        // Open with Database::open — should trigger v1→v2 migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify new columns exist (can write to them)
        db.conn().execute(
            "UPDATE nodes SET name_tokens = 'hello', return_type = 'void', param_types = '()' WHERE id = 1",
            [],
        ).unwrap();

        // Verify FTS5 has 8 columns (insert trigger fires on UPDATE with new columns)
        let fts_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH 'hello'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            fts_count >= 1,
            "FTS5 should find existing data after migration rebuild"
        );

        // Verify existing data preserved
        let name: String = db
            .conn()
            .query_row("SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "hello");
    }

    // P2-8 migration gate: a fresh DB (create_tables_sql) and a v1 DB upgraded
    // through every migrate_vN must end with identical files/nodes/edges column
    // schemas. Catches a FORGOTTEN migration — adding a column to
    // create_tables_sql without a guarded-ALTER migrate_vN (+ SCHEMA_VERSION
    // bump) makes fresh installs work while existing users crash with
    // "no such column" on upgrade (the v0.54 edges.confidence class) — and
    // catches create_tables vs migrate_vN column-definition drift (the confidence
    // column lives in two hand-maintained places). Runs in `cargo test`, so the
    // pre-commit + CI test gate fails the build instead of a user's first open.
    // Set comparison by column name (NOT ordered): ALTER ADD COLUMN always
    // appends, so column order legitimately differs between the two paths.
    #[test]
    fn open_disables_mmap_to_avoid_sigbus_on_truncation() {
        // Regression guard for the snapshot_integration with-embed SIGBUS: sqlite
        // mmap raises SIGBUS when the mapped DB file is truncated (VACUUM/checkpoint)
        // while a reader holds the mapping. Keep mmap off — see the open() pragmas.
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("index.db")).unwrap();
        let mmap: i64 = db
            .conn()
            .pragma_query_value(None, "mmap_size", |r| r.get(0))
            .unwrap();
        assert_eq!(
            mmap, 0,
            "mmap must stay disabled to avoid the truncation SIGBUS"
        );
    }

    #[test]
    fn open_readonly_also_disables_mmap() {
        // The secondary / flock-denied reader (open_readonly) holds a mapping while
        // the primary VACUUMs/checkpoints — the same truncation-SIGBUS hazard as
        // open(). Pin mmap=0 on this path too so the invariant is enforced in code,
        // not left to SQLite's compile default.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("index.db");
        // Materialize a real DB the read-only handle can attach to.
        Database::open(&path).unwrap();
        let ro = Database::open_readonly(&path).unwrap();
        let mmap: i64 = ro
            .conn()
            .pragma_query_value(None, "mmap_size", |r| r.get(0))
            .unwrap();
        assert_eq!(mmap, 0, "read-only secondary must also keep mmap disabled");
    }

    #[test]
    fn fresh_schema_matches_fully_migrated_schema() {
        use std::collections::BTreeMap;
        // column name -> (declared type, notnull, pk, default expr). The default
        // (PRAGMA table_info col 4) is compared too: a `DEFAULT` drift between
        // create_tables_sql and a migrate_vN ALTER — e.g. edges.confidence declared
        // `DEFAULT 'extracted'` in one site but a different literal in the other —
        // is invisible to a name/type/notnull/pk diff. feedback_schema_column_migration_seam.
        fn columns(
            conn: &Connection,
            table: &str,
        ) -> BTreeMap<String, (String, bool, bool, Option<String>)> {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        (
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)? != 0,
                            r.get::<_, i64>(5)? != 0,
                            r.get::<_, Option<String>>(4)?,
                        ),
                    ))
                })
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        }

        // Fresh DB at the current schema.
        let fresh_tmp = TempDir::new().unwrap();
        let fresh = Database::open(&fresh_tmp.path().join("index.db")).unwrap();

        // A v1 DB → Database::open runs every migration up to SCHEMA_VERSION.
        let mig_tmp = TempDir::new().unwrap();
        let mig_path = mig_tmp.path().join("index.db");
        {
            register_sqlite_vec();
            let conn = Connection::open(&mig_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
                .unwrap();
            // Frozen v1 schema (the shape the v1->v2 test builds).
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT, UNIQUE(source_id, target_id, relation)
                );
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
        let migrated = Database::open(&mig_path).unwrap();

        // `meta` (migrate_v6_to_v7) and `pending_unresolved_calls` (migrate_v7_to_v8)
        // are created by both create_tables_sql and a migrate_vN — include them so a
        // future column drift on either is caught, not just files/nodes/edges.
        for table in [
            "files",
            "nodes",
            "edges",
            "meta",
            "pending_unresolved_calls",
        ] {
            assert_eq!(
                columns(fresh.conn(), table),
                columns(migrated.conn(), table),
                "table `{table}`: fresh create_tables_sql schema diverges from the \
                 v1->vN migration result — a migrate_vN / SCHEMA_VERSION bump is \
                 missing, or a column definition drifted between create_tables_sql \
                 and its migrate_vN",
            );
        }
    }

    #[test]
    fn test_corrupt_db_auto_recovery() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        // Write garbage to simulate corruption
        std::fs::write(&db_path, b"this is not a valid sqlite database").unwrap();
        // Should auto-delete and recreate instead of crashing
        let db = Database::open(&db_path).unwrap();
        // Verify it works — tables were created
        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            tables.contains(&"files".to_string()),
            "Expected 'files' table after recovery"
        );
        assert!(
            tables.contains(&"nodes".to_string()),
            "Expected 'nodes' table after recovery"
        );
    }

    #[test]
    fn test_corrupt_db_removes_wal_and_shm() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let wal_path = db_path.with_extension("db-wal");
        let shm_path = db_path.with_extension("db-shm");
        // Create corrupt DB + stale WAL/SHM files
        std::fs::write(&db_path, b"not a database").unwrap();
        std::fs::write(&wal_path, b"stale wal").unwrap();
        std::fs::write(&shm_path, b"stale shm").unwrap();
        // Recovery should clean up stale WAL and SHM before recreating
        let _db = Database::open(&db_path).unwrap();
        // The new connection creates a fresh WAL (because we use PRAGMA journal_mode=WAL),
        // but the stale content must be gone — verify the WAL is not our sentinel value
        if wal_path.exists() {
            let content = std::fs::read(&wal_path).unwrap();
            assert_ne!(
                content, b"stale wal",
                "Stale WAL content should be replaced"
            );
        }
        // SHM may or may not be recreated depending on WAL activity
        if shm_path.exists() {
            let content = std::fs::read(&shm_path).unwrap();
            assert_ne!(
                content, b"stale shm",
                "Stale SHM content should be replaced"
            );
        }
    }

    #[test]
    fn test_non_corruption_error_still_propagates() {
        // Opening a path where the parent dir doesn't exist is not corruption
        let bad_path = Path::new("/nonexistent_dir_xyz/impossible/index.db");
        let result = Database::open(bad_path);
        assert!(
            result.is_err(),
            "Non-corruption errors should still propagate"
        );
    }

    // ============================================================
    // Sub-header corrupt-state matrix (size guard).
    //
    // SQLite's on-disk format starts with a 100-byte database header. Any
    // pre-existing file smaller than that cannot be a valid database. Without
    // a proactive guard, post-crash residue (0-byte main + stale .wal/.shm,
    // partial-write main, etc.) lands the open path in SQLite-version-
    // dependent territory: sometimes Connection::open silently treats the
    // file as fresh, sometimes the WAL frames are replayed against the empty
    // main, sometimes an error surfaces and triggers the existing
    // is_corruption_error recovery branch. This non-determinism produced the
    // user-observed "first run exit=1, second run exit=0" flakiness.
    //
    // The size guard wipes main + wal + shm proactively whenever main exists
    // but is < 100 bytes, so every recovery path starts from the same blank
    // state and the resulting DB is always a fresh, well-formed schema.
    // ============================================================

    #[test]
    fn test_zero_byte_main_with_stale_wal_shm_recovers_clean() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let wal_path = db_path.with_extension("db-wal");
        let shm_path = db_path.with_extension("db-shm");

        // Post-crash residue: main truncated to 0 bytes, but stale wal/shm
        // bytes from a prior process remain. Without the size guard, SQLite
        // may either ignore or partially apply the wal — non-deterministic.
        std::fs::write(&db_path, b"").unwrap();
        std::fs::write(&wal_path, b"STALE_WAL_SENTINEL_FROM_PRIOR_PROCESS").unwrap();
        std::fs::write(&shm_path, b"STALE_SHM_SENTINEL").unwrap();

        let db = Database::open(&db_path).unwrap();

        // Fresh schema must be in place.
        let tables: Vec<String> = db
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            tables.contains(&"files".to_string()),
            "expected 'files' after recovery"
        );
        assert!(
            tables.contains(&"nodes".to_string()),
            "expected 'nodes' after recovery"
        );

        // Stale wal/shm bytes must NOT survive — otherwise the next open
        // could replay them against the freshly-recreated main.
        if wal_path.exists() {
            let content = std::fs::read(&wal_path).unwrap();
            assert!(
                !content.starts_with(b"STALE_WAL_SENTINEL"),
                "stale WAL sentinel must be wiped; first 40 bytes: {:?}",
                &content[..content.len().min(40)]
            );
        }
        if shm_path.exists() {
            let content = std::fs::read(&shm_path).unwrap();
            assert!(
                !content.starts_with(b"STALE_SHM_SENTINEL"),
                "stale SHM sentinel must be wiped"
            );
        }

        // Main must now be a real DB (header + at least one page).
        let main_size = std::fs::metadata(&db_path).unwrap().len();
        assert!(
            main_size >= 100,
            "recovered main DB should be >= 100 bytes (SQLite header), got {}",
            main_size
        );
    }

    #[test]
    fn test_zero_byte_main_alone_recovers_clean() {
        // Even without stale wal/shm, a 0-byte main file is a corrupt-state
        // signal (some prior write was interrupted before SQLite committed
        // its header). The size guard normalises the recovery path so the
        // resulting DB is always a fresh schema, never a 0-byte file that a
        // later open would have to re-handle.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        std::fs::write(&db_path, b"").unwrap();

        let db = Database::open(&db_path).unwrap();
        let row_count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 0, "recovered DB must be empty (no carryover)");
        let main_size = std::fs::metadata(&db_path).unwrap().len();
        assert!(
            main_size >= 100,
            "main DB must be >= header size, got {}",
            main_size
        );
    }

    #[test]
    fn test_partial_write_under_header_size_recovers() {
        // 50 bytes is below the 100-byte SQLite header — structurally
        // invalid regardless of the magic-string prefix. Without the size
        // guard this either errors via is_corruption_error (newer SQLite)
        // or silently lands in undefined territory (older SQLite).
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let mut partial = b"SQLite format 3\0".to_vec();
        partial.extend_from_slice(&[0u8; 34]);
        assert_eq!(partial.len(), 50);
        std::fs::write(&db_path, partial).unwrap();

        let db = Database::open(&db_path).unwrap();
        let row_count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 0, "recovered DB must be empty");
    }

    #[test]
    fn test_size_guard_preserves_valid_db() {
        // Regression guard: the size threshold must not trigger on a real,
        // populated database. Insert one row, close, reopen — data survives.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        {
            let db = Database::open(&db_path).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO files (path, blake3_hash, last_modified, indexed_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params!["preserved.rs", "deadbeef", 0i64, 0i64],
                )
                .unwrap();
        }

        let pre_size = std::fs::metadata(&db_path).unwrap().len();
        assert!(
            pre_size > 100,
            "valid DB after one insert must exceed header size"
        );

        let db = Database::open(&db_path).unwrap();
        let path: String = db
            .conn()
            .query_row("SELECT path FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "preserved.rs", "valid DB must not be wiped on reopen");
    }

    #[test]
    fn test_v2_to_v3_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v2 database manually:
        // - nodes has name_tokens, return_type, param_types (added in v1->v2)
        // - edges has UNIQUE(source_id, target_id, relation) -- old constraint without metadata
        // - FTS5 has 8 columns but NO porter stemmer
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT,
                    UNIQUE(source_id, target_id, relation)
                );
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();

            // Insert test data
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'hello', 'hello', 1, 5, 'function hello() {}')",
                [],
            ).unwrap();
            // Insert an edge to verify data preservation through table recreation
            conn.execute(
                "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (1, 1, 'calls', 'GET /api')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }

        // Open with Database::open -- triggers v2->v3 (and v3->v4, v4->v5) migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify the new UNIQUE index exists on edges (includes metadata via COALESCE)
        let idx_exists: bool = db.conn().query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_edges_unique'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(
            idx_exists,
            "idx_edges_unique should exist after v2->v3 migration"
        );

        // Verify that edges with same (source, target, relation) but different metadata are allowed
        // (this was the whole point of v3: metadata is part of the unique constraint)
        db.conn().execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (1, 1, 'calls', 'POST /api')",
            [],
        ).unwrap();

        // Verify existing edge data preserved
        let edge_meta: String = db
            .conn()
            .query_row(
                "SELECT metadata FROM edges WHERE source_id = 1 AND metadata = 'GET /api'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edge_meta, "GET /api");

        // Verify existing node data preserved
        let name: String = db
            .conn()
            .query_row("SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "hello");
    }

    #[test]
    fn test_v3_to_v4_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v3 database manually:
        // - nodes has name_tokens, return_type, param_types
        // - edges has the v3 UNIQUE constraint (includes metadata)
        // - FTS5 has 8 columns but NO porter stemmer (plain tokenizer)
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT
                );
                CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();

            // Insert test data with a word that tests porter stemming
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'running', 'running', 1, 5, 'function running() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }

        // Open with Database::open -- triggers v3->v4 (and v4->v5) migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify porter stemming works: searching "run" should match "running"
        let fts_count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH 'run'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            fts_count >= 1,
            "Porter stemmer should allow 'run' to match 'running'"
        );

        // Verify existing node data preserved
        let name: String = db
            .conn()
            .query_row("SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "running");
    }

    #[test]
    fn test_v4_to_v5_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v4 database manually:
        // - nodes has name_tokens, return_type, param_types (but NO is_test column)
        // - edges has v3 UNIQUE constraint (includes metadata)
        // - FTS5 has porter stemmer
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT
                );
                CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id',
                    tokenize='porter unicode61'
                );
                CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;
                CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                END;
                CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;"
            ).unwrap();

            // Insert test data
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'myFunc', 'myFunc', 1, 5, 'function myFunc() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }

        // Open with Database::open -- triggers v4->v5 migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify is_test column exists and defaults to 0 for existing rows
        let is_test: i32 = db
            .conn()
            .query_row("SELECT is_test FROM nodes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(is_test, 0, "is_test should default to 0 for existing rows");

        // Verify we can set is_test to 1
        db.conn()
            .execute("UPDATE nodes SET is_test = 1 WHERE id = 1", [])
            .unwrap();
        let is_test_updated: i32 = db
            .conn()
            .query_row("SELECT is_test FROM nodes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(is_test_updated, 1);

        // Verify existing node data preserved
        let name: String = db
            .conn()
            .query_row("SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(name, "myFunc");
    }

    #[test]
    fn test_v5_to_v6_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v5 database manually:
        // - nodes has is_test column (added in v4->v5)
        // - NO idx_nodes_qualified_name index
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
                .unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT,
                    is_test INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX idx_nodes_file ON nodes(file_id);
                CREATE INDEX idx_nodes_type ON nodes(type);
                CREATE INDEX idx_nodes_name ON nodes(name);
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT
                );
                CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id',
                    tokenize='porter unicode61'
                );
                CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;
                CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                END;
                CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;"
            ).unwrap();

            // Insert test data
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'myFunc', 'MyModule.myFunc', 1, 5, 'function myFunc() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
        }

        // Open with Database::open -- triggers v5->v6 migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db
            .conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify idx_nodes_qualified_name index exists
        let idx_exists: bool = db.conn().query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_nodes_qualified_name'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(
            idx_exists,
            "idx_nodes_qualified_name should exist after v5->v6 migration"
        );

        // Verify existing node data preserved
        let qname: String = db
            .conn()
            .query_row("SELECT qualified_name FROM nodes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(qname, "MyModule.myFunc");
    }

    #[test]
    fn test_vec0_extension_loads() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open_with_vec(&tmp.path().join("test.db")).unwrap();
        // Try creating a vec0 table
        db.conn()
            .execute_batch("CREATE VIRTUAL TABLE test_vec USING vec0(embedding float[4]);")
            .unwrap();
        // Insert a vector
        let vec_data: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
        let bytes: &[u8] = bytemuck::cast_slice(&vec_data);
        db.conn()
            .execute(
                "INSERT INTO test_vec(rowid, embedding) VALUES (1, ?)",
                [bytes],
            )
            .unwrap();
    }

    #[test]
    fn test_vec0_vector_search() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open_with_vec(&tmp.path().join("test.db")).unwrap();
        db.conn()
            .execute_batch("CREATE VIRTUAL TABLE test_vec USING vec0(embedding float[4]);")
            .unwrap();

        // Insert vectors
        let vecs: Vec<Vec<f32>> = vec![
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0],
            vec![0.9, 0.1, 0.0, 0.0], // similar to first
        ];
        for (i, v) in vecs.iter().enumerate() {
            let bytes: &[u8] = bytemuck::cast_slice(v);
            db.conn()
                .execute(
                    "INSERT INTO test_vec(rowid, embedding) VALUES (?1, ?2)",
                    rusqlite::params![i as i64 + 1, bytes],
                )
                .unwrap();
        }

        // Search for similar to [1,0,0,0]
        let query: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
        let query_bytes: &[u8] = bytemuck::cast_slice(&query);
        let mut stmt = db.conn().prepare(
            "SELECT rowid, distance FROM test_vec WHERE embedding MATCH ?1 ORDER BY distance LIMIT 2"
        ).unwrap();
        let results: Vec<(i64, f64)> = stmt
            .query_map([query_bytes], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1); // exact match first
    }
}
