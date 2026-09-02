#!/usr/bin/env python3
"""Static code metrics for the audit baseline (docs/audit/00-baseline.md).

Standard library only, so it runs on any checkout. Prints Markdown to stdout;
`--json` prints the raw numbers instead. Every number is derived from
`git ls-files` (tracked files only) except the long-function count, which is
read from the project's own AST index (.code-graph/index.db) — the repo is
its own dogfood, and that index already carries function line ranges for
Rust and JavaScript. Run `code-graph-mcp incremental-index` first if you
need the index fresh; the report prints how many tracked source files the
index is missing so a stale index is visible rather than silent.

Metrics:
  files / lines          per category (Rust src, Rust tests, JS prod, JS tests, ...)
  largest files          top 10 by line count (all tracked source)
  long functions         production functions > 50 lines (from the AST index)
  duplication rate       6-line normalized sliding window, jscpd-like, per corpus
  circular dependencies  Rust module-level (src/<module>) and JS file-level (require)
  static test counts     #[test] attributes and node:test `test(`/`it(` calls
"""

import argparse
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LONG_FN_THRESHOLD = 50
DUP_WINDOW = 6
DUP_MIN_CHARS = 60  # a 6-line window of braces/`else` is not a clone


def sh(args):
    return subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def tracked_files():
    return [p for p in sh(["git", "ls-files"]).splitlines() if p]


def categorize(path):
    if path.endswith(".rs"):
        if path.startswith("src/") or path == "build.rs":
            return "rust_src"
        if path.startswith(("tests/", "benches/")):
            return "rust_test"
        return "rust_other"
    if path.endswith(".js"):
        return "js_test" if path.endswith(".test.js") else "js_prod"
    if path.endswith(".py"):
        return "python"
    if path.endswith((".md", ".txt")):
        return "docs"
    if path.endswith((".json", ".yml", ".yaml", ".toml", ".jsonl", ".sh", ".h")) or "/" not in path:
        return "config_other"
    return "config_other"


def read_lines(path):
    with open(os.path.join(ROOT, path), "rb") as f:
        return f.read().decode("utf-8", errors="replace").splitlines()


def is_rust_inline_test_file(path):
    """`src/**/tests.rs` (and `src/**/tests/*.rs`) are `#[cfg(test)] mod tests;`
    bodies split out of their parent — whole-file test code living under src/."""
    return os.path.basename(path) == "tests.rs" or "/tests/" in path


def strip_rust_comments_and_strings(text):
    """Blank out `//` / `/* */` comments and string literals so that a path
    mentioned in prose (`// see crate::graph::routes`) or in a test fixture
    string is not read as a dependency edge or a test marker. Newlines are
    preserved so line numbers still line up with the original file."""
    out = []
    i = 0
    n = len(text)

    def skip_to(j):
        nonlocal i
        out.append(text[i:j].translate(KEEP_NEWLINES))
        i = j

    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            j = text.find("\n", i)
            skip_to(n if j == -1 else j)
            continue
        if c == "/" and nxt == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            skip_to(j)
            continue
        raw = re.match(r'r(#*)"', text[i:]) if c == "r" else None
        if raw:
            close = '"' + raw.group(1)
            j = text.find(close, i + len(raw.group(0)))
            skip_to(n if j == -1 else j + len(close))
            continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            skip_to(min(n, j + 1))
            continue
        if c == "'" and i + 2 < n and text[i + 2] == "'" and text[i + 1] != "\\":
            # char literal such as '"' — skip so the quote does not open a string
            skip_to(i + 3)
            continue
        out.append(c)
        i += 1
    return "".join(out)


# translate table: every char except newline becomes a space
KEEP_NEWLINES = {cp: 32 for cp in range(0x110000) if cp != 10}


def rust_test_line_mask(stripped_lines):
    """True for every line that belongs to a `#[cfg(test)]`-gated item: the
    attribute line plus the item that follows it (`mod tests { ... }` through
    its matching brace, or a one-line `mod tests;` / `use ...;` / `const ...;`).
    Works on comment- and string-stripped lines so braces inside literals do
    not unbalance the scan."""
    mask = [False] * len(stripped_lines)
    i = 0
    while i < len(stripped_lines):
        if stripped_lines[i].strip().startswith("#[cfg(test)]"):
            mask[i] = True
            depth = 0
            opened = False
            j = i + 1
            while j < len(stripped_lines):
                mask[j] = True
                line = stripped_lines[j]
                for ch in line:
                    if ch == "{":
                        depth += 1
                        opened = True
                    elif ch == "}":
                        depth -= 1
                if opened and depth <= 0:
                    break
                if not opened and ";" in line:
                    break
                j += 1
            i = j + 1
        else:
            i += 1
    return mask


def rust_split(path, lines):
    """(production original lines, production stripped text) for a src file."""
    if is_rust_inline_test_file(path):
        return [], ""
    stripped = strip_rust_comments_and_strings("\n".join(lines)).split("\n")
    assert len(stripped) == len(lines), path
    mask = rust_test_line_mask(stripped)
    prod = [lines[i] for i in range(len(lines)) if not mask[i]]
    prod_stripped = "\n".join(stripped[i] for i in range(len(lines)) if not mask[i])
    return prod, prod_stripped


def rust_production_lines(path, lines):
    return rust_split(path, lines)[0]


# --- duplication -----------------------------------------------------------

COMMENT_PREFIXES = ("//", "#", "*", "/*", "*/")


def normalize_for_dup(lines):
    out = []
    for line in lines:
        s = re.sub(r"\s+", " ", line.strip())
        if not s or s.startswith(COMMENT_PREFIXES):
            continue
        out.append(s)
    return out


def duplication_rate(files_lines):
    """Fraction of normalized lines that sit inside a 6-line window seen >= 2 times.

    A window must carry at least DUP_MIN_CHARS characters and not be made of
    single-token lines only, so `} } else { } }` cascades do not count.
    """
    windows = defaultdict(list)  # hash -> [(file_idx, start)]
    corpus = []
    for path, lines in files_lines:
        norm = normalize_for_dup(lines)
        corpus.append((path, norm))
        fi = len(corpus) - 1
        for i in range(0, len(norm) - DUP_WINDOW + 1):
            win = norm[i : i + DUP_WINDOW]
            if sum(len(w) for w in win) < DUP_MIN_CHARS:
                continue
            if all(len(w.split()) <= 1 for w in win):
                continue
            h = hashlib.blake2b("\n".join(win).encode(), digest_size=16).digest()
            windows[h].append((fi, i))
    dup_marks = [set() for _ in corpus]
    clone_pairs = 0
    for h, occ in windows.items():
        if len(occ) < 2:
            continue
        clone_pairs += 1
        for fi, start in occ:
            dup_marks[fi].update(range(start, start + DUP_WINDOW))
    total = sum(len(norm) for _, norm in corpus)
    dup = sum(len(m) for m in dup_marks)
    per_file = sorted(
        ((len(dup_marks[i]), corpus[i][0]) for i in range(len(corpus)) if dup_marks[i]),
        reverse=True,
    )
    return {
        "normalized_lines": total,
        "duplicated_lines": dup,
        "rate": (dup / total) if total else 0.0,
        "duplicate_windows": clone_pairs,
        "top_files": per_file[:5],
    }


# --- circular dependencies -------------------------------------------------


def tarjan_scc(graph):
    index = {}
    low = {}
    on_stack = set()
    stack = []
    sccs = []
    counter = [0]

    def strong(v):
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on_stack.add(v)
        for w in graph.get(v, ()):
            if w not in index:
                strong(w)
                low[v] = min(low[v], low[w])
            elif w in on_stack:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                on_stack.discard(w)
                comp.append(w)
                if w == v:
                    break
            sccs.append(comp)

    sys.setrecursionlimit(10000)
    for v in list(graph):
        if v not in index:
            strong(v)
    return [c for c in sccs if len(c) > 1 or (len(c) == 1 and c[0] in graph.get(c[0], ()))]


def rust_module_of(path):
    if not path.startswith("src/"):
        return None
    rest = path[4:]
    if rest == "lib.rs":
        return None
    if rest == "main.rs":
        return "main"
    head = rest.split("/", 1)[0]
    return head[:-3] if head.endswith(".rs") else head


USE_BRACE_RE = re.compile(r"\b(?:crate|code_graph_mcp)::\{([^;]*?)\}\s*;", re.S)
PATH_RE = re.compile(r"\b(?:crate|code_graph_mcp)::([A-Za-z_][A-Za-z0-9_]*)")


def rust_refs(text):
    """Top-level module names referenced via `crate::X` / `code_graph_mcp::X`."""
    names = set(PATH_RE.findall(text))
    for inner in USE_BRACE_RE.findall(text):
        depth = 0
        piece = []
        for ch in inner + ",":
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
            if ch == "," and depth == 0:
                item = "".join(piece).strip()
                piece = []
                m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)", item)
                if m:
                    names.add(m.group(1))
            else:
                piece.append(ch)
    names.discard("self")
    return names


def rust_module_graph(rust_src_files):
    modules = set()
    for p in rust_src_files:
        m = rust_module_of(p)
        if m:
            modules.add(m)
    graph = defaultdict(set)
    for p in rust_src_files:
        m = rust_module_of(p)
        if not m:
            continue
        text = rust_split(p, read_lines(p))[1]
        for target in rust_refs(text):
            if target in modules and target != m:
                graph[m].add(target)
    for m in modules:
        graph.setdefault(m, set())
    return graph


REQUIRE_RE = re.compile(r"""require\(\s*['"](\.{1,2}/[^'"]+)['"]\s*\)""")


def js_file_graph(js_prod_files):
    known = set(js_prod_files)
    graph = defaultdict(set)
    for p in js_prod_files:
        text = "\n".join(read_lines(p))
        for spec in REQUIRE_RE.findall(text):
            target = os.path.normpath(os.path.join(os.path.dirname(p), spec))
            if not target.endswith(".js"):
                if target + ".js" in known:
                    target += ".js"
                elif os.path.join(target, "index.js") in known:
                    target = os.path.join(target, "index.js")
            if target in known and target != p:
                graph[p].add(target)
        graph.setdefault(p, set())
    return graph


# --- long functions from the AST index -------------------------------------


def long_functions(tracked_source):
    db = os.path.join(ROOT, ".code-graph", "index.db")
    if not os.path.exists(db):
        return {"available": False, "reason": "no .code-graph/index.db (run `code-graph-mcp incremental-index`)"}
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        indexed = {row[0] for row in conn.execute("SELECT path FROM files")}
        missing = sorted(p for p in tracked_source if p not in indexed)
        q = (
            "SELECT f.path, n.name, n.end_line - n.start_line + 1 AS len, n.is_test "
            "FROM nodes n JOIN files f ON f.id = n.file_id "
            "WHERE n.type IN ('function', 'method') AND f.language IN ('rust', 'javascript') "
            "AND n.end_line - n.start_line + 1 > ?"
        )
        rows = conn.execute(q, (LONG_FN_THRESHOLD,)).fetchall()
    finally:
        conn.close()
    prod = [r for r in rows if r[3] == 0 and categorize(r[0]) in ("rust_src", "js_prod")]
    test = [r for r in rows if r not in prod]
    prod.sort(key=lambda r: -r[2])
    return {
        "available": True,
        "threshold": LONG_FN_THRESHOLD,
        "production_count": len(prod),
        "test_count": len(test),
        "top": [(r[0], r[1], r[2]) for r in prod[:10]],
        "indexed_files": len(indexed),
        "tracked_source_missing_from_index": missing,
    }


# --- main -------------------------------------------------------------------


def collect():
    files = tracked_files()
    cats = defaultdict(list)
    for p in files:
        cats[categorize(p)].append(p)

    line_counts = {}
    per_file_lines = {}
    for cat, paths in cats.items():
        total = 0
        for p in paths:
            try:
                n = len(read_lines(p))
            except (OSError, UnicodeDecodeError):
                n = 0
            per_file_lines[p] = n
            total += n
        line_counts[cat] = total

    rust_prod_only = sum(len(rust_production_lines(p, read_lines(p))) for p in cats["rust_src"])
    rust_src_test_files = [p for p in cats["rust_src"] if is_rust_inline_test_file(p)]

    source_cats = ("rust_src", "rust_test", "js_prod", "js_test", "python")
    source_files = [p for c in source_cats for p in cats[c]]
    largest = sorted(((per_file_lines[p], p) for p in source_files), reverse=True)[:10]

    dup = {
        "rust_src_production": duplication_rate(
            [(p, rust_production_lines(p, read_lines(p))) for p in cats["rust_src"]]
        ),
        "rust_src_with_inline_tests": duplication_rate([(p, read_lines(p)) for p in cats["rust_src"]]),
        "rust_tests": duplication_rate([(p, read_lines(p)) for p in cats["rust_test"]]),
        "js_prod": duplication_rate([(p, read_lines(p)) for p in cats["js_prod"]]),
        "js_test": duplication_rate([(p, read_lines(p)) for p in cats["js_test"]]),
    }

    rust_graph = rust_module_graph(cats["rust_src"])
    rust_cycles = tarjan_scc(rust_graph)
    js_graph = js_file_graph(cats["js_prod"])
    js_cycles = tarjan_scc(js_graph)

    rs_test_attr_src = sum(len(re.findall(r"#\[test\]", "\n".join(read_lines(p)))) for p in cats["rust_src"])
    rs_test_attr_tests = sum(
        len(re.findall(r"#\[test\]", "\n".join(read_lines(p)))) for p in cats["rust_test"]
    )
    js_test_calls = sum(
        len(re.findall(r"^\s*(?:test|it)\(", "\n".join(read_lines(p)), re.M)) for p in cats["js_test"]
    )

    return {
        "head": sh(["git", "rev-parse", "--short", "HEAD"]).strip(),
        "files": {c: len(v) for c, v in cats.items()},
        "files_total": len(files),
        "lines": line_counts,
        "lines_total": sum(line_counts.values()),
        "rust_src_production_lines": rust_prod_only,
        "rust_src_test_files": rust_src_test_files,
        "largest_files": largest,
        "long_functions": long_functions(cats["rust_src"] + cats["js_prod"]),
        "duplication": dup,
        "rust_module_graph": {k: sorted(v) for k, v in sorted(rust_graph.items())},
        "rust_module_cycles": rust_cycles,
        "js_file_cycles": js_cycles,
        "static_test_counts": {
            "rust_test_attr_src": rs_test_attr_src,
            "rust_test_attr_tests": rs_test_attr_tests,
            "js_test_calls": js_test_calls,
        },
    }


def pct(x):
    return f"{100 * x:.2f}%"


def render(m):
    o = []
    o.append(f"### Files and lines (git-tracked, HEAD `{m['head']}`)")
    o.append("")
    o.append("| Category | Files | Lines |")
    o.append("|---|---:|---:|")
    order = ["rust_src", "rust_test", "js_prod", "js_test", "python", "docs", "config_other", "rust_other"]
    for c in order:
        if c in m["files"]:
            o.append(f"| {c} | {m['files'][c]} | {m['lines'][c]:,} |")
    o.append(f"| **total** | **{m['files_total']}** | **{m['lines_total']:,}** |")
    o.append("")
    o.append(
        f"Rust `src/` production lines (`#[cfg(test)]` items removed, excluding the "
        f"{len(m['rust_src_test_files'])} `src/**/tests.rs` files): "
        f"**{m['rust_src_production_lines']:,}** of {m['lines']['rust_src']:,}."
    )
    o.append("")
    o.append("### Largest 10 source files")
    o.append("")
    o.append("| Lines | File |")
    o.append("|---:|---|")
    for n, p in m["largest_files"]:
        o.append(f"| {n:,} | `{p}` |")
    o.append("")
    lf = m["long_functions"]
    o.append(f"### Functions longer than {LONG_FN_THRESHOLD} lines (Rust + JS, from the AST index)")
    o.append("")
    if not lf["available"]:
        o.append(f"_unavailable: {lf['reason']}_")
    else:
        o.append(f"- production: **{lf['production_count']}** · test code: {lf['test_count']}")
        miss = lf["tracked_source_missing_from_index"]
        o.append(
            f"- index provenance: {lf['indexed_files']} files indexed; "
            f"{len(miss)} tracked Rust/JS source files missing from the index"
            + (f" ({', '.join('`' + p + '`' for p in miss[:5])}{'…' if len(miss) > 5 else ''})" if miss else "")
        )
        o.append("")
        o.append("| Lines | Function | File |")
        o.append("|---:|---|---|")
        for path, name, n in lf["top"]:
            o.append(f"| {n} | `{name}` | `{path}` |")
    o.append("")
    o.append(f"### Duplication ({DUP_WINDOW}-line normalized sliding window, ≥{DUP_MIN_CHARS} chars)")
    o.append("")
    o.append("| Corpus | Normalized lines | Duplicated lines | Rate | Top files |")
    o.append("|---|---:|---:|---:|---|")
    for name, d in m["duplication"].items():
        top = ", ".join(f"`{os.path.basename(p)}` {n}" for n, p in d["top_files"][:3])
        o.append(f"| {name} | {d['normalized_lines']:,} | {d['duplicated_lines']:,} | {pct(d['rate'])} | {top} |")
    o.append("")
    o.append("### Circular dependencies")
    o.append("")
    rc = m["rust_module_cycles"]
    jc = m["js_file_cycles"]
    o.append(f"- Rust module-level (`src/<module>`, production code, `crate::`/`code_graph_mcp::` refs): **{len(rc)}**")
    for c in rc:
        o.append(f"  - {' ↔ '.join(sorted(c))}")
    o.append(f"- JS file-level (`require('./…')`, production files): **{len(jc)}**")
    for c in jc:
        o.append(f"  - {' ↔ '.join(sorted(os.path.basename(p) for p in c))}")
    o.append("")
    o.append("Rust module dependency edges (module → modules it uses):")
    o.append("")
    o.append("```")
    for k, v in m["rust_module_graph"].items():
        o.append(f"{k:>10} → {', '.join(v) if v else '(none)'}")
    o.append("```")
    o.append("")
    st = m["static_test_counts"]
    o.append("### Static test counts")
    o.append("")
    o.append(
        f"- `#[test]`: src {st['rust_test_attr_src']} + tests/ {st['rust_test_attr_tests']} "
        f"= **{st['rust_test_attr_src'] + st['rust_test_attr_tests']}**"
    )
    o.append(f"- node:test `test(`/`it(` calls in `*.test.js`: **{st['js_test_calls']}**")
    return "\n".join(o)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true", help="print raw numbers as JSON")
    args = ap.parse_args()
    m = collect()
    if args.json:
        json.dump(m, sys.stdout, indent=2, default=list)
        print()
    else:
        print(render(m))


if __name__ == "__main__":
    main()
