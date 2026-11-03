#!/usr/bin/env python3
"""memrepro.py BIN N WORKDIR: index one file holding a chain of N classes, each
with a metaclass name of its own (unbound, so taken as imported), with BIN;
print user CPU and peak RSS. Review of 0.183.0 (H1): an inherited metaclass
set copied into every class made memory grow with N squared."""
import os, resource, subprocess, sys
bin_, n, work = sys.argv[1], int(sys.argv[2]), sys.argv[3]
os.makedirs(work, exist_ok=True)
src = ["class A0(metaclass=M0):\n    def m(self): ...\n"]
src += [f"class A{i}(A{i-1},metaclass=M{i}):...\n" for i in range(1, n)]
src.append("def t_call():\n    return A0.m(None)\n")
open(os.path.join(work, "big.py"), "w").write("".join(src))
subprocess.run(["rm", "-rf", os.path.join(work, ".code-graph")], check=True)
subprocess.run(["git", "init", "-q"], cwd=work, check=True)
env = dict(os.environ, HOME=os.path.join(work, ".home"), CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
def cap():
    resource.setrlimit(resource.RLIMIT_AS, (8 << 30, 8 << 30))
p = subprocess.Popen([bin_, "incremental-index", "--no-embed"], cwd=work, env=env, preexec_fn=cap,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
_, status, ru = os.wait4(p.pid, 0)
print(f"N={n} {os.path.basename(bin_)}: user {ru.ru_utime:.2f}s maxrss {ru.ru_maxrss / 1024:.1f} MB exit {os.waitstatus_to_exitcode(status)}")
