# D#520 LOW 5/6 and neighbours: module paths moved by new modules and __init__.py changes.
S = {}
S["r01_new_root_helpers"] = [
    {"tests/helpers.py": "def h():\n    pass\n", "tests/test_a.py": "import helpers\n\ndef test_x():\n    helpers.h()\n"},
    {"helpers.py": "def h():\n    pass\n"},
    {"helpers.py": None},
    {"helpers.py": "def other():\n    pass\n"},
]
S["r02_new_root_helpers_from_import"] = [
    {"tests/helpers.py": "def h():\n    pass\n", "tests/test_a.py": "from helpers import h\n\ndef test_x():\n    h()\n"},
    {"helpers.py": "def h():\n    pass\n"},
    {"helpers.py": None},
]
S["r03_rel_parent_init_toggle"] = [
    {"pkg/__init__.py": "def X():\n    pass\n", "pkg/sub/__init__.py": "", "pkg/sub/m.py": "from .. import X\n\ndef f():\n    X()\n", "other.py": "def X():\n    pass\n"},
    {"pkg/__init__.py": None},
    {"pkg/__init__.py": "def X():\n    pass\n"},
    {"pkg/sub/__init__.py": None},
    {"pkg/sub/__init__.py": ""},
]
S["r04_rel_dot_import_submodule_init_toggle"] = [
    {"task_app/__init__.py": "", "task_app/tasks.py": "def add():\n    pass\n", "task_app/views.py": "from . import tasks\n\ndef v():\n    tasks.add()\n"},
    {"task_app/__init__.py": None},
    {"task_app/__init__.py": ""},
]
S["r05_rel_sibling_package_init_toggle"] = [
    {"pkg/__init__.py": "", "pkg/a/__init__.py": "", "pkg/a/m.py": "from ..b import X\n\ndef f():\n    X()\n",
     "pkg/b/__init__.py": "def X():\n    pass\n", "z.py": "def X():\n    pass\n"},
    {"pkg/b/__init__.py": None, "pkg/b/impl.py": "def X():\n    pass\n"},
    {"pkg/b/__init__.py": "from .impl import X\n"},
    {"pkg/b/__init__.py": None},
]
S["r06_new_module_in_package_relative"] = [
    {"pkg/__init__.py": "", "pkg/views.py": "from .tasks import add\n\ndef v():\n    add()\n", "z.py": "def add():\n    pass\n"},
    {"pkg/tasks.py": "def add():\n    pass\n"},
    {"pkg/tasks.py": None},
    {"pkg/tasks.py": "def add():\n    pass\n"},
]
S["r07_new_dir_root"] = [
    {"app.py": "import util\n\ndef f():\n    util.go()\n", "z.py": "def go():\n    pass\n"},
    {"lib/util.py": "def go():\n    pass\n"},
    {"lib/__init__.py": ""},
    {"lib/__init__.py": None},
    {"lib/util.py": None},
]
S["r08_new_module_joins_and_attr_call"] = [
    {"tests/helpers.py": "def h():\n    pass\n", "tests/test_a.py": "import helpers\n\ndef test_x():\n    helpers.h()\n", "z.py": "def h():\n    pass\n"},
    {"helpers.py": "def other():\n    pass\n"},
    {"helpers.py": "def h():\n    pass\n"},
    {"tests/__init__.py": ""},
    {"tests/__init__.py": None},
    {"helpers.py": None},
]
S["r09_rename_module_same_path"] = [
    {"src/m.py": "def X():\n    pass\n", "app.py": "import m\n\ndef f():\n    m.X()\n", "z.py": "def X():\n    pass\n"},
    {"src/m.py": None, "lib/m.py": "def X():\n    pass\n"},
    {"src/__init__.py": "", "lib/m.py": None, "src/m.py": "def X():\n    pass\n"},
]
S["r10_init_appears_under_new_module_run"] = [
    {"a/b/c.py": "def X():\n    pass\n", "app.py": "import b.c\nfrom b.c import X\n\ndef f():\n    X()\n", "z.py": "def X():\n    pass\n"},
    {"a/__init__.py": "", "a/b/__init__.py": "", "a/b/d.py": "def Y():\n    pass\n"},
    {"a/__init__.py": None},
    {"a/b/__init__.py": None, "a/b/c.py": None},
]
S["r11_relative_star_and_dots"] = [
    {"p/__init__.py": "", "p/q/__init__.py": "", "p/q/r/__init__.py": "", "p/q/r/m.py": "from ... import X\nfrom ...q import Y\n\ndef f():\n    X()\n    Y()\n",
     "z.py": "def X():\n    pass\n\ndef Y():\n    pass\n"},
    {"p/__init__.py": "def X():\n    pass\n"},
    {"p/q/__init__.py": "def Y():\n    pass\n"},
    {"p/__init__.py": None},
    {"p/q/__init__.py": None},
    {"p/__init__.py": "def X():\n    pass\n", "p/q/__init__.py": "def Y():\n    pass\n"},
]
S["r12_toplevel_init"] = [
    {"m.py": "from . import X\n\ndef f():\n    X()\n", "z.py": "def X():\n    pass\n"},
    {"__init__.py": "def X():\n    pass\n"},
    {"__init__.py": None},
]
SCENARIOS = S
