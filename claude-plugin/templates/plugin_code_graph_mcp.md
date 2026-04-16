---
name: code-graph-mcp 插件契约
description: code-graph-mcp 工具调度规则 — 何时用 MCP/CLI 替代 Grep/Read，invited-memory 模式
type: reference
---
# code-graph-mcp 插件契约

> Invited-memory 模式：MCP `instructions` 仅留指针，决策细则集中在此。
>
> **v0.9.0 起**：插件（`/plugin install`）模式下首次 SessionStart 自动 adopt，
> 本文件自动写入，自动切换 quietHooks（跳过每次 project_map 注入）。
> 退出：`CODE_GRAPH_NO_AUTO_ADOPT=1` 阻止，`code-graph-mcp unadopt` 回退。
> 手动强控：`CODE_GRAPH_QUIET_HOOKS=0` 强制注入 / `=1` 强制静默（覆盖 adoption 推导）。

## 何时调用 MCP/CLI（替代多步 Grep/Read）

| 意图 | 工具 | 关键参数 / 例子 |
|------|------|----------------|
| "谁调用 X？" / "X 调了啥？" | `get_call_graph` / `callgraph X` | 替代 `grep "X("` |
| "改 X 会炸啥？" | `impact_analysis` / `impact X` | 修改函数签名前必跑 |
| "Y 模块长啥样？" | `module_overview` / `overview Y/` | 替代逐文件 Read |
| "找做 Z 的代码"（概念） | `semantic_code_search` / `search "Z"` | 不知道精确名 |
| "返回 T 类型的函数" | `ast_search --returns T` | 结构化筛选 |
| "X 在哪被引用？" | `find_references` / `refs X` | 含 callers/importers |
| "未使用的代码" | `find_dead_code` / `dead-code [path]` | 清理 exports |
| "相似/重复函数" | `find_similar_code` / `similar X` | 需 embedding |
| "X 文件依赖谁？" | `dependency_graph` / `deps X` | file 级别 |
| "看 X 的源码 / 签名" | `get_ast_node` / `show X` | `--include-impact` 含影响面 |
| "项目结构总览" | `project_map` / `map` | 起手势用 `--compact` |
| HTTP 路由 → handler 链路 | `trace_http_chain` / `trace ROUTE` | API 调试 |

## 不要替代

- 非代码文件（README/JSON/log） → 用内置 `Grep`
- 代码里查常量/函数名/字符串首选 `code-graph-mcp grep "pattern" [path]`（每个命中带 containing function/module 上下文，结构化）；只做纯文本匹配且不关心上下文时用内置 `Grep`
- 即将编辑的具体文件 → 用 `Read`（`overview <file>` 看概览，`show SYMBOL` 看某符号）

## 工作流惯例

1. 起手 `project_map --compact` 看架构
2. `semantic_code_search` 默认带 `compact=true`，省 token
3. 展开节点：`get_ast_node node_id=N compact=true` 看签名 / 不带 compact 看全文
4. 改前必跑 `impact_analysis`
5. 搜不到结果 → `code-graph-mcp health-check` 检查索引与 embedding 覆盖率

可用 prompts：`impact-analysis`、`understand-module`、`trace-request`

## CLI 速查（替 Bash）

```
code-graph-mcp grep "pattern" [path]     # ripgrep + AST 上下文
code-graph-mcp search "concept"          # FTS5 语义搜索
code-graph-mcp ast-search "q" --type fn  # 结构化筛选
code-graph-mcp map                       # 项目架构
code-graph-mcp overview src/mcp/         # 模块总览
code-graph-mcp callgraph SYMBOL          # 调用图
code-graph-mcp impact SYMBOL             # 影响面
code-graph-mcp show SYMBOL                # 节点详情
code-graph-mcp refs SYMBOL --relation calls  # 引用筛选
code-graph-mcp dead-code [path]           # 未使用代码
code-graph-mcp health-check              # 索引健康
```

完整列表：`code-graph-mcp --help`。

## 质量门槛

- `compact=true` 一般够用；要看完整代码再去掉
- `impact` 在 `--change-type signature` 时返回最严格的破坏面
- 索引陈旧 → SessionStart 自带 `ensureIndexFresh`；手动跑 `incremental-index`

## 卸载 / 回退

- `code-graph-mcp unadopt` — 精确移除 sentinel 段 + 本文件，quietHooks 自动回到 false（下次 SessionStart 恢复 project_map 注入）。
- `CODE_GRAPH_NO_AUTO_ADOPT=1`（`~/.claude/settings.json` env） — 阻止未来自动 adopt，不影响已 adopted 状态。
- `CODE_GRAPH_QUIET_HOOKS=0` — 强制恢复 project_map 注入（即使已 adopted）。
