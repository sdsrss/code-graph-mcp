//! The `heritage` axis (`inherits` + `implements`), one table row per
//! (language, node kind), plus the extractors it names.
//!
//! Class inheritance and interface implementation across the tier-1 + tier-2
//! languages we parse: TS/JS, Python, Java, Ruby, Kotlin, Swift, PHP, plus C++
//! base clauses, Rust `impl Trait for Type`, Go embedding and C# base lists.
//! Inheritance shapes vary per grammar (`extends_clause`, `argument_list`,
//! `superclass`, `delegation_specifiers`, `base_clause`,
//! `inheritance_specifier`), so each is matched explicitly.
//!
//! These were arms of `walk_for_relations`'s match. Two things about the axis
//! were said to block tabling, and only one of them was real:
//!
//!   * "heritage dispatches on the `is_heritage_decl` PREDICATE rather than a
//!     fixed kind list" — but that predicate is `HERITAGE_DECL_KINDS.contains`,
//!     i.e. a fixed kind list wearing a function. The row points at the same
//!     const.
//!   * The C# `base_list` arm additionally inspects the PARENT node kind, to
//!     keep `enum Level : byte` from emitting `Level inherits byte`. That one is
//!     real, and it is why the row shape carries `not_under` — one field, not a
//!     new dispatch mechanism.
//!
//! Sibling tables: `calls::CALL_PASSES`, `imports::IMPORT_PASSES`,
//! `exports::EXPORT_PASSES`, `super::REFERENCE_PASSES`.
//!
//! The match was FIRST-MATCH-WINS; this table runs EVERY matching row. That is
//! only equivalent while no two rows can fire on the same node, which is no
//! longer a property of `match` and so is asserted by
//! `no_node_kind_reaches_two_heritage_or_export_rows` in `super::tests`.

use super::super::lang_config::LanguageConfig;
use super::super::node_text;
use super::{LangKey, ParsedRelation};
use crate::domain::{REL_IMPLEMENTS, REL_INHERITS};

/// Declaration node kinds that can carry heritage, across every grammar we
/// parse. Rows, not an `|`-chain, because the axis was unguarded: the walk
/// matched exactly `class_declaration | class_definition | class`, so a grammar
/// that spells its declaration differently emitted ZERO inheritance edges and
/// nothing failed — the graph was simply incomplete, and `find_dead_code` then
/// reported an interface's implementers as unused (audit 2026-08-16 P1-3).
///
/// Every kind here was read off a real parse of the language in question (see
/// `heritage_parity_across_declaration_kinds`), not off a grammar README.
/// Adding a language means adding a row here AND a row to that table.
///
/// Deliberately NOT listed:
///   * `struct_specifier` / `class_specifier` — C/C++ have their own arm below
///     (`extract_cpp_inheritance`), which understands access specifiers.
///   * `trait_declaration` (PHP) — a trait has no heritage clause; it is
///     `use`d by a class, which is a different relation than extends/implements.
///   * `struct_item` / `enum_item` (Rust) — Rust has no class inheritance at
///     all; `impl Trait for Type` is handled as `implements` elsewhere.
pub(super) const HERITAGE_DECL_KINDS: &[&str] = &[
    // TS/JS/Java/PHP/C#/Kotlin/Swift/Dart all spell their class this way.
    "class_declaration",
    "class_definition",
    "class",
    // Java (extends interfaces), TypeScript (extends interfaces), PHP, C#.
    "interface_declaration",
    // Java (implements), PHP (implements), Dart (implements).
    "enum_declaration",
    // Java, C#.
    "record_declaration",
    // C#.
    "struct_declaration",
    // Kotlin: `object Registry : BaseRegistry`.
    "object_declaration",
    // Swift: `protocol Cache: Store`.
    "protocol_declaration",
];

/// Everything a heritage extractor is allowed to see. Mirrors
/// `calls::CallCtx` / `imports::ImportCtx`: the resolved context arrives as
/// data so the extractor need not be a closure over `walk_for_relations`'s
/// locals, which is what kept these bodies inside the walk.
pub(super) struct HeritageCtx<'a> {
    pub node: tree_sitter::Node<'a>,
    pub source: &'a str,
    pub language: &'a str,
    pub config: &'a LanguageConfig,
    /// Only the C# `base_list` row reads it, as the fallback owner when the
    /// parent declaration carries no `name` field.
    pub active_scope: Option<&'a str>,
}

type HeritageExtractor = fn(&HeritageCtx, &mut Vec<ParsedRelation>);

/// `langs` for a row whose node kind is itself the guard, because no other
/// grammar spells that kind — same convention as `calls::ANY_LANG` and
/// `imports::ANY_LANG`.
pub(super) const ANY_LANG: &[&str] = &[];

pub(super) struct HeritagePass {
    /// Which language name to match on — see [`LangKey`]. Irrelevant for
    /// [`ANY_LANG`] rows.
    pub key: LangKey,
    pub langs: &'static [&'static str],
    pub kinds: &'static [&'static str],
    /// Parent node kinds that VETO the row. Empty for every row but C#, where
    /// `base_list` means two different things depending on what owns it.
    pub not_under: &'static [&'static str],
    pub extract: HeritageExtractor,
}

/// The `heritage` axis. Order is the original match order.
pub(super) const HERITAGE_PASSES: &[HeritagePass] = &[
    // Declarations that can carry heritage. The kinds are [`HERITAGE_DECL_KINDS`]
    // itself — this row used to be `kind if is_heritage_decl(kind)`, and the
    // predicate was never more than a lookup in that const.
    HeritagePass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: HERITAGE_DECL_KINDS,
        not_under: &[],
        extract: extract_declaration_heritage,
    },
    // C++: base classes → inherits (C++ has no separate interface concept, so
    // every base — public/private/protected — is an inherits parent). C structs
    // and plain C++ aggregates have no base_class_clause → nothing emitted, so
    // no language gate is needed. Deliberately NOT in HERITAGE_DECL_KINDS: this
    // extractor understands access specifiers and the generic one does not.
    HeritagePass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["class_specifier", "struct_specifier"],
        not_under: &[],
        extract: extract_cpp_heritage,
    },
    // Rust: `impl Trait for Type` → implements, type-level + method-level.
    HeritagePass {
        key: LangKey::Family,
        langs: ANY_LANG,
        kinds: &["impl_item"],
        not_under: &[],
        extract: extract_rust_impl_heritage,
    },
    // Go: struct/interface embedding → inherits (method promotion / interface
    // composition — Go's idiomatic "is-a"). Gated on the RAW language because
    // `type_spec` is a Go-grammar node and the guard keeps any other grammar's
    // same-named node from being misread.
    HeritagePass {
        key: LangKey::Raw,
        langs: &["go"],
        kinds: &["type_spec"],
        not_under: &[],
        extract: extract_go_heritage,
    },
    // C#: `class Dog : Animal, IWalkable`.
    //
    // NOT on an enum. C# spells an enum's UNDERLYING INTEGRAL TYPE with the same
    // `base_list` syntax a class uses for its base type, so `enum Level : byte`
    // produced `Level inherits byte` — a phantom edge bound to a real node,
    // which this repo has already learned is worse than a missing one (it makes
    // `byte` look inherited-from and pollutes every heritage traversal). The
    // grammar-level distinction is the parent kind, hence `not_under`.
    HeritagePass {
        key: LangKey::Family,
        langs: &["csharp"],
        kinds: &["base_list"],
        not_under: &["enum_declaration"],
        extract: extract_csharp_base_list,
    },
];

/// Run every [`HERITAGE_PASSES`] row that matches this node.
pub(super) fn run_heritage_passes(ctx: &HeritageCtx, results: &mut Vec<ParsedRelation>) {
    let kind = ctx.node.kind();
    for pass in HERITAGE_PASSES {
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
        // A node with no parent is not "under" anything, so it is never vetoed —
        // the original arm spelled this as `parent().map(kind) != Some(..)`.
        if let Some(parent) = ctx.node.parent() {
            if pass.not_under.contains(&parent.kind()) {
                continue;
            }
        }
        (pass.extract)(ctx, results);
    }
}

fn extract_declaration_heritage(ctx: &HeritageCtx, results: &mut Vec<ParsedRelation>) {
    let Some(cls) = ctx
        .node
        .child_by_field_name("name")
        .map(|n| node_text(&n, ctx.source).to_string())
    else {
        return;
    };
    // extends / superclass (supports multiple inheritance)
    for parent in extract_superclasses(&ctx.node, ctx.source) {
        results.push(ParsedRelation {
            source_name: cls.clone(),
            target_name: parent,
            relation: REL_INHERITS.into(),
            metadata: None,
            source_language: String::new(),
        });
    }
    extract_implements(&ctx.node, ctx.source, &cls, results);
}

fn extract_cpp_heritage(ctx: &HeritageCtx, results: &mut Vec<ParsedRelation>) {
    for rel in super::cpp::extract_cpp_inheritance(&ctx.node, ctx.source) {
        results.push(rel);
    }
}

fn extract_go_heritage(ctx: &HeritageCtx, results: &mut Vec<ParsedRelation>) {
    for rel in super::go::extract_go_inheritance(&ctx.node, ctx.source) {
        results.push(rel);
    }
}

fn extract_rust_impl_heritage(ctx: &HeritageCtx, results: &mut Vec<ParsedRelation>) {
    let Some(impl_rel) = super::rust::extract_rust_impl_trait(&ctx.node, ctx.source) else {
        return;
    };
    let type_name = impl_rel.source_name.clone();
    results.push(impl_rel);
    // For each method in the trait impl block, emit a method-level implements
    // edge: TypeName → method_name. This ensures dead code detection sees
    // incoming implements edges on trait methods.
    let Some(body) = ctx.node.child_by_field_name("body") else {
        return;
    };
    for i in 0..body.named_child_count() {
        let Some(child) = body.named_child(i) else {
            continue;
        };
        if child.kind() != "function_item" {
            continue;
        }
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let method_name = node_text(&name_node, ctx.source);
        if method_name.is_empty() {
            continue;
        }
        // Stamp impl_type so Phase 2 can filter method candidates by
        // qualified_name. Without it, a file with N structs each implementing
        // the same trait fans every impl's method edge to all N same-named
        // method nodes — every struct appears to implement every other
        // struct's methods.
        results.push(ParsedRelation {
            source_name: type_name.clone(),
            target_name: method_name.to_string(),
            relation: REL_IMPLEMENTS.into(),
            metadata: Some(super::serialize_impl_method_metadata(&type_name)),
            source_language: String::new(),
        });
    }
}

fn extract_csharp_base_list(ctx: &HeritageCtx, results: &mut Vec<ParsedRelation>) {
    // The class/struct name comes from the parent node.
    let owner_name = ctx
        .node
        .parent()
        .and_then(|p| p.child_by_field_name("name"))
        .map(|n| node_text(&n, ctx.source).to_string());
    let owner = owner_name
        .as_deref()
        .or(ctx.active_scope)
        .unwrap_or("<module>");
    for i in 0..ctx.node.named_child_count() {
        let Some(child) = ctx.node.named_child(i) else {
            continue;
        };
        let base_name = node_text(&child, ctx.source).to_string();
        if base_name.is_empty() {
            continue;
        }
        let rel = if ctx.config.interface_by_prefix
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

pub(super) fn extract_superclasses(node: &tree_sitter::Node, source: &str) -> Vec<String> {
    let mut parents = Vec::new();
    // Look for "extends" clause / superclass
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "class_heritage" | "extends_clause" => {
                // TS/JS: class_heritage -> extends_clause -> type_identifier
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "extends_clause" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "identifier"
                                        || type_node.kind() == "type_identifier"
                                    {
                                        parents.push(node_text(&type_node, source).to_string());
                                    }
                                }
                            }
                        }
                        if inner.kind() == "identifier" || inner.kind() == "type_identifier" {
                            parents.push(node_text(&inner, source).to_string());
                        }
                    }
                }
            }
            // Java: `interface Shape extends Drawable, Sized`
            // extends_interfaces -> type_list -> type_identifier
            "extends_interfaces" => {
                for k in 0..child.named_child_count() {
                    if let Some(list) = child.named_child(k) {
                        if list.kind() == "type_list" {
                            for m in 0..list.named_child_count() {
                                if let Some(t) = list.named_child(m) {
                                    if matches!(t.kind(), "type_identifier" | "identifier") {
                                        parents.push(node_text(&t, source).to_string());
                                    }
                                }
                            }
                        } else if matches!(list.kind(), "type_identifier" | "identifier") {
                            parents.push(node_text(&list, source).to_string());
                        }
                    }
                }
            }
            // TypeScript: `interface Admin extends User, Auditable`
            // extends_type_clause -> type_identifier (direct children)
            "extends_type_clause" => {
                for k in 0..child.named_child_count() {
                    if let Some(t) = child.named_child(k) {
                        match t.kind() {
                            "type_identifier" | "identifier" => {
                                parents.push(node_text(&t, source).to_string());
                            }
                            // `interface A extends B<T>` — index the base name.
                            "generic_type" => {
                                if let Some(inner) = t.named_child(0) {
                                    if matches!(inner.kind(), "type_identifier" | "identifier") {
                                        parents.push(node_text(&inner, source).to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "argument_list" => {
                // Python: class Dog(Animal, Pet) — extract all parent classes
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "identifier" || inner.kind() == "dotted_name" {
                            parents.push(node_text(&inner, source).to_string());
                        }
                    }
                }
            }
            "superclass" => {
                // Java: superclass -> type_identifier
                // Ruby: superclass -> constant (e.g., `< ApplicationController`)
                // Dart: superclass -> type_identifier (extends) + optional `mixins`
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        match inner.kind() {
                            "type_identifier" | "identifier" | "dotted_name" | "constant"
                            | "scope_resolution" => {
                                parents.push(node_text(&inner, source).to_string());
                            }
                            // Dart: `class C extends Base with M, N` — mixin
                            // application injects each mixin's methods into the
                            // class, so treat each mixin as an inherited parent.
                            // (Without this, a `with`-only class fell through to
                            // the text-clean fallback below and produced "with M".)
                            "mixins" => {
                                for m in 0..inner.named_child_count() {
                                    if let Some(mix) = inner.named_child(m) {
                                        if matches!(mix.kind(), "type_identifier" | "identifier") {
                                            parents.push(node_text(&mix, source).to_string());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if parents.is_empty() {
                    let text = node_text(&child, source);
                    let cleaned = text
                        .trim_start_matches(|c: char| c == '(' || c == '<' || c.is_whitespace())
                        .trim_end_matches(|c: char| c == ')' || c.is_whitespace())
                        .to_string();
                    if !cleaned.is_empty() {
                        parents.push(cleaned);
                    }
                }
            }
            "delegation_specifiers" => {
                // Kotlin: class UserService : BaseService, UserRepository
                // delegation_specifiers -> delegation_specifier -> user_type -> identifier
                for k in 0..child.named_child_count() {
                    if let Some(spec) = child.named_child(k) {
                        if spec.kind() == "delegation_specifier" {
                            // Walk through user_type to find the identifier
                            if let Some(user_type) = spec.named_child(0) {
                                if let Some(ident) = user_type.named_child(0) {
                                    let name = node_text(&ident, source).to_string();
                                    if !name.is_empty() {
                                        parents.push(name);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "base_clause" => {
                // PHP: class Dog extends Animal
                // base_clause -> name (the parent class)
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "name" || inner.kind() == "qualified_name" {
                            let name = node_text(&inner, source).to_string();
                            if !name.is_empty() {
                                parents.push(name);
                            }
                        }
                    }
                }
            }
            "inheritance_specifier" => {
                // Swift: class UserService: UserRepository, Codable
                // inheritance_specifier -> user_type -> type_identifier
                if let Some(inherits_from) = child.child_by_field_name("inherits_from") {
                    // Walk into user_type to find type_identifier
                    let name = if inherits_from.kind() == "user_type" {
                        inherits_from
                            .named_child(0)
                            .map(|n| node_text(&n, source).to_string())
                    } else {
                        Some(node_text(&inherits_from, source).to_string())
                    };
                    if let Some(name) = name {
                        if !name.is_empty() {
                            parents.push(name);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    parents
}

pub(super) fn extract_implements(
    node: &tree_sitter::Node,
    source: &str,
    class_name: &str,
    results: &mut Vec<ParsedRelation>,
) {
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            // TS/JS: class_heritage contains implements_clause children
            "class_heritage" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "implements_clause" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "type_identifier"
                                        || type_node.kind() == "identifier"
                                    {
                                        results.push(ParsedRelation {
                                            source_name: class_name.to_string(),
                                            target_name: node_text(&type_node, source).to_string(),
                                            relation: REL_IMPLEMENTS.into(),
                                            metadata: None,
                                            source_language: String::new(),
                                        });
                                    }
                                    // Handle generic_type: IService<T> -> extract IService
                                    if type_node.kind() == "generic_type" {
                                        if let Some(name_node) = type_node.named_child(0) {
                                            if name_node.kind() == "type_identifier"
                                                || name_node.kind() == "identifier"
                                            {
                                                results.push(ParsedRelation {
                                                    source_name: class_name.to_string(),
                                                    target_name: node_text(&name_node, source)
                                                        .to_string(),
                                                    relation: REL_IMPLEMENTS.into(),
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
                }
            }
            // PHP: class Dog implements Walkable, Swimmable
            // class_interface_clause -> name children
            "class_interface_clause" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "name" || inner.kind() == "qualified_name" {
                            let name = node_text(&inner, source).to_string();
                            if !name.is_empty() {
                                results.push(ParsedRelation {
                                    source_name: class_name.to_string(),
                                    target_name: name,
                                    relation: REL_IMPLEMENTS.into(),
                                    metadata: None,
                                    source_language: String::new(),
                                });
                            }
                        }
                    }
                }
            }
            // Dart: `class FileStore implements Store` / `enum Level implements C`
            // interfaces -> type_identifier (direct children, no type_list)
            "interfaces"
                if child.named_child_count() > 0
                    && child.named_child(0).map(|c| c.kind()) != Some("type_list") =>
            {
                for j in 0..child.named_child_count() {
                    if let Some(t) = child.named_child(j) {
                        if matches!(t.kind(), "type_identifier" | "identifier") {
                            results.push(ParsedRelation {
                                source_name: class_name.to_string(),
                                target_name: node_text(&t, source).to_string(),
                                relation: REL_IMPLEMENTS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
            // Java: super_interfaces -> type_list -> type_identifier
            "super_interfaces" | "interfaces" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "type_list" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "type_identifier"
                                        || type_node.kind() == "identifier"
                                    {
                                        results.push(ParsedRelation {
                                            source_name: class_name.to_string(),
                                            target_name: node_text(&type_node, source).to_string(),
                                            relation: REL_IMPLEMENTS.into(),
                                            metadata: None,
                                            source_language: String::new(),
                                        });
                                    }
                                }
                            }
                        }
                        // Fallback: direct type_identifier child
                        if inner.kind() == "type_identifier" || inner.kind() == "identifier" {
                            results.push(ParsedRelation {
                                source_name: class_name.to_string(),
                                target_name: node_text(&inner, source).to_string(),
                                relation: REL_IMPLEMENTS.into(),
                                metadata: None,
                                source_language: String::new(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
