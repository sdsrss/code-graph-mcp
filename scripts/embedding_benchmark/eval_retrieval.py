# scripts/embedding_benchmark/eval_retrieval.py
"""Evaluate one embedding backend on the labeled query set (vector-only ranking).

Usage:
  python eval_retrieval.py --backend minilm  --field context_string_nodoc \
      --db .code-graph/index.db --queries query_set.jsonl --out results/minilm_nodoc.json
  python eval_retrieval.py --backend potion  --field code_content \
      --db .code-graph/index.db --queries query_set.jsonl --out results/potion_code.json

`--field context_string_nodoc` is `context_string` with its `doc:` part removed
from EVERY candidate. Use it for the bootstrap (doc -> code) queries: on plain
`context_string` the gold carries its own query verbatim (see leakage.py), and
the run fails unless `--max-leak` is raised to accept that.
"""
import argparse
import json
import os
import sqlite3
import numpy as np

from leakage import is_leaked, strip_doc
from lexical import bm25_scores
from metrics import ndcg_at_k, recall_at_k, reciprocal_rank

MINILM_ID = "sentence-transformers/all-MiniLM-L6-v2"
MINILM_REV = "c9745ed1d9f2"  # must match src/embedding/model.rs:75
POTION_ID = "minishlab/potion-code-16M"
CODERANK_ID = "nomic-ai/CodeRankEmbed"            # MIT, 137M, 768-d, asymmetric (query/document prompts)
JINA_ID = "jinaai/jina-embeddings-v5-text-nano"   # CC-BY-NC-4.0 (non-commercial!), 210M base, asymmetric (task='retrieval')


def l2(mat: np.ndarray) -> np.ndarray:
    norms = np.linalg.norm(mat, axis=1, keepdims=True)
    norms[norms == 0] = 1.0
    return mat / norms


class Backend:
    """encode(texts, is_query): asymmetric backends (coderank, jina) prefix queries
    and documents differently — the standard retrieval gain for those models.
    Symmetric backends (minilm, potion) ignore is_query, so their output stays
    byte-identical to the committed baseline (no re-run needed to trust it)."""

    def __init__(self, name: str):
        self.name = name
        if name == "minilm":
            from sentence_transformers import SentenceTransformer
            self.model = SentenceTransformer(MINILM_ID, revision=MINILM_REV)
            self._encode = lambda texts, is_query: np.asarray(
                self.model.encode(texts, batch_size=64, show_progress_bar=False), dtype=np.float32)
        elif name == "potion":
            from model2vec import StaticModel
            self.model = StaticModel.from_pretrained(POTION_ID)
            self._encode = lambda texts, is_query: np.asarray(self.model.encode(texts), dtype=np.float32)
        elif name == "coderank":
            from sentence_transformers import SentenceTransformer
            self.model = SentenceTransformer(CODERANK_ID, trust_remote_code=True, device="cuda")
            self._encode = lambda texts, is_query: np.asarray(
                self.model.encode(texts, batch_size=64, show_progress_bar=False,
                                  prompt_name=("query" if is_query else "document")), dtype=np.float32)
        elif name == "jina":
            from sentence_transformers import SentenceTransformer
            self.model = SentenceTransformer(JINA_ID, trust_remote_code=True, device="cuda")
            self._encode = lambda texts, is_query: np.asarray(
                self.model.encode(texts, batch_size=64, show_progress_bar=False,
                                  task="retrieval", prompt_name=("query" if is_query else "document")),
                dtype=np.float32)
        else:
            raise SystemExit(f"unknown backend {name!r}")

    def encode(self, texts: list[str], is_query: bool = False) -> np.ndarray:
        if not texts:
            return np.zeros((0, 1), dtype=np.float32)
        return l2(self._encode(texts, is_query))  # explicit L2 — matches the Rust l2_normalize path


def load_candidates(dbs: list[str], field: str):
    """Return (global_ids, texts) for all non-test symbols across the DBs."""
    column = "context_string" if field == "context_string_nodoc" else field
    ids, texts = [], []
    for db_idx, db_path in enumerate(dbs):
        conn = sqlite3.connect(db_path)
        conn.row_factory = sqlite3.Row
        cur = conn.execute(
            f"""SELECT n.id, n.{column} AS text
                FROM nodes n JOIN files f ON n.file_id = f.id
                WHERE n.is_test = 0 AND f.language IS NOT NULL"""
        )
        for r in cur:
            ids.append(db_idx * 10_000_000 + int(r["id"]))
            text = r["text"] or ""
            if field == "context_string_nodoc":
                text = strip_doc(text)
            texts.append(text[:2000])  # cap to keep runtime bounded
        conn.close()
    return ids, texts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--backend", choices=["minilm", "potion", "coderank", "jina", "bm25"],
                    required=True, help="bm25 = lexical reference arm (lexical.py), no model")
    ap.add_argument("--field", choices=["context_string", "context_string_nodoc", "code_content"],
                    required=True)
    ap.add_argument("--db", action="append", required=True)
    ap.add_argument("--queries", default="query_set.jsonl")
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-leak", type=float, default=0.05,
                    help="fail when more than this fraction of doc-derived (bootstrap, "
                         "keyword) queries have their doc in the gold's text "
                         "(default 0.05; 1 accepts any)")
    args = ap.parse_args()

    ids, texts = load_candidates(args.db, args.field)

    queries = []
    with open(args.queries) as fh:
        for line in fh:
            line = line.strip()
            if line:
                queries.append(json.loads(line))

    # Leakage before any encoding cost: a leaked run is not worth the minutes.
    # Both doc-derived sources are checked. A bootstrap query is measured (its
    # first words, verbatim, in the gold's text). A keyword query is a few words
    # picked out of the doc, so no verbatim check sees it; it is leaked exactly
    # when the field still carries the doc, i.e. on plain context_string.
    text_by_id = dict(zip(ids, texts))
    doc_derived = [q for q in queries if q.get("source") in ("bootstrap", "keyword")]
    leaked = sum(
        1 for q in doc_derived
        if (args.field == "context_string" if q["source"] == "keyword" else
            any(is_leaked(q["query"], text_by_id.get(g, "")) for g in q["gold_node_ids"]))
    )
    leak_rate = leaked / len(doc_derived) if doc_derived else 0.0
    print(f"[eval] leakage: {leaked}/{len(doc_derived)} doc-derived queries have their doc in "
          f"the gold's {args.field} ({leak_rate:.1%})")
    if leak_rate > args.max_leak:
        raise SystemExit(
            f"[eval] leakage {leak_rate:.1%} > --max-leak {args.max_leak:.0%}: on this field the "
            f"doc-derived queries score string overlap, not retrieval. Use --field "
            f"context_string_nodoc, or pass --max-leak 1 to measure the leaked number on purpose.")

    q_texts = [q["query"] for q in queries]
    if args.backend == "bm25":
        print(f"[eval] scoring {len(texts)} candidates with bm25/{args.field}...")
        lex = np.asarray(bm25_scores(texts, q_texts), dtype=np.float32)  # (Q, N)
    else:
        backend = Backend(args.backend)
        print(f"[eval] encoding {len(texts)} candidates with {args.backend}/{args.field}...")
        cand = backend.encode(texts, is_query=False)  # (N, dim), L2-normalized
        q_emb = backend.encode(q_texts, is_query=True)  # (Q, dim), L2-normalized

    # Per-query: cosine == dot product on normalized vectors. Rank candidates, score.
    per_lang: dict[str, list[dict]] = {}
    per_source: dict[str, list[dict]] = {}
    overall: list[dict] = []
    for qi, q in enumerate(queries):
        sims = lex[qi] if args.backend == "bm25" else cand @ q_emb[qi]  # (N,)
        # stable sort by (-score, id): argsort on score desc, ties broken by id asc
        order = np.lexsort((np.array(ids), -sims))
        ranked = [ids[p] for p in order]
        gold = q["gold_node_ids"]
        gold_to_rel = {g: 1.0 for g in gold}
        rec = {
            "ndcg@10": ndcg_at_k(ranked, gold_to_rel, 10),
            "recall@1": recall_at_k(ranked, gold, 1),
            "recall@10": recall_at_k(ranked, gold, 10),
            "mrr": reciprocal_rank(ranked, gold),
        }
        overall.append(rec)
        per_lang.setdefault(q["language"], []).append(rec)
        per_source.setdefault(q["source"], []).append(rec)

    def agg(rows: list[dict]) -> dict:
        if not rows:
            return {"n": 0}
        keys = ["ndcg@10", "recall@1", "recall@10", "mrr"]
        out = {k: round(float(np.mean([r[k] for r in rows])), 4) for k in keys}
        out["n"] = len(rows)
        return out

    result = {
        "backend": args.backend,
        "field": args.field,
        "candidates": len(ids),
        "doc_derived_leak": {"leaked": leaked, "doc_derived": len(doc_derived),
                             "rate": round(leak_rate, 4)},
        "overall": agg(overall),
        "by_language": {lg: agg(rows) for lg, rows in sorted(per_lang.items())},
        "by_source": {src: agg(rows) for src, rows in sorted(per_source.items())},
    }
    out_dir = os.path.dirname(args.out)
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)
    with open(args.out, "w") as fh:
        json.dump(result, fh, indent=2)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
