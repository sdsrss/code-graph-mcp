# 0.189.0：D#746 JS 语言族导入调用（D#742 重做）、D#747 成员调用 pending 行

状态：0.189.0 只发 D#747；D#746 在第 3 轮评审后按用户事先的决定撤回，回到待办（2026-11-06）。

## 背景

0.188.0 撤回了 D#742（见 `tasks/specs/d742-js-family-import-calls.md`），重做记为 D#746；0.188.0 遗留 D#747（删文件后增量比重建少 pending 行）、D#741（链测增量/重建不一致）。

## 决定

2026-11-06 用户："继续完成剩余的工作，剩余的工作你先深入思考，给出科学准确的建议，然后按你的建议执行。做完了后发版。" 视为对下列建议的授权（含 L3 实现与发版）。

| 项 | 建议 | 理由 |
|----|------|------|
| D#746 | 做 | hono 298、sgc 2 处真实调用缺边；第 2 轮评审的 MEDIUM 有明确修法 |
| D#747 | 做（根因已定位） | 两文件即可复现；改动是 sweep 两个分支各加一处"空则保留" |
| D#741 | 不做，留待办 | 链测不一致全是 D#741 形状，13 个真实语料 0 处 |
| /tmp/claude-1000（约 830 MB） | 保留 | 可用内存 33 GiB，占 2.4%，tmpfs 可换出；记忆和脚本按路径引用 |
| /var/tmp/d516（32 MB） | 保留，更新 `.keep` | D#519 是按用户决定关闭的已知问题，重开要靠新复现，这些损坏库是对照证据；磁盘剩 285 GB |

## D#746 设计

- 重新应用 71ed014a + cbec9cc6 的 D#742 代码与测试（`git diff 2c277896 cbec9cc6 -- src/ tests/`）：同语言候选为空的裸调用，只绑调用方文件对该名字唯一的导入目标（兄弟语言、每条导入都经相对路径遍历到已索引文件）；延迟阶段与 pending 扫描共用 `may_widen_to_family` / `family_fallback` / `FileImports`。
- 新增（第 2 轮 MEDIUM 的修法）：`restore_inbound_edges` 不再按名恢复"JS 族调用方、被调文件是另一种 JS 族语言、元数据为空"的调用边，改为放回 pending，由同一次运行的扫描和后处理（2d-bind / 2d-prune）按重建的规则决定。
  - 最初版本只对"调用方语言没有同名节点"的调用这样做。加了"同语言同名节点 + 被调文件新增类方法"一条链后，那一版增量绑两条、重建零条（0.188.0 就有，2d-bind 把同语言候选改指到导入目标的情形）。去掉这一条件后全部一致，代码也更短，所以采用无条件版本。
- 混合扩展名链测（fam1 fc1_109 第 4 步）测出无条件版本的回归：调用方语言也有同名定义、调用方对该名字有成员调用（剪枝保留同语言边）时，扫描按名重绑的是调用方已有的边，插入为空操作，调用方不进后处理范围，2d-bind 不再把导入目标的边补回来（增量少 `K -> t.js:f`，0.188.0 在这一步与重建一致）。修法（21e167c5）：放回 pending 的调用方文件无条件加入后处理范围。测试链"the callee file is touched beside an own-language twin the prune keeps"修前失败。
- 第 2 轮 LOW（扫描对每个只有兄弟语言定义的 pending 行读一次导入）：真实语料里只有 hono 有这类行（46 行、27 个文件），claude-mem-lite、daagu、sgc、claudemd、gsd-lite 都是 0；合成树每次编辑多 125–156 ms，去掉 `gfn()` 调用后两边相同（355–373 ms）。按 runbook（LOW 的修复风险最大）写进 Not covered，不修。

## D#747 根因

- 成员调用（`x.then()`，Python 同形，或 typed receiver 的项目类没有该方法而回退到成员池）的同名候选全是自由函数时，延迟阶段过滤后池为空，缓冲成 pending 行；同一次运行的 pending 扫描用同样的过滤得到空集，却把行当已解决删掉。
- 函数删掉后，重建时没有同名节点，行留下；增量没有任何东西把行带回来。hono 删 `src/jsx/streaming.ts`（含 `then` 箭头函数）后少的 16 行就是这样。
- 修法：扫描的 Member 分支和 RecvType 回退分支，过滤后为空时保留行（`continue`），与延迟阶段一致。
- 全量输出变化：多出这类 pending 行；INDEX_VERSION 139 一并覆盖。

## 验收

- [x] AC1：D#746 链测覆盖 MEDIUM 的 5 种形状 + js 被调 + barrel + 两调用方 + 同语言孪生；按名恢复时 7 条链失败（实测）。
- [x] AC2：第 2 轮存活的非等价变异 M2、M5、M13、M15 被新测试杀死；M14（计入 `<external>`）在合法代码里等价（包导入按名绑到项目同名节点，不走 `<external>`），用一条反例测试记录。
- [x] AC3：D#747 测试 4 种形状修前 4/4 失败；两个分支分别改回"删行"都失败。
- [x] AC4：13 个语料差异逐条归因：边只在 hono +298、sgc +2（D#746）；pending 行 hono −298 +111、sgc −2 +9，其余只增（D#747），llm_wiki、tokio 不变。
- [x] AC5：链测不比 base 多不一致（见验证记录）。
- [x] AC6：耗时（CHANGELOG 0.189.0 的 Cost 表）。
- [x] AC7：CHANGELOG 0.189.0。
- [ ] AC8：预发评审（新 subagent、盲简报）。

## 验证记录

- BASE：`target/release`（md5 fc923307，0.188.0 撤回后的源码构建，与发布二进制 13 个语料逐字节相同）。
- h1 = e9b078c9（D#746），h2 = 55fb20a6（D#747），均 `git archive` + 独立 `CARGO_TARGET_DIR`，`--features embed-model`。
- 工作目录 `/var/tmp/agent-tmp-d746`（语料、二进制、链测集），任务结束删除。
- h3 = 21e167c5（加后处理范围修复，md5 9517e78d）。h2 与 h3 全量索引 13 个语料逐字节相同。
- 语料差异（base 对 h3）：hono 边 +298、pending +111 −298；sgc 边 +2、pending +9 −2；claude-mem-lite pending +903（基数 12,397）、daagu +646（24,584）、本仓库 +422、stdlib +145、gsd-lite +31、claudemd +29、flask +29、nltk +3、express +1；llm_wiki、tokio 不变。新边全是 inferred。
- 链测（h3 对 base）：s21 35/300、s22 42/300，链与步相同；fam1 整链 5/120（base 11）、fam2 15/200（base 27），h3 独有 0；逐步链测（每步从前一状态的全新索引开始）fam1 5/720（base 12）、fam2 16/1200（base 29），h3 独有 0，每条内容与 base 相同或只少一条成员调用 pending 行；kit38 38/38、r4sets61 61/61。
- 真实语料增量（hono）：real_inc 15 轮 0 不一致（两边）；famchain（h2）24 步 1 步不一致，删 `src/jsx/dom/index.ts`，改名再导出（`createElement as jsx`），base 同一步也不一致；delone `src/jsx/streaming.ts` h3 0/0（base 0/16）。
- 0.188.0 读 h3 建的 v139 索引：查询可用，`incremental-index` 拒写并提示删 `.code-graph/index.db*`；h3 遇到 v138 索引打印 "Index version mismatch (stored v138 ≠ binary v139): clearing + rebuilding"。
- 耗时：`timing-base-h3-2.txt`（CHANGELOG 表）、`timing-bigjs-nog.txt`。原始输出与脚本在 `tasks/d746-kit/`（`results.tar.gz`）。

## 预发评审

- 第 1 轮（新 subagent，盲简报，范围 bd6c12da..d88eaf97）：HIGH 1、MEDIUM 3、LOW 2、NIT 1。逐条核实：
  - H1 HIGH（复现）：e9b078c9 把所有跨语言族调用按"被调节点名"放回 pending。改名导出（`export { r as f }`）经 2d-bind 从同语言孪生改指到 `t.ts:r` 的调用，回来时成了 `A => r`，没有导入叫 `r`，只碰一下 `t.ts` 增量就丢边（0.188.0 一致）。修复（97fdd100）：只在被调文件对该名字有 ≥2 个节点时放回 pending，并用调用方文件导入该节点时的导入名（来自保存的 imports 边；只有一个名字时）；只有 1 个节点时按名恢复，同 0.188.0。21e167c5 的后处理范围扩展因此不再起作用（放回的调用导入目标必不唯一，2d-bind 无可绑、剪枝结论不变；去掉后变异存活、链测仍过），删除。
  - M3 MEDIUM（复现，成本）：扇入树（4,000 个 .tsx 对一个 .ts 的 40,000 次调用）编辑 545 ms → 6.9–9.0 s；同一根因（全部走扫描、逐条自动提交），随 H1 修复回到按名恢复。
  - M1 MEDIUM（复现）：FileImports 按导出名读导入，`import { f as g }` 旁的参数 `f()` 被绑；hono 298 条新边中 9 条（6 条 `useContext as useContextCommon` 旁的参数、3 条 `import type { keyframes as … }` 旁的局部变量）。同语言导入在 0.188.0 同样（2d-bind 同一读法）。修需要解析器记录本地绑定，写进 Not covered，不修。
  - M2 MEDIUM（复现）：`walked` 只证明说明符指向已索引文件，不证明遍历绑定了目标；`export * from 'lodash'` 死路时导入按名回落到项目节点，调用随之被绑。hono、sgc 新边 0 处。写进 Not covered，注释改正。
  - L1 LOW（复现）：D#747 的行和所有 pending 行一样 50 次解析运行后淘汰；CHANGELOG 写明。
  - L2 LOW（复现）：存活变异 R2/R3/FF1/F7 非等价。F7（FileImports 缓存跨文件共享）补"三个文件导入三个名字"用例杀死；R2/R3 在新规则下（≥2 节点 + 导入名）分别对应 P4/P5，P4 只在一个 0.188.0 也不一致的形状上有差别，P5 在测过的形状上等价。
  - N1：4 处注释改正。
  - 新发现（自测，0.188.0 同样）：同一文件既 `import { f }` 又 `import * as T` 同一模块，被调文件新增方法 `f` 时 `T.f()` 只恢复到函数，重建绑两个。记入 Not covered 和待办。
- 修复后的测量见下（rel2 = 97fdd100）。
- rel2（97fdd100，md5 824bfb47）：13 个语料全量索引与 h3 逐字节相同；整链 s21 35、s22 42、fam1 5、fam2 15，kit38 38/38、r4sets61 61/61，rel2 独有 0；逐步链测 fam1 5/720、fam2 16/1200（rel2 独有 0）；评审的 6,600 步（gen2 种子 31/32/33 各 1,800、famgen 种子 3 1,200）base 93/100/97/37、rel2 60/73/65/18，rel2 独有 0。内容比 base 多的 10 步都在 base 已不一致的步上：9 步是 D#747 在重建侧保留的成员调用行（增量侧是一条按名恢复到自由函数上的成员调用边，base 同样），1 步（g32_56_k5）是被剪枝后不留行的调用在同语言候选消失后重建绑定、增量无从重评（base 同一步同一原因，表现为 pending 行）。
- 耗时（rel2，`timing-base-rel2.txt`、`timing3-base-rel2-b.txt`、`timing-bigjs-nog-rel2.txt`）：CHANGELOG 0.189.0 Cost 表。hono 编辑 `src/jsx/hooks/index.ts` 72–85 → 77–79 ms（d88eaf97 时 107–116）；扇入树编辑 530–554 → 1,002–1,045 ms（d88eaf97 时评审测 6.9–9.0 s）。
- 第 2 轮（新 subagent，只看 d88eaf97..9ce3037a）：HIGH 1、MEDIUM 0、LOW 1、NIT 2。
  - H1 HIGH（复现）：同一节点以两个名字导入（`import { f, g }` + `export { f as g }`），被调文件新增方法 `f` 后，97fdd100 因导入名不唯一回落到按名恢复，绑两个节点，重建零个；base 与 d88eaf97 都一致。修复（6f5cfc4a）：不再放回 pending（pending 行需要调用时的名字，重命名导出、两个导入名都给不出），被调文件有 ≥2 个同名节点时登记调用方文件（`requeued_walk_import`），在同一次运行的 fan-out 轮重抽，等于重建；位于本次运行更晚批次的调用方由本批次重抽，更早批次的照样登记。删除 `imported_as`。变异：按名恢复 12 条链失败、不登记重抽 10 条；放宽条件（1 个节点、任何元数据、同语言）存活，只是重抽更多文件，结果精确、只多耗时。
  - L1：存活变异 X3/X6/X7/X8 对应的条件随重抽方案改写；X6（不看元数据）在新方案下等价于"多重抽"。
  - N1：imports 树每次编辑多 112–137 ms（kit 原始数），评审两次测得 119–153、128–141；CHANGELOG 改用三组合并后的范围。N2：合成树文件数为 4,401/4,001/4,001，描述改准确。
  - 深度上限两轮已到。6f5cfc4a 没有经过评审，是否再做一轮只看增量的评审后发版、或撤回 D#746 只发 D#747，交用户决定（见汇报）。
- rel3（6f5cfc4a，md5 b0d28b2d）：13 个语料全量索引与 rel2 逐字节相同。逐步链测 13,320 步（第 2 轮评审的 famgen2 种子 101/102/103 共 4,800，第 1 轮的 6,600，我的 fam1/fam2 1,920）：base 61/81/77、93/100/97/37、12/29，rel3 33/48/43、60/73/65/18、5/16，rel3 独有 0。整链 g102 45/300、g103 40/300（base 69、64）：8 条与 base 不同的行都是 base 先在更早一步出问题，rel3 晚出问题的那一步在逐步测试里 base 也不一致。s21 35、s22 42、fam1 5、fam2 15，kit38 38/38、r4sets61 61/61，rel3 独有 0。
- 耗时（rel3，`timing-base-rel3.txt`、`timing3-base-rel3.txt`、`timing4-base-rel3.txt`、`timing-bigjs-nog-rel3.txt`）：CHANGELOG Cost 表。新场景：扇入树的 `.ts` 文件给 50 个名字各加一个同名方法，4,000 个调用方文件重抽，975–983 → 3,552–3,571 ms；之后两边增量与重建都是 0 条调用边、40,000 行 pending。
- 2026-11-06 用户选择：对 6f5cfc4a 再做一轮只看增量的评审（第 3 轮，超出两轮上限，经用户同意），通过就发版；再发现问题则撤回 D#746、只发 D#747，不再修补。
- 第 3 轮（新 subagent，只看 97fdd100..92833f0d，经用户同意超出两轮上限）：HIGH 0、MEDIUM 2、LOW 4、NIT 3。逐条核实（R1min、R2min_lose_second、S_amb_lose 三个复现在 base/rel2/rel3 上自跑）：
  - F1 MEDIUM（复现）：6f5cfc4a 的重抽把调用方放进后处理范围，范围内 2d-bind 对"指向该文件的边"按目标节点名读调用（`export { Z as A, A as Z }` 换名桶），编辑另一个文件后增量多出 `c2.ts:C2 -> a.js:Z`；base 与 rel2 一致。误读本身 0.188.0 就有（只碰 `a.js` 三个版本都出），D#746 给了新的触发路径。
  - F2 MEDIUM（复现）：D#742 的扫描新绑定把调用方放进后处理范围，触发同一误读（`t.tsx` 失去第二个 `f` 后多出 `c2.tsx:C2 -> a.ts:A`）；rel2 与 rel3 都有，base 一致。
  - F3 LOW：fan-out 轮让 pending 行一次运行老化两次（6f5cfc4a）。F4 LOW（复现，D#747 自身）：被放回 pending 的 typed receiver 调用带着旧的 `"amb":1` 标记，重建的行没有；hono 删掉 hooks 文件里带 `use` 等方法的类后 29 行只差这个标记，反方向（加类）0.188.0 少 35 条边、D#747 一条不少。F5：`{ f }` + `* as T` 那条 Not covered 对跨语言调用方已过时。F6：放宽条件的变异会改输出（spec 里"只多耗时"不成立）。N1–N3：注释、日志、刷新路径的成本。
  - 按用户事先定的规则：第 3 轮再发现问题就撤回 D#746、只发 D#747、不再修补。撤回（1c6514b9）：`index_files.rs`、`domain.rs` 回到 bd6c12da（0.188.0），`resolve.rs`、`tests.rs` 回到 bd6c12da 加 55fb20a6（D#747）；INDEX_VERSION 139 只为 D#747。F4 写进 Not covered。
  - D#746 重做前要先修的根因：作用域 2d-bind 按目标节点名（而不是调用名）读边，改名导出时会误绑（F1/F2 的共同根因，0.188.0 就有）。
- 撤回后（w = 1c6514b9，md5 879d4fb2）：13 个语料边与 0.188.0 逐字节相同，pending 行只增（与 D#747 的数相同：claude-mem-lite +903 … express +1，llm_wiki、tokio 不变）。逐步链测 13,320 步 w 独有 0（base 不一致 587 步，w 527 步）；整链 s21 35、s22 42、fam1 10、fam2 25，w 独有 0；kit38 38/38、r4sets61 61/61。w 比 base 多出的内容只在 base 已不一致的 12 步上（重建侧显示 D#747 保留的成员调用行，增量侧是按名恢复到自由函数的旧问题）。hono 删 `streaming.ts`：base 0/16，w 0/0。hono hooks 文件加/删带 `use` 等方法的类：base 加时少 35 条边、删时一致；w 加时一致、删时 29 行只差 `"amb":1`（第 3 轮 F4，写进 Not covered）。耗时：`timing-base-w.txt`、`timing3-base-w.txt`（CHANGELOG 表）。升级/降级提示实测与 CHANGELOG 相同。
