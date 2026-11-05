import os, random, sys, shutil
root = sys.argv[1]; n = int(sys.argv[2]); seed = int(sys.argv[3])
rng = random.Random(seed)
DEFS = ["d0", "d1", "d2"]; BAR = ["b0", "b1", "b2"]
def defbody(fn_node, exp):
    decl = "function r() { return 1; }\n" if fn_node else "let r = 1;\n"
    return decl + {"plain": "export { r };\n", "renamed": "export { r as f };\n", "default": "export default r;\n"}[exp]
def gen():
    st = {}
    for d in DEFS:
        st[d] = [rng.random() < 0.5, rng.choice(["plain", "renamed", "default"])]
    bars = {}
    for i, b in enumerate(BAR):
        lines = []
        for _ in range(rng.randint(1, 2)):
            t = rng.choice(BAR[i+1:] + DEFS)
            k = rng.random()
            if k < 0.3: lines.append(f"export * from './{t}.js';")
            elif k < 0.55: lines.append(f"export {{ f }} from './{t}.js';")
            elif k < 0.8: lines.append(f"export {{ r as f }} from './{t}.js';")
            else: lines.append(f"export {{ default as f }} from './{t}.js';")
        bars[b] = "\n".join(lines) + "\n"
    return st, bars
def files(st, bars):
    fs = {f"{d}.js": defbody(*st[d]) for d in DEFS}
    fs.update({f"{b}.js": s for b, s in bars.items()})
    fs["decoy.js"] = "export function f() { return 'decoy-f'; }\nexport function r() { return 'decoy-r'; }\n"
    fs["app.js"] = "import { f } from './b0.js';\nexport function main() { return f(); }\n"
    fs["app2.js"] = "import { f as g } from './b1.js';\nexport function main2() { return g(); }\n"
    return fs
for c in range(n):
    st, bars = gen()
    before = files(st, bars)
    st2 = {k: list(v) for k, v in st.items()}
    for _ in range(rng.randint(1, 2)):
        d = rng.choice(DEFS)
        if rng.random() < 0.7: st2[d][0] = not st2[d][0]
        else: st2[d][1] = rng.choice(["plain", "renamed", "default"])
    after = files(st2, bars)
    dd = f"{root}/y{seed}_{c}"
    if os.path.exists(dd): shutil.rmtree(dd)
    for side, fs in (("before", before), ("after", after)):
        os.makedirs(f"{dd}/{side}")
        for fn, body in fs.items():
            open(f"{dd}/{side}/{fn}", "w").write(body)
