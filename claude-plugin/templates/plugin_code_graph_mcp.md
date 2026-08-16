---
name: code-graph-mcp 插件契约
description: code-graph-mcp 工具调度规则 — 何时用 MCP/CLI 替代 Grep/Read，invited-memory 模式
type: reference
---
# code-graph-mcp 插件契约

> 本文件是 code-graph 工具调度细则。项目根 `CLAUDE.md` 里有一个精炼的 managed
> 块（sentinel 包裹）做路由摘要 + 指针；本完整决策表按需打开，不每会话常驻。
>
> **v0.74 起**：插件（`/plugin install`）模式下首次 SessionStart 自动安装——
> 在项目根 `CLAUDE.md` 注入/创建 managed 块，并把本文件写到 `.claude/plugin_code_graph_mcp.md`。
> 旧版（≤v0.73）写到 `~/.claude/projects/<slug>/memory/`（MEMORY.md sentinel + 本文件），
> 升级后首次 SessionStart 自动清理那些遗留制品（你的其它 memory 不受影响）。
> 退出：`CODE_GRAPH_NO_AUTO_ADOPT=1` 阻止，`code-graph-mcp unadopt` 回退。
>
> 已安装的项目在下次 SessionStart 会自动对齐到插件 shipped 的最新决策表
> （本文件 / CLAUDE.md 块与 shipped 差异时覆盖）。手动编辑会被覆盖——
> 要锁定自己的版本，设 `CODE_GRAPH_NO_TEMPLATE_REFRESH=1`（不影响首次安装）。
>
> **Hook 默认值（两个 hook，默认不同 —— 故意的）**：
> - **SessionStart `project_map` 注入：默认 OFF**（v0.17.0 起）。本文件 + 7 个
>   工具描述已经覆盖路由所需决策信息，每次会话再 dump ≈2.3 KB 项目地图是冗余的
>   常驻上下文。显式启用：`CODE_GRAPH_VERBOSE_HOOKS=1`；或按需 `code-graph-mcp map --compact`。
> - **UserPromptSubmit context push：默认 ON**。基于用户消息 intent 推 impact /
>   overview / callgraph / search 结果（per-type cooldown 30s–5min）。routing-bench
>   P@1=100% 测的是分诊准确率（已决定查工具时选哪个），不等于触发率（是否
>   决定查工具）—— 真实 baseline 是 raw-grep ≈13× 偏向于内置 Grep。Push 是
>   pre-training bias 的矫正。Escape hatch：`CODE_GRAPH_QUIET_HOOKS=1`。
> - 优先级：`CODE_GRAPH_QUIET_HOOKS=1` (escape) > 其他 env > 默认。
>
> **v0.18.4 起**：原"进阶 5"（impact / similar / deps / dead-code / trace）已折叠
> 进核心 7 的 flag —— Claude Code 现在能直接通过 MCP 调用，不必落到 CLI:
> - `get_ast_node include_impact=true` / `include_similar=true`
> - `module_overview include_deps=true` / `include_dead=true`
> - `get_call_graph route_path="GET /api/x"`
>
> `impact_analysis` **已删除**——dispatcher 里没有这个 arm，按名调用返回
> `Unknown tool`。改用 `get_ast_node include_impact=true`，或 CLI `impact`。
> 其余旧名（`read_snippet` / `dependency_graph` / `find_similar_code` /
> `find_dead_code` / `trace_http_chain` + alias `find_http_route`）仍是向后兼容
> dispatcher 别名（raw JSON-RPC / SDK 脚本场景），但 Claude Code 内一律用上面的
> 新 flag 形式。CLI 子命令
> （`code-graph-mcp impact|similar|deps|dead-code|trace`）保持不变，给 Bash 工作流。

## 何时调用 MCP/CLI（替代多步 Grep/Read）

> **v0.49 起 CLI 优先**：Claude Code 里 MCP 工具是 deferred（首次调用前要
> ToolSearch 加载），而 Bash 永远在线——真实编程夜（2026-06-12）观测到的全部
> 转化都是 CLI 调用。结构化查询的最快路径是 Bash 直呼
> `code-graph-mcp callgraph X / show X / overview <dir> / grep "pat" / impact X`。
> `grep` 是 drop-in 替代：`-F` 字面 / `-i` / `-w` / `-l` / `-c` 计数 / `-t <lang>` 按语言筛 /
> `-g <glob>` 路径过滤 / `-A/-B/-C N` 上下文 / `-M N` 行宽上限（默认 512，防长行刷屏）/
> 多路径 / `-m 0` 取消每文件上限，退出码兼容 grep（0/1/2），召回达 git-grep 级
> （tracked-but-gitignored 也能搜到），每条命中标注所属 fn/class。
>
> v0.10.0 起：tools/list 默认只暴露 7 个核心工具；下表"进阶 5"中的工具
> 已从 tools/list 隐藏以节省 session 启动 tokens。**Claude Code 里请走 CLI
> 子命令**（MCP schema 不在 list，Claude Code 的 ToolSearch 不会加载，直接
> 调用会得到 `No such tool available`——实测验证见下方"进阶 5"）。写
> MCP SDK / 原生 `tools/call` JSON-RPC 的脚本场景仍可按名调用。

### 核心 7（tools/list 默认暴露）

| 意图 | 工具 | 关键参数 / 例子 |
|------|------|----------------|
| "谁调用 X？" / "X 调了啥？" | `get_call_graph` / `callgraph X` | 替代 `grep "X("` |
| "Y 模块长啥样？" | `module_overview` / `overview Y/` | 替代逐文件 Read |
| "找做 Z 的代码"（概念） | MCP `semantic_code_search`（RRF 混合）；CLI `search`（纯 FTS5） | 不知道精确名；要向量召回走 MCP |
| "返回 T 类型的函数" | `ast_search --returns T` | 结构化筛选 |
| "X 在哪被引用？" | `find_references` / `refs X` | 含 callers/importers；`refs X --min-confidence extracted` 滤掉跨文件裸名低置信边（inferred/ambiguous） |
| "看 X 的源码 / 签名" | `get_ast_node` / `show X` | `include_impact=true` 影响面 / `include_similar=true` 嵌入近邻 |
| "项目结构总览" | `project_map` / `map` | 起手势用 `--compact` |
| "X 文件依赖谁？" / "Y 模块下的死代码" | `module_overview path=Y include_deps=true` / `include_dead=true` | 文件路径走 deps；目录/文件走 dead |
| HTTP 路由 → handler 链 | `get_call_graph route_path="GET /api/x"` | 取代 trace_http_chain |

### 旧名兼容 + CLI 速查（v0.18.4 fold 后）

v0.18.4 把原"进阶 5"折叠进核心 7 的 flag。**Claude Code 内**首选上表的新 flag 形式。

可按名调用的只有这 7 个：`project_map` / `semantic_code_search` / `get_call_graph` /
`get_ast_node` / `module_overview` / `ast_search` / `find_references`。
`impact_analysis` **不在其列，已从 dispatcher 删除**——按名调用返回 `Unknown tool`。
隐藏旧名 `read_snippet` / `dependency_graph` / `find_similar_code` / `find_dead_code` /
`trace_http_chain`（+ alias `find_http_route`）仍有 dispatcher arm，raw JSON-RPC / MCP
SDK / 既有脚本不破；但它们不在 `tools/list`，Claude Code 的 ToolSearch 不为其加载
schema —— 实操中一律走新 flag。CLI 子命令保持原样：

| 意图 | CLI（Bash 工作流） | 等价 MCP 新形式 |
|------|--------------------|------------------|
| "改 X 会炸啥？" | `code-graph-mcp impact X` | `get_ast_node symbol_name=X include_impact=true` |
| HTTP 路由 → handler 链路 | `code-graph-mcp trace /api/x` | `get_call_graph route_path="GET /api/x"` |
| "X 文件依赖谁？" | `code-graph-mcp deps src/x.rs` | `module_overview path="src/x.rs" include_deps=true` |
| "相似/重复函数"（需 embedding） | `code-graph-mcp similar X` | `get_ast_node symbol_name=X include_similar=true` |
| "未使用的代码" | `code-graph-mcp dead-code [path]` | `module_overview path=<path> include_dead=true` |
| "架构咽喉/桥节点是谁？" | `code-graph-mcp centrality` | —（CLI-only；betweenness 中心性，补 `map` 的 caller_count 度中心性） |
| "循环导入依赖（哪些文件互相 import）？" | `code-graph-mcp cycles` | —（CLI-only；文件级 import 环 = SCC；JS/TS/Py/Go 是坏味，Rust 内部环常良性） |
| "可疑/意外的跨模块耦合？" | `code-graph-mcp surprising` | —（CLI-only；跨文件 calls/refs 按 低置信(ambiguous>inferred)+跨模块+sole-bridge 打分） |
| "代码健康总览（想要一份报告）？" | `code-graph-mcp report` | —（CLI-only；汇总 summary+置信度 / hot / chokepoints / cycles / surprising / dead-code） |

**dead-code 的 `ignore_paths`**：CLI 默认豁免 `["claude-plugin/", "benches/"]`
（macro/shell 入口点）；`--no-ignore` 关闭。MCP 端也接同名参数。

## 不要替代

- 非代码文件（README/JSON/log） → 用内置 `Grep`
- 代码里查常量/函数名/字符串首选 `code-graph-mcp grep "pattern" [path]`（每个命中带 containing function/module 上下文，结构化）；只做纯文本匹配且不关心上下文时用内置 `Grep`
- 即将编辑的具体文件 → 用 `Read`（`overview <file>` 看概览，`show SYMBOL` 看某符号）

## 工作流惯例

1. 起手 `project_map`（或 Bash 调 `code-graph-mcp map --compact`）看架构
2. `semantic_code_search` 默认带 `compact=true`，省 token
3. 展开节点：`get_ast_node node_id=N compact=true` 看签名 / 不带 compact 看全文
4. 改前评估影响：`get_ast_node symbol_name=X include_impact=true`（核心 7 内，首选）
   或 Bash 调 `code-graph-mcp impact X`（独立进程；输出更细：风险等级 + 路由 + 文件计数）
5. 搜不到结果 → `code-graph-mcp health-check` 检查索引与 embedding 覆盖率

可用 prompts：`impact-analysis`、`understand-module`、`trace-request`

## CLI 速查（替 Bash）

```
code-graph-mcp grep "pattern" [path]     # ripgrep + AST 上下文（-t lang / -g glob / -c 计数 / -M 行宽）
code-graph-mcp search "concept"          # 纯 FTS5（要混合检索走 MCP semantic_code_search）
code-graph-mcp ast-search "q" --type fn  # 结构化筛选
code-graph-mcp map                       # 项目架构
code-graph-mcp overview src/mcp/         # 模块总览
code-graph-mcp callgraph SYMBOL          # 调用图
code-graph-mcp impact SYMBOL             # 影响面（--change-type ∈ signature|behavior|remove，默认 behavior）
code-graph-mcp show SYMBOL                # 节点详情
code-graph-mcp refs SYMBOL --relation calls  # --relation ∈ calls|imports|inherits|implements|references|exports|routes_to|all
code-graph-mcp refs SYMBOL --min-confidence extracted  # ∈ extracted|inferred|ambiguous；extracted=只看精确边（callgraph/impact/trace 同款）
code-graph-mcp centrality                 # 架构咽喉（betweenness 桥节点；补 map 的 caller_count）
code-graph-mcp cycles                     # 循环导入依赖（文件级 import 环 / SCC）
code-graph-mcp surprising                 # 可疑跨模块耦合（低置信 + 跨模块 + sole-bridge 打分）
code-graph-mcp report                     # 代码健康总览（汇总 hot/chokepoints/cycles/surprising/dead-code）
code-graph-mcp dead-code [path]           # 未使用代码（默认豁免 claude-plugin/）
code-graph-mcp dead-code --ignore tmp/ --ignore scripts/bin/  # 自定义豁免前缀
code-graph-mcp dead-code --no-ignore      # 关掉默认豁免，看完整列表
code-graph-mcp health-check              # 索引健康
```

完整列表：`code-graph-mcp --help`。

## 质量门槛

- `compact=true` 一般够用；要看完整代码再去掉
- `impact` 在 `--change-type signature` 时返回最严格的破坏面
- 索引陈旧 → SessionStart 自带 `ensureIndexFresh`；手动跑 `incremental-index`

## 卸载 / 回退

- `code-graph-mcp unadopt` — 精确移除 CLAUDE.md 里的 managed 块 + `.claude/plugin_code_graph_mcp.md`（块移光后若 CLAUDE.md 只剩我们的内容则连文件一起删，否则保留你的正文）；并清理任何遗留 memory-dir 制品。
- `CODE_GRAPH_NO_AUTO_ADOPT=1`（`~/.claude/settings.json` env） — 阻止未来自动安装，不影响已安装状态。
- `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` — 锁定 CLAUDE.md 块 + 本文件不随插件升级刷新；允许手动编辑长久保留。
- `CODE_GRAPH_VERBOSE_HOOKS=1`（v0.17.0+） — opt in 到 SessionStart `project_map` 注入（默认 OFF）。
- `CODE_GRAPH_QUIET_HOOKS=1` — UserPromptSubmit context push 的 escape hatch（默认 ON）；同时强制 SessionStart `project_map` quiet。
- `CODE_GRAPH_QUIET_HOOKS=0` — 强制恢复 SessionStart `project_map` 注入（向后兼容路径）。
