step_1() { printf '\n// touch\n' >> src/hono.test.ts; }
step_2() { cp src/types.test.ts ../types.bak; sed -i '1841s/app.use(mw1, mw2)/app.use(mw1)/' src/types.test.ts; }
step_3() { cp ../types.bak src/types.test.ts; touch src/types.test.ts; }
step_4() { cp src/hono.test.ts ../hono.bak; sed -i '2848s/const mw2 =/const mw2x =/' src/hono.test.ts; }
step_5() { cp ../hono.bak src/hono.test.ts; touch src/hono.test.ts; }
step_6() { printf '\n// touch\n' >> src/middleware/serve-static/index.test.ts; }
step_7() { cp src/middleware/serve-static/index.ts ../ss.bak; rm src/middleware/serve-static/index.ts; }
step_8() { cp ../ss.bak src/middleware/serve-static/index.ts; touch src/middleware/serve-static/index.ts; }
step_9() { python3 - <<'P'
p='src/context.ts'; s=open(p).read()
s=s.replace('  json: JSONRespond = <','  shout = (t: string): Response => this.text(t.toUpperCase())\n  json: JSONRespond = <',1); open(p,'w').write(s)
P
cat > src/shout-user.ts <<'P'
import type { Context } from './context'
export const useShout = (c: Context) => c.shout('hi')
export function viaText(c: Context) { return c.text('x') }
P
}
step_10() { cp src/types.test.ts ../types2.bak; rm src/types.test.ts; }
step_11() { git mv -f src/hono.test.ts src/hono2.test.ts 2>/dev/null || mv src/hono.test.ts src/hono2.test.ts; cp ../types2.bak src/types.test.ts; touch src/types.test.ts; }
