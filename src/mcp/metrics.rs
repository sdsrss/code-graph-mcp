use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use crate::utils::telemetry::{
    iso8601_now, rotate_jsonl_if_over, JSONL_ROTATE_KEEP_BYTES, JSONL_ROTATE_MAX_BYTES,
};

/// Canonical error categories for tool invocations. Written to usage.jsonl
/// under `tools.<name>.err_kinds` so post-hoc analysis can separate real bugs
/// from startup-grace retries, user typos, and ambiguous-symbol guards without
/// re-classifying each error string by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrKind {
    /// 2s grace timeout in ensure_indexed while startup indexing runs.
    Timeout,
    /// User-supplied symbol or node_id not present in the index.
    NotFound,
    /// Multiple symbols with same name; needs file_path/node_id disambiguation.
    Ambiguous,
    /// SQLite FOREIGN KEY violation — DB state inconsistent. Rare; indicates bug.
    FkConstraint,
    /// Missing/empty required input (empty query, missing required param).
    EmptyInput,
    /// Invalid parameter VALUE — bad enum ("must be one of"), a mutually-exclusive
    /// combination, or an unknown filter. Distinct from EmptyInput (missing) and
    /// Other (truly unexpected). A high count is a model-misuse signal: the tool
    /// schema/description should make the valid values clearer.
    BadParam,
    /// Unclassified — expand classify() if this bucket grows.
    Other,
}

impl ErrKind {
    /// Classify an error message via substring match on known error phrases.
    /// Match order matters: FK check first (most specific), then grace, etc.
    ///
    /// EVERY PHRASE BELOW MUST BE MULTI-WORD. That is a load-bearing premise, not
    /// a coincidence: these messages echo caller-supplied values, so a phrase a
    /// caller could spell in a single token would let it pick its own telemetry
    /// bucket. `describe_arg` bounds the echo to one whitespace-delimited token,
    /// which closes the hole only for as long as no single-word phrase exists
    /// here. Adding `"timeout"` or `"ambiguous"` would silently re-open it —
    /// `every_classify_phrase_is_multi_word` fails the moment you do, rather than
    /// at the next audit. (Reordering these arms is NOT an alternative fix: it
    /// relocates which side is exposed, it does not close anything.)
    pub fn classify(err_msg: &str) -> Self {
        if err_msg.contains("FOREIGN KEY constraint failed") {
            Self::FkConstraint
        } else if err_msg.contains("Indexing in progress") || err_msg.contains("retry your request")
        {
            Self::Timeout
        } else if err_msg.contains("Ambiguous symbol") {
            Self::Ambiguous
        } else if err_msg.contains("not found in index")
            || err_msg.contains("not found in the index")
        {
            Self::NotFound
        } else if err_msg.contains("must be one of")
            || err_msg.contains("mutually exclusive")
            || err_msg.contains("Unknown relation filter")
            // CON-15's numeric half. Without these three the type rejections land
            // in `Other`, and the bucket whose whole job is "the model is calling
            // this tool wrong" would miss exactly the misuse that was just made
            // visible.
            || err_msg.contains("must be an integer")
            || err_msg.contains("must be a non-negative integer")
            || err_msg.contains("must be a number")
        {
            // Invalid parameter VALUE (bad enum / bad combination). Kept ahead of
            // EmptyInput so a wrong value isn't mistaken for a missing one.
            Self::BadParam
        } else if err_msg.contains("must not be empty")
            || err_msg.contains("Must pass")
            || err_msg.contains("is required") // missing required param
            // NOTE: classify() only runs on MCP tool-call errors (handle_tool in
            // server/mod.rs). MCP handlers emit "must not be empty"/"Must pass"; the
            // CLI's "Usage:" errors go anyhow→main()→exit, never through here. So the
            // CLI clap migration (audit #4) does NOT change usage.jsonl bucketing — the
            // "Usage:" arm below is defensive only and never fires in the MCP path.
            || err_msg.starts_with("Usage:")
        {
            Self::EmptyInput
        } else {
            Self::Other
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::FkConstraint => "fk",
            Self::EmptyInput => "empty_input",
            Self::BadParam => "bad_param",
            Self::Other => "other",
        }
    }
}

/// Per-tool call statistics.
pub struct ToolStats {
    pub count: u64,
    pub total_ms: u64,
    pub errors: u64,
    pub max_ms: u64,
    /// Breakdown of `errors` by ErrKind::as_str(). Empty when `errors == 0`.
    pub err_kinds: HashMap<String, u64>,
    /// One representative message for the catch-all `other` bucket (first-wins,
    /// first line, truncated). `None` until an `Other`-classified error occurs.
    /// Makes a large `other` count self-explaining in `stats` — classified kinds
    /// (not_found/ambiguous/…) are self-descriptive by name and get no sample.
    pub other_sample: Option<String>,
}

/// Aggregated search metrics for the session.
pub struct SearchMetrics {
    pub total_queries: u64,
    pub zero_results: u64,
    pub quality_sum: f64,
    pub fts_only: u64,
    pub hybrid: u64,
}

/// Lightweight session metrics — append-only JSONL flush at session end.
pub struct SessionMetrics {
    start: Instant,
    /// Wallclock session-start (ISO 8601 UTC), captured at construction. Used to
    /// window-join PreToolUse recommendations (`recommendations.jsonl`) that fired
    /// during this session, so the recommend→use funnel can attribute per-session.
    started_at: String,
    tools: HashMap<String, ToolStats>,
    search: SearchMetrics,
    pub full_index_ms: Option<u64>,
    pub incremental_count: u64,
    pub files_indexed: u64,
    pub nodes_created: u64,
}

impl Default for SessionMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionMetrics {
    /// Create a new empty session.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            started_at: iso8601_now(),
            tools: HashMap::new(),
            search: SearchMetrics {
                total_queries: 0,
                zero_results: 0,
                quality_sum: 0.0,
                fts_only: 0,
                hybrid: 0,
            },
            full_index_ms: None,
            incremental_count: 0,
            files_indexed: 0,
            nodes_created: 0,
        }
    }

    /// Record a tool invocation. `err_kind = None` means success. `err_msg` is
    /// the raw error string (used only to sample the `other` bucket; pass `None`
    /// on success or when no message is available).
    pub fn record_tool_call(
        &mut self,
        name: &str,
        elapsed_ms: u64,
        err_kind: Option<ErrKind>,
        err_msg: Option<&str>,
    ) {
        let stats = self.tools.entry(name.to_string()).or_insert(ToolStats {
            count: 0,
            total_ms: 0,
            errors: 0,
            max_ms: 0,
            err_kinds: HashMap::new(),
            other_sample: None,
        });
        stats.count += 1;
        stats.total_ms += elapsed_ms;
        if let Some(kind) = err_kind {
            stats.errors += 1;
            *stats.err_kinds.entry(kind.as_str().into()).or_insert(0) += 1;
            // Sample the catch-all `other` bucket (first-wins) so a large `other`
            // count is self-explaining in `stats` without re-deriving it from
            // source. First line only, truncated — usage.jsonl is local + gitignored.
            if kind == ErrKind::Other && stats.other_sample.is_none() {
                if let Some(msg) = err_msg {
                    let line = msg.lines().next().unwrap_or(msg);
                    stats.other_sample = Some(line.chars().take(160).collect());
                }
            }
        }
        if elapsed_ms > stats.max_ms {
            stats.max_ms = elapsed_ms;
        }
    }

    /// Record a search query result.
    pub fn record_search(&mut self, result_count: usize, quality: f64, is_fts_only: bool) {
        self.search.total_queries += 1;
        if result_count == 0 {
            self.search.zero_results += 1;
        }
        self.search.quality_sum += quality;
        if is_fts_only {
            self.search.fts_only += 1;
        } else {
            self.search.hybrid += 1;
        }
    }

    /// Record an indexing operation.
    pub fn record_index(&mut self, files: u64, nodes: u64, is_full: bool, elapsed_ms: u64) {
        self.files_indexed += files;
        self.nodes_created += nodes;
        if is_full {
            self.full_index_ms = Some(elapsed_ms);
        } else {
            self.incremental_count += 1;
        }
    }

    /// True if no tool calls were recorded (skip flush for empty sessions).
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// True if any recommendation event fired inside this session's window.
    /// Lets `flush_metrics` write a usage record for 0-tool-call sessions that
    /// still saw deny/hint/bypass/cli-use traffic — without this, the funnel
    /// denominator only ever contains sessions that already converted (the
    /// 2026-06-12 daagu night: 53 recs, 0 MCP calls, zero usage records).
    pub fn has_recs_in_window(&self, cg_dir: &Path) -> bool {
        let Ok(content) = std::fs::read_to_string(cg_dir.join("recommendations.jsonl")) else {
            return false;
        };
        let c = count_recs_in_window(&content, &self.started_at);
        c.deny > 0 || c.hint > 0 || c.cli_use > 0 || c.bypass > 0
    }

    /// Build the one-line JSON record for this session. Separated from `flush`
    /// so the `dogfood` tagging + field shape are unit-testable without env/FS
    /// races. `dogfood=true` segregates dev self-test traffic from real Claude
    /// Code usage in usage.jsonl so adoption can be measured (audit §7).
    fn build_record(&self, version: &str, dogfood: bool) -> serde_json::Value {
        let dur_s = self.start.elapsed().as_secs();
        let ts = iso8601_now();

        // Build tools map. `err_kinds` is additive — older readers ignore it;
        // we only emit when non-empty to keep lines compact for success-only sessions.
        let tools_json: serde_json::Map<String, serde_json::Value> = self
            .tools
            .iter()
            .map(|(name, stats)| {
                let mut obj = serde_json::json!({
                    "n": stats.count,
                    "ms": stats.total_ms,
                    "err": stats.errors,
                    "max_ms": stats.max_ms,
                });
                if !stats.err_kinds.is_empty() {
                    obj["err_kinds"] = serde_json::json!(stats.err_kinds);
                }
                // Additive — present only when an `other`-bucket error was sampled.
                if let Some(sample) = &stats.other_sample {
                    obj["other_sample"] = serde_json::json!(sample);
                }
                (name.clone(), obj)
            })
            .collect();

        let avg_quality = if self.search.total_queries > 0 {
            ((self.search.quality_sum / self.search.total_queries as f64) * 100.0).round() / 100.0
        } else {
            0.0
        };

        let mut record = serde_json::json!({
            "ts": ts,
            "dur_s": dur_s,
            "v": version,
            "tools": tools_json,
            "search": {
                "queries": self.search.total_queries,
                "zero": self.search.zero_results,
                "avg_quality": avg_quality,
                "fts_only": self.search.fts_only,
                "hybrid": self.search.hybrid,
            },
            "index": {
                "full_ms": self.full_index_ms,
                "incr": self.incremental_count,
                "files": self.files_indexed,
                "nodes": self.nodes_created,
            },
        });
        // Additive + only-when-true: older readers ignore it, success lines stay compact.
        if dogfood {
            record["dogfood"] = serde_json::json!(true);
        }
        record
    }

    /// Serialize session metrics to one-line JSON and append to the usage file.
    /// Performs size-based rotation: if file > 1MB, truncate to last 512KB.
    pub fn flush(&self, usage_path: &Path, version: &str) {
        // CODE_GRAPH_DOGFOOD=1 tags this session as dev self-test traffic so it
        // can be filtered out of real-adoption metrics (audit §7).
        let dogfood = std::env::var("CODE_GRAPH_DOGFOOD").ok().as_deref() == Some("1");
        let mut record = self.build_record(version, dogfood);

        // Window-join PreToolUse recommendations that fired during this session so
        // the recommend→use funnel can attribute per-session (P#5211). Additive +
        // only-when-non-zero: older readers ignore it, success lines stay compact.
        if let Some(dir) = usage_path.parent() {
            if let Ok(content) = std::fs::read_to_string(dir.join("recommendations.jsonl")) {
                let c = count_recs_in_window(&content, &self.started_at);
                if c.deny > 0 || c.hint > 0 || c.cli_use > 0 || c.bypass > 0 {
                    // deny/hint always present (v0.46 shape); cli_use/bypass
                    // additive, emitted only when non-zero.
                    let mut recs = serde_json::json!({ "deny": c.deny, "hint": c.hint });
                    if c.cli_use > 0 {
                        recs["cli_use"] = serde_json::json!(c.cli_use);
                    }
                    if c.bypass > 0 {
                        recs["bypass"] = serde_json::json!(c.bypass);
                    }
                    record["recs"] = recs;
                }
            }
        }

        let line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to serialize session metrics: {}", e);
                return;
            }
        };

        // Ensure parent directory exists. `ensure_owned_dir` rather than
        // `create_dir_all`: the latter silently succeeds when `.code-graph` is a
        // symlink to somewhere else, which is how the whole data directory could
        // be relocated outside the project root (audit 2026-08-29 SEC-03).
        if let Some(parent) = usage_path.parent() {
            if let Err(e) = crate::utils::owned::ensure_owned_dir(parent) {
                tracing::warn!("Failed to create metrics directory: {}", e);
                return;
            }
        }

        // Bounded growth: rotate before appending so the file never exceeds
        // ~max + one line. recommendations.jsonl shares this exact policy.
        rotate_jsonl_if_over(usage_path, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);

        // Append the line — refusing a symlink planted at `usage.jsonl`, the
        // same guard `record_cli_use` applies to its sibling telemetry file.
        match crate::utils::owned::append_owned(usage_path) {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{}", line) {
                    tracing::warn!("Failed to write session metrics: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to open usage file: {}", e);
            }
        }
    }
}

/// Per-window recommendation-event counts (see `count_recs_in_window`).
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct RecWindowCounts {
    pub(crate) deny: u64,
    pub(crate) hint: u64,
    /// Model-initiated `code-graph-mcp <query>` runs (action:"use", recorded by
    /// the CLI itself; hook-internal answer runs set CODE_GRAPH_INTERNAL=1 and
    /// are never recorded).
    pub(crate) cli_use: u64,
    pub(crate) bypass: u64,
}

/// Count recommendation events (`recommendations.jsonl` content) whose `ts`
/// falls inside the current session window `[started_at, ∞)`. Pure: ISO-8601
/// UTC strings compare lexicographically at second granularity (a sub-second
/// boundary event may be off by one — accepted per spec). Unknown actions are
/// ignored so future hook vocabularies stay additive.
pub(crate) fn count_recs_in_window(rec_content: &str, started_at: &str) -> RecWindowCounts {
    let mut c = RecWindowCounts::default();
    for line in rec_content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("");
        if ts < started_at {
            continue;
        }
        match v.get("action").and_then(|a| a.as_str()) {
            Some("deny") => c.deny += 1,
            Some("hint") => c.hint += 1,
            Some("use") => c.cli_use += 1,
            Some("bypass") => c.bypass += 1,
            _ => {}
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    /// The premise `describe_arg`'s bounded echo rests on (pre-tag review of
    /// v0.129.0, then a reviewer's follow-up: "every phrase is multi-word" is a
    /// PREMISE, not an incidental property).
    ///
    /// Error text echoes caller-supplied values; the echo is bounded to one
    /// whitespace-delimited token; therefore no caller can spell a classify
    /// phrase from a value position — but only while every phrase needs a space.
    /// A future single-word phrase re-opens the injection silently, and the
    /// behavioural guard over in `server/mod.rs` would keep passing, because it
    /// asserts the phrases that exist rather than the invariant protecting them.
    ///
    /// Reads the phrases out of `classify`'s own source rather than restating
    /// them: a copy here would go stale exactly when a phrase is added, which is
    /// the one moment this needs to fire. The floor makes an empty scan fail.
    #[test]
    fn every_classify_phrase_is_multi_word() {
        let src = include_str!("metrics.rs");
        let body = src
            .split_once("pub fn classify(err_msg: &str) -> Self {")
            .expect("classify's signature moved — re-point this guard, do not delete it")
            .1
            .split_once("\n    }\n")
            .expect("could not find the end of classify")
            .0;

        let phrases: Vec<&str> = body
            .match_indices("err_msg.contains(\"")
            .map(|(i, m)| {
                let rest = &body[i + m.len()..];
                &rest[..rest.find('"').expect("unterminated phrase literal")]
            })
            .collect();

        assert!(
            phrases.len() >= 12,
            "parsed only {} phrase(s) from classify — the guard would be vacuous",
            phrases.len()
        );

        let single_word: Vec<&&str> = phrases.iter().filter(|p| !p.contains(' ')).collect();
        assert!(
            single_word.is_empty(),
            "these classify phrases are a single token: {single_word:?}. Error messages echo \
             caller-supplied values, so a one-token phrase lets a caller choose its own \
             telemetry bucket. Either give the phrase a space, or stop echoing values into \
             the message it matches — do NOT reorder the arms, which only moves the hole."
        );
    }

    /// `usage.jsonl` sits in `.code-graph/` next to `recommendations.jsonl` and
    /// is written with the same `OpenOptions::append` shape, so it carries the
    /// same symlink exposure: a repo-supplied link made every MCP session flush
    /// append into an unrelated file (audit 2026-08-29 SEC-02).
    #[cfg(unix)]
    #[test]
    fn flush_refuses_to_append_through_a_symlinked_usage_file() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path().join(".code-graph");
        std::fs::create_dir(&cg).unwrap();
        let victim = dir.path().join("victim.conf");
        std::fs::write(&victim, "keep = 1\n").unwrap();
        std::os::unix::fs::symlink(&victim, cg.join("usage.jsonl")).unwrap();

        let mut m = SessionMetrics::new();
        m.files_indexed = 1; // non-empty, so the flush actually has work
        m.flush(&cg.join("usage.jsonl"), "0.0.0-test");

        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "keep = 1\n",
            "the link target must not be appended to"
        );

        // Positive control: a regular path in a sibling dir still receives the
        // record, so the assertion above is not green by the flush no-opping.
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let ok_path = plain.join("usage.jsonl");
        m.flush(&ok_path, "0.0.0-test");
        assert!(
            std::fs::read_to_string(&ok_path).unwrap().contains("\"v\""),
            "a regular usage.jsonl must still be written"
        );
    }

    #[test]
    fn test_new_session_is_empty() {
        let m = SessionMetrics::new();
        assert!(m.is_empty());
        assert_eq!(m.files_indexed, 0);
        assert_eq!(m.nodes_created, 0);
        assert!(m.full_index_ms.is_none());
    }

    #[test]
    fn test_count_recs_in_window() {
        let content = "\
{\"ts\":\"2026-06-10T00:00:00Z\",\"hook\":\"grep\",\"action\":\"deny\"}
{\"ts\":\"2026-06-10T01:00:00Z\",\"hook\":\"grep\",\"action\":\"deny\"}
{\"ts\":\"2026-06-10T01:00:05Z\",\"hook\":\"read\",\"action\":\"hint\"}
{\"ts\":\"2026-06-10T01:00:09Z\",\"hook\":\"grep\",\"action\":\"hint\"}
not json
{\"ts\":\"2026-06-10T01:30:00Z\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"callgraph\"}
{\"ts\":\"2026-06-10T01:31:00Z\",\"hook\":\"grep\",\"action\":\"bypass\"}
{\"ts\":\"2026-06-10T02:00:00Z\",\"hook\":\"grep\",\"action\":\"deny\"}
";
        // Window starts at 01:00:00 → the 00:00:00 deny is excluded.
        let c = count_recs_in_window(content, "2026-06-10T01:00:00Z");
        assert_eq!(
            c,
            RecWindowCounts {
                deny: 2,
                hint: 2,
                cli_use: 1,
                bypass: 1
            },
            "in-window counts per action, pre-window excluded, malformed skipped"
        );

        // Window after everything → nothing in range.
        assert_eq!(
            count_recs_in_window(content, "2026-06-10T03:00:00Z"),
            RecWindowCounts::default()
        );
        // Empty content → zero, no panic.
        assert_eq!(
            count_recs_in_window("", "2026-06-10T01:00:00Z"),
            RecWindowCounts::default()
        );
    }

    #[test]
    fn test_has_recs_in_window_gates_empty_session_flush() {
        let tmp = std::env::temp_dir().join(format!("cg-recs-window-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let m = SessionMetrics::new();
        // No recommendations.jsonl at all → false.
        assert!(!m.has_recs_in_window(&tmp));
        // A rec stamped AFTER session start (started_at captured at new()) → true.
        let future_ts = "2999-01-01T00:00:00Z";
        std::fs::write(
            tmp.join("recommendations.jsonl"),
            format!(
                "{{\"ts\":\"{}\",\"hook\":\"grep\",\"action\":\"deny\"}}\n",
                future_ts
            ),
        )
        .unwrap();
        assert!(m.has_recs_in_window(&tmp));
        // Only pre-window recs → false.
        std::fs::write(
            tmp.join("recommendations.jsonl"),
            "{\"ts\":\"2000-01-01T00:00:00Z\",\"hook\":\"grep\",\"action\":\"deny\"}\n",
        )
        .unwrap();
        assert!(!m.has_recs_in_window(&tmp));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_record_tool_call_basic() {
        let mut m = SessionMetrics::new();
        m.record_tool_call("semantic_code_search", 150, None, None);
        assert!(!m.is_empty());
        let stats = m.tools.get("semantic_code_search").unwrap();
        assert_eq!(stats.count, 1);
        assert_eq!(stats.total_ms, 150);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.max_ms, 150);
    }

    #[test]
    fn test_record_tool_call_accumulates() {
        let mut m = SessionMetrics::new();
        m.record_tool_call("get_call_graph", 100, None, None);
        m.record_tool_call("get_call_graph", 200, Some(ErrKind::Other), None);
        m.record_tool_call("get_call_graph", 50, None, None);
        let stats = m.tools.get("get_call_graph").unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.total_ms, 350);
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.max_ms, 200);
        assert_eq!(stats.err_kinds.get("other").copied(), Some(1));
    }

    #[test]
    fn test_record_search_metrics() {
        let mut m = SessionMetrics::new();
        m.record_search(5, 0.85, false);
        m.record_search(0, 0.4, true);
        assert_eq!(m.search.total_queries, 2);
        assert_eq!(m.search.zero_results, 1);
        assert_eq!(m.search.fts_only, 1);
        assert_eq!(m.search.hybrid, 1);
        assert!((m.search.quality_sum - 1.25).abs() < 0.001);
    }

    #[test]
    fn test_record_index_full() {
        let mut m = SessionMetrics::new();
        m.record_index(100, 500, true, 2000);
        assert_eq!(m.files_indexed, 100);
        assert_eq!(m.nodes_created, 500);
        assert_eq!(m.full_index_ms, Some(2000));
        assert_eq!(m.incremental_count, 0);
    }

    #[test]
    fn test_record_index_incremental() {
        let mut m = SessionMetrics::new();
        m.record_index(5, 20, false, 100);
        m.record_index(3, 10, false, 80);
        assert_eq!(m.files_indexed, 8);
        assert_eq!(m.nodes_created, 30);
        assert!(m.full_index_ms.is_none());
        assert_eq!(m.incremental_count, 2);
    }

    #[test]
    fn test_flush_creates_file_with_valid_json() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let mut m = SessionMetrics::new();
        m.record_tool_call("semantic_code_search", 150, None, None);
        m.record_tool_call("get_call_graph", 200, Some(ErrKind::Other), None);
        m.record_search(3, 0.85, false);
        m.record_index(50, 200, true, 1500);
        m.flush(&usage_path, "0.5.26");

        let mut content = String::new();
        std::fs::File::open(&usage_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 1);

        let record: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(record["v"], "0.5.26");
        assert!(record["ts"].as_str().unwrap().contains("T"));
        assert!(record["dur_s"].as_u64().is_some());

        // Verify tools
        assert_eq!(record["tools"]["semantic_code_search"]["n"], 1);
        assert_eq!(record["tools"]["semantic_code_search"]["ms"], 150);
        assert_eq!(record["tools"]["get_call_graph"]["err"], 1);

        // Verify search
        assert_eq!(record["search"]["queries"], 1);
        assert_eq!(record["search"]["hybrid"], 1);

        // Verify index
        assert_eq!(record["index"]["full_ms"], 1500);
        assert_eq!(record["index"]["files"], 50);
        assert_eq!(record["index"]["nodes"], 200);
    }

    #[test]
    fn test_flush_appends_multiple_sessions() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let mut m1 = SessionMetrics::new();
        m1.record_tool_call("project_map", 100, None, None);
        m1.flush(&usage_path, "0.5.26");

        let mut m2 = SessionMetrics::new();
        m2.record_tool_call("get_call_graph", 200, None, None);
        m2.flush(&usage_path, "0.5.26");

        let content = std::fs::read_to_string(&usage_path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        // Both lines should be valid JSON
        serde_json::from_str::<serde_json::Value>(lines[0]).unwrap();
        serde_json::from_str::<serde_json::Value>(lines[1]).unwrap();
    }

    #[test]
    fn test_flush_skipped_when_empty() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let m = SessionMetrics::new();
        assert!(m.is_empty());
        // flush on empty session should not create the file (caller checks is_empty)
        // but flush itself should still work if called directly
        m.flush(&usage_path, "0.5.26");
        // File is created even for empty because flush doesn't check is_empty.
        // The caller (flush_metrics on McpServer) is responsible for the guard.
        let content = std::fs::read_to_string(&usage_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(record["tools"], serde_json::json!({}));
    }

    #[test]
    fn test_flush_rotation_over_1mb() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        // Write > 1MB of data
        let big_line = "x".repeat(1200);
        {
            let mut f = std::fs::File::create(&usage_path).unwrap();
            for _ in 0..1000 {
                writeln!(f, "{}", big_line).unwrap();
            }
        }
        let size_before = std::fs::metadata(&usage_path).unwrap().len();
        assert!(size_before > 1_048_576);

        let mut m = SessionMetrics::new();
        m.record_tool_call("test", 10, None, None);
        m.flush(&usage_path, "0.5.26");

        let size_after = std::fs::metadata(&usage_path).unwrap().len();
        // After rotation, file should be around 512KB + the new line
        assert!(
            size_after < 600_000,
            "File should be rotated down, got {} bytes",
            size_after
        );
        assert!(
            size_after > 500_000,
            "File should retain ~512KB, got {} bytes",
            size_after
        );

        // Last line should be valid JSON from our flush
        let content = std::fs::read_to_string(&usage_path).unwrap();
        let last_line = content.trim().lines().last().unwrap();
        let record: serde_json::Value = serde_json::from_str(last_line).unwrap();
        assert_eq!(record["v"], "0.5.26");
    }

    #[test]
    fn build_record_tags_dogfood_when_flagged() {
        let mut m = SessionMetrics::new();
        m.record_tool_call("get_call_graph", 5, None, None);
        // No env/FS — build_record takes the flag directly (race-free).
        let plain = m.build_record("0.0.0", false);
        assert!(
            plain.get("dogfood").is_none(),
            "no dogfood field when not flagged"
        );
        let tagged = m.build_record("0.0.0", true);
        assert_eq!(tagged["dogfood"], serde_json::json!(true),
            "CODE_GRAPH_DOGFOOD sessions must be tagged so they can be filtered from adoption metrics");
    }

    #[test]
    fn test_avg_quality_calculation() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let mut m = SessionMetrics::new();
        m.record_tool_call("test", 10, None, None);
        m.record_search(5, 0.8, false);
        m.record_search(3, 0.6, true);
        m.flush(&usage_path, "0.5.26");

        let content = std::fs::read_to_string(&usage_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        // avg_quality = (0.8 + 0.6) / 2 = 0.7
        assert_eq!(record["search"]["avg_quality"], 0.7);
    }

    #[test]
    fn test_err_kind_classify_covers_canonical_patterns() {
        // Real error strings produced by the tool handlers — anchors the
        // classifier against regressions if the messages drift.
        use ErrKind::*;
        assert_eq!(
            ErrKind::classify("Error: FOREIGN KEY constraint failed"),
            FkConstraint,
        );
        assert_eq!(
            ErrKind::classify(
                "Indexing in progress — results will be available shortly. \
                 Please retry your request in a few seconds."
            ),
            Timeout,
        );
        assert_eq!(
            ErrKind::classify(
                "Ambiguous symbol 'open': 2 matches in different files. \
                 Specify file_path to disambiguate."
            ),
            Ambiguous,
        );
        assert_eq!(
            ErrKind::classify(
                "Symbol 'doesnotexist_ZZZ' not found in index. \
                 Use semantic_code_search to find the correct symbol name."
            ),
            NotFound,
        );
        assert_eq!(ErrKind::classify("query must not be empty"), EmptyInput);
        assert_eq!(
            ErrKind::classify("Must pass confirm: true to rebuild index"),
            EmptyInput
        );
        assert_eq!(
            ErrKind::classify("symbol_name or route_path is required"),
            EmptyInput
        );
        assert_eq!(
            ErrKind::classify("direction must be one of: callers, callees, both (got 'x')"),
            BadParam,
        );
        assert_eq!(ErrKind::classify("Unknown tool: nonexistent_tool"), Other);
    }

    #[test]
    fn test_flush_emits_err_kinds_breakdown() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let mut m = SessionMetrics::new();
        m.record_tool_call("get_ast_node", 100, Some(ErrKind::Ambiguous), None);
        m.record_tool_call("get_ast_node", 120, Some(ErrKind::Ambiguous), None);
        m.record_tool_call("get_ast_node", 90, Some(ErrKind::NotFound), None);
        m.record_tool_call("get_ast_node", 80, None, None); // success
                                                            // Tool with no errors — err_kinds must be omitted from output.
        m.record_tool_call("project_map", 2000, None, None);
        m.flush(&usage_path, "test");

        let content = std::fs::read_to_string(&usage_path).unwrap();
        let rec: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(rec["tools"]["get_ast_node"]["err"], 3);
        assert_eq!(rec["tools"]["get_ast_node"]["err_kinds"]["ambiguous"], 2);
        assert_eq!(rec["tools"]["get_ast_node"]["err_kinds"]["not_found"], 1);
        // Success-only tool omits err_kinds entirely.
        assert!(
            rec["tools"]["project_map"]["err_kinds"].is_null(),
            "err_kinds must not appear for success-only tool, got: {}",
            rec["tools"]["project_map"]
        );
    }

    #[test]
    fn test_other_sample_captures_first_unclassified_message() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let mut m = SessionMetrics::new();
        // First `other` message wins; a later one must NOT overwrite it. These are
        // genuinely-unclassified strings (no classify() keyword) — the residual the
        // sample is meant to surface after not_found/bad_param/etc. are split out.
        m.record_tool_call(
            "get_call_graph",
            5,
            Some(ErrKind::Other),
            Some("internal error: call graph query failed unexpectedly"),
        );
        m.record_tool_call(
            "get_call_graph",
            6,
            Some(ErrKind::Other),
            Some("some later unexpected failure"),
        );
        // A classified (non-other) error must NOT produce a sample.
        m.record_tool_call(
            "get_ast_node",
            7,
            Some(ErrKind::NotFound),
            Some("Symbol 'zzz' not found in index."),
        );
        m.flush(&usage_path, "test");

        let rec: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&usage_path).unwrap().trim()).unwrap();
        assert_eq!(
            rec["tools"]["get_call_graph"]["other_sample"],
            "internal error: call graph query failed unexpectedly",
            "first `other` message is sampled, later ones do not overwrite it"
        );
        assert!(rec["tools"]["get_ast_node"]["other_sample"].is_null(),
            "a not_found error must not populate other_sample (classified kinds are self-describing)");
    }

    #[test]
    fn test_get_call_graph_param_validation_classifies_out_of_other() {
        // The get_call_graph `other` bucket was dominated by param-validation
        // errors (usage.jsonl showed other:32 vs not_found:7). These VERBATIM
        // handler strings (src/mcp/server/tools/callgraph.rs) now split out of
        // `other`: bad VALUE → BadParam, missing param → EmptyInput. A residual
        // `other` after this is a genuinely-unexpected error worth chasing.
        use ErrKind::*;
        // Invalid value / bad combination → BadParam.
        assert_eq!(
            ErrKind::classify("direction must be one of: callers, callees, both (got 'up')"),
            BadParam
        );
        assert_eq!(
            ErrKind::classify(
                "min_confidence must be one of: extracted, inferred, ambiguous (got 'high')"
            ),
            BadParam
        );
        assert_eq!(
            ErrKind::classify(
                "symbol_name and route_path are mutually exclusive — pass exactly one"
            ),
            BadParam
        );
        assert_eq!(
            ErrKind::classify("Unknown relation filter: 'x'. Valid: calls, imports, inherits, implements, references, all"),
            BadParam);
        // Missing required param → EmptyInput (not a wrong value).
        assert_eq!(
            ErrKind::classify("symbol_name or route_path is required"),
            EmptyInput
        );
    }
}
