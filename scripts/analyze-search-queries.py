#!/usr/bin/env python3
"""Classify `code-graph-mcp search` queries from Claude Code transcripts.

Reads JSONL transcripts from ~/.claude/projects/<encoded-cwd>/*.jsonl, extracts
the positional query of every `code-graph-mcp search …` Bash invocation issued
by the agent, and buckets each query as keyword-like or concept-like.

Use to decide whether a hybrid (vector+FTS) CLI mode would pay off: if concept
queries are rare, FTS-only is already the right path and Claude's CLI preference
reflects a correct cost/benefit judgement.
"""

import json, re, shlex, sys
from collections import Counter
from pathlib import Path

SPLIT_PAT = re.compile(r'&&|\|\||;|\|')
# Shell redirect tokens shlex returns as single tokens: `<`, `>`, `>>`, `2>`,
# `2>&1`, `>&2`, `<<`, `>file`, `2>file`, etc. Any token whose leading chars are
# optional-digits + one-or-more `<>` is a redirect, not a search query.
REDIRECT_PAT = re.compile(r'^\d*[<>]+')
IDENT_STRICT = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*$')
IDENT_LOOSE  = re.compile(r'^[A-Za-z_][\w:./\-]*$')
FLAG_WITH_VAL = {'--limit','--top-k','--language','--node-type','--json-out'}
STOP_EN = {'the','a','an','of','in','on','to','for','is','are','was','were','and','or','but',
           'how','what','where','why','who','which','this','that','it','its','has','have','had',
           'do','does','did','can','should','would','by','from','with','about',
           'find','finds','uses','used','using','when','related','looks','lookup','show','shows'}


def extract_search_queries(cmd: str):
    out = []
    for seg in SPLIT_PAT.split(cmd):
        seg = seg.strip()
        if 'code-graph-mcp' not in seg or ' search' not in seg:
            continue
        try:
            toks = shlex.split(seg, posix=True)
        except ValueError:
            toks = seg.split()
        for i, t in enumerate(toks):
            if not (t.endswith('code-graph-mcp') and i+1 < len(toks) and toks[i+1] == 'search'):
                continue
            j = i + 2
            while j < len(toks):
                tk = toks[j]
                if REDIRECT_PAT.match(tk):        # skip `2>&1`, `>file`, etc.
                    j += 1
                    continue
                if tk.startswith('--'):
                    j += 2 if tk in FLAG_WITH_VAL else 1
                    continue
                out.append(tk)
                break
            break
    return out


def has_cjk(s: str) -> bool:
    return any('一' <= c <= '鿿' for c in s)


def classify(q: str):
    s = q.strip()
    if (s.startswith('"') and s.endswith('"')) or (s.startswith("'") and s.endswith("'")):
        s = s[1:-1]
    if not s:
        return 'empty', s
    tokens = s.split()
    if len(tokens) == 1:
        t = tokens[0]
        if IDENT_STRICT.match(t):
            return 'kw-single', s
        if IDENT_LOOSE.match(t) and any(c in t for c in ':./-'):
            return 'kw-qualified', s
        if has_cjk(t):
            return 'concept-cjk', s
        return 'kw-word', s
    if has_cjk(s):
        return 'concept-cjk', s
    ident_ratio = sum(1 for t in tokens if IDENT_STRICT.match(t) or IDENT_LOOSE.match(t)) / len(tokens)
    has_stop = any(w in STOP_EN for w in re.findall(r"\b[a-z]+\b", s.lower()))
    if has_stop:
        return 'concept-nl', s
    return 'kw-multi' if ident_ratio >= 0.7 else 'concept-nl', s


def iter_tool_uses(transcript_dir: Path):
    for f in sorted(transcript_dir.glob('*.jsonl')):
        for line in open(f, errors='replace'):
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:
                continue
            msg = rec.get('message') or {}
            content = msg.get('content')
            if not isinstance(content, list):
                continue
            for item in content:
                if isinstance(item, dict) and item.get('type') == 'tool_use':
                    yield item


def main():
    project = Path.cwd().resolve()
    # Claude Code slug: replace every non-[A-Za-z0-9-] char with '-' (underscores
    # and slashes both become dashes). E2E convention confirmed in memory.
    encoded = re.sub(r'[^A-Za-z0-9-]', '-', str(project))
    tdir = Path.home() / '.claude' / 'projects' / encoded
    if not tdir.is_dir():
        print(f"No transcripts at {tdir}", file=sys.stderr)
        return 1

    queries = []
    for item in iter_tool_uses(tdir):
        if item.get('name') != 'Bash':
            continue
        inp = item.get('input') or {}
        cmd = inp.get('command', '') if isinstance(inp, dict) else ''
        if cmd:
            queries.extend(extract_search_queries(cmd))

    total = len(queries)
    if total == 0:
        print(f"No `code-graph-mcp search` invocations found in {tdir}")
        return 0

    buckets = Counter()
    by_bucket: dict[str, list[str]] = {}
    for q in queries:
        b, s = classify(q)
        buckets[b] += 1
        by_bucket.setdefault(b, []).append(s)

    print(f"Project: {project}")
    print(f"Transcripts: {len(list(tdir.glob('*.jsonl')))}  search queries: {total}\n")
    print(f"{'bucket':<18}{'n':>5}{'%':>7}")
    for b, n in buckets.most_common():
        print(f"  {b:<16}{n:>5}{100*n/total:>6.1f}%")
    kw = sum(n for b, n in buckets.items() if b.startswith('kw'))
    concept = sum(n for b, n in buckets.items() if b.startswith('concept'))
    empty = buckets.get('empty', 0)
    denom = kw + concept
    print(f"\n  KEYWORD : {kw} ({100*kw/denom:.1f}% of classified)" if denom else "")
    print(f"  CONCEPT : {concept} ({100*concept/denom:.1f}% of classified)" if denom else "")
    if empty:
        print(f"  empty   : {empty} (excluded from ratio)")

    print('\n--- bucket samples (top 20 per bucket) ---')
    for b, _ in buckets.most_common():
        samples = Counter(by_bucket[b]).most_common(20)
        print(f"\n[{b}] ({buckets[b]}):")
        for q, n in samples:
            tag = f' ×{n}' if n > 1 else ''
            print(f"  {q!r}{tag}")
    return 0


if __name__ == '__main__':
    sys.exit(main())
