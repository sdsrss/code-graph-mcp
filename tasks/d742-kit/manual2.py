import os, shutil, sys
root = sys.argv[1]
def caller(imp="import { f } from './t';", body="return f();", name="A"):
    return f"{imp}\nexport function {name}(o: any) {{ {body} }}\n"
F = "export function f() { return 1; }\n"
CH = {
 "gain_method_plain": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": F + "export class K { f() { return 2; } }\n"},
 ],
 "gain_method_plain_js": [
   {"a.tsx": caller(), "t.js": F},
   {"a.tsx": caller(), "t.js": F + "export class K { f() { return 2; } }\n"},
 ],
 "gain_unexported_class_method": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": F + "class K { f() { return 2; } }\nexport const k = new K();\n"},
 ],
 "gain_object_method": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": F + "export const o = { f() { return 2; } };\n"},
 ],
 "gain_interface_member": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": F + "export interface I { f(): number }\n"},
 ],
 "gain_namespace_fn": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": F + "export namespace N { export function f() { return 2; } }\n"},
 ],
 "lose_method_plain": [
   {"a.tsx": caller(), "t.ts": F + "export class K { f() { return 2; } }\n"},
   {"a.tsx": caller(), "t.ts": F},
 ],
 "through_barrel_gain_method": [
   {"a.tsx": caller(imp="import { f } from './b';"), "b.ts": "export { f } from './t';\n", "t.ts": F},
   {"a.tsx": caller(imp="import { f } from './b';"), "b.ts": "export { f } from './t';\n", "t.ts": F + "export class K { f() { return 2; } }\n"},
 ],
 "callee_unexports": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": "function f() { return 1; }\nexport const z = f;\n"},
 ],
 "callee_becomes_default": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": "export default function f() { return 1; }\n"},
 ],
 "callee_becomes_const": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": "export const f = () => 1;\n"},
 ],
 "callee_becomes_class": [
   {"a.tsx": caller(), "t.ts": F},
   {"a.tsx": caller(), "t.ts": "export class f { }\n"},
 ],
 # import target in a sibling file also defines a method of the name in s0; then touched
 "two_targets_then_touch": [
   {"a.tsx": caller(), "t.ts": F + "export class K { f() { return 2; } }\n"},
   {"a.tsx": caller(), "t.ts": "// touched\n" + F + "export class K { f() { return 2; } }\n"},
 ],
 # the caller file holds the call in two functions; callee gains a second node
 "two_callers_gain": [
   {"a.tsx": caller() + "export function B() { return f(); }\n", "t.ts": F},
   {"a.tsx": caller() + "export function B() { return f(); }\n", "t.ts": F + "export class K { f() { return 2; } }\n"},
 ],
 # same-language analogs for comparison
 "samelang_gain_method_plain": [
   {"a.ts": caller(), "t.ts": F},
   {"a.ts": caller(), "t.ts": F + "export class K { f() { return 2; } }\n"},
 ],
}
for name, steps in CH.items():
    d = os.path.join(root, name)
    if os.path.exists(d):
        shutil.rmtree(d)
    for i, tree in enumerate(steps):
        for fn, body in tree.items():
            p = os.path.join(d, f"s{i}", fn)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "w").write(body)
