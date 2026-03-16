use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

/// Per-tool call statistics.
pub struct ToolStats {
    pub count: u64,
    pub total_ms: u64,
    pub errors: u64,
    pub max_ms: u64,
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
    tools: HashMap<String, ToolStats>,
    search: SearchMetrics,
    pub full_index_ms: Option<u64>,
    pub incremental_count: u64,
    pub files_indexed: u64,
    pub nodes_created: u64,
}

impl SessionMetrics {
    /// Create a new empty session.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
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

    /// Record a tool invocation.
    pub fn record_tool_call(&mut self, name: &str, elapsed_ms: u64, is_error: bool) {
        let stats = self.tools.entry(name.to_string()).or_insert(ToolStats {
            count: 0,
            total_ms: 0,
            errors: 0,
            max_ms: 0,
        });
        stats.count += 1;
        stats.total_ms += elapsed_ms;
        if is_error {
            stats.errors += 1;
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

    /// Serialize session metrics to one-line JSON and append to the usage file.
    /// Performs size-based rotation: if file > 1MB, truncate to last 512KB.
    pub fn flush(&self, usage_path: &Path, version: &str) {
        let dur_s = self.start.elapsed().as_secs();
        let ts = iso8601_now();

        // Build tools map
        let tools_json: serde_json::Map<String, serde_json::Value> = self.tools.iter().map(|(name, stats)| {
            (name.clone(), serde_json::json!({
                "n": stats.count,
                "ms": stats.total_ms,
                "err": stats.errors,
                "max_ms": stats.max_ms,
            }))
        }).collect();

        let avg_quality = if self.search.total_queries > 0 {
            ((self.search.quality_sum / self.search.total_queries as f64) * 100.0).round() / 100.0
        } else {
            0.0
        };

        let record = serde_json::json!({
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

        let line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to serialize session metrics: {}", e);
                return;
            }
        };

        // Ensure parent directory exists
        if let Some(parent) = usage_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("Failed to create metrics directory: {}", e);
                return;
            }
        }

        // Size-based rotation: if file > 1MB, keep last 512KB
        const MAX_SIZE: u64 = 1_048_576; // 1MB
        const KEEP_SIZE: usize = 524_288; // 512KB
        if let Ok(meta) = std::fs::metadata(usage_path) {
            if meta.len() > MAX_SIZE {
                if let Ok(content) = std::fs::read(usage_path) {
                    let start = content.len().saturating_sub(KEEP_SIZE);
                    // Find the first newline after start to avoid partial lines
                    let trim_start = content[start..]
                        .iter()
                        .position(|&b| b == b'\n')
                        .map(|pos| start + pos + 1)
                        .unwrap_or(start);
                    if let Err(e) = std::fs::write(usage_path, &content[trim_start..]) {
                        tracing::warn!("Failed to rotate usage file: {}", e);
                        return;
                    }
                }
            }
        }

        // Append the line
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(usage_path)
        {
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

/// Generate an ISO 8601 timestamp from SystemTime (no chrono dependency).
fn iso8601_now() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Calculate date/time components from unix timestamp
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch to year/month/day (civil_from_days algorithm)
    let (year, month, day) = civil_from_days(days as i64);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
/// Based on Howard Hinnant's civil_from_days algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    #[test]
    fn test_new_session_is_empty() {
        let m = SessionMetrics::new();
        assert!(m.is_empty());
        assert_eq!(m.files_indexed, 0);
        assert_eq!(m.nodes_created, 0);
        assert!(m.full_index_ms.is_none());
    }

    #[test]
    fn test_record_tool_call_basic() {
        let mut m = SessionMetrics::new();
        m.record_tool_call("semantic_code_search", 150, false);
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
        m.record_tool_call("get_call_graph", 100, false);
        m.record_tool_call("get_call_graph", 200, true);
        m.record_tool_call("get_call_graph", 50, false);
        let stats = m.tools.get("get_call_graph").unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.total_ms, 350);
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.max_ms, 200);
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
        m.record_tool_call("semantic_code_search", 150, false);
        m.record_tool_call("get_call_graph", 200, true);
        m.record_search(3, 0.85, false);
        m.record_index(50, 200, true, 1500);
        m.flush(&usage_path, "0.5.26");

        let mut content = String::new();
        std::fs::File::open(&usage_path).unwrap().read_to_string(&mut content).unwrap();
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
        m1.record_tool_call("project_map", 100, false);
        m1.flush(&usage_path, "0.5.26");

        let mut m2 = SessionMetrics::new();
        m2.record_tool_call("get_call_graph", 200, false);
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
        m.record_tool_call("test", 10, false);
        m.flush(&usage_path, "0.5.26");

        let size_after = std::fs::metadata(&usage_path).unwrap().len();
        // After rotation, file should be around 512KB + the new line
        assert!(size_after < 600_000, "File should be rotated down, got {} bytes", size_after);
        assert!(size_after > 500_000, "File should retain ~512KB, got {} bytes", size_after);

        // Last line should be valid JSON from our flush
        let content = std::fs::read_to_string(&usage_path).unwrap();
        let last_line = content.trim().lines().last().unwrap();
        let record: serde_json::Value = serde_json::from_str(last_line).unwrap();
        assert_eq!(record["v"], "0.5.26");
    }

    #[test]
    fn test_iso8601_format() {
        let ts = iso8601_now();
        // Should match YYYY-MM-DDTHH:MM:SSZ pattern
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn test_avg_quality_calculation() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join("usage.jsonl");

        let mut m = SessionMetrics::new();
        m.record_tool_call("test", 10, false); // need at least one tool call
        m.record_search(5, 0.8, false);
        m.record_search(3, 0.6, true);
        m.flush(&usage_path, "0.5.26");

        let content = std::fs::read_to_string(&usage_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        // avg_quality = (0.8 + 0.6) / 2 = 0.7
        assert_eq!(record["search"]["avg_quality"], 0.7);
    }
}
