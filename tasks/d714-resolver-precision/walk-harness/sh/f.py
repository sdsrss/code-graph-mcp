M = named("a") + star("c")
shapes["t1_named_target_walked_first"] = {"a.js": d("a.js"), "c.js": d("c.js"), "m.js": M, "index.js": star("p", "a", "m"), "p.js": star("m")}
shapes["t2_barrel_of_a_before"] = {"a.js": d("a.js"), "c.js": d("c.js"), "m.js": M, "index.js": star("A", "m"), "A.js": star("a")}
shapes["t3_naming_before_barrel"] = {"a.js": d("a.js"), "c.js": d("c.js"), "m.js": M, "index.js": star("m", "A"), "A.js": star("a")}
shapes["t4_named_reaches_a_by_star"] = {"a.js": d("a.js"), "c.js": d("c.js"), "m.js": named("B") + star("c"), "index.js": star("A", "m"), "A.js": star("a"), "B.js": star("a")}
shapes["t5_ambiguous_named"] = {"x.js": d("x.js"), "y.js": d("y.js"), "c.js": d("c.js"), "index.js": star("m"), "m.js": named("p") + star("c"), "p.js": star("x", "y")}
shapes["t6_import_then_star_invalid"] = {"c.js": d("c.js"), "x.js": d("x.js"), "p.js": "export function other() {}\n", "index.js": star("m"), "m.js": "import { f } from './p.js';\n" + star("c")}
