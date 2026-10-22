import glob, os, runpy
S = {}
for d in ("/var/tmp/d370a-review/scn", "/var/tmp/d370a-review2/scn", "/var/tmp/d520/scn518"):
    for p in sorted(glob.glob(d + "/*.py")):
        try:
            m = runpy.run_path(p)
        except Exception as e:
            continue
        if "BEFORE" not in m or "STEPS" not in m: continue
        S[os.path.basename(d.rstrip('/').split('/')[-2]) + "_" + os.path.basename(p)[:-3]] = [m["BEFORE"]] + list(m["STEPS"])
SCENARIOS = S
