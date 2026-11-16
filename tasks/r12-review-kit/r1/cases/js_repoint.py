FILES = {
 'hono-base.ts': 'class Hono { a() {} }\nclass Other {}\nexport { Hono as HonoBase }\n',
 'hono.ts': "import { HonoBase } from './hono-base'\nexport class App extends HonoBase {}\n",
 'dir/tiny.ts': "import { HonoBase as Base } from '../hono-base'\nexport class Tiny extends Base {}\n",
}
EDITS = [
 {'hono-base.ts': 'class Hono { a() {} }\nclass Other {}\nexport { Other as HonoBase }\n'},
 {'hono-base.ts': 'class Hono { a() {} }\nclass Other {}\nexport { Hono as HonoBase }\n'},
 {'hono-base.ts': 'class Hono { a() {} b() {} }\nclass Other {}\nfunction helper() {}\nexport { Hono as HonoBase }\n'},
 {'hono-base.ts': 'class Hono { a() {} b() {} }\nclass Other {}\nexport { Hono as HonoBase, Other as Hono2 }\n'},
]
