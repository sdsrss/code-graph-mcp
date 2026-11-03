EXT = "def go():\n    pass\n\nclass sub:\n    go = staticmethod(go)\n"
APP = "import ext\n\ndef go():\n    pass\n\ndef main():\n    ext.sub.go()\n    ext.go()\n"
BEFORE = {"ext.py": EXT, "app.py": APP}
STEPS = [{"app.py": APP + "\next.sub.go = go\n"}, {"app.py": APP + "\ndef f():\n    global ext\n"}, {"app.py": APP}]
