# D#435 / D#445 / D#447：MCP 按 CLI 的规则选符号

状态：进行中（2026-10-19）。级别 L2（`src/resolve.rs` 共用解析 + `src/mcp/server/tools` 三个工具 + `src/cli/symbols.rs`；`fix:`：D#435 恢复 0.152.0 发版说明写过的行为，D#445、D#447 让 MCP 和 CLI 对同一输入给同一个裁决）。D#446（测试代码专属名字由拒绝改为回答）不在本次范围，需要用户授权，见"遗留"。

## 问题

MCP 的 `get_call_graph`、`find_references`、`get_ast_node` 各自选符号；CLI `callgraph`/`refs`/`impact` 都走 `select_cli_symbol`（带点名字：精确限定名优先，多个就拒绝，没有就退回最后一段）加 `detect_ambiguity`（全量）。三处分歧：

1. **D#435**：带点名字没有任何定义（`Flask.route`，`route` 定义在 `Scaffold` 上），CLI 退回裸名 `route` 并说明；MCP `get_call_graph`/`find_references` 拿整个字符串跑模糊步骤（flask 答成 `FlaskGroup`），`get_ast_node` 说 not found。
2. **D#445**：MCP `find_references` 的精确名判断建立在 `LIMIT 10` 的模糊查询上：歧义数最多报 10（leveldb `Append` 报 10，`get_call_graph` 报 12）；同名定义超过 10 个、前 10 行全是测试代码时，说"全部在测试路径"，其实生产代码有定义。
3. **D#447**：两个测试代码定义共用一个限定名（flask `Index.dispatch_request`），MCP `get_call_graph` 合并回答；CLI 和 `find_references` 按歧义拒绝。

## 设计

把 `select_cli_symbol` 的选择逻辑移到 `resolve::select_symbol`（不打印），CLI 的 `select_cli_symbol` 调它再按原条件打印说明；三个 MCP 工具在没有 `node_id` 时先调它：

- `Err(Ambiguous)`：多个可选的限定名定义 → 歧义回答（D#447）。
- `Err(QualifiedNotFound)`：带点名字 + `file_path`，该文件没有这个限定名 → 文件未命中提示（与 CLI 相同）。
- `Ok(ExactQualified)`：照旧按限定名回答。
- `Ok(Bare)`：带点名字退回裸名时（D#435），之后的查找都用裸名，回答带 `note: Resolved '<输入>' → '<裸名>'.`（沿用 D#431 的说法，不对输入做断言）。

`find_references` 的裸名路径改成 CLI `refs` 的顺序（D#445）：`detect_ambiguity`（全量）→ 有非测试定义就按名字回答 → 只有测试代码或 `<external>` 定义 → 原来的 `NotFound` 分支（`<external>` 按导入回答，其余给测试路径提示，D#446 保持不变）→ 没有任何行叫这个名字才跑模糊步骤。

`get_ast_node`：`Ok(Bare)` 退回裸名时按裸名走原来的路径并带 `note`；其余输入照旧（只在测试代码里定义的名字仍说 not found，归 D#446）。

不选的：

- 只修 `resolve_fuzzy` 的截断（D#445 原记录的改法）：`find_references` 仍和 CLI 走两条路；改成 CLI 的顺序后，`resolve_fuzzy` 只在没有任何行叫这个名字时才跑（`find_references`、CLI `refs`），或在 `detect_ambiguity` 已经放行之后跑（`get_call_graph`、CLI `callgraph`），截断不再改变回答。
- 三项分开做：要搭三次语料、评审三轮、发三次版；合在一起后，CLI 对每个输入的回答就是 MCP 的参照答案。

## 输入形态表

性质：HEAD 的 MCP 裁决（回答 / 歧义 / 未找到 / 测试路径提示 / 近似名列表）和所答符号 == 同一输入的 CLI 裁决和所答符号；除下表"变"的几行外，HEAD 的 MCP 回答 == BASE 的 MCP 回答（逐字节）。

| 形态 | 例子 | BASE MCP | HEAD MCP | CLI |
|---|---|---|---|---|
| 裸名，一个生产定义 | `route` | 回答 | 不变 | 回答 |
| 裸名，多个生产定义，≤10 行 | `run` | 歧义 | 不变（候选取自 `detect_ambiguity`，见下） | 歧义 |
| 裸名，多个生产定义，>10 行 | leveldb `Append` | `find_references` 歧义，报 10 个 | 变：报全部 | 歧义，全部 |
| 裸名，>10 行，前 10 行全是测试代码，生产有定义 | 13 个 `run`，2 个在生产 | `find_references` 说全部在测试路径 | 变：歧义（或唯一时回答） | 同 HEAD |
| 裸名，只有测试代码定义 | flask `after` | `find_references` 测试路径提示；`get_ast_node` not found | 不变（D#446） | `refs` 回答 |
| 精确限定名，一个可选定义 | `Scaffold.route` | 回答 | 不变 | 回答 |
| 限定名，两个测试代码定义 | `Index.dispatch_request` | `get_call_graph` 合并回答 | 变：歧义 | 歧义 |
| 带点名字，没有定义，裸名有定义 | `Flask.route` | 整串模糊（`FlaskGroup`）或拒绝；`get_ast_node` not found | 变：按裸名回答 + `note` | 按裸名回答 + 说明 |
| 带点名字，没有定义，裸名也没有 | `Flask.rout` | 整串模糊或拒绝 | 待定（见决定） | 裸名再走模糊步骤 |
| 文件路径 | `app.py`、`db/db_impl.cc` | 未找到或整串模糊 | 待定（见决定） | 退回最后一段 `py` / `cc` |
| 带点 + `file_path`，该文件没有这个限定名 | `Flask.zap` + `app.py` | 文件未命中 | 不变 | 文件未命中 |
| 尾点、首点、双点、多段 | `Flask.`、`.route`、`Flask..route`、`flask.app.Flask.route` | 见测量 | 与 CLI 相同 | 见测量 |
| 标题 / gtest / bash 带点的名字 | `## Widget.run`、`TEST(Suite, Name)` | 见测量 | 与 CLI 相同 | 见测量 |
| 拼错、前缀（没有定义） | `afte_request` | 模糊 + `note` | 不变 | 同 |
| 以上加空白 | `Flask.route ` | 同不加空白 | 同不加空白 | 同 |

## 先测（改动前）

BASE = `29d3b812`（0.171.4 源码树，`cargo build --release`，默认特性），md5 `9da3e46f1cc70dd7c37c45bae180ca4f`。语料 `/var/tmp/d435/corpora`：flask `d086db8`、leveldb `bb74ef7`（与 D#440 相同）。生成器、驱动、比较脚本在 `/var/tmp/d435/harness`。

输入（`gen.py`，seed 435，两次生成逐字节相同）：flask 4,150、leveldb 6,232 个，分类：B 每个定义名（1,087 / 1,327）、Q 每个带点的限定名（386 / 1,167）、BF/QF 各带一个定义它的文件、D1 400 个 `类.成员` 组合（没有定义叫这个限定名）、D2 200 个成员拼错、D3 手选形态（文件路径、首尾点、双点、多段、`::`、gtest 名、20 个 D1 各 4 种填充）、DF 100 个 D1 带类所在文件、X 200 个拼错 + 200 个前缀。面：MCP `get_call_graph` 三个方向、`find_references`、`get_ast_node`（Q/D/X 另加 `include_impact`），CLI `callgraph --json` 三个方向、`refs --json`。每个版本 98,992 次（flask 22,726 MCP + 16,600 CLI，leveldb 34,738 + 24,928）。确定性：每个语料 300 个输入跑两遍 5,713/5,713 相同；CLI 8 并发 2,400/2,400 相同；输出没有时间类字段，不做归一化。

BASE 上 MCP 与 CLI 不一致（`diff.py oracle base`，每格是不一致的输入数，`get_call_graph` 每个方向相同）：

| 类 | flask `get_call_graph` | flask `find_references` | leveldb `get_call_graph` | leveldb `find_references` |
|---|---|---|---|---|
| D1（400） | 394 | 391 | 398 | 398 |
| D2（200） | 172 | 172 | 192 | 192 |
| D3 | 94 / 104 | 89 / 104 | 95 / 144 | 95 / 144 |
| DF（100） | 100 | 0 | 100 | 0 |
| D#445（>10 个同名行） | 0 / 10 | 5 / 10 | 0 / 13 | 13 / 13 |
| D#446（只有测试代码定义） | 0 / 749 | 749 / 749 | 0 / 37 | 33 / 37 |
| D#447（两个测试代码定义的限定名） | 9 / 9 | 0 / 9 | 无 | 无 |
| QF | 20 / 386 | 0 | 32 / 1,167 | 0 |

- D1：flask MCP 388 个 not found、6 个回答、6 个近似名列表；CLI 退回裸名回答 325、歧义 75。
- 整串模糊换错的例子（D3）：`Flask.route`→`FlaskGroup`、`flask.Flask`→`FlaskTask`、`App.route`→`AppGroup`、leveldb `.Write`→`DBTest.WriteSyncError`、`DBTest.Nope`→`DBTest.NoSpace`。
- D#445：语料里"全部在测试路径"的假拒绝 0 次；不一致的是歧义个数（leveldb `operator=` 10 对 43；27 个名字 MCP 多数了类旁边的构造函数，如 `VersionSet` 3 对 2）。
- 校准（B 中恰好一个非测试定义的名字）：`get_call_graph` flask 285/285、leveldb 1,027/1,028（`CacheTest.Erase`，gtest 节点与方法同名）；`find_references` flask 273/285，12 个差异都是 CLI `refs` 把测试代码里的同名定义也算作目标（CLI 的集合严格包含 MCP 的），leveldb 1,028/1,028。

## 决定

- 只在 MCP 里接入共用的选择，不改 CLI 的选择逻辑；CLI 的回答是参照答案。
- `get_call_graph` 带 `file_path` 时也走 `select_symbol`：同一文件里同一个限定名的两个定义（flask 20 个、leveldb 32 个，多是 getter/setter、构造函数重载）由合并回答改为歧义，带点名字在该文件没有定义时说"not found in file"（原来说"not found in the index"），与 CLI 和 `find_references` 相同。`find_references`、`get_ast_node` 带 `file_path` 的分支不变。
- 精确限定名跳过裸名的歧义检查（CLI 也跳过）：leveldb `CacheTest.Erase`（gtest `TEST` 与同文件的方法）不带文件时由拒绝改为回答方法，带文件时由回答改为歧义，两边都与 CLI 相同。
- 说明只写 `Resolved '<输入>' → '<回答的名字>'.`（D#431 的措辞），回答的名字与输入不同时一定带，相同时一定不带。
- 文件路径（`app.py` → 裸名 `py` 的近似名列表）：照 CLI，不另加规则。原来 MCP 说 not found，现在给扩展名的近似名列表，两者都是拒绝；写进 Not covered。
- `eba5db33` 的差分发现：C++ 嵌套类型（leveldb 存为 `Table::Rep`，成员 `Table::Rep.~Rep`）用点写时，退回裸名答成构造函数或别的类的构造函数（`Table.Rep` → `TableBuilder::Rep.Rep`），0.171.4 靠整串模糊碰巧答对 8/49；CLI 本来就这样。修在拼写层 `canonical_symbol_spelling`（CLI 和 MCP 都经过）：带点、没有任何节点叫这个名字时，换成精确限定名存在的 `::` 写法（先成员形式，再类型形式），不带说明（同一个符号）。成员形式与类型形式同时存在时取成员，两个语料里没有这种情况，交换顺序的变异存活。
- `find_references` 的说明改成选完之后统一算：`eba5db33` 在只有导入绑定裸名时（leveldb `a.b.c` → `<external>` `c`）不带说明。

## 验证记录

- RED：用 BASE 二进制在测试夹具（`/var/tmp/d435/fx1`）上逐项跑 MCP：`Widget.zap` 三个工具都是 not found；`Index.get` `get_call_graph` 合并回答、`get_ast_node` not found；`run_job` "all 13 match(es) are in test/bench paths"（有 2 个生产定义）；`append_row` "10 matches"（有 12 个）；`job_once` 测试路径提示（有 1 个生产定义）；`json` 答成 `json_dump`。`test_mcp_selects_the_symbol_the_cli_selects` 在改动前的代码上失败在第一处 `Widget.zap` 断言。
- 变异（`harness/mutate.py`，副本 `/var/tmp/d435/mut-src`，独立 target）：`eba5db33` 的 13 个修复点各改回一处，13/13 被测试抓到（最后一处，带 `file_path` 的 `QualifiedNotFound`，补了断言后才抓到）。`355892f3`：去掉 `::` 写法换算、MCP 分派只换算 `::`、导入分支不带说明，各自被测试抓到；成员形式与类型形式交换顺序存活（语料里没有两者并存）。
- 差分（`git archive` 构建，默认特性）：BASE md5 `9da3e46f…`，`eba5db33` md5 `eab15a67…`，`355892f3` md5 `9a3f2740…`。输入 flask 4,150、leveldb 6,371（另加 DC 类：leveldb 49 个存为 `::` 的节点 × 点写法 / 全 `::` / 原样，去重后 139 个），每个版本 100,382 次。
  - BASE → `355892f3`：flask 3,808 次改变、35,518 次相同；leveldb 5,003 次改变、56,053 次相同。CLI 只在 DC 和 `DBImpl.Writer` 上改变（364 次）。"答对的变成别的符号" 0；回答变拒绝 flask 32 个输入、leveldb 39 个（合并 → 歧义 29 / 33，整串模糊的错答 3 / 5，leveldb `EnvTest.Start` 是生成的组合，无本意可比）。
  - `eba5db33` 发现的回归：DC 的点写法 49 个里 7 个答成构造函数或别的类的构造函数、22 个歧义（BASE 靠模糊答对 8 个）；`355892f3` 上 DC 139/139 在三个 MCP 工具、CLI `callgraph`/`refs`/`show` 上都答对（`impact` 抽查 4 个）。`eba5db33` → `355892f3` 只改变 DC 900 次、`DBImpl.Writer` 10 次、`a.b.c` 的说明 1 次。
  - MCP 对 CLI（`355892f3`）：B/BF/Q/QF/DF/DC 上 `get_call_graph` 0 处不一致；剩下的不一致都是 D#446 的测试路径提示（flask 749 + 经点名退回 113、leveldb 33 + 37）、CLI `refs` 把测试代码的同名定义也算作目标（flask 24 个输入，CLI 的集合严格包含 MCP 的）、`Flask::` 的措辞，以及 `get_ast_node` 没有模糊步骤。
  - 加空白：80 个填充输入在 MCP 6 个面上 480/480、CLI 320/320 与不加空白的回答相同。
- 全量：`eba5db33`、`355892f3`、`fc29ef80` 的 pre-commit 都通过（2,292 / 2,294 / 2,294 个通过，0 失败，7 忽略；fmt、clippy 两条腿通过）。
- 不稳定测试（`fc29ef80`，只改测试）：根因是兄弟测试 fork 到 exec 之间持有锁 fd 的副本，`drop` 后锁还在。诊断代理：整套 `cli_e2e` 48 线程 + 24 个忙循环，60 轮失败 8 次 → 0 次；我复跑：未改 1/30 → 已改 0/30。`/tmp` 残留：doctor 测试经 `npm root -g` 留下 npm 的 `node-compile-cache`（73 个文件，456 KB），改后 0 个条目。负载下另有两个测试各失败 1 次（`test_cli_a_symbol_in_a_new_file_is_found_by_every_lookup`、`deps_refreshes_the_file_it_was_asked_about`），前者在未改动的 29d3b812 上也出现过，记入遗留。

## 遗留

- D#446：MCP `find_references` 拒绝只在测试代码里定义的名字，`get_ast_node` 对它们说 not found（假话），CLI `refs` 和 MCP `get_call_graph` 回答。改成回答是默认行为变化（L3），需要用户授权。本次之后带点名字退回裸名也会落到这里（flask D1 89 个）。
- 文件路径当符号名：退回扩展名，得到扩展名的近似名列表（CLI 原来就这样）。更好的回答是"这是文件，用 module_overview"，两个面一起做。
- CLI `refs` 对只有一个生产定义的名字，把测试代码里的同名定义也算作目标（flask 12 个名字，`invoke` 22 对 2）。
- CLI `refs` 在只有导入绑定裸名时不带说明（leveldb `a.b.c`）。
- 负载下的两个不稳定测试（见验证记录）。
