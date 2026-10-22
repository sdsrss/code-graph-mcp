EDITS = [
 [("src/flask/helpers.py", "replace", "def redirect(", "def redirect_renamed(")],
 [("src/flask/__init__.py", "replace", "from .app import Flask as Flask\n", "")],
 [("src/flask/__init__.py", "append", "\nfrom .app import Flask\n"), ("src/flask/sansio/blueprints.py", "replace", "class Blueprint(", "class BlueprintS(")],
 [("src/flask/app.py", "append", "\n\ndef make_response(x):\n    return x\n")],
]
