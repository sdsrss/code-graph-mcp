import os
def sub(path, old, new):
    def f(root):
        p = os.path.join(root, path); s = open(p).read()
        assert old in s, (path, old); open(p, "w").write(s.replace(old, new, 1))
    return f
def append(path, text):
    def f(root):
        open(os.path.join(root, path), "a").write(text)
    return f
def create(path, text):
    def f(root):
        open(os.path.join(root, path), "w").write(text)
    return f
def remove(path):
    def f(root):
        os.remove(os.path.join(root, path))
    return f
I = "src/flask/__init__.py"
EDITS = {
  "flask_new_reexport": [append(I, "from .helpers import get_root_path as get_root_path\n")],
  "flask_retarget_redirect": [sub(I, "from .helpers import redirect as redirect", "from werkzeug.utils import redirect as redirect")],
  "flask_retarget_Flask": [sub(I, "from .app import Flask as Flask", "from .sansio.app import App as Flask")],
  "flask_init_comment": [append(I, "\n# edit\n")],
  "hono_types_touch": [append("src/client/types.ts", "\n// edit\n")],
  "hono_iface_to_class": [sub("src/client/types.ts", "export interface Response extends ClientResponse<unknown> {}", "export class Response {}")],
  "hono_new_response_def": [create("src/zz_response.ts", "export function Response() {\n  return 1\n}\n")],
  "flask_plain": [append("src/flask/helpers.py", "\n# edit\n")],
  "flask_body": [sub("src/flask/helpers.py", "def get_debug_flag() -> bool:\n", "def get_debug_flag() -> bool:\n    _x = 1\n")],
  "flask_tests_init": [create("tests/__init__.py", "")],
  "nx_add_graph": [append("networkx/__init__.py", "from networkx.classes.graph import Graph\n")],
  "nx_plain": [append("networkx/algorithms/cycles.py", "\n# edit\n")],
  "nx_utils_init_delete": [remove("networkx/utils/__init__.py")],
  "std_plain": [append("argparse.py", "\n# edit\n")],
  "std_del_json_init": [remove("json/__init__.py")],
  "std_logging_body": [sub("logging/__init__.py", "def getLogger(name=None):\n", "def getLogger(name=None):\n    _x = 1\n")],
}
