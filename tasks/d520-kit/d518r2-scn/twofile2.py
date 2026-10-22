# Second-round review of D#518's fixes: module paths naming two or three files.
S = {}
ATTR = "import helpers\n\ndef test_x():\n    helpers.h()\n"
FROM = "from helpers import h\n\ndef test_x():\n    h()\n"
H = "def h():\n    pass\n"
O = "def other():\n    pass\n"
O2 = "def other2():\n    pass\n"
for tag, imp in (("attr", ATTR), ("from", FROM)):
    # the first file (the one the importer binds) loses and regains the name
    S[f"q01_{tag}_first_loses_second_has"] = [
        {"tests/helpers.py": H, "helpers.py": H, "z.py": H, "tests/test_a.py": imp},
        {"tests/helpers.py": O},
        {"tests/helpers.py": H},
    ]
    # the second file is created with the name, then deleted
    S[f"q02_{tag}_second_created_then_deleted"] = [
        {"tests/helpers.py": H, "z.py": H, "tests/test_a.py": imp},
        {"helpers.py": H},
        {"helpers.py": None},
    ]
    # the second file gains the name while a third file loses it: the run's count is flat
    S[f"q03_{tag}_gain_with_count_cancelled_elsewhere"] = [
        {"tests/helpers.py": H, "helpers.py": O, "z.py": H, "tests/test_a.py": imp},
        {"helpers.py": H, "z.py": O2},
        {"helpers.py": O, "z.py": H},
    ]
    # the two module files swap the name in one run
    S[f"q04_{tag}_swap"] = [
        {"tests/helpers.py": H, "helpers.py": O, "z.py": H, "tests/test_a.py": imp},
        {"tests/helpers.py": O, "helpers.py": H},
        {"tests/helpers.py": H, "helpers.py": O},
    ]
    # both gain the name in one run
    S[f"q05_{tag}_both_gain"] = [
        {"tests/helpers.py": O, "helpers.py": O2, "z.py": H, "tests/test_a.py": imp},
        {"tests/helpers.py": H, "helpers.py": H},
        {"tests/helpers.py": O, "helpers.py": O2},
    ]
    # second gains while the first is also edited (keeps the name)
    S[f"q06_{tag}_second_gains_first_edited"] = [
        {"tests/helpers.py": H, "helpers.py": O, "z.py": H, "tests/test_a.py": imp},
        {"tests/helpers.py": H + "\ndef more():\n    pass\n", "helpers.py": H},
        {"helpers.py": O},
    ]
    # the first file is deleted while the second defines the name
    S[f"q07_{tag}_first_deleted"] = [
        {"tests/helpers.py": H, "helpers.py": H, "z.py": H, "tests/test_a.py": imp},
        {"tests/helpers.py": None},
        {"tests/helpers.py": H},
    ]
    # second gains the name, no decoy anywhere (unique name)
    S[f"q08_{tag}_second_gains_no_decoy_then_first_loses"] = [
        {"tests/helpers.py": H, "helpers.py": O, "tests/test_a.py": imp},
        {"helpers.py": H},
        {"tests/helpers.py": O},
        {"helpers.py": O},
    ]

PKG_ATTR = "import pkg\n\ndef f():\n    pkg.h()\n"
PKG_FROM = "from pkg import h\n\ndef f():\n    h()\n"
for tag, imp in (("attr", PKG_ATTR), ("from", PKG_FROM)):
    # pkg/__init__.py re-exports h from .core; pkg.py gains h
    S[f"q10_{tag}_reexport_init_and_py_gains"] = [
        {"pkg/__init__.py": "from .core import h\n", "pkg/core.py": H, "pkg.py": O, "z.py": H, "app.py": imp},
        {"pkg.py": H},
        {"pkg.py": O},
    ]
    # pkg.py gains h while pkg/__init__.py is edited (keeps h)
    S[f"q11_{tag}_py_gains_init_edited"] = [
        {"pkg/__init__.py": H, "pkg.py": O, "z.py": H, "app.py": imp},
        {"pkg/__init__.py": H + "\nX = 1\n", "pkg.py": H},
        {"pkg.py": O},
    ]

# The synthetic import's `<module>` fallback across several module files.
GO = "import utils\n\ndef main():\n    utils.run()\n"
RUN = "def run():\n    pass\n"
S["q20_fallback_gainer_sorts_first_other_edited"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": RUN, "tools/utils.py": O2 + "\nY = 2\n"},
    {"scripts/utils.py": O},
]
S["q21_fallback_gainer_sorts_last_other_edited"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": O + "\nY = 2\n", "tools/utils.py": RUN},
    {"tools/utils.py": O2},
]
S["q22_fallback_three_files_middle_gains"] = [
    {"a/utils.py": O, "b/utils.py": O2, "c/utils.py": "def o3():\n    pass\n", "a/go.py": GO},
    {"b/utils.py": RUN},
    {"b/utils.py": O2},
]
S["q23_fallback_three_files_two_edited_first_gains"] = [
    {"a/utils.py": O, "b/utils.py": O2, "c/utils.py": "def o3():\n    pass\n", "c/go.py": GO},
    {"a/utils.py": RUN, "b/utils.py": O2 + "\nY = 2\n"},
    {"a/utils.py": O},
]
S["q24_fallback_root_and_dir"] = [
    {"utils.py": O, "scripts/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": RUN, "utils.py": O + "\nY = 2\n"},
    {"scripts/utils.py": O2},
]
S["q25_fallback_gainer_first_other_file_deleted"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": RUN, "tools/utils.py": None},
    {"tools/utils.py": O2},
]
S["q26_fallback_both_gain"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": RUN, "tools/utils.py": RUN},
    {"scripts/utils.py": O},
    {"tools/utils.py": O2},
]
S["q27_fallback_gain_with_bare_caller"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO, "zz.py": "def run():\n    pass\n"},
    {"tools/utils.py": RUN},
    {"tools/utils.py": O2},
]

# JS rows of the same fan-out query (single-file module): sanity
S["q30_js_import_gains_in_named_module"] = [
    {"src/helpers.js": "export function other() {}\n", "src/z.js": "export function h() {}\n",
     "src/app.js": "import { h } from './helpers';\nexport function main() { h(); }\n"},
    {"src/helpers.js": "export function h() {}\n"},
    {"src/helpers.js": "export function other() {}\n"},
]
SCENARIOS = S
