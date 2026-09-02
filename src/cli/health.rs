use super::*;

// Internal notes — `//` (not `///`) so clap leaves them out of `--help`: --json and
// --format coexist for back-compat (--json is shorthand for `--format json` and wins
// when both are given); resolved_format() below collapses them into the single `&str`
// the handler consumes, so cmd_health_check's signature and its JSON/oneline branches
// stay untouched (plan §2 item 14).
/// CLI arguments for the `health-check` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp health-check",
    about = "Query index status (nodes/edges/files, freshness, embedding coverage)"
)]
pub struct HealthCheckArgs {
    /// JSON output (shorthand for --format json; wins when both are set)
    #[arg(long)]
    pub json: bool,
    /// Output format: oneline (default) or json
    #[arg(long)]
    pub format: Option<String>,
    /// Run `PRAGMA quick_check` regardless of index size (see INTEGRITY_PRAGMA_MAX_BYTES)
    #[arg(long)]
    pub deep: bool,
}

impl HealthCheckArgs {
    /// Collapse `--json`/`--format` into the handler's format string.
    /// `--json` takes precedence; absent both, defaults to "oneline".
    /// Unrecognized `--format` values are REJECTED by the handler (`cmd_health_check_opts`
    /// bails before doing any work). They used to fall through to the oneline branch,
    /// carried over from the prior hand-parser, which meant a script asking for JSON
    /// got prose and exit 0.
    pub fn resolved_format(&self) -> &str {
        if self.json {
            "json"
        } else {
            self.format.as_deref().unwrap_or("oneline")
        }
    }
}

/// Recording-side state of the recommend→use conversion metric, surfaced by
/// `stats` and `health-check` so a dark metric is a visible signal rather than
/// silence. `"absent"` = `recommendations.jsonl` missing (the PreToolUse hooks
/// that record recommendations are not active in this project — e.g. it runs a
/// dev `.mcp.json` server with the marketplace plugin disabled, so the metric is
/// structurally dark); `"empty"` = file present, no recommendations yet;
/// `"live"` = recommendations recorded.
pub fn recommendation_metric_state(project_root: &Path) -> &'static str {
    let p = project_root
        .join(CODE_GRAPH_DIR)
        .join("recommendations.jsonl");
    match std::fs::read_to_string(&p) {
        Err(_) => "absent",
        Ok(c) => {
            if aggregate_recommendations_jsonl(&c).total > 0 {
                "live"
            } else {
                "empty"
            }
        }
    }
}

/// Size ceiling above which `health-check` skips `PRAGMA quick_check`.
///
/// quick_check reads every page, and this command is not only a diagnostic — the
/// statusline polls `health-check --format json` on every render, under a
/// 1500 ms inner budget (statusline.js) whose overrun renders the segment as
/// "offline". A/B on this repo's 110 MB index with the same binary: 0.02 s with
/// the pragma skipped, 0.28 s with it — ~2.4 ms/MB, so an unbounded scan would
/// trade a real signal for a broken one on exactly the largest indexes.
///
/// 32 MB keeps the polled path near 80 ms even when the page cache is cold,
/// which is the case that matters: the 2.4 ms/MB above was measured warm, and a
/// quick_check reads EVERY page, so the first render after a cold boot pays disk
/// latency for the whole file. At 128 MB that is what makes the statusline
/// segment vanish.
///
/// Above the limit the probe reports `"skipped_large"` — visibly absent, never
/// silently, and `doctor` renders that as a skipped row rather than a pass.
/// Full verification stays reachable via `--deep`, which ignores the gate.
/// `doctor` deliberately does NOT pass it: doctor's own budget for this call is
/// 5 s, and a multi-GB index would time out and report a phantom
/// "health-check failed" instead of the integrity answer it went looking for.
pub(crate) const INTEGRITY_PRAGMA_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Cheap read-only integrity probes for `health-check` (audit 2026-08-02 DB-1:
/// the command's `healthy` was `schema_ok && nodes>0 && files>0`, so page-level
/// corruption, an FTS index that stopped tracking `nodes`, and orphaned vectors
/// were all invisible).
///
/// Every field is `Option`: `None` means "could not be measured" (pragma error
/// under writer contention, table absent in a no-vec index), which must never be
/// reported as a fault. Only a quick_check that *ran* and *complained* counts as
/// corruption.
pub(crate) struct IndexIntegrity {
    /// `PRAGMA quick_check` verdict: `"ok"`, SQLite's first complaint, or
    /// `"skipped_large"` when the DB exceeds [`INTEGRITY_PRAGMA_MAX_BYTES`].
    quick_check: Option<String>,
    /// `COUNT(nodes)` − rows the FTS5 index actually holds. Non-zero means
    /// search silently misses (or invents) symbols.
    fts_drift: Option<i64>,
    /// Vectors whose node is gone — dead weight that also skews coverage math.
    orphan_vectors: Option<i64>,
}

impl IndexIntegrity {
    fn probe(conn: &rusqlite::Connection, db_size_bytes: u64, deep: bool) -> Self {
        // Overridable so the skip branch is testable without materializing a
        // 128 MB index (same escape-hatch shape as CODE_GRAPH_RESYNC_BUDGET and
        // CODE_GRAPH_RG_ARGV_BUDGET), and so a user on slow storage can tighten
        // it without waiting for a release.
        let ceiling = std::env::var("CODE_GRAPH_INTEGRITY_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(INTEGRITY_PRAGMA_MAX_BYTES);
        let quick_check = if !deep && db_size_bytes > ceiling {
            Some("skipped_large".to_string())
        } else {
            conn.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))
                .ok()
        };

        // NOT `COUNT(*) FROM nodes_fts`: `nodes_fts` is an EXTERNAL-CONTENT table
        // (`content='nodes'`, schema.rs:64), so counting it reads through to
        // `nodes` and can only ever return `COUNT(nodes)` — a control that
        // cannot fail, which is how a drift check gets shipped that never
        // detects drift. `nodes_fts_docsize` is the FTS5 shadow table with one
        // row per document the index really holds, maintained by the triggers,
        // so it moves independently of the content table.
        let fts_drift = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM nodes) - (SELECT COUNT(*) FROM nodes_fts_docsize)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok();

        // Guarded by the sqlite_master probe (same shape as
        // queries::count_nodes_with_vectors): `node_vectors` is a vec0 virtual
        // table, absent from a structure-only index.
        let orphan_vectors = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_vectors'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .filter(|present: &i64| *present > 0)
            .and_then(|_| {
                conn.query_row(
                    "SELECT COUNT(*) FROM node_vectors WHERE node_id NOT IN (SELECT id FROM nodes)",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            });

        Self {
            quick_check,
            fts_drift,
            orphan_vectors,
        }
    }

    /// The one finding severe enough to flip `healthy` — the DB pages themselves
    /// do not read back. A skipped or unmeasurable check is NOT corruption.
    fn corruption_reason(&self) -> Option<&str> {
        match self.quick_check.as_deref() {
            Some("ok") | Some("skipped_large") | None => None,
            Some(msg) => Some(msg),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "quick_check": self.quick_check,
            "fts_drift": self.fts_drift,
            "orphan_vectors": self.orphan_vectors,
        })
    }

    /// One human line, printed on the healthy and unhealthy paths alike so the
    /// two output faces never disagree about what was checked.
    fn to_line(&self) -> String {
        let fmt_count = |v: Option<i64>| match v {
            Some(n) => n.to_string(),
            None => "unavailable".to_string(),
        };
        format!(
            "Integrity: quick_check {} · FTS drift {} · orphan vectors {}",
            self.quick_check.as_deref().unwrap_or("unavailable"),
            fmt_count(self.fts_drift),
            fmt_count(self.orphan_vectors),
        )
    }
}

/// Run health check and print status, including index freshness.
pub fn cmd_health_check(project_root: &Path, format: &str) -> Result<()> {
    cmd_health_check_opts(project_root, format, false)
}

/// `cmd_health_check` with the `--deep` toggle. Kept as a separate entry point so
/// the existing two-argument signature (main.rs, tests) stays intact.
pub fn cmd_health_check_opts(project_root: &Path, format: &str, deep: bool) -> Result<()> {
    // Entry validation, the idiom every other enum-bearing flag in this CLI already
    // uses (`impact --change-type`, `--min-confidence`, the type filters). `--format`
    // was the one that skipped it: `resolved_format()` hands the raw string through
    // and only "json" is special-cased below, so `--format jsonn` printed the HUMAN
    // one-liner and exited 0. A script asking for JSON and getting prose with a
    // success code has no way to tell (2026-08-16 audit §四). The bail flows through
    // main's Tier-3 catch, so `--json`-shaped callers still get an error OBJECT.
    if !matches!(format, "oneline" | "json") {
        anyhow::bail!("--format must be one of: oneline, json (got '{format}')");
    }
    // JSON callers (doctor.js, scripts, MCP UIs) need a parseable response
    // even when the index is missing — bailing with a stderr-only anyhow error
    // forces them to grep messages instead of reading JSON fields.
    if format == "json" {
        // Worktree-aware, like every read command: the raw project_root check
        // reported {"healthy":false,"reason":"no_index"} from a linked worktree
        // whose MAIN checkout has a perfectly good index, while the human
        // format (via CliContext::open below) said "OK" — same command, two
        // formats, opposite verdicts, and doctor.js consumes the JSON one, so
        // every worktree showed a phantom broken install (audit 2026-08-02
        // MED-3).
        let db_path = effective_read_root(project_root)
            .join(CODE_GRAPH_DIR)
            .join("index.db");
        if !db_path.exists() {
            let payload = serde_json::json!({
                "healthy": false,
                "reason": "no_index",
                "issue": format!("No index found at {}. Run: code-graph-mcp incremental-index", db_path.display()),
                "nodes": 0,
                "edges": 0,
                "files": 0,
                "watching": false,
                "db_size_bytes": 0,
                "search_mode": "fts_only",
                "embedding_progress": "0/0",
                "embedding_coverage_pct": 0,
                "embedding_status": "unavailable",
                "model_available": cfg!(feature = "embed-model"),
                "snapshot": {"status": "absent", "source_url": null, "source_commit": null, "fetched_at": null, "commit_drift": null},
            });
            println!("{}", serde_json::to_string(&payload)?);
            return Ok(());
        }
    }
    // A corrupt index used to be invisible HERE: the reader open deleted the
    // file and retried on a blank one, so this command reported an empty index
    // (and `quick_check: ok`, on the replacement) while the user's symbols were
    // gone. Readers no longer delete, so the open fails — and this is the one
    // command whose whole job is to say what is wrong with the index, so it
    // renders that as its normal corrupt verdict instead of an opaque error.
    // Same `issue` wording and same `integrity.quick_check` shape as a
    // quick_check failure below, so doctor's `index-corrupt` repair routes off
    // it unchanged.
    let ctx = match CliContext::open(project_root) {
        Ok(c) => c,
        Err(e) if Database::is_corrupt_index_error(&e) => {
            let detail = e.to_string();
            if format == "json" {
                let payload = serde_json::json!({
                    "healthy": false,
                    // `reason` mirrors the `no_index` short-circuit above: a
                    // machine-readable tag so consumers route without grepping
                    // prose. `schema_version` is null and PRESENT rather than
                    // omitted — the database cannot be opened, so the version is
                    // genuinely unknown, and doctor's payload sniffer keys off
                    // this field's existence.
                    "reason": "corrupt",
                    "schema_version": null,
                    "issue": detail,
                    "integrity": {"quick_check": detail, "fts_drift": null, "orphan_vectors": null},
                    "nodes": 0, "edges": 0, "files": 0,
                    "watching": false,
                    "db_size_bytes": 0,
                    "search_mode": "fts_only",
                    "embedding_progress": "0/0",
                    "embedding_coverage_pct": 0,
                    "embedding_status": "unavailable",
                    "model_available": cfg!(feature = "embed-model"),
                    "snapshot": {"status": "absent", "source_url": null, "source_commit": null, "fetched_at": null, "commit_drift": null},
                });
                println!("{}", serde_json::to_string(&payload)?);
            } else {
                eprintln!("UNHEALTHY: {}", detail);
            }
            std::process::exit(1);
        }
        Err(e) => return Err(e),
    };
    // The reader open above is non-destructive: if the on-disk index was built by
    // an older INDEX_VERSION, the data is intact but a rebuild is owed. Report it
    // rather than (as before) silently wiping it on this status poll.
    let index_version_stale = ctx.db.index_version_stale();
    let conn = ctx.db.conn();
    let status = queries::get_index_status(conn, false)?;

    let expected_schema = crate::storage::schema::SCHEMA_VERSION;
    let schema_ok = status.schema_version == expected_schema;
    let has_data = status.nodes_count > 0 && status.files_count > 0;
    // DB-1: `healthy` used to mean only "right schema, non-empty". A database
    // whose pages no longer read back reported OK right up until a query hit the
    // damaged page.
    let integrity = IndexIntegrity::probe(conn, status.db_size_bytes.max(0) as u64, deep);
    let healthy = schema_ok && has_data && integrity.corruption_reason().is_none();

    // Compute index age from last_indexed_at (unix timestamp in seconds)
    let age_str = status.last_indexed_at.map(|ts| {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64 - ts)
            .unwrap_or(0);
        if elapsed < 60 {
            format!("{}s ago", elapsed)
        } else if elapsed < 3600 {
            format!("{}m ago", elapsed / 60)
        } else if elapsed < 86400 {
            format!("{}h ago", elapsed / 3600)
        } else {
            format!("{}d ago", elapsed / 86400)
        }
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
    let search_mode = if model_available && vectors_done > 0 {
        "hybrid"
    } else {
        "fts_only"
    };
    let embedding_status = if !model_available {
        "unavailable"
    } else if vectors_done == 0 {
        "pending"
    } else if vectors_done >= vectors_total && vectors_total > 0 {
        "complete"
    } else {
        "partial"
    };
    // Last model-download outcome. Without it, `pending` printed the same
    // optimistic "retry shortly" forever — a permanently-degraded install was
    // indistinguishable from one that just hadn't finished (issue #35).
    #[cfg(feature = "embed-model")]
    let model_download: Option<String> =
        crate::embedding::model::EmbeddingModel::download_state_summary();
    #[cfg(not(feature = "embed-model"))]
    let model_download: Option<String> = None;
    // On-disk model presence, independent of the download marker (the npm
    // plugin installs weights without writing it). Shared by the text arm's
    // pending message and the JSON arm — doctor.js classifies from the JSON,
    // so leaving the field out re-created the "NO download has ever been
    // attempted" contradiction one surface over (ISSUE-011's sibling).
    #[cfg(feature = "embed-model")]
    let model_files_state = crate::embedding::model::EmbeddingModel::model_files_state();
    #[cfg(not(feature = "embed-model"))]
    let model_files_state = "absent";
    // `present` stays the coarse "something is on disk" bool the plugin already
    // reads; `state` says whether this build will actually use it. Advice keyed
    // on `present` alone told an offline user who hand-filled the platform cache
    // to restart the MCP server — a restart that re-downloads instead (review
    // NOTE-7).
    let model_files_present = model_files_state != "absent";

    // Snapshot metadata block — reads keys written by `snapshot install`.
    let snapshot_url =
        crate::snapshot::meta::read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_SOURCE_URL)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
    let snapshot_commit =
        crate::snapshot::meta::read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_SOURCE_COMMIT)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
    let snapshot_fetched_at =
        crate::snapshot::meta::read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_FETCHED_AT)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok());
    let snapshot_status = if snapshot_url.is_some() {
        "present"
    } else {
        "absent"
    };
    // commit_drift: how many local commits landed after the snapshot was taken.
    let commit_drift = snapshot_commit.as_deref().and_then(|c| {
        std::process::Command::new("git")
            // `--` closes the revision list, same as the `ls-files` sibling at
            // :2832 which carries this comment already. Not exploitable here —
            // argv form, and `{c}` is a 40-hex commit id read from the snapshot
            // meta — but a commit-ish that git could read as a pathspec would
            // otherwise change what this counts, and the two call sites in one
            // file disagreeing is how the next one gets written without it.
            .args(["rev-list", "--count", &format!("{c}..HEAD"), "--"])
            .current_dir(project_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<i64>()
                        .ok()
                } else {
                    None
                }
            })
    });
    let snapshot_block = serde_json::json!({
        "status": snapshot_status,
        "source_url": snapshot_url,
        "source_commit": snapshot_commit,
        "fetched_at": snapshot_fetched_at,
        "commit_drift": commit_drift,
    });

    // Graph-resolution coverage (pending backlog + per-language edge counts).
    // .ok() so a stats failure never breaks the existing health-check contract.
    let resolution = queries::resolution_stats(conn).ok();

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
                "snapshot": snapshot_block,
                "conversion_metric": recommendation_metric_state(project_root),
                "index_version_stale": index_version_stale.is_some(),
                "integrity": integrity.to_json(),
            });
            // Additive field: absent when no download was ever recorded, which
            // is itself the "never attempted" diagnosis.
            if let Some(ref s) = model_download {
                json["model_download"] = serde_json::json!(s);
            }
            json["model_files_present"] = serde_json::json!(model_files_present);
            json["model_files_state"] = serde_json::json!(model_files_state);
            if let Some(ref r) = resolution {
                json["resolution"] = serde_json::to_value(r).unwrap_or(serde_json::Value::Null);
            }
            if let Some(ts) = status.last_indexed_at {
                json["last_indexed_at"] = serde_json::json!(ts);
            }
            if let Some(ref age) = age_str {
                json["index_age"] = serde_json::json!(age);
            }
            // Corruption outranks the other diagnoses: a bad page makes the
            // schema/emptiness verdicts unreliable in the first place.
            if let Some(reason) = integrity.corruption_reason() {
                json["issue"] = serde_json::json!(format!(
                    "database integrity check failed: {}. The index is a rebuildable cache — run: \
                     code-graph-mcp rebuild-index --confirm",
                    reason
                ));
            } else if !schema_ok {
                json["issue"] = serde_json::json!(format!(
                    "schema version mismatch: got {}, expected {}",
                    status.schema_version, expected_schema
                ));
            } else if !has_data {
                json["issue"] = serde_json::json!("index is empty");
            } else if let Some(old) = index_version_stale {
                // Has data + correct schema, but built by an older extractor
                // generation. Usable now (FTS/AST), but results sharpen after a
                // rebuild — which an indexer (reindex / incremental-index / server
                // startup), not this poll, performs.
                json["issue"] = serde_json::json!(format!(
                    "index built by older version (v{} ≠ v{}); rebuild pending",
                    old,
                    crate::domain::INDEX_VERSION
                ));
            }
            println!("{}", json);
            if !healthy {
                std::process::exit(1);
            }
        }
        _ => {
            // Print resolution coverage regardless of healthy, mirroring the JSON arm
            // which attaches the block unconditionally (F12). Healthy keeps `OK:` first.
            let print_resolution = || {
                if let Some(ref r) = resolution {
                    let summary: Vec<String> = r
                        .edges_by_language
                        .iter()
                        .map(|(lang, rels)| format!("{} {}", lang, rels.values().sum::<i64>()))
                        .collect();
                    println!(
                        "Resolution: {} pending; edges by lang: {}",
                        r.pending_unresolved_calls,
                        summary.join(", ")
                    );
                }
            };
            if healthy {
                let age_info = age_str
                    .map(|a| format!(" (updated {})", a))
                    .unwrap_or_default();
                println!(
                    "OK: {} nodes, {} edges, {} files{}",
                    status.nodes_count, status.edges_count, status.files_count, age_info
                );
                println!("Snapshot: {}", snapshot_status);
                println!(
                    "Conversion metric: {}",
                    match recommendation_metric_state(project_root) {
                        "live" => "live (recommendations recorded)",
                        "empty" => "active, no recommendations recorded yet",
                        _ =>
                            "DARK (no recommendations.jsonl — PreToolUse hooks not recording here)",
                    }
                );
                // Vector/embedding status — make a silent FTS5-only degradation visible
                // (the prior gap: text health-check never surfaced search_mode, so a user
                // whose model download failed had no way to see vector was inactive).
                // Model files can be on disk without any download marker (the
                // npm plugin installs them out-of-band) — claiming "no download
                // has been attempted" then contradicts the filesystem. Presence
                // is probed once above (shared with the JSON arm); the marker
                // only disambiguates the truly-absent case ("never attempted"
                // vs "attempted and failed").
                let pending_detail = if model_files_state == "ready" {
                    "model files present but not loaded in this process — vector \
                     search activates in the MCP server (embeddings backfill there)"
                        .to_string()
                } else if model_files_present {
                    // Weights are in the platform cache but carry no current
                    // `.model-id` marker, so the server will re-download rather
                    // than adopt them. Saying "restart" here would send an offline
                    // user through a restart that cannot succeed.
                    "model files are on disk in the cache dir but are not verified as \
                     this build's pinned weights — the MCP server re-downloads them on \
                     next start (needs network). To use hand-placed weights offline, \
                     point CODE_GRAPH_MODEL_DIR at them instead"
                        .to_string()
                } else {
                    match model_download.as_deref() {
                        // "never attempted" is itself actionable — it means the
                        // background download never even started, which is a
                        // different bug from one that started and failed.
                        None => "model not loaded yet; no download has been attempted on this \
                                 machine — start the MCP server, or set CODE_GRAPH_MODEL_DIR to \
                                 a manually populated model dir"
                            .to_string(),
                        Some(s) => format!("model not loaded yet; last download: {}", s),
                    }
                };
                println!(
                    "Search: {} — {}% embedded ({})",
                    // Names the SURFACE. `search_mode` describes what the index can
                    // support, which is reached through MCP `semantic_code_search`;
                    // this binary's own `search` subcommand is FTS5-only whatever
                    // the mode says. Reading "Search: hybrid (FTS5 + vector)" out of
                    // a CLI command and then getting FTS-only ranking from
                    // `code-graph-mcp search` on the same machine is the kind of
                    // mismatch a user has no way to diagnose (2026-08-16 audit §四).
                    if search_mode == "hybrid" {
                        "hybrid (FTS5 + vector) via MCP; CLI `search` is FTS5-only"
                    } else {
                        "FTS5-only (vector inactive)"
                    },
                    coverage_pct,
                    match embedding_status {
                        "unavailable" => "binary built without embed-model feature".to_string(),
                        "pending" => pending_detail,
                        "partial" => "embedding in progress".to_string(),
                        "complete" => "embeddings complete".to_string(),
                        other => other.to_string(),
                    }
                );
                println!("{}", integrity.to_line());
                // DB-3: the JSON face has reported `issue: "…rebuild pending"`
                // for a version-lagging index since it was added, while this one
                // printed a bare "OK" — the same command telling a human and a
                // script opposite things about the same database.
                if let Some(old) = index_version_stale {
                    println!(
                        "Index version: STALE (built by v{} ≠ v{}); results sharpen after a \
                         rebuild — run: code-graph-mcp reindex",
                        old,
                        crate::domain::INDEX_VERSION
                    );
                }
                print_resolution();
            } else if let Some(reason) = integrity.corruption_reason() {
                eprintln!(
                    "UNHEALTHY: database integrity check failed: {}. The index is a rebuildable \
                     cache — run: code-graph-mcp rebuild-index --confirm",
                    reason
                );
                eprintln!("{}", integrity.to_line());
                print_resolution();
                std::process::exit(1);
            } else if !schema_ok {
                eprintln!(
                    "UNHEALTHY: schema version mismatch (got {}, expected {})",
                    status.schema_version, expected_schema
                );
                eprintln!("{}", integrity.to_line());
                print_resolution();
                std::process::exit(1);
            } else {
                eprintln!("UNHEALTHY: index is empty");
                eprintln!("{}", integrity.to_line());
                print_resolution();
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
