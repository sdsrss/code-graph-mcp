# 0.175.0's review scenarios (round 1 s1-s8, round 2 cases1-6), re-run by my harness.
import runpy
S = {}
for i in range(1, 9):
    for k, v in runpy.run_path(f"/var/tmp/d520/harness/s{i}.py")["SCENARIOS"].items():
        S[f"s{i}_{k}"] = v
for i in range(1, 7):
    for k, v in runpy.run_path(f"/var/tmp/d175-r2/harness/cases{i}.py")["CASES"].items():
        S[f"c{i}_{k}"] = [v["init"]] + v["steps"]
SCENARIOS = S
