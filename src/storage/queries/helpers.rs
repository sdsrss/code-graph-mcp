/// Maximum number of parameters in a single IN clause to stay within SQLite limits.
pub(super) const MAX_IN_PARAMS: usize = 500;

pub(super) fn first_row<T>(
    mut rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> rusqlite::Result<Option<T>> {
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Escape LIKE metacharacters in user-supplied input for use in a `LIKE ? ESCAPE '\'`
/// pattern. The backslash itself MUST be escaped FIRST — it is the escape char, so a
/// literal `\` in the input would otherwise consume the following char (`a\b` wrongly
/// matches `ab`, a trailing `\` matches nothing). Order is load-bearing: `\` → `%` → `_`.
pub(super) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// True when `path` is `dir` itself or lies inside it: `src` covers `src/a.rs`
/// and a file named `src`, not `src2/a.rs` or `src.rs`. A trailing `/` on `dir`
/// changes nothing, and an empty `dir` (or `/`) covers nothing. A bare
/// `starts_with` also matched the sibling, which made `ignore_paths: ["src"]`
/// hide `src2/`'s dead code behind a "No dead code found".
pub(super) fn path_is_under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// [`path_is_under`] for SQL, as the two parameters of
/// `(f.path = :exact OR f.path LIKE :pattern ESCAPE '\')`. Here an empty `dir`
/// is the whole project — `module_overview`'s `.` — so it yields a pattern
/// that matches every path. `LIKE 'src%'` also matched `src2/` and `src.rs`.
pub(super) fn path_under_sql_params(dir: &str) -> (String, String) {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        (String::new(), "%".to_string())
    } else {
        (dir.to_string(), format!("{}/%", escape_like(dir)))
    }
}

pub(super) fn make_placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|i| format!("?{}", i))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
pub(crate) fn test_db() -> (crate::storage::db::Database, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = crate::storage::db::Database::open(&tmp.path().join("test.db")).unwrap();
    (db, tmp)
}
