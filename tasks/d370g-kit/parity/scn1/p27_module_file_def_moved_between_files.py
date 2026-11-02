# module P = single file; X moves from util.py into util.py class (method) - restore filter
BEFORE = {
 "util.py": "def helper():\n    pass\n",
 "zzz/decoy.py": "def helper():\n    pass\n",
 "yyy/decoy2.py": "def helper():\n    pass\n",
 "app.py": "import util\n\ndef main():\n    util.helper()\n",
}
STEPS = [{"util.py": "class Box:\n    def helper(self):\n        pass\n\nhelper = Box().helper\n"}]
SHOW = ["app"]
