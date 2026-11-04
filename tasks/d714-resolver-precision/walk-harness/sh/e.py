NS = "import * as ns from './index.js';\nexport function main() { return ns.f(); }\nconsole.log(main());\n"
shapes["e01_ns_star_with_decoy"] = {"app.js": NS, "index.js": star("a"), "a.js": d("a.js"), "z.js": d("z.js")}
shapes["e02_ns_named_with_decoy"] = {"app.js": NS, "index.js": named("a"), "a.js": d("a.js"), "z.js": d("z.js")}
shapes["e03_ns_star_chain_with_decoy"] = {"app.js": NS, "index.js": star("m"), "m.js": star("a"), "a.js": d("a.js"), "z.js": d("z.js")}
shapes["e04_named_import_star_with_decoy"] = {"index.js": star("a"), "a.js": d("a.js"), "z.js": d("z.js")}
