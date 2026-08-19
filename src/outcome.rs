use std::path::{Path, PathBuf};

/// cg PULL tools whose results are relevance-ordered → rank is meaningful.
///
/// MCP spellings ONLY. Each tool's CLI twin is derived in [`is_ranked_tool`]
/// rather than listed here, because listing both spellings by hand is what broke
/// this metric: the CLI event name is `<canonical>_cli`, and the canonical form
/// of `ast_search` is the HYPHENATED `ast-search`, so the hand-written entry
/// `ast_search` covered the MCP call and silently missed every
/// `ast-search_cli` one — dropping them from the field-MRR denominator and
/// throwing away their rank. A shrunken denominator still renders as a confident
/// number, so nothing looked wrong (2026-08-02 audit §16).
pub const RANKED_MCP_TOOLS: &[&str] = &["semantic_code_search", "ast_search"];

/// Is this event name a ranked tool — in either surface's spelling?
///
/// CLI events are named `<canonical>_cli`, so a CLI name is ranked exactly when
/// it canonicalizes to the same subcommand as one of [`RANKED_MCP_TOOLS`]. Both
/// sides go through `canonical_query_cmd`, the one table that already maps MCP
/// tool names and CLI subcommands onto a single canonical name, so the two
/// spellings cannot drift apart again.
pub fn is_ranked_tool(base: &str) -> bool {
    use crate::utils::telemetry::canonical_query_cmd;
    if RANKED_MCP_TOOLS.contains(&base) {
        return true;
    }
    let Some(canon) = base.strip_suffix("_cli").and_then(canonical_query_cmd) else {
        return false;
    };
    RANKED_MCP_TOOLS
        .iter()
        .any(|mcp| canonical_query_cmd(mcp) == Some(canon))
}

/// If `name` is a code-graph MCP tool_use name (`mcp__code-graph[-dev]__<tool>`),
/// return the bare `<tool>` when it is one of the known cg query tools. The server
/// namespace varies (`code-graph` marketplace vs `code-graph-dev` dogfood), so match
/// on the trailing `__<tool>` segment, not the full prefix.
pub fn cg_pull_tool(name: &str) -> Option<String> {
    let base = name.rsplit("__").next().unwrap_or(name);
    if name.starts_with("mcp__")
        && name.contains("code-graph")
        && crate::domain::LIVE_MCP_TOOLS.contains(&base)
    {
        Some(base.to_string())
    } else {
        None
    }
}

/// Encode an absolute project path to its Claude Code transcript-dir slug:
/// every non-alphanumeric character becomes `-` (so `/`, `_`, `.` all map to `-`;
/// existing hyphens are preserved). Verified against the real `~/.claude/projects/`
/// directory names (`/mnt/data_ssd/...` → `-mnt-data-ssd-...`).
pub fn project_slug(abs_path: &str) -> String {
    abs_path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn transcript_dir(target: &Path, home: &Path) -> PathBuf {
    transcript_dir_on(target, home, cfg!(windows))
}

/// Testable core of [`transcript_dir`]. `windows` says whether `\` is a path
/// SEPARATOR on the target platform (it is an ordinary filename character on
/// Unix, so this must not be assumed).
///
/// `project_slug` maps every non-alphanumeric byte to `-`, which makes it
/// immune to the separator itself — `D:\dev\r`, `D:/dev/r` and the mixed
/// `D:\dev/r` all slugify identically. The extended-length prefix is a
/// different matter: `\\?\D:\dev\r` slugifies to `----D--dev-r` rather than
/// `D--dev-r`, so a user who pastes back what `canonicalize()` printed gets a
/// transcript directory Claude Code never created, and `outcome` reports
/// "absent" with nothing actually wrong — the same silent-zero failure mode as
/// D7. Stripping the prefix first is a no-op on Unix, where it cannot occur.
///
/// Deliberately NOT case-folded: Windows filesystems are case-insensitive but
/// case-PRESERVING, and this slug has to match a directory name Claude Code
/// chose from its own spelling of the path. Lower-casing here would trade a
/// rare mismatch for a systematic one.
pub fn transcript_dir_on(target: &Path, home: &Path, windows: bool) -> PathBuf {
    let spelled =
        crate::indexer::merkle::normalize_path_display_on(&target.to_string_lossy(), windows);
    // A trailing separator is invisible to the user and fatal to the lookup:
    // `--project /repo/` slugifies to `-repo-`, a directory Claude Code never
    // created, and the command then answers `state: absent` with exit 0 — the
    // silent-zero shape again, a typo's worth of difference between "no data" and
    // "you asked wrong". Shell tab-completion supplies that slash for free.
    // The root itself is not a trailing separator to strip; it IS the path.
    let trimmed = spelled.trim_end_matches(['/', '\\']);
    let key = if trimmed.is_empty() {
        spelled.as_str()
    } else {
        trimmed
    };
    home.join(".claude")
        .join("projects")
        .join(project_slug(key))
}

/// True if `touched` (often absolute, from Read/Edit) ends with `returned` (often
/// repo-relative, from a cg result), compared by trailing path components. The
/// returned path carries directory context so basename collisions are unlikely.
///
/// Both separators split. These strings come out of a *recorded transcript* whose
/// producing platform is unknown to this process, so a Windows client's
/// `D:\repo\src\Foo.cs` must not collapse into one opaque component — that made
/// every comparison fail and pinned the Windows adoption half of the conversion
/// metric at a permanent zero. Same reasoning as the `.exe` token fix on the
/// call-recognition half: parse what was recorded, not what this host spells.
/// Unix filenames may legally contain `\`, but a path that reaches here already
/// went through a tool that treats it as a path, so over-splitting one exotic
/// name is strictly cheaper than a dark platform.
pub fn paths_match(returned: &str, touched: &str) -> bool {
    let split = |s: &str| {
        s.trim_start_matches(['/', '\\'])
            .split(['/', '\\'])
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
    };
    let r = split(returned);
    let t = split(touched);
    if r.is_empty() || t.is_empty() {
        return false;
    }
    let (long, short) = if t.len() >= r.len() {
        (&t, &r)
    } else {
        (&r, &t)
    };
    long[long.len() - short.len()..] == short[..]
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnedItem {
    pub file_path: String,
    pub rank: Option<usize>,
}

/// Extract the returned files from a cg tool_result payload. Ranked-list tools
/// (`ranked == true`) return EITHER a bare top-level array OR an object wrapping a
/// `results` array (ast_search always `{results,count}`; semantic_code_search when
/// FTS-only `{results,search_mode}` or compressed `{mode,results}`). Array index is
/// the rank. Structural tools return a nested object/tree → recursively collect every
/// `file_path`/`file`/`path` string value, rank = None. Robust to per-tool shape.
pub fn extract_returned(payload: &serde_json::Value, ranked: bool) -> Vec<ReturnedItem> {
    if ranked {
        // Ranked tools return EITHER a bare top-level array, OR an object wrapping a
        // `results` array: ast_search always (`{results,count}`); semantic_code_search
        // when FTS-only (`{results,search_mode}`) or compressed (`{mode,results}`).
        // Unwrap `results` so the rank (array index) is not silently dropped to None.
        let arr = payload
            .as_array()
            .or_else(|| payload.get("results").and_then(|r| r.as_array()));
        if let Some(arr) = arr {
            return arr
                .iter()
                .enumerate()
                .filter_map(|(i, el)| {
                    file_path_field(el).map(|fp| ReturnedItem {
                        file_path: fp,
                        rank: Some(i),
                    })
                })
                .collect();
        }
    }
    let mut out = Vec::new();
    collect_file_paths(payload, &mut out);
    out
}

/// First of `file_path` / `file` / `path` that is a non-empty string.
fn file_path_field(v: &serde_json::Value) -> Option<String> {
    for key in ["file_path", "file", "path"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn collect_file_paths(v: &serde_json::Value, out: &mut Vec<ReturnedItem>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(fp) = file_path_field(v) {
                out.push(ReturnedItem {
                    file_path: fp,
                    rank: None,
                });
            }
            for (_, val) in map {
                collect_file_paths(val, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for el in arr {
                collect_file_paths(el, out);
            }
        }
        _ => {}
    }
}

/// Extract file paths from cg CLI human output, where hits appear as `path:line`
/// or `path:line-line` (e.g. `src/foo.rs:63`, `CHANGELOG.md:3708-3709`). Returns the
/// path part of each `<path-like>:<digit>` token, in order WITH duplicates (caller
/// dedupes). A path-like token contains `/` or `.` and ends just before the `:`.
fn scan_path_line_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b':' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                let mut start = i;
                while start > 0 {
                    let c = b[start - 1];
                    if c == b'/' || c == b'.' || c == b'-' || c == b'_' || c.is_ascii_alphanumeric()
                    {
                        start -= 1;
                    } else {
                        break;
                    }
                }
                let token = &line[start..i];
                if !token.is_empty() && (token.contains('/') || token.contains('.')) {
                    out.push(token.to_string());
                }
            }
            i += 1;
        }
    }
    out
}

/// Paths appearing as bare parenthesized tokens — `symbol (src/foo.rs)` — the
/// shape callgraph/impact/overview human output uses (no `:line` suffix). A match
/// must be whitespace-free and contain `/` so prose parentheticals
/// (`(75 test callers hidden…)`) and version strings (`(v0.99.1)`) never match.
fn scan_paren_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(open) = rest.find('(') {
            let after = &rest[open + 1..];
            let Some(close) = after.find(')') else {
                break;
            };
            let token = &after[..close];
            if !token.is_empty() && token.contains('/') && !token.chars().any(|c| c.is_whitespace())
            {
                out.push(token.to_string());
            }
            rest = &after[close + 1..];
        }
    }
    out
}

/// Returned files from a model-initiated cg CLI call's stdout. JSON fast-path
/// (model passed `--json` → same `{results}` shape as MCP); else scan human
/// `path:line` tokens, falling back to `(path)` parenthesized tokens when there are
/// NO path:line hits (callgraph/impact-style output has no line numbers at all —
/// without the fallback those calls always read as returned_files = [] and adoption
/// was structurally impossible). Dedupe to unique paths in first-occurrence order;
/// `ranked` → rank = first-occurrence index, else None.
pub fn extract_returned_from_cli(stdout: &str, ranked: bool) -> Vec<ReturnedItem> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        return extract_returned(&v, ranked);
    }
    let mut paths = scan_path_line_paths(stdout);
    if paths.is_empty() {
        paths = scan_paren_paths(stdout);
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in paths {
        if seen.insert(path.clone()) {
            let rank = if ranked { Some(out.len()) } else { None };
            out.push(ReturnedItem {
                file_path: path,
                rank,
            });
        }
    }
    out
}

#[derive(Debug, Clone)]
pub enum Event {
    /// `turn` is the transcript line index the tool_use block came from — all
    /// tool_use blocks batched in one assistant message share it, so the scorer
    /// can tell batch-mates (shared forward window) from sequential calls
    /// (window ends at the next call).
    CgCall {
        tool: String,
        query: String,
        returned: Vec<ReturnedItem>,
        turn: usize,
    },
    /// `turn` is the transcript line index the Read/Edit/Write came from, for the
    /// same reason `CgCall` carries one: a touch batched into the SAME assistant
    /// message as a call was decided before that call's result existed, so it
    /// cannot be evidence the result was used.
    FileTouch {
        path: String,
        turn: usize,
    },
    RawGrep,
    Other,
}

#[derive(Debug, Default)]
pub struct ParsedTranscript {
    pub events: Vec<Event>,
    pub unresolved: usize,
    pub unparseable: usize,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
}

/// Pull the inner text payload out of a tool_result's `content` (array of
/// {type:text,text} blocks, or a bare string).
fn tool_result_text(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// If a Bash command invokes `code-graph-mcp <query-subcommand>`, return the
/// canonical query name (via cli::canonical_query_cmd). Scans whitespace tokens so
/// COMPOUND commands (`cd x && code-graph-mcp callgraph Y`) are detected, and
/// matches both the bare binary and a path-suffixed form
/// (`./target/release/code-graph-mcp`). Housekeeping subcommands (stats/serve/…)
/// return None (canonical_query_cmd yields None for them).
///
/// A newline is a command separator in shell, but `split_whitespace` collapses it,
/// so scan each line independently — otherwise a multi-line script
/// (`cd backend\ncode-graph-mcp callgraph Foo`) hides the binary mid-token-stream
/// behind a non-separator predecessor and the call goes uncounted.
#[cfg(test)]
fn detect_cli_cg_call(cmd: &str) -> Option<&'static str> {
    cli_call(cmd).map(|(canon, _)| canon)
}

/// Detection + best-effort query recovery: returns `(canonical subcommand, query)`.
/// The query is the first positional (non-flag) argument after the subcommand, with
/// surrounding quotes stripped — so ranked `search_cli` replay labels carry the real
/// query (`code-graph-mcp search "login flow"` → `"login flow"`) instead of empty.
/// Best-effort: a value-taking flag placed before the positional could be mistaken for
/// the query; the common shapes (`search "q"`, `search "q" --json`, `grep -i p path`)
/// are correct. Empty string when no positional is present.
fn cli_call(cmd: &str) -> Option<(&'static str, String)> {
    cmd.lines().find_map(cli_call_in_line)
}

/// Does this shell token name the code-graph binary?
///
/// Matched against a command string as the agent typed it, so every spelling a
/// user can actually get must be accepted — and the ways to get one are not
/// obvious from the Rust side:
/// - **Two bin names.** `package.json` publishes BOTH `code-graph` and
///   `code-graph-mcp`. The short alias is the one people type, and it used to be
///   invisible here, so those calls never reached the conversion metric.
/// - **Windows shims.** npm writes `.cmd` (and `.ps1`) wrappers beside the
///   binary; `.bat` shows up in hand-written scripts. The plugin's own resolved
///   path is `…\.cache\code-graph\bin\code-graph-mcp.exe`, which an earlier
///   `t.ends_with("/code-graph-mcp")` check missed on BOTH counts — separator and
///   suffix — leaving the funnel DARK on Windows. The JS side
///   (`find-binary.js`, `auto-update.js`) had `.exe` handling all along.
///
/// Extensions are case-folded (`PATHEXT` is upper-case by default and cmd.exe
/// echoes what it resolved); the stem comparison stays exact, because the binary
/// name is lower-case on every platform we publish. Only extensions we actually
/// ship as launchers are stripped — stripping any trailing `.foo` would make
/// `code-graph-mcp.zip` read as an invocation.
///
/// Accepting `\` on Unix too is deliberate: this parses a recorded command
/// string whose originating platform is unknown, not a live filesystem path, so
/// there is no Unix-filename ambiguity to preserve here.
fn is_cg_binary_token(t: &str) -> bool {
    const BINS: [&str; 2] = ["code-graph-mcp", "code-graph"];
    const LAUNCHER_EXTS: [&str; 4] = [".exe", ".cmd", ".bat", ".ps1"];
    let stem = t
        .rfind('.')
        .filter(|i| {
            LAUNCHER_EXTS
                .iter()
                .any(|ext| t[*i..].eq_ignore_ascii_case(ext))
        })
        .map(|i| &t[..i])
        .unwrap_or(t);
    BINS.iter().any(|bin| {
        stem == *bin || stem.ends_with(&format!("/{bin}")) || stem.ends_with(&format!("\\{bin}"))
    })
}

fn cli_call_in_line(line: &str) -> Option<(&'static str, String)> {
    let toks = shell_tokens(line);
    for (i, t) in toks.iter().enumerate() {
        let is_bin = is_cg_binary_token(t);
        if !is_bin {
            continue;
        }
        // Command-position guard: the binary must begin a command — token 0, or right
        // after a shell separator. Excludes mid-command mentions in echo / commit
        // messages / comments. Quoted spans are single tokens (so a binary name inside
        // `git commit -m "… code-graph-mcp grep …"` is never a bare token here). Forms
        // like `env X=Y code-graph-mcp …` or `$(code-graph-mcp …)` are conservatively skipped.
        let prev_is_separator = i > 0 && {
            let p = toks[i - 1].as_str();
            matches!(p, "&&" | "||" | "|" | ";" | "&")
                || p.ends_with(';')
                || p.ends_with('&')
                || p.ends_with('|')
        };
        if i != 0 && !prev_is_separator {
            continue;
        }
        if let Some(canon) = toks
            .get(i + 1)
            .and_then(|s| crate::utils::telemetry::canonical_query_cmd(s))
        {
            let query = toks[i + 2..]
                .iter()
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_default();
            return Some((canon, query));
        }
    }
    None
}

/// Minimal shell-ish tokenizer: splits on whitespace but keeps a `"…"` / `'…'` quoted
/// span as one token (quotes stripped), so a space-containing query survives. Shell
/// separators (`&&`, `|`, `;`) remain their own tokens. Best-effort — no escape or
/// variable-expansion handling.
fn shell_tokens(s: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                has = true;
                for n in chars.by_ref() {
                    if n == c {
                        break;
                    }
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if has {
                    toks.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        toks.push(cur);
    }
    toks
}

pub fn parse_transcript(content: &str) -> ParsedTranscript {
    use std::collections::HashMap;
    // Pass 1: tool_use_id -> result payload text.
    let mut results: HashMap<String, String> = HashMap::new();
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
            continue;
        };
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                if let (Some(id), Some(text)) = (
                    b.get("tool_use_id").and_then(|x| x.as_str()),
                    b.get("content").and_then(tool_result_text),
                ) {
                    results.insert(id.to_string(), text);
                }
            }
        }
    }
    // Pass 2: build events in order from tool_use blocks.
    let mut out = ParsedTranscript::default();
    for (turn, line) in content.lines().enumerate() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if out.first_ts.is_none() {
                out.first_ts = Some(ts.to_string());
            }
            out.last_ts = Some(ts.to_string());
        }
        let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
            continue;
        };
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let input = b.get("input").cloned().unwrap_or(serde_json::Value::Null);
            if let Some(tool) = cg_pull_tool(name) {
                match results.get(id) {
                    None => out.unresolved += 1,
                    Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
                        Ok(payload) => {
                            let query = input
                                .get("query")
                                .or_else(|| input.get("symbol"))
                                .or_else(|| input.get("name"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let returned = extract_returned(&payload, is_ranked_tool(&tool));
                            out.events.push(Event::CgCall {
                                tool,
                                query,
                                returned,
                                turn,
                            });
                        }
                        Err(_) => out.unparseable += 1,
                    },
                }
            } else if name == "Read" || name == "Edit" || name == "Write" {
                if let Some(fp) = input.get("file_path").and_then(|x| x.as_str()) {
                    out.events.push(Event::FileTouch {
                        path: fp.to_string(),
                        turn,
                    });
                }
            } else if name == "Bash" {
                let cmd = input.get("command").and_then(|x| x.as_str()).unwrap_or("");
                if let Some((canon, query)) = cli_call(cmd) {
                    let tool = format!("{canon}_cli");
                    let ranked = is_ranked_tool(&tool);
                    match results.get(id) {
                        None => out.unresolved += 1,
                        Some(stdout) => {
                            let returned = extract_returned_from_cli(stdout, ranked);
                            out.events.push(Event::CgCall {
                                tool,
                                query,
                                returned,
                                turn,
                            });
                        }
                    }
                } else if cmd.contains("grep ") || cmd.contains("rg ") {
                    out.events.push(Event::RawGrep);
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct CallOutcome {
    pub tool: String,
    pub query: String,
    pub returned_files: Vec<String>,
    pub adopted: bool,
    pub adopted_rank: Option<usize>,
    /// The returned file that was actually adopted (the path matched during the
    /// forward scan). Captured directly so it never depends on indexing the
    /// compacted `returned_files` by `adopted_rank` — those indices diverge when a
    /// ranked payload item lacks a `file_path` and is filtered out (rank carries the
    /// ORIGINAL array index; `returned_files` is positional). None when not adopted.
    pub adopted_file: Option<String>,
    pub ranked: bool,
    /// 1-based index of the FIRST adopting FileTouch among the file touches after
    /// the call (1 = the very next touch adopted). Calibrates the adoption window:
    /// if the mass sits at small N, the unbounded until-next-call window is
    /// insensitive; a long tail would mean late touches are being credited.
    pub adoption_distance: Option<usize>,
}

pub fn score_session(events: &[Event]) -> Vec<CallOutcome> {
    use std::collections::HashSet;
    let mut touched_before: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        match ev {
            Event::FileTouch { path, .. } => {
                touched_before.insert(path.clone());
            }
            Event::CgCall {
                tool,
                query,
                returned,
                turn,
            } => {
                // Candidate returned files not already opened before this call.
                let candidates: Vec<&ReturnedItem> = returned
                    .iter()
                    .filter(|it| !touched_before.iter().any(|t| paths_match(&it.file_path, t)))
                    .collect();
                // Forward scan until the next CgCall. Track the lowest-rank matched
                // ranked item (rank + its file) and the first matched file overall, so
                // `adopted_file` is the path that was actually touched — not a re-index
                // of the compacted `returned_files` by rank (those indices diverge).
                let mut best_ranked: Option<(usize, String)> = None;
                let mut first_match_file: Option<String> = None;
                let mut adopted = false;
                let mut touches_seen = 0usize;
                let mut adoption_distance: Option<usize> = None;
                for ev2 in &events[i + 1..] {
                    match ev2 {
                        // A batch-mate (same assistant turn) doesn't close the window —
                        // the model saw all batched results before acting on any of
                        // them. Only a call from a LATER turn ends attribution.
                        Event::CgCall { turn: t2, .. } if t2 != turn => break,
                        Event::CgCall { .. } => {}
                        // A touch from the SAME assistant message as the call was
                        // issued before its result existed — the model batched them.
                        // Crediting it inflated adoption and, because it always
                        // landed in the d1 bucket, corrupted the very histogram used
                        // to argue the attribution window is tight. Skipped entirely,
                        // so it does not shift the distance of a later real adoption
                        // either.
                        Event::FileTouch { turn: t2, .. } if *t2 == *turn => {}
                        Event::FileTouch { path, .. } => {
                            touches_seen += 1;
                            for it in &candidates {
                                if paths_match(&it.file_path, path) {
                                    adopted = true;
                                    if first_match_file.is_none() {
                                        first_match_file = Some(it.file_path.clone());
                                        adoption_distance = Some(touches_seen);
                                    }
                                    if let Some(r) = it.rank {
                                        if best_ranked.as_ref().is_none_or(|(b, _)| r < *b) {
                                            best_ranked = Some((r, it.file_path.clone()));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // Prefer the lowest-rank ranked match; fall back to the first match
                // (structural/unranked adoptions previously yielded a null adopted_file).
                let (adopted_rank, adopted_file) = match best_ranked {
                    Some((r, f)) => (Some(r), Some(f)),
                    None => (None, first_match_file),
                };
                out.push(CallOutcome {
                    tool: tool.clone(),
                    query: query.clone(),
                    returned_files: returned.iter().map(|r| r.file_path.clone()).collect(),
                    adopted,
                    adopted_rank,
                    adopted_file,
                    ranked: is_ranked_tool(tool),
                    adoption_distance,
                });
            }
            _ => {}
        }
    }
    out
}

pub const MIN_N: usize = 20;

#[derive(Debug, Default)]
pub struct OutcomeSummary {
    pub transcripts: usize,
    pub sessions: usize,
    pub cg_calls: usize,
    pub unresolved: usize,
    pub unparseable: usize,
    /// Transcript files in the directory that could not be READ at all (permissions,
    /// a directory named `*.jsonl`, an I/O error). They used to be skipped in
    /// silence, which shrank N while the run still printed `0/0 = 0%` as a finding
    /// — a confident answer built on files nobody looked at. Set by `run_outcome`.
    pub unreadable: usize,
    pub adopted: usize,
    pub adoption_rate: f64,
    pub ranked_calls: usize,
    pub ranked_adopted: usize,
    pub field_mrr_adopted: f64,
    pub field_mrr_all: f64,
    pub rank_histogram: std::collections::BTreeMap<usize, usize>,
    /// distance (Nth file-touch after the call) -> adoption count. Window calibration.
    pub adoption_distance_histogram: std::collections::BTreeMap<usize, usize>,
    pub by_tool: std::collections::BTreeMap<String, (usize, usize)>, // tool -> (calls, adopted)
    /// Adoption-rate confidence: total cg calls below MIN_N.
    pub low_confidence: bool,
    /// field-MRR confidence: RANKED calls below MIN_N. Distinct from `low_confidence`
    /// because the MRR denominator is `ranked_calls`, not `cg_calls` — a run can have
    /// plenty of adoption samples yet a single ranked sample (the headline MRR would
    /// otherwise read as a confident 1.00 off N=1).
    pub field_mrr_low_confidence: bool,
    /// Reporting-window context (design §8): oldest / newest transcript timestamp seen
    /// (ISO8601, compared lexicographically) and the `--since` day filter if any. Set by
    /// `run_outcome` after aggregation, not derived from the call list.
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    pub since_days: Option<u64>,
}

pub fn aggregate(
    calls: &[CallOutcome],
    transcripts: usize,
    sessions: usize,
    unresolved: usize,
    unparseable: usize,
) -> OutcomeSummary {
    let mut s = OutcomeSummary {
        transcripts,
        sessions,
        unresolved,
        unparseable,
        cg_calls: calls.len(),
        ..Default::default()
    };
    let mut rr_adopted_sum = 0.0f64;
    let mut rr_all_sum = 0.0f64;
    for c in calls {
        let e = s.by_tool.entry(c.tool.clone()).or_insert((0, 0));
        e.0 += 1;
        if c.adopted {
            s.adopted += 1;
            e.1 += 1;
            if let Some(r) = c.adopted_rank {
                *s.rank_histogram.entry(r).or_insert(0) += 1;
            }
            if let Some(d) = c.adoption_distance {
                *s.adoption_distance_histogram.entry(d).or_insert(0) += 1;
            }
        }
        if c.ranked {
            s.ranked_calls += 1;
            let rr = if c.adopted {
                c.adopted_rank
                    .map(|r| 1.0 / (r as f64 + 1.0))
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            rr_all_sum += rr;
            if c.adopted {
                s.ranked_adopted += 1;
                rr_adopted_sum += rr;
            }
        }
    }
    s.adoption_rate = if s.cg_calls > 0 {
        s.adopted as f64 / s.cg_calls as f64
    } else {
        0.0
    };
    s.field_mrr_adopted = if s.ranked_adopted > 0 {
        rr_adopted_sum / s.ranked_adopted as f64
    } else {
        0.0
    };
    s.field_mrr_all = if s.ranked_calls > 0 {
        rr_all_sum / s.ranked_calls as f64
    } else {
        0.0
    };
    s.low_confidence = s.cg_calls < MIN_N;
    s.field_mrr_low_confidence = s.ranked_calls < MIN_N;
    s
}

// ── Task 6: Orchestration, render, CLI wiring ────────────────────────────────

use anyhow::Result;
use clap::Parser;
use std::time::{Duration, SystemTime};

#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp outcome",
    about = "Measure whether code-graph retrieval results get adopted by the model (read-only; reads session transcripts)"
)]
pub struct OutcomeArgs {
    /// Project whose transcripts to read (absolute path; default: resolved project root)
    #[arg(long)]
    pub project: Option<String>,
    /// Only transcripts modified within the last N days
    #[arg(long)]
    pub since: Option<u64>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Append (query, returned, adopted, rank) label rows as JSONL to this path
    #[arg(long)]
    pub emit_labels: Option<String>,
}

/// Read every transcript in `dir`, score each session, aggregate. Pure-ish (only fs reads).
pub fn run_outcome(
    dir: &std::path::Path,
    since_days: Option<u64>,
) -> (OutcomeSummary, Vec<CallOutcome>) {
    let cutoff = since_days.map(|d| SystemTime::now() - Duration::from_secs(d * 86_400));
    let mut all_calls = Vec::new();
    let mut transcripts = 0usize;
    let mut unresolved = 0usize;
    let mut unparseable = 0usize;
    let mut unreadable = 0usize;
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    // A directory we cannot ENUMERATE is the same silent zero one level up: the
    // caller's `dir.is_dir()` gate already passed, so an unreadable transcript
    // directory (mode 000, a broken mount) would otherwise sail through to
    // `Adoption: 0/0 = 0%` with no UNREAD line and `unreadable: 0` — a rate
    // computed over nothing, presented as a rate computed over everything. One
    // count, because we cannot know how many entries we did not see.
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => {
            let mut summary = aggregate(&[], 0, 0, 0, 0);
            summary.unreadable = 1;
            summary.since_days = since_days;
            return (summary, Vec::new());
        }
    };
    for entry in entries {
        // An entry we cannot even stat is counted rather than dropped: we do not
        // know whether it was a transcript, and "might have been" is exactly what
        // the caller needs to see before reading the percentage as complete.
        let Ok(entry) = entry else {
            unreadable += 1;
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(cut) = cutoff {
            if let Ok(meta) = entry.metadata() {
                if meta.modified().map(|m| m < cut).unwrap_or(false) {
                    continue;
                }
            }
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            unreadable += 1;
            continue;
        };
        let parsed = parse_transcript(&content);
        unresolved += parsed.unresolved;
        unparseable += parsed.unparseable;
        if let Some(ft) = &parsed.first_ts {
            if first_ts.as_ref().is_none_or(|c| ft < c) {
                first_ts = Some(ft.clone());
            }
        }
        if let Some(lt) = &parsed.last_ts {
            if last_ts.as_ref().is_none_or(|c| lt > c) {
                last_ts = Some(lt.clone());
            }
        }
        all_calls.extend(score_session(&parsed.events));
        transcripts += 1;
    }
    let mut summary = aggregate(
        &all_calls,
        transcripts,
        transcripts,
        unresolved,
        unparseable,
    );
    summary.first_ts = first_ts;
    summary.last_ts = last_ts;
    summary.since_days = since_days;
    summary.unreadable = unreadable;
    (summary, all_calls)
}

pub fn cmd_outcome(project_root: &std::path::Path, args: OutcomeArgs) -> Result<()> {
    let home = crate::utils::paths::home_dir().ok_or_else(|| {
        anyhow::anyhow!("Cannot determine home directory ($HOME / $USERPROFILE not set)")
    })?;
    let target = match &args.project {
        Some(p) => std::path::PathBuf::from(p),
        None => project_root.to_path_buf(),
    };
    let dir = transcript_dir(&target, &home);
    if !dir.is_dir() {
        if args.json {
            println!(
                "{}",
                serde_json::json!({"outcome": {"state": "absent", "dir": dir.display().to_string()}})
            );
        } else {
            eprintln!(
                "No transcripts for {} at {}",
                target.display(),
                dir.display()
            );
        }
        return Ok(());
    }
    let (s, calls) = run_outcome(&dir, args.since);
    if let Some(path) = &args.emit_labels {
        emit_labels(&calls, path)?;
    }
    if args.json {
        render_json(&s, &target);
    } else {
        render_human(&s, &target);
    }
    Ok(())
}

/// Append one JSONL row per cg call: the phase-2 replay dataset. Only calls that
/// adopted a returned file carry a usable (query → adopted_file) relevance label,
/// but every call is emitted so non-adoption is visible too.
pub fn emit_labels(calls: &[CallOutcome], path: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for c in calls {
        let row = serde_json::json!({
            "tool": c.tool,
            "query": c.query,
            "returned_files": c.returned_files,
            "adopted": c.adopted,
            "adopted_rank": c.adopted_rank,
            "adopted_file": c.adopted_file,
        });
        writeln!(f, "{}", serde_json::to_string(&row)?)?;
    }
    Ok(())
}

fn render_human(s: &OutcomeSummary, target: &std::path::Path) {
    println!(
        "Outcome (retrieval adoption)  \u{2014}  project: {}",
        target.display()
    );
    println!(
        "Transcripts: {}   resolved cg calls: {}   (unresolved {}, unparseable {})",
        s.transcripts, s.cg_calls, s.unresolved, s.unparseable
    );
    if s.unreadable > 0 {
        // Said separately and before the numbers, not tucked into the parenthetical
        // above: every rate below is computed over the transcripts we COULD read, so
        // the reader has to know some were missing before reading them.
        println!(
            "UNREAD: {} transcript file(s) in this directory could not be read \u{2014} \
             every rate below excludes them",
            s.unreadable
        );
    }
    if let (Some(f), Some(l)) = (&s.first_ts, &s.last_ts) {
        let since = s
            .since_days
            .map(|d| format!("   (--since {d}d)"))
            .unwrap_or_default();
        println!("Window: {f}  \u{2192}  {l}{since}");
    }
    if s.low_confidence {
        println!(
            "LOW CONFIDENCE: N={} (< {}) \u{2014} too small to conclude",
            s.cg_calls, MIN_N
        );
    }
    println!(
        "Adoption: {}/{} = {:.0}%",
        s.adopted,
        s.cg_calls,
        s.adoption_rate * 100.0
    );
    let mrr_caveat = if s.field_mrr_low_confidence {
        format!(
            "   [LOW CONFIDENCE: ranked N={} < {}]",
            s.ranked_calls, MIN_N
        )
    } else {
        String::new()
    };
    println!(
        "field-MRR (ranked tools, {}/{} ranked adopted)  adopted: {:.2}   all: {:.2}{}",
        s.ranked_adopted, s.ranked_calls, s.field_mrr_adopted, s.field_mrr_all, mrr_caveat
    );
    let hist: Vec<String> = s
        .rank_histogram
        .iter()
        .map(|(r, n)| format!("r{r}={n}"))
        .collect();
    println!(
        "Adopted-rank histogram: {}",
        if hist.is_empty() {
            "-".into()
        } else {
            hist.join("  ")
        }
    );
    let dist: Vec<String> = s
        .adoption_distance_histogram
        .iter()
        .map(|(d, n)| format!("d{d}={n}"))
        .collect();
    println!(
        "Adoption-distance histogram (Nth file-touch after call): {}",
        if dist.is_empty() {
            "-".into()
        } else {
            dist.join("  ")
        }
    );
    for (tool, (calls, adopted)) in &s.by_tool {
        println!("  {:<24} {}/{}", tool, adopted, calls);
    }
}

fn render_json(s: &OutcomeSummary, target: &std::path::Path) {
    println!(
        "{}",
        serde_json::json!({"outcome": {
            "state": "live",
            "project": target.display().to_string(),
            "transcripts": s.transcripts,
            "n_sessions": s.sessions,
            "since_days": s.since_days,
            "first_ts": s.first_ts,
            "last_ts": s.last_ts,
            "cg_calls": s.cg_calls,
            "unresolved": s.unresolved,
            "unparseable": s.unparseable,
            "unreadable": s.unreadable,
            "adopted": s.adopted,
            "adoption_rate": (s.adoption_rate * 100.0).round() / 100.0,
            "ranked_calls": s.ranked_calls,
            "ranked_adopted": s.ranked_adopted,
            "field_mrr_adopted": (s.field_mrr_adopted * 1000.0).round() / 1000.0,
            "field_mrr_all": (s.field_mrr_all * 1000.0).round() / 1000.0,
            "field_mrr_low_confidence": s.field_mrr_low_confidence,
            "rank_histogram": s.rank_histogram.iter().map(|(k,v)| (k.to_string(), *v)).collect::<std::collections::BTreeMap<_,_>>(),
            "adoption_distance_histogram": s.adoption_distance_histogram.iter().map(|(k,v)| (k.to_string(), *v)).collect::<std::collections::BTreeMap<_,_>>(),
            "by_tool": s.by_tool.iter().map(|(k,(c,a))| (k.clone(), serde_json::json!({"calls": c, "adopted": a}))).collect::<serde_json::Map<_,_>>(),
            "low_confidence": s.low_confidence,
        }})
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cg_pull_tool_matches_namespaced_cg_tools() {
        assert_eq!(
            cg_pull_tool("mcp__code-graph-dev__semantic_code_search").as_deref(),
            Some("semantic_code_search")
        );
        assert_eq!(
            cg_pull_tool("mcp__code-graph__get_call_graph").as_deref(),
            Some("get_call_graph")
        );
        assert_eq!(cg_pull_tool("Read"), None);
        assert_eq!(cg_pull_tool("mcp__other__semantic_code_search"), None);
        assert_eq!(cg_pull_tool("mcp__code-graph-dev__no_such_tool"), None);
    }

    #[test]
    fn ranked_vs_structural() {
        assert!(is_ranked_tool("semantic_code_search"));
        assert!(is_ranked_tool("ast_search"));
        assert!(!is_ranked_tool("get_call_graph"));
    }

    #[test]
    fn slug_replaces_non_alphanumeric_with_dash() {
        // underscore must become a dash (matches the real ~/.claude/projects/ dir name)
        assert_eq!(
            project_slug("/mnt/data_ssd/dev/projects/code-graph-mcp"),
            "-mnt-data-ssd-dev-projects-code-graph-mcp"
        );
        assert_eq!(
            project_slug("/mnt/data_ssd/dev/projects/daagu"),
            "-mnt-data-ssd-dev-projects-daagu"
        );
        // dots also become dashes; existing hyphens are preserved
        assert_eq!(project_slug("/home/sds/.claude/x"), "-home-sds--claude-x");
    }

    #[test]
    fn paths_match_relative_vs_absolute() {
        assert!(paths_match(
            "claude-plugin/scripts/session-init.js",
            "/home/u/proj/claude-plugin/scripts/session-init.js"
        ));
        assert!(paths_match("src/outcome.rs", "/x/src/outcome.rs"));
        assert!(!paths_match("src/outcome.rs", "/x/src/cli.rs"));
        assert!(!paths_match("", "/x/y"));
    }

    #[test]
    fn transcript_dir_joins_claude_projects() {
        assert_eq!(
            transcript_dir(
                std::path::Path::new("/a/b"),
                std::path::Path::new("/home/u")
            ),
            std::path::PathBuf::from("/home/u/.claude/projects/-a-b")
        );
    }

    /// The three Windows spellings of one project directory must reach one
    /// transcript directory. `canonicalize()` prints the `\\?\` form and users
    /// paste it back, which is exactly how the extended prefix gets in.
    #[test]
    fn transcript_dir_collapses_windows_spellings_to_one_slug() {
        let home = std::path::Path::new("/home/u");
        let native = transcript_dir_on(std::path::Path::new(r"D:\dev\repo"), home, true);
        for spelling in [
            r"D:\dev\repo",     // as typed
            "D:/dev/repo",      // forward slashes
            r"D:\dev/repo",     // mixed, as PathBuf::join produces
            r"\\?\D:\dev\repo", // as canonicalize() prints it
        ] {
            assert_eq!(
                transcript_dir_on(std::path::Path::new(spelling), home, true),
                native,
                "{spelling} must reach the same transcript dir as the native spelling"
            );
        }
        assert_eq!(
            native,
            std::path::PathBuf::from("/home/u/.claude/projects/D--dev-repo")
        );
    }

    /// `\` is a legal filename character on Unix, so the Unix leg must treat a
    /// path containing one as a name — not rewrite it into a separator. Without
    /// the platform parameter this assertion could not run here at all.
    #[test]
    fn transcript_dir_leaves_unix_backslash_paths_alone() {
        let home = std::path::Path::new("/home/u");
        assert_eq!(
            transcript_dir_on(std::path::Path::new(r"/srv/od\bc"), home, false),
            std::path::PathBuf::from("/home/u/.claude/projects/-srv-od-bc")
        );
        // A leading `\\?\` is a PREFIX on Windows but ordinary data on Unix, so
        // the two legs must disagree about it — this is the assertion that fails
        // if the platform flag is ignored.
        let looks_like_prefix = std::path::Path::new(r"\\?\D:\dev\repo");
        assert_ne!(
            transcript_dir_on(looks_like_prefix, home, false),
            transcript_dir_on(looks_like_prefix, home, true),
            "the extended prefix must be stripped only where `\\` is a separator"
        );
        // …and the production entry point agrees with the flag its platform implies.
        assert_eq!(
            transcript_dir(std::path::Path::new("/a/b"), home),
            transcript_dir_on(std::path::Path::new("/a/b"), home, cfg!(windows))
        );
    }

    #[test]
    fn paths_match_when_returned_is_the_longer_path() {
        // returned absolute, touched relative — exercises the (long, short) swap
        assert!(paths_match("/x/src/outcome.rs", "src/outcome.rs"));
    }

    #[test]
    fn paths_match_windows_backslash_touched_path() {
        // Read/Edit on a Windows client records `D:\repo\src\Foo.cs`. Splitting on
        // '/' alone made that one component, so it could never match the
        // repo-relative path a cg tool returned — the adoption half of the
        // conversion metric was structurally zero on Windows while the
        // call-recognition half worked. Runs on every platform: the input is a
        // recorded string, not a path this host produced.
        assert!(paths_match("src/Foo.cs", r"D:\repo\src\Foo.cs"));
        assert!(paths_match(r"src\Foo.cs", "/home/u/repo/src/Foo.cs"));
        // Mixed spelling (PowerShell tab-completion mixes them freely).
        assert!(paths_match(
            "src/parser/rust.rs",
            r"D:\repo\src/parser\rust.rs"
        ));
        // Still discriminating — a different file must not match.
        assert!(!paths_match("src/Foo.cs", r"D:\repo\src\Bar.cs"));
        assert!(!paths_match("src/a/Foo.cs", r"D:\repo\src\b\Foo.cs"));
    }

    #[test]
    fn extract_ranked_array_assigns_index_rank() {
        let payload = serde_json::json!([
            {"file_path": "a/b.rs", "relevance": 0.9, "name": "f"},
            {"file_path": "c/d.rs", "relevance": 0.5, "name": "g"}
        ]);
        let items = extract_returned(&payload, true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].file_path, "a/b.rs");
        assert_eq!(items[0].rank, Some(0));
        assert_eq!(items[1].rank, Some(1));
    }

    #[test]
    fn extract_structural_tree_collects_paths_without_rank() {
        // callgraph-style nested payload: file_path values buried in callers/callees.
        let payload = serde_json::json!({
            "symbol": "foo",
            "callers": [{"name": "x", "file_path": "src/x.rs"}],
            "callees": [{"name": "y", "file": "src/y.rs"}]
        });
        let items = extract_returned(&payload, false);
        let paths: Vec<&str> = items.iter().map(|i| i.file_path.as_str()).collect();
        assert!(paths.contains(&"src/x.rs"));
        assert!(paths.contains(&"src/y.rs"));
        assert!(items.iter().all(|i| i.rank.is_none()));
    }

    #[test]
    fn extract_handles_empty_and_garbage() {
        assert!(extract_returned(&serde_json::json!([]), true).is_empty());
        assert!(extract_returned(&serde_json::json!("oops"), true).is_empty());
    }

    #[test]
    fn extract_ranked_object_with_results_array_assigns_rank() {
        // ast_search always returns {results,count}; semantic_code_search returns
        // {results,search_mode} (FTS-only) or {mode,results} (compressed). Rank must
        // come from the results-array index, not be dropped to None.
        let payload = serde_json::json!({
            "results": [
                {"file_path": "a/b.rs", "name": "f"},
                {"file_path": "c/d.rs", "name": "g"}
            ],
            "count": 2
        });
        let items = extract_returned(&payload, true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rank, Some(0));
        assert_eq!(items[1].rank, Some(1));
    }

    #[test]
    fn parse_pairs_cg_call_with_result_then_edit() {
        let call = r#"{"type":"assistant","timestamp":"2026-06-29T10:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"mcp__code-graph-dev__semantic_code_search","input":{"query":"login flow"}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"[{\"file_path\":\"src/auth.rs\",\"name\":\"login\"}]"}]}]}}"#;
        let edit = r#"{"type":"assistant","timestamp":"2026-06-29T10:01:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu2","name":"Edit","input":{"file_path":"/proj/src/auth.rs"}}]}}"#;
        let content = format!("{call}\n{result}\n{edit}\n");
        let p = parse_transcript(&content);
        assert_eq!(p.unresolved, 0);
        assert_eq!(p.events.len(), 2);
        match &p.events[0] {
            Event::CgCall {
                tool,
                query,
                returned,
                ..
            } => {
                assert_eq!(tool, "semantic_code_search");
                assert_eq!(query, "login flow");
                assert_eq!(returned[0].file_path, "src/auth.rs");
                assert_eq!(returned[0].rank, Some(0));
            }
            _ => panic!("expected CgCall"),
        }
        assert!(
            matches!(&p.events[1], Event::FileTouch { path, .. } if path == "/proj/src/auth.rs")
        );
        assert_eq!(p.first_ts.as_deref(), Some("2026-06-29T10:00:00Z"));
        assert_eq!(p.last_ts.as_deref(), Some("2026-06-29T10:01:00Z"));
    }

    #[test]
    fn parse_counts_unresolved_cg_call() {
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tuX","name":"mcp__code-graph-dev__ast_search","input":{"query":"q"}}]}}"#;
        let p = parse_transcript(&format!("{call}\n"));
        assert_eq!(p.unresolved, 1);
        assert!(p.events.iter().all(|e| !matches!(e, Event::CgCall { .. })));
    }

    #[test]
    fn parse_skips_malformed_lines() {
        let p = parse_transcript("not json\n{}\n");
        assert_eq!(p.events.len(), 0);
    }

    #[test]
    fn parse_counts_unparseable_result_payload() {
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"mcp__code-graph-dev__semantic_code_search","input":{"query":"q"}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"not valid json"}]}]}}"#;
        let p = parse_transcript(&format!("{call}\n{result}\n"));
        assert_eq!(p.unparseable, 1);
        assert_eq!(p.unresolved, 0);
        assert!(p.events.iter().all(|e| !matches!(e, Event::CgCall { .. })));
    }

    // ── score_session helpers ──────────────────────────────────────────────

    fn cg_at(turn: usize, tool: &str, files: &[(&str, Option<usize>)]) -> Event {
        Event::CgCall {
            tool: tool.into(),
            query: "q".into(),
            returned: files
                .iter()
                .map(|(f, r)| ReturnedItem {
                    file_path: f.to_string(),
                    rank: *r,
                })
                .collect(),
            turn,
        }
    }
    /// Each call on its own turn (the sequential, non-batched default).
    fn cg(tool: &str, files: &[(&str, Option<usize>)]) -> Event {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT_TURN: AtomicUsize = AtomicUsize::new(1000);
        cg_at(NEXT_TURN.fetch_add(1, Ordering::Relaxed), tool, files)
    }
    /// A touch on its own turn — i.e. a turn AFTER whatever call precedes it, which
    /// is what "the model acted on the result" looks like. Same-turn touches are a
    /// different case and get [`touch_at`].
    fn touch(p: &str) -> Event {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT_TURN: AtomicUsize = AtomicUsize::new(5000);
        touch_at(NEXT_TURN.fetch_add(1, Ordering::Relaxed), p)
    }
    fn touch_at(turn: usize, p: &str) -> Event {
        Event::FileTouch {
            path: p.into(),
            turn,
        }
    }

    // ── outcome metric five-pack (2026-08-02 audit §16) ──────────────────────

    #[test]
    fn cli_ast_search_is_a_ranked_tool() {
        // `canonical_query_cmd("ast-search")` is the HYPHENATED "ast-search", so the
        // CLI event is `ast-search_cli` while the hand-written ranked list carried
        // the MCP spelling `ast_search`. Every CLI ast-search call therefore fell
        // out of the field-MRR denominator AND lost its rank — silently, because a
        // smaller denominator still renders as a confident number.
        assert!(is_ranked_tool("ast-search_cli"), "CLI ast-search is ranked");
        assert!(is_ranked_tool("search_cli"), "CLI search is ranked");
        assert!(is_ranked_tool("ast_search"), "MCP ast_search is ranked");
        assert!(is_ranked_tool("semantic_code_search"));
        // Negative controls: structural tools have no meaningful rank.
        assert!(!is_ranked_tool("callgraph_cli"));
        assert!(!is_ranked_tool("grep_cli"));
        assert!(!is_ranked_tool("get_call_graph"));
    }

    #[test]
    fn same_turn_touch_is_not_adoption() {
        // A Read batched in the SAME assistant message as the cg call was decided
        // before its result existed. Counting it inflated adoption and piled into
        // the d1 bucket, which is the bucket used to argue the window is tight.
        let events = vec![
            cg_at(7, "semantic_code_search", &[("src/a.rs", Some(0))]),
            touch_at(7, "/proj/src/a.rs"),
        ];
        let outs = score_session(&events);
        assert!(
            !outs[0].adopted,
            "a touch issued in the same message as the call cannot have used its result"
        );
        assert_eq!(outs[0].adoption_distance, None);

        // The same touch one turn later IS adoption — the negative control that
        // keeps the fix from simply switching adoption off.
        let events = vec![
            cg_at(7, "semantic_code_search", &[("src/a.rs", Some(0))]),
            touch_at(8, "/proj/src/a.rs"),
        ];
        let outs = score_session(&events);
        assert!(outs[0].adopted);
        assert_eq!(outs[0].adoption_distance, Some(1));
    }

    #[test]
    fn cli_call_recognizes_both_published_bin_names_and_windows_shims() {
        // package.json publishes TWO bins: `code-graph` and `code-graph-mcp`. On
        // Windows npm writes `.cmd` / `.ps1` shims beside them. Only the bare
        // `code-graph-mcp` (and `.exe`) was recognised, so every call through the
        // short alias or a shim went uncounted — the same dark-metric shape the
        // `.exe` fix closed.
        for cmd in [
            "code-graph callgraph Foo",
            "code-graph-mcp callgraph Foo",
            "code-graph.cmd callgraph Foo",
            "code-graph-mcp.cmd callgraph Foo",
            "code-graph-mcp.CMD callgraph Foo",
            "code-graph-mcp.bat callgraph Foo",
            "code-graph-mcp.ps1 callgraph Foo",
            r"C:\Users\x\.cache\code-graph\bin\code-graph.cmd callgraph Foo",
            "/usr/local/bin/code-graph callgraph Foo",
        ] {
            assert_eq!(
                detect_cli_cg_call(cmd),
                Some("callgraph"),
                "must recognise: {cmd}"
            );
        }
        // Negative controls: a different binary whose name merely ends in ours, and
        // a shim spelling we do not publish.
        assert_eq!(detect_cli_cg_call("my-code-graph callgraph Foo"), None);
        assert_eq!(detect_cli_cg_call("code-graph-mcp.zip callgraph Foo"), None);
        assert_eq!(detect_cli_cg_call("code-graphs callgraph Foo"), None);
    }

    #[test]
    fn transcript_dir_ignores_a_trailing_separator() {
        // `--project /repo/` slugified to `-repo-`, a directory Claude Code never
        // created, and the command answered `state: absent` with exit 0 — a typo's
        // worth of difference between "no data" and "you asked wrong".
        let home = Path::new("/home/u");
        for windows in [false, true] {
            let want = transcript_dir_on(Path::new("/mnt/dev/repo"), home, windows);
            assert_eq!(
                transcript_dir_on(Path::new("/mnt/dev/repo/"), home, windows),
                want,
                "trailing / must not change the slug (windows={windows})"
            );
        }
        assert_eq!(
            transcript_dir_on(Path::new(r"D:\dev\repo\"), home, true),
            transcript_dir_on(Path::new(r"D:\dev\repo"), home, true),
            "trailing backslash must not change the slug on Windows"
        );
        // The root itself is not a trailing separator to strip — it is the path.
        assert_eq!(
            transcript_dir_on(Path::new("/"), home, false),
            home.join(".claude").join("projects").join("-")
        );
    }

    #[test]
    fn unreadable_transcripts_are_counted_not_silently_skipped() {
        // A transcript we cannot read used to `continue`, so N shrank and the run
        // still printed `0/0 = 0%` as if it had looked at everything. A directory
        // named `*.jsonl` reproduces the read failure on every platform without
        // touching permissions.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("good.jsonl"), "").unwrap();
        std::fs::create_dir(dir.path().join("bad.jsonl")).unwrap();

        let (s, _) = run_outcome(dir.path(), None);
        assert_eq!(s.transcripts, 1, "only the readable transcript is scored");
        assert_eq!(
            s.unreadable, 1,
            "the unreadable one must be counted, not dropped"
        );
    }

    /// The same silent zero one level up, found by the pre-tag review of the
    /// per-file fix above: `read_dir` failing on the DIRECTORY was swallowed by
    /// `.into_iter().flatten()`, so an unreadable transcript dir produced
    /// `0/0 = 0%` with `unreadable: 0` and no disclosure — a rate over nothing
    /// presented as a rate over everything. Sibling holes are this repo's top
    /// bug class; fixing one arm and not the other is how they survive.
    #[cfg(unix)]
    #[test]
    fn an_unenumerable_transcript_dir_is_disclosed_not_read_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: root ignores directory permissions");
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let locked = dir.path().join("transcripts");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("a.jsonl"), "").unwrap();
        let original = std::fs::metadata(&locked).unwrap().permissions();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let (s, calls) = run_outcome(&locked, None);

        // Restore before asserting so a failure still leaves a removable TempDir.
        std::fs::set_permissions(&locked, original).unwrap();
        assert_eq!(s.transcripts, 0);
        assert!(calls.is_empty());
        assert_eq!(
            s.unreadable, 1,
            "a directory we cannot enumerate must be disclosed, not read as empty"
        );
    }

    #[test]
    fn adopted_when_forward_edit_hits_returned_untouched_file() {
        let events = vec![
            cg(
                "semantic_code_search",
                &[("src/a.rs", Some(0)), ("src/b.rs", Some(1))],
            ),
            touch("/proj/src/b.rs"),
        ];
        let outs = score_session(&events);
        assert_eq!(outs.len(), 1);
        assert!(outs[0].adopted);
        assert_eq!(outs[0].adopted_rank, Some(1));
        assert_eq!(
            outs[0].adoption_distance,
            Some(1),
            "adopted on the very next touch"
        );
    }

    #[test]
    fn adoption_distance_counts_intervening_non_matching_touches() {
        let events = vec![
            cg("get_call_graph", &[("src/a.rs", None)]),
            touch("/proj/src/unrelated.rs"),
            touch("/proj/src/also_unrelated.rs"),
            touch("/proj/src/a.rs"), // 3rd touch after the call adopts
        ];
        let outs = score_session(&events);
        assert!(outs[0].adopted);
        assert_eq!(outs[0].adoption_distance, Some(3));
        let s = aggregate(&outs, 1, 1, 0, 0);
        assert_eq!(s.adoption_distance_histogram.get(&3), Some(&1));
    }

    #[test]
    fn not_adopted_when_file_touched_before_the_call() {
        let events = vec![
            touch("/proj/src/a.rs"),
            cg("semantic_code_search", &[("src/a.rs", Some(0))]),
            touch("/proj/src/a.rs"),
        ];
        let outs = score_session(&events);
        assert!(!outs[0].adopted, "a.rs was already open before the call");
    }

    #[test]
    fn window_stops_at_next_cg_call() {
        let events = vec![
            cg("ast_search", &[("src/a.rs", Some(0))]),
            cg("ast_search", &[("src/z.rs", Some(0))]),
            touch("/proj/src/a.rs"), // after the 2nd call → not credited to the 1st
        ];
        let outs = score_session(&events);
        assert!(!outs[0].adopted); // a.rs touched after call 2 — outside call 1's window
        assert!(!outs[1].adopted); // call 2 returned z.rs; only a.rs was touched
    }

    #[test]
    fn batched_same_turn_calls_share_forward_window() {
        // Two cg calls batched in ONE assistant turn (same `turn`): the model saw both
        // results before touching anything, so a touch after the batch must credit
        // whichever call returned the file — not just the last batch member.
        let events = vec![
            cg_at(7, "grep_cli", &[("src/a.rs", None)]),
            cg_at(7, "grep_cli", &[("src/z.rs", None)]),
            touch("/proj/src/a.rs"),
        ];
        let outs = score_session(&events);
        assert!(
            outs[0].adopted,
            "first batched call returned the touched file → credit"
        );
        assert!(!outs[1].adopted, "second batch member returned z.rs only");
    }

    #[test]
    fn batched_window_still_ends_at_next_turn_call() {
        // The shared batch window ends at the first call from a DIFFERENT turn.
        let events = vec![
            cg_at(7, "grep_cli", &[("src/a.rs", None)]),
            cg_at(7, "grep_cli", &[("src/z.rs", None)]),
            cg_at(9, "grep_cli", &[("src/other.rs", None)]),
            touch("/proj/src/a.rs"), // after the turn-9 call → outside the batch window
        ];
        let outs = score_session(&events);
        assert!(
            !outs[0].adopted,
            "touch after a later-turn call is outside the window"
        );
        assert!(!outs[1].adopted);
    }

    #[test]
    fn parse_batched_tool_uses_in_one_message_share_turn_and_first_gets_credit() {
        // One assistant message carrying TWO cg tool_use blocks (parallel calls in a
        // single turn) → both events share the line's turn; an Edit after the batch
        // credits the FIRST call (the one whose result was actually used).
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"mcp__code-graph-dev__get_call_graph","input":{"symbol":"foo"}},{"type":"tool_use","id":"tu2","name":"mcp__code-graph-dev__get_call_graph","input":{"symbol":"bar"}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"{\"file_path\":\"src/a.rs\"}"}]},{"type":"tool_result","tool_use_id":"tu2","content":[{"type":"text","text":"{\"file_path\":\"src/z.rs\"}"}]}]}}"#;
        let edit = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu3","name":"Edit","input":{"file_path":"/proj/src/a.rs"}}]}}"#;
        let p = parse_transcript(&format!("{call}\n{result}\n{edit}\n"));
        let turns: Vec<usize> = p
            .events
            .iter()
            .filter_map(|e| match e {
                Event::CgCall { turn, .. } => Some(*turn),
                _ => None,
            })
            .collect();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0], turns[1], "same assistant message → same turn");
        let outs = score_session(&p.events);
        assert!(
            outs[0].adopted,
            "first batched MCP call's returned file was edited"
        );
        assert!(!outs[1].adopted);
    }

    #[test]
    fn best_rank_is_lowest_when_multiple_returned_items_are_touched() {
        // rank 2 and rank 0 both touched → adopted_rank should be Some(0) (the lower)
        let events = vec![
            cg(
                "semantic_code_search",
                &[
                    ("src/a.rs", Some(2)),
                    ("src/b.rs", Some(0)),
                    ("src/c.rs", Some(5)),
                ],
            ),
            touch("/proj/src/a.rs"),
            touch("/proj/src/b.rs"),
        ];
        let outs = score_session(&events);
        assert!(outs[0].adopted);
        assert_eq!(
            outs[0].adopted_rank,
            Some(0),
            "lowest rank among touched items wins"
        );
    }

    #[test]
    fn structural_tool_adopted_with_no_rank() {
        // get_call_graph is NOT ranked — returned items have rank: None
        // touching a returned file should still mark adopted=true; adopted_rank stays None
        let events = vec![
            cg(
                "get_call_graph",
                &[("src/graph.rs", None), ("src/storage.rs", None)],
            ),
            touch("/proj/src/graph.rs"),
        ];
        let outs = score_session(&events);
        assert!(
            outs[0].adopted,
            "structural tool file should be adopted when touched"
        );
        assert_eq!(outs[0].adopted_rank, None, "structural items carry no rank");
        assert!(!outs[0].ranked, "get_call_graph is not a ranked tool");
    }

    // ── Phase-2 CLI tests ────────────────────────────────────────────────────

    #[test]
    fn scan_extracts_path_line_tokens() {
        // grep-style + search-style lines; path:line and path:line-line
        let text = "src/a.rs:5  fn foo\nsrc/a.rs:9  bar\nh3 Title  CHANGELOG.md:3708-3709";
        let paths = scan_path_line_paths(text);
        assert_eq!(paths, vec!["src/a.rs", "src/a.rs", "CHANGELOG.md"]); // raw, with dups
    }

    #[test]
    fn cli_extract_callgraph_paren_paths_fallback() {
        // callgraph/impact human output has NO path:line tokens — paths appear as
        // `symbol (src/foo.rs)`. Without the paren fallback these calls always
        // yielded returned_files = [] → adoption structurally impossible (the
        // callgraph_cli 0/7 reading was this artifact, not real non-adoption).
        let stdout = "run_full_index (src/indexer/pipeline/mod.rs)\n  \u{2190} called by: ensure_indexed (src/mcp/server/mod.rs) [function]\n  (75 test callers hidden, use --include-tests to show)";
        let items = extract_returned_from_cli(stdout, false);
        let paths: Vec<&str> = items.iter().map(|i| i.file_path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["src/indexer/pipeline/mod.rs", "src/mcp/server/mod.rs"]
        );
        assert!(items.iter().all(|i| i.rank.is_none()));
    }

    #[test]
    fn cli_extract_paren_fallback_not_used_when_path_line_present() {
        // grep-style output HAS path:line hits — a parenthesized path inside code
        // content must NOT leak into returned_files (fallback only fires on zero
        // path:line tokens).
        let stdout = "src/a.rs:5  include_str!(\"src/embedded.txt\")\nfn foo (src/phantom.rs)";
        let items = extract_returned_from_cli(stdout, false);
        let paths: Vec<&str> = items.iter().map(|i| i.file_path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs"]);
    }

    #[test]
    fn scan_paren_paths_rejects_prose_and_versions() {
        let text = "x (src/a.rs)\ny (75 test callers hidden, use --include-tests to show)\nz (v0.99.1)\nw (lines 111-136)";
        assert_eq!(scan_paren_paths(text), vec!["src/a.rs"]);
    }

    #[test]
    fn cli_extract_human_dedupes_first_occurrence() {
        let stdout = "src/a.rs:5\nsrc/a.rs:9\nsrc/b.rs:2";
        let items = extract_returned_from_cli(stdout, false);
        let paths: Vec<&str> = items.iter().map(|i| i.file_path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]); // unique, first-occurrence order
        assert!(items.iter().all(|i| i.rank.is_none()));
    }

    #[test]
    fn cli_extract_ranked_assigns_index_rank() {
        let stdout = "h3 A  x/a.rs:1-2\nh3 B  y/b.rs:3-4";
        let items = extract_returned_from_cli(stdout, true);
        assert_eq!(items[0].file_path, "x/a.rs");
        assert_eq!(items[0].rank, Some(0));
        assert_eq!(items[1].rank, Some(1));
    }

    #[test]
    fn cli_extract_json_fast_path() {
        let stdout = r#"{"results":[{"file_path":"src/z.rs"}]}"#;
        let items = extract_returned_from_cli(stdout, true);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].file_path, "src/z.rs");
        assert_eq!(items[0].rank, Some(0));
    }

    #[test]
    fn cli_extract_no_paths_is_empty() {
        assert!(
            extract_returned_from_cli("[code-graph] No call graph results for: foo", false)
                .is_empty()
        );
    }

    #[test]
    fn search_cli_is_ranked() {
        assert!(is_ranked_tool("search_cli"));
        assert!(!is_ranked_tool("callgraph_cli"));
    }

    #[test]
    fn parse_classifies_bash_grep_as_raw_grep() {
        let bash = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"rg some_fn src/"}}]}}"#;
        let p = parse_transcript(&format!("{bash}\n"));
        assert_eq!(p.events.len(), 1);
        assert!(matches!(&p.events[0], Event::RawGrep));
    }

    #[test]
    fn parse_detects_cli_callgraph_as_cgcall_not_rawgrep() {
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"code-graph-mcp callgraph Foo"}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":[{"type":"text","text":"src/foo.rs:10  fn Foo"}]}]}}"#;
        let p = parse_transcript(&format!("{call}\n{result}\n"));
        assert_eq!(p.events.len(), 1);
        match &p.events[0] {
            Event::CgCall { tool, returned, .. } => {
                assert_eq!(tool, "callgraph_cli");
                assert_eq!(returned[0].file_path, "src/foo.rs");
                assert_eq!(returned[0].rank, None); // structural
            }
            _ => panic!("expected CgCall, got RawGrep/Other"),
        }
    }

    #[test]
    fn parse_detects_cli_search_ranked_and_compound() {
        // compound command (cd && code-graph-mcp search) + ranked search_cli
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b2","name":"Bash","input":{"command":"cd backend && code-graph-mcp search \"login\""}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b2","content":[{"type":"text","text":"h3 a  src/a.rs:1-2\nh3 b  src/b.rs:3-4"}]}]}}"#;
        let p = parse_transcript(&format!("{call}\n{result}\n"));
        match &p.events[0] {
            Event::CgCall { tool, returned, .. } => {
                assert_eq!(tool, "search_cli");
                assert_eq!(returned[0].rank, Some(0));
                assert_eq!(returned[1].rank, Some(1));
            }
            _ => panic!("expected search_cli CgCall"),
        }
    }

    #[test]
    fn parse_raw_grep_still_rawgrep_and_housekeeping_ignored() {
        let raw = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"g1","name":"Bash","input":{"command":"grep -rn foo src/"}}]}}"#;
        let house = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"s1","name":"Bash","input":{"command":"code-graph-mcp stats"}}]}}"#;
        let p = parse_transcript(&format!("{raw}\n{house}\n"));
        assert!(matches!(&p.events[0], Event::RawGrep)); // raw grep unchanged
        assert!(p.events.iter().all(|e| !matches!(e, Event::CgCall { .. }))); // stats = housekeeping, not a cg call
    }

    // ── aggregate / OutcomeSummary helpers ───────────────────────────────────

    fn co(tool: &str, ranked: bool, adopted: bool, rank: Option<usize>) -> CallOutcome {
        CallOutcome {
            tool: tool.into(),
            query: "q".into(),
            returned_files: vec![],
            adopted,
            adopted_rank: rank,
            adopted_file: None,
            ranked,
            adoption_distance: adopted.then_some(1),
        }
    }

    #[test]
    fn mrr_reported_two_ways() {
        let calls = vec![
            co("semantic_code_search", true, true, Some(0)), // rr = 1.0
            co("semantic_code_search", true, true, Some(2)), // rr = 1/3
            co("semantic_code_search", true, false, None),   // rr = 0 for _all only
        ];
        let s = aggregate(&calls, 1, 1, 0, 0);
        assert_eq!(s.cg_calls, 3);
        assert_eq!(s.adopted, 2);
        // adopted-only: mean(1.0, 0.333) = 0.667
        assert!((s.field_mrr_adopted - 0.6667).abs() < 0.001);
        // all ranked: mean(1.0, 0.333, 0.0) = 0.444
        assert!((s.field_mrr_all - 0.4444).abs() < 0.001);
        assert!(s.low_confidence); // 3 < MIN_N
    }

    #[test]
    fn structural_tools_excluded_from_mrr_but_counted_in_adoption() {
        let calls = vec![co("get_call_graph", false, true, None)];
        let s = aggregate(&calls, 1, 1, 0, 0);
        assert_eq!(s.adopted, 1);
        assert_eq!(s.ranked_calls, 0);
        assert_eq!(s.field_mrr_adopted, 0.0); // no ranked calls → 0, not NaN
    }

    #[test]
    fn parse_handles_bare_string_tool_result_content() {
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"mcp__code-graph-dev__semantic_code_search","input":{"query":"q"}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"[{\"file_path\":\"src/a.rs\"}]"}]}}"#;
        let p = parse_transcript(&format!("{call}\n{result}\n"));
        assert_eq!(p.unresolved, 0);
        assert_eq!(p.unparseable, 0);
        match &p.events[0] {
            Event::CgCall { returned, .. } => assert_eq!(returned[0].file_path, "src/a.rs"),
            _ => panic!("expected CgCall"),
        }
    }

    #[test]
    fn run_outcome_e2e_over_temp_transcript_dir() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("session1.jsonl")).unwrap();
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"mcp__code-graph-dev__semantic_code_search","input":{"query":"q"}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"[{\"file_path\":\"src/a.rs\"}]"}]}]}}"#;
        let edit = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/x/src/a.rs"}}]}}"#;
        writeln!(f, "{call}\n{result}\n{edit}").unwrap();
        let (summary, calls) = run_outcome(dir.path(), None);
        assert_eq!(summary.cg_calls, 1);
        assert_eq!(summary.adopted, 1);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].adopted);
    }

    #[test]
    fn detect_rejects_non_head_code_graph_mcp_mentions() {
        // mid-command mentions (echo / commit message / comment) are NOT cg calls
        assert_eq!(detect_cli_cg_call("echo code-graph-mcp grep foo"), None);
        assert_eq!(
            detect_cli_cg_call("git commit -m \"fix code-graph-mcp grep parsing\""),
            None
        );
        // real invocations still detected: at head, after `&&`, and head-then-pipe
        assert_eq!(
            detect_cli_cg_call("code-graph-mcp callgraph Foo"),
            Some("callgraph")
        );
        assert_eq!(
            detect_cli_cg_call("cd be && code-graph-mcp search x"),
            Some("search")
        );
        assert_eq!(
            detect_cli_cg_call("code-graph-mcp grep p | head"),
            Some("grep")
        );
    }

    /// The plugin resolves an absolute binary path, which on Windows ends in
    /// `\code-graph-mcp.exe`. The old check (`ends_with("/code-graph-mcp")`)
    /// missed both the separator and the suffix, so every such invocation went
    /// unrecorded and the conversion metric read zero on Windows — `doctor`
    /// then reported the funnel DARK with nothing actually broken.
    ///
    /// Pure string logic, so every CI leg exercises the Windows spellings.
    #[test]
    fn detect_accepts_windows_binary_spellings() {
        for cmd in [
            r"C:\Users\jo\.cache\code-graph\bin\code-graph-mcp.exe grep Foo",
            r".\bin\code-graph-mcp.exe grep Foo",
            "code-graph-mcp.exe grep Foo",
            "/home/jo/.cache/code-graph/bin/code-graph-mcp grep Foo",
            "code-graph-mcp grep Foo",
        ] {
            assert_eq!(
                detect_cli_cg_call(cmd),
                Some("grep"),
                "binary spelling not recognized: {cmd}"
            );
        }
        // The command-position and name guards must survive the widening.
        assert_eq!(
            detect_cli_cg_call(r"echo C:\bin\code-graph-mcp.exe grep Foo"),
            None,
            "a mid-command mention is still not an invocation"
        );
        assert_eq!(
            detect_cli_cg_call("my-code-graph-mcp grep Foo"),
            None,
            "a different binary whose name merely ENDS with ours is not ours"
        );
        assert_eq!(
            detect_cli_cg_call("code-graph-mcp.exe.bak grep Foo"),
            None,
            "only a real .exe suffix is stripped"
        );
        // PATHEXT is upper-case by default and cmd.exe echoes what it resolved,
        // so a transcript can carry `.EXE`. Case-folding the suffix alone: the
        // stem stays exact, since the published binary name is lower-case.
        for cmd in [
            r"C:\bin\code-graph-mcp.EXE grep Foo",
            "code-graph-mcp.Exe grep Foo",
        ] {
            assert_eq!(
                detect_cli_cg_call(cmd),
                Some("grep"),
                "upper-case extension not recognized: {cmd}"
            );
        }
        assert_eq!(
            detect_cli_cg_call("CODE-GRAPH-MCP.EXE grep Foo"),
            None,
            "the stem is not case-folded — that is a different file on Unix"
        );
    }

    #[test]
    fn parse_cli_cg_call_without_result_increments_unresolved() {
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"x1","name":"Bash","input":{"command":"code-graph-mcp callgraph Foo"}}]}}"#;
        let p = parse_transcript(&format!("{call}\n"));
        assert_eq!(p.events.len(), 0);
        assert_eq!(p.unresolved, 1);
    }

    #[test]
    fn emit_labels_writes_jsonl_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("labels.jsonl");
        let calls = vec![CallOutcome {
            tool: "semantic_code_search".into(),
            query: "login".into(),
            returned_files: vec!["src/a.rs".into(), "src/b.rs".into()],
            adopted: true,
            adopted_rank: Some(1),
            adopted_file: Some("src/b.rs".into()),
            ranked: true,
            adoption_distance: Some(1),
        }];
        emit_labels(&calls, path.to_str().unwrap()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let row: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(row["query"], "login");
        assert_eq!(row["adopted_rank"], 1);
        assert_eq!(row["adopted"], true);
        assert_eq!(row["adopted_file"], "src/b.rs");
    }

    #[test]
    fn field_mrr_low_confidence_keys_on_ranked_n_not_total() {
        // adoption N can clear MIN_N while ranked N does not: 25 structural + 1 ranked.
        // low_confidence (adoption) must be false; field_mrr_low_confidence must be true
        // so the headline MRR isn't presented as a confident value off a single sample.
        let mut calls: Vec<CallOutcome> = (0..25)
            .map(|_| co("get_call_graph", false, true, None))
            .collect();
        calls.push(co("semantic_code_search", true, true, Some(0)));
        let s = aggregate(&calls, 1, 1, 0, 0);
        assert_eq!(s.cg_calls, 26);
        assert!(!s.low_confidence, "adoption N=26 ≥ MIN_N");
        assert_eq!(s.ranked_calls, 1);
        assert!(
            s.field_mrr_low_confidence,
            "ranked N=1 < MIN_N → MRR untrustworthy"
        );
    }

    #[test]
    fn adopted_file_tracks_matched_path_not_rank_index() {
        // Returned list has a rank GAP (rank 0 then rank 2 — the rank-1 payload item had
        // no file_path and was compacted out). returned_files is the positional list
        // [a, c]; indexing it by adopted_rank (2) would be out of bounds. The adopted
        // file must be the path actually touched, captured during the forward scan.
        let events = vec![
            Event::CgCall {
                tool: "semantic_code_search".into(),
                query: "q".into(),
                returned: vec![
                    ReturnedItem {
                        file_path: "src/a.rs".into(),
                        rank: Some(0),
                    },
                    ReturnedItem {
                        file_path: "src/c.rs".into(),
                        rank: Some(2),
                    },
                ],
                turn: 0,
            },
            touch("/proj/src/c.rs"),
        ];
        let outs = score_session(&events);
        assert!(outs[0].adopted);
        assert_eq!(outs[0].adopted_rank, Some(2));
        assert_eq!(outs[0].adopted_file.as_deref(), Some("src/c.rs"));
        assert_eq!(outs[0].returned_files, vec!["src/a.rs", "src/c.rs"]);
        // The old rank-as-index approach would have read returned_files[2] = out of bounds.
        assert!(outs[0].returned_files.get(2).is_none());
    }

    #[test]
    fn adopted_file_set_for_unranked_structural_adoption() {
        // Structural (unranked) adoptions previously yielded adopted_file = null because
        // it was derived from adopted_rank (always None here). Now it is the matched path.
        let events = vec![
            cg(
                "get_call_graph",
                &[("src/graph.rs", None), ("src/storage.rs", None)],
            ),
            touch("/proj/src/storage.rs"),
        ];
        let outs = score_session(&events);
        assert!(outs[0].adopted);
        assert_eq!(outs[0].adopted_rank, None);
        assert_eq!(outs[0].adopted_file.as_deref(), Some("src/storage.rs"));
    }

    #[test]
    fn detect_cli_cg_call_across_newline_separated_commands() {
        // A newline is a command separator — the binary at the head of a later line must
        // be detected even though the previous token isn't a shell separator (`backend`).
        assert_eq!(
            detect_cli_cg_call("cd backend\ncode-graph-mcp callgraph Foo"),
            Some("callgraph")
        );
        assert_eq!(
            detect_cli_cg_call("set -e\nexport X=1\ncode-graph-mcp search \"q\""),
            Some("search")
        );
        // Mid-command mentions on their own line are still rejected.
        assert_eq!(
            detect_cli_cg_call("echo hi\necho code-graph-mcp grep foo"),
            None
        );
    }

    #[test]
    fn cli_call_captures_query_and_skips_flags() {
        // first positional after the subcommand is the query; quotes stripped, flags skipped
        assert_eq!(
            cli_call("code-graph-mcp search \"login flow\""),
            Some(("search", "login flow".to_string()))
        );
        assert_eq!(
            cli_call("code-graph-mcp grep -i pat src/"),
            Some(("grep", "pat".to_string()))
        );
        assert_eq!(
            cli_call("code-graph-mcp callgraph Foo"),
            Some(("callgraph", "Foo".to_string()))
        );
        assert_eq!(cli_call("code-graph-mcp stats"), None); // housekeeping → not a query call
                                                            // a quoted span containing the binary name is one token, never a bare invocation
        assert_eq!(
            cli_call("git commit -m \"tweak code-graph-mcp grep parsing\""),
            None
        );
    }

    #[test]
    fn parse_cli_search_captures_query_through_event() {
        // the ranked search_cli label must carry the real query, not an empty string
        let call = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b9","name":"Bash","input":{"command":"code-graph-mcp search \"login flow\""}}]}}"#;
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b9","content":[{"type":"text","text":"h3 a  src/auth.rs:1-2"}]}]}}"#;
        let p = parse_transcript(&format!("{call}\n{result}\n"));
        match &p.events[0] {
            Event::CgCall {
                tool,
                query,
                returned,
                ..
            } => {
                assert_eq!(tool, "search_cli");
                assert_eq!(query, "login flow");
                assert_eq!(returned[0].rank, Some(0));
            }
            _ => panic!("expected search_cli CgCall"),
        }
    }

    #[test]
    fn run_outcome_populates_window_and_since() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("s.jsonl")).unwrap();
        let a = r#"{"type":"assistant","timestamp":"2026-06-01T08:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"mcp__code-graph-dev__semantic_code_search","input":{"query":"q"}}]}}"#;
        let r = r#"{"type":"user","timestamp":"2026-06-01T08:00:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"[{\"file_path\":\"src/a.rs\"}]"}]}]}}"#;
        writeln!(f, "{a}\n{r}").unwrap();
        let (s, _) = run_outcome(dir.path(), Some(3650)); // wide --since so the fresh temp file is in-window
        assert_eq!(s.first_ts.as_deref(), Some("2026-06-01T08:00:00Z"));
        assert_eq!(s.last_ts.as_deref(), Some("2026-06-01T08:00:05Z"));
        assert_eq!(s.since_days, Some(3650));
        assert_eq!(s.sessions, 1);
    }
}
