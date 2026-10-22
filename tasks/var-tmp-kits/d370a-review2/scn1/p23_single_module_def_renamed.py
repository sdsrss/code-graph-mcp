BEFORE = {
 "util.py": "def helper():\n    pass\n",
 "tests/helpers.py": "def helper():\n    pass\n",
 "scripts/tool.py": "def helper():\n    pass\n",
 "app.py": "import util\n\ndef main():\n    util.helper()\n",
}
STEPS = [{"util.py": "def helper2():\n    pass\n"}]
SHOW = ["app.py:main"]
