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
//! Three layers, on purpose:
//!
//! * [`refuse_non_regular`] — portable, gives the caller a message that names
//!   the path and the reason. Racy on its own (the check and the open are two
//!   syscalls).
//! * `O_NOFOLLOW` on the open itself — Unix only, atomic, closes that race.
//! * [`refuse_unowned_handle`] — `fstat` on the descriptor that was ACTUALLY
//!   opened, which is the only check describing the object the write lands on.
//!   It is what catches a HARDLINK: `lstat` reports a hardlinked victim as a
//!   plain regular file and `O_NOFOLLOW` says nothing about one, so the first
//!   two layers passed it through and the write reached a second path (found by
//!   the v0.131.0 pre-tag review on the JS twin, measured there at 1,200,020 →
//!   67 bytes; this module carried the identical shape since v0.129.0). `git`
//!   cannot deliver a hardlink — its index stores no such mode — but `tar x`
//!   can.
//!
//! No layer replaces another, and callers get all three by construction.
//!
//! The hardlink half is Unix-only: `std::fs::Metadata` exposes no link count on
//! Windows, so there the guard is the `is_file()` check alone. Stated rather
//! than papered over — see [`refuse_unowned_handle`].

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

/// Reject the OPENED OBJECT if it is not a file this tool may own — judged by
/// `fstat` on the descriptor rather than by another look at the path, so it
/// describes exactly what the subsequent write will reach.
///
/// `nlink > 1` refuses a hardlink: a second directory entry means the write
/// reaches a path the tool never chose, which is the same damage as the symlink
/// case with none of the same tells. Unix-only, because `Metadata` carries no
/// link count on Windows — there this degrades to the `is_file()` check, so the
/// Windows leg is knowingly hardlink-blind rather than silently assumed safe.
///
/// The `is_file()` half is what closes the `lstat`→`open` race for the
/// non-regular cases on every platform.
fn refuse_unowned_handle(path: &Path, file: &File) -> io::Result<()> {
    let meta = file.metadata()?; // fstat on the descriptor, not stat on the path
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to write {}: the opened object is not a regular file",
                path.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "refusing to write {}: it has more than one hard link \
                     (the write would also reach another path)",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

/// Open `path` with `opts`, refusing to traverse a final-component symlink and
/// refusing the handle if what opened is not a file this tool owns.
///
/// Callers must NOT put `truncate(true)` in `opts`: `O_TRUNC` empties the file
/// on the open itself, before [`refuse_unowned_handle`] can look at it. Truncate
/// through the returned handle instead — see [`rewrite_owned`].
fn open_owned(path: &Path, opts: &mut OpenOptions) -> io::Result<File> {
    refuse_non_regular(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    refuse_unowned_handle(path, &file)?;
    Ok(file)
}

/// Append to a file the tool owns, creating it if absent.
///
/// The shape every telemetry writer used to spell inline: `usage.jsonl`
/// (`mcp::metrics`), `recommendations.jsonl` (`cli::record_cli_use`) and the
/// repo's `.gitignore` (`utils::gitignore`).
pub(crate) fn append_owned(path: &Path) -> io::Result<File> {
    open_owned(path, OpenOptions::new().create(true).append(true))
}

/// Open a file the tool owns for a full rewrite (create, then truncate).
///
/// The truncation deliberately does NOT ride on the open: `O_TRUNC` empties the
/// target before [`open_owned`]'s handle check can refuse it, which would leave
/// the whole guard decorative for exactly the destructive caller — the telemetry
/// rotator, the one that measurably destroyed a 1.2 MB file through a planted
/// link. `set_len(0)` runs only once the descriptor has been vouched for.
pub(crate) fn rewrite_owned(path: &Path) -> io::Result<File> {
    let file = open_owned(path, OpenOptions::new().create(true).write(true))?;
    file.set_len(0)?;
    Ok(file)
}

/// Open a file the tool owns for writing while KEEPING its inode and contents —
/// the index lock, whose `flock` lives on the inode and which is truncated
/// explicitly after the lock is held.
///
/// `#[cfg(unix)]` because its only caller, `lock::try_acquire_index_lock`, is;
/// the non-Unix lock is an exclusive open HANDLE and reaches the same guard
/// through `open_lock_handle` → [`refuse_non_regular`]. Without the gate this is
/// dead code on Windows, and `[lints.rust] warnings = "deny"` makes that a hard
/// `cargo check` failure on a platform this dev box never compiles.
#[cfg(unix)]
pub(crate) fn hold_owned(path: &Path) -> io::Result<File> {
    open_owned(
        path,
        OpenOptions::new().write(true).create(true).truncate(false),
    )
}

/// Open an EXISTING file the tool owns, for writing, without creating or
/// truncating it — the index-lock probe, whose whole contract is "look, do not
/// disturb". Absent path stays an error, which the probe reads as "free".
///
/// `#[cfg(unix)]` for the same reason as [`hold_owned`]: the non-Unix probe goes
/// through `open_lock_handle`, which applies [`refuse_non_regular`] itself.
#[cfg(unix)]
pub(crate) fn probe_owned(path: &Path) -> io::Result<File> {
    open_owned(path, OpenOptions::new().write(true))
}

/// Refuse a symlinked (or non-) directory, WITHOUT creating anything.
///
/// The directory-level half of this module, and the one the first pass got
/// wrong by only calling it from the three places that CREATE `.code-graph`.
/// `O_NOFOLLOW` and [`refuse_non_regular`] judge the final path component only,
/// so `.code-graph -> ../outside` holding perfectly ordinary files defeats both:
/// the write lands on a real regular file that simply is not where the caller
/// thinks it is. Anything destructive that names a path *inside* the data
/// directory therefore has to ask this first — see the callers.
///
/// Absent is fine: nothing to be redirected by yet.
pub(crate) fn reject_symlinked_dir(path: &Path) -> io::Result<()> {
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
        _ => Ok(()),
    }
}

/// Ensure `path` is a directory this tool may write into, creating it if absent.
///
/// A symlink is refused rather than followed: `create_dir_all` silently succeeds
/// on `.code-graph -> ../outside` (the path exists and resolves to a directory),
/// which is how a repo could relocate the entire index — and every telemetry
/// file with it — outside the project root while the tool reported success.
pub(crate) fn ensure_owned_dir(path: &Path) -> io::Result<()> {
    reject_symlinked_dir(path)?;
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(path)
}

// `unix` on the MODULE, not just on each test: every test in here needs
// `std::os::unix::fs::symlink` to say anything, so on Windows the module is
// empty and its two `use` lines become unused-import errors under
// `warnings = "deny"`. Gating the tests one by one leaves the imports behind —
// which is exactly how this file broke the windows-latest leg of CI on the
// v0.127.0 release commit, alongside the two helpers above.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write as _;

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

    /// RED probe (pre-tag review of v0.131.0, deferred as out-of-batch): a
    /// HARDLINK defeats both layers. `lstat` reports the victim as a plain
    /// regular file, so `refuse_non_regular` passes it, and `O_NOFOLLOW` says
    /// nothing about one. The write then lands on a path the tool never chose.
    #[test]
    fn every_owned_open_refuses_a_hardlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        let payload = format!("SECRET-HEADER\n{}\n", "A".repeat(4096));
        std::fs::write(&victim, &payload).unwrap();

        type Opener = fn(&Path) -> io::Result<File>;
        let openers: [(&str, Opener); 4] = [
            ("append_owned", append_owned),
            ("rewrite_owned", rewrite_owned),
            ("hold_owned", hold_owned),
            ("probe_owned", probe_owned),
        ];

        for (name, open) in openers {
            let link = dir.path().join(format!("hard-{name}"));
            std::fs::hard_link(&victim, &link).unwrap();
            let err = open(&link).err();
            assert!(
                err.is_some(),
                "{name} must refuse a hardlink (the write would reach {})",
                victim.display()
            );
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                payload,
                "{name} must leave the hardlinked file's contents alone"
            );
            std::fs::remove_file(&link).unwrap();
        }
    }

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
