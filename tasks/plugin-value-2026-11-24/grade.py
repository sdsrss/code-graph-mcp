"""Grade a workspace for one task: overlay the task commit's test files, run them and (optionally)
the full backend suite in the daagu-backend:dev image.

    python3 grade.py TASK WS [--full] [--reference]   # --reference: also overlay the commit's source files

Prints one JSON line: hidden passed/failed, full-suite failed test ids (with --full).
Containers run as the invoking user with no bytecode/cache writes, so the workspace stays removable."""
import json, os, re, subprocess, sys
REPO = "/home/ai/dev/daagu"
TASKS = {k: v["commit"] for k, v in json.loads(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "private", "tasks.json")).read()).items()}


def git(*a):
    return subprocess.run(["git", "-C", REPO, *a], capture_output=True, text=True, check=True).stdout


def overlay(commit, ws, tests_only=True):
    paths = []
    for line in git("show", "--name-status", "--format=", commit).splitlines():
        st, p = line.split("\t", 1)
        if tests_only and "/tests/" not in p:
            continue
        dst = os.path.join(ws, p)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with open(dst, "wb") as f:
            f.write(subprocess.run(["git", "-C", REPO, "show", f"{commit}:{p}"], capture_output=True, check=True).stdout)
        if "/tests/" in p:
            paths.append(p)
    return paths


def pytest(ws, targets, timeout=1500):
    cmd = ["docker", "run", "--rm",
           "-e", "PYTHONDONTWRITEBYTECODE=1", "-v", f"{ws}:/repo", "-w", "/repo/backend",
           "-e", "ENV_MODE=DEV", "-e", "ALERT_EMAIL_TO=", "daagu-backend:dev",
           "python", "-m", "pytest", *targets, "-q", "--no-header", "-p", "no:warnings",
           "-p", "no:cacheprovider", "-m", "not real_network", "-rfE"]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    out = r.stdout + r.stderr
    failed = sorted(set(re.findall(r"^(?:FAILED|ERROR) (\S+)", out, re.M)))
    summ = re.findall(r"=+ (.*?(?:passed|failed|error).*?) in [\d.]+s", out)
    crash = bool(re.search(r"Fatal Python error|terminated abruptly|Segmentation fault", out))
    return {"rc": r.returncode, "summary": summ[-1] if summ else out[-400:], "failed": failed, "crash": crash}


def main():
    task, ws = sys.argv[1], os.path.abspath(sys.argv[2])
    commit = TASKS[task]
    if "--reference" in sys.argv:
        overlay(commit, ws, tests_only=False)
    hidden = overlay(commit, ws)
    row = {"task": task, "ws": ws, "hidden": pytest(ws, [p.removeprefix("backend/") for p in hidden])}
    if "--full" in sys.argv:
        row["full"] = pytest(ws, ["tests/"])
    print(json.dumps(row))


if __name__ == "__main__":
    main()
