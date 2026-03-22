use anyhow::{anyhow, Result};
use serde_json::json;

use crate::storage::queries;

use super::COMPRESSION_TOKEN_THRESHOLD;

/// Extract a required string argument, trimming whitespace and rejecting empty values.
pub(super) fn required_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    let s = args[key].as_str()
        .ok_or_else(|| anyhow!("{} is required", key))?
        .trim();
    if s.is_empty() {
        return Err(anyhow!("{} must not be empty", key));
    }
    Ok(s)
}

/// Parse route input like "GET /api/users" into (Some("GET"), "/api/users").
/// If no method prefix, returns (None, original_path).
pub(super) fn parse_route_input(input: &str) -> (Option<String>, &str) {
    let trimmed = input.trim();
    if let Some(space_idx) = trimmed.find(' ') {
        let prefix = &trimmed[..space_idx];
        let methods = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "USE"];
        if methods.contains(&prefix.to_uppercase().as_str()) {
            return (Some(prefix.to_uppercase()), trimmed[space_idx..].trim());
        }
    }
    (None, trimmed)
}

/// Filter route matches by HTTP method from metadata JSON.
pub(super) fn filter_routes_by_method(rows: &mut Vec<queries::RouteMatch>, method: &Option<String>) {
    if let Some(method) = method {
        rows.retain(|r| {
            r.metadata.as_ref().is_some_and(|m| {
                serde_json::from_str::<serde_json::Value>(m).ok()
                    .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(|s| s.to_string()))
                    .is_some_and(|rm| rm == *method)
            })
        });
    }
}

/// For inline handlers, override handler_name and start/end lines from metadata.
pub(super) fn apply_inline_handler_metadata(handler: &mut serde_json::Value, metadata: Option<&str>) {
    if let Some(meta_str) = metadata {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
            if meta.get("inline").and_then(|v| v.as_bool()).unwrap_or(false) {
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
pub(super) fn should_skip_indexing(args: &serde_json::Value) -> bool {
    args.get("skip_indexing").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Normalize user-facing type filter aliases to internal AST node types.
pub(super) fn normalize_type_filter_mcp(input: &str) -> Vec<String> {
    match input.to_lowercase().as_str() {
        "fn" | "func" | "function" | "method" => vec!["function".into(), "method".into()],
        "class" => vec!["class".into()],
        "struct" => vec!["struct".into()],
        "enum" => vec!["enum".into()],
        "interface" | "iface" | "trait" => vec!["interface".into(), "trait".into()],
        "type" | "type_alias" => vec!["type_alias".into()],
        "const" | "constant" => vec!["constant".into()],
        "var" | "variable" => vec!["variable".into()],
        "module" => vec!["module".into()],
        _ => vec![input.to_lowercase()],
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
pub(super) fn truncate_large_strings(value: serde_json::Value, token_budget: usize) -> serde_json::Value {
    // Target: reduce to roughly token_budget * CHARS_PER_TOKEN chars total
    let target_chars = token_budget * crate::domain::CHARS_PER_TOKEN;
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() <= target_chars {
        return value;
    }

    let mut result = truncate_value(value, target_chars);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("_truncated".to_string(), json!(true));
        obj.insert("_truncation_hint".to_string(),
            json!("Result exceeded token limit. Use get_ast_node(node_id) to read specific nodes."));
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

fn truncate_value_inner(value: serde_json::Value, budget: usize, depth: usize) -> serde_json::Value {
    if depth > MAX_TRUNCATE_DEPTH { return value; }
    match value {
        serde_json::Value::Object(map) => {
            // Calculate total size of large string fields eligible for truncation
            let large_fields: usize = map.values()
                .filter_map(|v| v.as_str().filter(|s| s.len() > TRUNCATE_MIN_LEN).map(|s| s.len()))
                .sum();
            let small_fields_size: usize = map.iter()
                .map(|(k, v)| k.len() + match v {
                    serde_json::Value::String(s) if s.len() <= TRUNCATE_MIN_LEN => s.len(),
                    serde_json::Value::String(_) => 0,
                    _ => serde_json::to_string(v).map(|s| s.len()).unwrap_or(0),
                })
                .sum();
            let large_budget = budget.saturating_sub(small_fields_size);

            let truncated: serde_json::Map<String, serde_json::Value> = map.into_iter()
                .map(|(k, v)| {
                    let tv = match &v {
                        serde_json::Value::String(s) if s.len() > TRUNCATE_MIN_LEN => {
                            let field_budget = if large_fields > 0 {
                                (large_budget as f64 * s.len() as f64 / large_fields as f64) as usize
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
                            let mut kept: Vec<serde_json::Value> = arr[..10].to_vec();
                            kept.push(json!(format!("... [{} items truncated]", arr.len() - 15)));
                            kept.extend_from_slice(&arr[arr.len()-5..]);
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
            serde_json::Value::Object(truncated)
        }
        serde_json::Value::Array(arr) if arr.len() > 20 => {
            let mut kept: Vec<serde_json::Value> = arr[..10].to_vec();
            kept.push(json!(format!("... [{} items truncated]", arr.len() - 15)));
            kept.extend_from_slice(&arr[arr.len()-5..]);
            serde_json::Value::Array(kept)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(
                arr.into_iter()
                    .map(|v| truncate_value_inner(v, budget, depth + 1))
                    .collect()
            )
        }
        other => other,
    }
}
