"""Find daagu commits that change a backend function's signature and update callers in other files, with tests."""
import re, subprocess, sys
REPO = "/home/ai/dev/daagu"
def git(*a): return subprocess.run(["git", "-C", REPO, *a], capture_output=True, text=True).stdout
DEF = re.compile(r"^([+-])\s*(?:async\s+)?def\s+([A-Za-z_]\w*)\s*\(")
out = []
for line in git("log", "--no-merges", "--format=%H %ci %s", "-n", "3000").splitlines():
    sha, date, _, subj = line.split(" ", 3)
    files = [f for f in git("show", "--name-only", "--format=", sha).split() if f.startswith("backend/") and f.endswith(".py")]
    tests = [f for f in files if "/tests/" in f]
    src = [f for f in files if "/tests/" not in f]
    if len(src) < 2 or not tests or len(files) > 25: continue
    diff = git("show", "-U0", "--format=", sha, "--", *src)
    cur = None; minus, plus = {}, {}
    for l in diff.splitlines():
        if l.startswith("+++ b/"): cur = l[6:]
        m = DEF.match(l)
        if m and cur:
            (minus if m.group(1) == "-" else plus).setdefault(m.group(2), set()).add(cur)
    changed = [n for n in minus if n in plus and not n.startswith("test")]
    if not changed: continue
    # callers in other changed src files mention the name on a + line
    hits = []
    for n in changed:
        deffiles = minus[n] | plus[n]
        others = set()
        cur = None
        for l in diff.splitlines():
            if l.startswith("+++ b/"): cur = l[6:]
            elif l.startswith("+") and cur not in deffiles and re.search(r"\b%s\s*\(" % re.escape(n), l): others.add(cur)
        if len(others) >= 1: hits.append((n, len(others)))
    if hits:
        out.append(f"{sha[:10]} {date} src={len(src)} tests={len(tests)} {hits} {subj[:90]}")
print("\n".join(out))
