//! The JS/TS re-export walk (D#740): what a name a file exports resolves to, by
//! ECMAScript's `ResolveExport` over the export entries each file's own text
//! records (`js_export_entries`, written in the file's batch), and the
//! incremental fan-out that keeps its answers equal to a rebuild's.
//!
//! 0.185.0's walk read a barrel's bound `imports` edges instead. Those are an
//! output of resolution, so the walk could not be a function of the files: an
//! edge exists only once bound (none for `export *` of a file not there yet,
//! D#739), one re-export bound to two namesakes is two rows, rows carry no
//! order, and a plain `import` read as a re-export. Measured on 0.185.0: 33 of
//! 400 random barrel graphs bound differently incrementally than on a rebuild,
//! and 18 of 62 valid probe programs differed from Node.

use anyhow::Result;
use std::collections::{BTreeSet, HashMap, HashSet};

use super::js_modules::resolve_js_specifier_path;

/// How deep the walk recurses before it gives a lookup up as [`Unknown`].
/// `ResolveExport` needs no bound (its resolve set ends every cycle); this one
/// keeps the stack bounded on a generated chain. Entries are read in source
/// order, so the same lookup meets the bound the same way on every run.
///
/// [`Unknown`]: JsResolution::Unknown
const MAX_JS_REEXPORT_DEPTH: usize = 256;

/// What a name a file exports resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JsResolution {
    /// The binding `local` of `file`.
    Binding { file: String, local: String },
    /// The namespace of `file` (`export * as ns from`, a re-exported
    /// `import * as`).
    Namespace { file: String },
    /// No such export, or only a cycle with none (`ResolveExport`'s null).
    Missing,
    /// Two `export *` sources resolve it differently.
    Ambiguous,
    /// Past [`MAX_JS_REEXPORT_DEPTH`], or through a specifier no project file
    /// answers (a package): the walk cannot say.
    Unknown,
}

/// One row of `js_export_entries`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Local {
        export: String,
        local: String,
    },
    Indirect {
        export: String,
        spec: String,
        import: Option<String>,
    },
    Star {
        spec: String,
    },
}

/// What the walk reads of one file.
#[derive(Default)]
struct FileExports {
    /// Its `js_export_entries`, in source order.
    entries: Vec<Entry>,
    /// `(published name, node name, keyed)` of each `exports` edge of its
    /// `<module>`; `keyed` when an export map names it (`{"as": …}`).
    published: Vec<(String, String, bool)>,
    /// `(id, name, start line, end line, is a function)` of its functions,
    /// methods and classes, to find a top-level function.
    spans: Vec<(i64, String, i64, i64, bool)>,
}

impl FileExports {
    fn load(conn: &rusqlite::Connection, file: &str) -> Result<Self> {
        let entries = conn
            .prepare_cached(&js_export_entries_sql())?
            .query_map([file], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(kind, name, target, spec)| entry_of(&kind, name, target, spec))
            .collect();
        let published = conn
            .prepare_cached(&js_export_map_sql())?
            .query_map([file], |r| {
                let name: String = r.get(1)?;
                let key: Option<String> = r.get(2)?;
                Ok(match key {
                    Some(k) => (k, name, true),
                    None => (name.clone(), name, false),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let spans = conn
            .prepare_cached(
                "SELECT n.id, n.name, n.start_line, n.end_line, n.type = 'function'
                 FROM files f CROSS JOIN nodes n ON n.file_id = f.id
                 WHERE f.path = ?1 AND n.type IN ('function', 'method', 'class')",
            )?
            .query_map([file], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            entries,
            published,
            spans,
        })
    }

    /// The file's own binding exported as `name`: a local entry, else an
    /// `exports` edge, an export map's key before a same-named function the map
    /// does not publish (D#120). The exports axis publishes `export default
    /// function f` as `f` too; a name the entries export only as `default` is
    /// no export of that name.
    fn local_of(&self, name: &str) -> Option<String> {
        self.entries
            .iter()
            .find_map(|e| match e {
                Entry::Local { export, local } if export == name => Some(local.clone()),
                _ => None,
            })
            .or_else(|| {
                let keyed = self.published.iter().find(|(p, _, k)| *k && p == name);
                let only_default = self.entries.iter().any(|e| {
                    matches!(e, Entry::Local { export, local } if export == "default" && local == name)
                });
                keyed
                    .or_else(|| {
                        self.published
                            .iter()
                            .find(|(p, _, k)| !k && p == name && !only_default)
                    })
                    .map(|(_, n, _)| n.clone())
            })
    }

    /// The first indirect entry exporting `name`: `(specifier, import)`.
    fn indirect_of(&self, name: &str) -> Option<(&str, Option<&str>)> {
        self.entries.iter().find_map(|e| match e {
            Entry::Indirect {
                export,
                spec,
                import,
            } if export == name => Some((spec.as_str(), import.as_deref())),
            _ => None,
        })
    }

    fn star_specs(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Star { spec } => Some(spec.as_str()),
            _ => None,
        })
    }

    /// The top-level functions named `local`: no other function, method or
    /// class of the file spans their lines. (Two sharing one line both fail
    /// it: a missed edge, never a method's.)
    fn functions_named(&self, local: &str) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .spans
            .iter()
            .filter(|(id, name, start, end, function)| {
                *function
                    && name == local
                    && !self
                        .spans
                        .iter()
                        .any(|(o, _, s, e, _)| o != id && s <= start && e >= end)
            })
            .map(|(id, ..)| *id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

fn entry_of(
    kind: &str,
    name: Option<String>,
    target: Option<String>,
    spec: Option<String>,
) -> Option<Entry> {
    Some(match kind {
        "local" => Entry::Local {
            export: name?,
            local: target?,
        },
        "indirect" => Entry::Indirect {
            export: name?,
            spec: spec?,
            import: target,
        },
        "star" => Entry::Star { spec: spec? },
        _ => return None,
    })
}

/// [`FileExports::load`]'s entry query. `CROSS JOIN` fixes the loop order file
/// → `<module>` → its rows, as [`js_export_map_sql`] does.
pub(super) fn js_export_entries_sql() -> String {
    "SELECT x.kind, x.name, x.target, x.spec
     FROM files f
     CROSS JOIN nodes m ON m.file_id = f.id AND m.name = '<module>'
     CROSS JOIN js_export_entries x ON x.module_id = m.id
     WHERE f.path = ?1
     ORDER BY x.ord"
        .to_string()
}

/// Every node the `<module>` of file `?1` exports, with the key it is published
/// under (`{"as": …}`), whether it is a function, and its lines. Only exports
/// to the file's own nodes: an export map naming another file's symbol binds
/// nothing.
///
/// `CROSS JOIN` fixes the loop order file → `<module>` → its `exports` edges.
/// Without statistics — a full index runs this before any `ANALYZE` — SQLite
/// drove the D#120 version from `idx_edges_relation`, scanning every `exports`
/// edge of the repo per call (review BLOCKER: +113% on a 5,000-file tree).
/// Pinned by `test_js_export_map_query_seeks_the_module_node_edges`.
pub(super) fn js_export_map_sql() -> String {
    format!(
        "SELECT n.id, n.name, json_extract(e.metadata, '$.as'), n.type = 'function',
                n.start_line, n.end_line
         FROM files f
         CROSS JOIN nodes m ON m.file_id = f.id AND m.name = '<module>'
         CROSS JOIN edges e ON e.source_id = m.id AND e.relation = '{}'
         CROSS JOIN nodes n ON n.id = e.target_id AND n.file_id = f.id
         WHERE f.path = ?1",
        crate::domain::REL_EXPORTS
    )
}

/// Per-pass memo of the walk: each file's exports read once, and each
/// `(file, name)` lookup answered once.
///
/// Both hold for a whole pass. Every file's entries and `exports` edges are
/// written in its own batch, before any deferred relation or pending call is
/// resolved, and nothing the walk reads changes while one is. A query per
/// call was the D#120 review's BLOCKER: the bundled SQLite is built with
/// `STAT4`, under which a statement whose parameters meet an indexed column is
/// re-prepared each time they are re-bound (~33 µs a call against ~2.5 µs).
#[derive(Default)]
pub(super) struct JsExports {
    by_file: HashMap<String, FileExports>,
    memo: HashMap<(String, String), JsResolution>,
}

impl JsExports {
    fn load(&mut self, conn: &rusqlite::Connection, file: &str) -> Result<()> {
        if !self.by_file.contains_key(file) {
            let loaded = FileExports::load(conn, file)?;
            self.by_file.insert(file.to_string(), loaded);
        }
        Ok(())
    }

    /// What `name`, exported by `file`, resolves to.
    pub(super) fn resolve(
        &mut self,
        conn: &rusqlite::Connection,
        paths: &HashSet<String>,
        file: &str,
        name: &str,
    ) -> Result<JsResolution> {
        let key = (file.to_string(), name.to_string());
        if let Some(r) = self.memo.get(&key) {
            return Ok(r.clone());
        }
        let mut seen = HashSet::new();
        let r = self.walk(conn, paths, file, name, &mut seen, 0)?;
        self.memo.insert(key, r.clone());
        Ok(r)
    }

    /// [`Self::resolve`], also returning every `(file, name)` the walk asked:
    /// its resolve set.
    fn resolve_visiting(
        &mut self,
        conn: &rusqlite::Connection,
        paths: &HashSet<String>,
        file: &str,
        name: &str,
    ) -> Result<(JsResolution, HashSet<(String, String)>)> {
        let mut seen = HashSet::new();
        let r = self.walk(conn, paths, file, name, &mut seen, 0)?;
        Ok((r, seen))
    }

    /// ECMAScript `ResolveExport(name, resolveSet)` of `file`: its own binding
    /// exported as `name`, else what its indirect entry for `name` resolves to
    /// (the answer as it is, `export *` sources never asked), else, unless the
    /// name is `default`, what its `export *` sources agree on. `seen` is the
    /// resolve set: a `(file, name)` asked before answers null, which ends
    /// cycles and counts a diamond once.
    fn walk(
        &mut self,
        conn: &rusqlite::Connection,
        paths: &HashSet<String>,
        file: &str,
        name: &str,
        seen: &mut HashSet<(String, String)>,
        depth: usize,
    ) -> Result<JsResolution> {
        if depth > MAX_JS_REEXPORT_DEPTH {
            return Ok(JsResolution::Unknown);
        }
        if !seen.insert((file.to_string(), name.to_string())) {
            return Ok(JsResolution::Missing);
        }
        self.load(conn, file)?;
        let here = &self.by_file[file];
        if let Some(local) = here.local_of(name) {
            return Ok(JsResolution::Binding {
                file: file.to_string(),
                local,
            });
        }
        if let Some((spec, import)) = here.indirect_of(name) {
            let import = import.map(str::to_string);
            let Some(next) = resolve_js_specifier_path(spec, file, paths) else {
                return Ok(JsResolution::Unknown);
            };
            return match import {
                None => Ok(JsResolution::Namespace { file: next }),
                Some(import) => self.walk(conn, paths, &next, &import, seen, depth + 1),
            };
        }
        if name == "default" {
            return Ok(JsResolution::Missing);
        }
        // A star specifier no file answers (a package, or a file not there)
        // adds nothing, as before; ECMAScript would fail to link.
        let stars: Vec<String> = here
            .star_specs()
            .filter_map(|spec| resolve_js_specifier_path(spec, file, paths))
            .collect();
        let mut found: Option<JsResolution> = None;
        for next in stars {
            match self.walk(conn, paths, &next, name, seen, depth + 1)? {
                JsResolution::Missing => {}
                r @ (JsResolution::Ambiguous | JsResolution::Unknown) => return Ok(r),
                r => match &found {
                    Some(f) if *f != r => return Ok(JsResolution::Ambiguous),
                    Some(_) => {}
                    None => found = Some(r),
                },
            }
        }
        Ok(found.unwrap_or(JsResolution::Missing))
    }

    /// The top-level functions of `file` named `local`.
    fn functions_named(
        &mut self,
        conn: &rusqlite::Connection,
        file: &str,
        local: &str,
    ) -> Result<Vec<i64>> {
        self.load(conn, file)?;
        Ok(self.by_file[file].functions_named(local))
    }

    /// Whether `file` exports `name` itself or by an indirect entry, so its
    /// `export *` sources are never asked for it.
    fn answers_without_stars(&self, file: &str, name: &str) -> bool {
        self.by_file
            .get(file)
            .is_some_and(|f| f.local_of(name).is_some() || f.indirect_of(name).is_some())
    }
}

/// The node a call through a renamed import binds (D#120): the top-level
/// function the export resolves to ([`JsExports::resolve`]) — a binding of the
/// file the specifier names, or of the file its re-exports lead to. Never a
/// method, a nested function, or a function no export names.
///
/// `None` when the specifier is a package's (nothing to bind, nothing to wait
/// for) or the export is a namespace (calling one binds nothing). `Some(empty)`
/// when a relative specifier names no indexed file yet, or the export resolves
/// to nothing or to no function: the caller buffers the call, so it binds when
/// that changes, as a rebuild would.
pub(super) fn js_import_targets(
    conn: &rusqlite::Connection,
    exports: &mut JsExports,
    caller_path: &str,
    module: &str,
    export: &str,
    all_file_paths: &HashSet<String>,
) -> Result<Option<Vec<i64>>> {
    let Some(file) = resolve_js_specifier_path(module, caller_path, all_file_paths) else {
        let relative = module.starts_with("./") || module.starts_with("../");
        return Ok(relative.then(Vec::new));
    };
    match exports.resolve(conn, all_file_paths, &file, export)? {
        JsResolution::Binding { file, local } => {
            exports.functions_named(conn, &file, &local).map(Some)
        }
        JsResolution::Namespace { .. } => Ok(None),
        _ => Ok(Some(Vec::new())),
    }
}

/// What a named JS import of `name` binds when `file`, the file its specifier
/// resolves to, defines no node of that name (D#714 A2): the nodes the export
/// resolves to — every node of the binding's name in the binding's file, or a
/// namespace's `<module>` — and the name they are bound under. `None` when it
/// resolves to nothing: the caller binds by name as before.
pub(super) fn js_import_edge_targets(
    conn: &rusqlite::Connection,
    exports: &mut JsExports,
    file: &str,
    name: &str,
    all_file_paths: &HashSet<String>,
    name_to_ids: &HashMap<String, Vec<i64>>,
    node_id_to_path: &HashMap<i64, String>,
) -> Result<Option<(Vec<i64>, String)>> {
    let (file, local) = match exports.resolve(conn, all_file_paths, file, name)? {
        JsResolution::Binding { file, local } => (file, local),
        JsResolution::Namespace { file } => (file, "<module>".to_string()),
        _ => return Ok(None),
    };
    let ids: Vec<i64> = name_to_ids
        .get(&local)
        .map(|ids| {
            ids.iter()
                .filter(|id| node_id_to_path.get(id).is_some_and(|p| *p == file))
                .copied()
                .collect()
        })
        .unwrap_or_default();
    Ok((!ids.is_empty()).then_some((ids, local)))
}

/// The metadata key an `imports` edge carries the name it imports under when
/// that is not its target's name (a renamed re-export, a namespace): Phase 0's
/// requeue reads the target's name back, so the deferred pass, the fan-out and
/// the post passes that bind and prune bare calls by imported name read this
/// one first.
pub(super) const JS_IMPORT_NAME_KEY: &str = "imported";

/// `metadata` with [`JS_IMPORT_NAME_KEY`] set to `name`.
pub(super) fn with_imported_name(metadata: Option<&str>, name: &str) -> Option<String> {
    let mut v: serde_json::Value = metadata
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v.as_object_mut()?
        .insert(JS_IMPORT_NAME_KEY.to_string(), serde_json::json!(name));
    Some(v.to_string())
}

/// The name a JS import relation imports: [`JS_IMPORT_NAME_KEY`] when its
/// metadata carries one (a requeued edge whose target has another name), else
/// its target name.
pub(super) fn js_imported_name<'a>(meta: &'a serde_json::Value, target_name: &'a str) -> &'a str {
    meta.get(JS_IMPORT_NAME_KEY)
        .and_then(|v| v.as_str())
        .unwrap_or(target_name)
}

// ---------------------------------------------------------------------------
// Incremental fan-out
// ---------------------------------------------------------------------------

/// Record, before an `index_files` run rewrites them, which of `paths` are
/// JS/TS files, whether each was indexed, and what each exports (its entries
/// and its `exports` edges); [`importers_of_js_reexport_drift`] compares them
/// with what the run left.
pub(super) fn snapshot_js_reexports<'a>(
    conn: &rusqlite::Connection,
    paths: impl Iterator<Item = &'a String>,
) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_js_reexport_paths;
         DROP TABLE IF EXISTS temp.cg_js_entries_before;
         DROP TABLE IF EXISTS temp.cg_js_published_before;
         CREATE TEMP TABLE cg_js_reexport_paths (path TEXT PRIMARY KEY, existed INTEGER NOT NULL);",
    )?;
    {
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO cg_js_reexport_paths (path, existed)
             VALUES (?1, EXISTS(SELECT 1 FROM files WHERE path = ?1))",
        )?;
        for p in paths.filter(|p| super::js_modules::is_js_module_path(p)) {
            stmt.execute([p])?;
        }
    }
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE cg_js_entries_before AS
           SELECT rp.path AS path, x.ord AS ord, x.kind AS kind, x.name AS name,
                  x.target AS target, x.spec AS spec
           FROM cg_js_reexport_paths rp
           CROSS JOIN files f ON f.path = rp.path
           CROSS JOIN nodes m ON m.file_id = f.id AND m.name = '<module>'
           CROSS JOIN js_export_entries x ON x.module_id = m.id;
         CREATE TEMP TABLE cg_js_published_before AS
           SELECT rp.path AS path, n.name AS name, json_extract(e.metadata, '$.as') AS key
           FROM cg_js_reexport_paths rp
           CROSS JOIN files f ON f.path = rp.path
           CROSS JOIN nodes m ON m.file_id = f.id AND m.name = '<module>'
           CROSS JOIN edges e ON e.source_id = m.id AND e.relation = '{}'
           CROSS JOIN nodes n ON n.id = e.target_id AND n.file_id = f.id;",
        crate::domain::REL_EXPORTS
    ))?;
    Ok(())
}

/// How one file of the run moved, as the walk reads it.
#[derive(Default)]
struct Drift {
    /// Added, deleted or new to the index: every name.
    all: bool,
    /// Names its own binding or its indirect entry now resolves differently.
    names: HashSet<String>,
    /// Its `export *` sources (as resolved) changed.
    stars: bool,
}

/// What a file exported, as `(name → own binding, name → (target file,
/// import) of its first indirect entry, resolved star sources)`.
type Shape = (
    HashMap<String, String>,
    HashMap<String, (Option<String>, Option<String>)>,
    Vec<String>,
);

fn shape_of(f: &FileExports, file: &str, paths: &HashSet<String>) -> Shape {
    let mut own = HashMap::new();
    let mut indirect = HashMap::new();
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for e in &f.entries {
        if let Entry::Local { export, .. } | Entry::Indirect { export, .. } = e {
            names.insert(export);
        }
    }
    for (p, ..) in &f.published {
        names.insert(p);
    }
    for n in names {
        if let Some(l) = f.local_of(n) {
            own.insert(n.to_string(), l);
        }
        if let Some((spec, import)) = f.indirect_of(n) {
            indirect.insert(
                n.to_string(),
                (
                    resolve_js_specifier_path(spec, file, paths),
                    import.map(str::to_string),
                ),
            );
        }
    }
    let stars = f
        .star_specs()
        .filter_map(|s| resolve_js_specifier_path(s, file, paths))
        .collect();
    (own, indirect, stars)
}

/// The files that import a name from a JS/TS file by a relative specifier and
/// would bind it differently on a rebuild after the last `index_files` run
/// (pre-tag review of 0.185.0, H1; D#740).
///
/// Each file of the run is compared with its snapshot: the names whose own
/// binding or indirect entry resolves differently, and whether its `export *`
/// sources do, each resolved against the files the run started from and the
/// current ones. An importer outside the run is pulled when the walk of its
/// import now passes through a moved name: `ResolveExport` of the same files
/// before and after agrees up to the first file whose input differs, and both
/// walks ask that file, so a walk that asks none answers as before. A moved
/// `export *` set moves only the names the file does not answer itself.
/// Also pulled: an importer whose specifier now names another file, and one
/// whose import the walk serves although the file it names, re-extracted, now
/// defines the name itself (a rebuild binds that directly). Drops the snapshot.
pub(super) fn importers_of_js_reexport_drift(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let has_snapshot: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name = 'cg_js_entries_before')",
        [],
        |r| r.get(0),
    )?;
    if !has_snapshot {
        return Ok(Vec::new());
    }
    let in_run: HashMap<String, bool> = {
        let mut stmt = conn.prepare("SELECT path, existed FROM cg_js_reexport_paths")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<HashMap<String, bool>>>()?;
        rows
    };
    let mut before: HashMap<String, FileExports> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT path, kind, name, target, spec FROM cg_js_entries_before ORDER BY path, ord",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (path, kind, name, target, spec) in rows {
            if let Some(e) = entry_of(&kind, name, target, spec) {
                before.entry(path).or_default().entries.push(e);
            }
        }
        let mut stmt = conn.prepare("SELECT path, name, key FROM cg_js_published_before")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (path, name, key) in rows {
            let row = match key {
                Some(k) => (k, name, true),
                None => (name.clone(), name, false),
            };
            before.entry(path).or_default().published.push(row);
        }
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.cg_js_reexport_paths;
         DROP TABLE IF EXISTS temp.cg_js_entries_before;
         DROP TABLE IF EXISTS temp.cg_js_published_before;",
    )?;
    if in_run.is_empty() {
        return Ok(Vec::new());
    }
    let now_paths: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT path FROM files")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        rows
    };
    let mut before_paths = now_paths.clone();
    for (path, existed) in &in_run {
        if *existed {
            before_paths.insert(path.clone());
        } else {
            before_paths.remove(path);
        }
    }
    let mut walker = JsExports::default();
    let mut drift: HashMap<String, Drift> = HashMap::new();
    for (path, existed) in &in_run {
        if !*existed || !now_paths.contains(path) {
            drift.entry(path.clone()).or_default().all = true;
            continue;
        }
        let was = before.remove(path).unwrap_or_default();
        walker.load(conn, path)?;
        let (own_b, ind_b, stars_b) = shape_of(&was, path, &before_paths);
        let (own_a, ind_a, stars_a) = shape_of(&walker.by_file[path], path, &now_paths);
        let mut d = Drift::default();
        for n in own_b.keys().chain(own_a.keys()) {
            if own_b.get(n) != own_a.get(n) {
                d.names.insert(n.clone());
            }
        }
        for n in ind_b.keys().chain(ind_a.keys()) {
            if ind_b.get(n) != ind_a.get(n) {
                d.names.insert(n.clone());
            }
        }
        d.stars = stars_b != stars_a;
        if d.stars || !d.names.is_empty() {
            drift.insert(path.clone(), d);
        }
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT f.path, json_extract(e.metadata, '$.js_module'),
                COALESCE(json_extract(e.metadata, '$.{JS_IMPORT_NAME_KEY}'), t.name), tf.path
         FROM edges e
         JOIN nodes s ON s.id = e.source_id
         JOIN files f ON f.id = s.file_id
         JOIN nodes t ON t.id = e.target_id
         LEFT JOIN files tf ON tf.id = t.file_id
         WHERE e.relation = '{}'
           AND json_extract(e.metadata, '$.js_module') IS NOT NULL
           AND json_extract(e.metadata, '$.q') IS NULL",
        crate::domain::REL_IMPORTS
    ))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut has_node = conn.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM files f CROSS JOIN nodes n ON n.file_id = f.id
                       WHERE f.path = ?1 AND n.name = ?2)",
    )?;
    let mut visited: HashMap<(String, String), HashSet<(String, String)>> = HashMap::new();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (importer, spec, name, target_file) in rows {
        if name == "<module>" || in_run.contains_key(&importer) || out.contains(&importer) {
            continue;
        }
        let now = resolve_js_specifier_path(&spec, &importer, &now_paths);
        if now != resolve_js_specifier_path(&spec, &importer, &before_paths) {
            out.insert(importer);
            continue;
        }
        let Some(file) = now else {
            continue;
        };
        if target_file.as_deref() == Some(file.as_str()) {
            // Bound in the file its specifier names: no walk served it.
            continue;
        }
        if in_run.contains_key(&file) && has_node.query_row([&file, &name], |r| r.get(0))? {
            out.insert(importer);
            continue;
        }
        if drift.is_empty() {
            continue;
        }
        let key = (file.clone(), name.clone());
        if !visited.contains_key(&key) {
            let (_, seen) = walker.resolve_visiting(conn, &now_paths, &file, &name)?;
            visited.insert(key.clone(), seen);
        }
        let moved = visited[&key].iter().any(|(g, m)| {
            drift.get(g).is_some_and(|d| {
                d.all
                    || d.names.contains(m)
                    || (d.stars && m != "default" && !walker.answers_without_stars(g, m))
            })
        });
        if moved {
            out.insert(importer);
        }
    }
    Ok(out.into_iter().collect())
}

/// The files outside this run whose export entries name a file the run adds
/// or deletes differently than before (D#739): `export * from './x'` written
/// before `x.js` existed, a `./x` that named `x/index.js` until `x.js`
/// appeared. Re-extracted in the run, so their own `imports` edges bind what a
/// rebuild binds and the fan-out compares their resolved entries. Reads
/// nothing unless a JS/TS file appears or vanishes.
pub(super) fn export_entry_dependents(
    conn: &rusqlite::Connection,
    files: &[String],
    delete_paths: &[String],
) -> Result<Vec<String>> {
    let mut known = conn.prepare_cached("SELECT 1 FROM files WHERE path = ?1")?;
    let mut appearing: Vec<&String> = Vec::new();
    for f in files
        .iter()
        .filter(|f| super::js_modules::is_js_module_path(f))
    {
        if !known.exists([f.as_str()])? {
            appearing.push(f);
        }
    }
    let vanishing: Vec<&String> = delete_paths
        .iter()
        .filter(|f| super::js_modules::is_js_module_path(f))
        .collect();
    if appearing.is_empty() && vanishing.is_empty() {
        return Ok(Vec::new());
    }
    let before_paths: HashSet<String> = conn
        .prepare("SELECT path FROM files")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut after_paths = before_paths.clone();
    for f in &appearing {
        after_paths.insert((*f).clone());
    }
    for f in &vanishing {
        after_paths.remove(*f);
    }
    let in_run: HashSet<&str> = files
        .iter()
        .chain(delete_paths.iter())
        .map(String::as_str)
        .collect();
    let rows = conn
        .prepare(
            "SELECT DISTINCT f.path, x.spec
             FROM js_export_entries x
             JOIN nodes m ON m.id = x.module_id
             JOIN files f ON f.id = m.file_id
             WHERE x.spec IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (path, spec) in rows {
        if in_run.contains(path.as_str()) || out.contains(&path) {
            continue;
        }
        if resolve_js_specifier_path(&spec, &path, &before_paths)
            != resolve_js_specifier_path(&spec, &path, &after_paths)
        {
            out.insert(path);
        }
    }
    Ok(out.into_iter().collect())
}
