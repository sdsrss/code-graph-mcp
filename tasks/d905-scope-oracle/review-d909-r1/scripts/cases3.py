CASES = {
    "D01 module decorator, decorated def nests a namesake": {"m.py":
        "def deco(f):\n    return f\n\n\n@deco\ndef g():\n    def deco():\n        pass\n    deco()\n"},
}
