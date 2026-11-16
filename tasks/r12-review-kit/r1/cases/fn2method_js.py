FILES = {
 'x/x.js': 'export class X { drainQueue() {} }\n',
 'conn/conn.js': 'export function drainQueue(c) {}\nexport class Conn {}\n',
 'app/use.js': 'export function use(c) { c.drainQueue() }\n',
}
EDITS = [
 {'conn/conn.js': 'export class Conn { drainQueue() {} }\n'},
]
