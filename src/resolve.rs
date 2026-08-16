//! Shared symbol-resolution / ambiguity detection used by BOTH the CLI
//! (`src/cli.rs`) and the MCP server (`src/mcp/server`). Single source of truth
//! for "is this bare name ambiguous?" so the two surfaces never give
//! contradictory verdicts.
//!
//! Audit 2026-06-03 #6: the CLI previously gated ambiguity on distinct *files*
//! (`detect_exact_ambiguity`), while MCP gated on the *count* of non-test
//! definitions (`disambiguate_symbol`). For two `fn new()` in the same file the
//! CLI called it unique and silently merged their call graphs, while MCP
//! correctly refused — the same input, opposite answers. Both now delegate here.

use anyhow::Result;
use rusqlite::Connection;

use crate::storage::queries::{self, NameCandidate};

/// Which surface is rendering the message — only affects flag/tool wording
/// (`--file` vs `file_path`, `show --node-id` vs `get_ast_node`), never the
/// ambiguity decision itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Mcp,
}

/// Detect whether a bare symbol `name` resolves to ≥2 non-test definitions.
/// Returns the candidate definitions when ambiguous (same-file OR cross-file),
/// `None` when unique or not found.
///
/// Same-file multi-defs (e.g. two `fn new()` in one module for different impl
/// blocks) count as ambiguous because `file_path` alone can't split them — they
/// need a `node_id` (see `find_references` / `get_ast_node`). This is the gate
/// MCP already used; the CLI now shares it.
///
/// `<external>` sentinels never count. They are not definitions the caller can
/// open or select, so counting one turns a symbol that RESOLVED into one that
/// refuses to and then offers `<external>` as the disambiguator — see
/// [`is_selectable_definition`].
pub fn detect_ambiguity(conn: &Connection, name: &str) -> Result<Option<Vec<NameCandidate>>> {
    let with_files = queries::get_nodes_with_files_by_name(conn, name)?;
    let non_test: Vec<NameCandidate> = with_files
        .iter()
        .filter(|nf| is_selectable_definition(&nf.file_path))
        .filter(|nf| !crate::domain::is_test_symbol(&nf.node.name, &nf.file_path))
        .map(|nf| NameCandidate {
            name: nf.node.name.clone(),
            file_path: nf.file_path.clone(),
            node_type: nf.node.node_type.clone(),
            node_id: nf.node.id,
            start_line: nf.node.start_line,
        })
        .collect();
    if non_test.len() > 1 {
        Ok(Some(non_test))
    } else {
        Ok(None)
    }
}

/// True when `file_path` names a definition the caller can actually act on.
///
/// The `<external>` pseudo-file holds sentinel nodes for imports that bind
/// outside the project. Every "which definitions does this name have?" surface
/// must drop them: a sentinel cannot be opened, cannot be passed back as
/// `--file` / `file_path`, and its mere presence flips a unique symbol to
/// ambiguous. Regression source: IDX v53 started binding Rust `use std::…` to
/// the sentinel, so any project `fn take` in a repo that also did
/// `use std::mem::take` stopped resolving in `callgraph` / `impact` /
/// `get_call_graph` — with `<external>` printed as the suggested fix.
/// `bind_calls_to_imported_targets` already carried this guard in SQL.
pub fn is_selectable_definition(file_path: &str) -> bool {
    file_path != crate::domain::EXTERNAL_FILE_PATH
}

/// True when the candidates span ≥2 distinct files (cross-file collision, which
/// `file_path` *can* disambiguate). False when every definition lives in one
/// file (same-file overloads, which only `node_id` can split).
pub fn spans_multiple_files(cands: &[NameCandidate]) -> bool {
    let mut files = std::collections::HashSet::new();
    for c in cands {
        files.insert(c.file_path.as_str());
    }
    files.len() > 1
}

/// Render candidate definitions as the canonical JSON suggestion shape shared by
/// every tool's ambiguity response (`name` / `file_path` / `type` / `node_id` /
/// `start_line`). Single-sourced so CLI `--json` and MCP stay byte-identical.
pub fn candidates_to_json(cands: &[NameCandidate]) -> Vec<serde_json::Value> {
    cands
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file_path": c.file_path,
                "type": c.node_type,
                "node_id": c.node_id,
                "start_line": c.start_line,
            })
        })
        .collect()
}

/// Build the accurate ambiguity message for `name` given its candidate defs.
///
/// Cross-file → advise the file selector (which works). Same-file overloads →
/// advise the `node_id` path via the node-oriented tools, because `get_call_graph`
/// / `impact` resolve by name and *cannot* split same-file defs; the file
/// selector would be a dead end. `surface` only swaps flag/tool names.
pub fn ambiguity_message(name: &str, cands: &[NameCandidate], surface: Surface) -> String {
    let n = cands.len();
    if spans_multiple_files(cands) {
        match surface {
            Surface::Cli => format!(
                "Ambiguous symbol '{name}': {n} matches in different files. Specify --file to disambiguate."
            ),
            Surface::Mcp => format!(
                "Ambiguous symbol '{name}': {n} matches in different files. Specify file_path to disambiguate."
            ),
        }
    } else {
        let file = cands.first().map(|c| c.file_path.as_str()).unwrap_or("");
        match surface {
            Surface::Cli => format!(
                "Ambiguous symbol '{name}': {n} definitions in the same file ({file}). \
                 callgraph/impact resolve by name and can't split same-file overloads — \
                 inspect a specific one with `show --node-id <N>` (node_ids below)."
            ),
            Surface::Mcp => format!(
                "Ambiguous symbol '{name}': {n} definitions in the same file ({file}). \
                 get_call_graph resolves by name and can't split same-file \
                 overloads — pass a node_id below to get_ast_node or find_references."
            ),
        }
    }
}

/// The MCP-side ambiguity RESPONSE, in one place.
///
/// Five sites emitted this and they had four different shapes and wordings
/// ("N matches." / "N matches found." / a CLI-only variant …), even though
/// [`ambiguity_message`] and [`candidates_to_json`] existed precisely so they
/// would not (2026-08-16 audit §四 / §六). A caller comparing two tools' answers
/// for the same symbol could not tell a wording difference from a verdict
/// difference. `emit_exact_ambiguity` in `cli::symbols` is the CLI counterpart.
pub fn ambiguity_response(name: &str, cands: &[NameCandidate]) -> serde_json::Value {
    serde_json::json!({
        "symbol": name,
        "error": ambiguity_message(name, cands, Surface::Mcp),
        "suggestions": candidates_to_json(cands).into_iter().take(5).collect::<Vec<_>>(),
    })
}

/// Outcome of fuzzy (substring-tolerant) name resolution.
pub enum FuzzyResolution {
    /// Exactly one candidate matched — use this name.
    Unique(String),
    /// Multiple candidates — the caller renders suggestions.
    Ambiguous(Vec<NameCandidate>),
    /// No candidates found.
    NotFound,
}

/// Resolve a possibly-partial symbol `name` to a unique symbol, a candidate
/// list, or nothing. Exact-name matches take precedence over substring matches:
/// without that, `find_functions_by_fuzzy_name("handle_tool")` returns the exact
/// `handle_tool` alongside `handle_tools_list` and every caller reports a false
/// "ambiguous".
///
/// Single-sourced for both surfaces. The CLI carried a hand-written twin
/// (`resolve_fuzzy_name_cli`) whose doc comment said "Mirrors MCP server's
/// resolve_fuzzy_name" — the same shape as the 2026-06-03 #6 incident this
/// module exists to prevent, where the two copies gave opposite answers for one
/// input. The MCP twin was regression-pinned; the CLI copy had zero tests.
pub fn resolve_fuzzy(conn: &Connection, name: &str) -> Result<FuzzyResolution> {
    let candidates: Vec<NameCandidate> = queries::find_functions_by_fuzzy_name(conn, name)?
        .into_iter()
        .filter(|c| is_selectable_definition(&c.file_path))
        .filter(|c| !crate::domain::is_test_symbol(&c.name, &c.file_path))
        .collect();

    // Prefer exact name matches if any exist.
    let exact: Vec<NameCandidate> = candidates
        .iter()
        .filter(|c| c.name == name)
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(FuzzyResolution::Unique(exact[0].name.clone()));
    }
    if exact.len() > 1 {
        // Same name in multiple files — still ambiguous, but scope the
        // suggestions to the exact matches so the caller sees the real collision.
        return Ok(FuzzyResolution::Ambiguous(exact));
    }
    match candidates.len() {
        0 => Ok(FuzzyResolution::NotFound),
        1 => Ok(FuzzyResolution::Unique(
            candidates.into_iter().next().unwrap().name,
        )),
        _ => Ok(FuzzyResolution::Ambiguous(candidates)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queries::NameCandidate;

    fn cand(name: &str, file: &str, id: i64, line: i64) -> NameCandidate {
        NameCandidate {
            name: name.to_string(),
            file_path: file.to_string(),
            node_type: "function".to_string(),
            node_id: id,
            start_line: line,
        }
    }

    #[test]
    fn same_file_overloads_do_not_span_multiple_files() {
        let cands = vec![cand("new", "lib.rs", 1, 5), cand("new", "lib.rs", 2, 9)];
        assert!(!spans_multiple_files(&cands));
    }

    #[test]
    fn cross_file_collision_spans_multiple_files() {
        let cands = vec![cand("open", "a.rs", 1, 5), cand("open", "b.rs", 2, 9)];
        assert!(spans_multiple_files(&cands));
    }

    #[test]
    fn cross_file_message_advises_file_selector() {
        let cands = vec![cand("open", "a.rs", 1, 5), cand("open", "b.rs", 2, 9)];
        // Stays byte-compatible with the pre-refactor MCP message + metrics
        // classifier fixture (mcp::metrics ErrKind::classify).
        assert_eq!(
            ambiguity_message("open", &cands, Surface::Mcp),
            "Ambiguous symbol 'open': 2 matches in different files. Specify file_path to disambiguate."
        );
        let cli = ambiguity_message("open", &cands, Surface::Cli);
        assert!(cli.contains("Specify --file"));
        assert!(
            !cli.contains("--node-id"),
            "cross-file: callgraph/impact have no --node-id"
        );
    }

    #[test]
    fn same_file_message_advises_node_id_path() {
        let cands = vec![cand("new", "lib.rs", 1, 5), cand("new", "lib.rs", 2, 9)];
        let cli = ambiguity_message("new", &cands, Surface::Cli);
        assert!(
            cli.contains("same file"),
            "must name the same-file case: {cli}"
        );
        assert!(
            cli.contains("--node-id"),
            "must point at the node_id path: {cli}"
        );
        let mcp = ambiguity_message("new", &cands, Surface::Mcp);
        assert!(mcp.contains("same file"));
        assert!(mcp.contains("node_id"));
        // Both surfaces keep the "Ambiguous symbol" prefix so metrics classify
        // them as Ambiguous, not Other.
        assert!(cli.starts_with("Ambiguous symbol"));
        assert!(mcp.starts_with("Ambiguous symbol"));
    }
}

#[cfg(test)]
mod fuzzy_tests {
    use super::*;
    use crate::storage::db::Database;
    use tempfile::TempDir;

    /// Build an index over `files` (name → source) and return the open DB.
    fn indexed(files: &[(&str, &str)]) -> (TempDir, TempDir, Database) {
        let project = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        for (name, src) in files {
            std::fs::write(project.path().join(name), src).unwrap();
        }
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        crate::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
        (project, db_dir, db)
    }

    /// The regression the MCP twin was pinned against (integration.rs) and the
    /// CLI copy was not: an EXACT match must win over substring matches, or
    /// every `callgraph handle_tool` in a codebase that also has
    /// `handle_tools_list` reports a false "ambiguous" and resolves nothing.
    #[test]
    fn exact_match_beats_substring_matches() {
        let (_p, _d, db) = indexed(&[(
            "src/a.rs",
            "pub fn handle_tool() {}\n\
             pub fn handle_tools_list() {}\n\
             pub fn handle_tool_call() {}\n",
        )]);

        match resolve_fuzzy(db.conn(), "handle_tool").unwrap() {
            FuzzyResolution::Unique(n) => assert_eq!(n, "handle_tool"),
            FuzzyResolution::Ambiguous(c) => panic!(
                "exact name must win over substring matches, got {:?}",
                c.iter().map(|c| &c.name).collect::<Vec<_>>()
            ),
            FuzzyResolution::NotFound => panic!("handle_tool is indexed"),
        }
    }

    #[test]
    fn substring_only_query_resolves_when_unique() {
        let (_p, _d, db) = indexed(&[("src/a.rs", "pub fn compute_checksum() {}\n")]);
        match resolve_fuzzy(db.conn(), "checksum").unwrap() {
            FuzzyResolution::Unique(n) => assert_eq!(n, "compute_checksum"),
            other => panic!(
                "a unique substring match must auto-promote, got {}",
                match other {
                    FuzzyResolution::Ambiguous(_) => "Ambiguous",
                    _ => "NotFound",
                }
            ),
        }
    }

    #[test]
    fn same_name_in_two_files_is_ambiguous_and_scoped_to_exact() {
        let (_p, _d, db) = indexed(&[
            ("src/a.rs", "pub fn parse() {}\npub fn parse_header() {}\n"),
            ("src/b.rs", "pub fn parse() {}\n"),
        ]);
        match resolve_fuzzy(db.conn(), "parse").unwrap() {
            FuzzyResolution::Ambiguous(cands) => {
                assert_eq!(cands.len(), 2, "got {:?}", cands);
                assert!(
                    cands.iter().all(|c| c.name == "parse"),
                    "suggestions must be scoped to the EXACT collisions, not the \
                     substring neighbourhood: {:?}",
                    cands.iter().map(|c| &c.name).collect::<Vec<_>>()
                );
                assert!(spans_multiple_files(&cands));
            }
            FuzzyResolution::Unique(n) => panic!("two definitions of `parse`, got Unique({n})"),
            FuzzyResolution::NotFound => panic!("parse is indexed"),
        }
    }

    #[test]
    fn unknown_name_is_not_found() {
        let (_p, _d, db) = indexed(&[("src/a.rs", "pub fn alpha() {}\n")]);
        assert!(matches!(
            resolve_fuzzy(db.conn(), "no_such_symbol_anywhere").unwrap(),
            FuzzyResolution::NotFound
        ));
    }
}

#[cfg(test)]
mod external_sentinel_tests {
    use super::*;

    /// Direct coverage for `is_selectable_definition`.
    ///
    /// Round 7 measured that NO test in the suite detects neutering this
    /// predicate: the by-name SQL guard (`EXCLUDE_EXTERNAL_BY_NAME`) runs first
    /// and no reachable input reaches here carrying an `<external>` path. That
    /// makes it dead weight today and load-bearing the moment the SQL exclusion
    /// is relaxed — the direction its own doc comment contemplates for `deps`
    /// disclosure. This test covers the FUNCTION, not its reachability; the
    /// end-to-end guard is
    /// `show_does_not_resolve_a_name_that_exists_only_as_an_import`.
    #[test]
    fn is_selectable_definition_rejects_only_the_external_sentinel() {
        assert!(!is_selectable_definition(crate::domain::EXTERNAL_FILE_PATH));
        for real in [
            "src/lib.rs",
            "src/a.rs",
            "external.rs",
            "src/<external>.rs",
            "",
        ] {
            assert!(
                is_selectable_definition(real),
                "{real:?} is a real path and must stay selectable"
            );
        }
    }
    use crate::storage::db::Database;
    use tempfile::TempDir;

    /// IDX v53 regression. Binding Rust `use std::…` to the `<external>` sentinel
    /// made the sentinel a second "definition" of the imported name, so a project
    /// symbol that resolved cleanly became ambiguous the moment ANY file imported
    /// the same name from std — and the disambiguation candidate offered was
    /// `<external>`, which the caller cannot pass back as `--file` / `file_path`.
    /// Risk names are exactly the ones the std-import change's own doc enumerates
    /// (swap / take / replace / read / write / spawn / min / max / exit / sleep).
    fn fixture() -> (TempDir, TempDir, Database) {
        let project = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(
            project.path().join("src/util.rs"),
            "pub fn take(v: &mut Vec<u8>) -> u8 { v.pop().unwrap() }\n\
             pub fn helper() { take(&mut vec![1u8]); }\n",
        )
        .unwrap();
        std::fs::write(
            project.path().join("src/other.rs"),
            "use std::mem::take;\npub fn run(a: &mut u8, b: &mut u8) { take(a, b); }\n",
        )
        .unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        crate::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
        (project, db_dir, db)
    }

    /// How many `<external>` sentinel nodes carry `name`. Read straight from the
    /// DB: every by-name query is what the fix filters, so asking through one
    /// would assert the precondition away.
    fn sentinel_count(db: &Database, name: &str) -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM nodes n JOIN files f ON f.id = n.file_id \
                 WHERE n.name = ?1 AND f.path = ?2",
                rusqlite::params![name, crate::domain::EXTERNAL_FILE_PATH],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// `impl Debug for S` with `Debug` imported from std yields a sentinel typed
    /// `trait` — the one shape that reaches `find_functions_by_fuzzy_name`.
    fn trait_sentinel_fixture() -> (TempDir, TempDir, Database) {
        let project = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(
            project.path().join("src/lib.rs"),
            "use std::fmt::Debug;\npub struct S;\nimpl Debug for S {}\n",
        )
        .unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        crate::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
        (project, db_dir, db)
    }

    #[test]
    fn external_sentinel_does_not_make_a_unique_symbol_ambiguous() {
        let (_p, _d, db) = fixture();

        // Precondition: the sentinel really exists, or this test proves nothing.
        // Asked of the DB directly — the by-name lookups are exactly what the fix
        // filters, so using one here would assert the sentinel away.
        assert!(
            sentinel_count(&db, "take") > 0,
            "fixture must produce an <external> sentinel for the std import"
        );

        assert!(
            detect_ambiguity(db.conn(), "take").unwrap().is_none(),
            "one project definition + one <external> sentinel is NOT ambiguous — \
             the sentinel is not a definition the caller can select"
        );

        // The `resolve_fuzzy` leg needs a TRAIT sentinel, not the module one
        // above: `find_functions_by_fuzzy_name` already carries
        // `n.type != 'module'`, so import sentinels never reach its filter and
        // asserting on `take` alone left that half of the fix vacuously
        // "covered". `implements` sentinels are typed `trait` and do reach it.
        let (_p2, _d2, db2) = trait_sentinel_fixture();
        assert!(
            sentinel_count(&db2, "Debug") > 0,
            "fixture must produce a TRAIT-typed <external> sentinel"
        );
        assert!(
            matches!(
                resolve_fuzzy(db2.conn(), "Debug").unwrap(),
                FuzzyResolution::NotFound
            ),
            "a trait sentinel is not a fuzzy candidate — it cannot be opened or selected"
        );

        match resolve_fuzzy(db.conn(), "take").unwrap() {
            FuzzyResolution::Unique(n) => assert_eq!(n, "take"),
            FuzzyResolution::Ambiguous(c) => panic!(
                "fuzzy resolution must ignore the sentinel too, got {:?}",
                c.iter().map(|c| &c.file_path).collect::<Vec<_>>()
            ),
            FuzzyResolution::NotFound => panic!("take is indexed"),
        }
    }

    #[test]
    fn two_real_definitions_are_still_ambiguous() {
        // Negative control: dropping the sentinel must not drop genuine
        // collisions — a filter that returned "never ambiguous" would satisfy
        // the assertions above just as well.
        let project = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/a.rs"), "pub fn take() {}\n").unwrap();
        std::fs::write(
            project.path().join("src/b.rs"),
            "use std::mem::take;\npub fn take() {}\n",
        )
        .unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        crate::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

        let cands = detect_ambiguity(db.conn(), "take")
            .unwrap()
            .expect("two real definitions must still be ambiguous");
        assert_eq!(cands.len(), 2, "got {cands:?}");
        assert!(
            cands
                .iter()
                .all(|c| c.file_path != crate::domain::EXTERNAL_FILE_PATH),
            "the sentinel must not appear among the suggestions: {:?}",
            cands.iter().map(|c| &c.file_path).collect::<Vec<_>>()
        );
    }
}
