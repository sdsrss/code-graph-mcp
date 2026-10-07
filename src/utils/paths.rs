//! Filesystem-location helpers shared across surfaces.

use std::path::PathBuf;

/// `$HOME` (Unix) / `%USERPROFILE%` (Windows) without pulling the `dirs` crate,
/// which lives behind the `embed-model` feature. `None` when unset → the walk is
/// simply unbounded (degrades to the pre-home-bound behavior).
///
/// Lives in `utils` rather than `cli` because `outcome` needs it too and must
/// not depend upward on `cli` (`tests/hardening.rs` forbidden-edge table).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

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
pub fn needs_lexical_windows_rejection(raw: &str, natively_absolute: bool) -> bool {
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
