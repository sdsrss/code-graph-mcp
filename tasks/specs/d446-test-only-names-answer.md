# D#446：MCP 回答只在测试代码里定义的名字

状态：进行中。级别 L3（LLM 看到的默认回答改变：约 800 个名字由拒绝变为回答，次版本号）。用户 2026-10-20 授权方案 (a)，范围 `src/mcp/server/tools/refs.rs`、`src/mcp/server/tools/ast_node.rs`、`src/resolve.rs`。

## 背景与目标

一个名字的定义全在测试代码里（`is_test_symbol`：测试路径，或名字以 `test_` 开头、以 `Test`/`Tests` 结尾）时：

- MCP `find_references` 拒绝："Symbol 'X' exists but all N match(es) are in test/bench paths … Pass node_id … or file_path …"。这条拒绝早于 `include_tests`（默认 true，"重命名审查要看到每个用处"），所以重命名一个测试辅助函数时，只有 MCP 这一面拒绝。
- MCP `get_ast_node` 说 "Symbol 'X' not found in index"，是假话：这个名字在索引里。
- MCP `get_call_graph`、CLI `refs`/`callgraph` 都回答（CLI `refs` 把所有同名行合并成一个目标）。

0.172.0 上的规模（D#435 的测量）：flask 749 个、leveldb 33 个名字在 `find_references` 上被拒绝；另有 flask 89 个、leveldb 29 个带点名字退回裸名后落到同一条拒绝。

目标：这两个工具直接回答这些名字。测试代码里只有一个定义 → 回答它；有多个 → 按歧义拒绝并列出 node_id（不像 CLI `refs` 那样合并）。

## 范围

- 做：`find_references`、`get_ast_node` 在没有 `node_id`、没有 `file_path` 时的裸名路径（含带点名字退回的裸名）；`resolve.rs` 增加"测试代码这一层的定义"，规则与 `preferred_definitions` 相同（标题让位给代码，构造函数让位给类）。
- 不做：`get_call_graph`（已经回答，多个测试定义时合并；与本次两个工具的歧义裁决不同，记入遗留）；CLI（`refs` 的合并、D#458）；带 `file_path` 或 `node_id` 的分支；`<external>` 与测试定义同名时 `find_references` 照旧回答导入；工具描述不提这条拒绝，不改。

## 输入形态表

性质：只有 BASE 回答是"测试路径拒绝"（`find_references`）或"not found 但有同名测试定义"（`get_ast_node`）的输入改变；其余输入在所有面上与 BASE 逐字节相同。

| 形态 | 例子 | BASE `find_references` | BASE `get_ast_node` | HEAD 两个工具 | CLI `refs` |
|---|---|---|---|---|---|
| 测试代码一个定义 | flask `after`（tests/） | 测试路径拒绝 | not found | 回答该定义 | 回答 |
| 测试代码多个定义（不同文件） | 两个 tests/ 文件的 `helper` | 测试路径拒绝 | not found | 歧义，列出 node_id | 合并回答 |
| 测试代码多个定义（同一文件） | 两个测试类的 `get` | 测试路径拒绝 | not found | 歧义（同文件措辞） | 合并回答 |
| 名字像测试、文件在生产路径 | src/ 里的 `test_inline_00_calls_target` | 测试路径拒绝（把生产文件说成测试路径） | not found | 回答 | 回答 |
| 测试类 + 它的构造函数（Java/C#/C++/Dart） | tests/ 里的 `FooTest` | 测试路径拒绝 | not found | 回答类（构造函数让位） | 合并 |
| 测试路径的标题 + 测试函数 | tests/README.md `## foo` + `def foo` | 测试路径拒绝 | not found | 回答函数（标题让位） | 合并 |
| 只有测试路径的标题 | tests/README.md `## Setup` | 测试路径拒绝 | not found | 回答标题 | 回答 |
| 测试定义 + `<external>` 导入同名 | `test_support` | 回答导入 | not found | `find_references` 不变（导入）；`get_ast_node` 回答测试定义 | 合并 |
| 精确限定名，生产路径里名字像测试的方法 + 测试文件里的同名 | `Job.test_step`（src/ + tests/） | 回答 src/ 的 | not found | `get_ast_node` 回答 src/ 的（与 `find_references` 相同，按位置选） | 回答 |
| 带点名字退回到上面任一形态 | `Flask.after` | 同裸名 | 同裸名 | 同裸名 + `note` | 同裸名 + 说明 |
| 精确限定名、只有测试定义 | `Index.dispatch_request` | 已回答 / 歧义 | 同 | 不变 | 不变 |
| 有生产定义 | `route` | 回答 / 歧义 | 同 | 不变 | 不变 |
| `<external>` 独有 | `json` | 回答导入 | not found | 不变 | 回答 |
| 没有任何行 | `afte` | 模糊步骤 | not found | 不变 | 模糊步骤 |
| 带 `file_path` / `node_id` | | 文件分支 / 按 id | | 不变 | |
| 以上加空白 | `after ` | 同不加 | 同不加 | 同不加 | 同不加 |
| `include_tests: false` | | 测试路径拒绝 | — | 回答，测试代码里的引用照常过滤并计数 | — |

## 方案

`resolve.rs`：把 `preferred_definitions` 的主体抽成按层取定义的私有函数（生产层 / 测试代码层），过滤、标题让位、构造函数让位的规则两层共用。新增 `test_code_definitions(conn, name)`：测试代码这一层的定义，调用方只在 `preferred_definitions` 为空时用它。`preferred_definitions` 的行为不变（差分验证）。

`find_references`（裸名、`NotFound` 分支）：`<external>` 行存在 → 照旧回答导入；否则取 `test_code_definitions`：1 个 → 回答它（目标只有这个 node）；≥2 个 → `ambiguity_response`；0 个（例如只有没有 `files` 行的孤立节点）→ 原来的拒绝不变。

`get_ast_node`（裸名）：`preferred_definitions` 为空时取 `test_code_definitions`：≥2 个 → 歧义；1 个 → 回答；0 个 → 原来的 not found。

不选的：

- 照 CLI `refs` 合并所有同名行：用户选 (a) 时明确排除；合并会把两个不同符号的引用加在一起，`get_call_graph` 的 D#447 刚因此改成歧义。
- 测试定义优先于 `<external>`：会把现在回答导入的输入改成回答别的东西（回答 → 另一个回答），超出"拒绝 → 回答"的范围。

## 验收标准

- [x] AC1：夹具上每个形态的 HEAD 回答如上表 — 验证：`cargo test test_a_name_only_test_code_defines_is_answered`（先在 BASE 代码上失败）
- [x] AC2：语料差分：BASE → HEAD 改变的输入都在"BASE 测试路径拒绝 / not found 且有测试定义"里；其余逐字节相同 — 验证：`tasks/d435-harness/d446.py base head2 --root /var/tmp/d446`
- [x] AC3：按输入所指节点打分：回答的节点就是那个名字的测试定义 — 验证：`tasks/d435-harness/d446_score.py base head2 <head2 binary>`
- [ ] AC4：全量 pre-commit（cargo test、fmt、clippy 两条腿）通过
- [ ] AC5：发版前评审（新开子代理）无未处理的 HIGH

## 计划

- [x] S1：BASE 测量（0.172.0 源码构建，flask/leveldb + 其他语言语料），填表 — 主会话
- [x] S2：RED 测试 — 主会话
- [x] S3：实现 + 改写编码了旧拒绝的测试 — 主会话
- [x] S4：差分 + 打分 + 变异 — 主会话
- [ ] S5：CHANGELOG（实测每条说法）、发版前评审 — 评审用新开子代理
- [ ] S6：发版 0.173.0 — 主会话

## 决策与进展

- 2026-10-20：用户选 (a) 并授权。
- 测试代码层只在 `preferred_definitions` 为空时才查；`<external>` 存在时 `find_references` 照旧回答导入（只把"拒绝 / not found"改成回答，不把一个回答换成另一个回答）。
- 差分时发现（夹具复现，语料里没有）：精确限定名 `Job.test_step`（src/ 里名字像测试的方法 + tests/ 里的同名）`get_ast_node` 走测试层后成了歧义，而 `find_references`、`get_call_graph`、CLI 按位置回答 src/ 的。改为：`select_symbol` 给出精确限定名时，`get_ast_node` 的测试层用 `selectable_qualified_definitions`（与 `find_references` 同一集合）。七个语料上这处改动前后 431,098/431,098 逐字节相同。
- 不改 `get_call_graph`（授权范围外）：多个测试定义时它合并回答，本次两个工具按歧义拒绝；七个语料 299 个名字，写进 Not covered。

## 先测

BASE = `16f9bdf2`（0.172.0 源码，`git archive`，`cargo build --release` 默认特性），md5 `2c54890c…`。语料在 `/var/tmp/d446/corpora`：flask `d086db85`、leveldb `bb74ef7`、express `1faf228`（4.21.2）、hono `4eb101d`（v4.6.14）、gin `75ccf94`（v1.10.0）、gson `828a97be`（gson-parent-2.11.0）、cgm（本仓库 `16f9bdf2` 的 `src/`+`tests/`）。输入用 `gen.py`（seed 435）：45,723 个；每个版本 431,098 次运行（MCP 6 个面、CLI 5 个面：`callgraph` 三个方向、`refs`、`impact` 788 次）。BASE 跑两遍 431,098/431,098 逐字节相同。

BASE 上 `find_references` 的测试路径拒绝：flask 862、leveldb 70、express 196、hono 727、gin 664、gson 2,228、cgm 2,437。其中提示里把非测试路径说成测试路径的：cgm 427、leveldb 70、flask 9（`test_cli_runner` 在 `src/flask/app.py`）、gson 1。

## 验证记录

- RED：`test_a_name_only_test_code_defines_is_answered` 在改动前的代码上失败：`get_ast_node make_client` → "Symbol 'make_client' not found in index"。`Job.test_step` 断言先加、先失败（歧义），再改 `get_ast_node`。
- GREEN：同一测试和 `test_an_exact_definition_is_answered_not_a_near_name`、`test_mcp_selects_the_symbol_the_cli_selects`、`test_a_nested_type_typed_with_dots_is_that_type` 通过；集成测试 `mcp_caller_count_prod_only_and_test_symbol_is_answered` 通过（原名 `..._error_explains`，断言旧拒绝，按新契约改写）。
- 变异（`mutate.py`，副本 `/var/tmp/d446/src-head2`，独立 target）：9 个变异 8 个被抓到；存活的"测试层也收生产定义"在现有调用点等价（只在没有生产定义时查测试层），加了一行直接断言 `test_code_definitions("ping")` 为空后被抓到，9/9。
- 差分 BASE → HEAD2（md5 `d209eeba…`）：431,098 次里改变 19,070 次（9,248 个输入），全部是 `find_references` 测试路径拒绝 → 回答 6,844 / 歧义 340，`get_ast_node` not found → 回答 8,908 / 歧义 340，`include_impact` not found → 回答 2,597 / 歧义 41；`get_call_graph` 和 CLI 五个面 0 处改变；HEAD2 上测试路径拒绝 0 次；`get_ast_node` 回答的节点都是测试代码里叫这个名字的节点。
- 打分：每个改变的输入，`find_references(name)` 等于 `find_references(node_id=get_ast_node 回答的节点)` 8,908/8,908；歧义时两个工具候选 node_id 相同 340/340。
- 被改变的输入在 BASE 上 `get_call_graph` 和 CLI `refs` 都回答（6,844 + 340），即本次之前只有这两个 MCP 工具拒绝。
- clippy 两条特性腿通过（`-D warnings`）。

## 发版前评审

### 第 1 轮（新开子代理，`16f9bdf2..c59d0b56`）

评审自己用 `git archive` 构建两个版本（md5 与上面相同），在自己的语料副本上重跑 431,098 次、用自己的统计脚本复算，CHANGELOG 的数字都对上（surface 个数除外）；另写夹具 575 次运行，改变的 132 次都是拒绝 / not found → 回答 / 歧义。0 个 HIGH。

- MEDIUM F1（已复现，我在自己的夹具上也复现）：`<external>` 导入与测试定义同名（`import requests` + 测试里的 `def requests()`）：`find_references` 回答导入，`get_ast_node` 现在回答测试定义（多个时歧义），两个工具答的不是同一个节点。七个语料里导入与定义同名的 10 个名字都有生产定义（我用 SQL 数的：flask 3、express 5、hono 1、cgm 1）。
- LOW F2：CLI 是 5 个面不是 4 个（`impact` 788 次）。F3：2,025 里列出的三个语料只合 1,985，另 40 个在其他四个语料。F4：`_AppCtxGlobalsProxy.test_no_routes` 是 flask 的例子，放在了 leveldb 后面。F5：提交说明把"精确限定名 `get_ast_node` 答 `find_references` 答的那个"写成了无条件的说法；生产层里 gtest `TEST(Foo, test_run)` 与方法 `Foo::test_run` 两个版本都不一致（`resolve.rs` 生产层按 `is_test_symbol`、`selectable_qualified_definitions` 按 `is_test`/路径），这次没改；提交说明的 6,844 + 8,908 + 340 + 340 漏了 `include_impact` 的 2,597 + 41。
- LOW F6（已复现）：测试代码定义的名字，同名 Markdown 标题在非测试路径时，两个工具都答标题（两个版本相同），`get_call_graph` 跟函数走（`test_callers_filtered: 1`）。七个语料 0 个。

处理（只改文档）：F1、F6 写进 Not covered（F1 与"只把拒绝改成回答"的决定一致，不改代码）；F2–F4 改 CHANGELOG 和本文件；F5 的说明错误在下一条提交说明里更正，行为本身记为延期项。
