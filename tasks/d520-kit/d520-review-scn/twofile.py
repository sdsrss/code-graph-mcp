# A module path naming two files (root helpers.py and tests/helpers.py); one gains/loses the attribute.
S = {}
T0 = {"tests/helpers.py": "def h():\n    pass\n", "helpers.py": "def other():\n    pass\n", "z.py": "def h():\n    pass\n"}
S["t01_attr_call_second_file_gains"] = [
    dict(T0, **{"tests/test_a.py": "import helpers\n\ndef test_x():\n    helpers.h()\n"}),
    {"helpers.py": "def h():\n    pass\n"},
    {"helpers.py": "def other():\n    pass\n"},
]
S["t02_from_import_second_file_gains"] = [
    dict(T0, **{"tests/test_a.py": "from helpers import h\n\ndef test_x():\n    h()\n"}),
    {"helpers.py": "def h():\n    pass\n"},
    {"helpers.py": "def other():\n    pass\n"},
]
S["t03_attr_call_unique_name_second_file_gains"] = [
    {"tests/helpers.py": "def hq():\n    pass\n", "helpers.py": "def other():\n    pass\n",
     "tests/test_a.py": "import helpers\n\ndef test_x():\n    helpers.hq()\n"},
    {"helpers.py": "def hq():\n    pass\n"},
    {"helpers.py": "def other():\n    pass\n"},
]
S["t04_from_import_unique_name_second_file_gains"] = [
    {"tests/helpers.py": "def hq():\n    pass\n", "helpers.py": "def other():\n    pass\n",
     "tests/test_a.py": "from helpers import hq\n\ndef test_x():\n    hq()\n"},
    {"helpers.py": "def hq():\n    pass\n"},
    {"helpers.py": "def other():\n    pass\n"},
]
# a module path naming pkg.py and pkg/__init__.py at once
S["t05_attr_call_py_and_init"] = [
    {"pkg.py": "def other():\n    pass\n", "pkg/__init__.py": "def X():\n    pass\n", "z.py": "def X():\n    pass\n",
     "app.py": "import pkg\n\ndef f():\n    pkg.X()\n"},
    {"pkg.py": "def X():\n    pass\n"},
    {"pkg.py": "def other():\n    pass\n"},
]
S["t06_from_import_py_and_init"] = [
    {"pkg.py": "def other():\n    pass\n", "pkg/__init__.py": "def X():\n    pass\n", "z.py": "def X():\n    pass\n",
     "app.py": "from pkg import X\n\ndef f():\n    X()\n"},
    {"pkg.py": "def X():\n    pass\n"},
    {"pkg.py": "def other():\n    pass\n"},
]
SCENARIOS = S
