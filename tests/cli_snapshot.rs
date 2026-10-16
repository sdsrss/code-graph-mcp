//! CLI smoke tests for `code-graph-mcp snapshot create|inspect`.

use std::process::Command;
use tempfile::TempDir;

fn cli_bin() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

fn init_git_repo() -> TempDir {
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
    std::fs::write(p.join("src/lib.rs"), "pub fn h() {}\n").unwrap();
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

/// The `[snapshot] url` trust gate, exercised through the real binary.
///
/// # Why this is not covered by the unit tests
///
/// `resolve_snapshot_source_impl` takes `url_trusted` as a PARAMETER, and every
/// unit test passes it directly. The env read lives in the thin public wrapper
/// `resolve_snapshot_source`, which no test reaches — so renaming the variable,
/// inverting the comparison, or reading `SNAPSHOT_TRUST_ORIGIN` here by
/// copy-paste would leave the whole suite green while the gate that blocks
/// malicious-repo snapshot injection silently stopped opening (or worse, stopped
/// closing). Subprocesses, not `set_var`: the env is process-global and the rest
/// of the suite runs in parallel.
///
/// Case B is the non-vacuity anchor. Without a case that is ALLOWED through,
/// "everything was refused" is equally consistent with a gate that works and a
/// gate wired to a variable nobody sets. Port 1 is reserved, so the trusted
/// fetch fails on connection-refused immediately and offline.
#[test]
fn cli_snapshot_url_override_honors_only_its_own_env_signal() {
    let repo = init_git_repo();
    std::fs::write(
        repo.path().join(".code-graph.toml"),
        "[snapshot]\nurl = \"https://127.0.0.1:1/attacker.db.zst\"\n",
    )
    .unwrap();

    const REFUSAL: &str = "CODE_GRAPH_SNAPSHOT_TRUST_URL=1 in your environment to trust it";
    const ATTEMPTED: &str = "Snapshot install failed";
    let pin = "a".repeat(64);

    /// label, the one trust signal this case sets (if any), and whether the
    /// committed url override is expected to be honored.
    type Case<'a> = (&'a str, Option<(&'a str, &'a str)>, bool);

    let cases: [Case; 6] = [
        ("no signal", None, false),
        (
            "TRUST_URL=1",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_URL", "1")),
            true,
        ),
        (
            "TRUST_ORIGIN=1 must not unlock a url override",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN", "1")),
            false,
        ),
        (
            "PIN must not unlock a url override",
            Some(("CODE_GRAPH_SNAPSHOT_PIN", pin.as_str())),
            false,
        ),
        (
            "TRUST_URL=0",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_URL", "0")),
            false,
        ),
        (
            "TRUST_URL=true is not the literal 1",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_URL", "true")),
            false,
        ),
    ];

    for (label, signal, honored) in cases {
        let _ = std::fs::remove_dir_all(repo.path().join(".code-graph"));
        let mut cmd = Command::new(cli_bin());
        cmd.args(["reindex", "--from-snapshot"])
            .current_dir(repo.path())
            .env("CODE_GRAPH_DISABLE_MODEL_DOWNLOAD", "1")
            // Pinned, not left to the ambient env (same reason cli_e2e.rs pins
            // it): the refusal this test matches on is a `tracing::warn!`, and
            // `EnvFilter::try_from_default_env()` lets a shell or CI exporting
            // `RUST_LOG=error` swallow it. The five refuse-cases would then fail
            // claiming the gate had been BYPASSED while their own captured
            // output says "No snapshot source resolved" — sending whoever hits
            // it to debug a security gate that is working.
            .env("RUST_LOG", "warn")
            // Never inherit the developer's own trust signals, and keep the
            // connection-refused outcome independent of an ambient proxy.
            .env_remove("CODE_GRAPH_SNAPSHOT_TRUST_URL")
            .env_remove("CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN")
            .env_remove("CODE_GRAPH_SNAPSHOT_PIN")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy");
        if let Some((k, v)) = signal {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        if honored {
            assert!(
                !text.contains(REFUSAL),
                "[{label}] override was refused despite its own trust signal — \
                 the env read in resolve_snapshot_source is not wired.\n{text}"
            );
            assert!(
                text.contains(ATTEMPTED),
                "[{label}] expected the trusted url to be fetched (and fail on \
                 connection-refused); without this the other cases prove nothing.\n{text}"
            );
        } else {
            assert!(
                text.contains(REFUSAL),
                "[{label}] a committed url override was honored without \
                 CODE_GRAPH_SNAPSHOT_TRUST_URL=1.\n{text}"
            );
            assert!(
                !text.contains(ATTEMPTED),
                "[{label}] the untrusted url was contacted.\n{text}"
            );
        }
    }
}

#[test]
fn cli_snapshot_create_then_inspect_round_trip() {
    let repo = init_git_repo();
    let out = repo.path().join("snap.db");

    let status = Command::new(cli_bin())
        .args(["snapshot", "create", "--out"])
        .arg(&out)
        .arg("--quiet")
        .arg("--root")
        .arg(repo.path())
        .status()
        .unwrap();
    assert!(status.success(), "create failed");
    assert!(out.exists());

    // Compress and inspect
    let bytes = std::fs::read(&out).unwrap();
    let zst = repo.path().join("snap.db.zst");
    std::fs::write(&zst, zstd::encode_all(&bytes[..], 9).unwrap()).unwrap();

    let output = Command::new(cli_bin())
        .args(["snapshot", "inspect"])
        .arg(&zst)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!json["tool_version"].as_str().unwrap().is_empty());
    assert!(json["schema_version"].as_i64().unwrap() > 0);
    assert!(json["created_at"].as_i64().unwrap() > 0);
    assert_eq!(json["includes_vec"].as_bool(), Some(false));
}

#[test]
fn cli_snapshot_inspect_missing_file_exits_nonzero() {
    let output = Command::new(cli_bin())
        .args(["snapshot", "inspect", "/nonexistent/path.db.zst"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

// clap-migrated (audit #4 Step 4): `snapshot` is now a nested #[command(subcommand)].
// clap owns --help for the parent and each sub, plus no-subcommand /
// unknown-subcommand rejection (exit 2), replacing the hand-rolled args[2]/args[3]
// dispatch. These lock the new surface and guard against internal-note leak.

const INTERNAL_TOKENS: &[&str] = &["audit #", "clap-migrat", "args[3]", "hand-rolled"];

#[test]
fn cli_snapshot_help_lists_subcommands_no_leak() {
    let out = Command::new(cli_bin())
        .args(["snapshot", "--help"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "snapshot --help should exit 0");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("create") && s.contains("inspect"),
        "parent help must list both subcommands; got: {s}"
    );
    let low = s.to_lowercase();
    for tok in INTERNAL_TOKENS {
        assert!(
            !low.contains(&tok.to_lowercase()),
            "snapshot --help leaked {tok:?}; got: {s}"
        );
    }
}

#[test]
fn cli_snapshot_create_help_shows_out_no_leak() {
    let out = Command::new(cli_bin())
        .args(["snapshot", "create", "--help"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "snapshot create --help should exit 0"
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("--out"), "create help must show --out; got: {s}");
    let low = s.to_lowercase();
    for tok in INTERNAL_TOKENS {
        assert!(
            !low.contains(&tok.to_lowercase()),
            "snapshot create --help leaked {tok:?}; got: {s}"
        );
    }
}

#[test]
fn cli_snapshot_no_subcommand_exits_2() {
    // clap requires a subcommand (was: hand-rolled Usage + exit 2).
    let out = Command::new(cli_bin()).args(["snapshot"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "snapshot with no subcommand must exit 2"
    );
}

#[test]
fn cli_snapshot_unknown_subcommand_exits_2() {
    let out = Command::new(cli_bin())
        .args(["snapshot", "bogus"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "unknown snapshot subcommand must exit 2"
    );
}

/// E2E 2026-10-16 round 2 (plugin QA, P1-1): the shipped workflow template
/// compressed with an external `zstd` and uploaded the `.db.zst` alone, while
/// install fails closed without its `.blake3` sidecar — so the documented team
/// flow could never install. The template must upload exactly what `snapshot
/// create --out X.db.zst` writes: X.db.zst and X.db.zst.blake3.
#[test]
fn the_snapshot_template_uploads_the_checksum_sidecar_create_writes() {
    let template = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("claude-plugin/templates/code-graph-snapshot.yml"),
    )
    .unwrap();
    let out = template
        .split("snapshot create --out ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the template runs `snapshot create --out <file>`")
        .trim_matches('"');
    assert!(
        out.ends_with(".db.zst"),
        "create must compress itself, writing the sidecar: {out}"
    );
    let upload = template
        .split("gh release upload")
        .nth(1)
        .expect("the template uploads to the release");
    assert!(
        upload.contains(&format!("\"{out}\"")),
        "uploads {out}: {upload}"
    );
    assert!(
        upload.contains(&format!("\"{out}.blake3\"")),
        "uploads {out}.blake3: {upload}"
    );
    assert!(
        !template.contains("zstd -9"),
        "no external compression: it writes no sidecar"
    );

    // And `create` does write that sidecar.
    let repo = init_git_repo();
    let snap = repo.path().join("s.db.zst");
    let status = Command::new(cli_bin())
        .args([
            "snapshot",
            "create",
            "--out",
            snap.to_str().unwrap(),
            "--quiet",
        ])
        .current_dir(repo.path())
        .env("CODE_GRAPH_INTERNAL", "1")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(repo.path().join("s.db.zst.blake3").is_file());
}
