//! Shared graph snapshot — producer/consumer for `code-graph-snapshot-*.db.zst`
//! GitHub Release artifacts. See docs/superpowers/specs/2026-05-10-shared-graph-snapshot-design.md.

pub mod config;
pub mod install;
pub mod meta;

#[cfg(test)]
mod tests;

pub use install::{resolve_snapshot_source, try_install};
pub use meta::SnapshotMeta;

use anyhow::{Context, Result};
use std::path::Path;

/// Build a snapshot at `out` by running a full index in a temp dir, dropping
/// the vec table, vacuuming, writing meta keys, then VACUUM INTO the output
/// path.
///
/// When `out` ends in `.db.zst`, the result is zstd-compressed (level 9, to
/// match the producer workflow template). For any other extension the raw
/// SQLite file is written, and the caller is responsible for compression.
pub fn create(root: &Path, out: &Path, include_vec: bool) -> Result<()> {
    use crate::indexer::pipeline::run_full_index;
    use crate::storage::db::Database;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Index into a staging DB in a temp dir so we don't clobber the
    // project's own .code-graph/index.db.
    let tmp = tempfile::tempdir().context("create tempdir for snapshot build")?;
    let staging_db = tmp.path().join("staging.db");

    {
        let db = if include_vec {
            Database::open_with_vec(&staging_db)?
        } else {
            Database::open(&staging_db)?
        };

        run_full_index(&db, root, None, None)?;

        let conn = db.conn();

        // Drop vec table when caller doesn't want it (defensive IF EXISTS).
        if !include_vec {
            conn.execute_batch("DROP TABLE IF EXISTS node_vectors;")?;
        }
        // Never ship the content-hash embedding_cache: it is a LOCAL reuse cache (rebuilt on
        // demand and tied to this machine's model fingerprint), so it only bloats the artifact
        // (~1x the vectors) and a recipient with a different model would drop it on open anyway.
        conn.execute_batch("DROP TABLE IF EXISTS embedding_cache;")?;

        // Best-effort git commit hash; empty string if not a git repo.
        // Silence stderr so non-repo callers (snapshot CLI on a non-git dir,
        // unit tests creating fixtures without git init) don't see git's
        // `fatal: not a git repository` line leak through.
        let source_commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        meta::write_meta(conn, meta::META_SNAPSHOT_SOURCE_COMMIT, &source_commit)?;
        meta::write_meta(conn, meta::META_SNAPSHOT_CREATED_AT, &now.to_string())?;
        meta::write_meta(
            conn,
            meta::META_SNAPSHOT_TOOL_VERSION,
            env!("CARGO_PKG_VERSION"),
        )?;
        meta::write_meta(
            conn,
            meta::META_SNAPSHOT_SCHEMA_VERSION,
            &crate::storage::schema::SCHEMA_VERSION.to_string(),
        )?;
        meta::write_meta(
            conn,
            meta::META_SNAPSHOT_INCLUDES_VEC,
            if include_vec { "true" } else { "false" },
        )?;

        // Merge WAL back into main file so VACUUM INTO produces a single
        // self-contained file with no -wal/-shm sidecars.
        conn.execute_batch("PRAGMA journal_mode = DELETE;")?;
    } // `db` (and its Connection) dropped here — file fully flushed.

    // VACUUM INTO writes a compacted copy; destination must not exist.
    let compress_output = out.to_string_lossy().ends_with(".db.zst");
    let vacuum_target: std::path::PathBuf = if compress_output {
        tmp.path().join("compacted.db")
    } else {
        out.to_path_buf()
    };

    let conn = rusqlite::Connection::open(&staging_db)?;
    // Uniformity: pin mmap=0 on every connection-open path (matches db.rs open()/
    // open_readonly). Not a live hazard here — a short-lived, single-process producer
    // connection, not a long-lived reader during a concurrent VACUUM — but keeps a
    // future nonzero SQLITE_DEFAULT_MMAP_SIZE from silently re-enabling mmap anywhere.
    conn.execute_batch("PRAGMA mmap_size = 0;")?;
    let target_str = vacuum_target.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{target_str}';"))
        .with_context(|| format!("VACUUM INTO '{}'", vacuum_target.display()))?;

    if compress_output {
        let raw = std::fs::read(&vacuum_target).context("read compacted db")?;
        let compressed = zstd::encode_all(&raw[..], 9).context("zstd encode snapshot")?;
        std::fs::write(out, &compressed).context("write compressed snapshot")?;
        // Integrity sidecar: blake3 of the compressed artifact, hex-encoded.
        // Consumers fetch `<asset>.blake3` and verify it before decompressing.
        // Mirrors the blake3 artifact-integrity convention used for the embedding
        // model download (src/embedding/model.rs). Upload BOTH the `.db.zst` and
        // this `.db.zst.blake3` to the GitHub release.
        let digest = blake3::hash(&compressed).to_hex();
        let sidecar = format!("{}.blake3", out.display());
        std::fs::write(&sidecar, digest.as_bytes())
            .with_context(|| format!("write checksum sidecar {sidecar}"))?;
    }

    Ok(())
}

/// Open a snapshot file and read its meta. Accepts both zstd-compressed
/// (`.db.zst`, what producers/consumers exchange) and raw SQLite (`.db`,
/// the direct output of [`create`] before zstd compression). The format is
/// detected from the file's magic bytes, not the extension.
pub fn inspect(file: &Path) -> Result<SnapshotMeta> {
    inspect_with_cap(file, install::MAX_DECOMPRESSED_BYTES)
}

/// [`inspect`], with the decompressed-size ceiling as a parameter so the cap is
/// reachable from a test without a 100 MB fixture.
pub(crate) fn inspect_with_cap(file: &Path, cap: u64) -> Result<SnapshotMeta> {
    use crate::storage::db::Database;

    // First-call site for the user's path. Without context the user-facing
    // error is just "No such file or directory (os error 2)" with no path —
    // typo at the CLI gives a useless error.
    let file_size_bytes = std::fs::metadata(file)
        .with_context(|| format!("stat snapshot file '{}'", file.display()))?
        .len();

    // Read the magic only. This used to be `fs::read` the whole file followed by
    // `zstd::decode_all`, which held the compressed AND the fully decompressed
    // payload in memory with no ceiling — while `install.rs` next door has capped
    // the identical artifact at 100 MB since it was written. `inspect` is a
    // scripted release-artifact check, so it is pointed at bytes nobody has
    // vouched for yet. Audit 2026-09-07 SURF-25.
    // `read_exact`, not `read`: a single `read` is permitted to return fewer
    // bytes than asked for, and a short read would leave the 16-byte SQLite
    // header comparison falsely negative — a valid snapshot reported as garbage.
    // Anything under 16 bytes cannot be either format, so it is refused below
    // through the same message rather than given a partial-magic special case.
    //
    // 20 bytes rather than 16 because the raw arm also needs byte 18, the SQLite
    // header's "file format write version" (1 = rollback journal, 2 = WAL). A
    // file of 16..=18 bytes is too short to carry byte 18 and so reads as
    // non-WAL; 19 and 20 byte files do carry it and can read as WAL. Harmless
    // either way — nothing that short survives the checks below — but stated
    // exactly, because an earlier version of this comment said "16..20 reads as
    // non-WAL" and that is false for two of those five sizes.
    let mut head = [0u8; 20];
    let head: &[u8] = if file_size_bytes >= 16 {
        use std::io::Read;
        // `min` on the u64, THEN cast. `file_size_bytes as usize` first would
        // truncate on a 32-bit target, and a file whose size is an exact
        // multiple of 2^32 would cast to 0 — `read_exact` on an empty slice
        // succeeds, so a perfectly valid snapshot would be refused as "not a
        // code-graph snapshot". The `>= 16` gate above is on the u64 and does
        // not catch it. Windows and Linux ship x86_64/aarch64 here, so this has
        // never been reachable in a released artifact; it is still the wrong
        // order to write.
        let want = std::cmp::min(20u64, file_size_bytes) as usize;
        let mut f = std::fs::File::open(file)
            .with_context(|| format!("read snapshot file '{}'", file.display()))?;
        f.read_exact(&mut head[..want])
            .with_context(|| format!("read snapshot file '{}'", file.display()))?;
        &head[..want]
    } else {
        &[]
    };
    let wal_mode = head.len() >= 19 && head[18] == 2;

    // zstd magic = 0x28 0xB5 0x2F 0xFD; SQLite = "SQLite format 3\0".
    //
    // Three ways in, and two of them materialise a staging copy: the compressed
    // arm always (decompression has to land somewhere, and `decompress_with_cap`
    // is what bounds where), the raw arm when the file is WAL-mode or when an
    // in-place read turns out to be impossible. This binding keeps whichever
    // tempdir was used alive for as long as `db` holds a path inside it, and is
    // declared BEFORE `db` so it drops after it.
    let mut _staging_dir: Option<tempfile::TempDir> = None;
    let mut opened_in_place = false;
    let mut db = if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        let tmp = tempfile::tempdir().context("inspect tempdir")?;
        let decompressed = tmp.path().join("snapshot.db");
        install::decompress_with_cap(file, &decompressed, cap).context("zstd decode")?;
        let opened = Database::open(&decompressed)?;
        _staging_dir = Some(tmp);
        opened
    } else if head.starts_with(b"SQLite format 3\0") {
        // Deliberately UNCAPPED, and the first version of this fix got it wrong:
        // it applied `cap` here too, which refuses this repository's own
        // `.code-graph/index.db` (163 MB) and so breaks `create --out x.db` ->
        // `inspect` for any project whose index outgrew the ceiling.
        //
        // What SURF-25 is about is decompression AMPLIFICATION: a small file on
        // disk expanding without bound in memory. A raw `.db` has none — its
        // size is what the caller already has on disk. There is also no
        // install-side precedent to match: `try_install` only ever consumes
        // `.db.zst` release assets, so `install` has no raw arm.
        //
        // A non-WAL raw file is opened in place and READ-ONLY rather than staged
        // through `fs::copy` first. That copy was the last cost in this arm
        // proportional to the file, for a command that reads seven meta rows and
        // two `COUNT(*)`s, and on the many systems where TMPDIR is a tmpfs the
        // copy is RAM. Measured against a 164 MB raw snapshot in `delete` mode:
        // under a 100 MB `RLIMIT_FSIZE` the staging version dies with SIGXFSZ
        // and this one exits 0 with byte-identical output. Read-only is the
        // other half — `inspect` must not write to a file the user only pointed
        // it at, and `Database::open` migrates and WAL-es what it opens. That
        // was harmless only for as long as what it opened was a throwaway copy.
        //
        // A WAL-mode file keeps the copy, and the reason is residue, not
        // correctness: SQLite cannot read a WAL database without a `-shm`, so an
        // in-place open MINTS `-wal` and `-shm` beside the user's file and a
        // read-only connection cannot clean them up on close. Measured — two
        // files left behind, on a command whose whole job is to look. Staging
        // also preserves the semantics this arm has always had: `fs::copy` takes
        // the main database file and not its `-wal`, so `inspect` reports the
        // last checkpointed state and never disturbs a live index.
        //
        // The split costs nothing in practice: `snapshot create` emits through
        // `VACUUM INTO`, and its output is `journal_mode = delete` (measured on
        // a real `create --out`), so every artifact this tool produces takes the
        // in-place arm. What reaches the staging arm is someone pointing
        // `inspect` at a live `.code-graph/index.db` — which is not a snapshot
        // and is refused below. Refused by the `schema_version == 0` verdict, to
        // be exact, NOT by the meta-table check: a live index HAS a `meta`
        // table, it just holds no `snapshot_*` rows. An earlier version of this
        // comment named the wrong one.
        //
        // No `.with_context` on the open. `open_readonly`'s own refusals are
        // already the actionable sentence — "Database schema version v99 is
        // newer than supported v10. Please update code-graph-mcp." — and
        // wrapping demoted it to a `Caused by:` line on this arm while the
        // staging arm printed it bare, so the two arms answered the same input
        // in different shapes.
        if wal_mode {
            let tmp = tempfile::tempdir().context("inspect tempdir")?;
            let staged = tmp.path().join("snapshot.db");
            std::fs::copy(file, &staged).context("stage snapshot for inspect")?;
            let opened = Database::open(&staged)?;
            _staging_dir = Some(tmp);
            opened
        } else {
            opened_in_place = true;
            Database::open_readonly(file)?
        }
    } else {
        anyhow::bail!(
            "{} is not a code-graph snapshot — expected zstd-compressed (.db.zst) or raw SQLite (.db)",
            file.display()
        );
    };
    // A file with no schema at all must be called the same thing whichever arm
    // read it. The staged arm gets `Database::open`'s bootstrap, so a file that
    // clears the 16-byte magic check while holding nothing else still opens onto
    // an empty `meta` and reaches the `schema_version == 0` verdict below.
    // Opening in place creates nothing, so the same file would instead surface
    // SQLite's `no such table: meta` from the first read. `open_readonly` cannot
    // report it — its `user_version` and `sqlite_master` probes are both
    // `unwrap_or`, so it returns Ok on a file it cannot actually read. Hence
    // this probe, run for both arms.
    //
    // Scoped claim, because an earlier version of this comment said the check
    // was "the price of the arms not disagreeing" and that is only true for the
    // narrow case it was written about. Pre-ship review found two inputs where
    // the arms still answer differently — a db whose `nodes` table is missing a
    // column makes the staging arm's bootstrap emit a multi-kilobyte SQL dump
    // where the in-place arm gives one line — so what this buys is agreement on
    // the schema-less file, not agreement in general.
    const META_PROBE: &str =
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meta'";
    let mut probe: rusqlite::Result<i64> = db.conn().query_row(META_PROBE, [], |r| r.get(0));

    // An Err here from the in-place arm is not "corrupt", it is "this file
    // cannot be read read-only", and the two have to be told apart. The case
    // that forced it: a non-WAL db with a HOT ROLLBACK JOURNAL beside it — left
    // by any writer that died mid-transaction, including a crashed
    // `snapshot create` — cannot be opened read-only at all, because SQLite must
    // roll the journal back first and a read-only connection may not write.
    // `unwrap_or(0)` swallowed that and reported the file corrupt, while the
    // staging copy reads it perfectly: the copy is opened writable, so the
    // rollback just happens. Pre-ship review demonstrated the two arms
    // disagreeing on identical bytes by flipping header byte 18 alone.
    //
    // This is the same misdiagnosis the WAL split exists to prevent — telling a
    // user their snapshot is corrupt when the real problem is that we chose an
    // access mode the file does not permit. Falling back to the copy costs a
    // staging write on a path that was already going to fail, and restores the
    // pre-0.147.0 behaviour exactly.
    if probe.is_err() && opened_in_place {
        let tmp = tempfile::tempdir().context("inspect tempdir")?;
        let staged = tmp.path().join("snapshot.db");
        std::fs::copy(file, &staged).context("stage snapshot for inspect")?;
        db = Database::open(&staged)?;
        _staging_dir = Some(tmp);
        probe = db.conn().query_row(META_PROBE, [], |r| r.get(0));
    }

    if probe.unwrap_or(0) <= 0 {
        anyhow::bail!(
            "{} is not a valid code-graph snapshot — meta is missing or unreadable (file may be truncated or corrupt)",
            file.display()
        );
    }
    let conn = db.conn();

    let source_commit =
        meta::read_meta(conn, meta::META_SNAPSHOT_SOURCE_COMMIT)?.unwrap_or_default();
    let source_url = meta::read_meta(conn, meta::META_SNAPSHOT_SOURCE_URL)?;
    let created_at = meta::read_meta(conn, meta::META_SNAPSHOT_CREATED_AT)?
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let tool_version = meta::read_meta(conn, meta::META_SNAPSHOT_TOOL_VERSION)?.unwrap_or_default();
    let schema_version = meta::read_meta(conn, meta::META_SNAPSHOT_SCHEMA_VERSION)?
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    // Magic check above only validates the SQLite header (first 16 bytes). A
    // truncated db file passes the header check, then Database::open creates
    // empty schema and every meta lookup returns None → defaults. Without
    // this guard, `inspect` would return a fake "valid empty snapshot" with
    // zeroed fields. Real snapshots always carry a non-zero schema_version.
    if schema_version == 0 && source_commit.is_empty() && tool_version.is_empty() {
        anyhow::bail!(
            "{} is not a valid code-graph snapshot — meta is missing or unreadable (file may be truncated or corrupt)",
            file.display()
        );
    }
    let includes_vec = meta::read_meta(conn, meta::META_SNAPSHOT_INCLUDES_VEC)?
        .map(|s| s == "true")
        .unwrap_or(false);
    let fetched_at =
        meta::read_meta(conn, meta::META_SNAPSHOT_FETCHED_AT)?.and_then(|s| s.parse::<i64>().ok());

    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap_or(0);
    let edge_count: i64 = crate::storage::queries::count_user_edges(conn).unwrap_or(0);

    Ok(SnapshotMeta {
        source_commit,
        source_url,
        created_at,
        tool_version,
        schema_version,
        includes_vec,
        fetched_at,
        node_count,
        edge_count,
        file_size_bytes,
    })
}
