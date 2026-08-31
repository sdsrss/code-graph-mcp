//! Keeping `.code-graph/` out of the user's commits.
//!
//! The index directory holds a multi-hundred-MB SQLite file that is a pure
//! cache — committing it is never what the user wants, and `git add -A` will do
//! exactly that unless `.gitignore` names it. The write used to live inside
//! `McpServer::from_project_root`, so a pure-CLI install (hook-driven
//! `incremental-index`, never starting the MCP server) left a fresh repo with an
//! untracked `.code-graph/` and no ignore entry (audit 2026-08-02 DB-4).

use std::path::Path;

use crate::domain::CODE_GRAPH_DIR;

/// Ensure `<project_root>/.gitignore` names `.code-graph/`.
///
/// Idempotent and best-effort: an unwritable / unreadable `.gitignore` is a
/// warning, never an error — indexing must not fail because the ignore file
/// could not be updated. Appends (rather than read-modify-write) so a
/// concurrent writer's line cannot be clobbered.
///
/// Shared by both index-creating entry points — the MCP server's
/// `from_project_root` and the CLI index commands — so the two cannot drift.
pub(crate) fn ensure_code_graph_dir_ignored(project_root: &Path) {
    let gitignore_path = project_root.join(".gitignore");
    let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
    // Match both `.code-graph` and `.code-graph/` spellings, so a user who wrote
    // the entry by hand does not get a duplicate appended on every run.
    if content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.trim_end_matches('/') == CODE_GRAPH_DIR
    }) {
        return;
    }
    use std::io::Write as _;
    // Through `owned::append_owned`: the repo can ship its own `.gitignore` as
    // a symlink, and a plain append followed it into the target (audit
    // 2026-08-29 SEC-03). Best-effort as before — a refusal is a warning.
    match crate::utils::owned::append_owned(&gitignore_path) {
        Ok(mut f) => {
            // Add newline separator if the file doesn't end with one
            if !content.ends_with('\n') && !content.is_empty() {
                let _ = f.write_all(b"\n");
            }
            let _ = f.write_all(format!("{}/\n", CODE_GRAPH_DIR).as_bytes());
        }
        Err(e) => tracing::warn!("Could not update .gitignore: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_entry_into_a_repo_that_has_no_gitignore() {
        let root = tempfile::TempDir::new().unwrap();
        ensure_code_graph_dir_ignored(root.path());
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, ".code-graph/\n", "got: {content:?}");
    }

    #[test]
    fn appends_after_a_missing_trailing_newline_without_joining_lines() {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::write(root.path().join(".gitignore"), "node_modules").unwrap();
        ensure_code_graph_dir_ignored(root.path());
        let content = std::fs::read_to_string(root.path().join(".gitignore")).unwrap();
        assert_eq!(content, "node_modules\n.code-graph/\n", "got: {content:?}");
    }

    /// A repo can ship its own `.gitignore` as a symlink. The append followed
    /// it and wrote `.code-graph/` into the LINK TARGET — the constant-content
    /// half of the same primitive that truncates files in `telemetry::rotate`
    /// (audit 2026-08-29 SEC-03): pollution rather than destruction, but the
    /// same "a repo-supplied path is treated as our own file" root cause.
    #[cfg(unix)]
    #[test]
    fn refuses_to_append_through_a_symlinked_gitignore() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let victim = dir.path().join("victim.conf");
        std::fs::write(&victim, "keep = 1\n").unwrap();
        std::os::unix::fs::symlink(&victim, root.join(".gitignore")).unwrap();

        ensure_code_graph_dir_ignored(&root);

        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "keep = 1\n",
            "the link target must not be appended to"
        );

        // Positive control: a regular `.gitignore` in a sibling repo still gets
        // the entry, so the assertion above is not green by inaction.
        let ok_root = dir.path().join("plain");
        std::fs::create_dir(&ok_root).unwrap();
        ensure_code_graph_dir_ignored(&ok_root);
        assert_eq!(
            std::fs::read_to_string(ok_root.join(".gitignore")).unwrap(),
            ".code-graph/\n"
        );
    }

    /// Idempotence across BOTH spellings — a hand-written `.code-graph` (no
    /// slash) must not collect a second `.code-graph/` line on every index run.
    #[test]
    fn is_idempotent_for_both_slash_spellings() {
        for existing in [".code-graph/\n", ".code-graph\n"] {
            let root = tempfile::TempDir::new().unwrap();
            let p = root.path().join(".gitignore");
            std::fs::write(&p, existing).unwrap();
            ensure_code_graph_dir_ignored(root.path());
            ensure_code_graph_dir_ignored(root.path());
            let content = std::fs::read_to_string(&p).unwrap();
            assert_eq!(
                content, existing,
                "existing {existing:?} entry must be recognized, got: {content:?}"
            );
        }
    }
}
