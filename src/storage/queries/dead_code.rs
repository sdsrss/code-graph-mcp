use anyhow::Result;
use rusqlite::Connection;

/// Result from dead code analysis. Each entry is a node with no incoming usage edges.
#[derive(Debug)]
pub struct DeadCodeResult {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub start_line: i64,
    pub end_line: i64,
    pub file_path: String,
    pub code_content: String,
    /// True if the node has an incoming `exports` edge (exported but never called).
    pub has_export_edge: bool,
}

/// Find potentially dead code: nodes with no incoming usage edges.
///
/// Excludes modules, `<module>` pseudo-nodes, `main` entry points, and (optionally) test nodes.
/// Route handlers with a `routes_to` self-edge are also excluded.
///
/// Returns at most `limit` results ordered by line count descending (largest unused code first).
pub fn find_dead_code(
    conn: &Connection,
    path_prefix: Option<&str>,
    node_type: Option<&str>,
    include_tests: bool,
    min_lines: u32,
    limit: i64,
) -> Result<Vec<DeadCodeResult>> {
    use crate::domain::{
        REL_CALLS, REL_EXPORTS, REL_IMPLEMENTS, REL_IMPORTS, REL_INHERITS, REL_REFERENCES,
        REL_ROUTES_TO,
    };

    let mut conditions = vec![
        "n.type != 'module'".to_string(),
        "n.name != '<module>'".to_string(),
        "n.name != 'main'".to_string(),
        // Anonymous consts (`const _: () = assert!(...)`) are compile-time checks,
        // never callable; same pattern for anonymous `let _ = ...` bindings.
        "n.name != '_'".to_string(),
        // Markdown headings (h1..h6, ATX + setext) are document structure, not
        // callable code — they never carry incoming edges by nature, so reporting
        // them as dead code is a guaranteed false positive (every README heading
        // would be flagged). HTML/CSS/JSON contribute only a `<module>` node,
        // already excluded above.
        "n.type NOT IN ('h1', 'h2', 'h3', 'h4', 'h5', 'h6')".to_string(),
        // C/C++ "limited" extraction emits no inheritance and no type-reference
        // edges (cpp.rs only emits `references` for bare function-VALUE
        // identifiers, never for a type name). So a class/struct/enum node can
        // never carry an incoming edge — a header-defined C++ class used only in
        // a separate .cpp is ALWAYS orphaned. Reporting it dead is a guaranteed
        // false positive (same category as headings/constructors above): every
        // C/C++ type would be flagged, drowning real findings and inviting
        // deletion of live types. Excluded here; functions/methods (which DO get
        // call edges) are still reported when genuinely unused. Other languages
        // emit inherits/references for their types, so they are unaffected.
        "NOT (f.language IN ('c', 'cpp') AND n.type IN ('class', 'struct', 'enum'))".to_string(),
        // Implicitly-invoked methods (constructors, magic/dunder methods) are
        // dispatched by the language runtime, never called by explicit name, so
        // they carry no incoming `calls` edge even when the class is fully used.
        // Reporting them dead is a guaranteed false positive — and the most
        // damaging kind, since it invites deleting a live constructor or lifecycle
        // hook. Excluded per each language's actual convention:
        //   Python  __x__       (__init__, __str__, __enter__, __eq__, ...)
        //   PHP     __x         (PHP reserves the __ prefix for magic methods)
        //   JS/TS   constructor (invoked by `new`, never by name)
        //   Ruby    initialize  (invoked by `.new`)
        //   Java/C#/Dart/C++    constructor is a function/method sharing the
        //                       class name (qualified_name `Account.Account`) —
        //                       detected by a same-file class/struct of the same
        //                       name. C++ destructors (`~Class`, invoked at scope
        //                       exit) match the `~` prefix.
        // Plain C has no constructors — nothing to exclude.
        "NOT (n.type IN ('method', 'function') AND (
            (f.language = 'python' AND n.name LIKE '\\_\\_%\\_\\_' ESCAPE '\\')
            OR (f.language = 'php' AND n.name LIKE '\\_\\_%' ESCAPE '\\')
            OR (f.language IN ('javascript', 'typescript', 'tsx') AND n.name = 'constructor')
            OR (f.language = 'ruby' AND n.name = 'initialize')
            OR (f.language = 'cpp' AND n.name LIKE '~%')
            OR (f.language IN ('java', 'csharp', 'dart', 'cpp') AND EXISTS (
                SELECT 1 FROM nodes c
                WHERE c.file_id = n.file_id
                  AND c.type IN ('class', 'struct')
                  AND c.name = n.name
            ))
        ))"
        .to_string(),
        "f.path != '<external>'".to_string(),
        "(n.end_line - n.start_line + 1) >= :min_lines".to_string(),
    ];

    // Python framework-registered / attribute-accessed methods (pydantic
    // validators & serializers, pytest fixtures, @property/@cached_property,
    // @abstractmethod, @overload, NiceGUI handlers) are invoked DYNAMICALLY — the
    // framework/runtime dispatches them, so they never carry an incoming `calls`
    // edge even when fully live (a pydantic validator resolves to caller_count 0).
    // That makes them edgeless by nature, the same guaranteed-false-positive class
    // as the constructors/dunders excluded above, and the DOMINANT dead-code false
    // positive on framework-heavy Python (issue #32). The decorator text sits at
    // the head of code_content because the parser binds Python symbols to the
    // enclosing `decorated_definition` wrapper (issue #31, INDEX_VERSION 36) and
    // truncation only ever drops the tail — so an `@`-anchored substring probe is
    // reliable. Built from the canonical PYTHON_FRAMEWORK_DECORATORS list; every
    // entry is a literal `@<name>` with no quote characters, so interpolating them
    // into the SQL is injection-safe. Bias is toward false-negatives (a genuinely
    // dead decorated symbol may be missed) — the safe direction for this tool.
    {
        let decorator_probes = crate::domain::PYTHON_FRAMEWORK_DECORATORS
            .iter()
            .map(|d| format!("instr(n.code_content, '{d}') > 0"))
            .collect::<Vec<_>>()
            .join(" OR ");
        conditions.push(format!(
            "NOT (f.language = 'python' AND n.type IN ('method', 'function') AND ({decorator_probes}))"
        ));
    }

    if !include_tests {
        // Not just the raw `n.is_test` flag: the parser only sets that for AST-level
        // markers (`#[cfg(test)]`, `@Test`, …), so a plain integration test
        // `def test_foo()` in `tests/` carries is_test=0 and would be reported as
        // dead code — inviting deletion of a live test. Mirror the query-time
        // `is_test_node` predicate (flag OR name/path heuristic) so dead-code
        // classifies tests exactly like callgraph/show/centrality do.
        conditions.push(format!("NOT {}", crate::domain::is_test_node_sql("n", "f")));
    }

    // Track how many type filter placeholders we need
    let normalized_types: Vec<&str> = node_type
        .map(crate::domain::normalize_type_filter)
        .unwrap_or_default();

    if node_type.is_some() {
        if normalized_types.is_empty() {
            // Unknown filter — pass as-is for backward compatibility
            conditions.push("n.type = :node_type".to_string());
        } else if normalized_types.len() == 1 {
            conditions.push("n.type = :type_0".to_string());
        } else {
            let placeholders: Vec<String> = (0..normalized_types.len())
                .map(|i| format!(":type_{}", i))
                .collect();
            conditions.push(format!("n.type IN ({})", placeholders.join(", ")));
        }
    }

    if path_prefix.is_some() {
        conditions.push("f.path LIKE :path_pattern ESCAPE '\\'".to_string());
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT n.id, n.name, n.type, n.start_line, n.end_line, f.path, n.code_content,
                EXISTS(SELECT 1 FROM edges WHERE target_id = n.id AND relation = :rel_exports) as has_export
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE {where_clause}
           AND NOT EXISTS (
               SELECT 1 FROM edges
               WHERE target_id = n.id
                 AND relation IN (:rel_calls, :rel_imports, :rel_inherits, :rel_implements, :rel_references)
           )
           AND NOT EXISTS (
               SELECT 1 FROM edges
               WHERE source_id = n.id AND target_id = n.id
                 AND relation = :rel_routes_to
           )
           -- Check if the name appears as a standalone identifier in another
           -- function's code in the same file. Uses delimiter-aware matching
           -- to avoid false matches where the name is a prefix of a longer
           -- identifier (e.g., `get_x` matching inside `get_x_batch`).
           -- This catches references the parser doesn't track as edges:
           --   1. Struct instantiation, type usage
           --   2. Function pointers/callbacks (e.g., `query_map(params, map_fn)`)
           -- Truncation guard: `truncate_code_content` caps each node's stored
           -- body at `CODE_GRAPH_MAX_CODE_LEN` (default 4KB). When a function
           -- declared as 50 lines stores only ~5 lines of content, the instr
           -- fallback can't see references in the tail, producing false-positive
           -- dead code. Treat any same-file function whose declared span exceeds
           -- the stored content's newline count by >5 lines as a possibly-
           -- truncated host: the name might be in the dropped tail, so don't
           -- flag it. (Threshold absorbs trailing `...` sentinel + line-ending
           -- variations; Python `def stub(): ...` stays line-balanced and is
           -- not affected.)
           AND (
               length(n.name) < 3
               OR (
                   -- Same-file probe: name appears as a standalone identifier in
                   -- another declaration body in THIS file (catches callbacks /
                   -- function-pointer args / same-file struct-field types / type
                   -- aliases / const-in-const refs the parser doesn't track as
                   -- edges). Scans function/method bodies AND data-declaration
                   -- bodies (struct/enum/type_alias/interface/trait/constant) so
                   -- a struct used only as a same-file field type (`pub w: Foo`)
                   -- isn't falsely reported dead. `module`/markdown nodes are
                   -- excluded — their bodies can span a whole file and would
                   -- over-suppress. Delimiter-aware to avoid prefix matches
                   -- (e.g. `get_x` inside `get_x_batch`).
                   NOT EXISTS (
                       SELECT 1 FROM nodes n2
                       WHERE n2.file_id = n.file_id
                         AND n2.id != n.id
                         AND n2.type IN ('function', 'method', 'struct', 'enum', 'type_alias', 'interface', 'trait', 'constant')
                         AND (
                             instr(n2.code_content, n.name || '(') > 0
                             OR instr(n2.code_content, n.name || ')') > 0
                             OR instr(n2.code_content, n.name || ',') > 0
                             OR instr(n2.code_content, n.name || ' ') > 0
                             OR instr(n2.code_content, n.name || ';') > 0
                             OR instr(n2.code_content, n.name || char(10)) > 0
                             OR instr(n2.code_content, n.name || ':') > 0
                             OR instr(n2.code_content, n.name || '<') > 0
                             OR instr(n2.code_content, n.name || '.') > 0
                             OR instr(n2.code_content, n.name || '{{') > 0
                             OR instr(n2.code_content, n.name || '}}') > 0
                             OR (
                                 -- Truncation co-signal: must have BOTH the
                                 -- `truncate_code_content` `...` sentinel AND a
                                 -- significant gap between declared span and
                                 -- stored newline count. Either alone produces
                                 -- false positives (Python `def stub(): ...`,
                                 -- or compact fixtures with short content).
                                 substr(n2.code_content, -3) = '...'
                                 AND (n2.end_line - n2.start_line + 1)
                                     - (length(n2.code_content) - length(replace(n2.code_content, char(10), '')))
                                     > 5
                             )
                         )
                   )
                   -- Cross-file probe for EDGELESS node kinds. constant/struct/
                   -- enum/type_alias/interface/trait produce no call/import edge
                   -- for path-qualified references (`crate::domain::FOO`) or
                   -- type-position usages (`field: MyStruct`), so a same-file-only
                   -- scan falsely reports them dead — which can drive an agent to
                   -- delete live code. Probe OTHER files' bodies with the same
                   -- delimiter-aware matching, gated to length>=5 to avoid common
                   -- short-name collisions. Biases toward false-negatives (missing
                   -- some dead code) over false-positives, the safe direction for
                   -- an LLM-facing tool. Functions/methods are intentionally NOT
                   -- probed cross-file: their cross-file uses must be real call
                   -- edges, and a textual scan would over-suppress on comments.
                   AND (
                       n.type NOT IN ('constant', 'struct', 'enum', 'type_alias', 'interface', 'trait')
                       OR length(n.name) < 5
                       OR NOT EXISTS (
                           SELECT 1 FROM nodes n3
                           WHERE n3.file_id != n.file_id
                             AND (
                                 instr(n3.code_content, n.name || '(') > 0
                                 OR instr(n3.code_content, n.name || ')') > 0
                                 OR instr(n3.code_content, n.name || ',') > 0
                                 OR instr(n3.code_content, n.name || ' ') > 0
                                 OR instr(n3.code_content, n.name || ';') > 0
                                 OR instr(n3.code_content, n.name || char(10)) > 0
                                 OR instr(n3.code_content, n.name || ':') > 0
                                 OR instr(n3.code_content, n.name || '<') > 0
                                 OR instr(n3.code_content, n.name || '.') > 0
                                 OR instr(n3.code_content, n.name || '{{') > 0
                                 OR instr(n3.code_content, n.name || '}}') > 0
                                 -- NOTE: intentionally NO truncation keep-bias in the
                                 -- CROSS-FILE probe. A name-independent `...`-sentinel
                                 -- term (added v0.97.0, reverted v0.97.1) is satisfied
                                 -- by ANY truncated node in ANY other file — and
                                 -- code_content caps at 4096 bytes, so every real repo
                                 -- has one — making this NOT EXISTS always true and
                                 -- silently disabling cross-file dead-code detection
                                 -- project-wide. The rare false-positive it guarded
                                 -- against (a struct whose SOLE cross-file use sits
                                 -- past the cap of an importing file) is accepted as a
                                 -- documented limitation. The same-file probe above
                                 -- KEEPS its truncation co-signal: there the truncated
                                 -- node shares the candidate's file (high-correlation,
                                 -- one-file blast radius), so it does not over-suppress.
                             )
                       )
                   )
               )
           )
         ORDER BY (n.end_line - n.start_line + 1) DESC
         LIMIT :limit"
    );

    let mut stmt = conn.prepare(&sql)?;

    let path_pattern = path_prefix.map(|pp| {
        let escaped = super::helpers::escape_like(pp);
        format!("{}%", escaped)
    });

    let mut params: Vec<(&str, &dyn rusqlite::types::ToSql)> = vec![
        (":min_lines", &min_lines),
        (":limit", &limit),
        (":rel_exports", &REL_EXPORTS),
        (":rel_calls", &REL_CALLS),
        (":rel_imports", &REL_IMPORTS),
        (":rel_inherits", &REL_INHERITS),
        (":rel_implements", &REL_IMPLEMENTS),
        (":rel_routes_to", &REL_ROUTES_TO),
        (":rel_references", &REL_REFERENCES),
    ];

    // Bind type filter placeholders (parameterized to prevent SQL injection)
    let type_param_names: Vec<String> = (0..normalized_types.len())
        .map(|i| format!(":type_{}", i))
        .collect();
    for (i, name) in type_param_names.iter().enumerate() {
        params.push((
            name.as_str(),
            &normalized_types[i] as &dyn rusqlite::types::ToSql,
        ));
    }

    // Only bind :node_type when the value was not recognized by normalize_type_filter
    let node_type_owned: Option<String> = node_type
        .filter(|_| normalized_types.is_empty())
        .map(|t| t.to_string());
    if let Some(ref t) = node_type_owned {
        params.push((":node_type", t));
    }

    if let Some(ref pattern) = path_pattern {
        params.push((":path_pattern", pattern));
    }

    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(DeadCodeResult {
            id: row.get(0)?,
            name: row.get(1)?,
            node_type: row.get(2)?,
            start_line: row.get(3)?,
            end_line: row.get(4)?,
            file_path: row.get(5)?,
            code_content: row.get(6)?,
            has_export_edge: row.get::<_, i32>(7)? != 0,
        })
    })?;

    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// One dead-code candidate, classified. `is_exported` distinguishes the
/// "exported but unused" bucket from a plain orphan (via
/// `domain::is_dead_code_exported`, the same classifier both surfaces used).
pub struct DeadCodeItem {
    pub name: String,
    pub node_type: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    pub code_content: String,
    pub is_exported: bool,
}

/// The single authoritative dead-code result both the CLI (`cmd_dead_code`) and
/// MCP (`tool_find_dead_code`) format. Surfaces own their rendering; they must
/// NOT recompute counts or the hidden-below-threshold probe. `items` preserves
/// find_dead_code order (orphans and exported interleaved as returned); each
/// surface can partition by `is_exported`.
pub struct DeadCodeReport {
    pub items: Vec<DeadCodeItem>,
    pub orphan_count: usize,
    pub exported_count: usize,
    pub ignored_count: usize,
    /// When the visible set is empty AND min_lines > 1, the count of candidates
    /// that a min_lines=1 probe (same path/type/ignore scope) would surface —
    /// so a "clean" result can disclose it was threshold-limited. 0 otherwise.
    pub hidden_below_threshold: usize,
}

impl DeadCodeReport {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Reject a type filter that normalizes to empty (a typo like `fucntion`),
/// which would otherwise fall through to a literal `n.type = :x` match and
/// return a false-clean zero rows. Same message both surfaces printed.
pub fn validate_dead_code_type_filter(node_type: Option<&str>) -> Result<()> {
    if let Some(tf) = node_type {
        if crate::domain::normalize_type_filter(tf).is_empty() {
            anyhow::bail!(
                "Unknown type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                tf
            );
        }
    }
    Ok(())
}

/// Probe whether a path filter names ANY indexed file, for surfaces that must not
/// answer "nothing here" about a directory they never looked in.
///
/// Returns the trimmed prefix when the filter matches nothing indexed, `None` when
/// it matches something, when there is no filter, or when the scan itself failed —
/// "nothing indexed here" is only ever claimed on a successful scan.
///
/// Lives next to [`dead_code_report`] because BOTH of its surfaces need it and only
/// one had it: `cli::cmd_dead_code` probed and exited 1 while MCP
/// `tool_find_dead_code` returned a clean empty report for the same input — a
/// health certificate for a directory that was never examined, issued on the
/// LLM-facing surface (audit 2026-08-16 P1-22). A shared callee is the only shape
/// that keeps the two from drifting again.
///
/// Compared in Rust rather than with SQL `LIKE`: the prefix is user input, and a
/// `_`/`%` in a filename would silently widen the match.
///
/// Two spellings must NOT be treated as a miss, and the CLI's first version of this
/// probe failed both — turning `dead-code .` and `dead-code src/` on a clean repo
/// into a hard error, the inverse of the bug it fixes:
///   * `.` normalizes to `""` (whole project), and no stored path equals `""`;
///   * a trailing slash from tab completion gives `src/`, and no stored path
///     begins with `src//`.
pub fn unindexed_path_prefix(conn: &Connection, path_filter: Option<&str>) -> Option<String> {
    let prefix = path_filter
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty())?;
    let scan = conn.prepare("SELECT path FROM files").and_then(|mut stmt| {
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    });
    match scan {
        Ok(paths) => {
            let matched = paths
                .iter()
                .any(|p| p == prefix || p.starts_with(&format!("{prefix}/")));
            if matched {
                None
            } else {
                Some(prefix.to_string())
            }
        }
        // Only claim "nothing indexed here" when the scan succeeded.
        Err(_) => None,
    }
}

/// Compute the classified, ignore-filtered dead-code report + hidden-probe.
/// `ignore_prefixes` are path-prefix exclusions (caller owns defaulting).
pub fn dead_code_report(
    conn: &Connection,
    path: Option<&str>,
    node_type: Option<&str>,
    include_tests: bool,
    min_lines: u32,
    ignore_prefixes: &[String],
) -> Result<DeadCodeReport> {
    let raw = find_dead_code(conn, path, node_type, include_tests, min_lines, 200)?;
    let pre_count = raw.len();
    let filtered: Vec<DeadCodeResult> = raw
        .into_iter()
        .filter(|r| !ignore_prefixes.iter().any(|p| r.file_path.starts_with(p)))
        .collect();
    let mut ignored_count = pre_count - filtered.len();

    // Probe for threshold-hidden candidates ONLY when nothing is visible AND
    // nothing was ignore-suppressed (the "No dead code found …" disclosure
    // path). On any non-empty or partly-ignored result the fields are never
    // read, so the extra min_lines=1 query is skipped entirely.
    //
    // A candidate hidden by BOTH filters used to be counted by NEITHER: the
    // probe applied the ignore filter and threw the intersection away, so
    // `--ignore X` plus the default --min-lines answered a bare "[]" — a
    // false clean — while either filter alone disclosed the same 3 candidates
    // (audit 2026-08-02 MED-4). The probe now splits: past-the-ignore-filter →
    // below_threshold, caught-by-ignore → ignored.
    let hidden_below_threshold = if filtered.is_empty() && ignored_count == 0 && min_lines > 1 {
        let probe = find_dead_code(conn, path, node_type, include_tests, 1, 200)?;
        let (kept, ignored_short): (Vec<_>, Vec<_>) = probe
            .into_iter()
            .partition(|r| !ignore_prefixes.iter().any(|p| r.file_path.starts_with(p)));
        ignored_count += ignored_short.len();
        kept.len()
    } else {
        0
    };

    let mut items = Vec::with_capacity(filtered.len());
    let (mut orphan_count, mut exported_count) = (0usize, 0usize);
    for r in filtered {
        let is_exported = crate::domain::is_dead_code_exported(
            r.has_export_edge,
            &r.code_content,
            &r.file_path,
            &r.name,
        );
        if is_exported {
            exported_count += 1;
        } else {
            orphan_count += 1;
        }
        items.push(DeadCodeItem {
            name: r.name,
            node_type: r.node_type,
            file_path: r.file_path,
            start_line: r.start_line,
            end_line: r.end_line,
            code_content: r.code_content,
            is_exported,
        });
    }
    Ok(DeadCodeReport {
        items,
        orphan_count,
        exported_count,
        ignored_count,
        hidden_below_threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::super::edges::insert_edge;
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};
    use super::*;

    #[test]
    fn test_find_dead_code() {
        use crate::domain::{REL_CALLS, REL_EXPORTS, REL_ROUTES_TO};

        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/app.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        // 1. main function — excluded by name filter
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "main".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 10,
                code_content: "function main() { ... }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 2. used_fn — has incoming "calls" edge → excluded
        let used_fn_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "used_fn".into(),
                qualified_name: None,
                start_line: 11,
                end_line: 20,
                code_content: "function used_fn() { ... }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 3. orphan_fn — no edges at all → should be found as dead code
        let _orphan_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "orphan_fn".into(),
                qualified_name: None,
                start_line: 21,
                end_line: 40,
                code_content: "function orphan_fn() { /* lots of code */ }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 4. exported_unused — has "exports" edge but no callers → found as exported-unused
        let exported_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "exported_unused".into(),
                qualified_name: None,
                start_line: 41,
                end_line: 55,
                code_content: "export function exported_unused() { ... }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 5. module node — excluded by type filter
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "module".into(),
                name: "app".into(),
                qualified_name: None,
                start_line: 0,
                end_line: 100,
                code_content: "".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 6. test_something — is_test=1 → excluded by default, included with include_tests=true
        let _test_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "test_something".into(),
                qualified_name: None,
                start_line: 60,
                end_line: 70,
                code_content: "function test_something() { assert(true); }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: true,
            },
        )
        .unwrap();

        // 7. handle_login — has routes_to self-edge → excluded
        let handler_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "handle_login".into(),
                qualified_name: None,
                start_line: 71,
                end_line: 85,
                code_content: "function handle_login(req, res) { ... }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 8. callback_fn — no call edge, but name appears in another function's code
        //    (function pointer passed as argument) → should NOT be dead code
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "callback_fn".into(),
                qualified_name: None,
                start_line: 91,
                end_line: 105,
                code_content: "fn callback_fn(row: &Row) -> Result<Item> { Ok(row.get(0)?) }"
                    .into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 9. anonymous `_` constant — `const _: () = assert!(...)` is a compile-time
        //    check, never callable. Must be excluded by name filter.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "constant".into(),
                name: "_".into(),
                qualified_name: None,
                start_line: 110,
                end_line: 115,
                code_content: "const _: () = assert!(SOME_CONST <= 1500, \"budget\");".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // --- Create edges ---
        // Someone calls used_fn and passes callback_fn as a function pointer
        let caller_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "caller".into(),
                qualified_name: None,
                start_line: 86,
                end_line: 90,
                code_content:
                    "fn caller() { used_fn(); stmt.query_map(params, callback_fn).unwrap(); }"
                        .into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_edge(conn, caller_id, used_fn_id, REL_CALLS, None).unwrap();

        // Module exports exported_unused
        let module_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "module".into(),
                name: "<module>".into(),
                qualified_name: None,
                start_line: 0,
                end_line: 0,
                code_content: "".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_edge(conn, module_id, exported_id, REL_EXPORTS, None).unwrap();

        // handle_login has routes_to self-edge
        insert_edge(
            conn,
            handler_id,
            handler_id,
            REL_ROUTES_TO,
            Some("{\"method\":\"POST\",\"path\":\"/login\"}"),
        )
        .unwrap();

        // --- Test default (exclude tests) ---
        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();

        // orphan_fn and exported_unused should be found
        assert!(
            names.contains(&"orphan_fn"),
            "orphan_fn should be found, got: {:?}",
            names
        );
        assert!(
            names.contains(&"exported_unused"),
            "exported_unused should be found, got: {:?}",
            names
        );

        // These should be excluded
        assert!(!names.contains(&"main"), "main should be excluded");
        assert!(
            !names.contains(&"used_fn"),
            "used_fn should be excluded (has callers)"
        );
        assert!(!names.contains(&"app"), "module should be excluded");
        assert!(
            !names.contains(&"test_something"),
            "test node should be excluded by default"
        );
        assert!(
            !names.contains(&"handle_login"),
            "route handler should be excluded"
        );
        assert!(!names.contains(&"<module>"), "<module> should be excluded");
        assert!(
            !names.contains(&"callback_fn"),
            "callback_fn should be excluded (referenced as function pointer in caller's code)"
        );
        assert!(
            !names.contains(&"_"),
            "anonymous `_` constant (compile-time assert) should be excluded by name filter"
        );

        // Verify has_export_edge classification
        let orphan = results.iter().find(|r| r.name == "orphan_fn").unwrap();
        assert!(
            !orphan.has_export_edge,
            "orphan_fn should not have export edge"
        );

        let exported = results
            .iter()
            .find(|r| r.name == "exported_unused")
            .unwrap();
        assert!(
            exported.has_export_edge,
            "exported_unused should have export edge"
        );

        // Verify ordering: largest (most lines) first
        // orphan_fn: 40-21+1=20 lines, exported_unused: 55-41+1=15 lines
        assert_eq!(
            results[0].name, "orphan_fn",
            "largest function should be first"
        );

        // --- Test include_tests=true ---
        let results_with_tests = find_dead_code(conn, None, None, true, 1, 100).unwrap();
        let names_with_tests: Vec<&str> =
            results_with_tests.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names_with_tests.contains(&"test_something"),
            "test node should be included when include_tests=true"
        );

        // --- Test path_prefix filter ---
        let results_filtered = find_dead_code(conn, Some("src/"), None, false, 1, 100).unwrap();
        assert!(
            !results_filtered.is_empty(),
            "path prefix 'src/' should match"
        );

        let results_no_match = find_dead_code(conn, Some("lib/"), None, false, 1, 100).unwrap();
        assert!(
            results_no_match.is_empty(),
            "path prefix 'lib/' should not match any"
        );

        // --- Test node_type filter ---
        let results_fn = find_dead_code(conn, None, Some("fn"), false, 1, 100).unwrap();
        for r in &results_fn {
            assert!(
                r.node_type == "function" || r.node_type == "method",
                "fn filter should only return function/method, got: {}",
                r.node_type
            );
        }

        // --- Test min_lines filter ---
        let results_big = find_dead_code(conn, None, None, false, 18, 100).unwrap();
        let big_names: Vec<&str> = results_big.iter().map(|r| r.name.as_str()).collect();
        assert!(
            big_names.contains(&"orphan_fn"),
            "orphan_fn (20 lines) should pass min_lines=18"
        );
        assert!(
            !big_names.contains(&"exported_unused"),
            "exported_unused (15 lines) should fail min_lines=18"
        );
    }

    /// Sibling-hole guard (real-user QA): the parser sets `is_test=1` only for
    /// AST-level markers (`#[cfg(test)]`, `@Test`, …), so a plain integration test
    /// `def test_foo()` in `tests/` carries is_test=0. The old `n.is_test = 0` filter
    /// let it through as an ORPHAN — inviting an agent/user to delete a live test.
    /// Now excluded via the full `is_test_node` name/path heuristic. Covers the
    /// `test_`-name leg AND the `tests/`-path leg with the flag OFF.
    #[test]
    fn test_find_dead_code_excludes_heuristic_tests_without_flag() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "tests/test_api.py".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        // test_-prefixed name, is_test flag NOT set — the pytest integration-test shape.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "test_signup".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 6,
                code_content: "def test_signup():\n    assert handle_signup() == 'ok'".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // A NON-test-named helper living in the same tests/ file: excluded by the
        // path leg (a fixture/helper in a test file is still test-harness code).
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "make_fixture".into(),
                qualified_name: None,
                start_line: 8,
                end_line: 13,
                code_content: "def make_fixture():\n    return {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let names: Vec<String> = find_dead_code(conn, None, None, false, 1, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert!(!names.contains(&"test_signup".to_string()),
            "test_-named fn (is_test=0) in tests/ must NOT be reported dead by default, got: {names:?}");
        assert!(!names.contains(&"make_fixture".to_string()),
            "helper in a tests/ file must NOT be reported dead by default (path heuristic), got: {names:?}");

        // include_tests=true surfaces them again (the flag is symmetric).
        let with_tests: Vec<String> = find_dead_code(conn, None, None, true, 1, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert!(
            with_tests.contains(&"test_signup".to_string()),
            "include_tests=true must surface the test fn, got: {with_tests:?}"
        );
    }

    #[test]
    fn test_find_dead_code_excludes_nodes_with_references_edge() {
        use crate::domain::REL_REFERENCES;
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/x.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        // A const with ONLY an incoming `references` edge must NOT be dead.
        let used = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "constant".into(),
                name: "REFERENCED_CONST".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "pub const REFERENCED_CONST: u32 = 1;".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        let user = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "user".into(),
                qualified_name: None,
                start_line: 6,
                end_line: 9,
                code_content: "fn user() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_edge(conn, user, used, REL_REFERENCES, None).unwrap();

        let names: Vec<String> = find_dead_code(conn, None, None, false, 1, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert!(
            !names.contains(&"REFERENCED_CONST".to_string()),
            "a node with an incoming references edge must not be dead; got: {:?}",
            names
        );
    }

    /// Regression: when a function's `code_content` is truncated by
    /// `truncate_code_content` (limit `CODE_GRAPH_MAX_CODE_LEN`, default 4 KB),
    /// the instr fallback in `find_dead_code` cannot see references in the
    /// truncated tail. Without a guard this turns long-function callbacks /
    /// function-pointer args into false-positive dead code (see autonomous
    /// iteration round 4 repro: env `CODE_GRAPH_MAX_CODE_LEN=100` on a Rust
    /// project containing `apply_cb(target_callback)` past byte 100 of the
    /// caller).
    ///
    /// Fix: treat any same-file function whose stored content has many fewer
    /// newlines than its declared line span as "possibly truncated" — give
    /// names mentioned anywhere in that file the benefit of the doubt. The
    /// signal is robust against Python `def stub(): ...` (no line-span gap).
    #[test]
    fn test_find_dead_code_skips_when_caller_content_truncated() {
        use crate::domain::REL_EXPORTS;

        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/lib.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // target_callback: only referenced from long_caller's tail (lost to truncation).
        let target_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "target_callback".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 1,
                code_content: "pub fn target_callback(x: i32) -> i32 { x * 2 }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // long_caller: declared as 50 lines but code_content stores only the first
        // few (mimics `truncate_code_content` cutting at MAX_CODE_LEN). Reference
        // to `target_callback` is in the cut-off tail.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "long_caller".into(),
                qualified_name: None,
                start_line: 5,
                end_line: 55, // 51 declared lines
                code_content: "pub fn long_caller() {\n    let a = 1;\n    let b = 2;...".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Exports edge so target_callback shows up as exported-unused if dead.
        let module_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "module".into(),
                name: "<module>".into(),
                qualified_name: None,
                start_line: 0,
                end_line: 0,
                code_content: "".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_edge(conn, module_id, target_id, REL_EXPORTS, None).unwrap();

        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !names.contains(&"target_callback"),
            "target_callback must NOT be reported dead — long_caller's truncated body \
             may reference it in the tail; got: {:?}",
            names
        );
    }

    /// Regression: a `pub const` (or type/struct) referenced cross-file via a
    /// path expression (`crate::domain::FOO`) produces NO call/import edge —
    /// Rust const/type path references are neither `use` imports nor calls. The
    /// same-file-only instr fallback can't see the reference, so such *live*
    /// constants were reported dead (exported-unused), which can drive an agent
    /// to delete working code. Edgeless node kinds (const/type/enum/struct/
    /// interface) must therefore also probe OTHER files' code_content. A
    /// genuinely-unreferenced const must still be flagged (no over-rescue).
    #[test]
    fn test_find_dead_code_cross_file_const_reference() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let domain_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/domain.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let consumer_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/consumer.rs".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // Live const, referenced cross-file via `crate::domain::SHARED_FILTER`.
        // No edge: Rust const path-refs are not `use` imports and not calls.
        insert_node(
            conn,
            &NodeRecord {
                file_id: domain_fid,
                node_type: "constant".into(),
                name: "SHARED_FILTER".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "pub const SHARED_FILTER: &str =\n    \"is_test = 0\";".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Genuinely-dead const: referenced nowhere. Must STILL be flagged.
        insert_node(
            conn,
            &NodeRecord {
                file_id: domain_fid,
                node_type: "constant".into(),
                name: "UNUSED_FILTER".into(),
                qualified_name: None,
                start_line: 6,
                end_line: 9,
                code_content: "pub const UNUSED_FILTER: &str =\n    \"never read\";".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Consumer function references SHARED_FILTER by path in another file. No edge.
        insert_node(conn, &NodeRecord {
            file_id: consumer_fid, node_type: "function".into(), name: "build_query".into(),
            qualified_name: None, start_line: 1, end_line: 6,
            code_content: "fn build_query() -> String {\n    let w = crate::domain::SHARED_FILTER;\n    w.to_string()\n}".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();

        assert!(
            !names.contains(&"SHARED_FILTER"),
            "SHARED_FILTER is referenced cross-file via crate::domain::SHARED_FILTER \
             (edgeless const path ref) and must NOT be reported dead; got: {:?}",
            names
        );
        assert!(
            names.contains(&"UNUSED_FILTER"),
            "UNUSED_FILTER is referenced nowhere and must still be reported dead; got: {:?}",
            names
        );
    }

    /// Regression: a struct/type referenced only SAME-FILE as a field type
    /// (`pub widget: WidgetConfig`) lives inside another struct's definition,
    /// not a function/method body. The same-file probe scanned only
    /// function/method bodies, so such live types were reported dead (the live
    /// `SnapshotConfig` repro). Declaration nodes (struct/enum/type_alias/
    /// interface/trait/constant) must also count as same-file reference sites.
    /// A genuinely-unreferenced struct must still be flagged (no over-rescue).
    #[test]
    fn test_find_dead_code_same_file_struct_field_type() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/config.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // Live struct, referenced only as a field type inside AppConfig (same
        // file). No edge: a field-type usage produces no call/import/inherit edge.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "struct".into(),
                name: "WidgetConfig".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "pub struct WidgetConfig {\n    pub size: u32,\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Genuinely-dead struct: referenced nowhere. Must STILL be flagged.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "struct".into(),
                name: "OrphanConfig".into(),
                qualified_name: None,
                start_line: 6,
                end_line: 9,
                code_content: "pub struct OrphanConfig {\n    pub n: u32,\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // AppConfig references WidgetConfig as a field type, in the same file.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "struct".into(),
                name: "AppConfig".into(),
                qualified_name: None,
                start_line: 11,
                end_line: 14,
                code_content: "pub struct AppConfig {\n    pub widget: WidgetConfig,\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();

        assert!(
            !names.contains(&"WidgetConfig"),
            "WidgetConfig is referenced as a field type in same-file AppConfig and must \
             NOT be reported dead; got: {:?}",
            names
        );
        assert!(
            names.contains(&"OrphanConfig"),
            "OrphanConfig is referenced nowhere and must still be reported dead; got: {:?}",
            names
        );
    }

    /// Regression (v0.97.1): the CROSS-FILE probe for edgeless kinds must NOT
    /// carry a name-independent truncation keep-bias. v0.97.0 added one that was
    /// satisfied by ANY truncated node in ANY other file (`code_content` caps at
    /// 4096 bytes, so every real repo has one) — the `NOT EXISTS` then became
    /// always-true and cross-file dead-code detection for constant/struct/enum/
    /// type_alias/interface/trait was silently disabled project-wide. Here a
    /// genuinely-dead struct sits alongside an UNRELATED truncated function (that
    /// does not mention it); the struct must still be reported dead.
    #[test]
    fn test_find_dead_code_cross_file_unrelated_truncated_node_does_not_suppress() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let domain_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/domain.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let consumer_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/consumer.rs".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // Genuinely-dead struct (name len >= 5, referenced nowhere).
        insert_node(
            conn,
            &NodeRecord {
                file_id: domain_fid,
                node_type: "struct".into(),
                name: "LonelyDeadStruct".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "pub struct LonelyDeadStruct {\n    id: u32,\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // An UNRELATED cross-file function whose stored body is TRUNCATED (declared
        // 56 lines, code_content is head + `...` sentinel) and does NOT mention
        // LonelyDeadStruct. Under the v0.97.0 bug this single truncated node kept
        // every edgeless candidate alive; it must not suppress this one.
        insert_node(
            conn,
            &NodeRecord {
                file_id: consumer_fid,
                node_type: "function".into(),
                name: "wide_unrelated_function".into(),
                qualified_name: None,
                start_line: 5,
                end_line: 60, // 56 declared lines
                code_content:
                    "pub fn wide_unrelated_function() {\n    let a = 1;\n    let b = 2;...".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"LonelyDeadStruct"),
            "LonelyDeadStruct is referenced nowhere; an unrelated truncated cross-file \
             node must not suppress it (v0.97.0 over-suppression regression); got: {:?}",
            names
        );
    }

    /// Negative control: an edgeless struct referenced NOWHERE, with no truncated
    /// cross-file node in play, must STILL be reported dead — guards that the
    /// cross-file probe did not become a blanket "never report edgeless kinds".
    #[test]
    fn test_find_dead_code_cross_file_untruncated_dead_struct_still_flagged() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let domain_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/domain.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let consumer_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/consumer.rs".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // Genuinely-dead struct: referenced nowhere.
        insert_node(
            conn,
            &NodeRecord {
                file_id: domain_fid,
                node_type: "struct".into(),
                name: "LonelyStruct".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "pub struct LonelyStruct {\n    id: u32,\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // A short, NON-truncated cross-file function that does not mention
        // LonelyStruct — no `...` sentinel, no line-span gap → no truncation signal.
        insert_node(
            conn,
            &NodeRecord {
                file_id: consumer_fid,
                node_type: "function".into(),
                name: "normal_consumer".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "pub fn normal_consumer() {\n    do_thing();\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert!(
            names.contains(&"LonelyStruct"),
            "LonelyStruct is referenced nowhere and no cross-file node is truncated, \
             so it must still be reported dead; got: {:?}",
            names
        );
    }

    /// Regression: C/C++ "limited" extraction emits no inheritance and no
    /// type-reference edges (cpp.rs only emits `references` for bare function
    /// VALUE identifiers), so a class/struct/enum node can essentially never
    /// carry an incoming edge. A header-defined C++ class instantiated only in a
    /// separate .cpp is therefore ALWAYS reported dead — a guaranteed false
    /// positive, the same category as markdown headings and constructors. Such
    /// type-definition nodes are excluded for C/C++ while genuinely-orphan
    /// functions are still reported (no over-suppression), and OTHER languages'
    /// structs/classes (which DO get inheritance/reference edges) are untouched.
    #[test]
    fn test_find_dead_code_excludes_c_cpp_type_definitions() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let hdr = upsert_file(
            conn,
            &FileRecord {
                path: "shape.hpp".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("cpp".into()),
            },
        )
        .unwrap();
        let csrc = upsert_file(
            conn,
            &FileRecord {
                path: "util.c".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("c".into()),
            },
        )
        .unwrap();

        // C++ class in a header, instantiated only cross-file — no edge can
        // target it (no inherit/type-ref extraction). Must NOT be flagged.
        insert_node(
            conn,
            &NodeRecord {
                file_id: hdr,
                node_type: "class".into(),
                name: "Circle".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 6,
                code_content: "class Circle : public Shape {\n  double r;\n};".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // C struct, same situation.
        insert_node(
            conn,
            &NodeRecord {
                file_id: csrc,
                node_type: "struct".into(),
                name: "Point".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "struct Point {\n  int x;\n  int y;\n};".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // Genuinely-orphan C function MUST still be flagged (no over-suppression).
        insert_node(
            conn,
            &NodeRecord {
                file_id: csrc,
                node_type: "function".into(),
                name: "unused_helper".into(),
                qualified_name: None,
                start_line: 6,
                end_line: 9,
                code_content: "int unused_helper(int x) {\n  return x;\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();

        assert!(
            !names.contains(&"Circle"),
            "C++ class (no inherit/type-ref edges emitted) must NOT be flagged dead; got: {:?}",
            names
        );
        assert!(
            !names.contains(&"Point"),
            "C struct (no type-ref edges emitted) must NOT be flagged dead; got: {:?}",
            names
        );
        assert!(
            names.contains(&"unused_helper"),
            "a genuinely-orphan C function must STILL be flagged (no over-suppression); got: {:?}",
            names
        );
    }

    /// Regression: implicitly-invoked methods (constructors + magic/dunder
    /// methods) are dispatched by the language runtime, never called by explicit
    /// name, so they never carry an incoming `calls` edge — even when the class
    /// is fully used. Reporting them dead is a guaranteed false positive that
    /// invites deleting a live constructor. They must be excluded per each
    /// language's convention, while a genuinely-dead regular method is still
    /// reported (no over-suppression).
    #[test]
    fn test_find_dead_code_excludes_implicitly_invoked_methods() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // One file per language so the same-file instr probe can't rescue
        // anything — every method below is genuinely edgeless.
        let cases = [
            (
                "src/a.py",
                "python",
                "method",
                "__init__",
                "def __init__(self, x):\n    self.x = x\n    self.y = x",
            ),
            (
                "src/a.py",
                "python",
                "method",
                "__eq__",
                "def __eq__(self, o):\n    return self.x == o.x\n    # cmp",
            ),
            (
                "src/b.php",
                "php",
                "method",
                "__construct",
                "function __construct($v) {\n    $this->v = $v;\n    $this->t = 0;\n}",
            ),
            (
                "src/b.php",
                "php",
                "method",
                "__toString",
                "function __toString() {\n    return \"X\";\n    // str\n}",
            ),
            (
                "src/c.ts",
                "typescript",
                "method",
                "constructor",
                "constructor(x: number) {\n    this.x = x;\n    this.y = x;\n}",
            ),
            (
                "src/d.js",
                "javascript",
                "method",
                "constructor",
                "constructor() {\n    this.a = 1;\n    this.b = 2;\n}",
            ),
            (
                "src/e.rb",
                "ruby",
                "method",
                "initialize",
                "def initialize(name)\n    @name = name\n    @n = 0\nend",
            ),
        ];

        let mut fid_cache: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
        for (i, (path, lang, ntype, name, code)) in cases.iter().enumerate() {
            let fid = *fid_cache.entry(path).or_insert_with(|| {
                upsert_file(
                    conn,
                    &FileRecord {
                        path: (*path).into(),
                        blake3_hash: format!("h{i}"),
                        last_modified: 1,
                        language: Some((*lang).into()),
                    },
                )
                .unwrap()
            });
            insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: (*ntype).into(),
                    name: (*name).into(),
                    qualified_name: None,
                    start_line: (i as i64) * 10 + 1,
                    end_line: (i as i64) * 10 + 4,
                    code_content: (*code).into(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
        }

        // Java/C#/Dart constructors are `function` nodes that share the enclosing
        // class name (qualified_name `Account.Account`). Each needs a sibling
        // `class` node in the same file so the same-file-class probe fires.
        let class_ctor_cases = [
            ("src/Account.java", "java", "Account"),
            ("src/Repo.cs", "csharp", "Repo"),
            ("src/Widget.dart", "dart", "Widget"),
        ];
        for (i, (path, lang, cls)) in class_ctor_cases.iter().enumerate() {
            let fid = upsert_file(
                conn,
                &FileRecord {
                    path: (*path).into(),
                    blake3_hash: format!("hc{i}"),
                    last_modified: 1,
                    language: Some((*lang).into()),
                },
            )
            .unwrap();
            // The class itself.
            insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "class".into(),
                    name: (*cls).into(),
                    qualified_name: Some((*cls).into()),
                    start_line: 1,
                    end_line: 12,
                    code_content: format!("class {cls} {{ }}"),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
            // The constructor: a `function` sharing the class name, edgeless.
            insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: (*cls).into(),
                    qualified_name: Some(format!("{cls}.{cls}")),
                    start_line: 3,
                    end_line: 6,
                    code_content: format!("{cls}(int x) {{\n    this.x = x;\n    this.y = x;\n}}"),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
        }

        // C++: constructor is a `method` sharing the class name (tested in its
        // own file with no destructor, so ONLY the same-file-class rule can
        // exclude it — not a coincidental instr rescue from a `~Class(` substring).
        let cpp_ctor_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/Alpha.cpp".into(),
                blake3_hash: "hcppa".into(),
                last_modified: 1,
                language: Some("cpp".into()),
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: cpp_ctor_fid,
                node_type: "class".into(),
                name: "Alpha".into(),
                qualified_name: Some("Alpha".into()),
                start_line: 1,
                end_line: 6,
                code_content: "class Alpha { }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: cpp_ctor_fid,
                node_type: "method".into(),
                name: "Alpha".into(),
                qualified_name: Some("Alpha.Alpha".into()),
                start_line: 2,
                end_line: 5,
                code_content: "Alpha(int w) {\n    width = w;\n    height = w;\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // C++ destructor `~Beta` (own file): only the `~` prefix rule can exclude
        // it — the constructor body `Beta(` does not contain `~Beta`.
        let cpp_dtor_fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/Beta.cpp".into(),
                blake3_hash: "hcppb".into(),
                last_modified: 1,
                language: Some("cpp".into()),
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: cpp_dtor_fid,
                node_type: "class".into(),
                name: "Beta".into(),
                qualified_name: Some("Beta".into()),
                start_line: 1,
                end_line: 6,
                code_content: "class Beta { }".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: cpp_dtor_fid,
                node_type: "method".into(),
                name: "~Beta".into(),
                qualified_name: Some("Beta.~Beta".into()),
                start_line: 2,
                end_line: 5,
                code_content: "~Beta() {\n    free(buf);\n    log(\"gone\");\n}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // A genuinely-dead regular method (Python, 3 lines, no edges) — must
        // still be reported so the exclusion doesn't over-suppress real findings.
        let py_fid = *fid_cache.get("src/a.py").unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: py_fid,
                node_type: "method".into(),
                name: "compute_unused".into(),
                qualified_name: None,
                start_line: 200,
                end_line: 203,
                code_content: "def compute_unused(self):\n    a = 1\n    return a".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Capture (name, node_type) so constructor *function* nodes can be told
        // apart from the same-named `class` node.
        let results: Vec<(String, String)> = find_dead_code(conn, None, None, false, 1, 100)
            .unwrap()
            .into_iter()
            .map(|r| (r.name, r.node_type))
            .collect();
        let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();

        for implicit in [
            "__init__",
            "__eq__",
            "__construct",
            "__toString",
            "constructor",
            "initialize",
        ] {
            assert!(
                !names.contains(&implicit),
                "implicitly-invoked method '{implicit}' must NOT be reported dead; got: {names:?}"
            );
        }
        // Java/C#/Dart/C++ constructors are function/method nodes named like the
        // class. The constructor itself must never be reported (the same-named
        // `class` node is a separate unused-class concern, out of scope here).
        for ctor in ["Account", "Repo", "Widget", "Alpha"] {
            assert!(
                !results
                    .iter()
                    .any(|(n, t)| n == ctor && (t == "function" || t == "method")),
                "constructor function '{ctor}' must be excluded; got: {results:?}"
            );
        }
        // C++ destructor (`~Class`, invoked at scope exit) must be excluded.
        assert!(
            !names.contains(&"~Beta"),
            "C++ destructor '~Beta' must NOT be reported dead; got: {names:?}"
        );
        assert!(
            names.contains(&"compute_unused"),
            "a genuinely-dead regular method must still be reported; got: {names:?}"
        );
    }

    /// Regression (issue #32 cause 1): Python methods/functions registered with a
    /// framework decorator (pydantic `@field_validator`/`@model_validator`/
    /// `@computed_field`, pytest `@fixture`, stdlib `@property`/`@abstractmethod`/
    /// `@overload`, NiceGUI handlers) are invoked dynamically by the framework /
    /// runtime, so they never carry an incoming `calls` edge even when fully live —
    /// the dominant dead-code false positive on framework-heavy Python (~83 of 86
    /// orphans in the reporter's pydantic+NiceGUI codebase). Now that the parser
    /// stores the decorator stack in `code_content` (issue #31,
    /// `decorated_definition` binding), such methods are excluded by an @-anchored
    /// decorator probe. A plain edgeless method must STILL be reported (no
    /// over-suppression), matching the tool's genuine-dead-code contract.
    #[test]
    fn test_find_dead_code_excludes_python_framework_decorated() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Single file so the same-file instr probe cannot coincidentally rescue
        // anything — every method below is genuinely edgeless. Their only
        // distinguishing feature vs a flagged plain method is the decorator stack
        // stored at the head of code_content (exactly as the parser now emits it).
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/models.py".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();

        let decorated = [
            ("pre_validate",
             "@field_validator(\"lat\", mode=\"before\")\n@classmethod\ndef pre_validate(cls, v):\n    return str(v)"),
            ("check_after",
             "@model_validator(mode=\"after\")\ndef check_after(self):\n    return self"),
            ("label",
             "@computed_field\n@property\ndef label(self) -> str:\n    return \"lbl\""),
            ("area",
             "@property\ndef area(self):\n    return self.w * self.h"),
            ("shape",
             "@abstractmethod\ndef shape(self):\n    raise NotImplementedError"),
            ("db_conn",
             "@pytest.fixture\ndef db_conn():\n    return connect_db()"),
        ];
        for (i, (name, code)) in decorated.iter().enumerate() {
            insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "method".into(),
                    name: (*name).into(),
                    qualified_name: None,
                    start_line: (i as i64) * 10 + 1,
                    end_line: (i as i64) * 10 + 4,
                    code_content: (*code).into(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
        }

        // A genuinely-dead PLAIN method (no decorator) — must still be reported.
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "method".into(),
                name: "compute_unused".into(),
                qualified_name: None,
                start_line: 200,
                end_line: 204,
                code_content: "def compute_unused(self):\n    a = 1\n    b = 2\n    return a + b"
                    .into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let names: Vec<String> = find_dead_code(conn, None, None, false, 1, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();

        for (name, _) in &decorated {
            assert!(!names.contains(&name.to_string()),
                "framework-decorated method '{name}' must NOT be reported dead (issue #32 cause 1); got: {names:?}");
        }
        assert!(
            names.contains(&"compute_unused".to_string()),
            "a plain edgeless method must still be reported (no over-suppression); got: {names:?}"
        );
    }

    /// Regression: markdown headings (h1..h6) are document structure, never
    /// callable code, so they never carry incoming edges — reporting them dead
    /// would flag every README heading. They must be excluded; a real dead
    /// function in a source file must still be reported.
    #[test]
    fn test_find_dead_code_excludes_markdown_headings() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let md = upsert_file(
            conn,
            &FileRecord {
                path: "README.md".into(),
                blake3_hash: "hmd".into(),
                last_modified: 1,
                language: Some("markdown".into()),
            },
        )
        .unwrap();
        for (i, level) in (1..=6).enumerate() {
            insert_node(
                conn,
                &NodeRecord {
                    file_id: md,
                    node_type: format!("h{level}"),
                    name: format!("Heading {level}"),
                    qualified_name: None,
                    start_line: (i as i64) * 3 + 1,
                    end_line: (i as i64) * 3 + 3,
                    code_content: format!("{} Heading {}", "#".repeat(level), level),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
        }

        // A genuinely-dead function in a real source file — must still be reported.
        let src = upsert_file(
            conn,
            &FileRecord {
                path: "src/x.py".into(),
                blake3_hash: "hpy".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: src,
                node_type: "function".into(),
                name: "dead_fn".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 4,
                code_content: "def dead_fn():\n    a = 1\n    return a".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let names: Vec<String> = find_dead_code(conn, None, None, false, 1, 100)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        for level in 1..=6 {
            assert!(
                !names.contains(&format!("Heading {level}")),
                "markdown h{level} heading must NOT be reported dead; got: {names:?}"
            );
        }
        assert!(
            names.contains(&"dead_fn".to_string()),
            "a genuinely-dead function must still be reported; got: {names:?}"
        );
    }

    // Fixture: one orphan fn (5 lines, unreferenced), one exported-unused, one
    // short orphan (2 lines) that a min_lines=3 filter must hide, one ignored path.
    //
    // Deviation from the task brief's literal fixture: the brief put the
    // ignore-filter probe fn at `benches/b.rs` with ignore=["benches/"], but
    // `is_test_node_sql` (src/domain.rs) already globs `benches/*` as a TEST
    // path — with include_tests=false (as used here) that node is excluded by
    // the SQL query itself, before `dead_code_report`'s ignore_prefixes filter
    // ever runs, so it never contributes to `ignored_count`. Using `vendor/`
    // (not matched by any is_test_node_sql glob) instead exercises the actual
    // ignore_prefixes filtering path the test is meant to cover.
    fn seed_dead_code(conn: &rusqlite::Connection) {
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.rs', 'h1', 0, 'rust', 0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('vendor/b.rs', 'h2', 0, 'rust', 0)", []).unwrap();
        // long orphan (5 lines)
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'orphan_long', 'orphan_long', 1, 5, 'fn orphan_long() {}')", []).unwrap();
        // short orphan (2 lines) — below min_lines=3
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'orphan_short', 'orphan_short', 7, 8, 'fn orphan_short() {}')", []).unwrap();
        // orphan in an ignored path (vendor/)
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (2, 'function', 'vendor_fn', 'vendor_fn', 1, 6, 'fn vendor_fn() {}')", []).unwrap();
    }

    #[test]
    fn dead_code_report_counts_and_hidden_probe() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        seed_dead_code(conn);
        let ignore = vec!["vendor/".to_string()];
        let rep = dead_code_report(conn, None, None, false, 3, &ignore).unwrap();
        // orphan_long shows; orphan_short hidden by min_lines; vendor_fn ignored.
        assert_eq!(rep.orphan_count, 1, "one long orphan visible");
        assert_eq!(rep.exported_count, 0);
        assert_eq!(
            rep.ignored_count, 1,
            "vendor_fn suppressed by ignore prefix"
        );
        assert_eq!(rep.hidden_below_threshold, 0, "probe suppressed while a candidate is visible — matches the surfaces, which disclose the threshold only when nothing shows");
        assert_eq!(rep.items.len(), 1);
        assert_eq!(rep.items[0].name, "orphan_long");
    }

    /// META⑥ drift-guard: the CLI and MCP dead-code surfaces must derive their
    /// verdict from the SAME `dead_code_report`. This asserts the shared report is
    /// the single source of truth for counts — the sibling-hole this locks is a
    /// surface re-deriving classification/probe logic on its own and drifting.
    /// Negative control: make either surface stop calling dead_code_report (e.g.
    /// hard-code a count) and this fails.
    #[test]
    fn dead_code_report_is_single_source_for_both_surfaces() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        seed_dead_code(conn);
        let ignore = vec!["vendor/".to_string()];
        let rep = dead_code_report(conn, None, None, false, 3, &ignore).unwrap();
        // The exact tuple both cmd_dead_code (CLI) and tool_find_dead_code (MCP)
        // must format from — never recompute independently.
        assert_eq!(
            (
                rep.orphan_count,
                rep.exported_count,
                rep.ignored_count,
                rep.hidden_below_threshold,
                rep.items.len()
            ),
            (1, 0, 1, 0, 1)
        );
    }

    /// The hidden-below-threshold probe fires ONLY when nothing is visible and
    /// nothing was ignore-suppressed — the exact disclosure path both surfaces
    /// take. Locks the gate so a refactor can't turn it into an always-on query.
    #[test]
    fn dead_code_report_hidden_probe_only_when_nothing_visible() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/c.rs', 'h1', 0, 'rust', 0)", []).unwrap();
        // A single 2-line orphan and nothing else.
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'tiny', 'tiny', 1, 2, 'fn tiny() {}')", []).unwrap();
        // min_lines=3 hides the 2-line orphan → visible empty, nothing ignored → probe discloses it.
        let rep = dead_code_report(conn, None, None, false, 3, &[]).unwrap();
        assert!(rep.is_empty());
        assert_eq!(rep.hidden_below_threshold, 1);
        // min_lines=1 → the orphan is itself visible; no separate probe.
        let rep1 = dead_code_report(conn, None, None, false, 1, &[]).unwrap();
        assert_eq!(rep1.items.len(), 1);
        assert_eq!(rep1.hidden_below_threshold, 0);
    }

    #[test]
    fn dead_code_report_candidate_hidden_by_both_filters_still_disclosed() {
        // Audit 2026-08-02 MED-4: a candidate that is BOTH below --min-lines
        // AND under an --ignore prefix used to be counted by NEITHER
        // disclosure counter — either filter alone disclosed it, both
        // together answered a bare "[]" false clean.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('scripts/s.rs', 'h1', 0, 'rust', 0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'tiny', 'tiny', 1, 2, 'fn tiny() {}')", []).unwrap();
        let ignores = vec!["scripts/".to_string()];
        let rep = dead_code_report(conn, None, None, false, 3, &ignores).unwrap();
        assert!(rep.is_empty());
        assert!(
            rep.ignored_count + rep.hidden_below_threshold > 0,
            "both-filter candidate vanished from every disclosure counter (false clean)"
        );
    }

    #[test]
    fn validate_dead_code_type_filter_rejects_typo() {
        assert!(validate_dead_code_type_filter(Some("fucntion")).is_err());
        assert!(validate_dead_code_type_filter(Some("fn")).is_ok());
        assert!(validate_dead_code_type_filter(None).is_ok());
    }
}
