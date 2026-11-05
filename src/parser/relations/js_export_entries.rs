//! The export entries of a JavaScript / TypeScript module, in source order: what
//! ECMAScript's `ResolveExport` reads (a module record's local, indirect and
//! star export entries), plus the CommonJS forms that forward another module.
//!
//! The re-export walk (`indexer::pipeline::resolve`) used to read a barrel's
//! bound `imports` edges instead. Those are an OUTPUT of resolution: they exist
//! only once bound (none at all for `export *` of a file not there yet, D#739),
//! one re-export bound by name to two namesakes is two of them, they carry no
//! order, and a plain `import` reads the same as a re-export (D#740). These
//! entries are a function of the file's own text, so a walk over them is a
//! function of the files and the file set alone.

use super::super::node_text;

/// One export entry, as ECMAScript's module record keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsExportEntry {
    /// The file's own binding `local`, exported as `export`: `export function
    /// f() {}` (`f` as `f`), `export { g as h }` (`g` as `h`), `export default
    /// function d() {}` (`d` as `default`).
    Local { export: String, local: String },
    /// `export` is what `spec` exports as `import` (`export { a as b } from
    /// './x'`, or an export of a binding the file imports or requires), or that
    /// module's namespace when `import` is `None` (`export * as ns from './x'`).
    Indirect {
        export: String,
        spec: String,
        import: Option<String>,
    },
    /// `export * from 'spec'`; in CommonJS `module.exports = require('spec')`
    /// and `module.exports = { ...require('spec') }`.
    Star { spec: String },
}

/// What a top-level binding of the file was imported as.
struct Imported {
    spec: String,
    /// `None`: the module's namespace (`import * as m`, `const m = require()`).
    import: Option<String>,
}

/// The export entries of a JS/TS file, in source order. Only top-level
/// statements: an export can be nowhere else, and a CommonJS assignment inside
/// a function or a branch may never run.
pub fn js_export_entries(tree: &tree_sitter::Tree, source: &str) -> Vec<JsExportEntry> {
    let root = tree.root_node();
    let statements: Vec<tree_sitter::Node> = (0..root.named_child_count())
        .filter_map(|i| root.named_child(i))
        .collect();
    // Imports are hoisted: an `export { x }` above `import { x } from './a'`
    // still exports the import.
    let mut imported: std::collections::HashMap<String, Imported> = Default::default();
    for s in &statements {
        collect_imported(s, source, &mut imported);
    }
    let mut out = Vec::new();
    let mut commonjs = CommonJs::default();
    for s in &statements {
        match s.kind() {
            "export_statement" => export_statement_entries(s, source, &imported, &mut out),
            "expression_statement" => {
                if let Some(e) = s
                    .named_child(0)
                    .filter(|e| e.kind() == "assignment_expression")
                {
                    commonjs.assignment(&e, source, &imported);
                }
            }
            _ => {}
        }
    }
    out.extend(commonjs.entries);
    out
}

fn unquote(text: &str) -> String {
    text.trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

/// An `identifier` or `string` export / import name.
fn module_export_name(node: &tree_sitter::Node, source: &str) -> String {
    if node.kind() == "string" {
        unquote(node_text(node, source))
    } else {
        node_text(node, source).to_string()
    }
}

/// `'./x'` of `require('./x')`, when `node` is that call.
fn require_spec(node: &tree_sitter::Node, source: &str) -> Option<String> {
    if node.kind() != "call_expression" {
        return None;
    }
    let function = node.child_by_field_name("function")?;
    if function.kind() != "identifier" || node_text(&function, source) != "require" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let first = args.named_child(0)?;
    if args.named_child_count() != 1 || first.kind() != "string" {
        return None;
    }
    let spec = unquote(node_text(&first, source));
    (!spec.is_empty()).then_some(spec)
}

/// `(spec, import)` of `require('./x')` (the namespace) or `require('./x').a`.
fn required_value(node: &tree_sitter::Node, source: &str) -> Option<Imported> {
    if let Some(spec) = require_spec(node, source) {
        return Some(Imported { spec, import: None });
    }
    if node.kind() == "member_expression" {
        let object = node.child_by_field_name("object")?;
        let property = node.child_by_field_name("property")?;
        let spec = require_spec(&object, source)?;
        return Some(Imported {
            spec,
            import: Some(node_text(&property, source).to_string()),
        });
    }
    None
}

fn collect_imported(
    statement: &tree_sitter::Node,
    source: &str,
    imported: &mut std::collections::HashMap<String, Imported>,
) {
    match statement.kind() {
        "import_statement" => {
            let Some(spec) = statement
                .child_by_field_name("source")
                .map(|s| unquote(node_text(&s, source)))
                .filter(|s| !s.is_empty())
            else {
                // TS `import x = require('./x')`.
                for i in 0..statement.named_child_count() {
                    let Some(c) = statement.named_child(i) else {
                        continue;
                    };
                    if c.kind() != "import_require_clause" {
                        continue;
                    }
                    let spec = c.child_by_field_name("source");
                    let name = (0..c.named_child_count())
                        .filter_map(|j| c.named_child(j))
                        .find(|n| n.kind() == "identifier");
                    if let (Some(spec), Some(name)) = (spec, name) {
                        imported.insert(
                            node_text(&name, source).to_string(),
                            Imported {
                                spec: unquote(node_text(&spec, source)),
                                import: None,
                            },
                        );
                    }
                }
                return;
            };
            for i in 0..statement.named_child_count() {
                let Some(clause) = statement.named_child(i) else {
                    continue;
                };
                if clause.kind() != "import_clause" {
                    continue;
                }
                for j in 0..clause.named_child_count() {
                    let Some(b) = clause.named_child(j) else {
                        continue;
                    };
                    match b.kind() {
                        "identifier" => {
                            imported.insert(
                                node_text(&b, source).to_string(),
                                Imported {
                                    spec: spec.clone(),
                                    import: Some("default".to_string()),
                                },
                            );
                        }
                        "namespace_import" => {
                            if let Some(n) = (0..b.named_child_count())
                                .filter_map(|k| b.named_child(k))
                                .find(|n| n.kind() == "identifier")
                            {
                                imported.insert(
                                    node_text(&n, source).to_string(),
                                    Imported {
                                        spec: spec.clone(),
                                        import: None,
                                    },
                                );
                            }
                        }
                        "named_imports" => {
                            for k in 0..b.named_child_count() {
                                let Some(s) = b.named_child(k) else {
                                    continue;
                                };
                                if s.kind() != "import_specifier" {
                                    continue;
                                }
                                let Some(name) = s.child_by_field_name("name") else {
                                    continue;
                                };
                                let name = module_export_name(&name, source);
                                let local = s
                                    .child_by_field_name("alias")
                                    .map(|a| node_text(&a, source).to_string())
                                    .unwrap_or_else(|| name.clone());
                                imported.insert(
                                    local,
                                    Imported {
                                        spec: spec.clone(),
                                        import: Some(name),
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            for i in 0..statement.named_child_count() {
                let Some(d) = statement.named_child(i) else {
                    continue;
                };
                if d.kind() != "variable_declarator" {
                    continue;
                }
                let (Some(name), Some(value)) = (
                    d.child_by_field_name("name"),
                    d.child_by_field_name("value"),
                ) else {
                    continue;
                };
                let Some(required) = required_value(&value, source) else {
                    continue;
                };
                match name.kind() {
                    "identifier" => {
                        imported.insert(node_text(&name, source).to_string(), required);
                    }
                    // `const { a, b: c } = require('./x')`; destructuring a
                    // member (`require('./x').a`) is no import of a name here.
                    "object_pattern" if required.import.is_none() => {
                        for j in 0..name.named_child_count() {
                            let Some(p) = name.named_child(j) else {
                                continue;
                            };
                            let (key, local) = match p.kind() {
                                "shorthand_property_identifier_pattern" => {
                                    let n = node_text(&p, source).to_string();
                                    (n.clone(), n)
                                }
                                "pair_pattern" => {
                                    let (Some(k), Some(v)) = (
                                        p.child_by_field_name("key"),
                                        p.child_by_field_name("value"),
                                    ) else {
                                        continue;
                                    };
                                    if v.kind() != "identifier" {
                                        continue;
                                    }
                                    (
                                        module_export_name(&k, source),
                                        node_text(&v, source).to_string(),
                                    )
                                }
                                _ => continue,
                            };
                            imported.insert(
                                local,
                                Imported {
                                    spec: required.spec.clone(),
                                    import: Some(key),
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// The entry exporting the file's binding `local` as `export`: indirect when
/// `local` is a binding the file imported, else local.
fn binding_entry(
    export: String,
    local: &str,
    imported: &std::collections::HashMap<String, Imported>,
) -> JsExportEntry {
    match imported.get(local) {
        Some(i) => JsExportEntry::Indirect {
            export,
            spec: i.spec.clone(),
            import: i.import.clone(),
        },
        None => JsExportEntry::Local {
            export,
            local: local.to_string(),
        },
    }
}

fn export_statement_entries(
    statement: &tree_sitter::Node,
    source: &str,
    imported: &std::collections::HashMap<String, Imported>,
    out: &mut Vec<JsExportEntry>,
) {
    let children: Vec<tree_sitter::Node> = (0..statement.child_count())
        .filter_map(|i| statement.child(i))
        .collect();
    if let Some(src) = statement.child_by_field_name("source") {
        let spec = unquote(node_text(&src, source));
        if spec.is_empty() {
            return;
        }
        if let Some(ns) = children.iter().find(|c| c.kind() == "namespace_export") {
            if let Some(name) = ns.named_child(0) {
                out.push(JsExportEntry::Indirect {
                    export: module_export_name(&name, source),
                    spec,
                    import: None,
                });
            }
        } else if let Some(clause) = children.iter().find(|c| c.kind() == "export_clause") {
            for s in export_specifiers(clause) {
                let Some(name) = s.child_by_field_name("name") else {
                    continue;
                };
                let name = module_export_name(&name, source);
                let export = s
                    .child_by_field_name("alias")
                    .map(|a| module_export_name(&a, source))
                    .unwrap_or_else(|| name.clone());
                out.push(JsExportEntry::Indirect {
                    export,
                    spec: spec.clone(),
                    import: Some(name),
                });
            }
        } else if children.iter().any(|c| c.kind() == "*") {
            out.push(JsExportEntry::Star { spec });
        }
        return;
    }
    if let Some(clause) = children.iter().find(|c| c.kind() == "export_clause") {
        for s in export_specifiers(clause) {
            let Some(name) = s.child_by_field_name("name") else {
                continue;
            };
            let local = module_export_name(&name, source);
            let export = s
                .child_by_field_name("alias")
                .map(|a| module_export_name(&a, source))
                .unwrap_or_else(|| local.clone());
            out.push(binding_entry(export, &local, imported));
        }
        return;
    }
    if children.iter().any(|c| c.kind() == "default") {
        let named = statement
            .child_by_field_name("declaration")
            .or_else(|| statement.child_by_field_name("value"));
        if let Some(v) = named {
            if v.kind() == "identifier" {
                out.push(binding_entry(
                    "default".to_string(),
                    node_text(&v, source),
                    imported,
                ));
            } else if let Some(name) = v.child_by_field_name("name") {
                out.push(JsExportEntry::Local {
                    export: "default".to_string(),
                    local: node_text(&name, source).to_string(),
                });
            }
        }
        return;
    }
    if let Some(decl) = statement.child_by_field_name("declaration") {
        let mut names = Vec::new();
        declared_names(&decl, source, &mut names, 0);
        for n in names {
            out.push(JsExportEntry::Local {
                export: n.clone(),
                local: n,
            });
        }
    }
}

fn export_specifiers<'a>(clause: &tree_sitter::Node<'a>) -> Vec<tree_sitter::Node<'a>> {
    (0..clause.named_child_count())
        .filter_map(|i| clause.named_child(i))
        .filter(|s| s.kind() == "export_specifier")
        .collect()
}

/// The names an exported declaration binds.
fn declared_names(decl: &tree_sitter::Node, source: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 4 {
        return;
    }
    match decl.kind() {
        "lexical_declaration" | "variable_declaration" => {
            for i in 0..decl.named_child_count() {
                let Some(d) = decl.named_child(i) else {
                    continue;
                };
                if d.kind() == "variable_declarator" {
                    if let Some(name) = d.child_by_field_name("name") {
                        pattern_names(&name, source, out, 0);
                    }
                }
            }
        }
        // `export declare function f(): void;`, `export declare const x: T;`
        "ambient_declaration" => {
            for i in 0..decl.named_child_count() {
                if let Some(d) = decl.named_child(i) {
                    declared_names(&d, source, out, depth + 1);
                }
            }
        }
        // `export import A = B.C` is an alias of a namespace member, not a module.
        "import_alias" => {}
        _ => {
            if let Some(name) = decl.child_by_field_name("name") {
                let n = node_text(&name, source);
                if !n.is_empty() {
                    out.push(n.to_string());
                }
            }
        }
    }
}

/// The identifiers a binding pattern binds (`{ a, b: [c, ...d] = e }`).
fn pattern_names(node: &tree_sitter::Node, source: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            out.push(node_text(node, source).to_string());
        }
        "pair_pattern" => {
            if let Some(v) = node.child_by_field_name("value") {
                pattern_names(&v, source, out, depth + 1);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(l) = node.child_by_field_name("left") {
                pattern_names(&l, source, out, depth + 1);
            }
        }
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            for i in 0..node.named_child_count() {
                if let Some(c) = node.named_child(i) {
                    pattern_names(&c, source, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// The CommonJS writes of a file that forward another module, applied in
/// order as its top level runs (pre-tag review of 0.186.0, MEDIUM-3: read as
/// ESM entries, a name written twice kept its first write): the last write of
/// a name wins; `module.exports = …` replaces everything before it and leaves
/// `exports` naming the old object, unless the same chain rebinds it
/// (`exports = module.exports = …`); rebinding `exports` alone exports
/// nothing. A file's own values exported this way are `exports` edges already
/// (the exports axis).
#[derive(Default)]
struct CommonJs {
    entries: Vec<JsExportEntry>,
    /// `exports` no longer names `module.exports`.
    detached: bool,
}

impl CommonJs {
    fn assignment(
        &mut self,
        assignment: &tree_sitter::Node,
        source: &str,
        imported: &std::collections::HashMap<String, Imported>,
    ) {
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            return;
        };
        if matches!(node_text(&left, source), "module.exports" | "exports") {
            let (mut module, mut alias) = (false, false);
            let mut node = *assignment;
            let value = loop {
                let (Some(l), Some(r)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) else {
                    return;
                };
                match node_text(&l, source) {
                    "module.exports" => module = true,
                    "exports" => alias = true,
                    _ => return,
                }
                if r.kind() == "assignment_expression" {
                    node = r;
                } else {
                    break r;
                }
            };
            // `exports = module.exports`: both name one object again.
            if matches!(node_text(&value, source), "module.exports" | "exports") {
                self.detached = false;
                return;
            }
            if module {
                self.entries.clear();
                whole_exports_entries(&value, source, imported, &mut self.entries);
            }
            self.detached = !(module && alias);
            return;
        }
        if left.kind() != "member_expression" {
            return;
        }
        let (Some(object), Some(property)) = (
            left.child_by_field_name("object"),
            left.child_by_field_name("property"),
        ) else {
            return;
        };
        match node_text(&object, source) {
            "module.exports" => {}
            "exports" if !self.detached => {}
            _ => return,
        }
        let export = node_text(&property, source).to_string();
        write_export(&mut self.entries, export, &right, source, imported);
    }
}

/// Write `value` as the export `export` over what `entries` had for it: an
/// entry when it forwards another module, none when it is the file's own.
fn write_export(
    entries: &mut Vec<JsExportEntry>,
    export: String,
    value: &tree_sitter::Node,
    source: &str,
    imported: &std::collections::HashMap<String, Imported>,
) {
    entries.retain(|e| !matches!(e, JsExportEntry::Indirect { export: x, .. } if *x == export));
    if let Some(e) = forwarded_value(export, value, source, imported) {
        entries.push(e);
    }
}

/// The entry for exporting `value` as `export`, when `value` forwards another
/// module: a binding the file required or imported, or a `require` itself.
fn forwarded_value(
    export: String,
    value: &tree_sitter::Node,
    source: &str,
    imported: &std::collections::HashMap<String, Imported>,
) -> Option<JsExportEntry> {
    if matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
        let i = imported.get(node_text(value, source))?;
        return Some(JsExportEntry::Indirect {
            export,
            spec: i.spec.clone(),
            import: i.import.clone(),
        });
    }
    let r = required_value(value, source)?;
    Some(JsExportEntry::Indirect {
        export,
        spec: r.spec,
        import: r.import,
    })
}

/// The module's namespace `value` forwards wholesale, if any: `require('./x')`
/// or a binding that holds it.
fn forwarded_namespace(
    value: &tree_sitter::Node,
    source: &str,
    imported: &std::collections::HashMap<String, Imported>,
) -> Option<String> {
    if let Some(spec) = require_spec(value, source) {
        return Some(spec);
    }
    if value.kind() == "identifier" {
        let i = imported.get(node_text(value, source))?;
        if i.import.is_none() {
            return Some(i.spec.clone());
        }
    }
    None
}

fn whole_exports_entries(
    value: &tree_sitter::Node,
    source: &str,
    imported: &std::collections::HashMap<String, Imported>,
    out: &mut Vec<JsExportEntry>,
) {
    if let Some(spec) = forwarded_namespace(value, source, imported) {
        out.push(JsExportEntry::Star { spec });
        return;
    }
    if value.kind() != "object" {
        return;
    }
    for i in 0..value.named_child_count() {
        let Some(p) = value.named_child(i) else {
            continue;
        };
        match p.kind() {
            // A spread overrides the properties before it with whatever its
            // module exports, which the file's text cannot say: those are left
            // to it, so the walk answers them through the module.
            "spread_element" => {
                if let Some(spec) = p
                    .named_child(0)
                    .and_then(|e| forwarded_namespace(&e, source, imported))
                {
                    out.retain(|e| matches!(e, JsExportEntry::Star { .. }));
                    out.push(JsExportEntry::Star { spec });
                }
            }
            "shorthand_property_identifier" => {
                write_export(out, node_text(&p, source).to_string(), &p, source, imported);
            }
            "pair" => {
                let (Some(k), Some(v)) =
                    (p.child_by_field_name("key"), p.child_by_field_name("value"))
                else {
                    continue;
                };
                if !matches!(k.kind(), "property_identifier" | "string") {
                    continue;
                }
                write_export(out, module_export_name(&k, source), &v, source, imported);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::JsExportEntry::{self, *};

    fn entries(language: &str, src: &str) -> Vec<JsExportEntry> {
        let tree = crate::parser::treesitter::parse_tree(src, language).unwrap();
        super::js_export_entries(&tree, src)
    }

    fn local(e: &str, l: &str) -> JsExportEntry {
        Local {
            export: e.into(),
            local: l.into(),
        }
    }

    fn indirect(e: &str, spec: &str, i: Option<&str>) -> JsExportEntry {
        Indirect {
            export: e.into(),
            spec: spec.into(),
            import: i.map(Into::into),
        }
    }

    fn star(spec: &str) -> JsExportEntry {
        Star { spec: spec.into() }
    }

    /// One row per export form: the source, and the entries it must produce, in
    /// order. JS and TS grammars both.
    #[test]
    fn every_export_form_yields_its_ecmascript_entries_in_source_order() {
        let rows: &[(&str, &str, Vec<JsExportEntry>)] = &[
            ("javascript", "export function f() {}", vec![local("f", "f")]),
            ("javascript", "export function* g() {}", vec![local("g", "g")]),
            ("javascript", "export async function h() {}", vec![local("h", "h")]),
            ("javascript", "export class K {}", vec![local("K", "K")]),
            ("javascript", "export const a = 1, { b, c: [d] } = o;", vec![local("a", "a"), local("b", "b"), local("d", "d")]),
            ("javascript", "export var v = 1;", vec![local("v", "v")]),
            ("javascript", "function f() {}\nexport { f, f as g };", vec![local("f", "f"), local("g", "f")]),
            ("javascript", "export default function d() {}", vec![local("default", "d")]),
            ("javascript", "export default class C {}", vec![local("default", "C")]),
            ("javascript", "const x = 1;\nexport default x;", vec![local("default", "x")]),
            ("javascript", "export default () => 1;", vec![]),
            ("javascript", "export { a, b as c } from './m';", vec![indirect("a", "./m", Some("a")), indirect("c", "./m", Some("b"))]),
            ("javascript", "export { default } from './m';\nexport { default as D } from './n';", vec![indirect("default", "./m", Some("default")), indirect("D", "./n", Some("default"))]),
            ("javascript", "export * from './m';", vec![star("./m")]),
            ("javascript", "export * as ns from './m';", vec![indirect("ns", "./m", None)]),
            // An export of an imported binding is an indirect entry (ECMAScript
            // ParseModule), hoisting included; a plain import exports nothing.
            ("javascript", "export { x, y as z, w, n };\nimport { a as x } from './a';\nimport y from './b';\nimport * as w from './c';\nimport { n } from './d';", vec![indirect("x", "./a", Some("a")), indirect("z", "./b", Some("default")), indirect("w", "./c", None), indirect("n", "./d", Some("n"))]),
            ("javascript", "import { f } from './a';\nexport function use() { return f(); }", vec![local("use", "use")]),
            ("javascript", "import f from './a';\nexport default f;", vec![indirect("default", "./a", Some("default"))]),
            // Source order across forms.
            ("javascript", "export * from './z';\nexport { q } from './y';\nexport function own() {}", vec![star("./z"), indirect("q", "./y", Some("q")), local("own", "own")]),
            // CommonJS forwarding.
            ("javascript", "module.exports = require('./mid.js');", vec![star("./mid.js")]),
            ("javascript", "exports = module.exports = require('./lib/x');", vec![star("./lib/x")]),
            ("javascript", "module.exports = { ...require('./a.js'), own };", vec![star("./a.js")]),
            ("javascript", "const m = require('./m');\nmodule.exports = m;", vec![star("./m")]),
            ("javascript", "const { f, g: h } = require('./a.js');\nconst k = require('./k').kk;\nconst ns = require('./n');\nmodule.exports = { f, h, alias: k, ns, sub: require('./s'), local };", vec![indirect("f", "./a.js", Some("f")), indirect("h", "./a.js", Some("g")), indirect("alias", "./k", Some("kk")), indirect("ns", "./n", None), indirect("sub", "./s", None)]),
            ("javascript", "const { f } = require('./a');\nexports.f = f;\nmodule.exports.g = require('./b').g;", vec![indirect("f", "./a", Some("f")), indirect("g", "./b", Some("g"))]),
            // CommonJS writes apply in order (pre-tag review of 0.186.0,
            // MEDIUM-3): the last write of a name wins, `module.exports = …`
            // drops what came before and detaches `exports`, rebinding
            // `exports` alone exports nothing, and a spread may override the
            // properties before it, so those are left to it.
            ("javascript", "exports.f = require('./a').f;\nexports.f = require('./b').f;", vec![indirect("f", "./b", Some("f"))]),
            ("javascript", "module.exports = require('./a');\nmodule.exports = require('./b');", vec![star("./b")]),
            ("javascript", "exports.f = require('./a').f;\nmodule.exports = { g: require('./b').g };", vec![indirect("g", "./b", Some("g"))]),
            ("javascript", "module.exports = { f: require('./a').f };\nexports.g = require('./b').g;", vec![indirect("f", "./a", Some("f"))]),
            ("javascript", "exports = module.exports = { f: require('./a').f };\nexports.g = require('./b').g;", vec![indirect("f", "./a", Some("f")), indirect("g", "./b", Some("g"))]),
            ("javascript", "module.exports = { f: require('./a').f };\nmodule.exports.g = require('./b').g;", vec![indirect("f", "./a", Some("f")), indirect("g", "./b", Some("g"))]),
            ("javascript", "exports = { f: require('./a').f };", vec![]),
            ("javascript", "module.exports = { g: require('./b').g };\nexports = module.exports;\nexports.f = require('./a').f;", vec![indirect("g", "./b", Some("g")), indirect("f", "./a", Some("f"))]),
            ("javascript", "module.exports = { f: require('./a').f, f: require('./b').f };", vec![indirect("f", "./b", Some("f"))]),
            ("javascript", "module.exports = { f: require('./b').f, ...require('./a') };", vec![star("./a")]),
            ("javascript", "module.exports = { ...require('./a'), f: require('./b').f };", vec![star("./a"), indirect("f", "./b", Some("f"))]),
            ("javascript", "const { f } = require('./a');\nexports.f = f;\nexports.f = function () {};", vec![]),
            // A file's own CommonJS values are the exports axis's.
            ("javascript", "function f() {}\nmodule.exports = { f };\nexports.g = function () {};", vec![]),
            // Only top-level statements.
            ("javascript", "if (x) { module.exports = require('./a'); }\nfunction w() { exports.f = require('./b'); }", vec![]),
            // TypeScript.
            ("typescript", "export interface I {}\nexport type T = 1;\nexport enum E {}\nexport abstract class A {}\nexport namespace N {}", vec![local("I", "I"), local("T", "T"), local("E", "E"), local("A", "A"), local("N", "N")]),
            ("typescript", "export declare function f(): void;\nexport declare const c: number;", vec![local("f", "f"), local("c", "c")]),
            ("typescript", "import x = require('./x');\nexport { x };", vec![indirect("x", "./x", None)]),
            ("typescript", "export type { T } from './t';\nexport * from './u';", vec![indirect("T", "./t", Some("T")), star("./u")]),
            ("tsx", "export const C = () => <div/>;\nexport { D as E } from './d';", vec![local("C", "C"), indirect("E", "./d", Some("D"))]),
        ];
        let mut failures = Vec::new();
        for (language, src, want) in rows {
            let got = entries(language, src);
            if &got != want {
                failures.push(format!(
                    "{language} {src:?}\n  want {want:?}\n  got  {got:?}"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
