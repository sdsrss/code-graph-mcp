# (id, file, old, new, description)
P = "src/parser/relations/python.rs"
C = "src/parser/relations/calls.rs"
MUTANTS = [
 ("M01", P, "scopes[top].end <= scopes[i].start", "scopes[top].end < scopes[i].start", "PyScopes::new: pop a scope only when it ends strictly before the next starts"),
 ("M02", P, "while self.0[i].end <= at {", "while self.0[i].end < at {", "innermost: a scope ending exactly at the read still holds it"),
 ("M03", P, "let mut i = self.0.partition_point(|s| s.start <= at).checked_sub(1)?;", "let mut i = self.0.partition_point(|s| s.start < at).checked_sub(1)?;", "innermost: a scope starting exactly at the read does not hold it"),
 ("M04", P, "class_bound |= innermost && scope.names.contains_key(name);", "class_bound |= scope.names.contains_key(name);", "read: any enclosing class body counts, not only the innermost"),
 ("M05", P, "Some(PyBinding::Global) => break,", "Some(PyBinding::Global) => {}", "read: `global` keeps climbing instead of jumping to module"),
 ("M06", P, "            || self.renamed.contains_key(name)\n", "", "module read: a renamed import no longer keeps the name"),
 ("M07", P, "            || self.globals.contains(name)\n        {", "        {", "module read: a `global` name no longer keeps the name"),
 ("M08", P, "        \"identifier\" => right,\n        \"attribute\" => right.child_by_field_name(\"attribute\")?,", "        \"attribute\" => right.child_by_field_name(\"attribute\")?,", "module_alias: `X = X` no longer an alias"),
 ("M09", P, "    let spelled = match right.kind() {\n        \"identifier\" => right,\n        \"attribute\" => right.child_by_field_name(\"attribute\")?,", "    let spelled = match right.kind() {\n        \"identifier\" => right,", "module_alias: `X = m.X` no longer an alias"),
 ("M10", P, "                let (gap_start, gap_end) = first_iterable", "                let (gap_start, gap_end) = None::<tree_sitter::Node>", "comprehension: no gap for the first iterable"),
 ("M11", P, "                    scopes.push(scope(\n                        body.start_byte(),\n                        body.end_byte(),\n                        std::rc::Rc::new(params),", "                    scopes.push(scope(\n                        n.start_byte(),\n                        body.end_byte(),\n                        std::rc::Rc::new(params),", "lambda: scope covers its parameters/defaults too"),
 ("M12", P, "                    scopes.push(scope(\n                        body.start_byte(),\n                        body.end_byte(),\n                        function_bindings(n, source),", "                    scopes.push(scope(\n                        n.start_byte(),\n                        body.end_byte(),\n                        function_bindings(n, source),", "def/class: scope covers decorators-less header (params, defaults, annotations, bases)"),
 ("M13", P, "        (_, true) | (PyScopeRead::Definition, _) => false,", "        (PyScopeRead::Definition, _) => false,", "reference read: class_bound flag ignored"),
 ("M14", P, "        || (!is_attribute_tail(node) && py_read_names_no_definition(*node, name))", "        || py_read_names_no_definition(*node, name)", "type reference: attribute tail checked as a bare read"),
 ("M15", P, "        || py_read_names_no_definition(*node, name)\n    {\n        return None;\n    }\n    // A decorator", "    {\n        return None;\n    }\n    // A decorator", "value reference: new rule removed"),
 ("M16", P, "            !class_bound && module.read(name, defined) == PyModuleRead::Builtin", "            module.read(name, defined) == PyModuleRead::Builtin", "builtin_calls: class_bound ignored"),
 ("M17", P, "        } else if self.values.contains(name) {\n            PyModuleRead::Value\n        } else if", "        } else if", "module read: module values no longer Value (fall to Builtin/Maybe)"),
 ("M18", P, "    module.star |= node\n                .named_children(&mut cursor)\n                .any(|c| c.kind() == \"wildcard_import\");", "    module.star |= false;", "star import never detected"),
 ("M19", P, "        \"assignment\" if module_alias(node, source).is_some() => {", "        \"assignment\" if false && module_alias(node, source).is_some() => {", "module_alias arm disabled"),
 ("M20", P, "                Some(PyBinding::Nonlocal) | None => {}", "                Some(PyBinding::Nonlocal) => break,\n                    None => {}", "read: nonlocal jumps to module"),
 ("M21", P, "            || defined.free.contains(name)\n", "", "module read: a def/class anywhere in the file no longer keeps the name"),
 ("M22", P, "        id(\"class_definition\"),\n        id(\"lambda\"),", "        id(\"lambda\"),", "scope_scan: class bodies not collected"),
 ("M23", C, "    if super::python::python_builtin_call(node) {\n        return;\n    }", "", "calls.rs: builtin early return removed"),
 ("M24", P, "            if scope.class {\n                class_bound |= innermost && scope.names.contains_key(name);\n            } else {", "            if scope.class && false {\n                class_bound |= innermost && scope.names.contains_key(name);\n            } else {", "read: class body treated as a function scope"),
 ("M25", P, "        \"assignment\" | \"augmented_assignment\" | \"for_statement\" => targets(\"left\"),\n        \"named_expression\" => targets(\"name\"),\n        \"as_pattern\" => targets(\"alias\"),\n        _ => {}\n    }\n    let mut cursor = node.walk();\n    for c in node.named_children(&mut cursor) {\n        collect_module_bindings(", "        \"assignment\" | \"augmented_assignment\" | \"for_statement\" => targets(\"left\"),\n        \"named_expression\" => targets(\"name\"),\n        \"as_pattern\" => targets(\"alias\"),\n        \"if_statement\" | \"try_statement\" => return,\n        _ => {}\n    }\n    let mut cursor = node.walk();\n    for c in node.named_children(&mut cursor) {\n        collect_module_bindings(", "module bindings: do not descend into if/try (conditional imports/defs)"),
]
MUTANTS2 = [
 ("M04b", P, "class_bound |= innermost && scope.names.contains_key(name);", "class_bound |= (innermost || !innermost) && scope.names.contains_key(name);", "read: any enclosing class body counts, not only the innermost (compile-clean)"),
 ("M26", P, "        (PyScopeRead::Module, _) => PY_MODULE.with(|m| {\n            PY_DEFINED.with(|d| m.borrow().read(name, &d.borrow()) != PyModuleRead::MaybeDefinition)", "        (PyScopeRead::Module, _) => PY_MODULE.with(|m| {\n            PY_DEFINED.with(|d| m.borrow().read(name, &d.borrow()) == PyModuleRead::Builtin)", "reference read: module values no longer dropped"),
 ("M27", P, "                    Some(PyBinding::Value) => return (PyScopeRead::Value, class_bound),", "                    Some(PyBinding::Value) => {}", "read: a scope's value binding is skipped (climb on)"),
 ("M28", P, "        } else if crate::domain::PYTHON_BUILTINS.contains(&name) {", "        } else if crate::domain::PYTHON_BUILTINS.contains(&name) && name.len() > 4 {", "builtins: names of <= 4 chars no longer builtins"),
 ("M29", P, "            || self.star\n", "", "module read: star import ignored (only the read; detection intact)"),
]
MUTANTS2 += [
 ("M14b", P, "        || (!is_attribute_tail(node) && py_read_names_no_definition(*node, name))", "        || ((!is_attribute_tail(node) || true) && py_read_names_no_definition(*node, name))", "type reference: attribute tail checked as a bare read (compile-clean)"),
 ("M16b", P, "            !class_bound && module.read(name, defined) == PyModuleRead::Builtin", "            (!class_bound || true) && module.read(name, defined) == PyModuleRead::Builtin", "builtin_calls: class_bound ignored (compile-clean)"),
 ("M17b", P, "        } else if self.values.contains(name) {\n            PyModuleRead::Value", "        } else if self.values.contains(name) && false {\n            PyModuleRead::Value", "module read: module values no longer Value (compile-clean)"),
 ("M21b", P, "            || defined.free.contains(name)\n", "            || (defined.free.contains(name) && false)\n", "module read: a def/class anywhere in the file no longer keeps the name (compile-clean)"),
 ("M23b", C, "    if super::python::python_builtin_call(node) {\n        return;\n    }", "    if super::python::python_builtin_call(node) && false {\n        return;\n    }", "calls.rs: builtin early return disabled (compile-clean)"),
]
