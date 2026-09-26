"""Unit tests for the SCIP call-edge oracle.

Run: python3 -m unittest discover -s scripts/scip_oracle -p 'test_*.py'

Everything is synthetic: a hand-encoded SCIP index, a minimal index.db with the
three tables the oracle reads, and source files in a temp dir. No rust-analyzer.
"""

import os
import sqlite3
import tempfile
import unittest

import oracle
import scip_decode

PKG = "rust-analyzer cargo demo 0.1.0 "


# ---- tiny protobuf encoder (test-only; mirrors the fields scip_decode reads) ----

def _varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def _len_field(fno, payload):
    return _varint(fno << 3 | 2) + _varint(len(payload)) + payload


def _int_field(fno, v):
    return _varint(fno << 3) + _varint(v)


def _packed(vals):
    return b"".join(_varint(v) for v in vals)


def occ(rng, symbol, definition=False, enclosing=None):
    b = _len_field(1, _packed(rng)) + _len_field(2, symbol.encode())
    if definition:
        b += _int_field(3, scip_decode.ROLE_DEFINITION)
    if enclosing:
        b += _len_field(7, _packed(enclosing))
    return b


def document(path, occurrences):
    b = _len_field(1, path.encode()) + _len_field(4, b"rust")
    for o in occurrences:
        b += _len_field(2, o)
    b += _int_field(6, 1)  # UTF8CodeUnitOffsetFromLineStart
    return b


def index_bytes(docs):
    return b"".join(_len_field(2, d) for d in docs)


# ---- fixture ----

SRC_A = (
    "fn callee() {}\n"            # 0
    "fn other() {}\n"             # 1
    "fn caller() {\n"             # 2
    "    callee();\n"             # 3  call            -> gold
    "    let f = other;\n"        # 4  fn pointer      -> not a call
    "    helper::<Vec<u8>>(1);\n"  # 5  turbofish call  -> gold
    "    ghost();\n"              # 6  callee has no node -> alignment loss
    "    caller()}\n"             # 7  recursion: the graph drops self-edges by design
    "fn helper<T>(_x: T) {}\n"    # 8
    "fn ghost() {}\n"             # 9
    "const K: () = callee();\n"   # 10 call outside any fn
)


def build_fixture(tmp, edges):
    os.makedirs(os.path.join(tmp, "src"))
    with open(os.path.join(tmp, "src/a.rs"), "w") as f:
        f.write(SRC_A)

    d = document("src/a.rs", [
        occ([0, 3, 9], PKG + "callee().", True, [0, 0, 14]),
        occ([1, 3, 8], PKG + "other().", True, [1, 0, 13]),
        occ([2, 3, 9], PKG + "caller().", True, [2, 0, 7, 13]),
        occ([3, 4, 10], PKG + "callee()."),
        occ([4, 12, 17], PKG + "other()."),
        occ([5, 4, 10], PKG + "helper()."),
        occ([6, 4, 9], PKG + "ghost()."),
        occ([7, 4, 10], PKG + "caller()."),
        occ([8, 3, 9], PKG + "helper().", True, [8, 0, 22]),
        occ([9, 3, 8], PKG + "ghost().", True, [9, 0, 13]),
        occ([10, 14, 20], PKG + "callee()."),
    ])
    scip = os.path.join(tmp, "index.scip")
    with open(scip, "wb") as f:
        f.write(index_bytes([d]))

    db = os.path.join(tmp, "index.db")
    con = sqlite3.connect(db)
    con.executescript("""
        CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT);
        CREATE TABLE nodes (id INTEGER PRIMARY KEY, file_id INTEGER, type TEXT,
                            name TEXT, start_line INTEGER, end_line INTEGER);
        CREATE TABLE edges (id INTEGER PRIMARY KEY, source_id INTEGER, target_id INTEGER,
                            relation TEXT, confidence TEXT);
        INSERT INTO files VALUES (1, 'src/a.rs', 'rust');
        -- 1-based lines; no node for ghost (alignment loss on the callee side)
        INSERT INTO nodes VALUES (1, 1, 'function', 'callee', 1, 1),
                                 (2, 1, 'function', 'other', 2, 2),
                                 (3, 1, 'function', 'caller', 3, 8),
                                 (4, 1, 'function', 'helper', 9, 9);
    """)
    con.executemany(
        "INSERT INTO edges (source_id, target_id, relation, confidence) VALUES (?, ?, 'calls', ?)",
        edges,
    )
    con.commit()
    con.close()
    return scip, db


TEST_SRC = "fn setup() {}\nfn t() {\n    setup();\n}\n"


class CollidingTestCrates(unittest.TestCase):
    """rust-analyzer gives every tests/*.rs crate the same package prefix, so a
    helper defined in two test files is one SCIP symbol. A reference binds to the
    definition in its own file; a file with several definitions stays ambiguous."""

    def test_reference_binds_to_the_definition_in_its_own_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            os.makedirs(os.path.join(tmp, "tests"))
            docs = []
            for name in ("x", "y"):
                with open(os.path.join(tmp, f"tests/{name}.rs"), "w") as f:
                    f.write(TEST_SRC)
                docs.append(document(f"tests/{name}.rs", [
                    occ([0, 3, 8], PKG + "setup().", True, [0, 0, 12]),
                    occ([1, 3, 4], PKG + f"t_{name}().", True, [1, 0, 3, 1]),
                    occ([2, 4, 9], PKG + "setup()."),
                ]))
            scip = os.path.join(tmp, "index.scip")
            with open(scip, "wb") as f:
                f.write(index_bytes(docs))
            db = os.path.join(tmp, "index.db")
            con = sqlite3.connect(db)
            con.executescript("""
                CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT);
                CREATE TABLE nodes (id INTEGER PRIMARY KEY, file_id INTEGER, type TEXT,
                                    name TEXT, start_line INTEGER, end_line INTEGER);
                CREATE TABLE edges (id INTEGER PRIMARY KEY, source_id INTEGER, target_id INTEGER,
                                    relation TEXT, confidence TEXT);
                INSERT INTO files VALUES (1, 'tests/x.rs', 'rust'), (2, 'tests/y.rs', 'rust');
                INSERT INTO nodes VALUES (1, 1, 'function', 'setup', 1, 1), (2, 1, 'function', 't', 2, 4),
                                         (3, 2, 'function', 'setup', 1, 1), (4, 2, 'function', 't', 2, 4);
                INSERT INTO edges (source_id, target_id, relation, confidence) VALUES
                    (2, 1, 'calls', 'extracted'),   -- x.t -> x.setup: right
                    (4, 1, 'calls', 'inferred');    -- y.t -> x.setup: wrong crate
            """)
            con.commit()
            con.close()
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp)
        self.assertEqual(r["gold_pairs"], 2)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 1, "correct": 1})
        self.assertEqual(r["tiers"]["inferred"], {"judged": 1, "correct": 0})
        self.assertEqual(r["funnel"]["colliding_definitions"], 1)
        self.assertNotIn("call_sites_to_unmapped_callee", r["funnel"])


class CallShape(unittest.TestCase):
    CASES = [
        ("    callee();", 4, 10, True),
        ("x.bar ()", 2, 5, True),
        ("helper::<Vec<u8>>(1);", 0, 6, True),
        ("Self::new(a)", 6, 9, True),
        ("use crate::foo;", 11, 14, False),
        ("v.iter().map(foo)", 13, 16, False),
        ("let f = other;", 8, 13, False),
        ("foo::<u8>;", 0, 3, False),
        ("é(x)", 0, 2, True),  # multi-byte name: UTF-8 byte offsets
    ]

    def test_shape_table(self):
        for text, start, end, want in self.CASES:
            with self.subTest(text=text):
                self.assertEqual(oracle.is_call_site(text.encode(), end), want)

    def test_callable_symbols(self):
        self.assertTrue(oracle.is_callable_symbol(PKG + "foo()."))
        self.assertTrue(oracle.is_callable_symbol(PKG + "impl#[A]b()."))
        self.assertTrue(oracle.is_callable_symbol(PKG + "m/foo(+1)."))
        self.assertFalse(oracle.is_callable_symbol(PKG + "foo!"))
        self.assertFalse(oracle.is_callable_symbol(PKG + "Foo#"))
        self.assertFalse(oracle.is_callable_symbol(PKG + "m/K."))
        self.assertFalse(oracle.is_callable_symbol("local 3"))


class Decode(unittest.TestCase):
    def test_round_trip_three_and_four_int_ranges(self):
        with tempfile.TemporaryDirectory() as tmp:
            scip, _db = build_fixture(tmp, [])
            (doc,) = scip_decode.read_index(scip)
        self.assertEqual(doc.relative_path, "src/a.rs")
        self.assertEqual(doc.position_encoding, 1)
        caller = doc.occurrences[2]
        self.assertTrue(caller.is_definition)
        self.assertEqual(caller.range, (2, 3, 2, 9))
        self.assertEqual(caller.enclosing_range, (2, 0, 7, 13))
        self.assertFalse(doc.occurrences[3].is_definition)


class Evaluate(unittest.TestCase):
    def run_oracle(self, edges):
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = build_fixture(tmp, edges)
            return oracle.evaluate(scip_decode.read_index(scip), db, tmp)

    def test_precision_recall_and_funnel(self):
        r = self.run_oracle([
            (3, 1, "extracted"),  # TP
            (3, 2, "inferred"),   # FP: `let f = other;` is not a call
        ])
        # gold = caller->callee, caller->helper (ghost unmapped, const has no fn)
        self.assertEqual(r["gold_pairs"], 2)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 1, "correct": 1})
        self.assertEqual(r["tiers"]["inferred"], {"judged": 1, "correct": 0})
        self.assertEqual(r["tiers"]["ambiguous"], {"judged": 0, "correct": 0})
        self.assertEqual(r["recall_at_floor"]["extracted"], {"found": 1, "gold": 2})
        self.assertEqual(r["recall_at_floor"]["ambiguous"], {"found": 1, "gold": 2})
        f = r["funnel"]
        self.assertEqual(f["call_sites_to_unmapped_callee"], 1)   # ghost()
        self.assertEqual(f["call_sites_without_enclosing_fn"], 1)  # const K
        self.assertEqual(f["non_call_references"], 1)              # `let f = other;`
        self.assertEqual(f["unmapped_definitions"], 1)
        self.assertEqual(f["self_calls_excluded"], 1)
        missed = {(m["caller"], m["callee"]) for m in r["samples"]["missed"]}
        self.assertEqual(missed, {("caller", "helper")})

    def test_duplicate_pair_counts_once_at_its_highest_tier(self):
        r = self.run_oracle([
            (3, 1, "ambiguous"),
            (3, 1, "extracted"),
        ])
        self.assertEqual(r["tiers"]["extracted"], {"judged": 1, "correct": 1})
        self.assertEqual(r["tiers"]["ambiguous"], {"judged": 0, "correct": 0})

    def test_edge_to_unmapped_node_is_unjudged_not_wrong(self):
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = build_fixture(tmp, [(3, 5, "extracted"), (3, 6, "extracted")])
            con = sqlite3.connect(db)
            con.execute("INSERT INTO nodes VALUES (5, 1, 'function', 'nowhere', 12, 12)")
            con.execute("INSERT INTO nodes VALUES (6, 1, 'struct', 'Point', 13, 13)")
            con.commit()
            con.close()
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 0, "correct": 0})
        self.assertEqual(r["funnel"]["unjudged_edges_unmapped_function"], 1)
        self.assertEqual(r["funnel"]["unjudged_edges_non_function_endpoint"], 1)


if __name__ == "__main__":
    unittest.main()
