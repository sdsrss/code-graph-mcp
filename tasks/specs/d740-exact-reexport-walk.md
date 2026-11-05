# D#740 + D#739：JS/TS 重导出遍历按 ECMAScript ResolveExport 精确实现（2026-11-05）

状态：进行中（L3，FULL-lite；新增表已获用户授权 2026-11-05）

## 背景与目标

0.185.0 的重导出遍历（`resolve.rs::js_export_through_reexports`）读的是 barrel 自己已绑定的 `imports` 边。这些边是解析的产物：
- 绑没绑上取决于延迟阶段的轮次（按轮绑定会先看到"歧义"）；
- 一条重导出按名字绑到 N 个同名定义就有 N 行；
- 行没有顺序，文件重抽后它的行排到最后；
- 普通 import 和重导出分不开（遍历把普通 import 也当重导出）；
- `export { r as f }` 按原名 `r` 跟进，`export * as ns` 当 `export *` 走；
- 目标文件不存在时 `export *` 不留任何记录（D#739）。

结果（0.185.0 实测）：随机 barrel 图增量≠重建 33/400（0.184.0 0/400）；Node 合法探测 44/62；稠密 `export *` 60 个文件全量 1,019–1,025 ms（0.184.0 77–96 ms）；`export *` 集合一变，经该 barrel 具名导入的文件全部重抽（claude-mem-lite `utils.mjs` 90 个，1,026–1,077 ms，对比 157–169 ms）。

目标：遍历只读解析时写下的语法记录，按 ECMAScript `ResolveExport` 求值，是全部文件内容和文件集的纯函数；增量扇出只拉"遍历确实经过变动处"的导入方。

## 范围

- 做：
  - 新表 `js_export_entries`（additive，`CREATE TABLE IF NOT EXISTS`，外键挂 `<module>` 节点 `ON DELETE CASCADE`，同 `cpp_fields`；不升 SCHEMA_VERSION，升 INDEX_VERSION）。
  - 解析器按源码顺序产出每个 JS/TS 文件的 export 条目：
    - 本地：`export function/class/const/let/var/function*`、TS `declare`/`namespace`/`enum`/`interface`/`type`、`export { x as y }`（x 不是导入的绑定）、`export default <具名声明|标识符>`；
    - 间接：`export { a as b } from 's'`、`export * as ns from 's'`、`export { x }`（x 是 `import`/`require` 来的绑定）、CJS `module.exports = { y: x }`、`exports.y = x`（x 是 require 来的绑定）；
    - 星号：`export * from 's'`、CJS `module.exports = require('s')`、`module.exports = { ...require('s') }`。
  - 遍历：`ResolveExport(file, name, resolveSet)`，resolveSet 按 (文件, 名字)；本地 → 间接（找到即为答案）→ 星号（`default` 不经星号；两个来源不同即歧义）；递归深度上限 256（超出则整次查找为"未知"，确定性）；每次索引过程按 (文件, 名字) 记忆顶层答案。
  - 延迟阶段：具名导入在指定文件里找不到名字时直接走一次遍历，去掉 `settle_js_reexports` 的按轮绑定。
  - 增量：Phase 0-pre 把"条目的说明符解析结果因文件出现/消失而改变"的文件加入本轮重抽（D#739）；扇出按快照比较本轮文件的条目、导出名、节点名，只拉遍历访问集碰到漂移处的导入方。
- 不做：
  - 命名空间成员调用 `ns.f()` 的解析路径（不走遍历，保持原样）。
  - 给 `export { f }` 子句补 `exports` 边（影响死代码等别的输出，另记）。
  - D#735（下个会话）、D#736（不跑付费评测）。

## 验收标准

- [ ] AC1：fuzz 400 个随机 barrel 图增量=重建 0/400 — 验证：`fuzz.py ROOT 200 7`、`fuzz.py ROOT 200 11` + `ivf.sh`
- [ ] AC2：Node 合法探测程序：调用边与导入边和 `node app.js` 一致；不一致的逐条说明原因 — 验证：`shapes.py sh/*.py new,old`
- [ ] AC3：kit38 38/38、r4sets61 61/61 一致（含 D#739 的 4 个形状） — 验证：`ivf.sh`
- [ ] AC4：9 个语料全量边集与 0.185.0 的差异逐条归因（只应出现在旧遍历读错的形状上）
- [ ] AC5：计时：稠密 `export *` 60 文件全量 ≤ 0.184.0 的 2 倍；claude-mem-lite `utils.mjs` 加一行 `export *` 的增量不再重抽 90 个文件
- [ ] AC6：`cargo test` 全部通过、clippy 两条 feature 腿、fmt
- [ ] AC7：预发评审（新 subagent，盲简报）

## 方案

见上面"做"。决定：
- 表而不是边元数据：遍历的输入必须在延迟阶段开始前就齐全、且与其他文件的绑定结果无关；`imports` 边做不到（目标不存在时没有边，绑定在延迟阶段才写）。`cpp_fields` 是同一形状的先例。
- 本地导出判断在遍历内部是严格的（只认导出的名字：`exports` 边的发布名 ∪ 表里的本地条目）；导入方直接目标文件的查找仍是宽松的（文件里任何同名节点），在遍历之外，行为不变。
- 遍历结果是 (文件, 本地绑定名)，消费者再映射到节点：导入边取该文件里同名的任何节点；调用取该文件同名的顶层函数；命名空间取该文件的 `<module>`。
- 导入名与绑定到的节点名不同（改名重导出、命名空间）时，导入边元数据加 `"imported"`；延迟阶段（含 requeue）、扇出、Phase 2d-bind 的唯一导入表和剪枝用的导入表都优先读它（否则 `import { f }` + `f()` 经 `export { r as f }` 时调用按名字绑到别处的 `f`）。
- 一个名字只以 `default` 导出时（`export default function f`），`exports` 边按原名发布的 `f` 不算导出（b08）。
- 扇出不需要节点名快照：导入方的直接目标文件在本轮里、且现在有这个名字的节点、而边不指向它 → 拉（重建会直接绑定）。

## 计划

- [x] S1：解析器 `js_export_entries` + 单测（33 种形状）
- [x] S2：表、写入、INDEX_VERSION 137、指纹
- [x] S3：遍历重写（`pipeline/js_exports.rs`）+ 延迟阶段/调用/pending 三个消费者；探测程序移植成管线测试
- [x] S4：Phase 0-pre（D#739）+ 扇出重写；增量=重建测试
- [x] S5：fuzz/探测/kit38/r4sets61/语料/计时
- [x] S6：D#738（afadb65e）
- [ ] S7：CHANGELOG、发版（CHANGELOG 已写，0.186.0）

## 决策与进展

- 2026-11-05：用户授权新增表；D#735 下个会话；D#736 不跑。
- 变异（9c481428 的副本、独立 target，24 个）：首轮 15 个被杀、3 个写错不能编译；为存活的补测试（星号环、自指 barrel、歧义来源不被后面的来源掩盖、`export *` 不传 `default`、包名重导出 = 未知、命名空间调用不进 pending、改名重导出的按名字调用要被剪掉、删除目标文件后的 requeue、星号目标出现时 barrel 自己的边——原测试行的三个同名定义掩盖了 Phase 0-pre）。重跑存活者：M2 M4 M5 M6 M8 M12 M15 M19 M20 被杀。
- 删掉两段没有变异能杀掉的扇出分支：新文件"所有名字漂移"（新文件只能经由一个解析结果变了的已有文件被访问到，那个文件已漂移；改为与空快照比较）；"直接目标文件新增同名节点"检查（新同名定义那一轮的 module-import 分支已经拉这个导入方，实测日志 `1 bare-name caller(s)`）。余下等价变异：新文件跳过而不是与空快照比较；requeue 读目标名（导入方同一轮会被重抽）。
- 2026-11-05 发版进度：发版提交 1dabea53（分支 d740-exact-reexport-walk，未推送）；插件清点（干净 clone、沙箱 HOME）0.186.0：Skills 2 · Agents 1 · Hooks 6 · MCP 1；JS 套件 1779 通过、0 失败、3 跳过；main CI 绿（41881101175、定时 41974016912）。第 1 轮预发评审结果与修复见下一条；修复提交 879767b3（pre-commit：cargo test 2418 通过、0 失败、7 忽略；fmt；两条 clippy 腿）。第 2 轮评审结果与修复见下一条，修复提交 40c0792c（pre-commit：cargo test 2419 通过、0 失败、7 忽略；fmt；两条 clippy 腿）。成熟方案对照：TypeScript BuilderProgram 按文件声明签名（.d.ts 哈希）变化沿引用 BFS 重查、签名不变即停；salsa/rustc red-green 记录每个查询读过的输入、输入变才重算并早停。本扇出是用手写规则近似后者；发版后登记待办：给 JS 导入边记录解析时读过的 (文件, 名字) 与节点名查找，按依赖重解析，取代规则式扇出。下一步：第 3 轮评审（只看第 2 轮修复提交，新 subagent，工作目录 /var/tmp/agent-tmp-review-0186c）；通过后合并到 main（快进）、推送、等 CI、打 tag。复审深度上限 2：第 2 轮若仍有 HIGH/MEDIUM，修复后对该修复再复审一次；第 3 次仍未通过则停下，带全部上下文上报用户。
- 2026-11-05 预发评审（第 1 轮，新 subagent，盲简报）：HIGH 1、MEDIUM 3、LOW 9，逐条对代码核实后修复：
  - HIGH-1（Phase 0 requeue 用旧目标名，by-name 兜底绑到旧名字的同名定义）：requeue 前把边还原成抽取时的样子（`js_exports::as_extracted`：用导入名、去掉 `imported`）。变异 M01（不还原）在终态上等价：被 requeue 意味着绑定文件里那个节点没了，这正是 MEDIUM-1 的"有无同名节点"漂移，扇出在同一次运行里会重抽这个导入方；保留还原是因为 requeue 交给延迟阶段的输入应当就是抽取结果，不靠后面一轮纠正。单测 `a_requeued_js_import_goes_back_as_it_was_extracted` 钉住函数本身（M11 被杀）。
  - MEDIUM-1（绑定文件增减了被绑定名字的节点，扇出看不到）：快照加运行文件的节点名（`cg_js_nodes_before`），本地导出的形状带"该本地名有没有节点"。还需要让落到 `<external>` 的相对具名导入保留 `js_module` 元数据（之前只保留 Rust 的 `ru`），否则扇出找不到这个导入方。变异显示"顶层函数"标志与"有节点"标志分开比较没有可观测差别（M12 存活：调用经 requeue 和 pending 重解析），所以只比较"有没有同名节点"。
  - 修完 HIGH-1/MEDIUM-1 后用评审的 `fuzz2.py`（种子 5–7）还剩 10/450 不一致，都是 barrel 自己的边：(A) 指向说明符文件、但名字与导入名不同的边是遍历绑定的（文件里 `export { r as f }`），不能当直接查找跳过；(B) 说明符文件本轮新增了导入名的节点、而另一文件同时删掉一个，同名计数没升，新同名定义那一轮不触发——恢复 276268a8 删掉的"直接目标文件新增同名节点"检查（当时以为新同名定义那一轮总能覆盖，计数抵消时不能）。
  - MEDIUM-3（CJS 按 ESM 建模）：CJS 写入按顺序生效：同名后写覆盖先写；`module.exports = …` 清掉之前的、并让之后的 `exports.y` 失效（除非同一链 `exports = module.exports = …` 重新绑定）；只给 `exports` 赋值不导出；spread 之前的属性交给 spread。两个 spread 同名仍不绑（Node 取后者），记入 Not covered。
  - MEDIUM-2（评审的 9 个存活变异）：H、I、W、AD、AG 用评审的用例写成 `the_reexport_rules_the_review_mutated_hold`，重跑全部被杀；AA（退避不翻倍）加耗时下限断言，被杀（4.3 ms < 15 ms）；E、L、AF 评审判为等价，未再处理。
  - 性能：修复后 160 文件稠密 DAG 加一个无关文件 102–106 ms（修复前 78–81，0.185.0 47–55）：200 个找不到名字的 `<external>` 导入现在带 `js_module`，每个名字走一遍整个 `export *` 图。加了按文件缓存解析过的 `export *` 来源（81–84 ms）。另外发现扇出每次都先读全部 JS 导入边（claude-mem-lite 7,189 条，查询 8.8 ms），而 0.185.0 在没有漂移时直接返回：加回早退（没有漂移、没有文件出现/消失、没有运行文件新增节点名时不查），只改注释的编辑回到 0.185.0 水平。试过两个没有可测收益的改动（Phase 0-pre 按 base 预过滤、扇出解析按 base 记忆、无转发文件不加载），已撤回。
  - LOW：1、3、4、8、9 改了 CHANGELOG/spec 措辞；5 的形状写进 Not covered；6（修复每次重试都加载模型；退避期间放开标志时进程内增量不会请求让出；CLI 写入方只靠重试等待共 15 s）：只在遇忙重试时发生，记入此处和 Not covered（最后一条），不改；7 见上。
- 2026-11-05 第 2 轮评审（只看 879767b3，新 subagent）：HIGH 1、MEDIUM 1、LOW 6，逐条核实：
  - HIGH-1（复现）：裸调用 `f()` 经一条被遍历以另一名字绑定的导入（`export { r as f }`）指向 `r`；绑定改名为导出名（`function r … export { r as f }` → `export function f`）或移进 barrel 时，Phase 0 按节点旧名 `r` 把调用 requeue 进 pending，导入边重解析后直接绑到说明符文件，扇出当成直接查找跳过，调用丢失或绑到别的文件的 `r`；0.185.0 无此问题。修复：Phase 0 requeue 一条带 `imported` 的导入边、且源文件不在本轮时，把源文件记入 `temp.cg_js_requeued`，扇出把它加入重抽（`requeued_walk_import` / `take_requeued_walk_importers`）。在 879767b3 上红，修复后绿（`a_bare_call_through_a_renamed_binding_follows_its_import_when_the_binding_moves`，5 个形状）。
  - MEDIUM-1（复现）：CHANGELOG 的 CJS 顺序说法对文件自己的值不成立（自己的值来自 exports 轴，不分顺序，且 `local_of` 先认它）。改 exports 轴超出本次范围：把说法限定为转发写入，自己的值写进 Not covered。
  - LOW-1（复现，0.185.0 同样）：按名字跨项目绑定的导入只在名字总数上升时重绑。记入 Not covered。
  - LOW-2（复现，回归）：`exports = {}; exports.f = …; module.exports = exports` 丢了脱钩期间的写入。CJS 改成两个对象（`module.exports` 持有的、`exports` 指向的），赋值和成员写入分别作用于对应对象；`module.exports = exports` / `exports = module.exports` 合并。
  - LOW-3：变异 (a)（requeue 不调用 `as_extracted`）不等价（直接导入边残留 `imported`）；修复 HIGH-1 后，pull 的条件就是 `as_extracted` 认出这个键，所以 (a) 现在也关掉 pull：在 handma 18 个里 13 个不一致，被 HIGH-1 测试杀。
  - LOW-4：变异 M8 存活：补行 `exports = { g: 1 }; exports.f = …` → 无条目。
  - LOW-5：Not covered 里"调用绑到 `a.js` 的 `f`"只对改名导入成立，改措辞。
  - LOW-6：同一模块两个顶层 `function f`（非法 JS，ESM 下是语法错误）不一致；合法变体（嵌套同名函数）在本版、0.185.0、0.184.0 都一致。不处理。
  - 本轮变异（独立树和 target）：N1、N3–N8 被杀；N2（本轮内的源文件也记录）只多重抽一次，等价。
- 2026-11-05 第 3 轮评审（只看 40c0792c，新 subagent）：HIGH 0、MEDIUM 2、LOW 4，逐条核实：
  - MEDIUM-1（复现，本分支 md5 839d70bf 3/4 不一致；0.185.0、0.184.0 4/4 一致）：`import { f } from './e.js'` + `f()`，`e.js` 里 `export { r as f }`，另一文件的 `export function f` 被删或改名。增量保留 `main → e.js:r`（Node 运行的正是它），重建没有调用边（pending 里留着 `main => f`）。因果链：重建时 Phase 2d-bind（`resolve.rs::bind_calls_to_imported_targets`）只把一条已按名字绑到同名节点的裸调用边改绑到导入目标；全项目没有名为 `f` 的节点就没有这条边可改，调用留在 pending；增量里那条边是诱饵 `f` 还在时改绑的，诱饵消失时没有任何东西重问 `app.js`。与 Not covered 里 `ff()` 那条是同一个重建缺口。配对链测（评审的 `chaingen.py`，种子 21，300 条 6 步链，边带元数据 + pending 行）：本分支 35 不一致，35 条全是这一类（增量有 Node 的边，重建 pending）；0.185.0 8、0.184.0 10，全是增量残留按名字绑到诱饵 `c.js:f` 的错边。
  - MEDIUM-2（复现）：CHANGELOG “those were lost, or bound another file's `r`” 说的是未发布代码的缺陷；整条第一句在 0.185.0 上也一致（4 个形状 + 评审 3 个，0.185.0/0.184.0 全 SAME）。改写为“本版新增的改名绑定在增量里保持重建的结果”。
  - LOW-1（读码 + 复现）：成员写入链只读最外层（r1 只记 `f → ./b.js`，r2/r3/r4 无条目，node 都运行 `a.js`）。写进 Not covered，第 37 行加“chained writes aside”。
  - LOW-2（复现）：`h()` 在 0.185.0 无调用边；`import { f as g }` 的导入边绑 barrel 私有 `f`，只有调用绑 `a.js`。改写该条。
  - LOW-3（复现）：N7（`INSERT OR IGNORE` → `INSERT`）存活。HIGH-1 测试加“一个文件两条改名导入、绑定同时移动”的形状：N7 下 `UNIQUE constraint failed: cg_js_requeued.path`，还原后绿。
  - LOW-4（复现）：C3、C4、C9 存活。解析器表加 3 行（node 核对：b.js、a.js、a.js），三个变异各被一行杀掉，还原后绿。LOW-3/LOW-4 提交 8d779bb1（pre-commit：cargo test 2419 通过、0 失败、7 忽略；fmt；clippy；指纹按“只改测试、不升版本”重记）。
  - 停止线：这是第 3 次复审仍有 MEDIUM，按事先定的深度上限停下，MEDIUM-1 的处理交用户决定（选项见汇报）。
- 2026-11-05 用户选择：修重建一侧，再对这一个提交做第 4 轮复审；第 4 轮若在新代码里再出 HIGH/MEDIUM，撤回这次修改，按“现在发版 + 写进 Not covered”发 0.186.0。
  - 实现（`resolve.rs::bind_calls_to_imported_targets`）：`cg_unique_imports` 加 `renamed` 列（组里有带 `imported` 的边）；新的一路从改名的唯一导入出发，连 `pending_unresolved_calls`（同一调用方文件、`target_name` = 导入名、元数据为空、同文件没有同名节点），插入调用边并删掉 pending 行。两种 scope 都按全图做。
  - scope 推理（交第 4 轮证伪）：一个不在 scope 里的调用方 C 要被这一路绑定，需要它的 pending 行和一条唯一的改名导入同时存在。C 的导入集合只经重抽（C 在本轮）或延迟阶段（C 是 deferred source，进 scope）改变；如果那条唯一改名导入在本轮之前就有，C 的裸调用上一轮已被 2d-bind 改绑、错边被剪，Phase 0 不会以这个名字把它 requeue。所以按全图做和按 scope 做结果相同，后面的剪枝、置信度只看 scope 也不会漏。曾写过把被绑调用方并入 scope 的代码，因为到不了而撤掉。
  - “同文件没有同名节点”一条与已有那一路的判定逐字相同（调用结果不应随别处有无诱饵而变）。实测同文件有同名方法时调用在 Phase 2 就绑到方法、不进 pending，所以这一条是等价分支。
  - 影响：9 个语料全量边集（带元数据）r10 与 r9 逐行相同。之前估计 hono 多 3 条，实际上那个文件对 `createElement` 还有一条来自 `'.'` 的导入，不唯一，不绑；估算查询只在改名导入之间判断唯一性，算错了。75 个 Node 探测程序 r10 = r9（合法 62 个里仍是 61 个与 Node 一致）。
  - 提交 ac69061f（pre-commit：check、test、fmt、clippy 通过；指纹重记不升版本，137 未发布）。r10（md5 0f4867c7，与 ac69061f 源码构建逐字节相同）对照 r9，带元数据：MEDIUM-1 4 个形状 CLI 和 refresh 都 4/4（r9 1/4）；评审 353 个手写用例 CLI 350、refresh 344 一致，余下 r9 同样不一致：s_4x 3 个（LOW-1 按名字重绑，0.185.0 同样），refresh 路径的 mb 6 个（一次 `affected` 点名 601–603 个文件，0.185.0 同样 6/8，另记）；kit38 + r4sets61 99/99；fuzz 400/400；fuzz2 1,050/1,050；链测种子 21、22 各 300 条：r10 0/600，r9 77/600，0.185.0 14/600。
  - 变异（独立 worktree + target，lib 1588 个测试）：MA7（去掉整路）被杀；MA3（不限元数据，成员调用 `x.f()` 的 `{"q":"member"}` 行被绑）、MA5（无自调用守卫，自导入生成 `r → r`）、MA6（不限调用方文件，未导入的 `o.js` 的 `f()` 被绑）存活，补断言后重跑都被杀；MA2（不删 pending 行）第一次写错不能编译，重写后被杀；MA4（同文件同名检查）等价，见上；MA1（不限改名导入）存活且不等价：JS 文件 `import { f } from './t.ts'` 后 `f()` 留在 pending（pending 扫描只在同语言内找），MA1 会把它绑上。这是本版之前就有的缺口，增量与重建一致；扩大到普通导入属另一个行为变更，要先在语料上量，记为待办，不写测试钉住现状。
  - 计时（6 次，`--no-embed`，v0.184.0 / 0.185.0 / r10 依次在同一台机器上，`timing-r10.sh`）：CHANGELOG 表整张换成这次的数。claude-mem-lite 加删文件 r10 119–131 / 74–82 ms（r8 那次 111–115 / 73–76）；交替 A/B 3 轮：r9 加 112–126、删 71–86，r10 加 114–130、删 72–91，范围重叠，新的一路没有可测开销。表后那句“增量边集等于全量”在 r10 上重验：claude-mem-lite 加 `export *` 31,097 条、hono 加 8,797 条、hono 删 8,793 条，逐行相同。
  - 下一步：提交测试和文档，派第 4 轮复审（新 subagent，盲简报，范围 e74e9c84..HEAD，工作目录 /var/tmp/agent-tmp-review-0186d）；通过就发版，在新代码里出 HIGH/MEDIUM 就撤回 ac69061f 及其测试，按选项 2 发版。
- D#738（afadb65e）：修复遇忙重试 5 次（间隔 1/2/4/8 s，期间放开标志）；增量请求 backfill 让出时不抢标志；panic 不跳过 backfill；`startup_follow_up` 可测。复现：另一连接持写事务 6.5 s，0.185.0 的修复后孤儿向量仍在（1），本版为 0。

## 验证记录

- 新测试在 0.185.0（main 88f50808 的 worktree）上：`a_js_reexport_resolves_as_node_does` 13 行 + s22 全红；`an_export_star_written_before_its_file_follows_the_file_when_it_appears` 第一个形状红；`a_fuzzed_barrel_graph_binds_the_same_incrementally` 红。`a_requeued_import_through_a_renamed_reexport_binds_as_a_rebuild_does` 在本分支加 `imported` 读取之前红（调用绑 `c.js.f`）。
- 改了期望的 4 个旧测试（新期望都按 Node / ECMAScript 核对过）：`test_js_renamed_import_binds_only_what_the_file_exports` 加 `esm.realLoad`、`impl.go`（node 实测 `esm.realLoad impl.go`）；`a_reexport_chain_is_followed_eight_files_deep_and_no_further` 9 层也绑上、300 层回退；`a_named_reexport_answered_elsewhere_is_not_retried_through_export_star` 的"界外"例子改成 300 层；`the_js_reexport_fanout_pulls_only_importers_a_rebuild_rebinds` 第三例只拉 `other.js`（F5）。
- 新二进制 md5 6bd9b675（工作树，含 `imported`）：fuzz 0/400 不一致（同一批图 0.185.0 源码构建 0be677c9：33/400）；kit38 38/38；r4sets61 61/61（0.185.0 57/61）；探测合法 62 个中 61 个调用边与 Node 一致，余 b18（`new g()` 构造类，调用只绑函数，0.185.0 同样无边）。
- 语料全量边集（0.185.0 源码构建 vs 新）：cgm-plugin、claude-mem-lite、claudemd、daagu、express、gsd-lite、llm_wiki、sgc 逐行相同；hono −14 +16，逐条对源码核对全部正确（`export { module as serveStatic }`、`export { Hono as HonoBase }`×5、`export { jsx as createElement }` 的导入与调用、`reactAPICompatVersion as version`×2、`Fragment as StrictMode`、`export { jsxDEV as jsx } from`、`createElement/cloneElement as …ForDom` 的 3 条调用）。
- 真实语料增量（`real_inc.py`，hono/gsd-lite/claude-mem-lite/daagu 各 15 轮，种子 7）：60 轮 0 不一致。
- 计时（6 次，`--no-embed`）：稠密 `export *` 全量 环形 60 文件 0.185.0 1,058–1,077 → 新 113–123 ms；DAG 1,086–1,121 → 226–238 ms。claude-mem-lite `utils.mjs` 增删 `export * from './hash-utils.mjs'`：0.185.0 1,140–1,687 ms、每次重抽 90 个 → 新 200–226 ms、重抽 0 个；加行后增量与全量重建 31,050 条边逐行相同。
- `cargo test --no-fail-fast`（加 `imported` 之前）：2406 通过、0 失败、7 忽略；clippy 两条 feature 腿无告警。之后每个提交的 pre-commit 都跑全量测试、fmt、两条 clippy 腿：通过。
- 最终二进制（276268a8 的源码，md5 ecabdb58）：fuzz 400/400、kit38 38/38、r4sets61 61/61、9 个语料同上（8 个逐行相同，hono −14 +16）；探测合法 62 个中 61 个与 Node 一致（b08 在加 `only_default` 后一致，余 b18）。
- 计时（6 次，三个版本都源码构建：v0.184.0 / 0.185.0 / 新）：环形 60 文件全量 75–96 / 1,030–1,050 / 93–97 ms；DAG 160 文件 153–174 / 1,072–1,116 / 222–239 ms；claude-mem-lite `utils.mjs` 加 `export *` 173–185 / 1,028–1,082（重抽 90）/ 175–194 ms（0）；hono `ssg/index.ts` 加 `export *` 68–82 / 89–126 / 93–105；删 `export * from './ssg'` 63–81 / 125–132 / 94–106；`hono-base.ts` 加 import 86–99 / 75–88 / 77–93。hono 两种 `export *` 编辑后增量与全量重建逐行相同（8,741、8,737 条）。

- 基线（0.185.0，`~/.cache/code-graph/bin/code-graph-mcp` md5 915cfad4）：`shapes.py sh/*.py` 合法 62 个，44 个一致；不一致 18 个：s13 s15 s17 s22 s23 s24 b01 b02 b03 b08 b18 b20 c01 c05 c06 d04 d05 d06。
