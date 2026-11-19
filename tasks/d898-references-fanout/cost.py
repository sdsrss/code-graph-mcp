#!/usr/bin/env python3
"""cost.py <work> <corpus-src> <reps> <scenario> <bin>...

Times incremental runs for each binary, the binaries interleaved within a rep
so machine noise falls on all of them. Each binary gets its own copy of the
corpus, indexed in full first. Scenarios:
  append:<file>          rep i appends a new function `zz_probe_i` to <file>
                         (the same copy throughout)
  new:<file>:<body>      rep i starts from a fresh copy of the indexed tree,
                         writes <file> with <body> (`\\n` for newlines) and
                         times that run, then deletes <file> and times that
                         run too: the first definition of a name and its
                         removal, each from the state before it
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
    return secs, [int(n) for n in re.findall(r"fan-out round: re-extracting (\d+)", r.stderr)]


def report(label, b, times, pulled):
    print(f"{os.path.basename(b)} {label}: median {statistics.median(times):.2f}s "
          f"(min {min(times):.2f}, max {max(times):.2f}, n={len(times)}); "
          f"fan-out files per run {sorted({tuple(p) for p in pulled})}", flush=True)


def main():
    work, src, reps, scenario = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
    bins = [os.path.abspath(b) for b in sys.argv[5:]]
    if os.path.exists(work):
        raise SystemExit(f"{work} exists")
    seeds = []
    for i, b in enumerate(bins):
        d = os.path.join(work, f"b{i}")
        shutil.copytree(src, d, symlinks=True)
        index(b, d)
        seeds.append(d)
    kind, arg = scenario.split(":", 1)
    if kind == "append":
        times = {b: [] for b in bins}
        pulled = {b: [] for b in bins}
        originals = {d: open(os.path.join(d, arg)).read() for d in seeds}
        for rep in range(1, reps + 1):
            for b, d in zip(bins, seeds):
                with open(os.path.join(d, arg), "w") as fh:
                    fh.write(originals[d] + f"\n\ndef zz_probe_{rep}():\n    return {rep}\n")
                secs, n = index(b, d)
                times[b].append(secs)
                pulled[b].append(n)
        for b in bins:
            report(scenario, b, times[b], pulled[b])
    else:
        rel, body = arg.split(":", 1)
        body = body.replace("\\n", "\n")
        add = {b: [] for b in bins}
        rm = {b: [] for b in bins}
        add_p = {b: [] for b in bins}
        rm_p = {b: [] for b in bins}
        for rep in range(reps):
            for b, d in zip(bins, seeds):
                r = d + f"-rep{rep}"
                shutil.copytree(d, r, symlinks=True)
                p = os.path.join(r, rel)
                os.makedirs(os.path.dirname(p), exist_ok=True)
                with open(p, "w") as fh:
                    fh.write(body)
                secs, n = index(b, r)
                add[b].append(secs)
                add_p[b].append(n)
                os.remove(p)
                secs, n = index(b, r)
                rm[b].append(secs)
                rm_p[b].append(n)
                shutil.rmtree(r)
        for b in bins:
            report(f"add {rel}", b, add[b], add_p[b])
            report(f"delete {rel}", b, rm[b], rm_p[b])
    shutil.rmtree(work)


if __name__ == "__main__":
    main()
