BEFORE = {
 "pkg/__init__.py": "from .core import Widget\n",
 "pkg/core.py": "class Widget:\n    pass\n",
 "aaa/decoys.py": "def t():\n    class Widget:\n        pass\n",
 "app.py": "import pkg\nfrom pkg import Widget\n\ndef main():\n    pkg.Widget()\n    Widget()\n",
}
STEPS = [{"pkg/core.py": "class Widget:\n    pass\n\nclass Other:\n    pass\n"}]
SHOW = ["app.py"]
