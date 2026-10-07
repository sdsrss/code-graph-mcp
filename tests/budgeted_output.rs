//! P1 #2 — budgeted output and a runnable next step at every cut.
//!
//! * `--budget <tokens>` (CLI) / `max_tokens` (MCP) on map/project_map,
//!   overview/module_overview, callgraph/get_call_graph and show/get_ast_node:
//!   the rendered answer lands within ±15% of the budget (bytes/3), lower-ranked
//!   units are shortened before any is dropped, and nothing is cut mid-member.
//! * Every truncation — budgeted or one of the existing default tiers — ends
//!   with a command; each test here RUNS that command and checks it returns
//!   what was left out.
//! * Without a budget the answers are the pre-change bytes plus the added
//!   `next:` line(s): pinned against output captured from the binary built at
//!   afffd6b (tests/data/budget_base/, see `BASE_CAPTURE` below).

mod common;

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};
use tempfile::TempDir;

// ---- fixture ----------------------------------------------------------------

/// 40 packages that import each other and a shared util module (80 module
/// dependencies, 43 modules), a `core` directory with 35 called and 22 uncalled
/// symbols, and a JS hub with 150 callees — every default compression tier of
/// the four tools fires on it.
pub fn write_fixture(root: &Path) {
    let w = |rel: &str, body: String| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    let mut helpers = String::new();
    for n in 0..20 {
        helpers.push_str(&format!("def h_{n}(x):\n    return x + {n}\n\n\n"));
    }
    w("util/helpers.py", helpers);
    w("util/__init__.py", String::new());
    for i in 0..40 {
        let j = (i + 1) % 40;
        let h = i % 20;
        let mut s = String::new();
        s.push_str(&format!("from util.helpers import h_0, h_{h}\n"));
        s.push_str(&format!("from pkg_{j:02}.mod import f_{j}_0\n\n\n"));
        s.push_str(&format!(
            "def f_{i}_0(x):\n    return h_0(x) + h_{h}(x)\n\n\n"
        ));
        s.push_str(&format!(
            "def f_{i}_1(x):\n    return f_{j}_0(x) + f_{i}_0(x)\n\n\n"
        ));
        s.push_str(&format!("def f_{i}_2(x):\n    return f_{i}_1(x)\n\n\n"));
        s.push_str(&format!(
            "class C_{i}:\n    def run(self):\n        return f_{i}_2(1)\n"
        ));
        w(&format!("pkg_{i:02}/mod.py"), s);
        w(&format!("pkg_{i:02}/__init__.py"), String::new());
    }
    let mut api = String::new();
    for n in 0..35 {
        api.push_str(&format!(
            "def api_{n:02}(request, payload, options=None):\n    return (request, payload, options, {n})\n\n\n"
        ));
    }
    for n in 0..12 {
        api.push_str(&format!("def unused_{n:02}():\n    return {n}\n\n\n"));
    }
    for n in 0..10 {
        api.push_str(&format!("class Unused{n:02}:\n    pass\n\n\n"));
    }
    w("core/api.py", api);
    w("core/__init__.py", String::new());
    let mut uses = String::from("from core.api import *\n\n\ndef use_all(r, p):\n");
    for n in 0..35 {
        uses.push_str(&format!("    api_{n:02}(r, p)\n"));
    }
    w("core/use.py", uses);
    let mut js = String::from("function hub() {\n");
    for n in 0..150 {
        js.push_str(&format!("  callee_{n}();\n"));
    }
    js.push_str("}\n");
    for n in 0..150 {
        js.push_str(&format!("function callee_{n}() {{}}\n"));
    }
    w("dense.js", js);
}

fn index(root: &Path) {
    let db_dir = root.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, root, None, None).unwrap();
}

/// Writes and indexes the fixture at `$CG_BUDGET_FIXTURE_DUMP`, so the
/// pre-change binary can be run over the very same index (how
/// tests/data/budget_base/ was captured — `scripts` not needed: run the base
/// binary's CLI and `serve` over that directory).
#[test]
#[ignore]
fn dump_fixture() {
    let dir = std::env::var("CG_BUDGET_FIXTURE_DUMP").expect("set CG_BUDGET_FIXTURE_DUMP");
    write_fixture(Path::new(&dir));
    index(Path::new(&dir));
}

// ---- harness ----------------------------------------------------------------

fn fixture() -> TempDir {
    let p = TempDir::new().unwrap();
    write_fixture(p.path());
    index(p.path());
    p
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_code-graph-mcp")
}

fn cli_in(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn cli(dir: &Path, args: &[&str]) -> String {
    let (out, code) = cli_in(dir, args);
    assert_eq!(code, 0, "{args:?} exited {code}: {out}");
    out
}

/// The words of a `next` command, as a POSIX shell would split them (the
/// commands only ever use bare words and single quotes, with `'\''` for a quote).
fn shell_words(cmd: &str) -> Vec<String> {
    assert!(!cmd.contains("; "), "one command expected: {cmd}");
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

/// Run a suggested `next` command in `dir` and return its stdout.
fn run_next(dir: &Path, cmd: &str) -> String {
    let words = shell_words(cmd);
    assert_eq!(
        words[0], "code-graph-mcp",
        "not a code-graph-mcp command: {cmd}"
    );
    let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
    cli(dir, &args)
}

fn mcp(server: &code_graph_mcp::mcp::server::McpServer, tool: &str, args: Value) -> (Value, usize) {
    let req = common::tool_call_json(tool, args);
    let resp = server.handle_message(&req).unwrap().unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool}: no text in {parsed}"))
        .to_string();
    (serde_json::from_str(&text).unwrap(), text.len())
}

fn base(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/budget_base")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Split a text answer into (the answer without the added `next:` lines, the
/// commands those lines named). Each `next:` line must directly follow a line
/// that announced a cut — that is the only place this change adds one.
fn strip_next_lines(out: &str) -> (String, Vec<String>) {
    let mut kept = String::new();
    let mut cmds = Vec::new();
    let mut prev = "";
    for line in out.split_inclusive('\n') {
        let t = line.trim_start();
        if let Some(cmd) = t.strip_prefix("next: ") {
            assert!(
                prev.contains("... and ") || prev.contains("… budget "),
                "a next line must follow a cut notice; previous line was {prev:?}"
            );
            cmds.push(cmd.trim_end().to_string());
        } else {
            kept.push_str(line);
        }
        prev = line;
    }
    (kept, cmds)
}

// ---- default answers: pre-change bytes + next lines ----------------------------

/// `BASE_CAPTURE`: tests/data/budget_base/ holds what the binary built at
/// afffd6b (md5 e891013b…) printed for these commands over `write_fixture`,
/// indexed by `dump_fixture`. The new binary must print the same bytes once
/// the `next:` lines are taken out, and exactly the listed next commands.
/// Deliberate edits since: in `cli_show_run.txt` the 40 `C_i.run` methods,
/// which nothing in the fixture calls, read `Impact: UNKNOWN` instead of
/// `Impact: LOW` — a function with no caller in the graph has unknown risk
/// (see `function_with_no_callers_at_all_is_unknown` in src/graph/impact.rs);
/// and in the two `mcp_project_map*.json` baselines the `freshness` object,
/// which counted files past the 32-file scan cap as changed on an untouched
/// fixture (`stale_kept` 25 and 20; af1a9fb); and in
/// `mcp_callgraph_hub_rollup.json` `boundaries.unresolved_calls` `{"total":
/// 0}`, the count of `hub`'s calls with no resolved target (D#229).
#[test]
fn default_text_answers_are_the_old_bytes_plus_next_lines() {
    let p = fixture();
    let cases: &[(&str, &[&str], &[&str])] = &[
        ("cli_map.txt", &["map"], &["code-graph-mcp map --json"]),
        (
            "cli_map_compact.txt",
            &["map", "--compact"],
            &[
                "code-graph-mcp map",
                "code-graph-mcp map --json",
                "code-graph-mcp map",
            ],
        ),
        ("cli_overview_core.txt", &["overview", "core"], &[]),
        ("cli_overview_api.txt", &["overview", "core/api.py"], &[]),
        ("cli_callgraph_h0.txt", &["callgraph", "h_0"], &[]),
        (
            "cli_show_run.txt",
            &["show", "run", "--refs", "--impact"],
            &[],
        ),
        ("cli_show_hub.txt", &["show", "hub", "--refs"], &[]),
    ];
    for (file, args, want_next) in cases {
        let out = cli(p.path(), args);
        let (stripped, next) = strip_next_lines(&out);
        assert_eq!(
            stripped,
            base(file),
            "{args:?}: differs from the pre-change bytes"
        );
        assert_eq!(&next, want_next, "{args:?}: next lines");
    }
}

fn without_next(mut v: Value) -> (Value, Option<String>) {
    let next = v
        .as_object_mut()
        .and_then(|o| o.remove("next"))
        .and_then(|n| n.as_str().map(str::to_string));
    (v, next)
}

#[test]
fn default_json_answers_are_the_old_ones_plus_next() {
    let p = fixture();
    let out = cli(p.path(), &["map", "--json", "--compact"]);
    let (v, next) = without_next(serde_json::from_str(&out).unwrap());
    let old: Value = serde_json::from_str(&base("cli_map_json_compact.json")).unwrap();
    assert_eq!(v, old, "map --json --compact");
    assert_eq!(next.as_deref(), Some("code-graph-mcp map --json"));

    let server = common::init_server(&p);
    let cases: &[(&str, &str, Value, Option<&str>)] = &[
        (
            "mcp_project_map.json",
            "project_map",
            json!({}),
            Some("code-graph-mcp map --json"),
        ),
        (
            "mcp_project_map_compact.json",
            "project_map",
            json!({"compact": true}),
            // The compact hot-function cut alone names `map`; the threshold
            // tier also cut modules and dependencies here, and `map`'s text
            // stops at 30 dependencies, so the whole answer is named.
            Some("code-graph-mcp map --json"),
        ),
        (
            "mcp_overview_core.json",
            "module_overview",
            json!({"path": "core"}),
            Some("code-graph-mcp overview core"),
        ),
        (
            "mcp_overview_core_compact.json",
            "module_overview",
            json!({"path": "core", "compact": true}),
            Some("code-graph-mcp overview core"),
        ),
        (
            "mcp_callgraph_hub_rollup.json",
            "get_call_graph",
            json!({"symbol_name": "hub", "depth": 1}),
            Some("code-graph-mcp callgraph hub --depth 1"),
        ),
        (
            // Negative control: 20 active exports, nothing cut, no `next`.
            "mcp_overview_util.json",
            "module_overview",
            json!({"path": "util"}),
            None,
        ),
        (
            "mcp_callgraph_f_1_1.json",
            "get_call_graph",
            json!({"symbol_name": "f_1_1"}),
            None,
        ),
        (
            "mcp_ast_node_hub_compressed.json",
            "get_ast_node",
            json!({"symbol_name": "hub", "file_path": "dense.js", "include_references": true}),
            Some("code-graph-mcp show hub --file dense.js --refs"),
        ),
        (
            "mcp_ast_node_hub_truncated.json",
            "get_ast_node",
            json!({"symbol_name": "hub", "include_references": true}),
            Some("code-graph-mcp show hub --file dense.js --refs"),
        ),
    ];
    for (file, tool, args, want) in cases {
        let (v, _) = mcp(&server, tool, args.clone());
        let (v, next) = without_next(v);
        let old: Value = serde_json::from_str(&base(file)).unwrap();
        assert_eq!(v, old, "{tool} {args}: differs from the pre-change answer");
        assert_eq!(next.as_deref(), *want, "{tool} {args}: next");
    }
}

// ---- every default cut's next returns what was cut -----------------------------

fn lines_with<'a>(text: &'a str, needle: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.contains(needle)).collect()
}

#[test]
fn default_cut_next_commands_return_what_was_cut() {
    let p = fixture();
    let dir = p.path();
    // The truth: every module, dependency and hot function.
    let all: Value = serde_json::from_str(&cli(dir, &["map", "--json"])).unwrap();
    let modules: Vec<String> = all["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap().to_string())
        .collect();
    let deps: Vec<(String, String)> = all["module_dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["from"].as_str().unwrap().into(),
                d["to"].as_str().unwrap().into(),
            )
        })
        .collect();
    let hot: Vec<String> = all["hot_functions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        (modules.len(), deps.len(), hot.len()),
        (43, 80, 15),
        "fixture shape"
    );

    // map: the dependency cut (30 shown) → map --json has all 80.
    let (_, next) = strip_next_lines(&cli(dir, &["map"]));
    let got: Value = serde_json::from_str(&run_next(dir, &next[0])).unwrap();
    assert_eq!(got["module_dependencies"].as_array().unwrap().len(), 80);

    // map --compact: modules (15 shown) → map lists all 43; hot (5 shown) → all 15.
    let compact = cli(dir, &["map", "--compact"]);
    let (_, next) = strip_next_lines(&compact);
    let full = run_next(dir, &next[0]);
    for m in &modules {
        let header = format!("{m} (");
        assert!(
            full.lines().any(|l| l.starts_with(&header)),
            "module {m} missing from `{}`",
            next[0]
        );
    }
    let full_hot = run_next(dir, &next[2]);
    for h in &hot {
        assert!(
            lines_with(&full_hot, &format!("  {h} (")).len() == 1,
            "hot function {h} missing from `{}`",
            next[2]
        );
    }

    let server = common::init_server(&p);
    // project_map (threshold tier cut modules and deps to 15) → map --json.
    let (v, _) = mcp(&server, "project_map", json!({}));
    assert_eq!(v["_array_truncations"]["modules"]["original"], 43, "{v}");
    let got: Value = serde_json::from_str(&run_next(dir, v["next"].as_str().unwrap())).unwrap();
    assert_eq!(got["modules"].as_array().unwrap().len(), 43);
    assert_eq!(got["module_dependencies"].as_array().unwrap().len(), 80);

    // project_map compact (hot 10 of 15; modules and deps cut to 15 by the
    // threshold tier) → map --json has all of them.
    let (v, _) = mcp(&server, "project_map", json!({"compact": true}));
    assert_eq!(v["hot_functions_total"], 15, "{v}");
    assert_eq!(
        v["_array_truncations"]["module_dependencies"]["original"], 80,
        "{v}"
    );
    let got: Value = serde_json::from_str(&run_next(dir, v["next"].as_str().unwrap())).unwrap();
    let got_hot: Vec<&str> = got["hot_functions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    for h in &hot {
        assert!(got_hot.contains(&h.as_str()), "{h}: {got_hot:?}");
    }
    assert_eq!(got["modules"].as_array().unwrap().len(), 43);
    assert_eq!(got["module_dependencies"].as_array().unwrap().len(), 80);

    // The handler's own `next` stands when the threshold tier cut nothing: a
    // repo small enough that compact project_map stays under it.
    let small = TempDir::new().unwrap();
    let mut src = String::new();
    for n in 0..12 {
        src.push_str(&format!("def g{n}():\n    return {n}\n\n\n"));
    }
    src.push_str("def use():\n");
    for n in 0..12 {
        src.push_str(&format!("    g{n}()\n"));
    }
    std::fs::write(small.path().join("m.py"), src).unwrap();
    index(small.path());
    let small_server = common::init_server(&small);
    let (v, _) = mcp(&small_server, "project_map", json!({"compact": true}));
    assert!(
        v.get("_truncated").is_none(),
        "precondition: under the tier: {v}"
    );
    assert_eq!(v["hot_functions_truncated"], true, "precondition: {v}");
    assert_eq!(v["next"], "code-graph-mcp map", "{v}");
    let text = run_next(small.path(), v["next"].as_str().unwrap());
    for n in 0..12 {
        assert_eq!(
            lines_with(&text, &format!("  g{n} (")).len(),
            1,
            "g{n}: {text}"
        );
    }

    // module_overview core (30 of 35 active, 8 of 12 / 10 inactive names).
    let (v, _) = mcp(&server, "module_overview", json!({"path": "core"}));
    assert_eq!(v["total_active"], 35, "{v}");
    let text = run_next(dir, v["next"].as_str().unwrap());
    for n in 0..35 {
        assert!(
            text.contains(&format!("api_{n:02} (1×)")),
            "api_{n:02}: {text}"
        );
    }
    for n in 0..12 {
        assert!(
            text.contains(&format!("unused_{n:02}")),
            "unused_{n:02}: {text}"
        );
    }
    for n in 0..10 {
        assert!(
            text.contains(&format!("Unused{n:02}")),
            "Unused{n:02}: {text}"
        );
    }

    // get_call_graph rollup (10 sample names of 150) → the CLI tree lists all.
    let (v, _) = mcp(
        &server,
        "get_call_graph",
        json!({"symbol_name": "hub", "depth": 1}),
    );
    assert_eq!(v["mode"], "rollup_call_graph", "{v}");
    let text = run_next(dir, v["next"].as_str().unwrap());
    for n in 0..150 {
        assert!(
            text.contains(&format!("→ calls: callee_{n} (dense.js)")),
            "callee_{n}: {text}"
        );
    }

    // get_ast_node compressed_node (source dropped) and the threshold tier
    // (`calls` cut to 15) → show prints the body and all 150 calls.
    for args in [
        json!({"symbol_name": "hub", "file_path": "dense.js", "include_references": true}),
        json!({"symbol_name": "hub", "include_references": true}),
    ] {
        let (v, _) = mcp(&server, "get_ast_node", args.clone());
        assert!(
            v["mode"] == "compressed_node" || v["_truncated"] == true,
            "precondition {args}: {v}"
        );
        let text = run_next(dir, v["next"].as_str().unwrap());
        assert!(text.contains("  function hub() {"), "{text}");
        assert!(text.contains("    callee_149();"), "{text}");
        for n in 0..150 {
            assert!(
                text.contains(&format!("    → callee_{n} (dense.js)")),
                "callee_{n}"
            );
        }
    }
}

// ---- budgeted answers ----------------------------------------------------------

/// `(tokens, floor, ceiling)` in bytes: ±15% of tokens × 3, written out as
/// numbers so that a change to `CHARS_PER_TOKEN` or to the budget arithmetic
/// fails here instead of moving the bounds with it.
const BOUNDS: &[(u64, usize, usize)] =
    &[(500, 1275, 1725), (1000, 2550, 3450), (4000, 10200, 13800)];

fn bounds(tokens: u64) -> (usize, usize) {
    let (_, lo, hi) = BOUNDS.iter().find(|(t, _, _)| *t == tokens).unwrap();
    (*lo, *hi)
}

/// A budgeted answer is at most the ceiling; one that left something out is
/// at least the floor (one that left nothing out is the whole answer, which
/// can be any size under the budget).
fn assert_size(label: &str, bytes: usize, cut: bool, tokens: u64) {
    let (lo, hi) = bounds(tokens);
    assert!(bytes <= hi, "{label} @{tokens}: {bytes} B > ceiling {hi}");
    if cut {
        assert!(
            bytes >= lo,
            "{label} @{tokens}: {bytes} B < floor {lo} (and something was cut)"
        );
    }
}

const CLI_BUDGET_CASES: &[&[&str]] = &[
    &["map"],
    &["overview", "."],
    &["overview", "core/api.py"],
    &["callgraph", "h_0"],
    &["show", "run", "--refs", "--impact"],
    &["show", "hub", "--refs"],
];

fn with_budget(args: &[&str], tokens: u64) -> Vec<String> {
    let mut v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    v.push("--budget".into());
    v.push(tokens.to_string());
    v
}

fn cli_budget(dir: &Path, args: &[&str], tokens: u64) -> String {
    let a = with_budget(args, tokens);
    let refs: Vec<&str> = a.iter().map(String::as_str).collect();
    cli(dir, &refs)
}

#[test]
fn cli_budget_lands_within_fifteen_percent() {
    let p = fixture();
    for tokens in [500u64, 1000] {
        for args in CLI_BUDGET_CASES {
            let out = cli_budget(p.path(), args, tokens);
            let cut = out.contains("… budget ");
            // Non-vacuous: every one of these answers is larger than 1000 tokens
            // unbudgeted, so each must have been cut.
            assert!(
                cut,
                "{args:?} @{tokens}: expected a cut, got {} B",
                out.len()
            );
            assert_size(&format!("{args:?}"), out.len(), cut, tokens);
        }
    }
}

#[test]
fn cli_budget_next_returns_every_line_it_left_out() {
    let p = fixture();
    let dir = p.path();
    for args in CLI_BUDGET_CASES {
        let out = cli_budget(dir, args, 500);
        let (_, next) = strip_next_lines(&out);
        assert!(!next.is_empty(), "{args:?}: no next line in {out}");
        let full = cli(dir, args);
        let shown: std::collections::HashSet<&str> = out.lines().collect();
        for cmd in &next {
            let got = run_next(dir, cmd);
            if cmd.ends_with("--json") {
                // `map` dropped a dependency past the text answer's 30.
                let v: Value = serde_json::from_str(&got).unwrap();
                assert_eq!(v["module_dependencies"].as_array().unwrap().len(), 80);
                continue;
            }
            let got_lines: std::collections::HashSet<&str> = got.lines().collect();
            for line in full.lines() {
                if !shown.contains(line) && !line.trim().is_empty() && !line.contains("... and ") {
                    assert!(
                        got_lines.contains(line),
                        "{args:?}: `{cmd}` does not return the left-out line {line:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn cli_budget_shortens_before_it_drops_and_ranks_by_callers() {
    let p = fixture();
    let dir = p.path();
    // map: `util` is imported by every package, `<root>` (dense.js) by nothing.
    let out = cli_budget(dir, &["map"], 500);
    assert!(out.contains("\nutil (1 file"), "{out}");
    assert!(!out.contains("<root> ("), "{out}");

    // overview of one file: signatures go before any symbol does.
    let out = cli_budget(dir, &["overview", "core/api.py"], 500);
    assert!(
        out.contains("symbols omitted") && out.contains("without signature"),
        "{out}"
    );
    // Every symbol line is a whole line of the unbudgeted outline, or that
    // line without its trailing signature.
    let full = cli(dir, &["overview", "core/api.py"]);
    for l in out.lines().skip(1) {
        if l.starts_with("  … budget") || l.starts_with("  next:") {
            continue;
        }
        let whole = full.lines().any(|f| f == l);
        let skeleton = full
            .lines()
            .any(|f| f.len() > l.len() && f.starts_with(l) && f[l.len()..].starts_with("  ("));
        assert!(whole || skeleton, "not a whole outline line: {l:?}");
    }

    // callgraph: deeper callers go before shallower ones. h_0 has 40 depth-1
    // callers, 40 at depth 2 and 40 at depth 3.
    let depth = |out: &str, d: usize| {
        let indent = "  ".repeat(d);
        out.lines()
            .filter(|l| l.starts_with(&format!("{indent}← ")))
            .count()
    };
    let out = cli_budget(dir, &["callgraph", "h_0"], 250);
    assert!(
        depth(&out, 1) < 40,
        "precondition: a depth-1 caller left out: {out}"
    );
    assert_eq!(
        depth(&out, 2) + depth(&out, 3),
        0,
        "deeper shown while depth 1 cut: {out}"
    );
    let out = cli_budget(dir, &["callgraph", "h_0"], 500);
    assert_eq!(
        depth(&out, 1),
        40,
        "precondition: every depth-1 caller fits: {out}"
    );
    assert!(
        depth(&out, 2) < 40,
        "precondition: a depth-2 caller left out: {out}"
    );
    assert_eq!(depth(&out, 3), 0, "depth 3 shown while depth 2 cut: {out}");

    // show: a definition loses its body before any definition is left out.
    let out = cli_budget(dir, &["show", "run", "--refs", "--impact"], 500);
    assert!(
        out.contains("definitions omitted") && out.contains("without their body"),
        "{out}"
    );
}

#[test]
fn a_body_is_whole_or_absent() {
    let p = fixture();
    for tokens in [100u64, 500, 1000, 2000, 3000] {
        let out = cli_budget(p.path(), &["show", "hub"], tokens);
        let whole =
            out.contains("  function hub() {\n") && out.contains("    callee_149();\n  }\n");
        let absent = !out.contains("callee_0();");
        assert!(whole ^ absent, "@{tokens}: body cut part-way:\n{out}");
    }
}

/// `core` holds one big file (57 symbols) and one tiny one: the big one's
/// block is held to 70% of the budget even though the whole answer would fit
/// if it took more.
#[test]
fn one_file_takes_at_most_seventy_percent() {
    let p = fixture();
    for (tokens, share) in [(200u64, 420usize), (250, 525), (300, 630)] {
        let out = cli_budget(p.path(), &["overview", "core"], tokens);
        let start = out
            .find("core/api.py\n")
            .unwrap_or_else(|| panic!("@{tokens}: {out}"));
        let end = out
            .find("core/use.py")
            .unwrap_or_else(|| panic!("@{tokens}: {out}"));
        let block = &out[start..end];
        assert!(
            block.len() <= share,
            "@{tokens}: api.py block {} B > {share}",
            block.len()
        );
        assert!(
            block.contains("lower-ranked symbols not shown"),
            "@{tokens}: {block}"
        );
        assert!(
            out.contains("core/use.py\n  function: use_all\n"),
            "@{tokens}: {out}"
        );
    }
}

#[test]
fn budget_refuses_json_and_compact() {
    let p = fixture();
    for extra in ["--json", "--compact"] {
        let (_, code) = cli_in(p.path(), &["map", "--budget", "500", extra]);
        assert_eq!(code, 2, "--budget with {extra} must be refused");
    }
}

// ---- MCP max_tokens -------------------------------------------------------------

fn mcp_budget_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("project_map", json!({})),
        ("module_overview", json!({"path": "."})),
        ("module_overview", json!({"path": "core/api.py"})),
        ("get_call_graph", json!({"symbol_name": "h_0"})),
        ("get_call_graph", json!({"symbol_name": "hub"})),
        (
            "get_ast_node",
            json!({"symbol_name": "hub", "include_references": true}),
        ),
    ]
}

#[test]
fn mcp_max_tokens_lands_within_fifteen_percent() {
    let p = fixture();
    let server = common::init_server(&p);
    for tokens in [500u64, 1000] {
        for (tool, args) in mcp_budget_cases() {
            let mut a = args.clone();
            a["max_tokens"] = json!(tokens);
            let (v, bytes) = mcp(&server, tool, a);
            let cut = v.get("budget").is_some();
            assert!(
                cut,
                "{tool} {args} @{tokens}: expected a cut, got {bytes} B"
            );
            assert_size(&format!("{tool} {args}"), bytes, cut, tokens);
            assert!(
                v.get("_truncated").is_none(),
                "threshold tier ran on a budgeted answer: {v}"
            );
        }
    }
}

#[test]
fn mcp_budget_next_returns_what_it_left_out() {
    let p = fixture();
    let dir = p.path();
    let server = common::init_server(&p);
    let next_of = |tool: &str, mut args: Value| -> (Value, String) {
        args["max_tokens"] = json!(500);
        let (v, _) = mcp(&server, tool, args);
        let next = v["budget"]["next"]
            .as_str()
            .unwrap_or_else(|| panic!("{v}"))
            .to_string();
        (v, run_next(dir, &next))
    };

    let (v, text) = next_of("project_map", json!({}));
    let shown: Vec<&str> = v["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    let all: Value = serde_json::from_str(&cli(dir, &["map", "--json"])).unwrap();
    for m in all["modules"].as_array().unwrap() {
        let path = m["path"].as_str().unwrap();
        if !shown.contains(&path) {
            assert!(
                text.contains(&format!("\"path\":\"{path}\""))
                    || text.contains(&format!("{path} (")),
                "{path}"
            );
        }
    }

    let (_, text) = next_of("module_overview", json!({"path": "."}));
    for n in 0..35 {
        assert!(text.contains(&format!("api_{n:02} (1×)")), "api_{n:02}");
    }
    for n in 0..150 {
        assert!(text.contains(&format!("callee_{n} (1×)")), "callee_{n}");
    }

    let (_, text) = next_of("module_overview", json!({"path": "core/api.py"}));
    for n in 0..35 {
        assert!(
            text.contains(&format!(
                "api_{n:02} (1×)  (request, payload, options=None)"
            )),
            "api_{n:02} with its signature"
        );
    }

    for sym in ["h_0", "hub"] {
        let (_, text) = next_of("get_call_graph", json!({"symbol_name": sym}));
        let truth: Value = serde_json::from_str(&cli(dir, &["callgraph", sym, "--json"])).unwrap();
        for n in truth["results"].as_array().unwrap() {
            let line = format!(
                "{} ({})",
                n["name"].as_str().unwrap(),
                n["file_path"].as_str().unwrap()
            );
            assert!(text.contains(&line), "{sym}: {line} missing from next");
        }
    }

    let (v, text) = next_of(
        "get_ast_node",
        json!({"symbol_name": "hub", "include_references": true}),
    );
    assert_eq!(v["budget"]["code_omitted"], true, "{v}");
    assert!(text.contains("    callee_149();"), "{text}");
    for n in 0..150 {
        assert!(
            text.contains(&format!("→ callee_{n} (dense.js)")),
            "callee_{n}"
        );
    }
}

/// Every section of a budgeted answer is inside the budget, the ones a flag
/// folds in too: `module_overview`'s `hot_paths`, `dependencies` and
/// `dead_code`, `project_map`'s `centrality` (review F-M3: `include_deps` at
/// 100 tokens came back at 20.9× the budget, undisclosed). What they lose is
/// counted, and the next step names the command that returns it.
#[test]
fn mcp_budget_covers_every_section() {
    let p = fixture();
    let dir = p.path();
    let server = common::init_server(&p);
    let cases = [
        (
            "module_overview",
            json!({"path": "util/helpers.py", "include_deps": true, "include_dead": true}),
        ),
        (
            "module_overview",
            json!({"path": "core/api.py", "include_deps": true, "include_dead": true}),
        ),
        (
            "project_map",
            json!({"include_centrality": true, "centrality_limit": 100}),
        ),
    ];
    for (tool, args) in &cases {
        for tokens in [500u64, 1000] {
            let mut a = args.clone();
            a["max_tokens"] = json!(tokens);
            let (v, bytes) = mcp(&server, tool, a);
            let cut = v.get("budget").is_some();
            assert!(
                cut,
                "{tool} {args} @{tokens}: expected a cut, got {bytes} B"
            );
            assert_size(&format!("{tool} {args}"), bytes, cut, tokens);
        }
        // At the smallest budget the fixed part may not fit; then the answer
        // says so rather than passing as sized.
        let mut a = args.clone();
        a["max_tokens"] = json!(100);
        let (v, bytes) = mcp(&server, tool, a);
        assert!(
            bytes <= 345 || v["budget"]["over_budget"] == true,
            "{tool} {args} @100: {bytes} B, no over_budget: {v}"
        );
    }

    // `hot_paths` is a unit too, and the last to go: it repeats the most
    // called exports, so every export is cut before a hot path is.
    for path in ["util/helpers.py", "core/api.py"] {
        let (v, bytes) = mcp(
            &server,
            "module_overview",
            json!({"path": path, "max_tokens": 200}),
        );
        assert!(bytes <= 690, "{path} @200: {bytes} B: {v}");
        assert!(v["budget"].get("over_budget").is_none(), "{path} @200: {v}");
        assert!(
            v["budget"]["omitted"]["hot_paths"].as_u64().unwrap_or(0) > 0,
            "{path} @200: {v}"
        );
    }
    let (v, _) = mcp(
        &server,
        "module_overview",
        json!({"path": "util/helpers.py", "max_tokens": 400}),
    );
    assert!(
        v["budget"]["omitted"]["active_exports"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "precondition: exports cut @400: {v}"
    );
    assert_eq!(v["hot_paths"].as_array().unwrap().len(), 5, "{v}");

    // What the folded sections lost comes back from the named commands.
    let (v, _) = mcp(
        &server,
        "module_overview",
        json!({"path": "util/helpers.py", "include_deps": true, "max_tokens": 500}),
    );
    let omitted = v["budget"]["omitted"]["dependencies"].as_u64().unwrap_or(0);
    assert!(omitted > 0, "precondition: dependencies cut: {v}");
    let next = v["budget"]["next"].as_str().unwrap();
    let deps_cmd = next
        .split("; ")
        .find(|c| c.starts_with("code-graph-mcp deps "))
        .unwrap_or_else(|| panic!("no deps command in {next}"));
    let text = run_next(dir, deps_cmd);
    for i in 0..40 {
        assert!(
            text.contains(&format!("pkg_{i:02}/mod.py")),
            "pkg_{i:02}/mod.py missing from `{deps_cmd}`:\n{text}"
        );
    }

    let (v, _) = mcp(
        &server,
        "module_overview",
        json!({"path": ".", "include_dead": true, "max_tokens": 500}),
    );
    assert!(
        v["budget"]["omitted"]["dead_code"].as_u64().unwrap_or(0) > 0,
        "precondition: dead code cut: {v}"
    );
    let next = v["budget"]["next"].as_str().unwrap();
    let dead_cmd = next
        .split("; ")
        .find(|c| c.starts_with("code-graph-mcp dead-code "))
        .unwrap_or_else(|| panic!("no dead-code command in {next}"));
    let text = run_next(dir, dead_cmd);
    for i in 0..40 {
        let line = format!("class C_{i} pkg_{i:02}/mod.py");
        assert!(text.contains(&line), "{line}:\n{text}");
    }

    let (v, _) = mcp(
        &server,
        "project_map",
        json!({"include_centrality": true, "centrality_limit": 100, "max_tokens": 500}),
    );
    assert!(
        v["budget"]["omitted"]["centrality"].as_u64().unwrap_or(0) > 0,
        "precondition: centrality cut: {v}"
    );
    let next = v["budget"]["next"].as_str().unwrap();
    assert!(
        next.split("; ")
            .any(|c| c == "code-graph-mcp centrality --limit 100"),
        "{next}"
    );
}

/// A next step over a path that starts with `-` runs (review F-L1: `show foo3
/// --file --json.js` exited 2): the path goes out as `./-x/b.js`.
#[test]
fn a_next_step_over_a_dash_path_runs() {
    let p = TempDir::new().unwrap();
    let mut body = String::from("export function baz() {\n");
    for n in 0..80 {
        body.push_str(&format!("  const v{n} = {n};\n"));
    }
    body.push_str("  return 0;\n}\n");
    std::fs::create_dir_all(p.path().join("-x")).unwrap();
    std::fs::write(p.path().join("-x/b.js"), &body).unwrap();
    index(p.path());
    let server = common::init_server(&p);
    let (v, _) = mcp(
        &server,
        "get_ast_node",
        json!({"symbol_name": "baz", "max_tokens": 100}),
    );
    let next = v["budget"]["next"]
        .as_str()
        .unwrap_or_else(|| panic!("precondition: cut: {v}"));
    assert!(next.contains(" --file ./-x/b.js"), "{next}");
    let text = run_next(p.path(), next);
    assert!(text.contains("const v79 = 79;"), "{text}");
    let (v, _) = mcp(
        &server,
        "module_overview",
        json!({"path": "-x/b.js", "include_deps": true, "include_dead": true, "max_tokens": 100}),
    );
    for cmd in v["budget"]["next"].as_str().unwrap().split("; ") {
        assert!(cmd.contains(" ./-x/b.js"), "{cmd}");
        run_next(p.path(), cmd);
    }
}

/// The CLI call graph's next step keeps the flags that shaped the answer.
#[test]
fn cli_callgraph_budget_next_keeps_include_tests() {
    let p = fixture();
    let out = cli(
        p.path(),
        &["callgraph", "h_0", "--include-tests", "--budget", "500"],
    );
    let (_, next) = strip_next_lines(&out);
    assert_eq!(
        next,
        vec!["code-graph-mcp callgraph h_0 --include-tests".to_string()],
        "{out}"
    );
}

#[test]
fn mcp_compact_is_reported_inert_beside_max_tokens() {
    let p = fixture();
    let server = common::init_server(&p);
    let (v, _) = mcp(
        &server,
        "module_overview",
        json!({"path": "core", "compact": true, "max_tokens": 500}),
    );
    assert_eq!(v["ignored_arguments"], json!(["compact"]), "{v}");
    assert!(
        v.get("active_exports").is_some(),
        "the budget starts from the full envelope: {v}"
    );
    // Without max_tokens compact is honoured and not reported.
    let (v, _) = mcp(
        &server,
        "module_overview",
        json!({"path": "core", "compact": true}),
    );
    assert!(v.get("ignored_arguments").is_none(), "{v}");
}

// ---- this repository --------------------------------------------------------------

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// The spec's acceptance numbers: 500 / 1000 / 4000 tokens on this repo's own
/// source (`src/`, copied so the index does not touch the checkout), every
/// budgeted surface.
#[test]
fn budget_on_this_repo_lands_within_fifteen_percent() {
    let p = TempDir::new().unwrap();
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &p.path().join("src"),
    );
    index(p.path());
    let dir = p.path();
    // Targets picked from the index, not named, so the test survives renames:
    // the most-called function (and its file) and the file with most symbols.
    let map: Value = serde_json::from_str(&cli(dir, &["map", "--json"])).unwrap();
    let hot = &map["hot_functions"][0];
    let (hot_name, hot_file) = (hot["name"].as_str().unwrap(), hot["file"].as_str().unwrap());
    let exports: Value = serde_json::from_str(&cli(dir, &["overview", "src", "--json"])).unwrap();
    let mut per_file: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for e in exports.as_array().unwrap() {
        *per_file.entry(e["file"].as_str().unwrap()).or_default() += 1;
    }
    let big_file = per_file
        .iter()
        .max_by_key(|(_, n)| **n)
        .unwrap()
        .0
        .to_string();

    let cli_cases: Vec<Vec<&str>> = vec![
        vec!["map"],
        vec!["overview", "src"],
        vec!["overview", &big_file],
        vec!["callgraph", hot_name, "--file", hot_file],
        vec!["show", "new", "--refs"],
    ];
    let server = common::init_server(&p);
    let mcp_cases: Vec<(&str, Value)> = vec![
        ("project_map", json!({})),
        ("module_overview", json!({"path": "src"})),
        (
            "get_call_graph",
            json!({"symbol_name": hot_name, "file_path": hot_file}),
        ),
        (
            "get_ast_node",
            json!({"symbol_name": hot_name, "file_path": hot_file, "include_references": true}),
        ),
        // Non-default parameters: the sections they fold in are budgeted too
        // (review F-M3 measured this file at 2.09× the budget at 1000 tokens).
        (
            "module_overview",
            json!({"path": "src/mcp/server/mod.rs", "include_deps": true, "include_dead": true}),
        ),
        ("project_map", json!({"include_centrality": true})),
    ];
    let mut cut_at = std::collections::BTreeMap::<u64, usize>::new();
    for tokens in [500u64, 1000, 4000] {
        for args in &cli_cases {
            let out = cli_budget(dir, args, tokens);
            let cut = out.contains("… budget ");
            *cut_at.entry(tokens).or_default() += cut as usize;
            eprintln!("repo {args:?} @{tokens}: {} B cut={cut}", out.len());
            assert_size(&format!("{args:?}"), out.len(), cut, tokens);
        }
        for (tool, args) in &mcp_cases {
            let mut a = args.clone();
            a["max_tokens"] = json!(tokens);
            let (v, bytes) = mcp(&server, tool, a);
            let cut = v.get("budget").is_some();
            *cut_at.entry(tokens).or_default() += cut as usize;
            eprintln!("repo {tool} {args} @{tokens}: {bytes} B cut={cut}");
            assert_size(&format!("{tool} {args}"), bytes, cut, tokens);
        }
    }
    // Non-vacuous: every surface is cut at 500 and 1000 tokens. At 4000, `src/`
    // alone is small enough that five answers are whole (2026-09-28: map 6.4 KB,
    // the largest file's outline 6.2 KB, `show new --refs` 8.6 KB,
    // project_map 9.1 KB, get_ast_node 11.2 KB), so four are cut.
    assert_eq!(cut_at[&500], 11, "{cut_at:?}");
    assert_eq!(cut_at[&1000], 11, "{cut_at:?}");
    assert!(cut_at[&4000] >= 4, "{cut_at:?}");
}

/// MCP twin of the CLI rank and share checks: `project_map` keeps the module
/// every package imports and drops the one nothing imports; `module_overview`
/// holds one file's exports to 70% of the budget.
#[test]
fn mcp_budget_ranks_by_callers_and_holds_the_file_share() {
    let p = fixture();
    let server = common::init_server(&p);
    let (v, _) = mcp(&server, "project_map", json!({"max_tokens": 500}));
    let paths: Vec<&str> = v["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"util") && !paths.contains(&"<root>"),
        "{paths:?}"
    );

    // get_call_graph: deeper callers go before shallower ones (h_0: 40 callers
    // at each of depths 1-3).
    let depths = |tokens: u64| -> std::collections::BTreeMap<i64, usize> {
        let (v, _) = mcp(
            &server,
            "get_call_graph",
            json!({"symbol_name": "h_0", "max_tokens": tokens}),
        );
        let mut m = std::collections::BTreeMap::new();
        for n in v["callers"].as_array().unwrap() {
            *m.entry(n["depth"].as_i64().unwrap()).or_default() += 1;
        }
        m
    };
    let d = depths(1000);
    assert!(
        d.get(&1).copied().unwrap_or(0) < 40,
        "precondition: depth 1 cut: {d:?}"
    );
    assert_eq!(
        d.keys().copied().collect::<Vec<_>>(),
        vec![1],
        "deeper shown while depth 1 cut: {d:?}"
    );
    let d = depths(2000);
    assert_eq!(d.get(&1), Some(&40), "precondition: depth 1 whole: {d:?}");
    assert!(
        d.get(&2).copied().unwrap_or(0) < 40,
        "precondition: depth 2 cut: {d:?}"
    );
    assert!(
        !d.contains_key(&3),
        "depth 3 shown while depth 2 cut: {d:?}"
    );

    for (tokens, share) in [(700u64, 1470usize), (1000, 2100), (1500, 3150)] {
        let (v, _) = mcp(
            &server,
            "module_overview",
            json!({"path": "core", "max_tokens": tokens}),
        );
        let api: usize = v["active_exports"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["file"] == "core/api.py")
            .map(|e| serde_json::to_string(e).unwrap().len() + 1)
            .sum();
        assert!(
            api > 0 && api <= share,
            "@{tokens}: api.py exports {api} B, share {share}"
        );
        assert!(
            v["budget"]["cut_for_file_share"].as_u64().unwrap_or(0) > 0,
            "@{tokens}: {v}"
        );
    }
}
