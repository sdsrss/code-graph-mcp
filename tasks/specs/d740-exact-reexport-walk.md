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
- [ ] AC4：11 个语料全量边集与 0.185.0 的差异逐条归因（只应出现在旧遍历读错的形状上）
- [ ] AC5：计时：稠密 `export *` 60 文件全量 ≤ 0.184.0 的 2 倍；claude-mem-lite `utils.mjs` 加一行 `export *` 的增量不再重抽 90 个文件
- [ ] AC6：`cargo test` 全部通过、clippy 两条 feature 腿、fmt
- [ ] AC7：预发评审（新 subagent，盲简报）

## 方案

见上面"做"。决定：
- 表而不是边元数据：遍历的输入必须在延迟阶段开始前就齐全、且与其他文件的绑定结果无关；`imports` 边做不到（目标不存在时没有边，绑定在延迟阶段才写）。`cpp_fields` 是同一形状的先例。
- 本地导出判断在遍历内部是严格的（只认导出的名字：`exports` 边的发布名 ∪ 表里的本地条目）；导入方直接目标文件的查找仍是宽松的（文件里任何同名节点），在遍历之外，行为不变。
- 遍历结果是 (文件, 本地绑定名)，消费者再映射到节点：导入边取该文件里同名的任何节点；调用取该文件同名的顶层函数；命名空间取该文件的 `<module>`。
- 导入名与绑定到的节点名不同（改名重导出）时，导入边元数据加 `"name"`，requeue 和扇出据此取导入名。

## 计划

- [ ] S1：解析器 `js_export_entries` + 单测（形状表，先红）
- [ ] S2：表、写入、INDEX_VERSION、指纹
- [ ] S3：遍历重写 + 延迟阶段/调用/pending 三个消费者；探测程序移植成管线测试（先红）
- [ ] S4：Phase 0-pre（D#739）+ 扇出重写；增量=重建测试（先红）
- [ ] S5：fuzz/探测/kit38/r4sets61/语料/计时
- [ ] S6：D#738
- [ ] S7：CHANGELOG、发版

## 决策与进展

- 2026-11-05：用户授权新增表；D#735 下个会话；D#736 不跑。

## 验证记录

- 基线（0.185.0，`~/.cache/code-graph/bin/code-graph-mcp` md5 915cfad4）：`shapes.py sh/*.py` 合法 62 个，44 个一致；不一致 18 个：s13 s15 s17 s22 s23 s24 b01 b02 b03 b08 b18 b20 c01 c05 c06 d04 d05 d06。
