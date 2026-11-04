# basic sanity
shapes["s01_named_and_star"] = {"index.js": named("a") + star("b"), "a.js": d("a.js"), "b.js": d("b.js")}
shapes["s02_star_named_first"] = {"index.js": star("b") + named("a"), "a.js": d("a.js"), "b.js": d("b.js")}
shapes["s03_star_diamond"] = {"index.js": star("p", "q"), "p.js": star("c"), "q.js": star("c"), "c.js": d("c.js")}
shapes["s04_star_conflict_same_level"] = {"index.js": star("x", "y"), "x.js": d("x.js"), "y.js": d("y.js")}
shapes["s05_conflict_lower_then_def"] = {"index.js": star("p", "y"), "p.js": star("x", "y"), "x.js": d("x.js"), "y.js": d("y.js")}
shapes["s06_conflict_lower_masked_by_named"] = {"index.js": named("x") + star("p"), "p.js": star("x", "y"), "x.js": d("x.js"), "y.js": d("y.js")}
shapes["s07_conflict_lower_masked_by_local"] = {"index.js": d("index.js") + star("p"), "p.js": star("x", "y"), "x.js": d("x.js"), "y.js": d("y.js")}
shapes["s08_named_into_ambiguous"] = {"index.js": named("p"), "p.js": star("x", "y"), "x.js": d("x.js"), "y.js": d("y.js")}
shapes["s09_star_cycle"] = {"index.js": star("p", "q"), "p.js": star("q"), "q.js": star("p", "a"), "a.js": d("a.js")}
shapes["s10_star_self_cycle"] = {"index.js": star("index", "a"), "a.js": d("a.js")}
shapes["s11_named_and_star_same_file"] = {"index.js": star("m", "a"), "m.js": named("a"), "a.js": d("a.js")}
shapes["s12_named_and_star_same_file_rev"] = {"index.js": star("a", "m"), "m.js": named("a"), "a.js": d("a.js")}
shapes["s13_star_to_named_to_star"] = {"index.js": star("m"), "m.js": named("p") + star("y"), "p.js": star("x"), "x.js": d("x.js"), "y.js": d("y.js")}
# depth
def chain(n, kind):
    fs = {}
    prev = "index"
    for i in range(1, n):
        fs[f"{prev}.js"] = (named if kind == "named" else star)(f"c{i}")
        prev = f"c{i}"
    fs[f"{prev}.js"] = d(f"{prev}.js")
    return fs
shapes["s14_star_chain_8"] = chain(8, "star")
shapes["s15_star_chain_9"] = chain(9, "star")
shapes["s16_named_chain_8"] = chain(8, "named")
shapes["s17_named_chain_9"] = chain(9, "named")
# different depths of same file
shapes["s18_same_file_two_depths"] = {"index.js": star("l1", "a"), "l1.js": star("l2"), "l2.js": star("a"), "a.js": d("a.js")}
shapes["s19_same_file_two_depths_rev"] = {"index.js": star("a", "l1"), "l1.js": star("l2"), "l2.js": star("a"), "a.js": d("a.js")}
# conflicting at different levels, 4 orders, deeper
for i, (o1, o2) in enumerate([(("p","y"),("x","y")), (("p","y"),("y","x")), (("y","p"),("x","y")), (("y","p"),("y","x"))]):
    shapes[f"s20_conflict_orders_{i}"] = {"index.js": star(*o1), "p.js": star(*o2), "x.js": d("x.js"), "y.js": d("y.js")}
# conflict at level 2, sibling at level 1 defines same as one side
for i, (o1, o2) in enumerate([(("p","x"),("q","z")), (("x","p"),("z","q"))]):
    shapes[f"s21_conflict_deep_{i}"] = {"index.js": star(*o1), "p.js": star(*o2), "q.js": star("y"), "z.js": star("x"), "x.js": d("x.js"), "y.js": d("y.js")}
# CJS require destructuring through an ESM barrel
shapes["s22_require_destructure"] = {"app.js": "const { f: g } = require('./index.js');\nfunction main() { return g(); }\nconsole.log(main());\nmodule.exports = { main };\n", "package.json": "{}\n", "index.js": "module.exports = require('./mid.js');\n", "mid.js": "const { f } = require('./a.js');\nmodule.exports = { f };\n", "a.js": "function f() { return 'a.js'; }\nmodule.exports = { f };\n"}
# plain import beside export *
shapes["s23_plain_import_beside_star"] = {"index.js": star("m"), "m.js": "import { f as h } from './a.js';\nexport function use() { return h(); }\n" + star("b"), "a.js": d("a.js"), "b.js": d("b.js")}
shapes["s24_plain_import_same_name_beside_star"] = {"index.js": star("m"), "m.js": "import { f } from './a.js';\nexport function use() { return f(); }\n" + star("b"), "a.js": d("a.js"), "b.js": d("b.js")}
