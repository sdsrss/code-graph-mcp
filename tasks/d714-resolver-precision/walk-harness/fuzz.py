import os, random, sys, shutil
root = sys.argv[1]; n = int(sys.argv[2]); seed = int(sys.argv[3])
rng = random.Random(seed)
DEFS = ["d0", "d1", "d2", "d3"]
BAR = ["b0", "b1", "b2", "b3", "b4", "b5"]
def gen():
    files = {}
    for dn in DEFS:
        body = ""
        if rng.random() < 0.8: body += f'export function f() {{ return "{dn}"; }}\n'
        if rng.random() < 0.5: body += f'export function g() {{ return "{dn}g"; }}\n'
        if rng.random() < 0.2: body += 'function h() {}\nexport class K { f() { return 0; } }\n'
        body += f"export function own_{dn}() {{ return 0; }}\n"
        files[f"{dn}.js"] = body
    for i, b in enumerate(BAR):
        lines = []
        for _ in range(rng.randint(1, 4)):
            tgt = rng.choice(BAR[i+1:] + DEFS if rng.random() < 0.8 else BAR + DEFS)
            if tgt == b: continue
            k = rng.random()
            nm = rng.choice(["f", "g"])
            if k < 0.45: lines.append(f"export * from './{tgt}.js';")
            elif k < 0.7: lines.append(f"export {{ {nm} }} from './{tgt}.js';")
            elif k < 0.8: lines.append(f"export {{ {nm} as alias_{nm}{rng.randint(0,9)} }} from './{tgt}.js';")
            else:
                u = rng.randint(0, 999)
                lines.append(f"import {{ {nm} as p{u} }} from './{tgt}.js';\nexport function use{u}() {{ return p{u}(); }}")
        if rng.random() < 0.1: lines.append(f'export function f() {{ return "{b}"; }}')
        files[f"{b}.js"] = "\n".join(lines) + "\n"
    files["app.js"] = "import { f, g } from './b0.js';\nexport function main() { return f() + g(); }\n"
    files["app2.js"] = "import { f as ff } from './b1.js';\nexport function main2() { return ff(); }\n"
    return files
def edit(files):
    out = dict(files)
    for _ in range(rng.randint(1, 3)):
        fn = rng.choice(sorted(out))
        k = rng.random()
        if k < 0.6: out[fn] = out[fn] + "// e\n"
        elif k < 0.8 and fn.startswith("d"): out[fn] = "\n" + out[fn]
        elif fn.startswith("b"):
            ls = out[fn].splitlines()
            if len(ls) > 1:
                ls.pop(rng.randrange(len(ls)))
                out[fn] = "\n".join(ls) + "\n"
        elif fn.startswith("d"):
            out[fn] = out[fn] + f'export function f() {{ return "{fn}2"; }}\n' if "function f()" not in out[fn] else out[fn].replace("export function f()", "export function f_renamed()")
    return out
for c in range(n):
    before = gen(); after = edit(before)
    d = f"{root}/z{seed}_{c}"
    if os.path.exists(d): shutil.rmtree(d)
    for side, fs in (("before", before), ("after", after)):
        os.makedirs(f"{d}/{side}")
        for fn, body in fs.items():
            open(f"{d}/{side}/{fn}", "w").write(body)
