//! Opening files the tool owns — without following a symlink out of the tree.
//!
//! Everything under `<project_root>/.code-graph/` (plus the repo's own
//! `.gitignore`) is written by fixed name into a directory that is **ordinary
//! repo content**: one `git clone` can carry a symlink where this tool expects
//! its own file. `fs::write`, `OpenOptions::append` and `File::set_len` all
//! follow symlinks, so every one of those writers was operating on the LINK
//! TARGET — a file the tool never chose and the user never named. Measured
//! consequences (audit 2026-08-29 SEC-02/SEC-03): a 1.2 MB file outside the
//! project truncated to its last ~512 KB by the telemetry rotator, an unrelated
//! config reduced to the digits of a PID by the index lock, and the whole data
//! directory relocated outside the project root by a symlinked `.code-graph`.
//!
//! The contrast that names the bug: the READ side already refuses to follow
//! symlinks into the tree (`WalkBuilder` runs `follow_links(false)`), while the
//! write side followed them out of it.
//!
//! Two layers, on purpose:
//!
//! * [`refuse_non_regular`] — portable, gives the caller a message that names
//!   the path and the reason. Racy on its own (the check and the open are two
//!   syscalls).
//! * `O_NOFOLLOW` on the open itself — Unix only, atomic, closes that race.
//!
//! Neither replaces the other, and callers get both by construction.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Reject anything at `path` that is not a regular file — a symlink above all,
/// but also a FIFO, socket or directory. An absent path is fine (the open will
/// create it); an unreadable one is left for the open to report, so this never
/// invents an error the real syscall would not have produced.
pub(crate) fn refuse_non_regular(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to write {}: not a regular file \
                 (a symlink here would redirect the write outside the project)",
                path.display()
            ),
        )),
        _ => Ok(()),
    }
}

/// Open `path` with `opts`, refusing to traverse a final-component symlink.
fn open_owned(path: &Path, opts: &mut OpenOptions) -> io::Result<File> {
    refuse_non_regular(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
}

/// Append to a file the tool owns, creating it if absent.
///
/// The shape every telemetry writer used to spell inline: `usage.jsonl`
/// (`mcp::metrics`), `recommendations.jsonl` (`cli::record_cli_use`) and the
/// repo's `.gitignore` (`utils::gitignore`).
pub(crate) fn append_owned(path: &Path) -> io::Result<File> {
    open_owned(path, OpenOptions::new().create(true).append(true))
}

/// Open a file the tool owns for a full rewrite (create + truncate).
pub(crate) fn rewrite_owned(path: &Path) -> io::Result<File> {
    open_owned(
        path,
        OpenOptions::new().create(true).write(true).truncate(true),
    )
}

/// Open a file the tool owns for writing while KEEPING its inode and contents —
/// the index lock, whose `flock` lives on the inode and which is truncated
/// explicitly after the lock is held.
pub(crate) fn hold_owned(path: &Path) -> io::Result<File> {
    open_owned(
        path,
        OpenOptions::new().write(true).create(true).truncate(false),
    )
}

/// Open an EXISTING file the tool owns, for writing, without creating or
/// truncating it — the index-lock probe, whose whole contract is "look, do not
/// disturb". Absent path stays an error, which the probe reads as "free".
pub(crate) fn probe_owned(path: &Path) -> io::Result<File> {
    open_owned(path, OpenOptions::new().write(true))
}

/// Ensure `path` is a directory this tool may write into, creating it if absent.
///
/// A symlink is refused rather than followed: `create_dir_all` silently succeeds
/// on `.code-graph -> ../outside` (the path exists and resolves to a directory),
/// which is how a repo could relocate the entire index — and every telemetry
/// file with it — outside the project root while the tool reported success.
pub(crate) fn ensure_owned_dir(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to use a symlinked {} — remove it and re-run",
                path.display()
            ),
        )),
        Ok(meta) if !meta.is_dir() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} exists and is not a directory", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(_) => std::fs::create_dir_all(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[cfg(unix)]
    #[test]
    fn every_owned_open_refuses_a_symlink_and_still_serves_a_regular_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep\n").unwrap();

        // One row per opener: a guard added to only some of them is the defect
        // class this module exists to close, so the table enumerates all three.
        type Opener = fn(&Path) -> io::Result<File>;
        let openers: [(&str, Opener); 3] = [
            ("append_owned", append_owned),
            ("rewrite_owned", rewrite_owned),
            ("hold_owned", hold_owned),
        ];

        for (name, open) in openers {
            let link = dir.path().join(format!("link-{name}"));
            std::os::unix::fs::symlink(&victim, &link).unwrap();
            let err = open(&link).expect_err("{name} must refuse a symlink");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name}: {err}");
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "keep\n",
                "{name} must leave the link target alone"
            );

            // Negative control: the same opener on a regular path works, so the
            // refusal above is about the symlink and not about the opener.
            let plain = dir.path().join(format!("plain-{name}"));
            let mut f = open(&plain).unwrap_or_else(|e| panic!("{name} on a regular path: {e}"));
            f.write_all(b"x").unwrap();
            assert!(plain.exists(), "{name} must create a regular file");
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuse_non_regular_accepts_absent_and_regular_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(refuse_non_regular(&dir.path().join("nope")).is_ok());
        let f = dir.path().join("f");
        std::fs::write(&f, "").unwrap();
        assert!(refuse_non_regular(&f).is_ok());
        assert!(
            refuse_non_regular(dir.path()).is_err(),
            "a directory is not a file we own"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_owned_dir_creates_accepts_and_refuses() {
        let dir = tempfile::TempDir::new().unwrap();
        let fresh = dir.path().join("a").join("b");
        ensure_owned_dir(&fresh).unwrap();
        assert!(fresh.is_dir());
        ensure_owned_dir(&fresh).unwrap(); // idempotent

        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        let err = ensure_owned_dir(&linked).unwrap_err();
        assert!(
            err.to_string().contains("symlinked"),
            "the refusal must say why: {err}"
        );

        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(ensure_owned_dir(&file).is_err());
    }
}
