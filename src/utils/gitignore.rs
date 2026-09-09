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
///
/// Set `CODE_GRAPH_NO_GITIGNORE=1` to disable this entirely — for a user whose
/// own ignore rules (e.g. a global `core.excludesFile`) already cover
/// `.code-graph/`, the unconditional append otherwise touches every repo the
/// tool runs in, including ones they don't own.
pub(crate) fn ensure_code_graph_dir_ignored(project_root: &Path) {
    let disabled = std::env::var("CODE_GRAPH_NO_GITIGNORE").ok().as_deref() == Some("1");
    ensure_code_graph_dir_ignored_unless(project_root, disabled);
}

/// [`ensure_code_graph_dir_ignored`] with the switch already read.
///
/// The env read stays in the caller so tests can drive BOTH arms by argument.
/// Setting `CODE_GRAPH_NO_GITIGNORE` from a test instead would be process-global
/// while four sibling tests in this module call the public entry point on other
/// threads — the same `env::set_var` race the embedding tests removed by
/// injection (`src/embedding/model.rs`, `record_download_state_at`).
fn ensure_code_graph_dir_ignored_unless(project_root: &Path, disabled: bool) {
    if disabled {
        return;
    }
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

    /// The switch disables the write entirely — for a user whose own ignore
    /// rules already cover `.code-graph/`, the tool never touches `.gitignore`
    /// at all, not even to create it.
    ///
    /// Driven by argument, not by `env::set_var`: the switch is read at the
    /// public entry point, and four sibling tests in this module call that
    /// entry point on other threads under `cargo test`. A process-global write
    /// here makes THEM take the early return — the failure is theirs, not this
    /// test's, which is what makes it easy to misread.
    #[test]
    fn the_switch_suppresses_the_write() {
        let root = tempfile::TempDir::new().unwrap();
        ensure_code_graph_dir_ignored_unless(root.path(), true);
        assert!(
            !root.path().join(".gitignore").exists(),
            "no .gitignore should be created while the switch is set"
        );

        let existing_root = tempfile::TempDir::new().unwrap();
        let p = existing_root.path().join(".gitignore");
        std::fs::write(&p, "node_modules/\n").unwrap();
        ensure_code_graph_dir_ignored_unless(existing_root.path(), true);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "node_modules/\n",
            "an existing .gitignore must be left untouched"
        );

        // Positive control: the same call with the switch off still writes, so
        // the assertions above are not green by inaction.
        let control_root = tempfile::TempDir::new().unwrap();
        ensure_code_graph_dir_ignored_unless(control_root.path(), false);
        assert_eq!(
            std::fs::read_to_string(control_root.path().join(".gitignore")).unwrap(),
            ".code-graph/\n"
        );
    }
}
