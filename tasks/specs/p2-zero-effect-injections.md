# P2-1：去掉零效果的注入（2026-11-03）

状态：spec 已写，**等用户同意后实现**（改的是发布插件注入给模型的文本，按 L3 处理）。来源：`docs/plugin-usage-audit-2026-11-03.md` P2-1、§三"噪声集中在三类注入"。

## 要改的两处

### 1. UserPromptSubmit 的 `[code-graph:hint]`

- 位置：`claude-plugin/scripts/user-prompt-context.js:535-541`（`query.type === 'symptom-hint'` 分支）。
- 证据（E 期，2026-10-23 到 11-03，中立项目）：触发 13 次，之后 15 次工具调用里用 code-graph 0 次；触发它的提示是发版指令、`/claudemd-doctor`、贴回来的上一轮报告，"失败""遗留"这类词命中了症状规则。以前单独的建议行两次测下来也没效果（`project_steering_file_ab_2026_10_06`、`project_claude_usage_eval_2026_09_28`）。
- 方案（推荐 A）：
  - A：删掉 symptom-hint 分支（不再输出这行），保留 cooldown 以外的其他分支不变。
  - B：只收窄触发词（去掉"失败""遗留"等），保留这行。证据只说明"现在的触发 0 跟进"，没说明收窄后会有跟进，所以不推荐。

### 2. grep 之后的 AST 视图

- 位置：`claude-plugin/scripts/post-grep-inject.js`（`buildInjectText`，头 `INJECT_HEADER`）。
- 证据：24 次里 15 次每条命中都只标注 `→ module <文件> (lines 1-N)`，没有比 grep 本身多出信息。
- 方案：答案里每条命中的上下文都是文件级 module 时，不输出这段注入（输出空，和"没有答案"走同一条路）；只要有一条命中落在函数/类里，照常输出全部。callgraph 模式不变。

## 默认行为变化的配套（发布要求）

- 版本：minor（注入文本变了）。
- CHANGELOG 顶部写明：哪两种注入不再出现、为什么（上面的数字）。
- 回退：`npm i -g @sdsrs/code-graph@<上一版>`（同 0.171.0 的步骤）；不加新的开关（这两处都是删减，没有用户依赖它们的输出）。
- 可发现性：发布说明里的 callout。

## 测试计划（先红后绿）

- `user-prompt-context.test.js`：症状型提示（含"失败"）不再输出 `[code-graph:hint]`；其他查询类型的输出逐字节不变。
- `post-grep-inject.test.js`：全部命中为文件级 module → 无输出；混合（一条在函数里）→ 输出和现在逐字节一致；callgraph 模式不受影响。
- `hardening.rs` 里镜像 JS 文本的守卫（如有）同步。
- JS 全量 + `make test-rs`。

## 没有覆盖

- 目录概览（"5+ Reads into X"）占中立项目注入字符的 56%，不在本项内；D#713 已改为按读者计数。
