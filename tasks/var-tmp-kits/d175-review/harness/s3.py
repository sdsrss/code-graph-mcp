SCENARIOS = {}
SCENARIOS["ts_iface_to_class"] = [
    {"src/types.ts": "export interface Response {\n  ok: boolean\n}\n",
     "src/a.ts": "export function mk() {\n  return new Response('x')\n}\n",
     "src/b.ts": "import { Response } from './types'\nexport function mk2(): Response {\n  return new Response()\n}\n",
     "src/c.js": "export function mk3() {\n  return new Response('y')\n}\n"},
    {"src/types.ts": "export class Response {\n  ok = true\n}\n"},
    {"src/types.ts": "export type Response = { ok: boolean }\n"},
    {"src/types.ts": "export interface Response {\n  ok: boolean\n}\nexport function Response(): Response { return { ok: true } }\n"},
    {"src/other.ts": "export class Response {}\n"},
    {"src/other.ts": None},
]
SCENARIOS["ts_same_file_type"] = [
    {"src/u.ts": "type User = { id: number }\nexport function f() {\n  return User()\n}\ninterface Svc { run(): void }\nexport function g(s: Svc) {\n  s.run()\n  return Svc()\n}\n",
     "src/v.tsx": "interface Props { a: number }\nexport function C(p: Props) {\n  return Props(p)\n}\n"},
    {"src/u.ts": "const User = (x?: unknown) => x\ntype User = { id: number }\nexport function f() {\n  return User()\n}\ninterface Svc { run(): void }\nexport function g(s: Svc) {\n  s.run()\n  return Svc()\n}\n"},
    {"src/w.ts": "export function User() { return 1 }\n"},
]
