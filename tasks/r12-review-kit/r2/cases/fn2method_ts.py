FILES = {
 'x/x.ts': 'export class X { drainQueue() {} }\n',
 'conn/conn.ts': 'export function drainQueue(c: any) {}\nexport class Conn {}\n',
 'app/use.ts': 'export function use(c: any) { c.drainQueue() }\n',
}
EDITS = [
 {'conn/conn.ts': 'export class Conn { drainQueue() {} }\n'},
]
