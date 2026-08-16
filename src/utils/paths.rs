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
