CASES = {
 "V1 body call sees no def, nested namesakes elsewhere (pre-existing keep-all?)": {"m.py":
  "def f():\n    return make()\n\n\ndef g():\n    def make():\n        pass\n    return make()\n\n\ndef k():\n    def make():\n        pass\n    return make()\n"},
 "V2 header call sees no def, nested namesakes elsewhere": {"m.py":
  "def f(x=make()):\n    return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n\n\ndef k():\n    def make():\n        pass\n    return make()\n"},
}
