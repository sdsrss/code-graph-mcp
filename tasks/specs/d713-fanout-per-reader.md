# D#713：目录概览的读取计数按会话和子代理分开（2026-11-03）

状态：已实现并验证，未发版。来源：`docs/plugin-usage-audit-2026-11-03.md` F1 / P1-1，待办 D#713。授权：用户 2026-11-03"按你建议继续完成剩下的"（报告 P1-1 点名此项）。

## 背景

`pre-read-guide.js` 在同一目录读到第 5 个不同的源文件时，注入该目录的概览（`overview`）。计数存在 `cgTmpDir()/.code-graph-readfan-<cwdHash>.json`，只按项目区分：同一项目的所有会话、子代理、worktree 共用一份计数。原注释把这当作有意的取舍（"会话之间最多多发约 1 次"）。

审计的说法是"27 次概览里只有 7 次发给了自己读满 5 个文件的会话"，建议按 `session_id` 分开。

## 实测（`tasks/d713-fanout-per-session/replay_fanout.py`，E 期，中立项目，会话记录回放）

- 回放 1,348 次源文件读取（Read 工具 + `sed -n a,bp`），29 个（项目, session_id）组合，99 个记录文件，其中 70 个是子代理。
- 旧规则（按项目）回放出 19 次概览（实际注入 27 次，回放没有重现其中 8 次，原因未查）。19 次全部落在 session_id 自己读满 5 个文件的会话里。
- 只按 `session_id` 分开：仍是 19 次，没有任何变化。原因：子代理的记录带父会话的 `sessionId`；`pre-edit-guide` 按 `input.session_id` 写下的 22 个编辑日志文件名，全部对应主会话 ID，没有一个对应子代理 ID。
- 按记录文件（主会话、每个子代理分开）：旧规则的 19 次里只有 12 次落在自己读满 5 个文件的上下文里；分开后共 14 次，每次都发给自己读满的上下文。
- 官方文档（code.claude.com/docs/en/hooks，2026-11-03 查阅）："When a subagent calls a tool, tool events such as `PreToolUse` and `PostToolUse` fire the same configured hooks as in the main conversation, and the input carries the `agent_id` and `agent_type` common input fields that identify the subagent."

所以审计说的"发错会话"实际是父会话和子代理的读取加在一起，按 `session_id` 分开解决不了，要用 `agent_id`。

## 规则

- 读取方 `readerOf(input)`：有 `agent_id` 时为 `session_id/agent_id`，否则为 `session_id`；都没有时为 null。
- 状态文件 `.code-graph-readfan-<cwdHash>-<key>.json`，`key` 由 `session-edits.js` 的 `sessionKey` 生成（UUID 原样，其他内容哈希，防止路径注入）；读取方为 null 时用原来的 `.code-graph-readfan-<cwdHash>.json`。
- Read 工具（`runMain`）和 sed 范围读取（`pre-grep-guide.js`）两条路径都传 `readerOf(input)`。
- 清理：文件都在 `cgTmpDir()` 里，SessionStart 的 `pruneCgTmp` 按 24 小时 mtime 删除，卸载时整个目录删除，不需要新的清理路径。

## 不做

- 概览文本、阈值（5）、TTL（30 分钟）不变。
- 回放与实际 27 次之间差的 8 次没有逐条追查。

## 验证

- 单元：`readerOf`、不同会话不累加、`statePath` 对路径注入取哈希。
- 入口：Read Hook 和 sed 路径分别用带 `session_id` / `agent_id` 的输入启动，另一会话、同会话的子代理的第一次读取不触发，原读取方的第 5 次触发。
- 变异：`readerOf` 忽略 `agent_id` → 3 个测试失败。
- JS 全量（CI 同款命令）。
- 审计 README 的复现步骤（gsd-lite 副本 `src/tools/orchestrator/`）。

## 决定记录

- 改为按会话 + 子代理，而不是报告写的只按会话：回放显示只按会话的效果为 0。
- 参数名从 `sessionId` 改为 `reader`，避免把"会话"误当成唯一的区分维度。

## 验证记录

- 修复前：3 个 D#713 测试都在"sess-B 第一次读"处失败。
- 修复后：D#713 的 4 个测试通过；变异（忽略 `agent_id`）→ 3 个失败，已还原。
- JS 全量（只按会话分开的版本）：1,794 个测试，1,791 通过，0 失败，3 跳过。
- README 复现（gsd-lite 副本）：读 1–4 由 sess-A，读 5 由 sess-B → 无输出；读 6 由 sess-A → 概览（1,321 字节）。审计时旧代码是 sess-B 收到、sess-A 没收到。
- 回放：旧规则 19 次（12 次落在自己读满的上下文）→ 新规则 14 次（全部落在自己读满的上下文）。
- 回放与实际对照（2026-11-03，D#714 会话）：报告里"实际 27 次"按去重后是 20 次（排除 code-graph-mcp、claude-mem-lite 两个项目；每次注入在会话记录里有两条附件记录）。回放的 19 次里只有 3 次和实际的同一会话记录、同一目录对得上。原因：(1) 8 次实际发生在 `daagu/.claude/worktrees/agent-*`，这些工作树已删除，回放把它们的读归到 `daagu`，还多算出 5 次 `.claude/worktrees/...` 目录；(2) 回放用现在磁盘上的索引判断项目根，`daagu/backend` 下的读变成 `app/...` 而不是 `backend/app/...`；(3) 回放只认 `Read` 和 `sed -n N,Mp 文件`，并按记录里的 cwd 解析相对路径，`cd 别处; sed ...`、`cat 文件 | sed ...` 都漏掉（claudemd、sgc 的几次）；(4) 钩子在索引构建期间不提示，回放没有模拟。结论：19 → 14 是模型上的数，不是钩子实际行为的计数；修复本身（按 session_id + agent_id 分开计数）由单元测试和 README 复现证明，不依赖这组数。CHANGELOG 已加说明。
