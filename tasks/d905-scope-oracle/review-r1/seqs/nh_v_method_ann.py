STATES = [
 {"lib/__init__.py": "", "lib/defs.py": "class T:\n    pass\n\n\ndef helper(*a):\n    return 1\n",
  "app/__init__.py": "", "app/u.py": "from lib.defs import T\n\n\ndef a5(x: T) -> T:\n    return x\n"},
 {"lib/other.py": "class KT:\n    def T(self):\n        return 1\n\n\ndef helper(*a):\n    return 1\n"},
]
