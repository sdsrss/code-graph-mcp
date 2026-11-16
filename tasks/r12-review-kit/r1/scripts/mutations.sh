#!/bin/bash
M=/var/tmp/agent-tmp-r12-review/scripts/mutate.py
F="go js heritage extends receiver member pending incremental rebuild inherit"
run() { echo "### $1"; python3 -I $M "$2" "$3" "$4" $F 2>&1 | tail -3; }
run "M1 chain on receiver typed" src/parser/relations/go.rs ' && root.id() == operand.id() =>' ' =>'
run "M2 = targets not values" src/parser/relations/go.rs 'decl.declared.contains(name) || decl.assigned.contains(name)' 'decl.declared.contains(name)'
run "M3 receive_statement not a decl" src/parser/relations/go.rs '"short_var_declaration" | "range_clause" | "receive_statement" =>' '"short_var_declaration" | "range_clause" =>'
run "M5 func literal params not decls" src/parser/relations/go.rs '        "func_literal" => {
            for field in ["parameters", "result"] {' '        "func_literal_x" => {
            for field in ["parameters", "result"] {'
run "M6 Go dispatches to overrides" src/indexer/pipeline/resolve.rs '    language != "go"' '    true'
run "M7 rtype fallback may bind a type" src/indexer/pipeline/resolve.rs 'Some(CalleeMeta::Member | CalleeMeta::RecvType(_))' 'Some(CalleeMeta::Member)'
run "M8 same-file tier keeps types" src/indexer/pipeline/index_files.rs '                        .filter(|id| !never.contains(id))' '                        .filter(|_id| true)'
run "M11 GO_DECLS not cleared per file" src/parser/relations/go.rs '        decls.clear();' '        let _ = 0;'
run "M12 no buffering on an all-type answer" src/indexer/pipeline/resolve.rs '            read_elsewhere: !ids.is_empty(),' '            read_elsewhere: false,'
run "M14 shadowed receiver still typed" src/parser/relations/go.rs 'Some((_, ty)) if !decl.declared.contains(name) && root.id()' 'Some((_, ty)) if root.id()'
run "M15 params not decls" src/parser/relations/go.rs '        for field in ["parameters", "result"] {
            if let Some(p) = decl.child_by_field_name(field) {' '        for field in ["result"] {
            if let Some(p) = decl.child_by_field_name(field) {'
run "M18 only first heritage stamped" src/parser/relations/inherits.rs '    for rel in &mut results[first..] {' '    for rel in results[first..].iter_mut().take(1) {'
run "M21 = target redeclares" src/parser/relations/go.rs '                bare(left, assigned);' '                bare(left, declared);'
run "M24 batch callable set: functions only" src/indexer/pipeline/index_files.rs '.filter(|(_, t)| matches!(t.as_str(), "function" | "method"))' '.filter(|(_, t)| matches!(t.as_str(), "function"))'
run "M19 walk answer also falls through to by-name" src/indexer/pipeline/index_files.rs '                            .filter(|id| !callables.contains(id))
                            .collect();
                        if !targets.is_empty() {
                            edges_created += insert_relation_edges(
                                db,
                                &mut written,
                                &source_ids,
                                &targets,
                                &d.relation,
                                d.metadata.as_deref(),
                                false,
                            )?;
                            continue;' '                            .filter(|id| !callables.contains(id))
                            .collect();
                        if !targets.is_empty() {
                            edges_created += insert_relation_edges(
                                db,
                                &mut written,
                                &source_ids,
                                &targets,
                                &d.relation,
                                d.metadata.as_deref(),
                                false,
                            )?;'
run "M10 walk keeps callables" src/indexer/pipeline/index_files.rs '                            .filter(|id| !callables.contains(id))
                            .collect();
                        if !targets.is_empty() {' '                            .collect();
                        if !targets.is_empty() {'
