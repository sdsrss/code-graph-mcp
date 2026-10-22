import runpy, sys, os
sys.path.insert(0, "/var/tmp/d520/harness")
os.environ.setdefault("BINS", "v175=/var/tmp/d520/bin/cgm-v175,r1=/var/tmp/d520/bin/cgm-r1")
import importlib
scen = runpy.run_path("/var/tmp/d520/harness/scen.py", run_name="lib")
for f in sys.argv[1:]:
    c = runpy.run_path(f)["CASES"]
    for k, v in c.items():
        scen["run"](k, [v["init"]] + v["steps"])
