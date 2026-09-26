# scripts/embedding_benchmark/build_query_set.py
"""Build a labeled retrieval query set from one or more .code-graph/index.db files.

Bootstrap strategy: a symbol with a non-trivial doc_comment becomes a (query=doc, gold=symbol)
pair. This is the doc->code retrieval task, auto-extracted, and naturally covers whatever
languages the indexed repos contain.

Usage:
  python build_query_set.py --db /path/a/.code-graph/index.db --db /path/b/.code-graph/index.db \
      --real real_queries.jsonl --out query_set.jsonl --min-doc-len 25
"""
import argparse
import json
import sqlite3
import sys

from leakage import is_leaked

def clean_doc(doc: str | None) -> str:
    """Strip comment markers so the doc reads as prose, not syntax."""
    doc = (doc or "").strip()
    doc = doc.replace("///", " ").replace("//!", " ").replace("//", " ")
    return doc.replace("/*", " ").replace("*/", " ").replace("*", " ").strip()


# node id namespacing: ids are per-DB, so prefix with a DB index to keep them globally unique.
def _rows(db_path: str, db_idx: int):
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cur = conn.execute(
        """
        SELECT n.id, n.name, n.qualified_name, n.type, n.code_content, n.context_string,
               n.doc_comment, n.is_test, f.path, f.language
        FROM nodes n JOIN files f ON n.file_id = f.id
        WHERE n.is_test = 0 AND f.language IS NOT NULL
        """
    )
    for r in cur:
        yield db_idx, dict(r)
    conn.close()


def build(dbs: list[str], min_doc_len: int):
    queries = []
    skipped_in_body = 0
    # Map (db_idx, name)/(db_idx, qualified_name) -> global id, for resolving real-query hint_symbol.
    name_index: dict[tuple[int, str], int] = {}
    for db_idx, db_path in enumerate(dbs):
        for _, row in _rows(db_path, db_idx):
            gid = db_idx * 10_000_000 + int(row["id"])  # global id namespacing
            if row["name"]:
                name_index.setdefault((db_idx, row["name"]), gid)
            if row["qualified_name"]:
                name_index.setdefault((db_idx, row["qualified_name"]), gid)
            doc = clean_doc(row["doc_comment"])
            if len(doc) >= min_doc_len and row["name"] and row["name"] not in doc:
                # Exclude docs that just restate the symbol name (trivial match).
                # Exclude docs that sit INSIDE the body (a Python docstring):
                # code_content then carries the query verbatim, and no field the
                # evaluator can strip removes it.
                if is_leaked(doc, row["code_content"] or ""):
                    skipped_in_body += 1
                    continue
                queries.append({
                    "query_id": f"doc:{gid}",
                    "query": " ".join(doc.split()[:75]),
                    "gold_node_ids": [gid],
                    "source": "bootstrap",
                    "language": row["language"],
                })
    if skipped_in_body:
        print(f"[build_query_set] skipped {skipped_in_body} bootstrap queries whose doc is inside "
              "the body (code_content carries the query verbatim)", file=sys.stderr)
    return queries, name_index


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", action="append", required=True, help="path to a .code-graph/index.db")
    ap.add_argument("--real", help="path to real_queries.jsonl (hint_symbol resolved against db 0)")
    ap.add_argument("--out", default="-")
    ap.add_argument("--min-doc-len", type=int, default=25)
    args = ap.parse_args()

    queries, name_index = build(args.db, args.min_doc_len)

    if args.real:
        with open(args.real) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                obj = json.loads(line)
                # Resolve hint_symbol against db 0 if gold not already set.
                if not obj.get("gold_node_ids") and obj.get("hint_symbol"):
                    gid = name_index.get((0, obj["hint_symbol"]))
                    if gid is not None:
                        obj["gold_node_ids"] = [gid]
                if obj.get("gold_node_ids"):
                    obj.pop("hint_symbol", None)
                    queries.append(obj)
                else:
                    print(f"[warn] real query {obj['query_id']} has no resolvable gold; skipped",
                          file=sys.stderr)

    out = sys.stdout if args.out == "-" else open(args.out, "w")
    for q in queries:
        out.write(json.dumps(q, ensure_ascii=False) + "\n")
    if out is not sys.stdout:
        out.close()
    # Summary to stderr (counts by language + source) so the executor sees coverage.
    by_lang: dict[str, int] = {}
    by_src: dict[str, int] = {}
    for q in queries:
        by_lang[q["language"]] = by_lang.get(q["language"], 0) + 1
        by_src[q["source"]] = by_src.get(q["source"], 0) + 1
    print(f"[build_query_set] {len(queries)} queries; by language={by_lang}; by source={by_src}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
