import runpy, sys
c = runpy.run_path(sys.argv[1])["CASES"]
SCENARIOS = {k: [v["init"]] + v["steps"] for k, v in c.items()}
