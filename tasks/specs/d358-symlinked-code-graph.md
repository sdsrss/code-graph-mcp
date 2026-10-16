# D#358 — every CLI reader refuses a symlinked `.code-graph`

## 背景与目标

v0.127.0 的 CHANGELOG 写明：符号链接的 `.code-graph` 会被拒绝（exit 1）。
但真正做了这个检查的只有创建或替换 index 的路径：MCP server、`rebuild-index`、
`reindex`、首次全量索引。CLI 的查询命令和"已有 index"时的 `incremental-index`
会穿过链接，去读、去改项目外的 index。
目标是让所有读 `.code-graph` 的 CLI 入口都拒绝，同时让 JSON 消费方
（doctor、状态栏、session-init）对这种拒绝给出准确的说法。

## 范围

- Rust：`CliContext::open_inner`/`try_open`、grep 的提示、`cmd_incremental_index_opts`、
  `cmd_stats`、`health-check` 的 JSON 短路。
- JS：`doctor.js` 给 `reason: "refused"` 增加分类；`statusline.js` 在读进度文件、
  调 health-check 之前，先检查 `.code-graph` 本身。
- 不做：真实目录里 `index.db`、`-wal`、`-shm` 本身是符号链接的情况（D#363）；
  JS 钩子透过链接读 `source-roots.json`、`recommendations.jsonl`（评审 F7，留作待办）。

## 验收标准

1. 形态 (a) 链接到有 index 的目录、(b) 链接到空目录：所有查询命令、`health-check`
   两种格式、`stats`、`incremental-index` 都 exit 1 并给出拒绝消息；
   链接外目录的字节和文件列表不变。
2. `health-check --format json` 在拒绝时 stdout 输出一个 JSON 对象：
   `healthy:false`，`reason:"refused"`，`issue` 是完整消息。
3. doctor 不把拒绝诊断成 `binary-broken`，不执行任何修复，显示完整消息。
4. 状态栏不显示 `offline`，也不显示链接目标里的 index 或进度。

## 方案

见各处代码注释。拒绝消息统一由 `utils::owned::reject_symlinked_dir` 生成。

## 决策与进展

- 拒绝而不是警告：0.127.0 已经把"拒绝"写进文档，MCP server 从那时起也一直拒绝。
- doctor 那一行标 `advisory`，不给 `fixId`：要不要删链接由用户决定
  （链接可能是用户有意建的），doctor 不替用户做。
  这一行仍然是 `warn`，并带上完整消息。
- 第 1 轮评审（新开的子代理，审 d8b4e367）：F1 High（doctor 判成 binary-broken）、
  F2 Medium（状态栏显示 offline）、F3 Medium（`--format json` 拒绝时 stdout 为 0 字节），
  在 d8b4e367 之后修复。
  F4（普通文件或 FIFO 的行为变化）、F5（CHANGELOG 里的数字）只改 CHANGELOG 措辞。
  F6（守卫相对锁探测的位置没被测试钉住，最坏后果是多一条误导性的 warn）
  按 Low 处理，不修，原因记在汇报里。
  F7 留作待办。

## 后续任务（用户 2026-10-16 提出：本任务发版之后再做）

像真实用户一样完整使用这个项目，做端到端测试，一轮一轮地进行：
理解目标 → 设计场景并实际运行 → 按严重程度记录问题 → 直接改代码 →
重跑测试、构建、lint，再手动模拟验证 → 修复引入的新问题 → 写本轮总结。
重复，直到没有明显问题为止。
要求：不做无意义的重构；不改变项目目标和核心设计；每处修改都要能说明原因；
优先修真实使用中影响用户的问题；不确定时按上下文做出判断后继续推进；
测试产生的文件要清理掉。

## 验证记录

- d8b4e367：pre-commit 跑了 31 个套件，2237 个通过、0 个失败、7 个忽略；
  变异 M1–M6 全部变红。
