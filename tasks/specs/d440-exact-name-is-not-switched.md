# D#440：有精确定义的名字不换成近似名

状态：评审两轮已收口，待发版（2026-10-19）。级别 L2（`src/resolve.rs` 共用解析器 + `src/mcp`；`fix:`，恢复"按输入的名字回答"）。来源：D#440（0.171.3 发版前评审 F1/F2），也是 0.171.3 写进 Not covered 的"填充写法由拒绝变成别的符号"的根因。

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

- `resolve_fuzzy`：非测试的精确匹配照旧优先；没有时，如果测试代码里有精确匹配，返回 `NotFound`（评审第 1 轮后；第一版返回 `Unique(输入本身)`，见"发版前评审"），不再看近似名。这正是 0.171.3 对没有近似名的测试专属名字走的路，各调用方的 `NotFound` 分支本来就处理测试代码：
  - MCP `get_call_graph`、CLI `callgraph`：有种子，回答这个名字自己的调用图（种子查询本来就不过滤测试）。
  - MCP `find_references`：不过滤再查一次，先分出 `<external>` 导入哨兵（有就按导入回答），其余给"全部定义都在测试路径"提示。
- MCP `get_call_graph`：精确限定名（`selectable_qualified_definitions` 非空，和 CLI `select_cli_symbol` 同一个判断）跳过模糊步骤。

不选的：

- MCP `get_call_graph` 改成"有种子就跳过模糊步骤"：结果相同（种子的名字分支和 `resolve_fuzzy` 的精确候选是同一批行，只差 `<module>` 这种名字），但要删掉几个分支，改动面更大，也不再和 CLI 用同一个判断。
- `find_references` 对测试专属名字直接回答（CLI `refs` 就这样）：会把这类输入从拒绝变成回答，范围是所有测试专属名字（flask BASE 上 709 个名字给出这个提示），不只是有近似名的那些。是另一个决定，记入遗留。

## 输入形态表

性质：HEAD 的回答 == BASE 的回答，唯一例外是 BASE 换了名字或给了近似名列表、而输入有精确定义的那些；它们在 HEAD 上按输入本身回答（`find_references`：测试路径提示，同名有 `<external>` 导入时按导入回答）。HEAD 上有精确定义的输入"换了"和"近似名列表"都是 0。

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
  - `resolve::fuzzy_tests::a_name_defined_only_in_test_code_resolves_to_itself`：得到 `after_request`，期望 `after`（评审第 1 轮后改名为 `a_name_defined_only_in_test_code_is_not_switched`，期望改为 `NotFound`）。
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
- 已提交树复跑：`git archive 6efd11be` 构建（默认特性，md5 `ce672edd…`），同一批 101,608 次运行，与上面工作树构建的 HEAD 逐字节相同 101,608/101,608，所以上表数字也是 `6efd11be` 的。截断判断在这两个语料上不触发。
- 全量：`b2706adc` 的 pre-commit 31 个套件 2,290 通过、0 失败、7 忽略，fmt、clippy 两条腿通过；`6efd11be` 的 pre-commit 2,291 通过、0 失败、7 忽略，fmt、clippy 通过，embed-model 的 clippy 腿手动跑过，通过。

## 决定

- `resolve_fuzzy` 对测试专属的精确名返回 `NotFound`，同名的测试定义有几个都一样。`get_call_graph`/`callgraph` 有种子时 `NotFound` 落到回答种子本身；`find_references` 的 `NotFound` 分支先分出 `<external>`，再给测试路径提示。
- MCP `get_call_graph` 用 CLI 同一个判断 `selectable_qualified_definitions(...)` 非空来认"精确限定名"，所以两边对同一输入做同一个决定。
- `file_path` 分支不会出现"把 X 解析成 X"的说明：`file_selector_miss` 不过滤测试文件，测试专属名字在别的文件里有定义时，模糊步骤之前就已拒绝。
- 截断（同名定义超过 10 个、模糊查询返回的 10 行全是测试代码）：`6efd11be` 曾用 `preferred_definitions` 为空才认"只在测试代码里定义"，防止 `Unique` 把两个生产定义合并回答。改成返回 `NotFound` 后，截断时两条路都得到 `NotFound`（10 行全是精确名，非测试候选为空），这个判断不再改变任何回答，已删除。测试 `a_name_production_defines_beyond_the_fuzzy_rows_is_not_test_only` 保留，钉住"不会是 `Unique`"（把规则改回 `Unique` 的变异会被它抓到）。这种情况下的回答与 0.171.3 相同（`find_references` 的假"全部在测试路径"，见遗留）。

## 遗留

- MCP `find_references` 拒绝只在测试文件里定义的名字（让调用方传 `node_id` 或 `file_path`），CLI `refs` 直接回答：flask 749 个名字、leveldb 33 个，CLI `refs` 全部回答。0.171.3 就是这样（flask 709 个），本次把有近似名的那 40 + 18 个也归到这一类。`include_tests` 默认 true 时这种拒绝是否合理，需要单独决定。
- 这句提示里的路径没有去重：leveldb `AddBoundaryInputsTest` 列出 `db/version_set_test.cc, db/version_set_test.cc`。0.171.3 对没有近似名的测试专属名字就这样。
- 带点名字没有任何定义时 MCP 不退回裸名：D#435。
- 0.171.3 就有：`resolve_fuzzy` 的精确名判断建立在 `LIMIT 10` 的模糊查询上。`find_references` 对一个有歧义的名字最多报 10 个（leveldb `Append`：`find_references` 说 10 个，`get_call_graph` 说 12 个；leveldb 有 18 个名字正好报 10 个）。同名定义超过 10 个、而前 10 行全是测试代码时，`find_references` 会说"全部在测试路径"，但其实有生产定义。改法是让精确名判断用全量列表，会改变这些名字的歧义回答，单独做。

## 发版前评审

### 第 1 轮（新开子代理，`2171df30..7d2ff671`）

3 个 MEDIUM、3 个 LOW。逐条核对后：

- F1 MEDIUM（已复现，是本次改动带进来的）：规则返回 `Unique(名字)`，`find_references` 的 `Unique` 分支只按名字规则划分测试代码，把同名的 `<external>` 导入哨兵也算成测试。`import test_support` + 测试里的 `def test_support()`：0.171.3 答 `imports: 1`，`7d2ff671` 拒绝并把 `<external>` 叫作测试路径。修复 `e195400a`：返回 `NotFound`，删掉 `preferred_definitions` 判断（见决定）。RED：MCP 测试新加的 `test_support` 断言在 `6efd11be` 上失败（就是评审的复现文字）。
- F2 MEDIUM（已复现）：变异 `!test_code.is_empty()` 跑完整套测试仍是绿的（2,291 通过）。原因是没有测试让测试代码的名字出现在拼错输入的近似行里。补了 `afte`（近似行里有测试的 `after`），单元测试和 MCP 测试各一处。变异结果（变异副本在 `/var/tmp/d440/src-mut`，不碰工作树）：`!test_code.is_empty()` 2 个红，改回 `Unique(name)` 3 个红，删掉规则 3 个红，未变异对照全绿。
- F3 MEDIUM：CHANGELOG 把这一类写成"只在测试文件里定义的名字"，不准确。核对：flask 749 个里 3 个是 `src/flask/app.py` 里的生产方法（`test_client`、`test_request_context`、`test_cli_runner`，0.171.3 就被拒绝）；leveldb 33 个都在 `*_test.cc` 里，路径规则不认，名字规则 `*Test` 认。另外，名字规则判为测试的生产名字如果有近似名，0.171.3 答近似名，现在被拒绝，提示还把生产文件说成测试路径（评审的 fx1：`test_mode`、`LoadTest`、`ParserTest`，`e195400a` 上复现）。CHANGELOG 已改写。
- F4 LOW（已复现，0.171.3 就有）：MCP `get_call_graph` 对两个测试代码定义共用的限定名合并回答，其他三个面按歧义拒绝。flask 9 个限定名在 BASE 和 HEAD 上一样；没有边时 0.171.3 答近似名，现在合并回答（fx2 `Index.get`）。写进 Not covered，不在本轮修。
- F5 LOW（已复现，0.171.3 就有）：同名定义超过 10 个时 `find_references` 的假拒绝；写进 Not covered（D#445）。
- F6 LOW：生成器每个名字只带一个定义它的文件（flask 121 个、leveldb 168 个名字定义在多个文件里）。CHANGELOG 改成"各带一个定义它的文件"。
- 另：评审观察到 `test_cli_a_build_whose_server_holds_the_index_lock_is_not_a_dead_writer` 在机器繁忙时失败 1 次，单独重跑 3 次通过，与本次改动无关。

修复后复测（`git archive e195400a` 构建，md5 `9d672730…`）：两个语料 101,608 次运行与 `6efd11be` 逐字节相同（CHANGELOG 的语料数字对 `e195400a` 成立）；评审的 fx1 480 次与评审的变异 M4 完全相同，与 `7d2ff671` 只差 `test_support` 的 2 次（回到 0.171.3 的回答）；fx2 与 `7d2ff671` 相同。`e195400a` 的 pre-commit：31 个套件 2,291 通过、0 失败、7 忽略，fmt、clippy 两条腿通过。

### 第 2 轮（差量，新开子代理，`7d2ff671..7f0e20e0`）

0 个 HIGH。评审确认：与 `7d2ff671` 相比，语料 101,608/101,608 相同，7 个夹具里唯一的差别是 `test_support`（回到 0.171.3）；与 0.171.3 相比，改变的 660 次全是 D#440 的输入；逐个追踪四个调用方对 `NotFound` 的处理，都是对的（CLI `refs` 那一支到不了）。

- F1–F3 MEDIUM（已复现）：三个变异能通过整套测试，并把语料上的修复改回去：`any` 改成 `!空 && all`（flask 345 次回退），条件加上"恰好一个近似名"（flask 203 次），规则移到非测试精确匹配之前（`find_references` 拒绝 flask `Config` 这种生产和测试都有定义的名字）。F4 LOW：大小写不敏感的变异也存活（语料上 0 次变化）。处理（`test(resolve)` 提交，只改测试和注释，不改行为）：单元测试加上测试代码里的近似名 `after_hook`、两个生产近似名的 `before`、生产和测试都定义的 `setup`、近似输入 `After`。变异复跑（副本 `/var/tmp/d440/src-mut`）：这四个各在自己那条断言上失败（after / before / setup / After），第 1 轮的三个仍分别有 2、3、3 个测试失败，未变异的对照通过。
- F5 LOW（已复现）：同名有 `<external>` 导入时，`find_references` 按导入回答，不是测试路径提示。CHANGELOG 已补。
- F6 LOW（已复现）：CHANGELOG 写的是 `Flask.test_client`，被拒绝的其实是裸名 `test_client`；限定名能回答（45 个引用，`e195400a` 上复现）。已改。
- F7 LOW（已复现）：测试代码多处定义的名字，`get_call_graph`/`callgraph` 会合并回答（flask 改变的 40 个名字里有 13 个，SQL 复核）；0.171.3 在有边的方向上也是合并。CHANGELOG 已补。
- F8 LOW：已写明。
- F9 LOW：MCP 一侧 `NotFound` 的注释仍是 "No candidates found."，已改，`resolve.rs` 的注释写清各调用方怎么处理。

评审深度到 2 轮的上限。第 2 轮没有发现行为缺陷，修复只补测试和文字，用变异结果收口，不开第 3 轮（先例：0.171.1）。
