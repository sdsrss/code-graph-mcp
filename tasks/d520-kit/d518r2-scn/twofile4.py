# Re-exports in the second module file, relative two-file paths, and TS rows.
S = {}
H = "def h():\n    pass\n"
O = "def other():\n    pass\n"
for tag, imp in (("attr", "import helpers\n\ndef test_x():\n    helpers.h()\n"),
                 ("from", "from helpers import h\n\ndef test_x():\n    h()\n")):
    S[f"u01_{tag}_second_gains_reexport_first_has_node"] = [
        {"tests/helpers.py": H, "helpers.py": O, "lib/__init__.py": "", "lib/impl.py": H, "tests/test_a.py": imp},
        {"helpers.py": "from lib.impl import h\n"},
        {"helpers.py": O},
    ]
    S[f"u02_{tag}_second_gains_reexport_first_lacks"] = [
        {"tests/helpers.py": O, "helpers.py": O, "lib/__init__.py": "", "lib/impl.py": H, "tests/test_a.py": imp},
        {"helpers.py": "from lib.impl import h\n"},
        {"helpers.py": O},
    ]
# relative: pkg/helpers.py and pkg/helpers/__init__.py both named by `from .helpers import h`
S["u03_relative_two_files_second_gains"] = [
    {"pkg/__init__.py": "", "pkg/helpers.py": H, "pkg/helpers/__init__.py": O, "z.py": H,
     "pkg/app.py": "from .helpers import h\n\ndef f():\n    h()\n"},
    {"pkg/helpers/__init__.py": H},
    {"pkg/helpers/__init__.py": O},
]
S["u04_relative_two_files_neither_then_one_gains"] = [
    {"pkg/__init__.py": "", "pkg/helpers.py": O, "pkg/helpers/__init__.py": "def o2():\n    pass\n",
     "pkg/app.py": "from .helpers import h\n\ndef f():\n    h()\n"},
    {"pkg/helpers.py": H},
    {"pkg/helpers.py": O},
]
# TypeScript: `./helpers` names one file; a gain there rebinds as before
S["u05_ts_import_gains_in_named_module"] = [
    {"src/helpers.ts": "export function other() {}\n", "src/z.ts": "export function h() {}\n",
     "src/app.ts": "import { h } from './helpers';\nexport function main() { h(); }\n"},
    {"src/helpers.ts": "export function h() {}\n"},
    {"src/helpers.ts": "export function other() {}\n"},
]
S["u06_ts_index_dir"] = [
    {"src/helpers/index.ts": "export function other() {}\n", "src/z.ts": "export function h() {}\n",
     "src/app.ts": "import { h } from './helpers';\nexport function main() { h(); }\n"},
    {"src/helpers/index.ts": "export function h() {}\n"},
    {"src/helpers/index.ts": "export function other() {}\n"},
]
SCENARIOS = S
