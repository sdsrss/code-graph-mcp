//! Shared plumbing for the append-only telemetry JSONL files.
//!
//! Two writers exist for these files — `mcp::metrics` (usage.jsonl) and
//! `cli::record_cli_use` (recommendations.jsonl) — plus a third in JS
//! (`claude-plugin/scripts/recommendation-log.js`). The rotation thresholds and
//! the timestamp format are therefore a cross-surface contract, not an MCP
//! detail; they live here so `cli` does not have to reach up into `mcp` for
//! them (`tests/hardening.rs` forbidden-edge table, 2026-08-16 audit §六).

use std::path::Path;

/// Size threshold above which append-only telemetry JSONL files are rotated.
/// Shared by usage.jsonl (`SessionMetrics::flush`) and recommendations.jsonl
/// (`cli::record_cli_use`). MUST stay in sync with the JS PreToolUse writer
/// `claude-plugin/scripts/recommendation-log.js` — recommendations.jsonl is
/// written from both Rust and JS, so both sides must rotate identically.
pub(crate) const JSONL_ROTATE_MAX_BYTES: u64 = 1_048_576; // 1 MB
/// Bytes retained (file tail) when a telemetry JSONL file is rotated.
pub(crate) const JSONL_ROTATE_KEEP_BYTES: usize = 524_288; // 512 KB

/// Canonical name for a CLI *query* subcommand (incl. MCP-name aliases), or
/// None for housekeeping (serve/index/stats/doctor/...). Drives `record_cli_use`:
/// only code-understanding queries count as funnel conversions.
///
/// Shared vocabulary rather than CLI-private: `outcome::cli_call_in_line` parses
/// the same names back out of transcripts, and the two must not drift.
pub fn canonical_query_cmd(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "grep" => "grep",
        "search" | "semantic_code_search" => "search",
        "ast-search" | "ast_search" => "ast-search",
        "callgraph" | "get_call_graph" => "callgraph",
        "impact" | "impact_analysis" => "impact",
        "affected" => "affected",
        "tour" => "tour",
        "map" | "project_map" => "map",
        "overview" | "module_overview" => "overview",
        "show" | "get_ast_node" => "show",
        "trace" | "trace_http_chain" => "trace",
        "deps" | "dependency_graph" => "deps",
        "similar" | "find_similar_code" => "similar",
        "refs" | "find_references" => "refs",
        "dead-code" | "find_dead_code" => "dead-code",
        "centrality" => "centrality",
        _ => return None,
    })
}

/// Generate an ISO 8601 timestamp from SystemTime (no chrono dependency).
/// pub(crate): stamps both usage.jsonl records and CLI `use` records
/// (`cli::record_cli_use`).
pub(crate) fn iso8601_now() -> String {
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

/// Best-effort size-based rotation for append-only telemetry JSONL files. If
/// `path` is larger than `max_bytes`, rewrite it keeping ~the last `keep_bytes`,
/// trimmed *forward* to the next line boundary so no partial line survives. All
/// errors are logged and swallowed — telemetry rotation must never break or
/// delay the caller. Mirrored in `claude-plugin/scripts/recommendation-log.js`
/// (recommendations.jsonl is also written by the JS PreToolUse hooks).
pub(crate) fn rotate_jsonl_if_over(path: &Path, max_bytes: u64, keep_bytes: usize) {
    // `symlink_metadata`, not `metadata`: a symlink here used to make the size
    // test read the TARGET's length and the rewrite below truncate the target —
    // an arbitrary file outside the project, cut to its last `keep_bytes`
    // (audit 2026-08-29 SEC-02). Refusing at the stat also means the target is
    // never read, so this is not merely a write guard.
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    }; // missing → nothing to do
    if !meta.file_type().is_file() {
        tracing::warn!(
            "Not rotating {}: not a regular file (leaving it untouched)",
            path.display()
        );
        return;
    }
    if meta.len() <= max_bytes {
        return; // under threshold → leave it
    }
    let Ok(content) = std::fs::read(path) else {
        return;
    };
    let start = content.len().saturating_sub(keep_bytes);
    // Advance to the first newline at/after `start` so the kept region begins on
    // a whole line (drop the partial line `start` may have landed inside).
    let trim_start = content[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|pos| start + pos + 1)
        .unwrap_or(start);
    use std::io::Write as _;
    match crate::utils::owned::rewrite_owned(path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(&content[trim_start..]) {
                tracing::warn!("Failed to rotate {}: {}", path.display(), e);
            }
        }
        Err(e) => tracing::warn!("Failed to rotate {}: {}", path.display(), e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_rotate_jsonl_if_over_trims_to_line_boundary_and_noops_when_small() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rec.jsonl");
        // ~2MB of distinct whole lines so we can assert the boundary is clean.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            let pad = "z".repeat(1000);
            for i in 0..2000 {
                writeln!(f, "{i:08}|{pad}").unwrap();
            }
        }
        assert!(std::fs::metadata(&path).unwrap().len() > JSONL_ROTATE_MAX_BYTES);

        rotate_jsonl_if_over(&path, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);

        let after = std::fs::metadata(&path).unwrap().len();
        assert!(
            after <= JSONL_ROTATE_KEEP_BYTES as u64,
            "kept tail must be <= keep_bytes, got {after}"
        );
        // No partial first line: it begins with the 8-digit counter + '|'.
        let content = std::fs::read_to_string(&path).unwrap();
        let first = content.lines().next().unwrap();
        assert_eq!(
            &first[8..9],
            "|",
            "first surviving line must start on a whole-line boundary: {first:.16}"
        );

        // Under threshold → untouched.
        let small = dir.path().join("small.jsonl");
        std::fs::write(&small, "a\nb\n").unwrap();
        rotate_jsonl_if_over(&small, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);
        assert_eq!(std::fs::read_to_string(&small).unwrap(), "a\nb\n");
    }

    /// `.code-graph/` is ordinary repo content — one `git clone` can carry a
    /// symlink where the rotator expects its own file. `fs::metadata` and
    /// `fs::write` both follow it, so the rotator read-modify-wrote the LINK
    /// TARGET: an unrelated >1 MB file outside the project was silently
    /// truncated to its last ~512 KB, first line and all (audit 2026-08-29
    /// SEC-02, measured 687,756 bytes destroyed).
    #[cfg(unix)]
    #[test]
    fn rotate_refuses_to_rewrite_through_a_symlink() {
        let dir = TempDir::new().unwrap();
        let victim = dir.path().join("victim.txt");
        let payload = format!("SECRET-HEADER-LINE\n{}\n", "A".repeat(1_500_000));
        std::fs::write(&victim, &payload).unwrap();
        let link = dir.path().join("recommendations.jsonl");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        rotate_jsonl_if_over(&link, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);

        let after = std::fs::read_to_string(&victim).unwrap();
        assert_eq!(
            after.len(),
            payload.len(),
            "the link target must not be rewritten (lost {} bytes)",
            payload.len().saturating_sub(after.len())
        );
        assert!(
            after.starts_with("SECRET-HEADER-LINE"),
            "the target's first line must survive"
        );

        // Positive control: the same call on a REGULAR oversized file still
        // rotates, so the assertions above cannot pass by the rotator having
        // become a no-op.
        let real = dir.path().join("real.jsonl");
        std::fs::write(&real, &payload).unwrap();
        rotate_jsonl_if_over(&real, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);
        assert!(
            std::fs::metadata(&real).unwrap().len() <= JSONL_ROTATE_KEEP_BYTES as u64,
            "a regular oversized file must still rotate"
        );
    }

    /// The symlink test above passes with the hardlink hole wide open: `lstat`
    /// reports a hardlinked victim as a plain regular file and `O_NOFOLLOW` says
    /// nothing about one, so the rotator read-modify-wrote a second path to the
    /// same inode. The guard that catches it lives in `utils::owned`, one level
    /// down — this row is here because the rotator is the caller that measurably
    /// destroyed a 1.2 MB file, and a guard pinned only at its own definition
    /// stops covering the caller the moment someone re-plumbs the open.
    ///
    /// `set_len` is the reason `rewrite_owned` must not carry `O_TRUNC`: with it,
    /// the victim is emptied by the open and this assertion fails before any
    /// refusal can happen.
    #[cfg(unix)]
    #[test]
    fn rotate_refuses_to_rewrite_through_a_hardlink() {
        let dir = TempDir::new().unwrap();
        let victim = dir.path().join("victim.txt");
        let payload = format!("SECRET-HEADER-LINE\n{}\n", "A".repeat(1_500_000));
        std::fs::write(&victim, &payload).unwrap();
        let link = dir.path().join("recommendations.jsonl");
        std::fs::hard_link(&victim, &link).unwrap();

        rotate_jsonl_if_over(&link, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);

        let after = std::fs::read_to_string(&victim).unwrap();
        assert_eq!(
            after.len(),
            payload.len(),
            "the hardlinked file must not be rewritten (lost {} bytes)",
            payload.len().saturating_sub(after.len())
        );
        assert!(
            after.starts_with("SECRET-HEADER-LINE"),
            "the hardlinked file's first line must survive"
        );

        // Positive control: the same call on a single-link oversized file still
        // rotates, so the assertions above cannot pass by the rotator having
        // become a no-op — which is exactly what dropping `set_len(0)` along with
        // `O_TRUNC` would produce.
        let real = dir.path().join("real.jsonl");
        std::fs::write(&real, &payload).unwrap();
        rotate_jsonl_if_over(&real, JSONL_ROTATE_MAX_BYTES, JSONL_ROTATE_KEEP_BYTES);
        assert!(
            std::fs::metadata(&real).unwrap().len() <= JSONL_ROTATE_KEEP_BYTES as u64,
            "a regular oversized file must still rotate"
        );
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
}
