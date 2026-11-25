"""Summarise results.jsonl from run_coding.py: correctness, cost, turns, time, code-graph use, paired diffs.

    python3 analyze.py OUT_DIR

Correct = every hidden test passed, no full-suite failure outside the reference solution's own
failures (OUT_DIR/ref/full-<task>.json), and, for A, no unawaited non-test caller.
code-graph calls are counted from tool_use blocks in the trace (a Bash command that runs a
code-graph-mcp subcommand, or an MCP code-graph tool), not from text matches.
"""
import json
import math
import re
import sys
from pathlib import Path

CG = re.compile(r"code-graph-mcp\s+(callgraph|impact|refs|show|search|grep|overview|map|deps|ast-search|trace|affected)")


def cg_calls(trace):
    n, kinds = 0, {}
    for line in trace.read_text(errors="replace").splitlines():
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        if ev.get("type") != "assistant":
            continue
        for b in ev["message"].get("content", []):
            if b.get("type") != "tool_use":
                continue
            if b["name"].startswith("mcp__plugin_code-graph"):
                k = b["name"].rsplit("__", 1)[-1]
            else:
                m = CG.search((b.get("input") or {}).get("command") or "")
                k = m.group(1) if m else None
            if k:
                n += 1
                kinds[k] = kinds.get(k, 0) + 1
    return n, kinds


def sign_test(wins, losses):
    n = wins + losses
    if n == 0:
        return 1.0
    k = min(wins, losses)
    p = sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n
    return min(1.0, 2 * p)


def main():
    out = Path(sys.argv[1])
    ref = {t: set(json.loads((out / "ref" / f"full-{t}.json").read_text())["full"]["failed"]) for t in "ABC"}
    rows = [json.loads(l) for l in (out / "results.jsonl").read_text().splitlines()]
    # results.jsonl from the first run counted each hook output twice; injections.json is the recount
    fixed = json.loads((out / "injections.json").read_text()) if (out / "injections.json").exists() else {}
    for r in rows:
        if r["tag"] in fixed:
            r["injections"], r["injection_kinds"] = fixed[r["tag"]]
    for r in rows:
        r["cg_calls"], r["cg_kinds"] = cg_calls(out / "runs" / r["tag"] / "trace.jsonl")
        hidden_ok = bool(r["hidden"]) and r["hidden"]["rc"] == 0 and not r["hidden"]["failed"]
        new_fail = sorted(set(r["full_failed"] or []) - ref[r["task"]])
        static_ok = not (r.get("static") or {}).get("unawaited_calls")
        r["new_failures"] = new_fail
        r["correct"] = hidden_ok and not new_fail and static_ok and r["full_failed"] is not None
    print(f"{'tag':14} {'ok':3} {'cost':>6} {'turns':>5} {'secs':>5} {'out_tok':>7} {'cg':>3} {'inj':>3}  notes")
    for r in sorted(rows, key=lambda r: (r["task"], r["arm"], r["tag"])):
        notes = []
        if r["new_failures"]:
            notes.append(f"new failures {r['new_failures'][:3]}")
        if (r.get("static") or {}).get("unawaited_calls"):
            notes.append(f"unawaited {r['static']['unawaited_calls']}")
        if r["hidden"] and r["hidden"]["failed"]:
            notes.append(f"hidden failed {len(r['hidden']['failed'])}")
        if r["cg_kinds"]:
            notes.append(f"cg {r['cg_kinds']}")
        if r["injection_kinds"]:
            notes.append(f"inj {r['injection_kinds']}")
        if r["subtype"] != "success":
            notes.append(r["subtype"])
        print(f"{r['tag']:14} {'Y' if r['correct'] else 'N':3} {r['cost']:6.2f} {r['turns'] or 0:5} {r['secs']:5} "
              f"{r['output_tokens'] or 0:7} {r['cg_calls']:3} {r['injections']:3}  {'; '.join(notes)}")
    by = {(r["tag"].split("-")[0], r["task"], r["arm"]): r for r in rows}
    pairs = [(by[(rd, t, "none")], by[(rd, t, "plugin")]) for (rd, t, a) in by if a == "none" and (rd, t, "plugin") in by]
    print(f"\n{len(pairs)} pairs (same round, same task, run concurrently)")
    for arm in ("none", "plugin"):
        rs = [r for r in rows if r["arm"] == arm]
        print(f"  {arm:6}: correct {sum(r['correct'] for r in rs)}/{len(rs)}, cost ${sum(r['cost'] for r in rs):.2f}, "
              f"turns {sum(r['turns'] or 0 for r in rs)}, secs {sum(r['secs'] for r in rs)}, "
              f"output tokens {sum(r['output_tokens'] or 0 for r in rs)}, cg calls {sum(r['cg_calls'] for r in rs)} "
              f"in {sum(1 for r in rs if r['cg_calls'])}/{len(rs)} sessions")
    for key in ("cost", "turns", "secs", "output_tokens"):
        a = sum(p[0][key] or 0 for p in pairs)
        b = sum(p[1][key] or 0 for p in pairs)
        w = sum((p[1][key] or 0) < (p[0][key] or 0) for p in pairs)
        l = sum((p[1][key] or 0) > (p[0][key] or 0) for p in pairs)
        print(f"  {key:13}: none {a:,.2f} vs plugin {b:,.2f} ({(b - a) / a * 100:+.1f}%), plugin lower in {w}/{len(pairs)}, "
              f"higher in {l}/{len(pairs)}, sign test p={sign_test(w, l):.2f}")
    # within-arm spread across rounds = the noise a between-arm difference has to beat
    for key in ("cost", "turns"):
        spreads = []
        for t in sorted({r["task"] for r in rows}):
            for arm in ("none", "plugin"):
                v = [r[key] or 0 for r in rows if r["task"] == t and r["arm"] == arm]
                if len(v) > 1:
                    spreads.append(f"{t}/{arm} {min(v):g}-{max(v):g}")
        print(f"  within-arm range of {key}: {', '.join(spreads)}")


if __name__ == "__main__":
    main()
