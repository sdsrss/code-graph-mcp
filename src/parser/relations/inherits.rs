//! Class inheritance and interface implementation extraction across the
//! tier-1 + tier-2 languages we parse: TS/JS, Python, Java, Ruby, Kotlin,
//! Swift, PHP. Inheritance shapes vary per grammar (`extends_clause`,
//! `argument_list`, `superclass`, `delegation_specifiers`, `base_clause`,
//! `inheritance_specifier`), so each is matched explicitly.

use super::super::node_text;
use super::ParsedRelation;
use crate::domain::REL_IMPLEMENTS;

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

pub(super) fn is_heritage_decl(kind: &str) -> bool {
    HERITAGE_DECL_KINDS.contains(&kind)
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
