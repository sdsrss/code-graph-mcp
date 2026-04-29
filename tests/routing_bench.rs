//! Tool-routing recall benchmark.
//!
//! Turns "does Claude Code intelligently invoke our tools?" from vibe-check
//! into a trackable number. For each natural-language query in the oracle,
//! ask a Claude-family model which tool it would pick (given our live 7-tool
//! schemas) and assert that the pick matches the expected tool.
//!
//! ## Backends
//!
//! Supports two API backends, auto-detected by env:
//! - `ANTHROPIC_API_KEY` — native Anthropic Messages API
//!   (`https://api.anthropic.com/v1/messages`, tools in Anthropic schema).
//! - `OPENROUTER_API_KEY` — OpenRouter's OpenAI-compatible
//!   `/api/v1/chat/completions` endpoint. Tools re-packaged as
//!   `{"type": "function", "function": {...}}`. Model defaults to
//!   `anthropic/claude-sonnet-4.5`; override with `ROUTING_BENCH_MODEL`.
//!
//! If both are set, `ANTHROPIC_API_KEY` wins. If neither, the test no-ops.
//!
//! ## Running
//!
//! ```bash
//! # Anthropic native
//! ANTHROPIC_API_KEY=sk-ant-... cargo test --test routing_bench -- --ignored --nocapture
//!
//! # OpenRouter
//! OPENROUTER_API_KEY=sk-or-... cargo test --test routing_bench -- --ignored --nocapture
//!
//! # Override model
//! OPENROUTER_API_KEY=... ROUTING_BENCH_MODEL=anthropic/claude-opus-4.1 \
//!   cargo test --test routing_bench -- --ignored --nocapture
//! ```
//!
//! ## Tuning
//!
//! Threshold starts at 0.70. Track per-release; raise as descriptions improve.
//! Misses print with `expected` vs `got` so you can see whether routing went to
//! a semantically-adjacent tool or a wrong tool entirely.
//!
//! ## Cost
//!
//! ~$0.10/run with `claude-sonnet-4-6` (20 queries × ~1.2K in + ~150 out tokens).
//! OpenRouter adds a small markup (~5–10%).

use code_graph_mcp::mcp::tools::ToolRegistry;
use serde_json::{json, Value};
use std::time::Duration;

const P_AT_1_THRESHOLD: f64 = 0.70;
const SYSTEM_PROMPT: &str = "You are a code-search assistant. For the user's query, \
    pick exactly ONE tool to invoke. Prefer the most specific tool whose description \
    matches the intent. Do not answer in prose — call a tool.";

/// (natural-language query, expected tool name).
/// 20 queries × 7 tools — 3 per tool except `find_references` with 2.
const ORACLE: &[(&str, &str)] = &[
    // project_map
    ("Show me the project architecture", "project_map"),
    ("Give me a high-level overview of this codebase", "project_map"),
    ("Which modules depend on which at the top level?", "project_map"),
    // module_overview
    ("What's in the src/indexer/ directory?", "module_overview"),
    ("Show me what's exported from src/storage/", "module_overview"),
    ("Give me an overview of the parser module", "module_overview"),
    // semantic_code_search
    ("Find the code that does reciprocal rank fusion", "semantic_code_search"),
    ("Where is the embedding model loaded from disk?", "semantic_code_search"),
    ("Show me code related to change detection via Merkle hashing", "semantic_code_search"),
    // get_call_graph
    ("Who calls the function ensure_indexed?", "get_call_graph"),
    ("What does run_full_index call during execution?", "get_call_graph"),
    ("Trace the call chain around extract_relations", "get_call_graph"),
    // find_references
    ("Find all references to the constant REL_CALLS", "find_references"),
    ("Is it safe to remove compute_diff? Show all usage sites.", "find_references"),
    // ast_search
    ("Find all functions that return Vec<Relation>", "ast_search"),
    ("List all structs in the storage module", "ast_search"),
    ("Which functions take a tree_sitter::Node as a parameter?", "ast_search"),
    // get_ast_node
    ("Show me the EmbeddingModel struct definition", "get_ast_node"),
    ("What's the signature of weighted_rrf_fusion?", "get_ast_node"),
    ("Display the implementation of format_call_graph_response", "get_ast_node"),
    // v0.17.0 — description-tightening regression guards.
    // semantic_code_search now says "If module path is known, prefer
    // module_overview". This query bait-tests that hint: it has both a
    // concept word ("pipeline") AND an explicit module path. Pre-tightening
    // it would route to semantic_code_search; post-tightening it should
    // settle on module_overview.
    ("How does the embedding pipeline work in src/embedding/?", "module_overview"),
    // find_references now says "For plain literals (string/regex), prefer
    // Grep". The bench cannot register Grep as a decoy tool, so we instead
    // assert the rename-audit phrasing still hits find_references — the
    // intent we want to preserve through the tightening.
    ("I need to rename parse_tree to parse_ast — find every place I'd update.", "find_references"),
];

enum Backend {
    Anthropic { key: String, model: String },
    OpenRouter { key: String, model: String },
}

impl Backend {
    fn label(&self) -> String {
        match self {
            Backend::Anthropic { model, .. } => format!("anthropic/{}", model),
            Backend::OpenRouter { model, .. } => format!("openrouter/{}", model),
        }
    }
}

fn detect_backend() -> Option<Backend> {
    let model_override = std::env::var("ROUTING_BENCH_MODEL").ok().filter(|s| !s.is_empty());
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Some(Backend::Anthropic {
                key: k,
                model: model_override.unwrap_or_else(|| "claude-sonnet-4-6".into()),
            });
        }
    }
    if let Ok(k) = std::env::var("OPENROUTER_API_KEY") {
        if !k.is_empty() {
            return Some(Backend::OpenRouter {
                key: k,
                model: model_override.unwrap_or_else(|| "anthropic/claude-sonnet-4.5".into()),
            });
        }
    }
    None
}

/// Call the backend, return the picked tool name, or None if the model produced no tool_use.
fn call_backend(
    client: &reqwest::blocking::Client,
    backend: &Backend,
    tools: &[Value],
    query: &str,
) -> Option<String> {
    match backend {
        Backend::Anthropic { key, model } => {
            let body = json!({
                "model": model,
                "max_tokens": 1024,
                "temperature": 0,
                "system": SYSTEM_PROMPT,
                "tools": tools,
                "tool_choice": { "type": "any" },
                "messages": [{ "role": "user", "content": query }],
            });
            let resp = client.post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .expect("POST to Anthropic API");
            if !resp.status().is_success() {
                panic!("Anthropic API {}: {}", resp.status(), resp.text().unwrap_or_default());
            }
            let json_resp: Value = resp.json().expect("parse Anthropic JSON");
            json_resp["content"]
                .as_array()
                .and_then(|arr| arr.iter().find(|c| c["type"] == "tool_use"))
                .and_then(|c| c["name"].as_str())
                .map(String::from)
        }
        Backend::OpenRouter { key, model } => {
            // Convert Anthropic-style {name, description, input_schema} tools to
            // OpenAI function-calling format {type, function: {name, description, parameters}}.
            let openai_tools: Vec<Value> = tools.iter().map(|t| json!({
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["input_schema"],
                }
            })).collect();
            let body = json!({
                "model": model,
                "max_tokens": 1024,
                "temperature": 0,
                "tools": openai_tools,
                "tool_choice": "required",
                "messages": [
                    { "role": "system", "content": SYSTEM_PROMPT },
                    { "role": "user",   "content": query },
                ],
            });
            let resp = client.post("https://openrouter.ai/api/v1/chat/completions")
                .header("authorization", format!("Bearer {}", key))
                .header("content-type", "application/json")
                .header("http-referer", "https://github.com/sdsrss/code-graph-mcp")
                .header("x-title", "code-graph-mcp routing_bench")
                .json(&body)
                .send()
                .expect("POST to OpenRouter");
            if !resp.status().is_success() {
                panic!("OpenRouter API {}: {}", resp.status(), resp.text().unwrap_or_default());
            }
            let json_resp: Value = resp.json().expect("parse OpenRouter JSON");
            // OpenAI shape: choices[0].message.tool_calls[0].function.name
            json_resp["choices"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|choice| choice["message"]["tool_calls"].as_array())
                .and_then(|calls| calls.first())
                .and_then(|call| call["function"]["name"].as_str())
                .map(String::from)
        }
    }
}

#[test]
#[ignore = "requires ANTHROPIC_API_KEY or OPENROUTER_API_KEY; run: cargo test --test routing_bench -- --ignored"]
fn routing_recall_benchmark() {
    let backend = match detect_backend() {
        Some(b) => b,
        None => {
            eprintln!("[routing_bench] Neither ANTHROPIC_API_KEY nor OPENROUTER_API_KEY set — skipping.");
            return;
        }
    };
    eprintln!("[routing_bench] using backend={}", backend.label());

    let registry = ToolRegistry::new();
    let tools: Vec<Value> = registry
        .list_tools()
        .iter()
        .map(|t| json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        }))
        .collect();
    assert!(!tools.is_empty(), "ToolRegistry returned no tools");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("build reqwest client");

    let mut hits = 0usize;
    let mut misses: Vec<(String, String, Option<String>)> = Vec::new();

    for &(query, expected) in ORACLE {
        let picked = call_backend(&client, &backend, &tools, query);
        if picked.as_deref() == Some(expected) {
            hits += 1;
        } else {
            misses.push((query.to_string(), expected.to_string(), picked));
        }
    }

    let total = ORACLE.len();
    let p_at_1 = hits as f64 / total as f64;
    eprintln!(
        "\n[routing_bench] backend={} P@1={}/{} = {:.1}% (threshold {:.0}%)",
        backend.label(), hits, total, p_at_1 * 100.0, P_AT_1_THRESHOLD * 100.0,
    );
    if !misses.is_empty() {
        eprintln!("[routing_bench] misses ({}):", misses.len());
        for (q, exp, got) in &misses {
            eprintln!("  expected={} got={:?}  query=\"{}\"", exp, got, q);
        }
    }

    assert!(
        p_at_1 >= P_AT_1_THRESHOLD,
        "Routing P@1 {:.1}% below threshold {:.0}% — {} miss(es), see output above",
        p_at_1 * 100.0, P_AT_1_THRESHOLD * 100.0, misses.len(),
    );
}

#[test]
fn oracle_well_formed() {
    // Invariant that runs without an API key: every expected tool in the oracle
    // is a real tool in the live registry, and the oracle covers all 7 core tools.
    let registry = ToolRegistry::new();
    let names: std::collections::HashSet<&str> = registry.list_tools().iter()
        .map(|t| t.name.as_str()).collect();
    let mut covered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for &(query, expected) in ORACLE {
        assert!(
            names.contains(expected),
            "Oracle references unknown tool '{}' (query='{}'). Registry has: {:?}",
            expected, query, names,
        );
        covered.insert(expected);
    }
    for name in &names {
        assert!(
            covered.contains(name),
            "Tool '{}' has no oracle coverage — add at least one query.",
            name,
        );
    }
}
