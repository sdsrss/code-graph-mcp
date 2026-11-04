PRIV = 'function f() { return "priv"; }\nexport function other() { return f(); }\n'
PLAIN = lambda src: f"import {{ f }} from './{src}.js';\nexport function use() {{ return f(); }}\n"
# false ambiguity from a plain import in a star source, propagated upward
shapes["b01_plain_import_mid_then_sibling"] = {"index.js": star("mid", "b"), "mid.js": star("a", "b"), "a.js": PLAIN("x"), "b.js": d("b.js"), "x.js": d("x.js")}
shapes["b02_plain_import_mid_only"] = {"index.js": star("mid"), "mid.js": star("a", "b"), "a.js": PLAIN("x"), "b.js": d("b.js"), "x.js": d("x.js")}
shapes["b03_plain_import_mid_named_top"] = {"index.js": named("mid"), "mid.js": star("a", "b"), "a.js": PLAIN("x"), "b.js": d("b.js"), "x.js": d("x.js")}
# private (non-exported) namesake in a star source
shapes["b04_private_namesake_mid_then_sibling"] = {"index.js": star("mid", "b"), "mid.js": star("a", "b"), "a.js": PRIV, "b.js": d("b.js")}
shapes["b05_private_namesake_same_level"] = {"index.js": star("a", "b"), "a.js": PRIV, "b.js": d("b.js")}
shapes["b06_private_namesake_deep_top_named"] = {"index.js": named("mid"), "mid.js": star("a", "b"), "a.js": PRIV, "b.js": d("b.js")}
# default import, namespace import in a barrel beside export *
shapes["b07_default_import_beside_star"] = {"index.js": star("m"), "m.js": "import f from './cfg.js';\nexport function use() { return f(); }\n" + star("b"), "cfg.js": 'export default function f() { return "cfg"; }\n', "b.js": d("b.js")}
shapes["b08_default_import_beside_star_cfg_seen"] = {"index.js": star("cfg", "m"), "m.js": "import f from './cfg.js';\nexport function use() { return f(); }\n" + star("b"), "cfg.js": 'export default function f() { return "cfg"; }\n', "b.js": d("b.js")}
shapes["b09_ns_import_beside_star"] = {"index.js": star("m"), "m.js": "import * as f from './ns.js';\nexport function use() { return f.x(); }\n" + star("b"), "ns.js": 'export function x() { return 1; }\n', "b.js": d("b.js")}
shapes["b10_ns_import_beside_star_seen"] = {"index.js": star("ns", "m"), "m.js": "import * as f from './ns.js';\nexport function use() { return f.x(); }\n" + star("b"), "ns.js": 'export function x() { return 1; }\n', "b.js": d("b.js")}
# duplicate star lines / two specifiers for one file
shapes["b11_dup_star_lines"] = {"index.js": star("m"), "m.js": star("b", "b"), "b.js": d("b.js")}
shapes["b12_two_specifiers_one_file"] = {"index.js": star("m"), "m.js": "export * from './b.js';\nexport * from './b';\n", "b.js": d("b.js")}
# named re-export whose target was walked earlier and found (valid)
shapes["b13_named_target_seen_found"] = {"index.js": star("a", "m"), "m.js": named("a") + star("c"), "a.js": d("a.js"), "c.js": d("c.js")}
shapes["b14_named_target_seen_found_rev"] = {"index.js": star("m", "a"), "m.js": named("a") + star("c"), "a.js": d("a.js"), "c.js": d("c.js")}
# diamond at different depths with named at the bottom
shapes["b15_diamond_mixed_depth"] = {"index.js": star("l1", "r1"), "l1.js": star("l2"), "l2.js": named("bot"), "r1.js": named("bot"), "bot.js": d("bot.js")}
# re-walk from nearer reveals a def past the bound on the long path
def longpath():
    fs = {"index.js": star("p1", "q")}
    for i in range(1, 7):
        fs[f"p{i}.js"] = star(f"p{i+1}")
    fs["p7.js"] = star("q")
    fs["q.js"] = star("r")
    fs["r.js"] = d("r.js")
    return fs
shapes["b16_long_path_then_short"] = longpath()
# local def + star: local wins
shapes["b17_local_const_and_star"] = {"index.js": star("m"), "m.js": "export const f = () => 'm-const';\n" + star("b"), "b.js": d("b.js")}
shapes["b18_local_class_and_star"] = {"app.js": "import { f as g } from './index.js';\nexport function main() { return new g(); }\nconsole.log(main().constructor.name);\n", "index.js": star("m"), "m.js": "export class f {}\n" + star("b"), "b.js": d("b.js")}
# re-export of an import (import then export {})
shapes["b19_import_then_export_list"] = {"index.js": star("m", "c"), "m.js": "import { f } from './a.js';\nexport { f };\n", "a.js": d("a.js"), "c.js": star("a")}
# export * as ns beside export *
shapes["b20_star_as_ns"] = {"index.js": "export * as f from './a.js';\n" + star("b"), "a.js": d("a.js"), "b.js": d("b.js"),
   "app.js": "import { f as g } from './index.js';\nexport function main() { return typeof g; }\nconsole.log(main());\n"}
