T = "src/parser/treesitter.rs"
P = "src/parser/relations/python.rs"
MUTANTS = {
 "M01 whole-def range (python guard never true)": (T, 'Some(body) if language == "python" => body,', 'Some(body) if language == "pythonX" => body,'),
 "M02 scope starts at def, ends at body end": (T, "return Some(LexicalScope::Within(range.start_byte(), range.end_byte()));", "return Some(LexicalScope::Within(n.start_byte(), range.end_byte()));"),
 "M03 body range for every language": (T, 'Some(body) if language == "python" => body,', 'Some(body) => body,'),
 "M04 rebound pass not called": (T, "        python_rebound_names_are_not_local(root, source, &mut nodes);\n", "        let _ = &mut nodes;\n"),
 "M05 only global (nonlocal id -> global)": (T, 'language.id_for_node_kind("nonlocal_statement", true),', 'language.id_for_node_kind("global_statement", true),'),
 "M06 only nonlocal (global id -> nonlocal)": (T, 'language.id_for_node_kind("global_statement", true),', 'language.id_for_node_kind("nonlocal_statement", true),'),
 "M07 contains negated": (T, "&& rebound.contains(node.name.as_str())", "&& !rebound.contains(node.name.as_str())"),
 "M08 only the first name of a statement": (T, "for i in 0..n.named_child_count() {", "for i in 0..n.named_child_count().min(1) {"),
 "M09 unscope File/any scope too": (T, "if matches!(node.lexical_scope, Some(LexicalScope::Within(..)))", "if node.lexical_scope.is_some()"),
 "M10 only the first global/nonlocal statement in the file": (T, "                    rebound.insert(node_text(&c, source));\n                }\n            }\n", "                    rebound.insert(node_text(&c, source));\n                }\n            }\n            break 'walk;\n"),
 "M11 annotation picks no scope": (P, '.then(|| crate::parser::treesitter::bare_call_at(node, "python"))', '.then(|| None::<crate::parser::treesitter::BareCall>)'),
 "M12 rebound only for classes": (T, "&& rebound.contains(node.name.as_str())", '&& node.node_type == "class" && rebound.contains(node.name.as_str())'),
 "M13 rebound only for functions": (T, "&& rebound.contains(node.name.as_str())", '&& node.node_type != "class" && rebound.contains(node.name.as_str())'),
 "M14 walk does not descend into function bodies": (T, "        if cursor.goto_first_child() {\n            continue;\n        }\n        while !cursor.goto_next_sibling() {\n            if !cursor.goto_parent() {\n                break 'walk;\n            }\n        }\n    }\n    if rebound.is_empty() {", "        if n.kind() != \"block\" && cursor.goto_first_child() {\n            continue;\n        }\n        while !cursor.goto_next_sibling() {\n            if !cursor.goto_parent() {\n                break 'walk;\n            }\n        }\n    }\n    if rebound.is_empty() {"),
}
