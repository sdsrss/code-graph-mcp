use anyhow::{anyhow, Result};
use serde_json::json;

use crate::storage::queries;

use super::COMPRESSION_TOKEN_THRESHOLD;

/// Extract a required string argument, trimming whitespace and rejecting empty values.
pub(super) fn required_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    let s = args[key]
        .as_str()
        .ok_or_else(|| anyhow!("{} is required", key))?
        .trim();
    if s.is_empty() {
        return Err(anyhow!("{} must not be empty", key));
    }
    Ok(s)
}

/// One-line, bounded description of what a caller actually sent, for error text.
///
/// The echoed string is cut at the FIRST WHITESPACE, not merely truncated —
/// pre-tag review found the reason. This text flows into `ErrKind::classify`,
/// which buckets errors by substring match, so a caller could pick its own
/// telemetry bucket: `{"depth": "Ambiguous symbol"}` would be recorded as
/// `ambiguous` instead of `bad_param`, quietly poisoning the metric this very
/// change exists to sharpen. Every phrase `classify` looks for is multi-word, so
/// a single token cannot spell one. Reordering `classify` instead would only
/// mirror the problem — a `symbol_name` of "must be an integer" would then land
/// in `bad_param` rather than `not_found`. Closing the injection beats ranking it.
///
/// The useful case survives intact: `"50"`, the spelling this whole fix is about,
/// is one token and echoes in full.
fn describe_arg(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            let first_token = s.split_whitespace().next().unwrap_or("");
            let mut shown: String = first_token.chars().take(40).collect();
            if shown.len() < s.len() {
                shown.push('…');
            }
            format!("the string \"{shown}\"")
        }
        serde_json::Value::Bool(b) => format!("the boolean {b}"),
        serde_json::Value::Number(n) => format!("{n}"),
        serde_json::Value::Array(_) => "an array".to_string(),
        serde_json::Value::Object(_) => "an object".to_string(),
        serde_json::Value::Null => "null".to_string(),
    }
}

/// Read a COUNT-LIKE numeric argument (a count, a depth, a line span), defaulting
/// only when it is genuinely absent and refusing anything a count cannot be.
///
/// CON-15 (audit 2026-08-29). Every numeric argument used to be read as
/// `args[key].as_i64().unwrap_or(default)`, which cannot tell "not sent" from
/// "sent as the wrong type": `{"limit": "50"}` — a plausible spelling for a model
/// filling a JSON schema — succeeded and quietly returned the default 20 results
/// with nothing in the response saying so. The string-enum half of this same defect
/// class was already fixed at every entry (bad `direction` / `node_type` / relation
/// values are rejected and the vocabulary is listed); this is the numeric half,
/// which was never done. Rejecting rather than coercing is what keeps the two
/// halves symmetric — and an error a model can read is worth more than a silent
/// answer to a question it did not ask.
///
/// It also refuses a NEGATIVE, and that half used to depend on which helper a
/// given argument happened to call. The `arg_i64` twin that stood here accepted
/// `-3` and handed it straight to `.clamp(lo, hi)`, so `depth: -5` silently
/// became `1` and `context_lines: -7` became `0` — the same "answered a question
/// you did not ask" shape CON-15 exists to close, reached by a different road.
/// Six of the thirteen count arguments already read through this `u64` side and
/// seven read through the twin, so the rule was real for six and absent for the
/// rest — and the regression test covered only two of the six, which is how the
/// split survived a pass that was specifically about numeric arguments. Every
/// count now reads through here; `arg_i64` had no callers left and was removed
/// (its absence is the guard: a new count argument cannot reach the lenient
/// helper, because there is no longer one to reach). Arguments that
/// are NOT counts keep their own helpers — [`arg_opt_i64`] for `node_id` (an id,
/// where absence selects a different code path) and [`arg_f64`] for
/// `max_distance` (a similarity threshold, not a quantity).
pub(super) fn arg_u64(args: &serde_json::Value, key: &str, default: u64) -> Result<u64> {
    match &args[key] {
        serde_json::Value::Null => Ok(default),
        v => v.as_u64().ok_or_else(|| {
            anyhow!(
                "{key} must be a non-negative integer (got {}); the request was rejected rather than answered with a default",
                describe_arg(v)
            )
        }),
    }
}

/// The clamp range of every count-like argument, keyed by (canonical tool, arg).
///
/// SINGLE SOURCE, and that is the whole point. Handlers read their bounds from
/// [`arg_clamped`] instead of spelling `.clamp(1, 20)` inline, and
/// `note_clamped_arguments` reports against these same rows — so the range that
/// is ENFORCED and the range that is DISCLOSED cannot drift apart. A table that
/// merely restated the literals would be a second copy to keep in sync, which is
/// the failure this repo has filed as `source-scanning-guard`.
///
/// Tool names are canonical: `trace_http_chain` and `read_snippet` are aliases
/// that `dispatch_tool` folds into `find_http_route` and `get_ast_node`, so they
/// do not get their own rows — see [`canonical_tool`].
///
/// `module_overview.deps_depth` is the odd row: the bound ORIGINATES one tool
/// away. The value is forwarded to `dependency_graph`'s `depth`, which clamps to
/// 1..=10, and that is where the number comes from. Since this batch it is also
/// clamped here at the read — a no-op on the result, and what makes the
/// disclosure possible at the tool the caller actually named.
pub(super) const COUNT_RANGES: &[(&str, &str, u64, u64)] = &[
    // Taken from the traversal's own cap, NOT restated. These two rows said 20
    // while `graph::query` stopped at 10, so a `depth: 30` call answered
    // `"applied": 20` and `"effective_max_depth": 10` in the same object — a
    // disclosure naming a value the code never used, which is worse than no
    // disclosure and is the thing this whole field exists to prevent. Deriving
    // is also what keeps batch B honest: the published schema text has to come
    // from the constant that enforces the bound, not from a second copy.
    (
        "get_call_graph",
        "depth",
        1,
        crate::graph::query::CALL_GRAPH_MAX_DEPTH as u64,
    ),
    (
        "find_http_route",
        "depth",
        1,
        crate::graph::query::CALL_GRAPH_MAX_DEPTH as u64,
    ),
    ("dependency_graph", "depth", 1, 10),
    ("module_overview", "deps_depth", 1, 10),
    ("find_similar_code", "top_k", 1, 100),
    ("get_ast_node", "similar_top_k", 1, 50),
    ("get_ast_node", "context_lines", 0, 100),
    ("ast_search", "limit", 1, 100),
    ("project_map", "centrality_limit", 1, 100),
    ("semantic_code_search", "top_k", 1, 100),
    ("semantic_code_search", "limit", 1, 100),
];

/// Fold a dispatch alias onto the name [`COUNT_RANGES`] is keyed by.
///
/// `dispatch_tool` maps two names onto one handler each. Without this, a call
/// spelled `trace_http_chain` would clamp exactly as `find_http_route` does and
/// then disclose nothing, which is the silent-truncation bug wearing the other
/// name.
pub(super) fn canonical_tool(name: &str) -> &str {
    match name {
        "trace_http_chain" => "find_http_route",
        "read_snippet" => "get_ast_node",
        other => other,
    }
}

/// The bound for `(tool, key)` as the published schema says it, e.g. `range 1-100`.
///
/// `mcp::tools` builds every count argument's `description` through this instead
/// of typing the numbers a second time. That direction matters: the audit trail
/// on this file already carries two cases where a restated bound and the enforced
/// bound diverged — `get_call_graph.depth` advertised 20 against a traversal that
/// stopped at 10, and the range table itself was written from the inline literals
/// rather than from the constant. A published description is a third copy, read by
/// a model that cannot check it, so it is derived here or it is not written.
///
/// A `(tool, key)` with no row is a caller bug, not a runtime condition:
/// `ToolRegistry::new()` runs in every test binary, so the debug assertion fires
/// the moment a description asks for a bound that no longer exists.
pub(crate) fn count_range_hint(tool: &str, key: &str) -> String {
    match count_range(tool, key) {
        Some((lo, hi)) => format!("range {lo}-{hi}"),
        None => {
            debug_assert!(
                false,
                "{tool}.{key} has no COUNT_RANGES row, so its schema description \
                 cannot state a bound"
            );
            String::new()
        }
    }
}

/// The clamp range for `(tool, key)`, or `None` when the argument is not clamped.
pub(super) fn count_range(tool: &str, key: &str) -> Option<(u64, u64)> {
    let tool = canonical_tool(tool);
    COUNT_RANGES
        .iter()
        .find(|(t, k, _, _)| *t == tool && *k == key)
        .map(|(_, _, lo, hi)| (*lo, *hi))
}

/// [`arg_u64`] plus the clamp, with the bounds taken from [`COUNT_RANGES`] rather
/// than written at the call site.
///
/// Argument order is `(args, key, tool, …)` on purpose: two source-scanning
/// guards in `tests/hardening.rs` read the first string literal after `args, `
/// as the argument name. With the tool first they read the TOOL name instead —
/// `test_no_new_undeclared_mcp_args` then reported `get_call_graph` as an
/// undeclared argument. Teaching each guard a new shape would have been the
/// other fix; keeping the shape they already parse is the one that does not
/// need every future guard to learn it too.
///
/// Panics in debug builds if `(tool, key)` has no row — a handler that clamps an
/// argument the table does not know about is the drift this design exists to
/// prevent, and it should fail loudly in tests rather than silently skip
/// disclosure in production. Release builds fall back to the raw value.
pub(super) fn arg_clamped(
    args: &serde_json::Value,
    key: &str,
    tool: &str,
    default: u64,
) -> Result<u64> {
    let raw = arg_u64(args, key, default)?;
    match count_range(tool, key) {
        Some((lo, hi)) => Ok(raw.clamp(lo, hi)),
        None => {
            debug_assert!(false, "{tool}.{key} is clamped but has no COUNT_RANGES row");
            // Release falls back to the DEFAULT, not to `raw`. A missing row means
            // the bound is unknown, and handing back the caller's unbounded number
            // is the wrong direction for a published server — `count_range` also
            // returns `None` there, so `note_clamped_arguments` would disclose
            // nothing about it. The default is our own constant and is in range by
            // construction. `every_numeric_mcp_argument_is_clamped` makes the case
            // statically unreachable; this is what happens if it ever is reached.
            Ok(default)
        }
    }
}

/// [`arg_u64`] for arguments with no default, where absence selects a different
/// code path (`node_id` picks id-lookup over name-lookup). Same rule: absent is
/// `None`, wrong type is an error — not a third silent spelling of absent.
pub(super) fn arg_opt_i64(args: &serde_json::Value, key: &str) -> Result<Option<i64>> {
    match &args[key] {
        serde_json::Value::Null => Ok(None),
        v => v.as_i64().map(Some).ok_or_else(|| {
            anyhow!(
                "{key} must be an integer (got {}); the request was rejected rather than answered with a default",
                describe_arg(v)
            )
        }),
    }
}

/// [`arg_u64`] for boolean flags — the other half of CON-15, which fixed only the
/// numeric arguments (audit 2026-09-02 P2-2). `args[key].as_bool().unwrap_or(d)`
/// has exactly the numeric defect: `{"compact": "true"}` returned the full
/// uncompacted envelope and `{"include_tests": 1}` dropped the tests, both with
/// nothing in the response saying the argument had been discarded.
///
/// `note_ignored_arguments` stays silent about it either way, but for two
/// different reasons — on the seven registry tools because the key IS declared,
/// and on the six schema-less ones (`dependency_graph`, `find_dead_code`,
/// `find_similar_code`, `find_http_route`, `trace_http_chain`, `rebuild_index`)
/// because a tool with no published schema is skipped before
/// `HONORED_UNDECLARED_ARGS` is consulted. The first cut of this comment gave
/// only the first reason, which covers half the sites.
///
/// Only `true`/`false` are accepted. `"true"` and `1` are NOT coerced: a
/// coercion here would have to guess, and the numeric half already established
/// that an error a model can read beats a silent answer to a question it did not
/// ask.
pub(super) fn arg_bool(args: &serde_json::Value, key: &str, default: bool) -> Result<bool> {
    match &args[key] {
        serde_json::Value::Null => Ok(default),
        v => v.as_bool().ok_or_else(|| {
            anyhow!(
                "{key} must be a boolean (got {}); the request was rejected rather than answered with a default",
                describe_arg(v)
            )
        }),
    }
}

/// [`arg_u64`] for fractional arguments (similarity thresholds).
pub(super) fn arg_f64(args: &serde_json::Value, key: &str, default: f64) -> Result<f64> {
    match &args[key] {
        serde_json::Value::Null => Ok(default),
        v => v.as_f64().ok_or_else(|| {
            anyhow!(
                "{key} must be a number (got {}); the request was rejected rather than answered with a default",
                describe_arg(v)
            )
        }),
    }
}

/// Parse route input like "GET /api/users" into (Some("GET"), "/api/users").
/// If no method prefix, returns (None, original_path).
pub(super) fn parse_route_input(input: &str) -> (Option<String>, &str) {
    let trimmed = input.trim();
    if let Some(space_idx) = trimmed.find(' ') {
        let prefix = &trimmed[..space_idx];
        let methods = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
        if methods.contains(&prefix.to_uppercase().as_str()) {
            return (Some(prefix.to_uppercase()), trimmed[space_idx..].trim());
        }
    }
    (None, trimmed)
}

/// Filter route matches by HTTP method from metadata JSON.
pub(super) fn filter_routes_by_method(
    rows: &mut Vec<queries::RouteMatch>,
    method: &Option<String>,
) {
    if let Some(method) = method {
        rows.retain(|r| {
            r.metadata.as_ref().is_some_and(|m| {
                serde_json::from_str::<serde_json::Value>(m)
                    .ok()
                    .and_then(|v| {
                        v.get("method")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string())
                    })
                    .is_some_and(|rm| crate::domain::route_method_matches(&rm, method))
            })
        });
    }
}

/// For inline handlers, override handler_name and start/end lines from metadata.
pub(super) fn apply_inline_handler_metadata(
    handler: &mut serde_json::Value,
    metadata: Option<&str>,
) {
    if let Some(meta_str) = metadata {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
            if meta
                .get("inline")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                handler["handler_name"] = json!(format!(
                    "{} {} (inline)",
                    meta.get("method").and_then(|v| v.as_str()).unwrap_or("?"),
                    meta.get("path").and_then(|v| v.as_str()).unwrap_or("?")
                ));
                if let Some(sl) = meta.get("handler_start_line").and_then(|v| v.as_i64()) {
                    handler["start_line"] = json!(sl);
                }
                if let Some(el) = meta.get("handler_end_line").and_then(|v| v.as_i64()) {
                    handler["end_line"] = json!(el);
                }
            }
        }
    }
}

/// Check if the caller requested to skip indexing (read-only mode).
///
/// Fallible for the same reason as [`arg_bool`], which it delegates to: this is
/// the 22nd boolean argument, and the one the flag-by-flag pass over the
/// declared schemas could not see. `skip_indexing` is DECLARED nowhere — it is
/// `("*", "skip_indexing")` in `HONORED_UNDECLARED_ARGS` — so a grep of the
/// tool schemas does not reach it, while every tool reads it through here.
/// Silently reading `{"skip_indexing": "true"}` as `false` made the server go
/// index precisely when the caller had asked it not to, which is the most
/// expensive way to get this particular argument wrong.
pub(super) fn should_skip_indexing(args: &serde_json::Value) -> Result<bool> {
    arg_bool(args, "skip_indexing", false)
}

/// Normalize user-facing type filter aliases to internal AST node types.
/// Delegates to shared domain implementation, logging a warning for unknown inputs.
pub(super) fn normalize_type_filter_mcp(input: &str) -> Vec<String> {
    let result = crate::domain::normalize_type_filter(input);
    if result.is_empty() {
        tracing::warn!(
            "Unknown node type filter: '{}'. Valid: {}.{}",
            input,
            crate::domain::TYPE_FILTER_HELP,
            crate::domain::type_filter_note(input)
        );
    }
    result.into_iter().map(String::from).collect()
}

/// Strip one layer of generic brackets, returning the most informative inner type.
/// Used by `tool_ast_search` to offer "did you mean?" hints when a returns-filter
/// like `Vec<Relation>` yields zero results.
///
/// Examples:
///   "Vec<Relation>"          -> Some("Relation")
///   "Result<Vec<Relation>>"  -> Some("Relation")   (innermost <> wins via rfind)
///   "Result<T, E>"           -> Some("E")          (last comma-separated param)
///   "HashMap<K, V>"          -> Some("V")
///   "&[T]"                   -> None
///   "String"                 -> None
pub(super) fn strip_outer_generic(s: &str) -> Option<String> {
    let last_lt = s.rfind('<')?;
    let after_lt = &s[last_lt + 1..];
    let first_gt = after_lt.find('>')?;
    let inner = after_lt[..first_gt].trim();
    let candidate = inner.rsplit(',').next().unwrap_or(inner).trim();
    if candidate.is_empty() || candidate == s {
        None
    } else {
        Some(candidate.to_string())
    }
}

/// Centralized compression for tool results that exceed the token threshold.
/// Handlers that already produce custom compressed output (with a "mode" key)
/// are left unchanged. For other results, this truncates large string values
/// and adds a `_truncated` marker.
pub(super) fn centralized_compress(value: serde_json::Value) -> serde_json::Value {
    use crate::sandbox::compressor::estimate_json_tokens;
    let tokens = estimate_json_tokens(&value);
    if tokens <= COMPRESSION_TOKEN_THRESHOLD {
        return value;
    }
    // If the handler already produced a compressed result, leave it alone
    if value.get("mode").is_some() {
        return value;
    }
    // Truncate large string values to bring result under threshold
    truncate_large_strings(value, COMPRESSION_TOKEN_THRESHOLD)
}

/// Recursively truncate string values in a JSON value to stay within a token budget.
/// Adds a `_truncated` key to the top-level object when truncation occurs.
pub(super) fn truncate_large_strings(
    value: serde_json::Value,
    token_budget: usize,
) -> serde_json::Value {
    // Target: reduce to roughly token_budget * CHARS_PER_TOKEN chars total
    let target_chars = token_budget * crate::domain::CHARS_PER_TOKEN;
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() <= target_chars {
        return value;
    }

    let mut result = truncate_value(value, target_chars);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("_truncated".to_string(), json!(true));
        obj.insert(
            "_truncation_hint".to_string(),
            json!("Result exceeded token limit. Use get_ast_node(node_id) to read specific nodes."),
        );
    }
    result
}

/// Minimum string length to consider for truncation — short metadata fields are never truncated.
const TRUNCATE_MIN_LEN: usize = 200;

/// Truncate a JSON value's large string fields to fit within a char budget.
/// Only strings longer than TRUNCATE_MIN_LEN are eligible for truncation,
/// preserving short metadata fields (names, types, paths) intact.
/// Recurses into nested objects/arrays up to MAX_TRUNCATE_DEPTH.
const MAX_TRUNCATE_DEPTH: usize = 8;

pub(super) fn truncate_value(value: serde_json::Value, budget: usize) -> serde_json::Value {
    truncate_value_inner(value, budget, 0)
}

fn truncate_value_inner(
    value: serde_json::Value,
    budget: usize,
    depth: usize,
) -> serde_json::Value {
    if depth > MAX_TRUNCATE_DEPTH {
        return value;
    }
    match value {
        serde_json::Value::Object(map) => {
            // Calculate total size of large string fields eligible for truncation
            let large_fields: usize = map
                .values()
                .filter_map(|v| {
                    v.as_str()
                        .filter(|s| s.len() > TRUNCATE_MIN_LEN)
                        .map(|s| s.len())
                })
                .sum();
            let small_fields_size: usize = map
                .iter()
                .map(|(k, v)| {
                    k.len()
                        + match v {
                            serde_json::Value::String(s) if s.len() <= TRUNCATE_MIN_LEN => s.len(),
                            serde_json::Value::String(_) => 0,
                            _ => serde_json::to_string(v).map(|s| s.len()).unwrap_or(0),
                        }
                })
                .sum();
            let large_budget = budget.saturating_sub(small_fields_size);

            // Record array truncations so consumers can reconcile `count`/`total`
            // sibling fields against what was actually returned.
            let mut array_truncations: serde_json::Map<String, serde_json::Value> =
                serde_json::Map::new();

            let truncated: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(k, v)| {
                    let tv = match &v {
                        serde_json::Value::String(s) if s.len() > TRUNCATE_MIN_LEN => {
                            let field_budget = if large_fields > 0 {
                                (large_budget as f64 * s.len() as f64 / large_fields as f64)
                                    as usize
                            } else {
                                large_budget
                            };
                            if s.len() > field_budget {
                                let trunc = &s[..s.floor_char_boundary(field_budget.min(s.len()))];
                                json!(format!("{}... [truncated, {} chars total]", trunc, s.len()))
                            } else {
                                v
                            }
                        }
                        serde_json::Value::Array(arr) if arr.len() > 20 => {
                            let original = arr.len();
                            let mut kept: Vec<serde_json::Value> = arr[..10].to_vec();
                            kept.extend_from_slice(&arr[arr.len() - 5..]);
                            // Keep array homogeneous — consumers can read
                            // `_array_truncations[k]` for the original length.
                            array_truncations.insert(
                                k.clone(),
                                json!({
                                    "original": original,
                                    "kept": kept.len(),
                                }),
                            );
                            serde_json::Value::Array(kept)
                        }
                        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                            truncate_value_inner(v, budget, depth + 1)
                        }
                        _ => v,
                    };
                    (k, tv)
                })
                .collect();

            let mut final_map = truncated;
            if !array_truncations.is_empty() {
                final_map.insert(
                    "_array_truncations".to_string(),
                    serde_json::Value::Object(array_truncations),
                );
            }
            serde_json::Value::Object(final_map)
        }
        serde_json::Value::Array(arr) if arr.len() > 20 => {
            // Top-level array: truncate silently, keep homogeneous. The outer
            // `truncate_large_strings` wrapper adds `_truncated` metadata only
            // when the root is an object; top-level arrays cannot carry it.
            let mut kept: Vec<serde_json::Value> = arr[..10].to_vec();
            kept.extend_from_slice(&arr[arr.len() - 5..]);
            serde_json::Value::Array(kept)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(|v| truncate_value_inner(v, budget, depth + 1))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every count argument a LISTED tool publishes has to state its bound, and
    /// state it in the words [`COUNT_RANGES`] produces.
    ///
    /// The gap this closes: `v0.132.0` made the clamp visible in the RESPONSE
    /// (`clamped_arguments`), which only helps a caller who already sent a value
    /// too large. The schema is what the model reads before it picks a number, and
    /// it named a default and no ceiling — `similar_top_k` said "default 5" for a
    /// bound of 50, `depth` said "default 3" for a bound of 10.
    ///
    /// Direction matters: the guard walks the PUBLISHED schema, not this file's
    /// source, and compares each description's bound against the row NUMERICALLY.
    /// The first version of this test asked `desc.contains(&hint)`, and that is a
    /// guard that cannot go red where it matters most: `range 1-100` CONTAINS
    /// `range 1-10`, so hand-typing a ceiling ten times the real one passed —
    /// which is precisely the defect (`get_call_graph.depth` advertising a bound
    /// the traversal never used) this whole disclosure exists to prevent.
    #[test]
    fn every_published_count_argument_states_its_bound() {
        /// Every `range <lo>-<hi>` in the text, parsed. Not a substring check:
        /// the numbers are compared as numbers, and finding two ranges in one
        /// description is itself a failure.
        fn published_ranges(desc: &str) -> Vec<(u64, u64)> {
            desc.match_indices("range ")
                .filter_map(|(at, marker)| {
                    let tail = &desc[at + marker.len()..];
                    let end = tail
                        .find(|c: char| !c.is_ascii_digit() && c != '-')
                        .unwrap_or(tail.len());
                    let (lo, hi) = tail[..end].split_once('-')?;
                    Some((lo.parse().ok()?, hi.parse().ok()?))
                })
                .collect()
        }

        let registry = crate::mcp::tools::ToolRegistry::new();
        let mut checked = 0usize;
        for tool in registry.list_tools() {
            let name = tool.name.as_str();
            let props = tool.input_schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("tool '{name}' publishes no properties object"));
            // EVERY property, not just the ones with a row. Skipping the rowless
            // ones is how the first version let a range be invented: a planted
            // `range 1-10` on `dead_min_lines` — a published count that is NOT
            // clamped, so the number would be a pure fabrication — was accepted,
            // and so was one planted on a string property. The rule is symmetric,
            // which also removes the need for an allowlist: state the row's bound
            // if there is a row, state no bound if there is not.
            for (key, spec) in props {
                let desc = spec["description"].as_str().unwrap_or_default();
                let expected: Vec<(u64, u64)> = count_range(name, key).into_iter().collect();
                assert_eq!(
                    published_ranges(desc),
                    expected,
                    "'{name}.{key}': published range(s) must equal its COUNT_RANGES \
                     row exactly, and a property with no row must state none — \
                     otherwise the number is invented: {desc:?}"
                );
                if !expected.is_empty() {
                    checked += 1;
                }
            }
        }
        // Vacuity floor over the CLAMPED ones. Eight of the eleven COUNT_RANGES
        // rows belong to a tool in `tools/list`; the other three
        // (`find_http_route.depth`, `dependency_graph.depth`,
        // `find_similar_code.top_k`) are backends the client is never offered, so
        // they have no description to check.
        //
        // This counts rows, not counts: `dead_min_lines` and the two `node_id`s
        // are published integers with no row, and the loop above now checks them
        // too — by requiring silence. The earlier wording here claimed a ninth
        // published count would trip this number, which was never true.
        assert_eq!(
            checked, 8,
            "expected 8 published arguments with a COUNT_RANGES row; a change to \
             the tool surface must be reflected here rather than silently \
             shrinking this guard"
        );
    }

    /// No published property may declare `number`, because none of them accept
    /// one.
    ///
    /// Every numeric argument these 7 tools take is a count, a depth or an id,
    /// and each reads through [`arg_u64`] / [`arg_opt_i64`], which call
    /// `as_u64` / `as_i64` — `3.5` is refused. `"type": "number"` told the
    /// caller otherwise on ten of the eleven; `find_references.node_id` was the
    /// one that already said `integer`, which is why this reads as drift rather
    /// than as a decision. The mismatch is the same shape as CON-15's: a
    /// declaration the producer does not honour, differing only in which
    /// direction the caller is misled.
    ///
    /// `f64` arguments do exist (`max_distance`), but only on tools that publish
    /// no schema, so there is nothing here to exempt. Add one and this test is
    /// where you say so.
    #[test]
    fn no_published_numeric_argument_declares_a_type_it_would_refuse() {
        let registry = crate::mcp::tools::ToolRegistry::new();
        let mut numeric = 0usize;
        for tool in registry.list_tools() {
            let name = tool.name.as_str();
            let props = tool.input_schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("tool '{name}' publishes no properties object"));
            for (key, spec) in props {
                match spec["type"].as_str() {
                    Some("integer") => numeric += 1,
                    Some("number") => panic!(
                        "'{name}.{key}' is published as `number`, but the handler \
                         reads it with as_u64/as_i64 and refuses 3.5. Declare \
                         `integer`."
                    ),
                    _ => {}
                }
            }
        }
        assert_eq!(
            numeric, 11,
            "vacuity floor: 11 numeric arguments are published across the 7 tools"
        );
    }

    #[test]
    fn truncate_array_keeps_items_homogeneous_and_records_original_len() {
        let items: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                json!({
                    "id": i, "name": format!("item_{}", i),
                })
            })
            .collect();
        let value = json!({
            "count": 50,
            "results": items,
            "pad": "x".repeat(200_000),
        });

        let out = truncate_large_strings(value, 50);
        let arr = out["results"].as_array().expect("results still an array");
        for (i, v) in arr.iter().enumerate() {
            assert!(v.is_object(), "results[{}] broke schema: {}", i, v);
        }
        let trunc = &out["_array_truncations"]["results"];
        assert_eq!(trunc["original"], 50);
        assert_eq!(trunc["kept"].as_u64().unwrap() as usize, arr.len());
        assert_eq!(out["_truncated"], true);
    }

    #[test]
    fn arrays_under_20_items_are_not_truncated() {
        let items: Vec<serde_json::Value> = (0..10).map(|i| json!({"id": i})).collect();
        let value = json!({
            "results": items,
            "pad": "y".repeat(200_000),
        });
        let out = truncate_large_strings(value, 50);
        let arr = out["results"].as_array().unwrap();
        assert_eq!(arr.len(), 10);
        assert!(
            out.get("_array_truncations").is_none(),
            "_array_truncations should only appear when arrays truncated"
        );
    }

    #[test]
    fn strip_outer_generic_simple() {
        assert_eq!(
            strip_outer_generic("Vec<Relation>"),
            Some("Relation".into())
        );
        assert_eq!(strip_outer_generic("Option<T>"), Some("T".into()));
    }

    #[test]
    fn strip_outer_generic_nested_picks_innermost() {
        assert_eq!(
            strip_outer_generic("Result<Vec<Relation>>"),
            Some("Relation".into())
        );
    }

    #[test]
    fn strip_outer_generic_multi_param_takes_last() {
        assert_eq!(strip_outer_generic("HashMap<K, V>"), Some("V".into()));
        assert_eq!(strip_outer_generic("Result<T, E>"), Some("E".into()));
    }

    #[test]
    fn strip_outer_generic_no_brackets_returns_none() {
        assert_eq!(strip_outer_generic("String"), None);
        assert_eq!(strip_outer_generic("&[T]"), None);
        assert_eq!(strip_outer_generic(""), None);
    }
}
