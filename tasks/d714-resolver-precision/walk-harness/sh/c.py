USE = "export function use() { return f(); }\n"
shapes["c01_import_from_ancestor_barrel"] = {"index.js": star("m"), "m.js": "import { f } from './index.js';\n" + USE + star("b"), "b.js": d("b.js")}
shapes["c02_dup_star_in_plain_import_target"] = {"index.js": star("m"), "m.js": "import { f } from './q.js';\n" + USE + star("b"), "q.js": "export const f = 1;\n" + star("z", "z"), "z.js": "export function zz() {}\n", "b.js": d("b.js")}
shapes["c02b_no_dup_control"] = {"index.js": star("m"), "m.js": "import { f } from './q.js';\n" + USE + star("b"), "q.js": "export const f = 1;\n" + star("z"), "z.js": "export function zz() {}\n", "b.js": d("b.js")}
shapes["c05_import_from_sibling_barrel_cycle"] = {"index.js": star("p", "m"), "p.js": star("m"), "m.js": "import { f } from './p.js';\n" + USE + star("b"), "b.js": d("b.js")}
shapes["c06_nested_pkg_import_from_top"] = {"index.js": star("utils/index"), "utils/index.js": "import { f } from '../index.js';\nexport function useTop() { return f(); }\nexport * from './cfg.js';\n", "utils/cfg.js": d("utils/cfg.js")}
