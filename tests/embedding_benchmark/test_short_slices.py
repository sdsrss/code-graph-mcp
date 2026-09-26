import sqlite3, sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2] / "scripts" / "embedding_benchmark"))
from build_short_slices import build, subtokens, keyword_words


# (id, file_id, name, type, is_test, doc_comment, code_content)
NODES = [
    # partial_identifier: "parse config" has 2 golds (parse_config has only 2
    # subtokens, so it is a gold but never a source); "config file" has 3.
    (10, 1, "parse_config_file", "function", 0, None, "fn parse_config_file() {}"),
    (11, 1, "parse_config", "function", 0, None, "fn parse_config() {}"),
    (12, 1, "load_config_file", "function", 0, None, "fn load_config_file() {}"),
    (13, 1, "save_config_file", "function", 0, None, "fn save_config_file() {}"),
    # fewest golds beats first: "config sync" (1) over "load config" (2).
    (14, 1, "load_config_sync", "function", 0, None, ""),
    # camelCase + acronym
    (20, 2, "HTTPServerBuilder", "class", 0, None, "class HTTPServerBuilder {}"),
    # "make task" has 4 golds (> 3): each symbol falls back to its own run.
    (30, 1, "make_task_alpha", "function", 0, None, ""),
    (31, 1, "make_task_beta", "function", 0, None, ""),
    (32, 1, "make_task_gamma", "function", 0, None, ""),
    (33, 1, "make_task_delta", "function", 0, None, ""),
    # its only usable run is "make task" (5 golds): no query at all.
    (34, 1, "make_task_x", "function", 0, None, ""),
    # 1-char and digit subtokens never form a run.
    (40, 1, "get_x_value", "function", 0, None, ""),
    # both pick "load user" with the same gold set: emitted once.
    (50, 1, "load_user_profile", "function", 0, None, ""),
    (51, 1, "load_user_profile_v2", "function", 0, None, ""),
    # a run holding a function word is not a query, even when first.
    (41, 1, "to_file_index", "function", 0, None, ""),
    # an English stopword that is a real identifier word stays a query word.
    (43, 1, "use_cache_entry", "function", 0, None, ""),
    # a digit subtoken is not a query word, even when first.
    (42, 1, "sha256_digest_hex", "function", 0, None, ""),
    # excluded as source and as gold: test path, test name, non-code type,
    # non-identifier name (an <external> placeholder).
    (63, 5, "std::io::Write", "trait", 0, None, ""),
    (60, 3, "parse_config_helper", "function", 0, None, ""),
    (61, 1, "test_parse_config_stub", "function", 0, None, ""),
    (62, 1, "PARSE_CONFIG_LIMIT", "constant", 0, None, ""),
    # keyword sources
    (70, 1, "rebuild_index", "function", 0,
     "/// Drop every table and rebuild the index from scratch when the schema version changed.",
     "fn rebuild_index() { drop_tables(); }"),
    (71, 1, "migrate_store", "function", 0,
     "/// Upgrade the table layout when the schema version is older than ours.\n"
     "/// Callers must hold the writer lock first.",
     "fn migrate_store() {}"),
    (72, 1, "resolve_edges", "function", 0,
     "/// Resolve `fooBar` callers using rewritePlan across crate boundaries.",
     "fn resolve_edges() {}"),
    (73, 1, "shortdoc", "function", 0, "/// The rewritePlan of `x` and HTTP tables.", ""),
    (74, 4, "pydoc_fn", "function", 0,
     "Compute the weighted rank of every candidate node.",
     'def pydoc_fn():\n    """Compute the weighted rank of every candidate node."""'),
    (75, 3, "walk_fixture_tree", "function", 0,
     "/// Walk the fixture tree breadth first and collect leaves.", ""),
    (76, 1, "cached_lookup", "function", 0,
     "/// cached_lookup returns the cached value for this key.", ""),
    (77, 1, "brief", "function", 0, "/// Flush queued rows.", ""),
]


def _make_db(path):
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT);
        CREATE TABLE nodes (id INTEGER PRIMARY KEY, file_id INTEGER, name TEXT,
                            qualified_name TEXT, type TEXT, is_test INTEGER,
                            doc_comment TEXT, code_content TEXT);
        INSERT INTO files VALUES (1, 'src/a.rs', 'rust'), (2, 'src/b.ts', 'typescript'),
                                 (3, 'tests/helpers.rs', 'rust'), (4, 'lib/m.py', 'python'),
                                 (5, '<external>', 'external');
        """
    )
    conn.executemany(
        "INSERT INTO nodes (id, file_id, name, type, is_test, doc_comment, code_content) "
        "VALUES (?, ?, ?, ?, ?, ?, ?)", NODES)
    conn.commit()
    conn.close()


def _built(tmp_path):
    db = str(tmp_path / "index.db")
    _make_db(db)
    out = build([db])
    by_class: dict[str, dict[str, dict]] = {}
    for q in out:
        by_class.setdefault(q["query_class"], {})[q["query"]] = q
    return out, by_class


def test_subtokens_split_snake_camel_acronym_digits():
    assert subtokens("parse_config_file") == ["parse", "config", "file"]
    assert subtokens("HTTPServerBuilder") == ["http", "server", "builder"]
    assert subtokens("loadUserProfileV2") == ["load", "user", "profile", "v", "2"]
    assert subtokens("__init__") == ["init"]


def test_partial_identifier_queries_and_golds(tmp_path):
    _, by = _built(tmp_path)
    pi = by["partial_identifier"]
    assert set(pi) == {
        "parse config",   # from parse_config_file: 2 golds beats "config file" (3)
        "load config",    # from load_config_file
        "config sync",    # from load_config_sync
        "save config",    # from save_config_file
        "http server",    # tie (1 vs 1) -> first run
        "task alpha", "task beta", "task gamma", "task delta",  # "make task" has 4 golds
        "load user",      # emitted once for both load_user_profile*
        "file index",     # "to file" holds a function word
        "digest hex",     # "sha 256" / "256 digest" hold a digit subtoken
        "use cache",      # "use" is a stopword in docs, not in identifiers
    }
    assert sorted(pi["parse config"]["gold_node_ids"]) == [10, 11]
    assert sorted(pi["load config"]["gold_node_ids"]) == [12, 14]
    assert pi["config sync"]["gold_node_ids"] == [14]
    assert pi["http server"]["gold_node_ids"] == [20]
    assert pi["http server"]["language"] == "typescript"
    assert sorted(pi["load user"]["gold_node_ids"]) == [50, 51]
    assert pi["task alpha"]["source"] == "partial_identifier"


def test_each_query_text_is_emitted_once(tmp_path):
    out, _ = _built(tmp_path)
    texts = [q["query"] for q in out if q["query_class"] == "partial_identifier"]
    assert len(texts) == len(set(texts))


def test_partial_identifier_excludes_test_and_non_code_symbols(tmp_path):
    out, by = _built(tmp_path)
    golds = {g for q in out for g in q["gold_node_ids"]}
    assert not golds & {60, 61, 62, 63}
    # get_x_value: every run has a 1-char subtoken
    assert not any("value" in q for q in by["partial_identifier"])


def test_keyword_words_drop_identifiers_stopwords_and_own_name():
    doc = ("Resolve `helper` callers using rewritePlan `x.y z` across crate boundaries and "
           "callers; SNAKE_CASE x_y HTTP. Second sentence words ignored.")
    assert keyword_words(doc, own_name="resolve_edges") == ["callers", "crate", "boundaries"]


def test_keyword_queries(tmp_path):
    _, by = _built(tmp_path)
    kw = by["keyword"]
    by_gold = {q["gold_node_ids"][0]: q for q in kw.values()}
    # rebuild_index: words drop, table, scratch, schema, version, changed; table,
    # schema, version also occur in migrate_store's doc (lower IDF) -> the three
    # rarest, in doc order. migrate_store's second sentence is never read.
    assert by_gold[70]["query"] == "drop scratch changed"
    assert by_gold[71]["query"] == "upgrade layout older"
    assert by_gold[72]["query"] == "callers crate boundaries"
    assert by_gold[70]["source"] == "keyword"
    # < 2 words; doc inside body; test path; name in doc; doc under 25 chars
    assert not {73, 74, 75, 76, 77} & set(by_gold)
