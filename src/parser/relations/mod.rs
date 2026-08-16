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
//! `walk_for_relations` is the single recursive dispatcher that maps tree-sitter
//! node kinds to the appropriate extractor. It must keep all language arms in
//! one match because they share `current_scope` / `current_class` propagation
//! (splitting it per-language would either duplicate the recursion or lose
//! scope context across language-specific arms).

use super::lang_config::LanguageConfig;
use super::node_text;
use crate::domain::{MAX_RELATION_DEPTH, REL_CALLS, REL_IMPLEMENTS, REL_IMPORTS, REL_INHERITS};
use anyhow::Result;

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

use cpp::{extract_cpp_inheritance, extract_cpp_value_reference};
use dart::{extract_dart_call_from_selector, extract_dart_imports};
use exports::{extract_cjs_exports, extract_export_names};
use go::{extract_go_inheritance, extract_go_type_reference, extract_go_value_reference};
use helpers::{extract_callee, extract_string_from_subtree, MAX_SUBTREE_DEPTH};
use imports::{
    extract_import_names, extract_python_from_import_names, extract_python_import_names,
};
use inherits::{extract_implements, extract_superclasses, is_heritage_decl};
use java::extract_java_type_reference;
use python::{
    extract_python_type_reference, extract_python_value_reference, infer_python_call_receiver_type,
};
use routes::{extract_python_route, extract_route_pattern};
use rust::{
    extract_rust_impl_trait, extract_rust_macro_token_call, extract_rust_path_reference,
    extract_rust_type_reference, extract_rust_use_imports, extract_rust_value_reference,
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

/// Which language name a pass matches on. The two are NOT interchangeable and
/// swapping one for the other silently changes coverage: `Raw` is the file's
/// own language, where `typescript` and `tsx` are DIFFERENT strings, while
/// `Family` is the dispatch name from [`LanguageConfig`], which folds `.tsx`
/// into its family. Each pass below keeps whichever one it was written against.
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
    // inherits/implements edge. Keyed on the RAW language: JS has no type
    // annotations so the kind never appears there anyway, but the `javascript`
    // FAMILY would otherwise sweep value identifiers in non-type contexts.
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

    // Call-expression dispatch — adding a new language with a non-standard
    // call node kind MUST add its own arm below. Tree-sitter grammars don't
    // agree on a single name: JS/TS/Rust/Go/Java/C/C++/Kotlin/Swift/Dart use
    // `call_expression` (the default arm), Python/Ruby use `call`, PHP splits
    // into three node kinds, C# uses `invocation_expression`, Bash uses
    // `command`. Missing arms = silently-dropped edges, not compile errors.
    match kind {
        // Call expressions
        "call_expression" => {
            // JS/TS CommonJS: require('./foo') / require('pkg') → IMPORTS edge.
            // Mirrors the Ruby `require` handling above; target is the last path
            // segment so node_modules imports become `<external>` sentinels and
            // relative imports can match a file module node by name.
            if matches!(config.name, "javascript" | "typescript" | "tsx")
                && node
                    .child_by_field_name("function")
                    .map(|f| node_text(&f, source) == "require")
                    .unwrap_or(false)
            {
                if let Some(args) = node.child_by_field_name("arguments") {
                    if let Some(first) = args.named_child(0) {
                        if let Some(path) = extract_string_from_subtree(&first, source) {
                            // Normalize `node:fs` → `fs`; strip trailing JS extensions.
                            let normalized = path.strip_prefix("node:").unwrap_or(&path);
                            let segment = normalized
                                .trim_end_matches(".js")
                                .trim_end_matches(".ts")
                                .trim_end_matches(".mjs")
                                .trim_end_matches(".cjs")
                                .rsplit('/')
                                .next()
                                .unwrap_or(normalized)
                                .to_string();
                            if !segment.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: segment,
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }

                            // Destructured require: `const { foo, bar } = require('./x')`.
                            // Emit a per-name import stamped with the full specifier so
                            // js_modules resolution binds each name to the required file's
                            // export (and Phase 2d-bind repoints calls made under the EXPORT
                            // name) — the CommonJS analog of ES named imports. The last-segment
                            // module import above is kept for module-level dep tracking.
                            if let Some(decl) =
                                node.parent().filter(|p| p.kind() == "variable_declarator")
                            {
                                if let Some(name_node) = decl.child_by_field_name("name") {
                                    if name_node.kind() == "object_pattern" {
                                        let metadata =
                                            serde_json::json!({ "js_module": &path }).to_string();
                                        for i in 0..name_node.named_child_count() {
                                            if let Some(binding) = name_node.named_child(i) {
                                                // Shorthand `{ foo }` → the binding name; renamed
                                                // `{ foo: f }` (pair_pattern) → the KEY (export name),
                                                // since that is what the required file exports.
                                                // The import edge is correct either way, but Phase
                                                // 2d-bind keys on the export name: a shorthand call
                                                // (`foo()`, local == export) is repointed; a renamed
                                                // local call (`f()`) is NOT (known long-tail limit —
                                                // see feedback_import_aware_call_resolution).
                                                let imported = match binding.kind() {
                                                    "shorthand_property_identifier_pattern" => {
                                                        Some(
                                                            node_text(&binding, source).to_string(),
                                                        )
                                                    }
                                                    "pair_pattern" => binding
                                                        .child_by_field_name("key")
                                                        .map(|k| node_text(&k, source).to_string()),
                                                    _ => None,
                                                };
                                                if let Some(name) = imported {
                                                    if !name.is_empty() {
                                                        results.push(ParsedRelation {
                                                            source_name: "<module>".into(),
                                                            target_name: name,
                                                            relation: REL_IMPORTS.into(),
                                                            metadata: Some(metadata.clone()),
                                                            source_language: String::new(),
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    } else if name_node.kind() == "identifier" {
                                        // Namespace require: `const helper = require('./x')`.
                                        // Emit a binding marker (var → specifier) so Phase 2
                                        // resolves `helper.foo()` member calls to the required
                                        // module file. Marker only — the variable itself is not
                                        // a symbol, so Phase 2 must consume it without a name edge.
                                        let var = node_text(&name_node, source).to_string();
                                        if !var.is_empty() {
                                            results.push(ParsedRelation {
                                                source_name: "<module>".into(),
                                                target_name: var,
                                                relation: REL_IMPORTS.into(),
                                                metadata: Some(serde_json::json!({ "q": crate::domain::IMPORT_Q_NS_REQUIRE, "js_module": &path }).to_string()),
                                                source_language: String::new(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Check for HTTP route registration patterns first. Vec: one axum
            // `.route(path, get(a).post(b))` call registers several routes.
            results.extend(extract_route_pattern(&node, source, language));

            // Call relation extraction. For JS/TS/TSX + Kotlin/Swift, fall back to
            // `<module>` when the call sits at file top level (imports, init code)
            // or inside an anonymous callback (test/describe/it, Array.map, etc.)
            // so same-file edges can still resolve — Kotlin and Swift both allow
            // executable statements at file top level (`main`-less scripts, global
            // `val x = compute()`). Rust/Go/C also route through this
            // `call_expression` arm but are deliberately excluded: their top-level
            // omission is intentional (no bare top-level call statements in Rust/Go;
            // C calls only appear inside functions), so leaving them at `None`
            // keeps their callgraphs clean.
            let call_scope: Option<String> = match active_scope {
                Some(s) => Some(s.to_string()),
                None if matches!(
                    config.name,
                    "javascript" | "typescript" | "tsx" | "kotlin" | "swift"
                ) =>
                {
                    Some("<module>".to_string())
                }
                None => None,
            };
            if let Some(scope) = call_scope {
                if let Some((callee, mut qualifier)) =
                    extract_callee(&node, source, language, current_rust_impl)
                {
                    // A BARE Rust callee whose name is a local binding of the
                    // enclosing fn is a closure/fn-pointer call, not a call of the
                    // same-named global fn — Rust's value namespace makes the
                    // local win.
                    //
                    // "Bare" must be decided STRUCTURALLY (the `function` field is
                    // an `identifier`), NOT from `CalleeQualifier::Bare`: that
                    // variant is also `extract_rust_field`'s fallback arm for a
                    // method call whose receiver is not self / a plain identifier /
                    // a call. `ctx.db.conn()` has a `field_expression` receiver and
                    // so reports Bare — gating on the enum dropped 14 real
                    // `Database::conn` edges in this repo alone, every `cmd_*` that
                    // writes `let conn = ctx.db.conn();`, because the method name
                    // matched the local it is assigned to.
                    let bare_call = node
                        .child_by_field_name("function")
                        .is_some_and(|f| f.kind() == "identifier");
                    let shadowed = language == "rust"
                        && bare_call
                        && rust::shadowed_by_enclosing_local(&node, source, &callee);
                    if !shadowed {
                        // Fill SelfRecv/SelfType payload from current impl context.
                        // The helper emits these with empty payload because it
                        // doesn't know the enclosing impl's type; we know it here.
                        let needs_payload = matches!(&qualifier,
                            helpers::CalleeQualifier::SelfRecv(t) | helpers::CalleeQualifier::SelfType(t) if t.is_empty()
                        );
                        if needs_payload {
                            match &mut qualifier {
                                helpers::CalleeQualifier::SelfRecv(t)
                                | helpers::CalleeQualifier::SelfType(t) => {
                                    if let Some(impl_type) = current_rust_impl {
                                        *t = impl_type.to_string();
                                    } else {
                                        // self/Self called outside an impl block — drop qualifier (Bare).
                                        qualifier = helpers::CalleeQualifier::Bare;
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }
                        let metadata = serialize_callee_qualifier(&qualifier);
                        results.push(ParsedRelation {
                            source_name: scope,
                            target_name: callee,
                            relation: REL_CALLS.into(),
                            metadata,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Rust/Go: struct instantiation → calls edge (enables cross-file dead code tracking)
        // e.g., `MyStruct { field: value }` or `MyStruct::new()` (calls already handled above)
        "struct_expression" => {
            if let Some(scope) = active_scope {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let struct_name = node_text(&name_node, source);
                    // Strip path prefix: path::MyStruct → MyStruct
                    let short_name = struct_name.rsplit("::").next().unwrap_or(struct_name);
                    if !short_name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: short_name.to_string(),
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // JS/TS/TSX: `new Foo()` / `new ns.Foo()` constructor instantiation →
        // calls edge to the class (constructor) name. A `new_expression`'s callee
        // is its `constructor` field, which never reaches the `call_expression`
        // arm above, so a class that is only instantiated (never called as a
        // `Foo.method()`) had NO incoming calls edge — invisible to
        // callgraph/impact. The JS value-reference pass emits a `references` edge
        // only in value positions (call arg, binding RHS, return, ...), NOT for a
        // `new` constructor slot (parent is `new_expression`), so such a class was
        // also edgeless and false-flagged dead-code. Mirrors the Rust
        // `struct_expression` arm. Generic args (`new Foo<T>()`) are a separate
        // `type_arguments` field, but a `<...>` tail is stripped defensively.
        // Member form `new ns.Foo()` yields callee `Foo` with a Receiver("ns")
        // qualifier when the receiver is a simple identifier — consistent with how
        // member `call_expression`s are handled. Scope mirrors the call arm
        // (`<module>` fallback at file top level).
        "new_expression" if matches!(config.name, "javascript" | "typescript" | "tsx") => {
            let scope = active_scope
                .map(|s| s.to_string())
                .unwrap_or_else(|| "<module>".to_string());
            if let Some(ctor) = node.child_by_field_name("constructor") {
                let callee: Option<(String, helpers::CalleeQualifier)> = match ctor.kind() {
                    "identifier" => {
                        let raw = node_text(&ctor, source);
                        let name = raw.split('<').next().unwrap_or(raw).trim();
                        (!name.is_empty())
                            .then(|| (name.to_string(), helpers::CalleeQualifier::Bare))
                    }
                    "member_expression" => ctor.child_by_field_name("property").and_then(|prop| {
                        let raw = node_text(&prop, source);
                        let name = raw.split('<').next().unwrap_or(raw).trim();
                        if name.is_empty() {
                            return None;
                        }
                        let qual = ctor
                            .child_by_field_name("object")
                            .filter(|o| o.kind() == "identifier")
                            .map(|o| {
                                helpers::CalleeQualifier::Receiver(
                                    node_text(&o, source).to_string(),
                                )
                            })
                            .unwrap_or(helpers::CalleeQualifier::Bare);
                        Some((name.to_string(), qual))
                    }),
                    _ => None,
                };
                if let Some((name, qualifier)) = callee {
                    results.push(ParsedRelation {
                        source_name: scope,
                        target_name: name,
                        relation: REL_CALLS.into(),
                        metadata: serialize_callee_qualifier(&qualifier),
                        source_language: String::new(),
                    });
                }
            }
        }

        // C#: `new Foo()` / `new Ns.Bar()` object creation → calls edge to the
        // class (constructor) name. `object_creation_expression` is distinct from
        // `invocation_expression` (handled below), and C# has no type-reference
        // pass, so a class that is only instantiated had NO incoming edge and was
        // false-flagged dead-code. Mirrors the JS `new_expression` arm; top-level
        // `new` (C# 9 top-level program) attributes to `<module>` like the C#
        // invocation arm. The class name is the `type` field: an `identifier`
        // (`new Foo()`) or a `qualified_name` (`new Ns.Bar()` → its `name` field).
        "object_creation_expression" if config.name == "csharp" => {
            let scope = active_scope.unwrap_or("<module>");
            if let Some(ty) = node.child_by_field_name("type") {
                let raw = match ty.kind() {
                    "qualified_name" => ty
                        .child_by_field_name("name")
                        .map(|n| node_text(&n, source)),
                    _ => Some(node_text(&ty, source)),
                };
                if let Some(raw) = raw {
                    let name = raw.split('<').next().unwrap_or(raw).trim();
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: name.to_string(),
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // PHP: `new Foo()` / `new Ns\Bar()` object creation → calls edge to the
        // class (constructor) name. PHP has no type-reference pass, so a class only
        // instantiated had NO incoming edge and was false-flagged dead-code.
        // Mirrors the JS/C# arms. The type is a positional `name` or
        // `qualified_name` child (no field); for a qualified name the class is the
        // last `name` segment (namespace prefix lives under `namespace_name`).
        // `new self()/static()/parent()` and `new $var()` are relative/dynamic —
        // skipped (a `self`/`static`/`parent` edge is pure noise; a variable is not
        // a class name).
        "object_creation_expression" if config.name == "php" => {
            let scope = active_scope.unwrap_or("<module>");
            let type_node = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| matches!(c.kind(), "name" | "qualified_name"));
            if let Some(ty) = type_node {
                let name = if ty.kind() == "qualified_name" {
                    (0..ty.named_child_count())
                        .filter_map(|i| ty.named_child(i))
                        .rfind(|c| c.kind() == "name")
                        .map(|n| node_text(&n, source).to_string())
                } else {
                    Some(node_text(&ty, source).to_string())
                };
                if let Some(name) = name {
                    if !name.is_empty() && !matches!(name.as_str(), "self" | "static" | "parent") {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: name,
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Python: tree-sitter-python uses `call` (not `call_expression`) for every
        // function/method invocation. Without this branch all Python call edges
        // are silently dropped — README documents Python as Full tier, and
        // module_overview / impact_analysis / find_dead_code all rely on this
        // edge. Routes and `from X import Y` already extracted via other arms.
        "call" if config.name == "python" => {
            // Top-level (module / class-body) calls attribute to `<module>`
            // rather than being dropped — a function invoked only at import time
            // (`app.run()`, a bare `main_entry()`) would otherwise have no
            // incoming edge and be reported dead. Undefined callees (print,
            // os.path.join, …) drop at Phase-2 same-language resolution, so this
            // only adds edges to defined same-project functions. Mirrors the
            // bash/js `<module>` fallback.
            let scope = active_scope.unwrap_or("<module>");
            if let Some(callee) = helpers::extract_callee_name(&node, source) {
                // Receiver-type propagation (issue #32 cause 2): when the call is
                // `recv.method()` and `recv`'s type is fixed by a single local
                // `recv = ClassName(...)` constructor assignment, stamp
                // `{"q":"rtype","v":"ClassName"}` so Phase-2 resolution binds it to
                // `ClassName.method` (self_filter_candidates) instead of dropping
                // the ambiguous by-name fan-out across every same-named method.
                // Falls back to the bare, metadata-less form (unchanged behavior)
                // whenever the type can't be proven — never emits a wrong-type edge.
                let metadata = infer_python_call_receiver_type(&node, source)
                    .map(|ty| serialize_rtype_metadata(&ty));
                results.push(ParsedRelation {
                    source_name: scope.to_string(),
                    target_name: callee,
                    relation: REL_CALLS.into(),
                    metadata,
                    source_language: String::new(),
                });
            }
        }

        // Ruby: `call` node kind for method calls (require, require_relative, and regular calls)
        "call" if config.name == "ruby" => {
            // Extract method name from the "method" field
            if let Some(method_node) = node.child_by_field_name("method") {
                let method_name = node_text(&method_node, source);
                // require 'json' / require_relative 'helper'
                if method_name == "require" || method_name == "require_relative" {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        if let Some(first_arg) = args.named_child(0) {
                            if let Some(string_val) =
                                extract_string_from_subtree(&first_arg, source)
                            {
                                results.push(ParsedRelation {
                                    source_name: active_scope.unwrap_or("<module>").to_string(),
                                    target_name: string_val,
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                } else {
                    // Regular method call. Top-level calls attribute to
                    // `<module>` (same rationale as the python/bash arms) so a
                    // top-level entry call isn't dropped; undefined callees drop
                    // at Phase-2 same-language resolution.
                    let scope = active_scope.unwrap_or("<module>");
                    results.push(ParsedRelation {
                        source_name: scope.to_string(),
                        target_name: method_name.to_string(),
                        relation: REL_CALLS.into(),
                        metadata: None,
                        source_language: String::new(),
                    });
                }
            }
        }

        // Java: tree-sitter-java uses `method_invocation` (NOT `call_expression`)
        // for every method call — `foo()`, `this.foo()`, `obj.foo()`, `Type.foo()`.
        // The callee name is the `name` field; the optional `object` field is the
        // receiver. Without this arm ALL Java call edges were silently dropped
        // despite Java being documented Full tier — callgraph / impact_analysis /
        // find_dead_code all depend on these edges. Bare callee name mirrors the
        // Python/Ruby arms; downstream target resolution disambiguates same names.
        "method_invocation" if config.name == "java" => {
            if let Some(scope) = active_scope {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let callee = node_text(&name_node, source).to_string();
                    if !callee.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: callee,
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // PHP: function_call_expression (doSomething()), member_call_expression ($this->move()),
        // scoped_call_expression (User::all())
        "function_call_expression" | "member_call_expression" | "scoped_call_expression"
            if config.name == "php" =>
        {
            // Top-level calls (outside any function/method) attribute to `<module>`
            // rather than being dropped, mirroring the python/ruby/bash arms — a
            // function invoked only at the top level (`greetPhp();`) would otherwise
            // have no incoming edge and be false-reported as dead. Undefined callees
            // drop at Phase-2 same-language resolution.
            {
                let scope = active_scope.unwrap_or("<module>");
                // All three PHP call types have a `name` child for the method/function name
                // For scoped_call_expression, there are multiple `name` children; the second is the method
                let callee = if kind == "scoped_call_expression" {
                    // User::all() -> children: name("User"), "::", name("all"), arguments
                    // The method name is the second `name` child
                    let mut names = Vec::new();
                    for i in 0..node.child_count() {
                        if let Some(child) = node.child(i) {
                            if child.kind() == "name" {
                                names.push(node_text(&child, source).to_string());
                            }
                        }
                    }
                    names.pop() // Last name is the method
                } else {
                    // function_call_expression: name("doSomething"), arguments
                    // member_call_expression: variable_name("$this"), "->", name("move"), arguments
                    node.child_by_field_name("name")
                        .or_else(|| {
                            // Fallback: find the `name` node among children
                            (0..node.child_count())
                                .filter_map(|i| node.child(i))
                                .find(|c| c.kind() == "name")
                        })
                        .map(|n| node_text(&n, source).to_string())
                };
                if let Some(name) = callee {
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: name,
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // PHP file includes: require / require_once / include / include_once
        // 'path/File.php' → IMPORTS to the bare file stem. Mirrors the C/C++
        // `#include` and JS `require` shape: strip the directory and the `.php`
        // extension so Phase 2 can resolve the import to a concrete file node.
        // Without this arm, PHP files got symbols + calls + `use` imports but no
        // file-include edges, so deps/cycles/affected/project_map under-reported
        // PHP cross-file dependencies (the AST node is a dedicated
        // `*_expression`, never a function_call_expression, so no double-count).
        "require_expression"
        | "require_once_expression"
        | "include_expression"
        | "include_once_expression"
            if config.name == "php" =>
        {
            if let Some(raw) = extract_string_from_subtree(&node, source) {
                let stem = {
                    let bare = raw.rsplit(['/', '\\']).next().unwrap_or(raw.as_str());
                    bare.strip_suffix(".php").unwrap_or(bare).to_string()
                };
                if !stem.is_empty() {
                    // Stamp the raw include path so Phase 2 can resolve it to the
                    // concrete indexed file (require_once 'lib.php' → lib.php's
                    // <module> node), mirroring the JS `js_module` specifier.
                    // target_name stays the bare stem for the name-based fallback
                    // when the path doesn't resolve to an indexed file.
                    let metadata = Some(serde_json::json!({ "php_include": &raw }).to_string());
                    results.push(ParsedRelation {
                        source_name: "<module>".into(),
                        target_name: stem,
                        relation: REL_IMPORTS.into(),
                        metadata,
                        source_language: String::new(),
                    });
                }
            }
        }

        // PHP: use App\Models\User;
        // namespace_use_declaration -> namespace_use_clause -> qualified_name -> name (last segment)
        "namespace_use_declaration" if config.name == "php" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "namespace_use_clause" {
                        // Get the last `name` segment from the qualified_name
                        fn find_last_name(n: &tree_sitter::Node, source: &str) -> Option<String> {
                            find_last_name_inner(n, source, 0)
                        }
                        fn find_last_name_inner(
                            n: &tree_sitter::Node,
                            source: &str,
                            depth: usize,
                        ) -> Option<String> {
                            if depth > MAX_SUBTREE_DEPTH {
                                return None;
                            }
                            let mut result = None;
                            for i in 0..n.child_count() {
                                if let Some(child) = n.child(i) {
                                    if child.kind() == "name" {
                                        result = Some(node_text(&child, source).to_string());
                                    } else if child.kind() == "qualified_name"
                                        || child.kind() == "namespace_name"
                                    {
                                        if let Some(inner) =
                                            find_last_name_inner(&child, source, depth + 1)
                                        {
                                            result = Some(inner);
                                        }
                                    }
                                }
                            }
                            result
                        }
                        if let Some(name) = find_last_name(&child, source) {
                            if !name.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: name,
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Swift: import Foundation, import UIKit
        // AST: import_declaration -> identifier -> simple_identifier
        "import_declaration" if config.name == "swift" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "identifier" {
                        // identifier may contain simple_identifier children (dotted: Foundation.NSObject)
                        // Use the full text as the import target
                        let name = node_text(&child, source).to_string();
                        if !name.is_empty() {
                            results.push(ParsedRelation {
                                source_name: "<module>".into(),
                                target_name: name,
                                relation: REL_IMPORTS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
        }

        // Java: import p.B; import java.util.List; import static x.Y.z;
        // AST: import_declaration -> scoped_identifier(scope, name) | identifier,
        //      optional trailing `.asterisk` for on-demand imports.
        // Target = the LAST segment (the imported type / static member), mirroring
        // Kotlin's qualified_identifier handling. A wildcard import (`java.util.*`)
        // names no single symbol → skip (never emit the package segment or `*`).
        "import_declaration" if config.name == "java" => {
            let is_wildcard = (0..node.child_count())
                .any(|i| node.child(i).is_some_and(|c| c.kind() == "asterisk"));
            if !is_wildcard {
                let target = node.named_child(0).and_then(|first| match first.kind() {
                    "scoped_identifier" => first
                        .child_by_field_name("name")
                        .map(|n| node_text(&n, source).to_string()),
                    "identifier" => Some(node_text(&first, source).to_string()),
                    _ => None,
                });
                if let Some(name) = target {
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Dart: import 'dart:async'; import 'package:foo/bar.dart';
        "import_or_export" if config.name == "dart" => {
            extract_dart_imports(&node, source, results);
        }

        // Import statements
        "import_statement" => {
            if config.name == "python" {
                extract_python_import_names(&node, source, results);
            } else {
                extract_import_names(&node, source, results);
            }
        }

        // Kotlin: import kotlinx.coroutines.flow.Flow
        // AST: import -> qualified_identifier -> identifier*
        // Extract the last identifier segment as the import target
        "import" if config.name == "kotlin" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "qualified_identifier" {
                        let count = child.named_child_count();
                        if count > 0 {
                            if let Some(last) = child.named_child(count - 1) {
                                let name = node_text(&last, source).to_string();
                                if !name.is_empty() && name != "*" {
                                    results.push(ParsedRelation {
                                        source_name: "<module>".into(),
                                        target_name: name,
                                        relation: REL_IMPORTS.into(),
                                        metadata: None,
                                        source_language: String::new(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Python: from X import Y
        "import_from_statement" => {
            extract_python_from_import_names(&node, source, results);
        }

        // Heritage-carrying declarations. See [`HERITAGE_DECL_KINDS`] — this arm
        // used to list only the three class-shaped kinds, so a Java `interface`,
        // a Kotlin `object`, a Swift `protocol` and the rest emitted NOTHING.
        kind if is_heritage_decl(kind) => {
            let class_name = node
                .child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string());

            if let Some(ref cls) = class_name {
                // Check for extends/superclass (supports multiple inheritance)
                for parent in extract_superclasses(&node, source) {
                    results.push(ParsedRelation {
                        source_name: cls.clone(),
                        target_name: parent,
                        relation: REL_INHERITS.into(),
                        metadata: None,
                        source_language: String::new(),
                    });
                }

                // Check for implements (TS/JS/Java)
                extract_implements(&node, source, cls, results);
            }
        }

        // C++: base classes → inherits (C++ has no separate interface concept, so
        // every base — public/private/protected — is an inherits parent). C
        // structs and plain C++ aggregates have no base_class_clause → nothing
        // emitted, so no language gate is needed.
        "class_specifier" | "struct_specifier" => {
            for rel in extract_cpp_inheritance(&node, source) {
                results.push(rel);
            }
        }

        // Export statements (TS/JS)
        "export_statement" => {
            extract_export_names(&node, source, results);
        }

        // CommonJS exports (`module.exports = { f }`, `exports.f = g`). The ESM
        // arm above has no counterpart for them, so dead-code classified an
        // unused CJS export as an ORPHAN — "nothing references this" — while the
        // identical ESM code came back EXPORTED_UNUSED.
        "assignment_expression" if matches!(config.name, "javascript" | "typescript" | "tsx") => {
            extract_cjs_exports(&node, source, results);
        }

        // Rust: impl Trait for Type → implements edge (type-level + method-level)
        "impl_item" => {
            if let Some(impl_rel) = extract_rust_impl_trait(&node, source) {
                let type_name = impl_rel.source_name.clone();
                results.push(impl_rel);
                // For each method in the trait impl block, emit a method-level
                // implements edge: TypeName → method_name. This ensures dead code
                // detection sees incoming implements edges on trait methods.
                if let Some(body) = node.child_by_field_name("body") {
                    for i in 0..body.named_child_count() {
                        if let Some(child) = body.named_child(i) {
                            if child.kind() == "function_item" {
                                if let Some(name_node) = child.child_by_field_name("name") {
                                    let method_name = node_text(&name_node, source);
                                    if !method_name.is_empty() {
                                        // Stamp impl_type so Phase 2 can filter
                                        // method candidates by qualified_name. Without
                                        // it, a file with N structs each implementing
                                        // the same trait fans every impl's method
                                        // edge to all N same-named method nodes —
                                        // every struct appears to implement every
                                        // other struct's methods.
                                        results.push(ParsedRelation {
                                            source_name: type_name.clone(),
                                            target_name: method_name.to_string(),
                                            relation: REL_IMPLEMENTS.into(),
                                            metadata: Some(serialize_impl_method_metadata(
                                                &type_name,
                                            )),
                                            source_language: String::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Rust: use std::collections::HashMap;
        // Also handles grouped imports: use std::collections::{HashMap, HashSet};
        "use_declaration" => {
            extract_rust_use_imports(&node, source, active_scope, results);
        }

        // Go: import "fmt" or import alias "fmt"
        "import_spec" => {
            if let Some(path_node) = node.child_by_field_name("path") {
                let path_text = node_text(&path_node, source).trim_matches('"').to_string();
                if let Some(pkg_name) = path_text.rsplit('/').next() {
                    if !pkg_name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: active_scope.unwrap_or("<module>").to_string(),
                            target_name: pkg_name.to_string(),
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Go: struct/interface embedding → inherits (method promotion / interface
        // composition — Go's idiomatic "is-a"). Gated on Go because `type_spec` is
        // a Go-grammar node; the guard keeps any other grammar's same-named node
        // from being misread.
        "type_spec" if language == "go" => {
            for rel in extract_go_inheritance(&node, source) {
                results.push(rel);
            }
        }

        // Python decorated definitions (for Flask/FastAPI route decorators)
        "decorated_definition" => {
            if let Some(route_rel) = extract_python_route(&node, source) {
                results.push(route_rel);
            }
        }

        // C# using directives: using System; using System.Collections.Generic;
        "using_directive" if config.name == "csharp" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "qualified_name" || child.kind() == "identifier" {
                        let name = node_text(&child, source).to_string();
                        if !name.is_empty() && name != "using" {
                            results.push(ParsedRelation {
                                source_name: "<module>".into(),
                                target_name: name,
                                relation: REL_IMPORTS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
        }

        // C# inheritance: class Dog : Animal, IWalkable
        //
        // NOT on an enum. C# spells an enum's UNDERLYING INTEGRAL TYPE with the
        // same `base_list` syntax a class uses for its base type, so
        // `enum Level : byte` produced `Level inherits byte` — a phantom edge
        // bound to a real node, which this repo has already learned is worse
        // than a missing one (it makes `byte` look inherited-from and pollutes
        // every heritage traversal). Grammar-level distinction: the parent kind.
        "base_list"
            if config.name == "csharp"
                && node.parent().map(|p| p.kind()) != Some("enum_declaration") =>
        {
            // Get the class/struct name from the parent node
            let owner_name = node
                .parent()
                .and_then(|p| p.child_by_field_name("name"))
                .map(|n| node_text(&n, source).to_string());
            let owner = owner_name.as_deref().or(active_scope).unwrap_or("<module>");
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    let base_name = node_text(&child, source).to_string();
                    if !base_name.is_empty() {
                        let rel = if config.interface_by_prefix
                            && base_name.starts_with('I')
                            && base_name.len() > 1
                            && base_name
                                .chars()
                                .nth(1)
                                .map(|c| c.is_uppercase())
                                .unwrap_or(false)
                        {
                            REL_IMPLEMENTS
                        } else {
                            REL_INHERITS
                        };
                        results.push(ParsedRelation {
                            source_name: owner.to_string(),
                            target_name: base_name,
                            relation: rel.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // C# method/function calls: invocation_expression (Console.WriteLine(...), Baz(), etc.)
        "invocation_expression" if config.name == "csharp" => {
            // Top-level statement calls (C# 9+ top-level programs) and calls in
            // field initializers outside any method attribute to `<module>`
            // rather than being dropped, mirroring the php/python/ruby arms — a
            // function invoked only from a top-level statement would otherwise
            // have no incoming edge and be false-reported as dead. Undefined
            // callees drop at Phase-2 same-language resolution.
            let scope = active_scope.unwrap_or("<module>");
            if let Some(func) = node.named_child(0) {
                let callee = match func.kind() {
                    "identifier" => Some(node_text(&func, source).to_string()),
                    "member_access_expression" => {
                        // e.g. Console.WriteLine — extract "WriteLine"
                        func.child_by_field_name("name")
                            .map(|n| node_text(&n, source).to_string())
                    }
                    _ => None,
                };
                if let Some(name) = callee {
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: name,
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Dart: a `selector` carrying an argument_part marks a call in ANY
        // position (return / assignment / argument / binary expr / bare
        // statement) — not just `expression_statement`. tree-sitter-dart has no
        // single call_expression node, so the selector is the reliable marker;
        // the callee is its preceding sibling. active_scope propagates down from
        // the enclosing function_body, so deeply-nested calls still attribute.
        "selector" if config.name == "dart" => {
            // Library-level (top-level) calls attribute to `<module>` rather than
            // being dropped, mirroring the php/python/ruby/C# arms — a top-level
            // `main()`-free Dart script's calls would otherwise have no incoming
            // edge. Undefined callees drop at Phase-2 same-language resolution.
            let scope = active_scope.unwrap_or("<module>");
            extract_dart_call_from_selector(&node, source, scope, results);
        }

        // C/C++: `#include "foo.h"` → IMPORTS to "foo"
        //         `#include <stdio.h>` → IMPORTS to "stdio"
        // Header extension stripped so cross-file resolution can match the
        // bare module name (mirrors JS require pattern).
        "preproc_include" if matches!(config.name, "c" | "cpp") => {
            let path_node = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| matches!(c.kind(), "string_literal" | "system_lib_string"));
            if let Some(p) = path_node {
                let raw = node_text(&p, source);
                // string_literal text includes quotes; system_lib_string
                // includes angle brackets. Trim both forms uniformly.
                let unquoted = raw.trim_matches(|c| c == '"' || c == '<' || c == '>');
                if !unquoted.is_empty() {
                    let last = unquoted.rsplit('/').next().unwrap_or(unquoted);
                    let stem = last
                        .trim_end_matches(".hpp")
                        .trim_end_matches(".hxx")
                        .trim_end_matches(".hh")
                        .trim_end_matches(".h");
                    if !stem.is_empty() {
                        // Stamp the raw include path so Phase 2 can resolve it to
                        // the concrete indexed header's <module> node (mirrors the
                        // PHP `php_include` / JS `js_module` specifiers). target_name
                        // stays the bare stem for the name-based fallback when the
                        // path doesn't resolve to an indexed file (system headers).
                        let metadata =
                            Some(serde_json::json!({ "c_include": unquoted }).to_string());
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: stem.to_string(),
                            relation: REL_IMPORTS.into(),
                            metadata,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        // Bash command invocation:
        //   `source <file>` / `. <file>` → IMPORTS edge (mirrors JS require).
        //   Otherwise: CALLS edge to command_name.
        // External commands (cat, grep, ...) without a function_definition
        // in any indexed shell file get dropped at Phase 2 same-language
        // edge resolution (see feedback_edge_resolution_same_language).
        "command" if config.name == "bash" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let raw = node_text(&name_node, source).trim();

                if raw == "source" || raw == "." {
                    // First non-command_name word/string sibling = the file path arg.
                    let arg = (0..node.named_child_count())
                        .filter_map(|i| node.named_child(i))
                        .find(|n| matches!(n.kind(), "word" | "string" | "raw_string"));
                    if let Some(arg_node) = arg {
                        let text = node_text(&arg_node, source);
                        let unquoted = text.trim_matches(|c| c == '"' || c == '\'');
                        // Skip dynamic paths ($VAR, $(...), ${...}).
                        if !unquoted.is_empty() && !unquoted.contains('$') {
                            let last = unquoted.rsplit('/').next().unwrap_or(unquoted);
                            let stem = last.trim_end_matches(".sh").trim_end_matches(".bash");
                            if !stem.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: "<module>".into(),
                                    target_name: stem.to_string(),
                                    relation: REL_IMPORTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                } else {
                    // Top-level bash commands ARE the script's imperative
                    // execution flow (unlike declarative-top-level langs like
                    // Rust/Go), so attribute them to `<module>` instead of
                    // dropping them — an entry-point function invoked only at
                    // top level (`run_app "$@"`) would otherwise look dead.
                    // External commands (cd, grep, …) with no same-language
                    // function_definition drop at Phase-2 resolution, so this
                    // doesn't pollute the callgraph. (Mirrors the JS/TS/TSX
                    // top-level `<module>` fallback in the call_expression arm.)
                    let scope = active_scope.unwrap_or("<module>");
                    // Strip path prefix: ./foo, /usr/bin/foo, path/to/foo → foo
                    let short = raw.rsplit('/').next().unwrap_or(raw);
                    // Reject variable expansions ($VAR, ${VAR}), substitutions
                    // ($(...), `...`), and concatenations (foo$VAR) — not statically
                    // resolvable. Allow [a-zA-Z_.][a-zA-Z0-9_.-]* (covers `cat`,
                    // `_helper`, `Backup_Files`, `script.sh`, `.bashrc`).
                    let first_ok = short
                        .chars()
                        .next()
                        .map(|c| c == '_' || c == '.' || c.is_ascii_alphabetic())
                        .unwrap_or(false);
                    let all_ok = short
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
                    if first_ok && all_ok {
                        results.push(ParsedRelation {
                            source_name: scope.to_string(),
                            target_name: short.to_string(),
                            relation: REL_CALLS.into(),
                            metadata: None,
                            source_language: String::new(),
                        });
                    }
                }
            }
        }

        _ => {}
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
