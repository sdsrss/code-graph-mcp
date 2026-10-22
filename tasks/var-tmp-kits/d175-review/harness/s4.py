SCENARIOS = {}
SCENARIOS["new_reexport_lone_sentinel"] = [
    {"pkg/__init__.py": "", "user.py": "from pkg import Nope\n\ndef go():\n    Nope()\n"},
    {"pkg/core.py": "def Nope():\n    pass\n", "pkg/__init__.py": "from .core import Nope\n"},
]
SCENARIOS["new_reexport_lone_sentinel2"] = [
    {"pkg/__init__.py": "", "pkg/core.py": "def Other():\n    pass\n", "user.py": "from pkg import Nope\n\ndef go():\n    Nope()\n"},
    {"pkg/core.py": "def Nope():\n    pass\n"},
    {"pkg/__init__.py": "from .core import Nope\n"},
]
SCENARIOS["two_candidates_namechain"] = [
    {"pkg/__init__.py": "", "pkg/core.py": "def X():\n    pass\n", "lib/x.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n"},
    {"pkg/__init__.py": "from .core import X\n"},
    {"pkg/__init__.py": ""},
]
SCENARIOS["submodule_and_reexport"] = [
    {"pkg/__init__.py": "from .impl import util\n", "pkg/util.py": "def helper():\n    pass\n",
     "pkg/impl.py": "def util():\n    pass\n",
     "pkg/a.py": "from . import util\n\ndef f():\n    util()\n", "user.py": "from pkg import util\n\ndef g():\n    util()\n"},
    {"pkg/__init__.py": ""},
    {"pkg/__init__.py": "from .impl import util\n"},
]
SCENARIOS["script_dir_reexport"] = [
    {"scripts/common.py": "from helpers_mod import run\n", "scripts/helpers_mod.py": "def run():\n    pass\n",
     "scripts/main.py": "from common import run\n\ndef main():\n    run()\n"},
    {"scripts/helpers_mod.py": "def walk():\n    pass\n"},
    {"scripts/helpers_mod.py": "def run():\n    pass\n"},
    {"scripts/__init__.py": ""},
    {"scripts/__init__.py": None},
]
SCENARIOS["reexport_target_class_method_mix"] = [
    {"pkg/__init__.py": "from .core import load\n", "pkg/core.py": "class Loader:\n    def load(self):\n        pass\n",
     "user.py": "from pkg import load\n\ndef go():\n    load()\n"},
    {"pkg/core.py": "class Loader:\n    def load(self):\n        pass\n\ndef load():\n    pass\n"},
    {"pkg/core.py": "class Loader:\n    def load(self):\n        pass\n"},
]
SCENARIOS["init_vanish_reexport_relative"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "def X():\n    pass\n",
     "pkg/sub/__init__.py": "", "pkg/sub/m.py": "from .. import X\n\ndef f():\n    X()\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n"},
    {"pkg/__init__.py": None},
    {"pkg/__init__.py": "from .core import X\n"},
]
SCENARIOS["multi_file_run_reexport_and_importer"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "def X():\n    pass\n", "pkg/alt.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n", "user2.py": "from pkg import X\n"},
    {"pkg/__init__.py": "from .alt import X\n", "user.py": "from pkg import X\n\ndef go():\n    X()\n    X()\n"},
    {"pkg/alt.py": None, "pkg/__init__.py": "from .core import X\n"},
]
