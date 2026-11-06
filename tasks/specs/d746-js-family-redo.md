# 0.189.0：D#746 JS 语言族导入调用（D#742 重做）、D#747 成员调用 pending 行

状态：实现与测量完成，0.189.0 预发评审中（2026-11-06）。

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
