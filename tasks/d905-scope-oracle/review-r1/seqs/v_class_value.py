STATES = [
 {"lib/__init__.py": "", "lib/defs.py": "class T:\n    pass\n\n\ndef helper(*a):\n    return 1\n",
  "app/__init__.py": "", "app/u.py": "from lib.defs import T\n\n\ndef f3():\n    helper = 3\n    def g():\n        return helper\n    return g\n\n\ndef a5():\n    return use(T)\n"},
 {"lib/other.py": "class T:\n    pass\n\n\ndef helper(*a):\n    return 1\n"},
]
