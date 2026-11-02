BEFORE = {
 "pkg/__init__.py": "from .slow import helper\n",
 "pkg/slow.py": "def helper():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import helper\n\ndef run():\n    pkg.helper()\n    helper()\n",
 "app/other.py": "def helper():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "try:\n    from _cext import helper\nexcept ImportError:\n    from .slow import helper\n"}]
SHOW = ["app/c.py"]
