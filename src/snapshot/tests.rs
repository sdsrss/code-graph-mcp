//! Unit tests for the snapshot module.

use crate::snapshot::meta::{read_meta, write_meta, META_SNAPSHOT_TOOL_VERSION};
use rusqlite::Connection;

fn open_with_meta_table() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);")
        .unwrap();
    conn
}

#[test]
fn write_meta_then_read_returns_same_value() {
    let conn = open_with_meta_table();
    write_meta(&conn, META_SNAPSHOT_TOOL_VERSION, "0.22.2").unwrap();
    let got = read_meta(&conn, META_SNAPSHOT_TOOL_VERSION).unwrap();
    assert_eq!(got, Some("0.22.2".to_string()));
}

#[test]
fn read_meta_returns_none_for_missing_key() {
    let conn = open_with_meta_table();
    let got = read_meta(&conn, "definitely_not_present").unwrap();
    assert_eq!(got, None);
}

#[test]
fn write_meta_overwrites_existing_value() {
    let conn = open_with_meta_table();
    write_meta(&conn, META_SNAPSHOT_TOOL_VERSION, "0.22.0").unwrap();
    write_meta(&conn, META_SNAPSHOT_TOOL_VERSION, "0.22.2").unwrap();
    let got = read_meta(&conn, META_SNAPSHOT_TOOL_VERSION).unwrap();
    assert_eq!(got, Some("0.22.2".to_string()));
}

use crate::snapshot::meta::{
    read_meta as snap_read_meta, META_SNAPSHOT_CREATED_AT, META_SNAPSHOT_INCLUDES_VEC,
    META_SNAPSHOT_SCHEMA_VERSION, META_SNAPSHOT_SOURCE_COMMIT,
};
use crate::storage::db::Database;
use std::process::Command;
use tempfile::TempDir;

fn init_git_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(p)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(p)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(p)
        .status()
        .unwrap();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(
        p.join("src/lib.rs"),
        "pub fn hello() {}\npub fn world() { hello(); }\n",
    )
    .unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(p)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(p)
        .status()
        .unwrap();
    dir
}

#[test]
fn create_writes_meta_and_drops_vec_table() {
    let fixture = init_git_fixture();
    let out = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &out, false).unwrap();

    assert!(
        out.exists(),
        "snapshot db should exist at {}",
        out.display()
    );

    let db = Database::open(&out).unwrap();
    let conn = db.conn();

    // node_vectors must NOT exist when include_vec is false
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_vectors'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_vec, 0, "node_vectors should be dropped");

    // Five producer-side meta keys present and non-empty
    for key in [
        META_SNAPSHOT_SOURCE_COMMIT,
        META_SNAPSHOT_CREATED_AT,
        META_SNAPSHOT_SCHEMA_VERSION,
        META_SNAPSHOT_INCLUDES_VEC,
    ] {
        let v = snap_read_meta(conn, key).unwrap();
        assert!(
            v.is_some() && !v.as_ref().unwrap().is_empty(),
            "meta {key} missing"
        );
    }
    let inc = snap_read_meta(conn, META_SNAPSHOT_INCLUDES_VEC)
        .unwrap()
        .unwrap();
    assert_eq!(inc, "false");
}

#[test]
fn inspect_round_trip() {
    let fixture = init_git_fixture();
    let out_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &out_db, false).unwrap();

    // Compress with zstd to mimic what the workflow produces
    let raw = std::fs::read(&out_db).unwrap();
    let compressed = zstd::encode_all(&raw[..], 9).unwrap();
    let zst_path = fixture.path().join("snapshot.db.zst");
    std::fs::write(&zst_path, &compressed).unwrap();

    let meta = crate::snapshot::inspect(&zst_path).unwrap();
    assert_eq!(meta.tool_version, env!("CARGO_PKG_VERSION"));
    assert!(!meta.includes_vec);
    assert!(meta.created_at > 0);
    assert!(meta.schema_version > 0);
    assert!(meta.file_size_bytes > 0);
}

use crate::snapshot::config::load_config;

#[test]
fn config_load_missing_file_returns_default() {
    let dir = TempDir::new().unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(cfg.snapshot.url, None);
    assert!(!cfg.snapshot.disabled);
}

#[test]
fn config_load_parses_snapshot_url() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[snapshot]\nurl = \"https://example.com/x.db.zst\"\n",
    )
    .unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert_eq!(
        cfg.snapshot.url.as_deref(),
        Some("https://example.com/x.db.zst")
    );
    assert!(!cfg.snapshot.disabled);
}

#[test]
fn config_load_parses_disabled() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[snapshot]\ndisabled = true\n",
    )
    .unwrap();
    let cfg = load_config(dir.path()).unwrap();
    assert!(cfg.snapshot.disabled);
}

#[test]
fn config_load_rejects_malformed_toml() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".code-graph.toml"), "not = valid = toml").unwrap();
    let err = load_config(dir.path()).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("parse")
            || err.to_string().to_lowercase().contains("expected")
            || err.to_string().to_lowercase().contains("invalid"),
        "got error message: {err}"
    );
}

use crate::snapshot::install::{
    gate_origin_url, resolve_snapshot_source, resolve_snapshot_source_impl, try_install,
};
use crate::snapshot::meta::{META_SNAPSHOT_FETCHED_AT, META_SNAPSHOT_SOURCE_URL};

#[test]
fn resolve_returns_none_when_no_git_no_toml() {
    let dir = TempDir::new().unwrap();
    assert_eq!(resolve_snapshot_source(dir.path()), None);
}

// Security: a .code-graph.toml url override is honored ONLY with the out-of-band
// trust signal. Without it, a committed url must NOT redirect the graph source
// (blocks malicious-repo snapshot injection — the audit's top finding).
#[test]
fn resolve_rejects_untrusted_url_override() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[snapshot]\nurl = \"https://example.com/x.db.zst\"\n",
    )
    .unwrap();
    // url_trusted=false (env var absent) → override ignored.
    assert_eq!(resolve_snapshot_source_impl(dir.path(), false, false), None);
}

#[test]
fn resolve_returns_url_from_trusted_override() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[snapshot]\nurl = \"https://example.com/x.db.zst\"\n",
    )
    .unwrap();
    // url_trusted=true (developer set CODE_GRAPH_SNAPSHOT_TRUST_URL=1) → honored.
    // origin_trusted is irrelevant here — the toml url branch returns first.
    assert_eq!(
        resolve_snapshot_source_impl(dir.path(), true, false),
        Some("https://example.com/x.db.zst".to_string()),
    );
}

// Security (#9): the auto-detected origin GitHub-release snapshot is gated by an
// out-of-band trust signal, symmetric with the toml-url override. Opening an
// untrusted repo (cloned to review) must NOT auto-install its published snapshot.
#[test]
fn origin_snapshot_is_gated_behind_trust() {
    let url = Some(
        "https://github.com/o/r/releases/download/v1/code-graph-snapshot-x.db.zst".to_string(),
    );
    // Untrusted (no CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN, no pin) → install skipped.
    assert_eq!(
        gate_origin_url(|| url.clone(), false),
        None,
        "an untrusted repo's published snapshot must not auto-install"
    );
    // Trusted (developer opted in, or a pin is set) → install proceeds.
    assert_eq!(
        gate_origin_url(|| url.clone(), true),
        url,
        "an opt-in / pinned origin snapshot installs"
    );
    // No published snapshot → None even when trusted.
    assert_eq!(gate_origin_url(|| None, true), None);
}

// SEC-07 (audit 2026-08-29): the gate must decide BEFORE the network call, not
// after. The resolver used to be an eagerly-evaluated argument, so `git remote
// get-url origin` + `gh api` ran on every project open no matter what this gate
// then decided — the check read as though it governed the fetch without preventing
// it. Counting resolver calls is the only way to see that: the return value is
// `None` under the old code and the new one alike.
#[test]
fn untrusted_origin_never_reaches_the_network() {
    use std::cell::Cell;
    let resolved = Cell::new(0);
    let out = gate_origin_url(
        || {
            resolved.set(resolved.get() + 1);
            Some("https://github.com/o/r/releases/download/v1/s.db.zst".to_string())
        },
        false,
    );
    assert_eq!(out, None);
    assert_eq!(
        resolved.get(),
        0,
        "untrusted origin must not spawn `git remote get-url` / `gh api` at all"
    );

    // Negative control: the same probe under trust DOES resolve, so the assertion
    // above is about the gate and not about a resolver that never runs.
    let resolved = Cell::new(0);
    let out = gate_origin_url(
        || {
            resolved.set(resolved.get() + 1);
            Some("https://github.com/o/r/releases/download/v1/s.db.zst".to_string())
        },
        true,
    );
    assert!(out.is_some());
    assert_eq!(resolved.get(), 1, "trusted origin resolves exactly once");
}

// SEC-07 part 2: `repo` is the tail of a `splitn(2, '/')`, so without a validator
// it carries embedded `/` and `..` into the `gh api` path segment.
#[test]
fn github_remote_rejects_names_outside_the_github_alphabet() {
    use crate::snapshot::install::parse_github_remote;

    // Well-formed remotes still parse, in both supported spellings.
    assert_eq!(
        parse_github_remote("https://github.com/octo-cat/my_repo.v2.git"),
        Some(("octo-cat".to_string(), "my_repo.v2".to_string()))
    );
    assert_eq!(
        parse_github_remote("git@github.com:octo-cat/my_repo"),
        Some(("octo-cat".to_string(), "my_repo".to_string()))
    );

    for hostile in [
        "https://github.com/o/r/../../../x",
        "https://github.com/o/..",
        "https://github.com/o/r?foo=bar",
        "https://github.com/o/r%2f..",
        "https://github.com/o/-r --flag",
        "https://github.com/o/r/releases",
    ] {
        assert_eq!(
            parse_github_remote(hostile),
            None,
            "remote {hostile:?} must not reach the gh api path"
        );
    }
}

#[test]
fn resolve_returns_none_when_disabled() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[snapshot]\ndisabled = true\n",
    )
    .unwrap();
    assert_eq!(resolve_snapshot_source(dir.path()), None);
}

fn build_local_snapshot(fixture: &TempDir) -> std::path::PathBuf {
    let raw_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &raw_db, false).unwrap();
    let raw = std::fs::read(&raw_db).unwrap();
    let compressed = zstd::encode_all(&raw[..], 9).unwrap();
    let zst_path = fixture.path().join("snapshot.db.zst");
    std::fs::write(&zst_path, &compressed).unwrap();
    zst_path
}

#[test]
fn install_round_trip_file_url() {
    let fixture = init_git_fixture();
    let zst = build_local_snapshot(&fixture);

    // Wipe .code-graph/ so install is the only path that creates it
    let target_root = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(target_root.path())
        .status()
        .unwrap();

    let url = format!("file://{}", zst.display());
    let commit = try_install(&url, target_root.path()).unwrap();
    assert!(!commit.is_empty(), "expected non-empty source commit");

    let installed = target_root.path().join(".code-graph").join("index.db");
    assert!(
        installed.exists(),
        "expected installed at {}",
        installed.display()
    );

    let db = crate::storage::db::Database::open(&installed).unwrap();
    let conn = db.conn();
    let url_meta = read_meta(conn, META_SNAPSHOT_SOURCE_URL).unwrap();
    assert_eq!(url_meta.as_deref(), Some(url.as_str()));
    let fetched = read_meta(conn, META_SNAPSHOT_FETCHED_AT).unwrap();
    assert!(fetched.is_some(), "fetched_at should be written");

    // No leftover .partial files
    let entries: Vec<_> = std::fs::read_dir(target_root.path().join(".code-graph"))
        .unwrap()
        .flatten()
        .collect();
    for entry in &entries {
        let n = entry.file_name();
        let s = n.to_string_lossy();
        assert!(!s.ends_with(".partial"), "leftover partial: {s}");
    }
}

#[test]
fn install_rejects_http_url() {
    let target_root = TempDir::new().unwrap();
    let err = try_install("http://example.com/x.db.zst", target_root.path()).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("https"),
        "got: {err}"
    );
}

#[test]
fn install_rejects_corrupt_archive() {
    let target_root = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(target_root.path())
        .status()
        .unwrap();

    let bad = TempDir::new().unwrap();
    let bad_path = bad.path().join("bad.db.zst");
    std::fs::write(&bad_path, b"not zstd data").unwrap();
    let url = format!("file://{}", bad_path.display());

    let err = try_install(&url, target_root.path()).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("zstd")
            || err.to_string().to_lowercase().contains("decode"),
        "got: {err}"
    );

    // Clean state — no index.db, no .partial
    let cg_dir = target_root.path().join(".code-graph");
    if cg_dir.exists() {
        for entry in std::fs::read_dir(&cg_dir).unwrap().flatten() {
            let s = entry.file_name().to_string_lossy().into_owned();
            assert!(
                s != "index.db" && !s.ends_with(".partial"),
                "leftover after failure: {s}"
            );
        }
    }
}

#[test]
fn resolve_rejects_http_url_from_toml() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[snapshot]\nurl = \"http://example.com/x.db.zst\"\n",
    )
    .unwrap();
    assert_eq!(resolve_snapshot_source(dir.path()), None);
}

#[test]
fn inspect_accepts_raw_db_when_no_zstd_magic() {
    // First-time users often run `snapshot create --out foo.db` then
    // `snapshot inspect foo.db` (forgot the zstd step). Cryptic
    // "Unknown frame descriptor" used to greet them; now it just works.
    let fixture = init_git_fixture();
    let raw_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &raw_db, false).unwrap();
    assert!(raw_db.exists());

    let meta = crate::snapshot::inspect(&raw_db).unwrap();
    assert_eq!(meta.tool_version, env!("CARGO_PKG_VERSION"));
    assert!(meta.schema_version > 0);
    assert!(meta.created_at > 0);
    assert!(!meta.includes_vec);
}

#[test]
fn inspect_rejects_truncated_sqlite_header() {
    // Regression: a file just long enough to pass the SQLite magic check
    // ("SQLite format 3\0" + a few extra bytes) used to slip through the
    // header gate. Database::open would create empty schema, every meta
    // lookup would return None → defaults, and inspect returned a fake
    // "valid empty snapshot" with zeroed fields. Real snapshots always
    // carry meta — bail when it's missing.
    let dir = TempDir::new().unwrap();
    let bad = dir.path().join("truncated.db");
    // 100 bytes starting with the SQLite header magic + zeros
    let mut buf = b"SQLite format 3\0".to_vec();
    buf.resize(100, 0);
    std::fs::write(&bad, &buf).unwrap();
    let err = crate::snapshot::inspect(&bad).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not a valid code-graph snapshot") || msg.contains("meta is missing"),
        "expected truncated-db rejection, got: {msg}"
    );
}

#[test]
fn inspect_rejects_garbage_with_clear_error() {
    let dir = TempDir::new().unwrap();
    let bad = dir.path().join("garbage.db.zst");
    std::fs::write(&bad, b"definitely not zstd or sqlite").unwrap();
    let err = crate::snapshot::inspect(&bad).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not a code-graph snapshot") && msg.contains(".db.zst") && msg.contains(".db"),
        "expected helpful error, got: {msg}"
    );
}

#[test]
fn create_auto_compresses_when_out_endswith_db_zst() {
    // Help text promises `--out foo.db.zst` produces a shareable .db.zst.
    // Verify we actually zstd-encode rather than silently writing raw SQLite.
    let fixture = init_git_fixture();
    let zst_out = fixture.path().join("snap.db.zst");
    crate::snapshot::create(fixture.path(), &zst_out, false).unwrap();

    // First 4 bytes must be the zstd magic.
    let head: Vec<u8> = std::fs::read(&zst_out)
        .unwrap()
        .into_iter()
        .take(4)
        .collect();
    assert_eq!(
        head,
        vec![0x28, 0xB5, 0x2F, 0xFD],
        "expected zstd magic, got {head:?}"
    );

    // Round-trip through inspect on the .db.zst itself.
    let meta = crate::snapshot::inspect(&zst_out).unwrap();
    assert_eq!(meta.tool_version, env!("CARGO_PKG_VERSION"));
    assert!(meta.schema_version > 0);
}

#[test]
fn create_writes_raw_db_when_out_endswith_db() {
    // Producer workflow uses `--out snapshot.db` then a separate `zstd -9`
    // step — this path must remain raw to keep that contract.
    let fixture = init_git_fixture();
    let db_out = fixture.path().join("snap.db");
    crate::snapshot::create(fixture.path(), &db_out, false).unwrap();

    let head: Vec<u8> = std::fs::read(&db_out)
        .unwrap()
        .into_iter()
        .take(16)
        .collect();
    assert_eq!(
        &head[..],
        b"SQLite format 3\0",
        "expected raw SQLite header"
    );
}

// Integrity: compressed-output snapshots get a `.blake3` sidecar the consumer
// verifies before decompressing. Upload BOTH files to the GitHub release.
#[test]
fn create_writes_blake3_sidecar_for_compressed_output() {
    let fixture = init_git_fixture();
    let zst_out = fixture.path().join("snap.db.zst");
    crate::snapshot::create(fixture.path(), &zst_out, false).unwrap();

    let sidecar = fixture.path().join("snap.db.zst.blake3");
    assert!(
        sidecar.exists(),
        "producer must write a .blake3 sidecar next to the .db.zst"
    );
    let recorded = std::fs::read_to_string(&sidecar).unwrap();
    let actual = blake3::hash(&std::fs::read(&zst_out).unwrap())
        .to_hex()
        .to_string();
    assert_eq!(
        recorded.trim(),
        actual,
        "sidecar must hold the blake3 of the compressed artifact"
    );
}

#[test]
fn create_no_sidecar_for_raw_db_output() {
    // Raw .db output is not the consumer-facing artifact (the workflow compresses
    // + checksums in a later step), so no sidecar.
    let fixture = init_git_fixture();
    let db_out = fixture.path().join("snap.db");
    crate::snapshot::create(fixture.path(), &db_out, false).unwrap();
    assert!(
        !fixture.path().join("snap.db.blake3").exists(),
        "raw .db output must not get a .blake3 sidecar"
    );
}

/// Leave a VALID, unmerged `-wal` next to `db` — the residue a partial cleanup
/// (killed installer, crashed server) leaves behind. Returns the marker value
/// written into that WAL and never checkpointed into the main file.
fn strand_a_wal_beside(db: &std::path::Path) -> String {
    let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
    let saved = db.with_extension("saved-wal");
    {
        let conn = Connection::open(db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);",
        )
        .unwrap();
        // Enough pages that a replay is unmistakable in the destination file.
        for i in 0..200 {
            conn.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                rusqlite::params![format!("stale_{i}"), "x".repeat(200)],
            )
            .unwrap();
        }
        assert!(
            wal.exists(),
            "precondition: writes must still be in the WAL"
        );
        std::fs::copy(&wal, &saved).unwrap();
    } // close checkpoints and removes the -wal
    std::fs::copy(&saved, &wal).unwrap();
    std::fs::remove_file(&saved).unwrap();
    "stale_0".to_string()
}

#[test]
fn install_clears_a_stranded_destination_wal() {
    // Audit 2026-08-16 P1-1. `try_install` renamed its finished partial over
    // `index.db` while removing only ITS OWN sidecars, so a `-wal` stranded next
    // to the destination survived the rename and SQLite replayed those pages into
    // the brand-new file on the next open — silently reverting the snapshot to
    // whatever the previous database contained, `integrity_check` still ok. Two
    // of the three callers had grown their own pre-rename cleanup; the MCP server
    // path (`McpServer::from_project_root` → `maybe_install_snapshot`) had not,
    // which is what makes the callee the only place the guard belongs.
    let fixture = init_git_fixture();
    let zst = build_local_snapshot(&fixture);

    let target_root = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(target_root.path())
        .status()
        .unwrap();
    let cg = target_root.path().join(".code-graph");
    std::fs::create_dir_all(&cg).unwrap();
    let dest = cg.join("index.db");
    let stale_key = strand_a_wal_beside(&dest);
    let wal = cg.join("index.db-wal");
    assert!(
        wal.exists(),
        "precondition: the destination must carry a stranded WAL before install"
    );

    let url = format!("file://{}", zst.display());
    try_install(&url, target_root.path()).unwrap();

    let db = crate::storage::db::Database::open(&dest).unwrap();
    let conn = db.conn();
    assert!(
        read_meta(conn, crate::snapshot::meta::META_SNAPSHOT_SOURCE_COMMIT)
            .unwrap()
            .is_some(),
        "the installed snapshot's own meta must survive"
    );
    assert_eq!(
        read_meta(conn, &stale_key).unwrap(),
        None,
        "a page from the pre-install database was replayed over the installed snapshot"
    );
    drop(db);
    assert!(
        !wal.exists(),
        "install left the destination's stale -wal in place, to be replayed over the new DB"
    );
    assert!(
        !cg.join("index.db-shm").exists(),
        "install left the destination's stale -shm in place"
    );
}
