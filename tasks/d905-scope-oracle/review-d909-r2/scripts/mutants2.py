T = "src/parser/treesitter.rs"
MUTANTS = {
 "M04b rebound pass not called (if false)": (T, "        python_rebound_names_are_not_local(root, source, &mut nodes);\n", "        if false {\n            python_rebound_names_are_not_local(root, source, &mut nodes);\n        }\n"),
}
