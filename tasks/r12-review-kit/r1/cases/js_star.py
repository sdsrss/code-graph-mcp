FILES = {
 'lib/impl.ts': 'export class Widget {}\n',
 'lib/index.ts': "export * from './impl'\n",
 'app.tsx': "import { Widget as W } from './lib'\nexport class Fancy extends W {}\nexport interface I extends W {}\n",
 'app2.js': "import { Widget } from './lib/index.ts'\nexport class Plain extends Widget {}\n",
 'decoy/w.ts': 'export class W {}\nexport class Widget {}\n',
}
EDITS = [
 {'lib/impl.ts': 'export class Gadget {}\n'},
 {'lib/impl2.ts': 'export class Widget {}\n', 'lib/index.ts': "export * from './impl'\nexport * from './impl2'\n"},
 {'lib/impl2.ts': None},
]
