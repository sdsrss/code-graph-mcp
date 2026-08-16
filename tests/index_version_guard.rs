//! Trip-wire for the two versions that gate an index rebuild.
//!
//! `INDEX_VERSION` (src/domain.rs) and `SCHEMA_VERSION` (src/storage/schema.rs)
//! have been bumped 60 and 10 times respectively, every one of them by
//! convention: a human noticed the extraction output moved and remembered to
//! bump. Nothing mechanical forces it. The failure that convention lets through
//! is silent and permanent — an existing index is only rebuilt when the stored
//! version differs, so a change that alters extraction output WITHOUT a bump
//! leaves every already-indexed user on the old, now-wrong graph forever. No
//! test fails, no error surfaces; queries just quietly answer from stale edges.
//!
//! This test cannot decide whether output changed — that judgement needs a human
//! who knows what the edit did. What it can do is refuse to let the edit pass
//! unnoticed: it fingerprints the extraction-relevant sources and fails when
//! they move, putting the question in front of whoever made the change.
//!
//! Deliberately NOT a semantic diff. A comment-only edit trips it too, and that
//! false positive is the accepted cost of a check that cannot be fooled by an
//! edit whose output effect the author misjudged. Clearing it is one command
//! (see `REGEN_CMD`), and the failure message asks the question that matters
//! before offering the command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the accepted fingerprints live. Committed, so a fresh clone and CI see
/// the same baseline the author accepted.
const RECORD_REL: &str = "tests/data/extraction_fingerprint.txt";

const UPDATE_ENV: &str = "UPDATE_EXTRACTION_FINGERPRINT";

const REGEN_CMD: &str =
    "UPDATE_EXTRACTION_FINGERPRINT=1 cargo test -p code-graph-mcp --test index_version_guard";

/// Files whose content feeds the extraction fingerprint.
///
/// `src/parser/**` is every tree-sitter node/relation extractor; the
/// `src/indexer/pipeline/*.rs` files turn that output into the nodes and edges
/// actually written to the DB (resolution, deferred-edge binding, post passes).
/// A change to either can move the stored graph for identical source input,
/// which is exactly what `INDEX_VERSION` exists to invalidate.
/// Coverage is the MAIN extraction surface, not a proof of totality: replaying
/// historical bumps showed at least two (`e06a738` build-dir exclusion in
/// src/utils, `9045f4c` call-edge resolution) that touched none of the
/// originally covered files — so the walk also folds in the file-selection
/// seams (`utils/config.rs` language detection, `utils/gitignore.rs`,
/// `indexer/merkle.rs` walk/exclusion). Changes outside this set can still
/// require a bump; this guard catches the common case, it does not replace the
/// judgment.
fn extraction_sources(root: &Path) -> Vec<(String, PathBuf)> {
    let mut files = collect_rs(root, "src/parser", true);
    files.extend(collect_rs(root, "src/indexer/pipeline", false));
    for extra in [
        "src/utils/config.rs",
        "src/utils/gitignore.rs",
        "src/indexer/merkle.rs",
    ] {
        files.push((extra.to_string(), root.join(extra)));
    }
    files.sort();
    files
}

/// Schema fingerprint is a single file: table/index/trigger DDL plus the
/// migration ladder. Kept separate from extraction because it is gated by a
/// DIFFERENT constant — a column addition needs `SCHEMA_VERSION` + a migration,
/// not an `INDEX_VERSION` bump, and conflating them teaches the wrong fix.
fn schema_sources(root: &Path) -> Vec<(String, PathBuf)> {
    vec![(
        "src/storage/schema.rs".to_string(),
        root.join("src/storage/schema.rs"),
    )]
}

/// `#[cfg(test)] mod tests;` files are excluded: they compile only under
/// `cfg(test)` and therefore cannot change what a release binary extracts, while
/// being the single most frequently edited file in these directories. Including
/// them would redden this guard on every new unit test — and a guard that goes
/// red for reasons the reader knows are irrelevant gets regenerated reflexively,
/// which is the same as not having it. Both current matches
/// (`src/parser/relations/tests.rs`, `src/indexer/pipeline/tests.rs`) are
/// declared behind `#[cfg(test)]` in their parent `mod.rs`.
fn is_test_only_module(rel: &str) -> bool {
    rel.rsplit('/').next() == Some("tests.rs")
}

fn collect_rs(root: &Path, rel_dir: &str, recursive: bool) -> Vec<(String, PathBuf)> {
    let dir = root.join(rel_dir);
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list {} — has the module moved? {e}", dir.display()));
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // Repo-relative with `/` separators so the fingerprint is identical on
        // Windows, where `Path::join` would otherwise feed `\` into the hash.
        let rel = format!("{rel_dir}/{name}");
        if path.is_dir() {
            if recursive {
                out.extend(collect_rs(root, &rel, true));
            }
        } else if name.ends_with(".rs") && !is_test_only_module(&rel) {
            out.push((rel, path));
        }
    }
    out.sort();
    out
}

/// Content hash over a file set. Path and length are folded in alongside the
/// bytes so that renaming a file, or moving a byte across a file boundary,
/// changes the digest — a plain concatenation would not notice either.
fn fingerprint(files: &[(String, PathBuf)]) -> String {
    assert!(
        !files.is_empty(),
        "fingerprint over an EMPTY file set — the source layout moved and this guard is now \
         inert. Fix the paths in extraction_sources()/schema_sources() rather than recording \
         the empty digest."
    );
    let mut hasher = blake3::Hasher::new();
    for (rel, abs) in files {
        let bytes =
            std::fs::read(abs).unwrap_or_else(|e| panic!("cannot read {}: {e}", abs.display()));
        let normalized = normalize_newlines(&bytes);
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(&(normalized.len() as u64).to_le_bytes());
        hasher.update(&normalized);
    }
    hasher.finalize().to_hex().to_string()
}

/// CRLF → LF. A Windows checkout under `core.autocrlf=true` holds different
/// bytes for the same commit, which would make the recorded digest unreachable
/// on that platform — the guard would be permanently red for Windows devs and
/// they would learn to ignore it.
fn normalize_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Read `pub const <name>: i32 = <digits>` out of a source file.
///
/// Parsed from source rather than read from the linked constant on purpose:
/// the recorded file has to name a version the NEXT reader can find by opening
/// `src/domain.rs`, and a parse that drifts from the real declaration is caught
/// by `parsed_constants_match_linked_constants` below.
fn parse_const(src: &str, name: &str) -> i32 {
    let needle = format!("pub const {name}: i32 = ");
    let start = src.find(&needle).unwrap_or_else(|| {
        panic!(
            "`{needle}` not found — the declaration was reformatted or renamed. Update \
             parse_const() to match, do not delete this guard."
        )
    }) + needle.len();
    let digits: String = src[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    assert!(
        !digits.is_empty(),
        "found `{needle}` but no digits after it — is the value now an expression?"
    );
    digits
        .parse()
        .unwrap_or_else(|e| panic!("cannot parse `{digits}` as i32: {e}"))
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Current on-disk state: the two digests and the two versions they belong to.
struct Observed {
    extraction_fp: String,
    index_version: i32,
    schema_fp: String,
    schema_version: i32,
}

fn observe(root: &Path) -> Observed {
    Observed {
        extraction_fp: fingerprint(&extraction_sources(root)),
        index_version: parse_const(
            &read_to_string(&root.join("src/domain.rs")),
            "INDEX_VERSION",
        ),
        schema_fp: fingerprint(&schema_sources(root)),
        schema_version: parse_const(
            &read_to_string(&root.join("src/storage/schema.rs")),
            "SCHEMA_VERSION",
        ),
    }
}

fn render_record(obs: &Observed) -> String {
    format!(
        "# Accepted fingerprints for the extraction + schema sources.\n\
         # Regenerate after a deliberate change:\n\
         #   {REGEN_CMD}\n\
         # See tests/index_version_guard.rs for what each digest covers.\n\
         # Do not hand-edit the hex — regenerate, so the recorded version is the\n\
         # one that was actually in the tree when the digest was taken.\n\
         \n\
         extraction_fingerprint = {}\n\
         extraction_recorded_at_index_version = {}\n\
         \n\
         schema_fingerprint = {}\n\
         schema_recorded_at_schema_version = {}\n",
        obs.extraction_fp, obs.index_version, obs.schema_fp, obs.schema_version
    )
}

fn parse_record(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once('=').unwrap_or_else(|| {
            panic!("malformed line in {RECORD_REL}: {line:?} (want `key = value`)")
        });
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    out
}

fn recorded_field<'a>(rec: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    rec.get(key).map(String::as_str).unwrap_or_else(|| {
        panic!("{RECORD_REL} has no `{key}` — the format changed; regenerate with:\n  {REGEN_CMD}")
    })
}

#[test]
fn extraction_and_schema_sources_match_recorded_fingerprints() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let record_path = root.join(RECORD_REL);
    let obs = observe(&root);

    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::create_dir_all(record_path.parent().expect("record has a parent"))
            .unwrap_or_else(|e| panic!("cannot create tests/data: {e}"));
        std::fs::write(&record_path, render_record(&obs))
            .unwrap_or_else(|e| panic!("cannot write {}: {e}", record_path.display()));
        // Panic AFTER writing, deliberately: `cargo test` swallows eprintln on
        // success, so a `return` here would let a shell that still exports the
        // env var (or a CI misconfiguration) silently record whatever tree it
        // sees and report PASS — the guard defeating itself. The failure makes
        // every regeneration a visible, one-off act.
        panic!(
            "{UPDATE_ENV} was set — baseline REGENERATED, not verified: extraction={} \
             (INDEX_VERSION {}), schema={} (SCHEMA_VERSION {}). Commit {RECORD_REL}, then \
             rerun WITHOUT {UPDATE_ENV} to actually check.",
            obs.extraction_fp, obs.index_version, obs.schema_fp, obs.schema_version
        );
    }

    assert!(
        record_path.exists(),
        "{RECORD_REL} is missing — this guard has no baseline and is inert. Create it:\n  \
         {REGEN_CMD}"
    );
    let record = parse_record(&read_to_string(&record_path));

    let rec_extraction_fp = recorded_field(&record, "extraction_fingerprint");
    let rec_index_version = recorded_field(&record, "extraction_recorded_at_index_version");
    let rec_schema_fp = recorded_field(&record, "schema_fingerprint");
    let rec_schema_version = recorded_field(&record, "schema_recorded_at_schema_version");

    let mut failures = Vec::new();

    if obs.extraction_fp != rec_extraction_fp {
        let version_moved = obs.index_version.to_string() != rec_index_version;
        failures.push(format!(
            "EXTRACTION SOURCES CHANGED (src/parser/**, src/indexer/pipeline/*.rs)\n\
             \x20 recorded {rec_extraction_fp} at INDEX_VERSION {rec_index_version}\n\
             \x20 current  {} at INDEX_VERSION {}\n\
             \x20 {}\n\
             \x20 Decide first: does this change what gets EXTRACTED — any node, edge, \
             qualifier, confidence, or metadata value that would differ for source the user \
             has already indexed?\n\
             \x20   YES -> bump INDEX_VERSION in src/domain.rs (with a note on the constant \
             saying what moved). Without the bump, every existing index keeps the old, now \
             wrong graph: the version is the ONLY rebuild trigger, so nothing else will ever \
             correct it.\n\
             \x20   NO  -> refactor/comment/test-support only; no bump needed.\n\
             \x20 Either way, record the new baseline:\n\
             \x20   {REGEN_CMD}",
            obs.extraction_fp,
            obs.index_version,
            if version_moved {
                "INDEX_VERSION already moved since the baseline — likely the deliberate case; \
                 this is just asking you to re-record."
            } else {
                "INDEX_VERSION is UNCHANGED — if the answer below is YES, this is the silent \
                 stale-index bug, not a stale test."
            },
        ));
    }

    if obs.schema_fp != rec_schema_fp {
        let version_moved = obs.schema_version.to_string() != rec_schema_version;
        failures.push(format!(
            "SCHEMA SOURCE CHANGED (src/storage/schema.rs)\n\
             \x20 recorded {rec_schema_fp} at SCHEMA_VERSION {rec_schema_version}\n\
             \x20 current  {} at SCHEMA_VERSION {}\n\
             \x20 {}\n\
             \x20 Decide first: does this change the DDL an existing database already has \
             (table, column, index, trigger, or FTS shape)?\n\
             \x20   YES -> bump SCHEMA_VERSION and add the matching `migrate_vN_to_vN+1` step. \
             `CREATE TABLE IF NOT EXISTS` is a NO-OP on an existing DB, so an edit to the \
             CREATE statement alone reaches new installs only — upgraders hit `no such \
             column` at query time.\n\
             \x20   NO  -> comment/format only; no bump needed.\n\
             \x20 Either way, record the new baseline:\n\
             \x20   {REGEN_CMD}",
            obs.schema_fp,
            obs.schema_version,
            if version_moved {
                "SCHEMA_VERSION already moved since the baseline — likely the deliberate case; \
                 this is just asking you to re-record."
            } else {
                "SCHEMA_VERSION is UNCHANGED — if the answer below is YES, upgraders get a \
                 database that no longer matches the code."
            },
        ));
    }

    assert!(
        failures.is_empty(),
        "{} version-gated source set(s) moved without a recorded decision.\n\n{}\n",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Negative control for the digest: the fingerprint must actually depend on
/// content, path, and file-set membership. A hasher that silently produced a
/// constant (empty read, swallowed error, unreachable loop) would make the test
/// above pass no matter how far the sources drifted.
#[test]
fn fingerprint_reacts_to_content_path_and_membership() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, b"fn extract() {}\n").unwrap();
    std::fs::write(&b, b"fn other() {}\n").unwrap();

    let base = vec![("src/a.rs".to_string(), a.clone())];
    let baseline = fingerprint(&base);

    std::fs::write(&a, b"fn extract() { /* changed */ }\n").unwrap();
    assert_ne!(
        baseline,
        fingerprint(&base),
        "content edit did not move the digest — the guard is inert"
    );

    std::fs::write(&a, b"fn extract() {}\n").unwrap();
    assert_eq!(
        baseline,
        fingerprint(&base),
        "restoring the bytes did not restore the digest — the digest is not a pure function \
         of content"
    );

    let renamed = vec![("src/renamed.rs".to_string(), a.clone())];
    assert_ne!(
        baseline,
        fingerprint(&renamed),
        "renaming a file did not move the digest — a moved extractor would slip through"
    );

    let with_extra = vec![("src/a.rs".to_string(), a), ("src/b.rs".to_string(), b)];
    assert_ne!(
        baseline,
        fingerprint(&with_extra),
        "adding a file did not move the digest — a NEW extractor would slip through"
    );
}

/// The recorded versions are parsed out of source text; this pins that parse to
/// the constants the binary actually links, so a reformat that made
/// `parse_const` read some other literal (or a stale copy) is caught here rather
/// than being written into the baseline as a wrong "recorded at" value.
#[test]
fn parsed_constants_match_linked_constants() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let obs = observe(&root);
    assert_eq!(
        obs.index_version,
        code_graph_mcp::domain::INDEX_VERSION,
        "parse_const read a different INDEX_VERSION than the crate links"
    );
    assert_eq!(
        obs.schema_version,
        code_graph_mcp::storage::schema::SCHEMA_VERSION,
        "parse_const read a different SCHEMA_VERSION than the crate links"
    );
}

/// The excluded-file rule is load-bearing (it decides what the guard can see),
/// so assert it matches exactly the files it was reasoned about — and that the
/// set it DOES cover is non-trivial.
#[test]
fn exclusion_covers_only_cfg_test_modules() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let covered = extraction_sources(&root);
    assert!(
        covered.len() >= 15,
        "extraction fingerprint covers only {} file(s) — the walk stopped early and the guard \
         is mostly blind",
        covered.len()
    );
    for (rel, _) in &covered {
        assert!(
            !is_test_only_module(rel),
            "{rel} should have been excluded as a test-only module"
        );
    }
    for rel in [
        "src/parser/treesitter.rs",
        "src/indexer/pipeline/resolve.rs",
    ] {
        assert!(
            covered.iter().any(|(r, _)| r == rel),
            "{rel} is missing from the extraction fingerprint — the walk does not reach it"
        );
    }
    // Both known test-only modules are declared `#[cfg(test)]`; if that ever
    // stops being true the exclusion is hiding production code.
    for (parent, file) in [
        (
            "src/parser/relations/mod.rs",
            "src/parser/relations/tests.rs",
        ),
        (
            "src/indexer/pipeline/mod.rs",
            "src/indexer/pipeline/tests.rs",
        ),
    ] {
        if root.join(file).exists() {
            let decl = read_to_string(&root.join(parent));
            assert!(
                decl.contains("#[cfg(test)]\nmod tests;"),
                "{file} is excluded from the fingerprint, but {parent} no longer declares it \
                 behind #[cfg(test)] — it may now ship in the release binary"
            );
        }
    }
}
