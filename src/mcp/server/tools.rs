//! Tool handlers split per family. Each child module adds an `impl McpServer`
//! block that contributes the `tool_*` method called by `handle_tool` in
//! `super::mod`. The dispatcher itself stays in `super::mod` next to the
//! JSON-RPC plumbing.
//!
//! v0.18.4 split: previously one 2354-line file. The split is mechanical (no
//! semantics changed) and bisectable — the matching commit "refactor(mcp):
//! split server/tools.rs into per-tool modules" is the diff target if you're
//! cherry-picking history.

mod advanced;
mod ast_node;
mod ast_search;
mod callgraph;
mod management;
mod overview;
mod project_map;
mod refs;
mod search;

/// Normalize a caller-supplied `file_path`/`path` tool argument to the `/`
/// spelling the index stores, at TOOL ENTRY — before the value is used either
/// as a freshness target or as an index lookup key.
///
/// Why entry and not inside the freshness helper: `ensure_file_fresh_opt`
/// normalizes internally, but its normalized value is local — it returns
/// `Result<()>`, so every caller went on to hand the RAW argument to
/// `get_nodes_by_file_path` / `get_call_graph_filtered` / `get_module_exports`.
/// An MCP client on Windows that echoes back `src\Foo.cs` therefore refreshed
/// the right file and then missed the index (which stores `src/Foo.cs`),
/// reporting `File 'src\Foo.cs' not found in index` for an indexed file — the
/// issue-#34 failure mode, half-fixed. Normalizing at entry also covers the
/// `should_skip_indexing` branch, where the freshness helper never runs at all.
///
/// MCP paths are root-relative, and a relative path is never resolved against
/// the process cwd — deliberately NOT `cli::normalize_user_path`, which does
/// (see `indexer::pipeline` docs). Two other spellings of a file under the root
/// ARE mapped to the stored key, because models send them: an absolute path
/// (Haiku did, in the tokio pilot, and every path-taking tool answered "not
/// found" or a false-clean empty result — D#228) and a leading `./`. An
/// absolute path outside the root is returned unchanged, so it misses the index
/// exactly as before.
pub(super) fn normalize_path_arg(raw: &str, project_root: Option<&std::path::Path>) -> String {
    let out = normalize_path_arg_on(raw, project_root, cfg!(windows));
    // The lexical strip missed but the path is absolute: the caller may spell
    // the root through a symlink (macOS `/tmp` is `/private/tmp`), or Windows
    // may hand back the `\\?\` long form. Same fallback as
    // `cli::normalize_user_path`; canonicalize resolves `..`, so a successful
    // strip is genuinely under the root.
    if let Some(root) = project_root {
        if std::path::Path::new(&out).is_absolute() {
            if let (Ok(p), Ok(r)) = (
                std::path::Path::new(raw).canonicalize(),
                root.canonicalize(),
            ) {
                if let Ok(rel) = p.strip_prefix(&r) {
                    let rel = crate::indexer::merkle::normalize_rel_path(rel);
                    return if rel.is_empty() { ".".to_string() } else { rel };
                }
            }
        }
    }
    out
}

/// Testable core of [`normalize_path_arg`], without the filesystem fallback.
/// `backslash_is_sep` is a parameter for the same reason it is one in
/// `merkle::normalize_rel_str_on` and `cli::normalize_user_path_from_on`:
/// without it the Windows branch of the MCP entry point is reachable only from
/// the `windows-latest` CI leg, and the audit that found `find_dead_code`
/// missing its normalization also found this — every defect in this family so
/// far has been pure string logic that a Linux leg could have caught if
/// anything had been able to call it.
pub(super) fn normalize_path_arg_on(
    raw: &str,
    project_root: Option<&std::path::Path>,
    backslash_is_sep: bool,
) -> String {
    let path = crate::indexer::merkle::normalize_rel_str_on(raw, backslash_is_sep);
    if let Some(root) = project_root {
        let root =
            crate::indexer::merkle::normalize_rel_str_on(&root.to_string_lossy(), backslash_is_sep);
        let root = root.trim_end_matches('/');
        // Windows volumes are spelled `D:\` and `d:\` alike; compare the root
        // prefix case-insensitively there, and only the prefix.
        let under_root = !root.is_empty()
            && path.get(..root.len()).is_some_and(|head| {
                if backslash_is_sep {
                    head.eq_ignore_ascii_case(root)
                } else {
                    head == root
                }
            });
        if under_root {
            // A separator must follow the root: `/repo-old/a.rs` is not under
            // `/repo`. A rest that climbs out of the root keeps the absolute
            // spelling, for `normalize_path_arg`'s filesystem fallback.
            match &path[root.len()..] {
                "" | "/" => return ".".to_string(),
                rest if rest.starts_with('/') => {
                    return resolve_dot_segments(&rest[1..]).unwrap_or(path);
                }
                _ => {}
            }
        }
    }
    if looks_absolute(&path) {
        return path;
    }
    resolve_dot_segments(&path).unwrap_or(path)
}

/// Resolve `.` and `..` segments in a root-relative path, lexically, so every
/// spelling of a file reaches the index as its stored key. A different key
/// is not just a miss: the freshness refresh indexed `src/./a.rs` as a second
/// file, and every symbol in it then existed twice. `./` alone becomes `.`,
/// which `module_overview` reads as the whole project. A trailing `/` is kept,
/// because directory tools match `files.path` against the path as a prefix.
/// `None` when a `..` climbs above the root: that path stays as it was and
/// misses. A path with no `.` or `..` segment is returned unchanged.
fn resolve_dot_segments(rel: &str) -> Option<String> {
    if !rel.split('/').any(|seg| seg == "." || seg == "..") {
        return Some(rel.to_string());
    }
    let mut kept: Vec<&str> = Vec::new();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                kept.pop()?;
            }
            name => kept.push(name),
        }
    }
    if kept.is_empty() {
        return Some(".".to_string());
    }
    let mut out = kept.join("/");
    if rel.ends_with('/') {
        out.push('/');
    }
    Some(out)
}

/// A path the index can never hold as a key: rooted (`/x`, and `//host` once
/// separators are unified) or drive-qualified (`C:/x`, `C:`). The drive test
/// is the CLI's own (`utils::paths`), lexical so the Windows branch runs on
/// every host.
fn looks_absolute(path: &str) -> bool {
    path.starts_with('/') || crate::utils::paths::needs_lexical_windows_rejection(path, false)
}

#[cfg(test)]
mod normalize_path_arg_tests {
    use super::normalize_path_arg_on;

    /// The MCP entry contract, asserted for BOTH platforms from any host.
    #[test]
    fn normalizes_windows_separators_only_where_backslash_is_one() {
        assert_eq!(
            normalize_path_arg_on(r"src\parser\mod.rs", None, true),
            "src/parser/mod.rs"
        );
        assert_eq!(normalize_path_arg_on("src//a.ts", None, true), "src/a.ts");
        assert_eq!(normalize_path_arg_on("src/a.ts", None, true), "src/a.ts");
        // On Unix `\` is a legal filename byte — rewriting it would build a key
        // that misses the indexed file, which is issue #34 in reverse.
        assert_eq!(
            normalize_path_arg_on(r"src/od\bc.rs", None, false),
            r"src/od\bc.rs"
        );
        assert_eq!(normalize_path_arg_on("src//a.ts", None, false), "src/a.ts");
    }

    /// D#228: the spellings of a file under the root that map to its stored
    /// key, and the look-alikes that must not.
    #[test]
    fn maps_absolute_and_dot_slash_spellings_under_the_root() {
        let unix = Some(std::path::Path::new("/home/u/repo"));
        let cases: [(&str, &str); 11] = [
            ("/home/u/repo/src/a.rs", "src/a.rs"),
            ("/home/u/repo//src/a.rs", "src/a.rs"),
            ("/home/u/repo/./src/a.rs", "src/a.rs"),
            ("/home/u/repo", "."),
            ("/home/u/repo/", "."),
            ("./src/a.rs", "src/a.rs"),
            ("././src", "src"),
            ("./", "."),
            (".", "."),
            ("src/a.rs", "src/a.rs"),
            ("", ""),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize_path_arg_on(raw, unix, false), want, "{raw}");
        }
        // `.` and `..` segments resolve to the stored key. Left as they were,
        // `src/./a.rs` missed the index and the freshness refresh then indexed
        // it as a SECOND file, so every symbol in it existed twice.
        let dots: [(&str, &str); 9] = [
            ("/home/u/repo/src/./a.rs", "src/a.rs"),
            ("/home/u/repo/src/../src/a.rs", "src/a.rs"),
            ("src/./a.rs", "src/a.rs"),
            ("src/../src/a.rs", "src/a.rs"),
            ("src/sub/..", "src"),
            ("src/..", "."),
            ("./src/", "src/"),
            ("src/./", "src/"),
            ("src/../lib/", "lib/"),
        ];
        for (raw, want) in dots {
            assert_eq!(normalize_path_arg_on(raw, unix, false), want, "{raw}");
        }
        // Look-alikes that are ordinary names, kept as they are.
        for raw in [
            ".../a.rs",
            "..a/b.rs",
            "a/.b/c.rs",
            "a../b.rs",
            "src/",
            "src",
        ] {
            assert_eq!(normalize_path_arg_on(raw, unix, false), raw, "{raw}");
        }
        // Not under the root, or climbing out of it: unchanged, so the lookup
        // misses as it always did (an absolute one still gets the filesystem
        // fallback in `normalize_path_arg`).
        for raw in [
            "/home/u/repo-old/src/a.rs",
            "/home/u/rep",
            "/etc/passwd",
            "../x.rs",
            "src/../../x.rs",
            "a/b/../../..",
            "/home/u/repo/../repo/src/a.rs",
            "/home/u/repo/..",
        ] {
            assert_eq!(normalize_path_arg_on(raw, unix, false), raw, "{raw}");
        }
        // Case matters on Unix: `/home/u/Repo` is another directory.
        assert_eq!(
            normalize_path_arg_on("/home/u/Repo/a.rs", unix, false),
            "/home/u/Repo/a.rs"
        );

        let win = Some(std::path::Path::new(r"C:\Users\u\repo"));
        assert_eq!(
            normalize_path_arg_on(r"C:\Users\u\repo\src\a.rs", win, true),
            "src/a.rs"
        );
        assert_eq!(
            normalize_path_arg_on(r"c:\users\u\repo\src\A.rs", win, true),
            "src/A.rs",
            "the drive and root compare case-insensitively, the rest keeps its case"
        );
        assert_eq!(
            normalize_path_arg_on("C:/Users/u/repo/src/a.rs", win, true),
            "src/a.rs"
        );
        assert_eq!(
            normalize_path_arg_on(r"C:\Users\u\repo2\a.rs", win, true),
            "C:/Users/u/repo2/a.rs"
        );
        assert_eq!(
            normalize_path_arg_on(r"C:\Users\u\repo\src\.\a.rs", win, true),
            "src/a.rs"
        );
        assert_eq!(
            normalize_path_arg_on(r".\src\..\lib\a.rs", win, true),
            "lib/a.rs"
        );
        // Outside the root it is left alone, `..` and all.
        assert_eq!(
            normalize_path_arg_on(r"D:\x\..\a.rs", win, true),
            "D:/x/../a.rs"
        );
    }

    /// The filesystem fallback: a root reached through a symlink.
    #[cfg(unix)]
    #[test]
    fn maps_an_absolute_path_through_a_symlinked_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("src")).unwrap();
        std::fs::write(real.join("src/a.rs"), "").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let via_link = link.join("src/a.rs").to_string_lossy().into_owned();
        assert_eq!(
            super::normalize_path_arg(&via_link, Some(&real)),
            "src/a.rs"
        );
        let via_real = real.join("src/a.rs").to_string_lossy().into_owned();
        assert_eq!(
            super::normalize_path_arg(&via_real, Some(&link)),
            "src/a.rs"
        );
    }
}
