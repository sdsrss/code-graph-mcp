import os, shutil, subprocess, sys, time, statistics, collections
# timeit.py ROUNDS DIR... : interleaved full index of each DIR with base-src and head-src;
# median wall and CPU (user+sys) seconds, max RSS (os.wait4 per child)
rounds = int(sys.argv[1]); dirs = sys.argv[2:]; res = collections.defaultdict(list)
for r in range(rounds):
    for arm in ("base-src", "head-src"):
        for d in dirs:
            cg = os.path.join(d, ".code-graph")
            if os.path.isdir(cg): shutil.rmtree(cg)
            t = time.monotonic()
            p = subprocess.Popen([f"/var/tmp/d700/bin/{arm}", "incremental-index", "--no-embed"], cwd=d,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            _, status, ru = os.wait4(p.pid, 0)
            res[(d, arm)].append((time.monotonic() - t, ru.ru_utime + ru.ru_stime, ru.ru_maxrss, os.waitstatus_to_exitcode(status)))
for d in dirs:
    b = res[(d, "base-src")]; h = res[(d, "head-src")]
    med = lambda xs, i: statistics.median(x[i] for x in xs)
    print(f"{d:12s} wall {med(b,0):7.3f}->{med(h,0):7.3f}s ({med(h,0)/med(b,0)-1:+.1%})  cpu {med(b,1):7.3f}->{med(h,1):7.3f}s ({med(h,1)/med(b,1)-1:+.1%})"
          f"  rss MB {max(x[2] for x in b)//1024}->{max(x[2] for x in h)//1024}  rc {sorted(set(x[3] for x in b+h))}")
