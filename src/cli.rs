use anyhow::Result;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};

use crate::domain::{CODE_GRAPH_DIR, NO_METRICS_SENTINEL};
use crate::storage::db::Database;
use crate::storage::queries;

/// `$HOME` (Unix) / `%USERPROFILE%` (Windows) without pulling the `dirs` crate,
/// which lives behind the `embed-model` feature. `None` when unset → the walk is
/// simply unbounded (degrades to the pre-home-bound behavior).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve the project root from an explicit `cwd`. Mirrors the JS
/// `resolveProjectRoot` (`claude-plugin/scripts/project-root.js`); keep the two
/// in lock-step (see `feedback_hook_class_bug_sweep`).
///
/// Order:
/// 1. cwd's OWN `.git` → cwd (a real project boundary: submodule, or a fresh
///    project with a `.git` but not yet an index — the metrics-isolation fixture).
/// 2. cwd's index wins UNLESS it is STRAY — an ancestor within the `.git`
///    boundary, below `$HOME`, is itself indexed (the monorepo-subdir relic an
///    older binary created and `priority-1` then pinned, so every tool read a
///    different DB per subdir).
/// 3. Otherwise the canonical project root: nearest INDEXED ancestor, else nearest
///    ancestor `.git`, else cwd.
///
/// The walk stops at `$HOME` (exclusive) so an unrelated `~/.code-graph` /
/// `~/.git` never poisons a project beneath it.
pub fn resolve_project_root_from(cwd: &Path) -> PathBuf {
    resolve_project_root_bounded(cwd, home_dir().as_deref())
}

/// `home`-injectable core so the `$HOME` boundary is unit-testable without
/// mutating the process environment (mirrors the JS resolver's `opts.home`).
fn resolve_project_root_bounded(cwd: &Path, home: Option<&Path>) -> PathBuf {
    // 1. cwd's own `.git` is always a boundary.
    if cwd.join(".git").exists() {
        return cwd.to_path_buf();
    }
    let cwd_has_index = cwd.join(CODE_GRAPH_DIR).join("index.db").exists();

    // Walk STRICT ancestors, stopping AT `$HOME` (exclusive) or the nearest
    // `.git` root. Track the nearest indexed ancestor (the canonical root of an
    // already-indexed project) and the nearest `.git` root within that bound.
    let mut nearest_indexed: Option<PathBuf> = None;
    let mut git_root_indexed: Option<PathBuf> = None;
    let mut nearest_git: Option<PathBuf> = None;
    let mut cursor = cwd.parent();
    while let Some(c) = cursor {
        if home == Some(c) {
            break; // an index/.git at-or-above home is an unrelated outer project
        }
        let c_indexed = c.join(CODE_GRAPH_DIR).join("index.db").exists();
        if nearest_indexed.is_none() && c_indexed {
            nearest_indexed = Some(c.to_path_buf());
        }
        if c.join(".git").exists() {
            nearest_git = Some(c.to_path_buf());
            if c_indexed {
                git_root_indexed = Some(c.to_path_buf());
            }
            break;
        }
        cursor = c.parent();
    }

    // 2. cwd's index wins only when it is NOT stray (no indexed ancestor in bound).
    if cwd_has_index && nearest_indexed.is_none() {
        return cwd.to_path_buf();
    }
    // 3. Prefer the indexed `.git` root — the canonical project root — over a
    //    nearer STRAY indexed ancestor (a monorepo-subdir relic with no `.git`).
    //    Returning the stray made the CLI read a different DB than the JS hooks
    //    (which resolve to the indexed git root), a split-brain (M7). Mirrors
    //    project-root.js: git_root_indexed → nearest indexed ancestor → a `.git`
    //    root to index → cwd.
    if let Some(g) = git_root_indexed {
        return g;
    }
    if let Some(idx) = nearest_indexed {
        return idx;
    }
    if let Some(g) = nearest_git {
        return g;
    }
    cwd.to_path_buf()
}

/// Resolve the project root from the current working directory.
pub fn resolve_project_root() -> std::io::Result<PathBuf> {
    Ok(resolve_project_root_from(&std::env::current_dir()?))
}

/// Main-checkout root of a LINKED git worktree, or None. A linked worktree's
/// `.git` is a FILE containing `gitdir: <main>/.git/worktrees/<name>`; a
/// regular repo has a `.git` DIRECTORY, and a submodule's `.git` file points
/// at `…/.git/modules/…` (a different codebase — hard boundary, returns None
/// because the `worktrees` marker is absent). Rust mirror of
/// `claude-plugin/scripts/project-root.js` `worktreeMainRoot` — keep the two
/// in sync by hand (they share the marker-substring contract, not code).
fn worktree_main_root(root: &Path) -> Option<PathBuf> {
    let git_path = root.join(".git");
    if !git_path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&git_path).ok()?;
    let gitdir_line = content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))?
        .trim();
    if gitdir_line.is_empty() {
        return None;
    }
    let gitdir = if Path::new(gitdir_line).is_absolute() {
        PathBuf::from(gitdir_line)
    } else {
        root.join(gitdir_line)
    };
    let s = gitdir.to_string_lossy();
    // git writes gitdir with FORWARD slashes even on Windows (and the JS side's
    // path.resolve normalizes to backslashes there) — a MAIN_SEPARATOR marker
    // never matched on Windows, so the fallback was silently dead (CI windows
    // red on v0.100.1). Normalize length-preservingly for the search; slice the
    // ORIGINAL string so the returned path keeps its native separators.
    let norm = s.replace('\\', "/");
    let idx = norm.rfind("/.git/worktrees/")?;
    let main_root = PathBuf::from(&s[..idx]);
    if main_root.as_os_str().is_empty() {
        None
    } else {
        Some(main_root)
    }
}

/// Read-side effective root (D#106): the given root when its index exists
/// (own index always wins); else, for a linked worktree whose MAIN checkout
/// is indexed, the main checkout root; else the given root unchanged (the
/// caller's "No index found" path fires with the original location).
fn effective_read_root(project_root: &Path) -> PathBuf {
    if project_root.join(CODE_GRAPH_DIR).join("index.db").exists() {
        return project_root.to_path_buf();
    }
    if let Some(main) = worktree_main_root(project_root) {
        if main.join(CODE_GRAPH_DIR).join("index.db").exists() {
            return main;
        }
    }
    project_root.to_path_buf()
}

/// Project-root markers — the literal set the JS activation gate uses
/// (`claude-plugin/scripts/project-detect.js` `PROJECT_MARKERS`). Both layers
/// must agree on "what is a real project"; kept in sync by hand.
pub const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
];

/// True when `cwd` carries none of the recognized project markers — e.g. `/tmp`
/// or Claude Code's `$TMPDIR`, where claude-mem-lite spawns headless `claude -p`
/// calls that never use code-graph. The MCP launcher gates the same way
/// (`mcp-launcher.js` → `isNonProjectCwd`); this is the Rust counterpart so the
/// binary self-protects even when invoked directly (bypassing the launcher).
///
/// Marker-based and cwd-only — deliberately NOT keyed on an existing
/// `.code-graph/index.db`: that file is created BY this tool, so counting it
/// would let a once-polluted dir self-certify as a project on the next run
/// (same rationale as `project-detect.js`).
pub fn is_non_project_cwd(cwd: &Path) -> bool {
    !PROJECT_MARKERS.iter().any(|m| cwd.join(m).exists())
}

/// Minimal JSON-RPC loop that answers `initialize` / `tools/list` with an empty
/// catalog and rejects everything else, WITHOUT opening a database, loading the
/// embedding model, or creating `.code-graph/`. Mirrors the JS launcher's
/// `serveEmptyMcpStub`. Driven by `run_serve` when `is_non_project_cwd` holds
/// and `CODE_GRAPH_FORCE_PLUGIN_MCP` is unset.
pub fn serve_non_project_stub<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = match req.get("method").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => continue,
        };
        // JSON-RPC notifications (no `id`) get no response.
        let id = match req.get("id") {
            Some(id) => id.clone(),
            None => continue,
        };
        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "code-graph-mcp (non-project stub)",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
            "tools/list" => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [] } })
            }
            "resources/list" => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "resources": [] } })
            }
            "prompts/list" => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "prompts": [] } })
            }
            "ping" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            _ => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": "method not found (non-project stub mode)" }
            }),
        };
        writeln!(writer, "{}", response)?;
        writer.flush()?;
    }
    Ok(())
}

/// Remove empty legacy database files left behind from past naming migrations.
/// Pre-v0.5 iterations briefly used `code-graph.db`, `code_graph.db`, `graph.db`
/// before settling on `index.db`; the renames never deleted the old 0-byte stubs.
pub fn cleanup_legacy_db_files(code_graph_dir: &Path) {
    const LEGACY: &[&str] = &["code-graph.db", "code_graph.db", "graph.db"];
    for name in LEGACY {
        let p = code_graph_dir.join(name);
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.len() == 0 {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Lightweight CLI context for subcommands called by hooks.
/// Does NOT load the embedding model (too slow for 5-10s hook timeouts).
pub struct CliContext {
    pub db: Database,
    pub project_root: PathBuf,
}

impl CliContext {
    pub fn open(project_root: &Path) -> Result<Self> {
        Self::open_inner(project_root, false)
    }

    /// Same reader contract as [`CliContext::open`], but with the sqlite-vec
    /// tables brought up for the one read command that needs vector search
    /// (`similar`). Kept distinct from `Database::open_with_vec` on purpose:
    /// that constructor also revalidates (wipes) on `INDEX_VERSION` mismatch,
    /// which a read command must never do.
    pub fn open_with_vec(project_root: &Path) -> Result<Self> {
        Self::open_inner(project_root, true)
    }

    fn open_inner(project_root: &Path, with_vec: bool) -> Result<Self> {
        // Read-side worktree fallback (D#106, roadmap §2.2 — Rust mirror of the
        // v0.99.0 project-root.js fix): a linked worktree with no OWN index
        // reads the main checkout's index instead of erroring/cold-building.
        // Own index wins (checked first inside effective_read_root); write side
        // (index/serve/rebuild) does not go through CliContext and still builds
        // a local index. Paths/line numbers in answers are the main checkout's,
        // same contract as the JS hooks/statusline side.
        let project_root = &effective_read_root(project_root);
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        if !db_path.exists() {
            anyhow::bail!(
                "No index found at {}. Run: code-graph-mcp incremental-index",
                db_path.display()
            );
        }
        cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));
        // CLI commands behind CliContext are READERS (grep, show, callgraph,
        // health-check, …). Open non-destructively so a status poll or one-off
        // query never triggers the INDEX_VERSION wipe — only an explicit indexer
        // (reindex / incremental-index / server startup) clears + rebuilds.
        let db = if with_vec {
            Database::open_nondestructive_with_vec(&db_path)?
        } else {
            Database::open_nondestructive(&db_path)?
        };
        Ok(Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Try to open, returning None if no index exists (for grep fallback).
    pub fn try_open(project_root: &Path) -> Option<Self> {
        // Same read-side worktree fallback as open() above.
        let project_root = &effective_read_root(project_root);
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        if !db_path.exists() {
            return None;
        }
        cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));
        Database::open_nondestructive(&db_path).ok().map(|db| Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }
}

// --- Argument helpers ---

/// Normalize a user-provided path argument to a project-relative string.
///
/// - `"."` → `""` (whole project — matches MCP `module_overview` semantics)
/// - `"./foo"` → `"foo"`
/// - absolute path under `project_root` → relative portion (lexical first, canonical fallback for symlinks)
/// - absolute path outside `project_root` → error
/// - relative path that escapes the root via `..` → error
/// - other relative path → unchanged
///
/// Why: indexed `file_path` columns in SQLite are project-relative. When users
/// paste an absolute path from an IDE (very common), the CLI used to silently
/// return empty/wrong results (`overview` "No symbols found", `dead-code` exit-0
/// "No dead code found", `deps` bogus barrel-scan fallback). All three are
/// indistinguishable from real "no results" → user trusts the wrong answer.
/// A relative `..` escape is worse than wrong: the index holds only in-root
/// paths, so the path can only match the disk — `deps`' barrel-scan reads
/// `project_root.join(raw)`, turning `deps ../../secret.js` into a path-traversal
/// file read that leaks the file's import/re-export lines. Reject the escape.
fn normalize_user_path(project_root: &Path, raw: &str) -> Result<String> {
    // Relative path args resolve against the caller's current directory (like
    // grep/ls/cat), not the project root — so `deps main.rs` works from `src/`.
    // Every programmatic caller (hooks, cg-answer) spawns the binary with
    // cwd==root, so for them this is byte-identical to the historical
    // root-relative behavior; only a human running the CLI from a subdirectory
    // sees paths resolve from where they stand. cwd lookup can only fail in
    // pathological environments — fall back to the root (root-relative reading).
    let cwd = std::env::current_dir().unwrap_or_else(|_| project_root.to_path_buf());
    normalize_user_path_from(project_root, &cwd, raw)
}

/// cwd-parameterized core of [`normalize_user_path`] (split out so tests pin the
/// working directory instead of depending on the process cwd). `cwd` is the
/// directory a relative `raw` resolves against; in production it is always the
/// project root or a descendant (`resolve_project_root` walks UP from cwd).
fn normalize_user_path_from(project_root: &Path, cwd: &Path, raw: &str) -> Result<String> {
    normalize_user_path_from_on(project_root, cwd, raw, cfg!(windows))
}

/// Platform-parameterized core of [`normalize_user_path_from`].
///
/// `backslash_is_sep` says whether `\` separates path components on the target
/// platform. Taking it as a parameter (rather than reading `cfg!(windows)` deep
/// inside) is what lets the Linux CI leg execute the Windows branches — the
/// structural reason `.\src\foo.rs` slipped through in the first place: the
/// `"."` / `"./"` prefix tests below are spelled Unix-only, and on Windows
/// PowerShell's tab completion produces `.\` by default, so the whole
/// cwd-anchored arm was dead there and the final `normalize_rel_str` emitted
/// `./src/foo.rs` — a lookup key with a `./` prefix the index never contains.
/// Does `raw` spell a Windows drive/UNC root that `Path::is_absolute` did NOT
/// claim on this host?
///
/// `natively_absolute` is `Path::new(raw).is_absolute()`, taken as a parameter
/// for the same reason `backslash_is_sep` is: it is the ONLY thing that differs
/// between hosts here, so passing it in lets the Linux CI leg execute the
/// Windows branch. Without that seam the Windows behaviour of this guard is
/// unobservable off-Windows, and both previous versions shipped a defect that
/// only the windows-latest leg could see.
///
/// The drive form requires a separator after the colon (`C:\x`, `C:/x`) or the
/// bare root (`C:`). A colon at byte 1 alone is not enough: `:` is legal in a
/// POSIX filename, so `a:b.rs` in the project root is a real, indexable file.
fn needs_lexical_windows_rejection(raw: &str, natively_absolute: bool) -> bool {
    if natively_absolute {
        // Windows claims `C:\x` and `\\srv\share` itself; the under-root check
        // is the right answer for them, and rejecting them lexically refused
        // `C:\repo\src\mod.rs` for a root that literally contains it.
        return false;
    }
    let b = raw.as_bytes();
    let drive_root = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    (drive_root && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')) || raw.starts_with(r"\\")
}

fn normalize_user_path_from_on(
    project_root: &Path,
    cwd: &Path,
    raw: &str,
    backslash_is_sep: bool,
) -> Result<String> {
    // Separator normalization (including collapsing `//`) lives in
    // `merkle::normalize_rel_str_on`, the crate's single implementation — the
    // first fix for the doubled-separator false clean put the collapse here
    // instead, which left the MCP entries (`tools::normalize_path_arg`) still
    // broken, and MCP's failure was the worse one: it re-indexed the file under
    // the non-canonical key rather than merely missing.
    normalize_user_path_key(project_root, cwd, raw, backslash_is_sep)
}

fn normalize_user_path_key(
    project_root: &Path,
    cwd: &Path,
    raw: &str,
    backslash_is_sep: bool,
) -> Result<String> {
    use crate::indexer::pipeline::is_safe_relative_path;
    // Single source of truth for the escape check (shared with the MCP freshness
    // path) — eliminates the three-way divergence between this fn, the MCP
    // `read_source_context` canonicalize guard, and `is_safe_relative_path`. ANY
    // project-relative result, whether typed directly OR derived by stripping the
    // root prefix off an absolute path, must not climb above the root via `..`.
    let escape = || {
        anyhow::anyhow!(
            "path '{}' escapes the project root '{}' \u{2014} use a path inside the project",
            raw,
            project_root.display()
        )
    };

    // Absolute path: cwd-independent. CRITICAL: `Path::strip_prefix` matches
    // components and does NOT collapse `..`, so `<root>/../../etc/passwd` strips
    // to `../../etc/passwd` — a remainder that still escapes. The old code
    // returned it unchecked (the escape check ran only in the relative branch),
    // so a barrel-scan `deps <root>/../../secret` did an out-of-root read (the
    // absolute-prefix sibling of the relative `..` traversal). Re-validate the
    // stripped remainder.
    // Every `return Ok(...)` below yields an INDEX LOOKUP KEY, so it must be in
    // the `/`-separated form `merkle::normalize_rel_path` stores. `to_string_lossy`
    // on a stripped `Path` keeps the NATIVE separator, so on Windows these
    // branches produced `src\Foo.cs` against an index holding `src/Foo.cs` — the
    // key never matched and `affected` / `deps` / `trace` / `show` reported a
    // present file as "not in index". Same defect class as issue #34.
    // (`collapse_within_root`, used by the subdirectory branch below, already
    // decomposes into `Component`s and re-joins with `/`, so it is unaffected.)
    // Windows-absolute spellings are NOT absolute on a Unix host, so
    // `Path::is_absolute` waves `D:\repo\src\Foo.cs` and `C:/repo/src` straight
    // into the relative branch below, where they normalize to
    // `D:/repo/src/Foo.cs` — a key no index contains, reported as an ordinary
    // "no results". That is the silent-miss shape this whole function exists to
    // prevent, reintroduced through the one predicate the `_on` seam does NOT
    // parameterize. A drive prefix or a UNC root can never name a
    // project-relative file on ANY platform, so reject them LEXICALLY rather
    // than platform-natively — which also brings the CLI in line with the MCP
    // entry (`tools/overview.rs` already rejects the drive-letter form).
    // Exact spellings, why `is_absolute` is the gate rather than `cfg!(windows)`,
    // and the two defects each earlier version shipped: see
    // `needs_lexical_windows_rejection`. (Drive-RELATIVE `C:foo` is deliberately
    // not matched: it is vanishingly rare next to ordinary colon-bearing
    // filenames, and the cost of guessing wrong is refusing a file that exists.)
    let p = Path::new(raw);
    if needs_lexical_windows_rejection(raw, p.is_absolute()) {
        anyhow::bail!(
            "path '{}' is outside the project root '{}' \u{2014} use a relative path or one under the project root",
            raw, project_root.display()
        );
    }

    if p.is_absolute() {
        if let Ok(rel) = p.strip_prefix(project_root) {
            let rel = crate::indexer::merkle::normalize_rel_path(rel);
            if !is_safe_relative_path(&rel) {
                return Err(escape());
            }
            return Ok(rel);
        }
        // Symlink fallback: canonicalize resolves `..` and links, so a successful
        // strip_prefix here is genuinely under the root (no `..` can survive).
        if let (Ok(canon_p), Ok(canon_root)) = (p.canonicalize(), project_root.canonicalize()) {
            if let Ok(rel) = canon_p.strip_prefix(&canon_root) {
                return Ok(crate::indexer::merkle::normalize_rel_path(rel));
            }
        }
        anyhow::bail!(
            "path '{}' is outside the project root '{}' \u{2014} use a relative path or one under the project root",
            raw, project_root.display()
        );
    }

    // Relative path: resolve against the cwd's offset from the root. In
    // production cwd is the root or a descendant; a non-descendant cwd (only
    // reachable in tests / pathological envs) yields an empty offset and the
    // historical root-relative reading.
    let cwd_rel = cwd
        .strip_prefix(project_root)
        .map(|r| r.to_path_buf())
        .unwrap_or_default();

    // Unify the separator BEFORE any prefix test. Windows users type and paste
    // `src\foo.rs` and `.\src\foo.rs` (Explorer, tab completion, other tools'
    // output); every `.`/`./`/`../` test below is spelled with `/`, so doing this
    // last — as the code used to — left the whole cwd-anchored arm unreachable on
    // Windows. `is_safe_relative_path` uses `Path::components`, which splits on
    // `\` under Windows, so the escape check sees the same components either way.
    let normalized = crate::indexer::merkle::normalize_rel_str_on(raw, backslash_is_sep);
    let raw: &str = &normalized;

    if cwd_rel.as_os_str().is_empty() {
        // cwd == project root: historical root-relative behavior, unchanged.
        if raw == "." {
            return Ok(String::new());
        }
        if let Some(rest) = raw.strip_prefix("./") {
            // `./foo` → `foo`, but `./../secret` still climbs out — validate the rest.
            if !is_safe_relative_path(rest) {
                return Err(escape());
            }
            return Ok(rest.to_string());
        }
        // Lexical escape check (no filesystem touch — target may be gitignored or
        // deleted): reject any prefix that climbs above the root.
        if !is_safe_relative_path(raw) {
            return Err(escape());
        }
        return Ok(raw.to_string());
    }

    // cwd is a subdirectory of the root: resolve `raw` against it, collapsing
    // `.`/`..` lexically so a `../` climbs back toward (but never above) the
    // root. A `..` that pops past the root is the subdir-relative escape.
    let subdir_rel = collapse_within_root(&cwd_rel.join(raw)).ok_or_else(escape)?;
    // Near-miss rebase (same field failure as cmd_grep's, 2026-07-24): the
    // agent's shell often sits in a subdir while it quotes repo-root-relative
    // paths (hook answers display them root-relative), doubling the prefix into
    // a path that exists nowhere — downstream that surfaces as a misleading
    // "No symbols found under: <the valid path the caller typed>". cwd-missing +
    // root-existing is unambiguous: take the root reading and say so on stderr.
    // When the cwd-relative target exists — or neither does (it may be indexed
    // but deleted/gitignored; don't guess) — the cwd reading stands. `.`, `./x`
    // and `../x` are explicitly cwd-anchored (and `root.join(".")` always
    // exists), so they never rebase; a bare `.hidden` name still can.
    if !is_cwd_anchored(raw)
        && !project_root.join(&subdir_rel).exists()
        && is_safe_relative_path(raw)
        && project_root.join(raw).exists()
    {
        note_root_rebase(raw, project_root);
        return collapse_within_root(Path::new(raw)).ok_or_else(escape);
    }
    Ok(subdir_rel)
}

/// A path the caller explicitly anchored to the current directory (`.`, `./x`,
/// `../x`, and their `\` spellings) — such paths never take the near-miss root
/// rebase. Single source of the exclusion list shared by BOTH rebase arms
/// (`normalize_user_path_from` and `cmd_grep`); extend it here so the two arms
/// can't drift apart again (audit 2026-07-24: the arms were hand-duplicated
/// copies).
///
/// Both spellings are recognized unconditionally, and deliberately so: this is a
/// *user-intent* predicate, not an index key. `normalize_user_path_from` hands it
/// already-normalized input, but `cmd_grep` does not, and PowerShell tab
/// completion emits `.\` by default — a Unix file literally named `.\x` merely
/// forgoes a rebase it would rarely want anyway.
fn is_cwd_anchored(raw: &str) -> bool {
    raw == "."
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with(".\\")
        || raw.starts_with("..\\")
}

/// The one stderr surface for a near-miss root rebase, shared by both arms so
/// the disclosure wording stays identical.
fn note_root_rebase(raw: &str, root: &Path) {
    eprintln!(
        "[code-graph] note: '{}' not found under the current directory; resolved against project root {}",
        raw,
        root.display()
    );
}

/// Lexically collapse a root-relative path's `.`/`..` components and return the
/// `/`-joined remainder. `None` when a `..` climbs above the root (escape) or an
/// absolute/Windows-prefix component appears (no filesystem access — the target
/// may be gitignored or not yet created).
fn collapse_within_root(rel: &Path) -> Option<String> {
    use std::path::Component;
    let mut stack: Vec<String> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => stack.push(c.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                stack.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(stack.join("/"))
}

/// Strip qualified name prefix (e.g. "McpServer.handle_message" -> "handle_message")
/// so users can copy-paste names from output and use them in lookups.
fn strip_qualified_prefix(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// CLI-side fuzzy name resolution — the shared implementation in
/// `crate::resolve`, so CLI `callgraph`/`refs` and the MCP tools cannot drift
/// into opposite answers for one input (audit 2026-06-03 #6; the hand-written
/// CLI copy this replaces was the same defect shape, and had zero tests).
use crate::resolve::FuzzyResolution as CliFuzzyResolution;

fn resolve_fuzzy_name_cli(conn: &rusqlite::Connection, name: &str) -> Result<CliFuzzyResolution> {
    crate::resolve::resolve_fuzzy(conn, name)
}

/// Emit the "ambiguous symbol" error in the same shape whether the command was
/// invoked with --json (one-line JSON) or default (human-readable stderr lines),
/// then exit(1). Shared by cmd_callgraph, cmd_impact when no file filter was
/// given and `crate::resolve::detect_ambiguity` returned candidates. The message
/// and JSON suggestion shape come from `crate::resolve` so the CLI and MCP give
/// identical verdicts on same-file overloads (audit 2026-06-03 #6).
fn emit_exact_ambiguity(symbol: &str, cands: &[queries::NameCandidate], json_mode: bool) -> ! {
    let message = crate::resolve::ambiguity_message(symbol, cands, crate::resolve::Surface::Cli);
    if json_mode {
        let sugg: Vec<serde_json::Value> = crate::resolve::candidates_to_json(cands)
            .into_iter()
            .take(5)
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "error": message,
                "suggestions": sugg,
            })
        );
    } else {
        eprintln!("[code-graph] {}", message);
        for c in cands.iter().take(5) {
            eprintln!(
                "  {} ({}) in {} [node_id {}]",
                c.name, c.node_type, c.file_path, c.node_id
            );
        }
    }
    std::process::exit(1);
}

/// Resolve a possibly-qualified symbol name (e.g. "Database.open") to a base name
/// and optional file path for disambiguation. When the user passes a qualified name,
/// we find the matching node and use its file_path as a filter so that downstream
/// queries (callgraph, impact, refs) pick the right symbol.
/// Returns (base_name, resolved_file_filter) where resolved_file_filter is Some only
/// if the qualified name resolved uniquely and no explicit --file was given.
fn resolve_qualified_symbol<'a>(
    conn: &rusqlite::Connection,
    raw_symbol: &'a str,
    explicit_file: Option<&'a str>,
) -> (&'a str, Option<String>) {
    // If user already provided --file, just strip the prefix and use their filter
    if explicit_file.is_some() {
        return (strip_qualified_prefix(raw_symbol), None);
    }
    // If the symbol contains '.', try qualified name resolution
    if raw_symbol.contains('.') {
        let base = strip_qualified_prefix(raw_symbol);
        if let Ok(nodes) = queries::get_nodes_by_name(conn, base) {
            let matched: Vec<_> = nodes
                .iter()
                .filter(|n| n.qualified_name.as_deref() == Some(raw_symbol))
                .collect();
            if matched.len() == 1 {
                if let Ok(Some(fp)) = queries::get_file_path(conn, matched[0].file_id) {
                    return (base, Some(fp));
                }
            }
        }
        return (base, None);
    }
    (raw_symbol, None)
}

// --- Output formatting ---

/// Format a node as a compact single line: `type QualifiedName  file:start-end  (params) -> return`
fn format_node_compact(node: &queries::NodeResult, file_path: &str) -> String {
    let mut out = String::with_capacity(128);
    // type prefix
    let short_type = match node.node_type.as_str() {
        "function" => "fn",
        "method" => "fn",
        "class" => "class",
        "struct" => "struct",
        "interface" => "iface",
        "trait" => "trait",
        "enum" => "enum",
        "type_alias" => "type",
        "constant" => "const",
        "variable" => "var",
        other => other,
    };
    out.push_str(short_type);
    out.push(' ');

    // name (prefer qualified)
    if let Some(ref qn) = node.qualified_name {
        out.push_str(qn);
    } else {
        out.push_str(&node.name);
    }

    // location
    out.push_str("  ");
    out.push_str(file_path);
    out.push(':');
    out.push_str(&node.start_line.to_string());
    out.push('-');
    out.push_str(&node.end_line.to_string());

    // signature parts. param_types is stored ALREADY parenthesized ("(a, b)") by the
    // parser — verified every non-empty param_types starts with '(' and ends with ')'
    // — so append it verbatim. Wrapping it in another pair printed "((a, b))" (and
    // "(())" for no-arg fns) in `show` / `search` / `ast_search` output.
    if let Some(ref params) = node.param_types {
        if !params.is_empty() {
            out.push_str("  ");
            out.push_str(params);
        }
    }
    if let Some(ref ret) = node.return_type {
        if !ret.is_empty() {
            out.push_str(" -> ");
            out.push_str(ret);
        }
    }
    out
}

// --- Subcommands ---

// Internal notes — `//` (not `///`) so clap leaves them out of `--help`: only flag
// parsing lives in this struct; the git/index existence guard stays in main() — it
// must precede any resolve_project_root indexing side effect and may skip the run
// entirely (issue #8). The handler keeps its `quiet: bool` signature so the internal
// reindex/rebuild-index callers are unaffected.
/// CLI arguments for the `incremental-index` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp incremental-index",
    about = "Run incremental index update (full index when none exists)"
)]
pub struct IncrementalIndexArgs {
    /// Suppress progress output (used by the PostToolUse hook)
    #[arg(long)]
    pub quiet: bool,
    /// Index structure only (nodes/edges/FTS) and skip embeddings for a fast,
    /// query-ready index. Vectors backfill later (MCP server / a later run).
    #[arg(long)]
    pub no_embed: bool,
    /// Print the run's counters as one JSON object on stdout (progress stays on
    /// stderr). For CI and scripts: `--json` used to be a clap parse error here,
    /// so the only way to learn what an index run did was to scrape prose.
    #[arg(long)]
    pub json: bool,
}

/// Run incremental index update.
/// If `quiet` is true, suppress non-error output.
/// Auto-creates the database and runs a full index if no index exists.
/// Map SQLITE_BUSY ("database is locked", error code 5) to an actionable hint —
/// surfaces when two indexers / an MCP server race on the same index.db. Shared
/// by the full / incremental / embed paths.
fn wrap_index_busy<T>(r: Result<T>) -> Result<T> {
    r.map_err(|e| {
        let msg = format!("{:#}", e);
        if msg.contains("database is locked") || msg.contains("Error code 5") {
            anyhow::anyhow!(
                "Another `code-graph-mcp` process is writing to .code-graph/index.db \
                 (an indexer or MCP server). Wait for it to finish, then retry. \
                 Original error: {}",
                e
            )
        } else {
            e
        }
    })
}

/// Embed any nodes still missing vectors (synchronous, unlike the server's
/// background thread). No-op without the `embed-model` feature or when the model
/// can't load. Shared by the full / incremental / rebuild paths so embedding
/// behaviour can't drift between them.
fn embed_missing_nodes(db: &Database, quiet: bool) -> Result<()> {
    if !db.vec_enabled() {
        return Ok(());
    }
    use crate::embedding::model::EmbeddingModel;
    use crate::indexer::pipeline::embed_and_store_batch;
    if let Some(model) = EmbeddingModel::load()? {
        let mut total = 0usize;
        // Skip nodes that fail to embed this run. This loop only stops on an empty
        // result, so without excluding failures a single deterministically-un-embeddable
        // node (which stays `node_vectors IS NULL` and sorts first by caller-count) would
        // be re-fetched at the head of every batch and spin the loop forever.
        let mut failed: std::collections::HashSet<i64> = std::collections::HashSet::new();
        loop {
            let exclude: Vec<i64> = failed.iter().copied().collect();
            let chunk = wrap_index_busy(queries::get_unembedded_nodes_excluding(
                db.conn(),
                64,
                &exclude,
            ))?;
            if chunk.is_empty() {
                break;
            }
            let chunk_len = chunk.len();
            let embedded_ids = wrap_index_busy(embed_and_store_batch(db, &model, &chunk))?;
            total += embedded_ids.len();
            if embedded_ids.len() < chunk_len {
                let ok: std::collections::HashSet<i64> = embedded_ids.into_iter().collect();
                for (id, _) in &chunk {
                    if !ok.contains(id) {
                        failed.insert(*id);
                    }
                }
            }
        }
        if total > 0 && !quiet {
            let (embedded, embeddable) = queries::count_nodes_with_vectors(db.conn())?;
            eprintln!("Embedded {} nodes ({}/{})", total, embedded, embeddable);
        }
        if !failed.is_empty() && !quiet {
            eprintln!("{} node(s) could not be embedded (skipped)", failed.len());
        }
    }
    Ok(())
}

/// Surface, on the CLI path, the count of files that parsed with tree-sitter
/// ERROR nodes (symbols may be incomplete). Dual-writes `tracing::warn!` AND a
/// stderr summary line: the CLI entry points install no tracing subscriber
/// (feedback_tracing_invisible_in_cli), so the eprintln is what the user
/// actually sees; the tracing line keeps it visible under a server/log setup.
/// Silent when the count is zero. `quiet` suppresses only the stderr line, like
/// the surrounding index summaries.
fn warn_parse_errors(stats: &crate::indexer::pipeline::IndexStats, quiet: bool) {
    let n = stats.files_with_parse_errors;
    if n == 0 {
        return;
    }
    tracing::warn!(
        "{} file(s) parsed with syntax errors (symbols may be incomplete)",
        n
    );
    if !quiet {
        eprintln!(
            "{} file(s) parsed with syntax errors (symbols may be incomplete)",
            n
        );
    }
}

/// Build a fresh FULL index into an explicit `db_path` and embed it. The DB is
/// opened and dropped within this call, so on return the WAL is checkpointed and
/// `db_path` is self-contained — which lets `rebuild-index` build into a temp
/// file and atomically rename it over `index.db`.
fn build_full_index_at(
    db_path: &Path,
    project_root: &Path,
    quiet: bool,
    no_embed: bool,
) -> Result<crate::indexer::pipeline::IndexResult> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
        cleanup_legacy_db_files(parent);
    }
    // Same `.gitignore` upkeep the MCP server does when IT creates the dir — a
    // pure-CLI install (hook-driven indexing, server never started) otherwise
    // leaves `?? .code-graph/` for `git add -A` to commit (audit DB-4).
    crate::utils::gitignore::ensure_code_graph_dir_ignored(project_root);
    // Open with vec support so embeddings can be stored.
    let db = Database::open_with_vec(db_path)?;
    use crate::indexer::pipeline::run_full_index;
    let result = wrap_index_busy(run_full_index(&db, project_root, None, None))?;
    if !quiet {
        eprintln!(
            "Full index: {} files, {} nodes, {} edges",
            result.files_indexed, result.nodes_created, result.edges_created
        );
    }
    warn_parse_errors(&result.stats, quiet);
    finish_embedding(&db, quiet, no_embed)?;
    Ok(result)
}

/// The `--json` object shared by `incremental-index`, `rebuild-index` and
/// `reindex`: one line on stdout, emitted only after the run has actually
/// succeeded, so the tier-3 error contract in `main` stays the sole producer of
/// output on the failure path (a command must never print both).
///
/// `mode` names the path that really ran, not the subcommand asked for —
/// `incremental-index` on a fresh checkout reports `full`, and `reindex
/// --from-snapshot` reports whatever the post-install pass did. A CI script
/// reading `files_indexed` needs to know which of the two it is looking at.
fn emit_index_json(
    mode: &str,
    result: &crate::indexer::pipeline::IndexResult,
    started: std::time::Instant,
) {
    println!(
        "{}",
        serde_json::json!({
            "mode": mode,
            "files_indexed": result.files_indexed,
            "files_deleted": result.files_deleted,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
            "files_with_parse_errors": result.stats.files_with_parse_errors,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        })
    );
}

/// Shared structure-first → embedding handoff for the CLI index commands.
///
/// The structural graph (nodes/edges/FTS) is already committed and usable for
/// AST / grep / callgraph queries the moment indexing returns — embedding is a
/// separate, slow (CPU-bound) pass that only powers semantic/vector search. On a
/// large repo it dominates wall-clock (≈5 nodes/s), so a foreground `reindex`
/// could block for many minutes after the graph was already query-ready.
///
/// `--no-embed` skips it: the caller gets the fast structural index and the
/// vectors backfill later (the MCP server's background embedder fills any node
/// lacking a vector, resumably; or rerun without the flag to embed now).
fn finish_embedding(db: &Database, quiet: bool, no_embed: bool) -> Result<()> {
    if no_embed {
        if !quiet && db.vec_enabled() {
            let (embedded, embeddable) =
                queries::count_nodes_with_vectors(db.conn()).unwrap_or((0, 0));
            eprintln!(
                "Structure index ready (AST/grep/callgraph usable now). Skipping embeddings \
                 (--no-embed): {}/{} nodes have vectors; the rest backfill in the background \
                 or via `code-graph-mcp incremental-index`.",
                embedded, embeddable
            );
        }
        return Ok(());
    }
    embed_missing_nodes(db, quiet)
}

/// Warn if another process holds the index lock. A running MCP server holds the
/// flock for its whole lifetime, so a CLI incremental index now would race its
/// writes. Best-effort and non-blocking — the run still proceeds (the incremental
/// path shares one index.db through SQLite's own locking, so the worst case is
/// contention, not loss); we only surface the hazard.
///
/// `quiet` suppresses the stderr line ONLY. The probe itself always runs and the
/// finding always reaches `tracing` (same split as `warn_parse_errors`): a flag
/// whose job is to keep hook output clean must not also decide whether a hazard
/// is looked for. Destructive callers use
/// [`lock_index_for_replace`] instead — for them this is a refusal,
/// not a warning.
fn warn_if_index_locked(code_graph_dir: &Path, quiet: bool) {
    if !crate::mcp::server::other_process_holds_index_lock(code_graph_dir) {
        return;
    }
    let lock = code_graph_dir.join("index.lock");
    tracing::warn!(
        "another process holds the index lock at {} — indexing now may race its writes",
        lock.display()
    );
    if !quiet {
        // Same holders as the replace-gate's refusal names: since the CLI takes
        // this lock too, "likely a running MCP server" was no longer true — it
        // sent the user to stop a server that may not exist while a concurrent
        // rebuild-index was the real holder.
        eprintln!(
            "[code-graph] Warning: another process (a running MCP server, or a \
             concurrent rebuild-index / reindex) holds the index lock at {}. \
             Indexing now may race its writes — wait for it to finish, or stop the \
             server, if results look inconsistent.",
            lock.display()
        );
    }
}

/// Gate for commands that REPLACE `index.db` wholesale (`rebuild-index`'s atomic
/// rename, `reindex --from-snapshot`'s unlink).
///
/// A running MCP server holds an open fd on `index.db`. POSIX `rename(2)` /
/// `unlink(2)` swap the directory entry but leave that fd pointing at the old,
/// now-unlinked inode — so every subsequent write from that server (watcher
/// increments, embedding backfill) lands in a deleted file and is lost the
/// moment it closes, while its queries keep answering from the pre-rebuild
/// snapshot. Nothing detects it; the user sees a rebuild that "worked" and a
/// server that never picks it up. The MCP `rebuild_index` tool avoids the same
/// inode trap by rebuilding inside one transaction, and snapshot install avoids
/// it by landing before the DB is opened — this path was the one left unguarded
/// (audit 2026-08-02 P1-3).
///
/// Refusing is therefore the safe default; `--force` is the escape hatch for a
/// user who knows the lock holder is defunct. As with `warn_if_index_locked`,
/// `quiet` gates printing, never probing.
///
/// The gate also TAKES the lock and hands the guard back, instead of only
/// probing it. Probing alone excluded nothing among CLI runs: two concurrent
/// `rebuild-index --confirm` invocations both saw a free lock, both entered the
/// temp-file sweep (which deletes ANY `index.db.rebuild-*`, by design, to clear
/// crashed runs), and the loser died with a bare SQLite `disk I/O error` —
/// no corruption, thanks to the atomic rename, but nothing a user could act on
/// (QA ISSUE-008). Holding the lock turns that collision into this function's
/// existing, explanatory refusal. Keep the returned guard alive until the swap
/// is complete; dropping it releases the lock.
///
/// Failure modes are kept asymmetric on purpose: a lock HELD by someone else
/// refuses, but a lock we merely cannot open (read-only dir, exotic FS with no
/// flock) proceeds unlocked exactly as before — this gate must not be the reason
/// a rebuild that used to work stops working.
#[must_use = "the returned guard holds the index lock; dropping it early reopens the race"]
fn lock_index_for_replace(
    code_graph_dir: &Path,
    force: bool,
    quiet: bool,
) -> Result<Option<crate::mcp::server::IndexLockGuard>> {
    let lock = code_graph_dir.join("index.lock");
    let refuse_or_force = |quiet: bool| -> Result<()> {
        if !force {
            anyhow::bail!(
                "another process (a running MCP server, or a concurrent rebuild-index / \
                 reindex) holds the index lock at {}. \
                 Replacing index.db now would leave that process writing into a deleted file — \
                 its indexing and embedding work would be lost silently, and its answers would \
                 stay on the pre-rebuild index until it restarts.\n  \
                 Stop the MCP server first (end the Claude Code session using this project) \
                 or wait for the other rebuild to finish, then rerun. Pass --force to replace \
                 the index anyway.",
                lock.display()
            );
        }
        tracing::warn!(
            "--force: replacing index.db while another process holds {} — its pending writes will be lost",
            lock.display()
        );
        if !quiet {
            eprintln!(
                "[code-graph] --force: another process holds the index lock at {}. \
                 Replacing the index anyway — that process's pending writes will be lost.",
                lock.display()
            );
        }
        Ok(())
    };

    if crate::mcp::server::other_process_holds_index_lock(code_graph_dir) {
        refuse_or_force(quiet)?;
        return Ok(None);
    }
    // Free a moment ago — claim it, so a rebuild starting now refuses instead of
    // racing us. Losing this acquisition means someone took it in between, which
    // is the same situation as the probe above; anything else (open error) is a
    // non-answer and must not block the run.
    match crate::mcp::server::acquire_index_lock_guard(code_graph_dir) {
        Some(guard) => Ok(Some(guard)),
        None if crate::mcp::server::other_process_holds_index_lock(code_graph_dir) => {
            refuse_or_force(quiet)?;
            Ok(None)
        }
        None => {
            tracing::warn!(
                "could not take the index lock at {} (it is not held by anyone) — proceeding unlocked",
                lock.display()
            );
            Ok(None)
        }
    }
}

pub fn cmd_incremental_index(project_root: &Path, quiet: bool, no_embed: bool) -> Result<()> {
    cmd_incremental_index_opts(project_root, quiet, no_embed, false)
}

/// `cmd_incremental_index` plus the `--json` switch, split out the same way
/// `cmd_health_check_opts` is: the three-positional-bool entry point has a dozen
/// call sites (tests included) that have no opinion about output format.
pub fn cmd_incremental_index_opts(
    project_root: &Path,
    quiet: bool,
    no_embed: bool,
    json: bool,
) -> Result<()> {
    let started = std::time::Instant::now();
    let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
    warn_if_index_locked(&project_root.join(CODE_GRAPH_DIR), quiet);
    // Covers the incremental path too, not just the full-index one inside
    // build_full_index_at: an index created before this existed (or by a user
    // who removed the line) gets the entry back on the next run (audit DB-4).
    crate::utils::gitignore::ensure_code_graph_dir_ignored(project_root);

    // The plugin hooks run this command periodically even when no MCP server is
    // alive — exactly the window where a killed server's indexing-status.json
    // would otherwise pin the statusline at a phantom "indexing N/M" forever.
    // Stale-only: a live server's file has a fresh mtime and is left alone.
    crate::indexer::pipeline::remove_stale_indexing_status(project_root);

    // No existing DB → full index. Delegate to build_full_index_at so the
    // full-index + embed path is shared with rebuild-index (no drift).
    if !db_path.exists() {
        if !quiet {
            eprintln!("No index found, creating full index...");
        }
        let result = build_full_index_at(&db_path, project_root, quiet, no_embed)?;
        if json {
            emit_index_json("full", &result, started);
        }
        return Ok(());
    }

    cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));

    // Open with vec support so embeddings can be stored
    let db = Database::open_with_vec(&db_path)?;

    // Incremental index for the existing database.
    use crate::indexer::pipeline::run_incremental_index;
    let stats = wrap_index_busy(run_incremental_index(&db, project_root, None, None))?;
    if !quiet {
        if stats.files_deleted > 0 {
            eprintln!(
                "Incremental index: {} files updated, {} files removed, {} nodes created",
                stats.files_indexed, stats.files_deleted, stats.nodes_created
            );
        } else {
            eprintln!(
                "Incremental index: {} files updated, {} nodes created",
                stats.files_indexed, stats.nodes_created
            );
        }
    }
    warn_parse_errors(&stats.stats, quiet);

    finish_embedding(&db, quiet, no_embed)?;
    if json {
        emit_index_json("incremental", &stats, started);
    }
    Ok(())
}

/// SQLite sidecar path: `<db>-wal` / `<db>-shm`. Appends the literal suffix to
/// the FULL filename (not an extension swap) — required for temp db names like
/// `index.db.rebuild-<pid>`, whose WAL is `index.db.rebuild-<pid>-wal`.
fn db_sidecar(db_path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

/// Drop the existing index.db (plus WAL/SHM) and trigger a full rebuild via
/// `cmd_incremental_index` (which auto-detects the missing DB and does a full
/// index). Mirrors MCP `rebuild_index` tool semantics.
/// `rebuild-index` arguments (clap-migrated, audit #4).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp rebuild-index",
    about = "Drop and rebuild the index from scratch (requires --confirm)"
)]
pub struct RebuildIndexArgs {
    /// Confirm the destructive drop-and-rebuild (required to proceed)
    #[arg(long)]
    pub confirm: bool,
    /// Suppress progress output
    #[arg(long)]
    pub quiet: bool,
    /// Index structure only and skip embeddings (vectors backfill later).
    #[arg(long)]
    pub no_embed: bool,
    /// Rebuild even while another process holds the index lock (its pending
    /// writes are lost — stop the MCP server instead when you can).
    #[arg(long)]
    pub force: bool,
    /// Print the rebuild's counters as one JSON object on stdout, after the
    /// atomic swap has succeeded (progress stays on stderr).
    #[arg(long)]
    pub json: bool,
}

pub fn cmd_rebuild_index(project_root: &Path, args: RebuildIndexArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let confirm = args.confirm;
    let quiet = args.quiet;
    let no_embed = args.no_embed;
    // `--confirm` is a business-logic confirmation gate, NOT a clap-required arg:
    // a missing confirm is a deliberate exit-1 anyhow bail (not a parse error),
    // preserving the prior contract (test_cli_rebuild_index_requires_confirm).
    if !confirm {
        anyhow::bail!(
            "rebuild-index drops the existing index and re-parses every file. \
             Pass --confirm to proceed. Use `incremental-index` for incremental updates."
        );
    }
    // Destructive-op sanity: refuse to operate on degenerate roots. Guards against
    // a resolve_project_root regression that could return `/` or `""`.
    if project_root.as_os_str().is_empty() || project_root == Path::new("/") {
        anyhow::bail!(
            "refusing to rebuild-index with degenerate project_root ({}). \
             Run from within a git-tracked project directory.",
            project_root.display()
        );
    }
    let code_graph_dir = project_root.join(CODE_GRAPH_DIR);
    let db_path = code_graph_dir.join("index.db");
    // Before any work: refuse to rename over an index another process has open,
    // and hold the lock for the whole rebuild so a concurrent one refuses here
    // rather than colliding in the temp sweep below. `_index_lock` must stay
    // bound to the end of the function — `let _ = …` would drop it immediately.
    let _index_lock = lock_index_for_replace(&code_graph_dir, args.force, quiet)?;

    // Atomic rebuild: build the fresh index into a temp file in the SAME dir,
    // then rename it over index.db in one syscall. Concurrent readers (a second
    // CLI invocation, or the MCP server reopening) therefore always see a
    // COMPLETE index — the old one until the rename, the new one after — instead
    // of the empty/partial window the old "remove index.db then rebuild in place"
    // left open for the entire (multi-second on large repos) rebuild.
    let temp_path = code_graph_dir.join(format!("index.db.rebuild-{}", std::process::id()));
    let temp_files = [
        temp_path.clone(),
        db_sidecar(&temp_path, "-wal"),
        db_sidecar(&temp_path, "-shm"),
    ];
    let remove_all = |paths: &[std::path::PathBuf]| {
        for p in paths {
            if p.exists() {
                let _ = std::fs::remove_file(p);
            }
        }
    };
    // Clear leftover temp files from previously-killed rebuilds (ANY pid). The
    // `index.db.rebuild-<pid>` prefix also matches their `-wal`/`-shm` sidecars.
    // A concurrent rebuild's in-progress temp could be swept too — that only
    // makes the other run's final rename fail (an error, never corruption);
    // concurrent rebuild-index runs were never supported.
    if let Ok(entries) = std::fs::read_dir(&code_graph_dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("index.db.rebuild-")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Build into the temp file. On failure, drop the temp and keep the existing
    // index.db intact — the rename below is the only mutation of the live index,
    // so a failed rebuild no longer leaves the user with NO index (the old
    // remove-first path did).
    let result = match build_full_index_at(&temp_path, project_root, quiet, no_embed) {
        Ok(result) => result,
        Err(e) => {
            remove_all(&temp_files);
            return Err(e);
        }
    };
    // The temp DB closed cleanly inside build_full_index_at (WAL checkpointed);
    // remove any residual temp -wal/-shm so the renamed file is self-contained.
    remove_all(&temp_files[1..]);

    // Drop the OLD index's -wal/-shm BEFORE the rename: afterwards a stale
    // index.db-wal would be (wrongly) replayed by SQLite onto the NEW index.db.
    // The old WAL is discardable here — we're replacing the whole index. A reader
    // in the sub-millisecond gap sees the old index.db (a valid, complete file).
    remove_all(&[db_sidecar(&db_path, "-wal"), db_sidecar(&db_path, "-shm")]);

    // Atomic swap (temp and index.db share .code-graph/ → POSIX rename is atomic).
    std::fs::rename(&temp_path, &db_path)?;
    // After the swap, never before: until the rename lands, the counters describe
    // a temp file the user cannot query.
    if args.json {
        emit_index_json("rebuild", &result, started);
    }
    Ok(())
}

// Internal notes — `//` (not `///`) so clap leaves them out of `--help`: --json and
// --format coexist for back-compat (--json is shorthand for `--format json` and wins
// when both are given); resolved_format() below collapses them into the single `&str`
// the handler consumes, so cmd_health_check's signature and its JSON/oneline branches
// stay untouched (plan §2 item 14).
/// CLI arguments for the `health-check` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp health-check",
    about = "Query index status (nodes/edges/files, freshness, embedding coverage)"
)]
pub struct HealthCheckArgs {
    /// JSON output (shorthand for --format json; wins when both are set)
    #[arg(long)]
    pub json: bool,
    /// Output format: oneline (default) or json
    #[arg(long)]
    pub format: Option<String>,
    /// Run `PRAGMA quick_check` regardless of index size (see INTEGRITY_PRAGMA_MAX_BYTES)
    #[arg(long)]
    pub deep: bool,
}

impl HealthCheckArgs {
    /// Collapse `--json`/`--format` into the handler's format string.
    /// `--json` takes precedence; absent both, defaults to "oneline".
    /// Unrecognized `--format` values fall through to the handler's oneline branch
    /// (preserved from the prior hand-parser: only "json" was special-cased).
    pub fn resolved_format(&self) -> &str {
        if self.json {
            "json"
        } else {
            self.format.as_deref().unwrap_or("oneline")
        }
    }
}

/// Recording-side state of the recommend→use conversion metric, surfaced by
/// `stats` and `health-check` so a dark metric is a visible signal rather than
/// silence. `"absent"` = `recommendations.jsonl` missing (the PreToolUse hooks
/// that record recommendations are not active in this project — e.g. it runs a
/// dev `.mcp.json` server with the marketplace plugin disabled, so the metric is
/// structurally dark); `"empty"` = file present, no recommendations yet;
/// `"live"` = recommendations recorded.
pub fn recommendation_metric_state(project_root: &Path) -> &'static str {
    let p = project_root
        .join(CODE_GRAPH_DIR)
        .join("recommendations.jsonl");
    match std::fs::read_to_string(&p) {
        Err(_) => "absent",
        Ok(c) => {
            if aggregate_recommendations_jsonl(&c).total > 0 {
                "live"
            } else {
                "empty"
            }
        }
    }
}

/// Size ceiling above which `health-check` skips `PRAGMA quick_check`.
///
/// quick_check reads every page, and this command is not only a diagnostic — the
/// statusline polls `health-check --format json` on every render, under a
/// 1500 ms inner budget (statusline.js) whose overrun renders the segment as
/// "offline". A/B on this repo's 110 MB index with the same binary: 0.02 s with
/// the pragma skipped, 0.28 s with it — ~2.4 ms/MB, so an unbounded scan would
/// trade a real signal for a broken one on exactly the largest indexes.
///
/// 32 MB keeps the polled path near 80 ms even when the page cache is cold,
/// which is the case that matters: the 2.4 ms/MB above was measured warm, and a
/// quick_check reads EVERY page, so the first render after a cold boot pays disk
/// latency for the whole file. At 128 MB that is the exact shape that makes the
/// statusline segment vanish — trading a real signal for a broken one on the
/// largest indexes, which is what the gate exists to prevent.
///
/// Above the limit the probe reports `"skipped_large"` — visibly absent, never
/// silently, and `doctor` renders that as a skipped row rather than a pass.
/// Full verification stays reachable via `--deep`, which ignores the gate.
/// `doctor` deliberately does NOT pass it: doctor's own budget for this call is
/// 5 s, and a multi-GB index would time out and report a phantom
/// "health-check failed" instead of the integrity answer it went looking for.
const INTEGRITY_PRAGMA_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Cheap read-only integrity probes for `health-check` (audit 2026-08-02 DB-1:
/// the command's `healthy` was `schema_ok && nodes>0 && files>0`, so page-level
/// corruption, an FTS index that stopped tracking `nodes`, and orphaned vectors
/// were all invisible).
///
/// Every field is `Option`: `None` means "could not be measured" (pragma error
/// under writer contention, table absent in a no-vec index), which must never be
/// reported as a fault. Only a quick_check that *ran* and *complained* counts as
/// corruption.
struct IndexIntegrity {
    /// `PRAGMA quick_check` verdict: `"ok"`, SQLite's first complaint, or
    /// `"skipped_large"` when the DB exceeds [`INTEGRITY_PRAGMA_MAX_BYTES`].
    quick_check: Option<String>,
    /// `COUNT(nodes)` − rows the FTS5 index actually holds. Non-zero means
    /// search silently misses (or invents) symbols.
    fts_drift: Option<i64>,
    /// Vectors whose node is gone — dead weight that also skews coverage math.
    orphan_vectors: Option<i64>,
}

impl IndexIntegrity {
    fn probe(conn: &rusqlite::Connection, db_size_bytes: u64, deep: bool) -> Self {
        // Overridable so the skip branch is testable without materializing a
        // 128 MB index (same escape-hatch shape as CODE_GRAPH_RESYNC_BUDGET and
        // CODE_GRAPH_RG_ARGV_BUDGET), and so a user on slow storage can tighten
        // it without waiting for a release.
        let ceiling = std::env::var("CODE_GRAPH_INTEGRITY_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(INTEGRITY_PRAGMA_MAX_BYTES);
        let quick_check = if !deep && db_size_bytes > ceiling {
            Some("skipped_large".to_string())
        } else {
            conn.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))
                .ok()
        };

        // NOT `COUNT(*) FROM nodes_fts`: `nodes_fts` is an EXTERNAL-CONTENT table
        // (`content='nodes'`, schema.rs:64), so counting it reads through to
        // `nodes` and can only ever return `COUNT(nodes)` — a control that
        // cannot fail, which is how a drift check gets shipped that never
        // detects drift. `nodes_fts_docsize` is the FTS5 shadow table with one
        // row per document the index really holds, maintained by the triggers,
        // so it moves independently of the content table.
        let fts_drift = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM nodes) - (SELECT COUNT(*) FROM nodes_fts_docsize)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok();

        // Guarded by the sqlite_master probe (same shape as
        // queries::count_nodes_with_vectors): `node_vectors` is a vec0 virtual
        // table, absent from a structure-only index.
        let orphan_vectors = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_vectors'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .filter(|present: &i64| *present > 0)
            .and_then(|_| {
                conn.query_row(
                    "SELECT COUNT(*) FROM node_vectors WHERE node_id NOT IN (SELECT id FROM nodes)",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            });

        Self {
            quick_check,
            fts_drift,
            orphan_vectors,
        }
    }

    /// The one finding severe enough to flip `healthy` — the DB pages themselves
    /// do not read back. A skipped or unmeasurable check is NOT corruption.
    fn corruption_reason(&self) -> Option<&str> {
        match self.quick_check.as_deref() {
            Some("ok") | Some("skipped_large") | None => None,
            Some(msg) => Some(msg),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "quick_check": self.quick_check,
            "fts_drift": self.fts_drift,
            "orphan_vectors": self.orphan_vectors,
        })
    }

    /// One human line, printed on the healthy and unhealthy paths alike so the
    /// two output faces never disagree about what was checked.
    fn to_line(&self) -> String {
        let fmt_count = |v: Option<i64>| match v {
            Some(n) => n.to_string(),
            None => "unavailable".to_string(),
        };
        format!(
            "Integrity: quick_check {} · FTS drift {} · orphan vectors {}",
            self.quick_check.as_deref().unwrap_or("unavailable"),
            fmt_count(self.fts_drift),
            fmt_count(self.orphan_vectors),
        )
    }
}

/// Run health check and print status, including index freshness.
pub fn cmd_health_check(project_root: &Path, format: &str) -> Result<()> {
    cmd_health_check_opts(project_root, format, false)
}

/// `cmd_health_check` with the `--deep` toggle. Kept as a separate entry point so
/// the existing two-argument signature (main.rs, tests) stays intact.
pub fn cmd_health_check_opts(project_root: &Path, format: &str, deep: bool) -> Result<()> {
    // JSON callers (doctor.js, scripts, MCP UIs) need a parseable response
    // even when the index is missing — bailing with a stderr-only anyhow error
    // forces them to grep messages instead of reading JSON fields.
    if format == "json" {
        // Worktree-aware, like every read command: the raw project_root check
        // reported {"healthy":false,"reason":"no_index"} from a linked worktree
        // whose MAIN checkout has a perfectly good index, while the human
        // format (via CliContext::open below) said "OK" — same command, two
        // formats, opposite verdicts, and doctor.js consumes the JSON one, so
        // every worktree showed a phantom broken install (audit 2026-08-02
        // MED-3).
        let db_path = effective_read_root(project_root)
            .join(CODE_GRAPH_DIR)
            .join("index.db");
        if !db_path.exists() {
            let payload = serde_json::json!({
                "healthy": false,
                "reason": "no_index",
                "issue": format!("No index found at {}. Run: code-graph-mcp incremental-index", db_path.display()),
                "nodes": 0,
                "edges": 0,
                "files": 0,
                "watching": false,
                "db_size_bytes": 0,
                "search_mode": "fts_only",
                "embedding_progress": "0/0",
                "embedding_coverage_pct": 0,
                "embedding_status": "unavailable",
                "model_available": cfg!(feature = "embed-model"),
                "snapshot": {"status": "absent", "source_url": null, "source_commit": null, "fetched_at": null, "commit_drift": null},
            });
            println!("{}", serde_json::to_string(&payload)?);
            return Ok(());
        }
    }
    // A corrupt index used to be invisible HERE: the reader open deleted the
    // file and retried on a blank one, so this command reported an empty index
    // (and `quick_check: ok`, on the replacement) while the user's symbols were
    // gone. Readers no longer delete, so the open fails — and this is the one
    // command whose whole job is to say what is wrong with the index, so it
    // renders that as its normal corrupt verdict instead of an opaque error.
    // Same `issue` wording and same `integrity.quick_check` shape as a
    // quick_check failure below, so doctor's `index-corrupt` repair routes off
    // it unchanged.
    let ctx = match CliContext::open(project_root) {
        Ok(c) => c,
        Err(e) if Database::is_corrupt_index_error(&e) => {
            let detail = e.to_string();
            if format == "json" {
                let payload = serde_json::json!({
                    "healthy": false,
                    // `reason` mirrors the `no_index` short-circuit above: a
                    // machine-readable tag so consumers route without grepping
                    // prose. `schema_version` is null and PRESENT rather than
                    // omitted — the database cannot be opened, so the version is
                    // genuinely unknown, and doctor's payload sniffer keys off
                    // this field's existence.
                    "reason": "corrupt",
                    "schema_version": null,
                    "issue": detail,
                    "integrity": {"quick_check": detail, "fts_drift": null, "orphan_vectors": null},
                    "nodes": 0, "edges": 0, "files": 0,
                    "watching": false,
                    "db_size_bytes": 0,
                    "search_mode": "fts_only",
                    "embedding_progress": "0/0",
                    "embedding_coverage_pct": 0,
                    "embedding_status": "unavailable",
                    "model_available": cfg!(feature = "embed-model"),
                    "snapshot": {"status": "absent", "source_url": null, "source_commit": null, "fetched_at": null, "commit_drift": null},
                });
                println!("{}", serde_json::to_string(&payload)?);
            } else {
                eprintln!("UNHEALTHY: {}", detail);
            }
            std::process::exit(1);
        }
        Err(e) => return Err(e),
    };
    // The reader open above is non-destructive: if the on-disk index was built by
    // an older INDEX_VERSION, the data is intact but a rebuild is owed. Report it
    // rather than (as before) silently wiping it on this status poll.
    let index_version_stale = ctx.db.index_version_stale();
    let conn = ctx.db.conn();
    let status = queries::get_index_status(conn, false)?;

    let expected_schema = crate::storage::schema::SCHEMA_VERSION;
    let schema_ok = status.schema_version == expected_schema;
    let has_data = status.nodes_count > 0 && status.files_count > 0;
    // DB-1: `healthy` used to mean only "right schema, non-empty". A database
    // whose pages no longer read back reported OK right up until a query hit the
    // damaged page.
    let integrity = IndexIntegrity::probe(conn, status.db_size_bytes.max(0) as u64, deep);
    let healthy = schema_ok && has_data && integrity.corruption_reason().is_none();

    // Compute index age from last_indexed_at (unix timestamp in seconds)
    let age_str = status.last_indexed_at.map(|ts| {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64 - ts)
            .unwrap_or(0);
        if elapsed < 60 {
            format!("{}s ago", elapsed)
        } else if elapsed < 3600 {
            format!("{}m ago", elapsed / 60)
        } else if elapsed < 86400 {
            format!("{}h ago", elapsed / 3600)
        } else {
            format!("{}d ago", elapsed / 86400)
        }
    });

    // Embedding coverage (works without sqlite-vec loaded)
    let (vectors_done, vectors_total) = queries::count_nodes_with_vectors(conn).unwrap_or((0, 0));
    let coverage_pct: i64 = if vectors_total > 0 {
        (vectors_done as f64 / vectors_total as f64 * 100.0).round() as i64
    } else {
        0
    };
    // Embedding model availability: compile-time feature flag proxy (runtime-cheap,
    // avoids loading weights which would violate CLI's hook-fast contract).
    // NOTE: This diverges from MCP `get_index_status` (which checks runtime
    // `embedding_model.is_some()` — true only after weights load). CLI reports
    // `model_available=true` whenever the binary was built with --features
    // embed-model, even if model weights are missing locally. Cross-check
    // `embedding_progress`/`embedding_status` to tell apart "compiled but not
    // loaded yet" from "compiled and embedding in progress".
    let model_available: bool = cfg!(feature = "embed-model");
    let search_mode = if model_available && vectors_done > 0 {
        "hybrid"
    } else {
        "fts_only"
    };
    let embedding_status = if !model_available {
        "unavailable"
    } else if vectors_done == 0 {
        "pending"
    } else if vectors_done >= vectors_total && vectors_total > 0 {
        "complete"
    } else {
        "partial"
    };
    // Last model-download outcome. Without it, `pending` printed the same
    // optimistic "retry shortly" forever — a permanently-degraded install was
    // indistinguishable from one that just hadn't finished (issue #35).
    #[cfg(feature = "embed-model")]
    let model_download: Option<String> =
        crate::embedding::model::EmbeddingModel::download_state_summary();
    #[cfg(not(feature = "embed-model"))]
    let model_download: Option<String> = None;
    // On-disk model presence, independent of the download marker (the npm
    // plugin installs weights without writing it). Shared by the text arm's
    // pending message and the JSON arm — doctor.js classifies from the JSON,
    // so leaving the field out re-created the "NO download has ever been
    // attempted" contradiction one surface over (ISSUE-011's sibling).
    #[cfg(feature = "embed-model")]
    let model_files_state = crate::embedding::model::EmbeddingModel::model_files_state();
    #[cfg(not(feature = "embed-model"))]
    let model_files_state = "absent";
    // `present` stays the coarse "something is on disk" bool the plugin already
    // reads; `state` says whether this build will actually use it. Advice keyed
    // on `present` alone told an offline user who hand-filled the platform cache
    // to restart the MCP server — a restart that re-downloads instead (review
    // NOTE-7).
    let model_files_present = model_files_state != "absent";

    // Snapshot metadata block — reads keys written by `snapshot install`.
    let snapshot_url =
        crate::snapshot::meta::read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_SOURCE_URL)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
    let snapshot_commit =
        crate::snapshot::meta::read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_SOURCE_COMMIT)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
    let snapshot_fetched_at =
        crate::snapshot::meta::read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_FETCHED_AT)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<i64>().ok());
    let snapshot_status = if snapshot_url.is_some() {
        "present"
    } else {
        "absent"
    };
    // commit_drift: how many local commits landed after the snapshot was taken.
    let commit_drift = snapshot_commit.as_deref().and_then(|c| {
        std::process::Command::new("git")
            // `--` closes the revision list, same as the `ls-files` sibling at
            // :2832 which carries this comment already. Not exploitable here —
            // argv form, and `{c}` is a 40-hex commit id read from the snapshot
            // meta — but a commit-ish that git could read as a pathspec would
            // otherwise change what this counts, and the two call sites in one
            // file disagreeing is how the next one gets written without it.
            .args(["rev-list", "--count", &format!("{c}..HEAD"), "--"])
            .current_dir(project_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .parse::<i64>()
                        .ok()
                } else {
                    None
                }
            })
    });
    let snapshot_block = serde_json::json!({
        "status": snapshot_status,
        "source_url": snapshot_url,
        "source_commit": snapshot_commit,
        "fetched_at": snapshot_fetched_at,
        "commit_drift": commit_drift,
    });

    // Graph-resolution coverage (pending backlog + per-language edge counts).
    // .ok() so a stats failure never breaks the existing health-check contract.
    let resolution = queries::resolution_stats(conn).ok();

    match format {
        "json" => {
            let mut json = serde_json::json!({
                "healthy": healthy,
                "nodes": status.nodes_count,
                "edges": status.edges_count,
                "files": status.files_count,
                "watching": false,
                "schema_version": status.schema_version,
                "db_size_bytes": status.db_size_bytes,
                "search_mode": search_mode,
                "embedding_progress": format!("{}/{}", vectors_done, vectors_total),
                "embedding_coverage_pct": coverage_pct,
                "embedding_status": embedding_status,
                "model_available": model_available,
                "snapshot": snapshot_block,
                "conversion_metric": recommendation_metric_state(project_root),
                "index_version_stale": index_version_stale.is_some(),
                "integrity": integrity.to_json(),
            });
            // Additive field: absent when no download was ever recorded, which
            // is itself the "never attempted" diagnosis.
            if let Some(ref s) = model_download {
                json["model_download"] = serde_json::json!(s);
            }
            json["model_files_present"] = serde_json::json!(model_files_present);
            json["model_files_state"] = serde_json::json!(model_files_state);
            if let Some(ref r) = resolution {
                json["resolution"] = serde_json::to_value(r).unwrap_or(serde_json::Value::Null);
            }
            if let Some(ts) = status.last_indexed_at {
                json["last_indexed_at"] = serde_json::json!(ts);
            }
            if let Some(ref age) = age_str {
                json["index_age"] = serde_json::json!(age);
            }
            // Corruption outranks the other diagnoses: a bad page makes the
            // schema/emptiness verdicts unreliable in the first place.
            if let Some(reason) = integrity.corruption_reason() {
                json["issue"] = serde_json::json!(format!(
                    "database integrity check failed: {}. The index is a rebuildable cache — run: \
                     code-graph-mcp rebuild-index --confirm",
                    reason
                ));
            } else if !schema_ok {
                json["issue"] = serde_json::json!(format!(
                    "schema version mismatch: got {}, expected {}",
                    status.schema_version, expected_schema
                ));
            } else if !has_data {
                json["issue"] = serde_json::json!("index is empty");
            } else if let Some(old) = index_version_stale {
                // Has data + correct schema, but built by an older extractor
                // generation. Usable now (FTS/AST), but results sharpen after a
                // rebuild — which an indexer (reindex / incremental-index / server
                // startup), not this poll, performs.
                json["issue"] = serde_json::json!(format!(
                    "index built by older version (v{} ≠ v{}); rebuild pending",
                    old,
                    crate::domain::INDEX_VERSION
                ));
            }
            println!("{}", json);
            if !healthy {
                std::process::exit(1);
            }
        }
        _ => {
            // Print resolution coverage regardless of healthy, mirroring the JSON arm
            // which attaches the block unconditionally (F12). Healthy keeps `OK:` first.
            let print_resolution = || {
                if let Some(ref r) = resolution {
                    let summary: Vec<String> = r
                        .edges_by_language
                        .iter()
                        .map(|(lang, rels)| format!("{} {}", lang, rels.values().sum::<i64>()))
                        .collect();
                    println!(
                        "Resolution: {} pending; edges by lang: {}",
                        r.pending_unresolved_calls,
                        summary.join(", ")
                    );
                }
            };
            if healthy {
                let age_info = age_str
                    .map(|a| format!(" (updated {})", a))
                    .unwrap_or_default();
                println!(
                    "OK: {} nodes, {} edges, {} files{}",
                    status.nodes_count, status.edges_count, status.files_count, age_info
                );
                println!("Snapshot: {}", snapshot_status);
                println!(
                    "Conversion metric: {}",
                    match recommendation_metric_state(project_root) {
                        "live" => "live (recommendations recorded)",
                        "empty" => "active, no recommendations recorded yet",
                        _ =>
                            "DARK (no recommendations.jsonl — PreToolUse hooks not recording here)",
                    }
                );
                // Vector/embedding status — make a silent FTS5-only degradation visible
                // (the prior gap: text health-check never surfaced search_mode, so a user
                // whose model download failed had no way to see vector was inactive).
                // Model files can be on disk without any download marker (the
                // npm plugin installs them out-of-band) — claiming "no download
                // has been attempted" then contradicts the filesystem. Presence
                // is probed once above (shared with the JSON arm); the marker
                // only disambiguates the truly-absent case ("never attempted"
                // vs "attempted and failed").
                let pending_detail = if model_files_state == "ready" {
                    "model files present but not loaded in this process — vector \
                     search activates in the MCP server (embeddings backfill there)"
                        .to_string()
                } else if model_files_present {
                    // Weights are in the platform cache but carry no current
                    // `.model-id` marker, so the server will re-download rather
                    // than adopt them. Saying "restart" here would send an offline
                    // user through a restart that cannot succeed.
                    "model files are on disk in the cache dir but are not verified as \
                     this build's pinned weights — the MCP server re-downloads them on \
                     next start (needs network). To use hand-placed weights offline, \
                     point CODE_GRAPH_MODEL_DIR at them instead"
                        .to_string()
                } else {
                    match model_download.as_deref() {
                        // "never attempted" is itself actionable — it means the
                        // background download never even started, which is a
                        // different bug from one that started and failed.
                        None => "model not loaded yet; no download has been attempted on this \
                                 machine — start the MCP server, or set CODE_GRAPH_MODEL_DIR to \
                                 a manually populated model dir"
                            .to_string(),
                        Some(s) => format!("model not loaded yet; last download: {}", s),
                    }
                };
                println!(
                    "Search: {} — {}% embedded ({})",
                    if search_mode == "hybrid" {
                        "hybrid (FTS5 + vector)"
                    } else {
                        "FTS5-only (vector inactive)"
                    },
                    coverage_pct,
                    match embedding_status {
                        "unavailable" => "binary built without embed-model feature".to_string(),
                        "pending" => pending_detail,
                        "partial" => "embedding in progress".to_string(),
                        "complete" => "embeddings complete".to_string(),
                        other => other.to_string(),
                    }
                );
                println!("{}", integrity.to_line());
                // DB-3: the JSON face has reported `issue: "…rebuild pending"`
                // for a version-lagging index since it was added, while this one
                // printed a bare "OK" — the same command telling a human and a
                // script opposite things about the same database.
                if let Some(old) = index_version_stale {
                    println!(
                        "Index version: STALE (built by v{} ≠ v{}); results sharpen after a \
                         rebuild — run: code-graph-mcp reindex",
                        old,
                        crate::domain::INDEX_VERSION
                    );
                }
                print_resolution();
            } else if let Some(reason) = integrity.corruption_reason() {
                eprintln!(
                    "UNHEALTHY: database integrity check failed: {}. The index is a rebuildable \
                     cache — run: code-graph-mcp rebuild-index --confirm",
                    reason
                );
                eprintln!("{}", integrity.to_line());
                print_resolution();
                std::process::exit(1);
            } else if !schema_ok {
                eprintln!(
                    "UNHEALTHY: schema version mismatch (got {}, expected {})",
                    status.schema_version, expected_schema
                );
                eprintln!("{}", integrity.to_line());
                print_resolution();
                std::process::exit(1);
            } else {
                eprintln!("UNHEALTHY: index is empty");
                eprintln!("{}", integrity.to_line());
                print_resolution();
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// Canonical name for a CLI *query* subcommand (incl. MCP-name aliases), or
/// None for housekeeping (serve/index/stats/doctor/...). Drives `record_cli_use`:
/// only code-understanding queries count as funnel conversions.
pub fn canonical_query_cmd(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "grep" => "grep",
        "search" | "semantic_code_search" => "search",
        "ast-search" | "ast_search" => "ast-search",
        "callgraph" | "get_call_graph" => "callgraph",
        "impact" | "impact_analysis" => "impact",
        "affected" => "affected",
        "tour" => "tour",
        "map" | "project_map" => "map",
        "overview" | "module_overview" => "overview",
        "show" | "get_ast_node" => "show",
        "trace" | "trace_http_chain" => "trace",
        "deps" | "dependency_graph" => "deps",
        "similar" | "find_similar_code" => "similar",
        "refs" | "find_references" => "refs",
        "dead-code" | "find_dead_code" => "dead-code",
        "centrality" => "centrality",
        "file-impact" => "file-impact",
        _ => return None,
    })
}

/// Append a `{hook:"cli",action:"use",cmd}` line to recommendations.jsonl so the
/// deny→use funnel can see model-initiated CLI conversions (the 2026-06-12 daagu
/// night: 3 post-deny CLI calls, all invisible to the funnel). Mirrors the JS
/// recordRecommendation posture: best-effort, NEVER creates `.code-graph/`
/// (zero footprint outside indexed projects). Hook-internal answer runs set
/// `CODE_GRAPH_INTERNAL=1` and are skipped — they are deliveries, not conversions.
pub fn record_cli_use(project_root: &Path, cmd: &str) {
    if std::env::var("CODE_GRAPH_INTERNAL").ok().as_deref() == Some("1") {
        return;
    }
    let dir = project_root.join(CODE_GRAPH_DIR);
    if !dir.is_dir() {
        return;
    }
    // Opt-in per-project metrics silence. A `.code-graph/.no-metrics` sentinel marks
    // a development/dogfood checkout where the tool's OWN CLI is run for functionality
    // testing, sims, or ad-hoc dev — those runs would otherwise append `use` events
    // to the project's own recommendations.jsonl and read back as genuine consumer
    // adoption (the 2026-06-23 self-pollution: 184 burst rows from in-repo CLI runs).
    // Guards ONLY this recommendations-log write; MCP usage.jsonl (flush_metrics) is
    // untouched, so a dev repo's real MCP tool metrics still flow. Mirrored in JS
    // recommendation-log.js. Reversible: delete the file to re-enable.
    if dir.join(NO_METRICS_SENTINEL).exists() {
        return;
    }
    let line = serde_json::json!({
        "ts": crate::mcp::metrics::iso8601_now(),
        "hook": "cli",
        "action": "use",
        "cmd": cmd,
    });
    let rec_path = dir.join("recommendations.jsonl");
    // Bounded growth: recommendations.jsonl is append-only and (unlike
    // usage.jsonl) written per-event from both here and the JS PreToolUse hooks,
    // so rotate before appending. Same policy/constants as usage.jsonl.
    crate::mcp::metrics::rotate_jsonl_if_over(
        &rec_path,
        crate::mcp::metrics::JSONL_ROTATE_MAX_BYTES,
        crate::mcp::metrics::JSONL_ROTATE_KEEP_BYTES,
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rec_path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{}", line);
    }
}

/// Aggregated per-tool counts across sessions.
pub struct ToolAgg {
    pub n: u64,
    pub total_ms: u64,
    pub err: u64,
    pub max_ms: u64,
    /// Sum of per-session `err_kinds` maps (ErrKind::as_str() → count). Empty for
    /// tools whose errors all predate the err_kinds field. `sum(err_kinds) <= err`;
    /// the gap is pre-feature sessions that logged `err` without a breakdown.
    pub err_kinds: HashMap<String, u64>,
    /// First non-empty `other_sample` seen across sessions — one representative
    /// message for the catch-all `other` bucket, so it is self-explaining.
    pub other_sample: Option<String>,
}

/// Summary produced by `aggregate_usage_jsonl` — drives both human + JSON output.
pub struct UsageSummary {
    pub sessions: u64,
    pub parse_errors: u64,
    pub tools: HashMap<String, ToolAgg>,
    pub search_queries: u64,
    pub search_zero: u64,
    pub search_quality_weighted_sum: f64,
    pub search_fts_only: u64,
    pub search_hybrid: u64,
    pub full_index_count: u64,
    pub full_index_ms_sum: u64,
    pub incr_count: u64,
    pub files_indexed: u64,
    pub versions: std::collections::BTreeSet<String>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
    /// Recommend→use funnel (per-session, window-joined from `recs` field).
    pub sessions_with_deny: u64,
    pub sessions_with_deny_and_cg: u64,
    pub sessions_with_hint: u64,
    pub sessions_with_hint_and_cg: u64,
    /// CLI-conversion legs (recs.cli_use > 0 in the session window) and the
    /// combined "any use" legs (MCP cg tool OR CLI query) — the honest funnel
    /// numerator now that deny→CLI is the proven conversion path.
    pub sessions_with_deny_and_cli: u64,
    pub sessions_with_hint_and_cli: u64,
    pub sessions_with_deny_and_use: u64,
    pub sessions_with_hint_and_use: u64,
}

impl UsageSummary {
    pub fn total_tool_calls(&self) -> u64 {
        self.tools.values().map(|a| a.n).sum()
    }
}

/// Code-understanding cg tools the DENY hook steers grep toward. Housekeeping
/// tools (start/stop_watch, get_index_status, rebuild_index) are excluded so the
/// funnel measures real "used cg instead of grep" substitution, not background
/// bookkeeping. Kept in sync by hand with the `src/mcp/tools.rs` registry.
const CG_QUERY_TOOLS: &[&str] = &[
    "get_call_graph",
    "get_ast_node",
    "module_overview",
    "semantic_code_search",
    "ast_search",
    "find_references",
    "project_map",
    "impact_analysis",
    "trace_http_chain",
    "dependency_graph",
    "find_similar_code",
    "find_dead_code",
    "find_http_route",
    "read_snippet",
];

/// Per-session funnel conversion = `num/denom` rounded to 2 decimals, or JSON
/// `null` when the bucket is empty (avoids a misleading 0.0 for "no data").
fn session_conversion(num: u64, denom: u64) -> serde_json::Value {
    if denom == 0 {
        serde_json::Value::Null
    } else {
        serde_json::json!((num as f64 / denom as f64 * 100.0).round() / 100.0)
    }
}

/// Parse and aggregate `.code-graph/usage.jsonl` content.
/// Pure function: no IO, no panics — malformed lines are counted, not fatal.
/// `last_n`: if Some, keep only the last N records before aggregating.
pub fn aggregate_usage_jsonl(content: &str, last_n: Option<usize>) -> UsageSummary {
    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut parse_errors: u64 = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(v) => records.push(v),
            Err(_) => parse_errors += 1,
        }
    }
    if let Some(n) = last_n {
        if records.len() > n {
            let drop = records.len() - n;
            records.drain(..drop);
        }
    }

    let mut summary = UsageSummary {
        sessions: records.len() as u64,
        parse_errors,
        tools: HashMap::new(),
        search_queries: 0,
        search_zero: 0,
        search_quality_weighted_sum: 0.0,
        search_fts_only: 0,
        search_hybrid: 0,
        full_index_count: 0,
        full_index_ms_sum: 0,
        incr_count: 0,
        files_indexed: 0,
        versions: std::collections::BTreeSet::new(),
        first_ts: None,
        last_ts: None,
        sessions_with_deny: 0,
        sessions_with_deny_and_cg: 0,
        sessions_with_hint: 0,
        sessions_with_hint_and_cg: 0,
        sessions_with_deny_and_cli: 0,
        sessions_with_hint_and_cli: 0,
        sessions_with_deny_and_use: 0,
        sessions_with_hint_and_use: 0,
    };

    for rec in &records {
        if let Some(v) = rec.get("v").and_then(|v| v.as_str()) {
            summary.versions.insert(v.to_string());
        }
        if let Some(ts) = rec.get("ts").and_then(|v| v.as_str()) {
            if summary.first_ts.is_none() {
                summary.first_ts = Some(ts.to_string());
            }
            summary.last_ts = Some(ts.to_string());
        }
        if let Some(tools_obj) = rec.get("tools").and_then(|v| v.as_object()) {
            for (name, s) in tools_obj {
                let agg = summary.tools.entry(name.clone()).or_insert(ToolAgg {
                    n: 0,
                    total_ms: 0,
                    err: 0,
                    max_ms: 0,
                    err_kinds: HashMap::new(),
                    other_sample: None,
                });
                agg.n += s.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.total_ms += s.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
                agg.err += s.get("err").and_then(|v| v.as_u64()).unwrap_or(0);
                let m = s.get("max_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                if m > agg.max_ms {
                    agg.max_ms = m;
                }
                // Merge the per-session err_kinds breakdown (additive; absent on
                // pre-feature rows, so `sum(err_kinds)` may trail `err`).
                if let Some(ek) = s.get("err_kinds").and_then(|v| v.as_object()) {
                    for (kind, cnt) in ek {
                        *agg.err_kinds.entry(kind.clone()).or_insert(0) +=
                            cnt.as_u64().unwrap_or(0);
                    }
                }
                // First `other_sample` wins — one representative message is enough.
                if agg.other_sample.is_none() {
                    if let Some(sample) = s.get("other_sample").and_then(|v| v.as_str()) {
                        agg.other_sample = Some(sample.to_string());
                    }
                }
            }
        }
        if let Some(s) = rec.get("search") {
            let q = s.get("queries").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_queries += q;
            summary.search_zero += s.get("zero").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_fts_only += s.get("fts_only").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.search_hybrid += s.get("hybrid").and_then(|v| v.as_u64()).unwrap_or(0);
            // Per-session avg_quality → re-weight by query count to merge.
            let avg = s.get("avg_quality").and_then(|v| v.as_f64()).unwrap_or(0.0);
            summary.search_quality_weighted_sum += avg * q as f64;
        }
        if let Some(idx) = rec.get("index") {
            if let Some(ms) = idx.get("full_ms").and_then(|v| v.as_u64()) {
                summary.full_index_count += 1;
                summary.full_index_ms_sum += ms;
            }
            summary.incr_count += idx.get("incr").and_then(|v| v.as_u64()).unwrap_or(0);
            summary.files_indexed += idx.get("files").and_then(|v| v.as_u64()).unwrap_or(0);
        }
        // Recommend→use funnel: per-session, did a session that saw a deny/hint
        // (window-joined into the `recs` field at flush) also call a cg query tool?
        let used_cg = rec
            .get("tools")
            .and_then(|v| v.as_object())
            .is_some_and(|tools| {
                CG_QUERY_TOOLS.iter().any(|t| {
                    tools
                        .get(*t)
                        .and_then(|s| s.get("n"))
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0)
                        > 0
                })
            });
        if let Some(recs) = rec.get("recs") {
            let deny = recs.get("deny").and_then(|v| v.as_u64()).unwrap_or(0);
            let hint = recs.get("hint").and_then(|v| v.as_u64()).unwrap_or(0);
            // CLI query runs window-joined into the session (additive v0.49 field).
            let used_cli = recs.get("cli_use").and_then(|v| v.as_u64()).unwrap_or(0) > 0;
            let used_any = used_cg || used_cli;
            if deny > 0 {
                summary.sessions_with_deny += 1;
                if used_cg {
                    summary.sessions_with_deny_and_cg += 1;
                }
                if used_cli {
                    summary.sessions_with_deny_and_cli += 1;
                }
                if used_any {
                    summary.sessions_with_deny_and_use += 1;
                }
            }
            if hint > 0 {
                summary.sessions_with_hint += 1;
                if used_cg {
                    summary.sessions_with_hint_and_cg += 1;
                }
                if used_cli {
                    summary.sessions_with_hint_and_cli += 1;
                }
                if used_any {
                    summary.sessions_with_hint_and_use += 1;
                }
            }
        }
    }
    summary
}

/// Aggregate of `.code-graph/recommendations.jsonl` — the JS PreToolUse hooks'
/// record of how often a code-graph tool was RECOMMENDED (raw-grep hint/deny,
/// read-fanout hint). Joined against actual tool calls in `stats` to surface the
/// real-session conversion rate the synthetic routing_bench oracle can't see.
#[derive(Default)]
pub struct RecommendationSummary {
    /// Recommendation events only (deny/hint/bypass…) — `action:"use"` lines are
    /// conversions, counted in `cli_uses` instead.
    pub total: u64,
    /// "hint" / "deny" / "bypass" → count
    pub by_action: std::collections::BTreeMap<String, u64>,
    /// "grep" / "read" → count
    pub by_hook: std::collections::BTreeMap<String, u64>,
    /// Model-initiated `code-graph-mcp <query>` runs (action:"use").
    pub cli_uses: u64,
    /// Deny segmentation: answered:true denies satisfied the need in-place, so a
    /// low deny→use read is EXPECTED for them; only static (unanswered) denies
    /// ask the model to convert. Pre-v0.47 denies lack the field → unanswered.
    pub deny_answered: u64,
    pub deny_unanswered: u64,
    /// Outcome proxy ("search-decay"): silent grep/read allows recorded by the
    /// PreToolUse hooks (action:"observe"), so the model's raw fan-out is visible
    /// alongside the deny/hint events.
    pub observe: u64,
    /// SessionStart "live context" injections (action:"live_impact", hook:"session"):
    /// the recent-change blast radius pushed at session start (v0.63). A separate
    /// counter — like observe/use it is NOT a tool-call recommendation, so it stays
    /// out of `total`/`by_action`. Surfaced in stats so the feature isn't dark.
    pub live_impact: u64,
    /// Per-mode breakdown of PostToolUse inject events (mode:"callgraph"|"grep"|"show").
    /// A SUB-breakdown of the `inject` count in `by_action` (NOT double-counted; an
    /// inject event still lands in total/by_action once). callgraph is the only inject
    /// mode with marginal value over the model's own grep — it carries the cross-file
    /// caller tree; grep/show echo the hits the model already saw (2026-06-26 audit:
    /// 0 CONSUMED). Surfaced so the callgraph-vs-echo mix is directly readable — the
    /// lever is raising callgraph's share (widening its eligibility). Injects recorded
    /// without a `mode` (pre-v0.75) are absent from every bucket, so the map may sum to
    /// less than `by_action["inject"]`.
    pub inject_by_mode: std::collections::BTreeMap<String, u64>,
    /// Of `deny_answered` (cg delivered a grep answer in-place), how many were
    /// IMMEDIATELY followed by ANY grep/read event. Computed in append
    /// (chronological) order; a single-user-sequential approximation (truly
    /// concurrent sessions interleave in the shared file). NOTE: this raw count
    /// is NOT a failure rate — it lumps together healthy drill-down that cg also
    /// answered (`sustained_after_answer`), file-reads acting on the answer
    /// (observe), and genuine fall-through (`fallthrough_after_answer`). Only the
    /// last means the inline answer was insufficient. Read `fallthrough_after_answer`
    /// for the honest signal.
    pub researched_after_answer: u64,
    /// Subset of `researched_after_answer`: the follow-up search was ITSELF
    /// answered by cg (an answered deny / delivered hint) AND searched a DIFFERENT
    /// pattern, so the model drilled deeper and cg kept up — each step replaced
    /// another raw grep with an answer. A win, not a miss. A verbatim re-grep of
    /// the SAME pattern is excluded (scored as fall-through) when the hook recorded
    /// the pattern; pre-fix events without a pattern field still land here (the old
    /// upper-bound behavior, back-compatible).
    pub sustained_after_answer: u64,
    /// Subset of `researched_after_answer`: the follow-up was a search cg could
    /// NOT satisfy (static deny / advisory-only hint / bypass). THIS is the honest
    /// "the inline answer was insufficient and cg couldn't help the next step"
    /// signal — the actual fan-out leak. `observe` (a file read acting on the
    /// delivered answer) is excluded from both subsets: it is not a search cg failed.
    pub fallthrough_after_answer: u64,
    /// Subset of `researched_after_answer` EXCLUDED from both sustained AND
    /// fall-through: the follow-up search is itself a NULL signal about the prior
    /// answer's sufficiency. Two shapes: `fallthrough:"no-hits"` (cg ran the next
    /// grep and found nothing — necessarily a DIFFERENT query, since a verbatim
    /// re-grep of the answered pattern would re-hit the prior answer's lines, so
    /// 0 hits ⇒ a new search, not "the answer was wrong") and `reason:"unavailable"`
    /// (cg CLI couldn't run — infra, orthogonal to answer quality). Counting either
    /// as fall-through over-states "answer insufficient" — the same over-count class
    /// as lumping in drill-down/observe (v0.64). Tracked so the named subsets of
    /// `researched_after_answer` stay legible.
    pub followup_inconclusive: u64,
    /// PostToolUse inject events that did NOT deliver (answered:false — cg ran but
    /// hit no-hits/no-binary/unavailable, recorded with `fallthrough`/`reason`).
    /// Before this counter the non-hits path recorded NOTHING, so the funnel could
    /// not distinguish "hook dark (binary missing)" from "ran, genuinely empty"
    /// (disclosure-gap class, roadmap 2026-07-18 §1.6). A sub-breakdown of
    /// `by_action["inject"]`; `by_action["inject"] - inject_skipped` = delivered.
    pub inject_skipped: u64,
}

/// Parse and aggregate `recommendations.jsonl` content. Pure: no IO, no panics —
/// malformed lines are skipped silently (telemetry, not a contract surface).
pub fn aggregate_recommendations_jsonl(content: &str) -> RecommendationSummary {
    let mut s = RecommendationSummary::default();
    // Outcome-proxy state: `armed` means the previous tool event was an answered
    // deny (cg satisfied the grep in-place); the next grep/read event of ANY
    // action is a re-search — the inline answer wasn't enough. Lines are appended
    // chronologically so a single forward pass suffices.
    let mut armed = false;
    // Pattern of the armed answered deny (when the hook recorded one). A follow-up
    // search carrying the SAME pattern is a verbatim re-grep = the inline answer was
    // ignored/insufficient (a real fall-through), NOT a deeper drill-down. Absent on
    // pre-fix events → falls back to the answered/observe split (back-compatible).
    let mut armed_pattern: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let action = v.get("action").and_then(|x| x.as_str());
        let hook = v.get("hook").and_then(|x| x.as_str());

        // Re-search detection runs on every tool event, before action bucketing.
        let is_search_event = matches!(hook, Some("grep") | Some("read"))
            && matches!(
                action,
                Some("deny") | Some("hint") | Some("bypass") | Some("observe") | Some("inject")
            );
        if armed {
            if is_search_event {
                s.researched_after_answer += 1;
                let follow_pattern = v.get("pattern").and_then(|x| x.as_str());
                // Split the follow-up honestly. Same-pattern takes precedence: a
                // verbatim re-grep of the SAME denied pattern (re-deny after the
                // cooldown, or a grep observe within it) means the inline answer
                // didn't end the hunt for THAT query → fall-through, NOT a win and
                // NOT "acting on the answer". Otherwise: observe = a file read
                // acting on the delivered answer; answered:true = cg ALSO answered
                // the next (deeper) step (sustained drill-down, a win); anything
                // else (static deny / advisory hint / bypass) = cg fell through.
                // The is_some() guard keeps absent==absent (pre-fix events) OUT of
                // the same-pattern branch.
                let follow_inconclusive = v.get("fallthrough").and_then(|x| x.as_str())
                    == Some("no-hits")
                    || v.get("reason").and_then(|x| x.as_str()) == Some("unavailable");
                if armed_pattern.is_some() && armed_pattern.as_deref() == follow_pattern {
                    s.fallthrough_after_answer += 1;
                } else if follow_inconclusive {
                    // The follow-up is a NULL signal about the prior answer: `no-hits`
                    // = cg ran the next grep and found nothing (a verbatim re-grep of
                    // the answered pattern would have re-hit it, so 0 hits ⇒ a NEW
                    // query, not "the answer was wrong"); `unavailable` = cg CLI
                    // couldn't run (infra). Neither means the inline answer was
                    // insufficient → exclude from fall-through (same over-count class
                    // as the observe/drill-down split). Ordered after the same-pattern
                    // check so a verbatim re-grep still scores as fall-through.
                    s.followup_inconclusive += 1;
                } else if action == Some("observe") {
                    // acting on the answer — neither sustained nor fall-through
                } else if v.get("answered").and_then(|x| x.as_bool()) == Some(true) {
                    s.sustained_after_answer += 1;
                } else {
                    s.fallthrough_after_answer += 1;
                }
            }
            armed = false; // only the IMMEDIATELY-next tool event counts
            armed_pattern = None;
        }

        // observe / use are not recommendation events: count separately, like cli use.
        match action {
            Some("use") => {
                s.cli_uses += 1;
                continue;
            }
            Some("observe") => {
                s.observe += 1;
                continue;
            }
            Some("live_impact") => {
                s.live_impact += 1;
                continue;
            }
            _ => {}
        }
        s.total += 1;
        if let Some(a) = action {
            *s.by_action.entry(a.to_string()).or_insert(0) += 1;
            if a == "deny" {
                if v.get("answered").and_then(|x| x.as_bool()) == Some(true) {
                    s.deny_answered += 1;
                    armed = true; // watch the next event for a re-search
                                  // Remember the pattern (if recorded) so a verbatim re-grep of it
                                  // is scored as fall-through, not sustained.
                    armed_pattern = v.get("pattern").and_then(|x| x.as_str()).map(String::from);
                } else {
                    s.deny_unanswered += 1;
                }
            } else if a == "inject" {
                // Compound-grep PostToolUse: an answered inject delivered cg's
                // AST-aware view of a grep that rode inside a compound command
                // (so PreToolUse never denied it). It arms the funnel exactly like
                // an answered deny — the next search event scores whether the
                // inline inject sufficed (inject→fallthrough) or cg also answered
                // the deeper step (sustained), parallel to deny→fallthrough.
                // answered:false injects (v0.99.1+) are the non-delivering path
                // (no-hits / no-binary / unavailable) — counted in inject_skipped
                // below, and they do NOT arm the funnel. Still land in
                // total/by_action via the generic map above.
                // Sub-breakdown by payload mode (best-effort: pre-v0.75 injects have
                // no `mode` → uncounted here, still counted in by_action["inject"]).
                if let Some(mode) = v.get("mode").and_then(|x| x.as_str()) {
                    *s.inject_by_mode.entry(mode.to_string()).or_insert(0) += 1;
                }
                if v.get("answered").and_then(|x| x.as_bool()) == Some(true) {
                    armed = true;
                    armed_pattern = v.get("pattern").and_then(|x| x.as_str()).map(String::from);
                } else if v.get("answered").and_then(|x| x.as_bool()) == Some(false) {
                    // Explicit answered:false only — pre-v0.99.1 injects lack the
                    // field but were ALWAYS delivered (recorded only on hits).
                    s.inject_skipped += 1;
                }
            }
        }
        if let Some(h) = hook {
            *s.by_hook.entry(h.to_string()).or_insert(0) += 1;
        }
    }
    s
}

// Idiomatic-flavor UX change — `//` (not `///`) so it stays out of clap `--help`:
// `--last <non-number>` is now a hard parse error (exit 2, clap message) instead of
// the prior warn-and-show-all fallback.
/// CLI arguments for the `stats` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp stats",
    about = "Aggregate session metrics from .code-graph/usage.jsonl"
)]
pub struct StatsArgs {
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Limit to the last N sessions (default: all)
    #[arg(long)]
    pub last: Option<usize>,
}

/// Numeric (semver) sort key for a version string. `versions` is stored in a
/// BTreeSet, which orders lexically — so "0.5.40" sorted AFTER "0.32.2". Parse the
/// leading digits of the first three dot-separated components so ordering is by
/// (major, minor, patch); non-numeric/missing components fall back to 0, keeping
/// the sort total and panic-free for odd version strings.
fn version_sort_key(v: &str) -> (u64, u64, u64) {
    let mut parts = v.split('.').map(|part| {
        part.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// One-line stderr hint after a symbol-name miss. Query-time freshness
/// (`refresh_files_if_stale`) can only re-sync files the symbol is already
/// indexed in — a symbol ADDED since the last index has no file to refresh, so
/// a lookup miss is indistinguishable from "doesn't exist" without this hint.
fn hint_symbol_maybe_unindexed(symbol: &str) {
    eprintln!(
        "[code-graph] If '{}' was added recently, the index may be stale — run \
         `code-graph-mcp incremental-index` and retry.",
        symbol
    );
}

/// Pluralize a count for human-readable output: `1 file`, `0 files`, `2 files`.
/// Avoids the "1 files"/"1 lines" grammar glitch on single-item results (common
/// for single-file modules and one-line dead-code candidates). Naive `+s` only —
/// callers pass already-plural-friendly stems (file, line, symbol).
fn plural(n: i64, singular: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {singular}s")
    }
}

/// Print aggregated session metrics from `.code-graph/usage.jsonl`.
/// Diagnostic: shows which tools you actually use + search/index activity.
/// `--last N` limits to the most recent N sessions. `--json` emits structured output.
pub fn cmd_stats(project_root: &Path, args: StatsArgs) -> Result<()> {
    // Broken-pipe safety (mirrors grep's `test_cli_grep_sigpipe_graceful`
    // contract): route every stdout write through this macro so an early-closing
    // reader (`stats | head`, a `| less` the user quits) exits 0 silently instead
    // of panicking on EPIPE the way raw `println!` does — that surfaced as a
    // SIGABRT/134 crash with a `failed printing to stdout: Broken pipe` panic.
    macro_rules! sout {
        ($($a:tt)*) => {
            if let Err(e) = writeln!(std::io::stdout(), $($a)*) {
                if e.kind() == std::io::ErrorKind::BrokenPipe { grep_exit(0); }
                return Err(e.into());
            }
        };
    }
    let json_mode = args.json;
    let last_n = args.last;

    let usage_path = project_root.join(CODE_GRAPH_DIR).join("usage.jsonl");
    if !usage_path.exists() {
        if json_mode {
            sout!(
                "{}",
                serde_json::json!({
                    "sessions": 0,
                    "tools": {},
                    "note": format!("no usage data at {}", usage_path.display()),
                })
            );
        } else {
            eprintln!("No usage data yet at {}", usage_path.display());
            eprintln!("Run an MCP session first (sessions flush metrics on EOF).");
        }
        return Ok(());
    }

    let content = std::fs::read_to_string(&usage_path)?;
    let summary = aggregate_usage_jsonl(&content, last_n);

    // Conversion metric: cg tool calls vs PreToolUse recommendations. The JSONL
    // has no per-session boundary, so it is aggregated whole (last_n applies only
    // to usage sessions). Absent file → empty (default) summary.
    let rec_path = project_root
        .join(CODE_GRAPH_DIR)
        .join("recommendations.jsonl");
    let rec_exists = rec_path.exists();
    let recs = std::fs::read_to_string(&rec_path)
        .ok()
        .map(|c| aggregate_recommendations_jsonl(&c))
        .unwrap_or_default();
    // Recording-side state of the conversion metric, made explicit so a dark
    // metric (file absent → PreToolUse hooks not recording here) is never
    // silently indistinguishable from "feature absent" or "no data yet".
    let rec_state = if recs.total > 0 || recs.cli_uses > 0 {
        "live"
    } else if rec_exists {
        "empty"
    } else {
        "absent"
    };

    if summary.sessions == 0 {
        if json_mode {
            sout!("{}", serde_json::json!({"sessions": 0, "tools": {}}));
        } else {
            eprintln!("No sessions recorded.");
        }
        return Ok(());
    }

    if json_mode {
        let tools_json: serde_json::Map<String, serde_json::Value> = summary.tools.iter().map(|(name, a)| {
            let avg = a.total_ms.checked_div(a.n).unwrap_or(0);
            let mut o = serde_json::json!({
                "n": a.n, "total_ms": a.total_ms, "avg_ms": avg, "err": a.err, "max_ms": a.max_ms,
            });
            // Additive: only present when the tool logged a classified error.
            if !a.err_kinds.is_empty() {
                o["err_kinds"] = serde_json::json!(a.err_kinds);
            }
            if let Some(sample) = &a.other_sample {
                o["other_sample"] = serde_json::json!(sample);
            }
            (name.clone(), o)
        }).collect();
        let avg_q = if summary.search_queries > 0 {
            summary.search_quality_weighted_sum / summary.search_queries as f64
        } else {
            0.0
        };
        let full_avg = summary
            .full_index_ms_sum
            .checked_div(summary.full_index_count)
            .unwrap_or(0);
        let mut sorted_versions: Vec<String> = summary.versions.iter().cloned().collect();
        sorted_versions.sort_by_key(|v| version_sort_key(v));
        let mut stats_json = serde_json::json!({
            "sessions": summary.sessions,
            "parse_errors": summary.parse_errors,
            "versions": sorted_versions,
            "first_ts": summary.first_ts,
            "last_ts": summary.last_ts,
            "total_tool_calls": summary.total_tool_calls(),
            "live_tools": crate::domain::LIVE_MCP_TOOLS,
            "tools": tools_json,
            "search": {
                "queries": summary.search_queries,
                "zero": summary.search_zero,
                "avg_quality": (avg_q * 100.0).round() / 100.0,
                "fts_only": summary.search_fts_only,
                "hybrid": summary.search_hybrid,
            },
            "index": {
                "full_count": summary.full_index_count,
                "full_avg_ms": full_avg,
                "incr_count": summary.incr_count,
                "files_indexed": summary.files_indexed,
            },
            "recommendations": {
                "state": rec_state,
                "total": recs.total,
                "by_action": recs.by_action.iter().map(|(k, v)| (k.clone(), serde_json::json!(v)))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
                "by_hook": recs.by_hook.iter().map(|(k, v)| (k.clone(), serde_json::json!(v)))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
                // Sub-breakdown of by_action["inject"] by payload mode. callgraph =
                // the marginal-value cross-file tree; grep/show = redundant echo.
                "inject_by_mode": recs.inject_by_mode.iter().map(|(k, v)| (k.clone(), serde_json::json!(v)))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
                "cg_tool_calls": summary.total_tool_calls(),
                "cli_uses": recs.cli_uses,
                "deny_answered": recs.deny_answered,
                "deny_unanswered": recs.deny_unanswered,
                // Outcome proxy: observe = silent grep/read allows recorded by the
                // hooks. re_search_rate = fraction of answered denies immediately
                // followed by ANY grep/read — kept for back-compat, but it OVER-counts
                // insufficiency (it includes drill-down cg also answered + file-reads).
                // fallthrough_rate is the honest "inline answer insufficient" fraction:
                // the follow-up was a search cg could NOT satisfy. Both null until an
                // answered deny exists to divide by.
                "observe": recs.observe,
                "live_impact": recs.live_impact,
                "researched_after_answer": recs.researched_after_answer,
                "re_search_rate": if recs.deny_answered > 0 {
                    serde_json::json!((recs.researched_after_answer as f64 / recs.deny_answered as f64 * 100.0).round() / 100.0)
                } else { serde_json::Value::Null },
                "sustained_after_answer": recs.sustained_after_answer,
                "fallthrough_after_answer": recs.fallthrough_after_answer,
                "followup_inconclusive": recs.followup_inconclusive,
                "fallthrough_rate": if recs.deny_answered > 0 {
                    serde_json::json!((recs.fallthrough_after_answer as f64 / recs.deny_answered as f64 * 100.0).round() / 100.0)
                } else { serde_json::Value::Null },
                // tool_calls / recommendations: two independent populations, so
                // this is an activity/volume ratio, NOT a recommend→use rate. The
                // real conversion is funnel.deny_conversion / hint_conversion.
                "tool_calls_per_rec": if recs.total > 0 {
                    (summary.total_tool_calls() as f64 / recs.total as f64 * 100.0).round() / 100.0
                } else { 0.0 },
                // Per-session deny→use / hint→use funnel (window-joined attribution).
                // v0.49: *_conversion is ANY-use (MCP cg tool OR CLI query) — the
                // deny→CLI leg is the proven conversion path; *_then_cg / *_then_cli
                // keep the legs separable.
                "funnel": {
                    "deny_sessions": summary.sessions_with_deny,
                    "deny_then_cg": summary.sessions_with_deny_and_cg,
                    "deny_then_cli": summary.sessions_with_deny_and_cli,
                    "deny_then_use": summary.sessions_with_deny_and_use,
                    "deny_conversion": session_conversion(summary.sessions_with_deny_and_use, summary.sessions_with_deny),
                    "hint_sessions": summary.sessions_with_hint,
                    "hint_then_cg": summary.sessions_with_hint_and_cg,
                    "hint_then_cli": summary.sessions_with_hint_and_cli,
                    "hint_then_use": summary.sessions_with_hint_and_use,
                    "hint_conversion": session_conversion(summary.sessions_with_hint_and_use, summary.sessions_with_hint),
                },
            },
        });
        // Assigned post-hoc: the json! literal above is at the macro recursion
        // limit — one more inline key fails to expand (`json_internal!` recursion).
        // Non-delivering injects (answered:false: no-hits/no-binary/unavailable) —
        // distinguishes "hook ran, nothing to say" from "hook dark" (§1.6).
        stats_json["recommendations"]["inject_skipped"] = serde_json::json!(recs.inject_skipped);
        sout!("{}", stats_json);
    } else {
        let mut versions: Vec<&str> = summary.versions.iter().map(|s| s.as_str()).collect();
        versions.sort_by_key(|v| version_sort_key(v));
        sout!(
            "Sessions: {}   versions: {}   {} → {}",
            summary.sessions,
            if versions.is_empty() {
                "-".into()
            } else {
                versions.join(",")
            },
            summary.first_ts.as_deref().unwrap_or("-"),
            summary.last_ts.as_deref().unwrap_or("-"),
        );
        sout!("Total tool calls: {}", summary.total_tool_calls());
        if summary.parse_errors > 0 {
            sout!(
                "(warning: {} malformed line(s) skipped)",
                summary.parse_errors
            );
        }
        sout!();

        let mut sorted: Vec<(&String, &ToolAgg)> = summary.tools.iter().collect();
        sorted.sort_by_key(|(_, a)| std::cmp::Reverse(a.n));

        if sorted.is_empty() {
            sout!("(no tool calls recorded)");
        } else {
            sout!(
                "{:<28} {:>6} {:>10} {:>6} {:>8}",
                "Tool",
                "n",
                "avg_ms",
                "err",
                "max_ms"
            );
            sout!("{}", "-".repeat(62));
            let mut any_legacy = false;
            for (name, agg) in &sorted {
                let avg = agg.total_ms.checked_div(agg.n).unwrap_or(0);
                // Mark tool names no longer in the live tools/list surface (folded
                // or hidden, recorded by older sessions) so the table doesn't
                // commingle historical names with the current live set.
                let legacy = !crate::domain::LIVE_MCP_TOOLS.contains(&name.as_str());
                if legacy {
                    any_legacy = true;
                }
                let label = if legacy {
                    format!("{name} †")
                } else {
                    name.to_string()
                };
                sout!(
                    "{:<28} {:>6} {:>10} {:>6} {:>8}",
                    label,
                    agg.n,
                    avg,
                    agg.err,
                    agg.max_ms
                );
            }
            if any_legacy {
                sout!("  † not in the current tools/list surface (folded/hidden; from older sessions)");
            }
        }

        // Error-kind breakdown: turn the opaque `err` column into actionable
        // buckets. `not_found` = benign name-miss (model guessed a symbol name);
        // `other` = unclassified — a large `other` is the real signal to chase
        // (expand ErrKind::classify in metrics.rs). A gap between `err` and the
        // sum of kinds is pre-feature sessions (shown as `unrecorded`).
        let mut err_tools: Vec<(&String, &ToolAgg)> =
            summary.tools.iter().filter(|(_, a)| a.err > 0).collect();
        if !err_tools.is_empty() {
            err_tools.sort_by_key(|(_, a)| std::cmp::Reverse(a.err));
            sout!();
            sout!("Error kinds (per tool, most errors first — large `other` = unclassified, investigate):");
            for (name, agg) in &err_tools {
                let mut kinds: Vec<(&String, &u64)> = agg.err_kinds.iter().collect();
                kinds.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
                let classified: u64 = agg.err_kinds.values().sum();
                let mut parts: Vec<String> =
                    kinds.iter().map(|(k, c)| format!("{} {}", k, c)).collect();
                if agg.err > classified {
                    parts.push(format!("unrecorded {}", agg.err - classified));
                }
                let label = if !crate::domain::LIVE_MCP_TOOLS.contains(&name.as_str()) {
                    format!("{name} †")
                } else {
                    name.to_string()
                };
                sout!("  {:<26} {} err = {}", label, agg.err, parts.join(" · "));
                // Surface the sampled `other` message so the bucket self-explains.
                if let Some(sample) = &agg.other_sample {
                    sout!("  {:<26}   ↳ other e.g. {:?}", "", sample);
                }
            }
        }

        if summary.search_queries > 0 {
            let zero_pct =
                (summary.search_zero as f64 / summary.search_queries as f64 * 100.0).round() as u64;
            let avg_q = summary.search_quality_weighted_sum / summary.search_queries as f64;
            sout!();
            sout!(
                "Search: {} queries, {} zero-result ({}%), hybrid/fts {}/{}, avg quality {:.2}",
                summary.search_queries,
                summary.search_zero,
                zero_pct,
                summary.search_hybrid,
                summary.search_fts_only,
                avg_q
            );
        }

        if summary.full_index_count > 0 || summary.incr_count > 0 {
            let full_part = match summary
                .full_index_ms_sum
                .checked_div(summary.full_index_count)
            {
                Some(avg) if summary.full_index_count > 0 => format!(" (avg {}ms)", avg),
                _ => String::new(),
            };
            sout!(
                "Index:  {} full{}, {} incremental, {} files indexed",
                summary.full_index_count,
                full_part,
                summary.incr_count,
                summary.files_indexed
            );
        }

        sout!();
        if recs.total > 0 {
            let actions: Vec<String> = recs
                .by_action
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect();
            let ratio = summary.total_tool_calls() as f64 / recs.total as f64;
            sout!(
                "Recommendations: {} emitted ({})",
                recs.total,
                actions.join(", ")
            );
            // Inject payload mix. callgraph is the only mode with marginal value over
            // the model's own grep (cross-file tree); grep/show echo hits it already
            // saw (2026-06-26 audit: 0 CONSUMED). Lead with the callgraph share — the
            // lever is raising it. Modes may sum below by_action.inject (pre-v0.75
            // injects carry no mode).
            if !recs.inject_by_mode.is_empty() {
                let modes: Vec<String> = recs
                    .inject_by_mode
                    .iter()
                    .map(|(k, v)| format!("{v} {k}"))
                    .collect();
                let inj_total: u64 = recs.inject_by_mode.values().sum();
                let cg = recs.inject_by_mode.get("callgraph").copied().unwrap_or(0);
                let cg_pct = if inj_total > 0 {
                    (cg as f64 / inj_total as f64 * 100.0).round() as u64
                } else {
                    0
                };
                sout!("Inject payloads: {} by mode ({}) — callgraph (cross-file, high-value) = {cg_pct}%",
                    inj_total, modes.join(", "));
            }
            if recs.deny_answered + recs.deny_unanswered > 0 {
                // answered:true denies satisfy the need in-place — read their
                // conversion separately or the funnel under-reports the feature.
                sout!(
                    "Denies: {} answered in-place, {} static",
                    recs.deny_answered,
                    recs.deny_unanswered
                );
            }
            if recs.cli_uses > 0 {
                sout!(
                    "CLI uses: {} model-initiated code-graph-mcp queries",
                    recs.cli_uses
                );
            }
            // Outcome proxy ("search-decay"): of the answered denies (cg delivered
            // the grep result in-place), how often did the model immediately keep
            // searching? Lower = the inline answer was enough. observe = the silent
            // grep/read allows that make the fan-out visible.
            if recs.deny_answered > 0 {
                // Honest fan-out signal. The follow-up after an answered deny is
                // one of: cg ALSO answered a DIFFERENT next step (sustained drill-down
                // — a win), a file read acting on the answer (observe), or the inline
                // answer didn't end the hunt — a verbatim re-grep of the same pattern
                // or a search cg couldn't satisfy (fall-through). Only fall-through
                // means the inline answer was insufficient. The raw "kept searching"
                // count lumps all three
                // and reads alarmingly high even when cg wins every step, so lead
                // with fall-through and show the raw count correctly framed.
                let ft_pct = (recs.fallthrough_after_answer as f64 / recs.deny_answered as f64
                    * 100.0)
                    .round() as u64;
                sout!("Fall-through after cg answer: {}/{} answered denies → inline answer didn't end the hunt (verbatim re-grep or a search cg couldn't satisfy) = {ft_pct}% (the real 'answer insufficient' rate; lower is better)",
                    recs.fallthrough_after_answer, recs.deny_answered);
                if recs.sustained_after_answer > 0 {
                    sout!("  ↳ drill-down sustained: {} follow-up search(es) cg also answered — cg kept up, not a miss",
                        recs.sustained_after_answer);
                }
                if recs.followup_inconclusive > 0 {
                    sout!("  ↳ inconclusive (excluded): {} follow-up(s) where cg found nothing (no-hits = a new query) or was unavailable — says nothing about the prior answer",
                        recs.followup_inconclusive);
                }
                let raw_pct = (recs.researched_after_answer as f64 / recs.deny_answered as f64
                    * 100.0)
                    .round() as u64;
                sout!("  ↳ any follow-up (raw): {}/{} = {raw_pct}% — incl. drill-down + file-reads; NOT a failure rate",
                    recs.researched_after_answer, recs.deny_answered);
            }
            if recs.observe > 0 {
                sout!(
                    "Tool observes: {} silent grep/read allows recorded (fan-out timeline)",
                    recs.observe
                );
            }
            // Volume ratio (NOT a conversion rate): cg tool calls and hook
            // recommendations are independent populations, so this only signals
            // activity level. The real recommend→use conversion is the Deny→use /
            // Hint→use funnel printed below.
            sout!("Tool-call volume: {} cg calls / {} recommendations = {ratio:.2} (activity ratio, not conversion)",
                summary.total_tool_calls(), recs.total);
        } else if rec_exists {
            // File present but empty: hooks are wired and recording, just no
            // recommendation has fired yet.
            sout!("Recommendations: 0 recorded (PreToolUse hooks active; conversion metric live, no data yet)");
        } else {
            // No file at all: the recording hooks are not active in this project
            // (e.g. a dev `.mcp.json` server with the marketplace plugin's
            // PreToolUse hooks disabled). Surface the dark state instead of
            // printing nothing — silence reads as "feature absent".
            sout!("Conversion metric: DARK — no recommendations.jsonl. PreToolUse hooks are not");
            sout!(
                "  recording here, so recommend→use conversion cannot be measured in this project."
            );
        }
        // v0.63 — SessionStart live-context injections. Printed outside the
        // total>0 branch (it's a separate counter): a session whose only event was
        // the SessionStart injection still surfaces it instead of reading dark.
        if recs.live_impact > 0 {
            sout!(
                "Live-context: {} recent-change blast-radius injection(s) at SessionStart",
                recs.live_impact
            );
        }
        // Per-session funnel: of sessions that saw a deny/hint, how many also called
        // a cg query tool. This is the deny→use attribution the aggregate ratio can't give.
        if summary.sessions_with_deny > 0 {
            let pct = (summary.sessions_with_deny_and_use as f64
                / summary.sessions_with_deny as f64
                * 100.0)
                .round() as u64;
            sout!(
                "Deny→use: {}/{} deny-sessions used cg = {}% (mcp {}, cli {})",
                summary.sessions_with_deny_and_use,
                summary.sessions_with_deny,
                pct,
                summary.sessions_with_deny_and_cg,
                summary.sessions_with_deny_and_cli
            );
        }
        if summary.sessions_with_hint > 0 {
            let pct = (summary.sessions_with_hint_and_use as f64
                / summary.sessions_with_hint as f64
                * 100.0)
                .round() as u64;
            sout!(
                "Hint→use: {}/{} hint-sessions used cg = {}% (mcp {}, cli {})",
                summary.sessions_with_hint_and_use,
                summary.sessions_with_hint,
                pct,
                summary.sessions_with_hint_and_cg,
                summary.sessions_with_hint_and_cli
            );
        }
    }

    Ok(())
}

// --- grep subcommand ---

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
fn split_attached_context(tok: &str) -> Option<(String, String)> {
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
fn first_unsupported_grep_flag(args: &[String]) -> Option<String> {
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
fn grep_flaglike_pattern_hint(
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
        if json {
            println!("[]");
        }
        eprintln!(
            "[code-graph] unsupported flag: {bad}. Supported: -i -w -F -l -c -A -B -C -m -M -t -g \
             (and no-op -n/-r/-R/-H). To search a literal flag-shaped string, put it \
             after --: code-graph-mcp grep -- {bad}"
        );
        grep_exit(2);
    }
    let mut parsed = GrepArgs::parse_from(normalize_grep_argv(raw.clone()));
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
fn grep_exit(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

/// GNU BRE inverts escaping for its operators: `\|` `\(` `\)` `\{` `\}` `\+`
/// `\?` mean alternation/grouping/repetition, and the UNESCAPED forms are
/// literals. ripgrep's Rust regex dialect is the other way around, so a
/// grep-muscle-memory pattern like `protocol\|proto` silently becomes the
/// literal string "protocol|proto" and zero-hits — an LLM consumer then
/// concludes "no such code". Returns the escapes present so the no-match path
/// can disclose the dialect.
fn bre_style_escapes(pattern: &str) -> Vec<&'static str> {
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
fn rg_spawn_failure_message(project_root: &Path) -> String {
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
fn emit_no_match(pattern: &str, fixed_strings: bool) {
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
fn tracked_files_missed_by_walk(project_root: &Path, scope_rels: &[String]) -> Vec<String> {
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
        if json_mode {
            println!("[]");
        }
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
            if json_mode {
                println!("[]");
            }
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
                if json_mode {
                    println!("[]");
                }
                eprintln!("[code-graph] {}", rg_spawn_failure_message(project_root));
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
            if json_mode {
                println!("[]");
            }
            eprintln!(
                "[code-graph] ripgrep error: {}",
                if stderr.is_empty() {
                    "invalid pattern or unreadable path"
                } else {
                    stderr
                }
            );
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
    // Lazy query-time freshness (parity with the MCP file_path tools'
    // ensure_file_indexed, v0.18.0): before annotating from the index,
    // hash-compare the file and re-index it when dirty — bounded by a sync
    // budget so a repo-wide grep over many dirty files keeps its latency.
    // Beyond budget (or on write contention) annotations carry [stale].
    let sync_budget: usize = std::env::var("CODE_GRAPH_GREP_SYNC_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let mut synced = 0usize;
    let mut stale_count = 0usize;
    let mut node_cache: std::collections::HashMap<String, (Vec<queries::NodeResult>, bool)> =
        std::collections::HashMap::new();
    let mut lookup_container = |file: &str,
                                line: u64|
     -> Option<(String, String, i64, i64, bool)> {
        let ctx = ctx.as_ref()?;
        if !node_cache.contains_key(file) {
            let mut stale = false;
            // Only files already in the index are sync candidates: indexing a
            // brand-new path here could pull gitignored supplement files into
            // the index, diverging from scan_directory's scope.
            let stored: Option<String> = ctx
                .db
                .conn()
                .query_row(
                    "SELECT blake3_hash FROM files WHERE path = ?1",
                    [file],
                    |r| r.get(0),
                )
                .ok();
            if let Some(stored_hash) = stored {
                let abs = ctx.project_root.join(file);
                let disk = crate::indexer::merkle::hash_file(&abs).ok();
                if disk.as_deref() != Some(stored_hash.as_str()) {
                    if synced < sync_budget {
                        match crate::indexer::pipeline::ensure_file_indexed(
                            &ctx.db,
                            &ctx.project_root,
                            file,
                            None,
                        ) {
                            Ok(changed) => {
                                if changed {
                                    synced += 1;
                                }
                            }
                            // SQLITE_BUSY / parse failure: annotate honestly.
                            Err(_) => stale = true,
                        }
                    } else {
                        stale = true;
                    }
                }
            }
            if stale {
                stale_count += 1;
            }
            let nodes = queries::get_nodes_by_file_path(ctx.db.conn(), file).unwrap_or_default();
            node_cache.insert(file.to_string(), (nodes, stale));
        }
        let (nodes, stale) = node_cache.get(file)?;
        find_containing_node_in(nodes, line).map(|(t, n, s, e)| (t, n, s, e, *stale))
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
struct RgRun {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Worst status across batches: 2 (error) > 0 (matched) > 1 (no match).
    code: i32,
}

struct GrepMatch {
    file: String,
    line: u64,
    text: String,
    /// true for -A/-B/-C context lines (rg JSON `type: "context"` records)
    is_context: bool,
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

/// Testable core of [`normalize_path_display`]. `backslash_is_sep` says whether
/// `\` is a path SEPARATOR on the target platform.
///
/// It must not be assumed: on Unix `\` is an ordinary filename character (only
/// `/` and NUL are illegal), so rewriting it unconditionally would rename a
/// legitimate `src/od\bc.rs` to `src/od/bc.rs` — printing a path that does not
/// exist and, worse, producing a lookup key that misses the indexed one, since
/// `indexer::merkle::normalize_rel_path` also rewrites separators only under
/// `#[cfg(windows)]`. That is the very failure mode issue #34 was about, so the
/// fix must not reintroduce it in the other direction.
///
/// The flag is a parameter rather than a `cfg!` so the Windows behaviour is
/// exercised by the Linux and macOS CI legs too. That matters here: the three
/// #34 defects were pure string handling that a `windows-latest` job already in
/// the matrix never caught, because nothing asserted on path spellings at all.
pub(crate) fn normalize_path_display_on(path: &str, backslash_is_sep: bool) -> String {
    if !backslash_is_sep {
        return path.to_string();
    }
    let stripped = path
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{}", rest))
        .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_string());
    // Separator rewrite lives in ONE place crate-wide (the module that owns the
    // index-key invariant); this function only adds the `\\?\` strip on top.
    crate::indexer::merkle::normalize_rel_str_on(&stripped, backslash_is_sep)
}

/// Make an rg-reported path relative to the project root, in canonical
/// (`/`-separated, prefix-free) display form.
///
/// Both sides are normalized before the strip so the mixed spellings above
/// compare equal; on Windows the comparison is ASCII-case-insensitive because
/// the same volume legitimately appears as `D:\` and `d:\` (and rg may echo back
/// whichever spelling it was handed).
fn relativize_path(path_str: &str, root_str: &str) -> String {
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
fn relativize_path_dual(path_str: &str, root_primary: &str, root_fallback: &str) -> String {
    let stripped = relativize_path(path_str, root_primary);
    if stripped != relativize_path(path_str, "") {
        return stripped;
    }
    relativize_path(path_str, root_fallback)
}

/// Testable core of [`relativize_path`] — see [`normalize_path_display_on`] for
/// why the platform is a parameter rather than a `cfg!`.
fn relativize_path_on(path_str: &str, root_str: &str, windows: bool) -> String {
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
fn parse_rg_json(stdout: &[u8], root_canonical: &Path, root_raw: &Path) -> Vec<GrepMatch> {
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
fn truncate_columns(line: &str, max_cols: u64) -> (String, Option<usize>) {
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
fn find_containing_node_in(
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

/// CLI arguments for the `search` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp search",
    about = "FTS5 text search by concept (CLI is FTS-only; MCP adds vector+RRF fusion)"
)]
pub struct SearchArgs {
    /// Search query (concept keywords)
    pub query: String,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// Filter by language
    #[arg(long)]
    pub language: Option<String>,
    /// Filter by node type: fn, class, struct, enum, trait, type, const, var
    #[arg(long = "node-type")]
    pub node_type: Option<String>,
    // --limit and --top-k are the same arg (alias); supplying both is a clap
    // duplicate-arg error. clamp(1,100) stays in the handler; clap parse-errors
    // (exit 2) on a non-numeric value, replacing the old warn+fallback.
    /// Limit results (default: 20, max: 100); alias: --top-k
    #[arg(long, alias = "top-k")]
    pub limit: Option<i64>,
}

/// FTS5 semantic search.
///
/// Output format:
/// ```text
/// fn McpServer::handle_tool_call  src/mcp/server.rs:350-420  (name: &str, params: Value) -> Result<Value>
/// ```
pub fn cmd_search(project_root: &Path, args: SearchArgs) -> Result<()> {
    // clap accepts an empty-string positional (e.g. an unset `search "$X"`);
    // preserve the non-empty query guard with the exact Usage string.
    let query = args.query.as_str();
    if query.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp search <query> [--json] [--limit N] [--top-k N] [--language <lang>] [--compact]");
    }

    let json_mode = args.json;
    let compact = args.compact;
    let node_type_filter = args.node_type.as_deref();
    let limit: i64 = args.limit.unwrap_or(20).clamp(1, 100);

    // Validate --node-type up-front: unknown alias normalizes to an empty Vec
    // and silently filters every node away (see ast-search same fix).
    if let Some(ntf) = node_type_filter {
        if crate::domain::normalize_type_filter(ntf).is_empty() {
            anyhow::bail!(
                "Unknown node-type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                ntf
            );
        }
    }

    // Validate --language up-front and normalize to canonical case: an unknown
    // language matches no node's stored `language` field and would otherwise be
    // reported as a too-narrow filter ("Broaden or clear") rather than a bad value.
    // Parity with --node-type above and MCP semantic_code_search.
    let language_filter = match args.language.as_deref() {
        Some(lf) => Some(crate::utils::config::canonical_language(lf).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown language filter: '{}'. Valid: {}",
                lf,
                crate::utils::config::SUPPORTED_LANGUAGES.join(", ")
            )
        })?),
        None => None,
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Over-fetch so post-fetch filtering can still return `limit` results. The filter
    // below ALWAYS drops <module>/test symbols, and a language/node-type filter can drop
    // far more — a selective filter over a minority language/type silently under-returns.
    // Widen the pool when a filter is active (shared policy with MCP semantic_code_search
    // via search_fetch_count); the unfiltered value stays (limit*4).max(20).
    let filtered = language_filter.is_some() || node_type_filter.is_some();
    let fetch_limit = crate::domain::search_fetch_count(limit, filtered);
    // FTS5 + file join, wrapped so a query-time freshness resync can re-run it
    // against the refreshed index (parity with show/refs/… via refresh_files_if_stale).
    let run_query =
        |conn: &rusqlite::Connection| -> Result<(queries::FtsResult, Vec<queries::NodeWithFile>)> {
            let fts_result = queries::fts5_search(conn, query, fetch_limit)?;
            let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
            let nodes_with_files = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;
            Ok((fts_result, nodes_with_files))
        };
    let (mut fts_result, mut nodes_with_files) = run_query(conn)?;
    // Re-index any matched file edited since indexing so start_line/end_line are
    // post-edit, then re-run once. Bounded by the fetched pool (fetch_limit), not
    // the whole index.
    let files: Vec<String> = nodes_with_files
        .iter()
        .map(|nwf| nwf.file_path.clone())
        .collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        let (f, n) = run_query(conn)?;
        fts_result = f;
        nodes_with_files = n;
    }
    outcome.disclose();

    if fts_result.nodes.is_empty() {
        if json_mode {
            println!("[]");
        }
        eprintln!("[code-graph] No results for: {}", query);
        // Hint: if query looks like code syntax, suggest ast-search
        if query.contains('(')
            || query.contains(')')
            || query.contains("->")
            || query.contains("::")
            || query.contains('<')
        {
            // Replace non-word chars with spaces, collapse multiple spaces, extract clean keywords
            let clean: String = query
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        ' '
                    }
                })
                .collect();
            let keywords: Vec<&str> = clean.split_whitespace().collect();
            if !keywords.is_empty() {
                eprintln!("  Tip: For structural queries, try: code-graph-mcp ast-search --type fn --returns \"{}\"",
                    keywords.join(" "));
            }
        }
        return Ok(());
    }

    // Build id->NodeWithFile map preserving FTS rank order
    let nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf))
        .collect();

    // Normalize node_type filter for matching
    let normalized_node_types: Vec<&'static str> = node_type_filter
        .map(normalize_type_filter)
        .unwrap_or_default();

    // Filter by language, node_type, and skip test/module nodes (align with MCP behavior).
    // Count language/node_type drops separately so an over-selective filter that empties
    // the result set can say so (vs a generic "no results"), mirroring MCP's filter hint.
    let mut filtered_nodes: Vec<&queries::NodeResult> = Vec::new();
    let mut dropped_by_filter = 0usize;
    for n in &fts_result.nodes {
        // Skip <module>/<external> placeholders and test symbols, consistent with
        // MCP semantic_code_search (domain::is_skippable_result = the shared triad;
        // the CLI path previously omitted the <external> leg the MCP path applied).
        let fp = nwf_map
            .get(&n.id)
            .map(|nwf| nwf.file_path.as_str())
            .unwrap_or("");
        if crate::domain::is_skippable_result(&n.node_type, &n.name, fp) {
            continue;
        }
        if let Some(lang) = language_filter {
            let lang_ok = nwf_map
                .get(&n.id)
                .and_then(|nwf| nwf.language.as_deref())
                .map(|l| l.eq_ignore_ascii_case(lang))
                .unwrap_or(false);
            if !lang_ok {
                dropped_by_filter += 1;
                continue;
            }
        }
        if !normalized_node_types.is_empty()
            && !normalized_node_types.iter().any(|t| n.node_type == *t)
        {
            dropped_by_filter += 1;
            continue;
        }
        filtered_nodes.push(n);
        if filtered_nodes.len() >= limit as usize {
            break;
        }
    }

    if filtered_nodes.is_empty() {
        if filtered && dropped_by_filter > 0 {
            // Matches existed but the language/node_type filter removed them all — the
            // index has hits, just not of this language/type. Disclose IN-BAND
            // (stdout), not only stderr: under `--json 2>/dev/null` a bare `[]`
            // is byte-identical to a true zero-hit and the LLM consumer reports
            // "no such code" (disclosure-gap class, roadmap 2026-07-18 §1.1).
            // True zero-hit keeps the plain `[]` / stderr shape below.
            let filter_desc = format!(
                "language: {}{}",
                language_filter.unwrap_or("any"),
                node_type_filter
                    .map(|t| format!(", node-type: {t}"))
                    .unwrap_or_default()
            );
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "query": query,
                        "filtered_out": dropped_by_filter,
                        "filter": filter_desc,
                    })
                );
            } else {
                println!(
                    "[code-graph] No results for: {} — {} candidate(s) matched but were removed by the active filter ({}). Broaden or clear the filter.",
                    query, dropped_by_filter, filter_desc
                );
            }
            eprintln!(
                "[code-graph] No results for: {} — {} candidate(s) matched the query but were removed by the active filter ({}). Broaden or clear the filter.",
                query, dropped_by_filter, filter_desc
            );
        } else {
            if json_mode {
                println!("[]");
            }
            eprintln!(
                "[code-graph] No results for: {} (language: {})",
                query,
                language_filter.unwrap_or("any")
            );
        }
        return Ok(());
    }

    // Build file_path map from filtered results
    let file_map: std::collections::HashMap<i64, &str> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf.file_path.as_str()))
        .collect();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = filtered_nodes
            .iter()
            .map(|n| {
                let fp = file_map.get(&n.id).copied().unwrap_or("?");
                serde_json::json!({
                    "node_id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file_path": fp,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "signature": n.signature,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for node in &filtered_nodes {
        let fp = file_map.get(&node.id).copied().unwrap_or("?");
        if compact {
            let name = node.qualified_name.as_deref().unwrap_or(&node.name);
            writeln!(
                stdout,
                "{}  {}:{}-{}",
                name, fp, node.start_line, node.end_line
            )?;
        } else {
            writeln!(stdout, "{}", format_node_compact(node, fp))?;
        }
    }

    if fts_result.or_fallback {
        eprintln!("[code-graph] Note: AND match insufficient, showing OR results (broader match).");
    }
    if !json_mode {
        eprintln!("[code-graph] Tip: CLI search is FTS5-only. For vector+RRF hybrid recall use MCP semantic_code_search.");
    }

    Ok(())
}

// --- ast-search subcommand ---

/// CLI arguments for the `ast-search` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp ast-search",
    about = "Structured search with --type/--returns/--params filters"
)]
pub struct AstSearchArgs {
    /// Search query (optional if a --type/--returns/--params filter is given)
    pub query: Option<String>,
    /// Filter by node type: fn, class, struct, enum, trait, type, const, var
    #[arg(long = "type")]
    pub type_filter: Option<String>,
    /// Filter by return type
    #[arg(long)]
    pub returns: Option<String>,
    /// Filter by parameter text
    #[arg(long)]
    pub params: Option<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Limit results (default: 20, max: 100)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Structured AST search: FTS5 + column filtering.
///
/// Flags: --type <type>, --returns <type>, --params <text>
pub fn cmd_ast_search(project_root: &Path, args: AstSearchArgs) -> Result<()> {
    // clap accepts an empty-string positional; treat "" as "no query" (the old
    // .filter(|q| !q.is_empty())) so the query-or-filter requirement still fires.
    let query = args.query.as_deref().filter(|q| !q.is_empty());

    let type_filter = args.type_filter.as_deref();
    let returns_filter = args.returns.as_deref();
    let params_filter = args.params.as_deref();
    let json_mode = args.json;
    let limit: usize = args.limit.unwrap_or(20).clamp(1, 100);

    // Require either a query or at least one structural filter
    let has_filters = type_filter.is_some() || returns_filter.is_some() || params_filter.is_some();
    if query.is_none() && !has_filters {
        anyhow::bail!(
            "Usage: code-graph-mcp ast-search <query> [--type fn|class|...] [--returns type] [--params text] [--json]\n\
             Either a query or at least one filter (--type, --returns, --params) is required."
        );
    }

    // Validate --type up-front: an unknown alias normalizes to an empty Vec,
    // which silently filters every node away. Surface as an error so the user
    // doesn't read "No results matching filters" and assume the index is empty.
    if let Some(tf) = type_filter {
        if crate::domain::normalize_type_filter(tf).is_empty() {
            anyhow::bail!(
                "Unknown type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                tf
            );
        }
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Both paths (FTS5+filter, and filter-only SQL) live in the shared core the
    // MCP `ast_search` tool also calls — the two used to be copies and had
    // drifted (audit 2026-08-16 P1-8). Wrapped in a closure so a query-time
    // freshness resync can re-run it against the refreshed index.
    let run_query =
        |conn: &rusqlite::Connection| -> Result<crate::search::ast_query::AstSearchOutcome> {
            crate::search::ast_query::run(
                conn,
                &crate::search::ast_query::AstSearchParams {
                    query,
                    type_filter,
                    returns_filter,
                    params_filter,
                    limit,
                },
            )
        };

    let mut search = run_query(conn)?;
    // Re-index any displayed file edited since indexing so start_line/end_line are
    // post-edit, then re-run once (shared resync with show/refs/…).
    let files: Vec<String> = search
        .results
        .iter()
        .map(|nwf| nwf.file_path.clone())
        .collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        search = run_query(conn)?;
    }
    outcome.disclose();

    let results_with_files = &search.results;
    let dropped_by_filter = search.dropped_by_filter;

    if search.fts_empty {
        if json_mode {
            println!("{}", serde_json::json!({"results": [], "count": 0}));
        }
        eprintln!("[code-graph] No results for: {}", query.unwrap_or_default());
        return Ok(());
    }

    if results_with_files.is_empty() {
        if dropped_by_filter > 0 {
            // The query HAD hits; the structural filters removed every one. Say so
            // in-band — a bare empty envelope under `2>/dev/null` reads as "no such
            // code" (disclosure-gap class, roadmap 2026-07-18 §1.1). Mirrors the
            // cmd_search filter-emptied object.
            let filter_desc = [
                type_filter.map(|t| format!("type: {t}")),
                returns_filter.map(|r| format!("returns: {r}")),
                params_filter.map(|p| format!("params: {p}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            // The remedy depends on WHY it is empty. When the candidate pool
            // came back full, matches may exist below the cut and "broaden the
            // filter" is the wrong advice — the query is what needs narrowing
            // (audit 2026-08-16 P1-8 measured that exact misdirection).
            let remedy = if search.pool_saturated {
                format!(
                    "The candidate pool was full ({} rows), so matches may exist below it. Narrow the query, raise --limit, or drop the query and enumerate with the filters alone.",
                    search.fetch_count
                )
            } else {
                "The index has no symbol matching both the query and the filter. Broaden or clear the filter.".to_string()
            };
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "count": 0,
                        "filtered_out": dropped_by_filter,
                        "filter": filter_desc,
                        "pool_saturated": search.pool_saturated,
                        "hint": remedy,
                    })
                );
            } else {
                println!(
                    "[code-graph] No results — {} candidate(s) matched the query but were removed by the active filter ({}). {}",
                    dropped_by_filter, filter_desc, remedy
                );
            }
            eprintln!(
                "[code-graph] No results matching filters — {} candidate(s) removed by ({}). {}",
                dropped_by_filter, filter_desc, remedy
            );
        } else {
            if json_mode {
                println!("{}", serde_json::json!({"results": [], "count": 0}));
            }
            eprintln!("[code-graph] No results matching filters.");
        }
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = results_with_files
            .iter()
            .map(|nwf| {
                let n = &nwf.node;
                serde_json::json!({
                    "node_id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file_path": &nwf.file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        // Envelope matches MCP ast_search: {results, count, matched_total, truncated}
        let mut envelope = serde_json::json!({
            "results": results,
            "count": results_with_files.len(),
        });
        if let Some(total) = search.matched_total {
            envelope["matched_total"] = serde_json::json!(total);
        }
        if search.truncated {
            envelope["truncated"] = serde_json::json!(true);
            envelope["hint"] = serde_json::json!(truncation_hint(search.matched_total, limit));
        }
        if search.fallback_used {
            envelope["hint"] = serde_json::json!(format!(
                "FTS rank had no '{}' under the active filter; falling back to name-substring match.",
                query.unwrap_or_default()
            ));
        }
        outcome.attach_partial(&mut envelope);
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        return Ok(());
    }

    for nwf in results_with_files {
        writeln!(stdout, "{}", format_node_compact(&nwf.node, &nwf.file_path))?;
    }
    // "20 results" must not read as "20 matches exist" — name the cut and the
    // remedy (raise --limit), which is the opposite of the "broaden the filter"
    // advice the under-fetching version gave (audit 2026-08-16 P1-8).
    if search.truncated {
        eprintln!(
            "[code-graph] {}",
            truncation_hint(search.matched_total, limit)
        );
    }
    if search.fallback_used {
        eprintln!(
            "[code-graph] Note: FTS rank had no '{}' under the active filter; showing name-substring matches.",
            query.unwrap_or_default()
        );
    }
    Ok(())
}

/// Wording for a result set cut by `--limit`. `matched_total` is `None` when the
/// count is SQL-bounded (name-substring fallback / filter-only path), so the
/// message states "more" instead of inventing a number.
fn truncation_hint(matched_total: Option<usize>, limit: usize) -> String {
    match matched_total {
        Some(total) => format!(
            "{} symbols matched but --limit {} was in effect — raise --limit to see the rest.",
            total, limit
        ),
        None => format!(
            "More symbols matched than --limit {} — raise --limit to see the rest.",
            limit
        ),
    }
}

/// Normalize type filter shorthand: fn → function/method, class → class/struct, etc.
fn normalize_type_filter(input: &str) -> Vec<&'static str> {
    let result = crate::domain::normalize_type_filter(input);
    if result.is_empty() {
        eprintln!(
            "[code-graph] Unknown type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
            input
        );
    }
    result
}

// --- callgraph subcommand ---

/// CLI arguments for the `callgraph` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp callgraph",
    about = "Show call graph (callers/callees)"
)]
pub struct CallgraphArgs {
    /// Symbol name to analyze
    pub symbol: String,
    // --direction stays an in-handler String (NOT a clap ValueEnum) so the exact
    // "must be one of: callers, callees, both" exit-1 message is preserved.
    /// Direction: callers, callees, or both
    #[arg(long, default_value = "both")]
    pub direction: String,
    // .max(1) only (NOT clamp) stays in the handler: the engine caps depth and
    // reports requested vs effective separately, so the CLI must not pre-rewrite it.
    /// Max traversal depth (engine caps internally; default: 3)
    #[arg(long, default_value_t = 3)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// Show test callers/callees (hidden by default)
    #[arg(long)]
    pub include_tests: bool,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    /// Minimum edge-resolution confidence to FOLLOW: extracted, inferred, or
    /// ambiguous. Default 'inferred' hides the ambiguous by-name fan-out (a
    /// method name shared by many defs resolving to all of them); pass
    /// 'ambiguous' to show every edge.
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
}

/// Call graph display.
///
/// Output format:
/// ```text
/// handle_tool_call (src/mcp/server.rs:350)
///   ← called by: process_message (src/mcp/server.rs:130)
///   → calls: tool_semantic_search (src/mcp/server.rs:1360)
/// ```
pub fn cmd_callgraph(project_root: &Path, args: CallgraphArgs) -> Result<()> {
    // clap accepts an empty-string positional; preserve the non-empty guard.
    let raw_symbol = args.symbol.as_str();
    if raw_symbol.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp callgraph <symbol> [--direction callers|callees|both] [--depth N] [--file <path>] [--json]");
    }

    let direction = crate::domain::normalize_call_direction(args.direction.as_str())
        .ok_or_else(|| anyhow::anyhow!("--direction must be one of: callers, callees, both"))?;
    let depth: i32 = args.depth.max(1);
    let json_mode = args.json;
    let compact = args.compact;
    let include_tests = args.include_tests;
    let explicit_file_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let explicit_file = explicit_file_owned.as_deref();

    // Confidence floor: default 'inferred' hides the ambiguous by-name fan-out
    // (the known false-positive class) from the traversal; --min-confidence
    // ambiguous restores every edge. Validated at entry, mirroring `refs`.
    let min_conf_tier: &'static str = match args.min_confidence.as_deref() {
        None | Some("") => crate::domain::CONF_INFERRED,
        Some(c) => crate::domain::normalize_confidence(c).ok_or_else(|| {
            anyhow::anyhow!(
                "--min-confidence must be one of: extracted, inferred, ambiguous (got '{}')",
                c
            )
        })?,
    };
    let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (symbol, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
    let file_filter = explicit_file.or(resolved_file.as_deref());

    // Exact-name ambiguity guard: a bare name with ≥2 non-test definitions
    // (cross-file OR same-file overloads) would silently merge call graphs.
    // Shared with MCP via crate::resolve so both surfaces agree (audit #6).
    if file_filter.is_none() {
        if let Some(cands) = crate::resolve::detect_ambiguity(conn, symbol)? {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
    }

    let mut result = crate::graph::query::get_call_graph_filtered(
        conn,
        symbol,
        direction,
        depth,
        file_filter,
        min_conf_rank,
    )?;
    // Fuzzy auto-resolve: if exact-name lookup returned nothing (or only the seed
    // node with no edges) and no --file was specified, promote a unique fuzzy
    // match. Matches MCP get_call_graph behavior.
    let has_edges = result.nodes.iter().any(|n| n.depth > 0);
    let has_seed = result.nodes.iter().any(|n| n.depth == 0);
    let mut resolved_symbol: String = symbol.to_string();
    if !(has_edges || (has_seed && file_filter.is_some())) {
        match resolve_fuzzy_name_cli(conn, symbol)? {
            CliFuzzyResolution::Unique(resolved) => {
                if resolved != symbol {
                    result = crate::graph::query::get_call_graph_filtered(
                        conn,
                        &resolved,
                        direction,
                        depth,
                        file_filter,
                        min_conf_rank,
                    )?;
                    eprintln!("[code-graph] Resolved '{}' → '{}'", symbol, resolved);
                }
                resolved_symbol = resolved;
            }
            CliFuzzyResolution::Ambiguous(cands) => {
                if json_mode {
                    let sugg: Vec<serde_json::Value> = cands
                        .iter()
                        .take(5)
                        .map(|c| {
                            serde_json::json!({
                                "name": c.name, "file_path": c.file_path, "type": c.node_type,
                                "node_id": c.node_id, "start_line": c.start_line,
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::json!({
                            "results": [],
                            "error": format!("Ambiguous symbol '{}': {} matches", symbol, cands.len()),
                            "candidates": sugg,
                        })
                    );
                } else {
                    eprintln!(
                        "[code-graph] Ambiguous symbol '{}': {} matches. Did you mean:",
                        symbol,
                        cands.len()
                    );
                    for c in cands.iter().take(5) {
                        eprintln!(
                            "  {} ({}) in {} [node_id {}]",
                            c.name, c.node_type, c.file_path, c.node_id
                        );
                    }
                }
                std::process::exit(1);
            }
            CliFuzzyResolution::NotFound => { /* fall through to empty-nodes branch */ }
        }
    }
    // Intentional shadow: if fuzzy promoted, `resolved_symbol` holds the resolved
    // name; otherwise it still equals the original input (initialized at
    // `symbol.to_string()` above). Either way, `symbol` below is the correct
    // identifier to print in the "No call graph results" eprintln.
    let symbol = resolved_symbol.as_str();
    if result.nodes.is_empty() {
        if json_mode {
            // In-band error (disclosure-gap class, roadmap 2026-07-18 §1.3):
            // a bare `{"results":[]}` under `2>/dev/null` is indistinguishable
            // from a legitimately edge-less symbol. Same shape as the ambiguous
            // branch above ({results, error, …}) and impact's error object.
            println!(
                "{}",
                serde_json::json!({
                    "results": [],
                    "error": format!("No call graph results for: {}", symbol),
                    "symbol": symbol,
                })
            );
        }
        eprintln!("[code-graph] No call graph results for: {}", symbol);
        // ISSUE-006's sibling surface (pre-tag review SF-1): callgraph is the
        // command the decision table points at first, so a just-added symbol
        // landing here needs the same stale-index hint as show/impact/similar/
        // refs. Gated on the symbol being genuinely ABSENT — a symbol that
        // exists with zero edges also reaches this branch, and hinting at
        // reindexing there would send the user chasing a non-problem.
        if queries::get_nodes_by_name(conn, symbol)
            .map(|nodes| nodes.is_empty())
            .unwrap_or(false)
        {
            hint_symbol_maybe_unindexed(symbol);
        }
        std::process::exit(1);
    }

    // Filter test callers unless --include-tests is set.
    // The seed (depth=0) is kept here because the human-readable renderer
    // below uses it as the tree root. The JSON path filters it separately
    // for parity with MCP `get_call_graph` (which excludes the seed).
    let (display_nodes, test_count) = if include_tests {
        (result.nodes.iter().collect::<Vec<_>>(), 0usize)
    } else {
        let mut display = Vec::new();
        let mut tests = 0usize;
        for n in &result.nodes {
            if n.depth > 0
                && matches!(n.direction, crate::graph::query::Direction::Callers)
                && crate::domain::is_test_node(n.is_test, &n.name, &n.file_path)
            {
                tests += 1;
            } else {
                display.push(n);
            }
        }
        (display, tests)
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Drop the seed (depth=0) — parity with MCP `get_call_graph`
        // (`format_call_graph_response` filters `n.depth > 0`). With
        // `direction=both` the seed appears twice (once per direction),
        // inflating result counts.
        let results: Vec<serde_json::Value> = display_nodes
            .iter()
            .filter(|n| n.depth > 0)
            .map(|n| {
                serde_json::json!({
                    "node_id": n.node_id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": n.file_path,
                    "depth": n.depth,
                    "direction": n.direction.as_str(),
                    "parent_id": n.parent_id,
                })
            })
            .collect();
        let mut output = serde_json::json!({ "results": results });
        if test_count > 0 {
            output["test_callers_hidden"] = serde_json::json!(test_count);
        }
        if result.limit_hit {
            output["limit_hit"] = serde_json::json!(true);
        }
        if result.depth_capped {
            output["depth_capped"] = serde_json::json!(true);
            output["effective_max_depth"] = serde_json::json!(result.effective_max_depth);
            output["requested_max_depth"] = serde_json::json!(result.requested_max_depth);
        }
        if result.suppressed_ambiguous > 0 {
            output["ambiguous_edges_hidden"] = serde_json::json!(result.suppressed_ambiguous);
        }
        writeln!(stdout, "{}", serde_json::to_string(&output)?)?;
        return Ok(());
    }

    // Find root node (depth 0)
    let root = display_nodes.iter().find(|n| n.depth == 0);
    if let Some(root) = root {
        writeln!(stdout, "{} ({})", root.name, root.file_path)?;
    } else {
        return Ok(());
    }
    let root_id = root.unwrap().node_id;

    // Build parent_id → children map per direction, so depth-N nodes nest under
    // their *actual* depth-(N-1) parent rather than visually clumping under the
    // last sibling. Same direction filter so callers/callees subtrees stay
    // separate when --direction=both.
    use std::collections::HashMap;
    let mut children: HashMap<(i64, &'static str), Vec<&crate::graph::query::CallGraphNode>> =
        HashMap::new();
    let mut dedup = std::collections::HashSet::new();
    for n in &display_nodes {
        if n.depth == 0 {
            continue;
        }
        // Dedup cfg-gated duplicates (same name+file+direction+depth, different node_id).
        if !dedup.insert((&n.name, &n.file_path, n.direction.as_str(), n.depth)) {
            continue;
        }
        let parent = n.parent_id.unwrap_or(root_id);
        children
            .entry((parent, n.direction.as_str()))
            .or_default()
            .push(n);
    }

    fn render_subtree<W: std::io::Write>(
        out: &mut W,
        children: &HashMap<(i64, &'static str), Vec<&crate::graph::query::CallGraphNode>>,
        parent_id: i64,
        direction: &'static str,
        compact: bool,
    ) -> std::io::Result<()> {
        let arrow = match direction {
            "callers" => "←",
            _ => "→",
        };
        let arrow_text = match direction {
            "callers" => "← called by",
            _ => "→ calls",
        };
        if let Some(kids) = children.get(&(parent_id, direction)) {
            for n in kids {
                let indent = "  ".repeat(n.depth as usize);
                if compact {
                    writeln!(out, "{}{} {} ({})", indent, arrow, n.name, n.file_path)?;
                } else {
                    writeln!(
                        out,
                        "{}{}: {} ({}) [{}]",
                        indent, arrow_text, n.name, n.file_path, n.node_type
                    )?;
                }
                render_subtree(out, children, n.node_id, direction, compact)?;
            }
        }
        Ok(())
    }

    render_subtree(&mut stdout, &children, root_id, "callers", compact)?;
    render_subtree(&mut stdout, &children, root_id, "callees", compact)?;

    if test_count > 0 {
        writeln!(
            stdout,
            "  ({} test callers hidden, use --include-tests to show)",
            test_count
        )?;
    }
    if result.limit_hit {
        writeln!(
            stdout,
            "  ⚠ result truncated: hit row limit ({} rows) — more callers/callees may exist; pick a leaf and re-query",
            crate::graph::query::CALL_GRAPH_ROW_LIMIT,
        )?;
    }
    if result.depth_capped {
        writeln!(
            stdout,
            "  ⚠ depth capped to {} (requested {}) — deeper chains may exist",
            result.effective_max_depth, result.requested_max_depth,
        )?;
    }
    if result.suppressed_ambiguous > 0 {
        writeln!(
            stdout,
            "  ({} direct ambiguous by-name edge(s) hidden — use --min-confidence ambiguous to show)",
            result.suppressed_ambiguous,
        )?;
    }

    Ok(())
}

// --- impact subcommand ---

/// CLI arguments for the `impact` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp impact",
    about = "Impact analysis (callers, routes, risk level)"
)]
pub struct ImpactArgs {
    /// Symbol name to analyze
    pub symbol: String,
    // clamp(1,20) stays in the handler; clap parse-errors (exit 2) on non-numeric.
    /// Max traversal depth (default: 3)
    #[arg(long, default_value_t = 3)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    // --change-type stays an in-handler String (NOT a clap ValueEnum) so the exact
    // "must be one of: signature, behavior, remove" exit-1 message is preserved.
    /// Change type: signature, behavior, or remove
    #[arg(long = "change-type", default_value = "behavior")]
    pub change_type: String,
    /// Minimum caller-edge confidence to count toward risk: extracted, inferred,
    /// or ambiguous. Default 'inferred' folds the ambiguous by-name fan-out out
    /// of the blast radius (the excluded count is still reported); pass
    /// 'ambiguous' to count every resolved caller.
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
}

/// Impact analysis.
///
/// Shows callers with route info and risk level.
pub fn cmd_impact(project_root: &Path, args: ImpactArgs) -> Result<()> {
    // clap accepts an empty-string positional; preserve the non-empty guard.
    let raw_symbol = args.symbol.as_str();
    if raw_symbol.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp impact <symbol> [--depth N] [--file <path>] [--change-type signature|behavior|remove] [--json]");
    }

    let depth: i32 = args.depth.clamp(1, 20);
    let json_mode = args.json;
    let explicit_file_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let explicit_file = explicit_file_owned.as_deref();
    let change_type = args.change_type.as_str();
    if !matches!(change_type, "signature" | "behavior" | "remove") {
        anyhow::bail!("--change-type must be one of: signature, behavior, remove");
    }
    // Confidence floor for caller traversal: default 'inferred' folds the
    // ambiguous by-name fan-out out of the risk count; --min-confidence ambiguous
    // counts every caller. The excluded count is disclosed below so a folded
    // ambiguous caller never silently under-states risk.
    let min_conf_tier: &'static str = match args.min_confidence.as_deref() {
        None | Some("") => crate::domain::CONF_INFERRED,
        Some(c) => crate::domain::normalize_confidence(c).ok_or_else(|| {
            anyhow::anyhow!(
                "--min-confidence must be one of: extracted, inferred, ambiguous (got '{}')",
                c
            )
        })?,
    };
    let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (symbol, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
    let file_filter = explicit_file.or(resolved_file.as_deref());

    // Verify symbol exists before running impact analysis
    let mut symbol_nodes = queries::get_nodes_by_name(conn, symbol)?;
    if symbol_nodes.is_empty() {
        if json_mode {
            println!(
                "{}",
                serde_json::json!({"error": "Symbol not found", "symbol": symbol})
            );
        }
        eprintln!("[code-graph] Symbol not found: {}", symbol);
        let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
        if !candidates.is_empty() {
            eprintln!("[code-graph] Did you mean:");
            for c in candidates.iter().take(5) {
                eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
            }
        } else {
            hint_symbol_maybe_unindexed(symbol);
        }
        std::process::exit(1);
    }

    // An explicit `--file` that holds no such definition is a MISS, not a
    // filter that legitimately matches nothing. The existence check above uses
    // `get_nodes_by_name`, which ignores the filter, so it passed on a
    // definition in ANOTHER file; the ambiguity guard below is skipped whenever
    // a filter is present; and the caller query then ran with a filter no
    // definition satisfies — zero callers, `"risk":"LOW"`, exit 0. That is a
    // safety endorsement handed to a typo'd path on the command the decision
    // table puts BEFORE an edit. `refs` (`print_refs_notfound_json` +
    // exit 1), `show` and `callgraph` already exit 1 on this exact input;
    // impact was the fourth `--file` taker and the only one that answered
    // (audit 2026-08-16 P1-9).
    if let Some(fp) = explicit_file {
        let in_file = queries::get_nodes_by_file_path(conn, fp)?;
        let present = in_file
            .iter()
            .any(|n| n.name == symbol || n.qualified_name.as_deref() == Some(symbol));
        if !present {
            if json_mode {
                // Same in-band miss contract as `show`: {error, symbol, …} +
                // exit 1, with the files that DO define the symbol so the
                // caller can correct the path instead of re-querying.
                let candidates: Vec<serde_json::Value> = symbol_nodes
                    .iter()
                    .take(5)
                    .map(|n| {
                        serde_json::json!({
                            "name": n.name,
                            "type": n.node_type,
                            "file_path": queries::get_file_path(conn, n.file_id)
                                .ok()
                                .flatten()
                                .unwrap_or_default(),
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "error": "Symbol not found in file",
                        "symbol": symbol,
                        "file": fp,
                        "candidates": candidates,
                    })
                );
            }
            eprintln!(
                "[code-graph] Symbol '{}' not found in file '{}'.",
                symbol, fp
            );
            let defined_in: Vec<String> = symbol_nodes
                .iter()
                .filter_map(|n| queries::get_file_path(conn, n.file_id).ok().flatten())
                .take(5)
                .collect();
            if !defined_in.is_empty() {
                eprintln!("[code-graph] Defined in: {}", defined_in.join(", "));
            }
            std::process::exit(1);
        }
    }

    // Exact-name ambiguity guard: a bare name with ≥2 non-test definitions
    // (cross-file OR same-file overloads) would silently merge callers across
    // both, misreporting risk/blast radius. Shared with MCP via crate::resolve.
    if file_filter.is_none() {
        if let Some(cands) = crate::resolve::detect_ambiguity(conn, symbol)? {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
    }

    let mut callers = crate::graph::routes::get_callers_with_route_info(
        conn,
        symbol,
        file_filter,
        depth,
        min_conf_rank,
    )?;
    // Query-time freshness (shared resync with show/refs/… via refresh_files_if_stale):
    // re-index the symbol's own file(s) and its caller files so the blast radius
    // reflects disk (a caller added/removed since indexing). impact prints no line
    // numbers, so this refreshes the caller SET; re-run the caller query and re-fetch
    // symbol_nodes when anything changed.
    let fresh_outcome = {
        let mut files: Vec<String> = symbol_nodes
            .iter()
            .filter_map(|n| queries::get_file_path(conn, n.file_id).ok().flatten())
            .collect();
        for c in &callers {
            files.push(c.file_path.clone());
        }
        let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
        if outcome.any_changed {
            callers = crate::graph::routes::get_callers_with_route_info(
                conn,
                symbol,
                file_filter,
                depth,
                min_conf_rank,
            )?;
            symbol_nodes = queries::get_nodes_by_name(conn, symbol)?;
        }
        outcome.disclose();
        outcome
    };
    // Ambiguous callers folded out of the blast radius by the confidence floor,
    // counted across the whole returned frontier (seed direct + every kept
    // caller's pruned callers) so a TRANSITIVE ambiguous caller of a
    // uniquely-named symbol is disclosed too. Surfaced (not silently dropped) so a
    // folded real caller never under-states risk; --min-confidence ambiguous counts them.
    let caller_ids: Vec<i64> = callers
        .iter()
        .filter(|c| c.depth > 0)
        .map(|c| c.node_id)
        .collect();
    let ambiguous_callers_excluded =
        crate::graph::query::count_suppressed_seed_edges(
            conn,
            symbol,
            file_filter,
            crate::graph::query::Direction::Callers,
            min_conf_rank,
        )? + crate::graph::query::count_suppressed_into(conn, &caller_ids, min_conf_rank)?;

    // Partition prod/test callers (deduped by name,file,depth), count routes/files,
    // and assess risk via the surface-shared classifier — the MCP impact tool runs
    // the identical rule. crate::graph::impact owns the prod-only route policy (a
    // test-only endpoint is not a production blast radius) and the dedup.
    let is_function_like = symbol_nodes
        .iter()
        .any(|n| crate::domain::is_function_node_type(n.node_type.as_str()));
    let impact = crate::graph::impact::classify_impact(&callers, change_type, is_function_like);
    let prod_callers = &impact.prod_callers;
    let routes = &impact.route_callers;
    let direct_callers = prod_callers.iter().filter(|c| c.depth == 1).count();
    let risk = impact.risk_level;

    // Value references (REL_REFERENCES): callbacks / fn-pointers / type-position
    // couplings the call graph misses. Prod sources, deduped by referencing symbol.
    // Mirrors the MCP impact tool (server/tools/advanced.rs) so both surfaces report
    // the same signal — CLI/MCP parity. NEVER folded into the caller counts above.
    let value_references = {
        use std::collections::HashSet;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for n in &symbol_nodes {
            for r in
                queries::get_incoming_references(conn, n.id, Some(crate::domain::REL_REFERENCES))?
            {
                if !crate::domain::is_test_symbol(&r.name, &r.file_path) {
                    seen.insert((r.name, r.file_path));
                }
            }
        }
        seen.len()
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let mut result = serde_json::json!({
            "symbol": symbol,
            "risk": risk,
            "direct_callers": direct_callers,
            "total_callers": prod_callers.len(),
            "tests_affected": impact.test_count,
            "affected_files": impact.affected_files,
            "affected_routes": routes.len(),
            "value_references": value_references,
            "callers": prod_callers.iter().map(|c| serde_json::json!({
                "name": c.name,
                "type": c.node_type,
                "file": c.file_path,
                "depth": c.depth,
                "route": c.route_info,
            })).collect::<Vec<_>>(),
            // Covering tests behind `tests_affected` — name + file is enough for a
            // hook to build a runnable test command (e.g. `cargo test`/`pytest`).
            // Full list (not capped here); display-side capping is the surface's job.
            "test_callers": impact.test_callers.iter().map(|c| serde_json::json!({
                "name": c.name,
                "file": c.file_path,
            })).collect::<Vec<_>>(),
        });
        if let Some(warning) = impact.type_warning {
            result["warning"] = serde_json::json!(warning);
        }
        if ambiguous_callers_excluded > 0 {
            result["ambiguous_callers_excluded"] = serde_json::json!(ambiguous_callers_excluded);
            result["ambiguous_note"] = serde_json::json!(format!(
                "{} direct caller(s) resolved only by ambiguous name-match were excluded from this risk assessment; actual blast radius may be larger. Re-run with --min-confidence ambiguous to include them.",
                ambiguous_callers_excluded
            ));
        }
        fresh_outcome.attach_partial(&mut result);
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "Impact: {} — Risk: {}", symbol, risk)?;
    if let Some(warning) = impact.type_warning {
        writeln!(stdout, "  (warning: {})", warning)?;
    }
    writeln!(
        stdout,
        "  {} direct callers, {} total, {} files, {} routes ({} tests affected)",
        direct_callers,
        prod_callers.len(),
        impact.affected_files,
        routes.len(),
        impact.test_count
    )?;
    if ambiguous_callers_excluded > 0 {
        writeln!(
            stdout,
            "  ⚠ {} ambiguous by-name caller(s) excluded from risk — actual blast radius may be larger; use --min-confidence ambiguous to include",
            ambiguous_callers_excluded
        )?;
    }
    if value_references > 0 {
        writeln!(
            stdout,
            "  {} value reference(s) — callbacks / fn-pointers / type positions (not call-graph callers)",
            value_references
        )?;
    }

    if !routes.is_empty() {
        writeln!(stdout, "Routes:")?;
        for r in routes {
            let route_str = r.route_info.as_deref().unwrap_or("?");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(route_str) {
                let method = v["method"].as_str().unwrap_or("?");
                let path = v["path"].as_str().unwrap_or("?");
                writeln!(
                    stdout,
                    "  {} {} → {} ({})",
                    method, path, r.name, r.file_path
                )?;
            } else {
                writeln!(stdout, "  {} → {} ({})", route_str, r.name, r.file_path)?;
            }
        }
    }

    if !prod_callers.is_empty() {
        writeln!(stdout, "Callers:")?;
        for c in prod_callers {
            let indent = "  ".repeat(c.depth as usize);
            writeln!(
                stdout,
                "{}{}  ({}) {}",
                indent, c.name, c.node_type, c.file_path
            )?;
        }
    }

    Ok(())
}

// --- affected subcommand ---

#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp affected",
    about = "Changed files → test files to re-run (+ full blast radius)"
)]
pub struct AffectedArgs {
    /// Changed file paths (relative to project root, or absolute under it)
    pub files: Vec<String>,
    /// Also read newline-separated paths from stdin (e.g. `git diff --name-only | …`)
    #[arg(long)]
    pub stdin: bool,
    /// Max reverse-dependency traversal depth (default: 10; clamped 1..=10)
    #[arg(long, default_value_t = 10)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Reverse-impact: given changed files, list the test files that transitively
/// depend on them (primary) plus the full affected-file set (secondary).
pub fn cmd_affected(project_root: &Path, args: AffectedArgs) -> Result<()> {
    use std::collections::{BTreeMap, HashSet};
    use std::io::Read;

    let depth = args.depth.clamp(1, 10);

    // 1. Gather raw paths: positional + optional stdin. read_to_end + lossy UTF-8 so a
    //    non-UTF-8 path (legal on Linux) cannot break the --json envelope (F6).
    let mut raw: Vec<String> = args.files.clone();
    if args.stdin {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        raw.extend(
            String::from_utf8_lossy(&buf)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
        );
    }

    // Bare invocation (no positional files AND no --stdin) has no input to work
    // from — the run then prints "0 test file(s) to re-run", indistinguishable
    // from a genuine "nothing is affected" result and easy to misread as "no
    // tests needed" when the real cause is a forgotten argument. `affected` takes
    // an explicit file list by design (it does NOT auto-diff git), so point the
    // user at the intended pipe. Stderr only: stdout keeps its same-shape (empty)
    // output/JSON envelope. Gated on `!args.stdin` so a real empty pipe (clean
    // `git diff`) stays silent — that path used --stdin correctly, just found no
    // changes.
    if args.files.is_empty() && !args.stdin {
        eprintln!(
            "[code-graph] No files given — nothing to analyze. Pass changed files as \
             arguments, or pipe them from git:\n  \
             git diff --name-only HEAD | code-graph-mcp affected --stdin"
        );
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // 2. Classify each raw input. `changed` holds normalized, INDEXED paths only;
    //    `not_indexed` reports the user's RAW input (one consistent form, F7). Inputs
    //    that normalize to "" (e.g. `.` / project root) are skipped — not a file (F2).
    let mut changed: Vec<String> = Vec::new();
    let mut not_indexed: Vec<String> = Vec::new();
    let mut seen_changed: HashSet<String> = HashSet::new();
    for r in &raw {
        let norm = match normalize_user_path(project_root, r) {
            Ok(p) => p,
            Err(_) => {
                if !not_indexed.contains(r) {
                    not_indexed.push(r.clone());
                }
                continue;
            }
        };
        if norm.is_empty() {
            continue;
        }
        if !queries::file_is_indexed(conn, &norm)? {
            if !not_indexed.contains(r) {
                not_indexed.push(r.clone());
            }
            continue;
        }
        if seen_changed.insert(norm.clone()) {
            changed.push(norm);
        }
    }

    // 3. Union reverse dependents across all changed files over EVERY dependency
    //    relation (imports∪calls∪references∪implements∪inherits, F1), keeping only
    //    language-compatible dependents (F10) and excluding the changed files
    //    themselves from the blast radius (F4).
    let changed_set: HashSet<&str> = changed.iter().map(|s| s.as_str()).collect();
    let mut affected: BTreeMap<String, i32> = BTreeMap::new();
    for f in &changed {
        for (dep_path, dep_depth) in queries::get_reverse_dependents(conn, f, depth)? {
            if !crate::utils::config::is_compatible_lang(f, &dep_path) {
                continue;
            }
            if changed_set.contains(dep_path.as_str()) {
                continue;
            }
            affected
                .entry(dep_path)
                .and_modify(|d| {
                    if dep_depth < *d {
                        *d = dep_depth
                    }
                })
                .or_insert(dep_depth);
        }
    }

    // 4. Primary output: test files among the dependents ∪ changed files that are
    //    themselves tests. `changed` is indexed-only, so a nonexistent test path can no
    //    longer land in both `tests` and `not_indexed` (F3).
    let mut tests: Vec<String> = affected
        .keys()
        .filter(|p| crate::domain::is_test_path(p))
        .cloned()
        .collect();
    for f in &changed {
        if crate::domain::is_test_path(f) && !tests.contains(f) {
            tests.push(f.clone());
        }
    }
    tests.sort();

    // 5. Emit (same-shape JSON on every path — empty included).
    let mut stdout = std::io::stdout().lock();
    if args.json {
        let affected_files: Vec<_> = affected
            .iter()
            .map(|(p, d)| {
                serde_json::json!({
                    "path": p, "depth": d, "is_test": crate::domain::is_test_path(p),
                })
            })
            .collect();
        let result = serde_json::json!({
            "changed": changed,
            "tests": tests,
            "affected_files": affected_files,
            "not_indexed": not_indexed,
        });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(
        stdout,
        "Affected by {} changed file(s) — {} test file(s) to re-run:",
        changed.len(),
        tests.len()
    )?;
    for t in &tests {
        writeln!(stdout, "  {}", t)?;
    }
    // Blast radius, grouped by proximity. A flat depth-ordered-by-path dump put
    // the depth-1 dependents a developer would actually inspect in among
    // hundreds of depth-8..10 transitive hits — on a monorepo with a shared core
    // that is "12% of the repo, unranked" and nobody can act on it (issue #36).
    // Grouping + a display cap keeps the actionable head; `--json` is uncapped
    // and ungrouped, so scripted consumers are unaffected.
    const AFFECTED_DISPLAY_CAP: usize = 40;
    let mut by_depth: BTreeMap<i32, Vec<&String>> = BTreeMap::new();
    for (p, d) in &affected {
        by_depth.entry(*d).or_default().push(p);
    }
    let capped = affected.len() > AFFECTED_DISPLAY_CAP;
    writeln!(
        stdout,
        "Full blast radius: {} file(s) (depth <= {}){}",
        affected.len(),
        depth,
        if capped {
            format!(", nearest {} shown", AFFECTED_DISPLAY_CAP)
        } else {
            String::new()
        }
    )?;
    let mut shown = 0usize;
    let mut withheld = 0usize;
    let mut withheld_from_depth: Option<i32> = None;
    for (d, paths) in &by_depth {
        if shown >= AFFECTED_DISPLAY_CAP {
            withheld += paths.len();
            withheld_from_depth.get_or_insert(*d);
            continue;
        }
        // Header counts must describe THIS listing, not the ungrouped total: with
        // 300 files at depth 1 and a cap of 40, a bare `(300 file(s))` sat above
        // 40 paths, and the `… N more` footer below attributes the remainder to
        // the whole `depth X-Y` range rather than to this group. A reader — or a
        // script scraping the header — could not reconcile the two. Show
        // `shown/total` whenever the cap truncates this group.
        let room = AFFECTED_DISPLAY_CAP - shown;
        if room < paths.len() {
            writeln!(
                stdout,
                "  depth {} ({} of {} file(s) shown):",
                d,
                room,
                paths.len()
            )?;
        } else {
            writeln!(stdout, "  depth {} ({} file(s)):", d, paths.len())?;
        }
        for p in paths {
            if shown >= AFFECTED_DISPLAY_CAP {
                withheld += 1;
                withheld_from_depth.get_or_insert(*d);
                continue;
            }
            writeln!(stdout, "    {}", p)?;
            shown += 1;
        }
    }
    if withheld > 0 {
        writeln!(
            stdout,
            "  … {} more at depth {}-{} — narrow with --depth N, or use --json for the full list",
            withheld,
            withheld_from_depth.unwrap_or(depth),
            by_depth.keys().next_back().copied().unwrap_or(depth)
        )?;
    }
    if !not_indexed.is_empty() {
        writeln!(
            stdout,
            "{} input file(s) not in index: {}",
            not_indexed.len(),
            not_indexed.join(", ")
        )?;
    }
    Ok(())
}

// --- map subcommand ---

/// CLI arguments for the `map` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp map",
    about = "Project architecture map (modules, deps, entry points)"
)]
pub struct MapArgs {
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output (top modules/deps/hot functions only)
    #[arg(long)]
    pub compact: bool,
}

/// Project map — aider repo-map style.
///
/// Output format:
/// ```text
/// src/mcp/server.rs (158KB, 98 symbols)
///   McpServer: handle_tool_call, process_message, flush_metrics
/// ```
pub fn cmd_map(project_root: &Path, args: MapArgs) -> Result<()> {
    let json_mode = args.json;
    let compact = args.compact;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (modules, deps, entry_points, hot_functions) = queries::get_project_map(conn)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Field names (`caller_count` / `test_caller_count`) and `--compact`
        // cap (top-10) match MCP `project_map`. CLI default returns top-15
        // (the DB LIMIT in get_project_map).
        let hot_cap = if compact { 10 } else { hot_functions.len() };
        let hot_json: Vec<serde_json::Value> = hot_functions
            .iter()
            .take(hot_cap)
            .map(|h| {
                let mut obj = serde_json::json!({
                    "name": h.name,
                    "type": h.node_type,
                    "file": h.file,
                    "caller_count": h.caller_count,
                });
                if h.test_caller_count > 0 {
                    obj["test_caller_count"] = serde_json::json!(h.test_caller_count);
                }
                obj
            })
            .collect();

        let result = serde_json::json!({
            "modules": modules.iter().map(|m| serde_json::json!({
                "path": m.path,
                "files": m.files,
                "functions": m.functions,
                "classes": m.classes,
                "interfaces_traits": m.interfaces_traits,
                "constants": m.constants,
                "languages": m.languages,
                "key_symbols": m.key_symbols,
            })).collect::<Vec<_>>(),
            "module_dependencies": deps.iter().map(|d| serde_json::json!({
                "from": d.from,
                "to": d.to,
                "imports": d.import_count,
            })).collect::<Vec<_>>(),
            "entry_points": entry_points.iter().map(|e| serde_json::json!({
                "route": e.route,
                "handler": e.handler,
                "file": e.file,
                "kind": e.kind,
            })).collect::<Vec<_>>(),
            "hot_functions": hot_json,
        });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    // Entry points
    if !entry_points.is_empty() {
        writeln!(stdout, "Entry Points:")?;
        for ep in &entry_points {
            writeln!(stdout, "  {} → {} ({})", ep.route, ep.handler, ep.file)?;
        }
        writeln!(stdout)?;
    }

    // Modules
    if modules.is_empty() {
        if entry_points.is_empty() {
            writeln!(stdout, "(empty project — no indexed source files)")?;
        }
        return Ok(());
    }
    writeln!(stdout, "Modules:")?;
    let max_modules = if compact { 15 } else { modules.len() };
    for m in modules.iter().take(max_modules) {
        // Include constants: key_symbols can list exported consts (e.g. a TS
        // `export const db`), so leaving them out of the total made the header
        // claim fewer symbols than the names printed right under it.
        let total_symbols = m.functions + m.classes + m.interfaces_traits + m.constants;
        write!(
            stdout,
            "{} ({}, {}",
            m.path,
            plural(m.files as i64, "file"),
            plural(total_symbols as i64, "symbol")
        )?;
        if !m.languages.is_empty() {
            write!(stdout, ", {}", m.languages.join("/"))?;
        }
        writeln!(stdout, ")")?;
        if !m.key_symbols.is_empty() {
            writeln!(stdout, "  {}", m.key_symbols.join(", "))?;
        }
    }
    if compact && modules.len() > max_modules {
        writeln!(
            stdout,
            "  ... and {} more modules",
            modules.len() - max_modules
        )?;
    }

    // Dependencies (compact: top 10)
    if !deps.is_empty() {
        writeln!(stdout)?;
        writeln!(stdout, "Dependencies:")?;
        let max_deps = if compact { 10 } else { deps.len().min(30) };
        for d in deps.iter().take(max_deps) {
            writeln!(
                stdout,
                "  {} → {} ({} imports)",
                d.from, d.to, d.import_count
            )?;
        }
        // Truncation marker (roadmap 2026-07-18 §1.7): the silent .min(30) cap
        // read as "that's every dependency" — same pattern as the modules cap.
        if deps.len() > max_deps {
            writeln!(
                stdout,
                "  ... and {} more dependencies",
                deps.len() - max_deps
            )?;
        }
    }

    // Hot functions (compact: top 5)
    if !hot_functions.is_empty() {
        writeln!(stdout)?;
        writeln!(stdout, "Hot Functions:")?;
        let max_hot = if compact { 5 } else { hot_functions.len() };
        for h in hot_functions.iter().take(max_hot) {
            if h.test_caller_count > 0 {
                writeln!(
                    stdout,
                    "  {} ({}) — {} callers + {} test ({})",
                    h.name, h.node_type, h.caller_count, h.test_caller_count, h.file
                )?;
            } else {
                writeln!(
                    stdout,
                    "  {} ({}) — {} callers ({})",
                    h.name, h.node_type, h.caller_count, h.file
                )?;
            }
        }
        if hot_functions.len() > max_hot {
            writeln!(
                stdout,
                "  ... and {} more hot functions",
                hot_functions.len() - max_hot
            )?;
        }
    }

    Ok(())
}

// --- tour subcommand ---

/// CLI arguments for the `tour` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp tour",
    about = "Dependency-ordered reading order: where to start reading a repo (or subtree)"
)]
pub struct TourArgs {
    /// Optional path prefix to scope the tour to a subtree (omit = whole project;
    /// absolute paths under the project root are accepted)
    pub path: Option<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// True when module directory `module_path` is the prefix `pre` or sits under it.
/// `pre` is a normalized path; an empty prefix (from "." or omitted) matches all.
fn module_under_prefix(module_path: &str, pre: &str) -> bool {
    let pre = pre.trim_end_matches('/');
    pre.is_empty() || module_path == pre || module_path.starts_with(&format!("{}/", pre))
}

/// Reading order — lists a module's prerequisites before the modules that build
/// on them (Kahn topological sort over import edges), so reading top-to-bottom
/// orients you from the ground up. Reuses the project-map graph; read-only.
pub fn cmd_tour(project_root: &Path, args: TourArgs) -> Result<()> {
    use crate::graph::reading_order::compute_reading_order;

    let json_mode = args.json;

    // Optional subtree scope. Omitted → whole project.
    let scope: Option<String> = match args.path.as_deref() {
        None => None,
        Some("") => anyhow::bail!("path must not be empty — omit it to tour the whole project"),
        Some(raw) => Some(normalize_user_path(project_root, raw)?),
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (modules, deps, entry_points, _hot) = queries::get_project_map(conn)?;

    let modules: Vec<_> = match &scope {
        None => modules,
        Some(prefix) => modules
            .into_iter()
            .filter(|m| module_under_prefix(&m.path, prefix))
            .collect(),
    };

    let order = compute_reading_order(&modules, &deps, &entry_points);

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Object envelope (cli_json_empty contract: same shape on the empty path).
        let arr: Vec<serde_json::Value> = order
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path,
                    "role": e.role.as_str(),
                    "depended_on_by": e.depended_on_by,
                    "depends_on": e.depends_on,
                    "key_symbols": e.key_symbols,
                    "in_cycle": e.in_cycle,
                })
            })
            .collect();
        let result = serde_json::json!({ "reading_order": arr });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    if order.is_empty() {
        match &scope {
            Some(p) => writeln!(stdout, "(no indexed modules under: {})", p)?,
            None => writeln!(stdout, "(empty project — no indexed source files)")?,
        }
        return Ok(());
    }

    let cycles = order.iter().filter(|e| e.in_cycle).count();
    if cycles > 0 {
        writeln!(
            stdout,
            "Reading order (foundational → entry; {} modules, {} via cycle-break):",
            order.len(),
            cycles
        )?;
    } else {
        writeln!(
            stdout,
            "Reading order (foundational → entry; {} modules):",
            order.len()
        )?;
    }
    for (i, e) in order.iter().enumerate() {
        let mut annot: Vec<String> = vec![format!("[{}]", e.role.as_str())];
        if e.in_cycle {
            annot.push("[cycle]".to_string());
        }
        if e.depended_on_by > 0 {
            annot.push(format!("depended-on-by {}", e.depended_on_by));
        }
        if !e.depends_on.is_empty() {
            let shown = e
                .depends_on
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(",");
            let extra = e.depends_on.len().saturating_sub(3);
            let suffix = if extra > 0 {
                format!("+{}", extra)
            } else {
                String::new()
            };
            annot.push(format!("imports {}{}", shown, suffix));
        }
        write!(stdout, "  {:>2}. {}  {}", i + 1, e.path, annot.join(" · "))?;
        if !e.key_symbols.is_empty() {
            let syms = e
                .key_symbols
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            write!(stdout, "  — {}", syms)?;
        }
        writeln!(stdout)?;
    }

    Ok(())
}

// --- overview subcommand ---

/// CLI arguments for the `overview` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp overview",
    about = "Module overview (symbols grouped by file and type)"
)]
pub struct OverviewArgs {
    /// Path prefix to scan ('.' = whole project; absolute paths under root OK)
    pub path: String,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output (no caller counts)
    #[arg(long)]
    pub compact: bool,
}

/// Module overview: all symbols in files under a path prefix.
pub fn cmd_overview(project_root: &Path, args: OverviewArgs) -> Result<()> {
    // clap requires the positional (missing → exit 2), but accepts an empty
    // string; preserve the empty-path guard below for unset-shell-var `overview "$X"`.
    let raw_path = args.path.as_str();
    // Reject empty-string path: mirrors MCP `tool_module_overview` (script users
    // hit this when a shell variable is unset and overview "$X" expands to "").
    if raw_path.is_empty() {
        anyhow::bail!("path must not be empty — use '.' to scan the whole project root");
    }
    // Normalize: strip leading "./", treat bare "." as empty prefix, and resolve
    // absolute paths under the project root to their relative portion. Mirrors MCP
    // `tool_module_overview` for "./"/"." and additionally supports paste-from-IDE
    // absolute paths (the indexed `file_path` column is project-relative, so
    // unnormalized absolute paths returned "No symbols found").
    let path_prefix_owned = normalize_user_path(project_root, raw_path)?;
    let path_prefix = path_prefix_owned.as_str();

    let json_mode = args.json;
    let compact = args.compact;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Filter out test symbols (align with MCP module_overview behavior).
    let run_query = |conn: &rusqlite::Connection| -> Result<Vec<queries::ModuleExport>> {
        Ok(queries::get_module_exports(conn, path_prefix)?
            .into_iter()
            .filter(|e| !crate::domain::is_test_symbol(&e.name, &e.file_path))
            .collect())
    };
    let mut exports = run_query(conn)?;
    // Query-time freshness (shared resync with show/refs/…): re-index any displayed
    // file edited since indexing so the printed L{start}-{end} ranges are post-edit,
    // then re-run the query against the refreshed index.
    let files: Vec<String> = exports.iter().map(|e| e.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        exports = run_query(conn)?;
    }
    outcome.disclose();

    if exports.is_empty() {
        // JSON empty-result contract (feedback_cli_json_empty_contract):
        // stdout must always be valid JSON. Use a clean eprintln + exit 1
        // instead of `anyhow::bail!` so the JSON-mode stderr doesn't carry
        // the anyhow `Error:` prefix that confuses log consumers.
        if json_mode {
            // In-band error object (roadmap 2026-07-18 §1.3): a bare `[]` under
            // `2>/dev/null` is indistinguishable from an empty-but-indexed dir.
            println!(
                "{}",
                serde_json::json!({
                    "error": "No symbols found", "path": raw_path,
                })
            );
            eprintln!("[code-graph] No symbols found under: {}", raw_path);
            std::process::exit(1);
        }
        anyhow::bail!("[code-graph] No symbols found under: {}", raw_path);
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // `caller_count` matches MCP `module_overview.active_exports[].caller_count`.
        let results: Vec<serde_json::Value> = exports
            .iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "name": e.name,
                    "type": e.node_type,
                    "file": e.file_path,
                    "signature": e.signature,
                    "caller_count": e.caller_count,
                    "start_line": e.start_line,
                    "end_line": e.end_line,
                });
                // Disambiguate same-named methods of different classes (parity with
                // MCP module_overview active_exports). Present only when it adds info.
                if e.qualified_name != e.name {
                    obj["qualified_name"] = serde_json::json!(e.qualified_name);
                }
                obj
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<&str, Vec<&queries::ModuleExport>> =
        std::collections::BTreeMap::new();
    for e in &exports {
        by_file.entry(&e.file_path).or_default().push(e);
    }

    // Single-file path → outline format (sorted by line, signature + line range visible).
    // Replaces Read on huge files: a 3000+ line source emits ~symbol-count lines instead.
    if by_file.len() == 1 {
        let (file, symbols) = by_file.iter().next().unwrap();
        writeln!(stdout, "{}", file)?;
        let mut sorted: Vec<&queries::ModuleExport> = symbols.to_vec();
        sorted.sort_by_key(|e| e.start_line);
        for s in sorted {
            let callers = if s.caller_count > 0 {
                format!(" ({}×)", s.caller_count)
            } else {
                String::new()
            };
            if compact {
                writeln!(
                    stdout,
                    "  L{}-{}  {}  {}{}",
                    s.start_line,
                    s.end_line,
                    s.node_type,
                    s.display_name(),
                    callers
                )?;
            } else {
                let sig = s.signature.as_deref().unwrap_or("");
                let sig_display = if sig.is_empty() {
                    String::new()
                } else {
                    format!("  {}", sig.lines().next().unwrap_or("").trim())
                };
                writeln!(
                    stdout,
                    "  L{}-{}  {}  {}{}{}",
                    s.start_line,
                    s.end_line,
                    s.node_type,
                    s.display_name(),
                    callers,
                    sig_display
                )?;
            }
        }
        return Ok(());
    }

    for (file, symbols) in &by_file {
        writeln!(stdout, "{}", file)?;
        // Group by type within file
        let mut by_type: std::collections::BTreeMap<&str, Vec<&&queries::ModuleExport>> =
            std::collections::BTreeMap::new();
        for s in symbols {
            by_type.entry(&s.node_type).or_default().push(s);
        }
        for (typ, syms) in &by_type {
            let names: Vec<String> = syms
                .iter()
                .map(|s| {
                    if compact {
                        s.display_name().to_string()
                    } else if s.caller_count > 0 {
                        format!("{} ({}×)", s.display_name(), s.caller_count)
                    } else {
                        s.display_name().to_string()
                    }
                })
                .collect();
            writeln!(stdout, "  {}: {}", typ, names.join(", "))?;
        }
    }

    Ok(())
}

// --- show subcommand ---

/// CLI arguments for the `show` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp show",
    about = "Show symbol details (code, type, signature)"
)]
pub struct ShowArgs {
    /// Symbol name (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Look up by node ID instead of name
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    /// Show callers/callees (hidden aliases: --include-refs, --include-references)
    #[arg(long = "refs", aliases = ["include-refs", "include-references"])]
    pub refs: bool,
    /// Show impact summary (hidden alias: --include-impact)
    #[arg(long = "impact", alias = "include-impact")]
    pub impact: bool,
    /// Show test callers/callees in the --refs section (hidden by default)
    #[arg(long)]
    pub include_tests: bool,
    /// Surrounding source lines (default: 3 with --node-id, else 0)
    #[arg(long = "context-lines")]
    pub context_lines: Option<usize>,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Outcome of a query-time freshness resync (`refresh_files_if_stale`) over the
/// files a read command is about to print. Callers re-run their query when
/// `any_changed` and call `disclose()` to honestly surface a partial refresh.
#[derive(Default, Debug)]
struct FreshOutcome {
    /// At least one displayed file was dirty and successfully re-indexed → the
    /// query must re-run so line numbers reflect the post-edit index.
    any_changed: bool,
    /// Dirty files re-indexed within budget.
    refreshed: usize,
    /// Dirty files left stale because the reindex budget was exhausted.
    skipped_over_budget: usize,
    /// Dirty files whose reindex failed (write contention / parse error) — kept
    /// stale, never worse than before.
    failed: usize,
}

impl FreshOutcome {
    /// Some displayed files stayed stale (budget exhausted or reindex failed),
    /// so the printed line numbers for those files may be pre-edit.
    fn is_partial(&self) -> bool {
        self.skipped_over_budget > 0 || self.failed > 0
    }

    /// One-line honest disclosure when the refresh was only partial. stderr only
    /// — stdout carries the JSON/text contract and must not be polluted — and
    /// dual-written to `tracing` per the project's user-facing-warning rule
    /// (`feedback_tracing_invisible_in_cli`). No-op when everything was fresh or
    /// fully refreshed.
    fn disclose(&self) {
        if !self.is_partial() {
            return;
        }
        let stale = self.skipped_over_budget + self.failed;
        let msg = format!(
            "{} file(s) changed since indexing; refreshed {}, line numbers for the rest may be stale (rerun after 'code-graph-mcp incremental-index')",
            stale, self.refreshed
        );
        eprintln!("[code-graph] note: {msg}");
        tracing::warn!("cli freshness partial: {msg}");
    }

    /// In-band partial-freshness marker for OBJECT-shaped `--json` outputs
    /// (roadmap 2026-07-18 §1.4): the stderr note above is invisible under
    /// `--json 2>/dev/null`, so envelope emitters attach `freshness_partial:
    /// true` when some displayed files stayed stale. Array-shaped outputs
    /// (search/show/overview/similar/dead-code) cannot carry a top-level field
    /// without breaking their success shape — for those the stderr note remains
    /// the only channel (documented boundary). No-op when fully fresh.
    fn attach_partial(&self, obj: &mut serde_json::Value) {
        if self.is_partial() {
            obj["freshness_partial"] = serde_json::json!(true);
        }
    }
}

/// Query-time freshness resync shared by the read commands that print
/// `start_line`/`end_line` straight from the index (`show`, `refs`, `overview`,
/// `search`, `ast-search`, `trace`, `similar`, `impact`, `dead-code`). Semantics
/// lifted from `cmd_show`'s original inline loop and the MCP tools'
/// `ensure_file_fresh_opt`: for each DISPLAYED file (dedup + sorted), hash-compare
/// against the index and re-index the dirty ones through `ensure_file_indexed` so
/// their line numbers reflect the post-edit source.
///
/// Bounded (8-file reindex budget — overridable via `CODE_GRAPH_RESYNC_BUDGET`
/// for tests — plus a 250ms busy_timeout) so a common name spanning many dirty
/// files can't stall an interactive command; on write contention / parse failure
/// the stale node is kept, exactly the pre-resync behavior, never worse. `paths`
/// must be the POST-limit result set (what the command will print), not the whole
/// index. Callers re-run their query when the outcome reports `any_changed`.
fn refresh_files_if_stale(db: &Database, root: &Path, paths: &[String]) -> FreshOutcome {
    let mut outcome = FreshOutcome::default();
    let conn = db.conn();
    // Never let a concurrent writer (MCP watcher, another index run) stall an
    // interactive command for the default 5s busy_timeout — fail fast, keep stale.
    let _ = conn.execute_batch("PRAGMA busy_timeout = 250;");

    let mut files: Vec<&str> = paths.iter().map(String::as_str).collect();
    files.sort_unstable();
    files.dedup();

    let mut budget: usize = std::env::var("CODE_GRAPH_RESYNC_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);

    for f in files {
        // Only files already in the index are candidates (parity with cmd_grep):
        // indexing a brand-new path here could pull gitignored supplement files
        // into the index, diverging from scan_directory's scope.
        let stored: Option<String> = conn
            .query_row("SELECT blake3_hash FROM files WHERE path = ?1", [f], |r| {
                r.get(0)
            })
            .ok();
        let Some(stored_hash) = stored else { continue };
        let abs = root.join(f);
        if crate::indexer::merkle::hash_file(&abs).ok().as_deref() == Some(stored_hash.as_str()) {
            continue; // already fresh
        }
        // Dirty from here down.
        if budget == 0 {
            outcome.skipped_over_budget += 1;
            continue;
        }
        match crate::indexer::pipeline::ensure_file_indexed(db, root, f, None) {
            Ok(true) => {
                outcome.any_changed = true;
                outcome.refreshed += 1;
                budget -= 1;
            }
            // Hash differed but the reindex reported no node change — nothing
            // stale to re-query or disclose.
            Ok(false) => {}
            // SQLITE_BUSY / parse failure: keep the stale node, disclose below.
            Err(_) => outcome.failed += 1,
        }
    }
    outcome
}

/// Show symbol details (code, type, signature).
/// CLI equivalent of MCP `get_ast_node`.
/// Resolve a `show` positional symbol to its node(s), applying the shared
/// `Class.method` base-name fallback. Factored out of `cmd_show` so it can be
/// re-run after a query-time freshness resync without duplicating the fallback.
fn resolve_show_nodes(
    conn: &rusqlite::Connection,
    symbol: &str,
    file_filter: Option<&str>,
) -> Result<Vec<queries::NodeResult>> {
    let nodes = if let Some(fp) = file_filter {
        let mut found: Vec<_> = queries::get_nodes_by_file_path(conn, fp)?
            .into_iter()
            .filter(|n| n.name == symbol || n.qualified_name.as_deref() == Some(symbol))
            .collect();
        // Same `Class.method` fallback as the name path: if exact match fails
        // but the symbol has a dot, fall back to the base name within the file.
        // Why: parsers populate qualified_name inconsistently across languages
        // (Rust `impl` blocks: yes; free functions: no), so the literal-match
        // filter above used to silently miss legitimate symbols.
        if found.is_empty() && symbol.contains('.') {
            if let Some(base_name) = symbol.rsplit('.').next() {
                found = queries::get_nodes_by_file_path(conn, fp)?
                    .into_iter()
                    .filter(|n| n.name == base_name)
                    .collect();
            }
        }
        found
    } else {
        let mut found = queries::get_nodes_by_name(conn, symbol)?;
        // `Class.method` fallback: when no node has the exact qualified name
        // stored in DB, prefer nodes whose qualified_name matches; otherwise
        // fall back to all nodes with the base name. Without this fallback,
        // `show McpServer.lock_or_recover` was reporting "Symbol not found"
        // even though `callgraph` resolves the same input via prefix-strip.
        if found.is_empty() && symbol.contains('.') {
            if let Some(base_name) = symbol.rsplit('.').next() {
                let by_name = queries::get_nodes_by_name(conn, base_name)?;
                let any_qualified = by_name
                    .iter()
                    .any(|n| n.qualified_name.as_deref() == Some(symbol));
                if any_qualified {
                    found = by_name
                        .into_iter()
                        .filter(|n| n.qualified_name.as_deref() == Some(symbol))
                        .collect();
                } else {
                    found = by_name;
                }
            }
        }
        found
    };
    Ok(nodes)
}

pub fn cmd_show(project_root: &Path, args: ShowArgs) -> Result<()> {
    let json_mode = args.json;
    let compact = args.compact;
    let include_refs = args.refs;
    let include_impact = args.impact;
    let file_filter_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let file_filter = file_filter_owned.as_deref();
    let context_lines_explicit: Option<usize> = args.context_lines;
    let node_id_arg: Option<i64> = args.node_id;
    // Default context_lines=3 when using --node-id (align with MCP behavior), 0 otherwise
    let context_lines: usize =
        context_lines_explicit.unwrap_or(if node_id_arg.is_some() { 3 } else { 0 });

    // If positional arg points at a real file on disk (has a recognized code
    // extension), nudge the user toward `overview` — `show` takes symbol names.
    if node_id_arg.is_none() {
        if let Some(arg) = args.symbol.as_deref() {
            if !arg.is_empty()
                && crate::utils::config::detect_language(arg).is_some()
                && project_root.join(arg).is_file()
            {
                eprintln!(
                    "[code-graph] `{}` looks like a file path. `show` takes a symbol name (function/struct/const).",
                    arg
                );
                eprintln!(
                    "            File-level symbols: code-graph-mcp overview {}",
                    arg
                );
                eprintln!("            Full file content:  Read the file directly.");
                std::process::exit(1);
            }
        }
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve node(s): by --node-id, or by positional symbol name
    let nodes_with_paths: Vec<(queries::NodeResult, String)> = if let Some(nid) = node_id_arg {
        match queries::get_node_with_file_by_id(conn, nid)? {
            Some(nwf) => vec![(nwf.node, nwf.file_path)],
            None => {
                if json_mode {
                    // In-band error object (roadmap 2026-07-18 §1.3), matching
                    // impact's `{"error", "symbol"}` miss contract.
                    println!(
                        "{}",
                        serde_json::json!({
                            "error": "Node ID not found", "node_id": nid,
                        })
                    );
                }
                eprintln!("[code-graph] Node ID {} not found.", nid);
                std::process::exit(1);
            }
        }
    } else {
        let symbol = args.symbol.as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp show <symbol> [--node-id N] [--file <path>] [--refs] [--impact] [--context-lines N] [--compact] [--json]"
            ))?;

        let mut nodes = resolve_show_nodes(conn, symbol, file_filter)?;

        // Lazy query-time freshness (parity with `cmd_grep`'s resync and the MCP
        // tools' `ensure_file_fresh_opt`): `show` prints start_line/end_line +
        // code_content straight from the index, so a file edited after the last
        // index would report pre-edit line numbers — the "sed to a `show` line and
        // land off by the inserted-line count" bug. Hash-compare each file the
        // symbol resolves into, re-index the dirty ones, then re-resolve. Bounded
        // so a common name spanning many dirty files can't stall an interactive
        // show; on write contention / parse failure we keep the (stale-but-present)
        // node — exactly the pre-fix behavior, never worse.
        let mut files: Vec<String> = nodes
            .iter()
            .filter_map(|n| queries::get_file_path(conn, n.file_id).ok().flatten())
            .collect();
        // With --file, also refresh the named file when the symbol didn't resolve
        // yet — an edit that ADDED the symbol post-index is then picked up too.
        if let Some(fp) = file_filter {
            files.push(fp.to_string());
        }
        let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
        if outcome.any_changed {
            nodes = resolve_show_nodes(conn, symbol, file_filter)?;
        }
        outcome.disclose();

        if nodes.is_empty() {
            let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
            if json_mode {
                // In-band error + fuzzy candidates (roadmap 2026-07-18 §1.3):
                // the stderr-only "Did you mean" list was invisible under
                // `--json 2>/dev/null`, so the miss read as "symbol absent".
                // Shape matches impact's `{"error", "symbol"}` miss contract.
                let sugg: Vec<serde_json::Value> = candidates
                    .iter()
                    .take(5)
                    .map(|c| {
                        serde_json::json!({
                            "name": c.name, "type": c.node_type, "file_path": c.file_path,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "error": "Symbol not found", "symbol": symbol, "candidates": sugg,
                    })
                );
            }
            eprintln!("[code-graph] Symbol not found: {}", symbol);
            if !candidates.is_empty() {
                eprintln!("[code-graph] Did you mean:");
                for c in candidates.iter().take(5) {
                    eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
                }
            } else {
                hint_symbol_maybe_unindexed(symbol);
            }
            std::process::exit(1);
        }

        nodes
            .into_iter()
            .map(|n| {
                let fp = queries::get_file_path(conn, n.file_id)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "?".to_string());
                (n, fp)
            })
            .collect()
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = nodes_with_paths.iter().map(|(node, fp)| {
            let mut obj = serde_json::json!({
                "node_id": node.id,
                "type": node.node_type,
                "name": node.qualified_name.as_deref().unwrap_or(&node.name),
                "file_path": fp,
                "start_line": node.start_line,
                "end_line": node.end_line,
                "signature": node.signature,
                "return_type": node.return_type,
                "param_types": node.param_types,
            });
            if !compact {
                if context_lines > 0 {
                    // ctx.project_root, NOT the raw one: from a linked worktree
                    // with no own index, CliContext reads the MAIN checkout's
                    // index (effective_read_root), so start_line/end_line below
                    // are the main checkout's. Slicing the WORKTREE's bytes at
                    // those offsets prints whatever happens to sit on those lines
                    // on the other branch (audit 2026-08-02 FRS-4).
                    if let Some(code) = read_source_context(&ctx.project_root, fp, node.start_line, node.end_line, context_lines) {
                        obj["code_content"] = serde_json::json!(code);
                    } else {
                        obj["code_content"] = serde_json::json!(node.code_content);
                    }
                } else {
                    obj["code_content"] = serde_json::json!(node.code_content);
                }
            }
            if include_refs {
                use crate::domain::REL_CALLS;
                let include_tests = args.include_tests;
                let callees = queries::get_edge_targets_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
                let callers = queries::get_edge_sources_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
                obj["calls"] = serde_json::json!(callees.iter().map(|(n, f)| serde_json::json!({"name": n, "file": f})).collect::<Vec<_>>());
                let filtered_callers: Vec<_> = if include_tests {
                    callers.iter().collect()
                } else {
                    callers.iter().filter(|(n, f, t)| !crate::domain::is_test_node(*t, n, f)).collect()
                };
                obj["called_by"] = serde_json::json!(filtered_callers.iter().map(|(n, f, _)| serde_json::json!({"name": n, "file": f})).collect::<Vec<_>>());
                if !include_tests {
                    let test_count = callers.len() - filtered_callers.len();
                    if test_count > 0 {
                        obj["test_callers_hidden"] = serde_json::json!(test_count);
                    }
                }
            }
            if include_impact {
                // Shared prod/test partition + risk (graph::impact) — same source as
                // `cmd_impact`/MCP get_ast_node. Trusts the AST `is_test` flag so inline
                // `#[cfg(test)]` unit tests don't inflate the prod count / risk level.
                let callers = crate::graph::routes::get_callers_with_route_info(conn, &node.name, Some(fp.as_str()), 3, 0).unwrap_or_default();
                let is_function_like = crate::domain::is_function_node_type(&node.node_type);
                let cls = crate::graph::impact::classify_impact(&callers, "behavior", is_function_like);
                obj["impact"] = serde_json::json!({
                    "risk_level": cls.risk_level,
                    "direct_callers": cls.prod_callers.iter().filter(|c| c.depth == 1).count(),
                    "transitive_callers": cls.prod_callers.iter().filter(|c| c.depth > 1).count(),
                    "affected_files": cls.affected_files,
                    "affected_routes": cls.route_callers.len(),
                });
                // Disclose how many test callers were excluded from the prod risk count
                // (parity with MCP get_ast_node's impact.test_callers_filtered, and with
                // callgraph's test_callers_hidden / project_map's test_caller_count).
                if cls.test_count > 0 {
                    obj["impact"]["test_callers_filtered"] = serde_json::json!(cls.test_count);
                }
            }
            obj
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for (node, fp) in &nodes_with_paths {
        writeln!(stdout, "{}", format_node_compact(node, fp))?;
        if !compact {
            if context_lines > 0 {
                // Same worktree-aware root as the JSON arm above (FRS-4).
                if let Some(code) = read_source_context(
                    &ctx.project_root,
                    fp,
                    node.start_line,
                    node.end_line,
                    context_lines,
                ) {
                    for line in code.lines() {
                        writeln!(stdout, "  {}", line)?;
                    }
                } else if !node.code_content.is_empty() {
                    for line in node.code_content.lines() {
                        writeln!(stdout, "  {}", line)?;
                    }
                }
            } else if !node.code_content.is_empty() {
                for line in node.code_content.lines() {
                    writeln!(stdout, "  {}", line)?;
                }
            }
        }
        if include_refs {
            use crate::domain::REL_CALLS;
            let include_tests = args.include_tests;
            let callees =
                queries::get_edge_targets_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
            let callers =
                queries::get_edge_sources_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
            if !callees.is_empty() {
                writeln!(stdout, "  Calls:")?;
                for (name, file) in &callees {
                    writeln!(stdout, "    → {} ({})", name, file)?;
                }
            }
            if !callers.is_empty() {
                let mut test_count = 0usize;
                writeln!(stdout, "  Called by:")?;
                for (name, file, is_test) in &callers {
                    if !include_tests && crate::domain::is_test_node(*is_test, name, file) {
                        test_count += 1;
                    } else {
                        writeln!(stdout, "    ← {} ({})", name, file)?;
                    }
                }
                if test_count > 0 {
                    writeln!(
                        stdout,
                        "    ({} test callers hidden, use --include-tests to show)",
                        test_count
                    )?;
                }
            }
        }
        if include_impact {
            let callers = crate::graph::routes::get_callers_with_route_info(
                conn,
                &node.name,
                Some(fp.as_str()),
                3,
                0,
            )
            .unwrap_or_default();
            let is_function_like = crate::domain::is_function_node_type(&node.node_type);
            let cls = crate::graph::impact::classify_impact(&callers, "behavior", is_function_like);
            writeln!(
                stdout,
                "  Impact: {} — {} direct, {} transitive, {} files, {} routes",
                cls.risk_level,
                cls.prod_callers.iter().filter(|c| c.depth == 1).count(),
                cls.prod_callers.iter().filter(|c| c.depth > 1).count(),
                cls.affected_files,
                cls.route_callers.len()
            )?;
            if cls.test_count > 0 {
                writeln!(
                    stdout,
                    "  ({} test callers excluded from the risk count)",
                    cls.test_count
                )?;
            }
        }
    }

    Ok(())
}

/// Read source code with context lines from the project file system.
fn read_source_context(
    project_root: &Path,
    file_path: &str,
    start_line: i64,
    end_line: i64,
    context_lines: usize,
) -> Option<String> {
    use std::io::BufRead;
    let abs_path = project_root.join(file_path);
    let canonical = abs_path.canonicalize().ok()?;
    let root_canonical = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&root_canonical) {
        return None;
    }
    let file = std::fs::File::open(&canonical).ok()?;
    let reader = std::io::BufReader::new(file);
    let start = (start_line as usize).saturating_sub(1 + context_lines);
    let end = (end_line as usize) + context_lines;
    let mut collected = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        if i >= end {
            break;
        }
        if i >= start {
            collected.push(line.ok()?);
        }
    }
    if collected.is_empty() {
        return None;
    }
    Some(collected.join("\n"))
}

// --- trace subcommand ---

/// CLI arguments for the `trace` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp trace",
    about = "Trace HTTP route → handler → downstream calls"
)]
pub struct TraceArgs {
    /// Route to trace (e.g. "/api/login" or "POST /api/login")
    pub route: String,
    // clamp(1,20) stays in the handler; clap parse-errors (exit 2) on non-numeric.
    /// Max traversal depth
    #[arg(long, default_value_t = 3)]
    pub depth: i32,
    // The old usage string advertised a phantom --include-middleware that the code
    // never read; --no-middleware is the real flag (middleware shown by default).
    // Migration drops the phantom and advertises --no-middleware (user-approved,
    // audit #4); --include-middleware now errors like any other stray flag.
    /// Hide downstream middleware/calls (shown by default)
    #[arg(long)]
    pub no_middleware: bool,
    /// Include test symbols in the call chain (hidden by default, matching the MCP trace tool)
    #[arg(long)]
    pub include_tests: bool,
    /// Minimum edge-resolution confidence to FOLLOW: extracted, inferred, or
    /// ambiguous. Default 'inferred' hides the ambiguous by-name fan-out (a method
    /// name shared by many defs resolving to all of them) from both the call chain
    /// and the downstream list; pass 'ambiguous' to show every edge.
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Trace HTTP route → handler → downstream calls.
/// CLI equivalent of MCP `trace_http_chain`.
pub fn cmd_trace(project_root: &Path, args: TraceArgs) -> Result<()> {
    // clap requires the positional (missing → exit 2) but accepts ""; keep the
    // non-empty guard with a Usage string (now advertising --no-middleware).
    let route_path = args.route.as_str();
    if route_path.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp trace <route> [--depth N] [--no-middleware] [--json]");
    }

    let depth: i32 = args.depth.clamp(1, 20);
    let json_mode = args.json;
    let include_middleware = !args.no_middleware;
    // Hide test symbols from the recursive call chain by default, matching the MCP
    // trace_http_chain tool (server/tools/advanced.rs). The one-hop downstream list
    // stays unfiltered FOR TEST SYMBOLS on both surfaces (it still honors the
    // confidence floor below). --include-tests opts the chain back in.
    let include_tests = args.include_tests;

    // Confidence floor (default 'inferred'): hide the ambiguous by-name fan-out from
    // both the recursive chain and the one-hop downstream list, matching callgraph /
    // impact / get_call_graph (v0.77 — trace was previously rank-0 show-all).
    // --min-confidence ambiguous restores every edge. Validated at entry, mirroring
    // cmd_callgraph.
    let min_conf_tier: &'static str = match args.min_confidence.as_deref() {
        None | Some("") => crate::domain::CONF_INFERRED,
        Some(c) => crate::domain::normalize_confidence(c).ok_or_else(|| {
            anyhow::anyhow!(
                "--min-confidence must be one of: extracted, inferred, ambiguous (got '{}')",
                c
            )
        })?,
    };
    let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Parse method filter (e.g., "POST /api/login" → method=POST, path=/api/login)
    let (method_filter, path) = if let Some(idx) = route_path.find(' ') {
        (
            Some(route_path[..idx].to_uppercase()),
            &route_path[idx + 1..],
        )
    } else {
        (None, route_path)
    };

    use crate::domain::REL_ROUTES_TO;
    // Fetch + method-filter the route handlers. Wrapped so a query-time freshness
    // resync can re-run it against the refreshed index (shared with show/refs/…) —
    // the printed handler start_line then reflects a post-edit route file.
    let run_query = |conn: &rusqlite::Connection| -> Result<Vec<queries::RouteMatch>> {
        let mut rows = queries::find_routes_by_path(conn, path, REL_ROUTES_TO)?;
        // Filter by HTTP method if specified (parse metadata JSON for accurate matching)
        if let Some(ref method) = method_filter {
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
        Ok(rows)
    };
    let mut rows = run_query(conn)?;
    let files: Vec<String> = rows.iter().map(|rm| rm.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        rows = run_query(conn)?;
    }
    outcome.disclose();

    if rows.is_empty() {
        // Disclose the framework-coverage limit (mirrors the MCP trace path's
        // richer message): route extraction is implemented for Express/Connect
        // (JS/TS/TSX), Go net/http, Flask/FastAPI (Python), and axum (Rust,
        // v51) — an actix (Rust) or Java (Spring) project has real routes the
        // extractor never sees, so a bare "no match" reads as "no such route"
        // and misleads.
        let hint = "route extraction covers Express/Connect (JS/TS), Go net/http, Flask/FastAPI (Python), and axum (Rust); \
                    actix and Java web frameworks are not yet extracted";
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "handlers": [],
                    "message": format!("No routes matching: {} ({})", route_path, hint),
                })
            );
        }
        // Match the refs/impact/show not-found pattern (clean `[code-graph] …` on
        // stderr + exit 1) instead of `anyhow::bail!`, which main renders as the
        // double-prefixed `Error: [code-graph] No routes matching`.
        eprintln!(
            "[code-graph] No routes matching: {}\n  Note: {}.",
            route_path, hint
        );
        std::process::exit(1);
    }

    let mut stdout = std::io::stdout().lock();

    // Batch-fetch downstream calls if middleware included
    use crate::domain::REL_CALLS;
    let downstream_map = if include_middleware {
        let node_ids: Vec<i64> = rows.iter().map(|rm| rm.node_id).collect();
        queries::get_edge_target_names_batch(conn, &node_ids, REL_CALLS, min_conf_rank)?
    } else {
        std::collections::HashMap::new()
    };

    if json_mode {
        // Single JSON object envelope matching MCP trace_http_chain shape
        let mut handlers = Vec::with_capacity(rows.len());
        let mut ambiguous_hidden: usize = 0;
        for rm in &rows {
            let chain = crate::graph::query::get_call_graph_filtered(
                conn,
                &rm.handler_name,
                "callees",
                depth,
                Some(&rm.file_path),
                min_conf_rank,
            )?;
            ambiguous_hidden += chain.suppressed_ambiguous;
            let chain_nodes: Vec<serde_json::Value> = chain
                .nodes
                .iter()
                .filter(|n| n.depth > 0)
                .filter(|n| {
                    include_tests || !crate::domain::is_test_node(n.is_test, &n.name, &n.file_path)
                })
                .map(|n| {
                    serde_json::json!({
                        "name": n.name, "file_path": n.file_path, "depth": n.depth,
                    })
                })
                .collect();
            let mut entry = serde_json::json!({
                "handler_name": rm.handler_name,
                "file_path": rm.file_path,
                "start_line": rm.start_line,
                "end_line": rm.end_line,
                "metadata": rm.metadata,
                "call_chain": chain_nodes,
            });
            if chain.limit_hit || chain.depth_capped {
                entry["call_chain_truncated"] = serde_json::json!(true);
            }
            if include_middleware {
                let downstream = downstream_map.get(&rm.node_id).cloned().unwrap_or_default();
                entry["downstream_calls"] = serde_json::json!(downstream);
            }
            handlers.push(entry);
        }
        let mut envelope = serde_json::json!({
            "route": path,
            "handlers": handlers,
        });
        if ambiguous_hidden > 0 {
            envelope["ambiguous_edges_hidden"] = serde_json::json!(ambiguous_hidden);
        }
        outcome.attach_partial(&mut envelope);
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        return Ok(());
    }

    let mut ambiguous_hidden: usize = 0;
    for rm in &rows {
        // Render the route label as "METHOD path" from the routes_to metadata
        // (matching the map's Entry Points) instead of dumping the raw JSON blob.
        let route_label = rm
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .map(|v| {
                format!(
                    "{} {}",
                    v["method"].as_str().unwrap_or("ALL"),
                    v["path"].as_str().unwrap_or(path)
                )
            })
            .unwrap_or_else(|| path.to_string());
        writeln!(
            stdout,
            "{} → {} ({}:{})",
            route_label, rm.handler_name, rm.file_path, rm.start_line
        )?;

        if include_middleware {
            if let Some(downstream) = downstream_map.get(&rm.node_id) {
                if !downstream.is_empty() {
                    writeln!(stdout, "  downstream: {}", downstream.join(", "))?;
                }
            }
        }

        // Show call chain
        let chain = crate::graph::query::get_call_graph_filtered(
            conn,
            &rm.handler_name,
            "callees",
            depth,
            Some(&rm.file_path),
            min_conf_rank,
        )?;
        ambiguous_hidden += chain.suppressed_ambiguous;
        for n in &chain.nodes {
            if n.depth == 0 {
                continue;
            }
            if !include_tests && crate::domain::is_test_node(n.is_test, &n.name, &n.file_path) {
                continue;
            }
            let indent = "  ".repeat(n.depth as usize);
            writeln!(stdout, "{}→ {} ({})", indent, n.name, n.file_path)?;
        }
        if chain.limit_hit || chain.depth_capped {
            writeln!(stdout, "  ⚠ chain truncated for {}", rm.handler_name)?;
        }
    }
    if ambiguous_hidden > 0 {
        writeln!(
            stdout,
            "  ({} direct ambiguous by-name edge(s) hidden — use --min-confidence ambiguous to show)",
            ambiguous_hidden,
        )?;
    }

    Ok(())
}

/// File-level dependency graph.
/// CLI equivalent of MCP `dependency_graph`.
/// Scan a file for language-appropriate barrel / re-export / import patterns.
/// Used by `cmd_deps` as a fallback when the graph has no tracked edges for
/// a file (e.g. Rust `mod.rs` barrels that only contain `pub mod X;`).
fn scan_barrel_patterns(project_root: &Path, file_path: &str) -> Option<Vec<(usize, String)>> {
    // Resolve symlinks and confine to the project root before reading. This
    // function echoes import/export lines from a caller-supplied path, so an
    // in-repo symlink pointing outside the root would turn `deps` into a
    // restricted file-read oracle. Mirrors read_source_context's guard (M2).
    let canonical = project_root.join(file_path).canonicalize().ok()?;
    let root_canonical = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&root_canonical) {
        return None;
    }
    let content = std::fs::read_to_string(&canonical).ok()?;
    let lang = crate::utils::config::detect_language(file_path);
    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate().take(1000) {
        let t = line.trim_start();
        let matched = match lang {
            Some("rust") => {
                t.starts_with("pub mod ")
                    || t.starts_with("mod ")
                    || t.starts_with("pub use ")
                    || t.starts_with("use ")
            }
            Some("typescript") | Some("tsx") | Some("javascript") => {
                t.starts_with("import ") || (t.starts_with("export ") && t.contains(" from "))
            }
            Some("python") => {
                (t.starts_with("from ") && t.contains(" import ")) || t.starts_with("import ")
            }
            Some("go") | Some("java") | Some("csharp") | Some("kotlin") => t.starts_with("import "),
            Some("ruby") => t.starts_with("require ") || t.starts_with("require_relative "),
            Some("php") => {
                t.starts_with("use ") || t.starts_with("require ") || t.starts_with("include ")
            }
            _ => false,
        };
        if matched {
            hits.push((idx + 1, line.to_string()));
        }
    }
    if hits.is_empty() {
        None
    } else {
        Some(hits)
    }
}

// --- deps subcommand ---

/// CLI arguments for the `deps` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(name = "code-graph-mcp deps", about = "File-level dependency graph")]
pub struct DepsArgs {
    /// File whose dependencies to show (absolute paths under root OK)
    pub file: String,
    // --direction stays a String validated in-handler (not a clap ValueEnum) so
    // the exact "must be one of" message + exit 1 are preserved for callers.
    /// Direction: outgoing, incoming, or both
    #[arg(long, default_value = "both")]
    pub direction: String,
    // clamp(1,10) stays in the handler; clap parse-errors (exit 2) on non-numeric.
    /// Max traversal depth
    #[arg(long, default_value_t = 2)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
}

/// File-level dependency graph. CLI equivalent of MCP `dependency_graph`.
pub fn cmd_deps(project_root: &Path, args: DepsArgs) -> Result<()> {
    // clap requires the positional (missing → exit 2) but accepts ""; keep the
    // non-empty guard with the exact Usage string.
    let raw_file_path = args.file.as_str();
    if raw_file_path.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp deps <file> [--direction outgoing|incoming|both] [--depth N] [--json]");
    }
    let file_path_owned = normalize_user_path(project_root, raw_file_path)?;
    let file_path = file_path_owned.as_str();

    let direction = crate::domain::normalize_dep_direction(args.direction.as_str())
        .ok_or_else(|| anyhow::anyhow!("--direction must be one of: outgoing, incoming, both"))?;
    let depth: i32 = args.depth.clamp(1, 10);
    let json_mode = args.json;
    let compact = args.compact;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let deps = queries::get_import_tree(conn, file_path, direction, depth)?;
    if deps.is_empty() {
        // Barrel / index-file fallback — scan source for re-export / import lines.
        // Rust `mod.rs` with only `pub mod X;` has no tracked edges in the graph.
        // ctx.project_root: the barrel scan echoes source lines WITH line
        // numbers, so it must read the same checkout the index describes — the
        // main one when this runs from a linked worktree (FRS-4, sibling of the
        // `show --context-lines` fix).
        if let Some(lines) = scan_barrel_patterns(&ctx.project_root, file_path) {
            let mut stdout = std::io::stdout().lock();
            if json_mode {
                let result = serde_json::json!({
                    "file": file_path,
                    "depends_on": [],
                    "depended_by": [],
                    "barrel_scan": lines.iter().map(|(ln, t)| {
                        serde_json::json!({"line": ln, "text": t.trim()})
                    }).collect::<Vec<_>>(),
                    "note": "no tracked dep edges; barrel_scan is raw re-export/import lines from file scan",
                });
                writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
            } else {
                writeln!(stdout, "{}", file_path)?;
                writeln!(
                    stdout,
                    "  (no tracked dep edges \u{2014} raw re-export/import lines from file scan:)"
                )?;
                for (ln, text) in lines {
                    writeln!(stdout, "    {}: {}", ln, text.trim())?;
                }
            }
            return Ok(());
        }
        // Existence is judged against the checkout the index describes too —
        // otherwise a file that exists only on the worktree's branch is reported
        // as "no tracked dependencies" instead of "not found", and vice versa.
        let abs_path = ctx.project_root.join(file_path);
        let file_exists = abs_path.is_file();
        // A directory reaches here too (get_import_tree finds no file-node, the
        // barrel scan can't read it). Distinguish it from a genuinely missing path
        // so the error points at `overview` instead of the misleading "File not
        // found" (the directory plainly exists).
        let is_dir = !file_exists && abs_path.is_dir();
        if json_mode {
            let result = serde_json::json!({
                "file": file_path,
                "depends_on": [],
                "depended_by": [],
                "error": if file_exists {
                    "No tracked dependencies (not a barrel/import file)"
                } else if is_dir {
                    "Path is a directory (deps analyzes a single file; try overview)"
                } else {
                    "File not found"
                },
            });
            println!("{}", serde_json::to_string(&result)?);
        }
        let msg = if file_exists {
            format!(
                "[code-graph] No tracked dependencies for: {} (not a barrel/import file \u{2014} try `code-graph-mcp overview {}` or Read directly)",
                file_path, file_path
            )
        } else if is_dir {
            format!(
                "[code-graph] {} is a directory \u{2014} `deps` analyzes a single file. Try `code-graph-mcp overview {}` for a directory, or pass a file path.",
                file_path, file_path
            )
        } else {
            format!(
                "[code-graph] File not found: {} (run `code-graph-mcp incremental-index` if you just created it, or check the path)",
                file_path
            )
        };
        if json_mode {
            // The disclosure object above IS this command's JSON answer;
            // exiting through Err would make main's tier-3 catch (audit
            // 2026-08-02 P1-7) print a SECOND error object on stdout.
            eprintln!("{msg}");
            std::process::exit(1);
        }
        anyhow::bail!(msg);
    }

    // Filter out cross-language false edges (name-based resolution artifacts)
    // and the synthetic `<external>` bucket (unresolved imports, not a real file).
    let is_compatible_lang =
        |dep_path: &str| crate::utils::config::is_compatible_lang(file_path, dep_path);

    let outgoing: Vec<&_> = deps
        .iter()
        .filter(|d| d.direction == "outgoing" && is_compatible_lang(&d.file_path))
        .collect();
    let incoming: Vec<&_> = deps
        .iter()
        .filter(|d| d.direction == "incoming" && is_compatible_lang(&d.file_path))
        .collect();

    // Distinguish "no edges at all" (handled by the barrel-fallback branch above)
    // from "edges exist but all targets are <external> or cross-language" — the
    // latter previously rendered as a bare filename with no explanation, which
    // looked like a successful no-op even when the file had unresolved imports.
    let unresolved_outgoing = deps
        .iter()
        .filter(|d| d.direction == "outgoing" && !is_compatible_lang(&d.file_path))
        .count();
    let unresolved_incoming = deps
        .iter()
        .filter(|d| d.direction == "incoming" && !is_compatible_lang(&d.file_path))
        .count();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let mut result = serde_json::json!({
            "file": file_path,
            "depends_on": outgoing.iter().map(|d| {
                let mut obj = serde_json::json!({"file": d.file_path, "depth": d.depth});
                if !compact && d.depth == 1 { obj["symbols"] = serde_json::json!(d.symbol_count); }
                obj
            }).collect::<Vec<_>>(),
            "depended_by": incoming.iter().map(|d| {
                let mut obj = serde_json::json!({"file": d.file_path, "depth": d.depth});
                if !compact && d.depth == 1 { obj["symbols"] = serde_json::json!(d.symbol_count); }
                obj
            }).collect::<Vec<_>>(),
        });
        if unresolved_outgoing > 0 {
            result["unresolved_outgoing"] = serde_json::json!(unresolved_outgoing);
        }
        if unresolved_incoming > 0 {
            result["unresolved_incoming"] = serde_json::json!(unresolved_incoming);
        }
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "{}", file_path)?;
    if !outgoing.is_empty() {
        writeln!(stdout, "  Depends on:")?;
        for d in &outgoing {
            if compact {
                writeln!(stdout, "    {}", d.file_path)?;
            } else if d.depth == 1 {
                writeln!(
                    stdout,
                    "    {} ({})",
                    d.file_path,
                    plural(d.symbol_count, "symbol")
                )?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }
    if !incoming.is_empty() {
        writeln!(stdout, "  Depended by:")?;
        for d in &incoming {
            if compact {
                writeln!(stdout, "    {}", d.file_path)?;
            } else if d.depth == 1 {
                writeln!(
                    stdout,
                    "    {} ({})",
                    d.file_path,
                    plural(d.symbol_count, "symbol")
                )?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }
    if outgoing.is_empty()
        && incoming.is_empty()
        && (unresolved_outgoing > 0 || unresolved_incoming > 0)
    {
        writeln!(
            stdout,
            "  (no resolved deps; {} unresolved outgoing, {} unresolved incoming — targets are <external> or in another language)",
            unresolved_outgoing, unresolved_incoming
        )?;
    }

    Ok(())
}

// --- similar subcommand ---

/// CLI arguments for the `similar` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp similar",
    about = "Find semantically similar code (requires embeddings)"
)]
pub struct SimilarArgs {
    /// Symbol name (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Look up by node ID instead of name
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    // clamp(1,100) stays in the handler; clap parse-errors (exit 2) on non-numeric.
    // `--limit` is a hidden alias so users who learned `--limit` from search /
    // ast-search / centrality don't hit a cryptic "unexpected argument" (mirrors
    // SearchArgs, where `--top-k` aliases `--limit`, and MCP semantic_code_search,
    // which accepts both `top_k` and `limit`).
    /// Number of results (default: 5, max: 100); alias: --limit
    #[arg(long = "top-k", alias = "limit")]
    pub top_k: Option<i64>,
    /// Max cosine distance (default: 0.8)
    #[arg(long = "max-distance")]
    pub max_distance: Option<f64>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Find semantically similar code.
/// CLI equivalent of MCP `find_similar_code`.
pub fn cmd_similar(project_root: &Path, args: SimilarArgs) -> Result<()> {
    let top_k: i64 = args.top_k.unwrap_or(5).clamp(1, 100);
    let max_distance: f64 = args.max_distance.unwrap_or(0.8);
    let json_mode = args.json;
    let node_id_arg: Option<i64> = args.node_id;

    // Open with vec support for vector search — but as a READER. `similar` is a
    // passive consumer: reaching for the indexer constructor (`open_with_vec`)
    // made it wipe a version-lagging index to 0 nodes with nothing rebuilding it,
    // and it was the one read command bypassing CliContext's worktree fallback.
    let ctx = CliContext::open_with_vec(project_root)?;
    let db = &ctx.db;
    let conn = db.conn();

    if !db.vec_enabled() {
        // Disclosure object, not `[]`. This is the CAPABILITY-missing case: a
        // bare array under `2>/dev/null` says "no similar code exists", when the
        // truth is that similarity could not be computed at all. Middle tier of
        // the three-tier JSON contract (feedback_cli_json_empty_contract).
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "results": [],
                    "unavailable": "vector search (sqlite-vec extension not loaded)",
                })
            );
        }
        eprintln!("[code-graph] Vector search not available (sqlite-vec extension not loaded).");
        eprintln!("  To enable: build with `cargo build --release --features embed-model`.");
        eprintln!("  Alternative: use `code-graph-mcp search <query>` for text-based similarity.");
        return Ok(());
    }

    // Resolve to node_id: by --node-id or by positional symbol name. `target_label`
    // is what we display in error messages — symbol name when resolved by name,
    // "node_id N" when resolved by --node-id.
    let (node_id, target_label) = if let Some(nid) = node_id_arg {
        // Validate existence up-front — BEFORE the embedding checks below. The
        // symbol path already validates (get_first_node_id_by_name); the --node-id
        // path used not to, so a missing id fell through to the embedded_count==0
        // guard and reported a misleading "No embeddings found" instead of the
        // true cause. This check is embedding-independent → reachable and testable
        // in the default (no embed-model) build, and mirrors refs --node-id.
        if queries::get_node_by_id(conn, nid)?.is_none() {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({ "error": "node_id not found", "node_id": nid })
                );
            }
            eprintln!("[code-graph] node_id {} not found in index", nid);
            std::process::exit(1);
        }
        (nid, format!("node_id {}", nid))
    } else {
        let symbol = args.symbol.as_deref()
            .filter(|s| !s.is_empty())
            .map(strip_qualified_prefix)
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp similar <symbol> [--node-id N] [--top-k N] [--max-distance N] [--json]"
            ))?;
        match queries::get_first_node_id_by_name(conn, symbol)? {
            Some(id) => (id, symbol.to_string()),
            None => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::json!({ "error": "Symbol not found", "symbol": symbol })
                    );
                }
                // All-digit positional is almost certainly a node_id mistakenly passed
                // without the flag — guide the user instead of "Symbol not found: 1010".
                if !symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_digit()) {
                    eprintln!(
                        "[code-graph] Symbol not found: {} \u{2014} did you mean `code-graph-mcp similar --node-id {}`?",
                        symbol, symbol
                    );
                } else {
                    eprintln!("[code-graph] Symbol not found: {}", symbol);
                    hint_symbol_maybe_unindexed(symbol);
                }
                std::process::exit(1);
            }
        }
    };

    // Check embedding exists
    let (embedded_count, total_nodes) = queries::count_nodes_with_vectors(conn)?;
    if embedded_count == 0 {
        // Empty-JSON contract: every --json exit path must emit parseable stdout
        // (feedback_cli_json_empty_contract.md). This path (vec extension present
        // but no embeddings generated yet) is the only one in cmd_similar that was
        // missing it — a consumer piping stdout got an empty string → parse error.
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "error": "No embeddings found",
                    "symbol": target_label,
                    "embedded_count": embedded_count,
                    "total_nodes": total_nodes,
                })
            );
        }
        eprintln!(
            "[code-graph] No embeddings found ({}/{} nodes embedded).",
            embedded_count, total_nodes
        );
        // Tailor the remedy to THIS binary: telling an embed-model build to
        // rebuild with --features embed-model sends the user to fix a problem
        // they don't have (the missing step is just running the MCP server).
        if cfg!(feature = "embed-model") {
            eprintln!("  To enable: start the MCP server to generate embeddings.");
        } else {
            eprintln!("  To enable: build with `cargo build --release --features embed-model`,");
            eprintln!("  then restart the MCP server to generate embeddings.");
        }
        eprintln!("  Alternative: use `code-graph-mcp search <query>` for text-based similarity.");
        std::process::exit(1);
    }

    let embedding: Vec<f32> = {
        let bytes = match queries::get_node_embedding(conn, node_id) {
            Ok(b) => b,
            Err(_) => {
                // Node exists (validated above) but this one has no embedding yet —
                // embeddings still generating. Empty-JSON contract: emit [] under
                // --json instead of bailing with empty stdout.
                if json_mode {
                    println!("[]");
                }
                eprintln!(
                    "[code-graph] No embedding for {} ({}/{} nodes embedded \u{2014} embeddings still generating; try again shortly or pick a node with `--node-id` from `show {}`).",
                    target_label, embedded_count, total_nodes, target_label
                );
                std::process::exit(1);
            }
        };
        bytemuck::cast_slice(&bytes).to_vec()
    };

    // Over-fetch so self-exclusion + max_distance + test/module post-filters don't
    // silently starve top_k (vec0 KNN can't pre-filter on joined node columns). Parity
    // with the MCP twin tool_find_similar_code; the old `top_k + 1` fell short on any drop.
    let fetch_count = crate::domain::similar_fetch_count(top_k);
    let raw_results = queries::vector_search(conn, &embedding, fetch_count)?;

    // Collect filtered results
    let mut similar: Vec<(queries::NodeResult, String, f64)> = Vec::new();
    for (id, distance) in &raw_results {
        if *id == node_id || *distance > max_distance {
            continue;
        }
        let Some(node) = queries::get_node_by_id(conn, *id)? else {
            continue;
        };
        let fp = queries::get_file_path(conn, node.file_id)?.unwrap_or_default();
        if crate::domain::is_skippable_result(&node.node_type, &node.name, &fp) {
            continue;
        }
        similar.push((node, fp, *distance));
        if similar.len() >= top_k as usize {
            break;
        }
    }

    // Observability: post-filters (max_distance + test/module) can shrink results below
    // top_k even with over-fetch. Surface to stderr; stdout JSON stays a bare array.
    let cutoff_dropped = raw_results
        .iter()
        .filter(|(id, dist)| *id != node_id && *dist > max_distance)
        .count();
    if (similar.len() as i64) < top_k && cutoff_dropped > 0 {
        eprintln!(
            "[code-graph] {} result(s) within max_distance={} (< top_k={}); {} nearer candidate(s) exceeded the cutoff. Raise --max-distance to widen.",
            similar.len(), max_distance, top_k, cutoff_dropped
        );
    }

    // Query-time freshness (shared with show/refs/… via refresh_files_if_stale):
    // re-index any displayed file edited since indexing so the printed
    // start_line/end_line are post-edit. NOTE: unlike the other read commands we do
    // NOT re-run the vector search afterward — `ensure_file_indexed` re-indexes with
    // model=None, dropping the touched nodes' embeddings until backfill, so a re-run
    // of vector_search would lose exactly the just-edited rows. Instead we patch the
    // line numbers in place by matching name+file in the refreshed index, preserving
    // the similarity ranking and set.
    let files: Vec<String> = similar.iter().map(|(_, fp, _)| fp.clone()).collect();
    let outcome = refresh_files_if_stale(db, &ctx.project_root, &files);
    if outcome.any_changed {
        for (node, fp, _) in similar.iter_mut() {
            if let Ok(fresh) = queries::get_nodes_by_file_path(conn, fp) {
                if let Some(m) = fresh
                    .iter()
                    .find(|n| n.name == node.name && n.qualified_name == node.qualified_name)
                {
                    node.start_line = m.start_line;
                    node.end_line = m.end_line;
                }
            }
        }
    }
    outcome.disclose();

    let mut stdout = std::io::stdout().lock();

    if similar.is_empty() {
        if json_mode {
            writeln!(stdout, "[]")?;
        }
        eprintln!(
            "[code-graph] No similar code found for node_id: {}",
            node_id
        );
        return Ok(());
    }

    if json_mode {
        let json_results: Vec<serde_json::Value> = similar.iter().map(|(node, fp, distance)| {
            let similarity = 1.0 / (1.0 + distance);
            serde_json::json!({
                "node_id": node.id, "name": node.name, "type": node.node_type, "file_path": fp,
                "start_line": node.start_line, "similarity": (similarity * 10000.0).round() / 10000.0,
                "distance": (distance * 10000.0).round() / 10000.0,
            })
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&json_results)?)?;
        return Ok(());
    }

    for (node, fp, distance) in &similar {
        let similarity = 1.0 / (1.0 + distance);
        writeln!(
            stdout,
            "{:.1}%  {} {}  {}:{}-{}",
            similarity * 100.0,
            node.node_type,
            node.qualified_name.as_deref().unwrap_or(&node.name),
            fp,
            node.start_line,
            node.end_line
        )?;
    }

    Ok(())
}

// --- refs subcommand ---

/// CLI arguments for the `refs` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp refs",
    about = "Find all references to a symbol (callers, importers, etc.)"
)]
pub struct RefsArgs {
    /// Symbol name (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Look up by node ID (authoritative over --file)
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    // --relation stays an in-handler String validated at entry (before index open),
    // NOT a clap ValueEnum — so a bad --relation on a nonexistent symbol reports the
    // relation error (exit 1), not "symbol not found", and the message is preserved.
    /// Filter: calls, imports, inherits, implements, references, all
    #[arg(long)]
    pub relation: Option<String>,
    // Validated in-handler (not a clap ValueEnum) so a bad value reports a clear
    // tier error before symbol resolution, consistent with --relation.
    /// Minimum edge confidence: extracted (precise), inferred, ambiguous (default: show all)
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Emit the refs not-found JSON envelope on stdout. Mirrors the success-case
/// envelope shape (object with `references`/`by_relation`) plus an `error` key,
/// so a single consumer parser handles found, empty, and not-found alike — and
/// every `--json` exit path produces parseable stdout (empty-JSON contract).
/// Used by all three not-found branches: symbol, --file miss, and --node-id miss.
fn print_refs_notfound_json(symbol: &str) {
    println!(
        "{}",
        serde_json::json!({
            "symbol": symbol,
            "total_references": 0,
            "by_relation": {},
            "references": [],
            "error": "Symbol not found",
        })
    );
}

/// Find all references to a symbol. CLI equivalent of MCP `find_references`.
pub fn cmd_refs(project_root: &Path, args: RefsArgs) -> Result<()> {
    let explicit_file_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let explicit_file = explicit_file_owned.as_deref();
    // Validate + case-normalize --relation at command entry — before opening the
    // index and before symbol resolution — so a nonexistent symbol with a bad
    // --relation reports the relation error, not "symbol not found".
    // normalize_relation canonicalizes case. feedback-enum-validate-at-entry.
    let relation: Option<&'static str> = match args.relation.as_deref() {
        None => None,
        Some(r) => match crate::domain::normalize_relation(r) {
            Some(rel) => Some(rel),
            None => anyhow::bail!(
                "--relation must be one of: calls, imports, inherits, implements, references, all (got '{}')",
                r
            ),
        },
    };
    // Validate --min-confidence at entry (before index open), mirroring --relation,
    // so a typo'd tier errors loudly instead of silently passing all rows.
    let min_confidence: Option<&'static str> = match args.min_confidence.as_deref() {
        None => None,
        Some(c) => match crate::domain::normalize_confidence(c) {
            Some(tier) => Some(tier),
            None => anyhow::bail!(
                "--min-confidence must be one of: extracted, inferred, ambiguous (got '{}')",
                c
            ),
        },
    };
    let json_mode = args.json;
    let compact = args.compact;
    let node_id_arg: Option<i64> = args.node_id;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve to (target_ids, symbol_name) — prefer --node-id for same-file multi-def disambiguation.
    // When --node-id is given, it is authoritative: --file is ignored (matches MCP find_references).
    if node_id_arg.is_some() && explicit_file.is_some() {
        eprintln!("[code-graph] Note: --file is ignored when --node-id is given (node_id is authoritative).");
    }
    let (target_ids, symbol): (Vec<i64>, String) = if let Some(nid) = node_id_arg {
        let node = match queries::get_node_by_id(conn, nid)? {
            Some(n) => n,
            None => {
                // Empty-JSON contract: emit a parseable envelope, not empty stdout.
                if json_mode {
                    print_refs_notfound_json(&format!("node_id {}", nid));
                }
                eprintln!("[code-graph] node_id {} not found in index", nid);
                std::process::exit(1);
            }
        };
        (vec![nid], node.name)
    } else {
        let raw_symbol = args.symbol.as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp refs <symbol> [--node-id N] [--file path] [--relation calls|imports|inherits|implements|references] [--min-confidence extracted|inferred|ambiguous] [--compact] [--json]"
            ))?;
        let (base, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
        let file_path = explicit_file.or(resolved_file.as_deref());

        if let Some(fp) = file_path {
            let nodes = queries::get_nodes_by_file_path(conn, fp)?;
            let matched: Vec<i64> = nodes
                .iter()
                .filter(|n| n.name == base)
                .map(|n| n.id)
                .collect();
            if matched.is_empty() {
                // Empty-JSON contract: emit a parseable envelope, not empty stdout.
                if json_mode {
                    print_refs_notfound_json(base);
                }
                eprintln!("[code-graph] Symbol '{}' not found in file '{}'.", base, fp);
                std::process::exit(1);
            }
            (matched, base.to_string())
        } else {
            // Exact-name ambiguity guard — shared with callgraph/impact and the
            // MCP twin via crate::resolve so every surface gives ONE answer for
            // one input (audit 2026-08-02 P1-6: refs was the third consumer and
            // skipped this gate, silently MERGING all same-name definitions'
            // references into a single total while callgraph/MCP errored
            // Ambiguous on the same symbol — the 2026-06-03 #6 shape).
            if let Some(cands) = crate::resolve::detect_ambiguity(conn, base)? {
                emit_exact_ambiguity(base, &cands, json_mode);
            }
            let ids = queries::get_node_ids_by_name(conn, base)?;
            if ids.is_empty() {
                // Fuzzy auto-resolve: unique match → promote; multi → suggest; none → bail
                match resolve_fuzzy_name_cli(conn, base)? {
                    CliFuzzyResolution::Unique(resolved) => {
                        let resolved_ids = queries::get_node_ids_by_name(conn, &resolved)?;
                        (
                            resolved_ids.into_iter().map(|(id, _)| id).collect(),
                            resolved,
                        )
                    }
                    CliFuzzyResolution::Ambiguous(cands) => {
                        if json_mode {
                            let sugg: Vec<serde_json::Value> = cands.iter().take(5).map(|c| serde_json::json!({
                                "name": c.name, "file_path": c.file_path,
                                "type": c.node_type, "node_id": c.node_id, "start_line": c.start_line,
                            })).collect();
                            println!(
                                "{}",
                                serde_json::json!({
                                    "error": format!("Ambiguous symbol '{}': {} matches. Specify --file or --node-id to disambiguate.", base, cands.len()),
                                    "suggestions": sugg,
                                })
                            );
                        } else {
                            eprintln!("[code-graph] Ambiguous symbol '{}': {} matches. Specify --file or --node-id.", base, cands.len());
                            for c in cands.iter().take(5) {
                                eprintln!(
                                    "  {} ({}) in {} [node_id {}]",
                                    c.name, c.node_type, c.file_path, c.node_id
                                );
                            }
                        }
                        std::process::exit(1);
                    }
                    CliFuzzyResolution::NotFound => {
                        // Match the success-case envelope shape (object with
                        // references/by_relation), not a bare `[]`. Object-success
                        // commands (callgraph/trace/deps) all emit an object on the
                        // empty/error path so one parser handles both — refs was the
                        // outlier returning `[]`, which broke `.references` access.
                        if json_mode {
                            print_refs_notfound_json(base);
                        }
                        eprintln!("[code-graph] Symbol not found: {}", base);
                        hint_symbol_maybe_unindexed(base);
                        std::process::exit(1);
                    }
                }
            } else {
                (
                    ids.into_iter().map(|(id, _)| id).collect(),
                    base.to_string(),
                )
            }
        }
    };
    // Intentional shadow: downstream paths want &str. Do NOT "simplify" into a
    // single binding — the tuple above must own the String so `get_node_by_id`'s
    // return doesn't get dropped across the .as_str() borrow.
    let symbol = symbol.as_str();

    use crate::domain::{REL_CALLS, REL_IMPLEMENTS, REL_IMPORTS, REL_INHERITS, REL_REFERENCES};
    let relation_filter = match relation {
        Some("calls") => Some(REL_CALLS),
        Some("imports") => Some(REL_IMPORTS),
        Some("inherits") => Some(REL_INHERITS),
        Some("implements") => Some(REL_IMPLEMENTS),
        Some("references") => Some(REL_REFERENCES),
        Some("all") | None => None,
        Some(other) => anyhow::bail!(
            "Unknown relation '{}'. Valid: calls, imports, inherits, implements, references, all",
            other
        ),
    };

    // Build the deduped reference set. Wrapped in a closure so a query-time
    // freshness resync can re-run it against the refreshed index (parity with
    // show/overview/… via refresh_files_if_stale) — after re-indexing an edited
    // source file its referencing symbol's start_line is post-edit.
    // Dedup key is (name, file_path, relation) — it does NOT include the target,
    // so two edges from the same source to DIFFERENT same-name targets collapse to
    // one row. When their confidence differs, show the LOWEST (most conservative)
    // tier: the displayed confidence must not understate a hidden sibling's
    // ambiguity (L1 — surfacing low confidence is the whole point of the feature).
    let build_refs =
        |conn: &rusqlite::Connection| -> Result<(Vec<queries::IncomingReference>, usize)> {
            let mut all_refs: Vec<queries::IncomingReference> = Vec::new();
            let mut seen: std::collections::HashMap<(String, String, String), usize> =
                std::collections::HashMap::new();
            let mut conf_filtered = 0usize;
            for target_id in &target_ids {
                let refs = queries::get_incoming_references(conn, *target_id, relation_filter)?;
                for r in refs {
                    // --min-confidence: drop refs below the requested tier (default: keep all).
                    if let Some(min) = min_confidence {
                        if crate::domain::confidence_rank(&r.confidence)
                            < crate::domain::confidence_rank(min)
                        {
                            conf_filtered += 1;
                            continue;
                        }
                    }
                    let key = (r.name.clone(), r.file_path.clone(), r.relation.clone());
                    match seen.get(&key) {
                        Some(&idx) => {
                            // Keep the worst-case (lowest) confidence among deduped siblings.
                            if crate::domain::confidence_rank(&r.confidence)
                                < crate::domain::confidence_rank(&all_refs[idx].confidence)
                            {
                                all_refs[idx].confidence = r.confidence;
                            }
                        }
                        None => {
                            seen.insert(key, all_refs.len());
                            all_refs.push(r);
                        }
                    }
                }
            }
            Ok((all_refs, conf_filtered))
        };
    let (mut all_refs, mut conf_filtered) = build_refs(conn)?;
    let files: Vec<String> = all_refs.iter().map(|r| r.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        let (a, c) = build_refs(conn)?;
        all_refs = a;
        conf_filtered = c;
    }
    outcome.disclose();

    if json_mode {
        let items: Vec<serde_json::Value> = all_refs
            .iter()
            .map(|r| {
                if compact {
                    serde_json::json!({
                        "name": r.name,
                        "file_path": r.file_path,
                        "start_line": r.start_line,
                        "relation": r.relation,
                        "confidence": r.confidence,
                        "node_id": r.node_id,
                    })
                } else {
                    serde_json::json!({
                        "node_id": r.node_id,
                        "name": r.name,
                        "type": r.node_type,
                        "file_path": r.file_path,
                        "start_line": r.start_line,
                        "relation": r.relation,
                        "confidence": r.confidence,
                    })
                }
            })
            .collect();
        // Group counts by relation, mirroring MCP find_references envelope
        let mut by_relation: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for r in &all_refs {
            *by_relation.entry(r.relation.clone()).or_insert(0) += 1;
        }
        let mut envelope = serde_json::json!({
            "symbol": symbol,
            "total_references": items.len(),
            "by_relation": by_relation,
            "references": items,
        });
        // Machine surface must not be LESS informative than the human one:
        // human mode prints the hidden count below, and the sibling commands
        // disclose theirs in-band (callgraph ambiguous_edges_hidden, impact
        // ambiguous_callers_excluded, ast-search filtered_out) — audit
        // 2026-08-02 MED-1.
        if conf_filtered > 0 {
            envelope["confidence_filtered"] = serde_json::json!(conf_filtered);
        }
        outcome.attach_partial(&mut envelope);
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        let mut stdout = std::io::stdout().lock();
        // Annotate only non-extracted edges so precise refs stay visually clean;
        // inferred/ambiguous are the ones worth scrutiny (by-name cross-file).
        let tag = |c: &str| -> String {
            if c == crate::domain::CONF_EXTRACTED {
                String::new()
            } else {
                format!(" ~{c}")
            }
        };
        if all_refs.is_empty() {
            writeln!(stdout, "No references found for '{}'.", symbol)?;
        } else {
            writeln!(stdout, "{} references to '{}':", all_refs.len(), symbol)?;
            for r in &all_refs {
                if compact {
                    writeln!(
                        stdout,
                        "  [{}] {} {}{}",
                        r.relation,
                        r.name,
                        r.file_path,
                        tag(&r.confidence)
                    )?;
                } else {
                    writeln!(
                        stdout,
                        "  [{}] {} ({}:{}){}",
                        r.relation,
                        r.name,
                        r.file_path,
                        r.start_line,
                        tag(&r.confidence)
                    )?;
                }
            }
        }
        if conf_filtered > 0 {
            writeln!(
                stdout,
                "({} lower-confidence ref(s) hidden by --min-confidence)",
                conf_filtered
            )?;
        }
    }

    Ok(())
}

// --- dead-code subcommand ---

/// CLI arguments for the `dead-code` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp dead-code",
    about = "Find unused code (orphans and exported-unused symbols)"
)]
pub struct DeadCodeArgs {
    /// Restrict the scan to this path prefix (absolute paths under root OK)
    pub path: Option<String>,
    // --node-type is preferred (matches `search` CLI + MCP param); --type is the
    // legacy alias. clap accepts any string here — the handler validates it via
    // normalize_type_filter so a typo errors loudly instead of false-clean exit 0.
    // --node-type and --type are ONE arg (alias), so supplying both is a clap
    // duplicate-arg error (exit 2) — deliberately stricter than the old parser,
    // which silently honored --node-type and ignored --type (masking a bad --type).
    /// Filter by node type: fn, class, struct, enum, trait, type, const, var (alias: --type)
    #[arg(long = "node-type", alias = "type")]
    pub node_type: Option<String>,
    /// Show test callers (hidden by default)
    #[arg(long)]
    pub include_tests: bool,
    // clap parse-errors (exit 2) on a non-numeric value, replacing the hand
    // parser's warn-and-fallback — consistent with `stats --last` under flavor B.
    /// Minimum lines to report
    #[arg(long, default_value_t = 3)]
    pub min_lines: u32,
    /// Show full code snippets (default: compact, names only)
    #[arg(long)]
    pub no_compact: bool,
    /// Exclude a path prefix (repeatable; default: claude-plugin/, benches/)
    #[arg(long)]
    pub ignore: Vec<String>,
    /// Disable the default --ignore prefixes
    #[arg(long)]
    pub no_ignore: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Find dead code: orphans and exported-unused symbols.
/// CLI equivalent of MCP `find_dead_code`.
pub fn cmd_dead_code(project_root: &Path, args: DeadCodeArgs) -> Result<()> {
    let DeadCodeArgs {
        path,
        node_type,
        include_tests,
        min_lines,
        no_compact,
        ignore,
        no_ignore,
        json: json_mode,
    } = args;

    let path_filter_owned: Option<String> = match path.as_deref() {
        Some(p) => Some(normalize_user_path(project_root, p)?),
        None => None,
    };
    let path_filter = path_filter_owned.as_deref();
    // --node-type (preferred) and its --type alias both land in `node_type`.
    let type_filter = node_type.as_deref();
    // Validate --type/--node-type up-front: an unknown alias normalizes to an
    // empty Vec, and find_dead_code then falls through to a literal `n.type = :x`
    // match that returns zero rows — so a typo'd `--type fucntion` prints a
    // false-clean "No dead code found" with exit 0. Mirror the cmd_ast_search guard.
    queries::validate_dead_code_type_filter(type_filter)?;
    let compact = !no_compact;

    // --ignore <pref>: repeatable, prefix-match exclusion. --no-ignore disables defaults.
    // Defaults are owned by `domain::default_dead_code_ignores()` (claude-plugin/, benches/).
    // Separator-normalized like every other path argument: these are matched with
    // `starts_with` against `/`-stored paths, so a Windows user's
    // `--ignore src\generated` would exclude nothing and silently over-report.
    // Not routed through `normalize_user_path` — a PREFIX is not required to name
    // an existing file, and the escape check would reject legitimate ones.
    let ignore_prefixes: Vec<String> = if no_ignore {
        Vec::new()
    } else if ignore.is_empty() {
        crate::domain::default_dead_code_ignores()
    } else {
        ignore
            .iter()
            .map(|p| crate::indexer::merkle::normalize_rel_str(p))
            .collect()
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();
    let run_query = |conn: &rusqlite::Connection| -> Result<queries::DeadCodeReport> {
        queries::dead_code_report(
            conn,
            path_filter,
            type_filter,
            include_tests,
            min_lines,
            &ignore_prefixes,
        )
    };
    let mut report = run_query(conn)?;
    // Query-time freshness (shared resync with show/refs/…): re-index any displayed
    // candidate's file edited since indexing so its start_line/end_line are post-edit,
    // then re-run against the refreshed index.
    let files: Vec<String> = report.items.iter().map(|it| it.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        report = run_query(conn)?;
    }
    outcome.disclose();

    if report.is_empty() {
        // Empty-but-something-hidden discloses IN-BAND (stdout/JSON), not only
        // stderr: under `--json 2>/dev/null` a bare `[]` reads as "clean" even
        // when --ignore/--min-lines hid real candidates (disclosure-gap class,
        // roadmap 2026-07-18 §1.2). True clean keeps the plain `[]`.
        // A path filter that matches NO indexed file is zero coverage, not a
        // clean bill of health, and it is the one empty case `dead-code` still
        // reported as `[]` + exit 0. `overview` answers the same input with an
        // error object + exit 1 (:5641), and `normalize_user_path`'s own doc
        // names this failure mode — a path can be in-root and well-formed while
        // naming nothing indexed, so normalization cannot catch it. Under
        // `--json 2>/dev/null` the old answer was indistinguishable from "this
        // directory genuinely has no dead code".
        //
        // The probe itself (incl. the `.` / trailing-slash spellings that must
        // NOT count as a miss) now lives in `queries::unindexed_path_prefix`,
        // shared with MCP `tool_find_dead_code` — which had no probe at all and
        // answered the same input with a clean report (audit 2026-08-16 P1-22).
        if let Some(prefix) = queries::unindexed_path_prefix(conn, path_filter) {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({ "error": "No indexed files under path", "path": prefix })
                );
                eprintln!("[code-graph] No indexed files under: {prefix}");
                std::process::exit(1);
            }
            anyhow::bail!("[code-graph] No indexed files under: {prefix}");
        }

        if report.ignored_count > 0 {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "ignored_count": report.ignored_count,
                    })
                );
            } else {
                println!(
                    "[code-graph] No dead code found after filtering; {} suppressed by --ignore (use --no-ignore to see them).",
                    report.ignored_count,
                );
            }
            eprintln!(
                "[code-graph] No dead code found after filtering; {} suppressed by --ignore (use --no-ignore to see them).",
                report.ignored_count,
            );
        } else if report.hidden_below_threshold > 0 {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "below_threshold_count": report.hidden_below_threshold,
                        "min_lines": min_lines,
                    })
                );
            } else {
                println!(
                    "[code-graph] No dead code found at \u{2265}{min_lines} lines ({} shorter symbol(s) below the threshold; rerun with --min-lines 1 to include them).",
                    report.hidden_below_threshold
                );
            }
            eprintln!(
                "[code-graph] No dead code found at \u{2265}{min_lines} lines ({} shorter symbol(s) below the threshold; rerun with --min-lines 1 to include them).",
                report.hidden_below_threshold
            );
        } else {
            if json_mode {
                writeln!(std::io::stdout().lock(), "[]")?;
            }
            eprintln!("[code-graph] No dead code found.");
        }
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let items: Vec<serde_json::Value> = report
            .items
            .iter()
            .map(|it| {
                let mut obj = serde_json::json!({
                    "name": it.name,
                    "type": it.node_type,
                    "file_path": it.file_path,
                    "start_line": it.start_line,
                    "end_line": it.end_line,
                    "category": if it.is_exported { "exported_unused" } else { "orphan" },
                    "lines": it.end_line - it.start_line + 1,
                });
                if !compact {
                    obj["code"] = serde_json::json!(it.code_content);
                }
                obj
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    writeln!(
        stdout,
        "Dead code: {} candidates ({} orphan, {} exported-unused)",
        report.items.len(),
        report.orphan_count,
        report.exported_count
    )?;
    writeln!(stdout, "(candidates to verify — receiver-method calls (obj.method()) and cross-file const/type uses are not edge-tracked)\n")?;

    let (orphans, exported_unused): (Vec<_>, Vec<_>) =
        report.items.iter().partition(|it| !it.is_exported);

    if !orphans.is_empty() {
        writeln!(
            stdout,
            "ORPHAN ({}) — no tracked references, not exported",
            orphans.len()
        )?;
        for it in &orphans {
            let lines = it.end_line - it.start_line + 1;
            writeln!(
                stdout,
                "  {} {} {}:{} ({})",
                it.node_type,
                it.name,
                it.file_path,
                it.start_line,
                plural(lines, "line")
            )?;
            if !compact {
                for line in it.code_content.lines().take(5) {
                    writeln!(stdout, "    {}", line)?;
                }
                if it.code_content.lines().count() > 5 {
                    writeln!(stdout, "    ...")?;
                }
            }
        }
    }

    if !exported_unused.is_empty() {
        if !orphans.is_empty() {
            writeln!(stdout)?;
        }
        writeln!(
            stdout,
            "EXPORTED-UNUSED ({}) — exported/public, no tracked callers",
            exported_unused.len()
        )?;
        for it in &exported_unused {
            let lines = it.end_line - it.start_line + 1;
            writeln!(
                stdout,
                "  {} {} {}:{} ({})",
                it.node_type,
                it.name,
                it.file_path,
                it.start_line,
                plural(lines, "line")
            )?;
            if !compact {
                for line in it.code_content.lines().take(5) {
                    writeln!(stdout, "    {}", line)?;
                }
                if it.code_content.lines().count() > 5 {
                    writeln!(stdout, "    ...")?;
                }
            }
        }
    }

    Ok(())
}

// --- centrality subcommand ---

/// CLI arguments for the `centrality` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp centrality",
    about = "Rank architectural chokepoints by betweenness centrality (call graph)"
)]
pub struct CentralityArgs {
    /// Number of functions to report (default: 15)
    #[arg(long, default_value_t = 15)]
    pub limit: u32,
    /// Include test symbols in the graph (excluded by default)
    #[arg(long)]
    pub include_tests: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Rank functions by betweenness centrality over the `calls` graph — the
/// structural bridges that lie on the most shortest call paths between other
/// functions. Complements `map`'s caller_count "hot functions" (degree
/// centrality): a chokepoint can have few callers yet route most cross-cluster
/// traffic. CLI-only; not exposed as an MCP tool.
pub fn cmd_centrality(project_root: &Path, args: CentralityArgs) -> Result<()> {
    let CentralityArgs {
        limit,
        include_tests,
        json: json_mode,
    } = args;
    // Clamp to >=1 (mirrors cmd_callgraph's depth.max(1)): --limit 0 would return
    // an empty ranking and trip the "No chokepoints found (graph has no multi-hop
    // call paths)" branch below — a message that falsely blames the graph when the
    // user merely asked for zero rows.
    let limit = limit.max(1);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let ranked =
        crate::graph::centrality::betweenness_centrality(conn, include_tests, limit as usize)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Empty → `[]` (array-shaped success), per the CLI JSON-empty contract.
        let items: Vec<serde_json::Value> = ranked
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "type": c.node_type,
                    "file_path": c.file_path,
                    "betweenness": c.score,
                    "normalized": c.normalized,
                    "caller_count": c.caller_count,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    if ranked.is_empty() {
        eprintln!(
            "[code-graph] No chokepoints found (graph has no multi-hop call paths{}).",
            if include_tests {
                ""
            } else {
                "; try --include-tests"
            }
        );
        return Ok(());
    }

    writeln!(
        stdout,
        "Architectural chokepoints (betweenness centrality, top {}):",
        ranked.len()
    )?;
    writeln!(stdout, "(functions on the most shortest call paths between others — high score = structural bridge)\n")?;
    for c in &ranked {
        writeln!(
            stdout,
            "  {:>8.1} ({:.3}) {} {} — {} callers ({})",
            c.score, c.normalized, c.node_type, c.name, c.caller_count, c.file_path
        )?;
    }

    Ok(())
}

/// CLI arguments for the `cycles` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp cycles",
    about = "Detect circular import dependencies (file-level)"
)]
pub struct CyclesArgs {
    /// Maximum number of cycles to report (default: 50)
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Detect circular import dependencies — strongly-connected components of the
/// file-level `imports` graph. Each cycle is a set of files that transitively
/// import each other, shown with a representative shortest loop `a → b → … → a`.
/// Reported over imports only: a `calls` cycle is mutual recursion, not a
/// circular import. Most actionable for JS/TS/Python/Go; Rust intra-crate module
/// cycles are frequently benign. CLI-only; not exposed as an MCP tool.
pub fn cmd_cycles(project_root: &Path, args: CyclesArgs) -> Result<()> {
    let CyclesArgs {
        limit,
        json: json_mode,
    } = args;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let edges = crate::storage::queries::all_file_import_edges(conn)?;
    let mut cycles = crate::graph::cycles::find_cycles(&edges);
    // Record the pre-truncation total: printing "(N found)" from the truncated
    // length under-reported ("50 found" when 80 exist) with no truncation marker
    // (disclosure-gap class, roadmap 2026-07-18 §1.5).
    let total_found = cycles.len();
    cycles.truncate(limit as usize);
    let truncated = total_found > cycles.len();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Empty → `[]` (array-shaped success), per the CLI JSON-empty contract.
        let items: Vec<serde_json::Value> = cycles
            .iter()
            .map(|c| {
                serde_json::json!({
                    "files": c.files,
                    "size": c.size,
                    "cycle": c.path,
                })
            })
            .collect();
        if truncated {
            // Disclosure envelope only when --limit actually cut results
            // (mirrors callgraph's `limit_hit`); the common untruncated case
            // keeps the plain array shape.
            writeln!(
                stdout,
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "results": items,
                    "total_found": total_found,
                    "truncated": true,
                }))?
            )?;
        } else {
            writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        }
        return Ok(());
    }

    if cycles.is_empty() {
        eprintln!("[code-graph] No circular import dependencies found.");
        return Ok(());
    }

    if truncated {
        writeln!(
            stdout,
            "Circular import dependencies (showing {} of {} found — raise --limit for the rest):",
            cycles.len(),
            total_found
        )?;
    } else {
        writeln!(
            stdout,
            "Circular import dependencies ({} found):",
            cycles.len()
        )?;
    }
    writeln!(
        stdout,
        "(files that transitively import each other — a → b → … → a)\n"
    )?;
    for c in &cycles {
        writeln!(stdout, "  {}", c.headline())?;
        // When the SCC has more files than the representative loop visits, list them all.
        if c.size + 1 > c.path.len() {
            writeln!(stdout, "    files: {}", c.files.join(", "))?;
        }
    }

    Ok(())
}

/// CLI arguments for the `surprising` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp surprising",
    about = "Surface unexpected cross-module couplings (uncertain / sole-bridge edges)"
)]
pub struct SurprisingArgs {
    /// Number of connections to report (default: 15)
    #[arg(long, default_value_t = 15)]
    pub limit: u32,
    /// Include test symbols (excluded by default)
    #[arg(long)]
    pub include_tests: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Rank "surprising connections" — cross-file `calls`/`references` edges scored by
/// resolution confidence (ambiguous > inferred > extracted), whether they cross
/// module boundaries, and whether they are the sole edge between two modules.
/// Surfaces uncertain or non-obvious couplings for review/audit; structural edges
/// (imports/inherits) are excluded. CLI-only; not exposed as an MCP tool.
pub fn cmd_surprising(project_root: &Path, args: SurprisingArgs) -> Result<()> {
    let SurprisingArgs {
        limit,
        include_tests,
        json: json_mode,
    } = args;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let found =
        crate::graph::surprising::surprising_connections(conn, include_tests, limit as usize)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Empty → `[]` (array-shaped success), per the CLI JSON-empty contract.
        let items: Vec<serde_json::Value> = found
            .iter()
            .map(|c| {
                serde_json::json!({
                    "source": c.source,
                    "source_file": c.source_file,
                    "target": c.target,
                    "target_file": c.target_file,
                    "relation": c.relation,
                    "confidence": c.confidence,
                    "score": c.score,
                    "why": c.reasons,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    if found.is_empty() {
        eprintln!(
            "[code-graph] No surprising connections found{}.",
            if include_tests {
                ""
            } else {
                " (try --include-tests)"
            }
        );
        return Ok(());
    }

    writeln!(stdout, "Surprising connections (top {}):", found.len())?;
    writeln!(
        stdout,
        "(score = low resolution confidence + crosses modules + sole bridge between them)\n"
    )?;
    for c in &found {
        writeln!(
            stdout,
            "  [{}] {} → {}  ({} {})",
            c.score, c.source, c.target, c.confidence, c.relation
        )?;
        writeln!(stdout, "      {} → {}", c.source_file, c.target_file)?;
        writeln!(stdout, "      {}", c.reasons.join("; "))?;
    }

    Ok(())
}

/// CLI arguments for the `report` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp report",
    about = "Consolidated code-health report (summary, hot functions, chokepoints, cycles, surprising, dead code)"
)]
pub struct ReportArgs {
    /// Items per section (default: 5)
    #[arg(long, default_value_t = 5)]
    pub top: u32,
    /// Include test symbols in the analyses (excluded by default)
    #[arg(long)]
    pub include_tests: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// One-shot architecture/health overview that bundles the structural analyses
/// (hot functions, betweenness chokepoints, import cycles, surprising
/// connections, dead code) plus a corpus summary with edge-confidence breakdown.
/// Pure read-time aggregation of existing analyses. CLI-only; not an MCP tool.
pub fn cmd_report(project_root: &Path, args: ReportArgs) -> Result<()> {
    use crate::domain::{CONF_AMBIGUOUS, CONF_EXTRACTED, CONF_INFERRED};
    let ReportArgs {
        top,
        include_tests,
        json: json_mode,
    } = args;
    let top = top as usize;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Dead code is computed FIRST so the query-time freshness resync can run
    // before the rest of the report: this command prints dead-code start_line
    // (JSON `line` below, and the text `file:line` rows) straight from the
    // index, and was the one line-printing subcommand with no refresh at all —
    // its own standalone `dead-code` command has had one since the shared resync
    // landed (audit 2026-08-02 FRS-5). Refreshing here rather than after the
    // other analyses also keeps the whole report on ONE index state instead of
    // mixing pre- and post-reindex counts.
    let run_dead = |conn: &rusqlite::Connection| {
        crate::storage::queries::find_dead_code(conn, None, None, include_tests, 3, top as i64)
    };
    let mut dead = run_dead(conn)?;
    let files: Vec<String> = dead.iter().map(|d| d.file_path.clone()).collect();
    let freshness = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if freshness.any_changed {
        dead = run_dead(conn)?;
    }
    freshness.disclose();

    let status = crate::storage::queries::get_index_status(conn, false)?;

    // Edge-confidence breakdown.
    let mut conf: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT confidence, COUNT(*) FROM edges GROUP BY confidence")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (c, n) = row?;
            conf.insert(c, n);
        }
    }
    let conf_get = |k: &str| conf.get(k).copied().unwrap_or(0);

    let (_modules, _deps, _entry, hot) = crate::storage::queries::get_project_map(conn)?;
    let chokepoints = crate::graph::centrality::betweenness_centrality(conn, include_tests, top)?;
    let mut cycles = {
        let edges = crate::storage::queries::all_file_import_edges(conn)?;
        crate::graph::cycles::find_cycles(&edges)
    };
    cycles.truncate(top);
    let surprising = crate::graph::surprising::surprising_connections(conn, include_tests, top)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Object envelope (sections may be empty arrays), per the CLI JSON contract.
        let mut report = serde_json::json!({
            "summary": {
                "files": status.files_count,
                "nodes": status.nodes_count,
                "edges": status.edges_count,
                "confidence": {
                    "extracted": conf_get(CONF_EXTRACTED),
                    "inferred": conf_get(CONF_INFERRED),
                    "ambiguous": conf_get(CONF_AMBIGUOUS),
                },
            },
            "hot_functions": hot.iter().take(top).map(|h| serde_json::json!({
                "name": h.name, "type": h.node_type, "file": h.file, "caller_count": h.caller_count,
            })).collect::<Vec<_>>(),
            "chokepoints": chokepoints.iter().map(|c| serde_json::json!({
                "name": c.name, "file": c.file_path, "betweenness": c.score, "caller_count": c.caller_count,
            })).collect::<Vec<_>>(),
            "import_cycles": cycles.iter().map(|c| serde_json::json!({
                "files": c.files, "size": c.size, "cycle": c.path,
            })).collect::<Vec<_>>(),
            "surprising_connections": surprising.iter().map(|c| serde_json::json!({
                "source": c.source, "target": c.target, "confidence": c.confidence, "score": c.score,
                "source_file": c.source_file, "target_file": c.target_file,
            })).collect::<Vec<_>>(),
            "dead_code": dead.iter().map(|d| serde_json::json!({
                "name": d.name, "type": d.node_type, "file": d.file_path, "line": d.start_line,
            })).collect::<Vec<_>>(),
        });
        // Object-shaped envelope, so the in-band marker applies (the stderr note
        // from `disclose()` is invisible under `--json 2>/dev/null`).
        freshness.attach_partial(&mut report);
        writeln!(stdout, "{}", serde_json::to_string(&report)?)?;
        return Ok(());
    }

    writeln!(stdout, "# Code Health Report\n")?;
    writeln!(stdout, "## Summary")?;
    writeln!(
        stdout,
        "  {} files · {} nodes · {} edges",
        status.files_count, status.nodes_count, status.edges_count
    )?;
    writeln!(
        stdout,
        "  edge confidence: {} extracted · {} inferred · {} ambiguous",
        conf_get(CONF_EXTRACTED),
        conf_get(CONF_INFERRED),
        conf_get(CONF_AMBIGUOUS)
    )?;

    writeln!(stdout, "\n## Hot functions (most-called)")?;
    if hot.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for h in hot.iter().take(top) {
        writeln!(
            stdout,
            "  {:>4} callers  {} ({}) — {}",
            h.caller_count, h.name, h.node_type, h.file
        )?;
    }

    writeln!(stdout, "\n## Architectural chokepoints (betweenness)")?;
    if chokepoints.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for c in &chokepoints {
        writeln!(stdout, "  {:>8.1}  {} — {}", c.score, c.name, c.file_path)?;
    }

    writeln!(stdout, "\n## Import cycles")?;
    if cycles.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for c in &cycles {
        writeln!(stdout, "  {}", c.headline())?;
        // For larger SCCs the shortest loop omits members — name them so the report is actionable.
        if c.size + 1 > c.path.len() {
            writeln!(stdout, "    files: {}", c.files.join(", "))?;
        }
    }

    writeln!(stdout, "\n## Surprising connections")?;
    if surprising.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for c in &surprising {
        writeln!(
            stdout,
            "  [{}] {} → {}  ({} {})",
            c.score, c.source, c.target, c.confidence, c.relation
        )?;
    }

    writeln!(stdout, "\n## Dead code (unused symbols)")?;
    if dead.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for d in &dead {
        writeln!(
            stdout,
            "  {} ({}) — {}:{}",
            d.name, d.node_type, d.file_path, d.start_line
        )?;
    }

    Ok(())
}

/// CLI arguments for the `benchmark` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp benchmark",
    about = "Benchmark index speed, query latency, token savings"
)]
pub struct BenchmarkArgs {
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Run benchmark: full index, incremental index, query latency, DB size, token savings.
pub fn cmd_benchmark(project_root: &Path, args: BenchmarkArgs) -> Result<()> {
    use crate::domain::CODE_GRAPH_DIR;
    use crate::indexer::pipeline::{run_full_index, run_incremental_index};
    use std::time::Instant;

    let json_mode = args.json;

    // Create a temporary database for benchmarking
    let data_dir = project_root.join(CODE_GRAPH_DIR);
    std::fs::create_dir_all(&data_dir)?;
    let bench_db_path = data_dir.join("benchmark-temp.db");
    if bench_db_path.exists() {
        std::fs::remove_file(&bench_db_path)?;
    }

    eprintln!("[benchmark] Indexing {}...", project_root.display());

    // 1. Full index timing
    let bench_db = Database::open(&bench_db_path)?;
    let t_full = Instant::now();
    let result = run_full_index(&bench_db, project_root, None, None)?;
    let full_index_ms = t_full.elapsed().as_millis() as u64;

    let files_indexed = result.files_indexed;
    let nodes_created = result.nodes_created;
    let edges_created = result.edges_created;

    eprintln!(
        "[benchmark] Full index: {}ms ({} files, {} nodes, {} edges)",
        full_index_ms, files_indexed, nodes_created, edges_created
    );

    // 2. Incremental index (no-change detection — should be fast)
    let t_incr = Instant::now();
    let _ = run_incremental_index(&bench_db, project_root, None, None)?;
    let incr_index_ms = t_incr.elapsed().as_millis() as u64;

    eprintln!("[benchmark] Incremental (no-change): {}ms", incr_index_ms);

    // 3. Query latency: run 5 FTS searches, compute P50/P99
    let test_queries = ["function", "error", "config", "parse", "index"];
    let mut query_times_us: Vec<u64> = Vec::with_capacity(test_queries.len());
    let conn = bench_db.conn();

    for q in &test_queries {
        let t_q = Instant::now();
        let _ = queries::fts5_search(conn, q, 10)?;
        query_times_us.push(t_q.elapsed().as_micros() as u64);
    }

    query_times_us.sort();
    let p50_us = query_times_us[query_times_us.len() / 2];
    let p99_us = query_times_us[query_times_us.len() - 1]; // with 5 samples, P99 ≈ max

    eprintln!(
        "[benchmark] Query latency P50: {}us, P99: {}us",
        p50_us, p99_us
    );

    // 4. DB size
    let db_size_bytes = std::fs::metadata(&bench_db_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let db_size_mb = db_size_bytes as f64 / (1024.0 * 1024.0);

    // 5. Token savings estimate: avg code_content length / 3.0 tokens per char
    let avg_content_len: f64 = conn
        .query_row(
            "SELECT COALESCE(AVG(LENGTH(code_content)), 0) FROM nodes WHERE code_content IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0.0);
    let avg_tokens = avg_content_len / 3.0;

    // Clean up: drop connection before deleting file
    drop(bench_db);
    if bench_db_path.exists() {
        std::fs::remove_file(&bench_db_path)?;
    }
    // Also clean up WAL/SHM files that SQLite may leave behind
    let wal_path = bench_db_path.with_extension("db-wal");
    let shm_path = bench_db_path.with_extension("db-shm");
    if wal_path.exists() {
        let _ = std::fs::remove_file(&wal_path);
    }
    if shm_path.exists() {
        let _ = std::fs::remove_file(&shm_path);
    }

    if json_mode {
        let json = serde_json::json!({
            "full_index_ms": full_index_ms,
            "incremental_index_ms": incr_index_ms,
            "files_indexed": files_indexed,
            "nodes_created": nodes_created,
            "edges_created": edges_created,
            "query_p50_us": p50_us,
            "query_p99_us": p99_us,
            "db_size_mb": (db_size_mb * 100.0).round() / 100.0,
            "avg_tokens_per_node": (avg_tokens * 10.0).round() / 10.0,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "Benchmark Results")?;
        writeln!(stdout, "=================")?;
        writeln!(stdout)?;
        writeln!(
            stdout,
            "Full index:          {:>8}ms  ({} files, {} nodes, {} edges)",
            full_index_ms, files_indexed, nodes_created, edges_created
        )?;
        writeln!(stdout, "Incremental (noop):  {:>8}ms", incr_index_ms)?;
        writeln!(stdout, "Query latency P50:   {:>8}us", p50_us)?;
        writeln!(stdout, "Query latency P99:   {:>8}us", p99_us)?;
        writeln!(stdout, "DB size:             {:>8.2}MB", db_size_mb)?;
        writeln!(stdout, "Avg tokens/node:     {:>8.1}", avg_tokens)?;
    }

    Ok(())
}

// --- snapshot subcommand (nested create/inspect) ---

/// CLI arguments for the `snapshot` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp snapshot",
    about = "Build or inspect a portable graph snapshot"
)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub command: SnapshotCommand,
}

/// `snapshot` sub-subcommands (replaces the hand-rolled args[2]/args[3] dispatch).
#[derive(Subcommand, Debug)]
pub enum SnapshotCommand {
    /// Build a portable graph snapshot (auto zstd when --out ends in .db.zst)
    Create(SnapshotCreateArgs),
    /// Print snapshot metadata as JSON (accepts .db or .db.zst)
    Inspect(SnapshotInspectArgs),
}

/// `snapshot create` arguments.
#[derive(Parser, Debug)]
pub struct SnapshotCreateArgs {
    /// Output path (auto zstd-compresses when it ends in .db.zst)
    #[arg(long)]
    pub out: String,
    /// Include embedding vectors in the snapshot
    #[arg(long)]
    pub include_embeddings: bool,
    /// Project root to snapshot (default: the resolved project root)
    #[arg(long)]
    pub root: Option<String>,
    /// Suppress the "snapshot created" confirmation
    #[arg(long)]
    pub quiet: bool,
}

/// `snapshot inspect` arguments.
#[derive(Parser, Debug)]
pub struct SnapshotInspectArgs {
    /// Snapshot file to inspect (.db or .db.zst; format from magic bytes)
    pub file: String,
}

/// Build a portable graph snapshot. `snapshot create --out <path>
/// [--include-embeddings] [--root <dir>] [--quiet]`.
pub fn cmd_snapshot_create(project_root: &Path, args: SnapshotCreateArgs) -> Result<()> {
    let SnapshotCreateArgs {
        out,
        include_embeddings: include,
        root,
        quiet,
    } = args;

    let root = root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_root.to_path_buf());

    // Pre-flight checks for --out so SQLite VACUUM INTO doesn't leak its
    // raw "unable to open database file" error when the user passed a dir
    // or a path with a missing parent directory.
    let out_path = std::path::Path::new(&out);
    if out_path.is_dir() || out.ends_with('/') {
        anyhow::bail!(
            "--out '{}' is a directory; expected a file path (e.g. '{}snapshot.db' or '{}snapshot.db.zst')",
            out, out, out
        );
    }
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            anyhow::bail!(
                "--out parent directory does not exist: {} (create it first with `mkdir -p {}`)",
                parent.display(),
                parent.display()
            );
        }
    }

    crate::snapshot::create(&root, out_path, include)?;
    if !quiet {
        eprintln!("snapshot created: {}", out);
        if out.ends_with(".db.zst") {
            eprintln!(
                "integrity sidecar: {out}.blake3 \u{2014} upload BOTH to the release; \
                 consumers verify the checksum before decompressing"
            );
        }
    }
    Ok(())
}

/// Print snapshot metadata as JSON to stdout. Accepts `.db` or `.db.zst`
/// (format detected from magic bytes, not extension).
pub fn cmd_snapshot_inspect(args: SnapshotInspectArgs) -> Result<()> {
    let meta = crate::snapshot::inspect(std::path::Path::new(&args.file))?;
    println!("{}", serde_json::to_string_pretty(&meta)?);
    Ok(())
}

// --- reindex subcommand ---

/// CLI arguments for the `reindex` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp reindex",
    about = "Incremental index refresh; --from-snapshot drops the index and refetches the published snapshot (rebuild-index for an unconditional rebuild)"
)]
pub struct ReindexArgs {
    /// Refetch the published snapshot before indexing (falls back to full index)
    #[arg(long)]
    pub from_snapshot: bool,
    /// Index structure only and skip embeddings (vectors backfill later).
    #[arg(long)]
    pub no_embed: bool,
    /// With --from-snapshot: drop the index even while another process holds the
    /// index lock (its pending writes are lost).
    #[arg(long)]
    pub force: bool,
    /// Print the resulting index run's counters as one JSON object on stdout
    /// (progress and snapshot notices stay on stderr).
    #[arg(long)]
    pub json: bool,
}

/// `reindex [--from-snapshot]` — wipe `.code-graph/` index files and re-fetch
/// snapshot (or full-index if no snapshot available). Without `--from-snapshot`,
/// behaves identically to `incremental-index`.
///
/// Equivalent to user-side `rm -rf .code-graph/index.db*` + restarting the
/// MCP server, but with optional snapshot-bootstrap acceleration.
pub fn cmd_reindex(project_root: &Path, args: ReindexArgs) -> Result<()> {
    let from_snapshot = args.from_snapshot;
    let no_embed = args.no_embed;
    let cg_dir = project_root.join(crate::domain::CODE_GRAPH_DIR);

    // Held across the unlink AND the snapshot install — the whole window in
    // which index.db is missing or half-landed — then released explicitly before
    // the incremental step below. It cannot stay held through that call:
    // `cmd_incremental_index` probes the same lock, and flock is per open file
    // DESCRIPTION, so our own guard would answer "another process holds it" and
    // print a warning about ourselves.
    let mut index_lock: Option<crate::mcp::server::IndexLockGuard> = None;
    if from_snapshot && cg_dir.exists() {
        // Same door as `rebuild-index`: unlinking index.db under a running
        // server strands its open fd on the deleted inode (audit P1-3). Taken
        // BEFORE the removal so a refusal leaves the index untouched.
        index_lock = lock_index_for_replace(&cg_dir, args.force, false)?;
        // Remove just index.db + WAL files; leave usage.jsonl etc. intact.
        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            let _ = std::fs::remove_file(cg_dir.join(name));
        }
    }

    if from_snapshot {
        if let Some(url) = crate::snapshot::resolve_snapshot_source(project_root) {
            match crate::snapshot::try_install(&url, project_root) {
                Ok(commit) => {
                    eprintln!("Snapshot installed at commit {commit}");
                    drop(index_lock);
                    return cmd_incremental_index_opts(project_root, false, no_embed, args.json);
                }
                Err(e) => eprintln!("Snapshot install failed ({e}), falling back to full index"),
            }
        } else {
            eprintln!("No snapshot source resolved, falling back to full index");
        }
    }

    drop(index_lock);
    cmd_incremental_index_opts(project_root, false, no_embed, args.json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawning `rg` reports `ErrorKind::NotFound` for two different causes —
    /// the binary is missing, or `current_dir` does not exist — and the message
    /// named only the first. A user whose indexed project root had been moved was
    /// told to install a tool that was already on their PATH.
    #[test]
    fn rg_spawn_failure_message_distinguishes_missing_cwd_from_missing_binary() {
        let present = tempfile::TempDir::new().unwrap();
        let msg = rg_spawn_failure_message(present.path());
        assert!(
            msg.contains("ripgrep (rg) not found"),
            "an existing root must still point at the binary: {msg}"
        );

        let gone = tempfile::TempDir::new().unwrap();
        let gone_path = gone.path().to_path_buf();
        drop(gone);
        let msg = rg_spawn_failure_message(&gone_path);
        assert!(
            msg.contains("does not exist"),
            "a vanished project root must be named as the cause: {msg}"
        );
        assert!(
            !msg.contains("Install"),
            "must not tell the user to install a tool they have: {msg}"
        );
    }

    /// M2: `deps` barrel-pattern scanning must not follow a symlink that escapes
    /// the project root. The scanner reads a caller-supplied path and echoes its
    /// import/export lines; an in-repo symlink to an outside file would turn it
    /// into a restricted file-read oracle. Mirrors read_source_context's guard.
    #[test]
    #[cfg(unix)]
    fn scan_barrel_patterns_refuses_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let secret = outside.path().join("secret.ts");
        std::fs::write(
            &secret,
            "import { KEY } from './creds';\nexport { KEY } from './creds';\n",
        )
        .unwrap();
        let link = root.path().join("link.ts");
        symlink(&secret, &link).unwrap();

        assert!(
            scan_barrel_patterns(root.path(), "link.ts").is_none(),
            "scan_barrel_patterns must not follow a symlink escaping the project root",
        );

        // Positive control: an in-root barrel file with the same shape IS scanned.
        std::fs::write(root.path().join("real.ts"), "import { A } from './a';\n").unwrap();
        let ok = scan_barrel_patterns(root.path(), "real.ts");
        assert!(
            ok.as_ref().is_some_and(|h| !h.is_empty()),
            "an in-root barrel file must still be scanned; got: {ok:?}",
        );
    }

    /// META⑤ drift-guard: `deps` confines file reads to the project root via
    /// canonicalize. This asserts an out-of-root symlink yields NO leaked
    /// content on EVERY read path `deps` uses for a given file. Step 1 audit
    /// (cli.rs cmd_deps, 4945-5111) found `scan_barrel_patterns` is the SOLE
    /// content-reader reachable from `deps`: `get_import_tree` only queries the
    /// DB, and the MCP twin `tool_dependency_graph`
    /// (src/mcp/server/tools/advanced.rs) never reads file contents at all —
    /// no barrel fallback exists on the MCP side. `read_source_context`
    /// (cli.rs:4626) has the same guard shape but is a `cmd_show`/`cmd_similar`
    /// helper, not reachable from `deps` for a given `file_path`, so it is not
    /// a `deps` sibling and is out of scope here. This test therefore documents
    /// the single-reader finding and pins `scan_barrel_patterns` as that sole
    /// path, so the confinement can't be fixed on one path and silently left
    /// open on a newly-added sibling later.
    /// Negative control: remove the `canonical.starts_with(&root_canonical)`
    /// guard in `scan_barrel_patterns` and this returns `Some(...)` → fails.
    #[test]
    #[cfg(unix)]
    fn deps_read_paths_all_refuse_symlink_escape() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outside = tmp
            .path()
            .parent()
            .unwrap()
            .join("secret-deps-drift-guard.ts");
        std::fs::write(&outside, "import { s } from './secret-source';\n").unwrap();
        let link = root.join("link.ts");
        symlink(&outside, &link).unwrap();

        // Sole read path deps uses for file contents: the barrel-scan fallback.
        // Must refuse the escape and must never surface the outside content.
        let result = scan_barrel_patterns(root, "link.ts");
        assert!(
            result.is_none(),
            "deps' sole content-read path (scan_barrel_patterns) followed a \
             symlink escaping the project root: {result:?}",
        );

        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn test_no_embed_flag_parses_on_index_commands() {
        // `--no-embed` is the published fast-path opt-out: structure-first index,
        // skip the slow embedding pass. Verify it wires on every index command and
        // defaults off (embedding stays the default so existing behaviour holds).
        assert!(IncrementalIndexArgs::parse_from(["incremental-index", "--no-embed"]).no_embed);
        assert!(!IncrementalIndexArgs::parse_from(["incremental-index"]).no_embed);
        assert!(ReindexArgs::parse_from(["reindex", "--no-embed"]).no_embed);
        assert!(!ReindexArgs::parse_from(["reindex"]).no_embed);
        assert!(
            RebuildIndexArgs::parse_from(["rebuild-index", "--confirm", "--no-embed"]).no_embed
        );
        assert!(!RebuildIndexArgs::parse_from(["rebuild-index", "--confirm"]).no_embed);
    }

    #[test]
    fn test_no_embed_builds_structural_index_without_vectors() {
        // A --no-embed full index must still produce the structural graph (nodes),
        // and must leave zero vectors regardless of model availability — the fast,
        // query-ready state the flag promises.
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.rs"), "fn alpha() { beta(); }\nfn beta() {}\n").unwrap();
        let db_path = root.join(CODE_GRAPH_DIR).join("index.db");

        build_full_index_at(&db_path, root, true, true).unwrap();

        let db = Database::open_nondestructive(&db_path).unwrap();
        let nodes: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert!(
            nodes > 0,
            "structure index must be built even with --no-embed"
        );
        let (embedded, _embeddable) = queries::count_nodes_with_vectors(db.conn()).unwrap();
        assert_eq!(embedded, 0, "--no-embed must leave zero vectors");
    }

    #[test]
    fn test_aggregate_recommendations_research_after_answer_and_observe() {
        // Append order = chronological. Each answered deny "arms"; the next
        // grep/read event is a re-search (inline answer didn't end the hunt).
        //   t1 answered deny → t2 grep observe  = re-search
        //   t3 answered deny → t4 cli use       = conversion, NOT re-search
        //   t5 answered deny → t6 read observe  = re-search (read after answer)
        //   t7 UNanswered deny → t8 grep observe = not armed (only answered denies)
        let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"observe\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t4\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"t5\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t6\",\"hook\":\"read\",\"action\":\"observe\"}
{\"ts\":\"t7\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false}
{\"ts\":\"t8\",\"hook\":\"grep\",\"action\":\"observe\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(s.deny_answered, 3, "t1,t3,t5 answered");
        assert_eq!(s.deny_unanswered, 1, "t7 unanswered");
        assert_eq!(s.researched_after_answer, 2,
            "t1→t2 (re-grep) and t5→t6 (read) count; t3→t4 (cli use) is a conversion, not re-search");
        // Both follow-ups here are observe (a file read acting on the delivered
        // answer) → neither a sustained drill-down nor a fall-through cg failed.
        assert_eq!(
            s.sustained_after_answer, 0,
            "no follow-up was itself answered by cg"
        );
        assert_eq!(
            s.fallthrough_after_answer, 0,
            "no follow-up was a search cg couldn't satisfy"
        );
        assert_eq!(s.observe, 3, "t2,t6,t8 observes");
        assert_eq!(s.cli_uses, 1, "t4");
        assert_eq!(
            s.by_action.get("observe"),
            None,
            "observe is not a recommendation action"
        );
        assert_eq!(
            s.total, 4,
            "4 denies counted; observe + cli use excluded from total"
        );
    }

    #[test]
    fn test_aggregate_recommendations_followup_split_sustained_vs_fallthrough() {
        // The honest split of "follow-up after an answered deny": cg either
        // answered the next step too (sustained drill-down — a win), the model
        // read a file (observe — acting on the answer), or fell through to a
        // search cg couldn't satisfy (the real insufficiency). `use` between pairs
        // is a clean disarm (conversion, not a search).
        //   L1 answered deny → L2 answered deny       = sustained (cg kept up); L2 re-arms
        //   L2 (armed) → L3 cli use                   = conversion → disarm
        //   L4 answered deny → L5 static deny         = fall-through (cg couldn't); L5 unanswered → no arm
        //   L6 answered deny → L7 grep hint (advisory) = fall-through (no delivered answer)
        //   L8 answered deny → L9 read observe        = neither (acting on answer) → disarm
        //   L10 answered deny → L11 cli use           = conversion → disarm
        //   L12 answered deny → (end)                 = no follow-up (answer sufficed)
        let content = "\
{\"ts\":\"L1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L2\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"mode\":\"grep\"}
{\"ts\":\"L3\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"L4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L5\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false}
{\"ts\":\"L6\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L7\",\"hook\":\"grep\",\"action\":\"hint\"}
{\"ts\":\"L8\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L9\",\"hook\":\"read\",\"action\":\"observe\"}
{\"ts\":\"L10\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"L11\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"L12\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(s.deny_answered, 7, "L1,L2,L4,L6,L8,L10,L12 answered");
        assert_eq!(s.deny_unanswered, 1, "L5 static");
        assert_eq!(
            s.researched_after_answer, 4,
            "L2,L5,L7,L9 are search follow-ups; L3/L11 use disarm"
        );
        assert_eq!(
            s.sustained_after_answer, 1,
            "L1→L2: cg answered the follow-up too"
        );
        assert_eq!(
            s.fallthrough_after_answer, 2,
            "L4→L5 (static) and L6→L7 (advisory hint): cg couldn't satisfy"
        );
        assert_eq!(s.observe, 1, "L9");
        assert_eq!(s.cli_uses, 2, "L3,L11");
    }

    #[test]
    fn test_aggregate_recommendations_same_pattern_regrep_is_fallthrough() {
        // Pattern fingerprint tightens `sustained` (the documented upper bound):
        // a verbatim re-grep of the SAME denied pattern after cg answered means
        // the inline answer was ignored/insufficient → fall-through, NOT a
        // drill-down win (and NOT "acting on the answer" even when it lands as a
        // grep observe within the cooldown window). A DIFFERENT pattern is genuine
        // drill-down → sustained. A follow-up WITHOUT a pattern (read observe, or
        // any pre-fix event) keeps the old behavior — back-compatible.
        //   A1 answered deny pattern=foo → arm(foo)
        //   A2 answered deny pattern=foo → SAME (re-deny after cooldown) → fall-through; re-arm(foo)
        //   A3 cli use                   → disarm
        //   A4 answered deny pattern=bar → arm(bar)
        //   A5 grep observe pattern=bar  → SAME (re-grep within cooldown) → fall-through
        //   A6 answered deny pattern=baz → arm(baz)
        //   A7 answered deny pattern=qux → DIFFERENT → sustained; re-arm(qux)
        //   A8 cli use                   → disarm
        //   A9 answered deny pattern=zap → arm(zap)
        //   A10 read observe (no pattern)→ neither (acting on the answer)
        let content = "\
{\"ts\":\"A1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"foo\"}
{\"ts\":\"A2\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"foo\"}
{\"ts\":\"A3\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"A4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"bar\"}
{\"ts\":\"A5\",\"hook\":\"grep\",\"action\":\"observe\",\"pattern\":\"bar\"}
{\"ts\":\"A6\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"baz\"}
{\"ts\":\"A7\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"qux\"}
{\"ts\":\"A8\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"A9\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"zap\"}
{\"ts\":\"A10\",\"hook\":\"read\",\"action\":\"observe\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(s.deny_answered, 6, "A1,A2,A4,A6,A7,A9 answered");
        assert_eq!(
            s.researched_after_answer, 4,
            "A2,A5,A7,A10 follow answered denies; A3/A8 use disarm"
        );
        assert_eq!(
            s.fallthrough_after_answer, 2,
            "A1→A2 (same-pattern re-deny) and A4→A5 (same-pattern re-grep observe): answer ignored"
        );
        assert_eq!(
            s.sustained_after_answer, 1,
            "A6→A7: different pattern = genuine drill-down cg also answered"
        );
        assert_eq!(s.observe, 2, "A5,A10");
        assert_eq!(s.cli_uses, 2, "A3,A8");
    }

    #[test]
    fn test_aggregate_recommendations_inconclusive_followup_excluded_from_fallthrough() {
        // Consumer-data over-count fix: a follow-up after an answered deny that is
        // itself a NULL signal about the prior answer must NOT count as fall-through.
        // Two shapes — `no-hits` (cg ran the next grep, found nothing → a NEW query,
        // since a verbatim re-grep of the answered pattern would re-hit it) and
        // `unavailable` (cg CLI couldn't run → infra). Same honesty principle as the
        // v0.64 drill-down/observe exclusion. Same-pattern still wins (verbatim
        // re-grep = answer ignored = real fall-through, even if it now finds nothing).
        //   N1 answered deny → N2 grep hint fallthrough=no-hits          = inconclusive
        //   N3 answered deny → N4 grep deny answered:false reason=unavail = inconclusive
        //   N5 answered deny → N6 grep static deny (answered:false)       = fall-through (cg couldn't)
        //   N7 answered deny pattern=foo → N8 same-pattern deny no-hits   = fall-through (pattern wins)
        let content = "\
{\"ts\":\"N1\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"N2\",\"hook\":\"grep\",\"action\":\"hint\",\"fallthrough\":\"no-hits\"}
{\"ts\":\"N3\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"N4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false,\"reason\":\"unavailable\"}
{\"ts\":\"N5\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"N6\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false}
{\"ts\":\"N7\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"foo\"}
{\"ts\":\"N8\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":false,\"pattern\":\"foo\",\"fallthrough\":\"no-hits\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(s.deny_answered, 4, "N1,N3,N5,N7 answered");
        assert_eq!(
            s.researched_after_answer, 4,
            "N2,N4,N6,N8 all follow answered denies"
        );
        assert_eq!(
            s.followup_inconclusive, 2,
            "N2 (no-hits) + N4 (unavailable): null signal, excluded"
        );
        assert_eq!(
            s.fallthrough_after_answer, 2,
            "N6 (static deny cg couldn't satisfy) + N8 (same-pattern re-grep wins over no-hits)"
        );
        assert_eq!(
            s.sustained_after_answer, 0,
            "no follow-up was itself answered by cg"
        );
    }

    #[test]
    fn test_aggregate_recommendations_inject_arms_and_scores_fallthrough_vs_sustained() {
        // Compound-grep PostToolUse inject: an ANSWERED inject (cg delivered the
        // AST-aware view of a compound-command grep, permission-neutrally) arms the
        // funnel exactly like an answered deny. The immediately-next search event
        // scores the inject's sufficiency, parallel to deny→fallthrough:
        //   I1 inject pattern=foo → arm(foo)
        //   I2 grep observe pattern=foo  → SAME pattern re-grep = inline answer ignored → fall-through; disarm
        //   I3 inject pattern=bar → arm(bar)
        //   I4 grep deny answered=true pattern=qux → DIFFERENT pattern, cg also answered = sustained; re-arm(qux)
        //   I5 cli use → conversion → disarm
        //   I6 inject pattern=baz → arm(baz)
        //   I7 (end) → no follow-up (answer sufficed)
        // inject also counts in total/by_action via the generic map (it is a
        // recommendation event, like deny/hint — NOT observe/use/live_impact).
        let content = "\
{\"ts\":\"I1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"foo\",\"mode\":\"grep\"}
{\"ts\":\"I2\",\"hook\":\"grep\",\"action\":\"observe\",\"pattern\":\"foo\"}
{\"ts\":\"I3\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"bar\",\"mode\":\"grep\"}
{\"ts\":\"I4\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"qux\"}
{\"ts\":\"I5\",\"hook\":\"cli\",\"action\":\"use\",\"cmd\":\"grep\"}
{\"ts\":\"I6\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"baz\",\"mode\":\"grep\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(
            s.by_action.get("inject"),
            Some(&3),
            "I1,I3,I6 are inject recommendation events"
        );
        assert_eq!(
            *s.by_hook.get("grep").unwrap(),
            4,
            "I1,I3,I4,I6 are grep recommendation events (I2 observe excluded)"
        );
        assert_eq!(
            s.total, 4,
            "I1,I3,I4,I6 in total; I2 observe + I5 use excluded"
        );
        assert_eq!(
            s.researched_after_answer, 2,
            "I2 (after I1) and I4 (after I3) follow answered injects"
        );
        assert_eq!(
            s.fallthrough_after_answer, 1,
            "I1→I2: same-pattern re-grep = inline inject ignored"
        );
        assert_eq!(
            s.sustained_after_answer, 1,
            "I3→I4: different pattern, cg also answered = drill-down"
        );
        assert_eq!(s.observe, 1, "I2");
        assert_eq!(s.cli_uses, 1, "I5");
    }

    #[test]
    fn test_aggregate_recommendations_inject_by_mode() {
        // The inject payload has a `mode` (callgraph | grep | show). callgraph is the
        // marginal-value cross-file tree; grep/show echo the model's own hits (audit:
        // 0 CONSUMED). `inject_by_mode` breaks the mix down so the callgraph share is
        // directly readable — it is a SUB-breakdown of the inject events, so the total
        // inject count in by_action stays authoritative (not double-counted).
        let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"a\",\"mode\":\"callgraph\"}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"b\",\"mode\":\"grep\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"c\",\"mode\":\"callgraph\"}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"d\",\"mode\":\"show\"}
{\"ts\":\"t5\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"e\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(
            s.by_action.get("inject"),
            Some(&5),
            "all 5 are inject events"
        );
        assert_eq!(s.inject_by_mode.get("callgraph"), Some(&2), "t1,t3");
        assert_eq!(s.inject_by_mode.get("grep"), Some(&1), "t2");
        assert_eq!(s.inject_by_mode.get("show"), Some(&1), "t4");
        // t5 has no `mode` field → not counted in any mode bucket; the mode map sums
        // to 4 while by_action.inject stays 5 (mode is best-effort metadata).
        assert_eq!(
            s.inject_by_mode.values().sum::<u64>(),
            4,
            "t5 (no mode) is uncounted"
        );
    }

    #[test]
    fn test_worktree_main_root_boundaries() {
        // Linked worktree (.git FILE, gitdir → …/.git/worktrees/<n>) resolves to
        // the main checkout; a submodule pointer (…/.git/modules/…) and a regular
        // repo (.git DIRECTORY) must both return None (hard boundaries — mirrors
        // project-root.js worktreeMainRoot).
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();

        std::fs::write(
            d.join(".git"),
            format!("gitdir: {}/main/.git/worktrees/wt\n", d.display()),
        )
        .unwrap();
        assert_eq!(worktree_main_root(d), Some(d.join("main")));

        std::fs::write(
            d.join(".git"),
            format!("gitdir: {}/outer/.git/modules/sub\n", d.display()),
        )
        .unwrap();
        assert_eq!(
            worktree_main_root(d),
            None,
            "submodule gitdir is a hard boundary"
        );

        // Separator-agnostic marker (CI windows-latest caught this on v0.100.1):
        // git writes forward slashes in gitdir even on Windows — the first case
        // above already covers that; this one pins the backslash shape, with the
        // returned prefix keeping its ORIGINAL separators.
        std::fs::write(
            d.join(".git"),
            format!("gitdir: {}\\main\\.git\\worktrees\\wt\n", d.display()),
        )
        .unwrap();
        assert_eq!(
            worktree_main_root(d),
            Some(PathBuf::from(format!("{}\\main", d.display()))),
            "backslash gitdir must resolve, preserving native separators"
        );

        std::fs::remove_file(d.join(".git")).unwrap();
        std::fs::create_dir(d.join(".git")).unwrap();
        assert_eq!(worktree_main_root(d), None, ".git DIRECTORY = regular repo");
    }

    #[test]
    fn test_aggregate_recommendations_inject_skipped() {
        // v0.99.1 (roadmap §1.6): the PostToolUse non-hits path now RECORDS
        // (answered:false + fallthrough/reason) instead of staying dark, so the
        // funnel can tell "hook ran, nothing to say" from "hook never fired".
        // answered:false must count in inject_skipped, must NOT arm the funnel,
        // and a skip following an answered deny must score via the existing
        // inconclusive shapes (fallthrough:"no-hits" / reason:"unavailable").
        let content = "\
{\"ts\":\"t1\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"a\",\"fallthrough\":\"no-hits\",\"reason\":\"no-hits\"}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":true,\"pattern\":\"b\",\"mode\":\"callgraph\"}
{\"ts\":\"t3\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true,\"pattern\":\"c\"}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"inject\",\"answered\":false,\"pattern\":\"d\",\"fallthrough\":\"unavailable\",\"reason\":\"unavailable\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(s.inject_skipped, 2, "t1 + t4 are skips");
        assert_eq!(
            s.by_action.get("inject"),
            Some(&3),
            "skips still count as inject events"
        );
        // t1 (answered:false) must NOT arm — so t2 is not scored as a re-search.
        // t2 (answered inject) arms → t3 (answered deny, different pattern) scores
        // sustained; t3 arms → t4's reason:"unavailable" scores inconclusive, not
        // fallthrough. Total re-searches: t2→t3 and t3→t4 (NOT t1→t2).
        assert_eq!(
            s.researched_after_answer, 2,
            "t2→t3 and t3→t4; t1 must not arm"
        );
        assert_eq!(s.sustained_after_answer, 1, "t3 after answered t2");
        assert_eq!(
            s.followup_inconclusive, 1,
            "unavailable skip is a null signal"
        );
        assert_eq!(s.fallthrough_after_answer, 0);
    }

    #[test]
    fn test_aggregate_recommendations_counts_live_impact_separately() {
        // v0.63 — SessionStart live-context injections are a separate counter,
        // like observe/use: NOT in total/by_action, and they don't trip the
        // re-search arming (hook:"session" is not a grep/read search event).
        let content = "\
{\"ts\":\"t1\",\"hook\":\"session\",\"action\":\"live_impact\",\"blast\":72,\"direct\":41,\"wip\":true}
{\"ts\":\"t2\",\"hook\":\"grep\",\"action\":\"deny\",\"answered\":true}
{\"ts\":\"t3\",\"hook\":\"session\",\"action\":\"live_impact\",\"blast\":3,\"direct\":1,\"wip\":false}
{\"ts\":\"t4\",\"hook\":\"grep\",\"action\":\"hint\"}
";
        let s = aggregate_recommendations_jsonl(content);
        assert_eq!(s.live_impact, 2, "t1,t3 live_impact");
        assert_eq!(s.total, 2, "only the deny + hint are recommendation events");
        assert_eq!(
            s.by_action.get("live_impact"),
            None,
            "live_impact is not a recommendation action"
        );
        assert_eq!(
            s.by_hook.get("session"),
            None,
            "session hook is not a recommendation hook"
        );
        // t2 answered deny arms; t3 is live_impact (not a search event) → it must
        // NOT count as a re-search and must disarm.
        assert_eq!(
            s.researched_after_answer, 0,
            "live_impact after an answered deny is not a re-search"
        );
    }

    #[test]
    fn resolve_project_root_prefers_existing_index_at_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let idx_dir = cwd.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&idx_dir).unwrap();
        std::fs::write(idx_dir.join("index.db"), b"").unwrap();
        assert_eq!(resolve_project_root_from(cwd), cwd);
    }

    // Helper: give `dir` a `.code-graph/index.db` (explicit join per #937).
    fn write_index(dir: &Path) {
        let idx = dir.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(idx.join("index.db"), b"").unwrap();
    }

    #[test]
    fn resolve_project_root_skips_stray_nested_index() {
        // monorepo: root has .git + index; a subdir carries a STRAY index (relic
        // from an older binary) but no .git of its own. Resolving from the subdir
        // must climb to the real root, not pin the stray nested index.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_index(root);
        let sub = root.join("backend");
        std::fs::create_dir_all(&sub).unwrap();
        write_index(&sub);
        assert_eq!(resolve_project_root_from(&sub), root);
    }

    #[test]
    fn resolve_project_root_stray_index_between_cwd_and_git_root_prefers_git_root() {
        // 3-level monorepo: the real root has .git + index; an INTERMEDIATE subdir
        // carries a STRAY index (no .git of its own); cwd is BELOW that stray. The
        // nearest indexed ancestor is the stray, but the canonical project root is
        // the indexed .git root — resolving must prefer it, matching the JS resolver
        // (project-root.js). Otherwise the CLI reads the stray DB while the hooks
        // read the root DB = split-brain (M7).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_index(root);
        let mid = root.join("packages").join("app");
        std::fs::create_dir_all(&mid).unwrap();
        write_index(&mid); // stray nested index, no .git
        let cwd = mid.join("src");
        std::fs::create_dir_all(&cwd).unwrap();
        assert_eq!(
            resolve_project_root_from(&cwd),
            root,
            "cwd below a stray nested index must resolve to the indexed .git root, not the stray",
        );
    }

    #[test]
    fn resolve_project_root_nested_index_with_own_git_still_wins() {
        // A real nested repo (submodule / vendored project) has its OWN .git, so
        // its index is legitimate even under an indexed parent — keep it.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write_index(root);
        let sub = root.join("vendored");
        std::fs::create_dir_all(sub.join(".git")).unwrap();
        write_index(&sub);
        assert_eq!(resolve_project_root_from(&sub), sub);
    }

    #[test]
    fn resolve_project_root_standalone_index_no_ancestor_still_wins() {
        // No ancestor index → a cwd index is the genuine root (guards against the
        // stray-skip over-reaching into the common single-project case).
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        write_index(cwd);
        assert_eq!(resolve_project_root_from(cwd), cwd);
    }

    #[test]
    fn resolve_project_root_cwd_own_git_no_index_is_boundary() {
        // A fresh project dir with its own `.git` but no index yet (the metrics-
        // isolation fixture) roots at itself, never an indexed ancestor.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_index(root);
        let sub = root.join("pkg");
        std::fs::create_dir_all(sub.join(".git")).unwrap();
        assert_eq!(resolve_project_root_from(&sub), sub);
    }

    #[test]
    fn resolve_project_root_home_boundary_ignores_outer_index() {
        // `~` is both a git repo AND indexed; a project below it with its own
        // index but no `.git` must resolve to itself, not be hijacked to `~`.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".git")).unwrap();
        write_index(home);
        let proj = home.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        write_index(&proj);
        assert_eq!(resolve_project_root_bounded(&proj, Some(home)), proj);
    }

    #[test]
    fn resolve_project_root_non_git_monorepo_prefers_indexed_ancestor() {
        // No `.git` anywhere: a stray subdir index under a non-git indexed root
        // resolves to the indexed ancestor (parity with the JS resolver).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_index(root);
        let sub = root.join("backend");
        std::fs::create_dir_all(&sub).unwrap();
        write_index(&sub);
        // Bound at the tmp parent so the real `~/.code-graph` can't interfere.
        assert_eq!(resolve_project_root_bounded(&sub, root.parent()), root);
    }

    #[test]
    fn resolve_project_root_unindexed_git_root_uses_indexed_mid() {
        // outer/.git (unindexed) / proj/index / backend/stray-index → resolve to
        // the indexed mid dir, not the empty git root.
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        write_index(&proj);
        let backend = proj.join("backend");
        std::fs::create_dir_all(&backend).unwrap();
        write_index(&backend);
        assert_eq!(resolve_project_root_bounded(&backend, outer.parent()), proj);
    }

    #[test]
    fn test_record_cli_use_rotates_recommendations_jsonl() {
        // record_cli_use is the sole reader of CODE_GRAPH_INTERNAL and no other
        // test mutates it, so toggling it here is race-free.
        std::env::remove_var("CODE_GRAPH_INTERNAL");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cg = root.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&cg).unwrap();
        let rec = cg.join("recommendations.jsonl");
        // Pre-fill > 1MB of prior recommendation lines.
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&rec).unwrap();
            let pad = "x".repeat(1024);
            for i in 0..1200 {
                writeln!(f, "{{\"old\":{i},\"pad\":\"{pad}\"}}").unwrap();
            }
        }
        assert!(std::fs::metadata(&rec).unwrap().len() > 1_048_576);

        record_cli_use(root, "callgraph");

        let size = std::fs::metadata(&rec).unwrap().len();
        assert!(
            size < 600_000,
            "recommendations.jsonl should be rotated, got {size} bytes"
        );
        // The freshly recorded use line is last + valid; first surviving line is whole JSON.
        let content = std::fs::read_to_string(&rec).unwrap();
        let last: serde_json::Value =
            serde_json::from_str(content.trim().lines().last().unwrap()).unwrap();
        assert_eq!(last["action"], "use");
        assert_eq!(last["cmd"], "callgraph");
        serde_json::from_str::<serde_json::Value>(content.lines().next().unwrap()).unwrap();
    }

    #[test]
    fn test_record_cli_use_skips_when_no_metrics_sentinel_present() {
        // A `.code-graph/.no-metrics` sentinel silences the recommendations-log
        // writer so a dev/dogfood checkout's own CLI runs (functionality testing,
        // sims, ad-hoc dev) don't self-pollute its adoption metrics with `use`
        // events that read back as genuine consumer traffic. Safe to toggle
        // CODE_GRAPH_INTERNAL (no test SETS it to "1"; parallel removes are idempotent).
        std::env::remove_var("CODE_GRAPH_INTERNAL");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cg = root.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&cg).unwrap();
        let rec = cg.join("recommendations.jsonl");

        // No sentinel → the use event is recorded.
        record_cli_use(root, "grep");
        let after_first = std::fs::read_to_string(&rec).unwrap();
        assert_eq!(
            after_first.lines().count(),
            1,
            "use event recorded when no sentinel present"
        );

        // Sentinel present → record_cli_use is a no-op; the file is byte-unchanged.
        std::fs::write(cg.join(crate::domain::NO_METRICS_SENTINEL), b"").unwrap();
        record_cli_use(root, "callgraph");
        let after_second = std::fs::read_to_string(&rec).unwrap();
        assert_eq!(
            after_second, after_first,
            "sentinel must suppress the second use event"
        );
    }

    #[test]
    fn resolve_project_root_climbs_to_git_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let subdir = root.join("sub").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();
        assert_eq!(resolve_project_root_from(&subdir), root);
    }

    #[test]
    fn resolve_project_root_falls_back_to_cwd_when_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // canonicalize both sides: on macOS `/tmp` ↔ `/private/tmp` symlinking;
        // on Linux they match directly, so this is a no-op but keeps the test portable.
        assert_eq!(resolve_project_root_from(cwd), cwd);
    }

    #[test]
    fn is_non_project_cwd_bare_dir_is_non_project() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(is_non_project_cwd(tmp.path()));
    }

    #[test]
    fn is_non_project_cwd_each_marker_makes_it_a_project() {
        for marker in PROJECT_MARKERS {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join(marker), b"").unwrap();
            assert!(
                !is_non_project_cwd(tmp.path()),
                "{marker} should classify cwd as a project"
            );
        }
    }

    #[test]
    fn non_project_stub_answers_initialize_tools_list_and_rejects_rest() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"x"}}"#,
            "\n",
        );
        let mut out: Vec<u8> = Vec::new();
        serve_non_project_stub(std::io::Cursor::new(input), &mut out).unwrap();
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        // The notification (no `id`) produces no response → exactly 3 responses.
        assert_eq!(lines.len(), 3, "got: {lines:?}");

        let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            init["result"]["serverInfo"]["name"],
            "code-graph-mcp (non-project stub)"
        );

        let tl: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(tl["result"]["tools"], serde_json::json!([]));

        let call: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(call["error"]["code"], -32601);
    }

    #[test]
    fn cleanup_legacy_db_files_removes_empty_legacy_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Empty legacy files — should be removed
        std::fs::write(dir.join("code-graph.db"), b"").unwrap();
        std::fs::write(dir.join("code_graph.db"), b"").unwrap();
        std::fs::write(dir.join("graph.db"), b"").unwrap();
        // Non-empty legacy file — must NOT be removed (guard against deleting real data)
        std::fs::write(dir.join("index.db"), b"real data").unwrap();
        // Unrelated file — must NOT be touched
        std::fs::write(dir.join("usage.jsonl"), b"").unwrap();

        cleanup_legacy_db_files(dir);

        assert!(!dir.join("code-graph.db").exists());
        assert!(!dir.join("code_graph.db").exists());
        assert!(!dir.join("graph.db").exists());
        assert!(
            dir.join("index.db").exists(),
            "non-empty index.db must survive"
        );
        assert!(
            dir.join("usage.jsonl").exists(),
            "unrelated file must survive"
        );
    }

    #[test]
    fn cleanup_legacy_db_files_keeps_non_empty_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // If a legacy file has content, it might be a real backup — don't delete.
        std::fs::write(dir.join("graph.db"), b"some content").unwrap();
        cleanup_legacy_db_files(dir);
        assert!(dir.join("graph.db").exists());
    }

    #[test]
    fn resolve_project_root_prefers_cwd_index_over_git_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let subdir = root.join("sub");
        let sub_idx = subdir.join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&sub_idx).unwrap();
        std::fs::write(sub_idx.join("index.db"), b"").unwrap();
        assert_eq!(resolve_project_root_from(&subdir), subdir);
    }

    // ── Windows path spellings (issue #34) ────────────────────────────────
    // Platform-independent on purpose: the bug is pure string handling, so the
    // Linux/macOS CI legs catch a regression too, not just windows-latest.

    /// Everything `normalize_user_path_from` returns becomes an INDEX LOOKUP
    /// KEY, so it must be spelled exactly the way `merkle::normalize_rel_path`
    /// stores it. Asserted as a RELATION against the index normalizer rather
    /// than a literal, so the same assertion is meaningful on every platform.
    ///
    /// Before the fix, the absolute and root-relative branches returned
    /// `to_string_lossy()` verbatim, so on Windows `affected D:\repo\src\Foo.cs`
    /// produced the key `src\Foo.cs` against an index holding `src/Foo.cs` and
    /// the file was reported "not in index" — a present file, silently dropped.
    #[test]
    fn normalize_user_path_returns_index_key_spelling() {
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let native_rel: PathBuf = ["src", "parser", "mod.rs"].iter().collect();

        // (1) relative, spelled with the platform's own separator
        let typed = native_rel.to_string_lossy().into_owned();
        let got = normalize_user_path_from(root, root, &typed).unwrap();
        assert_eq!(
            got,
            crate::indexer::merkle::normalize_rel_path(&native_rel),
            "relative input must normalize to the index's stored spelling"
        );

        // (2) the same file given as an absolute path
        let abs = root.join(&native_rel);
        let got_abs = normalize_user_path_from(root, root, &abs.to_string_lossy()).unwrap();
        assert_eq!(
            got_abs, got,
            "absolute and relative spellings of one file must yield ONE key"
        );

        // (3) a backslash-typed relative path — what a Windows user pastes.
        // On Windows that is a two-component path and must become `src/a.rs`;
        // on Unix it is a single legal filename and must survive verbatim.
        let got_bs = normalize_user_path_from(root, root, r"src\a.rs").unwrap();
        assert_eq!(
            got_bs,
            crate::indexer::merkle::normalize_rel_str(r"src\a.rs")
        );
        if cfg!(windows) {
            assert!(
                !got_bs.contains('\\'),
                "no index key may carry a native separator on Windows"
            );
        }
    }

    /// The Windows legs of `normalize_user_path_from`, executed on every platform
    /// via the `backslash_is_sep` seam. Without the parameter these branches were
    /// reachable only on the windows-latest CI leg, which is exactly how the `.\`
    /// spelling stayed broken: PowerShell tab completion emits `.\src\foo.rs` by
    /// default, the `"./"` prefix test never matched it, and the function fell
    /// through to produce the key `./src/foo.rs` — a `./`-prefixed key the index
    /// never contains (same silent-miss shape as issue #34).
    #[test]
    fn normalize_user_path_handles_windows_dot_backslash_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // `.\src\foo.rs` must reduce to the bare key, not `./src/foo.rs`.
        let got = normalize_user_path_from_on(root, root, r".\src\foo.rs", true).unwrap();
        assert_eq!(got, "src/foo.rs", "`.\\` is the Windows spelling of `./`");

        // `.` and `.\` both mean "whole project".
        assert_eq!(
            normalize_user_path_from_on(root, root, ".", true).unwrap(),
            ""
        );
        assert_eq!(
            normalize_user_path_from_on(root, root, r".\", true).unwrap(),
            ""
        );

        // Plain backslash relative paths keep working.
        assert_eq!(
            normalize_user_path_from_on(root, root, r"src\foo.rs", true).unwrap(),
            "src/foo.rs"
        );

        // The escape check must still fire through the `.\` prefix — it did not
        // get looser by moving normalization ahead of it.
        assert!(normalize_user_path_from_on(root, root, r".\..\secret", true).is_err());
        assert!(normalize_user_path_from_on(root, root, r"..\secret", true).is_err());

        // Unix leg (`backslash_is_sep = false`): `\` is an ordinary filename
        // character and must survive verbatim — no over-normalization.
        assert_eq!(
            normalize_user_path_from_on(root, root, r"src\foo.rs", false).unwrap(),
            r"src\foo.rs"
        );
    }

    /// The `_on` seam parameterizes the SEPARATOR but not `Path::is_absolute`,
    /// which is irreducibly platform-native. On a Unix host `D:\repo\src\Foo.cs`
    /// and `C:/repo/src` are not absolute, so without a lexical guard they fall
    /// into the relative branch and come back out as `D:/repo/src/Foo.cs` — a
    /// key no index holds, answered as an ordinary empty result.
    ///
    /// The VERDICT is identical on every platform; the MECHANISM is not, and the
    /// first version of this guard got that wrong. Off-Windows the lexical
    /// spelling check rejects them. ON Windows `is_absolute` recognises the
    /// COMPLETE forms natively, `strip_prefix(root)` fails for a foreign drive,
    /// and the same error is raised — so the lexical check is not merely
    /// redundant for those, it is harmful: applied unconditionally it also
    /// rejected `C:\<temp>\src\parser\mod.rs` for a project root that literally
    /// contains it, reddening four tests on the windows-latest CI leg while every
    /// Linux run stayed green.
    ///
    /// The INCOMPLETE roots are the ones `is_absolute` misses on Windows too —
    /// bare `C:` is drive-relative and `\\server` has no share — so they are
    /// asserted unconditionally below. The `!cfg!(windows)` version of the guard
    /// handed exactly these two back to the relative branch.
    #[test]
    fn windows_absolute_spellings_are_rejected_by_spelling_off_windows() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for raw in [
            r"D:\repo\src\Foo.cs",
            "C:/repo/src/Foo.cs",
            r"\\server\share\src\Foo.cs",
            "c:/x",
            // Incomplete roots: rejected on EVERY platform, `is_absolute` claims
            // neither anywhere.
            "C:",
            "z:",
            r"\\server",
        ] {
            for backslash_is_sep in [true, false] {
                let got = normalize_user_path_from_on(root, root, raw, backslash_is_sep);
                assert!(
                    got.is_err(),
                    "{raw:?} (backslash_is_sep={backslash_is_sep}) must be rejected, not \
                     silently turned into an index key; got {got:?}"
                );
            }
        }

        // Near-misses that must NOT be swept up. The first three put a colon at
        // BYTE 1 — the exact position the predicate keys on — because that is
        // the boundary; the earlier version of this control never did, so it
        // passed while the guard rejected the real, indexed root-level file
        // `a:b.rs` with "outside the project root".
        //
        // The colon-bearing cases are Unix-only, and asserting them "on every
        // platform" is what reddened this test on the windows-latest CI leg. `:`
        // is legal in a POSIX filename, so `a:b.rs` there is an ordinary file
        // sitting in the project root and refusing it is a false answer. On
        // Windows it is neither: `a:` is a DRIVE-RELATIVE prefix, and NTFS
        // forbids `:` in a name outright (it introduces an alternate data
        // stream). Rejecting them there is correct, so the expectation, not the
        // behaviour, is what has to be platform-scoped.
        let colon_names: &[&str] = if cfg!(windows) {
            &[]
        } else {
            &["a:b.rs", "z:name", "a:b/c.rs", "src/a:b.rs", "a/b:c"]
        };
        for ok in [&["d/repo/src/Foo.cs"][..], colon_names].concat() {
            for backslash_is_sep in [true, false] {
                assert!(
                    normalize_user_path_from_on(root, root, ok, backslash_is_sep).is_ok(),
                    "{ok:?} (backslash_is_sep={backslash_is_sep}) is an ordinary \
                     relative path — `:` is legal in a Unix filename — and must survive"
                );
            }
        }
    }

    /// Repeated separators collapse, because no index key contains `//`.
    ///
    /// `dead-code src// --json` used to answer `[]` exit 0 on a directory with
    /// real dead code — the key matched nothing and the empty result was
    /// reported as clean. That is the same false-clean the unindexed-path probe
    /// exists to prevent: the probe trimmed a TRAILING slash for its own
    /// comparison while the query kept the untrimmed filter, so they disagreed
    /// and the disclosure never fired. `overview src//` errored on the same
    /// input, re-splitting two surfaces that had just been aligned.
    ///
    /// Known limitation, stated rather than hidden: the collapse runs on the
    /// OUTPUT (so the `\\`-prefixed UNC rejection still sees its prefix), which
    /// means an input whose DOUBLED separator changes how the input itself is
    /// parsed is unaffected — `.//src/foo.rs` still errors "escapes the project
    /// root", because stripping `./` leaves a leading `/`. Pre-existing, and not
    /// reachable from tab completion; `./src//foo.rs` is covered below.
    #[test]
    fn normalize_user_path_collapses_repeated_separators() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for (raw, want) in [
            ("src//foo.rs", "src/foo.rs"),
            ("src///foo.rs", "src/foo.rs"),
            ("src//", "src/"),
            ("./src//foo.rs", "src/foo.rs"),
            ("a//b//c.rs", "a/b/c.rs"),
            // Single separators are untouched.
            ("src/foo.rs", "src/foo.rs"),
            // A single trailing separator round-trips unchanged, pinned by
            // `test_normalize_user_path_passes_relative_through`; consumers
            // prefix-match, so `src/` works wherever `src` does.
            ("src/parser/", "src/parser/"),
        ] {
            for backslash_is_sep in [true, false] {
                let got = normalize_user_path_from_on(root, root, raw, backslash_is_sep).unwrap();
                assert_eq!(
                    got, want,
                    "{raw:?} (backslash_is_sep={backslash_is_sep}) must normalize to a key \
                     the index can actually contain"
                );
            }
        }

        // The collapse runs on the OUTPUT, so the UNC rejection still sees its
        // `\\` prefix. Collapsing the input first would turn `\\server\share`
        // into `\server\share` and walk past that guard entirely.
        assert!(
            normalize_user_path_from_on(root, root, r"\\server\share\x.rs", false).is_err(),
            "a UNC root must still be rejected after the collapse was added"
        );
    }

    /// The WINDOWS branch of that guard, executed on the Linux CI leg.
    ///
    /// `natively_absolute` is the only thing that differs between hosts here, so
    /// taking it as a parameter is what makes these cases observable at all —
    /// and both earlier versions of the predicate shipped a defect that ONLY
    /// windows-latest could see. The test above cannot reach either: it drives
    /// the real `Path::is_absolute`, which on Linux answers `false` for every
    /// Windows spelling, so the whole "Windows already claimed it" arm is dead
    /// there.
    #[test]
    fn lexical_windows_rejection_only_covers_what_is_absolute_did_not_claim() {
        // Simulated Windows, COMPLETE roots: `is_absolute` claims them, the
        // under-root check is the right answer, and firing here is what reddened
        // four tests on windows-latest by refusing `C:\<root>\src\mod.rs` for a
        // root that literally contains it.
        for complete in [r"C:\repo\src\mod.rs", "C:/repo/src", r"\\srv\share\x"] {
            assert!(
                !needs_lexical_windows_rejection(complete, true),
                "{complete:?} is natively absolute — the lexical check must stand down"
            );
        }
        // Simulated Windows, INCOMPLETE roots: `is_absolute` does NOT claim these
        // there either (bare `C:` is drive-relative, `\\server` has no share), so
        // the `!cfg!(windows)` version let them fall into the relative branch and
        // be answered as an ordinary empty result.
        for incomplete in ["C:", "z:", r"\\server"] {
            assert!(
                needs_lexical_windows_rejection(incomplete, false),
                "{incomplete:?} names no project-relative file on any host and must be rejected"
            );
        }
        // Simulated Unix: nothing here is absolute, so every Windows-shaped form
        // is rejected...
        for shaped in [
            r"D:\repo\src\Foo.cs",
            "C:/repo/src/Foo.cs",
            r"\\server\share\x",
            "c:",
        ] {
            assert!(
                needs_lexical_windows_rejection(shaped, false),
                "{shaped:?} must be rejected lexically where `is_absolute` misses it"
            );
        }
        // ...while ordinary names whose second byte is `:` are not.
        for ok in ["a:b.rs", "z:name", "src/a:b.rs", "d/repo/src/Foo.cs", "1:x"] {
            assert!(
                !needs_lexical_windows_rejection(ok, false),
                "{ok:?} is an ordinary relative path — `:` is legal in a POSIX filename"
            );
        }
    }

    /// `had_literal_separator` must describe the PATTERN's slot, not the first
    /// token that happens to spell the same string.
    ///
    /// `position(|a| *a == pattern)` scanned from index 0, so an earlier flag's
    /// VALUE claimed the slot: `grep -t rust -- rust` computed pat=1 against
    /// sep=2 and concluded "no separator" for a command that plainly had one.
    /// Downstream that makes `grep_flaglike_pattern_hint` advise a user who
    /// already typed `--` to type it. The `sep == pat` case (`grep -- --`) was
    /// wrong for the same reason, but is unreachable: clap consumes `--` itself,
    /// so `grep -- --` never yields a `--` pattern (it exits 2 for a missing
    /// PATTERN). Only the value-collision case above is a real command, and it
    /// is the one asserted here.
    #[test]
    fn grep_literal_separator_tracks_the_pattern_slot_not_the_first_match() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // The pattern string also appears as `-t`'s value, before the separator.
        let a = parse_grep_args(&argv(&[
            "code-graph-mcp",
            "grep",
            "-t",
            "rust",
            "--",
            "rust",
        ]));
        assert_eq!(a.pattern, "rust");
        assert!(
            a.had_literal_separator,
            "the pattern's own slot is after `--`; an earlier flag VALUE spelling \
             the same string must not claim it"
        );

        // Ordinary separated use, and unseparated use, both unchanged.
        assert!(
            parse_grep_args(&argv(&["code-graph-mcp", "grep", "--", "-foo"])).had_literal_separator
        );
        assert!(
            !parse_grep_args(&argv(&["code-graph-mcp", "grep", "needle"])).had_literal_separator
        );
        assert!(
            !parse_grep_args(&argv(&["code-graph-mcp", "grep", "-t", "rust", "rust"]))
                .had_literal_separator
        );
    }

    #[test]
    fn is_cwd_anchored_recognizes_both_separator_spellings() {
        // The "cwd-anchored paths never rebase to the project root" promise was
        // Unix-only; on Windows `.\x` fell through and could silently rebase.
        for anchored in [".", "./x", "../x", r".\x", r"..\x"] {
            assert!(is_cwd_anchored(anchored), "{anchored} is cwd-anchored");
        }
        for plain in ["src/x", r"src\x", ".hidden", "x"] {
            assert!(!is_cwd_anchored(plain), "{plain} is not cwd-anchored");
        }
    }

    #[test]
    fn normalize_path_display_strips_windows_prefixes_and_separators() {
        assert_eq!(
            normalize_path_display_on(r"\\?\D:\code\repo\src\main.rs", true),
            "D:/code/repo/src/main.rs",
            r"the \\?\ extended prefix must never reach stdout"
        );
        assert_eq!(
            normalize_path_display_on(r"\\?\UNC\server\share\repo\a.rs", true),
            "//server/share/repo/a.rs",
            "UNC form keeps the double-slash host root"
        );
        assert_eq!(
            normalize_path_display_on("src/main.rs", true),
            "src/main.rs"
        );
        assert_eq!(
            normalize_path_display_on(r"src\main.rs", true),
            "src/main.rs"
        );
    }

    /// On Unix `\` is a legal FILENAME character (only `/` and NUL are illegal),
    /// so normalizing separators there would rename a real file and, worse, build
    /// a lookup key that misses the indexed path — `merkle::normalize_rel_path`
    /// also rewrites only under `#[cfg(windows)]`. Same failure mode as #34, just
    /// in the other direction.
    #[test]
    fn normalize_path_display_leaves_unix_backslash_filenames_alone() {
        assert_eq!(
            normalize_path_display_on(r"src/od\bc.rs", false),
            r"src/od\bc.rs",
            r"a Unix file literally named `od\bc.rs` must survive verbatim"
        );
        assert_eq!(
            relativize_path_on(r"/home/u/repo/src/od\bc.rs", "/home/u/repo", false),
            r"src/od\bc.rs",
            "…including through relativization, which feeds the AST lookup key"
        );
        // The same input on Windows IS a two-segment path — the flag is the whole
        // difference, which is why it must not be inferred from the string.
        assert_eq!(
            normalize_path_display_on(r"src/od\bc.rs", true),
            "src/od/bc.rs"
        );
    }

    /// The three spellings one file arrives in on Windows — canonicalized walk
    /// output, `project_root.join(rel)` mixed separators, and a bare relative
    /// supplement operand — must all reduce to ONE key. When they did not, the
    /// same match printed once per spelling and the AST lookup (indexed paths are
    /// `/`-relative) missed every file.
    #[test]
    fn relativize_path_collapses_windows_spellings_to_one_key() {
        let root = r"D:\code\repo";
        let expected = "src/Web/Endpoints.cs";
        for spelling in [
            r"\\?\D:\code\repo\src\Web\Endpoints.cs",
            r"D:\code\repo\src\Web\Endpoints.cs",
            r"D:\code\repo\src/Web/Endpoints.cs",
            "src/Web/Endpoints.cs",
        ] {
            assert_eq!(
                relativize_path_on(spelling, root, true),
                expected,
                "spelling {:?} must relativize to the indexed form",
                spelling
            );
        }
    }

    #[test]
    fn relativize_path_handles_posix_and_dot_walk_output() {
        assert_eq!(
            relativize_path("/home/u/repo/src/main.rs", "/home/u/repo"),
            "src/main.rs"
        );
        assert_eq!(
            relativize_path("/home/u/repo/src/main.rs", "/home/u/repo/"),
            "src/main.rs",
            "a trailing slash on the root must not leave a leading slash"
        );
        assert_eq!(
            relativize_path("./src/main.rs", "/home/u/repo"),
            "src/main.rs"
        );
        // Out-of-root paths can't occur (the starts_with(root) guard rejects
        // them upstream); pinning the pre-existing shape so the normalization
        // rewrite is behaviour-preserving here.
        assert_eq!(
            relativize_path("/elsewhere/x.rs", "/home/u/repo"),
            "elsewhere/x.rs"
        );
    }

    #[test]
    fn relativize_path_is_case_insensitive_on_windows_drive() {
        assert_eq!(
            relativize_path_on(r"d:\code\repo\src\a.rs", r"D:\code\repo", true),
            "src/a.rs",
            "the same volume may be spelled with either drive-letter case"
        );
        // Unix filesystems ARE case-sensitive: two differently-cased roots are
        // two different directories and must not be collapsed.
        assert_eq!(
            relativize_path_on("/home/U/repo/src/a.rs", "/home/u/repo", false),
            "/home/U/repo/src/a.rs".trim_start_matches('/'),
            "case-insensitive matching must stay Windows-only"
        );
    }

    #[test]
    fn test_normalize_type_filter() {
        assert_eq!(normalize_type_filter("fn"), vec!["function", "method"]);
        assert_eq!(normalize_type_filter("class"), vec!["class"]);
        assert_eq!(normalize_type_filter("trait"), vec!["interface", "trait"]);
        assert!(normalize_type_filter("unknown").is_empty());
    }

    #[test]
    fn test_format_node_compact() {
        let node = queries::NodeResult {
            id: 1,
            file_id: 1,
            node_type: "function".into(),
            name: "foo".into(),
            qualified_name: Some("MyClass::foo".into()),
            start_line: 10,
            end_line: 20,
            code_content: String::new(),
            signature: None,
            doc_comment: None,
            context_string: None,
            name_tokens: None,
            return_type: Some("Result<Value>".into()),
            // Mirror how the parser stores it: ALREADY parenthesized. The old fixture
            // used a bare "name: &str, value: i64" (no parens), which never exercised
            // the real shape and hid the "((...))" double-wrap bug.
            param_types: Some("(name: &str, value: i64)".into()),
            is_test: false,
        };
        let formatted = format_node_compact(&node, "src/lib.rs");
        assert!(formatted.contains("fn MyClass::foo"));
        assert!(formatted.contains("src/lib.rs:10-20"));
        assert!(formatted.contains("(name: &str, value: i64)"));
        assert!(formatted.contains("-> Result<Value>"));
        // Guard the fix: param_types already carries its parens, so the formatter must
        // not add a second pair.
        assert!(
            !formatted.contains("(("),
            "must not double-wrap params: {formatted}"
        );
    }

    #[test]
    fn test_parse_rg_json_empty() {
        let root = Path::new("/project");
        assert!(parse_rg_json(b"", root, root).is_empty());
    }

    #[test]
    fn test_parse_rg_json_match() {
        let root = Path::new("/project");
        let json_line = br#"{"type":"match","data":{"path":{"text":"/project/src/main.rs"},"line_number":42,"lines":{"text":"fn main() {\n"}}}"#;
        let matches = parse_rg_json(json_line, root, root);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].file, "src/main.rs");
        assert_eq!(matches[0].line, 42);
    }

    #[test]
    fn test_aggregate_usage_empty() {
        let s = aggregate_usage_jsonl("", None);
        assert_eq!(s.sessions, 0);
        assert_eq!(s.parse_errors, 0);
        assert!(s.tools.is_empty());
        assert_eq!(s.total_tool_calls(), 0);
    }

    #[test]
    fn test_aggregate_usage_skips_malformed_and_blank() {
        let content =
            "\n\nnot-json\n{\"ts\":\"2026-04-20T00:00:00Z\",\"v\":\"0.12.1\",\"tools\":{}}\n";
        let s = aggregate_usage_jsonl(content, None);
        assert_eq!(s.sessions, 1);
        assert_eq!(s.parse_errors, 1);
    }

    #[test]
    fn test_aggregate_usage_merges_err_kinds_and_tracks_unrecorded_gap() {
        // Shapes mirror metrics.rs `build_record`: err_kinds is an object nested
        // in the tool entry, emitted only when non-empty. Session 3 predates the
        // field (err but no err_kinds) so it must land in the `err`-vs-classified
        // gap, not vanish.
        let s1 = r#"{"ts":"2026-07-01T00:00:00Z","v":"0.90.0","tools":{"get_call_graph":{"n":5,"ms":500,"err":2,"max_ms":200,"err_kinds":{"other":1,"not_found":1}}}}"#;
        let s2 = r#"{"ts":"2026-07-02T00:00:00Z","v":"0.90.0","tools":{"get_call_graph":{"n":3,"ms":300,"err":2,"max_ms":150,"err_kinds":{"other":2}}}}"#;
        let s3 = r#"{"ts":"2026-06-01T00:00:00Z","v":"0.60.0","tools":{"get_call_graph":{"n":1,"ms":90,"err":1,"max_ms":90}}}"#;
        let content = format!("{s1}\n{s2}\n{s3}\n");
        let s = aggregate_usage_jsonl(&content, None);
        let cg = s
            .tools
            .get("get_call_graph")
            .expect("get_call_graph aggregated");
        assert_eq!(cg.n, 9);
        assert_eq!(
            cg.err, 5,
            "err sums across all sessions incl. the pre-err_kinds one"
        );
        assert_eq!(cg.err_kinds.get("other").copied(), Some(3));
        assert_eq!(cg.err_kinds.get("not_found").copied(), Some(1));
        let classified: u64 = cg.err_kinds.values().sum();
        assert_eq!(classified, 4);
        assert_eq!(
            cg.err - classified,
            1,
            "the pre-feature session is the unrecorded remainder"
        );
    }

    #[test]
    fn test_aggregate_usage_merges_tool_counts_across_sessions() {
        let line1 = r#"{"ts":"2026-04-19T10:00:00Z","v":"0.12.0","tools":{"get_call_graph":{"n":2,"ms":200,"err":0,"max_ms":150},"project_map":{"n":1,"ms":1000,"err":0,"max_ms":1000}}}"#;
        let line2 = r#"{"ts":"2026-04-20T10:00:00Z","v":"0.12.1","tools":{"get_call_graph":{"n":3,"ms":900,"err":1,"max_ms":500}}}"#;
        let content = format!("{}\n{}\n", line1, line2);
        let s = aggregate_usage_jsonl(&content, None);
        assert_eq!(s.sessions, 2);
        assert_eq!(s.total_tool_calls(), 6);

        let cg = s.tools.get("get_call_graph").unwrap();
        assert_eq!(cg.n, 5);
        assert_eq!(cg.total_ms, 1100);
        assert_eq!(cg.err, 1);
        assert_eq!(cg.max_ms, 500); // max across sessions

        let pm = s.tools.get("project_map").unwrap();
        assert_eq!(pm.n, 1);
        assert_eq!(pm.max_ms, 1000);

        assert_eq!(s.versions.len(), 2);
        assert!(s.versions.contains("0.12.0") && s.versions.contains("0.12.1"));
        assert_eq!(s.first_ts.as_deref(), Some("2026-04-19T10:00:00Z"));
        assert_eq!(s.last_ts.as_deref(), Some("2026-04-20T10:00:00Z"));
    }

    #[test]
    fn test_aggregate_funnel_deny_and_hint_to_use() {
        // s1: deny + called cg (converted). s2: deny + NO cg (not converted).
        // s3: hint + called cg. s4: no recs (ignored by funnel). s5: deny but only
        // a housekeeping tool (get_index_status) → NOT counted as cg use.
        let s1 = r#"{"ts":"2026-06-10T10:00:00Z","v":"0.45.4","tools":{"get_call_graph":{"n":1,"ms":5,"err":0,"max_ms":5}},"recs":{"deny":2,"hint":0}}"#;
        let s2 =
            r#"{"ts":"2026-06-10T11:00:00Z","v":"0.45.4","tools":{},"recs":{"deny":1,"hint":1}}"#;
        let s3 = r#"{"ts":"2026-06-10T12:00:00Z","v":"0.45.4","tools":{"find_references":{"n":3,"ms":9,"err":0,"max_ms":4}},"recs":{"deny":0,"hint":1}}"#;
        let s4 = r#"{"ts":"2026-06-10T13:00:00Z","v":"0.45.4","tools":{"get_call_graph":{"n":1,"ms":5,"err":0,"max_ms":5}}}"#;
        let s5 = r#"{"ts":"2026-06-10T14:00:00Z","v":"0.45.4","tools":{"get_index_status":{"n":1,"ms":0,"err":0,"max_ms":0}},"recs":{"deny":1,"hint":0}}"#;
        let content = format!("{s1}\n{s2}\n{s3}\n{s4}\n{s5}\n");
        let s = aggregate_usage_jsonl(&content, None);
        // deny sessions: s1, s2, s5 = 3; of those, only s1 called a cg query tool.
        assert_eq!(s.sessions_with_deny, 3, "s1+s2+s5 saw a deny");
        assert_eq!(
            s.sessions_with_deny_and_cg, 1,
            "only s1 called a cg query tool (s5's get_index_status is housekeeping)"
        );
        // hint sessions: s2, s3 = 2; of those, only s3 called cg.
        assert_eq!(s.sessions_with_hint, 2);
        assert_eq!(s.sessions_with_hint_and_cg, 1);
    }

    #[test]
    fn test_version_sort_key_is_numeric_not_lexical() {
        // Regression: the stats `versions:` list is stored in a BTreeSet (lexical),
        // so "0.5.40" sorted AFTER "0.32.2". version_sort_key must order by numeric
        // (major, minor, patch) so the displayed list reads in true version order.
        let mut vs = vec!["0.32.2", "0.5.40", "0.11.0", "0.9.0", "0.5.43", "0.7.1"];
        vs.sort_by_key(|v| version_sort_key(v));
        assert_eq!(
            vs,
            vec!["0.5.40", "0.5.43", "0.7.1", "0.9.0", "0.11.0", "0.32.2"]
        );
        // Lexical sort would have put "0.11.0"/"0.32.2" before "0.5.40" — guard that.
        assert!(
            vs.iter().position(|v| *v == "0.5.40").unwrap()
                < vs.iter().position(|v| *v == "0.11.0").unwrap(),
            "0.5.40 must sort before 0.11.0 (numeric), not after (lexical)"
        );
        // Odd/suffixed components fall back to 0 without panicking.
        assert_eq!(version_sort_key("0.5.40-rc1"), (0, 5, 40));
        assert_eq!(version_sort_key("weird"), (0, 0, 0));
        assert_eq!(version_sort_key("1.2"), (1, 2, 0));
    }

    #[test]
    fn test_aggregate_usage_last_n_keeps_tail() {
        let lines: Vec<String> = (0..5).map(|i|
            format!(r#"{{"ts":"2026-04-2{}T00:00:00Z","v":"0.12.1","tools":{{"t":{{"n":1,"ms":{},"err":0,"max_ms":{}}}}}}}"#, i, (i + 1) * 10, (i + 1) * 10)
        ).collect();
        let content = lines.join("\n");
        let s = aggregate_usage_jsonl(&content, Some(2));
        assert_eq!(s.sessions, 2);
        let t = s.tools.get("t").unwrap();
        // Last 2 sessions: ms 40 + 50 = 90
        assert_eq!(t.total_ms, 90);
        assert_eq!(t.max_ms, 50);
    }

    #[test]
    fn test_aggregate_recommendations_counts_by_action_and_hook() {
        let content = [
            r#"{"ts":"t1","hook":"grep","action":"deny"}"#,
            r#"{"ts":"t2","hook":"grep","action":"hint"}"#,
            r#"  "#,         // blank → skipped
            r#"{not json}"#, // malformed → skipped, not counted
            r#"{"ts":"t3","hook":"read","action":"hint"}"#,
        ]
        .join("\n");
        let s = aggregate_recommendations_jsonl(&content);
        assert_eq!(s.total, 3, "only 3 well-formed lines counted");
        assert_eq!(s.by_action.get("hint").copied(), Some(2));
        assert_eq!(s.by_action.get("deny").copied(), Some(1));
        assert_eq!(s.by_hook.get("grep").copied(), Some(2));
        assert_eq!(s.by_hook.get("read").copied(), Some(1));
    }

    #[test]
    fn test_aggregate_recommendations_cli_uses_and_answered_split() {
        let content = [
            // answered deny (v0.47+) vs static deny (no field = pre-v0.47 or fallback)
            r#"{"ts":"t1","hook":"grep","action":"deny","answered":true}"#,
            r#"{"ts":"t2","hook":"grep","action":"deny","answered":false}"#,
            r#"{"ts":"t3","hook":"grep","action":"deny"}"#,
            r#"{"ts":"t4","hook":"grep","action":"bypass"}"#,
            // CLI conversions: counted in cli_uses, NOT in total/by_action/by_hook
            r#"{"ts":"t5","hook":"cli","action":"use","cmd":"callgraph"}"#,
            r#"{"ts":"t6","hook":"cli","action":"use","cmd":"grep"}"#,
        ]
        .join("\n");
        let s = aggregate_recommendations_jsonl(&content);
        assert_eq!(s.total, 4, "use lines are conversions, not recommendations");
        assert_eq!(s.cli_uses, 2);
        assert_eq!(s.deny_answered, 1);
        assert_eq!(
            s.deny_unanswered, 2,
            "answered:false and missing field are both static"
        );
        assert_eq!(s.by_action.get("bypass").copied(), Some(1));
        assert!(
            !s.by_hook.contains_key("cli"),
            "cli use lines stay out of by_hook"
        );
    }

    #[test]
    fn test_aggregate_recommendations_empty() {
        let s = aggregate_recommendations_jsonl("");
        assert_eq!(s.total, 0);
        assert!(s.by_action.is_empty());
        assert!(s.by_hook.is_empty());
    }

    #[test]
    fn test_aggregate_usage_search_and_index_merged() {
        let l1 = r#"{"ts":"t1","v":"0.12.1","tools":{"t":{"n":1,"ms":1,"err":0,"max_ms":1}},"search":{"queries":10,"zero":2,"avg_quality":0.8,"fts_only":3,"hybrid":7},"index":{"full_ms":2000,"incr":5,"files":50,"nodes":100}}"#;
        let l2 = r#"{"ts":"t2","v":"0.12.1","tools":{"t":{"n":1,"ms":1,"err":0,"max_ms":1}},"search":{"queries":5,"zero":0,"avg_quality":0.6,"fts_only":1,"hybrid":4},"index":{"full_ms":null,"incr":3,"files":10,"nodes":20}}"#;
        let s = aggregate_usage_jsonl(&format!("{}\n{}", l1, l2), None);
        assert_eq!(s.search_queries, 15);
        assert_eq!(s.search_zero, 2);
        assert_eq!(s.search_fts_only, 4);
        assert_eq!(s.search_hybrid, 11);
        // Weighted quality: (0.8 * 10 + 0.6 * 5) / 15 = 11.0 / 15 ≈ 0.7333
        let weighted_avg = s.search_quality_weighted_sum / s.search_queries as f64;
        assert!((weighted_avg - 0.7333).abs() < 0.01, "got {}", weighted_avg);
        assert_eq!(s.full_index_count, 1);
        assert_eq!(s.full_index_ms_sum, 2000);
        assert_eq!(s.incr_count, 8);
        assert_eq!(s.files_indexed, 60);
    }

    // --- normalize_user_path ---
    // Indexed file_path columns are project-relative; users who paste absolute
    // paths from an IDE used to get silent "no results" across overview/deps/dead-code.

    #[test]
    fn test_normalize_user_path_dot_means_whole_project() {
        // From the project root, `.` is the whole project (empty prefix).
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_user_path_from(tmp.path(), tmp.path(), ".").unwrap(),
            ""
        );
    }

    #[test]
    fn test_normalize_user_path_strips_dot_slash() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            normalize_user_path_from(tmp.path(), tmp.path(), "./src/parser").unwrap(),
            "src/parser"
        );
    }

    #[test]
    fn test_normalize_user_path_passes_relative_through() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(
            normalize_user_path_from(root, root, "src/parser").unwrap(),
            "src/parser"
        );
        assert_eq!(
            normalize_user_path_from(root, root, "src/parser/").unwrap(),
            "src/parser/"
        );
    }

    #[test]
    fn test_normalize_user_path_relative_resolves_against_subdir_cwd() {
        // Running from a subdirectory, a relative path resolves against the cwd
        // (like grep/ls), then maps to a root-relative key. This is the
        // subdir-cwd fix — from `src/`, `main.rs` means `src/main.rs`.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("src");
        let deep = root.join("src/parser");
        // Plain name + `./name` from src/ → src/name.
        assert_eq!(
            normalize_user_path_from(root, &sub, "main.rs").unwrap(),
            "src/main.rs"
        );
        assert_eq!(
            normalize_user_path_from(root, &sub, "./parser").unwrap(),
            "src/parser"
        );
        assert_eq!(
            normalize_user_path_from(root, &sub, "parser/mod.rs").unwrap(),
            "src/parser/mod.rs"
        );
        // `.` from a subdir is that subdir, NOT the whole repo (the bug fixed).
        assert_eq!(normalize_user_path_from(root, &sub, ".").unwrap(), "src");
        assert_eq!(
            normalize_user_path_from(root, &deep, ".").unwrap(),
            "src/parser"
        );
        // `../` climbs back toward the root.
        assert_eq!(
            normalize_user_path_from(root, &deep, "../mod.rs").unwrap(),
            "src/mod.rs"
        );
        assert_eq!(
            normalize_user_path_from(root, &sub, "../Cargo.toml").unwrap(),
            "Cargo.toml"
        );
        // An absolute path is still root-relative regardless of cwd.
        let abs = root.join("lib.rs");
        assert_eq!(
            normalize_user_path_from(root, &sub, abs.to_str().unwrap()).unwrap(),
            "lib.rs"
        );
    }

    #[test]
    fn test_normalize_user_path_root_relative_from_subdir_rebases() {
        // Field failure 2026-07-24 (same class as cmd_grep's): the agent's shell
        // sits in a subdir while it quotes repo-root-relative paths (hook answers
        // display them root-relative), so the cwd-relative reading doubles the
        // prefix into a path that exists nowhere. cwd-missing + root-existing is
        // unambiguous — take the root reading.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sub = root.join("src");
        std::fs::create_dir_all(sub.join("parser")).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        // File: `src/main.rs` from `src/` — src/src/main.rs missing, src/main.rs exists.
        assert_eq!(
            normalize_user_path_from(root, &sub, "src/main.rs").unwrap(),
            "src/main.rs"
        );
        // Directory: `src` from `src/` — the shape of the observed overview failure.
        assert_eq!(normalize_user_path_from(root, &sub, "src").unwrap(), "src");
        // Parity guard: when the cwd-relative reading exists, it wins — no rebase.
        assert_eq!(
            normalize_user_path_from(root, &sub, "parser").unwrap(),
            "src/parser"
        );
        // Neither exists: historical cwd-relative reading stands (the target may
        // be indexed but deleted/gitignored — do not guess).
        assert_eq!(
            normalize_user_path_from(root, &sub, "ghost.rs").unwrap(),
            "src/ghost.rs"
        );
    }

    #[test]
    fn test_normalize_user_path_rebase_same_name_collision_documented_tradeoff() {
        // Audit 2026-07-24 P2: the rebase heuristic decides by filesystem
        // existence, so when the cwd-relative target was JUST deleted while an
        // unrelated same-named file exists at the root, it picks the root file.
        // Disk state alone cannot distinguish this from the doubling case the
        // heuristic exists for; the stderr note is the disclosure. This test
        // PINS that tradeoff — if it starts failing because the heuristic got
        // smarter (e.g. consults the index), update it deliberately.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // Root-level `utils.rs` exists; `src/utils.rs` (what a user in src/
        // plausibly means) does not — deleted, never existed, or gitignored.
        std::fs::write(root.join("utils.rs"), "").unwrap();
        assert_eq!(
            normalize_user_path_from(root, &root.join("src"), "utils.rs").unwrap(),
            "utils.rs"
        );
    }

    #[test]
    fn test_normalize_user_path_subdir_cwd_rejects_climb_above_root() {
        // From a subdir, enough `../` to climb above the project root is an escape.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let deep = root.join("src/parser");
        // src/parser is depth 2, so escaping the root needs 3+ `../`.
        for escape in ["../../../etc/passwd", "../../../../secret.js"] {
            let err = normalize_user_path_from(root, &deep, escape)
                .expect_err(&format!("{escape:?} from src/parser must escape"));
            assert!(
                format!("{err}").contains("escapes the project root"),
                "got: {err}"
            );
        }
        // Exactly enough `../` to reach the root (not past) is still in-root.
        assert_eq!(
            normalize_user_path_from(root, &deep, "../../lib.rs").unwrap(),
            "lib.rs"
        );
    }

    #[test]
    fn test_normalize_user_path_absolute_under_root_lexical() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let abs = root.join("src/parser");
        assert_eq!(
            normalize_user_path(root, abs.to_str().unwrap()).unwrap(),
            "src/parser"
        );
    }

    #[test]
    fn test_normalize_user_path_rejects_absolute_prefix_climb() {
        // v0.79.1 audit (#5): an ABSOLUTE path that begins with the root then
        // climbs out via `..` (`<root>/../../etc/passwd`) must error. `strip_prefix`
        // matches components and does NOT collapse `..`, so it returned the
        // escaping remainder unchecked — the absolute-prefix sibling of the
        // relative `..` escape. Also covers the `./`-prefix shortcut (`./../x`).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let root_str = root.to_str().unwrap();
        let climbs = [
            format!("{root_str}/../../etc/passwd"),
            format!("{root_str}/../sibling/secret.js"),
            "./../secret".to_string(),
            "./../../etc/passwd".to_string(),
        ];
        for c in &climbs {
            let err = normalize_user_path(root, c)
                .expect_err(&format!("{c:?} must be rejected as an escape"));
            assert!(
                format!("{err}").contains("escapes the project root"),
                "{c:?} should be rejected as an escape; got: {err}"
            );
        }
        // Absolute path under root with interior `..` that stays in-root is allowed.
        let inroot = format!("{root_str}/src/../lib.rs");
        assert_eq!(normalize_user_path(root, &inroot).unwrap(), "src/../lib.rs");
    }

    #[test]
    fn test_db_sidecar_appends_suffix_to_full_filename() {
        // SQLite names the WAL `<dbfile>-wal` — a literal suffix, NOT an extension
        // swap. For `index.db` both happen to agree, but for the rebuild temp
        // `index.db.rebuild-<pid>` only the literal append is correct.
        let canonical = std::path::Path::new("/p/.code-graph/index.db");
        assert_eq!(
            db_sidecar(canonical, "-wal"),
            std::path::PathBuf::from("/p/.code-graph/index.db-wal")
        );
        assert_eq!(
            db_sidecar(canonical, "-shm"),
            std::path::PathBuf::from("/p/.code-graph/index.db-shm")
        );
        let temp = std::path::Path::new("/p/.code-graph/index.db.rebuild-1234");
        assert_eq!(
            db_sidecar(temp, "-wal"),
            std::path::PathBuf::from("/p/.code-graph/index.db.rebuild-1234-wal"),
            "WAL of a multi-dot temp db must append -wal, not swap the extension"
        );
    }

    #[test]
    fn test_normalize_user_path_rejects_relative_dotdot_escape() {
        // A relative path climbing above the root must error, not pass through:
        // the index holds only in-root paths, so an escaping path can only match
        // the disk. `deps`' barrel-scan reads `project_root.join(raw)`, so this is
        // a path-traversal file read (leaks import/re-export lines), not just a
        // wrong query.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for escape in ["../secret.js", "../../etc/passwd", "a/../../b", ".."] {
            let err = normalize_user_path(root, escape).unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains("escapes the project root"),
                "{escape:?} should be rejected as an escape; got: {msg}"
            );
        }
        // Non-escaping `..` (stays at or below the root) is still allowed through.
        assert_eq!(normalize_user_path(root, "a/../b").unwrap(), "a/../b");
        assert_eq!(
            normalize_user_path(root, "src/sub/../mod.rs").unwrap(),
            "src/sub/../mod.rs"
        );
    }

    #[test]
    fn test_normalize_user_path_absolute_outside_root_errors() {
        let tmp_root = tempfile::tempdir().unwrap();
        let tmp_other = tempfile::tempdir().unwrap();
        let abs_outside = tmp_other.path().join("foo.rs");
        let err = normalize_user_path(tmp_root.path(), abs_outside.to_str().unwrap()).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("outside the project root"), "got: {}", msg);
    }

    #[test]
    fn test_relativize_path_windows_verbatim_and_case() {
        // cmd_grep hands rg CANONICALIZED search paths; on Windows canonicalize
        // yields the \\?\ verbatim long form while the raw root may be plain
        // (or vice versa when rg echoes verbatim), and drive letters vary in
        // case. The lexical relativize must equate every combination — the
        // remaining 8.3-short-name vs long-name pairing is filesystem-only and
        // is covered by cmd_grep relativizing against root_canonical (the same
        // canonical spelling the rg args were built from), exercised by the
        // grep e2e suite on the windows CI leg (first lit in v0.112.0).
        assert_eq!(
            relativize_path_on(
                r"C:\Users\dev\proj\src\a.rs",
                r"\\?\C:\Users\dev\proj",
                true
            ),
            "src/a.rs"
        );
        assert_eq!(
            relativize_path_on(
                r"\\?\C:\Users\dev\proj\src\a.rs",
                r"C:\Users\dev\proj",
                true
            ),
            "src/a.rs"
        );
        assert_eq!(
            relativize_path_on(r"c:\proj\x.rs", r"C:\proj", true),
            "x.rs"
        );
    }

    #[test]
    fn test_relativize_path_dual_falls_back_to_raw_root() {
        // A nonexistent path never canonicalizes, so its rg echo keeps the RAW
        // (possibly 8.3-short) root spelling; the canonical long form misses
        // lexically and the raw fallback must strip it instead. Unmatched by
        // both roots → normalized passthrough.
        assert_eq!(
            relativize_path_dual("/repo/src/a.rs", "/repo", "/elsewhere"),
            "src/a.rs"
        );
        assert_eq!(
            relativize_path_dual("/short/form/src/a.rs", "/canonical/long", "/short/form"),
            "src/a.rs",
            "canonical miss must fall back to the raw root"
        );
        assert_eq!(
            relativize_path_dual("/outside/a.rs", "/repo", "/elsewhere"),
            // Unmatched passthrough keeps relativize_path's historical shape:
            // normalized with the leading separator trimmed.
            "outside/a.rs"
        );
    }

    #[test]
    fn test_normalize_user_path_absolute_under_root_canonicalize_via_symlink() {
        // Symlink case: lexical strip fails but canonicalize succeeds.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/parser")).unwrap();
        let link_root = tmp
            .path()
            .parent()
            .unwrap()
            .join(format!("cg-norm-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link_root);
        #[cfg(unix)]
        std::os::unix::fs::symlink(root, &link_root).unwrap();
        #[cfg(unix)]
        {
            let abs_via_link = link_root.join("src/parser");
            let res = normalize_user_path(root, abs_via_link.to_str().unwrap()).unwrap();
            assert_eq!(res, "src/parser");
            let _ = std::fs::remove_file(&link_root);
        }
    }

    #[test]
    fn test_normalize_grep_argv_attached_context() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Attached numeric context forms split into flag + value.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-A2", "pat"])),
            s(&["grep", "-A", "2", "pat"])
        );
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-B1", "pat"])),
            s(&["grep", "-B", "1", "pat"])
        );
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-C10", "pat"])),
            s(&["grep", "-C", "10", "pat"])
        );
        // Bundled boolean short(s) + trailing attached context: peel the digits,
        // keep the cluster so clap parses `-nA 2` as `-n -A=2`.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-nA2", "pat"])),
            s(&["grep", "-nA", "2", "pat"])
        );
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-niB3", "pat"])),
            s(&["grep", "-niB", "3", "pat"])
        );
        // Value flag not last in the bundle (`-A2B3`) → digit in the middle → left alone.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-A2B3"])),
            s(&["grep", "-A2B3"])
        );
        // Bare `-A` (clap takes the next token as its value) is untouched.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-A", "2", "pat"])),
            s(&["grep", "-A", "2", "pat"])
        );
        // Non-context single-dash flags and `--long` patterns are untouched.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-n", "pat"])),
            s(&["grep", "-n", "pat"])
        );
        assert_eq!(
            normalize_grep_argv(s(&["grep", "--no-default-features"])),
            s(&["grep", "--no-default-features"])
        );
        // `-m` is the `--max-count` short alias: attached `-m2` splits like `-A2`
        // (the same allow_hyphen_values quirk forces the peel — see the fn doc).
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-m2", "pat"])),
            s(&["grep", "-m", "2", "pat"])
        );
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-nm2", "pat"])),
            s(&["grep", "-nm", "2", "pat"])
        );
        // `-M` (`--max-columns`) is also a numeric value short → attached splits.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-M512", "pat"])),
            s(&["grep", "-M", "512", "pat"])
        );
        // Digit-suffix on an unsupported short (`-z2`) is left alone.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-z2", "pat"])),
            s(&["grep", "-z2", "pat"])
        );
        // Non-digit tail (`-A2x`) is not a valid attached form → left alone.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "-A2x"])),
            s(&["grep", "-A2x"])
        );
        // `--` stops normalization so a literal `-A2` pattern survives.
        assert_eq!(
            normalize_grep_argv(s(&["grep", "--", "-A2"])),
            s(&["grep", "--", "-A2"])
        );
    }

    #[test]
    fn test_first_unsupported_grep_flag() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Common grep flags we don't implement are flagged (would otherwise be
        // swallowed as the pattern → cryptic "No such file").
        for bad in ["-v", "-o", "-e", "-P", "-x", "-nv"] {
            assert_eq!(
                first_unsupported_grep_flag(&s(&["grep", bad, "pat"])).as_deref(),
                Some(bad),
                "{bad} should be reported as unsupported"
            );
        }
        // Supported shorts (incl. bundles + attached/standalone value shorts) pass.
        // -c (count), -t (type), -g (glob), -M (max-columns) were added in v0.79.
        for ok in [
            "-i", "-w", "-F", "-l", "-n", "-r", "-R", "-H", "-A2", "-nA2", "-niB3", "-C", "-m",
            "-m5", "-iw", "-c", "-t", "-g", "-M", "-M512",
        ] {
            assert_eq!(
                first_unsupported_grep_flag(&s(&["grep", ok, "pat"])),
                None,
                "{ok} is supported, must not be flagged"
            );
        }
        // Dash-then-symbol/digit terms are searchable patterns, not flags.
        for pat in ["->", "-1", "-.*", "-->foo"] {
            assert_eq!(
                first_unsupported_grep_flag(&s(&["grep", pat])),
                None,
                "{pat} is a pattern, must not be flagged"
            );
        }
        // `--` escapes a literal flag-shaped term.
        assert_eq!(first_unsupported_grep_flag(&s(&["grep", "--", "-v"])), None);
        // Unsupported flag after a supported value short's value is still caught.
        assert_eq!(
            first_unsupported_grep_flag(&s(&["grep", "-A", "2", "-v", "pat"])).as_deref(),
            Some("-v")
        );
    }

    /// A LONG flag this subcommand does not implement is deliberately left to the
    /// pattern positional (so `grep --no-default-features` stays searchable), and
    /// the real pattern then becomes a path. That behavior is a published CLI
    /// contract and is unchanged; what is new is that the resulting rg error gets
    /// explained instead of standing alone as `rg: <your pattern>: No such file`.
    #[test]
    fn grep_flaglike_pattern_hint_fires_only_on_the_confusing_pairing() {
        const MISSING: &str = "rg: /repo/alpha: No such file or directory (os error 2)";

        // The confusing case: flag-shaped pattern AND a missing-path complaint.
        for flag in ["--quiet", "--json", "--check-only", "--no_color", "--x1"] {
            let hint = grep_flaglike_pattern_hint(flag, MISSING, false)
                .unwrap_or_else(|| panic!("{flag} + missing-path must be explained"));
            assert!(hint.contains(flag), "hint must name the token: {hint}");
            assert!(
                hint.contains("grep -- "),
                "hint must give the escape that keeps the literal search working: {hint}"
            );
        }

        // Same pattern, a DIFFERENT rg failure: not this diagnosis, stay quiet.
        assert_eq!(
            grep_flaglike_pattern_hint("--quiet", "rg: regex parse error", false),
            None
        );

        // An explicit `--` means the caller already said they meant the literal.
        // Their missing path is their own typo (`grep -- --no-default-features
        // src/nope`), and the hint would tell them to add the separator they
        // just typed.
        assert_eq!(
            grep_flaglike_pattern_hint("--no-default-features", MISSING, true),
            None,
            "an explicit `--` must silence the hint entirely"
        );

        // A genuine literal search that happens to fail on a missing path must not
        // be told it typed a flag — these are ordinary patterns, not `--word`.
        for pat in ["alpha", "-v", "->", "-1", "-.*", "--", "--=x", "--a b"] {
            assert_eq!(
                grep_flaglike_pattern_hint(pat, MISSING, false),
                None,
                "{pat} is not a long-flag spelling and must not be explained as one"
            );
        }
    }

    // --- rebuild-index / reindex concurrency gate (audit 2026-08-02 P1-3) ---

    /// A project dir with one source file and an index already built, plus a
    /// HELD `index.lock`. The guard file is returned: flock is released when it
    /// drops, so the caller must keep it alive for the length of the assertion.
    #[cfg(unix)]
    fn locked_project() -> (tempfile::TempDir, std::fs::File) {
        use std::os::unix::io::AsRawFd;
        let project = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(
            project.path().join("src/a.ts"),
            "export function alpha(): number { return 1; }\n",
        )
        .unwrap();
        let cg = project.path().join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&cg).unwrap();
        // Built through the CLI's own indexer entry point rather than a raw
        // `Database::open`: the reader_nondestructive drift guard scans this
        // whole file (test module included) for the destructive constructor, and
        // it is right to — the fixture does not need an exemption when the real
        // command builds the same index.
        cmd_incremental_index(project.path(), true, true).unwrap();

        // flock is held per OPEN FILE DESCRIPTION, not per process: a second
        // `open()` of the same path — which is exactly what
        // `other_process_holds_index_lock` does — conflicts with this one even
        // from inside this same test process. That makes the "another server is
        // running" state reproducible without spawning anything.
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cg.join("index.lock"))
            .unwrap();
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "test fixture failed to take the index lock");
        assert!(
            crate::mcp::server::other_process_holds_index_lock(&cg),
            "fixture precondition: the probe must SEE the held lock, else every \
             assertion below passes vacuously"
        );
        (project, lock)
    }

    /// The P1-3 defect: `rebuild-index` warned and then renamed a fresh index
    /// over the one a running MCP server still had open, stranding that server's
    /// writes on a deleted inode. Refusal is the default now.
    #[cfg(unix)]
    #[test]
    fn rebuild_index_refuses_while_another_process_holds_the_index_lock() {
        let (project, _lock) = locked_project();
        let before = std::fs::metadata(project.path().join(CODE_GRAPH_DIR).join("index.db"))
            .unwrap()
            .len();

        let err = cmd_rebuild_index(
            project.path(),
            RebuildIndexArgs {
                confirm: true,
                quiet: false,
                no_embed: true,
                force: false,
                json: false,
            },
        )
        .expect_err("a held index lock must block the rename");
        let msg = err.to_string();
        assert!(
            msg.contains("holds the index lock"),
            "the refusal must name the cause: {msg}"
        );
        assert!(
            msg.contains("--force"),
            "the refusal must name the escape hatch: {msg}"
        );
        // The refusal happens BEFORE any mutation — the live index is untouched.
        let after = std::fs::metadata(project.path().join(CODE_GRAPH_DIR).join("index.db"))
            .unwrap()
            .len();
        assert_eq!(before, after, "a refused rebuild must not touch index.db");
    }

    /// `--quiet` used to `return` out of `warn_if_index_locked` before the probe
    /// ran, so the noisiest-path caller (hooks) was the one with NO protection.
    /// quiet governs printing only.
    #[cfg(unix)]
    #[test]
    fn rebuild_index_quiet_still_detects_the_lock() {
        let (project, _lock) = locked_project();
        let err = cmd_rebuild_index(
            project.path(),
            RebuildIndexArgs {
                confirm: true,
                quiet: true,
                no_embed: true,
                force: false,
                json: false,
            },
        )
        .expect_err("--quiet must silence output, not the check");
        assert!(
            err.to_string().contains("holds the index lock"),
            "got: {err}"
        );
    }

    /// The escape hatch has to actually work, or users with a defunct lock file
    /// are stuck with no way to rebuild.
    #[cfg(unix)]
    #[test]
    fn rebuild_index_force_proceeds_despite_the_lock() {
        let (project, _lock) = locked_project();
        cmd_rebuild_index(
            project.path(),
            RebuildIndexArgs {
                confirm: true,
                quiet: true,
                no_embed: true,
                force: true,
                json: false,
            },
        )
        .expect("--force must override the refusal");
        let db =
            Database::open_nondestructive(&project.path().join(CODE_GRAPH_DIR).join("index.db"))
                .unwrap();
        let nodes: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert!(nodes > 0, "the forced rebuild must produce a usable index");
    }

    /// Sibling path: `reindex --from-snapshot` unlinks index.db outright, which
    /// strands the same open fd. It had no warning at all before this.
    #[cfg(unix)]
    #[test]
    fn reindex_from_snapshot_refuses_while_locked_and_keeps_the_index() {
        let (project, _lock) = locked_project();
        let db_path = project.path().join(CODE_GRAPH_DIR).join("index.db");

        let err = cmd_reindex(
            project.path(),
            ReindexArgs {
                from_snapshot: true,
                no_embed: true,
                force: false,
                json: false,
            },
        )
        .expect_err("a held index lock must block the unlink");
        assert!(
            err.to_string().contains("holds the index lock"),
            "got: {err}"
        );
        assert!(
            db_path.exists(),
            "the refusal must come BEFORE the remove_file, or the damage is done anyway"
        );
    }

    /// Negative control: with no lock held, the same call goes through. Without
    /// it, a guard that refused unconditionally would pass every test above.
    #[cfg(unix)]
    #[test]
    fn rebuild_index_proceeds_when_no_one_holds_the_lock() {
        let (project, lock) = locked_project();
        drop(lock); // release
        assert!(
            !crate::mcp::server::other_process_holds_index_lock(
                &project.path().join(CODE_GRAPH_DIR)
            ),
            "control precondition: the lock must read as free once released"
        );
        cmd_rebuild_index(
            project.path(),
            RebuildIndexArgs {
                confirm: true,
                quiet: true,
                no_embed: true,
                force: false,
                json: false,
            },
        )
        .expect("an unlocked index must rebuild without --force");
    }

    /// The gate has to EXCLUDE, not just observe. While it only probed, two
    /// `rebuild-index --confirm` runs both read the lock as free, both entered
    /// the `index.db.rebuild-*` temp sweep — which deletes any other run's
    /// in-progress temp on purpose — and the loser died with a bare SQLite
    /// `disk I/O error` (QA ISSUE-008).
    #[cfg(unix)]
    #[test]
    fn lock_index_for_replace_claims_the_lock_it_reports_free() {
        let (project, lock) = locked_project();
        drop(lock);
        let cg = project.path().join(CODE_GRAPH_DIR);
        assert!(
            !crate::mcp::server::other_process_holds_index_lock(&cg),
            "precondition: the lock must start free, else the claim below is unobservable"
        );

        let guard = lock_index_for_replace(&cg, false, true).unwrap();
        assert!(
            guard.is_some(),
            "a free lock must be CLAIMED, not merely observed to be free"
        );
        assert!(
            crate::mcp::server::other_process_holds_index_lock(&cg),
            "while the guard is alive the lock must read as HELD — that is the whole \
             difference between excluding a concurrent rebuild and warning about a server"
        );

        drop(guard);
        assert!(
            !crate::mcp::server::other_process_holds_index_lock(&cg),
            "the guard must release on drop, or one rebuild would poison every later run"
        );
    }

    /// Release is platform-asymmetric, and so is what "held" even LOOKS like.
    /// On unix the flock dies with the handle and the FILE is kept on purpose —
    /// deleting it would hand a concurrent holder's lock to a different inode.
    /// On Windows the file IS the lock, so the guard must delete it; a stranded
    /// dead-PID lock file would refuse every later rebuild and push every server
    /// start into secondary read-only mode.
    ///
    /// The probe cannot carry both arms. `other_process_holds_index_lock` answers
    /// "does ANOTHER process hold it", and its non-unix arm says so literally
    /// (`pid != std::process::id()`), so a lock this very process holds reads as
    /// free there — correctly. Only unix's flock conflicts with a second open in
    /// the same process. So the held-state precondition goes through the lock
    /// FILE, which both platforms create and which is the whole lock on non-unix;
    /// the probe is asserted on unix only. Keeping that precondition cfg-free is
    /// deliberate: the line the Windows arm depends on is then executed by every
    /// platform's CI, not just the one that cannot run here.
    #[test]
    fn index_lock_guard_releases_on_drop_on_this_platform() {
        // Deliberately does NOT use `locked_project()` — that fixture is unix-only
        // (it takes a raw flock), and the arm this test exists for is the Windows
        // one.
        let project = tempfile::TempDir::new().unwrap();
        let cg = project.path().join(CODE_GRAPH_DIR);
        std::fs::create_dir_all(&cg).unwrap();
        assert!(
            !crate::mcp::server::other_process_holds_index_lock(&cg),
            "precondition: a fresh project has no lock"
        );

        let guard = crate::mcp::server::acquire_index_lock_guard(&cg)
            .expect("a free lock must be acquirable");
        assert!(
            cg.join("index.lock").exists(),
            "precondition: the guard must have created the lock file — on non-unix \
             that file IS the lock, and the removal assertion below is vacuous without it"
        );
        #[cfg(unix)]
        assert!(
            crate::mcp::server::other_process_holds_index_lock(&cg),
            "precondition: on unix the flock must read as held — a second open in \
             this same process conflicts, which is what the CLI gate relies on"
        );
        drop(guard);

        assert!(
            !crate::mcp::server::other_process_holds_index_lock(&cg),
            "the lock must read as FREE after the guard drops, or one CLI rebuild \
             poisons every later run on this machine"
        );
        #[cfg(unix)]
        assert!(
            cg.join("index.lock").exists(),
            "unix keeps the file (flock lives on the inode); removing it would \
             break mutual exclusion with a concurrent holder"
        );
        #[cfg(not(unix))]
        assert!(
            !cg.join("index.lock").exists(),
            "non-unix must remove the file — its existence IS the lock"
        );
    }

    /// The third arm: the lock is not HELD, it simply cannot be opened. That arm
    /// decides a rebuild proceeds UNLOCKED, so a wrong verdict here either bricks
    /// a rebuild that used to work (if it refused) or silently drops the
    /// exclusion. Reached with a read-only `.code-graph/`, which is also why the
    /// asymmetry exists: refusing would make this gate the reason the command
    /// stopped working, on a directory whose permissions have nothing to do with
    /// concurrency.
    #[cfg(unix)]
    #[test]
    fn lock_index_for_replace_proceeds_when_the_lock_file_cannot_be_opened() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping: root ignores directory permissions");
            return;
        }
        let (project, lock) = locked_project();
        drop(lock);
        let cg = project.path().join(CODE_GRAPH_DIR);
        std::fs::remove_file(cg.join("index.lock")).unwrap();
        let original = std::fs::metadata(&cg).unwrap().permissions();
        std::fs::set_permissions(&cg, std::fs::Permissions::from_mode(0o555)).unwrap();

        let outcome = lock_index_for_replace(&cg, false, true);

        // Restore before asserting so a failure still leaves a removable TempDir.
        std::fs::set_permissions(&cg, original).unwrap();
        let guard = outcome.expect("an unopenable lock file must not refuse the replace");
        assert!(
            guard.is_none(),
            "nothing was locked, so no guard can be handed back"
        );
    }

    /// End of the same story, at the command level: with rebuild #1's guard held,
    /// rebuild #2 gets the explanatory refusal instead of a SQLite error, and the
    /// refusal names the concurrent-rebuild case (the old text blamed only "a
    /// running MCP server", which was never true here).
    #[cfg(unix)]
    #[test]
    fn a_second_rebuild_refuses_while_the_first_holds_the_lock() {
        let (project, lock) = locked_project();
        drop(lock);
        let cg = project.path().join(CODE_GRAPH_DIR);
        // Stand-in for rebuild #1: the exact guard cmd_rebuild_index now keeps
        // alive for the length of its run.
        let _first = lock_index_for_replace(&cg, false, true)
            .unwrap()
            .expect("rebuild #1 must get the lock");

        let err = cmd_rebuild_index(
            project.path(),
            RebuildIndexArgs {
                confirm: true,
                quiet: true,
                no_embed: true,
                force: false,
                json: false,
            },
        )
        .expect_err("rebuild #2 must refuse while #1 holds the lock");
        let msg = err.to_string();
        assert!(
            msg.contains("holds the index lock"),
            "the refusal must name the cause: {msg}"
        );
        assert!(
            msg.contains("rebuild-index"),
            "the refusal must cover the concurrent-CLI case, not just the MCP server: {msg}"
        );
    }
}
