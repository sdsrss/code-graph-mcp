"""RRF-layer end-to-end A/B: minilm vs coderank vector channel, SAME real FTS-BM25 channel.

Reproduces the production hybrid pipeline's RRF layer (src/mcp/server/tools/search.rs +
src/search/fusion.rs) to isolate the embedding variable end-to-end (i.e. AFTER BM25 fusion,
which is the main thing that dilutes a vector-only gain):

  fts_channel  = REAL nodes_fts BM25 (bm25 weights 5,3,2,2,1,5,1,1; AND-first, OR fallback)
  vec_channel  = cosine KNN over context_string embeddings (minilm 384d  vs  coderank 768d)
  fuse         = weighted RRF, k=30, fts_w=1.0, vec_w=1.2  (blend term omitted — fusion.rs
                 proves it's a bounded tie-breaker that never flips adjacent RRF ranks)
  fetch_count  = (top_k*4).max(20) = 80 per channel; final top_k=20

NOT modeled (honest boundary): the Phase-2 adjusted-score re-rank (name_boost × size ×
doc_penalty) that sits ABOVE RRF. It's a second-order layer applied identically to both
arms; the primary dilution source (BM25 fusion) IS captured here. acronym expansion skipped
(NL doc-comment queries don't trigger it; weights stay default 1.0/1.2). A true full-pipeline
number needs binary integration (768-d schema + re-index) — see project_cocoindex memory.
"""
import argparse
import json
import sqlite3
import sys

import numpy as np
from metrics import ndcg_at_k, recall_at_k, reciprocal_rank
from eval_retrieval import Backend
from leakage import is_leaked

DB_NS = 10_000_000
STOP = {"a", "an", "and", "the", "or", "in", "of", "for", "to", "with", "is", "it", "this",
        "that", "by", "from", "on", "at", "as", "be", "are", "was", "were", "been", "all",
        "each", "how", "what", "when"}
BM25 = "bm25(nodes_fts, 5.0, 3.0, 2.0, 2.0, 1.0, 5.0, 1.0, 1.0)"
K, FW, VW, FETCH, TOPK = 30, 1.0, 1.2, 80, 20


def split_identifier(name: str) -> str:
    """Port of src/search/tokenizer.rs split_identifier (camelCase/snake/acronym + original)."""
    parts, cur = [], ""
    chars = list(name)
    n = len(chars)
    i = 0
    while i < n:
        c = chars[i]
        if c == "_":
            if cur:
                parts.append(cur)
                cur = ""
            i += 1
            continue
        if c.isupper() and cur:
            last_lower = cur[-1].islower()
            acronym_end = cur[-1].isupper() and i + 1 < n and chars[i + 1].islower()
            if last_lower or acronym_end:
                parts.append(cur)
                cur = ""
        cur += c
        i += 1
    if cur:
        parts.append(cur)
    if name not in parts:
        parts.append(name)
    return " ".join(parts)


def build_terms(query: str) -> list[str]:
    terms = set()
    for w in query.split():
        if w.lower() in STOP:
            continue
        for piece in split_identifier(w).split():
            san = "".join(c for c in piece if c.isalnum() or c == "_")
            if len(san) >= 2:
                terms.add(san)
    return sorted(terms)


def fts_search(conn, query: str, limit: int = FETCH) -> list[int]:
    """Real FTS-BM25 ranked local ids; mirrors fts5_search_impl AND-first → OR fallback."""
    terms = build_terms(query)
    if not terms:
        return []
    quoted = [f'"{t}"' for t in terms]
    sql = (f"SELECT fts.rowid, {BM25} FROM nodes_fts fts JOIN nodes n ON n.id = fts.rowid "
           f"WHERE nodes_fts MATCH ? AND n.is_test = 0 ORDER BY {BM25} LIMIT ?")
    if len(terms) > 1:
        rows = conn.execute(sql, (" AND ".join(quoted), limit)).fetchall()
        if len(rows) >= max(3, limit // 10):
            return [r[0] for r in rows]
    rows = conn.execute(sql, (" OR ".join(quoted), limit)).fetchall()
    return [r[0] for r in rows]


def rrf(fts_ids: list[int], vec_ids: list[int]) -> list[int]:
    score: dict[int, float] = {}
    for rank, nid in enumerate(fts_ids):
        score[nid] = score.get(nid, 0.0) + FW / (K + rank + 1)
    for rank, nid in enumerate(vec_ids):
        score[nid] = score.get(nid, 0.0) + VW / (K + rank + 1)
    return [nid for nid, _ in sorted(score.items(), key=lambda x: -x[1])][:TOPK]


def agg(rows: list[dict]) -> dict:
    if not rows:
        return {"n": 0}
    keys = ["ndcg@10", "recall@1", "recall@10", "mrr"]
    out = {k: round(float(np.mean([r[k] for r in rows])), 4) for k in keys}
    out["n"] = len(rows)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", action="append", required=True, help="frozen index.db, in build's db_idx order")
    ap.add_argument("--queries", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-leak", type=float, default=0.05,
                    help="fail when more than this fraction of doc-derived (bootstrap, keyword) "
                         "queries have their doc in the gold's context_string "
                         "(default 0.05; 1 accepts any)")
    args = ap.parse_args()

    queries = []
    with open(args.queries) as fh:
        for line in fh:
            line = line.strip()
            if line:
                queries.append(json.loads(line))
    by_db: dict[int, list] = {}
    for q in queries:
        g = q.get("gold_node_ids")
        if g:
            by_db.setdefault(g[0] // DB_NS, []).append(q)

    # Both channels here read the doc a bootstrap query was made from: the vector
    # channel embeds context_string (which carries `doc:`), and the FTS channel is
    # the real nodes_fts, which indexes doc_comment and context_string. Unlike
    # eval_retrieval.py there is no doc-free variant of the FTS side short of a
    # rebuilt index, so a leaked query set is refused rather than half-cleaned.
    # A keyword query is words picked out of that doc: always leaked here.
    boot = [q for q in queries if q.get("source") in ("bootstrap", "keyword")]
    leaked = 0
    for db_idx, qs in by_db.items():
        conn = sqlite3.connect(args.db[db_idx])
        for q in qs:
            if q.get("source") == "keyword":
                leaked += 1
                continue
            if q.get("source") != "bootstrap":
                continue
            for g in q["gold_node_ids"]:
                row = conn.execute("SELECT context_string FROM nodes WHERE id = ?",
                                   (g - db_idx * DB_NS,)).fetchone()
                if row and is_leaked(q["query"], (row[0] or "")[:2000]):
                    leaked += 1
                    break
        conn.close()
    leak_rate = leaked / len(boot) if boot else 0.0
    print(f"[rrf_ab] leakage: {leaked}/{len(boot)} doc-derived queries have their doc in their "
          f"gold's context_string ({leak_rate:.1%})", file=sys.stderr)
    if leak_rate > args.max_leak:
        raise SystemExit(
            f"[rrf_ab] leakage {leak_rate:.1%} > --max-leak {args.max_leak:.0%}: both channels "
            f"see the query's own doc. Run it on real_queries / tier3_slice queries, or pass "
            f"--max-leak 1 to measure the leaked number on purpose.")

    mb, cb = Backend("minilm"), Backend("coderank")
    arms = {"minilm": {"overall": [], "lang": {}}, "coderank": {"overall": [], "lang": {}}}

    for db_idx in sorted(by_db):
        conn = sqlite3.connect(args.db[db_idx])
        conn.row_factory = sqlite3.Row
        cur = conn.execute(
            "SELECT n.id, n.context_string FROM nodes n JOIN files f ON n.file_id = f.id "
            "WHERE n.is_test = 0 AND f.language IS NOT NULL AND n.context_string IS NOT NULL "
            "AND n.context_string != ''")
        cand_ids, cand_texts = [], []
        for r in cur:
            cand_ids.append(int(r["id"]))
            cand_texts.append(r["context_string"][:2000])
        print(f"[db{db_idx}] {len(cand_ids)} candidates; encoding...", file=sys.stderr)
        m_emb = mb.encode(cand_texts, is_query=False)
        c_emb = cb.encode(cand_texts, is_query=False)

        qs = by_db[db_idx]
        qm = mb.encode([q["query"] for q in qs], is_query=True)
        qc = cb.encode([q["query"] for q in qs], is_query=True)

        for qi, q in enumerate(qs):
            gold_local = [g - db_idx * DB_NS for g in q["gold_node_ids"]]
            fts_ids = fts_search(conn, q["query"])
            m_order = np.argsort(-(m_emb @ qm[qi]))[:FETCH]
            c_order = np.argsort(-(c_emb @ qc[qi]))[:FETCH]
            vec_m = [cand_ids[i] for i in m_order]
            vec_c = [cand_ids[i] for i in c_order]
            for arm, vec in (("minilm", vec_m), ("coderank", vec_c)):
                ranked = rrf(fts_ids, vec)
                rec = {
                    "ndcg@10": ndcg_at_k(ranked, {g: 1.0 for g in gold_local}, 10),
                    "recall@1": recall_at_k(ranked, gold_local, 1),
                    "recall@10": recall_at_k(ranked, gold_local, 10),
                    "mrr": reciprocal_rank(ranked, gold_local),
                }
                arms[arm]["overall"].append(rec)
                arms[arm]["lang"].setdefault(q.get("language", "?"), []).append(rec)
        conn.close()

    result = {arm: {"overall": agg(d["overall"]),
                    "by_language": {lg: agg(rs) for lg, rs in sorted(d["lang"].items())}}
              for arm, d in arms.items()}
    with open(args.out, "w") as fh:
        json.dump(result, fh, indent=2)

    mo, co = result["minilm"]["overall"], result["coderank"]["overall"]
    print(f"\n=== RRF-layer end-to-end A/B (real FTS-BM25 + vector, n={mo['n']}) ===")
    print(f"{'arm':10} {'NDCG@10':9} {'recall@1':9} {'recall@10':10} {'mrr':8}")
    for arm in ("minilm", "coderank"):
        o = result[arm]["overall"]
        print(f"{arm:10} {o['ndcg@10']:<9} {o['recall@1']:<9} {o['recall@10']:<10} {o['mrr']:<8}")
    print(f"\nΔ NDCG@10 = {co['ndcg@10'] - mo['ndcg@10']:+.4f}  "
          f"({(co['ndcg@10'] - mo['ndcg@10']) * 100:+.2f}pp)   "
          f"[vector-only was +2.49pp; this is after BM25 fusion]")
    print(f"Δ recall@1 = {co['recall@1'] - mo['recall@1']:+.4f}")


if __name__ == "__main__":
    main()
