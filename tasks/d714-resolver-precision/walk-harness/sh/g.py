CJSAPP = "const { f: g } = require('./index.js');\nfunction main() { return g(); }\nconsole.log(main());\nmodule.exports = { main };\n"
CJSAPP2 = "const { f } = require('./index.js');\nfunction main() { return f(); }\nconsole.log(main());\nmodule.exports = { main };\n"
shapes["g01_cjs_destructure_esm_star_barrel"] = {"package.json": "{}\n", "app.js": CJSAPP, "index.js": star("a"), "a.js": d("a.js"), "z.js": d("z.js")}
shapes["g02_cjs_destructure_plain_name_esm_star"] = {"package.json": "{}\n", "app.js": CJSAPP2, "index.js": star("a"), "a.js": d("a.js"), "z.js": d("z.js")}
shapes["g03_cjs_destructure_esm_named_barrel"] = {"package.json": "{}\n", "app.js": CJSAPP2, "index.js": named("a"), "a.js": d("a.js"), "z.js": d("z.js")}
shapes["g04_cjs_barrel_spread"] = {"package.json": "{}\n", "app.js": CJSAPP2, "index.js": "module.exports = { ...require('./a.js') };\n", "a.js": "function f() { return 'a.js'; }\nmodule.exports = { f };\n", "z.js": "function f() { return 'z.js'; }\nmodule.exports = { f };\n"}
shapes["g05_cjs_barrel_named_prop"] = {"package.json": "{}\n", "app.js": CJSAPP2, "index.js": "const { f } = require('./a.js');\nmodule.exports = { f };\n", "a.js": "function f() { return 'a.js'; }\nmodule.exports = { f };\n", "z.js": "function f() { return 'z.js'; }\nmodule.exports = { f };\n"}
