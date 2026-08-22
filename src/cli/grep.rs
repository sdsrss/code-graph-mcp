use super::*;

/// CLI arguments for the `grep` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp grep",
    about = "AST-context grep (ripgrep + containing function/class)"
)]
pub struct GrepArgs {
    /// Search pattern (ripgrep regex; use -F for literal strings)
    #[arg(allow_hyphen_values = true)]
    pub pattern: String,
    /// Set by [`parse_grep_args`] when the caller wrote an explicit `--`.
    ///
    /// Not a flag — clap skips it. It exists so the flag-shaped-pattern hint can
    /// stay silent for someone who has already said, in the only way the CLI
    /// offers, that they meant the literal: telling them to add the `--` they
    /// just typed is worse than saying nothing.
    #[arg(skip)]
    pub had_literal_separator: bool,
    /// Optional paths to restrict the search (must be within the project root)
    pub paths: Vec<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Case-insensitive search
    #[arg(short = 'i', long)]
    pub ignore_case: bool,
    /// Only match whole words
    #[arg(short = 'w', long)]
    pub word_regexp: bool,
    /// Treat the pattern as a literal string, not a regex
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,
    /// Print only the names of files with matches
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,
    /// Show N lines before and after each match
    #[arg(short = 'C', long, value_name = "N")]
    pub context: Option<u64>,
    /// Show N lines after each match
    #[arg(short = 'A', long, value_name = "N")]
    pub after_context: Option<u64>,
    /// Show N lines before each match
    #[arg(short = 'B', long, value_name = "N")]
    pub before_context: Option<u64>,
    /// Max matches per file; 0 = unlimited
    #[arg(short = 'm', long, value_name = "N", default_value_t = 100)]
    pub max_count: u64,
    /// Truncate displayed lines to N chars; 0 = unlimited (default 512 — keeps a
    /// long minified/generated line from flooding output).
    #[arg(
        short = 'M',
        long = "max-columns",
        value_name = "N",
        default_value_t = 512
    )]
    pub max_columns: u64,
    /// Print only a count of matching lines per file (the per-file cap is ignored)
    #[arg(short = 'c', long)]
    pub count: bool,
    /// Restrict to a ripgrep file type (e.g. rust, py, js, ts, go)
    #[arg(short = 't', long = "type", value_name = "TYPE")]
    pub file_type: Option<String>,
    /// Only search files matching this glob; repeatable; prefix `!` to exclude
    #[arg(short = 'g', long = "glob", value_name = "GLOB")]
    pub glob: Vec<String>,
    /// Accepted for grep parity; line numbers are always printed (no-op).
    #[arg(short = 'n', long = "line-number")]
    pub line_number: bool,
    /// Accepted for grep parity; the search is always recursive (no-op).
    #[arg(short = 'r', long = "recursive", visible_short_alias = 'R')]
    pub recursive: bool,
    /// Accepted for grep parity; filenames are always shown (no-op).
    #[arg(short = 'H', long = "with-filename")]
    pub with_filename: bool,
}

/// Split attached short-option context forms (`-A2` → `-A`, `2`; bundled
/// `-nA2` → `-nA`, `2`) so the `grep` subcommand accepts grep/ripgrep's attached
/// numeric syntax.
///
/// The `pattern` positional carries `allow_hyphen_values` so a flag-shaped
/// search term (e.g. `--no-default-features`) is searchable without a `--`
/// escape. The side effect: clap binds an attached short value like `-A2` —
/// which is not an *exact* registered token — to the positional as the pattern
/// instead of parsing `-A` with value `2`, leaving the real pattern to be
/// misrouted into the path list (rg then errors "No such file"). Splitting the
/// digits into a separate token makes `-A`/`-B`/`-C` exact tokens again (and a
/// bundle like `-nA2` becomes `-nA 2`, which clap parses as `-n -A=2`).
///
/// Stops at the first `--` so an intentional literal `-A2` *pattern* after the
/// separator (`grep -- -A2`) is preserved verbatim.
pub fn normalize_grep_argv(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut after_sep = false;
    for a in args {
        if after_sep || a == "--" {
            after_sep = after_sep || a == "--";
            out.push(a);
            continue;
        }
        if let Some((cluster, digits)) = split_attached_context(&a) {
            out.push(cluster);
            out.push(digits);
            continue;
        }
        out.push(a);
    }
    out
}

/// If `tok` is a single-dash short-flag cluster ending in an attached value —
/// `-A2`, `-C10`, `-m5`, or a bundle like `-nA2`/`-niB3` (leading boolean shorts,
/// then a value flag `-A`/`-B`/`-C`/`-m`/`-M`, then digits) — return
/// `(cluster_without_digits, digits)`. Returns `None` otherwise (incl. `--long`,
/// bare `-A`, `-z2`, `-A2x`, and `-A2B3` where a value flag is not last in the
/// bundle).
///
/// grep and ripgrep both accept these attached forms, and the value flag is only
/// ever last in a bundle (`-nA2` valid; `-A2n`/`-An2` rejected by real grep), so
/// we peel a trailing `[ABCmM][0-9]+` run that sits after a run of ASCII-letter
/// shorts. clap then bundle-parses the cluster (`-nA` → `-n -A`) and the bare
/// `-A`/`-B`/`-C`/`-m`/`-M` takes the now-separate digit token as its value.
pub(crate) fn split_attached_context(tok: &str) -> Option<(String, String)> {
    let b = tok.as_bytes();
    // single dash, not `--`, at least `-X0` (a flag char + ≥1 digit).
    if b.len() < 3 || b[0] != b'-' || b[1] == b'-' {
        return None;
    }
    // Start of the trailing ASCII-digit run (one past the last non-digit byte).
    let digit_start = b.iter().rposition(|&c| !c.is_ascii_digit())? + 1;
    if digit_start == b.len() {
        return None; // no trailing digits (e.g. `-A2x`, `-nr`)
    }
    // The byte immediately before the digits must be a value-taking flag, and
    // everything between `-` and the digits must be ASCII letters (the leading
    // boolean shorts + that value flag). Rejects `-A2B3`, `-z2`, `-2`.
    if !matches!(b[digit_start - 1], b'A' | b'B' | b'C' | b'm' | b'M')
        || !b[1..digit_start].iter().all(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    Some((
        tok[..digit_start].to_string(),
        tok[digit_start..].to_string(),
    ))
}

/// Return the first single-dash short-flag cluster (pre-`--`) that contains a
/// flag the `grep` subcommand does not implement — e.g. `-v`, `-c`, `-o`, `-e`,
/// `-P`. The pattern positional's `allow_hyphen_values` would otherwise swallow
/// such a flag AS the search term, pushing the real pattern into the path list →
/// a cryptic `rg: No such file or directory: <pattern>` (same failure class the
/// `-A2`/`-n` parity fixes addressed). Surfacing it lets the caller emit a clear
/// "unsupported flag" message instead.
///
/// Only clusters starting with an ASCII letter are flag candidates; `--long`
/// tokens, bare `-`, and dash-then-symbol/digit terms (`->`, `-1`, `-.*`) are
/// legitimate searchable patterns and are left for the positional. A value-taking
/// short (`-A`/`-B`/`-C`/`-m`/`-M`) consumes the rest of the cluster, so judging stops
/// there. Scanning stops at the first `--` so `grep -- -v` searches the literal.
pub(crate) fn first_unsupported_grep_flag(args: &[String]) -> Option<String> {
    const BOOL_SHORTS: &[u8] = b"iwFlnrRHhc"; // supported value-less shorts (+ -h help)
    const VALUE_SHORTS: &[u8] = b"ABCmMtg"; // shorts that take a value (consume the tail)
    for a in args {
        if a == "--" {
            break;
        }
        let b = a.as_bytes();
        if b.len() < 2 || b[0] != b'-' || !b[1].is_ascii_alphabetic() {
            continue;
        }
        let mut i = 1;
        let mut bad = false;
        while i < b.len() {
            let c = b[i];
            if VALUE_SHORTS.contains(&c) {
                break; // value short eats the remainder (attached or next token)
            }
            if !BOOL_SHORTS.contains(&c) {
                bad = true;
                break;
            }
            i += 1;
        }
        if bad {
            return Some(a.clone());
        }
    }
    None
}

/// Explain a `rg: <path>: No such file or directory` that was really caused by a
/// long flag being consumed as the search pattern.
///
/// Returns `None` unless BOTH hold: the pattern looks like a long flag
/// (`--word`), and ripgrep's complaint is a missing path. That pairing is what
/// separates "the user typed `--quiet` expecting a flag" from "the user is
/// genuinely searching for the literal `--quiet`" — the latter finds files (or
/// reports no match) rather than erroring on a missing path.
///
/// Advisory only. Changing `grep --quiet` from "search for that literal" to "flag
/// error" would be a behavior change on a published CLI surface, so this reports
/// rather than decides.
pub(crate) fn grep_flaglike_pattern_hint(
    pattern: &str,
    stderr: &str,
    had_literal_separator: bool,
) -> Option<String> {
    // Someone who wrote `--` has already told us they meant the literal. Their
    // missing path is their own typo'd path argument, not a displaced pattern,
    // and the hint would advise adding the separator they just used.
    if had_literal_separator {
        return None;
    }
    let looks_like_long_flag = pattern.starts_with("--")
        && pattern.len() > 2
        && pattern[2..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !looks_like_long_flag || !stderr.contains("No such file or directory") {
        return None;
    }
    Some(format!(
        "[code-graph] note: `{pattern}` was taken as the SEARCH PATTERN, not as a \
         flag — `grep` accepts a flag-shaped pattern so terms like \
         `--no-default-features` are searchable. Your real pattern was then read \
         as a path, which is the error above. If you meant a flag, `grep` does \
         not implement `{pattern}`; if you meant the literal, write \
         `grep -- {pattern}`."
    ))
}

/// Parse `grep` arguments from the full process argv (including argv\[0]),
/// applying [`normalize_grep_argv`] first. Mirrors the other subcommands'
/// `skip(1)`; clap consumes the leading `grep` token as the binary-name slot.
///
/// Rejects unsupported short flags ([`first_unsupported_grep_flag`]) up front so
/// they fail with a clear message instead of being swallowed as the pattern.
pub fn parse_grep_args(argv: &[String]) -> GrepArgs {
    let raw: Vec<String> = argv.iter().skip(1).cloned().collect();
    // Position matters. `grep -- --quiet foo` means "search for the literal";
    // `grep --quiet -- foo` is someone who typo'd a flag and then wrote a
    // separator for their PATH, and they should still get the hint. Only a
    // separator standing BEFORE the pattern is the "I meant the literal" signal.
    let separator_at = raw.iter().position(|a| a == "--");
    if let Some(bad) = first_unsupported_grep_flag(&raw) {
        // --json early-bail must still emit an empty array (CLI JSON contract).
        let json = raw
            .iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a.as_str() == "--json");
        let unsupported = format!(
            "unsupported flag: {bad}. Supported: -i -w -F -l -c -A -B -C -m -M -t -g \
             (and no-op -n/-r/-R/-H). To search a literal flag-shaped string, put it \
             after --: code-graph-mcp grep -- {bad}"
        );
        emit_grep_json_error(json, &unsupported);
        eprintln!(
            "[code-graph] unsupported flag: {bad}. Supported: -i -w -F -l -c -A -B -C -m -M -t -g \
             (and no-op -n/-r/-R/-H). To search a literal flag-shaped string, put it \
             after --: code-graph-mcp grep -- {bad}"
        );
        grep_exit(2);
    }
    // Element 0 is clap's program name. `raw` starts with the bare subcommand
    // token, which rendered `Usage: grep <PATTERN>` — not a runnable command, and
    // it overrode this struct's own `name = "code-graph-mcp grep"`. Only the copy
    // handed to clap is renamed: `raw`'s indices drive the separator / flag
    // scanning above.
    let mut for_clap = raw.clone();
    if for_clap.is_empty() {
        for_clap.push("code-graph-mcp grep".to_string());
    } else {
        for_clap[0] = "code-graph-mcp grep".to_string();
    }
    let mut parsed = GrepArgs::parse_from(normalize_grep_argv(for_clap));
    // True exactly when the pattern's own slot sits after the `--`. Search only
    // the tail: scanning from index 0 found the FIRST token equal to the pattern
    // string, which may be an earlier flag's VALUE. `grep -g '--quiet' -- --quiet`
    // computed pat=1 against sep=2, concluded "no separator", and the hint told
    // the user to write `grep -- --quiet` — which is what they had just typed.
    // (It also mishandled `grep -- --`, where sep == pat.)
    parsed.had_literal_separator = match separator_at {
        Some(sep) => raw.iter().skip(sep + 1).any(|a| *a == parsed.pattern),
        None => false,
    };
    parsed
}

/// AST-context grep: ripgrep + AST context from index.
///
/// Output format:
/// ```text
/// src/mcp/server.rs:142  let result = handle_request(params);
///   → fn McpServer::process_message (lines 130-180)
/// ```
/// grep-parity exit codes (v0.50): 0 = matched, 1 = no match, 2 = error/usage.
/// Flushes stdout before exiting so piped consumers see complete output.
pub(crate) fn grep_exit(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

/// Emit grep's `--json` FAILURE shape: an error object, never the success-shaped
/// empty array.
///
/// Every error leg used to print `[]`, which is byte-identical to a genuine
/// zero-match run. Exit codes carried the whole distinction, so the very common
/// `code-graph-mcp grep … --json 2>/dev/null` — a shape the agent-facing docs
/// themselves suggest — reported "no matches in this repo" for an unsupported
/// flag, a path outside the project, a missing ripgrep, or an invalid pattern
/// (2026-08-16 audit §四). Object vs array is also what makes it detectable
/// without reading the exit code: `JSON.parse(out).length` is `undefined`, not 0.
///
/// The genuine no-match legs keep the bare `[]` — that is the Tier-1 empty
/// contract and it is correct there.
fn emit_grep_json_error(json_mode: bool, message: &str) {
    if json_mode {
        println!("{}", serde_json::json!({ "error": message }));
    }
}

/// GNU BRE inverts escaping for its operators: `\|` `\(` `\)` `\{` `\}` `\+`
/// `\?` mean alternation/grouping/repetition, and the UNESCAPED forms are
/// literals. ripgrep's Rust regex dialect is the other way around, so a
/// grep-muscle-memory pattern like `protocol\|proto` silently becomes the
/// literal string "protocol|proto" and zero-hits — an LLM consumer then
/// concludes "no such code". Returns the escapes present so the no-match path
/// can disclose the dialect.
pub(crate) fn bre_style_escapes(pattern: &str) -> Vec<&'static str> {
    ["\\|", "\\(", "\\)", "\\{", "\\}", "\\+", "\\?"]
        .into_iter()
        .filter(|e| pattern.contains(e))
        .collect()
}

/// What to say when spawning `rg` fails with `ErrorKind::NotFound`.
///
/// Two different causes produce that one error kind, because `current_dir` is
/// applied as part of the spawn: the `rg` binary is missing from PATH, or the
/// working directory does not exist (an index whose project root was moved or
/// deleted, a stale worktree). The message used to name only the first, sending
/// the user to install a tool they already have.
pub(crate) fn rg_spawn_failure_message(project_root: &Path) -> String {
    if !project_root.is_dir() {
        format!(
            "cannot run ripgrep: working directory {} does not exist — \
             the project root recorded for this index is gone (moved or deleted)",
            project_root.display()
        )
    } else {
        "ripgrep (rg) not found. Install: https://github.com/BurntSushi/ripgrep".to_string()
    }
}

/// Shared zero-hit note for every grep mode. Skips the dialect hint under -F,
/// where backslashes are genuinely literal and the pattern means what it says.
pub(crate) fn emit_no_match(pattern: &str, fixed_strings: bool) {
    eprintln!("[code-graph] No matches for: {}", pattern);
    if fixed_strings {
        return;
    }
    let escapes = bre_style_escapes(pattern);
    if !escapes.is_empty() {
        eprintln!(
            "[code-graph] hint: pattern contains BRE-style escapes ({}) — this grep uses ripgrep's Rust regex dialect, where those match the literal character; write the operator unescaped (GNU `a\\|b` → `a|b`)",
            escapes.join(" ")
        );
    }
}

/// git-tracked files that ripgrep's walk skips: tracked ∖ `rg --files`.
/// Three blind-spot classes share this root cause (rg prunes by its own
/// ignore/hidden rules without checking tracked status):
///   1. tracked file under a gitignored dir (`docs/` ignored, doc force-added)
///   2. `dir/` + `!dir/keep/` negation — git whitelists the file, rg prunes
///      `dir/` during the walk before evaluating the negation (rg 14.x)
///   3. tracked hidden files (rg skips hidden by default)
///
/// Passing the difference as explicit file args restores `git grep` semantics.
/// Empty when git is absent / not a work tree (then rg's walk is the answer).
/// `scope_rels` (relative, validated) restricts both sides to the user paths.
pub(crate) fn tracked_files_missed_by_walk(
    project_root: &Path,
    scope_rels: &[String],
) -> Vec<String> {
    let mut ls = Command::new("git");
    ls.args(["ls-files", "-z"]).current_dir(project_root);
    // `--` so a scope path that starts with `-` reaches git as a pathspec, not a flag.
    ls.arg("--");
    for rel in scope_rels {
        ls.arg(rel);
    }
    let Ok(out) = ls.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let tracked: Vec<String> = out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| String::from_utf8(s.to_vec()).ok())
        .collect();
    if tracked.is_empty() {
        return Vec::new();
    }

    // The same walk the search performs (cwd-relative output).
    let mut rg_files = Command::new("rg");
    rg_files.arg("--files").current_dir(project_root);
    // `--` so a scope path that starts with `-` reaches rg as a path, not a flag.
    rg_files.arg("--");
    for rel in scope_rels {
        rg_files.arg(rel);
    }
    // `rg --files` emits NATIVE separators (`src\foo.rs` on Windows) while
    // `git ls-files` always emits `/`. Comparing the two spellings directly made
    // `walked.contains(t)` miss EVERY file on Windows, so the "supplement" became
    // the entire tracked set — 3,284 absolute paths appended to one argv, which
    // is the `os error 206` (command line > 32 KB) in issue #34, and the source of
    // the duplicated matches (each file scanned once by the walk and once as an
    // explicit arg). Normalize both sides to the `/` form.
    let walked: std::collections::HashSet<String> = match rg_files.output() {
        // rg --files exits 1 with empty stdout when the walk finds nothing —
        // same parse either way; only spawn failure disables the supplement.
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(normalize_path_display)
            .map(|l| l.trim_start_matches("./").to_string())
            .collect(),
        Err(_) => return Vec::new(),
    };

    tracked
        .into_iter()
        .filter(|t| !walked.contains(&normalize_path_display(t)))
        .collect()
}

pub fn cmd_grep(project_root: &Path, args: GrepArgs) -> Result<()> {
    let GrepArgs {
        pattern,
        had_literal_separator,
        paths,
        json: json_mode,
        ignore_case,
        word_regexp,
        fixed_strings,
        max_count,
        files_with_matches,
        context,
        after_context,
        before_context,
        max_columns,
        count: count_mode,
        file_type,
        glob,
        // -n/-r/-R/-H: accepted for grep muscle-memory parity, all no-ops here
        // (line numbers, recursion, and filenames are already the default).
        line_number: _,
        recursive: _,
        with_filename: _,
    } = args;
    let context_requested =
        context.is_some() || after_context.is_some() || before_context.is_some();
    // clap accepts an empty-string positional (e.g. an unset shell var expanding
    // to ""); preserve the non-empty guard + Usage string. Usage error → exit 2.
    if pattern.is_empty() {
        emit_grep_json_error(
            json_mode,
            "no pattern given. Usage: code-graph-mcp grep <pattern> [paths...]",
        );
        eprintln!("Usage: code-graph-mcp grep <pattern> [paths...] [-i] [-w] [-F] [-c] [-t TYPE] [-g GLOB] [-m N] [-M N] [--json]");
        grep_exit(2);
    }

    let root_canonical = project_root
        .canonicalize()
        .unwrap_or(project_root.to_path_buf());
    // Relative search paths resolve against the caller's cwd (like ripgrep/grep),
    // so `grep foo parser` from `src/` searches `src/parser`. Hooks and agents
    // spawn the binary with cwd==root, where `cwd.join` equals the historical
    // `project_root.join`; the canonicalize + starts_with(root) guard below still
    // rejects any path that escapes the project. Absolute paths ignore cwd.
    let cwd = std::env::current_dir().unwrap_or_else(|_| project_root.to_path_buf());

    // Validate every search path is within the project root (path traversal guard).
    let mut search_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut search_rels: Vec<String> = Vec::new();
    for path in &paths {
        let resolved = cwd.join(path);
        let canonical = match resolved.canonicalize() {
            Ok(c) => c,
            // Near-miss rebase (field failure 2026-07-24): the agent's shell often
            // sits in a subdir while it quotes repo-root-relative paths (the deny
            // hook displays them root-relative), so cwd.join doubles the prefix —
            // rg got `<root>/<sub>/<sub>/…` and exited 2 with a cryptic "No such
            // file". cwd-missing + root-existing is unambiguous: take the root
            // reading and say so on stderr. Paths that exist under cwd never reach
            // this arm, so grep's cwd-relative parity is untouched; the rebased
            // path still passes the starts_with(root) traversal guard below.
            // `.`, `./x` and `../x` are explicitly cwd-anchored and never rebase
            // (same rule as normalize_user_path_from) — they fall through to the
            // rg "No such file" error, matching what rg itself would say.
            Err(_) => match root_canonical.join(path).canonicalize() {
                Ok(c) if Path::new(path).is_relative() && !is_cwd_anchored(path) => {
                    note_root_rebase(path, &root_canonical);
                    c
                }
                _ => resolved,
            },
        };
        // Dual-root: `canonical` is only LEXICAL for a nonexistent path (the
        // canonicalize above failed), and on Windows a short-name cwd join can
        // never lexically start_with the long-form canonical root. Accept the
        // raw-root spelling too — the path then flows to rg, which reports the
        // honest "No such file" / partial error instead of a bogus traversal
        // rejection (windows CI leg, first lit in v0.112.0).
        if !canonical.starts_with(&root_canonical) && !canonical.starts_with(project_root) {
            emit_grep_json_error(
                json_mode,
                &format!("search path must be within project root: {path}"),
            );
            eprintln!(
                "[code-graph] search path must be within project root: {}",
                path
            );
            grep_exit(2);
        }
        if let Ok(rel) = canonical
            .strip_prefix(&root_canonical)
            .or_else(|_| canonical.strip_prefix(project_root))
        {
            // `/`-separated: these go out as `git ls-files` / `rg --files`
            // pathspecs (git pathspecs are always `/`) and are compared against
            // relativized output rows in `-c` mode, which is also `/`.
            search_rels.push(normalize_path_display(&rel.to_string_lossy()));
        }
        search_paths.push(canonical);
    }

    // Flags + pattern, WITHOUT the path operands: paths are appended per batch by
    // `run_rg` below, because one argv cannot hold an unbounded supplement list
    // (Windows caps a command line at ~32 KB — issue #34's `os error 206`).
    let mut rg_args: Vec<std::ffi::OsString> = Vec::new();
    macro_rules! rg_arg {
        ($v:expr) => {
            rg_args.push(std::ffi::OsString::from($v))
        };
    }
    // Determinism note: ripgrep parallelizes the walk and emits files in
    // worker-completion order, so the same grep shuffled every run (observed up to
    // 8/8 distinct) — the determinism class fixed for the graph commands in v0.85.x.
    // We do NOT use `rg --sort path`: it only orders WITHIN each traversal root and
    // preserves the given order of top-level path args, so the supplement (explicit
    // trailing file args, below) and multi-path input would stay unsorted; it also
    // disables rg's parallelism and requires rg >= 11. Instead each mode below sorts
    // the collected result set by path — a true global ascending order that keeps rg
    // parallel and imposes no rg version floor.
    if files_with_matches {
        // -l: plain one-path-per-line output (rg stops at the first match per
        // file); context flags are meaningless here, like grep, and ignored.
        rg_arg!("-l");
    } else if count_mode {
        // -c: ripgrep --count prints `path:N` (matching LINES per file, listing
        // only files with ≥1 match). The per-file --max-count cap is intentionally
        // NOT applied so the count is exhaustive; context flags don't apply.
        // --with-filename forces the `path:` prefix even for a single file (rg
        // omits it otherwise, like `grep -c`), so the `path:N` parse is uniform.
        rg_arg!("--count");
        rg_arg!("--with-filename");
    } else {
        rg_arg!("--json");
        rg_arg!("-n");
        if let Some(n) = context {
            rg_arg!(format!("--context={}", n));
        }
        if let Some(n) = after_context {
            rg_arg!(format!("--after-context={}", n));
        }
        if let Some(n) = before_context {
            rg_arg!(format!("--before-context={}", n));
        }
        if max_count > 0 {
            rg_arg!(format!("--max-count={}", max_count));
        }
    }
    if ignore_case {
        rg_arg!("-i");
    }
    if word_regexp {
        rg_arg!("-w");
    }
    if fixed_strings {
        rg_arg!("-F");
    }
    // Scope filters (apply to every mode): --type by language, --glob by path.
    // rg validates a --type name and errors (exit 2) on an unknown one, surfaced
    // like any other rg error below.
    if let Some(ref t) = file_type {
        rg_arg!("--type");
        rg_arg!(t);
    }
    for g in &glob {
        rg_arg!("--glob");
        rg_arg!(g);
    }
    // `--` so leading-dash patterns (e.g. searching for "--no-default-features")
    // reach rg as the pattern instead of being parsed as flags.
    rg_arg!("--");
    rg_arg!(&pattern);

    // Walk operands: the user's paths, or the whole root.
    let mut walk_operands: Vec<std::ffi::OsString> = Vec::new();
    if search_paths.is_empty() {
        // root_canonical, not the raw root: rg echoes back the spelling it was
        // handed, and the raw root can be an 8.3 short name on Windows while
        // every explicit search path above is canonicalized long-form. One
        // spelling for everything keeps the relativize below single-rooted for
        // the common case (dual-root fallback covers the rest).
        walk_operands.push(root_canonical.clone().into());
    } else {
        for p in &search_paths {
            walk_operands.push(p.into());
        }
    }

    // git-grep parity: append tracked files the rg walk misses as explicit
    // args (explicit file args bypass rg's ignore rules). git ls-files
    // pathspecs + rg --files args are both scoped to the user's paths, so the
    // supplement honors path restrictions; files passed explicitly by the
    // user appear in the walk output and dedup naturally.
    // Bounds the number of extra rg spawns (each batch is one), not the argv
    // length — that is `ARGV_PATH_BUDGET` below. Raised from 500 in v0.105.x:
    // 500 was a proxy for the command-line limit and silently dropped tracked
    // files (a "no matches" that had matches), which the batching makes
    // unnecessary. Reaching this cap is still reported on stderr.
    const SUPPLEMENT_CAP: usize = 20_000;
    let mut supplement = tracked_files_missed_by_walk(project_root, &search_rels);
    // rg does NOT apply --type/--glob to files passed explicitly on the command
    // line, so the supplement (appended as explicit args below) would leak files
    // the -t/-g filters should exclude. Re-apply the same filters here via
    // ripgrep's own `ignore` crate matchers (rg-identical) before appending.
    if file_type.is_some() || !glob.is_empty() {
        let types = file_type.as_deref().and_then(|t| {
            let mut b = ignore::types::TypesBuilder::new();
            b.add_defaults();
            b.select(t);
            b.build().ok()
        });
        let overrides = if glob.is_empty() {
            None
        } else {
            let mut b = ignore::overrides::OverrideBuilder::new(project_root);
            let ok = glob.iter().all(|g| b.add(g).is_ok());
            if ok {
                b.build().ok()
            } else {
                None
            }
        };
        supplement.retain(|rel| {
            let p = Path::new(rel);
            if let Some(t) = &types {
                if t.matched(p, false).is_ignore() {
                    return false;
                }
            }
            if let Some(ov) = &overrides {
                if ov.matched(p, false).is_ignore() {
                    return false;
                }
            }
            true
        });
    }
    if supplement.len() > SUPPLEMENT_CAP {
        eprintln!(
            "[code-graph] {} tracked files outside the rg walk; searching the first {} only",
            supplement.len(),
            SUPPLEMENT_CAP
        );
        supplement.truncate(SUPPLEMENT_CAP);
    }
    // Supplement operands are RELATIVE (resolved by rg against `current_dir`,
    // set to the project root below). Absolute ones cost the repeated root
    // prefix on every entry — ~40 chars × 500 ≈ 20 KB of pure prefix on the
    // reporter's layout — which is what pushed the argv past Windows' 32 KB
    // limit. rg echoes the operand back verbatim, and `relativize_path`
    // normalizes both spellings to the same root-relative form, so walk and
    // supplement results still dedup against each other.
    let supplement_operands: Vec<std::ffi::OsString> = supplement
        .iter()
        .filter(|rel| project_root.join(rel).is_file())
        .map(std::ffi::OsString::from)
        .collect();

    // Windows caps a whole command line at 32,767 chars; POSIX ARG_MAX is
    // ~2 MB. Budget the PATH operands conservatively under each, leaving room
    // for the flags, the pattern, and the exe path.
    //
    // The accounting is `len + 1` per operand — one separator, no quoting. On
    // Windows an operand containing a space is quoted by the runtime, costing 2
    // more chars, so the true line is longer than the budget believes. The
    // headroom absorbs it: 32,767 − 24,000 = 8,767 chars would need ~4,400
    // space-bearing paths in a single batch to exhaust, and `SUPPLEMENT_CAP`
    // stops at 500. Stated rather than fixed because a tighter budget is the
    // wrong trade — it would split batches on every repo to cover a case the cap
    // makes unreachable.
    const ARGV_PATH_BUDGET: usize = if cfg!(windows) { 24_000 } else { 512_000 };
    // Override exists so the batching path is testable without materializing a
    // 32 KB argv, and as an escape hatch for a shell with a tighter limit.
    let argv_budget = std::env::var("CODE_GRAPH_RG_ARGV_BUDGET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(ARGV_PATH_BUDGET);
    let flags_len: usize = rg_args.iter().map(|a| a.len() + 1).sum();
    let path_budget = argv_budget.saturating_sub(flags_len).max(1);

    let run_rg = |paths: &[std::ffi::OsString]| -> std::io::Result<std::process::Output> {
        let mut cmd = Command::new("rg");
        cmd.current_dir(project_root);
        cmd.args(&rg_args);
        cmd.args(paths);
        cmd.output()
    };
    // `current_dir` is part of the spawn: a missing working directory fails with
    // the SAME ErrorKind::NotFound the missing-binary case does, so the error arm
    // has to tell them apart before it names a cause. See
    // `rg_spawn_failure_message`.

    // Batch 1 is always the walk; the supplement follows in argv-sized chunks.
    // Each batch is an independent rg run whose stdout is concatenated — every
    // consumer below (rg --json records, `-l` lines, `-c` rows) is line- or
    // record-oriented and already sorts + dedups the merged set globally.
    let mut batches: Vec<&[std::ffi::OsString]> = vec![&walk_operands];
    {
        let mut start = 0usize;
        while start < supplement_operands.len() {
            let mut end = start;
            let mut used = 0usize;
            while end < supplement_operands.len() {
                let next = supplement_operands[end].len() + 1;
                if end > start && used + next > path_budget {
                    break;
                }
                used += next;
                end += 1;
            }
            batches.push(&supplement_operands[start..end]);
            start = end;
        }
    }

    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();
    // Merged exit code, worst-first: 2 (error) > 0 (matched) > 1 (no match).
    let mut merged_code: i32 = 1;
    for batch in batches {
        let output = match run_rg(batch) {
            Ok(output) => output,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let msg = rg_spawn_failure_message(project_root);
                emit_grep_json_error(json_mode, &msg);
                eprintln!("[code-graph] {}", msg);
                grep_exit(2);
            }
            Err(e) => return Err(e.into()),
        };
        stdout_buf.extend_from_slice(&output.stdout);
        stderr_buf.extend_from_slice(&output.stderr);
        merged_code = match output.status.code() {
            Some(2) => 2,
            Some(0) if merged_code != 2 => 0,
            _ => merged_code,
        };
    }
    let rg_output = RgRun {
        stdout: stdout_buf,
        stderr: stderr_buf,
        code: merged_code,
    };

    // ripgrep exit codes: 0 = matched, 1 = no match, 2 = error (invalid regex,
    // unreadable path). grep-parity: surface as exit 2 — a regex parse error
    // (e.g. an unescaped `(` in `res.json(`) must not look like a no-match.
    // An error with NON-empty stdout (e.g. one of several paths missing) still
    // carries matches from the readable paths — GNU grep prints those and exits
    // 2; discarding them here turned a one-bad-path multi-path grep into a
    // silent exit 2. Deliver the partial results and keep the exit code.
    let mut partial_error = false;
    if rg_output.code == 2 {
        let stderr = String::from_utf8_lossy(&rg_output.stderr);
        let stderr = stderr.trim();
        // A long flag this subcommand does not implement is bound to the PATTERN
        // positional (`allow_hyphen_values`, so that `grep --no-default-features`
        // can search for that literal), which pushes the real pattern into the
        // path list. rg then reports the user's search term as a missing file —
        // technically accurate and completely opaque. `first_unsupported_grep_flag`
        // catches this for short clusters but deliberately leaves `--long` tokens
        // alone, because treating them as flags would break the documented
        // literal search. So: keep the behavior, explain it.
        if let Some(hint) = grep_flaglike_pattern_hint(&pattern, stderr, had_literal_separator) {
            eprintln!("{hint}");
            // Short, distinct text for the log — matching the freshness-partial
            // site below. Repeating the full hint verbatim printed it twice on
            // any run that DOES have a subscriber installed.
            tracing::warn!("grep: flag-shaped pattern taken as pattern: {}", pattern);
        }
        if rg_output.stdout.is_empty() {
            let detail = if stderr.is_empty() {
                "invalid pattern or unreadable path"
            } else {
                stderr
            };
            emit_grep_json_error(json_mode, &format!("ripgrep error: {detail}"));
            eprintln!("[code-graph] ripgrep error: {detail}");
            grep_exit(2);
        }
        eprintln!(
            "[code-graph] ripgrep error (results below cover the remaining paths): {}",
            if stderr.is_empty() {
                "unreadable path"
            } else {
                stderr
            }
        );
        partial_error = true;
    }

    // -l mode: rg already printed one path per line; relativize and pass through.
    if files_with_matches {
        // Dual-root relativize: rg rows echo whichever spelling the operand
        // carried — canonical long-form for explicit/walk paths, but a raw
        // (possibly 8.3-short on Windows) spelling can still appear for paths
        // that never canonicalized. Try canonical first, raw second; a single
        // lexical root can never equate the two spellings (first surfaced when
        // CI gained ripgrep and the grep tests actually RAN on windows-latest).
        let root_str = root_canonical.to_string_lossy().into_owned();
        let root_raw_str = project_root.to_string_lossy().into_owned();
        let mut files: Vec<String> = String::from_utf8_lossy(&rg_output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| relativize_path_dual(l, &root_str, &root_raw_str))
            .collect();
        files.sort(); // global ascending-path order (walk + supplement + multi-path)
        files.dedup(); // overlapping/repeated path args can list one file twice
        if files.is_empty() {
            if json_mode {
                println!("[]");
            }
            emit_no_match(&pattern, fixed_strings);
            grep_exit(1);
        }
        let write_result: std::io::Result<()> = (|| {
            let mut stdout = std::io::stdout().lock();
            if json_mode {
                let serialized = serde_json::to_string(&files).unwrap_or_else(|_| "[]".to_string());
                writeln!(stdout, "{}", serialized)?;
            } else {
                for f in &files {
                    writeln!(stdout, "{}", f)?;
                }
            }
            Ok(())
        })();
        match write_result {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => grep_exit(0),
            other => other?,
        }
        if partial_error {
            grep_exit(2);
        }
        return Ok(());
    }

    // -c mode: rg --count printed `path:N` per file with a match; relativize and
    // pass through. No AST annotation (like -l); the count is exhaustive.
    if count_mode {
        // Same dual-root rationale as the -l branch above (8.3 vs long).
        let root_str = root_canonical.to_string_lossy().into_owned();
        let root_raw_str = project_root.to_string_lossy().into_owned();
        let mut counts: Vec<(String, u64)> = String::from_utf8_lossy(&rg_output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| {
                let (path, n) = l.rsplit_once(':')?;
                Some((
                    relativize_path_dual(path, &root_str, &root_raw_str),
                    n.trim().parse().ok()?,
                ))
            })
            .collect();
        // GNU parity: every explicitly named FILE arg gets a count row, zero
        // included (`grep -c pat f1 f2` prints `f2:0`). Scoped to file args —
        // enumerating a dir/repo walk's zero-match files would be
        // `--include-zero` noise at repo scale (deliberate GNU deviation).
        // `search_rels` entries share the root-relative shape of the
        // relativized rg rows, so the sort/dedup below treats them uniformly.
        for (p, rel) in search_paths.iter().zip(&search_rels) {
            if p.is_file() && !counts.iter().any(|(f, _)| f == rel) {
                counts.push((rel.clone(), 0));
            }
        }
        counts.sort_by(|a, b| a.0.cmp(&b.0)); // global ascending-path order
                                              // Overlapping/repeated path args make rg emit a file's `path:N` line once
                                              // per instance (identical count each); keep a single row per file.
        counts.dedup_by(|a, b| a.0 == b.0);
        if counts.is_empty() {
            if json_mode {
                println!("[]");
            }
            emit_no_match(&pattern, fixed_strings);
            grep_exit(1);
        }
        let total_matches: u64 = counts.iter().map(|(_, n)| n).sum();
        let write_result: std::io::Result<()> = (|| {
            let mut stdout = std::io::stdout().lock();
            if json_mode {
                let arr: Vec<_> = counts
                    .iter()
                    .map(|(f, n)| serde_json::json!({ "file": f, "count": n }))
                    .collect();
                let serialized = serde_json::to_string(&arr).unwrap_or_else(|_| "[]".to_string());
                writeln!(stdout, "{}", serialized)?;
            } else {
                for (f, n) in &counts {
                    writeln!(stdout, "{}:{}", f, n)?;
                }
            }
            Ok(())
        })();
        match write_result {
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => grep_exit(0),
            other => other?,
        }
        if partial_error {
            grep_exit(2);
        }
        if total_matches == 0 {
            // Zero rows printed (named files only) — still a no-match run:
            // grep parity exits 1 and the stderr note + dialect hint fire.
            emit_no_match(&pattern, fixed_strings);
            grep_exit(1);
        }
        return Ok(());
    }

    // Parse rg JSON output into matches
    let mut matches = parse_rg_json(&rg_output.stdout, &root_canonical, project_root);
    // Global ascending order by (path, line). rg already emits a file's lines in
    // order (sequential scan), so this only reorders ACROSS files (supplement /
    // multi-path); context lines carry their own line number and stay adjacent to
    // their match. Stable sort keeps any same-(file,line) records in input order.
    matches.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    // Overlapping or repeated path args (`grep pat . src/parser`, or the same
    // file passed twice) make rg scan a file more than once and emit identical
    // records; the global sort above makes those duplicates adjacent. Collapse
    // exact-identical rows so an accidental path overlap doesn't double every
    // match line (and its AST arrow / token cost). Done before the per-file cap
    // tally below so the count isn't inflated by the duplicates.
    matches.dedup_by(|a, b| {
        a.file == b.file && a.line == b.line && a.is_context == b.is_context && a.text == b.text
    });
    if matches.is_empty() {
        if json_mode {
            println!("[]");
        }
        // rg --json emits a trailing summary line even with zero matches, so an
        // error-only run (e.g. the single named path is missing) has non-empty
        // stdout and reaches here with partial_error set — that's an error (2),
        // not a no-match (1), and its stderr was already surfaced above.
        if partial_error {
            grep_exit(2);
        }
        // Surface ripgrep errors (e.g., path not found) instead of a silent exit
        let stderr = String::from_utf8_lossy(&rg_output.stderr);
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            eprintln!("[code-graph] {}", stderr);
        } else {
            emit_no_match(&pattern, fixed_strings);
        }
        // grep parity: no match exits 1.
        grep_exit(1);
    }

    // Per-file cap honesty: a file whose match count equals the cap was likely
    // truncated — silent truncation reads as "complete results" to the caller.
    // Context lines don't count toward the cap.
    let capped_files: Vec<&str> = if max_count > 0 {
        let mut counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for m in matches.iter().filter(|m| !m.is_context) {
            *counts.entry(m.file.as_str()).or_insert(0) += 1;
        }
        let mut capped: Vec<&str> = counts
            .iter()
            .filter(|(_, &c)| c >= max_count)
            .map(|(&f, _)| f)
            .collect();
        capped.sort_unstable();
        capped
    } else {
        Vec::new()
    };
    // Fast membership for the per-match `truncated` JSON marker below: stderr
    // alone is invisible to a `--json` consumer parsing stdout, so each match in
    // a file that hit the cap carries `"truncated": true`.
    let capped_set: std::collections::HashSet<&str> = capped_files.iter().copied().collect();

    // Try to open index for AST context; cache per-file nodes for both modes.
    let ctx = CliContext::try_open(project_root);
    if let Some(ref c) = ctx {
        // Annotation syncs below may write; never let a concurrent writer
        // (MCP server watcher, another index run) stall an interactive grep
        // for the default 5s busy_timeout — fail fast and mark stale instead.
        let _ = c.db.conn().execute_batch("PRAGMA busy_timeout = 250;");
    }
    // Query-time freshness (parity with the MCP file_path tools'
    // ensure_file_indexed, v0.18.0): before annotating from the index,
    // hash-compare each file about to be annotated and re-index the dirty ones
    // — bounded by a sync budget so a repo-wide grep over many dirty files
    // keeps its latency. Beyond budget (or on write contention) annotations
    // carry [stale].
    //
    // Done ONCE up front over the matched files rather than lazily per lookup:
    // `matches` is already fully materialized and sorted above, so the file set
    // is known, and one batched resync replaces up to `budget` separate
    // whole-graph index passes (audit 2026-08-22 P1-3). It also puts this
    // surface on the shared predicate instead of a third hand-written copy.
    let mut stale_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(ref c) = ctx {
        let annotated: Vec<String> = {
            let mut v: Vec<String> = matches.iter().map(|m| m.file.clone()).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let outcome = crate::indexer::resync::resync_stale_files(
            &c.db,
            &c.project_root,
            &annotated,
            // `CODE_GRAPH_GREP_SYNC_BUDGET` predates the unified knob and is
            // still honoured; `CODE_GRAPH_RESYNC_BUDGET` is the shared one.
            crate::indexer::resync::resync_budget_named("CODE_GRAPH_GREP_SYNC_BUDGET"),
        );
        stale_files = outcome.stale_paths;
    }
    let stale_count = stale_files.len();
    let mut node_cache: std::collections::HashMap<String, Vec<queries::NodeResult>> =
        std::collections::HashMap::new();
    let mut lookup_container = |file: &str,
                                line: u64|
     -> Option<(String, String, i64, i64, bool)> {
        let ctx = ctx.as_ref()?;
        if !node_cache.contains_key(file) {
            let nodes = queries::get_nodes_by_file_path(ctx.db.conn(), file).unwrap_or_default();
            node_cache.insert(file.to_string(), nodes);
        }
        let nodes = node_cache.get(file)?;
        let stale = stale_files.contains(file);
        find_containing_node_in(nodes, line).map(|(t, n, s, e)| (t, n, s, e, stale))
    };

    // Output. EPIPE (reader hung up, e.g. `| head`) is not an error — finish
    // silently with exit 0 like grep instead of spraying "Broken pipe".
    let write_result: std::io::Result<()> = (|| {
        let mut stdout = std::io::stdout().lock();
        if json_mode {
            let mut json_results = Vec::new();
            for m in &matches {
                let (text, line_omitted) = truncate_columns(&m.text, max_columns);
                let mut entry = serde_json::json!({
                    "file": m.file,
                    "line": m.line,
                    "text": text,
                });
                if let Some(omitted) = line_omitted {
                    // chars dropped by the -M/--max-columns width cap
                    entry["line_truncated"] = serde_json::json!(omitted);
                }
                if m.is_context {
                    entry["context"] = serde_json::json!(true);
                } else {
                    if let Some(container) = lookup_container(&m.file, m.line) {
                        let mut c = serde_json::json!({
                            "type": container.0,
                            "name": container.1,
                            "lines": format!("{}-{}", container.2, container.3),
                        });
                        if container.4 {
                            c["stale"] = serde_json::json!(true);
                        }
                        entry["container"] = c;
                    }
                    // This file hit the per-file cap — results for it are truncated.
                    if capped_set.contains(m.file.as_str()) {
                        entry["truncated"] = serde_json::json!(true);
                    }
                }
                json_results.push(entry);
            }
            let serialized =
                serde_json::to_string(&json_results).unwrap_or_else(|_| "[]".to_string());
            writeln!(stdout, "{}", serialized)?;
        } else {
            // grep formatting: matches `file:line`, context lines `file-line`,
            // `--` between non-contiguous groups when context is shown.
            let mut prev: Option<(String, u64)> = None;
            for m in &matches {
                if context_requested {
                    if let Some((ref pf, pl)) = prev {
                        if pf != &m.file || m.line > pl + 1 {
                            writeln!(stdout, "--")?;
                        }
                    }
                    prev = Some((m.file.clone(), m.line));
                }
                let sep = if m.is_context { '-' } else { ':' };
                let (text, line_omitted) = truncate_columns(&m.text, max_columns);
                write!(stdout, "{}{}{}  {}", m.file, sep, m.line, text)?;
                if let Some(omitted) = line_omitted {
                    write!(stdout, " … [+{} chars]", omitted)?;
                }
                writeln!(stdout)?;
                if !m.is_context {
                    if let Some((node_type, name, start, end, stale)) =
                        lookup_container(&m.file, m.line)
                    {
                        let marker = if stale { " [stale]" } else { "" };
                        writeln!(
                            stdout,
                            "  → {} {} (lines {}-{}){}",
                            node_type, name, start, end, marker
                        )?;
                    }
                }
            }
        }
        Ok(())
    })();
    match write_result {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => grep_exit(0),
        other => other?,
    }

    if !capped_files.is_empty() {
        eprintln!(
            "[code-graph] truncated: {} file(s) hit the per-file cap of {} matches: {}. Use --max-count 0 for all matches.",
            capped_files.len(),
            max_count,
            capped_files.join(", ")
        );
    }
    if stale_count > 0 {
        eprintln!(
            "[code-graph] {} file(s) changed since last index; annotations marked [stale] — run: code-graph-mcp incremental-index",
            stale_count
        );
    }
    if ctx.is_none() {
        // `try_open` returns Option, so "absent" and "present but unreadable"
        // arrive identically. They used to be the same situation in practice —
        // a corrupt index was deleted by this very open, making "No index
        // found" true by the time it printed. Readers no longer delete, so the
        // file survives and that sentence became false: it names the wrong
        // state and points at the wrong file. Distinguish by asking the disk.
        let db_path = effective_read_root(project_root)
            .join(CODE_GRAPH_DIR)
            .join("index.db");
        if db_path.exists() {
            eprintln!(
                "[code-graph] Index at {} could not be read (corrupt or unreadable). \
                 Run: code-graph-mcp rebuild-index --confirm",
                db_path.display()
            );
        } else {
            eprintln!("[code-graph] No index found. Run: code-graph-mcp incremental-index");
        }
        eprintln!("[code-graph] Showing plain grep results (no AST context).");
    }

    if partial_error {
        grep_exit(2);
    }
    Ok(())
}

/// Merged result of the one-or-more ripgrep invocations a single `grep` runs
/// (walk + argv-sized supplement batches — see `ARGV_PATH_BUDGET`).
pub(crate) struct RgRun {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Worst status across batches: 2 (error) > 0 (matched) > 1 (no match).
    code: i32,
}

pub(crate) struct GrepMatch {
    pub(crate) file: String,
    pub(crate) line: u64,
    pub(crate) text: String,
    /// true for -A/-B/-C context lines (rg JSON `type: "context"` records)
    pub(crate) is_context: bool,
}

/// Canonical display form for any filesystem path that reaches stdout or is used
/// as an index key: forward slashes, no Windows `\\?\` / `\\?\UNC\` extended
/// prefix. The index stores `/`-separated relative paths
/// (`indexer::merkle::normalize_rel_path`), so every path the CLI prints or looks
/// up must land in that same spelling — on Windows `rg` emits `\`, `git ls-files`
/// emits `/`, and `Path::canonicalize` emits `\\?\D:\…`, three spellings of one
/// file that compare unequal (issue #34: duplicated matches, `\\?\` leaking into
/// output, and AST annotation silently missing because the lookup key never
/// matched the indexed path).
pub(crate) fn normalize_path_display(path: &str) -> String {
    normalize_path_display_on(path, cfg!(windows))
}

/// Make an rg-reported path relative to the project root, in canonical
/// (`/`-separated, prefix-free) display form.
///
/// Both sides are normalized before the strip so the mixed spellings above
/// compare equal; on Windows the comparison is ASCII-case-insensitive because
/// the same volume legitimately appears as `D:\` and `d:\` (and rg may echo back
/// whichever spelling it was handed).
pub(crate) fn relativize_path(path_str: &str, root_str: &str) -> String {
    relativize_path_on(path_str, root_str, cfg!(windows))
}

/// Relativize against the canonical root first, the raw root second.
///
/// rg echoes back the spelling each operand was handed. Explicit and default
/// walk operands are canonical (long-form on Windows), but a nonexistent path
/// stays a raw lexical join — and on Windows the raw project root can be an
/// 8.3 short name (GitHub runners: `…\RUNNER~1\…`) that no lexical compare
/// can equate with the canonical long form. "Primary missed" is detected by
/// comparing against the empty-root normalization, which relativize returns
/// unchanged when the prefix does not match.
pub(crate) fn relativize_path_dual(
    path_str: &str,
    root_primary: &str,
    root_fallback: &str,
) -> String {
    let stripped = relativize_path(path_str, root_primary);
    if stripped != relativize_path(path_str, "") {
        return stripped;
    }
    relativize_path(path_str, root_fallback)
}

/// Testable core of [`relativize_path`] — see [`normalize_path_display_on`] for
/// why the platform is a parameter rather than a `cfg!`.
pub(crate) fn relativize_path_on(path_str: &str, root_str: &str, windows: bool) -> String {
    let path = normalize_path_display_on(path_str, windows);
    let root = normalize_path_display_on(root_str, windows);
    let root = root.trim_end_matches('/');
    let rest = if root.is_empty() {
        None
    } else if windows {
        // eq_ignore_ascii_case on the prefix only — the remainder keeps its case.
        // Windows volumes are legitimately spelled `D:\` or `d:\`, and rg echoes
        // back whichever spelling it was handed.
        path.get(..root.len())
            .filter(|head| head.eq_ignore_ascii_case(root))
            .map(|_| &path[root.len()..])
    } else {
        path.strip_prefix(root)
    };
    // `./x` (rg walking `.`) and a leftover leading separator both reduce to `x`.
    rest.unwrap_or(&path)
        .trim_start_matches('/')
        .trim_start_matches("./")
        .to_string()
}

/// Parse ripgrep JSON output into structured matches (and context lines when
/// -A/-B/-C were passed — rg interleaves `context` records in print order).
pub(crate) fn parse_rg_json(
    stdout: &[u8],
    root_canonical: &Path,
    root_raw: &Path,
) -> Vec<GrepMatch> {
    let root_str = root_canonical.to_string_lossy().into_owned();
    let root_raw_str = root_raw.to_string_lossy().into_owned();
    let mut matches = Vec::new();
    for line in stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let is_context = match v["type"].as_str() {
            Some("match") => false,
            Some("context") => true,
            _ => continue,
        };
        let data = &v["data"];
        let Some(path_str) = data["path"]["text"].as_str() else {
            continue;
        };
        let Some(line_number) = data["line_number"].as_u64() else {
            continue;
        };
        let text = data["lines"]["text"].as_str().unwrap_or("").to_string();

        matches.push(GrepMatch {
            file: relativize_path_dual(path_str, &root_str, &root_raw_str),
            line: line_number,
            text,
            is_context,
        });
    }
    matches
}

/// Truncate a line to `max_cols` characters for display (0 = no limit). Returns
/// the line without its trailing newline plus the number of characters omitted
/// (`None` if untouched). Counts characters, not bytes, so multibyte UTF-8 is
/// never split mid-codepoint — keeps one long minified/generated line from
/// flooding output (and an agent's context).
pub(crate) fn truncate_columns(line: &str, max_cols: u64) -> (String, Option<usize>) {
    let line = line.strip_suffix('\n').unwrap_or(line);
    if max_cols == 0 {
        return (line.to_string(), None);
    }
    let max = max_cols as usize;
    let total = line.chars().count();
    if total <= max {
        return (line.to_string(), None);
    }
    let kept: String = line.chars().take(max).collect();
    (kept, Some(total - max))
}

/// Find the innermost AST node containing the given line (from pre-loaded nodes).
pub(crate) fn find_containing_node_in(
    nodes: &[queries::NodeResult],
    line: u64,
) -> Option<(String, String, i64, i64)> {
    let mut best: Option<&queries::NodeResult> = None;
    for node in nodes {
        if node.start_line as u64 <= line && line <= node.end_line as u64 {
            match best {
                None => best = Some(node),
                Some(prev) => {
                    let prev_span = prev.end_line - prev.start_line;
                    let cur_span = node.end_line - node.start_line;
                    if cur_span < prev_span {
                        best = Some(node);
                    }
                }
            }
        }
    }

    best.map(|n| {
        let short_type = match n.node_type.as_str() {
            "function" | "method" => "fn",
            other => other,
        };
        let name = n.qualified_name.as_deref().unwrap_or(&n.name).to_string();
        (short_type.to_string(), name, n.start_line, n.end_line)
    })
}

// --- search subcommand ---
