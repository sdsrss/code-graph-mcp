# 提示词阶段不再推送调用方列表；impact 到达深度上限时明说（2026-11-24）

状态：完成（2026-11-24，0.205.0 已发布）。用户 2026-11-24 在带偏测试结果出来后选择**只发选项 3**。选项 2 不发，提交 ad46735b 保留在分支 `ups-inject-off-opt2`。

## 背景与目标

2026-11-24 的试验（`tasks/specs/maintenance-mode-2026-11-24.md` 的结果和补充两节）发现，用户自然地问一句"我要改一下 `_now` 的行为，哪些地方会受影响？"，插件的 UserPromptSubmit hook 就会在模型开始工作前推送一份调用方列表，而这份列表可能不全或有错：

- **T1 形态**：`impact` 默认只查 3 层，输出 "4 callers total"，没有任何提示；真值是 9 个。
- **T2 形态**：另一个文件里有一个同名的模块级变量，被绑定成了目标函数的调用方。注入的 7 个里有 2 个错，还漏了 1 个。

F 期中立主会话里，这个 hook 的 impact / search 注入 5 次，跟进 0 次。

用户的授权原话："按你的建议执行，授权，先测一下会不会带偏"。授权的是对话里的选项 2 和选项 3：

- 选项 2：停掉提示词阶段的 impact 和 search 自动注入；
- 选项 3：`impact` 查到深度上限还没查完时明说，不再写 "N callers total"。

带偏测试另外登记在 maintenance-mode spec 的 P0-2b。

## 范围

- **做**：
  1. `claude-plugin/scripts/user-prompt-context.js` 不再运行 impact、callgraph、search。
     - **范围扩展：callgraph**。只关 impact 不够：T1n、T2n 这两句提示同时带"修改"和"调用"两种意图，关掉 impact 后查询顺序会落到 callgraph，注入只查 2 层的调用方列表（离线调用 `determineQueryType`、把 impact 设为冷却中，两句都返回 `callgraph`）。所以 callgraph 是选项 2 的目标"不在提示词阶段推送调用方列表"的必要部分，汇报里单列。
     - 歧义提示（`[code-graph:definitions]`）只由 impact 和 callgraph 的拒绝产生，随之去掉。
     - 保留：提示里点名了一个文件时的模块结构概览（overview）、中途安装的重启提示。overview 不含调用方列表，也不在授权范围内。
  2. `impact` 的调用方遍历到达请求的深度、而最深一层的函数还有调用方时（按同一个置信度下限），`RouteCallers` 标记截断。所有复用 `truncation_note()` / `callers_truncated` 的出口都带上说明并给出正确的调整方式：CLI `impact` 的文本和 JSON、`show --impact`、MCP `get_ast_node include_impact`、编辑前 hook。
- **不做**：
  - T2 的同名变量误绑定：这是解析问题，修它要升 INDEX_VERSION；按维护模式的规则不修。
  - `callgraph`、`trace`、MCP `get_call_graph` 的输出：它们不声称"总数"，展示的就是请求的深度。
  - 编辑前 hook 本身是否保留。
  - Read 扇出触发的目录概览：P1 里的另一部分，不在这次授权范围内。

## 验收标准

- [x]（只在 ups-inject-off-opt2 上，不发）AC1：impact、callgraph、search 三种意图的提示，hook 都没有输出（测试先红后绿）——验证：`node --test claude-plugin/scripts/user-prompt-context.test.js`
- [x]（只在 ups-inject-off-opt2 上，不发）AC2：点名一个已索引文件的提示仍然输出 overview；没有 manifest 时仍然输出重启提示——验证：同上
- [x] AC3：遍历到达请求深度、更深处还有按同一置信度下限可达的调用方时，`truncated()` 为真；刚好查完、置信度低于下限、或者只剩已访问过的节点（环）时为假——验证：`cargo test --lib graph::`
- [x] AC4：说明文字只点名自己的原因和调整方式（`--depth`，最大 10），与行数上限、深度硬上限的说明互不包含——验证：`cargo test --lib graph::routes`
- [x] AC5：用 CLI 端到端跑一条深度 4 的调用链：`impact`（默认深度 3）的文本带 ⚠ 说明，JSON 里 `callers_truncated` 为 true；加 `--depth 4` 后两者都没有——验证：新的 e2e 测试
- [x] AC6：daagu 副本上，`impact normalize_email` 带上说明；编辑前 hook 的摘要带上"这是下限"那一行——验证：手工命令，写进验证记录（选项 2 不发，提示词 hook 那半条作废）
- [x] AC7：全量测试和两条 clippy 腿都是绿的；pre-commit 通过

## 方案

- **hook**：`determineQueryType` 只剩 overview 一个分支；runMain 里删掉 impact、callgraph、search 的运行和歧义提示的输出。intent 检测、`extractSymbols` 等导出函数，测试和别的地方还在用，保留。
- **遍历**：在 `query_direction_seeded` 的 BFS 循环结束后：如果是因为达到 `max_depth` 而停下、且 frontier 非空，就用同一条扩展 SQL（同样的关系和置信度门槛）查一次，看 frontier 有没有未访问过的子节点。结果作为第三个返回值，存进 `CallGraphResult.deeper_beyond_depth`，再传给 `RouteCallers.deeper_beyond_depth`。
  - 和 `depth_capped` 的关系：请求深度超过 10 时只报硬上限，因为那种情况加深已经没用。
  - 代价：每次 impact 多 1–⌈frontier/400⌉ 条查询。

## 计划

- [x] S1（只在 ups-inject-off-opt2 上，不发）：hook 测试改为新行为（先红），再改 hook（转绿）——覆盖 AC1、AC2
- [x] S2：query.rs / routes.rs 单元测试（先红），再实现——覆盖 AC3、AC4
- [x] S3：CLI e2e 测试——覆盖 AC5
- [x] S4：文档和 CHANGELOG；版本 0.205.0（INDEX_VERSION 不变）
- [x] S5：daagu 副本实测——覆盖 AC6；全量检查——覆盖 AC7
- [x] S6：发版前评审（新开的子代理，独立简报），修复，然后按发版手册发版

## 决策与进展

- 2026-11-24：在分支 `ups-inject-off` 上开发。带偏测试在后台跑，结果写进 maintenance-mode spec 和 CHANGELOG。
- 选项 3 已提交（f8d82ed3），AC3、AC4、AC5 都已验证。选项 2 已单独提交（ad46735b），AC1、AC2 已验证。
- **用户决定（2026-11-24，问答）：只发选项 3。** 下面这条是当时问的问题，留作记录：P0-2b 发现装插件组少用 41% 的轮数（10/10 对），正确率不变。选项 2 删掉的正是这条注入，所以选项 2 的前提动摇了，要请用户在新证据下决定：只发选项 3（丢弃 ad46735b）、2 和 3 都发，还是再测一轮（开发版、装插件但关掉提示词注入，约 $1）。

- 发版前评审第 1 轮（新开的子代理，独立简报）：HIGH 0、MEDIUM 2、LOW 8。处理如下：
  - F1（深度 10 时建议'用更大的深度'；impact 先把深度限到 10，所以硬上限的说明在 impact 上不可能出现）、F3（撞了行数上限时，加深没有用）、F8（没人指定深度时也写'requested depth'）：一起修。说明改为点名实际走的深度；只有比 10 浅、且没撞行数上限时才建议加深；走到 10 时改为建议从第 10 层的调用方再查。
  - F2（CHANGELOG 两句错话）：改正。
  - F6（变异体 M4、M10、M15 存活）：补了测试，三个变异都会被测出。M6（both 方向）没有补：没有任何地方读它，理由记在这里。
  - F4（深处只有测试时也说是下限）、F5（callgraph、trace 也多跑一条查询；评审实测的数字）、F9（JSON 没有结构化的原因）：写进 CHANGELOG 的成本说明和未覆盖部分，代码不改。
  - F7（详细说明文档里还写着 Grep）：不在授权范围内，也不是本次修复的必要部分，记为遗留，交给用户决定，同时写进 CHANGELOG 的未覆盖部分。
  - F10（spec 状态没更新）：发版后更新。
- 第 2 轮评审（新开的子代理，只审修复提交 a5e0abae）：HIGH 0、MEDIUM 0、LOW 6；结论 a–f 全部成立。按规则到这一轮为止，只做不改变程序行为的修补：三处注释如实改写；补了只触发硬上限时的建议断言（R4 变异会被测出），以及更深调用方落在第一个分块的用例（只读最后一个分块的变异会被测出）；measure.py 能算出深度 10 时的 7 条（6+1）；CHANGELOG 的成本说明补上 `find_http_route`，未覆盖部分补上两处早就存在的说明文字问题。提示文字本身不改，所以不需要第 3 轮评审。

## 验证记录

- `node --test user-prompt-context.test.js` → 通过：105/105。先红 3 个：impact 被推送、CLI 被运行。
- `cargo test --lib graph::` → 通过：104/104。先红 3 个：字段恒为 false 时的断言失败。
- 新的 CLI e2e 和 MCP 测试 → 通过：impact_* 29/29，lib 107/107。用变异探针确认过：关掉检测后两个测试都在"requested depth"断言上失败。
- JS 全量 → 通过：1785 过，0 失败，3 跳过。hook-fire 的正对照改用 overview 提示。
- pre-commit（cargo test、两条 clippy 腿）→ 通过：f8d82ed3、ad46735b。
- AC7 → 通过：发版提交 87e289c9、修复提交 a5e0abae 和 d662a118 的 pre-commit 都通过（全量 cargo test、两条 clippy 腿）。d662a118 的 CI 45479103125 → 通过，7/7。新测试在 macOS、Windows 两条腿的日志里各 10/10。
- AC6 → 通过，用的是发布出去的产物：npm 装 `@sdsrs/code-graph@0.205.0`，linux-x64 包自带的二进制，以及包里自带的 `pre-edit-guide.js`。
  - `impact normalize_email`（默认深度 3）：4 callers total，带"stopped at depth 3 … FLOOR, not a total … impact --depth N"说明。
  - `--depth 10`：9 个，没有说明。真值是 9 个。
  - 编辑前 hook 的摘要带"⚠ the caller traversal stopped early, so these counts are a floor, not a total"。
- 发版：tag v0.205.0（d662a118）。Release 45481732742 → 通过，10/10（约 33 分钟）。GitHub Release 非草稿，14 个附件，说明取自 CHANGELOG 0.205.0 节。npm 六个包的 latest 都是 0.205.0，`_npmUser` 都是 GitHub Actions（trustedPublisher github）。
- 发布产物上的 5 层调用链（chain_0 … chain_4）：默认深度给 3 个，带说明，JSON 里 `callers_truncated` 为 true；`--depth 4` 给 4 个，没有说明。
