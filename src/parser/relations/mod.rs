//! Relation extraction from parsed tree-sitter trees.
//!
//! Public surface:
//! - `ParsedRelation` (intermediate struct consumed by the indexer's edge resolver)
//! - `extract_relations` (parses + walks)
//! - `extract_relations_from_tree` (walks a pre-parsed tree)
//!
//! Internals are split per concern:
//! - `helpers`: shared callee/string utilities used by every language arm
//! - `imports`: generic JS/TS/Java + Python imports
//! - `inherits`: superclass + implements across class-style languages
//! - `exports`: TS/JS export statements
//! - `routes`: Express/Go/Python HTTP route registrations
//! - `rust`: Rust-specific `use` and `impl Trait for Type`
//! - `dart`: Dart-specific imports and call expressions
//!
//! - `calls`: the `calls` axis as a table, one row per (language, node kind)
//!
//! `walk_for_relations` is the single recursive dispatcher that maps tree-sitter
//! node kinds to the appropriate extractor. The RECURSION must stay in one place
//! — it is what propagates `current_scope` / `current_class` / `current_rust_impl`
//! down the tree, and splitting it per language would either duplicate the walk
//! or lose that context. The EXTRACTION does not have to, and no longer does:
//! every relation axis is a table whose extractors receive the resolved context
//! as data — `REFERENCE_PASSES`, `calls::CALL_PASSES`, `imports::IMPORT_PASSES`,
//! `inherits::HERITAGE_PASSES`, `exports::EXPORT_PASSES`. That makes each axis
//! enumerable: a language missing from a table is a visible empty slot rather
//! than an absent `if` nobody notices, which is the failure that produced the
//! v0.83.0 per-language gaps and the 2026-08-16 heritage gaps.
//!
//! One arm is left in the `match`: the Python `decorated_definition` route,
//! the sole walk-resident member of the `routes` axis (Express and axum routes
//! are calls, so they arrive through `CALL_PASSES`). A one-row table is a shape
//! without evidence; it becomes one when a second decorator-spelled framework
//! arrives.
//!
//! The tables run EVERY matching row, where the `match` they replaced ran at
//! most one. `super::tests::no_node_kind_reaches_two_heritage_or_export_rows`
//! is what keeps that equivalent.

use super::lang_config::LanguageConfig;
use super::node_text;
use crate::domain::MAX_RELATION_DEPTH;
#[cfg(test)]
use crate::domain::REL_IMPORTS;
use anyhow::Result;

mod calls;
mod cpp;
mod dart;
mod exports;
mod go;
mod helpers;
mod imports;
mod inherits;
mod java;
mod python;
mod routes;
mod rust;
mod typescript;

/// Serialize a CalleeQualifier into the wire-format JSON for `edges.metadata`.
/// Bare → None (matches non-Rust callers and old DB rows).
/// See spec §"Wire protocol" for the q/v key shapes.
fn serialize_callee_qualifier(q: &helpers::CalleeQualifier) -> Option<String> {
    use helpers::CalleeQualifier::*;
    match q {
        Bare => None,
        // Build via serde_json so source-derived payloads (`v`) are escaped
        // correctly — a receiver/type/path segment containing `"` or `\` would
        // otherwise produce malformed JSON that parse_callee_metadata / the
        // json_extract SQL consumers silently reject. Key order matches the old
        // format! output for the common identifier-only case.
        Path(segments) => {
            let v = segments.join("::");
            Some(serde_json::json!({ "q": "path", "v": v }).to_string())
        }
        SelfType(t) => Some(serde_json::json!({ "q": "stype", "v": t }).to_string()),
        SelfRecv(t) => Some(serde_json::json!({ "q": "self", "v": t }).to_string()),
        Receiver(r) => Some(serde_json::json!({ "q": "recv", "v": r }).to_string()),
        Chain => Some(serde_json::json!({ "q": "chain" }).to_string()),
    }
}

/// Build the `{"q":"rtype","v":<ty>}` call-edge metadata (Python receiver-type
/// inference). Uses serde_json so a type name containing `"`/`\` is escaped into
/// valid JSON — byte-identical to the old `format!` form for the identifier-only
/// inputs it actually receives. Mirrors `serialize_callee_qualifier`.
fn serialize_rtype_metadata(ty: &str) -> String {
    serde_json::json!({ "q": "rtype", "v": ty }).to_string()
}

/// Build the `{"q":"impl_method","v":<ty>}` implements-edge metadata (Rust trait
/// impl method disambiguation). serde_json escapes `"`/`\` in the type name;
/// byte-identical to the old `format!` form for identifier-only inputs.
fn serialize_impl_method_metadata(ty: &str) -> String {
    serde_json::json!({ "q": "impl_method", "v": ty }).to_string()
}

#[cfg(test)]
mod tests;

use cpp::extract_cpp_value_reference;
use dart::extract_dart_call_from_selector;
use go::{extract_go_type_reference, extract_go_value_reference};
use java::extract_java_type_reference;
use python::{
    extract_python_type_reference, extract_python_value_reference, infer_python_call_receiver_type,
};
use routes::{extract_python_route, extract_route_pattern};
use rust::{
    extract_rust_macro_token_call, extract_rust_path_reference, extract_rust_type_reference,
    extract_rust_value_reference,
};
use typescript::{extract_js_value_reference, extract_ts_type_reference};

pub struct ParsedRelation {
    pub source_name: String,
    pub target_name: String,
    pub relation: String,
    pub metadata: Option<String>,
    /// Language of the file that produced this relation. Stamped by
    /// `extract_relations_from_tree`; used by the edge resolver in pipeline.rs
    /// to enforce same-language hard equality on cross-file `calls` edges
    /// (prevents false positives like Python `foo()` matching a C `foo()`).
    pub source_language: String,
}

pub fn extract_relations(source: &str, language: &str) -> Result<Vec<ParsedRelation>> {
    let tree = super::treesitter::parse_tree(source, language)?;
    Ok(extract_relations_from_tree(&tree, source, language))
}

/// Extract relations from a pre-parsed tree (avoids re-parsing).
pub fn extract_relations_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
) -> Vec<ParsedRelation> {
    let mut relations = Vec::new();
    let config = LanguageConfig::for_language(language);
    // The Rust local-binding memo is keyed by tree-sitter node id, and ids are
    // unique only WITHIN a tree. Clearing here — the one entry point every file's
    // walk goes through — is what makes a cross-file hit impossible; a leaked
    // entry would silently suppress real call edges in an unrelated file.
    // Unconditional (not gated on `language == "rust"`) so a non-Rust file can
    // never carry a previous Rust file's entries into the next Rust one.
    rust::reset_fn_local_names_cache();
    walk_for_relations(
        tree.root_node(),
        source,
        language,
        &config,
        None,
        None,
        None,
        &mut relations,
        0,
    );
    // Ruby bare (parens-less) method calls are `identifier` nodes structurally
    // identical to local-variable reads (the `"call"` arm above only fires on
    // parenthesized / receiver calls). Resolve them with Ruby's own rule in a
    // dedicated scope-aware pass.
    if config.name == "ruby" {
        ruby_bare_calls::extract(tree.root_node(), source, "<module>", &mut relations);
    }
    // Stamp source_language on every relation. walk_for_relations constructs
    // ParsedRelation with source_language: String::new(), and we fill it in
    // here so every call site inside walk doesn't need to propagate language.
    for r in &mut relations {
        r.source_language = language.to_string();
    }
    relations
}

/// Ruby bare (parens-less) method-call extraction.
///
/// tree-sitter-ruby parses a bare name (`helper`, no receiver/parens) as an
/// `identifier`, structurally indistinguishable from a local-variable read
/// (`result` after `result = …`). Ruby itself disambiguates by scope: a bare
/// name is a local variable iff it was BOUND (assigned / parameter) in the
/// enclosing method scope, otherwise it is a method call. This pass replicates
/// that rule, biased to false-negatives (the safe direction for an LLM-facing
/// tool, matching the dead-code philosophy):
///
/// - only STATEMENT-position bare identifiers emit a call (not RHS / args /
///   conditions — those add ambiguity for little dead-code value);
/// - a name bound ANYWHERE in its scope is treated as a local for ALL its bare
///   uses (no order tracking) — so a real local never invents an edge;
/// - undefined callees additionally drop at Phase-2 same-language resolution.
///
/// Parenthesized / receiver calls are already handled by the `"call"` arm.
mod ruby_bare_calls {
    use super::{node_text, ParsedRelation};
    use crate::domain::REL_CALLS;
    use std::collections::HashSet;
    use tree_sitter::Node;

    /// Process one scope rooted at `scope_root` (the `program` for top level, or
    /// a `method`/`singleton_method` node). Nested methods recurse as fresh scopes.
    pub(super) fn extract<'a>(
        scope_root: Node,
        source: &'a str,
        scope_name: &str,
        out: &mut Vec<ParsedRelation>,
    ) {
        let is_method = matches!(scope_root.kind(), "method" | "singleton_method");
        // Method parameters are locals.
        let mut bound: HashSet<&'a str> = HashSet::new();
        if is_method {
            if let Some(params) = scope_root.child_by_field_name("parameters") {
                collect_idents(params, source, &mut bound);
            }
        }
        let body = if is_method {
            scope_root.child_by_field_name("body")
        } else {
            Some(scope_root)
        };
        let Some(body) = body else { return };
        collect_locals(body, source, &mut bound);
        emit(body, source, scope_name, &bound, out);
    }

    /// Collect names bound as locals in this scope's `body` (assignment targets,
    /// block params, `for` vars, rescue vars). Stops at nested method bodies —
    /// those are separate scopes.
    fn collect_locals<'a>(node: Node, source: &'a str, set: &mut HashSet<&'a str>) {
        let mut c = node.walk();
        for child in node.named_children(&mut c) {
            match child.kind() {
                "method" | "singleton_method" => {} // nested scope — skip
                "assignment" | "operator_assignment" => {
                    if let Some(left) = child.child_by_field_name("left") {
                        collect_idents(left, source, set);
                    }
                    if let Some(right) = child.child_by_field_name("right") {
                        collect_locals(right, source, set);
                    }
                }
                "block_parameters" => collect_idents(child, source, set),
                "exception_variable" => collect_idents(child, source, set),
                "for" => {
                    if let Some(p) = child.child_by_field_name("pattern") {
                        collect_idents(p, source, set);
                    }
                    collect_locals(child, source, set);
                }
                _ => collect_locals(child, source, set),
            }
        }
    }

    /// Insert every `identifier` in `node`'s subtree (used for assignment LHS and
    /// parameter lists). Over-collecting a default-value call name as "bound" only
    /// suppresses one of its own bare-call edges — a safe false-negative.
    fn collect_idents<'a>(node: Node, source: &'a str, set: &mut HashSet<&'a str>) {
        if node.kind() == "identifier" {
            set.insert(node_text(&node, source));
            return;
        }
        let mut c = node.walk();
        for child in node.named_children(&mut c) {
            collect_idents(child, source, set);
        }
    }

    /// Walk `node`, emitting a `calls` edge for each statement-position bare
    /// identifier that is callable and not a bound local. Recurses into nested
    /// methods as fresh scopes.
    fn emit<'a>(
        node: Node,
        source: &'a str,
        scope_name: &str,
        bound: &HashSet<&'a str>,
        out: &mut Vec<ParsedRelation>,
    ) {
        let stmt_position = matches!(
            node.kind(),
            "body_statement" | "then" | "else" | "ensure" | "program" | "do" | "begin"
        );
        let mut c = node.walk();
        for child in node.named_children(&mut c) {
            match child.kind() {
                "method" | "singleton_method" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(&n, source))
                        .unwrap_or("<module>");
                    extract(child, source, name, out);
                }
                "identifier" if stmt_position => {
                    let name = node_text(&child, source);
                    if is_callable(name) && !bound.contains(name) {
                        out.push(ParsedRelation {
                            source_name: scope_name.to_string(),
                            target_name: name.to_string(),
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
                _ => emit(child, source, scope_name, bound, out),
            }
        }
    }

    /// A bare name is a candidate method call when it looks like a Ruby method
    /// name: lowercase/underscore first char (Constants/Classes are uppercase and
    /// never bare method calls), and not a literal keyword that surfaces as an
    /// identifier in some grammar positions.
    fn is_callable(name: &str) -> bool {
        let Some(first) = name.chars().next() else {
            return false;
        };
        if !(first == '_' || first.is_ascii_lowercase()) {
            return false;
        }
        !matches!(
            name,
            "self" | "nil" | "true" | "false" | "super" | "__method__"
        )
    }
}

/// Signature every additive `references` extractor shares.
type ReferenceExtractor = fn(&tree_sitter::Node, &str, Option<&str>) -> Option<ParsedRelation>;

/// Which language name a pass matches on: `Raw` is the file's own language as
/// `detect_language` returned it, `Family` the dispatch name on
/// [`LanguageConfig`].
///
/// **The two select identically today, for every supported language.** An
/// earlier version of this comment claimed `Family` "folds `.tsx` into its
/// family" and that swapping the keys silently changes coverage; neither is
/// true. `LanguageConfig::for_language` is an identity map over
/// `SUPPORTED_LANGUAGES` — `"tsx"` maps to name `"tsx"`, not `"typescript"` —
/// and it differs from the raw name only for a language the parser does not
/// support at all, which by definition appears in no row's `langs` list.
/// `lang_config::tests::every_supported_language_has_consistent_config` already
/// asserts that round-trip, so the statement above is proven, not merely made.
///
/// The enum stays because it records which name each pass was written against,
/// and because that guard turns a future real divergence into a failing test
/// instead of a silent coverage change: the day `for_language` starts folding
/// names, every row here must be re-read to decide which key it wants.
/// Until then, do not reason about a row's `langs` list as if the family
/// widened it — a pass that should cover `.tsx` has to list `"tsx"`.
enum LangKey {
    Raw,
    Family,
}

/// One additive `references` pass: when the node kind matches and the file's
/// language is listed, run the extractors.
struct ReferencePass {
    key: LangKey,
    langs: &'static [&'static str],
    kind: &'static str,
    /// Usually one. More than one means they are alternatives for the same
    /// (language, kind) slot — see `first_match_wins`.
    extract: &'static [ReferenceExtractor],
    /// True → stop at the first extractor that returns `Some`. False → run all
    /// of them and push every `Some`. Only meaningful with 2+ extractors, and
    /// the difference is load-bearing: Rust's two are an either/or chain, while
    /// Python's two are independent passes that happen to share a node kind.
    first_match_wins: bool,
}

/// The additive `references` axis, one row per (language, node kind).
///
/// This used to be a run of hand-written `if config.name == "…" && kind == "…"`
/// blocks. That shape is the top recurring bug class in this crate — one arm
/// per language per relation, where a missing arm is not a compile error but a
/// silently absent edge. As a table it is enumerable, so `reference_passes_
/// cover_every_extractor` (tests/) can assert that every `extract_*_reference`
/// defined in this module is actually wired to something.
const REFERENCE_PASSES: &[ReferencePass] = &[
    // Rust path-qualified value usage (`crate::domain::FOO`) — skips call
    // callees, `use` paths, type-position paths, intermediate path segments.
    ReferencePass {
        key: LangKey::Raw,
        langs: &["rust"],
        kind: "scoped_identifier",
        extract: &[extract_rust_path_reference],
        first_match_wins: false,
    },
    // Rust type-position usage (`field: MyType`, `-> MyType`, `Vec<MyType>`) —
    // skips the type's own definition name, the `struct_expression` name
    // (already a `calls` edge) and `Self`.
    ReferencePass {
        key: LangKey::Raw,
        langs: &["rust"],
        kind: "type_identifier",
        extract: &[extract_rust_type_reference],
        first_match_wins: false,
    },
    // Rust bare `identifier` used as a function value (callback / fn pointer)
    // in call-argument or address-of position. Self-excludes call callees
    // (parent `call_expression`, not `arguments`) and enclosing-fn params (M2);
    // path-qualified values are `scoped_identifier` (above). Inside a macro
    // token_tree the value-reference pass never fires (parent is `token_tree`),
    // so the macro-token-call alternative below it is disjoint, not a fallback.
    ReferencePass {
        key: LangKey::Raw,
        langs: &["rust"],
        kind: "identifier",
        extract: &[extract_rust_value_reference, extract_rust_macro_token_call],
        first_match_wins: true,
    },
    // TS/TSX type-position `type_identifier` (annotation, return type, generic
    // arg, field type). Self-excludes the type's own definition name and
    // heritage (extends/implements) types already covered by an
    // inherits/implements edge. JavaScript is out of scope for this pass and
    // stays out because it is absent from `langs` — NOT, as this comment once
    // said, because the raw key keeps a `javascript` family from sweeping in.
    // Both keys select the same set (see [`LangKey`]), and `.tsx` is covered
    // here only because `"tsx"` is listed explicitly.
    ReferencePass {
        key: LangKey::Raw,
        langs: &["typescript", "tsx"],
        kind: "type_identifier",
        extract: &[extract_ts_type_reference],
        first_match_wins: false,
    },
    // JS/TS/TSX bare `identifier` passed as a function value (callback) in
    // call-argument position. Self-excludes call callees (parent is
    // `call_expression`/`member_expression`, not `arguments`) and
    // enclosing-function params (M2). Family-keyed, same as the call arm.
    ReferencePass {
        key: LangKey::Family,
        langs: &["javascript", "typescript", "tsx"],
        kind: "identifier",
        extract: &[extract_js_value_reference],
        first_match_wins: false,
    },
    // Python type-annotation usage. UNLIKE Rust/TS, Python annotation type
    // names are plain `identifier` nodes (same kind as value identifiers), so
    // this fires on `identifier` and the extractor gates on ANNOTATION CONTEXT
    // (an enclosing `type` node) — gating on kind alone would emit a reference
    // for every variable/function name. Self-excludes builtins/typing generics
    // and base classes (those live under `argument_list`, not a `type` node,
    // and are already an inherits edge).
    ReferencePass {
        key: LangKey::Family,
        langs: &["python"],
        kind: "identifier",
        extract: &[extract_python_type_reference],
        first_match_wins: false,
    },
    // Python value reference (callback / fn pointer by bare name) in call-arg /
    // keyword / assignment-RHS / return / dict-value position. A separate row
    // from the annotation pass above, not an alternative: the two are mutually
    // exclusive by context (annotation vs value position), so both are always
    // attempted.
    ReferencePass {
        key: LangKey::Family,
        langs: &["python"],
        kind: "identifier",
        extract: &[extract_python_value_reference],
        first_match_wins: false,
    },
    // Go type-position `type_identifier` (field type, param/return type, var
    // type, slice/map element, composite literal, generic constraint,
    // qualified-type tail). Self-excludes the type's own definition name
    // (`type_spec[field=name]`) and Go predeclared builtins
    // (GO_TYPE_REFERENCE_NOISE — UNLIKE TS, Go builtins are `type_identifier`).
    // Value selectors (`pkg.Func()` / `obj.field`) are `field_identifier` /
    // `identifier`, and the qualified-type head (`pkg` in `pkg.Type`) is a
    // `package_identifier`, so neither reaches here.
    ReferencePass {
        key: LangKey::Family,
        langs: &["go"],
        kind: "type_identifier",
        extract: &[extract_go_type_reference],
        first_match_wins: false,
    },
    // Go bare `identifier` (Go value names are `identifier`, types are
    // `type_identifier`) passed/stored/returned as a fn value — call-arg /
    // `:=`-or-`=` RHS / return / `var` value. Self-excludes call callees and
    // enclosing-fn params/locals (M2/M2.5).
    ReferencePass {
        key: LangKey::Family,
        langs: &["go"],
        kind: "identifier",
        extract: &[extract_go_value_reference],
        first_match_wins: false,
    },
    // C/C++ bare `identifier` passed/stored/returned as a function value
    // (function pointer) — C's primary callback mechanism. Call-arg / `&fn` /
    // designated-or-positional initializer (vtable) / init-declarator RHS /
    // assignment RHS / return. Self-excludes call callees and enclosing-fn
    // params/locals (M2/M2.5). Member accesses are `field_identifier`.
    ReferencePass {
        key: LangKey::Family,
        langs: &["c", "cpp"],
        kind: "identifier",
        extract: &[extract_cpp_value_reference],
        first_match_wins: false,
    },
    // Java type-position `type_identifier` (field/param/return/local type,
    // generic arg, array element, `new` type, qualified-type tail).
    // Self-excludes heritage types (`extends`/`implements` — already an
    // inherits/implements edge), qualified-type package-path segments (only the
    // chain tail emits), and JDK common reference types
    // (JAVA_TYPE_REFERENCE_NOISE). The type's OWN definition name needs no
    // skip: Java class/interface/enum/record names are plain `identifier`s.
    // Primitives are distinct kinds (`integral_type` / `floating_point_type` /
    // `boolean_type` / `void_type`) and never reach here.
    ReferencePass {
        key: LangKey::Family,
        langs: &["java"],
        kind: "type_identifier",
        extract: &[extract_java_type_reference],
        first_match_wins: false,
    },
];

#[allow(clippy::too_many_arguments)]
fn walk_for_relations(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    config: &LanguageConfig,
    current_scope: Option<&str>,
    current_class: Option<&str>,
    current_rust_impl: Option<&str>,
    results: &mut Vec<ParsedRelation>,
    depth: usize,
) {
    if depth > MAX_RELATION_DEPTH {
        return;
    }
    let kind = node.kind();

    // Determine if this node creates a new scope
    let scope_name = match kind {
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "method_definition"
        | "method_declaration"
        | "constructor_declaration"
        | "async_function_definition"
        | "method"
        | "singleton_method" => {
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string())
                .or_else(|| {
                    // C/C++ function/method names live in the declarator subtree,
                    // not a `name` field (so this arm used to return None and the
                    // call's source attributed to `<module>` / got dropped). Pull
                    // the declarator name, e.g. `void Foo::bar(){}` → "Foo::bar".
                    if config.name == "c" || config.name == "cpp" {
                        node.child_by_field_name("declarator")
                            .and_then(|d| cpp_declarator_name(&d, source, 0))
                    } else {
                        None
                    }
                })
                .map(|name| {
                    // An out-of-class `Foo::bar` carries its own class; otherwise
                    // inherit the enclosing class context (current_class).
                    let (own_cls, method) = match name.rsplit_once("::") {
                        Some((c, m)) => (
                            Some(c.rsplit("::").next().unwrap_or(c).to_string()),
                            m.to_string(),
                        ),
                        None => (None, name),
                    };
                    match own_cls.as_deref().or(current_class) {
                        Some(cls) => format!("{}.{}", cls, method),
                        None => method,
                    }
                })
        }
        "arrow_function" => {
            // Inline HTTP route handler → its synthetic node name "METHOD path",
            // so calls inside the handler attribute to the materialized handler
            // node (treesitter.rs) instead of the file <module>. Otherwise:
            // `const foo = () => {}` → scope name is the binding; other anonymous
            // arrows (`test(() => {...})` callbacks, `.map(x => x)` lambdas)
            // inherit the parent scope. (Returning `Some("<anonymous>")` would
            // emit unresolvable edges — no node is named that — silently dropping
            // callback calls and causing false-positive orphans.)
            super::route_handler_name(&node, source).or_else(|| {
                node.parent()
                    .filter(|p| p.kind() == "variable_declarator")
                    .and_then(|p| p.child_by_field_name("name"))
                    .map(|n| {
                        let name = node_text(&n, source).to_string();
                        match current_class {
                            Some(cls) => format!("{}.{}", cls, name),
                            None => name,
                        }
                    })
            })
        }
        "function_expression" => {
            // Only materialized inline route handlers get a scope here; other
            // function expressions keep inheriting the parent scope (no node is
            // created for them, so a synthetic scope would dangle).
            super::route_handler_name(&node, source)
        }
        // Dart: function_body is a sibling of either method_signature
        // (in class_body) or function_signature (top-level declaration).
        // Look at previous sibling to find the function/method name.
        "function_body" if config.function_body_has_methods => {
            node.prev_sibling()
                .and_then(|s| match s.kind() {
                    // Top-level Dart function: declaration > function_signature + function_body
                    "function_signature" => s
                        .child_by_field_name("name")
                        .map(|n| node_text(&n, source).to_string()),
                    // Class method: method_signature wraps function_signature
                    "method_signature" => (0..s.named_child_count())
                        .filter_map(|i| s.named_child(i))
                        .find(|c| {
                            matches!(
                                c.kind(),
                                "function_signature"
                                    | "constructor_signature"
                                    | "getter_signature"
                                    | "setter_signature"
                            )
                        })
                        .and_then(|sig| sig.child_by_field_name("name"))
                        .map(|n| node_text(&n, source).to_string()),
                    _ => None,
                })
                .map(|name| match current_class {
                    Some(cls) => format!("{}.{}", cls, name),
                    None => name,
                })
        }
        _ => None,
    };

    let active_scope = scope_name.as_deref().or(current_scope);

    // Additive `references` passes, table-driven (see `REFERENCE_PASSES`).
    // They run BEFORE the `match kind` call-dispatch below so they cannot
    // disturb its arms or the child recursion; every extractor self-excludes
    // the nodes already covered by a `calls`/`imports`/`inherits` edge.
    for pass in REFERENCE_PASSES {
        if pass.kind != kind {
            continue;
        }
        let lang = match pass.key {
            LangKey::Raw => language,
            LangKey::Family => config.name,
        };
        if !pass.langs.contains(&lang) {
            continue;
        }
        for extract in pass.extract {
            if let Some(r) = extract(&node, source, active_scope) {
                results.push(r);
                if pass.first_match_wins {
                    break;
                }
            }
        }
    }

    // The `calls` axis lives in a TABLE (`calls::CALL_PASSES`), not in arms of
    // the match below. Tree-sitter grammars disagree on what a call node is
    // called — `call_expression` / `call` / `method_invocation` /
    // `invocation_expression` / three PHP kinds / a Dart `selector` / a Bash
    // `command` — so the (language, node kind) → extractor mapping is data, and
    // as data it is enumerable instead of being a run of `if`s where a missing
    // language is an absent edge nobody notices. The table's kinds are disjoint
    // from every arm left in the match, so running it here preserves the
    // one-handler-per-node semantics it was carved out of.
    calls::run_call_passes(
        &calls::CallCtx {
            node,
            source,
            language,
            config,
            active_scope,
            current_rust_impl,
        },
        results,
    );

    // The `imports` axis is a TABLE too (`imports::IMPORT_PASSES`). It is the
    // axis where grammars disagree most about spelling — `import_declaration`
    // alone means two different shapes in Swift and Java — so the (language,
    // node kind) → extractor mapping is data, and a language missing from it is
    // a visible empty slot rather than an absent arm.
    //
    // It returns nothing, deliberately. An earlier version handed back "did a
    // row fire" so this caller could skip the match below, and the caller threw
    // it away — a documented contract no code honoured. The kinds the table owns
    // are disjoint from every arm left below, so short-circuiting would only
    // hide a future overlap instead of letting it double-emit visibly. The child
    // recursion below runs either way — the walk's scope bookkeeping is shared.
    imports::run_import_passes(
        &imports::ImportCtx {
            node,
            source,
            language,
            config,
            active_scope,
        },
        results,
    );

    // The `heritage` axis (`inherits` + `implements`) is a TABLE too
    // (`inherits::HERITAGE_PASSES`). Its row shape carries one field the other
    // tables do not — `not_under`, the parent-kind veto that keeps a C#
    // `enum Level : byte` from emitting `Level inherits byte` — because that
    // arm was the only genuinely non-uniform one. The other stated blocker,
    // "heritage dispatches on a PREDICATE", was not one: `is_heritage_decl` was
    // `HERITAGE_DECL_KINDS.contains`, so the row simply points at that const.
    inherits::run_heritage_passes(
        &inherits::HeritageCtx {
            node,
            source,
            language,
            config,
            active_scope,
        },
        results,
    );

    // The `exports` axis is a TABLE (`exports::EXPORT_PASSES`) — two rows, ESM
    // and CommonJS.
    exports::run_export_passes(
        &exports::ExportCtx {
            node,
            source,
            language,
            config,
        },
        results,
    );

    // The last axis still living in the walk is `routes`, and only its Python
    // arm: Flask/FastAPI spell a route registration as a DECORATOR, so it hangs
    // off `decorated_definition`, while Express and axum spell it as a call and
    // are reached through `CALL_PASSES`. One arm for one language is not a table
    // yet — it is a row waiting for a second language to justify the shape.
    if kind == "decorated_definition" {
        if let Some(route_rel) = extract_python_route(&node, source) {
            results.push(route_rel);
        }
    }

    // Determine class context for children: when entering a class body,
    // pass the class name so methods can build qualified scope names.
    let child_class = match kind {
        "class_declaration" | "class_definition" | "class" | "class_specifier"
        | "struct_specifier" => node
            .child_by_field_name("name")
            .map(|n| node_text(&n, source).to_string()),
        _ => None,
    };
    let effective_class = child_class.as_deref().or(current_class);

    // Compute child_rust_impl: when entering a Rust impl_item, capture the
    // type name so nested call_expression arms can fill SelfRecv/SelfType
    // payloads. NOT folded into current_class because that would change
    // scope_name building and break source_id matching downstream
    // (relations source_name="conn" matches pf.node_names "conn"; would
    // become "Database.conn" if folded into current_class).
    let child_rust_impl: Option<String> = if language == "rust" && kind == "impl_item" {
        node.child_by_field_name("type").map(|t| {
            let full = node_text(&t, source);
            // Strip path prefix: `impl crate::db_a::Db` → "Db". Mirrors
            // treesitter.rs's parent_class strip so SelfRecv payloads
            // match qualified_name (which uses just the rightmost type
            // segment).
            full.rsplit("::").next().unwrap_or(full).to_string()
        })
    } else {
        None
    };
    let effective_rust_impl = child_rust_impl.as_deref().or(current_rust_impl);

    // Recurse into children
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_for_relations(
                child,
                source,
                language,
                config,
                active_scope,
                effective_class,
                effective_rust_impl,
                results,
                depth + 1,
            );
        }
    }
}

/// Extract a C/C++ function/method name from a declarator subtree, returning the
/// declarator text of the innermost `function_declarator` — e.g.
/// `void Foo::bar()` → "Foo::bar", `void bar()` → "bar". The caller splits the
/// `Foo::` scope. Recursion stays in the declarator subtree (the function body
/// is a separate `body` field), so it is shallow; the depth cap is a backstop.
fn cpp_declarator_name(node: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    if node.kind() == "function_declarator" {
        return node
            .child_by_field_name("declarator")
            .map(|d| node_text(&d, source).to_string());
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if let Some(name) = cpp_declarator_name(&child, source, depth + 1) {
                return Some(name);
            }
        }
    }
    None
}
