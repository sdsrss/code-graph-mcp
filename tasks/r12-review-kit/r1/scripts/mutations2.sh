#!/bin/bash
M=/var/tmp/agent-tmp-r12-review/scripts/mutate.py
F="go js heritage extends receiver member pending incremental rebuild inherit"
run() { echo "### $1"; python3 -I $M "$2" "$3" "$4" $F 2>&1 | tail -3; }
run "M2' = targets not values" src/parser/relations/go.rs 'decl.declared.contains(name) || decl.assigned.contains(name)' 'decl.declared.contains(name) || (false && decl.assigned.contains(name))'
run "M6' Go dispatches to overrides" src/indexer/pipeline/resolve.rs '    language != "go"' '    { let _ = language; true }'
run "M10' walk keeps callables" src/indexer/pipeline/index_files.rs '                            .filter(|id| !callables.contains(id))' '                            .filter(|id| { let _ = (callables, id); true })'
run "M26 pending sweep keeps types" src/indexer/pipeline/resolve.rs '        let never = if row.source_language == "go" {' '        let never = if row.source_language == "go_x" {'
run "M27 deferred pass keeps types" src/indexer/pipeline/index_files.rs '            let never = if d.language == "go" {' '            let never = if d.language == "go_x" {'
run "M29 heritage ignores a nearer declaration" src/parser/relations/member.rs '            break scope;
        }
        if binding_in(scope, child, name, source).is_some() {
            return None;
        }
        child = scope;
    };' '            break scope;
        }
        child = scope;
    };'
run "M30 heritage follows package specifiers" src/parser/relations/member.rs '    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let mut child = class;' '    if false {
        return None;
    }
    let mut child = class;'
run "M32 type switch alias not a decl" src/parser/relations/go.rs '        "type_switch_statement" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                collect_go_idents(&alias, source, declared, 0);' '        "type_switch_statement" => {
            if let Some(alias) = node.child_by_field_name("alias_x") {
                collect_go_idents(&alias, source, declared, 0);'
