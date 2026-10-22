S = {}
GO = "from utils import run\n\ndef main():\n    run()\n"
S["c01_py_callee_added_later"] = [
    {"utils.py": "def other():\n    pass\n", "go.py": "def main():\n    run()\n"},
    {"utils.py": "def other():\n    pass\n\ndef run():\n    pass\n"},
]
S["c02_py_callee_file_added_later"] = [
    {"go.py": "def main():\n    run()\n"},
    {"utils.py": "def run():\n    pass\n"},
]
S["c03_rs_callee_added_later"] = [
    {"Cargo.toml": "[package]\nname = \"d\"\nversion = \"0.1.0\"\n", "src/lib.rs": "pub mod a;\npub mod b;\n",
     "src/a.rs": "pub fn other() {}\n", "src/b.rs": "pub fn main() {\n    run();\n}\n"},
    {"src/a.rs": "pub fn other() {}\npub fn run() {}\n"},
]
S["c04_twofile_q20"] = [
    {"scripts/utils.py": "def other():\n    pass\n", "tools/utils.py": "def other2():\n    pass\n", "scripts/go.py": "import utils\n\ndef main():\n    utils.run()\n"},
    {"scripts/utils.py": "def run():\n    pass\n", "tools/utils.py": "def other2():\n    pass\n\nY = 2\n"},
    {"scripts/utils.py": "def other():\n    pass\n"},
]
SCENARIOS = S
