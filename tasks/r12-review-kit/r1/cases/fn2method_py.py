FILES = {
 'x/x.py': 'class X:\n    def drainQueue(self):\n        pass\n',
 'conn/conn.py': 'def drainQueue(c):\n    pass\n\nclass Conn:\n    pass\n',
 'app/use.py': 'def use(c):\n    c.drainQueue()\n',
}
EDITS = [
 {'conn/conn.py': 'class Conn:\n    def drainQueue(self):\n        pass\n'},
]
