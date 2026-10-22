S = {}
LIB = "def run():\n    pass\n\nclass C:\n    def run(self):\n        pass\n"
LIB2 = "def walk():\n    pass\n\nclass C:\n    def walk(self):\n        pass\n"
GO = "def f(o):\n    o.run()\n    run()\n"
S["m01_py_member_and_bare_same_name_edit"] = [{"lib.py": LIB, "go.py": GO}, {"lib.py": LIB2}, {"lib.py": LIB}]
S["m02_py_member_and_bare_same_name_delete"] = [{"lib.py": LIB, "go.py": GO}, {"lib.py": None}, {"lib.py": LIB}]
S["m03_py_member_and_bare_added_later"] = [{"lib.py": LIB2, "go.py": GO}, {"lib.py": LIB}]
CT = "[package]\nname = \"d\"\nversion = \"0.1.0\"\n"
RLIB = "pub fn run() {}\npub struct C;\nimpl C {\n    pub fn run(&self) {}\n}\n"
RLIB2 = "pub fn walk() {}\npub struct C;\nimpl C {\n    pub fn walk(&self) {}\n}\n"
RGO = "use crate::a::C;\npub fn f(o: &C) {\n    o.run();\n    crate::a::run();\n    run();\n}\n"
S["m04_rs_three_shapes_edit"] = [{"Cargo.toml": CT, "src/lib.rs": "pub mod a;\npub mod b;\n", "src/a.rs": RLIB, "src/b.rs": RGO}, {"src/a.rs": RLIB2}, {"src/a.rs": RLIB}]
JLIB = "export function run() {}\nexport class C { run() {} }\n"
JLIB2 = "export function walk() {}\nexport class C { walk() {} }\n"
JGO = "import { run as go } from './lib';\nexport function f(o) {\n  o.run();\n  go();\n  run();\n}\n"
S["m05_js_shapes_edit"] = [{"lib.js": JLIB, "go.js": JGO}, {"lib.js": JLIB2}, {"lib.js": JLIB}]
SCENARIOS = S
