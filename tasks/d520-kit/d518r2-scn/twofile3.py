# Probes of the restore-path DELETE (e699bdec) and of the fallback ordering.
S = {}
GO = "import utils\n\ndef main():\n    utils.run()\n"
RUN = "def run():\n    pass\n"
O = "def other():\n    pass\n"
O2 = "def other2():\n    pass\n"
S["r01_gains_run_as_method_only"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": "class K:\n    def run(self):\n        pass\n"},
    {"tools/utils.py": RUN},
    {"scripts/utils.py": O},
]
S["r02_gains_run_nested_only"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": "def outer():\n    def run():\n        pass\n    return run\n"},
    {"scripts/utils.py": O},
]
S["r03_alias_import"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": "import utils as u\n\ndef main():\n    u.run()\n"},
    {"tools/utils.py": RUN},
    {"tools/utils.py": O2},
]
S["r04_also_from_import_same_module"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2,
     "scripts/go.py": "import utils\nfrom utils import other\n\ndef main():\n    utils.run()\n    other()\n"},
    {"tools/utils.py": RUN},
    {"scripts/utils.py": O + RUN},
    {"tools/utils.py": O2},
]
S["r05_two_attrs_one_gains"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2,
     "scripts/go.py": "import utils\n\ndef main():\n    utils.run()\n    utils.stop()\n"},
    {"tools/utils.py": RUN},
    {"scripts/utils.py": "def stop():\n    pass\n"},
    {"tools/utils.py": O2, "scripts/utils.py": O},
]
S["r06_pkg_py_and_init_fallback_py_gains_init_edited"] = [
    {"a/b.py": O, "a/b/__init__.py": O2, "a/__init__.py": "", "app.py": "import a.b as m\n\ndef f():\n    m.run()\n"},
    {"a/b.py": RUN, "a/b/__init__.py": O2 + "\nY = 1\n"},
    {"a/b.py": O},
]
S["r07_pkg_py_and_init_fallback_init_gains_py_edited"] = [
    {"a/b.py": O, "a/b/__init__.py": O2, "a/__init__.py": "", "app.py": "import a.b as m\n\ndef f():\n    m.run()\n"},
    {"a/b.py": O + "\nY = 1\n", "a/b/__init__.py": RUN},
    {"a/b/__init__.py": O2},
]
# importer in the same run as both module files
S["r08_importer_and_both_modules_in_run"] = [
    {"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO},
    {"scripts/utils.py": RUN, "tools/utils.py": O2 + "\nY = 2\n", "scripts/go.py": GO + "\n# touched\n"},
]
# big runs: the gaining module file and the other one land in different batches
FILL = {f"sz/f{i:03d}.py": f"def g{i}():\n    pass\n" for i in range(560)}
FILL2 = {f"sz/f{i:03d}.py": f"def g{i}():\n    return 1\n" for i in range(560)}
S["r09_big_run_gainer_early_other_late"] = [
    dict(FILL, **{"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO}),
    dict(FILL2, **{"scripts/utils.py": RUN, "tools/utils.py": O2 + "\nY = 2\n"}),
]
S["r10_big_run_gainer_late_other_early"] = [
    dict(FILL, **{"scripts/utils.py": O, "tools/utils.py": O2, "scripts/go.py": GO}),
    dict(FILL2, **{"scripts/utils.py": O + "\nY = 2\n", "tools/utils.py": RUN}),
]
# a from-import bound to the first file while the run moves `h` into the second
# from a third file (count flat), and the same with the attribute form
H = "def h():\n    pass\n"
for tag, imp in (("attr", "import helpers\n\ndef test_x():\n    helpers.h()\n"),
                 ("from", "from helpers import h\n\ndef test_x():\n    h()\n")):
    S[f"r11_{tag}_move_h_from_lib_into_second"] = [
        {"tests/helpers.py": H, "helpers.py": O, "lib/z.py": H + O2, "tests/test_a.py": imp},
        {"helpers.py": O + H, "lib/z.py": O2},
    ]
    # control: the third file loses another name, not `h`
    S[f"r12_{tag}_control_third_loses_other_name"] = [
        {"tests/helpers.py": H, "helpers.py": O, "lib/z.py": H + O2, "tests/test_a.py": imp},
        {"helpers.py": O + H, "lib/z.py": H},
    ]
SCENARIOS = S
