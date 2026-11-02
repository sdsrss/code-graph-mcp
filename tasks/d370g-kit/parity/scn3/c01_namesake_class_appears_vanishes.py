W = ("class BaseWidget:\n    def __init__(self):\n        pass\n\n"
     "class Widget(BaseWidget):\n    pass\n\n"
     "class Button(Widget):\n    def __init__(self):\n        Widget.__init__(self)\n\n"
     "class Sub(Widget):\n    def __init__(self):\n        super().__init__()\n")
TTK = "class Widget:\n    def __init__(self):\n        pass\n"
BEFORE = {"w.py": W, "other.py": "def f():\n    pass\n"}
STEPS = [{"ttk.py": TTK}, {"ttk.py": "class Widget:\n    pass\n"}, {"ttk.py": TTK}, {"ttk.py": None}]
