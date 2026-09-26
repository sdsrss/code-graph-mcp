# scripts/embedding_benchmark/lexical.py
"""BM25 over identifier subtokens: the lexical reference arm for eval_retrieval.py.
Stdlib only.

Not the production FTS: nodes_fts is `porter unicode61`, which stems and splits
snake_case but not camelCase. This arm answers a narrower question — does the
query's wording appear in the gold's text at all — so a dense model's number on a
slice can be read against what plain term overlap gets on the same candidates.
"""
import math
import re

from build_short_slices import subtokens

K1 = 1.2
B = 0.75

_WORDISH = re.compile(r"[A-Za-z0-9_]+")


def tokens(text: str) -> list[str]:
    """Every word-ish run of the text, split into lowercase subtokens."""
    return [t for w in _WORDISH.findall(text or "") for t in subtokens(w)]


def bm25_scores(docs: list[str], queries: list[str]) -> list[list[float]]:
    """scores[q][d] for each query against each doc (Lucene's non-negative IDF)."""
    doc_toks = [tokens(d) for d in docs]
    n = len(docs)
    avgdl = (sum(len(t) for t in doc_toks) / n) if n else 0.0
    postings: dict[str, list[tuple[int, int]]] = {}
    for di, toks in enumerate(doc_toks):
        tf: dict[str, int] = {}
        for t in toks:
            tf[t] = tf.get(t, 0) + 1
        for t, c in tf.items():
            postings.setdefault(t, []).append((di, c))
    norm = [K1 * (1 - B + B * len(t) / avgdl) if avgdl else K1 for t in doc_toks]
    out: list[list[float]] = []
    for q in queries:
        scores = [0.0] * n
        for t in set(tokens(q)):
            plist = postings.get(t)
            if not plist:
                continue
            idf = math.log(1 + (n - len(plist) + 0.5) / (len(plist) + 0.5))
            for di, c in plist:
                scores[di] += idf * c * (K1 + 1) / (c + norm[di])
        out.append(scores)
    return out
