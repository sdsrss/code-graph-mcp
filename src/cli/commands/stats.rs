use super::*;

// Idiomatic-flavor UX change — `//` (not `///`) so it stays out of clap `--help`:
// `--last <non-number>` is now a hard parse error (exit 2, clap message) instead of
// the prior warn-and-show-all fallback.
/// Broken-pipe safety (mirrors grep's `test_cli_grep_sigpipe_graceful`
/// contract): route every stdout write through this macro so an early-closing
/// reader (`stats | head`, a `| less` the user quits) exits 0 silently instead
/// of panicking on EPIPE the way raw `println!` does — that surfaced as a
/// SIGABRT/134 crash with a `failed printing to stdout: Broken pipe` panic.
///
/// At module scope rather than inside `cmd_stats` so the rendering half can be
/// its own function; it still only expands inside a `-> Result<()>` body,
/// because it returns the error it cannot swallow.
macro_rules! sout {
    ($($a:tt)*) => {
        if let Err(e) = writeln!(std::io::stdout(), $($a)*) {
            if e.kind() == std::io::ErrorKind::BrokenPipe { grep_exit(0); }
            return Err(e.into());
        }
    };
}

/// CLI arguments for the `stats` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp stats",
    about = "Aggregate session metrics from .code-graph/usage.jsonl"
)]
pub struct StatsArgs {
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Limit to the last N sessions (default: all)
    #[arg(long)]
    pub last: Option<usize>,
}

/// Numeric (semver) sort key for a version string. `versions` is stored in a
/// BTreeSet, which orders lexically — so "0.5.40" sorted AFTER "0.32.2". Parse the
/// leading digits of the first three dot-separated components so ordering is by
/// (major, minor, patch); non-numeric/missing components fall back to 0, keeping
/// the sort total and panic-free for odd version strings.
pub(crate) fn version_sort_key(v: &str) -> (u64, u64, u64) {
    let mut parts = v.split('.').map(|part| {
        part.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// One-line stderr hint after a symbol-name miss. Query-time freshness
/// (`refresh_files_if_stale`) can only re-sync files the symbol is already
/// indexed in — a symbol ADDED since the last index has no file to refresh, so
/// a lookup miss is indistinguishable from "doesn't exist" without this hint.
pub(crate) fn hint_symbol_maybe_unindexed(symbol: &str) {
    eprintln!(
        "[code-graph] If '{}' was added recently, the index may be stale — run \
         `code-graph-mcp incremental-index` and retry.",
        symbol
    );
}

/// Pluralize a count for human-readable output: `1 file`, `0 files`, `2 files`.
/// Avoids the "1 files"/"1 lines" grammar glitch on single-item results (common
/// for single-file modules and one-line dead-code candidates). Naive `+s` only —
/// callers pass already-plural-friendly stems (file, line, symbol).
pub(crate) fn plural(n: i64, singular: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {singular}s")
    }
}

/// Print aggregated session metrics from `.code-graph/usage.jsonl`.
/// Diagnostic: shows which tools you actually use + search/index activity.
/// `--last N` limits to the most recent N sessions. `--json` emits structured output.
/// Build the `--json` envelope. Extracted from `cmd_stats` (audit 2026-08-22
/// P2-15): it is 120 lines of pure construction with no I/O, and inlining it
/// made the surrounding function's two output modes hard to see at all.
fn build_stats_json(
    summary: &UsageSummary,
    recs: &RecommendationSummary,
    rec_state: &str,
) -> serde_json::Value {
    let tools_json: serde_json::Map<String, serde_json::Value> = summary
        .tools
        .iter()
        .map(|(name, a)| {
            let avg = a.total_ms.checked_div(a.n).unwrap_or(0);
            let mut o = serde_json::json!({
                "n": a.n, "total_ms": a.total_ms, "avg_ms": avg, "err": a.err, "max_ms": a.max_ms,
            });
            // Additive: only present when the tool logged a classified error.
            if !a.err_kinds.is_empty() {
                o["err_kinds"] = serde_json::json!(a.err_kinds);
            }
            if let Some(sample) = &a.other_sample {
                o["other_sample"] = serde_json::json!(sample);
            }
            (name.clone(), o)
        })
        .collect();
    let avg_q = if summary.search_queries > 0 {
        summary.search_quality_weighted_sum / summary.search_queries as f64
    } else {
        0.0
    };
    let full_avg = summary
        .full_index_ms_sum
        .checked_div(summary.full_index_count)
        .unwrap_or(0);
    let mut sorted_versions: Vec<String> = summary.versions.iter().cloned().collect();
    sorted_versions.sort_by_key(|v| version_sort_key(v));
    let mut stats_json = serde_json::json!({
        "sessions": summary.sessions,
        "parse_errors": summary.parse_errors,
        "versions": sorted_versions,
        "first_ts": summary.first_ts,
        "last_ts": summary.last_ts,
        "total_tool_calls": summary.total_tool_calls(),
        "live_tools": crate::domain::LIVE_MCP_TOOLS,
        "tools": tools_json,
        "search": {
            "queries": summary.search_queries,
            "zero": summary.search_zero,
            "avg_quality": (avg_q * 100.0).round() / 100.0,
            "fts_only": summary.search_fts_only,
            "hybrid": summary.search_hybrid,
        },
        "index": {
            "full_count": summary.full_index_count,
            "full_avg_ms": full_avg,
            "incr_count": summary.incr_count,
            "files_indexed": summary.files_indexed,
        },
        "recommendations": {
            "state": rec_state,
            "total": recs.total,
            "by_action": recs.by_action.iter().map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
            "by_hook": recs.by_hook.iter().map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
            // Sub-breakdown of by_action["inject"] by payload mode. callgraph =
            // the marginal-value cross-file tree; grep/show = redundant echo.
            "inject_by_mode": recs.inject_by_mode.iter().map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
            "cg_tool_calls": summary.total_tool_calls(),
            "cli_uses": recs.cli_uses,
            "deny_answered": recs.deny_answered,
            "deny_unanswered": recs.deny_unanswered,
            // Outcome proxy: observe = silent grep/read allows recorded by the
            // hooks. re_search_rate = fraction of answered denies immediately
            // followed by ANY grep/read — kept for back-compat, but it OVER-counts
            // insufficiency (it includes drill-down cg also answered + file-reads).
            // fallthrough_rate is the honest "inline answer insufficient" fraction:
            // the follow-up was a search cg could NOT satisfy. Both null until an
            // answered deny exists to divide by.
            "observe": recs.observe,
            "live_impact": recs.live_impact,
            "researched_after_answer": recs.researched_after_answer,
            "re_search_rate": if recs.deny_answered > 0 {
                serde_json::json!((recs.researched_after_answer as f64 / recs.deny_answered as f64 * 100.0).round() / 100.0)
            } else { serde_json::Value::Null },
            "sustained_after_answer": recs.sustained_after_answer,
            "fallthrough_after_answer": recs.fallthrough_after_answer,
            "followup_inconclusive": recs.followup_inconclusive,
            "fallthrough_rate": if recs.deny_answered > 0 {
                serde_json::json!((recs.fallthrough_after_answer as f64 / recs.deny_answered as f64 * 100.0).round() / 100.0)
            } else { serde_json::Value::Null },
            // tool_calls / recommendations: two independent populations, so
            // this is an activity/volume ratio, NOT a recommend→use rate. The
            // real conversion is funnel.deny_conversion / hint_conversion.
            "tool_calls_per_rec": if recs.total > 0 {
                (summary.total_tool_calls() as f64 / recs.total as f64 * 100.0).round() / 100.0
            } else { 0.0 },
            // Per-session deny→use / hint→use funnel (window-joined attribution).
            // v0.49: *_conversion is ANY-use (MCP cg tool OR CLI query) — the
            // deny→CLI leg is the proven conversion path; *_then_cg / *_then_cli
            // keep the legs separable.
            "funnel": {
                "deny_sessions": summary.sessions_with_deny,
                "deny_then_cg": summary.sessions_with_deny_and_cg,
                "deny_then_cli": summary.sessions_with_deny_and_cli,
                "deny_then_use": summary.sessions_with_deny_and_use,
                "deny_conversion": session_conversion(summary.sessions_with_deny_and_use, summary.sessions_with_deny),
                "hint_sessions": summary.sessions_with_hint,
                "hint_then_cg": summary.sessions_with_hint_and_cg,
                "hint_then_cli": summary.sessions_with_hint_and_cli,
                "hint_then_use": summary.sessions_with_hint_and_use,
                "hint_conversion": session_conversion(summary.sessions_with_hint_and_use, summary.sessions_with_hint),
            },
        },
    });
    // Assigned post-hoc: the json! literal above is at the macro recursion
    // limit — one more inline key fails to expand (`json_internal!` recursion).
    // Non-delivering injects (answered:false: no-hits/no-binary/unavailable) —
    // distinguishes "hook ran, nothing to say" from "hook dark" (§1.6).
    stats_json["recommendations"]["inject_skipped"] = serde_json::json!(recs.inject_skipped);
    // Which mode burned the skipped attempts (D#147). Separate from
    // inject_by_mode, which stays delivered-only — see usage.rs. May sum
    // below inject_skipped: pre-fix skips carry no mode.
    stats_json["recommendations"]["inject_skipped_by_mode"] = recs
        .inject_skipped_by_mode
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::json!(v)))
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();
    stats_json
}

/// The human-readable half of `stats`. Extracted from `cmd_stats` (audit
/// 2026-08-22 P2-15): 300 lines of rendering sat inside the `else` arm of a
/// 500-line function, so the two output modes could not be read side by side.
/// Uses the module-level `sout!`, which is why it returns `Result`.
fn render_stats_text(
    summary: &UsageSummary,
    recs: &RecommendationSummary,
    rec_exists: bool,
) -> Result<()> {
    let mut versions: Vec<&str> = summary.versions.iter().map(|s| s.as_str()).collect();
    versions.sort_by_key(|v| version_sort_key(v));
    sout!(
        "Sessions: {}   versions: {}   {} → {}",
        summary.sessions,
        if versions.is_empty() {
            "-".into()
        } else {
            versions.join(",")
        },
        summary.first_ts.as_deref().unwrap_or("-"),
        summary.last_ts.as_deref().unwrap_or("-"),
    );
    sout!("Total tool calls: {}", summary.total_tool_calls());
    if summary.parse_errors > 0 {
        sout!(
            "(warning: {} malformed line(s) skipped)",
            summary.parse_errors
        );
    }
    sout!();

    let mut sorted: Vec<(&String, &ToolAgg)> = summary.tools.iter().collect();
    sorted.sort_by_key(|(_, a)| std::cmp::Reverse(a.n));

    if sorted.is_empty() {
        sout!("(no tool calls recorded)");
    } else {
        sout!(
            "{:<28} {:>6} {:>10} {:>6} {:>8}",
            "Tool",
            "n",
            "avg_ms",
            "err",
            "max_ms"
        );
        sout!("{}", "-".repeat(62));
        let mut any_legacy = false;
        for (name, agg) in &sorted {
            let avg = agg.total_ms.checked_div(agg.n).unwrap_or(0);
            // Mark tool names no longer in the live tools/list surface (folded
            // or hidden, recorded by older sessions) so the table doesn't
            // commingle historical names with the current live set.
            let legacy = !crate::domain::LIVE_MCP_TOOLS.contains(&name.as_str());
            if legacy {
                any_legacy = true;
            }
            let label = if legacy {
                format!("{name} †")
            } else {
                name.to_string()
            };
            sout!(
                "{:<28} {:>6} {:>10} {:>6} {:>8}",
                label,
                agg.n,
                avg,
                agg.err,
                agg.max_ms
            );
        }
        if any_legacy {
            sout!("  † not in the current tools/list surface (folded/hidden; from older sessions)");
        }
    }

    // Error-kind breakdown: turn the opaque `err` column into actionable
    // buckets. `not_found` = benign name-miss (model guessed a symbol name);
    // `other` = unclassified — a large `other` is the real signal to chase
    // (expand ErrKind::classify in metrics.rs). A gap between `err` and the
    // sum of kinds is pre-feature sessions (shown as `unrecorded`).
    let mut err_tools: Vec<(&String, &ToolAgg)> =
        summary.tools.iter().filter(|(_, a)| a.err > 0).collect();
    if !err_tools.is_empty() {
        err_tools.sort_by_key(|(_, a)| std::cmp::Reverse(a.err));
        sout!();
        sout!("Error kinds (per tool, most errors first — large `other` = unclassified, investigate):");
        for (name, agg) in &err_tools {
            let mut kinds: Vec<(&String, &u64)> = agg.err_kinds.iter().collect();
            kinds.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
            let classified: u64 = agg.err_kinds.values().sum();
            let mut parts: Vec<String> =
                kinds.iter().map(|(k, c)| format!("{} {}", k, c)).collect();
            if agg.err > classified {
                parts.push(format!("unrecorded {}", agg.err - classified));
            }
            let label = if !crate::domain::LIVE_MCP_TOOLS.contains(&name.as_str()) {
                format!("{name} †")
            } else {
                name.to_string()
            };
            sout!("  {:<26} {} err = {}", label, agg.err, parts.join(" · "));
            // Surface the sampled `other` message so the bucket self-explains.
            if let Some(sample) = &agg.other_sample {
                sout!("  {:<26}   ↳ other e.g. {:?}", "", sample);
            }
        }
    }

    if summary.search_queries > 0 {
        let zero_pct =
            (summary.search_zero as f64 / summary.search_queries as f64 * 100.0).round() as u64;
        let avg_q = summary.search_quality_weighted_sum / summary.search_queries as f64;
        sout!();
        sout!(
            "Search: {} queries, {} zero-result ({}%), hybrid/fts {}/{}, avg quality {:.2}",
            summary.search_queries,
            summary.search_zero,
            zero_pct,
            summary.search_hybrid,
            summary.search_fts_only,
            avg_q
        );
    }

    if summary.full_index_count > 0 || summary.incr_count > 0 {
        let full_part = match summary
            .full_index_ms_sum
            .checked_div(summary.full_index_count)
        {
            Some(avg) if summary.full_index_count > 0 => format!(" (avg {}ms)", avg),
            _ => String::new(),
        };
        sout!(
            "Index:  {} full{}, {} incremental, {} files indexed",
            summary.full_index_count,
            full_part,
            summary.incr_count,
            summary.files_indexed
        );
    }

    sout!();
    if recs.total > 0 {
        let actions: Vec<String> = recs
            .by_action
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect();
        let ratio = summary.total_tool_calls() as f64 / recs.total as f64;
        sout!(
            "Recommendations: {} emitted ({})",
            recs.total,
            actions.join(", ")
        );
        // Inject payload mix. callgraph is the only mode with marginal value over
        // the model's own grep (cross-file tree); grep/show echo hits it already
        // saw (2026-06-26 audit: 0 CONSUMED). Lead with the callgraph share — the
        // lever is raising it. Modes may sum below by_action.inject (pre-v0.75
        // injects carry no mode).
        if !recs.inject_by_mode.is_empty() {
            let modes: Vec<String> = recs
                .inject_by_mode
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect();
            let inj_total: u64 = recs.inject_by_mode.values().sum();
            let cg = recs.inject_by_mode.get("callgraph").copied().unwrap_or(0);
            let cg_pct = if inj_total > 0 {
                (cg as f64 / inj_total as f64 * 100.0).round() as u64
            } else {
                0
            };
            sout!(
                "Inject payloads: {} by mode ({}) — callgraph (cross-file, high-value) = {cg_pct}%",
                inj_total,
                modes.join(", ")
            );
        }
        // The other half of the attempt funnel (D#147): injects that RAN and
        // delivered nothing, attributed to the mode that burned the attempt.
        // Printed separately from the payload mix above precisely because it
        // is not a payload — reading the two as one number is what made the
        // skips unattributable in the first place.
        if !recs.inject_skipped_by_mode.is_empty() {
            let modes: Vec<String> = recs
                .inject_skipped_by_mode
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect();
            let attributed: u64 = recs.inject_skipped_by_mode.values().sum();
            // Pre-fix skips carry no mode; say so rather than let the map
            // read as the whole of inject_skipped.
            let unattributed = recs.inject_skipped.saturating_sub(attributed);
            let tail = if unattributed > 0 {
                format!(", {unattributed} unattributed (recorded before the mode split)")
            } else {
                String::new()
            };
            sout!(
                "Inject skips: {} delivered nothing by mode ({}){}",
                attributed,
                modes.join(", "),
                tail
            );
        }
        if recs.deny_answered + recs.deny_unanswered > 0 {
            // answered:true denies satisfy the need in-place — read their
            // conversion separately or the funnel under-reports the feature.
            sout!(
                "Denies: {} answered in-place, {} static",
                recs.deny_answered,
                recs.deny_unanswered
            );
        }
        if recs.cli_uses > 0 {
            sout!(
                "CLI uses: {} model-initiated code-graph-mcp queries",
                recs.cli_uses
            );
        }
        // Outcome proxy ("search-decay"): of the answered denies (cg delivered
        // the grep result in-place), how often did the model immediately keep
        // searching? Lower = the inline answer was enough. observe = the silent
        // grep/read allows that make the fan-out visible.
        if recs.deny_answered > 0 {
            // Honest fan-out signal. The follow-up after an answered deny is
            // one of: cg ALSO answered a DIFFERENT next step (sustained drill-down
            // — a win), a file read acting on the answer (observe), or the inline
            // answer didn't end the hunt — a verbatim re-grep of the same pattern
            // or a search cg couldn't satisfy (fall-through). Only fall-through
            // means the inline answer was insufficient. The raw "kept searching"
            // count lumps all three
            // and reads alarmingly high even when cg wins every step, so lead
            // with fall-through and show the raw count correctly framed.
            let ft_pct = (recs.fallthrough_after_answer as f64 / recs.deny_answered as f64 * 100.0)
                .round() as u64;
            sout!("Fall-through after cg answer: {}/{} answered denies → inline answer didn't end the hunt (verbatim re-grep or a search cg couldn't satisfy) = {ft_pct}% (the real 'answer insufficient' rate; lower is better)",
                recs.fallthrough_after_answer, recs.deny_answered);
            if recs.sustained_after_answer > 0 {
                sout!("  ↳ drill-down sustained: {} follow-up search(es) cg also answered — cg kept up, not a miss",
                    recs.sustained_after_answer);
            }
            if recs.followup_inconclusive > 0 {
                sout!("  ↳ inconclusive (excluded): {} follow-up(s) where cg found nothing (no-hits = a new query) or was unavailable — says nothing about the prior answer",
                    recs.followup_inconclusive);
            }
            let raw_pct = (recs.researched_after_answer as f64 / recs.deny_answered as f64 * 100.0)
                .round() as u64;
            sout!("  ↳ any follow-up (raw): {}/{} = {raw_pct}% — incl. drill-down + file-reads; NOT a failure rate",
                recs.researched_after_answer, recs.deny_answered);
        }
        if recs.observe > 0 {
            sout!(
                "Tool observes: {} silent grep/read allows recorded (fan-out timeline)",
                recs.observe
            );
        }
        // Volume ratio (NOT a conversion rate): cg tool calls and hook
        // recommendations are independent populations, so this only signals
        // activity level. The real recommend→use conversion is the Deny→use /
        // Hint→use funnel printed below.
        sout!("Tool-call volume: {} cg calls / {} recommendations = {ratio:.2} (activity ratio, not conversion)",
            summary.total_tool_calls(), recs.total);
    } else if rec_exists {
        // File present but empty: hooks are wired and recording, just no
        // recommendation has fired yet.
        sout!("Recommendations: 0 recorded (PreToolUse hooks active; conversion metric live, no data yet)");
    } else {
        // No file at all: the recording hooks are not active in this project
        // (e.g. a dev `.mcp.json` server with the marketplace plugin's
        // PreToolUse hooks disabled). Surface the dark state instead of
        // printing nothing — silence reads as "feature absent".
        sout!("Conversion metric: DARK — no recommendations.jsonl. PreToolUse hooks are not");
        sout!("  recording here, so recommend→use conversion cannot be measured in this project.");
    }
    // v0.63 — SessionStart live-context injections. Printed outside the
    // total>0 branch (it's a separate counter): a session whose only event was
    // the SessionStart injection still surfaces it instead of reading dark.
    if recs.live_impact > 0 {
        sout!(
            "Live-context: {} recent-change blast-radius injection(s) at SessionStart",
            recs.live_impact
        );
    }
    // Per-session funnel: of sessions that saw a deny/hint, how many also called
    // a cg query tool. This is the deny→use attribution the aggregate ratio can't give.
    if summary.sessions_with_deny > 0 {
        let pct = (summary.sessions_with_deny_and_use as f64 / summary.sessions_with_deny as f64
            * 100.0)
            .round() as u64;
        sout!(
            "Deny→use: {}/{} deny-sessions used cg = {}% (mcp {}, cli {})",
            summary.sessions_with_deny_and_use,
            summary.sessions_with_deny,
            pct,
            summary.sessions_with_deny_and_cg,
            summary.sessions_with_deny_and_cli
        );
    }
    if summary.sessions_with_hint > 0 {
        let pct = (summary.sessions_with_hint_and_use as f64 / summary.sessions_with_hint as f64
            * 100.0)
            .round() as u64;
        sout!(
            "Hint→use: {}/{} hint-sessions used cg = {}% (mcp {}, cli {})",
            summary.sessions_with_hint_and_use,
            summary.sessions_with_hint,
            pct,
            summary.sessions_with_hint_and_cg,
            summary.sessions_with_hint_and_cli
        );
    }
    Ok(())
}

pub fn cmd_stats(project_root: &Path, args: StatsArgs) -> Result<()> {
    let json_mode = args.json;
    let last_n = args.last;

    let usage_path = project_root.join(CODE_GRAPH_DIR).join("usage.jsonl");
    // A missing file is read as empty rather than short-circuiting here. Both
    // empty legs used to return their own hand-written envelope — three keys of
    // eleven when the file was absent, two and no disclosure at all when it was
    // present but sessionless — so `stats --json` broke the object-envelope half
    // of the CLI JSON-empty contract on exactly the projects where a consumer
    // most needs a zero to read. Sharing `build_stats_json` makes the shape a
    // property of construction instead of a list somebody has to remember to
    // extend (`test_cli_stats_json_empty_keeps_the_populated_shape`).
    let usage_missing = !usage_path.exists();
    let content = if usage_missing {
        String::new()
    } else {
        std::fs::read_to_string(&usage_path)?
    };
    let summary = aggregate_usage_jsonl(&content, last_n);

    // Conversion metric: cg tool calls vs PreToolUse recommendations. The JSONL
    // has no per-session boundary, so it is aggregated whole (last_n applies only
    // to usage sessions). Absent file → empty (default) summary.
    let rec_path = project_root
        .join(CODE_GRAPH_DIR)
        .join("recommendations.jsonl");
    let rec_exists = rec_path.exists();
    let recs = std::fs::read_to_string(&rec_path)
        .ok()
        .map(|c| aggregate_recommendations_jsonl(&c))
        .unwrap_or_default();
    // Recording-side state of the conversion metric, made explicit so a dark
    // metric (file absent → PreToolUse hooks not recording here) is never
    // silently indistinguishable from "feature absent" or "no data yet".
    let rec_state = if recs.total > 0 || recs.cli_uses > 0 {
        "live"
    } else if rec_exists {
        "empty"
    } else {
        "absent"
    };

    if summary.sessions == 0 {
        if json_mode {
            let mut v = build_stats_json(&summary, &recs, rec_state);
            if let Some(obj) = v.as_object_mut() {
                // Tier-3 disclosure rides ALONGSIDE the full shape, never
                // instead of it: the zeros are real, and `note` says why they
                // are zeros rather than leaving the caller to guess whether the
                // feature is off, the file is missing, or nothing happened yet.
                obj.insert(
                    "note".to_string(),
                    serde_json::json!(if usage_missing {
                        format!("no usage data at {}", usage_path.display())
                    } else {
                        format!("no sessions recorded in {}", usage_path.display())
                    }),
                );
            }
            sout!("{}", v);
        } else if usage_missing {
            eprintln!("No usage data yet at {}", usage_path.display());
            eprintln!("Run an MCP session first (sessions flush metrics on EOF).");
        } else {
            eprintln!("No sessions recorded.");
        }
        return Ok(());
    }

    if json_mode {
        sout!("{}", build_stats_json(&summary, &recs, rec_state));
    } else {
        render_stats_text(&summary, &recs, rec_exists)?;
    }

    Ok(())
}

// --- grep subcommand ---
