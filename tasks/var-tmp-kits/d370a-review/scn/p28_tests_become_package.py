BEFORE = {
 "tests/helpers.py": "def make():\n    pass\n",
 "tests/test_a.py": "import helpers\nfrom helpers import make\n\ndef test_a():\n    helpers.make()\n    make()\n",
 "zzz/decoy.py": "def make():\n    pass\n",
 "yyy/decoy2.py": "def make():\n    pass\n",
}
STEPS = [{"tests/__init__.py": ""}]
SHOW = ["tests/test_a"]
