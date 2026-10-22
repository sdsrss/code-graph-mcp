BEFORE = {
 "pkg/__init__.py": "try:\n    from ._speedups import helper\nexcept ImportError:\n    from ._slow import helper\n",
 "pkg/_slow.py": "def helper():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import helper\n\ndef run():\n    pkg.helper()\n    helper()\n",
 "app/other.py": "def helper():\n    pass\n",
}
STEPS = [{"pkg/_speedups.py": "def helper():\n    pass\n"}, {"pkg/_speedups.py": "def nothing():\n    pass\n"}, {"pkg/_speedups.py": None}][:1]
SHOW = ["app/c.py"]
