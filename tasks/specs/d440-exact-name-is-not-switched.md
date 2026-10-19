# D#440：有精确定义的名字不换成近似名

状态：进行中（2026-10-19）。级别 L2（`src/resolve.rs` 共用解析器 + `src/mcp`；`fix:`，恢复"按输入的名字回答"）。来源：D#440（0.171.3 发版前评审 F1/F2），也是 0.171.3 写进 Not covered 的"填充写法由拒绝变成别的符号"的根因。

## 问题

模糊步骤只该用于"没有任何定义叫这个名字"的输入，但它在两种有精确定义的输入上也会换名字：

1. **只在测试文件里定义的名字。** `resolve::resolve_fuzzy` 先丢掉测试文件里的候选，再比精确名，所以这种名字永远不算精确，子串或编辑距离的近似名就被选中。flask `after` → `after_request`。影响 MCP `find_references`、MCP `get_call_graph`、CLI `callgraph`（没有边时才走模糊步骤）。CLI `refs` 不受影响：它只在 `get_node_ids_by_name` 为空时才走模糊步骤，而那个查询不过滤测试文件。
2. **精确的限定名，在所问方向上没有边。** MCP `get_call_graph` 照样跑模糊步骤；CLI `callgraph` 对精确限定名跳过（`is_exact_qualified`）。leveldb `Block.size --direction callees` → `doc/index.md` 的标题 `Block size`。

## 先测（改动前）

BASE = 2171df30（0.171.3 源码，`cargo build --release`，默认特性，md5 `be5b52af…`），语料 `/var/tmp/d440/corpora`：flask d086db8、leveldb bb74ef7（浅克隆）。

生成器 `gen.py`（不抽样）：索引里每个不同的定义名（flask 1,087、leveldb 1,327）、每个带点且不等于名字的限定名（386、1,167），各自再带上它所在的 `file_path` 跑一遍；另外 200 个拼错、200 个前缀（seed 440，都没有定义叫这个名字）作对照。面：MCP `get_call_graph` 三个方向、`find_references`，CLI `callgraph --json` 三个方向、`refs --json`；名字和限定名再加 4 种填充（尾空格、首空格、`\t…\n`、尾 NBSP），只跑 MCP `get_call_graph`（both）和 `find_references`。共 flask 38,552 次、leveldb 63,056 次。

"换了"= MCP 回答带 `note: Resolved '…'`，或 CLI stderr 有 `Resolved '…'`。"近似名列表"= `No exact match for 'X'. Did you mean`，X 有定义时这句话是假的。

BASE 上，有精确定义的输入（不含填充）：

| 语料 | 面 | 换了 | 近似名列表 |
|---|---|---|---|
| flask | MCP `find_references`，名字 | 21 | 0 |
| flask | MCP `get_call_graph`，名字，三个方向合计 | 45 | 36 |
| flask | CLI `callgraph`，名字，三个方向合计 | 45 | 0 |
| flask | MCP `get_call_graph`，限定名，三个方向合计 | 1 | 3 |
| leveldb | MCP `find_references`，名字 | 2 | 0 |
| leveldb | MCP `get_call_graph`，名字，三个方向合计 | 4 | 27 |
| leveldb | CLI `callgraph`，名字，三个方向合计 | 4 | 0 |
| leveldb | MCP `get_call_graph`，限定名，三个方向合计 | 6 | 12 |

CLI `refs` 0；CLI `callgraph` 限定名 0。填充写法和不填充的数字相同（`a18d687b` 之后填充写法继承不填充的回答）。

## 设计

规则：一个名字只要有精确定义（名字或限定名），就按它回答；模糊步骤只处理没有任何定义叫这个名字的输入。

- `resolve_fuzzy`：非测试的精确匹配照旧优先；没有时，如果测试代码里有精确匹配，返回 `Unique(输入本身)`，不再看近似名。
  - MCP `get_call_graph`、CLI `callgraph`：回答这个名字自己的调用图（种子查询本来就不过滤测试）。
  - MCP `find_references`：走 `Unique` 分支里已有的"全部定义都在测试路径"提示，和 0.171.3 对没有近似名的测试专属名字给的回答一样。
- MCP `get_call_graph`：精确限定名（`selectable_qualified_definitions` 非空，和 CLI `select_cli_symbol` 同一个判断）跳过模糊步骤。

不选的：

- MCP `get_call_graph` 改成"有种子就跳过模糊步骤"：结果相同（种子的名字分支和 `resolve_fuzzy` 的精确候选是同一批行，只差 `<module>` 这种名字），但要删掉几个分支，改动面更大，也不再和 CLI 用同一个判断。
- `find_references` 对测试专属名字直接回答（CLI `refs` 就这样）：会把这类输入从拒绝变成回答，范围是所有测试专属名字（flask BASE 上 709 个名字给出这个提示），不只是有近似名的那些。是另一个决定，记入遗留。

## 输入形态表

性质：HEAD 的回答 == BASE 的回答，唯一例外是 BASE 换了名字或给了近似名列表、而输入有精确定义的那些；它们在 HEAD 上按输入本身回答（`find_references`：测试路径提示）。HEAD 上有精确定义的输入"换了"和"近似名列表"都是 0。

| 形态 | 例子 | BASE | HEAD |
|---|---|---|---|
| 非测试的精确名 | `route` | 按本身 | 不变 |
| 只在测试文件里定义的名字，有近似名 | `after` | 换成 `after_request` / 近似名列表 | 按本身；`find_references` 给测试路径提示 |
| 名字规则判为测试的生产函数（`test_*`、`*Test`） | `CompressionTableTest` | 同上 | 同上 |
| 只在测试文件里定义，没有近似名 | `a` | 按本身 / 测试路径提示 | 不变 |
| 精确限定名，所问方向有边 | `DBImpl.Write` callers | 按本身 | 不变 |
| 精确限定名，所问方向没有边 | `Block.size` callees | MCP 换成标题 / 近似名列表；CLI 按本身 | MCP 按本身；CLI 不变 |
| 带 `file_path` | `after` + 它的文件 | 不走模糊步骤 | 不变 |
| 拼错、前缀（没有定义叫这个名字） | `afte_request` | 模糊命中 + `note` | 不变 |
| 上面各项加空白 | `after ` | 同不加空白 | 同不加空白 |

## 验证记录

- RED（修复前）：
  - `resolve::fuzzy_tests::a_name_defined_only_in_test_code_resolves_to_itself`：得到 `after_request`，期望 `after`。
  - `mcp::server::tests::test_an_exact_definition_is_answered_not_a_near_name`：先在 `after` 上失败（`function: after_request` 带 `note`）；第一处修好后在 `Block.size` 上单独失败（得到 `Block size`）。两半都不是空过。
  - `test_cli_callgraph_answers_a_name_only_tests_define`：放进 0.171.3 源码树跑，失败在 stderr `Resolved 'after' → 'after_request'`。
  - 修复后三个都通过。D#431 的测试里原来"两种都接受"的 `validate` 分支，改成钉住 `get_call_graph` 按 `validate` 回答、不带 `note`。
- 差分：HEAD = 工作树 `cargo build --release`（默认特性，与 BASE 相同），md5 `4197d9d6…`。两个二进制用同一份索引，查询前后语料都没改动。

| 语料 | 运行次数 | 逐字节相同 | 改变 | 改变的输入 |
|---|---|---|---|---|
| flask | 38,552 | 38,098 | 454 | 40 个名字、2 个限定名 |
| leveldb | 63,056 | 62,850 | 206 | 18 个名字、14 个限定名 |

  - 改变的 660 次全部是有精确定义的输入，BASE 上的回答是：换了名字、近似名列表、CLI 带近似名列表退出 1，或 `find_references` 的"Ambiguous symbol 'X'"列出别的名字。HEAD 上 660/660 按输入本身回答；`find_references` 给出 `Error: Symbol 'X' exists but all N match(es) are in test/bench paths`。
  - 拼错 400、前缀 400、所有带 `file_path` 的运行：与 BASE 逐字节相同。
  - HEAD 上，有精确定义的输入"换了"或"近似名列表"：0（BASE 见上表）。
  - HEAD 上填充写法与不填充的回答逐字节相同：flask 11,784/11,784，leveldb 19,952/19,952。
  - flask `Flask.route`（没有定义叫这个名字，D#435 的范围）：BASE 和 HEAD 都答 `FlaskGroup`，带 `note`，加空白也一样。

## 决定

- `resolve_fuzzy` 对测试专属的精确名返回 `Unique(输入本身)`，同名的测试定义有几个都一样。`get_call_graph`/`callgraph` 的种子查询本来就合并同名的全部定义；`find_references` 在 `Unique` 分支里把它们全部判为测试路径，给出已有的提示。
- MCP `get_call_graph` 用 CLI 同一个判断 `selectable_qualified_definitions(...)` 非空来认"精确限定名"，所以两边对同一输入做同一个决定。
- `file_path` 分支不会出现"把 X 解析成 X"的说明：`file_selector_miss` 不过滤测试文件，测试专属名字在别的文件里有定义时，模糊步骤之前就已拒绝。

## 遗留

- MCP `find_references` 拒绝只在测试文件里定义的名字（让调用方传 `node_id` 或 `file_path`），CLI `refs` 直接回答：flask 749 个名字、leveldb 33 个，CLI `refs` 全部回答。0.171.3 就是这样（flask 709 个），本次把有近似名的那 40 + 18 个也归到这一类。`include_tests` 默认 true 时这种拒绝是否合理，需要单独决定。
- 这句提示里的路径没有去重：leveldb `AddBoundaryInputsTest` 列出 `db/version_set_test.cc, db/version_set_test.cc`。0.171.3 对没有近似名的测试专属名字就这样。
- 带点名字没有任何定义时 MCP 不退回裸名：D#435。
