---
status: implemented
level: L2
roadmap: COMPETITIVE-ANALYSIS-2026-09-25 #1(a), second half
---

# Retrieval eval: keyword and partial-identifier slices

## Why

The leak-free doc->code baseline (`d46d9fd`, minilm NDCG@10 0.4082) measures
one query shape: a full doc comment. Agents also search with 2-3 keywords and
with fragments of an identifier. CoREB (arXiv 2605.04615, paper-reported)
says dense models collapse to near-zero nDCG on keyword queries; we have no
number of our own for either shape.

## Accepted shapes (the builder emits exactly these)

partial_identifier (source/query_class `partial_identifier`):
- name split into subtokens (snake, camel, acronym, digits); names with fewer
  than 3 subtokens emit nothing (a 2-subtoken run would be the whole name,
  which is the tier3 exact_symbol slice).
- query = 2 contiguous subtokens, lowercase, space-joined.
- a run with a subtoken shorter than 2 chars, all digits, or a function word
  (closed list: an and as at by for from if in into is it of on or the to
  with) is not a query. English stopwords are NOT function words here:
  make/use/out/per are real identifier words.
- gold = every eligible symbol whose subtoken list contains the run
  contiguously; a run with more than 3 golds is too vague and is skipped.
- per symbol, the run with the fewest golds wins; ties go to the first run.
- the same query text is emitted once.

keyword (source/query_class `keyword`):
- same eligibility as the bootstrap doc set (doc >= 25 chars, name not in the
  doc, doc not inside the body), plus the binary's test-symbol exclusion.
- words = plain words of the doc's first sentence (capped at 40 words; the
  summary sentence reads like a query, later ones drift into caveats): lowercase or Capitalized
  alphabetic, >= 3 chars, not a stopword, not a subtoken of the symbol's own
  name; `backticked` spans, camelCase, snake_case and ALLCAPS are identifiers,
  not keywords.
- query = the 3 highest-IDF words (IDF over all eligible docs; ties by
  position), in doc order; fewer than 2 words emits nothing.
- gold = the symbol.

Both: code types only (tier3's CODE_TYPES), identifier-ish names only (tier3's
`replace("_", "").isalnum()`, which drops `<external>` placeholders like
`std::io::Write`), non-test by column AND by the binary's is_test_symbol.

keyword queries are synthetic (picked by IDF, not written by a person); a
share of them read as word salad ("side place", "try take"). The slice
measures short bag-of-words queries, not human keyword phrasing.

## Leakage

keyword queries come from the doc, so on plain `context_string` they are
leaked by construction; eval_retrieval counts them as leaked on that field.
partial_identifier queries are meant to match the name; they are not leak-
checked.

## Acceptance

- tests/embedding_benchmark/test_short_slices.py covers every shape above;
  each rule has a mutation that turns it red (23/23).
- eval_retrieval.py reports per-source aggregates, and has a `bm25` reference
  arm (lexical.py, stdlib) so a slice's dense number can be read against plain
  term overlap on the same candidates.
- README gets the minilm/potion x context_string_nodoc/code_content table for
  both slices, with the bootstrap row alongside.
