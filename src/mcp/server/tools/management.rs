//! Management tools: `start_watch`, `stop_watch`, `get_index_status`, `rebuild_index`.
//! Hidden from `tools/list` (call by name via raw JSON-RPC or CLI subcommand).

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_start_watch(&self) -> Result<serde_json::Value> {
        if !self.is_primary() {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. File watching is handled by the primary instance."
            }));
        }
        let project_root = self
            .project_root
            .as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if watcher_guard.is_some() {
            return Ok(json!({
                "status": "already_watching",
                "message": "File watcher is already running"
            }));
        }

        let (tx, rx) = mpsc::sync_channel(crate::indexer::watcher::WATCHER_CHANNEL_BOUND);
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

    pub(in crate::mcp::server) fn tool_stop_watch(&self) -> Result<serde_json::Value> {
        if !self.is_primary() {
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

    pub(in crate::mcp::server) fn tool_get_index_status(&self) -> Result<serde_json::Value> {
        let mut status = serde_json::to_value(queries::get_index_status(
            self.db.conn(),
            self.is_watching(),
        )?)?;

        // Add embedding status fields
        let model_available = lock_or_recover(&self.embedding_model, "embedding_model").is_some();
        let (vectors_done, vectors_total) = if self.db.vec_enabled() {
            queries::count_nodes_with_vectors(self.db.conn()).unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        let embedding_status = if !model_available {
            "unavailable"
        } else if self.indexing.embedding_in_progress.load(Ordering::Acquire) {
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
            obj.insert(
                "embedding_progress".into(),
                json!(format!("{}/{}", vectors_done, vectors_total)),
            );
            obj.insert("model_available".into(), json!(model_available));
            // coverage_pct: integer for status-at-a-glance; avoid rounding to 0 when
            // real progress exists (embedding_status == "in_progress" with small fraction)
            // so callers don't mistake "making progress" for "stuck at zero".
            let coverage_pct = if vectors_total > 0 {
                let ratio = vectors_done as f64 / vectors_total as f64;
                let rounded = (ratio * 100.0).round() as i64;
                if rounded == 0 && vectors_done > 0 {
                    1
                } else {
                    rounded
                }
            } else {
                0
            };
            obj.insert("embedding_coverage_pct".into(), json!(coverage_pct));
            obj.insert(
                "search_mode".into(),
                json!(if model_available && vectors_done > 0 {
                    "hybrid"
                } else {
                    "fts_only"
                }),
            );

            // Add indexing observability stats (skipped files, truncations)
            let stats = lock_or_recover(&self.last_index_stats, "last_index_stats").clone();
            let skipped_total = stats.files_skipped_size
                + stats.files_skipped_parse
                + stats.files_skipped_read
                + stats.files_skipped_hash;
            if skipped_total > 0 {
                obj.insert(
                    "skipped_files".into(),
                    json!({
                        "total": skipped_total,
                        "too_large": stats.files_skipped_size,
                        "parse_error": stats.files_skipped_parse,
                        "read_error": stats.files_skipped_read,
                        "hash_error": stats.files_skipped_hash,
                    }),
                );
            }
            if stats.files_skipped_language > 0 {
                obj.insert(
                    "files_skipped_unsupported_language".into(),
                    json!(stats.files_skipped_language),
                );
            }
            // The counterpart to `skipped_files.parse_error` above, and the more
            // dangerous of the two: a file that failed to parse outright is
            // skipped and visibly absent, but a file that parsed WITH syntax
            // errors had its symbols salvaged by tree-sitter error recovery and
            // sits in the index looking exactly like a clean one. The CLI has
            // disclosed this count since the counter existed (index_ops.rs: the
            // stderr summary and `--json` both carry it); the MCP surface had no
            // way to say it, so an agent could not tell that a thin result set
            // came from a broken file rather than from the code really being
            // thin. Zero stays silent, like every counter around it —
            // `last_index_stats` is per-process, so a server that started
            // against a fresh index holds zeros it never earned.
            if stats.files_with_parse_errors > 0 {
                obj.insert(
                    "files_with_parse_errors".into(),
                    json!(stats.files_with_parse_errors),
                );
            }
            obj.insert(
                "instance_mode".into(),
                json!(if self.is_primary() {
                    "primary"
                } else {
                    "secondary"
                }),
            );

            // Health and age fields (consistent with CLI health-check)
            let expected_schema = crate::storage::schema::SCHEMA_VERSION;
            let schema_ok = obj
                .get("schema_version")
                .and_then(|v| v.as_i64())
                .map(|v| v == expected_schema as i64)
                .unwrap_or(false);
            let has_data = obj
                .get("nodes_count")
                .and_then(|v| v.as_i64())
                .map(|v| v > 0)
                .unwrap_or(false);
            obj.insert("healthy".into(), json!(schema_ok && has_data));
            if let Some(ts) = obj.get("last_indexed_at").and_then(|v| v.as_i64()) {
                let elapsed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64 - ts)
                    .unwrap_or(0);
                let age = if elapsed < 60 {
                    format!("{}s ago", elapsed)
                } else if elapsed < 3600 {
                    format!("{}m ago", elapsed / 60)
                } else if elapsed < 86400 {
                    format!("{}h ago", elapsed / 3600)
                } else {
                    format!("{}d ago", elapsed / 86400)
                };
                obj.insert("index_age".into(), json!(age));
            }
        }

        Ok(status)
    }

    pub(in crate::mcp::server) fn tool_rebuild_index(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        if !self.is_primary() {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. Rebuild must be done from the primary instance."
            }));
        }
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        if !confirm {
            return Err(anyhow!("Must pass confirm: true to rebuild index"));
        }

        let project_root = self
            .project_root
            .as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        // Wait for background embedding to finish before clearing data
        // to avoid race where embedding thread writes vectors for deleted nodes.
        // Returning Ok({status:"busy"}) rather than Err matches
        // `run_incremental_with_cache_restore`'s precedent and keeps the
        // usage-metrics error counter from inflating on legitimate retry signals.
        // 30s accommodates larger projects whose embedding pass exceeds 10s;
        // historical usage.jsonl showed rebuild_index max_ms=10009 across 5/9
        // calls — a deadline cliff, not a real failure mode.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while self.indexing.embedding_in_progress.load(Ordering::Acquire) {
            if std::time::Instant::now() > deadline {
                return Ok(json!({
                    "status": "busy",
                    "message": "Background embedding still in progress. Retry in a few seconds.",
                    "retry_after_ms": 2000,
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        self.send_log("info", "Rebuilding index...");
        let progress_cb =
            |_phase: crate::indexer::pipeline::IndexPhase, current: usize, total: usize| {
                self.send_progress("rebuild-index", current, total);
            };

        // Atomic rebuild: the DELETE and the full re-index run inside ONE outer
        // transaction. In WAL mode, external fresh-connection readers (a CLI `grep`,
        // a secondary MCP instance) keep seeing the last committed — old, complete —
        // index until this commits, never the empty/partial mid-rebuild window the
        // old "DELETE-commit then rebuild in place" left open for the whole (multi-
        // second) rebuild; and a failure ANYWHERE below rolls the DELETE back too,
        // leaving the old index intact instead of wiped. run_full_index's phase
        // transactions are SAVEPOINTs (see index_files), so they nest inside this
        // BEGIN rather than erroring with "cannot start a transaction within a
        // transaction". (The CLI's temp-file + atomic-rename swap can't be reused
        // here: this server holds a persistent self.db connection whose fd would keep
        // pointing at the old unlinked inode after a rename — see the snapshot-install
        // inode-swap note in server/mod.rs. So we get atomicity from one transaction
        // on the live connection instead.)
        let result = {
            // `write_db()` is `self.db` for a normal primary and the read-write
            // handle opened at promotion for an instance that started as a
            // secondary — `self.db` is read-only in that case and every write
            // below would fail with SQLITE_READONLY.
            let write_db = self.write_db();
            let tx = write_db.conn().unchecked_transaction()?;
            tx.execute("DELETE FROM files", [])?; // CASCADE handles nodes→edges
                                                  // Skip inline embedding; the background thread (spawned below) handles it.
            let result = run_full_index(&write_db, project_root, None, Some(&progress_cb))?;
            tx.commit()?;
            result
        };

        // Save indexing stats for observability
        *lock_or_recover(&self.last_index_stats, "last_index_stats") = result.stats.clone();

        // Reset indexed flag and invalidate caches
        *lock_or_recover(&self.indexed, "indexed") = true;
        *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
        lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();

        self.spawn_background_embedding();

        Ok(json!({
            "status": "rebuilt",
            "files_indexed": result.files_indexed,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
            // Parity with the CLI's index envelope (emit_index_json), which has
            // carried this since the counter existed. Emitted UNCONDITIONALLY,
            // unlike the get_index_status copy: the rebuild ran in-band right
            // here, so 0 genuinely means "this run hit no syntax errors" rather
            // than "nobody indexed anything yet", and omitting it would leave the
            // caller unable to tell a clean rebuild from a surface that cannot
            // report.
            "files_with_parse_errors": result.stats.files_with_parse_errors,
        }))
    }
}
