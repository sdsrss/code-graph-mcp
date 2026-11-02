BEFORE = {
 "pkg/__init__.py": "from .core import _RegistryImpl as Registry\n",
 "pkg/core.py": "class _RegistryImpl:\n    pass\n",
 "zzz/decoy.py": "class Registry:\n    pass\n",
 "yyy/decoy2.py": "class Registry:\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.Registry()\n",
 "app2.py": "from pkg import Registry\n\ndef main2():\n    Registry()\n",
}
STEPS = [{"pkg/core.py": "class _Registry:\n    pass\n", "pkg/__init__.py": "from .core import _Registry as Registry\n"}]
SHOW = ["app"]
