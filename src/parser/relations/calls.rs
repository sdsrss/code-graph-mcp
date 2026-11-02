//! The `calls` axis, one table row per (language, node kind).
//!
//! These used to be thirteen arms of `walk_for_relations`'s giant `match`, which
//! is the shape this crate's top recurring bug class lives in: one arm per
//! language per relation, where a missing arm is not a compile error but a
//! silently absent edge. Tree-sitter grammars do not agree on a call node's name
//! — JS/TS/Rust/Go/Kotlin/Swift use `call_expression`, Python/Ruby use `call`,
//! Java uses `method_invocation`, PHP splits into three kinds plus its own
//! `object_creation_expression`, C# uses `invocation_expression`, Dart marks
//! calls with a `selector`, Bash with `command` — so the mapping is data, and as
//! data it can be enumerated: `call_passes_wire_every_extractor`
//! (tests/call_pass_wiring.rs) asserts every extractor defined here appears in
//! the table, and `table_tests` below asserts no two rows claim the same
//! (language, kind) slot.
//!
//! The table is scanned in order and the FIRST matching row wins, which is
//! exactly the semantics of the `match` it replaces — the rows are disjoint
//! today, and first-match-wins keeps them behaving like arms if they ever stop
//! being.
//!
//! Extraction only. Scope/class/impl propagation and the recursion stay in
//! `walk_for_relations`, which hands each extractor the context it resolved
//! (`CallCtx`) — splitting THAT per language would duplicate the walk.

use super::helpers::{self, extract_callee, extract_string_from_subtree};
use super::{
    node_text, serialize_callee_qualifier, serialize_field_metadata, serialize_rtype_metadata,
};
use super::{LangKey, LanguageConfig, ParsedRelation};
use crate::domain::{REL_CALLS, REL_IMPORTS};

/// Everything a call extractor may read: the node plus the scope context
/// `walk_for_relations` resolved on the way down.
pub(super) struct CallCtx<'a> {
    pub node: tree_sitter::Node<'a>,
    pub source: &'a str,
    /// The file's OWN language (`typescript` and `tsx` are different strings).
    pub language: &'a str,
    /// Dispatch config, whose `name` is the language FAMILY (`.tsx` → its family).
    pub config: &'a LanguageConfig,
    pub active_scope: Option<&'a str>,
    pub current_rust_impl: Option<&'a str>,
}

impl CallCtx<'_> {
    /// Enclosing scope, or `<module>` for languages whose top level is executable
    /// (a function invoked only at file top level would otherwise have no
    /// incoming edge and be reported dead). Rust/Go/C deliberately do NOT use
    /// this — their top-level omission is intentional.
    fn scope_or_module(&self) -> &str {
        self.active_scope.unwrap_or("<module>")
    }
}

type CallExtractor = fn(&CallCtx, &mut Vec<ParsedRelation>);

/// `langs` for a row that fires in EVERY language, because the node kind itself
/// is the guard — `call_expression` and `struct_expression` mean the same thing
/// in every grammar that has them, and what varies (CommonJS require, Rust
/// shadowing, which languages get a `<module>` fallback) is decided inside the
/// extractor.
///
/// Note this is the OPPOSITE of `REFERENCE_PASSES`, where an empty `langs` is a
/// row that can never fire and `reference_passes_have_no_inert_rows` rejects it.
/// That table has no language-agnostic pass; this one has two, so they are
/// spelled with this constant rather than a bare `&[]`.
pub(super) const ANY_LANG: &[&str] = &[];

pub(super) struct CallPass {
    /// Which language name to match on — see [`LangKey`]. Irrelevant for
    /// [`ANY_LANG`] rows.
    pub key: LangKey,
    pub langs: &'static [&'static str],
    pub kinds: &'static [&'static str],
    pub extract: CallExtractor,
}

/// The `calls` axis. Order is the original match order; first match wins.
pub(super) const CALL_PASSES: &[CallPass] = &[
    // The shared arm: JS/TS/TSX, Rust, Go, Java(new), C/C++, Kotlin, Swift, Dart
    // all spell an invocation `call_expression`. Language-specific behaviour
    // inside it (CommonJS require, Rust shadowing, the `<module>` fallback list)
    // is guarded in the extractor, so this row stays language-agnostic.
    CallPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["call_expression"],
        extract: extract_generic_call,
    },
    // Rust/Go struct literal → calls edge, so cross-file dead-code tracking sees
    // a type that is only ever constructed.
    CallPass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["struct_expression"],
        extract: extract_struct_literal,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["javascript", "typescript", "tsx"],
        kinds: &["new_expression"],
        extract: extract_js_new,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["csharp"],
        kinds: &["object_creation_expression"],
        extract: extract_csharp_new,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["php"],
        kinds: &["object_creation_expression"],
        extract: extract_php_new,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["python"],
        kinds: &["call"],
        extract: extract_python_call,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["ruby"],
        kinds: &["call"],
        extract: extract_ruby_call,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["java"],
        kinds: &["method_invocation"],
        extract: extract_java_call,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["php"],
        kinds: &[
            "function_call_expression",
            "member_call_expression",
            "scoped_call_expression",
        ],
        extract: extract_php_call,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["csharp"],
        kinds: &["invocation_expression"],
        extract: extract_csharp_call,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["dart"],
        kinds: &["selector"],
        extract: extract_dart_call,
    },
    CallPass {
        key: LangKey::Family,
        langs: &["bash"],
        kinds: &["command"],
        extract: extract_bash_command,
    },
];

/// Run the first table row matching this node, if any.
///
/// First-match-wins, not run-them-all: these rows were `match` arms, where one
/// arm excludes the others. The rows are disjoint today — and disjoint from the
/// kinds still handled by the caller's `match` — so the stop is belt and braces;
/// it is what keeps a future overlapping row from silently double-emitting.
pub(super) fn run_call_passes(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let kind = ctx.node.kind();
    for pass in CALL_PASSES {
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
        return;
    }
}

// ── extractors ───────────────────────────────────────────────────────────────

fn extract_generic_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);

    // JS/TS CommonJS: require('./foo') / require('pkg') → IMPORTS edge.
    // Mirrors the Ruby `require` handling; target is the last path segment so
    // node_modules imports become `<external>` sentinels and relative imports can
    // match a file module node by name.
    if matches!(ctx.config.name, "javascript" | "typescript" | "tsx")
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
                        // A package (`require('send')`) is no project file: its
                        // import cannot bind a node of this file (D7).
                        let package = !path.starts_with('.') && !path.starts_with('/');
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: segment,
                            relation: REL_IMPORTS.into(),
                            metadata: package.then(|| {
                                serde_json::json!({
                                    "q": crate::domain::CALL_Q_PACKAGE,
                                    "v": normalized,
                                })
                                .to_string()
                            }),
                            source_language: String::new(),
                            source_line: None,
                        });
                    }

                    // `module.exports = require('./lib/express')` (express's index.js;
                    // `exports = module.exports = require(…)` too) re-exports that
                    // file: bind this module to it, as a namespace require does, so
                    // deps/affected follow the package through its index (D7 / C3).
                    let reexport = node
                        .parent()
                        .filter(|p| p.kind() == "assignment_expression")
                        .filter(|p| {
                            p.child_by_field_name("right")
                                .is_some_and(|r| r.id() == node.id())
                        })
                        .and_then(|p| p.child_by_field_name("left"))
                        .is_some_and(|l| {
                            matches!(node_text(&l, source), "module.exports" | "exports")
                        });
                    if reexport {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: "module.exports".into(),
                            relation: REL_IMPORTS.into(),
                            metadata: Some(serde_json::json!({ "q": crate::domain::IMPORT_Q_NS_REQUIRE, "js_module": &path }).to_string()),
                            source_language: String::new(),
                            source_line: None,
                        });
                    }

                    // Destructured require: `const { foo, bar } = require('./x')`.
                    // Emit a per-name import stamped with the full specifier so
                    // js_modules resolution binds each name to the required file's
                    // export (and Phase 2d-bind repoints calls made under the EXPORT
                    // name) — the CommonJS analog of ES named imports. The last-segment
                    // module import above is kept for module-level dep tracking.
                    if let Some(decl) = node.parent().filter(|p| p.kind() == "variable_declarator")
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
                                                Some(node_text(&binding, source).to_string())
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
                                                    source_line: None,
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
                                        source_line: None,
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
    results.extend(super::extract_route_pattern(&node, source, ctx.language));

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
    let call_scope: Option<String> = match ctx.active_scope {
        Some(s) => Some(s.to_string()),
        None if matches!(
            ctx.config.name,
            "javascript" | "typescript" | "tsx" | "kotlin" | "swift"
        ) =>
        {
            Some("<module>".to_string())
        }
        None => None,
    };
    if let Some(scope) = call_scope {
        if let Some((mut callee, mut qualifier)) =
            extract_callee(&node, source, ctx.language, ctx.current_rust_impl)
        {
            // A BARE Rust callee whose name is a local binding of the
            // enclosing fn is a closure/fn-pointer call, not a call of the
            // same-named global fn — Rust's value namespace makes the
            // local win.
            //
            // "Bare" must be decided STRUCTURALLY (the `function` field is
            // an `identifier`), NOT from `CalleeQualifier::Bare`: that
            // variant was once `extract_rust_field`'s fallback arm for a
            // method call whose receiver is not self / a plain identifier /
            // a call. `ctx.db.conn()` has a `field_expression` receiver and
            // so reported Bare — gating on the enum dropped 14 real
            // `Database::conn` edges in this repo alone, every `cmd_*` that
            // writes `let conn = ctx.db.conn();`, because the method name
            // matched the local it is assigned to.
            let bare_call = node
                .child_by_field_name("function")
                .is_some_and(|f| f.kind() == "identifier");
            let shadowed = ctx.language == "rust"
                && bare_call
                && super::rust::shadowed_by_enclosing_local(&node, source, &callee);
            // `self.get_mut()` with `self: Pin<&mut Self>` is `Pin::get_mut`, no
            // project function (`rust_impls::answered_by_the_self_wrapper`).
            let shadowed = shadowed
                || (ctx.language == "rust"
                    && matches!(qualifier, helpers::CalleeQualifier::SelfRecv(_))
                    && super::rust_impls::answered_by_the_self_wrapper(node, source, &callee));
            // `Object.create(null)` runs no project code (D7): with
            // `exports.create = function () {}` a node, express's seven bound it.
            let shadowed = shadowed
                || super::member::js_builtin_global_call(node, source, ctx.language)
                || super::member::js_node_builtin_call(node, source, ctx.language)
                || super::member::js_node_builtin_member_call(node, source, ctx.language);
            // The file's `use` names what the call's leading name stands for
            // (D#132): `use std::sync::Mutex; Mutex::new()` is std's.
            let use_root = if ctx.language == "rust" && !shadowed {
                rust_use_rewrite(node, source, &mut callee, &mut qualifier)
            } else {
                None
            };
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
                            if let Some(impl_type) = ctx.current_rust_impl {
                                *t = impl_type.to_string();
                            } else if matches!(qualifier, helpers::CalleeQualifier::SelfRecv(_)) {
                                // `self.f()` outside an impl (a trait's default
                                // method) is still a method call: as a bare call it
                                // could bind only a function not taking `self`.
                                qualifier = helpers::CalleeQualifier::Member;
                            } else if let Some(trait_name) = enclosing_rust_trait(node, source) {
                                // `Self::f()` in a trait's default method names the
                                // trait: as a bare call it could reach no
                                // associated function at all.
                                qualifier = helpers::CalleeQualifier::SelfType(trait_name);
                            } else {
                                // `Self::f()` outside an impl block — drop qualifier (Bare).
                                qualifier = helpers::CalleeQualifier::Bare;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                // A member call on an object (not this / a module binding) can
                // only run a method: tell the resolver (see `member.rs`). A JS
                // identifier receiver arrives as `Receiver`, kept only when it is
                // an import binding, whose namespace resolution needs it.
                let unqualified = matches!(
                    qualifier,
                    helpers::CalleeQualifier::Bare | helpers::CalleeQualifier::Receiver(_)
                );
                let member =
                    unqualified && super::member::is_member_call(node, source, ctx.config.name);
                // A receiver whose class the source writes down binds that class's
                // method (see `receiver.rs`).
                let receiver_type = unqualified
                    .then(|| super::receiver::receiver_type(node, source, ctx.config.name))
                    .flatten();
                // A field the enclosing function cannot see declared (an
                // out-of-line member, a gtest body): typed at resolution.
                let field = (receiver_type.is_none() && member)
                    .then(|| super::receiver::cpp_field_receiver(node, source, ctx.config.name))
                    .flatten();
                // A receiver that is itself a member access or call: typed at
                // resolution through recorded fields and return types.
                // (`this->opts_.env->f()` is one too, though its root is `this`.)
                let chain = (receiver_type.is_none() && field.is_none() && unqualified)
                    .then(|| super::receiver::cpp_chain_receiver(node, source, ctx.config.name))
                    .flatten();
                let metadata = if let Some(ty) = receiver_type {
                    Some(serialize_rtype_metadata(&ty))
                } else if let Some((class, field, arrow)) = field {
                    Some(serialize_field_metadata(&class, &field, arrow))
                } else if chain.is_some() {
                    chain
                } else if member {
                    Some(super::member::MEMBER_META.to_string())
                } else if ctx.config.name == "cpp" && is_cpp_scoped_call(node) {
                    // `DB::Put()` names its class: tell the resolver it is not a
                    // bare `Put()` (whose implicit `this` prefers the caller's
                    // own class). Resolves as a bare name otherwise.
                    Some(CPP_SCOPED_META.to_string())
                } else {
                    serialize_callee_qualifier(&qualifier)
                };
                // A qualified Rust call carries its argument count: with no
                // overloading, default or variadic parameters, it reaches only a
                // function taking that many (`resolve::rust_call_shape_admits`).
                let metadata = if ctx.language == "rust" {
                    // A method call on a receiver whose type the source writes
                    // down carries it (D#112, `rust_receiver.rs`).
                    let receiver = matches!(
                        qualifier,
                        helpers::CalleeQualifier::Receiver(_)
                            | helpers::CalleeQualifier::Member
                            | helpers::CalleeQualifier::Chain
                    )
                    .then(|| super::rust_receiver::receiver_type(node, source))
                    .flatten();
                    // A `self`/`Self` call cannot reach the same-named method of
                    // another impl the language keeps apart (`rust_impls.rs`).
                    // In an inherent impl it never leaves the crate either.
                    let (facts, inherent) = match &qualifier {
                        helpers::CalleeQualifier::SelfRecv(_)
                        | helpers::CalleeQualifier::SelfType(_)
                            if ctx.current_rust_impl.is_some() =>
                        {
                            (
                                super::rust_impls::self_call_facts(node, source, &callee),
                                super::rust_impls::in_inherent_impl(node),
                            )
                        }
                        _ => (super::rust_impls::SelfCallFacts::default(), false),
                    };
                    metadata.map(|m| {
                        with_self_call_facts(
                            with_receiver_type(
                                with_use_root(with_rust_arity(m, node), use_root),
                                receiver,
                            ),
                            &facts,
                            inherent,
                        )
                    })
                } else {
                    metadata
                };
                // `b()` through `import { a as b }` / `const { a: b } =
                // require()` calls the export `a` (D#120, `member.rs`).
                let (callee, metadata) =
                    match super::member::js_renamed_import_call(node, source, ctx.config.name) {
                        Some((export, module)) => {
                            let metadata = serde_json::json!({
                                "q": crate::domain::CALL_Q_IMPORT,
                                "js_module": module,
                                "v": export,
                            })
                            .to_string();
                            (export, Some(metadata))
                        }
                        None => (callee, metadata),
                    };
                // `send(req)` through `var send = require('send')` (D7).
                let metadata = match metadata {
                    None => super::member::js_package_bound_call(node, source, ctx.config.name)
                        .map(|spec| {
                            serde_json::json!({
                                "q": crate::domain::CALL_Q_PACKAGE,
                                "v": spec,
                            })
                            .to_string()
                        }),
                    some => some,
                };
                results.push(ParsedRelation {
                    source_name: scope,
                    target_name: callee,
                    relation: REL_CALLS.into(),
                    metadata,
                    source_language: String::new(),
                    source_line: None,
                });
            }
        }
    }
}

/// Apply what the file's `use` declarations make of a Rust call (see
/// [`super::rust_use`]) to its callee and qualifier: a name bound to a path
/// (a renamed project import too) becomes a path call through it. Returns the root kind of a rewritten path, for [`with_use_root`].
/// Only a callee spelled with a plain leading name is looked up: `crate::`,
/// `self::`, `super::`, `Self::`, a leading `::` and `<T as Tr>::` bind nothing.
pub(super) fn rust_use_rewrite(
    node: tree_sitter::Node,
    source: &str,
    callee: &mut String,
    qualifier: &mut helpers::CalleeQualifier,
) -> Option<super::rust_use::UseRoot> {
    use super::rust_use::{leftmost_name, rewrite_call, UseRewrite};
    let function = node.child_by_field_name("function")?;
    let lead = leftmost_name(&function)?;
    let lead = node_text(&lead, source);
    let rewrite = match &*qualifier {
        helpers::CalleeQualifier::Bare if lead == callee => {
            rewrite_call(&node, source, callee, None)
        }
        helpers::CalleeQualifier::Path(path) if path.first().is_some_and(|f| f == lead) => {
            rewrite_call(&node, source, callee, Some(path))
        }
        _ => None,
    }?;
    let UseRewrite::Path {
        name,
        segments,
        root,
    } = rewrite;
    *callee = name;
    *qualifier = helpers::CalleeQualifier::Path(segments);
    Some(root)
}

/// `metadata` with `"u"`: a path a `use` spelled out, rooted at this crate
/// (`"c"`: `crate`/`self`/`super`, counted from the file's module) or at a crate
/// name (`"x"`). See `resolve::rust_use_anchor`.
pub(super) fn with_use_root(metadata: String, root: Option<super::rust_use::UseRoot>) -> String {
    let Some(root) = root else {
        return metadata;
    };
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(&metadata) else {
        return metadata;
    };
    let tag = match root {
        super::rust_use::UseRoot::Project => "c",
        super::rust_use::UseRoot::Extern => "x",
    };
    map.insert("u".into(), tag.into());
    serde_json::Value::Object(map).to_string()
}

/// `metadata` with the receiver type keys of [`super::rust_receiver`].
fn with_receiver_type(metadata: String, receiver: Option<super::rust_receiver::RecvTy>) -> String {
    let Some(ty) = receiver else {
        return metadata;
    };
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(&metadata) else {
        return metadata;
    };
    for (k, v) in super::rust_receiver::receiver_keys(&ty) {
        map.insert(k.into(), v.into());
    }
    serde_json::Value::Object(map).to_string()
}

/// `metadata` with what a `self`/`Self` call's own file tells the resolver
/// (`rust_impls::self_call_facts`): `"xl"`, the start lines of same-file methods
/// the call cannot reach; `"wide"`, set when the file defines the type's method
/// of that name only in trait impls, which an inherent one elsewhere outranks;
/// and `"inh"`, set in an inherent impl, whose calls stay in the crate
/// (`rust_impls::in_inherent_impl`). Unchanged when there is none of these.
fn with_self_call_facts(
    metadata: String,
    facts: &super::rust_impls::SelfCallFacts,
    inherent: bool,
) -> String {
    if facts.excluded.is_empty() && !facts.trait_only_here && !inherent {
        return metadata;
    }
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(&metadata) else {
        return metadata;
    };
    if !facts.excluded.is_empty() {
        map.insert("xl".into(), serde_json::json!(facts.excluded));
    }
    if facts.trait_only_here {
        map.insert("wide".into(), serde_json::json!(1));
    }
    if inherent {
        map.insert("inh".into(), serde_json::json!(1));
    }
    serde_json::Value::Object(map).to_string()
}

/// The name of the `trait` whose body holds `node`, stopping at an `impl`.
fn enclosing_rust_trait(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "trait_item" => {
                return n
                    .child_by_field_name("name")
                    .map(|name| node_text(&name, source).to_string());
            }
            "impl_item" => return None,
            _ => cur = n.parent(),
        }
    }
    None
}

/// `metadata` with `"n"`: the arguments `call` passes. Comments and attributes
/// in the list are not arguments.
fn with_rust_arity(metadata: String, call: tree_sitter::Node) -> String {
    let Some(args) = call.child_by_field_name("arguments") else {
        return metadata;
    };
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(&metadata) else {
        return metadata;
    };
    let n = (0..args.named_child_count())
        .filter_map(|i| args.named_child(i))
        .filter(|a| !a.is_extra() && a.kind() != "attribute_item")
        .count();
    map.insert("n".into(), n.into());
    serde_json::Value::Object(map).to_string()
}

/// Metadata of a C++ call through a scope (`DB::Put()`, `ns::f<T>()`).
pub(crate) const CPP_SCOPED_META: &str = r#"{"q":"scoped"}"#;

fn is_cpp_scoped_call(call: tree_sitter::Node) -> bool {
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    match function.kind() {
        "qualified_identifier" => true,
        "template_function" => function
            .child_by_field_name("name")
            .is_some_and(|n| n.kind() == "qualified_identifier"),
        _ => false,
    }
}

/// Rust/Go: struct instantiation → calls edge (enables cross-file dead code
/// tracking) e.g. `MyStruct { field: value }` (`MyStruct::new()` is a call
/// already handled by the generic arm).
fn extract_struct_literal(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    if let Some(scope) = ctx.active_scope {
        if let Some(name_node) = ctx.node.child_by_field_name("name") {
            let struct_name = node_text(&name_node, ctx.source);
            // Strip path prefix: path::MyStruct → MyStruct
            let short_name = struct_name.rsplit("::").next().unwrap_or(struct_name);
            if !short_name.is_empty() {
                results.push(ParsedRelation {
                    source_name: scope.to_string(),
                    target_name: short_name.to_string(),
                    relation: REL_CALLS.into(),
                    metadata: None,
                    source_language: String::new(),
                    source_line: None,
                });
            }
        }
    }
}

/// JS/TS/TSX: `new Foo()` / `new ns.Foo()` constructor instantiation → calls edge
/// to the class (constructor) name.
///
/// A `new_expression`'s callee is its `constructor` field, which never reaches
/// the generic `call_expression` arm, so a class that is only instantiated (never
/// called as a `Foo.method()`) had NO incoming calls edge — invisible to
/// callgraph/impact. The JS value-reference pass emits a `references` edge only in
/// value positions (call arg, binding RHS, return, ...), NOT for a `new`
/// constructor slot (parent is `new_expression`), so such a class was also
/// edgeless and false-flagged dead-code. Mirrors the Rust `struct_expression`
/// arm. Generic args (`new Foo<T>()`) are a separate `type_arguments` field, but a
/// `<...>` tail is stripped defensively. Member form `new ns.Foo()` yields callee
/// `Foo` with a Receiver("ns") qualifier when the receiver is a simple identifier
/// — consistent with how member `call_expression`s are handled. Scope mirrors the
/// call arm (`<module>` fallback at file top level).
fn extract_js_new(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    let scope = ctx.scope_or_module().to_string();
    if let Some(ctor) = node.child_by_field_name("constructor") {
        let callee: Option<(String, helpers::CalleeQualifier)> = match ctor.kind() {
            "identifier" => {
                let raw = node_text(&ctor, source);
                let name = raw.split('<').next().unwrap_or(raw).trim();
                (!name.is_empty()).then(|| (name.to_string(), helpers::CalleeQualifier::Bare))
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
                    .map(|o| helpers::CalleeQualifier::Receiver(node_text(&o, source).to_string()))
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
                source_line: None,
            });
        }
    }
}

/// C#: `new Foo()` / `new Ns.Bar()` object creation → calls edge to the class
/// (constructor) name.
///
/// `object_creation_expression` is distinct from `invocation_expression`, and C#
/// has no type-reference pass, so a class that is only instantiated had NO
/// incoming edge and was false-flagged dead-code. Mirrors the JS `new_expression`
/// arm; top-level `new` (C# 9 top-level program) attributes to `<module>` like the
/// C# invocation arm. The class name is the `type` field: an `identifier`
/// (`new Foo()`) or a `qualified_name` (`new Ns.Bar()` → its `name` field).
fn extract_csharp_new(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    let scope = ctx.scope_or_module();
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
                    source_line: None,
                });
            }
        }
    }
}

/// PHP: `new Foo()` / `new Ns\Bar()` object creation → calls edge to the class
/// (constructor) name. PHP has no type-reference pass, so a class only
/// instantiated had NO incoming edge and was false-flagged dead-code. Mirrors the
/// JS/C# arms. The type is a positional `name` or `qualified_name` child (no
/// field); for a qualified name the class is the last `name` segment (namespace
/// prefix lives under `namespace_name`). `new self()/static()/parent()` and
/// `new $var()` are relative/dynamic — skipped (a `self`/`static`/`parent` edge is
/// pure noise; a variable is not a class name).
fn extract_php_new(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    let scope = ctx.scope_or_module();
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
                    source_line: None,
                });
            }
        }
    }
}

/// Python: tree-sitter-python uses `call` (not `call_expression`) for every
/// function/method invocation. Without this pass all Python call edges are
/// silently dropped — README documents Python as Full tier, and module_overview /
/// impact_analysis / find_dead_code all rely on this edge. Routes and
/// `from X import Y` are extracted elsewhere.
fn extract_python_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    // Top-level (module / class-body) calls attribute to `<module>` rather than
    // being dropped — a function invoked only at import time (`app.run()`, a bare
    // `main_entry()`) would otherwise have no incoming edge and be reported dead.
    // Undefined callees (print, os.path.join, …) drop at Phase-2 same-language
    // resolution, so this only adds edges to defined same-project functions.
    let scope = ctx.scope_or_module();
    if let Some(callee) = helpers::extract_callee_name(&node, source) {
        // Receiver-type propagation (issue #32 cause 2): when the call is
        // `recv.method()` and `recv`'s type is fixed by a single local
        // `recv = ClassName(...)` constructor assignment, stamp
        // `{"q":"rtype","v":"ClassName"}` so Phase-2 resolution binds it to
        // `ClassName.method` (self_filter_candidates) instead of dropping
        // the ambiguous by-name fan-out across every same-named method.
        // Falls back to the bare, metadata-less form (unchanged behavior)
        // whenever the type can't be proven — never emits a wrong-type edge.
        let metadata = super::python::infer_python_super_type(&node, source)
            .map(|ty| serde_json::json!({ "q": "super", "v": ty }).to_string())
            .or_else(|| {
                super::infer_python_call_receiver_type(&node, source)
                    .map(|ty| serialize_rtype_metadata(&ty))
            })
            .or_else(|| {
                super::member::is_member_call(node, source, "python")
                    .then(|| super::member::MEMBER_META.to_string())
            })
            .or_else(|| super::member::python_module_call_meta(node, source))
            .or_else(|| super::member::python_untyped_receiver_meta(node, source));
        results.push(ParsedRelation {
            source_name: scope.to_string(),
            target_name: callee,
            relation: REL_CALLS.into(),
            metadata,
            source_language: String::new(),
            source_line: None,
        });
    }
}

/// Ruby: `call` covers require, require_relative and regular method calls.
fn extract_ruby_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    // Extract method name from the "method" field
    if let Some(method_node) = node.child_by_field_name("method") {
        let method_name = node_text(&method_node, source);
        // require 'json' / require_relative 'helper'
        if method_name == "require" || method_name == "require_relative" {
            if let Some(args) = node.child_by_field_name("arguments") {
                if let Some(first_arg) = args.named_child(0) {
                    if let Some(string_val) = extract_string_from_subtree(&first_arg, source) {
                        results.push(ParsedRelation {
                            source_name: ctx.scope_or_module().to_string(),
                            target_name: string_val,
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                            source_language: String::new(),
                            source_line: None,
                        });
                    }
                }
            }
        } else {
            // Regular method call. Top-level calls attribute to `<module>` (same
            // rationale as the python/bash arms) so a top-level entry call isn't
            // dropped; undefined callees drop at Phase-2 same-language resolution.
            let scope = ctx.scope_or_module();
            results.push(ParsedRelation {
                source_name: scope.to_string(),
                target_name: method_name.to_string(),
                relation: REL_CALLS.into(),
                metadata: None,
                source_language: String::new(),
                source_line: None,
            });
        }
    }
}

/// Java: tree-sitter-java uses `method_invocation` (NOT `call_expression`) for
/// every method call — `foo()`, `this.foo()`, `obj.foo()`, `Type.foo()`. The
/// callee name is the `name` field; the optional `object` field is the receiver.
/// Without this pass ALL Java call edges were silently dropped despite Java being
/// documented Full tier — callgraph / impact_analysis / find_dead_code all depend
/// on these edges. Bare callee name mirrors the Python/Ruby arms; downstream
/// target resolution disambiguates same names.
///
/// Unlike the python/ruby/php/csharp arms this one requires a REAL scope: Java has
/// no executable file top level, so there is nothing for `<module>` to mean.
fn extract_java_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    if let Some(scope) = ctx.active_scope {
        if let Some(name_node) = ctx.node.child_by_field_name("name") {
            let callee = node_text(&name_node, ctx.source).to_string();
            if !callee.is_empty() {
                results.push(ParsedRelation {
                    source_name: scope.to_string(),
                    target_name: callee,
                    relation: REL_CALLS.into(),
                    metadata: None,
                    source_language: String::new(),
                    source_line: None,
                });
            }
        }
    }
}

/// PHP: function_call_expression (doSomething()), member_call_expression
/// ($this->move()), scoped_call_expression (User::all()).
fn extract_php_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    // Top-level calls (outside any function/method) attribute to `<module>`
    // rather than being dropped, mirroring the python/ruby/bash arms — a
    // function invoked only at the top level (`greetPhp();`) would otherwise
    // have no incoming edge and be false-reported as dead. Undefined callees
    // drop at Phase-2 same-language resolution.
    let scope = ctx.scope_or_module();
    // All three PHP call types have a `name` child for the method/function name.
    // For scoped_call_expression there are multiple `name` children; the second
    // is the method.
    let callee = if node.kind() == "scoped_call_expression" {
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
                source_line: None,
            });
        }
    }
}

/// C# method/function calls: invocation_expression (Console.WriteLine(...), Baz(), …).
fn extract_csharp_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
    // Top-level statement calls (C# 9+ top-level programs) and calls in field
    // initializers outside any method attribute to `<module>` rather than being
    // dropped, mirroring the php/python/ruby arms — a function invoked only from a
    // top-level statement would otherwise have no incoming edge and be
    // false-reported as dead. Undefined callees drop at Phase-2 same-language
    // resolution.
    let scope = ctx.scope_or_module();
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
                    source_line: None,
                });
            }
        }
    }
}

/// Dart: a `selector` carrying an argument_part marks a call in ANY position
/// (return / assignment / argument / binary expr / bare statement) — not just
/// `expression_statement`. tree-sitter-dart has no single call_expression node, so
/// the selector is the reliable marker; the callee is its preceding sibling.
/// active_scope propagates down from the enclosing function_body, so deeply-nested
/// calls still attribute.
fn extract_dart_call(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    // Library-level (top-level) calls attribute to `<module>` rather than being
    // dropped, mirroring the php/python/ruby/C# arms — a top-level `main()`-free
    // Dart script's calls would otherwise have no incoming edge. Undefined callees
    // drop at Phase-2 same-language resolution.
    let scope = ctx.scope_or_module();
    super::extract_dart_call_from_selector(&ctx.node, ctx.source, scope, results);
}

/// Bash command invocation:
///   `source <file>` / `. <file>` → IMPORTS edge (mirrors JS require).
///   Otherwise: CALLS edge to command_name.
/// External commands (cat, grep, ...) without a function_definition in any indexed
/// shell file get dropped at Phase 2 same-language edge resolution (see
/// feedback_edge_resolution_same_language).
fn extract_bash_command(ctx: &CallCtx, results: &mut Vec<ParsedRelation>) {
    let (node, source) = (ctx.node, ctx.source);
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
                            source_line: None,
                        });
                    }
                }
            }
        } else {
            // Top-level bash commands ARE the script's imperative execution flow
            // (unlike declarative-top-level langs like Rust/Go), so attribute them
            // to `<module>` instead of dropping them — an entry-point function
            // invoked only at top level (`run_app "$@"`) would otherwise look dead.
            // External commands (cd, grep, …) with no same-language
            // function_definition drop at Phase-2 resolution, so this doesn't
            // pollute the callgraph. (Mirrors the JS/TS/TSX top-level `<module>`
            // fallback in the call_expression arm.)
            let scope = ctx.scope_or_module();
            // Strip path prefix: ./foo, /usr/bin/foo, path/to/foo → foo
            let short = raw.rsplit('/').next().unwrap_or(raw);
            // Reject variable expansions ($VAR, ${VAR}), substitutions ($(...),
            // `...`), and concatenations (foo$VAR) — not statically resolvable.
            // Allow [a-zA-Z_.][a-zA-Z0-9_.-]* (covers `cat`, `_helper`,
            // `Backup_Files`, `script.sh`, `.bashrc`).
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
                    source_line: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    /// These rows replaced `match` arms, where the compiler rejects a duplicate
    /// arm. A table has no such check: a second row for the same (language, kind)
    /// is simply unreachable, and the edges its extractor would have emitted are
    /// silently absent — the exact failure mode the table exists to remove.
    #[test]
    fn no_two_rows_claim_the_same_language_and_kind() {
        let mut claimed: Vec<(&str, &str)> = Vec::new();
        for pass in CALL_PASSES {
            for kind in pass.kinds {
                // An ANY_LANG row claims the kind for every language, so any
                // other row naming that kind is dead.
                if pass.langs.is_empty() {
                    let conflict = CALL_PASSES
                        .iter()
                        .filter(|p| p.kinds.contains(kind))
                        .count();
                    assert_eq!(
                        conflict,
                        1,
                        "kind {kind:?} is claimed by an ANY_LANG row and by {} other row(s) — \
                         those can never fire",
                        conflict - 1
                    );
                    continue;
                }
                for lang in pass.langs {
                    let slot = (*lang, *kind);
                    assert!(
                        !claimed.contains(&slot),
                        "two CALL_PASSES rows claim {slot:?} — the second can never fire"
                    );
                    claimed.push(slot);
                }
            }
        }
    }

    /// A row that matches nothing compiles fine and emits nothing forever.
    #[test]
    fn no_row_is_inert() {
        for pass in CALL_PASSES {
            assert!(
                !pass.kinds.is_empty(),
                "a CALL_PASSES row has no node kinds — it can never fire"
            );
            assert!(
                pass.kinds.iter().all(|k| !k.is_empty()),
                "a CALL_PASSES row has an empty node kind — it can never fire"
            );
            assert!(
                pass.langs.iter().all(|l| !l.is_empty()),
                "a CALL_PASSES row has an empty language name — that slot can never match"
            );
        }
    }
}
