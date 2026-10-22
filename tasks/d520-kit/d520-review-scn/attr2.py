S = {}
S["d01_no_X_anywhere_then_gain"] = [
    {"m.py": "def other():\n    pass\n", "app.py": "import m\n\ndef f():\n    return m.X()\n"},
    {"m.py": "def other():\n    pass\n\ndef X():\n    pass\n"},
    {"q.py": "def X():\n    pass\n"},
    {"m.py": "def other():\n    pass\n"},
    {"q.py": None},
]
S["d02_scripts_two_roots_utils"] = [
    {"scripts/utils.py": "def run():\n    pass\n", "tools/utils.py": "def other():\n    pass\n", "scripts/go.py": "import utils\n\ndef main():\n    utils.run()\n"},
    {"tools/utils.py": "def run():\n    pass\n"},
    {"tools/utils.py": "def other():\n    pass\n"},
    {"scripts/utils.py": "def other2():\n    pass\n"},
    {"scripts/utils.py": "def run():\n    pass\n"},
]
S["d03_two_roots_one_deleted"] = [
    {"scripts/utils.py": "def run():\n    pass\n", "tools/utils.py": "def run():\n    pass\n", "scripts/go.py": "import utils\n\ndef main():\n    utils.run()\n"},
    {"tools/utils.py": None},
    {"tools/utils.py": "def run():\n    pass\n"},
]
S["d04_two_roots_reexport"] = [
    {"scripts/utils.py": "from impl import run\n", "scripts/impl.py": "def run():\n    pass\n", "tools/utils.py": "def other():\n    pass\n", "scripts/go.py": "import utils\n\ndef main():\n    utils.run()\n"},
    {"tools/utils.py": "def run():\n    pass\n"},
    {"tools/utils.py": "from impl2 import run\n", "tools/impl2.py": "def run():\n    pass\n"},
]
S["d05_caller_bare_and_attr_bare_unresolved_then_decoy"] = [
    {"m.py": "def X():\n    pass\n", "app.py": "import m\n\ndef f():\n    m.X()\n    X()\n"},
    {"q.py": "def X():\n    pass\n"},
    {"q.py": None},
    {"m.py": "def Y():\n    pass\n"},
    {"m.py": "def X():\n    pass\n"},
]
SCENARIOS = S
