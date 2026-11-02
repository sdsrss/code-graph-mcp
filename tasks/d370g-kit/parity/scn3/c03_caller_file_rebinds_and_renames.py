W = ("class BaseWidget:\n    def __init__(self):\n        pass\n\n"
     "class Widget(BaseWidget):\n    pass\n\n"
     "class Button(Widget):\n    def __init__(self):\n        Widget.__init__(self)\n")
TTK = "class Widget:\n    def __init__(self):\n        pass\n"
BEFORE = {"w.py": W, "ttk.py": TTK}
STEPS = [{"w.py": W + "Widget = None\n"}, {"w.py": W}, {"w.py": W.replace("class Widget(BaseWidget)", "class Widget2(BaseWidget)")}, {"w.py": W}]
