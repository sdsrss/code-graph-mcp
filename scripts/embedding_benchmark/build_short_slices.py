#!/usr/bin/env python3
"""Build the two short-query retrieval slices from one or more .code-graph/index.db
files. Stdlib only.

- partial_identifier: two contiguous subtokens of a symbol name, space-joined
  (`parse_config_file` -> "parse config"). Gold = every symbol whose name holds
  that run; runs matching more than MAX_PARTIAL_GOLDS symbols are too vague.
- keyword: the 3 highest-IDF plain words of the first sentence of a symbol's doc
  comment, in doc order ("drop scratch changed"). Synthetic: picked by IDF, not
  written by a person. Gold = the symbol. The doc is the source, so on plain
  `context_string` (which embeds the doc) these are leaked by construction; score
  them on `context_string_nodoc` or `code_content`.

Accepted shapes: tasks/specs/retrieval-short-query-slices.md.

Usage:
  python3 build_short_slices.py --db .code-graph/index.db --out short_slices.jsonl
"""
import argparse
import json
import math
import re
import sqlite3
import sys

from build_query_set import clean_doc
from build_tier3_slice import CODE_TYPES, DB_NS, is_test_symbol
from leakage import is_leaked

MAX_PARTIAL_GOLDS = 3
KEYWORDS_PER_QUERY = 3
MIN_KEYWORDS = 2
KEYWORD_DOC_WORDS = 40

_SUBTOKEN = re.compile(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|\d+")
_BACKTICKED = re.compile(r"`[^`]*`")
# Plain words only: lowercase or Capitalized, bounded by non-word chars, so
# camelCase, snake_case and ALLCAPS identifiers never match.
_PLAIN_WORD = re.compile(r"\b[A-Za-z][a-z]{2,}\b")
_SENTENCE_END = re.compile(r"[.!?](?=\s|$)")

STOPWORDS = frozenset("""
about above across after again against all also although among and another any are
around because been before being below between both but can cannot could did does
doing done down during each either else even every few for from further had has
have having her here hers him his how however into its itself just least less like
made make makes many may might more most much must near need needs neither never
nor not now off once one only onto other others otherwise our ours out over own per
rather same shall she should since some such than that the their theirs them then
there these they this those though through thus too under until upon use used uses
using very via was way well were what when where whether which while who whom whose
why will with within without would yet you your yours
""".split())
# A partial-identifier run holding one of these ("nodes by") names no concept.
# A closed list of its own, not STOPWORDS: make/use/out/per/one are real
# identifier words (make_task, use_cache, out_dir, per_file).
FUNCTION_SUBTOKENS = frozenset(
    "an and as at by for from if in into is it of on or the to with".split())


def subtokens(name: str) -> list[str]:
    """snake / camel / acronym / digit split, lowercased."""
    return [t.lower() for t in _SUBTOKEN.findall(name or "")]


def keyword_words(doc: str, own_name: str) -> list[str]:
    """Candidate keywords of a doc's first sentence, in order, first occurrence only.
    The first sentence is the summary; later ones drift into caveats."""
    own = set(subtokens(own_name))
    text = " ".join(_BACKTICKED.sub(" ", doc).split())
    end = _SENTENCE_END.search(text)
    if end:
        text = text[:end.start()]
    text = " ".join(text.split()[:KEYWORD_DOC_WORDS])
    out: list[str] = []
    for w in _PLAIN_WORD.findall(text):
        w = w.lower()
        if w in STOPWORDS or w in own or w in out:
            continue
        out.append(w)
    return out


def _runs(tokens: list[str]) -> list[tuple[str, str]]:
    """Every 2-subtoken run that can be a query; none for names under 3 subtokens."""
    if len(tokens) < 3:
        return []
    return [(a, b) for a, b in zip(tokens, tokens[1:])
            if all(len(t) >= 2 and not t.isdigit() and t not in FUNCTION_SUBTOKENS
                   for t in (a, b))]


def _rows(db_path: str):
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT n.id, n.name, n.type, n.doc_comment, n.code_content, f.language, f.path
        FROM nodes n JOIN files f ON n.file_id = f.id
        WHERE n.is_test = 0 AND f.language IS NOT NULL
        """
    ).fetchall()
    conn.close()
    return [
        r for r in rows
        if r["type"] in CODE_TYPES and r["name"]
        and r["name"].replace("_", "").isalnum()  # identifier-ish, as tier3; drops std::io::Write
        and not is_test_symbol(r["name"], r["path"])  # match the binary's exclusion
    ]


def _partial_identifier(rows, db_idx: int) -> list[dict]:
    toks = {r["id"]: subtokens(r["name"]) for r in rows}
    holders: dict[tuple[str, str], set[int]] = {}
    for nid, ts in toks.items():
        for run in zip(ts, ts[1:]):
            holders.setdefault(run, set()).add(nid)
    lang = {r["id"]: r["language"] for r in rows}
    out: list[dict] = []
    seen: set[str] = set()
    for r in sorted(rows, key=lambda r: r["id"]):
        usable = [run for run in _runs(toks[r["id"]]) if len(holders[run]) <= MAX_PARTIAL_GOLDS]
        if not usable:
            continue
        run = min(usable, key=lambda run: len(holders[run]))  # min() keeps the first on ties
        query = " ".join(run)
        if query in seen:
            continue
        seen.add(query)
        gold = sorted(holders[run])
        out.append({
            "query_id": f"part:{db_idx * DB_NS + gold[0]}:{query.replace(' ', '_')}",
            "query": query,
            "gold_node_ids": [db_idx * DB_NS + g for g in gold],
            "source": "partial_identifier",
            "query_class": "partial_identifier",
            "language": lang[r["id"]],
        })
    return out


def _keyword(rows, db_idx: int, min_doc_len: int) -> list[dict]:
    eligible = []
    for r in sorted(rows, key=lambda r: r["id"]):
        doc = clean_doc(r["doc_comment"])
        # Same gate as the bootstrap doc set (build_query_set.py).
        if (len(doc) < min_doc_len or r["name"] in doc
                or is_leaked(doc, r["code_content"] or "")):
            continue
        eligible.append((r, keyword_words(doc, r["name"])))
    df: dict[str, int] = {}
    for _, words in eligible:
        for w in words:
            df[w] = df.get(w, 0) + 1
    n = len(eligible)
    out: list[dict] = []
    for r, words in eligible:
        ranked = sorted(range(len(words)), key=lambda i: (-math.log(n / df[words[i]]), i))
        picked = sorted(ranked[:KEYWORDS_PER_QUERY])
        if len(picked) < MIN_KEYWORDS:
            continue
        gid = db_idx * DB_NS + int(r["id"])
        out.append({
            "query_id": f"kw:{gid}",
            "query": " ".join(words[i] for i in picked),
            "gold_node_ids": [gid],
            "source": "keyword",
            "query_class": "keyword",
            "language": r["language"],
        })
    return out


def build(dbs: list[str], min_doc_len: int = 25) -> list[dict]:
    queries: list[dict] = []
    for db_idx, db_path in enumerate(dbs):
        rows = _rows(db_path)
        queries += _partial_identifier(rows, db_idx)
        queries += _keyword(rows, db_idx, min_doc_len)
    return queries


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", action="append", required=True, help="path to a .code-graph/index.db")
    ap.add_argument("--out", default="-")
    ap.add_argument("--min-doc-len", type=int, default=25)
    args = ap.parse_args()

    queries = build(args.db, args.min_doc_len)
    out = sys.stdout if args.out == "-" else open(args.out, "w")
    for q in queries:
        out.write(json.dumps(q, ensure_ascii=False) + "\n")
    if out is not sys.stdout:
        out.close()

    counts: dict[tuple[str, str], int] = {}
    for q in queries:
        key = (q["query_class"], q["language"])
        counts[key] = counts.get(key, 0) + 1
    for cls in ("partial_identifier", "keyword"):
        by_lang = {lg: c for (qc, lg), c in sorted(counts.items()) if qc == cls}
        print(f"[build_short_slices] {sum(by_lang.values())} {cls} queries; by language={by_lang}",
              file=sys.stderr)


if __name__ == "__main__":
    main()
