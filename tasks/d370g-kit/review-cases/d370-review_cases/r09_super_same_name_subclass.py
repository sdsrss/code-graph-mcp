FILES = {
"ext.py": """from base import Widget


class Widget(Widget):
    def __init__(self):
        super().__init__()

    def close(self):
        super().close()
""",
"base.py": """class Widget:
    def __init__(self):
        pass

    def close(self):
        pass
""",
}
