# P2-1：去掉零效果的注入（2026-11-03）

状态：**已实现，待随 0.185.0 发布**（2026-11-03 用户同意按推荐方案执行；改的是发布插件注入给模型的文本，按 L3 处理）。来源：`docs/plugin-usage-audit-2026-11-03.md` P2-1、§三"噪声集中在三类注入"。

## 要改的两处

### 1. UserPromptSubmit 的 `[code-graph:hint]`

- 位置：`claude-plugin/scripts/user-prompt-context.js:535-541`（`query.type === 'symptom-hint'` 分支）。
- 证据（E 期，2026-10-23 到 11-03）：一共触发 13 次（9 次的提示是贴回来的报告，以"遗留"开头，命中的是正文里的"报错""不对""不准"；"遗留"本身不在规则里，审计报告那句说法不准），其中中立项目 9 次；这 9 次之后各取最多 15 次工具调用（共 130 次），用 code-graph 0 次（另外 4 次在插件自己的仓库里，之后有 9 次 code-graph 调用，是开发本身的调用）。审计报告写的"13 次触发、0 次跟进"混了两个口径，以这里为准；触发它的提示是发版指令、`/claudemd-doctor`、贴回来的上一轮报告，"失败""遗留"这类词命中了症状规则。以前单独的建议行两次测下来也没效果（`project_steering_file_ab_2026_10_06`、`project_claude_usage_eval_2026_09_28`）。
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

## 决定与实现（2026-11-03）

- 第 1 处按方案 A：删掉 symptom-hint 分支、`SYMPTOM_PATTERNS`、`hasSymptom` 和 `symptom` 冷却项；`determineQueryType` 去掉只给它用的 `message` 参数。
- 第 2 处的证据更正：重新从会话记录取 E 期中立项目的 grep 模式注入，共 21 次（另有 2 次 callgraph 模式，合计 23 次；审计写的是 24 次，差 1 次的原因没有追查）。没有 AST 信息的是 15 次，其中 10 次是 `grep -l` 的文件列表（完全没有标注，审计把它们也算成了"只有 module"），5 次全部是 `→ module`；6 次带函数或类。29 次 grep 注入（含插件自己的仓库）之前，模型自己的 grep 都有输出。
- 第 2 处规则因此写成：答案里没有任何一条命中落在 module 以下的节点（`hasAstContext`），**并且**模型自己的输出里至少有一行像 grep 命中（`grepShowedHits`，复用 `grepFoundPattern` 的行形判断：`路径:`、`行号:`、`-l` 的单独路径），才跳过，记一条 `{hook:'grep', action:'observe', reason:'no-ast-context'}`，和"grep 已命中"那条跳过同一种记法。读不到输出、或输出里没有命中形状的行（例如只有 echo 的输出，grep 没搜到），照旧注入：这时命中行本身就是新信息。spec 原文"输出空，和没有答案走同一条路"改为这个条件，原因是已有测试"grep 什么都没找到 → 注入"代表的情形仍然成立。
- 回放（用实现里的两个函数跑 E 期 29 次 grep 注入的完整文本）：中立项目 15 次无上下文里跳过 14 次（剩 1 次是 `xargs grep -h` 的输出，没有命中形状的行），6 次有上下文全部保留；插件仓库 3 次跳过、4 次保留。
- hook 自检的连带改动：`lifecycle.js` 的 UserPromptSubmit 探针过去靠 hint 证明"真的读到了 prompt"。改的时候发现 `hook-fire.test.js` 原来那条"必须有输出"的断言在没有安装清单的 HOME 沙箱里跑，hook 先打印重启提示就返回了，从没读到 stdin：这条守卫一直是空过的。新测试在有清单和桩二进制的 HOME 里跑：探针提示必须不产生查询（自检从不运行 CLI），同一 HOME 里带锚点的提示必须走到桩二进制并输出（对照）。用 HEAD 版本的 hook 跑同一探针，emitted 为 true，新断言会失败。
- 回退：`npm i -g @sdsrs/code-graph@0.184.0`，同 CHANGELOG 顶部。
