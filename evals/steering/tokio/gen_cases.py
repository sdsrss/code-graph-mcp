#!/usr/bin/env python3
"""Write the tokio caller cases from rust-analyzer's call pairs.

    evals/steering/tokio/template.sh /var/tmp/cg-steer/tokio
    (cd /var/tmp/cg-steer/tokio/tokio && rust-analyzer scip . --output /tmp/tokio.scip)
    python3 scripts/scip_oracle/oracle.py --scip /tmp/tokio.scip \
        --root /var/tmp/cg-steer/tokio/tokio --dump-gold /tmp/tokio-gold.json
    python3 evals/steering/tokio/gen_cases.py --gold /tmp/tokio-gold.json \
        --db /var/tmp/cg-steer/tokio/tokio/.code-graph/index.db

Each case asks for the direct callers of one definition, counted in library
code (`tokio/src/`, no test nodes). The answer is the SCIP oracle's gold pairs,
not code-graph's edges, so a case cannot reward the plugin for sharing its own
blind spots. One grader per caller, matching its name and its file on one line
of the reply, so the score is recall. Run rust-analyzer on a copy, not on the
template: it writes target/ into the tree it indexes.
"""
import argparse
import json
import re
import sqlite3
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent

# name, callee as the index qualifies it, its definition. Chosen to mix the
# shapes code-graph resolves at the default floor (path calls: all found) with
# those it does not (method calls on common names: none found), and names that
# grep cannot tell apart (`new` has 284 definitions, `remove` 14).
CASES = [
    ("pointers-new", "Pointers.new", "tokio/src/util/linked_list.rs:423"),
    ("poll-evented-new", "PollEvented.new", "tokio/src/io/poll_evented.rs:89"),
    ("poll-evented-into-inner", "PollEvented.into_inner", "tokio/src/io/poll_evented.rs:135"),
    ("linked-list-remove", "LinkedList.remove", "tokio/src/util/linked_list.rs:200"),
    ("spawn-blocking", "spawn_blocking", "tokio/src/runtime/blocking/pool.rs:179"),
    ("registration-poll-read-ready", "Registration.poll_read_ready", "tokio/src/runtime/io/registration.rs:109"),
]

PROMPT = """---
description: "{desc}"
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `{shown}`, the {kind} defined at `{at}`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`).
"""


def lib(path):
    return path.startswith("tokio/src/") and "/tests/" not in path


def path_tail(path):
    parts = path.split("/")
    keep = 3 if parts[-1] in ("mod.rs", "lib.rs") else 2
    return re.escape("/".join(parts[-keep:]))


def name_part(qualified):
    *owner, name = qualified.split(".")
    if not owner:
        return rf"\b{re.escape(name)}\b"
    t, n = re.escape(owner[-1]), re.escape(name)
    # `Type::name`, `Type.name`, `<Type as Trait>::name`, or the bare name; a
    # name qualified by some other type does not count.
    return rf"(?:\b{t}\b[^\n]*?(?:::|\.){n}\b|(?<![\w:.]){n}\b)"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--gold", required=True, help="oracle.py --dump-gold output for the template")
    ap.add_argument("--db", required=True, help="the template's .code-graph/index.db")
    ap.add_argument("--out", default=str(HERE / "cases"))
    a = ap.parse_args()
    con = sqlite3.connect(f"file:{a.db}?mode=ro", uri=True)
    meta = {(p, line, name): (qn or name, bool(t)) for p, name, line, qn, t in con.execute(
        "SELECT f.path, n.name, n.start_line, n.qualified_name, n.is_test FROM nodes n JOIN files f ON f.id = n.file_id")}
    defs = Counter(n for (n,) in con.execute("SELECT name FROM nodes WHERE type IN ('function', 'method')"))
    gold = json.loads(Path(a.gold).read_text())

    def node(at, name):
        path, line = at.rsplit(":", 1)
        return (path, int(line), name), meta.get((path, int(line), name))

    for case, callee, at in CASES:
        callers = {}
        for g in gold:
            if g["callee_at"] != at:
                continue
            key, m = node(g["caller_at"], g["caller"])
            if m and lib(key[0]) and not m[1]:
                callers[key] = (m[0], g["best_tier"])
        assert callers, f"{case}: no gold callers for {callee} at {at}"
        files = {k[0] for k in callers}
        found = sum(1 for _, tier in callers.values() if tier in ("extracted", "inferred"))
        d = Path(a.out) / case
        (d / "graders").mkdir(parents=True, exist_ok=True)
        for old in (d / "graders").glob("item-*.md"):
            old.unlink()
        short = callee.split(".")[-1]
        desc = (f"Direct callers of {callee} in tokio/src: {len(callers)} in {len(files)} files "
                f"(rust-analyzer). `{short}` has {defs[short]} definitions in the index; code-graph's "
                f"default floor finds {found} of the {len(callers)}. One grader per caller, so the score is recall.")
        shown = callee.replace(".", "::")
        kind = "method" if "." in callee else "function"
        (d / "prompt.md").write_text(PROMPT.format(desc=desc, shown=shown, kind=kind, at=at))
        for i, (key, (qn, tier)) in enumerate(sorted(callers.items(), key=lambda c: (c[0][0], c[0][1]))):
            pattern = rf"(?m)^(?=[^\n]*{name_part(qn)})(?=[^\n]*{path_tail(key[0])})"
            assert "'" not in pattern
            slug = re.sub(r"[^A-Za-z0-9_]+", "-", qn)
            (d / "graders" / f"item-{i:02d}-{slug}.md").write_text(
                f"---\ntype: regex\npattern: '{pattern}'\n---\n\n{qn} @ {key[0]}:{key[1]} (code-graph: {tier or 'missed'})\n")
        print(f"{case}: {len(callers)} callers, {len(files)} files, default floor finds {found}")


if __name__ == "__main__":
    main()
