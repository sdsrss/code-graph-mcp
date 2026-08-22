use super::*;

/// Append a `{hook:"cli",action:"use",cmd}` line to recommendations.jsonl so the
/// deny→use funnel can see model-initiated CLI conversions (the 2026-06-12 daagu
/// night: 3 post-deny CLI calls, all invisible to the funnel). Mirrors the JS
/// recordRecommendation posture: best-effort, NEVER creates `.code-graph/`
/// (zero footprint outside indexed projects). Hook-internal answer runs set
/// `CODE_GRAPH_INTERNAL=1` and are skipped — they are deliveries, not conversions.
pub fn record_cli_use(project_root: &Path, cmd: &str) {
    if std::env::var("CODE_GRAPH_INTERNAL").ok().as_deref() == Some("1") {
        return;
    }
    let dir = project_root.join(CODE_GRAPH_DIR);
    if !dir.is_dir() {
        return;
    }
    // Opt-in per-project metrics silence. A `.code-graph/.no-metrics` sentinel marks
    // a development/dogfood checkout where the tool's OWN CLI is run for functionality
    // testing, sims, or ad-hoc dev — those runs would otherwise append `use` events
    // to the project's own recommendations.jsonl and read back as genuine consumer
    // adoption (the 2026-06-23 self-pollution: 184 burst rows from in-repo CLI runs).
    // Guards ONLY this recommendations-log write; MCP usage.jsonl (flush_metrics) is
    // untouched, so a dev repo's real MCP tool metrics still flow. Mirrored in JS
    // recommendation-log.js. Reversible: delete the file to re-enable.
    if dir.join(NO_METRICS_SENTINEL).exists() {
        return;
    }
    let line = serde_json::json!({
        "ts": crate::utils::telemetry::iso8601_now(),
        "hook": "cli",
        "action": "use",
        "cmd": cmd,
    });
    let rec_path = dir.join("recommendations.jsonl");
    // Bounded growth: recommendations.jsonl is append-only and (unlike
    // usage.jsonl) written per-event from both here and the JS PreToolUse hooks,
    // so rotate before appending. Same policy/constants as usage.jsonl.
    crate::utils::telemetry::rotate_jsonl_if_over(
        &rec_path,
        crate::utils::telemetry::JSONL_ROTATE_MAX_BYTES,
        crate::utils::telemetry::JSONL_ROTATE_KEEP_BYTES,
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rec_path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{}", line);
    }
}

/// Aggregated per-tool counts across sessions.
pub struct ToolAgg {
    pub n: u64,
    pub total_ms: u64,
    pub err: u64,
    pub max_ms: u64,
    /// Sum of per-session `err_kinds` maps (ErrKind::as_str() → count). Empty for
    /// tools whose errors all predate the err_kinds field. `sum(err_kinds) <= err`;
    /// the gap is pre-feature sessions that logged `err` without a breakdown.
    pub err_kinds: HashMap<String, u64>,
    /// First non-empty `other_sample` seen across sessions — one representative
    /// message for the catch-all `other` bucket, so it is self-explaining.
    pub other_sample: Option<String>,
}

/// Summary produced by `aggregate_usage_jsonl` — drives both human + JSON output.
pub struct UsageSummary {
    pub sessions: u64,
    pub parse_errors: u64,
    pub tools: HashMap<String, ToolAgg>,
    pub search_queries: u64,
    pub search_zero: u64,
    pub search_quality_weighted_sum: f64,
    pub search_fts_only: u64,
    pub search_hybrid: u64,
    pub full_index_count: u64,
    pub full_index_ms_sum: u64,
    pub incr_count: u64,
    pub files_indexed: u64,
    pub versions: std::collections::BTreeSet<String>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    /// Recommend→use funnel (per-session, window-joined from `recs` field).
    pub sessions_with_deny: u64,
    pub sessions_with_deny_and_cg: u64,
    pub sessions_with_hint: u64,
    pub sessions_with_hint_and_cg: u64,
    /// CLI-conversion legs (recs.cli_use > 0 in the session window) and the
    /// combined "any use" legs (MCP cg tool OR CLI query) — the honest funnel
    /// numerator now that deny→CLI is the proven conversion path.
    pub sessions_with_deny_and_cli: u64,
    pub sessions_with_hint_and_cli: u64,
    pub sessions_with_deny_and_use: u64,
    pub sessions_with_hint_and_use: u64,
}

impl UsageSummary {
    pub fn total_tool_calls(&self) -> u64 {
        self.tools.values().map(|a| a.n).sum()
    }
}

/// Code-understanding cg tools the DENY hook steers grep toward. Housekeeping
/// tools (start/stop_watch, get_index_status, rebuild_index) are excluded so the
/// funnel measures real "used cg instead of grep" substitution, not background
/// bookkeeping. Every entry must be dispatchable — `cg_query_tools_are_all_dispatchable`
/// (tests/hardening.rs) fails the build if one is not.
pub const CG_QUERY_TOOLS: &[&str] = &[
    "get_call_graph",
    "get_ast_node",
    "module_overview",
    "semantic_code_search",
    "ast_search",
    "find_references",
    "project_map",
    // NOTE: entries here must be names `McpServer::dispatch_tool` actually
    // answers — `cg_query_tools_are_all_dispatchable` (tests/hardening.rs)
    // enforces it. `impact_analysis` sat here as dead configuration long after
    // the tool stopped existing; it could never match a usage.jsonl key, so it
    // silently contributed nothing to the funnel it is supposed to measure.
    "trace_http_chain",
    "dependency_graph",
    "find_similar_code",
    "find_dead_code",
    "find_http_route",
    "read_snippet",
];

/// Per-session funnel conversion = `num/denom` rounded to 2 decimals, or JSON
/// `null` when the bucket is empty (avoids a misleading 0.0 for "no data").
pub(crate) fn session_conversion(num: u64, denom: u64) -> serde_json::Value {
    if denom == 0 {
        serde_json::Value::Null
    } else {
        serde_json::json!((num as f64 / denom as f64 * 100.0).round() / 100.0)
    }
}

/// Parse and aggregate `.code-graph/usage.jsonl` content.
/// Pure function: no IO, no panics — malformed lines are counted, not fatal.
/// `last_n`: if Some, keep only the last N records before aggregating.
pub fn aggregate_usage_jsonl(content: &str, last_n: Option<usize>) -> UsageSummary {
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut parse_errors: u64 = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(v) => records.push(v),
            Err(_) => parse_errors += 1,
        }
    }
    if let Some(n) = last_n {
        if records.len() > n {
            let drop = records.len() - n;
            records.drain(..drop);
        }
    }

    let mut summary = UsageSummary {
        sessions: records.len() as u64,
        parse_errors,
        tools: HashMap::new(),
        search_queries: 0,
        search_zero: 0,
        search_quality_weighted_sum: 0.0,
        search_fts_only: 0,
        search_hybrid: 0,
        full_index_count: 0,
        full_index_ms_sum: 0,
        incr_count: 0,
        files_indexed: 0,
        versions: std::collections::BTreeSet::new(),
        first_ts: None,
        last_ts: None,
        sessions_with_deny: 0,
        sessions_with_deny_and_cg: 0,
        sessions_with_hint: 0,
        sessions_with_hint_and_cg: 0,
        sessions_with_deny_and_cli: 0,
        sessions_with_hint_and_cli: 0,
        sessions_with_deny_and_use: 0,
        sessions_with_hint_and_use: 0,
    };

    for rec in &records {
        if let Some(v) = rec.get("v").and_then(|v| v.as_str()) {
            summary.versions.insert(v.to_string());
        }
        if let Some(ts) = rec.get("ts").and_then(|v| v.as_str()) {
            if summary.first_ts.is_none() {
                summary.first_ts = Some(ts.to_string());
            }
            summary.last_ts = Some(ts.to_string());
        }
        if let Some(tools_obj) = rec.get("tools").and_then(|v| v.as_object()) {
            for (name, s) in tools_obj {
                let agg = summary.tools.entry(name.clone()).or_insert(ToolAgg {
                    n: 0,
                    total_ms: 0,
                    err: 0,
                    max_ms: 0,
                    err_kinds: HashMap::new(),
                    other_sample: None,
                });
                agg.n += s.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.total_ms += s.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.err += s.get("err").and_then(|v| v.as_u64()).unwrap_or(0);
                let m = s.get("max_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                if m > agg.max_ms {
                    agg.max_ms = m;
                }
                // Merge the per-session err_kinds breakdown (additive; absent on
                // pre-feature rows, so `sum(err_kinds)` may trail `err`).
                if let Some(ek) = s.get("err_kinds").and_then(|v| v.as_object()) {
                    for (kind, cnt) in ek {
                        *agg.err_kinds.entry(kind.clone()).or_insert(0) +=
                            cnt.as_u64().unwrap_or(0);
                    }
                }
                // First `other_sample` wins — one representative message is enough.
                if agg.other_sample.is_none() {
                    if let Some(sample) = s.get("other_sample").and_then(|v| v.as_str()) {
                        agg.other_sample = Some(sample.to_string());
                    }
                }
            }
        }
        if let Some(s) = rec.get("search") {
            let q = s.get("queries").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_queries += q;
            summary.search_zero += s.get("zero").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_fts_only += s.get("fts_only").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_hybrid += s.get("hybrid").and_then(|v| v.as_u64()).unwrap_or(0);
            // Per-session avg_quality → re-weight by query count to merge.
            let avg = s.get("avg_quality").and_then(|v| v.as_f64()).unwrap_or(0.0);
            summary.search_quality_weighted_sum += avg * q as f64;
        }
        if let Some(idx) = rec.get("index") {
            if let Some(ms) = idx.get("full_ms").and_then(|v| v.as_u64()) {
                summary.full_index_count += 1;
                summary.full_index_ms_sum += ms;
            }
            summary.incr_count += idx.get("incr").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.files_indexed += idx.get("files").and_then(|v| v.as_u64()).unwrap_or(0);
        }
        // Recommend→use funnel: per-session, did a session that saw a deny/hint
        // (window-joined into the `recs` field at flush) also call a cg query tool?
        let used_cg = rec
            .get("tools")
            .and_then(|v| v.as_object())
            .is_some_and(|tools| {
                CG_QUERY_TOOLS.iter().any(|t| {
                    tools
                        .get(*t)
                        .and_then(|s| s.get("n"))
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0)
                        > 0
                })
            });
        if let Some(recs) = rec.get("recs") {
            let deny = recs.get("deny").and_then(|v| v.as_u64()).unwrap_or(0);
            let hint = recs.get("hint").and_then(|v| v.as_u64()).unwrap_or(0);
            // CLI query runs window-joined into the session (additive v0.49 field).
            let used_cli = recs.get("cli_use").and_then(|v| v.as_u64()).unwrap_or(0) > 0;
            let used_any = used_cg || used_cli;
            if deny > 0 {
                summary.sessions_with_deny += 1;
                if used_cg {
                    summary.sessions_with_deny_and_cg += 1;
                }
                if used_cli {
                    summary.sessions_with_deny_and_cli += 1;
                }
                if used_any {
                    summary.sessions_with_deny_and_use += 1;
                }
            }
            if hint > 0 {
                summary.sessions_with_hint += 1;
                if used_cg {
                    summary.sessions_with_hint_and_cg += 1;
                }
                if used_cli {
                    summary.sessions_with_hint_and_cli += 1;
                }
                if used_any {
                    summary.sessions_with_hint_and_use += 1;
                }
            }
        }
    }
    summary
}

/// Aggregate of `.code-graph/recommendations.jsonl` — the JS PreToolUse hooks'
/// record of how often a code-graph tool was RECOMMENDED (raw-grep hint/deny,
/// read-fanout hint). Joined against actual tool calls in `stats` to surface the
/// real-session conversion rate the synthetic routing_bench oracle can't see.
#[derive(Default)]
pub struct RecommendationSummary {
    /// Recommendation events only (deny/hint/bypass…) — `action:"use"` lines are
    /// conversions, counted in `cli_uses` instead.
    pub total: u64,
    /// "hint" / "deny" / "bypass" → count
    pub by_action: std::collections::BTreeMap<String, u64>,
    /// "grep" / "read" → count
    pub by_hook: std::collections::BTreeMap<String, u64>,
    /// Model-initiated `code-graph-mcp <query>` runs (action:"use").
    pub cli_uses: u64,
    /// Deny segmentation: answered:true denies satisfied the need in-place, so a
    /// low deny→use read is EXPECTED for them; only static (unanswered) denies
    /// ask the model to convert. Pre-v0.47 denies lack the field → unanswered.
    pub deny_answered: u64,
    pub deny_unanswered: u64,
    /// Outcome proxy ("search-decay"): silent grep/read allows recorded by the
    /// PreToolUse hooks (action:"observe"), so the model's raw fan-out is visible
    /// alongside the deny/hint events.
    pub observe: u64,
    /// SessionStart "live context" injections (action:"live_impact", hook:"session"):
    /// the recent-change blast radius pushed at session start (v0.63). A separate
    /// counter — like observe/use it is NOT a tool-call recommendation, so it stays
    /// out of `total`/`by_action`. Surfaced in stats so the feature isn't dark.
    pub live_impact: u64,
    /// Per-mode breakdown of PostToolUse inject events (mode:"callgraph"|"grep"|"show").
    /// A SUB-breakdown of the `inject` count in `by_action` (NOT double-counted; an
    /// inject event still lands in total/by_action once). callgraph is the only inject
    /// mode with marginal value over the model's own grep — it carries the cross-file
    /// caller tree; grep/show echo the hits the model already saw (2026-06-26 audit:
    /// 0 CONSUMED). Surfaced so the callgraph-vs-echo mix is directly readable — the
    /// lever is raising callgraph's share (widening its eligibility). Injects recorded
    /// without a `mode` (pre-v0.75) are absent from every bucket, so the map may sum to
    /// less than `by_action["inject"]`.
    ///
    /// DELIVERED ONLY. Skipped injects (`answered:false`) also carry a `mode`
    /// since D#147, and they are counted in `inject_skipped_by_mode` instead —
    /// folding them in here would silently redefine the callgraph SHARE that
    /// `stats` prints from "of what cg delivered" to "of what cg attempted",
    /// breaking comparability against every figure recorded before the split.
    pub inject_by_mode: std::collections::BTreeMap<String, u64>,
    /// Per-mode breakdown of the injects that did NOT deliver — the `answered:false`
    /// rows counted in `inject_skipped`, keyed by the mode that burned the attempt
    /// (the LAST mode tried: callgraph → show/grep, mirroring the hook's own
    /// fallback order). Without it the funnel can see THAT injects fail but not
    /// WHICH mode is failing, which is the attribution the gate work needed
    /// (measured 2026-08-19: 13 of 74 mem injects and 39 of 96 daagu injects were
    /// skips aggregating under one unattributable bucket).
    ///
    /// Skips recorded before the fix carry no `mode` → absent from every bucket,
    /// so this map may sum to less than `inject_skipped`. That gap is deliberate:
    /// it keeps unattributable history visible instead of misfiling it.
    pub inject_skipped_by_mode: std::collections::BTreeMap<String, u64>,
    /// Of `deny_answered` (cg delivered a grep answer in-place), how many were
    /// IMMEDIATELY followed by ANY grep/read event. Computed in append
    /// (chronological) order; a single-user-sequential approximation (truly
    /// concurrent sessions interleave in the shared file). NOTE: this raw count
    /// is NOT a failure rate — it lumps together healthy drill-down that cg also
    /// answered (`sustained_after_answer`), file-reads acting on the answer
    /// (observe), and genuine fall-through (`fallthrough_after_answer`). Only the
    /// last means the inline answer was insufficient. Read `fallthrough_after_answer`
    /// for the honest signal.
    pub researched_after_answer: u64,
    /// Subset of `researched_after_answer`: the follow-up search was ITSELF
    /// answered by cg (an answered deny / delivered hint) AND searched a DIFFERENT
    /// pattern, so the model drilled deeper and cg kept up — each step replaced
    /// another raw grep with an answer. A win, not a miss. A verbatim re-grep of
    /// the SAME pattern is excluded (scored as fall-through) when the hook recorded
    /// the pattern; pre-fix events without a pattern field still land here (the old
    /// upper-bound behavior, back-compatible).
    pub sustained_after_answer: u64,
    /// Subset of `researched_after_answer`: the follow-up was a search cg could
    /// NOT satisfy (static deny / advisory-only hint / bypass). THIS is the honest
    /// "the inline answer was insufficient and cg couldn't help the next step"
    /// signal — the actual fan-out leak. `observe` (a file read acting on the
    /// delivered answer) is excluded from both subsets: it is not a search cg failed.
    pub fallthrough_after_answer: u64,
    /// Subset of `researched_after_answer` EXCLUDED from both sustained AND
    /// fall-through: the follow-up search is itself a NULL signal about the prior
    /// answer's sufficiency. Two shapes: `fallthrough:"no-hits"` (cg ran the next
    /// grep and found nothing — necessarily a DIFFERENT query, since a verbatim
    /// re-grep of the answered pattern would re-hit the prior answer's lines, so
    /// 0 hits ⇒ a new search, not "the answer was wrong") and `reason:"unavailable"`
    /// (cg CLI couldn't run — infra, orthogonal to answer quality). Counting either
    /// as fall-through over-states "answer insufficient" — the same over-count class
    /// as lumping in drill-down/observe (v0.64). Tracked so the named subsets of
    /// `researched_after_answer` stay legible.
    pub followup_inconclusive: u64,
    /// PostToolUse inject events that did NOT deliver (answered:false — cg ran but
    /// hit no-hits/no-binary/unavailable, recorded with `fallthrough`/`reason`).
    /// Before this counter the non-hits path recorded NOTHING, so the funnel could
    /// not distinguish "hook dark (binary missing)" from "ran, genuinely empty"
    /// (disclosure-gap class, roadmap 2026-07-18 §1.6). A sub-breakdown of
    /// `by_action["inject"]`; `by_action["inject"] - inject_skipped` = delivered.
    pub inject_skipped: u64,
}

/// Parse and aggregate `recommendations.jsonl` content. Pure: no IO, no panics —
/// malformed lines are skipped silently (telemetry, not a contract surface).
pub fn aggregate_recommendations_jsonl(content: &str) -> RecommendationSummary {
    let mut s = RecommendationSummary::default();
    // Outcome-proxy state: `armed` means the previous tool event was an answered
    // deny (cg satisfied the grep in-place); the next grep/read event of ANY
    // action is a re-search — the inline answer wasn't enough. Lines are appended
    // chronologically so a single forward pass suffices.
    let mut armed = false;
    // Pattern of the armed answered deny (when the hook recorded one). A follow-up
    // search carrying the SAME pattern is a verbatim re-grep = the inline answer was
    // ignored/insufficient (a real fall-through), NOT a deeper drill-down. Absent on
    // pre-fix events → falls back to the answered/observe split (back-compatible).
    let mut armed_pattern: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let action = v.get("action").and_then(|x| x.as_str());
        let hook = v.get("hook").and_then(|x| x.as_str());

        // Re-search detection runs on every tool event, before action bucketing.
        let is_search_event = matches!(hook, Some("grep") | Some("read"))
            && matches!(
                action,
                Some("deny") | Some("hint") | Some("bypass") | Some("observe") | Some("inject")
            );
        if armed {
            if is_search_event {
                s.researched_after_answer += 1;
                let follow_pattern = v.get("pattern").and_then(|x| x.as_str());
                // Split the follow-up honestly. Same-pattern takes precedence: a
                // verbatim re-grep of the SAME denied pattern (re-deny after the
                // cooldown, or a grep observe within it) means the inline answer
                // didn't end the hunt for THAT query → fall-through, NOT a win and
                // NOT "acting on the answer". Otherwise: observe = a file read
                // acting on the delivered answer; answered:true = cg ALSO answered
                // the next (deeper) step (sustained drill-down, a win); anything
                // else (static deny / advisory hint / bypass) = cg fell through.
                // The is_some() guard keeps absent==absent (pre-fix events) OUT of
                // the same-pattern branch.
                let follow_inconclusive = v.get("fallthrough").and_then(|x| x.as_str())
                    == Some("no-hits")
                    || v.get("reason").and_then(|x| x.as_str()) == Some("unavailable");
                if armed_pattern.is_some() && armed_pattern.as_deref() == follow_pattern {
                    s.fallthrough_after_answer += 1;
                } else if follow_inconclusive {
                    // The follow-up is a NULL signal about the prior answer: `no-hits`
                    // = cg ran the next grep and found nothing (a verbatim re-grep of
                    // the answered pattern would have re-hit it, so 0 hits ⇒ a NEW
                    // query, not "the answer was wrong"); `unavailable` = cg CLI
                    // couldn't run (infra). Neither means the inline answer was
                    // insufficient → exclude from fall-through (same over-count class
                    // as the observe/drill-down split). Ordered after the same-pattern
                    // check so a verbatim re-grep still scores as fall-through.
                    s.followup_inconclusive += 1;
                } else if action == Some("observe") {
                    // acting on the answer — neither sustained nor fall-through
                } else if v.get("answered").and_then(|x| x.as_bool()) == Some(true) {
                    s.sustained_after_answer += 1;
                } else {
                    s.fallthrough_after_answer += 1;
                }
            }
            armed = false; // only the IMMEDIATELY-next tool event counts
            armed_pattern = None;
        }

        // observe / use are not recommendation events: count separately, like cli use.
        match action {
            Some("use") => {
                s.cli_uses += 1;
                continue;
            }
            Some("observe") => {
                s.observe += 1;
                continue;
            }
            Some("live_impact") => {
                s.live_impact += 1;
                continue;
            }
            _ => {}
        }
        s.total += 1;
        if let Some(a) = action {
            *s.by_action.entry(a.to_string()).or_insert(0) += 1;
            if a == "deny" {
                if v.get("answered").and_then(|x| x.as_bool()) == Some(true) {
                    s.deny_answered += 1;
                    armed = true; // watch the next event for a re-search
                                  // Remember the pattern (if recorded) so a verbatim re-grep of it
                                  // is scored as fall-through, not sustained.
                    armed_pattern = v.get("pattern").and_then(|x| x.as_str()).map(String::from);
                } else {
                    s.deny_unanswered += 1;
                }
            } else if a == "inject" {
                // Compound-grep PostToolUse: an answered inject delivered cg's
                // AST-aware view of a grep that rode inside a compound command
                // (so PreToolUse never denied it). It arms the funnel exactly like
                // an answered deny — the next search event scores whether the
                // inline inject sufficed (inject→fallthrough) or cg also answered
                // the deeper step (sustained), parallel to deny→fallthrough.
                // answered:false injects (v0.99.1+) are the non-delivering path
                // (no-hits / no-binary / unavailable) — counted in inject_skipped
                // below, and they do NOT arm the funnel. Still land in
                // total/by_action via the generic map above.
                // Sub-breakdown by payload mode (best-effort: pre-v0.75 injects have
                // no `mode` → uncounted here, still counted in by_action["inject"]).
                // Skips carry a mode too (D#147) and go to their OWN map: `inject_by_mode`
                // feeds the delivered-payload mix and its callgraph share, which a
                // failed attempt did not contribute to. The delivered test is
                // "not explicitly false" so v0.75..v0.99.1 rows — a `mode`, no
                // `answered`, recorded only on hits — keep counting as delivered.
                let answered = v.get("answered").and_then(|x| x.as_bool());
                if let Some(mode) = v.get("mode").and_then(|x| x.as_str()) {
                    let bucket = if answered == Some(false) {
                        &mut s.inject_skipped_by_mode
                    } else {
                        &mut s.inject_by_mode
                    };
                    *bucket.entry(mode.to_string()).or_insert(0) += 1;
                }
                if answered == Some(true) {
                    armed = true;
                    armed_pattern = v.get("pattern").and_then(|x| x.as_str()).map(String::from);
                } else if answered == Some(false) {
                    // Explicit answered:false only — pre-v0.99.1 injects lack the
                    // field but were ALWAYS delivered (recorded only on hits).
                    s.inject_skipped += 1;
                }
            }
        }
        if let Some(h) = hook {
            *s.by_hook.entry(h.to_string()).or_insert(0) += 1;
        }
    }
    s
}
