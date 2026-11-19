#!/usr/bin/env python3
"""cost.py <work> <corpus-src> <reps> <scenario> <bin>...

Times one incremental run per rep for each binary, the binaries interleaved
within a rep so machine noise falls on all of them. Each binary gets its own
copy of the corpus, indexed in full first. Scenarios:
  append:<file>       rep i appends a new function `zz_probe_i` to <file>
  define:<name>       rep i writes `zz_probe.py` defining `def <name>()`
                      (odd reps) or deletes it (even reps, not timed)
Prints per binary the median seconds and the fan-out round's file counts.
"""
import os, re, shutil, statistics, subprocess, sys, time

ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1", RUST_LOG="info")


def index(binary, d):
    t = time.monotonic()
    r = subprocess.run([binary, "incremental-index", "--no-embed"], cwd=d, env=ENV,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    secs = time.monotonic() - t
    if r.returncode != 0:
        raise SystemExit(f"index failed in {d}: {r.stderr[-800:]}")
    m = re.search(r"fan-out round: re-extracting (\d+) caller file", r.stderr)
    return secs, int(m.group(1)) if m else 0


def main():
    work, src, reps, scenario = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
    bins = [os.path.abspath(b) for b in sys.argv[5:]]
    if os.path.exists(work):
        raise SystemExit(f"{work} exists")
    dirs = []
    for i, b in enumerate(bins):
        d = os.path.join(work, f"b{i}")
        shutil.copytree(src, d, symlinks=True)
        index(b, d)
        dirs.append(d)
    kind, arg = scenario.split(":", 1)
    times = {b: [] for b in bins}
    pulled = {b: [] for b in bins}
    originals = {d: open(os.path.join(d, arg)).read() for d in dirs} if kind == "append" else {}
    timed_reps = 0
    for rep in range(1, (2 * reps if kind == "define" else reps) + 1):
        for b, d in zip(bins, dirs):
            if kind == "append":
                with open(os.path.join(d, arg), "w") as fh:
                    fh.write(originals[d] + f"\n\ndef zz_probe_{rep}():\n    return {rep}\n")
                timed = True
            else:
                p = os.path.join(d, "zz_probe.py")
                if rep % 2:
                    with open(p, "w") as fh:
                        fh.write(f"def {arg}():\n    return {rep}\n")
                    timed = True
                else:
                    os.remove(p)
                    timed = False
            secs, n = index(b, d)
            if timed:
                times[b].append(secs)
                pulled[b].append(n)
    for b in bins:
        print(f"{os.path.basename(b)} {scenario}: median {statistics.median(times[b]):.2f}s "
              f"(min {min(times[b]):.2f}, max {max(times[b]):.2f}, n={len(times[b])}); "
              f"fan-out files {sorted(set(pulled[b]))}", flush=True)
    shutil.rmtree(work)


if __name__ == "__main__":
    main()
