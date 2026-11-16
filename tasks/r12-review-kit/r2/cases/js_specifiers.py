FILES = {
 'src/base.ts': 'export class Base {}\nclass Hono {}\nexport { Hono as HonoBase }\n',
 'src/app_js.ts': "import { Base as B } from './base.js'\nexport class AppJs extends B {}\n",
 'src/app_js_alias.ts': "import { HonoBase } from './base.js'\nexport class AppJsAlias extends HonoBase {}\n",
 'src/app_plain.ts': "import { HonoBase } from './base'\nexport class AppPlain extends HonoBase {}\n",
 'src/app_plain_named.ts': "import { Base } from './base.js'\nexport class AppPlainNamed extends Base {}\n",
 'src/other/b.ts': 'export class B {}\n',
 'src/cjs.js': "const { Base } = require('./base2')\nclass C extends Base {}\nmodule.exports = C\n",
 'src/base2.js': "class Real {}\nmodule.exports = { Base: Real }\n",
 'src/dflt.ts': "import D from './dbase'\nexport class Dd extends D {}\n",
 'src/dbase.ts': "export default class Inner {}\n",
}
