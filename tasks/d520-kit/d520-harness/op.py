"""op.py <corpus> <bin> <op> <args...>: full index a copy, apply one op, incremental, compare with fresh.
ops: rename A B | delete A | create A TEXT | append A TEXT"""
import sys, os, shutil
sys.path.insert(0, "/var/tmp/d520/harness")
from corp import *
corpus, binary, op, args = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
w = f"{R}/work/op/{os.path.basename(binary)}"; inc = w + "/inc"; fr = w + "/fresh"
copy(corpus, inc); index(binary, inc)
p = os.path.join(inc, args[0])
if op == "rename": os.rename(p, os.path.join(inc, args[1]))
elif op == "delete": os.remove(p)
elif op == "create": os.makedirs(os.path.dirname(p) or ".", exist_ok=True); open(p, "w").write(args[1].encode().decode("unicode_escape"))
elif op == "append": open(p, "a").write(args[1].encode().decode("unicode_escape"))
t, cpu, r = index(binary, inc)
print(f"incremental {t*1000:.0f} ms", r.stderr.strip().splitlines()[-1:] if r.stderr else "")
copy(inc, fr); index(binary, fr)
diff(dump(inc), dump(fr), 40, op)
