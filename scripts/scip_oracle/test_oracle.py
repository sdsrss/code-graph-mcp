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


def symbol_info(symbol, implements=()):
    """SymbolInformation with an is_implementation relationship to each of `implements`."""
    b = _len_field(1, symbol.encode())
    for parent in implements:
        b += _len_field(4, _len_field(1, parent.encode()) + _int_field(3, 1))
    return b


def document(path, occurrences, language="rust", encoding=1, symbols=()):
    b = _len_field(1, path.encode()) + _len_field(4, language.encode())
    for o in occurrences:
        b += _len_field(2, o)
    for s in symbols:
        b += _len_field(3, s)
    if encoding:  # 1 = UTF8CodeUnitOffsetFromLineStart; 0 (unset) is what scip-typescript/-python emit
        b += _int_field(6, encoding)
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
        ("arrow?.();", 0, 5, True),     # JS optional call
        ("x?.foo()", 0, 1, False),      # Rust `?` then a method: not a call of x
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

    def test_only_is_implementation_relationships_are_overrides(self):
        ref_only = _len_field(4, _len_field(1, b"p B#") + _int_field(2, 1))  # is_reference
        doc = document("a.ts", [], symbols=[
            symbol_info("p A#f().", ["p B#f()."]),
            _len_field(1, b"p C#") + ref_only,
        ])
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "index.scip")
            with open(path, "wb") as f:
                f.write(index_bytes([doc]))
            (d,) = scip_decode.read_index(path)
        self.assertEqual(d.implements, {"p A#f().": ["p B#f()."]})

    def test_constructor_owner(self):
        self.assertEqual(oracle.constructor_owner("cxx . . $ leveldb/DBImpl#DBImpl(6e)."), "cxx . . $ leveldb/DBImpl#")
        self.assertEqual(oracle.constructor_owner("cxx . . $ Widget#Widget(aa)."), "cxx . . $ Widget#")
        self.assertEqual(oracle.constructor_owner("npm . . src/`e.ts`/Box#`<constructor>`()."), "npm . . src/`e.ts`/Box#")
        self.assertEqual(oracle.constructor_owner("cxx . . $ a/Outer#Inner#Inner(1)."), "cxx . . $ a/Outer#Inner#")
        self.assertIsNone(oracle.constructor_owner("cxx . . $ a/DBImpl#Get(1)."))
        self.assertIsNone(oracle.constructor_owner("cxx . . $ a/DBImpl#"))


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


_SCHEMA = """
    CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT);
    CREATE TABLE nodes (id INTEGER PRIMARY KEY, file_id INTEGER, type TEXT,
                        name TEXT, start_line INTEGER, end_line INTEGER);
    CREATE TABLE edges (id INTEGER PRIMARY KEY, source_id INTEGER, target_id INTEGER,
                        relation TEXT, confidence TEXT);
"""


def _write_case(tmp, path, src, docs, sql, edges):
    os.makedirs(os.path.join(tmp, os.path.dirname(path)), exist_ok=True)
    with open(os.path.join(tmp, path), "w", encoding="utf-8") as f:
        f.write(src)
    scip = os.path.join(tmp, "index.scip")
    with open(scip, "wb") as f:
        f.write(index_bytes(docs))
    db = os.path.join(tmp, "index.db")
    con = sqlite3.connect(db)
    con.executescript(_SCHEMA + sql)
    con.executemany(
        "INSERT INTO edges (source_id, target_id, relation, confidence) VALUES (?, ?, 'calls', ?)", edges)
    con.commit()
    con.close()
    return scip, db


JSPKG = "scip-typescript npm . . src/`a.js`/"
# Line 3 carries a 2-UTF-16-unit character (U+1F600) before the call: scip-typescript
# columns count UTF-16 units, so `callee` sits at unit 18 but byte 20.
SRC_JS = (
    "function callee() {}\n"            # 0
    "const arrow = () => {};\n"         # 1  term symbol `arrow.`, aligned to a function node
    "function caller(cb) {\n"           # 2
    "  const s = '\U0001F600'; callee();\n"  # 3  gold caller->callee, UTF-16 columns
    "  arrow?.();\n"                    # 4  optional call -> gold caller->arrow
    "  cb();\n"                         # 5  a parameter: no function node, not gold, not a loss
    "  function inner() {\n"            # 6  `local 2`: SCIP gives nested fns no enclosing_range
    "    callee();\n"                   # 7  gold inner->callee (not caller->callee)
    "  }\n"                             # 8
    "  inner();\n"                      # 9  gold caller->inner
    "  obj.method();\n"                 # 10 gold caller->method, shape `member`
    "}\n"                               # 11
    "new Widget();\n"                   # 12 a class, not a function
    "class Obj { method() {} }\n"       # 13
    "class Widget {}\n"                 # 14
    "function user() {\n"               # 15
    "  require('./b').helper();\n"      # 16 SCIP: the property `helper0:` -> aliased to helper()
    "  timer.unref();\n"                # 17 untyped receiver: SCIP has no occurrence at all
    "}\n"                               # 18
    "callee();\n"                       # 19 top level: gold (<module>, callee), as the index attributes it
)
# (a const bound to an expression is the same case as the parameter: see test_const_binding)
JSPKG_B = "scip-typescript npm . . src/`b.js`/"
SRC_JS_B = (
    "function helper(x) {}\n"                 # 0 `x` is b.js's `local 2`, a.js's is `inner`
    "function unref() {}\n"                   # 1
    "module.exports = { helper, unref };\n"   # 2 shorthand: property def + function ref, same range
    "function cb() {}\n"                      # 3 shares a name with caller's parameter
)


def js_case(tmp, edges):
    d = document("src/a.js", [
        occ([0, 9, 15], JSPKG + "callee().", True, [0, 0, 20]),
        occ([1, 6, 11], JSPKG + "arrow.", True, [1, 0, 23]),
        occ([2, 9, 15], JSPKG + "caller().", True, [2, 0, 11, 1]),
        occ([2, 16, 18], "local 1", True),
        occ([3, 18, 24], JSPKG + "callee()."),
        occ([4, 2, 7], JSPKG + "arrow."),
        occ([5, 2, 4], "local 1"),
        occ([6, 11, 16], "local 2", True),
        occ([7, 4, 10], JSPKG + "callee()."),
        occ([9, 2, 7], "local 2"),
        occ([10, 6, 12], JSPKG + "Obj#method()."),
        occ([12, 4, 10], JSPKG + "Widget#"),
        occ([13, 6, 9], JSPKG + "Obj#", True),
        occ([13, 12, 18], JSPKG + "Obj#method().", True, [13, 12, 23]),
        occ([14, 6, 12], JSPKG + "Widget#", True),
        occ([15, 9, 13], JSPKG + "user().", True, [15, 0, 18, 1]),
        occ([16, 17, 23], JSPKG_B + "helper0:"),
        occ([19, 0, 6], JSPKG + "callee()."),
    ], language="javascript", encoding=0)
    b = document("src/b.js", [
        occ([0, 9, 15], JSPKG_B + "helper().", True, [0, 0, 21]),
        occ([0, 16, 17], "local 2", True),
        occ([1, 9, 14], JSPKG_B + "unref().", True, [1, 0, 19]),
        occ([2, 19, 25], JSPKG_B + "helper0:", True),
        occ([2, 19, 25], JSPKG_B + "helper()."),
        occ([2, 27, 32], JSPKG_B + "unref0:", True),
        occ([2, 27, 32], JSPKG_B + "unref()."),
        occ([3, 9, 11], JSPKG_B + "cb().", True, [3, 0, 16]),
    ], language="javascript", encoding=0)
    os.makedirs(os.path.join(tmp, "src"), exist_ok=True)
    with open(os.path.join(tmp, "src/b.js"), "w") as f:
        f.write(SRC_JS_B)
    return _write_case(tmp, "src/a.js", SRC_JS, [d, b], """
        INSERT INTO files VALUES (1, 'src/a.js', 'javascript'), (2, 'src/b.rs', 'rust'),
                                 (3, 'src/b.js', 'javascript');
        INSERT INTO nodes VALUES (1, 1, 'function', 'callee', 1, 1),
                                 (2, 1, 'function', 'arrow', 2, 2),
                                 (3, 1, 'function', 'caller', 3, 12),
                                 (4, 1, 'function', 'inner', 7, 9),
                                 (5, 1, 'class', 'Obj', 14, 14),
                                 (6, 1, 'method', 'method', 14, 14),
                                 (7, 1, 'class', 'Widget', 15, 15),
                                 (8, 2, 'function', 'callee', 1, 1),
                                 (9, 1, 'function', 'user', 16, 19),
                                 (10, 3, 'function', 'helper', 1, 1),
                                 (11, 3, 'function', 'unref', 2, 2),
                                 (12, 3, 'function', 'cb', 4, 4),
                                 (13, 1, 'module', '<module>', 1, 20);
    """, edges)


class JavaScript(unittest.TestCase):
    def run_js(self, edges, language="javascript"):
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = js_case(tmp, edges)
            return oracle.evaluate(scip_decode.read_index(scip), db, tmp, language=language)

    def test_gold_pairs_callers_and_shapes(self):
        r = self.run_js([
            (3, 1, "extracted"),  # caller->callee: TP
            (4, 1, "extracted"),  # inner->callee: TP (attributed to the nested fn)
            (3, 2, "inferred"),   # caller->arrow: TP
            (3, 6, "ambiguous"),  # caller->method: TP
            (2, 1, "inferred"),   # arrow->callee: FP, arrow calls nothing
            (3, 8, "inferred"),   # to a rust node: outside the language scope
            (9, 10, "extracted"), # user->helper through require(...).helper: TP via the export alias
            (9, 11, "inferred"),  # user->unref: `timer.unref()` is untyped, cannot be judged
            (3, 12, "inferred"),  # caller->cb: `cb()` calls a parameter, whose binding SCIP cannot see
            (13, 1, "extracted"), # <module>->callee: the top-level call
        ])
        self.assertEqual(r["gold_pairs"], 7)  # + caller->inner, which we miss
        self.assertEqual(r["tiers"]["extracted"], {"judged": 4, "correct": 4})
        self.assertEqual(r["tiers"]["inferred"], {"judged": 2, "correct": 1})
        self.assertEqual(r["tiers"]["ambiguous"], {"judged": 1, "correct": 1})
        self.assertEqual(r["recall_by_call_shape"],
                         {"bare": {"found": 4, "gold": 5}, "member": {"found": 2, "gold": 2}})
        f = r["funnel"]
        self.assertEqual(f["call_sites_via_export_alias"], 1)
        self.assertEqual(f["call_sites_attributed_to_module"], 1)
        self.assertNotIn("call_sites_without_enclosing_fn", f)
        self.assertEqual(f["unjudged_edges_untyped_call"], 1)
        self.assertEqual(f["call_sites_to_non_function_symbol"], 2)  # cb(), new Widget()
        self.assertEqual(f["unjudged_edges_unknown_binding"], 1)
        self.assertEqual(f["enclosing_from_index_span"], 1)       # inner
        self.assertEqual(f["wrong_inferred_no_call_by_that_name"], 1)
        self.assertEqual(f["edges_to_outside_javascript_scope"], 1)
        self.assertNotIn("unmapped_definitions", f)                # cb, Widget, Obj are not losses
        self.assertNotIn("call_sites_to_unmapped_callee", f)
        missed = {(m["caller"], m["callee"]) for m in r["samples"]["missed"]}
        self.assertEqual(missed, {("caller", "inner")})

    def test_const_bound_to_an_expression_is_an_unknown_binding(self):
        src = ("function real() {}\n"
               "const pick = globalThis.x || real;\n"
               "function go() { pick(); }\n")
        d = document("src/c.js", [
            occ([0, 9, 13], "p `c.js`/real().", True, [0, 0, 18]),
            occ([1, 6, 10], "p `c.js`/pick.", True),
            occ([2, 9, 11], "p `c.js`/go().", True, [2, 0, 26]),
            occ([2, 16, 20], "p `c.js`/pick."),
        ], language="javascript", encoding=0)
        other = document("src/d.js", [occ([0, 9, 13], "p `d.js`/pick().", True, [0, 0, 18])],
                         language="javascript", encoding=0)
        with tempfile.TemporaryDirectory() as tmp:
            os.makedirs(os.path.join(tmp, "src"))
            with open(os.path.join(tmp, "src/d.js"), "w") as f:
                f.write("function pick() {}\n")
            scip, db = _write_case(tmp, "src/c.js", src, [d, other], """
                INSERT INTO files VALUES (1, 'src/c.js', 'javascript'), (2, 'src/d.js', 'javascript');
                INSERT INTO nodes VALUES (1, 1, 'function', 'real', 1, 1), (2, 1, 'function', 'go', 3, 3),
                                         (3, 2, 'function', 'pick', 1, 1);
            """, [(2, 3, "inferred")])  # go->pick in another file: SCIP cannot say
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="javascript")
        self.assertEqual(r["tiers"]["inferred"], {"judged": 0, "correct": 0})
        self.assertEqual(r["funnel"]["unjudged_edges_unknown_binding"], 1)

    def test_rust_scope_sees_none_of_it(self):
        r = self.run_js([(3, 1, "extracted")], language="rust")
        self.assertEqual(r["gold_pairs"], 0)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 0, "correct": 0})


PYPKG = "scip-python python demo 0 `lib.m`/"
SRC_PY = (
    "def callee():\n"          # 0
    "    pass\n"               # 1
    "class Box:\n"             # 2
    "    def get(self):\n"     # 3
    "        return callee()\n"  # 4 gold get->callee
    "def caller():\n"          # 5
    "    b = Box()\n"          # 6 a class: not gold, not a loss
    "    b.get()\n"            # 7 gold caller->get, shape member
    "    callee()\n"           # 8 gold caller->callee
    "def uses_import():\n"     # 9
    "    from helpers import util\n"  # 10 unresolved (sys.path) import: scip-python makes `util` a local
    "    util()\n"             # 11 not gold; our edge to a `util` cannot be judged
)


class Python(unittest.TestCase):
    def test_methods_and_classes(self):
        d = document("lib/m.py", [
            occ([0, 4, 10], PYPKG + "callee().", True, [0, 0, 1, 8]),
            occ([2, 6, 9], PYPKG + "Box#", True, [2, 0, 4, 23]),
            occ([3, 8, 11], PYPKG + "Box#get().", True, [3, 4, 4, 23]),
            occ([4, 15, 21], PYPKG + "callee()."),
            occ([5, 4, 10], PYPKG + "caller().", True, [5, 0, 8, 12]),
            occ([6, 8, 11], PYPKG + "Box#"),
            occ([7, 6, 9], PYPKG + "Box#get()."),
            occ([8, 4, 10], PYPKG + "callee()."),
            occ([9, 4, 15], PYPKG + "uses_import().", True, [9, 0, 11, 10]),
            occ([10, 24, 28], "local 7"),  # scip-python emits no definition role here
            occ([11, 4, 8], "local 7"),
        ], language="python", encoding=0)
        helpers = document("lib/helpers.py", [
            occ([0, 4, 8], "scip-python python demo 0 `lib.helpers`/util().", True, [0, 0, 13]),
            occ([1, 4, 9], "scip-python python demo 0 `lib.helpers`/other().", True, [1, 0, 14]),
        ], language="python", encoding=0)
        with tempfile.TemporaryDirectory() as tmp:
            os.makedirs(os.path.join(tmp, "lib"))
            with open(os.path.join(tmp, "lib/helpers.py"), "w") as f:
                f.write("def util(): 1\ndef other(): 1\n")
            scip, db = _write_case(tmp, "lib/m.py", SRC_PY, [d, helpers], """
                INSERT INTO files VALUES (1, 'lib/m.py', 'python'), (2, 'lib/helpers.py', 'python');
                INSERT INTO nodes VALUES (1, 1, 'function', 'callee', 1, 2),
                                         (2, 1, 'class', 'Box', 3, 5),
                                         (3, 1, 'method', 'get', 4, 5),
                                         (4, 1, 'function', 'caller', 6, 9),
                                         (5, 1, 'function', 'uses_import', 10, 12),
                                         (6, 2, 'function', 'util', 1, 1),
                                         (7, 2, 'function', 'other', 2, 2);
            """, [(3, 1, "extracted"), (4, 3, "inferred"), (4, 2, "inferred"),
                  (5, 6, "inferred"),     # to the name the import binds: unjudged
                  (5, 7, "ambiguous")])   # to a name nobody calls: still wrong
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="python")
        self.assertEqual(r["gold_pairs"], 3)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 1, "correct": 1})
        self.assertEqual(r["tiers"]["inferred"], {"judged": 1, "correct": 1})  # caller->Box unjudged
        self.assertEqual(r["funnel"]["unjudged_edges_non_function_endpoint"], 1)
        self.assertEqual(r["funnel"]["call_sites_to_unresolved_import"], 1)
        self.assertEqual(r["funnel"]["unjudged_edges_unknown_binding"], 1)
        self.assertEqual(r["tiers"]["ambiguous"], {"judged": 1, "correct": 0})
        self.assertEqual(r["recall_by_call_shape"],
                         {"bare": {"found": 1, "gold": 2}, "member": {"found": 1, "gold": 1}})


    def test_calls_inside_overload_stubs_are_not_the_modules(self):
        # scip-python gives every `@overload` stub and the implementation one symbol:
        # the definitions are ambiguous, and a call in their bodies has no caller.
        src = ("@overload\n"                   # 0
               "def f(x: int) -> int: ...\n"  # 1
               "@overload\n"                   # 2
               "def f(x: str) -> str: ...\n"  # 3
               "def f(x):\n"                   # 4
               "    return g()\n"              # 5
               "def g():\n"                    # 6
               "    pass\n")                   # 7
        p = "scip-python python demo 0 `lib.o`/"
        # scip-typescript gives a local no enclosing_range: the node spans stand in.
        for enclosing in (True, False):
            e = (lambda rng: rng) if enclosing else (lambda rng: None)
            d = document("lib/o.py", [
                occ([1, 4, 5], p + "f().", True, e([0, 0, 1, 25])),
                occ([3, 4, 5], p + "f().", True, e([2, 0, 3, 25])),
                occ([4, 4, 5], p + "f().", True, e([4, 0, 5, 14])),
                occ([5, 11, 12], p + "g()."),
                occ([6, 4, 5], p + "g().", True, e([6, 0, 7, 8])),
            ], language="python", encoding=0)
            with self.subTest(enclosing=enclosing), tempfile.TemporaryDirectory() as tmp:
                scip, db = _write_case(tmp, "lib/o.py", src, [d], """
                    INSERT INTO files VALUES (1, 'lib/o.py', 'python');
                    INSERT INTO nodes VALUES (1, 1, 'function', 'f', 1, 2), (2, 1, 'function', 'f', 3, 4),
                                             (3, 1, 'function', 'f', 5, 6), (4, 1, 'function', 'g', 7, 8),
                                             (5, 1, 'module', '<module>', 1, 8);
                """, [(3, 4, "extracted"), (1, 4, "extracted")])
                r = oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="python")
                self.assertEqual(r["gold_pairs"], 0)
                self.assertEqual(r["funnel"]["call_sites_in_ambiguous_definition"], 1)
                self.assertNotIn("call_sites_attributed_to_module", r["funnel"])
                self.assertEqual(r["funnel"]["unjudged_edges_unmapped_function"], 2)


# scip-clang: columns are UTF-8 bytes although position_encoding is unset, header
# declarations are role-0 references shaped like calls, every occurrence in a header
# repeats once per translation unit, and there is no enclosing_range at all.
CXX = "cxx . . $ "
SRC_CPP = (
    "struct Base { virtual void Next(); };\n"          # 0 declaration: call-shaped, role 0
    "struct Derived : Base { void Next() override; };\n"  # 1 declaration
    "struct Widget { Widget(int x) {} };\n"            # 2 constructor defined in the class
    "void Base::Next() {}\n"                           # 3
    "void Derived::Next() {}\n"                        # 4 implements Base::Next
    "template <typename T> void helper(T x) {}\n"      # 5
    "void use(Base* b) {\n"                            # 6
    "  const char* s = \"é\"; b->Next();\n"        # 7 gold use->Base::Next; `é` is 2 bytes
    "  helper<int>(1);\n"                              # 8 gold use->helper through template args
    "  Widget* w = new Widget(1);\n"                   # 9 gold use->Widget::Widget
    "}\n"                                              # 10
)


def _at(line, token, nth=0):
    """[line, start, end] in UTF-8 bytes of the nth `token` on SRC_CPP's line."""
    text = SRC_CPP.split("\n")[line].encode()
    i = -1
    for _ in range(nth + 1):
        i = text.index(token.encode(), i + 1)
    return [line, i, i + len(token.encode())]


def cpp_case(tmp, edges):
    use_def = occ(_at(6, "use"), CXX + "use(cc).", True)
    d = document("src/a.cc", [
        occ(_at(0, "Base"), CXX + "Base#", True),
        occ(_at(0, "Next"), CXX + "Base#Next(49)."),
        occ(_at(1, "Derived"), CXX + "Derived#", True),
        occ(_at(1, "Next"), CXX + "Derived#Next(49)."),
        occ(_at(2, "Widget"), CXX + "Widget#", True),
        occ(_at(2, "Widget", 1), CXX + "Widget#Widget(aa).", True),
        occ(_at(3, "Base"), CXX + "Base#"),
        occ(_at(3, "Next"), CXX + "Base#Next(49).", True),
        occ(_at(4, "Next"), CXX + "Derived#Next(49).", True),
        occ(_at(5, "helper"), CXX + "helper(bb).", True),
        use_def, use_def,  # one per translation unit that saw it
        occ(_at(7, "Next"), CXX + "Base#Next(49)."),
        occ(_at(8, "helper"), CXX + "helper(bb)."),
        occ(_at(9, "Widget"), CXX + "Widget#"),
        occ(_at(9, "Widget", 1), CXX + "Widget#Widget(aa)."),
    ], language="CPP", encoding=0,
        symbols=[symbol_info(CXX + "Derived#Next(49).", [CXX + "Base#Next(49)."])])
    return _write_case(tmp, "src/a.cc", SRC_CPP, [d], """
        INSERT INTO files VALUES (1, 'src/a.cc', 'cpp');
        INSERT INTO nodes VALUES (1, 1, 'struct', 'Base', 1, 1), (2, 1, 'struct', 'Derived', 2, 2),
                                 (3, 1, 'struct', 'Widget', 3, 3), (4, 1, 'method', 'Widget', 3, 3),
                                 (5, 1, 'method', 'Next', 4, 4), (6, 1, 'method', 'Next', 5, 5),
                                 (7, 1, 'function', 'helper', 6, 6), (8, 1, 'function', 'use', 7, 11),
                                 (9, 1, 'module', '<module>', 1, 11);
    """, edges)


class Cpp(unittest.TestCase):
    def run_cpp(self, edges):
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = cpp_case(tmp, edges)
            return oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="cpp")

    def test_declarations_dispatch_constructors_and_byte_columns(self):
        r = self.run_cpp([
            (8, 5, "extracted"),   # use->Base::Next: TP
            (8, 6, "extracted"),   # use->Derived::Next: TP, virtual dispatch to an override
            (8, 7, "inferred"),    # use->helper: TP
            (8, 3, "inferred"),    # use->Widget (the class): TP, `new Widget(1)` runs its constructor
            (7, 8, "ambiguous"),   # helper->use: FP
        ])
        self.assertEqual(r["gold_pairs"], 3)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 2, "correct": 2})
        self.assertEqual(r["tiers"]["inferred"], {"judged": 2, "correct": 2})
        self.assertEqual(r["tiers"]["ambiguous"], {"judged": 1, "correct": 0})
        self.assertEqual(r["recall_at_floor"]["inferred"], {"found": 3, "gold": 3})
        self.assertEqual(r["recall_by_call_shape"], {"bare": {"found": 1, "gold": 1},
                                                     "constructor": {"found": 1, "gold": 1},
                                                     "method": {"found": 1, "gold": 1}})
        f = r["funnel"]
        self.assertEqual(f["call_sites_without_enclosing_fn"], 2)  # the two declarations
        self.assertEqual(f["edges_credited_via_override"], 1)
        self.assertEqual(f["edges_credited_to_constructor_class"], 1)
        self.assertNotIn("colliding_definitions", f)
        self.assertNotIn("call_sites_attributed_to_module", f)

    def test_dump_judged_lists_every_judged_edge_with_its_verdict(self):
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = cpp_case(tmp, [(8, 5, "extracted"), (8, 3, "inferred"), (7, 8, "ambiguous")])
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="cpp", dump_judged=True)
        got = {(j["tier"], j["caller"], j["callee"], j["verdict"]) for j in r["judged_edges"]}
        self.assertEqual(got, {("extracted", "use", "Next", "correct"), ("inferred", "use", "Widget", "correct"),
                               ("ambiguous", "helper", "use", "no_call_by_that_name")})
        self.assertEqual(len(r["judged_edges"]), sum(t["judged"] for t in r["tiers"].values()))

    def test_override_edge_alone_is_recall(self):
        r = self.run_cpp([(8, 6, "extracted")])
        self.assertEqual(r["recall_at_floor"]["extracted"], {"found": 1, "gold": 3})

    def test_class_edge_with_no_constructor_call_stays_unjudged(self):
        r = self.run_cpp([(7, 3, "inferred")])  # helper->Widget: nothing to credit
        self.assertEqual(r["tiers"]["inferred"], {"judged": 0, "correct": 0})
        self.assertEqual(r["funnel"]["unjudged_edges_non_function_endpoint"], 1)


SRC_CPP2 = (
    "struct Iface { virtual int Get() = 0; };\n"                   # 0 pure virtual: never defined
    "struct Impl : Iface { int Get() override; };\n"               # 1
    "int Impl::Get() { return 1; }\n"                              # 2 implements Iface::Get
    "Impl::~Impl() { Get(); }\n"                                   # 3 scip-clang's name range is `~`
    "bool Impl::operator==(const Impl&) const { return Get(); }\n"  # 4 ... and `operator`
    "TEST_F(Suite, Case) { Impl x; x.Get(); }\n"                   # 5 gtest: the index names it Suite.Case
    "int call(Iface* i) { return i->Get(); }\n"                    # 6 dispatch through a declaration
)


def _at2(line, token):
    text = SRC_CPP2.split("\n")[line].encode()
    i = text.index(token.encode())
    return [line, i, i + len(token.encode())]


class CppNamesAndDeclarationOnlyMethods(unittest.TestCase):
    def test_destructor_operator_gtest_and_pure_virtual(self):
        d = document("src/b.cc", [
            occ(_at2(0, "Iface"), CXX + "Iface#", True),
            occ(_at2(0, "Get"), CXX + "Iface#Get(1)."),
            occ(_at2(1, "Impl"), CXX + "Impl#", True),
            occ(_at2(1, "Get"), CXX + "Impl#Get(1)."),
            occ(_at2(2, "Get"), CXX + "Impl#Get(1).", True),
            occ(_at2(3, "~"), CXX + "Impl#`~Impl`(2).", True),
            occ(_at2(3, "Get"), CXX + "Impl#Get(1)."),
            occ(_at2(4, "operator"), CXX + "Impl#`operator==`(3).", True),
            occ(_at2(4, "Get"), CXX + "Impl#Get(1)."),
            occ(_at2(5, "TEST_F"), CXX + "Suite_Case_Test#", True),
            occ(_at2(5, "TEST_F"), CXX + "Suite_Case_Test#TestBody(4).", True),
            occ(_at2(5, "Get"), CXX + "Impl#Get(1)."),
            occ(_at2(6, "call"), CXX + "call(5).", True),
            occ(_at2(6, "Get"), CXX + "Iface#Get(1)."),
        ], language="CPP", encoding=0,
            symbols=[symbol_info(CXX + "Impl#Get(1).", [CXX + "Iface#Get(1)."])])
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = _write_case(tmp, "src/b.cc", SRC_CPP2, [d], """
                INSERT INTO files VALUES (1, 'src/b.cc', 'cpp');
                INSERT INTO nodes VALUES (1, 1, 'struct', 'Iface', 1, 1), (2, 1, 'struct', 'Impl', 2, 2),
                                         (3, 1, 'method', 'Get', 3, 3), (4, 1, 'method', '~Impl', 4, 4),
                                         (5, 1, 'method', 'operator==', 5, 5),
                                         (6, 1, 'function', 'Suite.Case', 6, 6),
                                         (7, 1, 'function', 'call', 7, 7);
            """, [(4, 3, "extracted"), (5, 3, "extracted"), (6, 3, "extracted"),
                  (7, 3, "inferred")])  # call->Impl::Get: `i->Get()` dispatches to it
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="cpp")
        self.assertEqual(r["gold_pairs"], 3)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 3, "correct": 3})
        self.assertEqual(r["tiers"]["inferred"], {"judged": 1, "correct": 1})
        f = r["funnel"]
        self.assertNotIn("unmapped_definitions", f)
        self.assertEqual(f["call_sites_to_declaration_only_method"], 1)
        self.assertEqual(f["call_sites_without_enclosing_fn"], 2)  # the two declarations
        self.assertEqual(f["edges_credited_via_override"], 1)
        self.assertNotIn("call_sites_to_external", f)


class TypeScriptConstructor(unittest.TestCase):
    def test_new_credits_the_class_node(self):
        src = ("class Box {\n"                 # 0
               "  constructor() {}\n"          # 1
               "}\n"                           # 2
               "function make() {\n"           # 3
               "  return new Box();\n"         # 4
               "}\n")                          # 5
        p = "scip-typescript npm . . src/`e.ts`/"
        d = document("src/e.ts", [
            occ([0, 6, 9], p + "Box#", True, [0, 0, 2, 1]),
            occ([1, 2, 13], p + "Box#`<constructor>`().", True, [1, 2, 18]),
            occ([3, 9, 13], p + "make().", True, [3, 0, 5, 1]),
            occ([4, 13, 16], p + "Box#`<constructor>`()."),
        ], language="typescript", encoding=0)
        with tempfile.TemporaryDirectory() as tmp:
            scip, db = _write_case(tmp, "src/e.ts", src, [d], """
                INSERT INTO files VALUES (1, 'src/e.ts', 'typescript');
                INSERT INTO nodes VALUES (1, 1, 'class', 'Box', 1, 3), (2, 1, 'method', 'constructor', 2, 2),
                                         (3, 1, 'function', 'make', 4, 6);
            """, [(3, 1, "extracted")])
            r = oracle.evaluate(scip_decode.read_index(scip), db, tmp, language="javascript")
        self.assertEqual(r["gold_pairs"], 1)
        self.assertEqual(r["tiers"]["extracted"], {"judged": 1, "correct": 1})
        self.assertEqual(r["recall_at_floor"]["extracted"], {"found": 1, "gold": 1})


class Utf16Columns(unittest.TestCase):
    def test_utf16_units_become_utf8_bytes(self):
        line = "  const s = '\U0001F600'; callee();".encode()
        self.assertEqual(oracle.utf16_to_byte(line, 18), 20)
        self.assertEqual(oracle.utf16_to_byte(line, 0), 0)
        self.assertEqual(oracle.utf16_to_byte("é(x)".encode(), 1), 2)


if __name__ == "__main__":
    unittest.main()
