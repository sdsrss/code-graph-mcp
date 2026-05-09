//! TypeScript/JavaScript export-statement extraction.
//! Captures `export function`, `export class`, `export interface`,
//! `export type`, `export enum`, `export abstract class`, and
//! `export const|let` declarations as REL_EXPORTS edges off `<module>`.

use super::ParsedRelation;
use super::super::node_text;
use crate::domain::REL_EXPORTS;

pub(super) fn extract_export_names(
    node: &tree_sitter::Node,
    source: &str,
    results: &mut Vec<ParsedRelation>,
) {
    // Walk direct children for exported declarations
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        match child.kind() {
            "function_declaration" | "class_declaration" | "interface_declaration"
            | "type_alias_declaration" | "enum_declaration" | "abstract_class_declaration" => {
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
