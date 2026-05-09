//! Class inheritance and interface implementation extraction across the
//! tier-1 + tier-2 languages we parse: TS/JS, Python, Java, Ruby, Kotlin,
//! Swift, PHP. Inheritance shapes vary per grammar (`extends_clause`,
//! `argument_list`, `superclass`, `delegation_specifiers`, `base_clause`,
//! `inheritance_specifier`), so each is matched explicitly.

use super::ParsedRelation;
use super::super::node_text;
use crate::domain::REL_IMPLEMENTS;

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
                                    if type_node.kind() == "identifier" || type_node.kind() == "type_identifier" {
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
                for k in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(k) {
                        if inner.kind() == "type_identifier" || inner.kind() == "identifier"
                            || inner.kind() == "dotted_name" || inner.kind() == "constant" || inner.kind() == "scope_resolution" {
                            parents.push(node_text(&inner, source).to_string());
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
                        inherits_from.named_child(0)
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
                                    if type_node.kind() == "type_identifier" || type_node.kind() == "identifier" {
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
                                            if name_node.kind() == "type_identifier" || name_node.kind() == "identifier" {
                                                results.push(ParsedRelation {
                                                    source_name: class_name.to_string(),
                                                    target_name: node_text(&name_node, source).to_string(),
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
            // Java: super_interfaces -> type_list -> type_identifier
            "super_interfaces" | "interfaces" => {
                for j in 0..child.named_child_count() {
                    if let Some(inner) = child.named_child(j) {
                        if inner.kind() == "type_list" {
                            for k in 0..inner.named_child_count() {
                                if let Some(type_node) = inner.named_child(k) {
                                    if type_node.kind() == "type_identifier" || type_node.kind() == "identifier" {
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
