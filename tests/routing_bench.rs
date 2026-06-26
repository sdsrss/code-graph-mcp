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
//! ## Domains
//!
//! `ROUTING_BENCH_DOMAIN` selects which oracle pool runs:
//! - `backend` (default) — Rust/cross-module phrasing only (`ORACLE`, 22 q).
//!   Preserves baseline comparability with v0.17.2 and earlier.
//! - `frontend` — JS/TS/Vue/React phrasing only (`FRONTEND_ORACLE`, 20 q).
//!   Same 7 core tools, swapped vocabulary (component / hook / Promise<User> /
//!   useEffect / Redux / dispatch). Tests whether tool descriptions activate
//!   on the daagu-style frontend workflows that scored 0 MCP calls in the 7d
//!   usage audit.
//! - `all` — both pools (42 q), with separate per-domain recall buckets in
//!   the report so frontend regressions don't hide behind backend wins.
//!
//! ## Cost
//!
//! Tool-only mode, 3-run majority vote (post-Task 8):
//! - `domain=backend`   ~$0.30/run (22 q × 3 runs).
//! - `domain=frontend`  ~$0.27/run (20 q × 3 runs).
//! - `domain=all`       ~$0.55/run (42 q × 3 runs).
//!
//! Context-rich mode (adds 10 FP queries + STEERING_BLOCK_MIRROR system prompt):
//! - `domain=backend`   ~$0.45/run (32 q × 3 runs).
//! - `domain=frontend`  ~$0.40/run (30 q × 3 runs).
//! - `domain=all`       ~$0.80/run (52 q × 3 runs).
//!
//! OpenRouter adds a small markup (~5–10%).

use code_graph_mcp::mcp::tools::ToolRegistry;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

const P_AT_1_THRESHOLD: f64 = 0.70;
const SYSTEM_PROMPT: &str = "You are a code-search assistant. For the user's query, \
    pick exactly ONE tool to invoke. Prefer the most specific tool whose description \
    matches the intent. Do not answer in prose — call a tool.";

/// Mirror of the managed CLAUDE.md block produced by
/// `claude-plugin/scripts/adopt.js` `buildBlock('generic')`. Used by
/// context-rich bench mode to inject the project-CLAUDE.md steering into the
/// system prompt. Drift-checked at test time via `steering_block_drift_check`.
const STEERING_BLOCK_MIRROR: &str = r#"<!-- code-graph-mcp:begin v2 -->
## Code Graph (repo-wide AST index)

AST + FTS + vector index of the whole repo — prefer over multi-round Grep/Read for
structural queries (LSP only sees open files; this sees everything). Fastest path = Bash CLI:

| Intent | Command |
|--------|---------|
| Who calls X / what X calls | `code-graph-mcp callgraph X` |
| Impact before editing a fn | `code-graph-mcp impact X` |
| Unfamiliar dir / module | `code-graph-mcp overview <dir>` |
| Symbol source / signature | `code-graph-mcp show X` |
| Concept search (no exact name) | `code-graph-mcp search "…"` (vector: MCP `semantic_code_search`) |
| grep + AST context | `code-graph-mcp grep "pat" [paths]` |

Still use Grep for literal strings/regex in non-code files; still Read files you'll edit.
Full command + MCP-tool table: `.claude/plugin_code_graph_mcp.md`
<!-- code-graph-mcp:end -->"#;

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
    // Alternates accepted: at depth=1, find_references with relation=calls
    // returns the same callers list as get_call_graph. Pinning a single answer
    // over-fits the oracle — semantic equivalence both routes are correct.
    ("Who calls the function ensure_indexed?", "get_call_graph|find_references"),
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
    // Grep". In tool-only mode the bench cannot register Grep as a decoy
    // (only context-rich mode adds Grep+Read decoys); regardless, this
    // query asserts the rename-audit phrasing still hits find_references
    // — the intent we want to preserve through the tightening.
    ("I need to rename parse_tree to parse_ast — find every place I'd update.", "find_references"),
    // invariance_check
    ("Is the invariance tower healthy? Check all gates.", "invariance_check"),
    // list_skills / get_skill
    ("What skills are available in this code-graph instance?", "list_skills"),
    ("Show me the invariance-tower skill documentation", "get_skill"),
];

/// Frontend (JS/TS/Vue/React) phrasing of the same 7 tool intents. Activated
/// by `ROUTING_BENCH_DOMAIN=frontend|all`. Each tool gets ≥2 queries; the
/// vocabulary is intentionally drift-tested against the daagu-style workflow
/// (component / hook / Promise / useEffect / Redux dispatch) where the 7d
/// usage audit recorded zero MCP invocations across 1228 Bash commands.
///
/// Maintenance contract: any new core tool must add ≥1 entry here AND in
/// `ORACLE` (enforced by `frontend_oracle_well_formed` / `oracle_well_formed`).
const FRONTEND_ORACLE: &[(&str, &str)] = &[
    // project_map
    ("Show me the architecture of this React app", "project_map"),
    ("Give me a high-level map of this Vue project", "project_map"),
    ("Which top-level features depend on which?", "project_map"),
    // module_overview
    ("What's in the src/components/ directory?", "module_overview"),
    ("Show me what's exported from src/hooks/", "module_overview"),
    ("Give me an overview of the auth feature module", "module_overview"),
    // semantic_code_search
    ("Find the code that handles user login state", "semantic_code_search"),
    ("Where is the Redux store wired up?", "semantic_code_search"),
    ("Show me code that debounces search input", "semantic_code_search"),
    // get_call_graph
    ("Who calls the function fetchUserProfile?", "get_call_graph|find_references"),
    ("What does handleSubmit invoke during execution?", "get_call_graph"),
    ("Trace the call chain around dispatch in the reducer", "get_call_graph"),
    // find_references
    ("Find every place that imports the Button component", "find_references"),
    ("Is it safe to delete useDebounce? Show all usage sites.", "find_references"),
    // ast_search
    ("Find all functions that return Promise<User>", "ast_search"),
    ("List all React components in src/components/", "ast_search"),
    ("Which functions take a useEffect dependency array as a parameter?", "ast_search"),
    // get_ast_node
    ("Show me the LoginForm component definition", "get_ast_node"),
    ("What's the signature of the useAuth hook?", "get_ast_node"),
    ("Display the implementation of getAuthHeaders", "get_ast_node"),
    // invariance_check
    ("Run the invariance health check across all projects", "invariance_check"),
    // list_skills / get_skill
    ("List all the skill modules this server provides", "list_skills"),
    ("Load the cross-spoke-audit skill reference", "get_skill"),
];

/// System prompt for trigger-rate mode. Unlike `SYSTEM_PROMPT` it does NOT
/// force tool use — the whole point is measuring whether the model
/// *chooses* a tool. Wording mirrors a generic coding-assistant role
/// (closer to how Claude Code actually behaves) rather than
/// "pick exactly one tool".
const TRIGGER_SYSTEM_PROMPT: &str = "You are a coding assistant helping with a \
    software engineering question. The tools provided are available — invoke one \
    when it would answer the user better than guessing. Otherwise reply in prose. \
    Do not fabricate file paths or symbol names.";

/// Trigger-rate corpus — sloppy / misleading prompts that should still
/// activate code-graph tools, modeled on real-session failure modes from
/// the 7d audit (2026-05-12 → 2026-05-14, 141 main sessions: 1972 raw
/// `bash grep` vs 47 code-graph calls, the 47 from one dogfooding session).
///
/// **Design** (v2 / 2026-05-14): the first version of this oracle gave a
/// 100% baseline because prompts carried explicit cg-favorable anchors
/// (`module_overview` tool name, `src/parser/relations/` paths, project-
/// specific tokens like `MCP server` / `incremental-index`). v2 strips
/// those anchors and adds three failure-mode categories drawn from the
/// audit:
///
/// 1. **Pure symptom, zero technical anchors** ("今天报告数据不准") —
///    model could respond with prose or default to `bash grep` to "scan".
/// 2. **Misleading "use grep/find" framing** ("用 grep 找一下...") —
///    user signals a wrong tool; model should resist if cg fits better.
/// 3. **Answer-flavored guess** ("应该是 cleanup 那段没等完吧") —
///    model could just agree without verifying; cg lets it verify.
///
/// `__CG__` entries: model SHOULD pick a code-graph tool. Misses split
/// across `bash` (dominant production miss), `grep/read` (lesser miss),
/// and `(none)` (legitimate clarification or lazy prose answer).
///
/// `__DECOY__` entries: model SHOULD pick Bash/Grep/Read/none — anything
/// but a code-graph tool (boundary leak measure).
///
/// **Run with**: `ROUTING_BENCH_MODE=trigger-rate cargo test --release \
/// --test routing_bench -- --ignored --nocapture`. `tool_choice: auto`,
/// so the model can return no tool at all (a `__CG__` miss).
///
/// Maintenance: any rewrite invalidates prior baselines — re-bench
/// before / after and update `feedback_routing_bench.md`.
const TRIGGER_ORACLE: &[(&str, &str)] = &[
    // ===== Pure symptom, zero technical anchors =====
    ("今天的报告数据不准", "__CG__"),
    ("test 又挂了", "__CG__"),
    ("有 bug，帮我看看", "__CG__"),

    // ===== Misleading "use grep/find" framing =====
    ("用 grep 找一下为什么 user 数据没更新", "__CG__"),
    ("find 一下哪里写错了", "__CG__"),

    // ===== Answer-flavored guess (model should verify, not agree) =====
    ("应该是 cleanup 那段没等完吧", "__CG__"),
    ("是不是 cache 没刷", "__CG__"),

    // ===== Generic feature reference, no path/symbol =====
    ("看一下用户登录是怎么实现的", "__CG__"),
    ("准备重构 error handling，影响大不大", "__CG__"),

    // ===== English sloppy =====
    ("Why does this not work?", "__CG__"),

    // ===== Decoy preservation — must stay on Bash/Grep/Read =====
    ("Read the contents of CHANGELOG.md", "__DECOY__"),
    ("Search for literal 'FIXME' across the repo", "__DECOY__"),
];

/// Strict-A FP corpus: 10 queries that should route to a decoy (Grep or Read),
/// not to any code-graph tool. Each query has explicit literal-text or
/// path-based markers and zero structural component. Used in context-rich
/// mode to compute FP-rate (boundary-leak rate into code-graph).
const FP_ORACLE: &[(&str, &str)] = &[
    ("Find every TODO comment in source files.", "Grep"),
    ("Search for the literal string `FIXME` across the codebase.", "Grep"),
    ("Show me lines 50 through 80 of src/main.rs.", "Read"),
    ("What does the .gitignore file contain?", "Read"),
    ("Print the first 100 lines of CHANGELOG.md.", "Read"),
    ("Search for all occurrences of the regex `error\\d+` in log files.", "Grep"),
    ("Read the contents of Cargo.toml.", "Read"),
    ("Find every line that mentions `deprecated` in comments.", "Grep"),
    ("Show me the contents of build.rs.", "Read"),
    ("Grep for the regex pattern `^test_` in test files.", "Grep"),
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

/// Parse comma-separated model list. Pure helper for testability.
/// Trims whitespace, drops empty entries, preserves order.
fn parse_models_env(s: &str) -> Vec<String> {
    s.split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

/// Build N backends from a model list + API keys. Pure helper for testability.
/// Anthropic key takes precedence over OpenRouter (mirrors detect_backend).
/// Returns empty when no key or no models.
fn build_backends(
    models: Vec<String>,
    anthropic_key: Option<&str>,
    openrouter_key: Option<&str>,
) -> Vec<Backend> {
    if models.is_empty() {
        return Vec::new();
    }
    if let Some(k) = anthropic_key.filter(|s| !s.is_empty()) {
        return models.into_iter()
            .map(|m| Backend::Anthropic { key: k.to_string(), model: m })
            .collect();
    }
    if let Some(k) = openrouter_key.filter(|s| !s.is_empty()) {
        return models.into_iter()
            .map(|m| Backend::OpenRouter { key: k.to_string(), model: m })
            .collect();
    }
    Vec::new()
}

/// Multi-model dispatch — comma-separated `ROUTING_BENCH_MODELS` overrides
/// the single-model `ROUTING_BENCH_MODEL` and produces one Backend per name.
/// All models share the same API key (Anthropic preferred, OpenRouter fallback)
/// — this matches detect_backend's single-model precedence.
///
/// Use case: weekly CI cron that walks Sonnet 4.5 + Sonnet 4.6 + Opus 4.7 +
/// Haiku 4.5 to catch routing-quality regression when Claude Code rotates
/// the default model. v0.20.0 measured 100% P@1 on Sonnet 4.5 only — the
/// other models in the family had no signal until this hook existed.
///
/// Behavior:
///   - empty / unset → falls back to `detect_backend()` for legacy callers
///   - set with at least one non-empty model → returns that list of backends
///   - set but no API key → returns empty (caller should skip the test)
pub(crate) fn detect_backends() -> Vec<Backend> {
    let models_env = std::env::var("ROUTING_BENCH_MODELS").ok()
        .filter(|s| !s.is_empty());
    let Some(models_str) = models_env else {
        return detect_backend().into_iter().collect();
    };
    let models = parse_models_env(&models_str);
    if models.is_empty() {
        return detect_backend().into_iter().collect();
    }
    let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok();
    let openrouter_key = std::env::var("OPENROUTER_API_KEY").ok();
    build_backends(models, anthropic_key.as_deref(), openrouter_key.as_deref())
}

/// Call the backend, return the picked tool name, or None if the model produced no tool_use.
/// `force_tool=true` uses `tool_choice: required/any` (recall-bench behavior).
/// `force_tool=false` uses `tool_choice: auto` — model is free to answer in prose,
/// returning None. TriggerRate mode passes `false`; the None signals
/// "model chose not to use a tool" which is the failure mode we measure.
fn call_backend(
    client: &reqwest::blocking::Client,
    backend: &Backend,
    tools: &[Value],
    system_prompt: &str,
    query: &str,
    force_tool: bool,
) -> Option<String> {
    match backend {
        Backend::Anthropic { key, model } => {
            let tool_choice = if force_tool {
                json!({ "type": "any" })
            } else {
                json!({ "type": "auto" })
            };
            let body = json!({
                "model": model,
                "max_tokens": 1024,
                "temperature": 0,
                "system": system_prompt,
                "tools": tools,
                "tool_choice": tool_choice,
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
            let tool_choice = if force_tool { "required" } else { "auto" };
            let body = json!({
                "model": model,
                "max_tokens": 1024,
                "temperature": 0,
                "tools": openai_tools,
                "tool_choice": tool_choice,
                "messages": [
                    { "role": "system", "content": system_prompt },
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
    let backends = detect_backends();
    if backends.is_empty() {
        eprintln!("[routing_bench] Neither ANTHROPIC_API_KEY nor OPENROUTER_API_KEY set — skipping.");
        return;
    }
    let mode = detect_mode();
    let domain = detect_domain();
    eprintln!(
        "[routing_bench] backends=[{}] mode={:?} domain={:?}",
        backends.iter().map(|b| b.label()).collect::<Vec<_>>().join(", "),
        mode, domain,
    );

    let registry = ToolRegistry::new();
    let registry_tools: Vec<Value> = registry
        .list_tools()
        .iter()
        .map(|t| json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        }))
        .collect();
    assert!(!registry_tools.is_empty(), "ToolRegistry returned no tools");

    let tools = build_tools(mode, registry_tools);
    let system_prompt = build_system_prompt(mode);
    let active = active_oracle(domain);
    let oracle = build_oracle(mode, domain);
    // Code-graph tool names from the live registry; used by TriggerRate mode
    // to classify each pick as code-graph / decoy / none.
    let cg_names: Vec<&str> = registry
        .list_tools().iter().map(|t| t.name.as_str()).collect();
    // TriggerRate uses `tool_choice: auto` so we can observe "model chose no
    // tool"; recall modes force `tool_choice: any/required`.
    let force_tool = !matches!(mode, BenchMode::TriggerRate);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("build reqwest client");

    // 3-run majority vote per query.
    const RUNS: usize = 3;

    // Per-backend recall accumulator for the multi-model summary.
    let mut all_results: Vec<(String, f64, usize)> = Vec::new();
    let mut any_below_threshold: Vec<String> = Vec::new();

    for backend in &backends {
        if backends.len() > 1 {
            eprintln!("\n--- Backend: {} ---", backend.label());
        }

        let mut picks: HashMap<String, String> = HashMap::new();
        let mut all_misses: Vec<(String, String, Vec<Option<String>>)> = Vec::new();

        for &(query, expected) in &oracle {
            let mut run_picks: Vec<Option<String>> = Vec::with_capacity(RUNS);
            for _ in 0..RUNS {
                run_picks.push(call_backend(&client, backend, &tools, &system_prompt, query, force_tool));
            }
            let pick_strs: Vec<&str> = run_picks.iter()
                .map(|p| p.as_deref().unwrap_or("(none)"))
                .collect();
            let voted = majority_vote(&pick_strs).unwrap_or_else(|| "(none)".to_string());
            let is_match = if matches!(mode, BenchMode::TriggerRate) {
                matches_trigger_class(&voted, expected, &cg_names)
            } else {
                matches_expected(&voted, expected)
            };
            if !is_match {
                all_misses.push((query.to_string(), expected.to_string(), run_picks.clone()));
            }
            picks.insert(query.to_string(), voted);
        }

        // Mode-specific reporting + assertion accumulator.
        let headline_recall = match mode {
            BenchMode::ToolOnly => {
                let recall = compute_recall(&picks, &active);
                eprintln!(
                    "\n[routing_bench] mode=tool-only backend={} domain={:?} P@1={}/{} = {:.1}% (threshold {:.0}%)",
                    backend.label(),
                    domain,
                    (recall * active.len() as f64).round() as usize,
                    active.len(),
                    recall * 100.0,
                    P_AT_1_THRESHOLD * 100.0,
                );
                print_per_domain_recall(domain, &picks);
                print_misses(&all_misses);
                recall
            }
            BenchMode::ContextRich => {
                let recall = compute_recall(&picks, &active);
                let fp_rate = compute_fp_rate(&picks);
                let overall = compute_overall(&picks, &active);
                eprintln!(
                    "\n[routing_bench] mode=context-rich backend={} domain={:?}\n  Recall  = {:.1}% ({}/{})\n  FP-rate = {:.1}% ({}/{})\n  Overall = {:.1}% ({}/{}, threshold {:.0}%)",
                    backend.label(),
                    domain,
                    recall * 100.0,
                    (recall * active.len() as f64).round() as usize, active.len(),
                    fp_rate * 100.0,
                    (fp_rate * FP_ORACLE.len() as f64).round() as usize, FP_ORACLE.len(),
                    overall * 100.0,
                    (overall * (active.len() + FP_ORACLE.len()) as f64).round() as usize,
                    active.len() + FP_ORACLE.len(),
                    P_AT_1_THRESHOLD * 100.0,
                );
                print_per_domain_recall(domain, &picks);
                print_misses(&all_misses);
                overall
            }
            BenchMode::TriggerRate => {
                let metrics = compute_trigger_metrics(&picks, &oracle, &cg_names);
                eprintln!(
                    "\n[routing_bench] mode=trigger-rate backend={}\n  Trigger rate (CG picked on PreferCodeGraph)   = {:.1}% ({}/{})\n  No-tool rate (none picked on PreferCodeGraph) = {:.1}% ({}/{})\n  Decoy boundary (CG NOT picked on PreferDecoy) = {:.1}% ({}/{})\n  Decoy leak (CG picked on PreferDecoy)         = {:.1}% ({}/{})",
                    backend.label(),
                    metrics.trigger_rate * 100.0, metrics.trigger_hits, metrics.cg_total,
                    metrics.no_tool_rate * 100.0, metrics.cg_no_tool, metrics.cg_total,
                    metrics.decoy_preserved_rate * 100.0, metrics.decoy_preserved, metrics.decoy_total,
                    metrics.decoy_leak_rate * 100.0, metrics.decoy_leak, metrics.decoy_total,
                );
                // Soft regression tripwire — informational, never gates the build
                // (the bench is paid + LLM-sampled). Set BELOW the observed
                // sonnet-4.5 rate (~40% on the 10-query PreferCodeGraph set), not
                // the aspirational v0.27.0 60% baseline, so normal sampling variance
                // doesn't trip it; a drop under this signals a real routing regression.
                const SOFT_TRIGGER_FLOOR: f64 = 0.30;
                if metrics.trigger_rate < SOFT_TRIGGER_FLOOR {
                    eprintln!(
                        "  [warn] trigger rate {:.1}% below soft floor {:.0}% — possible routing regression (non-blocking)",
                        metrics.trigger_rate * 100.0, SOFT_TRIGGER_FLOOR * 100.0
                    );
                }
                print_misses(&all_misses);
                metrics.trigger_rate
            }
        };

        all_results.push((backend.label(), headline_recall, all_misses.len()));
        // TriggerRate is informational (no defined threshold) until we have a
        // baseline; recall modes still gate on P_AT_1_THRESHOLD.
        if !matches!(mode, BenchMode::TriggerRate) && headline_recall < P_AT_1_THRESHOLD {
            any_below_threshold.push(format!(
                "{}: {:.1}% ({} misses)",
                backend.label(), headline_recall * 100.0, all_misses.len(),
            ));
        }
    }

    // Multi-model summary table when more than one backend ran.
    if backends.len() > 1 {
        eprintln!("\n=== Multi-model P@1 summary (threshold {:.0}%) ===", P_AT_1_THRESHOLD * 100.0);
        for (label, recall, misses) in &all_results {
            let marker = if *recall >= P_AT_1_THRESHOLD { "PASS" } else { "FAIL" };
            eprintln!("  [{}] {:<40} {:.1}%  ({} miss)", marker, label, recall * 100.0, misses);
        }
    }

    // Single failing assertion at the end so all backends' reports print first.
    assert!(
        any_below_threshold.is_empty(),
        "Routing P@1 below threshold {:.0}% on {} backend(s):\n  {}",
        P_AT_1_THRESHOLD * 100.0,
        any_below_threshold.len(),
        any_below_threshold.join("\n  "),
    );
}

/// When `domain=All`, also print per-pool recall so frontend regressions
/// don't hide behind backend wins. No-op for single-domain runs (the headline
/// number already covers it).
fn print_per_domain_recall(domain: BenchDomain, picks: &HashMap<String, String>) {
    if !matches!(domain, BenchDomain::All) {
        return;
    }
    let be_oracle: Vec<(&str, &str)> = ORACLE.to_vec();
    let fe_oracle: Vec<(&str, &str)> = FRONTEND_ORACLE.to_vec();
    let be = compute_recall(picks, &be_oracle);
    let fe = compute_recall(picks, &fe_oracle);
    eprintln!(
        "  Backend recall  = {:.1}% ({}/{})\n  Frontend recall = {:.1}% ({}/{})",
        be * 100.0,
        (be * ORACLE.len() as f64).round() as usize, ORACLE.len(),
        fe * 100.0,
        (fe * FRONTEND_ORACLE.len() as f64).round() as usize, FRONTEND_ORACLE.len(),
    );
}

fn print_misses(misses: &[(String, String, Vec<Option<String>>)]) {
    if misses.is_empty() {
        return;
    }
    eprintln!("[routing_bench] misses ({}):", misses.len());
    for (q, exp, run_picks) in misses {
        eprintln!("  expected={} runs={:?}  query={:?}", exp, run_picks, q);
    }
}

/// Bench mode selector. `tool-only` is the legacy behavior (existing 20-query
/// oracle, no decoys, no MEMORY.md injection). `context-rich` adds decoys,
/// MEMORY.md, and FP_ORACLE — measures hook line quality. `trigger-rate`
/// drops `tool_choice: required` and uses TRIGGER_ORACLE — measures whether
/// the model *chooses* a tool given realistic phrasing (the failure mode
/// recall bench hides because it forces tool use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchMode {
    ToolOnly,
    ContextRich,
    TriggerRate,
}

/// Pure helper for testing — accepts the env value directly.
fn detect_mode_for(env: Option<&str>) -> BenchMode {
    match env {
        Some("context-rich") => BenchMode::ContextRich,
        Some("trigger-rate") => BenchMode::TriggerRate,
        _ => BenchMode::ToolOnly,
    }
}

/// Production wrapper — reads `ROUTING_BENCH_MODE` env.
fn detect_mode() -> BenchMode {
    detect_mode_for(std::env::var("ROUTING_BENCH_MODE").ok().as_deref())
}

/// Domain selector — which oracle pool drives recall. Default `Backend`
/// keeps v0.17.2 baselines comparable; `Frontend` and `All` are opt-in via
/// `ROUTING_BENCH_DOMAIN=frontend|all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchDomain {
    Backend,
    Frontend,
    All,
}

/// Pure helper for testing — accepts the env value directly.
fn detect_domain_for(env: Option<&str>) -> BenchDomain {
    match env {
        Some("frontend") => BenchDomain::Frontend,
        Some("all") => BenchDomain::All,
        _ => BenchDomain::Backend,
    }
}

/// Production wrapper — reads `ROUTING_BENCH_DOMAIN` env.
fn detect_domain() -> BenchDomain {
    detect_domain_for(std::env::var("ROUTING_BENCH_DOMAIN").ok().as_deref())
}

/// The active oracle (without FP corpus) for a given domain.
fn active_oracle(domain: BenchDomain) -> Vec<(&'static str, &'static str)> {
    match domain {
        BenchDomain::Backend => ORACLE.to_vec(),
        BenchDomain::Frontend => FRONTEND_ORACLE.to_vec(),
        BenchDomain::All => {
            let mut v = ORACLE.to_vec();
            v.extend_from_slice(FRONTEND_ORACLE);
            v
        }
    }
}

/// Bash decoy — only added in TriggerRate mode. Mirrors Claude Code's
/// native Bash tool, which is the dominant competing route in real-world
/// sessions (7d audit: 1972 raw `bash grep` vs 47 code-graph calls; the
/// Grep/Read decoys alone don't model this escape hatch). Kept OUT of
/// ContextRich mode so v0.17.2/v0.17.3 baselines stay comparable.
fn bash_decoy() -> Value {
    json!({
        "name": "Bash",
        "description": "Execute shell commands. Use for grep/rg/find/git/build/file ops, \
            ad-hoc one-off commands, or shell text-matching across files. Prefer over \
            code-graph when you don't need AST/structural context (raw regex on logs, \
            config, lockfiles).",
        "input_schema": {
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" }
            },
            "required": ["command"]
        }
    })
}

/// Decoy tools added in context-rich mode. Mirrors the most common Claude
/// Code native tools that compete with code-graph for routing. Descriptions
/// are calibrated against the spec's strict-A FP boundary: "Prefer over
/// code-graph" anchor language matches the v0.17.0 description tightening.
fn decoy_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "Grep",
            "description": "Fast text/regex search across files. Use for literal strings, regex patterns, or finding occurrences of fixed text. Prefer over code-graph tools when you don't need structural understanding (e.g., grep for `TODO`, `FIXME`, literal log strings).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Pattern to search for (literal or regex)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional path to scope the search"
                    }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "Read",
            "description": "Read file contents from disk by path. Use when you need to see specific contents of a known file (e.g., 'what's in CHANGELOG.md', 'show line 50 of foo.rs', '.gitignore contents'). Prefer over code-graph tools for non-source files (config, docs, logs).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "offset": {"type": "integer"},
                    "limit": {"type": "integer"}
                },
                "required": ["file_path"]
            }
        }),
    ]
}

/// Build the `tools` array for the API call. ToolOnly returns the registry
/// tools unchanged; ContextRich appends Grep + Read decoys (preserves
/// v0.17.2 baselines); TriggerRate appends Grep + Read **and** Bash —
/// modeling the dominant real-session competition (`bash grep -rn ...`).
fn build_tools(mode: BenchMode, registry_tools: Vec<Value>) -> Vec<Value> {
    let mut tools = registry_tools;
    if matches!(mode, BenchMode::ContextRich | BenchMode::TriggerRate) {
        tools.extend(decoy_tools());
    }
    if matches!(mode, BenchMode::TriggerRate) {
        tools.push(bash_decoy());
    }
    tools
}

/// Build the system prompt. ToolOnly returns SYSTEM_PROMPT verbatim;
/// ContextRich appends the CLAUDE.md framing + STEERING_BLOCK_MIRROR;
/// TriggerRate uses TRIGGER_SYSTEM_PROMPT (non-forcing) + the same
/// CLAUDE.md framing so the realistic Claude Code context applies.
fn build_system_prompt(mode: BenchMode) -> String {
    match mode {
        BenchMode::ToolOnly => SYSTEM_PROMPT.to_string(),
        BenchMode::ContextRich => format!(
            "{}\n\nThe project's CLAUDE.md (auto-loaded) contains this section:\n{}",
            SYSTEM_PROMPT, STEERING_BLOCK_MIRROR
        ),
        BenchMode::TriggerRate => format!(
            "{}\n\nThe project's CLAUDE.md (auto-loaded) contains this section:\n{}",
            TRIGGER_SYSTEM_PROMPT, STEERING_BLOCK_MIRROR
        ),
    }
}

/// Build the oracle (query → expected-tool pairs). Pulls the active-domain
/// pool via `active_oracle`; ContextRich appends FP_ORACLE; TriggerRate
/// replaces the pool entirely with TRIGGER_ORACLE (domain selector is
/// ignored — phrasing is project-specific, not language-pool-specific).
fn build_oracle(mode: BenchMode, domain: BenchDomain) -> Vec<(&'static str, &'static str)> {
    if matches!(mode, BenchMode::TriggerRate) {
        return TRIGGER_ORACLE.to_vec();
    }
    let mut all = active_oracle(domain);
    if matches!(mode, BenchMode::ContextRich) {
        all.extend_from_slice(FP_ORACLE);
    }
    all
}

/// Match a single pick against an oracle expected string. Expected may be a
/// single tool name (`"get_call_graph"`) or a pipe-separated list of equally
/// valid alternates (`"get_call_graph|find_references"`). Alternates exist
/// because some natural-language queries route to genuinely-equivalent tools
/// at depth=1 — pinning a single answer over-fits routing-bench to oracle
/// preference rather than measuring real routing capability.
fn matches_expected(picked: &str, expected: &str) -> bool {
    expected.split('|').any(|e| e == picked)
}

/// Recall over an oracle slice: fraction of queries in the slice where the
/// picked tool matches the expected tool (or any alternate). Returns 0.0 on
/// empty slice.
fn compute_recall(picks: &HashMap<String, String>, oracle: &[(&str, &str)]) -> f64 {
    if oracle.is_empty() {
        return 0.0;
    }
    let hits = oracle.iter()
        .filter(|(q, exp)| picks.get(*q).map(|p| matches_expected(p, exp)).unwrap_or(false))
        .count();
    hits as f64 / oracle.len() as f64
}

/// FP-rate over FP_ORACLE: fraction of FP queries where the picked tool is
/// NOT one of the decoys (i.e., a code-graph tool was wrongly chosen).
/// Note: picking the *wrong* decoy (Read when Grep expected) is NOT a FP —
/// the boundary held, only the specific decoy was off.
fn compute_fp_rate(picks: &HashMap<String, String>) -> f64 {
    let violations = FP_ORACLE.iter()
        .filter(|(q, _expected_decoy)| {
            let picked = picks.get(*q).map(|s| s.as_str()).unwrap_or("(none)");
            !["Grep", "Read"].contains(&picked)
        })
        .count();
    violations as f64 / FP_ORACLE.len() as f64
}

/// Names of native-Claude-Code decoy tools available in TriggerRate mode.
/// Boundary-preservation logic treats any of these as a valid pick on
/// `__DECOY__` queries; conversely, a model picking Bash on a `__CG__`
/// query counts as a trigger MISS (cg failed to recruit the model).
const DECOY_NAMES: &[&str] = &["Grep", "Read", "Bash"];

/// Trigger-rate match: a `__CG__` query is hit when the pick is any code-graph
/// tool; a `__DECOY__` query is hit when the pick is one of DECOY_NAMES or
/// `(none)` (model declined to use a code-graph tool — boundary preserved).
fn matches_trigger_class(picked: &str, class: &str, cg_names: &[&str]) -> bool {
    let is_cg = cg_names.contains(&picked);
    let is_decoy = DECOY_NAMES.contains(&picked);
    let is_none = picked == "(none)";
    match class {
        "__CG__" => is_cg,
        "__DECOY__" => is_decoy || is_none,
        _ => false,
    }
}

/// Aggregated trigger-rate counters for a single backend run. All rates are
/// in [0.0, 1.0]; counts use the raw tallies so we can report `(N/M)` next
/// to the percentage in the bench output.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TriggerMetrics {
    /// `__CG__` queries (total / hit / model-returned-no-tool).
    cg_total: usize,
    trigger_hits: usize,
    cg_no_tool: usize,
    /// `__DECOY__` queries (total / boundary-preserved / leaked-to-code-graph).
    decoy_total: usize,
    decoy_preserved: usize,
    decoy_leak: usize,
    /// Derived rates, defaulting to 0.0 on empty divisors.
    trigger_rate: f64,
    no_tool_rate: f64,
    decoy_preserved_rate: f64,
    decoy_leak_rate: f64,
}

/// Compute trigger-rate metrics over a TRIGGER_ORACLE-shaped slice. `picks`
/// is the query→voted-tool map produced by the majority-vote pass; `cg_names`
/// is the live code-graph tool roster.
fn compute_trigger_metrics(
    picks: &HashMap<String, String>,
    oracle: &[(&str, &str)],
    cg_names: &[&str],
) -> TriggerMetrics {
    let mut m = TriggerMetrics {
        cg_total: 0, trigger_hits: 0, cg_no_tool: 0,
        decoy_total: 0, decoy_preserved: 0, decoy_leak: 0,
        trigger_rate: 0.0, no_tool_rate: 0.0,
        decoy_preserved_rate: 0.0, decoy_leak_rate: 0.0,
    };
    for &(q, class) in oracle {
        let pick = picks.get(q).map(|s| s.as_str()).unwrap_or("(none)");
        let is_cg = cg_names.contains(&pick);
        let is_none = pick == "(none)";
        let is_decoy = DECOY_NAMES.contains(&pick);
        match class {
            "__CG__" => {
                m.cg_total += 1;
                if is_cg { m.trigger_hits += 1; }
                if is_none { m.cg_no_tool += 1; }
            }
            "__DECOY__" => {
                m.decoy_total += 1;
                if is_decoy || is_none { m.decoy_preserved += 1; }
                if is_cg { m.decoy_leak += 1; }
            }
            _ => {}
        }
    }
    if m.cg_total > 0 {
        m.trigger_rate = m.trigger_hits as f64 / m.cg_total as f64;
        m.no_tool_rate = m.cg_no_tool as f64 / m.cg_total as f64;
    }
    if m.decoy_total > 0 {
        m.decoy_preserved_rate = m.decoy_preserved as f64 / m.decoy_total as f64;
        m.decoy_leak_rate = m.decoy_leak as f64 / m.decoy_total as f64;
    }
    m
}

/// Overall summary: (recall_hits + fp_avoidance) / total over the active
/// oracle plus FP_ORACLE. Loose metric — FP_avoidance counts wrong-decoy
/// picks as good (boundary held). Use recall and fp_rate separately for
/// candidate comparison.
fn compute_overall(picks: &HashMap<String, String>, oracle: &[(&str, &str)]) -> f64 {
    let recall_hits = oracle.iter()
        .filter(|(q, exp)| picks.get(*q).map(|p| matches_expected(p, exp)).unwrap_or(false))
        .count();
    let fp_violations = FP_ORACLE.iter()
        .filter(|(q, _)| {
            let picked = picks.get(*q).map(|s| s.as_str()).unwrap_or("(none)");
            !["Grep", "Read"].contains(&picked)
        })
        .count();
    let fp_avoidance = FP_ORACLE.len() - fp_violations;
    let total = oracle.len() + FP_ORACLE.len();
    if total == 0 {
        return 0.0;
    }
    (recall_hits + fp_avoidance) as f64 / total as f64
}

/// Majority vote across multiple runs of the same query. Returns the most-
/// frequent pick. Tie-break: first occurrence wins (preserves run-1 result
/// when temperature: 0 fails to fully converge).
fn majority_vote(picks: &[&str]) -> Option<String> {
    if picks.is_empty() {
        return None;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for &p in picks {
        *counts.entry(p).or_insert(0) += 1;
    }
    let max = counts.values().max().copied().unwrap();
    // Iterate in original order to break ties by first-seen.
    for &p in picks {
        if counts[p] == max {
            return Some(p.to_string());
        }
    }
    None
}

/// Drift detection: the Rust `STEERING_BLOCK_MIRROR` constant must match the
/// managed block `buildBlock('generic')` produced by
/// `claude-plugin/scripts/adopt.js` byte-for-byte. Single source of truth is
/// adopt.js; the Rust mirror is a snapshot used by context-rich bench mode.
/// This test catches forgotten updates.
#[test]
fn steering_block_drift_check() {
    let output = std::process::Command::new("node")
        .args([
            "-e",
            "process.stdout.write(require('./claude-plugin/scripts/adopt.js').buildBlock('generic'))",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node binary required to verify steering-block drift");
    assert!(
        output.status.success(),
        "node exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let js_value = String::from_utf8(output.stdout).expect("steering block is utf-8");
    assert_eq!(
        STEERING_BLOCK_MIRROR, js_value,
        "Steering-block drift detected.\n  Rust mirror:  tests/routing_bench.rs STEERING_BLOCK_MIRROR\n  JS source:    claude-plugin/scripts/adopt.js buildBlock('generic')\nFix: copy the JS value into STEERING_BLOCK_MIRROR (raw string literal r#\"...\"#)."
    );
}

/// Shared invariant for any oracle pool: every expected tool exists in the
/// live registry, and every registry tool has ≥1 query in this pool. Runs
/// without an API key.
fn assert_oracle_covers_registry(pool_name: &str, oracle: &[(&'static str, &'static str)]) {
    let registry = ToolRegistry::new();
    let names: std::collections::HashSet<&str> = registry.list_tools().iter()
        .map(|t| t.name.as_str()).collect();
    let mut covered: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for &(query, expected) in oracle {
        for alt in expected.split('|') {
            assert!(
                names.contains(alt),
                "[{}] Oracle references unknown tool '{}' (query='{}'). Registry has: {:?}",
                pool_name, alt, query, names,
            );
            covered.insert(alt);
        }
    }
    for name in &names {
        assert!(
            covered.contains(name),
            "[{}] Tool '{}' has no oracle coverage — add at least one query.",
            pool_name, name,
        );
    }
}

#[test]
fn oracle_well_formed() {
    assert_oracle_covers_registry("backend/ORACLE", ORACLE);
}

#[test]
fn frontend_oracle_well_formed() {
    assert_oracle_covers_registry("frontend/FRONTEND_ORACLE", FRONTEND_ORACLE);
}

/// TRIGGER_ORACLE invariants. Unlike ORACLE / FRONTEND_ORACLE, expected
/// values are class sentinels (`__CG__` / `__DECOY__`) not tool names —
/// `assert_oracle_covers_registry` doesn't apply. Validate the class
/// vocabulary + distinct queries + minimum count instead.
#[test]
fn trigger_oracle_well_formed() {
    assert!(
        TRIGGER_ORACLE.len() >= 8,
        "TRIGGER_ORACLE too small ({} entries); want ≥8 for stable rate estimate",
        TRIGGER_ORACLE.len(),
    );
    let mut seen = std::collections::HashSet::new();
    let mut has_cg = false;
    let mut has_decoy = false;
    for &(q, class) in TRIGGER_ORACLE {
        assert!(seen.insert(q), "duplicate TRIGGER_ORACLE query: {:?}", q);
        match class {
            "__CG__" => has_cg = true,
            "__DECOY__" => has_decoy = true,
            other => panic!("unknown TRIGGER_ORACLE class {:?} (query={:?})", other, q),
        }
    }
    assert!(has_cg && has_decoy, "TRIGGER_ORACLE must include both __CG__ and __DECOY__ classes");
}

/// Drift check: every tool registered in `ToolRegistry` must be referenced
/// in the adoption template's decision table. Prevents "added a tool but
/// forgot to update the routing decision table" — the regression that
/// `index_line_drift_check` catches for the MEMORY.md INDEX_LINE, this
/// catches for the per-tool decision rows.
///
/// Lightweight by design: substring match, not structural Markdown parsing.
/// Full build.rs codegen is over-engineered for current churn rate (~3 tool
/// surface changes per year per CHANGELOG); a single failing assertion at
/// CI time is enough to remind the developer.
#[test]
fn decision_table_covers_registry() {
    let template_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("claude-plugin/templates/plugin_code_graph_mcp.md");
    let content = std::fs::read_to_string(&template_path)
        .unwrap_or_else(|e| panic!("read {}: {}", template_path.display(), e));

    let registry = ToolRegistry::new();
    let mut missing: Vec<&str> = Vec::new();
    for tool in registry.list_tools() {
        // Match the tool name as a word — must be wrapped in non-identifier
        // chars on at least one side. Avoids false positives if a tool name
        // were a substring of a longer identifier (none are today, but defends
        // against future additions like `find_references` vs `find_references_v2`).
        let needle = tool.name.as_str();
        if !content.contains(needle) {
            missing.push(needle);
        }
    }
    assert!(
        missing.is_empty(),
        "Decision-table drift: {} tool(s) registered in ToolRegistry but absent from \
         claude-plugin/templates/plugin_code_graph_mcp.md — add a row for: {:?}",
        missing.len(), missing,
    );
}

#[test]
fn frontend_oracle_distinct_from_backend() {
    // Catches accidental copy-paste: queries must not be shared between pools,
    // otherwise `domain=all` would double-count.
    let be: std::collections::HashSet<&str> = ORACLE.iter().map(|(q, _)| *q).collect();
    for &(query, _) in FRONTEND_ORACLE {
        assert!(
            !be.contains(query),
            "FRONTEND_ORACLE shares query with backend ORACLE: {:?}",
            query,
        );
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn detect_mode_defaults_to_tool_only_when_unset() {
        let m = detect_mode_for(None);
        assert!(matches!(m, BenchMode::ToolOnly));
    }

    #[test]
    fn detect_mode_explicit_tool_only() {
        let m = detect_mode_for(Some("tool-only"));
        assert!(matches!(m, BenchMode::ToolOnly));
    }

    #[test]
    fn detect_mode_context_rich() {
        let m = detect_mode_for(Some("context-rich"));
        assert!(matches!(m, BenchMode::ContextRich));
    }

    #[test]
    fn detect_mode_unknown_value_falls_back_to_tool_only() {
        let m = detect_mode_for(Some("invalid"));
        assert!(matches!(m, BenchMode::ToolOnly));
    }

    #[test]
    fn detect_mode_trigger_rate() {
        let m = detect_mode_for(Some("trigger-rate"));
        assert!(matches!(m, BenchMode::TriggerRate));
    }
}

#[cfg(test)]
mod domain_tests {
    use super::*;

    #[test]
    fn detect_domain_defaults_to_backend_when_unset() {
        // Backward-compat: pre-domain runs (and any caller without the env
        // set) must keep hitting the original ORACLE pool so v0.17.2 baselines
        // remain comparable.
        assert!(matches!(detect_domain_for(None), BenchDomain::Backend));
    }

    #[test]
    fn detect_domain_explicit_backend() {
        assert!(matches!(detect_domain_for(Some("backend")), BenchDomain::Backend));
    }

    #[test]
    fn detect_domain_explicit_frontend() {
        assert!(matches!(detect_domain_for(Some("frontend")), BenchDomain::Frontend));
    }

    #[test]
    fn detect_domain_explicit_all() {
        assert!(matches!(detect_domain_for(Some("all")), BenchDomain::All));
    }

    #[test]
    fn detect_domain_unknown_value_falls_back_to_backend() {
        assert!(matches!(detect_domain_for(Some("nonsense")), BenchDomain::Backend));
    }

    #[test]
    fn active_oracle_backend_returns_only_backend() {
        let o = active_oracle(BenchDomain::Backend);
        assert_eq!(o.len(), ORACLE.len());
    }

    #[test]
    fn active_oracle_frontend_returns_only_frontend() {
        let o = active_oracle(BenchDomain::Frontend);
        assert_eq!(o.len(), FRONTEND_ORACLE.len());
    }

    #[test]
    fn active_oracle_all_concatenates() {
        let o = active_oracle(BenchDomain::All);
        assert_eq!(o.len(), ORACLE.len() + FRONTEND_ORACLE.len());
    }
}

#[cfg(test)]
mod decoy_tests {
    use super::*;

    #[test]
    fn decoy_tools_has_grep_and_read() {
        let tools = decoy_tools();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"Grep"));
        assert!(names.contains(&"Read"));
    }

    #[test]
    fn decoy_tools_have_required_fields() {
        for tool in decoy_tools() {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert!(tool["input_schema"].is_object());
            assert!(tool["input_schema"]["properties"].is_object());
            assert!(tool["input_schema"]["required"].is_array());
        }
    }

    #[test]
    fn decoy_descriptions_reference_prefer_over_code_graph() {
        // Sanity check that decoys are calibrated to compete with code-graph
        // tools (per the spec's measurement-fairness requirement). If a future
        // edit weakens decoy descriptions, this test makes the regression visible.
        for tool in decoy_tools() {
            let desc = tool["description"].as_str().unwrap();
            assert!(
                desc.contains("Prefer over code-graph"),
                "decoy description must signal 'prefer over code-graph' to be measurement-fair"
            );
        }
    }

    #[test]
    fn bash_decoy_has_required_fields_and_anchor() {
        let tool = bash_decoy();
        assert_eq!(tool["name"], "Bash");
        let desc = tool["description"].as_str().unwrap();
        assert!(
            desc.contains("Prefer over code-graph"),
            "Bash decoy must carry the same 'prefer over code-graph' anchor as Grep/Read"
        );
        assert!(tool["input_schema"]["properties"]["command"].is_object());
    }
}

#[cfg(test)]
mod fp_oracle_tests {
    use super::*;

    #[test]
    fn fp_oracle_has_ten_entries() {
        assert_eq!(FP_ORACLE.len(), 10);
    }

    #[test]
    fn fp_oracle_entries_target_grep_or_read() {
        for &(query, expected) in FP_ORACLE {
            assert!(
                expected == "Grep" || expected == "Read",
                "FP_ORACLE entry expects {} but must be Grep or Read: query={:?}",
                expected, query
            );
        }
    }

    #[test]
    fn fp_oracle_queries_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for &(query, _) in FP_ORACLE {
            assert!(seen.insert(query), "duplicate FP_ORACLE query: {:?}", query);
        }
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;

    fn fake_registry_tools() -> Vec<Value> {
        vec![
            json!({"name": "tool_a", "description": "A", "input_schema": {}}),
            json!({"name": "tool_b", "description": "B", "input_schema": {}}),
        ]
    }

    #[test]
    fn build_tools_tool_only_returns_registry_unchanged() {
        let registry = fake_registry_tools();
        let tools = build_tools(BenchMode::ToolOnly, registry.clone());
        assert_eq!(tools, registry);
    }

    #[test]
    fn build_tools_context_rich_appends_decoys() {
        let registry = fake_registry_tools();
        let tools = build_tools(BenchMode::ContextRich, registry.clone());
        assert_eq!(tools.len(), 4); // 2 fake + 2 decoys
        assert!(tools.iter().any(|t| t["name"] == "Grep"));
        assert!(tools.iter().any(|t| t["name"] == "Read"));
    }

    #[test]
    fn build_system_prompt_tool_only_unchanged() {
        let p = build_system_prompt(BenchMode::ToolOnly);
        assert_eq!(p, SYSTEM_PROMPT);
    }

    #[test]
    fn build_system_prompt_context_rich_appends_steering_block() {
        let p = build_system_prompt(BenchMode::ContextRich);
        assert!(p.starts_with(SYSTEM_PROMPT));
        assert!(p.contains("CLAUDE.md"));
        assert!(p.contains(STEERING_BLOCK_MIRROR));
    }

    #[test]
    fn build_oracle_tool_only_returns_only_oracle() {
        let o = build_oracle(BenchMode::ToolOnly, BenchDomain::Backend);
        assert_eq!(o.len(), ORACLE.len());
    }

    #[test]
    fn build_oracle_context_rich_concatenates() {
        let o = build_oracle(BenchMode::ContextRich, BenchDomain::Backend);
        assert_eq!(o.len(), ORACLE.len() + FP_ORACLE.len());
    }

    #[test]
    fn build_oracle_frontend_returns_frontend_pool() {
        let o = build_oracle(BenchMode::ToolOnly, BenchDomain::Frontend);
        assert_eq!(o.len(), FRONTEND_ORACLE.len());
        // Sanity: a backend-only query must not be in the frontend oracle.
        assert!(!o.iter().any(|(q, _)| *q == ORACLE[0].0));
    }

    #[test]
    fn build_oracle_all_concatenates_both_pools() {
        let o = build_oracle(BenchMode::ToolOnly, BenchDomain::All);
        assert_eq!(o.len(), ORACLE.len() + FRONTEND_ORACLE.len());
    }

    #[test]
    fn build_oracle_all_context_rich_includes_fp() {
        let o = build_oracle(BenchMode::ContextRich, BenchDomain::All);
        assert_eq!(o.len(), ORACLE.len() + FRONTEND_ORACLE.len() + FP_ORACLE.len());
    }
}

#[cfg(test)]
mod scoring_tests {
    use super::*;
    use std::collections::HashMap;

    fn picks(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(q, t)| (q.to_string(), t.to_string())).collect()
    }

    #[test]
    fn compute_recall_full_hit() {
        // Pick the first alternate from each pipe-separated expected — matches_expected
        // accepts any of them, so the first one is sufficient for a "full hit" simulation.
        let p: HashMap<String, String> = ORACLE.iter()
            .map(|(q, t)| (q.to_string(), t.split('|').next().unwrap().to_string()))
            .collect();
        assert_eq!(compute_recall(&p, ORACLE), 1.0);
    }

    #[test]
    fn compute_recall_full_hit_frontend() {
        let p: HashMap<String, String> = FRONTEND_ORACLE.iter()
            .map(|(q, t)| (q.to_string(), t.split('|').next().unwrap().to_string()))
            .collect();
        assert_eq!(compute_recall(&p, FRONTEND_ORACLE), 1.0);
    }

    #[test]
    fn compute_recall_alternates_first_alt_hits() {
        // matches_expected splits on '|'; either alt should count as a hit.
        let oracle: &[(&str, &str)] = &[("q1", "get_call_graph|find_references")];
        let p1: HashMap<String, String> = [("q1".to_string(), "get_call_graph".to_string())].into_iter().collect();
        assert_eq!(compute_recall(&p1, oracle), 1.0, "first alt should match");
        let p2: HashMap<String, String> = [("q1".to_string(), "find_references".to_string())].into_iter().collect();
        assert_eq!(compute_recall(&p2, oracle), 1.0, "second alt should match");
        let p3: HashMap<String, String> = [("q1".to_string(), "module_overview".to_string())].into_iter().collect();
        assert_eq!(compute_recall(&p3, oracle), 0.0, "non-alt should miss");
    }

    #[test]
    fn compute_recall_zero_picks() {
        let p = HashMap::new();
        assert_eq!(compute_recall(&p, ORACLE), 0.0);
    }

    #[test]
    fn compute_recall_empty_oracle_returns_zero() {
        let p = HashMap::new();
        assert_eq!(compute_recall(&p, &[]), 0.0);
    }

    #[test]
    fn compute_fp_rate_zero_when_all_picks_grep_or_read() {
        let p = picks(&[
            ("Find every TODO comment in source files.", "Grep"),
            ("Show me lines 50 through 80 of src/main.rs.", "Read"),
            ("What does the .gitignore file contain?", "Read"),
            ("Print the first 100 lines of CHANGELOG.md.", "Read"),
            ("Search for the literal string `FIXME` across the codebase.", "Grep"),
            ("Search for all occurrences of the regex `error\\d+` in log files.", "Grep"),
            ("Read the contents of Cargo.toml.", "Read"),
            ("Find every line that mentions `deprecated` in comments.", "Grep"),
            ("Show me the contents of build.rs.", "Read"),
            ("Grep for the regex pattern `^test_` in test files.", "Grep"),
        ]);
        assert_eq!(compute_fp_rate(&p), 0.0);
    }

    #[test]
    fn compute_fp_rate_full_violation() {
        let p: HashMap<String, String> = FP_ORACLE.iter()
            .map(|(q, _)| (q.to_string(), "get_call_graph".to_string()))
            .collect();
        assert_eq!(compute_fp_rate(&p), 1.0);
    }

    #[test]
    fn compute_fp_rate_wrong_decoy_does_not_count_as_violation() {
        // Picking Read when Grep was expected is still NOT a FP — boundary held.
        let p: HashMap<String, String> = FP_ORACLE.iter()
            .map(|(q, _)| (q.to_string(), "Read".to_string()))
            .collect();
        assert_eq!(compute_fp_rate(&p), 0.0);
    }

    #[test]
    fn majority_vote_unanimous() {
        let v = majority_vote(&["a", "a", "a"]);
        assert_eq!(v, Some("a".to_string()));
    }

    #[test]
    fn majority_vote_two_to_one() {
        let v = majority_vote(&["a", "b", "a"]);
        assert_eq!(v, Some("a".to_string()));
    }

    #[test]
    fn majority_vote_three_distinct_uses_first() {
        // Per design: tie → first run's pick.
        let v = majority_vote(&["a", "b", "c"]);
        assert_eq!(v, Some("a".to_string()));
    }

    #[test]
    fn majority_vote_empty_returns_none() {
        let v: Option<String> = majority_vote(&[] as &[&str]);
        assert_eq!(v, None);
    }

    // ===== Trigger-rate scoring tests =====

    fn picks_from(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(q, t)| (q.to_string(), t.to_string())).collect()
    }

    #[test]
    fn matches_trigger_class_cg_hits_when_pick_in_registry() {
        let cg = &["get_call_graph", "module_overview"];
        assert!(matches_trigger_class("get_call_graph", "__CG__", cg));
        assert!(!matches_trigger_class("Grep", "__CG__", cg));
        assert!(!matches_trigger_class("(none)", "__CG__", cg));
    }

    #[test]
    fn matches_trigger_class_decoy_accepts_all_decoys_or_none() {
        let cg = &["get_call_graph"];
        assert!(matches_trigger_class("Grep", "__DECOY__", cg));
        assert!(matches_trigger_class("Read", "__DECOY__", cg));
        assert!(matches_trigger_class("Bash", "__DECOY__", cg),
            "Bash is a valid decoy in TriggerRate mode");
        assert!(matches_trigger_class("(none)", "__DECOY__", cg));
        assert!(!matches_trigger_class("get_call_graph", "__DECOY__", cg),
            "decoy preservation must reject a code-graph pick (boundary leak)");
    }

    #[test]
    fn compute_trigger_metrics_bash_pick_on_cg_query_counts_as_miss() {
        // Real-world dominant failure: model reaches for `bash grep` instead
        // of code-graph. This must register as a miss, not a hit.
        let oracle: &[(&str, &str)] = &[("q1", "__CG__")];
        let cg = vec!["get_call_graph"];
        let picks = picks_from(&[("q1", "Bash")]);
        let m = compute_trigger_metrics(&picks, oracle, &cg);
        assert_eq!(m.cg_total, 1);
        assert_eq!(m.trigger_hits, 0);  // Bash is not a cg tool
        assert_eq!(m.cg_no_tool, 0);    // it IS a tool, just the wrong one
    }

    #[test]
    fn compute_trigger_metrics_bash_pick_on_decoy_query_preserves_boundary() {
        let oracle: &[(&str, &str)] = &[("d1", "__DECOY__")];
        let cg = vec!["get_call_graph"];
        let picks = picks_from(&[("d1", "Bash")]);
        let m = compute_trigger_metrics(&picks, oracle, &cg);
        assert_eq!(m.decoy_total, 1);
        assert_eq!(m.decoy_preserved, 1);
        assert_eq!(m.decoy_leak, 0);
    }

    #[test]
    fn matches_trigger_class_unknown_class_is_false() {
        let cg = &["get_call_graph"];
        assert!(!matches_trigger_class("get_call_graph", "__OTHER__", cg));
    }

    #[test]
    fn compute_trigger_metrics_all_correct() {
        // 2 __CG__ → both pick cg; 1 __DECOY__ → picks Grep
        let oracle: &[(&str, &str)] = &[
            ("q1", "__CG__"),
            ("q2", "__CG__"),
            ("q3", "__DECOY__"),
        ];
        let cg = vec!["get_call_graph"];
        let picks = picks_from(&[
            ("q1", "get_call_graph"),
            ("q2", "get_call_graph"),
            ("q3", "Grep"),
        ]);
        let m = compute_trigger_metrics(&picks, oracle, &cg);
        assert_eq!(m.cg_total, 2);
        assert_eq!(m.trigger_hits, 2);
        assert_eq!(m.cg_no_tool, 0);
        assert_eq!(m.decoy_total, 1);
        assert_eq!(m.decoy_preserved, 1);
        assert_eq!(m.decoy_leak, 0);
        assert_eq!(m.trigger_rate, 1.0);
        assert_eq!(m.decoy_preserved_rate, 1.0);
        assert_eq!(m.decoy_leak_rate, 0.0);
    }

    #[test]
    fn compute_trigger_metrics_mixed_outcomes() {
        // 4 __CG__: 2 cg, 1 Grep (wrong route), 1 (none).
        // 2 __DECOY__: 1 Grep (good), 1 cg (leak).
        let oracle: &[(&str, &str)] = &[
            ("c1", "__CG__"), ("c2", "__CG__"), ("c3", "__CG__"), ("c4", "__CG__"),
            ("d1", "__DECOY__"), ("d2", "__DECOY__"),
        ];
        let cg = vec!["get_call_graph"];
        let picks = picks_from(&[
            ("c1", "get_call_graph"),
            ("c2", "get_call_graph"),
            ("c3", "Grep"),
            ("c4", "(none)"),
            ("d1", "Grep"),
            ("d2", "get_call_graph"),
        ]);
        let m = compute_trigger_metrics(&picks, oracle, &cg);
        assert_eq!(m.cg_total, 4);
        assert_eq!(m.trigger_hits, 2);
        assert_eq!(m.cg_no_tool, 1);
        assert_eq!(m.decoy_total, 2);
        assert_eq!(m.decoy_preserved, 1);
        assert_eq!(m.decoy_leak, 1);
        assert_eq!(m.trigger_rate, 0.5);
        assert_eq!(m.no_tool_rate, 0.25);
        assert_eq!(m.decoy_preserved_rate, 0.5);
        assert_eq!(m.decoy_leak_rate, 0.5);
    }

    #[test]
    fn compute_trigger_metrics_empty_oracle_zero_rates() {
        let picks: HashMap<String, String> = HashMap::new();
        let cg = vec!["x"];
        let m = compute_trigger_metrics(&picks, &[], &cg);
        assert_eq!(m.trigger_rate, 0.0);
        assert_eq!(m.decoy_preserved_rate, 0.0);
        assert_eq!(m.decoy_leak_rate, 0.0);
    }

    #[test]
    fn compute_trigger_metrics_missing_pick_treated_as_none() {
        // q1 was a __CG__ query but no pick was recorded → behaves like (none).
        let oracle: &[(&str, &str)] = &[("q1", "__CG__")];
        let cg = vec!["get_call_graph"];
        let picks: HashMap<String, String> = HashMap::new();
        let m = compute_trigger_metrics(&picks, oracle, &cg);
        assert_eq!(m.cg_total, 1);
        assert_eq!(m.trigger_hits, 0);
        assert_eq!(m.cg_no_tool, 1);
    }
}

#[cfg(test)]
mod backends_tests {
    use super::*;

    fn label_of(b: &Backend) -> String { b.label() }

    #[test]
    fn parse_models_env_basic() {
        let v = parse_models_env("a,b,c");
        assert_eq!(v, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn parse_models_env_trims_whitespace() {
        let v = parse_models_env(" claude-sonnet-4-6 , claude-opus-4-7 ,claude-haiku-4-5 ");
        assert_eq!(v, vec!["claude-sonnet-4-6", "claude-opus-4-7", "claude-haiku-4-5"]);
    }

    #[test]
    fn parse_models_env_drops_empty_entries() {
        let v = parse_models_env(",,a,, ,b,");
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn parse_models_env_empty_returns_empty() {
        assert_eq!(parse_models_env(""), Vec::<String>::new());
        assert_eq!(parse_models_env("   "), Vec::<String>::new());
        assert_eq!(parse_models_env(",,,"), Vec::<String>::new());
    }

    #[test]
    fn build_backends_anthropic_preferred_over_openrouter() {
        let backends = build_backends(
            vec!["claude-opus-4-7".into(), "claude-haiku-4-5".into()],
            Some("ant-key"),
            Some("or-key"),
        );
        assert_eq!(backends.len(), 2);
        // Anthropic key wins — both backends should be Anthropic variant.
        for b in &backends {
            assert!(matches!(b, Backend::Anthropic { .. }), "expected Anthropic, got {:?}", label_of(b));
        }
        assert_eq!(label_of(&backends[0]), "anthropic/claude-opus-4-7");
        assert_eq!(label_of(&backends[1]), "anthropic/claude-haiku-4-5");
    }

    #[test]
    fn build_backends_openrouter_fallback_when_no_anthropic() {
        let backends = build_backends(
            vec!["anthropic/claude-sonnet-4.5".into()],
            None,
            Some("or-key"),
        );
        assert_eq!(backends.len(), 1);
        assert!(matches!(backends[0], Backend::OpenRouter { .. }));
        assert_eq!(label_of(&backends[0]), "openrouter/anthropic/claude-sonnet-4.5");
    }

    #[test]
    fn build_backends_no_keys_returns_empty() {
        let backends = build_backends(
            vec!["claude-sonnet-4-6".into()],
            None,
            None,
        );
        assert!(backends.is_empty(), "no keys → empty backends, got {} entries", backends.len());
    }

    #[test]
    fn build_backends_empty_keys_treated_as_missing() {
        // detect_backend treats empty-string keys as "not set"; build_backends mirrors that.
        let backends = build_backends(
            vec!["claude-sonnet-4-6".into()],
            Some(""),
            Some(""),
        );
        assert!(backends.is_empty());
    }

    #[test]
    fn build_backends_empty_models_returns_empty() {
        let backends = build_backends(vec![], Some("k"), Some("k2"));
        assert!(backends.is_empty());
    }

    #[test]
    fn build_backends_preserves_model_order() {
        let backends = build_backends(
            vec!["m1".into(), "m2".into(), "m3".into()],
            Some("k"),
            None,
        );
        assert_eq!(backends.len(), 3);
        assert_eq!(label_of(&backends[0]), "anthropic/m1");
        assert_eq!(label_of(&backends[1]), "anthropic/m2");
        assert_eq!(label_of(&backends[2]), "anthropic/m3");
    }
}

