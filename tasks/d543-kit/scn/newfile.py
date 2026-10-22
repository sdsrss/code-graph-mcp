S = {}
CT = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n"
B = "pub fn caller(p: crate::a::Phase) {\n    crate::a::run_full();\n}\n"
A = "pub struct Phase;\npub fn run_full() {}\n"
S["n01_rs_new_file_named_by_inline_path_and_type"] = [
    {"Cargo.toml": CT, "src/lib.rs": "pub mod a;\npub mod b;\n", "src/b.rs": B},
    {"src/a.rs": A},
]
S["n02_rs_new_file_named_by_extern_crate_path"] = [
    {"Cargo.toml": CT, "src/lib.rs": "pub mod a;\npub mod z;\n", "src/z.rs": "pub fn run_full() {}\n", "tests/t.rs": "fn t() {\n    demo::a::run_full();\n}\n"},
    {"src/a.rs": A},
]
S["n03_rs_delete_then_restore"] = [
    {"Cargo.toml": CT, "src/lib.rs": "pub mod a;\npub mod b;\n", "src/b.rs": B, "src/a.rs": A},
    {"src/a.rs": None},
    {"src/a.rs": A},
]
SCENARIOS = S
S["n04_rs_new_file_then_caller_edited"] = [
    {"Cargo.toml": CT, "src/lib.rs": "pub mod a;\npub mod b;\n", "src/b.rs": B},
    {"src/a.rs": A},
    {"src/b.rs": B + "\n"},
]
S["n05_rs_caller_adds_a_call"] = [
    {"Cargo.toml": CT, "src/lib.rs": "pub mod a;\npub mod b;\n", "src/a.rs": A, "src/b.rs": "pub fn caller() {}\n"},
    {"src/b.rs": "pub fn caller() {\n    crate::a::run_full();\n}\n"},
]
S["n06_py_caller_adds_a_call"] = [
    {"a.py": "def run():\n    pass\n", "b.py": "def main():\n    pass\n"},
    {"b.py": "from a import run\n\ndef main():\n    run()\n"},
]
