//! Cross-surface drift-guards for two recurring bug classes.
//!
//! 1. **Compact ↔ full key-set parity** (`compact_field_allowlist`, 3rd recurrence):
//!    `tool_module_overview` builds a full JSON envelope, then `compact_module_overview`
//!    forwards a hand-maintained allowlist of top-level keys. Every prior recurrence
//!    was a new top-level key added to the full envelope that nobody added to the
//!    allowlist, so it silently vanished in `compact: true` mode with no disclosure.
//!    `compact_allowlist_covers_all_result_keys` scans the source and fails if any
//!    `result["k"] =` key in the producer is neither forwarded nor explicitly listed
//!    as deliberately compacted.
//!
//! 2. **CLI ↔ MCP query-time freshness parity** (AUDIT-2026-07-16 MED-2 follow-up):
//!    every line-number-emitting CLI subcommand must resync stale files via
//!    `refresh_files_if_stale` before reading line numbers out of the DB, and every
//!    file-path-accepting MCP tool must do the same via `ensure_file_fresh_opt`.
//!    A new command that emits line numbers without wiring in the resync ships a
//!    stale-line-number regression. The source-scanning guards below lock the known
//!    call sites so a missing one fails CI instead of shipping.
//!
//! These are source-scanning tests: they read the crate's own `.rs` files as text.
//! Cargo runs integration tests with the crate root as CWD, so the relative paths
//! below resolve. A text edit that removes a guarded call fails the guard even
//! without recompiling — that is the point.

mod common;

use common::{init_server, parse_tool_result, tool_call_json};
use serde_json::json;
use std::fs;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Task 1 runtime proof: compact mode forwards the `dependencies` payload.
// ---------------------------------------------------------------------------

/// RED before the allowlist fix: `module_overview` with `include_deps: true` sets a
/// top-level `dependencies` key on the full envelope, but the compact forwarder's
/// allowlist did not include it, so `compact: true` dropped it with no disclosure.
/// GREEN after adding the key to the allowlist in `compact_module_overview`.
#[test]
fn compact_module_overview_forwards_dependencies() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/mod_b.ts"),
        "export function bee(): number { return 1; }\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/mod_a.ts"),
        "import { bee } from './mod_b';\n\
         export function ay(): number { return bee(); }\n",
    )
    .unwrap();

    let server = init_server(&project);

    let msg = tool_call_json(
        "module_overview",
        json!({
            "path": "src/mod_a.ts",
            "include_deps": true,
            "compact": true,
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    assert!(
        result.get("dependencies").is_some(),
        "compact + include_deps must forward `dependencies`; compact result was:\n{}",
        serde_json::to_string_pretty(&result).unwrap()
    );
}

// ---------------------------------------------------------------------------
// Task 2 drift-guard: every full-envelope top-level key is compact-forwarded.
// ---------------------------------------------------------------------------

const OVERVIEW_SRC: &str = "src/mcp/server/tools/overview.rs";

/// Keys the compact form intentionally rewrites/renames/drops rather than
/// forwarding verbatim through the `for key in [...]` allowlist. `warning` is
/// forwarded through its own dedicated `if full.get("warning")` branch (a value,
/// not a copied key), so it is covered but not in the array. If a future author
/// adds a new full-envelope key that compact handles specially, list it here with
/// a comment — do NOT add keys here just to silence the guard.
const DELIBERATELY_COMPACTED: &[&str] = &[
    // Forwarded via a dedicated `if full.get("warning").is_some()` branch.
    "warning",
];

/// Extract every `result["<key>"] =` assignment key found in `region`.
fn assigned_result_keys(region: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let marker = "result[\"";
    let mut rest = region;
    while let Some(i) = rest.find(marker) {
        let after = &rest[i + marker.len()..];
        if let Some(j) = after.find("\"]") {
            let key = &after[..j];
            let tail = after[j + 2..].trim_start();
            // Only assignments (`=`), not comparisons (`==`) or index reads.
            if tail.starts_with('=') && !tail.starts_with("==") {
                keys.push(key.to_string());
            }
            rest = &after[j + 2..];
        } else {
            break;
        }
    }
    keys
}

/// Extract the quoted keys from the `for key in [ ... ]` compact allowlist array.
fn compact_allowlist(compact_region: &str) -> Vec<String> {
    let anchor = "for key in [";
    let start = compact_region
        .find(anchor)
        .expect("compact allowlist `for key in [` not found — did the forwarder shape change?");
    let after = &compact_region[start + anchor.len()..];
    let end = after
        .find(']')
        .expect("unterminated compact allowlist array");
    after[..end]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Return the body of `fn <name>` (braces included) by brace-matching from the
/// opening `{` of the signature to its balanced close. Robust to indentation
/// (methods inside `impl` blocks) and to nested inner `fn` helpers — both of
/// which defeat line-based boundary detection. Skips `//` line comments and
/// `"…"` string literals so braces inside `format!("{}")` / error strings and
/// `// { }` comments do not unbalance the count (the target fns contain no raw
/// strings, block-comment braces, or char-literal braces — verified at authoring).
fn fn_region<'a>(src: &'a str, name: &str) -> &'a str {
    let decl = format!("fn {}(", name);
    let start = src
        .find(&decl)
        .unwrap_or_else(|| panic!("function `{name}` not found in source"));
    let bytes = src.as_bytes();
    // First `{` after the declaration opens the body (signatures have no braces).
    let mut i = start + decl.len();
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    let body_start = i;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut in_line_comment = false;
    let mut prev = 0u8;
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
        } else if in_str {
            if c == b'"' && prev != b'\\' {
                in_str = false;
            }
        } else if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return &src[body_start..=i];
            }
        }
        prev = c;
        i += 1;
    }
    &src[body_start..]
}

/// Returns full-envelope keys that compact neither forwards nor deliberately drops.
fn uncovered_compact_keys(overview_src: &str) -> Vec<String> {
    let producer = fn_region(overview_src, "tool_module_overview");
    let forwarder = fn_region(overview_src, "compact_module_overview");
    let allowlist = compact_allowlist(forwarder);
    let mut uncovered: Vec<String> = assigned_result_keys(producer)
        .into_iter()
        .filter(|k| !allowlist.contains(k) && !DELIBERATELY_COMPACTED.contains(&k.as_str()))
        .collect();
    uncovered.sort();
    uncovered.dedup();
    uncovered
}

#[test]
fn compact_allowlist_covers_all_result_keys() {
    let src = fs::read_to_string(OVERVIEW_SRC).expect("read overview.rs");
    let uncovered = uncovered_compact_keys(&src);
    assert!(
        uncovered.is_empty(),
        "tool_module_overview sets top-level key(s) {uncovered:?} that compact_module_overview \
         neither forwards (allowlist / dedicated branch) nor lists in DELIBERATELY_COMPACTED. \
         Add each to the `for key in [...]` allowlist in {OVERVIEW_SRC}, or document it in \
         DELIBERATELY_COMPACTED (tests/freshness_parity.rs)."
    );
}

// ---------------------------------------------------------------------------
// Same class, sibling surface: project_map's PER-MODULE key set.
//
// `module_overview` got the guard above after three recurrences; `project_map`
// has the identical producer/compactor split and had none, so it drifted the
// same way. The full builder grew an `other` bucket specifically because a docs-
// or types-only module read as `functions: 0, classes: 0` — and compact still
// answered exactly that, having dropped `other`, `classes` and `constants`.
// ---------------------------------------------------------------------------

/// Per-module keys compact drops on purpose. Keep this list short and reasoned:
/// the point of the guard is that dropping a COUNT makes a populated module read
/// as empty, which is a wrong answer, not a terse one.
const PROJECT_MAP_MODULE_DELIBERATELY_DROPPED: &[&str] = &[
    // A display nicety, not a signal for "is this module worth opening".
    "languages",
];

/// The `modules_json` builder — the full per-module envelope.
fn project_map_module_producer(src: &str) -> &str {
    let start = src
        .find("let modules_json")
        .expect("`let modules_json` not found — repoint the project_map producer region");
    let after = &src[start..];
    let end = after
        .find("let deps_json")
        .expect("`let deps_json` not found — the producer region no longer ends where expected");
    &after[..end]
}

/// Keys a json! literal or an `obj["k"] =` assignment introduces in `region`.
fn module_envelope_keys(region: &str) -> Vec<String> {
    let bytes: Vec<char> = region.chars().collect();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != '"' {
                j += 1;
            }
            let literal: String = bytes[start..j].iter().collect();
            // A key is followed by `:` (json! literal) or `] =` (obj index).
            let mut k = j + 1;
            while k < bytes.len() && bytes[k].is_whitespace() {
                k += 1;
            }
            let is_json_key = bytes.get(k) == Some(&':');
            let is_index_assign = bytes.get(k) == Some(&']')
                && bytes[k + 1..]
                    .iter()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(|c| *c == '=');
            if (is_json_key || is_index_assign)
                && !literal.is_empty()
                && literal
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                keys.push(literal);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Per-module producer keys the compact rebuild neither names nor drops on record.
fn project_map_uncovered_module_keys(src: &str) -> Vec<String> {
    let produced = module_envelope_keys(project_map_module_producer(src));
    assert!(
        produced.len() > 3,
        "producer scan found only {produced:?} — the region anchors are wrong, \
         not the code (a guard that reads nothing passes vacuously)"
    );
    let forwarder = fn_region(src, "compact_project_module");
    let mut uncovered: Vec<String> = produced
        .into_iter()
        .filter(|k| {
            !forwarder.contains(&format!("\"{k}\""))
                && !PROJECT_MAP_MODULE_DELIBERATELY_DROPPED.contains(&k.as_str())
        })
        .collect();
    uncovered.sort();
    uncovered.dedup();
    uncovered
}

#[test]
fn project_map_compact_forwards_every_module_key() {
    let src = fs::read_to_string(PROJECT_MAP_SRC).expect("read project_map.rs");
    let uncovered = project_map_uncovered_module_keys(&src);
    assert!(
        uncovered.is_empty(),
        "the full project_map module envelope emits {uncovered:?}, which \
         compact_project_module neither forwards nor drops on the record. A dropped COUNT \
         makes a populated module read as `functions: 0` — forward it in {PROJECT_MAP_SRC}, \
         or add it to PROJECT_MAP_MODULE_DELIBERATELY_DROPPED with a reason."
    );
}

/// Permanent negative control, matching the envelope guard's below: a guard that
/// cannot fire is worth nothing. Both key SOURCES in the producer are mutated,
/// since a scanner that read only the `json!` literal — and not the conditional
/// `obj["…"] =` assignments, where `other` and `constants` live — would still
/// pass the positive test above.
#[test]
fn project_map_module_guard_detects_missing_key() {
    let src = fs::read_to_string(PROJECT_MAP_SRC).expect("read project_map.rs");

    // (a) A key from the compact json! literal.
    let broken = src.replace("\"functions\": m[\"functions\"],", "");
    let uncovered = project_map_uncovered_module_keys(&broken);
    assert!(
        uncovered.iter().any(|k| k == "functions"),
        "negative control (a) failed: dropping `functions` from the compact literal must \
         surface it as uncovered, got {uncovered:?}"
    );

    // (b) A key that reaches the full envelope through a conditional assignment.
    let broken = src.replace(
        "for key in [\"classes\", \"interfaces_traits\", \"constants\", \"other\"] {",
        "for key in [\"classes\", \"interfaces_traits\", \"constants\"] {",
    );
    let uncovered = project_map_uncovered_module_keys(&broken);
    assert!(
        uncovered.iter().any(|k| k == "other"),
        "negative control (b) failed: dropping `other` from the forwarding list must \
         surface it as uncovered, got {uncovered:?}"
    );
}

/// Permanent negative control: prove the guard actually fires when a key is
/// missing from the allowlist. Removing `"dead_code"` from the array in a working
/// copy of the source must surface `dead_code` as uncovered.
#[test]
fn compact_allowlist_guard_detects_missing_key() {
    let src = fs::read_to_string(OVERVIEW_SRC).expect("read overview.rs");
    // Drop the `"dead_code",` allowlist entry (the trailing comma pins it to the
    // allowlist array — the producer's `result["dead_code"] =` has no trailing
    // comma, and `"dead_code_unavailable",` is a different token).
    let broken = src.replace("\"dead_code\",", "");
    let uncovered = uncovered_compact_keys(&broken);
    assert!(
        uncovered.iter().any(|k| k == "dead_code"),
        "negative control failed: removing \"dead_code\" from the allowlist should make the \
         guard report it as uncovered, but got {uncovered:?}"
    );
}

// ---------------------------------------------------------------------------
// Task 2b drift-guard: the SAME rule for `tool_project_map`.
//
// The guard above covered only `tool_module_overview`, and the compact
// silent-drop bug class has now recurred three times (v0.90, v0.97.1, and the
// deps triple). `tool_project_map` is the other whitelist-rebuild surface and
// had no guard at all. Its shape differs — producer and compact form live in ONE
// function, and both are `json!({...})` literals rather than a `for key in [...]`
// allowlist — so it needs its own extractors, not a parameterization of the
// overview ones.
// ---------------------------------------------------------------------------

const PROJECT_MAP_SRC: &str = "src/mcp/server/tools/project_map.rs";

/// Producer keys `tool_project_map` sets that compact deliberately does NOT
/// carry. Empty today; add with a comment if a key is ever intentionally
/// dropped, and never to silence the guard.
const PROJECT_MAP_DELIBERATELY_DROPPED: &[&str] = &[];

/// Top-level keys of the FIRST `json!({ ... })` literal following `anchor`.
/// Brace-matches from the macro's `{` and takes `"key":` pairs at depth 1, so
/// nested object keys (`"name"`, `"file"` inside a hot-function entry) are not
/// mistaken for envelope keys.
fn json_literal_keys(region: &str, anchor: &str) -> Vec<String> {
    let start = region
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor `{anchor}` not found — did the envelope shape change?"));
    let bytes = region.as_bytes();
    let mut i = start + anchor.len() - 1; // anchor ends with the opening `{`
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    let mut depth = 0i32;
    let mut keys = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b'"' if depth == 1 => {
                let after = &region[i + 1..];
                if let Some(j) = after.find('"') {
                    let key = &after[..j];
                    if after[j + 1..].trim_start().starts_with(':') {
                        keys.push(key.to_string());
                    }
                    i += 1 + j;
                }
            }
            _ => {}
        }
        i += 1;
    }
    keys
}

/// Every `<var>["<key>"] =` assignment key in `region`, for exactly `var`.
/// The preceding-char check is what keeps `result[` from also matching
/// `compact_result[` — in this file BOTH live inside the same function, unlike
/// overview.rs where producer and forwarder are separate fns.
fn assigned_keys_for(region: &str, var: &str) -> Vec<String> {
    let marker = format!("{var}[\"");
    let mut keys = Vec::new();
    let mut offset = 0usize;
    while let Some(i) = region[offset..].find(&marker) {
        let abs = offset + i;
        let preceded_by_ident = abs > 0 && {
            let prev = region.as_bytes()[abs - 1];
            prev.is_ascii_alphanumeric() || prev == b'_'
        };
        let after = &region[abs + marker.len()..];
        if let Some(j) = after.find("\"]") {
            if !preceded_by_ident {
                let tail = after[j + 2..].trim_start();
                if tail.starts_with('=') && !tail.starts_with("==") {
                    keys.push(after[..j].to_string());
                }
            }
            offset = abs + marker.len() + j + 2;
        } else {
            break;
        }
    }
    keys
}

/// Producer keys that the compact rebuild neither includes nor deliberately drops.
fn project_map_uncovered_keys(src: &str) -> Vec<String> {
    let region = fn_region(src, "tool_project_map");
    let mut produced = json_literal_keys(region, "let r = json!({");
    produced.extend(assigned_keys_for(region, "result"));

    let mut compacted = json_literal_keys(region, "let mut compact_result = json!({");
    compacted.extend(assigned_keys_for(region, "compact_result"));

    let mut uncovered: Vec<String> = produced
        .into_iter()
        .filter(|k| {
            !compacted.contains(k) && !PROJECT_MAP_DELIBERATELY_DROPPED.contains(&k.as_str())
        })
        .collect();
    uncovered.sort();
    uncovered.dedup();
    uncovered
}

#[test]
fn project_map_compact_covers_all_result_keys() {
    let src = fs::read_to_string(PROJECT_MAP_SRC).expect("read project_map.rs");
    let uncovered = project_map_uncovered_keys(&src);
    assert!(
        uncovered.is_empty(),
        "tool_project_map produces top-level key(s) {uncovered:?} that its compact rebuild \
         drops. Add each to the `compact_result` literal (trimmed as appropriate) in \
         {PROJECT_MAP_SRC}, or document it in PROJECT_MAP_DELIBERATELY_DROPPED \
         (tests/freshness_parity.rs)."
    );
}

/// Permanent negative control: the guard must actually fire. Both halves are
/// mutated because the producer has two key SOURCES (the cached envelope literal
/// and the later `result["centrality"] =` assignment) and a guard that read only
/// one of them would still pass the positive test above.
#[test]
fn project_map_compact_guard_detects_missing_key() {
    let src = fs::read_to_string(PROJECT_MAP_SRC).expect("read project_map.rs");

    // (a) A key from the producer's envelope literal, dropped from compact.
    let broken = src.replace("\"hot_functions\": compact_hot,", "");
    let uncovered = project_map_uncovered_keys(&broken);
    assert!(
        uncovered.iter().any(|k| k == "hot_functions"),
        "negative control (a) failed: dropping `hot_functions` from the compact literal must \
         surface it as uncovered, got {uncovered:?}"
    );

    // (b) The key that arrives via a post-literal assignment instead.
    let broken = src.replace("compact_result[\"centrality\"] = json!(compact_cent);", "");
    let uncovered = project_map_uncovered_keys(&broken);
    assert!(
        uncovered.iter().any(|k| k == "centrality"),
        "negative control (b) failed: dropping the compact `centrality` assignment must \
         surface it as uncovered, got {uncovered:?}"
    );
}

// ---------------------------------------------------------------------------
// Task 3 drift-guard: CLI + MCP query-time freshness resync coverage.
// ---------------------------------------------------------------------------

const CLI_SRC: &str = "src/cli/";

/// The CLI is a module tree (`src/cli/**`), not one file, so the source-level
/// guards below read it whole. Concatenation is safe for them: every handler
/// name they look for is unique crate-wide.
fn read_cli_sources() -> String {
    fn walk(dir: &std::path::Path, out: &mut String) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort(); // deterministic order
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push_str(&fs::read_to_string(&path).unwrap());
                out.push('\n');
            }
        }
    }
    let mut out = String::new();
    walk(std::path::Path::new(CLI_SRC), &mut out);
    assert!(
        out.contains("pub fn cmd_refs"),
        "read_cli_sources found no CLI handlers under {CLI_SRC} — the module tree moved"
    );
    out
}

/// Line-number-emitting CLI subcommand handlers. Each MUST call
/// `refresh_files_if_stale` before reading line numbers out of the DB, or an
/// edited-but-not-yet-reindexed file yields stale line numbers.
///
/// ADD NEW LINE-NUMBER-EMITTING COMMANDS HERE. If you add a CLI subcommand that
/// prints file:line locations, add its handler fn name below and wire in the
/// `refresh_files_if_stale` resync — otherwise this list drifts silently.
const CLI_FRESHNESS_HANDLERS: &[&str] = &[
    "cmd_search",
    "cmd_ast_search",
    "cmd_impact",
    "cmd_overview",
    "cmd_show",
    "cmd_trace",
    "cmd_similar",
    "cmd_refs",
    "cmd_dead_code",
];

/// File-path-accepting MCP tools. Each MUST call `ensure_file_fresh_opt` (the MCP
/// shared resync path) so an edited file is reindexed before its line numbers are
/// read. ADD NEW FILE-PATH-ACCEPTING MCP TOOLS HERE.
const MCP_FRESHNESS_TOOLS: &[(&str, &str)] = &[
    ("src/mcp/server/tools/advanced.rs", "tool_dependency_graph"),
    // find_dead_code was the SIXTH path-taking tool and the one this
    // hand-list missed — the same tool the v0.107.0 entry-normalization
    // hand-enum missed (audit 2026-08-02 MED-6). If you add a path-taking
    // tool, it goes here AND calls ensure_file_fresh_opt.
    ("src/mcp/server/tools/advanced.rs", "tool_find_dead_code"),
    ("src/mcp/server/tools/ast_node.rs", "tool_get_ast_node"),
    ("src/mcp/server/tools/callgraph.rs", "tool_get_call_graph"),
    ("src/mcp/server/tools/overview.rs", "tool_module_overview"),
    ("src/mcp/server/tools/refs.rs", "tool_find_references"),
];

/// A CLI handler is "missing" the resync if its fn body has no `refresh_files_if_stale(`
/// CALL (the trailing `(` excludes bare mentions in comments like "via refresh_files_if_stale)").
fn cli_handlers_missing_refresh(cli_src: &str) -> Vec<String> {
    CLI_FRESHNESS_HANDLERS
        .iter()
        .filter(|name| !fn_region(cli_src, name).contains("refresh_files_if_stale("))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn cli_line_number_commands_call_refresh() {
    let src = read_cli_sources();
    let missing = cli_handlers_missing_refresh(&src);
    assert!(
        missing.is_empty(),
        "CLI handler(s) {missing:?} emit line numbers but do not call refresh_files_if_stale — \
         edited-but-unindexed files will yield stale line numbers. Add the resync (see the other \
         handlers in {CLI_SRC}), or if a handler no longer emits line numbers remove it from \
         CLI_FRESHNESS_HANDLERS (tests/freshness_parity.rs)."
    );
}

#[test]
fn mcp_file_path_tools_call_ensure_fresh() {
    let mut missing = Vec::new();
    for (file, tool) in MCP_FRESHNESS_TOOLS {
        let src = fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file}: {e}"));
        if !fn_region(&src, tool).contains("ensure_file_fresh_opt(") {
            missing.push(format!("{tool} ({file})"));
        }
    }
    assert!(
        missing.is_empty(),
        "MCP file-path tool(s) {missing:?} do not call ensure_file_fresh_opt — edited files are \
         served with stale line numbers. Add the resync, or remove the tool from \
         MCP_FRESHNESS_TOOLS (tests/freshness_parity.rs) if it no longer accepts file paths."
    );
}

/// Permanent negative control: neutralizing the `refresh_files_if_stale(&ctx.db, ...)`
/// call sites in a working copy of the CLI source must make the guard report the
/// affected handlers as missing. Proves the guard fires on a real omission without
/// mutating the shared, concurrently-edited src/cli.rs on disk.
#[test]
fn cli_freshness_guard_detects_missing_refresh() {
    let src = read_cli_sources();
    let broken = src.replace(
        "refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);",
        "/* neutralized for negative control */;",
    );
    let missing = cli_handlers_missing_refresh(&broken);
    assert!(
        missing.iter().any(|h| h == "cmd_refs"),
        "negative control failed: neutralizing the refresh call should flag cmd_refs as missing, \
         but got {missing:?}"
    );
}
