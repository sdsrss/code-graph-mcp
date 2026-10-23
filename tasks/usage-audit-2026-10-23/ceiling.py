#!/usr/bin/env python3
"""Upper-bound savings and side questions for the 2026-10-23 audit:
  - a declaration-lookup grep (`grep -n "def X"`) followed within 3 calls by a
    Read / sed -n / cat of a file the grep printed: `code-graph-mcp show X`
    returns the body in one call, so each such pair is at most one call saved;
  - which subagent types call code-graph (parent Agent call's subagent_type);
  - assistant text blocks that name code-graph (is the model talking about it?).
Neutral projects, interactive sessions, era D (from 2026-10-16T12:01)."""
import collections
import glob
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import deep_usage as du  # noqa: E402

SINCE = "2026-10-16T12:01"


def files_in(text):
    return set(re.findall(r"([\w./-]+\.\w{1,5}):\d+", text))


def read_target(c):
    inp = c["input"] if isinstance(c["input"], dict) else {}
    if c["name"] == "Read":
        return str(inp.get("file_path", ""))
    if c["name"] == "Bash":
        m = re.search(r"\b(?:sed -n|cat|head|tail|nl|awk)\b[^|;&]*?([\w./-]+\.\w{1,5})", str(inp.get("command", "")))
        return m.group(1) if m else ""
    return ""


def main():
    pd = os.path.expanduser("~/.claude/projects")
    mains = sorted(glob.glob(os.path.join(pd, "*", "*.jsonl")))
    out = collections.Counter()
    sub_types = collections.defaultdict(collections.Counter)
    texts = []
    for mp in mains:
        try:
            s = du.parse(mp)
        except OSError:
            continue
        if s["entry"] != "cli" or not s["cwd"] or not s["start"] or s["start"] < SINCE:
            continue
        root, proj = du.root_of(s["cwd"])
        if not root or proj in du.OWN:
            continue
        # subagent transcripts of this session, keyed by agent id -> type via meta file
        sid = os.path.splitext(os.path.basename(mp))[0]
        for sp in glob.glob(os.path.join(os.path.dirname(mp), sid, "subagents", "agent-*.jsonl")):
            meta = sp[:-len(".jsonl")] + ".meta.json"
            typ = "?"
            try:
                typ = json.load(open(meta)).get("agentType") or "?"
            except (OSError, ValueError):
                pass
            try:
                ss = du.parse(sp)
            except OSError:
                continue
            calls = du.analyse(ss, root)
            ncg = sum(1 for c in calls if c["kind"] == "cg")
            sub_types[typ]["sessions"] += 1
            sub_types[typ]["tools"] += len(calls)
            sub_types[typ]["cg"] += ncg
            sub_types[typ]["used"] += int(ncg > 0)
            sub_types[typ]["symgrep"] += sum(1 for c in calls if c["kind"] == "grep" and c.get("symbol")
                                             and c.get("known") and c["target"] == "code" and not c.get("icase"))
            sub_types[typ]["subagent_start_ctx"] += sum(n for (p, h), n in ss["inject_n"].items()
                                                        if p == "cg" and h and h.startswith("SubagentStart"))
        for kind, sess in (("main", s),):
            calls = du.analyse(sess, root)
            out["tools"] += len(calls)
            for i, c in enumerate(calls):
                if not (c["kind"] == "grep" and c.get("symbol") and c.get("known") and c["target"] == "code"
                        and not c.get("icase")):
                    continue
                nxt = calls[i + 1:i + 4]
                if not c.get("decl"):
                    # a reference lookup followed by reads of 2+ distinct files: the chase callgraph shortcuts
                    reads = {read_target(d) for d in calls[i + 1:i + 6] if read_target(d)}
                    out["ref_greps"] += 1
                    out["ref_then_2plus_reads"] += int(len(reads) >= 2)
                    continue
                out["decl_greps"] += 1
                out["decl_with_context_flag"] += int(bool(re.search(r"\s-[ABC]\s?\d|\s-[ABC]\d|context", json.dumps(c["input"]))))
                out["decl_then_any_read"] += int(any(os.path.splitext(read_target(d))[1] in du.CODE_EXT for d in nxt))
                r = s["results"].get(c["id"])
                fs = files_in(r[0]) if r else set()
                if any(read_target(d) and any(read_target(d).endswith(f.lstrip("./")) or f.endswith(
                        os.path.basename(read_target(d))) for f in fs) for d in nxt):
                    out["decl_then_read"] += 1
        for line in open(mp, errors="replace"):
            if '"type":"assistant"' not in line and '"type": "assistant"' not in line:
                continue
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            for b in (rec.get("message") or {}).get("content") or []:
                if isinstance(b, dict) and b.get("type") == "text":
                    out["texts"] += 1
                    t = str(b.get("text", ""))
                    if re.search(r"code-graph|codegraph", t, re.I):
                        out["texts_cg"] += 1
                        texts.append((proj, t[:220].replace("\n", " ")))
    print(dict(out))
    print("subagent types (neutral, D):")
    for typ, c in sorted(sub_types.items(), key=lambda x: -x[1]["sessions"]):
        print(f"  {typ:32} {dict(c)}")
    print("assistant texts naming code-graph:")
    for t in texts[:30]:
        print("  ", t)


if __name__ == "__main__":
    main()
