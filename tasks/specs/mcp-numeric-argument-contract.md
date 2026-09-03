---
status: draft
revision: 1
---

# MCP 数值参数契约：范围披露、clamp 披露、负数策略统一

## goal

让 MCP 数值参数的**声明**与**实际行为**一致，三处不一致同批消除：

1. **上限从未披露。** 10 个参数在 handler 里被 `.clamp(lo, hi)`，schema 描述只写默认值。
   调用方（Claude Code）读到 `similar_top_k` "default 5" 便无从知道上限是 50。
2. **clamp 静默。** `limit: 5000` 变成 100，响应里没有任何字段说明发生过截断。
   现存唯一提到 clamp 的地方是测试注释；生产代码零披露。
3. **负数策略按 helper 分裂。** `arg_u64` 拒绝负数，`arg_i64` 接受。
   同一个 JSON 数字在不同参数上两种命运，且都不是调用方能预期的。

顺带解决 CON-13：`get_call_graph` 的 `symbol_name` / `route_path` 二选一、
`get_ast_node` 的 `symbol_name` / `node_id` 二选一，handler 会拒绝但 schema 说不出——
`anyOf` 已实测会让客户端静默丢弃整个工具（见 `feedback_published_schema_needs_a_real_client`），
剩下的唯一通道是 description。

## non-goals

- **不改 clamp 的取值本身。** 上限是否合理是另一个问题；本批只让它们可见。
- **不给 `deps_depth` 补 clamp**（见 open-questions），除非调查表明当前行为有害。
- 不动 `max_distance`（f64，无 clamp，语义是相似度阈值不是计数）。
- 不改任何工具的 `required` 数组——`anyOf` 已被证伪，`required` 表达不了二选一。

## constraints

- **published 面**：`src/mcp/tools.rs` 的 schema 与 description 面向外部客户端（Claude Code），
  属 §5 hard-AUTH（已获授权）+ LLM 可见元数据 → L3。
- **routing-bench 前后基线（HARD）**：改 description 必须前后跑 `tests/routing_bench.rs`。
  基线已取：`8588e1a` 的 CI run `33667760311`，
  `mode=tool-only backend=openrouter/anthropic/claude-sonnet-4.5 domain=Backend P@1=22/22 = 100.0% (threshold 70%)`。
  本机无 `OPENROUTER_API_KEY` / `ANTHROPIC_API_KEY`，bench 是 `--ignored`，
  **本机跑不了**——改后数只能由 CI 提供（见 open-questions）。
- **描述预算**：`instructions` 字段有编译期 assert ≤1500 字节（`feedback_mcp_instructions_budget`）；
  单个工具 description 无硬上限，但描述变长会挤占客户端上下文，且 routing-bench 是唯一裁判。
- **负向措辞禁用**：本仓实测「DO NOT for X」形式的引导反而拉低 20pp
  （`feedback_negative_steering_backfire`），范围披露要写成正向陈述。
- 六个 schema-less 工具（`dependency_graph` / `find_dead_code` / `find_similar_code` /
  `find_http_route` / `trace_http_chain` / `rebuild_index`）没有 published schema，
  它们的数值参数只能靠 handler 行为一致，描述无处可写。

## success-criteria

1. 每个被 clamp 的参数，其 schema description 含实际上下限，且**由测试从生产常量推导**核对，
   不是再钉一次字面量（`feedback_layout_change_breaks_guards`：读文本的守卫会静默失效）。
2. clamp 真实发生时，响应含一个披露字段，命名参数、原值、生效值；未发生时不出现该字段
   （沿用本仓 `ambiguous_edges_hidden` / `hot_functions_truncated` 的既有形状）。
3. 负数在所有计数类数值参数上被统一拒绝，错误文案一致；
   现有 17 行类型守卫表扩出**负数轴**，每行预期红→绿（`feedback_parity_table_over_unguarded_axis`）。
4. `get_call_graph` / `get_ast_node` 的二选一约束出现在 description 里，措辞为正向。
5. 门：fmt 0 · clippy 两腿 0 · 两条 test 腿全绿 · routing-bench P@1 不低于 22/22 基线。

## open-questions

- ~~**Q1**~~ **已裁决（r3，用户选 (c)）**：本批**不改 description**，因而不触发
  routing-bench 前置。批次拆分：
  - **批 A（本批）**：负数策略统一 · clamp 披露字段 · `deps_depth` 的跨工具范围。
    全部是 handler 行为，本地门可以判定完。
  - **批 B（后续，需 bench）**：schema description 的范围文字 · CON-13 二选一措辞。
    等 API key 到位或另行裁定验证路径。
  success-criteria 1 与 4 随之移入批 B；本批只对 2、3、5（去掉 bench 那一项）负责。
- ~~**Q2**~~ **已解答（r2）**：`deps_depth`（`overview.rs:32`）本地无 clamp，但在
  `overview.rs:276` 被原样转发为 `tool_dependency_graph` 的 `depth`，
  后者 `advanced.rs:216` 做 `.clamp(1, 10)`。所以有效范围是 **1–10**，
  它是第 **11** 个 clamp 生效点，只是跨工具边界。不需要补 clamp。
  两点后果进入 scope：(a) 它的范围披露数字来自**另一个文件**的 clamp，
  正是 success-criteria 1 要求「从生产常量推导」的实例；
  (b) `deps_depth: -5` 在此被静默变成 1，属负数轴的一行。
- **Q3**：clamp 披露字段加进响应属 additive Δ-contract。是否需要 CHANGELOG 迁移说明，
  取决于是否算「released-artifact user-visible default behavior change」（§2-EXT 清单）。
  倾向：additive、无行为回退，`feat:` 而非 `change:`。

# Change log

- r1（2026-09-03）：初稿。基线、约束、四条 success-criteria 就位；Q1 需用户裁决后才能定验证路径。
- r2（2026-09-03）：Q2 关闭——`deps_depth` 由下游 `dependency_graph` clamp(1,10)，
  clamp 生效点从 10 个更正为 **11** 个。non-goals 里「不给 deps_depth 补 clamp」的前提
  由「未调查」变为「已确认下游已限界」。
- r3（2026-09-03）：Q1 裁决，拆 A/B 两批。本批 = 批 A（handler 行为，无 description 改动，
  不触发 routing-bench 前置）。status 仍 draft——批 B 未做，spec 未实现完。
- r4（2026-09-03）：**批 A 完成**（`66ea53c` 负数统一 + 本次 clamp 披露）。
  success-criteria 2 与 3 达成，5 的本地部分达成（bench 那项属批 B）。
  实现中一处设计被守卫推翻并订正：`arg_clamped` 初版签名把 tool 放在 key 之前，
  而 `tests/hardening.rs` 的两个源码扫描守卫都把 `args, ` 之后的第一个字符串当参数名——
  于是工具名被当成了未声明参数。改签名顺序（而不是教两个守卫认新形状），
  理由记在 `arg_clamped` 的 doc 里。
  status 仍 draft：批 B（description 范围文字 + CON-13）未做。
