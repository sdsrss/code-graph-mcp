FILES = {
 'base.ts': 'export default class Base {}\nexport const Mixed = class {}\nexport function Ctor() {}\nexport class Real {}\nexport { Real as Alias }\n',
 'use.ts': "import Base from './base'\nimport { Mixed, Ctor, Alias as R } from './base'\nimport * as ns from './base'\n"
           "export class A extends Base {}\nexport class B extends Mixed {}\nexport class C extends Ctor {}\nexport class D extends ns.Real {}\nexport class E extends R {}\n"
           "function f() { class R {}; class F extends R {} }\n",
 'other.ts': 'export class Base {}\nexport class R {}\nexport function Mixed() {}\n',
}
