# 1: remove helpers.url_for (rename it away), 2: bring it back  (D9 name-pool rebind)
step_1() { python3 - <<'P'
p='src/flask/helpers.py'; s=open(p).read(); s=s.replace('def url_for(\n','def url_for_gone(\n',1); open(p,'w').write(s)
P
}
step_2() { python3 - <<'P'
p='src/flask/helpers.py'; s=open(p).read(); s=s.replace('def url_for_gone(\n','def url_for(\n',1); open(p,'w').write(s)
P
}
# 3: delete signals.py; 4: restore it
step_3() { cp src/flask/signals.py ../signals.py.bak; rm src/flask/signals.py; }
step_4() { cp ../signals.py.bak src/flask/signals.py; touch src/flask/signals.py; }
# 5: rename debughelpers.py -> debug_helpers.py and fix relative importers
step_5() { git mv -f src/flask/debughelpers.py src/flask/debug_helpers.py 2>/dev/null || mv src/flask/debughelpers.py src/flask/debug_helpers.py; grep -rl "debughelpers" src tests | xargs sed -i 's/debughelpers/debug_helpers/g'; }
# 6: add a new sibling module with relative imports + overload + bare decorator
step_6() { cat > src/flask/newmod.py <<'P'
from __future__ import annotations
import typing as t
from .helpers import get_root_path
from . import signals
from .sansio.scaffold import setupmethod
from .globals import request


class Thing:
    @setupmethod
    def go(self, x):
        return get_root_path(x)

    @t.overload
    def pick(self, x: int) -> int: ...
    @t.overload
    def pick(self, x: str) -> str: ...
    def pick(self, x):
        signals.template_rendered.send(x)
        return request.get(x)


def use_it():
    Thing().go("a")
    return Thing().pick(1)
P
}
# 7: make stream_with_context lose its implementation (stubs only), 8: restore
step_7() { cp src/flask/helpers.py ../helpers.bak; python3 - <<'P'
p='src/flask/helpers.py'; s=open(p).read(); i=s.index('def stream_with_context(\n    generator_or_function: t.Iterator[t.AnyStr] | t.Callable'); 
# rename the implementation so only the stubs define stream_with_context
s=s[:i]+s[i:].replace('def stream_with_context(','def stream_with_context_impl(',1); open(p,'w').write(s)
P
}
step_8() { cp ../helpers.bak src/flask/helpers.py; touch src/flask/helpers.py; }
# 9: add setupmethod decorator to a method that lacked it in blueprints.py? instead: remove setupmethod def (rename) then 10 restore
step_9() { cp src/flask/sansio/scaffold.py ../scaffold.bak; sed -i 's/^def setupmethod(/def setupmethod_x(/' src/flask/sansio/scaffold.py; }
step_10() { cp ../scaffold.bak src/flask/sansio/scaffold.py; touch src/flask/sansio/scaffold.py; }
