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

/// Extract every `<binding>["<key>"] =` assignment key found in `region`.
///
/// `binding` because the producers do not agree on a name: `tool_module_overview`
/// and `tool_project_map` build `result`, `tool_find_similar_code` builds `out`.
fn assigned_keys(region: &str, binding: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let marker = format!("{binding}[\"");
    let mut rest = region;
    while let Some(i) = rest.find(&marker) {
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

/// Top-level keys of the `let mut result = json!({ … })` SEED literal.
///
/// CON-18(a): the guard used to read `result["k"] =` assignments only, and the
/// producer introduces most of its envelope in the seed — `path`, `files_count`,
/// `active_exports`, `inactive_summary`, `hot_paths`, `summary` were six keys the
/// guard could not see, so a seventh could have been added and dropped by compact
/// with the guard still green. That is the shape of hole this whole file exists
/// to close, sitting in the guard itself.
///
/// Depth-aware so nested object literals (a per-symbol `{"name": …}` inside the
/// seed) do not read as top-level keys, and string-aware so a `format!("{}")` in
/// a value cannot unbalance the nesting.
fn seed_literal_keys(region: &str, binding: &str) -> Vec<String> {
    let anchor = format!("let mut {binding} = json!(");
    let Some(start) = region.find(&anchor) else {
        return Vec::new();
    };
    let bytes: Vec<char> = region[start + anchor.len()..].chars().collect();
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            '/' if bytes.get(i + 1) == Some(&'/') => {
                while i < bytes.len() && bytes[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
            }
            '"' => {
                let key_start = i + 1;
                let mut j = key_start;
                while j < bytes.len() && !(bytes[j] == '"' && bytes[j - 1] != '\\') {
                    j += 1;
                }
                // A key sits at depth 1 of the literal and is followed by `:`.
                let next = bytes[j + 1..].iter().find(|c| !c.is_whitespace());
                if depth == 1 && next == Some(&':') {
                    keys.push(bytes[key_start..j].iter().collect());
                }
                i = j + 1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    keys
}

/// Keys the producer inserts through `result…insert("k", …)` — the third shape
/// the report names, alongside the seed literal and index assignment. Anchored on
/// a `result` receiver so an unrelated `cache.insert(…)` in the same function is
/// not mistaken for an envelope key.
fn inserted_keys(region: &str, binding: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = region;
    while let Some(i) = rest.find(binding) {
        let after = &rest[i + binding.len()..];
        // Same statement only: an `insert` past a `;` belongs to something else.
        let stmt_end = after.find(';').unwrap_or(after.len());
        if let Some(k) = after[..stmt_end].find(".insert(\"") {
            let key_start = k + ".insert(\"".len();
            if let Some(j) = after[key_start..stmt_end].find('"') {
                keys.push(after[key_start..key_start + j].to_string());
            }
        }
        rest = after;
    }
    keys
}

/// Every top-level key the producer can put on the response envelope, in all
/// three shapes it uses to do so.
fn produced_envelope_keys(producer: &str, binding: &str) -> Vec<String> {
    let mut keys = seed_literal_keys(producer, binding);
    keys.extend(assigned_keys(producer, binding));
    keys.extend(inserted_keys(producer, binding));
    keys.sort();
    keys.dedup();
    keys
}

/// Keys the compact forwarder demonstrably handles: the `for key in [...]`
/// verbatim allowlist, plus every key it READS off the full envelope
/// (`full["k"]` / `full.get("k")`).
///
/// The read side is what covers a RENAME: compact answers `files` from
/// `full["files_count"]` and `active` from `full["active_exports"]`, so those
/// producer keys are handled even though neither name appears in the allowlist.
/// Without this, extending the producer scan to the seed literal would have
/// reported six false positives and pushed a future author to silence them in
/// DELIBERATELY_COMPACTED — turning the fix into a bigger hole than the bug.
fn compact_covered_keys(forwarder: &str) -> Vec<String> {
    let mut keys = compact_allowlist(forwarder);
    for (marker, terminator) in [("full[\"", "\""), ("full.get(\"", "\"")] {
        let mut rest = forwarder;
        while let Some(i) = rest.find(marker) {
            let after = &rest[i + marker.len()..];
            if let Some(j) = after.find(terminator) {
                keys.push(after[..j].to_string());
                rest = &after[j..];
            } else {
                break;
            }
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Extract the quoted keys from the `for key in [ ... ]` compact allowlist array.
///
/// Comment lines are stripped BEFORE splitting on `,`. Splitting first made a
/// comma inside an in-array `//` comment fuse the comment with the entry that
/// followed it, so that entry silently vanished from the parsed allowlist and the
/// guard demanded a key the author had already added — a false failure with no
/// way to see why from the message. The array holds only string literals, so
/// pulling the quoted spans out is also what makes a bare prose fragment
/// impossible to mistake for a key.
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
        .lines()
        .map(|l| l.split("//").next().unwrap_or("").trim())
        .flat_map(|l| {
            l.split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .collect::<Vec<_>>()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Return the body of `fn <name>` (braces included) by brace-matching from the
/// opening `{` of the signature to its balanced close. Robust to indentation
/// (methods inside `impl` blocks) and to nested inner `fn` helpers — both of
/// which defeat line-based boundary detection. Skips `//` line comments and
/// `"…"` string literals so braces inside `format!("{}")` / error strings and
/// `// { }` comments do not unbalance the count.
///
/// Raw strings are tracked, and that is not a hypothetical: `tool_module_overview`
/// contains `r"\\\\"`, whose closing quote sits right after a backslash. The old
/// `prev != b'\\'` escape test read that quote as escaped, so the scanner stayed
/// "inside a string" for ~30 lines and every brace there went uncounted. It
/// happened to resync where the count came out right — until a `"` in a COMMENT
/// moved the resync point, at which case the region ended 66 lines early and the
/// guard stopped seeing `result["dead_code"]` at all. A drift-guard whose region
/// detection depends on where the quotes in the prose fall is not a guard, so
/// both halves are handled properly here: `r"…"`/`r#"…"#` take no escapes, and an
/// ordinary string's quote is escaped only after an ODD run of backslashes.
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
    // `None` = ordinary string; `Some(n)` = raw string closed by `"` + n `#`.
    let mut raw_hashes: Option<usize> = None;
    let mut in_line_comment = false;
    // True when the quote at `i` is escaped: an ODD number of backslashes
    // immediately precedes it. `\\"` (an escaped BACKSLASH then the terminator)
    // has two, so it closes — which is the case the old single-char test failed.
    let escaped_quote = |bytes: &[u8], at: usize| -> bool {
        let mut back = 0usize;
        let mut j = at;
        while j > 0 && bytes[j - 1] == b'\\' {
            back += 1;
            j -= 1;
        }
        back % 2 == 1
    };
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
        } else if in_str {
            if c == b'"' {
                match raw_hashes {
                    // Raw string: no escapes; needs `"` followed by N `#`.
                    Some(n) => {
                        let closes = (0..n).all(|k| bytes.get(i + 1 + k) == Some(&b'#'));
                        if closes {
                            in_str = false;
                            raw_hashes = None;
                            i += n;
                        }
                    }
                    None => {
                        if !escaped_quote(bytes, i) {
                            in_str = false;
                        }
                    }
                }
            }
        } else if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
        } else if c == b'"' {
            in_str = true;
            // `r"`, `r#"`, `r##"` … — count the `#`s between the `r` and the quote.
            let mut hashes = 0usize;
            let mut j = i;
            while j > 0 && bytes[j - 1] == b'#' {
                hashes += 1;
                j -= 1;
            }
            raw_hashes = (j > 0 && bytes[j - 1] == b'r').then_some(hashes);
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return &src[body_start..=i];
            }
        }
        i += 1;
    }
    &src[body_start..]
}

/// The scanner every guard in this file stands on must survive the literal
/// shapes the sources it reads actually contain.
///
/// `tool_module_overview` holds `r"\\\\"`. Under the old single-char escape test
/// its closing quote read as escaped, the scanner stayed "in a string" past the
/// end of the fn, and the region it returned depended on where the next quote —
/// in code OR in a comment — happened to fall. That is how a guard reports a key
/// as uncovered that the allowlist already names, and, in the other direction,
/// how it stops seeing a key that IS silently dropped. Both directions are pinned
/// here, on inputs small enough to read.
#[test]
fn fn_region_survives_raw_strings_and_escaped_backslashes() {
    // Raw string whose content ends in a backslash, then a real brace after it.
    let raw = "fn f() { let p = r\"\\\\\\\\\"; if x { y(); } }\nfn g() { }";
    assert_eq!(
        fn_region(raw, "f"),
        "{ let p = r\"\\\\\\\\\"; if x { y(); } }",
        "a raw string ending in backslashes must not swallow the rest of the fn"
    );
    // Ordinary string ending in an ESCAPED backslash: `"a\\"` closes.
    let esc = "fn f() { let s = \"a\\\\\"; if x { y(); } }\nfn g() { }";
    assert_eq!(
        fn_region(esc, "f"),
        "{ let s = \"a\\\\\"; if x { y(); } }",
        "an even backslash run leaves the quote unescaped, so the string closes"
    );
    // ...while a genuinely escaped quote (odd run) does NOT close: the `}` inside
    // the literal must not be counted.
    let inner = "fn f() { let s = \"a\\\"}\"; z(); }\nfn g() { }";
    assert_eq!(
        fn_region(inner, "f"),
        "{ let s = \"a\\\"}\"; z(); }",
        "an escaped quote keeps the string open, so the brace inside it is text"
    );
    // A quote inside a `//` comment must not shift anything.
    let commented = "fn f() {\n // says \"hi\n if x { y(); }\n}\nfn g() { }";
    assert!(
        fn_region(commented, "f").ends_with("}\n}") || fn_region(commented, "f").contains("y();"),
        "comment quotes must not truncate the region: {:?}",
        fn_region(commented, "f")
    );

    // And the real file: the region must reach the producer's own last key.
    let src = fs::read_to_string(OVERVIEW_SRC).expect("read overview.rs");
    assert!(
        fn_region(&src, "tool_module_overview").contains("\"dead_code\""),
        "the producer region must span to its dead_code assignment — it is 200 lines \
         past the r\"\\\\\\\\\" literal that used to end the scan early"
    );
}

/// Returns full-envelope keys that compact neither forwards nor deliberately drops.
fn uncovered_compact_keys(overview_src: &str) -> Vec<String> {
    let producer = fn_region(overview_src, "tool_module_overview");
    let forwarder = fn_region(overview_src, "compact_module_overview");
    let covered = compact_covered_keys(forwarder);
    let mut uncovered: Vec<String> = produced_envelope_keys(producer, "result")
        .into_iter()
        .filter(|k| !covered.contains(k) && !DELIBERATELY_COMPACTED.contains(&k.as_str()))
        .collect();
    uncovered.sort();
    uncovered.dedup();
    uncovered
}

/// The guard's own negative control (CON-18(a)).
///
/// `compact_allowlist_covers_all_result_keys` passing proves nothing on its own —
/// it passed for the whole time the scan could not see a seed-literal key. So
/// each shape a key can enter the envelope by is fed through the real extractors
/// on a synthetic source, and each must be REPORTED when the compactor ignores
/// it. A shape this misses is a shape the production guard is blind to.
#[test]
fn the_guard_reports_an_uncovered_key_in_every_shape_it_can_arrive_in() {
    let compactor_ignoring_everything = "
        fn compact_module_overview(&self, full: &serde_json::Value) -> Result<Value> {
            let mut result = json!({ \"path\": full[\"path\"] });
            for key in [\"hint\"] { if let Some(v) = full.get(key) { result[key] = v.clone(); } }
            Ok(result)
        }";
    for (shape, producer_body) in [
        (
            "json! seed literal",
            "fn tool_module_overview(&self) -> Result<Value> {
                 let mut result = json!({ \"path\": p, \"leaked_key\": v });
                 Ok(result)
             }",
        ),
        (
            "index assignment",
            "fn tool_module_overview(&self) -> Result<Value> {
                 let mut result = json!({ \"path\": p });
                 result[\"leaked_key\"] = json!(v);
                 Ok(result)
             }",
        ),
        (
            "as_object_mut insert",
            "fn tool_module_overview(&self) -> Result<Value> {
                 let mut result = json!({ \"path\": p });
                 result.as_object_mut().unwrap().insert(\"leaked_key\".into(), json!(v));
                 Ok(result)
             }",
        ),
    ] {
        let src = format!("{producer_body}\n{compactor_ignoring_everything}");
        assert!(
            uncovered_compact_keys(&src).contains(&"leaked_key".to_string()),
            "a key introduced by {shape} must be reported as uncovered; \
             the guard saw {:?}",
            uncovered_compact_keys(&src)
        );
    }

    // ...and a key the compactor DOES read must not be reported, or the guard
    // pushes authors to silence real coverage in DELIBERATELY_COMPACTED.
    let covered_src = format!(
        "fn tool_module_overview(&self) -> Result<Value> {{
             let mut result = json!({{ \"path\": p, \"files_count\": n }});
             Ok(result)
         }}
         {compactor_ignoring_everything}"
    )
    .replace(
        "json!({ \"path\": full[\"path\"] })",
        "json!({ \"path\": full[\"path\"], \"files\": full[\"files_count\"] })",
    );
    assert!(
        uncovered_compact_keys(&covered_src).is_empty(),
        "a renamed-but-forwarded key must not be reported: {:?}",
        uncovered_compact_keys(&covered_src)
    );

    // A comment INSIDE the allowlist array must not eat the entry after it. The
    // parser split on `,` before stripping comments, so a comma in the prose
    // fused the comment with the next literal and that entry disappeared from
    // the allowlist — the guard then reported a key the author HAD added, and
    // the failure message pointed at the one place that already looked right.
    let commented_allowlist = "
        fn compact_module_overview(&self, full: &serde_json::Value) -> Result<Value> {
            let mut result = json!({ \"path\": full[\"path\"] });
            for key in [
                \"hint\",
                // forwarded because it matters, and because nothing else carries it
                \"after_comment\",
            ] { if let Some(v) = full.get(key) { result[key] = v.clone(); } }
            Ok(result)
        }";
    let src = format!(
        "fn tool_module_overview(&self) -> Result<Value> {{
             let mut result = json!({{ \"path\": p, \"after_comment\": v }});
             Ok(result)
         }}
         {commented_allowlist}"
    );
    assert!(
        uncovered_compact_keys(&src).is_empty(),
        "an entry that follows a comma-bearing comment is still in the allowlist; \
         the guard saw {:?}",
        uncovered_compact_keys(&src)
    );
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
// Task 2c drift-guard: the SAME rule for project_map's NESTED array entries.
//
// The two guards above cover the top-level envelope and the `modules[]` entry
// shape. `project_map` builds three MORE arrays — `module_dependencies`,
// `entry_points`, `hot_functions` — each with its own producer literal and its
// own hand-written compact rebuild, and none of them had a guard. Both had
// drifted: compact dropped `entry_points.route` (the URL — i.e. compact mode
// answered "what is the HTTP surface" with handler names and no endpoints) and
// `module_dependencies.imports` (the edge weight that makes the dependency list
// rankable). Same bug class, one nesting level down.
// ---------------------------------------------------------------------------

/// Per-entry keys the compact rebuild drops on purpose, keyed by array name.
/// Add with a reason; never to silence the guard. Empty today — every field in
/// these three arrays is a single scalar that carries a distinct signal.
const PROJECT_MAP_ENTRY_DELIBERATELY_DROPPED: &[(&str, &str)] = &[];

/// The `let <anchor> …;` builder region, ending at `terminator`.
///
/// Anchored on the statement's own terminator rather than on "the next `let`",
/// so reordering the four builders inside `tool_project_map` cannot silently
/// make one region swallow another.
fn builder_region<'a>(src: &'a str, anchor: &str, terminator: &str) -> &'a str {
    let start = src
        .find(anchor)
        .unwrap_or_else(|| panic!("anchor `{anchor}` not found in project_map.rs"));
    let after = &src[start..];
    let end = after.find(terminator).unwrap_or_else(|| {
        panic!("`{anchor}` region no longer ends with `{terminator}` — repoint the guard")
    });
    &after[..end]
}

/// Keys one array-entry builder introduces: the `json!({…})` literal plus any
/// conditional `obj["k"] =` assignment that follows it in the same closure.
fn entry_keys(region: &str) -> Vec<String> {
    let mut keys = json_literal_keys(region, "json!({");
    keys.extend(assigned_keys_for(region, "obj"));
    keys.sort();
    keys.dedup();
    keys
}

/// `(array, producer-anchor, compact-anchor)` for every nested array.
const PROJECT_MAP_ARRAYS: &[(&str, &str, &str)] = &[
    ("module_dependencies", "let deps_json", "let compact_deps"),
    ("entry_points", "let routes_json", "let compact_entries"),
    ("hot_functions", "let hot_json", "let compact_hot"),
];

fn project_map_uncovered_entry_keys(src: &str, array: &str) -> Vec<String> {
    let (_, producer_anchor, compact_anchor) = PROJECT_MAP_ARRAYS
        .iter()
        .find(|(name, _, _)| *name == array)
        .expect("unknown array");
    let produced = entry_keys(builder_region(src, producer_anchor, ".collect();"));
    assert!(
        produced.len() > 1,
        "producer scan for `{array}` found only {produced:?} — the anchors are wrong, \
         not the code (a guard that reads nothing passes vacuously)"
    );
    let compacted = entry_keys(builder_region(src, compact_anchor, ".unwrap_or_default();"));
    let mut uncovered: Vec<String> = produced
        .into_iter()
        .filter(|k| {
            !compacted.contains(k)
                && !PROJECT_MAP_ENTRY_DELIBERATELY_DROPPED.contains(&(array, k.as_str()))
        })
        .collect();
    uncovered.sort();
    uncovered.dedup();
    uncovered
}

#[test]
fn project_map_compact_forwards_every_nested_entry_key() {
    let src = fs::read_to_string(PROJECT_MAP_SRC).expect("read project_map.rs");
    for (array, _, _) in PROJECT_MAP_ARRAYS {
        let uncovered = project_map_uncovered_entry_keys(&src, array);
        assert!(
            uncovered.is_empty(),
            "the full `{array}` entries emit {uncovered:?}, which the compact rebuild in \
             {PROJECT_MAP_SRC} neither forwards nor drops on the record. Forward them, or add \
             (\"{array}\", \"<key>\") to PROJECT_MAP_ENTRY_DELIBERATELY_DROPPED with a reason."
        );
    }
}

/// Permanent negative control, one mutation per array and per key SOURCE: the
/// `json!` literal for all three, plus the conditional `obj[…] =` assignment that
/// only `hot_functions` uses. A scanner that read the literal alone would still
/// pass the positive test above.
#[test]
fn project_map_nested_entry_guard_detects_missing_key() {
    let src = fs::read_to_string(PROJECT_MAP_SRC).expect("read project_map.rs");

    for (array, key, cut) in [
        (
            "module_dependencies",
            "imports",
            "\"imports\": d[\"imports\"],",
        ),
        ("entry_points", "route", "\"route\": e[\"route\"],"),
        ("hot_functions", "name", "\"name\": h[\"name\"],"),
    ] {
        let broken = src.replace(cut, "");
        // Compare lengths, not the strings: an `assert_ne!` on two ~14 KB
        // sources dumps both into the failure output and buries the message.
        assert!(
            broken.len() < src.len(),
            "negative control for `{array}.{key}` cut nothing — `{cut}` is no longer \
             the literal in the compact rebuild, so the control is inert"
        );
        let uncovered = project_map_uncovered_entry_keys(&broken, array);
        assert!(
            uncovered.iter().any(|k| k == key),
            "negative control failed: dropping `{key}` from the compact `{array}` literal must \
             surface it as uncovered, got {uncovered:?}"
        );
    }

    // The other key source: a field that reaches the entry via a conditional
    // assignment rather than the literal.
    let broken = src.replace(
        "obj[\"test_caller_count\"] = h[\"test_caller_count\"].clone();",
        "",
    );
    assert!(
        broken.len() < src.len(),
        "negative control for `test_caller_count` cut nothing — the conditional assignment \
         in the compact rebuild was renamed, so the control is inert"
    );
    let uncovered = project_map_uncovered_entry_keys(&broken, "hot_functions");
    assert!(
        uncovered.iter().any(|k| k == "test_caller_count"),
        "negative control failed: dropping the conditional `test_caller_count` assignment from \
         the compact rebuild must surface it as uncovered, got {uncovered:?}"
    );
}

/// Runtime proof of the same drift, from the outside: the symptom is that
/// `compact: true` answers an HTTP-surface question without any URL in it.
#[test]
fn compact_project_map_keeps_route_and_import_counts() {
    // Two DIRECTORIES: module_dependencies is a module→module edge, so a
    // single-directory fixture would leave the deps assertion vacuous.
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src/handlers")).unwrap();
    fs::create_dir_all(project.path().join("src/api")).unwrap();
    fs::write(
        project.path().join("src/handlers/widgets.js"),
        "function widgetsHandler(req, res) { res.json([]); }\n\
         function healthHandler(req, res) { res.json({ ok: true }); }\n\
         module.exports = { widgetsHandler, healthHandler };\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/api/server.js"),
        "const { widgetsHandler, healthHandler } = require('../handlers/widgets');\n\
         const app = require('express')();\n\
         function mount() {\n\
        \x20 app.get('/widgets', widgetsHandler);\n\
        \x20 app.post('/widgets/:id', widgetsHandler);\n\
        \x20 app.get('/health', healthHandler);\n\
         }\n\
         module.exports = { mount };\n",
    )
    .unwrap();

    let server = init_server(&project);
    let msg = tool_call_json("project_map", json!({ "compact": true }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let entries = result["entry_points"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !entries.is_empty(),
        "fixture produced no entry_points at all — the test cannot prove anything about \
         `route`; compact result was:\n{}",
        serde_json::to_string_pretty(&result).unwrap()
    );
    let routes: Vec<&str> = entries
        .iter()
        .filter_map(|e| e.get("route").and_then(|r| r.as_str()))
        .collect();
    // The extractor emits `"<METHOD> <path>"`, so match on the path substring.
    assert!(
        routes.iter().any(|r| r.contains("/widgets")),
        "compact entry_points must carry the URL — that IS the answer to \"what is the HTTP \
         surface\". Got {entries:?}"
    );

    let deps = result["module_dependencies"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !deps.is_empty(),
        "fixture produced no module_dependencies; compact result was:\n{}",
        serde_json::to_string_pretty(&result).unwrap()
    );
    assert!(
        deps.iter().all(|d| d.get("imports").is_some()),
        "compact module_dependencies must keep the `imports` weight — without it every edge \
         reads as equally important. Got {deps:?}"
    );
}

/// The trim itself is fine; answering with a silently short list is not. With
/// more hot functions than compact shows, the envelope must say so.
#[test]
fn compact_project_map_discloses_hot_function_truncation() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    let mut helpers = String::new();
    let mut calls = String::new();
    for i in 0..12 {
        helpers.push_str(&format!(
            "export function helper{i}(): number {{ return {i}; }}\n"
        ));
        calls.push_str(&format!("  t += helper{i}();\n"));
    }
    fs::write(project.path().join("src/helpers.ts"), &helpers).unwrap();
    fs::write(
        project.path().join("src/main.ts"),
        format!(
            "import {{ {} }} from './helpers';\n\
             export function run(): number {{\n  let t = 0;\n{calls}  return t;\n}}\n",
            (0..12)
                .map(|i| format!("helper{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .unwrap();

    let server = init_server(&project);
    let full = parse_tool_result(
        &server
            .handle_message(&tool_call_json("project_map", json!({})))
            .unwrap(),
    );
    let full_hot = full["hot_functions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        full_hot.len() > 10,
        "fixture produced only {} hot functions — cannot exercise the trim; full envelope:\n{}",
        full_hot.len(),
        serde_json::to_string_pretty(&full).unwrap()
    );

    let compact = parse_tool_result(
        &server
            .handle_message(&tool_call_json("project_map", json!({ "compact": true })))
            .unwrap(),
    );
    let compact_hot = compact["hot_functions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        compact_hot.len() < full_hot.len(),
        "precondition: compact is expected to trim here, got {} vs {}",
        compact_hot.len(),
        full_hot.len()
    );
    assert_eq!(
        compact["hot_functions_truncated"],
        json!(true),
        "compact showed {} of {} hot functions and must disclose the cut — an undisclosed trim \
         reads as \"this is the whole list\". Compact envelope was:\n{}",
        compact_hot.len(),
        full_hot.len(),
        serde_json::to_string_pretty(&compact).unwrap()
    );
}

/// The disclosure must be conditional, not a constant `true`: a project with
/// fewer hot functions than the cap loses nothing and must not claim it did.
#[test]
fn compact_project_map_omits_truncation_flag_when_nothing_was_cut() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/tiny.ts"),
        "export function leaf(): number { return 1; }\n\
         export function root(): number { return leaf(); }\n",
    )
    .unwrap();

    let server = init_server(&project);
    let compact = parse_tool_result(
        &server
            .handle_message(&tool_call_json("project_map", json!({ "compact": true })))
            .unwrap(),
    );
    let n = compact["hot_functions"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        n <= 10,
        "precondition: fixture must stay under the compact cap, got {n}"
    );
    assert!(
        compact.get("hot_functions_truncated").is_none(),
        "nothing was cut, so the envelope must not claim a truncation. Got:\n{}",
        serde_json::to_string_pretty(&compact).unwrap()
    );
}

// ---------------------------------------------------------------------------
// CON-10: `get_ast_node(node_id)` slices LIVE file bytes at INDEXED line numbers.
//
// The node_id branch returns before any refresh — the source comment reasoned
// that a node_id lookup "has no path to refresh against", but the row it just
// read carries the path. With `context_lines` defaulting to 3 on this branch,
// `read_source_context` then opens the CURRENT file and takes a window at the
// PRE-EDIT offsets, so inserting lines above the symbol silently returns a
// window of unrelated code labelled as that symbol's source.
// ---------------------------------------------------------------------------

/// The MCP runtime proof lives in `src/mcp/server/mod.rs` next to the other
/// FRS-2 tests, not here: it needs `close_other_freshness_paths` to shut the
/// no-watcher debounce, and integration tests cannot reach `server.timing`.
/// Written here first, it passed vacuously — `TimingConfig::for_tests` sets that
/// debounce to ZERO, so every `ensure_indexed()` ran a full merkle pass and
/// refreshed the file through a path production does not take.
///
/// Same defect, same mechanism, on the CLI leg: `show --node-id` skips the
/// resync that `show <symbol>` performs, because the resync sits in the ELSE
/// branch. The existing `cli_line_number_commands_call_refresh` guard reads
/// `cmd_show` as covered — a per-function "does it call refresh" scan cannot see
/// that one of two branches returns before reaching the call.
/// The `--node-id` arm of `cmd_show`, as source text.
fn cmd_show_node_id_branch(src: &str) -> &str {
    let region = fn_region(src, "cmd_show");
    let i = region
        .find("if let Some(nid) = node_id_arg {")
        .expect("`cmd_show` no longer has an `if let Some(nid) = node_id_arg` branch");
    let rest = &region[i..];
    let end = rest
        .find("    } else {")
        .expect("`cmd_show`'s node_id branch no longer ends at the `else` — repoint the guard");
    &rest[..end]
}

#[test]
fn cli_show_by_node_id_refreshes_like_show_by_symbol() {
    let src = fs::read_to_string("src/cli/commands/show.rs").expect("read show.rs");
    let branch = cmd_show_node_id_branch(&src);
    assert!(
        branch.contains("refresh_files_if_stale("),
        "`cmd_show`'s --node-id branch returns its node without a resync, while the symbol \
         branch resyncs. `show` prints start_line/end_line and slices live file bytes at those \
         offsets, so the branch that skips the refresh answers with a window cut at pre-edit \
         coordinates. Branch source was:\n{branch}"
    );
    assert!(
        branch.contains("reresolve_node_by_identity("),
        "the --node-id branch resyncs but then keeps resolving by id. `nodes.id` is a rowid \
         alias with no AUTOINCREMENT, so a re-index reuses freed ids and the caller's id can \
         come back attached to a DIFFERENT symbol — re-resolve by identity. Branch source \
         was:\n{branch}"
    );
}

/// Permanent negative control. Both halves are mutated: a guard that only
/// checked for the resync would go green again the moment someone "simplified"
/// the identity re-resolution back into a lookup by id, which is the half that
/// silently answers about the wrong symbol.
#[test]
fn cli_show_node_id_guard_detects_a_branch_that_skips_either_half() {
    let src = fs::read_to_string("src/cli/commands/show.rs").expect("read show.rs");
    for cut in ["refresh_files_if_stale(", "reresolve_node_by_identity("] {
        // Rename only inside the node_id branch, so the symbol branch's own
        // `refresh_files_if_stale` call cannot mask the mutation.
        let branch = cmd_show_node_id_branch(&src);
        assert!(
            branch.contains(cut),
            "negative control cut `{cut}` is not in the branch — the control is inert"
        );
        let broken_branch = branch.replace(cut, "mutated_away(");
        let broken = src.replace(branch, &broken_branch);
        assert!(
            !cmd_show_node_id_branch(&broken).contains(cut),
            "negative control failed: after mutating `{cut}` the branch scan still finds it, \
             so the guard would stay green on a real regression"
        );
    }
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
    // Wired for a long time, absent from this list until the 2026-08-29 audit
    // (CON-18b) — their resync could have been deleted and every guard would
    // still have been green. A list that omits what it already covers is not a
    // guard, it is a coincidence.
    "cmd_callgraph",
    "cmd_report",
    // Architecture-level commands. They were written before query-time freshness
    // existed and were never swept when it was wired command by command, so an
    // edited file kept its pre-edit rows here while every sibling refreshed
    // (audit 2026-08-29 CON-03). `cycles` and `surprising` are deliberately NOT
    // here: neither emits a file path, so there is no result set to refresh —
    // adding a whole-index scan to a read command is the cost this budgeted,
    // result-set-scoped mechanism exists to avoid.
    "cmd_map",
    "cmd_tour",
    "cmd_centrality",
];

/// CLI handlers that refresh the paths the CALLER named rather than the paths
/// their own query produced, via `refresh_input_files` (`RefreshScope::IncludeNew`).
/// `affected` is the reason the distinction exists: its input is typically
/// `git diff --name-only`, so a file the branch just added is the normal case,
/// and the `IndexedOnly` scope every other command wants would drop it —
/// producing "0 test file(s) to re-run" from a real change (audit CON-03).
const CLI_INPUT_REFRESH_HANDLERS: &[&str] = &["cmd_affected", "cmd_deps"];

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
fn cli_input_taking_commands_refresh_their_inputs() {
    let src = read_cli_sources();
    let missing: Vec<_> = CLI_INPUT_REFRESH_HANDLERS
        .iter()
        .filter(|name| !fn_region(&src, name).contains("refresh_input_files("))
        .collect();
    assert!(
        missing.is_empty(),
        "CLI handler(s) {missing:?} classify caller-supplied paths without calling \
         refresh_input_files — a file the branch just added is not in the index, so it is \
         dropped from the answer instead of being indexed. Add the refresh, or remove the \
         handler from CLI_INPUT_REFRESH_HANDLERS (tests/freshness_parity.rs)."
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

/// MCP tools whose `path` argument names a DIRECTORY (or is omitted entirely),
/// not a file. `ensure_file_fresh_opt` cannot serve them — it classifies a
/// directory as fresh and returns, `did_reindex` stays false, and the caches it
/// would have evicted keep serving pre-edit line numbers. So the guard above
/// counted their no-op call as coverage, which is how CON-02 stayed invisible
/// while a green parity table said otherwise.
///
/// Coverage for these is `RESULT_REFRESH_TOOLS`, which refreshes the files the
/// ANSWER points at. ADD A DIRECTORY-SCOPED TOOL HERE, not just to
/// `MCP_FRESHNESS_TOOLS`.
const MCP_DIRECTORY_SCOPED_TOOLS: &[&str] = &["module_overview", "find_dead_code"];

/// The `RESULT_REFRESH_TOOLS` entries as spelled in the source, so this guard
/// reads the production list rather than a copy of it.
fn result_refresh_tools() -> Vec<String> {
    let src = fs::read_to_string("src/mcp/server/freshness.rs")
        .expect("read src/mcp/server/freshness.rs — has the module moved?");
    let start = src
        .find("RESULT_REFRESH_TOOLS: &[&str] = &[")
        .expect("RESULT_REFRESH_TOOLS declaration not found — has it been renamed?");
    let body = &src[start..];
    let end = body.find("];").expect("unterminated RESULT_REFRESH_TOOLS");
    body[..end]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            // Only quoted list entries; the block comment inside the list names
            // some of these tools in prose and must not count as membership.
            l.strip_prefix('"')
                .and_then(|r| r.split('"').next())
                .filter(|_| l.ends_with("\","))
                .map(|s| s.to_string())
        })
        .collect()
}

#[test]
fn directory_scoped_mcp_tools_are_result_refreshed() {
    let listed = result_refresh_tools();
    assert!(
        listed.len() >= 8,
        "parsed RESULT_REFRESH_TOOLS as {listed:?} — the parse, not the list, is what broke"
    );
    let missing: Vec<_> = MCP_DIRECTORY_SCOPED_TOOLS
        .iter()
        .filter(|t| !listed.iter().any(|l| l == *t))
        .collect();
    assert!(
        missing.is_empty(),
        "directory-scoped MCP tool(s) {missing:?} are not in RESULT_REFRESH_TOOLS. \
         `ensure_file_fresh_opt` is a no-op for a directory path, so without result-set \
         refresh they answer from a pre-edit index with no disclosure — and the \
         mcp_file_path_tools_call_ensure_fresh guard above will still pass, because the \
         no-op call is present in the source."
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

// ---------------------------------------------------------------------------
// 4th recurrence of the forwarder-drops-keys class, on `attach_similar`.
//
// `tool_find_similar_code` builds the full envelope; `attach_similar` is the
// ONLY public path to it (`find_similar_code` is no longer a listed tool —
// src/mcp/tools.rs asserts its absence — so every MCP caller reaches this code
// through `get_ast_node include_similar`). The forwarder hand-picks `results`
// and `hint`, which means the SURF-06 noise disclosure added on 2026-09-05
// stops at the function boundary: the CLI twin prints it, the MCP caller gets a
// short list and no reason. Same shape as the three guards above.
// ---------------------------------------------------------------------------

/// Build a project whose only vector neighbours are test-file symbols.
///
/// All three vectors are identical, so distance separates nothing and the noise
/// filter is the only thing that can shorten the list. Vectors are hand-built
/// (not embedded) so this runs on the no-embed leg: `find_similar_code` reads a
/// STORED embedding for `node_id` rather than embedding a query, so no model is
/// involved. Mirrors the CLI twin's fixture
/// (cli_e2e.rs::test_cli_similar_discloses_noise_filtered_candidates).
fn similar_noise_fixture() -> (TempDir, i64) {
    use code_graph_mcp::storage::queries;

    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("app.rs"), "pub fn prod_alpha() {}\n").unwrap();
    fs::create_dir_all(project.path().join("tests")).unwrap();
    fs::write(
        project.path().join("tests/helpers.rs"),
        "pub fn helper_one() {}\npub fn helper_two() {}\n",
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    let conn = db.conn();
    // Created AFTER `Database::open`, which is what lets them survive its dim
    // migration — the obstacle `advanced.rs` cited for having no fixture here.
    conn.execute_batch(&code_graph_mcp::storage::schema::create_vec_tables_sql())
        .unwrap();

    let mut vectors = Vec::new();
    for name in ["prod_alpha", "helper_one", "helper_two"] {
        let n = queries::get_nodes_by_name(conn, name)
            .unwrap()
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("fixture node {name} must be indexed"));
        vectors.push((n.id, vec![0.1f32; 384]));
    }
    let seed = vectors[0].0;
    queries::insert_node_vectors_batch(conn, &vectors).unwrap();
    drop(db);
    (project, seed)
}

/// `get_ast_node include_similar` must say WHY the similar list came back short.
///
/// Without this the caller sees `similar: []` and reads it as "the index has
/// nothing else" — the exact misreading SURF-06 existed to end. Raising
/// `similar_top_k` would not have helped: the filter that removed them is not
/// adjustable, and nothing on this surface said so.
#[test]
fn get_ast_node_include_similar_discloses_noise_filtered_neighbours() {
    let (project, seed) = similar_noise_fixture();
    let server = init_server(&project);
    let resp = server
        .handle_message(&tool_call_json(
            "get_ast_node",
            json!({ "node_id": seed, "include_similar": true, "similar_top_k": 5 }),
        ))
        .expect("get_ast_node should not error");
    let out = parse_tool_result(&resp);

    assert_eq!(
        out["similar"].as_array().map(|a| a.len()),
        Some(0),
        "fixture precondition: both neighbours are test-file symbols, so the \
         noise filter should empty the list. Got: {out}"
    );
    let disclosure = out
        .as_object()
        .expect("tool result is an object")
        .iter()
        .filter(|(k, _)| k.starts_with("similar"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        disclosure.contains("dropped as non-answers"),
        "the two neighbours were removed by a filter nothing named on this \
         surface. `tool_find_similar_code` sets `noise_filtered_note`; \
         `attach_similar` must forward it. similar* keys were: {disclosure}"
    );
    assert!(
        disclosure.contains("not adjustable"),
        "and the message must not send the caller to raise similar_top_k, which \
         would do nothing here. similar* keys were: {disclosure}"
    );
}

const AST_NODE_SRC: &str = "src/mcp/server/tools/ast_node.rs";
const ADVANCED_SRC: &str = "src/mcp/server/tools/advanced.rs";

/// Envelope keys `attach_similar` deliberately does NOT forward, each with the
/// reason it is not a hole. Anything not listed here and not forwarded is one.
const SIMILAR_DELIBERATELY_DROPPED: &[(&str, &str)] = &[
    // The caller passed this in; echoing it back at a nested key adds nothing.
    ("query_node_id", "the caller's own argument"),
    ("count", "`similar` is an array — the caller can count it"),
    ("top_k", "the caller's own `similar_top_k` argument"),
    (
        "max_distance",
        "not settable through get_ast_node; a fixed default",
    ),
    // These two are the STRUCTURED half of what `hint` already says in prose,
    // and `hint` IS forwarded (as `similar_hint`). Forwarding the numbers too
    // would be reasonable; not forwarding them leaves nothing unsaid.
    (
        "cutoff_applied",
        "carried in prose by `hint` → `similar_hint`",
    ),
    (
        "cutoff_dropped",
        "carried in prose by `hint` → `similar_hint`",
    ),
];

/// Keys `attach_similar` reads off the inner envelope (`v.get("k")`).
fn similar_forwarded_keys(forwarder: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let marker = "v.get(\"";
    let mut rest = forwarder;
    while let Some(i) = rest.find(marker) {
        let after = &rest[i + marker.len()..];
        if let Some(j) = after.find('"') {
            keys.push(after[..j].to_string());
            rest = &after[j..];
        } else {
            break;
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn unforwarded_similar_keys(advanced_src: &str, ast_node_src: &str) -> Vec<String> {
    let producer = fn_region(advanced_src, "tool_find_similar_code");
    let forwarder = fn_region(ast_node_src, "attach_similar");
    let forwarded = similar_forwarded_keys(forwarder);
    let mut missing: Vec<String> = produced_envelope_keys(producer, "out")
        .into_iter()
        .filter(|k| {
            !forwarded.contains(k) && !SIMILAR_DELIBERATELY_DROPPED.iter().any(|(d, _)| d == k)
        })
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

/// 4th recurrence of the forwarder-drops-keys class, this one on `attach_similar`.
///
/// `find_similar_code` is not a listed tool, so this forwarder is the only path
/// from `tool_find_similar_code`'s envelope to any caller: a key it does not
/// copy is UNREACHABLE. That is how the SURF-06 noise disclosure shipped
/// visible on the CLI and invisible on MCP — `noise_filtered_note` was set,
/// then dropped one frame later, and `similar: []` was all the caller saw.
#[test]
fn attach_similar_forwards_every_key_find_similar_code_produces() {
    let missing = unforwarded_similar_keys(
        &fs::read_to_string(ADVANCED_SRC).expect("read advanced.rs"),
        &fs::read_to_string(AST_NODE_SRC).expect("read ast_node.rs"),
    );
    assert!(
        missing.is_empty(),
        "tool_find_similar_code sets top-level key(s) {missing:?} that attach_similar \
         never copies onto the get_ast_node result. `find_similar_code` is not a listed \
         tool, so those keys reach nobody. Forward each in {AST_NODE_SRC}, or add it to \
         SIMILAR_DELIBERATELY_DROPPED with the reason it is not a hole."
    );
}

/// Negative control: the guard above is green, which proves nothing until the
/// detector is shown to still see an omission. Neutralizes the forwarding loop
/// in a COPY of the source — the working tree is never touched.
#[test]
fn attach_similar_guard_detects_a_dropped_key() {
    let ast_node = fs::read_to_string(AST_NODE_SRC).expect("read ast_node.rs");
    let broken = ast_node.replace(
        "v.get(\"noise_filtered_note\")",
        "None::<&serde_json::Value>",
    );
    assert_ne!(broken, ast_node, "negative control never mutated anything");
    let missing = unforwarded_similar_keys(
        &fs::read_to_string(ADVANCED_SRC).expect("read advanced.rs"),
        &broken,
    );
    assert!(
        missing.iter().any(|k| k == "noise_filtered_note"),
        "removing the noise_filtered_note forward should flag it as unreachable, got {missing:?}"
    );
}
