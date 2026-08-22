//! The `exports` axis, one table row per (language, node kind), plus the
//! extractors it names.
//!
//! TypeScript/JavaScript export-statement extraction. Captures
//! `export function`, `export class`, `export interface`, `export type`,
//! `export enum`, `export abstract class`, and `export const|let` declarations
//! as REL_EXPORTS edges off `<module>`, plus `export { X } from './mod'`
//! re-exports (barrel/index files) as REL_IMPORTS dependency edges, plus the
//! CommonJS forms.
//!
//! Sibling tables: `calls::CALL_PASSES`, `imports::IMPORT_PASSES`,
//! `inherits::HERITAGE_PASSES`, `super::REFERENCE_PASSES`. This axis is the
//! smallest of them — two rows — but it was carved out of the same
//! first-match-wins match for the same reason: a language missing from a table
//! is a visible empty slot, while a missing `match` arm is an absent edge
//! nobody notices.

use super::super::lang_config::LanguageConfig;
use super::super::node_text;
use super::{LangKey, ParsedRelation};
use crate::domain::{REL_EXPORTS, REL_IMPORTS};

/// Everything an export extractor is allowed to see. Mirrors
/// `inherits::HeritageCtx`; neither row needs the enclosing scope, because an
/// export edge is always attributed to `<module>`.
pub(super) struct ExportCtx<'a> {
    pub node: tree_sitter::Node<'a>,
    pub source: &'a str,
    /// Carried for [`LangKey::Raw`] rows. No row uses one today; the field
    /// exists so that adding one resolves against the raw language rather than
    /// silently against the family, which is the "declared but not honored"
    /// failure this repo keeps paying for.
    pub language: &'a str,
    pub config: &'a LanguageConfig,
}

type ExportExtractor = fn(&ExportCtx, &mut Vec<ParsedRelation>);

/// `langs` for a row whose node kind is itself the guard — same convention as
/// `calls::ANY_LANG`.
pub(super) const ANY_LANG: &[&str] = &[];

pub(super) struct ExportPass {
    /// Which language name to match on — see [`LangKey`]. Irrelevant for
    /// [`ANY_LANG`] rows.
    pub key: LangKey,
    pub langs: &'static [&'static str],
    pub kinds: &'static [&'static str],
    pub extract: ExportExtractor,
}

/// The `exports` axis. Order is the original match order.
pub(super) const EXPORT_PASSES: &[ExportPass] = &[
    // ESM. `export_statement` is spelled by no grammar but JS/TS/TSX, so the
    // kind is its own guard.
    ExportPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["export_statement"],
        extract: run_export_names,
    },
    // CommonJS (`module.exports = { f }`, `exports.f = g`). The ESM row has no
    // counterpart for them, so dead-code classified an unused CJS export as an
    // ORPHAN — "nothing references this" — while the identical ESM code came
    // back EXPORTED_UNUSED. `assignment_expression` IS spelled by other
    // grammars, hence the language gate.
    ExportPass {
        key: LangKey::Family,
        langs: &["javascript", "typescript", "tsx"],
        kinds: &["assignment_expression"],
        extract: run_cjs_exports,
    },
];

/// Run every [`EXPORT_PASSES`] row that matches this node.
pub(super) fn run_export_passes(ctx: &ExportCtx, results: &mut Vec<ParsedRelation>) {
    let kind = ctx.node.kind();
    for pass in EXPORT_PASSES {
        if !pass.kinds.contains(&kind) {
            continue;
        }
        if !pass.langs.is_empty() {
            let lang = match pass.key {
                LangKey::Raw => ctx.language,
                LangKey::Family => ctx.config.name,
            };
            if !pass.langs.contains(&lang) {
                continue;
            }
        }
        (pass.extract)(ctx, results);
    }
}

fn run_export_names(ctx: &ExportCtx, results: &mut Vec<ParsedRelation>) {
    extract_export_names(&ctx.node, ctx.source, results);
}

fn run_cjs_exports(ctx: &ExportCtx, results: &mut Vec<ParsedRelation>) {
    extract_cjs_exports(&ctx.node, ctx.source, results);
}

pub(super) fn extract_export_names(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    // Re-export with a module source: `export { X, Y } from './mod'` /
    // `export { X as Z } from './mod'` — the barrel / index.ts pattern. Such a
    // statement is simultaneously a DEPENDENCY on './mod' and a re-export of its
    // symbols, but `extract_export_names` previously ignored the `source` field
    // entirely, so a barrel file had ZERO tracked edges: `deps` showed nothing,
    // `find-references` missed it, and affected/impact/cycles/tour could not
    // traverse THROUGH it. Emit a REL_IMPORTS edge per re-exported name, stamped
    // with the same `js_module` metadata a regular `import { X } from './mod'`
    // carries (extract_import_names), so Phase-2 resolves each to the concrete
    // file. The `name` field is the source module's export (the resolution
    // target), matching import specifiers; the optional alias is the local
    // re-export name and is irrelevant to the dependency edge.
    //
    // Star forms — `export * from './mod'` and `export * as ns from './mod'` —
    // carry no named specifiers, so they emit a MODULE-LEVEL q:"star_reexport"
    // marker instead (roadmap 2026-07-18 §2.3): the indexer binds it to the
    // resolved file's `<module>` node (the PHP-include/C-include pattern), so
    // the barrel finally participates in deps/affected/cycles/map. Name-level
    // resolution THROUGH a star barrel (`import {X} from './barrel'` where the
    // barrel star-re-exports X) still rides the default name-based fallback —
    // following star chains at resolution time remains a future enhancement.
    if let Some(src) = node.child_by_field_name("source") {
        let js_module = node_text(&src, source)
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .to_string();
        if !js_module.is_empty() {
            let metadata = Some(serde_json::json!({ "js_module": js_module }).to_string());
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    collect_reexport_specifiers(&child, source, metadata.as_deref(), results);
                }
            }
            let is_star = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .any(|c| c.kind() == "*" || c.kind() == "namespace_export");
            if is_star {
                results.push(ParsedRelation {
                    source_name: "<module>".into(),
                    target_name: "<module>".into(),
                    relation: REL_IMPORTS.into(),
                    metadata: Some(
                        serde_json::json!({ "q": crate::domain::IMPORT_Q_STAR_REEXPORT, "js_module": js_module })
                            .to_string(),
                    ),
                    source_language: String::new(),
                });
            }
        }
        // A re-export statement carries no inline declaration to extract below.
        return;
    }

    // Walk direct children for exported declarations
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "function_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "abstract_class_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(&name_node, source).to_string();
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_EXPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
            "lexical_declaration" => {
                // export const foo = ..., export let bar = ...
                for j in 0..child.named_child_count() {
                    if let Some(decl) = child.named_child(j) {
                        if decl.kind() == "variable_declarator" {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name = node_text(&name_node, source).to_string();
                                if !name.is_empty() {
                                    results.push(ParsedRelation {
                                        source_name: "<module>".into(),
                                        target_name: name,
                                        relation: REL_EXPORTS.into(),
                                        metadata: None,
                                        source_language: String::new(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Emit a REL_IMPORTS edge per re-exported name in an `export { A, B as C } from
/// '...'` clause (the `export_clause` → `export_specifier` children). Mirrors
/// extract_import_specifiers: the `name` field is the source module's export and
/// thus the dependency/resolution target; the optional `as` alias (the local
/// re-export name) does not affect the edge. Recurses through the clause wrapper.
fn collect_reexport_specifiers(
    node: &tree_sitter::Node,
    source: &str,
    metadata: Option<&str>,
    results: &mut Vec<ParsedRelation>,
) {
    if node.kind() == "export_specifier" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = node_text(&name_node, source).to_string();
            if !name.is_empty() {
                results.push(ParsedRelation {
                    source_name: "<module>".into(),
                    target_name: name,
                    relation: REL_IMPORTS.into(),
                    metadata: metadata.map(str::to_string),
                    source_language: String::new(),
                });
            }
        }
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_reexport_specifiers(&child, source, metadata, results);
        }
    }
}

/// True when the `exports` / `module` this assignment writes through is a
/// FUNCTION PARAMETER rather than the real module object.
///
/// `(function (module, exports) { exports.x = y })` — the UMD/webpack wrapper —
/// assigns to whatever the loader handed in, not to this file's exports. The
/// object's text is identical either way, so text matching alone treated every
/// wrapper body as a module-export site.
fn exports_ident_is_a_parameter(node: &tree_sitter::Node, source: &str) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(
            n.kind(),
            "function_declaration"
                | "function_expression"
                | "function"
                | "arrow_function"
                | "method_definition"
                | "generator_function"
                | "generator_function_declaration"
        ) {
            if let Some(params) = n.child_by_field_name("parameters") {
                let text = node_text(&params, source);
                // Word-boundary check: `exports` as a whole parameter name, not
                // a substring of `myExports`.
                for want in ["exports", "module"] {
                    if text
                        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
                        .any(|t| t == want)
                    {
                        return true;
                    }
                }
            }
        }
        cur = n.parent();
    }
    false
}

/// CommonJS export forms → the same `REL_EXPORTS` edges the ESM `export`
/// keyword produces.
///
/// `find_dead_code` uses an incoming `exports` edge to decide whether an unused
/// symbol is reported as an ORPHAN ("nothing references this") or as
/// EXPORTED_UNUSED ("this is public surface; something outside may use it").
/// With CommonJS unhandled, identical dead code got opposite verdicts —
/// `export function f(){}` in a `.ts` came back `exported_unused` while
/// `module.exports = { f }` in a `.js` came back `orphan`, the stronger and more
/// dangerous claim, inviting deletion of a module's public API. Every JS file in
/// this repo's own plugin is CommonJS.
///
/// Handled, all as `<module> --exports--> <symbol>`:
///   * `module.exports = { helper }`      (shorthand)
///   * `module.exports = { key: helper }` (pair — the VALUE names the symbol)
///   * `module.exports = helper`          (single binding)
///   * `exports.name = helper` / `module.exports.name = helper`
///
/// The edge targets the identifier that names a real symbol, not the export
/// key, because that is the node whose deadness is being classified. An inline
/// `exports.f = function () {}` names no node; the edge is emitted against the
/// key and simply drops in Phase 2 (an unresolved `exports` relation reaches no
/// sentinel), which is the same no-op as before.
pub(super) fn extract_cjs_exports(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    let (Some(left), Some(right)) = (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) else {
        return;
    };
    if left.kind() != "member_expression" {
        return;
    }
    // `exports` / `module` bound as a FUNCTION PARAMETER is not the module —
    // it is the UMD/webpack wrapper, `(function (module, exports) { exports.x =
    // y })`, whose `exports` is whatever the loader passed in. Matching on the
    // object's TEXT alone made every such assignment look like a real export.
    // An IIFE that closes over the real `module` (no parameter of that name) is
    // still a genuine module-level export and is unaffected.
    if exports_ident_is_a_parameter(node, source) {
        return;
    }
    let left_text = node_text(&left, source);

    let mut emit = |name: &str| {
        if name.is_empty() {
            return;
        }
        results.push(ParsedRelation {
            source_name: "<module>".into(),
            target_name: name.to_string(),
            relation: REL_EXPORTS.into(),
            metadata: None,
            source_language: String::new(),
        });
    };

    if left_text == "module.exports" {
        match right.kind() {
            // `module.exports = { a, b: c }`
            "object" => {
                for i in 0..right.named_child_count() {
                    let Some(prop) = right.named_child(i) else {
                        continue;
                    };
                    match prop.kind() {
                        "shorthand_property_identifier" => emit(node_text(&prop, source)),
                        "pair" => {
                            // ONLY the value, and only when it names a symbol.
                            // Falling back to the KEY bound the wrong node:
                            // `module.exports = { foo: 42 }` alongside a real
                            // `function foo()` marked that function exported
                            // when the export is a number. A non-identifier
                            // value (inline function, literal, member chain)
                            // names no node, so there is nothing to mark.
                            if let Some(v) = prop
                                .child_by_field_name("value")
                                .filter(|v| v.kind() == "identifier")
                            {
                                emit(node_text(&v, source));
                            }
                        }
                        _ => {}
                    }
                }
            }
            // `module.exports = helper`
            "identifier" => emit(node_text(&right, source)),
            _ => {}
        }
        return;
    }

    // `exports.name = helper` / `module.exports.name = helper`
    let is_exports_member = left
        .child_by_field_name("object")
        .map(|o| {
            let t = node_text(&o, source);
            t == "exports" || t == "module.exports"
        })
        .unwrap_or(false);
    if !is_exports_member {
        return;
    }
    let named = if right.kind() == "identifier" {
        Some(right)
    } else {
        left.child_by_field_name("property")
    };
    if let Some(n) = named {
        emit(node_text(&n, source));
    }
}
