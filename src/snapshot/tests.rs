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

/// D#611: `try_install` accepted streams `snapshot inspect` refuses, and
/// replaced the user's index with an EMPTY one. An empty zstd frame decodes to
/// 0 bytes, the indexer open bootstraps a fresh schema over them, and
/// `validate` only refused a NEWER schema, so the empty database passed and was
/// renamed over `index.db`. A stream that starts with a skippable frame, a
/// compressed SQLite file with no snapshot meta (a live index) and a
/// compressed non-database went the same way. Each row is first shown refused
/// by `inspect` (the parity this asserts), then by `try_install`, with the
/// existing index byte-identical afterwards.
#[test]
fn install_refuses_every_stream_inspect_refuses() {
    let fixture = init_git_fixture();
    let real = build_local_snapshot(&fixture);
    let real_bytes = std::fs::read(&real).unwrap();
    // A plain index, not a snapshot: has `meta`, holds no snapshot_* rows.
    let live_dir = TempDir::new().unwrap();
    let live = live_dir.path().join("index.db");
    Database::open(&live).unwrap();
    let live_bytes = std::fs::read(&live).unwrap();
    // A skippable frame (magic 0x184D2A50, little-endian), 4 payload bytes.
    let skippable: Vec<u8> = [0x50, 0x2A, 0x4D, 0x18, 4, 0, 0, 0, 1, 2, 3, 4].to_vec();
    let empty_frame = zstd::encode_all(&[][..], 1).unwrap();
    let shapes: Vec<(&str, Vec<u8>)> = vec![
        ("empty frame", empty_frame.clone()),
        (
            "skippable + empty frame",
            [skippable.clone(), empty_frame].concat(),
        ),
        (
            "skippable + real snapshot",
            [skippable, real_bytes].concat(),
        ),
        (
            "live index, no snapshot meta",
            zstd::encode_all(&live_bytes[..], 1).unwrap(),
        ),
        (
            "not a database",
            zstd::encode_all(&[7u8; 4096][..], 1).unwrap(),
        ),
    ];

    let target_root = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(target_root.path())
        .status()
        .unwrap();
    let url = format!("file://{}", real.display());
    try_install(&url, target_root.path()).unwrap();
    let installed = target_root.path().join(".code-graph").join("index.db");
    let before = std::fs::read(&installed).unwrap();

    let streams = TempDir::new().unwrap();
    for (name, bytes) in shapes {
        let path = streams.path().join("x.db.zst");
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            crate::snapshot::inspect(&path).is_err(),
            "{name}: precondition — inspect refuses it"
        );
        let err = try_install(&format!("file://{}", path.display()), target_root.path())
            .err()
            .unwrap_or_else(|| panic!("{name}: install must refuse what inspect refuses"));
        assert!(
            err.to_string().contains("not a")
                || format!("{err:#}").contains("not a valid code-graph snapshot"),
            "{name}: {err:#}"
        );
        assert!(
            std::fs::read(&installed).unwrap() == before,
            "{name}: the installed index was replaced"
        );
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

/// 0.180.0 pre-tag review M-1: `open_readonly` refuses a symlinked database
/// (D#363, aimed at a planted `.code-graph/index.db`), and inspect's in-place
/// arm opened the user's own file through it, so `snapshot inspect link.db`
/// with `link.db -> snapshot.db` was refused where 0.179.4 printed the meta. A
/// path the user names is theirs to follow; the zstd arm already did.
#[cfg(unix)]
#[test]
fn inspect_follows_a_symlink_the_user_names() {
    let fixture = init_git_fixture();
    let raw_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &raw_db, false).unwrap();
    let bytes = std::fs::read(&raw_db).unwrap();
    let link = fixture.path().join("link.db");
    std::os::unix::fs::symlink(&raw_db, &link).unwrap();
    let direct = crate::snapshot::inspect(&raw_db).unwrap();
    let through = crate::snapshot::inspect(&link).unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(through.source_commit, direct.source_commit);
    assert_eq!(through.node_count, direct.node_count);
    assert_eq!(
        std::fs::read(&raw_db).unwrap(),
        bytes,
        "inspect must not write"
    );
}

/// 0.180.0 pre-tag review R2: the first M-1 repair staged a linked input and
/// opened the copy writable, so a read-only target (Nix store, Bazel outputs)
/// failed with "attempt to write a readonly database", and an index one
/// INDEX_VERSION behind had its counts zeroed by the copy's version sweep.
/// 0.179.4 read both through the link. So does this.
#[cfg(unix)]
#[test]
fn inspect_reads_through_a_link_as_it_reads_the_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = init_git_fixture();
    let raw_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &raw_db, false).unwrap();
    let older = fixture.path().join("older.db");
    std::fs::copy(&raw_db, &older).unwrap();
    {
        let c = Connection::open(&older).unwrap();
        c.pragma_update(None, "application_id", crate::domain::INDEX_VERSION - 1)
            .unwrap();
    }
    for (name, file) in [("read-only", &raw_db), ("older INDEX_VERSION", &older)] {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444)).unwrap();
        let link = fixture.path().join(format!("link-{}.db", name.len()));
        std::os::unix::fs::symlink(file, &link).unwrap();
        let direct = crate::snapshot::inspect(file).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert!(direct.node_count > 0, "{name}: fixture has nodes");
        let through = crate::snapshot::inspect(&link).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(through.node_count, direct.node_count, "{name}");
        assert_eq!(through.edge_count, direct.edge_count, "{name}");
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

/// D#629 (0.180.0 pre-tag review R2): the arms that read a staged COPY (the
/// `.db.zst` arm, the WAL-mode raw arm and the read-only fallback) opened it
/// with the indexer's `Database::open`, whose INDEX_VERSION sweep deleted the
/// copy's nodes and edges. A snapshot one INDEX_VERSION behind printed
/// node_count 0 and `clearing + rebuilding`, while the in-place arm printed
/// the real counts for the same bytes. One version ahead kept its counts but
/// told the user to delete `.code-graph/index.db*`; it is here so that
/// direction stays covered too.
#[test]
fn inspect_counts_another_index_versions_snapshot_alike_on_every_arm() {
    let fixture = init_git_fixture();
    let raw_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &raw_db, false).unwrap();
    let current = crate::snapshot::inspect(&raw_db).unwrap();
    assert!(
        current.node_count > 0 && current.edge_count > 0,
        "fixture must carry nodes and edges, or a sweep has nothing to delete"
    );

    for stored in [
        crate::domain::INDEX_VERSION - 1,
        crate::domain::INDEX_VERSION + 1,
    ] {
        let dir = TempDir::new().unwrap();
        let in_place = dir.path().join("delete.db");
        std::fs::copy(&raw_db, &in_place).unwrap();
        Connection::open(&in_place)
            .unwrap()
            .pragma_update(None, "application_id", stored)
            .unwrap();
        let wal = dir.path().join("wal.db");
        std::fs::copy(&in_place, &wal).unwrap();
        {
            let c = Connection::open(&wal).unwrap();
            let mode: String = c
                .query_row("PRAGMA journal_mode = wal", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
        }
        let zst = dir.path().join("snapshot.db.zst");
        let raw = std::fs::read(&in_place).unwrap();
        std::fs::write(&zst, zstd::encode_all(&raw[..], 3).unwrap()).unwrap();
        // A journal SQLite calls hot is what makes the in-place read-only open
        // fail and fall back to a staged copy, the third place a copy is
        // opened. One whose first header rolls nothing back, so the copy —
        // which does not take the journal (`stage_copy`) — holds the same
        // snapshot the other three arms read.
        let fallback = dir.path().join("fallback.db");
        std::fs::copy(&in_place, &fallback).unwrap();
        std::fs::write(
            format!("{}-journal", fallback.display()),
            hot_looking_journal(None),
        )
        .unwrap();
        let read_only_in_place =
            Connection::open_with_flags(&fallback, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .and_then(|c| {
                    c.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                });
        assert!(
            read_only_in_place.is_err(),
            "precondition: this arm needs an in-place read-only open that fails, got \
             {read_only_in_place:?}"
        );

        for (arm, file, write_version) in [
            ("in place", &in_place, Some(1u8)),
            ("WAL staging", &wal, Some(2)),
            ("zstd", &zst, None),
            ("read-only fallback", &fallback, Some(1)),
        ] {
            if let Some(v) = write_version {
                let head = std::fs::read(file).unwrap();
                assert_eq!(head[18], v, "v{stored} {arm}: fixture takes another arm");
                let app_id = i32::from_be_bytes(head[68..72].try_into().unwrap());
                assert_eq!(app_id, stored, "v{stored} {arm}: fixture's INDEX_VERSION");
            }
            let meta =
                crate::snapshot::inspect(file).unwrap_or_else(|e| panic!("v{stored} {arm}: {e:#}"));
            assert_eq!(
                (meta.node_count, meta.edge_count),
                (current.node_count, current.edge_count),
                "v{stored} {arm}: counts differ from the same bytes read at the current version"
            );
        }
    }
}

/// 0.180.0 pre-tag review R2: the planted-file refusal (D#363) reached
/// `snapshot inspect`, so a regular snapshot with a linked or directory
/// `<file>-journal` / `-wal` beside it was refused; 0.179.4 read it. The
/// refusal is for `.code-graph/index.db`, not for a file the user names.
#[cfg(unix)]
#[test]
fn inspect_reads_a_named_file_whatever_sits_beside_it() {
    let fixture = init_git_fixture();
    let raw_db = fixture.path().join("snapshot.db");
    crate::snapshot::create(fixture.path(), &raw_db, false).unwrap();
    let victim = fixture.path().join("victim.txt");
    std::fs::write(&victim, "keep\n").unwrap();
    for suffix in ["-journal", "-wal"] {
        let side = fixture.path().join(format!("snapshot.db{suffix}"));
        std::os::unix::fs::symlink(&victim, &side).unwrap();
        let meta = crate::snapshot::inspect(&raw_db).unwrap_or_else(|e| panic!("{suffix}: {e:#}"));
        assert!(meta.node_count > 0, "{suffix}");
        std::fs::remove_file(&side).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep\n");
}

/// 0.180.0 pre-tag review L-6: no test pinned a condition of the shared
/// verdict. `&&` for `||` would refuse every snapshot made outside a git
/// repository (its `source_commit` is empty) and still pass the install tests.
#[test]
fn snapshot_meta_verdict_needs_any_one_of_its_three_fields() {
    use crate::snapshot::is_snapshot_meta;
    assert!(!is_snapshot_meta(0, "", ""), "nothing: not a snapshot");
    assert!(
        is_snapshot_meta(10, "", ""),
        "schema only (made outside git)"
    );
    assert!(is_snapshot_meta(0, "abc", ""), "commit only");
    assert!(is_snapshot_meta(0, "", "0.1.0"), "tool version only");
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
fn inspect_bounds_the_decompressed_payload() {
    // Audit 2026-09-07 SURF-25. `inspect` is a scripted release-artifact check,
    // so it is pointed at bytes nobody has vouched for yet, and it used to
    // `fs::read` the whole file and `zstd::decode_all` it with no ceiling —
    // while `install.rs` has capped the identical artifact at 100 MB since it
    // was written. A 4 KiB cap against a 5 KiB payload proves the ceiling is
    // enforced without a 100 MB fixture, and the numbers are explicit rather
    // than derived from MAX_DECOMPRESSED_BYTES, so raising that constant cannot
    // quietly make this test vacuous.
    let dir = TempDir::new().unwrap();
    let zst_path = dir.path().join("bomb.db.zst");
    let payload = vec![0u8; 5 * 1024];
    std::fs::write(&zst_path, zstd::encode_all(&payload[..], 1).unwrap()).unwrap();

    let err = crate::snapshot::inspect_with_cap(&zst_path, 4 * 1024).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("cap"),
        "expected the decompression cap to refuse this, got: {msg}"
    );

    // The raw-SQLite arm is deliberately NOT capped, and this assertion is the
    // second version: the first required the cap here too, which refuses this
    // repository's own 163 MB `.code-graph/index.db` and breaks
    // `create --out x.db` -> `inspect`. SURF-25 is about decompression
    // amplification; a raw `.db` has none (this arm reads the file in place or
    // copies it byte for byte), and
    // `try_install` only ever consumes `.db.zst`, so there is no install-side
    // ceiling for it to match. A file far over the cap must get PAST the
    // ceiling and fail later on its content, if at all.
    let raw_path = dir.path().join("big.db");
    let mut raw = b"SQLite format 3\0".to_vec();
    raw.resize(5 * 1024, 0);
    std::fs::write(&raw_path, &raw).unwrap();
    let err = crate::snapshot::inspect_with_cap(&raw_path, 1024).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("cap"),
        "the raw arm must not be refused by the decompression ceiling — a raw file's size is \
         what the caller already has on disk, and this arm never holds it in memory: {msg}"
    );
    assert!(
        msg.contains("not a valid code-graph snapshot") || msg.contains("meta is missing"),
        "the oversize raw file must reach the content check, not stop at a size gate: {msg}"
    );
}

/// A raw `.db` that `inspect` accepts, in a chosen journal mode.
/// `Database::open` leaves a file in WAL; `delete` is what `snapshot create`'s
/// `VACUUM INTO` actually emits. The header assertion is not decoration — a
/// fixture that silently came out in the other mode would make every caller
/// below test the arm it was not aiming at.
fn raw_snapshot_fixture(path: &std::path::Path, journal_mode: &str, write_version: u8) {
    {
        let db = Database::open(path).unwrap();
        db.conn()
            .query_row(&format!("PRAGMA journal_mode = {journal_mode}"), [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        write_meta(db.conn(), META_SNAPSHOT_SOURCE_COMMIT, "deadbeefcafe").unwrap();
        write_meta(db.conn(), META_SNAPSHOT_TOOL_VERSION, "9.9.9").unwrap();
        write_meta(db.conn(), META_SNAPSHOT_SCHEMA_VERSION, "10").unwrap();
        write_meta(db.conn(), META_SNAPSHOT_CREATED_AT, "1757600000").unwrap();
    }
    let head = std::fs::read(path).unwrap();
    assert_eq!(
        head[18], write_version,
        "fixture asked for journal_mode={journal_mode} but the header says otherwise"
    );
    let own_name = path.file_name().unwrap().to_string_lossy().into_owned();
    let stray: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| *n != own_name)
        .collect();
    assert!(
        stray.is_empty(),
        "fixture left side files behind, so a residue assertion on it would be vacuous: {stray:?}"
    );
}

#[test]
fn inspect_leaves_no_side_files_beside_the_file_it_reads() {
    // Regression caught while making the raw arm open in place instead of
    // staging through `fs::copy`: SQLite cannot read a WAL-mode database
    // without a `-shm`, so an in-place read-only open MINTS `snapshot.db-wal`
    // and `snapshot.db-shm` next to the user's file, and a read-only connection
    // cannot remove them on close. A command whose entire job is to look at a
    // file left two behind.
    //
    // Both modes run: `delete` is every artifact `snapshot create` produces and
    // takes the in-place arm, `wal` is what a live `.code-graph/index.db` is and
    // takes the staging arm. The read-only-directory sibling below is what stops
    // a "clean up afterwards" fix from satisfying this one.
    for (mode, write_version) in [("delete", 1u8), ("wal", 2u8)] {
        let dir = TempDir::new().unwrap();
        let raw = dir.path().join("snapshot.db");
        raw_snapshot_fixture(&raw, mode, write_version);

        let before = std::fs::read(&raw).unwrap();
        crate::snapshot::inspect(&raw).unwrap();
        let after = std::fs::read(&raw).unwrap();

        assert_eq!(
            before, after,
            "{mode}: inspect modified the file it was only pointed at"
        );
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "snapshot.db")
            .collect();
        assert!(
            residue.is_empty(),
            "{mode}: inspect left files beside the snapshot: {residue:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn inspect_reads_a_wal_mode_raw_db_from_a_read_only_directory() {
    // The WAL arm's reason to keep staging, stated as a failure rather than as
    // residue: an in-place open has to create a `-shm` beside the file, and a
    // read-only directory refuses that outright. The residue sibling above runs
    // in a writable directory and could still be satisfied by deleting the side
    // files after the fact; this one cannot.
    //
    // Mutation-verified by forcing `wal_mode` false in `inspect_with_cap`, and
    // the verdict is worth recording because it is not the one to expect: the
    // failure is `not a valid code-graph snapshot — meta is missing or
    // unreadable`, NOT a permission error. `Database::open_readonly` returns Ok
    // on an unreadable file (its `user_version` and `sqlite_master` probes are
    // both `unwrap_or`), so the refusal surfaces at the `meta` lookup, which
    // cannot tell "no shm" from "corrupt". An in-place WAL arm would therefore
    // tell a user their snapshot is corrupt when the directory is merely
    // read-only.
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let raw = dir.path().join("snapshot.db");
    raw_snapshot_fixture(&raw, "wal", 2);

    let original = std::fs::metadata(dir.path()).unwrap().permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = crate::snapshot::inspect(&raw);
    std::fs::set_permissions(dir.path(), original).unwrap();

    let meta = result.expect("a WAL-mode raw db in a read-only directory must still inspect");
    assert_eq!(meta.tool_version, "9.9.9");
    assert_eq!(meta.source_commit, "deadbeefcafe");
}

#[test]
fn inspect_refuses_a_live_index_that_has_meta_but_no_snapshot_rows() {
    // The `schema_version == 0 && source_commit.is_empty() && tool_version.is_empty()`
    // verdict, which until now had no guard at all.
    //
    // Pre-ship review measured it: disabling that verdict alone left the full
    // suite at 1816 passed / 0 failed. The two tests that used to reach it —
    // `inspect_rejects_truncated_sqlite_header` and
    // `inspect_bounds_the_decompressed_payload` — build a magic-plus-zeros
    // fixture, which after the in-place arm landed has no `meta` table at all,
    // so they now stop at the meta probe instead and never get that far. Both
    // checks emit a byte-identical message, so nothing went red when the guard
    // moved out from under them.
    //
    // This fixture reaches the verdict on purpose: a LIVE `.code-graph/index.db`
    // has a full schema — `meta` table included — and simply holds no
    // `snapshot_*` rows. Without the verdict, `inspect` reports it as a valid
    // snapshot with zeroed fields, which is precisely the "fake valid empty
    // snapshot" the truncated-header test's comment says must not happen.
    // `journal_mode = delete` so it takes the in-place arm, where the loss was.
    let dir = TempDir::new().unwrap();
    let live = dir.path().join("index.db");
    {
        let db = Database::open(&live).unwrap();
        db.conn()
            .query_row("PRAGMA journal_mode = delete", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
    }
    let head = std::fs::read(&live).unwrap();
    assert_eq!(head[18], 1, "fixture must take the in-place arm");
    let has_meta: i64 = rusqlite::Connection::open(&live)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        has_meta, 1,
        "fixture must HAVE a meta table, or it stops at the meta probe and proves nothing"
    );

    let err = crate::snapshot::inspect(&live).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not a valid code-graph snapshot"),
        "an index with an empty meta table is not a snapshot and must be refused: {msg}"
    );
}

/// Make `db` a copy of `committed`, a non-WAL snapshot, taken in the middle of
/// a write transaction, plus its hot `-journal`: what a writer leaves when it
/// dies mid-transaction. The transaction rewrites the tool version to `torn`,
/// then outgrows a 10-page cache writing another table, so SQLite spills the
/// page it dirtied first, the one holding `torn`, into the main file. Without
/// the journal those bytes read as the unfinished transaction (or as a torn
/// b-tree); with it SQLite rolls back to the last commit. The spill order is
/// SQLite's to choose, so the precondition below reads the main file alone
/// rather than trusting it. Returns the journal's path.
fn hot_journal_pair(committed: &std::path::Path, db: &std::path::Path) -> std::path::PathBuf {
    let src_dir = TempDir::new().unwrap();
    let src = src_dir.path().join("src.db");
    std::fs::copy(committed, &src).unwrap();
    let src_journal = src_dir.path().join("src.db-journal");
    let journal = std::path::PathBuf::from(format!("{}-journal", db.display()));
    let tool_version = |c: &Connection| -> rusqlite::Result<String> {
        c.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [META_SNAPSHOT_TOOL_VERSION],
            |r| r.get(0),
        )
    };

    let conn = Connection::open(&src).unwrap();
    let last_commit = tool_version(&conn).unwrap();
    conn.execute_batch("CREATE TABLE pad(v TEXT); PRAGMA cache_size = 10; BEGIN IMMEDIATE;")
        .unwrap();
    conn.execute(
        "UPDATE meta SET value = 'torn' WHERE key = ?1",
        [META_SNAPSHOT_TOOL_VERSION],
    )
    .unwrap();
    for _ in 0..400 {
        conn.execute("INSERT INTO pad(v) VALUES (?1)", ["x".repeat(400)])
            .unwrap();
    }
    assert!(
        std::fs::metadata(&src_journal).unwrap().len() > 0,
        "precondition: the open transaction has a non-empty rollback journal"
    );
    std::fs::copy(&src, db).unwrap();
    std::fs::copy(&src_journal, &journal).unwrap();
    conn.execute_batch("ROLLBACK;").unwrap();

    let alone = src_dir.path().join("alone.db");
    std::fs::copy(db, &alone).unwrap();
    let read_alone = Connection::open(&alone).and_then(|c| tool_version(&c));
    assert!(
        !matches!(&read_alone, Ok(v) if *v == last_commit),
        "precondition: without its journal the main file must not read as the last \
         commit, or the journal is not needed and this fixture proves nothing ({read_alone:?})"
    );
    journal
}

/// A journal SQLite calls hot — it starts with the journal magic, so
/// `hasHotJournal` says yes — whose first header rolls nothing back: the zeros
/// after the magic fail `readJournalHdr`'s page- and sector-size checks, which
/// returns SQLITE_DONE. With `names` set it also carries the trailer SQLite
/// reads a SUPER-JOURNAL path out of (`readSuperJournal`): the name, then its
/// length and the sum of its bytes as big-endian u32s, then the magic again, in
/// the last `16 + len` bytes of the file.
fn hot_looking_journal(names: Option<&std::path::Path>) -> Vec<u8> {
    const MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    let mut journal = MAGIC.to_vec();
    journal.resize(512, 0);
    if let Some(path) = names {
        let name = path.to_str().expect("test path is UTF-8").as_bytes();
        let cksum = name
            .iter()
            .fold(0u32, |sum, b| sum.wrapping_add(u32::from(*b)));
        journal.extend_from_slice(name);
        journal.extend_from_slice(&(name.len() as u32).to_be_bytes());
        journal.extend_from_slice(&cksum.to_be_bytes());
        journal.extend_from_slice(&MAGIC);
    }
    journal
}

/// Everything in `dir`, by name and bytes, to compare before against after.
fn files_in(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let bytes = std::fs::read(e.path()).unwrap_or_default();
            (e.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect();
    all.sort();
    all
}

/// What `inspect` says about `file`, with the path taken out of an error so
/// that two copies of the same bytes in different directories compare equal.
fn inspect_outcome(file: &std::path::Path) -> Result<String, String> {
    crate::snapshot::inspect(file)
        .map(|m| m.tool_version)
        .map_err(|e| format!("{e:#}").replace(&file.display().to_string(), "<file>"))
}

/// D#630 (0.180.0 pre-tag review R2) staged the `-journal` with the file so
/// that SQLite would roll it back, and 0.181.0's R3 WITHDREW it (D#646):
/// rolling a journal back also deletes the file a path inside the journal names
/// (`stage_copy`, and the test below). So a hot pair reads as the main file
/// alone reads — the unfinished transaction's values, or a malformed-image
/// refusal — which is what 0.180.0 and earlier did, where `sqlite3` reads the
/// last commit. What is pinned here is that the journal beside the file changes
/// NOTHING about the answer, and that the pair is left as it was. Three ways
/// in: the read-only fallback (non-WAL), the WAL arm (a crash while switching a
/// file to WAL leaves the header saying WAL beside a hot journal), and a link,
/// whose journal SQLite looks for beside the TARGET.
#[test]
fn inspect_reads_a_hot_journal_pair_as_the_main_file_alone() {
    let committed_dir = TempDir::new().unwrap();
    let committed = committed_dir.path().join("committed.db");
    raw_snapshot_fixture(&committed, "delete", 1);
    let dir = TempDir::new().unwrap();
    let hot = dir.path().join("snapshot.db");
    let journal = hot_journal_pair(&committed, &hot);
    let flipped_dir = TempDir::new().unwrap();
    let flipped = flipped_dir.path().join("snapshot.db");
    let mut header_says_wal = std::fs::read(&hot).unwrap();
    header_says_wal[18] = 2;
    header_says_wal[19] = 2;
    std::fs::write(&flipped, &header_says_wal).unwrap();
    std::fs::copy(&journal, flipped_dir.path().join("snapshot.db-journal")).unwrap();

    // The same main file with no journal beside it: what `inspect` must answer
    // for the pair too.
    let alone = |bytes: &[u8]| {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("snapshot.db");
        std::fs::write(&file, bytes).unwrap();
        let outcome = inspect_outcome(&file);
        (dir, outcome)
    };
    let (_hot_alone_dir, hot_alone) = alone(&std::fs::read(&hot).unwrap());
    let (_flipped_alone_dir, flipped_alone) = alone(&header_says_wal);
    assert_ne!(
        hot_alone.as_deref(),
        Ok("9.9.9"),
        "precondition: the main file alone must not read as the last commit, or this \
         fixture proves nothing"
    );

    #[cfg(unix)]
    let link_dir = TempDir::new().unwrap();
    #[cfg(unix)]
    let link = {
        let link = link_dir.path().join("link.db");
        std::os::unix::fs::symlink(&hot, &link).unwrap();
        link
    };
    let before = (files_in(dir.path()), files_in(flipped_dir.path()));
    assert_eq!(inspect_outcome(&hot), hot_alone, "non-WAL");
    assert_eq!(inspect_outcome(&flipped), flipped_alone, "header says WAL");
    #[cfg(unix)]
    assert_eq!(
        inspect_outcome(&link).map_err(|e| e.replace(&hot.display().to_string(), "<file>")),
        hot_alone,
        "link"
    );
    assert_eq!(
        (files_in(dir.path()), files_in(flipped_dir.path())),
        before,
        "inspect rolled back, or otherwise touched, the files it was pointed at"
    );
}

/// 0444, on each arm. The staged copy is a new file rather than an `fs::copy`,
/// which carries the source's mode over, and the reader open writes the copy:
/// a 0444 snapshot failed with "attempt to write a readonly database". The hot
/// pairs are here too — the journal is not copied any more, so the read-only
/// fallback has nothing to roll back, and the answer must be the 0644 answer.
#[cfg(unix)]
#[test]
fn inspect_reads_a_read_only_snapshot_on_every_arm() {
    use std::os::unix::fs::PermissionsExt;

    let committed_dir = TempDir::new().unwrap();
    let committed = committed_dir.path().join("committed.db");
    raw_snapshot_fixture(&committed, "delete", 1);

    // Shapes that read as a clean snapshot whatever their mode: the three arms
    // that stage a copy, and the in-place arm.
    for (arm, mode, write_version) in [
        ("WAL staging", "wal", 2u8),
        ("in place", "delete", 1u8),
        ("compressed", "delete", 1u8),
    ] {
        let dir = TempDir::new().unwrap();
        let raw = dir.path().join("snapshot.db");
        raw_snapshot_fixture(&raw, mode, write_version);
        let file = if arm == "compressed" {
            let zst = dir.path().join("snapshot.db.zst");
            std::fs::write(
                &zst,
                zstd::encode_all(&std::fs::read(&raw).unwrap()[..], 1).unwrap(),
            )
            .unwrap();
            zst
        } else {
            raw.clone()
        };
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        let got = inspect_outcome(&file);
        assert_eq!(got.as_deref(), Ok("9.9.9"), "{arm} at 0444");
    }

    // A hot pair at 0444 answers what the same pair answers at 0644.
    for (name, write_version) in [("non-WAL", 1u8), ("header says WAL", 2u8)] {
        let mut outcomes = Vec::new();
        let mut kept = Vec::new();
        for mode in [0o644, 0o444] {
            let dir = TempDir::new().unwrap();
            let hot = dir.path().join("snapshot.db");
            let journal = hot_journal_pair(&committed, &hot);
            if write_version == 2 {
                let mut bytes = std::fs::read(&hot).unwrap();
                bytes[18] = 2;
                bytes[19] = 2;
                std::fs::write(&hot, &bytes).unwrap();
            }
            for f in [&hot, &journal] {
                std::fs::set_permissions(f, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            let before = files_in(dir.path());
            outcomes.push(inspect_outcome(&hot));
            assert_eq!(
                files_in(dir.path()),
                before,
                "{name} at {mode:o}: inspect changed a file it was pointed at"
            );
            kept.push(dir);
        }
        assert_eq!(
            outcomes[1], outcomes[0],
            "{name}: 0444 answered differently from 0644"
        );
        drop(kept);
    }
}

/// Whatever sits at `<file>-journal`, `inspect` reads the snapshot and leaves
/// that file alone: the copy takes the main file only (`stage_copy`), and a
/// directory or a FIFO there must not turn into a copy error or a copy that
/// blocks forever. The non-WAL arm blocked on the FIFO without any
/// copy (D#636): opening in place, SQLite's hot-journal probe opens
/// `<file>-journal`, and opening a FIFO waits for a writer; 0.180.0 was killed
/// by `timeout 10` (rc=124). Through a link too: SQLite opens the journal beside
/// the link's target. In a thread with a deadline, so that a FIFO that slips
/// through fails the test instead of hanging the suite.
#[cfg(unix)]
#[test]
fn inspect_stages_a_journal_only_when_it_is_a_regular_file() {
    for (mode, write_version) in [("delete", 1u8), ("wal", 2u8)] {
        for shape in ["directory", "FIFO"] {
            for via_link in [false, true] {
                let dir = TempDir::new().unwrap();
                let raw = dir.path().join("snapshot.db");
                raw_snapshot_fixture(&raw, mode, write_version);
                let side = dir.path().join("snapshot.db-journal");
                if shape == "directory" {
                    std::fs::create_dir(&side).unwrap();
                } else {
                    let made = Command::new("mkfifo").arg(&side).status().unwrap();
                    assert!(made.success(), "mkfifo {}", side.display());
                }
                let link_dir = TempDir::new().unwrap();
                let named = if via_link {
                    let link = link_dir.path().join("link.db");
                    std::os::unix::fs::symlink(&raw, &link).unwrap();
                    link
                } else {
                    raw.clone()
                };
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(
                        crate::snapshot::inspect(&named)
                            .map(|m| m.tool_version)
                            .map_err(|e| format!("{e:#}")),
                    );
                });
                let case = format!("{mode} {shape} (link: {via_link})");
                let got = rx
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap_or_else(|_| panic!("{case}: inspect blocked on the side file"));
                assert_eq!(got.as_deref(), Ok("9.9.9"), "{case}");
                let mut left: Vec<_> = std::fs::read_dir(dir.path())
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                left.sort();
                assert_eq!(
                    left,
                    ["snapshot.db", "snapshot.db-journal"],
                    "{case}: inspect added or removed files beside the snapshot"
                );
            }
        }
    }
}

/// A `-wal` from ANOTHER database, its frames not yet checkpointed: a stray WAL
/// that SQLite applies to whatever file it finds it beside.
#[cfg(unix)]
fn foreign_wal(into: &std::path::Path) {
    let dir = TempDir::new().unwrap();
    let other = dir.path().join("other.db");
    let conn = Connection::open(&other).unwrap();
    conn.query_row("PRAGMA journal_mode = wal", [], |r| r.get::<_, String>(0))
        .unwrap();
    conn.execute_batch("PRAGMA wal_autocheckpoint = 0; CREATE TABLE z(a);")
        .unwrap();
    for _ in 0..50 {
        conn.execute("INSERT INTO z(a) VALUES (?1)", ["y".repeat(500)])
            .unwrap();
    }
    std::fs::copy(dir.path().join("other.db-wal"), into).unwrap();
}

/// D#637. SQLite opens `<file>-wal` whenever there is something there it
/// counts as one (anything but an empty regular file), whatever the header
/// says, and a read-only connection then creates a `-shm` beside the user's
/// file that it cannot remove. 0.180.0 left one beside a non-WAL snapshot for a
/// FIFO at that name and for another database's WAL, and applied that WAL's
/// frames: the snapshot read as having no `meta` and was refused. The copy
/// leaves the `-wal` behind, as the WAL arm does. Through a link too, whose
/// side files SQLite looks for beside the target. A single byte is enough for
/// SQLite to open it. In a thread with a deadline, for the FIFO.
#[cfg(unix)]
#[test]
fn inspect_reads_a_non_wal_file_past_a_stray_wal() {
    for shape in ["another database's WAL", "FIFO", "1-byte file"] {
        for via_link in [false, true] {
            let dir = TempDir::new().unwrap();
            let raw = dir.path().join("snapshot.db");
            raw_snapshot_fixture(&raw, "delete", 1);
            let side = dir.path().join("snapshot.db-wal");
            if shape == "FIFO" {
                let made = Command::new("mkfifo").arg(&side).status().unwrap();
                assert!(made.success(), "mkfifo {}", side.display());
            } else if shape == "1-byte file" {
                std::fs::write(&side, [1u8]).unwrap();
            } else {
                foreign_wal(&side);
            }
            let wal_before = (shape != "FIFO").then(|| std::fs::read(&side).unwrap());
            let link_dir = TempDir::new().unwrap();
            let named = if via_link {
                let link = link_dir.path().join("link.db");
                std::os::unix::fs::symlink(&raw, &link).unwrap();
                link
            } else {
                raw.clone()
            };
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(
                    crate::snapshot::inspect(&named)
                        .map(|m| m.tool_version)
                        .map_err(|e| format!("{e:#}")),
                );
            });
            let got = rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("{shape} (link: {via_link}): inspect blocked"));
            assert_eq!(got.as_deref(), Ok("9.9.9"), "{shape} (link: {via_link})");
            let mut left: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            left.sort();
            assert_eq!(
                left,
                ["snapshot.db", "snapshot.db-wal"],
                "{shape} (link: {via_link}): inspect added or removed files beside the snapshot"
            );
            if let Some(before) = wal_before {
                assert!(
                    std::fs::read(&side).unwrap() == before,
                    "{shape} (link: {via_link}): inspect changed the -wal"
                );
            }
        }
    }
}

/// The staged copy is the main database file and nothing else: not the
/// `-journal`, whose rollback deletes a path the journal itself names and can
/// grow the copy past any limit (`stage_copy`), and not the `-wal`, so the WAL
/// arm reports the last checkpoint. Whatever sits at either name, the staged
/// directory holds one file.
#[test]
fn stage_copy_takes_the_main_file_alone() {
    let committed_dir = TempDir::new().unwrap();
    let committed = committed_dir.path().join("committed.db");
    raw_snapshot_fixture(&committed, "delete", 1);
    let victim_dir = TempDir::new().unwrap();
    let victim = victim_dir.path().join("victim");
    std::fs::write(&victim, b"not a super-journal").unwrap();

    let shapes: [(&str, &str, Vec<u8>); 4] = [
        ("hot journal", "-journal", Vec::new()),
        (
            "journal naming a file",
            "-journal",
            hot_looking_journal(Some(&victim)),
        ),
        ("journal of zeros", "-journal", vec![0u8; 64 * 1024]),
        ("stray wal", "-wal", vec![1u8; 4096]),
    ];
    for (name, suffix, side_bytes) in shapes {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fixture.db");
        let side = dir.path().join(format!("fixture.db{suffix}"));
        if name == "hot journal" {
            hot_journal_pair(&committed, &file);
        } else {
            std::fs::copy(&committed, &file).unwrap();
            std::fs::write(&side, &side_bytes).unwrap();
        }
        let before = files_in(dir.path());

        let into = TempDir::new().unwrap();
        let staged = super::stage_copy(&file, into.path()).unwrap();
        assert_eq!(staged, into.path().join("snapshot.db"), "{name}");
        assert_eq!(
            files_in(into.path())
                .iter()
                .map(|(n, _)| n.clone())
                .collect::<Vec<_>>(),
            ["snapshot.db"],
            "{name}: the staged directory holds more than the main file"
        );
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            std::fs::read(&file).unwrap(),
            "{name}: the copy is not the main file byte for byte"
        );
        assert_eq!(
            files_in(dir.path()),
            before,
            "{name}: stage_copy touched the files it read"
        );
        assert!(victim.exists(), "{name}: stage_copy deleted the named file");
    }
}

/// 0.181.0 pre-tag review R3 (D#646). Staging the `-journal` so that SQLite
/// could roll it back (D#630) handed `inspect` a delete: `pager_playback` reads
/// a super-journal path out of the journal (sqlite3.c:61755) and
/// `pager_delsuper` deletes the file that path names (61912 -> 61542). The
/// bytes at `<file>-journal` are nobody's promise — they are whatever sits
/// beside the file the user named — so those bytes chose which file went. The
/// journal is not staged any anymore; the control proves the fixture really
/// does delete through this SQLite, so a green test is not a fixture that
/// disarmed itself.
#[test]
fn inspect_does_not_delete_a_file_a_journal_names() {
    for (header, write_version) in [("rollback", 1u8), ("wal", 2u8)] {
        let dir = TempDir::new().unwrap();
        let raw = dir.path().join("snapshot.db");
        raw_snapshot_fixture(&raw, "delete", 1);
        if write_version == 2 {
            let mut bytes = std::fs::read(&raw).unwrap();
            bytes[18] = 2;
            bytes[19] = 2;
            std::fs::write(&raw, &bytes).unwrap();
        }
        // Outside `dir`, so that the residue check below stays about the pair.
        let victim_dir = TempDir::new().unwrap();
        let victim = victim_dir.path().join("victim");
        let plant_victim = || std::fs::write(&victim, b"not a super-journal").unwrap();
        plant_victim();
        let journal = dir.path().join("snapshot.db-journal");
        std::fs::write(&journal, hot_looking_journal(Some(&victim))).unwrap();

        // Control: the pair as the withdrawn staging opened it — a writable
        // copy with the journal beside it — deletes the victim.
        {
            let control_dir = TempDir::new().unwrap();
            let control = control_dir.path().join("snapshot.db");
            std::fs::copy(&raw, &control).unwrap();
            std::fs::copy(&journal, control_dir.path().join("snapshot.db-journal")).unwrap();
            let _ = Connection::open(&control)
                .and_then(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)));
            assert!(
                !victim.exists(),
                "{header}: control did not fire — this journal does not name a file SQLite \
                 deletes, so the assertion below would pass on any code"
            );
            plant_victim();
        }

        let before = files_in(dir.path());
        let got = inspect_outcome(&raw);
        assert!(
            victim.exists(),
            "{header}: inspect deleted the file the journal named"
        );
        assert_eq!(got.as_deref(), Ok("9.9.9"), "{header}");
        assert_eq!(
            files_in(dir.path()),
            before,
            "{header}: inspect changed the pair it was pointed at"
        );
    }
}

/// Every arm refuses a snapshot whose schema is newer than this binary's with
/// the open's own sentence. Nothing pinned it on the staged arms (pre-tag
/// review: `open_staged` mapping every error to "not a valid code-graph
/// snapshot" kept the suite green), and that sentence is the one that says
/// what to do.
#[test]
fn inspect_says_a_newer_schema_is_newer_on_every_arm() {
    let fixture = |mode: &str, write_version: u8| {
        let dir = TempDir::new().unwrap();
        let raw = dir.path().join("snapshot.db");
        raw_snapshot_fixture(&raw, mode, write_version);
        Connection::open(&raw)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        (dir, raw)
    };
    let (_in_place_dir, in_place) = fixture("delete", 1);
    let (_wal_dir, wal) = fixture("wal", 2);
    let (zst_dir, rollback) = fixture("delete", 1);
    let zst = zst_dir.path().join("snapshot.db.zst");
    std::fs::write(
        &zst,
        zstd::encode_all(&std::fs::read(&rollback).unwrap()[..], 1).unwrap(),
    )
    .unwrap();

    for (arm, file) in [("in place", &in_place), ("WAL", &wal), ("compressed", &zst)] {
        let msg = format!("{:#}", crate::snapshot::inspect(file).unwrap_err());
        assert!(
            msg.contains("newer than supported"),
            "{arm}: expected the newer-schema refusal, got: {msg}"
        );
    }
}

/// A `-journal` `inspect` cannot read is read PAST, as 0.180.0 read past it:
/// the staged copy does not open the journal at all (`stage_copy`), and the
/// in-place arm, where SQLite's own probe opens it and so calls it hot, falls
/// back to that copy. 0.181.0 briefly refused these pairs, naming the journal,
/// which `sqlite3` also does ("unable to open database file") — withdrawn with
/// the rollback (D#646). Three shapes, because SQLite treats them differently
/// and `inspect` must not: an empty one is no journal to it at all
/// (`unixAccess` counts a regular file as there only when it has bytes), a
/// non-empty one it cannot open it calls hot (`hasHotJournal`), and one
/// starting with the journal magic is the real thing.
#[cfg(unix)]
#[test]
fn inspect_reads_past_a_journal_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt;

    for (shape, bytes) in [
        ("empty", Vec::new()),
        ("non-empty", vec![7u8; 512]),
        ("hot", hot_looking_journal(None)),
    ] {
        for (mode, write_version) in [("delete", 1u8), ("wal", 2u8)] {
            let dir = TempDir::new().unwrap();
            let raw = dir.path().join("snapshot.db");
            raw_snapshot_fixture(&raw, mode, write_version);
            let journal = dir.path().join("snapshot.db-journal");
            std::fs::write(&journal, &bytes).unwrap();
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o000)).unwrap();
            let readable_as_root = std::fs::File::open(&journal).is_ok();
            let got = crate::snapshot::inspect(&raw).map(|m| m.tool_version);
            let left = std::fs::metadata(&journal).unwrap();
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
            let case = format!("{shape} {mode} (unreadable: {})", !readable_as_root);
            assert_eq!(
                got.map_err(|e| format!("{e:#}")).as_deref(),
                Ok("9.9.9"),
                "{case}"
            );
            assert_eq!(left.len() as usize, bytes.len(), "{case}: the journal grew");
            assert_eq!(
                std::fs::read(&journal).unwrap(),
                bytes,
                "{case}: inspect changed the journal"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn inspect_reads_a_snapshot_whose_file_is_read_only() {
    // An improvement the in-place arm bought that its own commit did not claim,
    // found by pre-ship review. `fs::copy` preserves the source's permission
    // bits, so staging a mode-0444 snapshot produced a 0444 copy that
    // `Database::open` could not write to, and `inspect` failed with
    // "attempt to write a readonly database". Opening in place read-only has no
    // such problem. The scenario is ordinary: `chmod -w` on a downloaded release
    // artifact, or anything restored from a backup.
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let raw = dir.path().join("snapshot.db");
    raw_snapshot_fixture(&raw, "delete", 1);

    let original = std::fs::metadata(&raw).unwrap().permissions();
    std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o444)).unwrap();
    let result = crate::snapshot::inspect(&raw);
    std::fs::set_permissions(&raw, original).unwrap();

    let meta = result.expect("a read-only snapshot file must still inspect");
    assert_eq!(meta.tool_version, "9.9.9");
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
