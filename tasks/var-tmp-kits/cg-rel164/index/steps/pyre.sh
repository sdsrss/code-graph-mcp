step_1() { printf 'def helper():\n    return 1\n' > pkg/util2.py; printf 'class Box:\n    def helper(self):\n        return 2\n' > pkg/util.py; sed -i 's/from .util import helper as helper/from .util2 import helper as helper/' pkg/__init__.py; sed -i 's/from .util import helper/from .util2 import helper/' pkg/core.py; }
step_2() { sed -i 's/^def helper/def helper_x/' pkg/util2.py; }
step_3() { sed -i 's/^def helper_x/def helper/' pkg/util2.py; }
step_4() { mv pkg/sub/deep.py pkg/sub/deeper.py; sed -i 's/pkg.sub.deep /pkg.sub.deeper /' tests/test_app.py; }
step_5() { printf 'def run():\n    return 0\n' > pkg/extra.py; }
step_6() { cp pkg/core.py ../core.bak; rm pkg/core.py; }
step_7() { cp ../core.bak pkg/core.py; touch pkg/core.py; }
step_8() { printf 'import typing as t\n\n@t.overload\ndef helper(x: int) -> int: ...\n@t.overload\ndef helper(x: str) -> str: ...\ndef helper(x=None):\n    return 1\n' > pkg/util2.py; }
step_9() { printf 'import typing as t\n\n@t.overload\ndef helper(x: int) -> int: ...\n@t.overload\ndef helper(x: str) -> str: ...\n' > pkg/util2.py; }
step_10() { rm pkg/sub/sibling.py; }
step_11() { printf 'def side():\n    return 4\n' > pkg/sub/sibling.py; }
