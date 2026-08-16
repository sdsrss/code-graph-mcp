//! Read commands must never destroy the index.
//!
//! `INDEX_VERSION` revalidation (wipe + rebuild) is an *indexer* responsibility.
//! When a passive consumer performs it, the wipe happens and nothing rebuilds —
//! the index stays at 0 nodes until the user notices. `health-check` and `grep`
//! caused exactly that once (the daagu incident) and were moved onto
//! `Database::open_nondestructive`; `similar` was left behind on the indexer
//! constructor because it also needed sqlite-vec, and vector support and
//! destructive revalidation were entangled in one constructor.
//!
//! This test drives the real binary, so it covers the CLI wiring (which
//! constructor `cmd_similar` reaches for), not just the storage layer.

use std::process::Command;
use tempfile::TempDir;

use code_graph_mcp::domain::CODE_GRAPH_DIR;
use code_graph_mcp::storage::db::Database;

fn cli_bin() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

fn node_count(db_path: &std::path::Path) -> i64 {
    let db = Database::open_nondestructive(db_path).unwrap();
    db.conn()
        .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
        .unwrap()
}

/// A project directory the indexer accepts: a `.git` anchor (the activation
/// gate refuses to index anything else) plus one source file.
fn fixture_project() -> TempDir {
    let project = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(project.path())
        .status()
        .unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub fn alpha() { beta(); }\npub fn beta() {}\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    project
}

#[test]
fn similar_does_not_wipe_a_version_lagging_index() {
    let project = fixture_project();
    let db_path = project.path().join(CODE_GRAPH_DIR).join("index.db");

    let status = Command::new(cli_bin())
        .args(["incremental-index", "--quiet", "--no-embed"])
        .current_dir(project.path())
        .status()
        .unwrap();
    assert!(status.success(), "fixture index build failed");

    let before = node_count(&db_path);
    assert!(before > 0, "fixture must index some nodes (got {before})");

    // Simulate "binary upgraded past the on-disk index generation, no rebuild
    // has run yet" — the window every INDEX_VERSION bump opens for every user.
    {
        let db = Database::open_nondestructive(&db_path).unwrap();
        db.conn()
            .pragma_update(
                None,
                "application_id",
                code_graph_mcp::domain::INDEX_VERSION - 1,
            )
            .unwrap();
    }

    // `similar` may legitimately fail here (no embeddings in a --no-embed index).
    // What it must not do is take the index down with it.
    let out = Command::new(cli_bin())
        .args(["similar", "alpha"])
        .current_dir(project.path())
        .output()
        .unwrap();

    let after = node_count(&db_path);
    assert_eq!(
        after,
        before,
        "read-only `similar` wiped the index ({before} → {after} nodes); it must \
         open non-destructively like every other read command. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // And the staleness must still be *owed*: a reader that silently re-stamped
    // application_id would mask the pending rebuild from the next indexer open.
    let db = Database::open_nondestructive(&db_path).unwrap();
    let stamped: i32 = db
        .conn()
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    assert_eq!(
        stamped,
        code_graph_mcp::domain::INDEX_VERSION - 1,
        "reader must leave the stale generation stamped so the rebuild is still owed"
    );
}

/// The test above pins the `similar` INSTANCE. This one pins the CLASS.
///
/// Contract audit 2026-07-27 measured all 25 read subcommands against a
/// version-lagging index: none wipe today. But the guard for that fact was a
/// single hardcoded `.args(["similar", "alpha"])`, so read command #26 could
/// reach for the indexer constructor and every test stays green — which is
/// precisely how `similar` itself survived four audits. Sibling-hole class,
/// first-ranked finding five audits running.
#[test]
fn no_read_subcommand_wipes_a_version_lagging_index() {
    // Every subcommand that answers a question about an existing index. Failure
    // modes vary (some exit non-zero without embeddings, some print "not found")
    // and that is fine — the assertion is only that the index survives.
    const READ_COMMANDS: &[&[&str]] = &[
        &["grep", "alpha"],
        &["search", "alpha"],
        &["ast-search", "alpha"],
        &["callgraph", "alpha"],
        &["impact", "alpha"],
        &["show", "alpha"],
        &["refs", "alpha"],
        &["similar", "alpha"],
        &["map"],
        &["overview", "src"],
        &["deps"],
        &["tour"],
        &["trace", "GET /x"],
        &["dead-code"],
        &["centrality"],
        &["cycles"],
        &["surprising"],
        &["report"],
        &["stats"],
        &["health-check"],
        &["affected"],
    ];

    let project = fixture_project();
    let db_path = project.path().join(CODE_GRAPH_DIR).join("index.db");
    assert!(
        Command::new(cli_bin())
            .args(["incremental-index", "--quiet", "--no-embed"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success(),
        "fixture index build failed"
    );
    let before = node_count(&db_path);
    assert!(before > 0, "fixture must index some nodes (got {before})");

    let stale = code_graph_mcp::domain::INDEX_VERSION - 1;
    for cmd in READ_COMMANDS {
        // Re-stamp before each command: a command that DID wipe would otherwise
        // leave a rebuilt, current-generation index and let the rest pass.
        {
            let db = Database::open_nondestructive(&db_path).unwrap();
            db.conn()
                .pragma_update(None, "application_id", stale)
                .unwrap();
        }
        let out = Command::new(cli_bin())
            .args(*cmd)
            .current_dir(project.path())
            .output()
            .unwrap();
        let after = node_count(&db_path);
        assert_eq!(
            after,
            before,
            "read command `{}` wiped a version-lagging index ({before} → {after} \
             nodes). Route it through CliContext (open_nondestructive*), not the \
             indexer constructor. stderr: {}",
            cmd.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Source-level companion to the behavioural sweep above: no `cmd_*` function
/// may reach for the destructive indexer constructor.
///
/// The behavioural test can only cover subcommands someone remembered to list.
/// This one fails on the *edit* — the moment a read path types
/// `Database::open_with_vec` — without needing anybody to extend a table.
#[test]
fn only_indexer_entry_points_use_the_destructive_constructor() {
    // The CLI is a module tree (`src/cli/**`), not one file. Scan it whole:
    // splitting cli.rs must not silently shrink this guard's reach.
    let cli_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli");
    let mut cli_files: Vec<std::path::PathBuf> = Vec::new();
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }
    collect(&cli_root, &mut cli_files);
    assert!(
        cli_files.len() > 5,
        "expected the CLI module tree under src/cli/, found {} file(s)",
        cli_files.len()
    );

    // The two legitimate sites, both of which BUILD an index rather than read
    // one: a full index into an explicit path, and the incremental refresh.
    // The name is matched EXACTLY, so it tracks the function that holds the
    // call: `cmd_incremental_index` is now a thin wrapper and the body (with the
    // open) lives in `cmd_incremental_index_opts`.
    const ALLOWED_ANCHORS: [&str; 2] = ["fn build_full_index_at", "fn cmd_incremental_index_opts"];

    // `Database::open` is the SAME destructive constructor with a different
    // vec flag (open_impl(path, false, revalidate=TRUE)) — the guard's first
    // version matched only `open_with_vec` and a mutation experiment showed a
    // `Database::open(&db_path)` planted in a read command left it green
    // (audit 2026-08-02 MED-5). cmd_benchmark opens its OWN temp DB, which is
    // fine — it never touches the user's index.
    const ALLOWED_OPEN_ANCHORS: [&str; 3] = [
        "fn build_full_index_at",
        "fn cmd_incremental_index_opts",
        "fn cmd_benchmark",
    ];

    let mut offenders: Vec<String> = Vec::new();
    for cli_file in &cli_files {
        let src = std::fs::read_to_string(cli_file)
            .unwrap_or_else(|e| panic!("read {}: {e}", cli_file.display()));
        let rel = cli_file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(cli_file)
            .display()
            .to_string();
        let lines: Vec<&str> = src.lines().collect();
        let mut current_fn = "<file scope>";
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            // The same mutation round showed `pub(crate) fn` slipping past a
            // `fn `/`pub fn `-only tracker: current_fn stayed at the PREVIOUS
            // declaration, inheriting its allowlist exemption.
            let decl = t
                .strip_prefix("pub(crate) fn ")
                .or_else(|| t.strip_prefix("pub(super) fn "))
                .or_else(|| t.strip_prefix("pub async fn "))
                .or_else(|| t.strip_prefix("async fn "))
                .or_else(|| t.strip_prefix("pub fn "))
                .or_else(|| t.strip_prefix("fn "));
            if let Some(rest) = decl {
                current_fn = rest.split('(').next().unwrap_or(rest);
            }
            // Skip doc comments and ordinary comments: several of them name the
            // constructor while explaining why a reader must NOT use it.
            if t.starts_with("//") {
                continue;
            }
            let hits_destructive = line.contains("Database::open_with_vec")
                || (line.contains("Database::open(") && !line.contains("open_nondestructive"));
            let allowed = if line.contains("Database::open_with_vec") {
                ALLOWED_ANCHORS
                    .iter()
                    .any(|a| a.trim_start_matches("fn ") == current_fn)
            } else {
                ALLOWED_OPEN_ANCHORS
                    .iter()
                    .any(|a| a.trim_start_matches("fn ") == current_fn)
            };
            if hits_destructive && !allowed {
                offenders.push(format!("{rel}:{} (in fn {current_fn})", i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "`Database::open_with_vec` / `Database::open` perform the destructive \
         INDEX_VERSION revalidation. Reached from a read command they wipe the \
         user's index and nothing rebuilds it (the daagu failure; `similar` \
         shipped this way). Use CliContext::open / open_with_vec instead. \
         Offending sites: {offenders:?}"
    );
}

/// The `<external>` query-layer exclusion has no other live guard.
///
/// Round-6 finding, on my own work: `external_sentinel_tests` in src/resolve.rs
/// and the "negative control" in the MCP integration test both survive deleting
/// `EXCLUDE_EXTERNAL_BY_NAME` *and* neutering `is_selectable_definition` — the
/// by-name fuzzy path already carries `AND n.type != 'module'`, and a sentinel is
/// typed non-`module` only when NO project symbol shares the name, which is
/// exactly when there is nothing to discriminate. So those tests cannot fail.
///
/// This one drives the real binary at the surface where the defect was actually
/// observed: before the fix, `show HashMap` in a project that merely imports it
/// printed `module <external>/HashMap` and exited 0, inventing a definition that
/// has no file on disk.
///
/// SCOPE, corrected by round 7 after an earlier version of this comment claimed
/// more: it goes red under the SQL mutation (`EXCLUDE_EXTERNAL_BY_NAME` emptied)
/// and NOT under `is_selectable_definition` → `true`. Those two guards sit in
/// series with the SQL one first, so no reachable input carries an `<external>`
/// path into the Rust predicate. Nothing in the suite detects that second
/// mutation end-to-end; `is_selectable_definition` is unit-tested directly in
/// src/resolve.rs instead, which covers the function but not its reachability.
/// If the SQL exclusion is ever relaxed — the direction its own doc comment
/// contemplates for `deps` disclosure — that predicate becomes load-bearing and
/// needs an end-to-end guard of its own.
#[test]
fn show_does_not_resolve_a_name_that_exists_only_as_an_import() {
    let project = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(project.path())
        .status()
        .unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod a;\n").unwrap();
    // HashMap exists ONLY as an import — no project symbol by that name.
    std::fs::write(
        src.join("a.rs"),
        "use std::collections::HashMap;\npub fn build() -> HashMap<u8, u8> { HashMap::new() }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"f\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    assert!(
        Command::new(cli_bin())
            .args(["incremental-index", "--quiet", "--no-embed"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success(),
        "fixture index build failed"
    );

    let out = Command::new(cli_bin())
        .args(["show", "HashMap"])
        .current_dir(project.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stdout.contains(code_graph_mcp::domain::EXTERNAL_FILE_PATH),
        "`show` resolved an import-only name to the `<external>` pseudo-file. That \
         is not a location the user can open, and the sentinel exists to hold \
         import edges, not to be a definition.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Sanity: a real project symbol still resolves, so the exclusion has not
    // simply made `show` unable to find anything.
    let ok = Command::new(cli_bin())
        .args(["show", "build"])
        .current_dir(project.path())
        .output()
        .unwrap();
    let ok_stdout = String::from_utf8_lossy(&ok.stdout);
    assert!(
        ok_stdout.contains("src/a.rs"),
        "a real project symbol must still resolve: {ok_stdout}"
    );
}

/// Byte offset 0 of a SQLite file holds the magic header string; clobbering it
/// makes `Connection::open` itself fail (`file is not a database`), which is the
/// trigger for the corruption branch. This is a DIFFERENT trigger from the
/// `INDEX_VERSION` wipe the tests above cover, and it reached the same
/// `open_impl` for readers as for indexers.
fn clobber_header(db_path: &std::path::Path) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(db_path)
        .unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(b"NOTSQLITE".repeat(4).as_slice()).unwrap();
}

fn indexed_fixture() -> (TempDir, std::path::PathBuf) {
    let project = fixture_project();
    let db_path = project.path().join(CODE_GRAPH_DIR).join("index.db");
    let status = Command::new(cli_bin())
        .args(["incremental-index", "--quiet", "--no-embed"])
        .current_dir(project.path())
        .status()
        .unwrap();
    assert!(status.success(), "fixture index build failed");
    assert!(node_count(&db_path) > 0, "fixture must index some nodes");
    (project, db_path)
}

#[test]
fn read_command_reports_a_corrupt_index_instead_of_deleting_it() {
    // The `INDEX_VERSION` wipe was moved off readers; the CORRUPTION wipe was
    // not, so the documented invariant held for one trigger and not the other.
    // Reproduced before the fix: a plain `health-check` (what the statusline
    // polls on every render) deleted a 151 552-byte index holding real symbols
    // and left a 4 096-byte empty one behind. Nothing rebuilds after a status
    // poll, and the integrity probes then reported `quick_check: ok` — on the
    // replacement — so the user lost the index and was told it was fine.
    let (project, db_path) = indexed_fixture();
    let size_before = std::fs::metadata(&db_path).unwrap().len();
    clobber_header(&db_path);

    let out = Command::new(cli_bin())
        .args(["health-check", "--format", "json"])
        .current_dir(project.path())
        .output()
        .unwrap();

    // The bytes are still there. This is the whole point: a passive consumer
    // must not be the thing that destroys the user's index.
    let size_after = std::fs::metadata(&db_path)
        .expect("a read command must not DELETE the index file")
        .len();
    assert_eq!(
        size_after, size_before,
        "a read command rewrote the index (before {size_before} B, after {size_after} B)"
    );

    // And it must be diagnosable, not an opaque failure: same `issue` +
    // `integrity.quick_check` shape a page-level corruption produces, which is
    // what doctor's `index-corrupt` repair routes off.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("non-JSON stdout {stdout:?}: {e}"));
    assert_eq!(v["healthy"], serde_json::json!(false));
    let issue = v["issue"].as_str().unwrap_or_default();
    assert!(
        issue.contains("corrupt"),
        "issue must name corruption, got {issue:?}"
    );
    assert!(
        issue.contains("rebuild-index --confirm"),
        "issue must carry the remedy so the user is not left guessing, got {issue:?}"
    );
    assert!(
        v["integrity"]["quick_check"].is_string(),
        "integrity.quick_check must carry the verdict doctor reads, got {}",
        v["integrity"]
    );
    assert_eq!(out.status.code(), Some(1), "an unhealthy index must exit 1");
}

#[test]
fn read_command_reports_a_sub_header_index_instead_of_deleting_it() {
    // Second reader-side wipe with the same shape: `sub_header_size_guard`
    // unconditionally removed main+wal+shm for any file under 100 bytes. Post-
    // crash residue is a real recovery case, but performing it from a status
    // poll destroys a truncated-but-present file that an indexer could have
    // reported on. Distinct from the branch above (it never reaches
    // `Connection::open`), so it needs its own guard.
    let (project, db_path) = indexed_fixture();
    std::fs::write(&db_path, b"short").unwrap();

    let out = Command::new(cli_bin())
        .args(["health-check", "--format", "json"])
        .current_dir(project.path())
        .output()
        .unwrap();

    assert_eq!(
        std::fs::read(&db_path).expect("a read command must not delete a truncated index"),
        b"short",
        "a read command rewrote the truncated index"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("non-JSON stdout {stdout:?}: {e}"));
    assert_eq!(v["healthy"], serde_json::json!(false));
    assert!(
        v["issue"].as_str().unwrap_or_default().contains("corrupt"),
        "issue must name corruption, got {}",
        v["issue"]
    );
}

#[test]
fn indexer_still_self_heals_a_corrupt_index() {
    // Negative control for both tests above. The wipe is not being removed, it
    // is being confined to callers that rebuild in the same breath. If this
    // goes red, the fix overshot and every user with a corrupt index is stuck
    // instead of self-healing on the next index run.
    let (project, db_path) = indexed_fixture();
    clobber_header(&db_path);

    let status = Command::new(cli_bin())
        .args(["incremental-index", "--quiet", "--no-embed"])
        .current_dir(project.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "an INDEXER must still recover from corruption by rebuilding"
    );
    assert!(
        node_count(&db_path) > 0,
        "indexer recovery must leave a populated index behind"
    );
}
