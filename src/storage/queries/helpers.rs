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

pub(super) fn make_placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|i| format!("?{}", i))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
pub(super) fn test_db() -> (crate::storage::db::Database, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = crate::storage::db::Database::open(&tmp.path().join("test.db")).unwrap();
    (db, tmp)
}
