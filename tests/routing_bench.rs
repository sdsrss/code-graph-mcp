//! Tool-routing recall benchmark.
//!
//! Turns "does Claude Code intelligently invoke our tools?" from vibe-check
//! into a trackable number. For each natural-language query in the oracle,
//! ask the Claude API which tool it would pick (given our live 7-tool schemas)
//! and assert that the pick matches the expected tool.
//!
//! ## Running
//!
//! ```bash
//! # locally (needs your key):
//! ANTHROPIC_API_KEY=sk-... cargo test --test routing_bench -- --ignored --nocapture
//!
//! # skipped by default (no key, no cost):
//! cargo test --test routing_bench
//! ```
//!
//! ## Tuning
//!
//! Threshold starts at 0.70. Track per-release; raise it as tool descriptions
//! improve. Misses print with `expected` vs `got` so you can see whether the
//! routing went to a semantically-adjacent tool (e.g. `find_references` where
//! `get_call_graph` was expected) or a wrong tool entirely.
//!
//! ## Cost
//!
//! ~$0.10/run with `claude-sonnet-4-6` (20 queries × ~1.2K in + ~150 out tokens).

use code_graph_mcp::mcp::tools::ToolRegistry;
use serde_json::json;
use std::time::Duration;

const MODEL: &str = "claude-sonnet-4-6";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const P_AT_1_THRESHOLD: f64 = 0.70;

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
];

#[test]
#[ignore = "requires ANTHROPIC_API_KEY; run: cargo test --test routing_bench -- --ignored"]
fn routing_recall_benchmark() {
    let key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("[routing_bench] ANTHROPIC_API_KEY not set — skipping (run with the env var to exercise the bench).");
            return;
        }
    };

    // Source of truth: the same ToolRegistry that the live MCP server advertises.
    let registry = ToolRegistry::new();
    let tools: Vec<serde_json::Value> = registry
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

    let system_prompt = "You are a code-search assistant. For the user's query, \
        pick exactly ONE tool to invoke. Prefer the most specific tool whose description \
        matches the intent. Do not answer in prose — call a tool.";

    let mut hits = 0usize;
    let mut misses: Vec<(String, String, Option<String>)> = Vec::new();

    for &(query, expected) in ORACLE {
        let body = json!({
            "model": MODEL,
            "max_tokens": 1024,
            "system": system_prompt,
            "tools": tools,
            "tool_choice": { "type": "any" },
            "messages": [{ "role": "user", "content": query }],
        });

        let resp = client
            .post(API_URL)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .expect("POST to Anthropic API");

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            panic!("Anthropic API returned {}: {}", status, text);
        }

        let json_resp: serde_json::Value = resp.json().expect("parse JSON response");
        let picked = json_resp["content"]
            .as_array()
            .and_then(|arr| arr.iter().find(|c| c["type"] == "tool_use"))
            .and_then(|c| c["name"].as_str())
            .map(String::from);

        if picked.as_deref() == Some(expected) {
            hits += 1;
        } else {
            misses.push((query.to_string(), expected.to_string(), picked));
        }
    }

    let total = ORACLE.len();
    let p_at_1 = hits as f64 / total as f64;
    eprintln!(
        "\n[routing_bench] model={} P@1={}/{} = {:.1}% (threshold {:.0}%)",
        MODEL, hits, total, p_at_1 * 100.0, P_AT_1_THRESHOLD * 100.0,
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
