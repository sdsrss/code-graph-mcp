import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2] / "scripts" / "embedding_benchmark"))
from lexical import bm25_scores, tokens


def test_tokens_split_identifiers_into_subtokens():
    assert tokens("fn parseConfigFile(path) -> load_user_v2") == [
        "fn", "parse", "config", "file", "path", "load", "user", "v", "2"]


def test_bm25_ranks_the_doc_holding_the_rare_term_first():
    docs = ["parse config file", "load config file", "config config config", "unrelated text"]
    s = bm25_scores(docs, ["parse config"])[0]
    assert s[0] > s[1] > 0          # "parse" is rare, "config" is everywhere
    assert s[3] == 0.0              # no shared term, no score
    assert len(s) == len(docs)


def test_bm25_repeated_term_saturates_and_long_docs_are_penalized():
    docs = ["config", "config config config config config config", "config " + "pad " * 40]
    s = bm25_scores(docs, ["config"])[0]
    assert s[1] < 2 * s[0]           # tf saturates (k1)
    assert s[2] < s[0]               # length normalization (b)


def test_bm25_query_term_absent_from_corpus_scores_zero():
    s = bm25_scores(["alpha beta"], ["gamma"])[0]
    assert s == [0.0]


def test_bm25_a_rare_term_outweighs_a_common_one():
    docs = ["parse x", "config y", "config z", "config w"]
    s = bm25_scores(docs, ["parse config"])[0]
    assert s[0] > s[1]


def test_bm25_repeating_a_query_term_adds_nothing():
    docs = ["config file", "other"]
    assert bm25_scores(docs, ["config config"]) == bm25_scores(docs, ["config"])
