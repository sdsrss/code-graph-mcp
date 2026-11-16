FILES = {
 'src/base.ts': 'export class Base {}\n',
 'src/app.ts': "import { Base as B } from './base.js'\nexport class App extends B {}\n",
 'src/other/b.ts': 'export class B {}\n',
}
