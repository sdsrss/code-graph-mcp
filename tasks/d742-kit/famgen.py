import os, random, shutil, sys
# famgen.py ROOT N SEED STEPS : chains of JS-family trees whose files move between .ts/.tsx/.js
root, n, seed, steps = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
rng = random.Random(seed)
EXT = ["ts", "tsx", "js"]
T = {
    "DIRECT": "export function f() { return 1; }\n",
    "RENAMED": "function r() { return 1; }\nexport { r as f };\n",
    "FWD_U": "export { f } from './u';\n",
    "STAR_U": "export * from './u';\n",
    "NONE": "export const z = 1;\n",
    "IMPEXP": "import { f } from './u';\nexport { f };\n",
    "TWO": "export function f() { return 1; }\nexport function q() { return f(); }\n",
}
U = {
    "DIRECT": "export function f() { return 'u'; }\n",
    "NONE": "export const y = 1;\n",
    "RENAMED": "function s() { return 'u'; }\nexport { s as f };\n",
}
C = {
    "F": "export function f() { return 'decoy'; }\n",
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
}
def jsify(body, ext):
    return body.replace("(x: any)", "(x)").replace("(o: any)", "(o)") if ext == "js" else body
def tree(st):
    fs = {}
    fs["t." + st["ext"]["t"]] = T[st["t"]]
    fs["u." + st["ext"]["u"]] = U[st["u"]]
    if C[st["c"]] is not None:
        fs["c." + st["ext"]["c"]] = C[st["c"]]
    for k in st["callers"]:
        e = st["ext"][k]
        fs[k + "." + e] = jsify(CALLERS[k], e) + ("// touched %d\n" % st["touch"].get(k, 0) if st["touch"].get(k) else "")
    return fs
for ci in range(n):
    names = ["t", "u", "c"] + list(CALLERS)
    st = {"t": rng.choice(list(T)), "u": rng.choice(list(U)), "c": rng.choice(list(C)),
          "ext": {k: rng.choice(EXT) for k in names},
          "callers": set(rng.sample(list(CALLERS), rng.randint(1, 4))), "touch": {}}
    dd = os.path.join(root, f"fc{seed}_{ci}")
    if os.path.exists(dd): shutil.rmtree(dd)
    for s in range(steps + 1):
        if s > 0:
            for _ in range(rng.randint(1, 2)):
                k = rng.random()
                if k < 0.3: st["t"] = rng.choice(list(T))
                elif k < 0.4: st["u"] = rng.choice(list(U))
                elif k < 0.5: st["c"] = rng.choice(list(C))
                elif k < 0.75: st["ext"][rng.choice(names)] = rng.choice(EXT)
                elif k < 0.85:
                    i = rng.choice(list(CALLERS)); st["callers"].symmetric_difference_update({i})
                    if not st["callers"]: st["callers"].add("a")
                else:
                    i = rng.choice(list(st["callers"]))
                    st["touch"][i] = st["touch"].get(i, 0) + 1
        for fn, body in tree(st).items():
            p = os.path.join(dd, f"s{s}", fn); os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "w").write(body)
