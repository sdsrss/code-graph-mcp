FILES = {
 'src/impl/base.ts': 'export class Base { run() {} }\n',
 'src/index.ts': "export { Base as PublicBase } from './impl/base'\n",
 'src/app.ts': "import { PublicBase } from './index'\nexport class App extends PublicBase {}\n",
 'src/other.ts': 'export class PublicBase {}\nexport class Base {}\n',
}
EDITS = [
 {'src/impl/base.ts': 'export class Base2 { run() {} }\nexport { Base2 as Base }\n'},
 {'src/impl/base.ts': 'export class Nope {}\n'},
 {'src/impl/base.ts': 'export class Base { run() {} }\n'},
 {'src/index.ts': "export { Base as PublicBase } from './impl/base2'\n", 'src/impl/base2.ts': 'export class Base {}\n'},
 {'src/impl/base2.ts': 'export class Other {}\nexport { Other as Base }\n'},
]
