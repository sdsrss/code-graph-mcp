use super::*;

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
pub(crate) fn resolve_project_root_bounded(cwd: &Path, home: Option<&Path>) -> PathBuf {
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
pub(crate) fn worktree_main_root(root: &Path) -> Option<PathBuf> {
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
pub(crate) fn effective_read_root(project_root: &Path) -> PathBuf {
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
pub(crate) fn normalize_user_path(project_root: &Path, raw: &str) -> Result<String> {
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
pub(crate) fn normalize_user_path_from(
    project_root: &Path,
    cwd: &Path,
    raw: &str,
) -> Result<String> {
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
pub(crate) fn needs_lexical_windows_rejection(raw: &str, natively_absolute: bool) -> bool {
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

pub(crate) fn normalize_user_path_from_on(
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

pub(crate) fn normalize_user_path_key(
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
pub(crate) fn is_cwd_anchored(raw: &str) -> bool {
    raw == "."
        || raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with(".\\")
        || raw.starts_with("..\\")
}

/// The one stderr surface for a near-miss root rebase, shared by both arms so
/// the disclosure wording stays identical.
pub(crate) fn note_root_rebase(raw: &str, root: &Path) {
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
pub(crate) fn collapse_within_root(rel: &Path) -> Option<String> {
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
