import os, random, shutil, sys
# famgen2.py ROOT N SEED STEPS: JS-family edit chains aimed at the D#746 delta (two nodes of a name,
# aliases imported under two names, renamed exports, barrels, deletions, own-language twins)
root, n, seed, steps = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
rng = random.Random(seed)
EXT = ["ts", "tsx", "js"]
K = "export class K { f() { return 2; } }\n"
KR = "export class K { r() { return 2; } }\n"
T = {
    "DIRECT": "export function f() { return 1; }\n",
    "RENAMED": "function r() { return 1; }\nexport { r as f };\n",
    "RENAMED_M": "function r() { return 1; }\nexport { r as f };\n" + KR,
    "RENAMED2": "function r() { return 1; }\nexport { r as f, r as g };\n",
    "RENAMED2_M": "function r() { return 1; }\nexport { r as f, r as g };\n" + KR,
    "METHOD": "export function f() { return 1; }\n" + K,
    "NESTED": "export function f() { return 1; }\nexport function q() { function f() { return 3; } return f(); }\n",
    "NS": "export function f() { return 1; }\nexport namespace N { export function f() { return 4; } }\n",
    "OBJ": "export function f() { return 1; }\nexport const o = { f() { return 5; } };\n",
    "ONLY_M": K,
    "ALIAS": "export function f() { return 1; }\nexport { f as g };\n",
    "ALIAS_M": "export function f() { return 1; }\nexport { f as g };\n" + K,
    "DEFAULT": "export function f() { return 1; }\nexport default f;\n",
    "DEFAULT_M": "export function f() { return 1; }\nexport default f;\n" + K,
    "FWD_U": "export { f } from './u';\n",
    "STAR_U": "export * from './u';\n",
    "NONE": "export const z = 1;\n",
    "GONE": None,
}
U = {
    "DIRECT": "export function f() { return 'u'; }\n",
    "NONE": "export const y = 1;\n",
    "RENAMED": "function s() { return 'u'; }\nexport { s as f };\n",
    "METHOD": "export function f() { return 'u'; }\n" + K,
}
IDX = {
    "STAR": "export * from './t';\n",
    "NAMED": "export { f } from './t';\n",
    "ALIAS": "export { f, f as g } from './t';\n",
    "U": "export { f } from './u';\n",
    None: None,
}
C = {
    "F": "export function f() { return 'decoy'; }\n",
    "FG": "export function f() { return 'decoy'; }\nexport function g() { return 'decoy'; }\n",
    "NONE": "export const w = 1;\n",
    None: None,
}
CALLERS = {
    "a": "import { f } from './t';\nexport function A() { return f(); }\n",
    "b": "import { f } from './u';\nexport function B() { return f(); }\n",
    "m": "export function M() { return f(); }\n",
    "n": "import { f as g } from './t';\nexport function N(x: any) { return g() + f(); }\n",
    "k": "import { f } from './t';\nexport function K(o: any) { o.f(); return f(); }\n",
    "d": "import { f } from './c';\nexport function D() { return f(); }\n",
    "e": "import { f, g } from './t';\nexport function E() { return f(); }\n",
    "e2": "import { f, g } from './t';\nexport function E2() { return f() + g(); }\n",
    "i": "import { f } from './idx';\nexport function I() { return f(); }\n",
    "j": "import { f, g } from './idx';\nexport function J() { return f() + g(); }\n",
    "p": "import p, { f } from './t';\nexport function P() { return f() + p(); }\n",
}
def jsify(body, ext):
    return body.replace("(x: any)", "(x)").replace("(o: any)", "(o)") if ext == "js" else body
def tree(st):
    fs = {}
    if T[st["t"]] is not None: fs["t." + st["ext"]["t"]] = jsify(T[st["t"]], st["ext"]["t"]) + ("// tt %d\n" % st["tt"] if st["tt"] else "")
    fs["u." + st["ext"]["u"]] = U[st["u"]]
    if IDX[st["idx"]] is not None: fs["idx." + st["ext"]["idx"]] = IDX[st["idx"]]
    if C[st["c"]] is not None: fs["c." + st["ext"]["c"]] = C[st["c"]]
    for k in st["callers"]:
        e = st["ext"][k]
        fs[k + "." + e] = jsify(CALLERS[k], e) + ("// touched %d\n" % st["touch"].get(k, 0) if st["touch"].get(k) else "")
    return fs
for ci in range(n):
    names = ["t", "u", "c", "idx"] + list(CALLERS)
    st = {"t": rng.choice(list(T)), "u": rng.choice(list(U)), "c": rng.choice(list(C)), "idx": rng.choice(list(IDX)), "tt": 0,
          "ext": {k: rng.choice(EXT) for k in names},
          "callers": set(rng.sample(list(CALLERS), rng.randint(1, 4))), "touch": {}}
    dd = os.path.join(root, f"g{seed}_{ci}")
    if os.path.exists(dd): shutil.rmtree(dd)
    for s in range(steps + 1):
        if s > 0:
            for _ in range(rng.randint(1, 2)):
                k = rng.random()
                if k < 0.35: st["t"] = rng.choice(list(T))
                elif k < 0.42: st["u"] = rng.choice(list(U))
                elif k < 0.5: st["c"] = rng.choice(list(C))
                elif k < 0.57: st["idx"] = rng.choice(list(IDX))
                elif k < 0.7: st["ext"][rng.choice(names)] = rng.choice(EXT)
                elif k < 0.8:
                    i = rng.choice(list(CALLERS)); st["callers"].symmetric_difference_update({i})
                    if not st["callers"]: st["callers"].add("a")
                elif k < 0.9: st["tt"] += 1
                else:
                    i = rng.choice(list(st["callers"]))
                    st["touch"][i] = st["touch"].get(i, 0) + 1
        for fn, body in tree(st).items():
            p = os.path.join(dd, f"s{s}", fn); os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "w").write(body)
