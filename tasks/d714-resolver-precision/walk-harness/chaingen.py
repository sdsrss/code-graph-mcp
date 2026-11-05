import os, random, shutil, sys
# chaingen.py ROOT N SEED STEPS : N chains of STEPS successive trees as ROOT/ch<SEED>_<i>/s0..sSTEPS
root, n, seed, steps = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
rng = random.Random(seed)
D = {
    "RENAMED": "function r() { return 1; }\nexport { r as f };\n",
    "DIRECT": "export function f() { return 1; }\n",
    "LOCALF": "function f() { return 1; }\nexport { f };\n",
    "LETR": "let r = 1;\nexport { r as f };\n",
    "RPRIV": "function r() { return 0; }\nexport function f() { return 1; }\n",
    "FWD_E": "export { f } from './e.js';\n",
    "RFWD_E": "export { r as f } from './e.js';\n",
    "STAR_E": "export * from './e.js';\n",
    "NONE": "export const z = 1;\n",
    "DEFAULT": "function r() { return 1; }\nexport default r;\nexport { r as f };\n",
    "SWAP": "function f() { return 1; }\nfunction r() { return 2; }\nexport { r as f, f as r };\n",
    "S_AS_F": "function r() { return 1; }\nfunction s() { return 2; }\nexport { s as f };\n",
}
E = {
    "RENAMED": "function r() { return 'e'; }\nexport { r as f };\n",
    "DIRECT": "export function f() { return 'e'; }\n",
    "EXPR": "export function r() { return 'e'; }\n",
    "NONE": "export const y = 1;\n",
    "LETR": "let r = 1;\nexport { r as f, r };\n",
}
M = {
    "FWD_D": "export { f } from './d.js';\n",
    "STAR_D": "export * from './d.js';\n",
    "RFWD_E": "export { r as f } from './e.js';\n",
    "OWN": "export function f() { return 'm'; }\n",
    "IMPEXP": "import { f } from './d.js';\nexport { f };\nexport function mu() { return f(); }\n",
}
C = {
    "F": "export function f() { return 'decoy-f'; }\n",
    "FR": "export function f() { return 'decoy-f'; }\nexport function r() { return 'decoy-r'; }\n",
    "R": "export function r() { return 'decoy-r'; }\n",
    "Q": "export const q = 1;\n",
    None: None,
}
IMP = {
    "b.js": "import { f } from './d.js';\nexport function use() { return f(); }\n",
    "app.js": "import { f } from './m.js';\nexport function main() { return f(); }\n",
    "app2.js": "import { f as g } from './d.js';\nexport function main2() { return g(); }\n",
    "app3.js": "import { f } from './e.js';\nexport function main3() { return f(); }\n",
}
def tree(st):
    fs = {"d.js": D[st["d"]], "e.js": E[st["e"]], "m.js": M[st["m"]]}
    if C[st["c"]] is not None: fs["c.js"] = C[st["c"]]
    for k, v in IMP.items():
        if k in st["imps"]:
            fs[k] = v + ("// touched %d\n" % st["touch"].get(k, 0) if st["touch"].get(k) else "")
    return fs
for c in range(n):
    st = {"d": rng.choice(list(D)), "e": rng.choice(list(E)), "m": rng.choice(list(M)), "c": rng.choice(list(C)),
          "imps": set(rng.sample(list(IMP), rng.randint(1, 4))), "touch": {}}
    dd = os.path.join(root, f"ch{seed}_{c}")
    if os.path.exists(dd): shutil.rmtree(dd)
    for s in range(steps + 1):
        if s > 0:
            for _ in range(rng.randint(1, 2)):
                k = rng.random()
                if k < 0.45: st["d"] = rng.choice(list(D))
                elif k < 0.6: st["e"] = rng.choice(list(E))
                elif k < 0.72: st["m"] = rng.choice(list(M))
                elif k < 0.85: st["c"] = rng.choice(list(C))
                else:
                    i = rng.choice(list(IMP))
                    st["touch"][i] = st["touch"].get(i, 0) + 1
                    st["imps"].add(i)
        for fn, body in tree(st).items():
            p = os.path.join(dd, f"s{s}", fn); os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "w").write(body)
