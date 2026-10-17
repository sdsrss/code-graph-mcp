# D#431：模糊匹配换了符号要说出来

状态：进行中（2026-10-17）。级别 L2（跨 `src/mcp`、`src/cli`；`fix:`，返回结果只新增 `note` 字段）。来源：D#431；0.171.3 待发版本里"填充写法由拒绝变错 16 次"的根因。

## 问题

精确查找落空后，MCP `get_call_graph` 和 `find_references` 进入模糊步骤；找到唯一一个近似名时，按那个名字回答，不说换了符号。CLI `callgraph` 在同一情况打印 `Resolved 'X' → 'Y'`；CLI `refs` 也不说。

## 先测（改动前，HEAD ef4dd425 带 embed-model 构建，`/var/tmp/d426/bin/head-embed`）

生成的输入（`gen.py`，seed 431）：flask 164 个、leveldb 165 个，分 13 类（精确名、精确限定名及其 `::` 写法、`类.别处的方法`、成员不存在、成员拼错、裸名拼错、前缀、不存在、带 `file_path` 的三类）。每个输入跑 CLI `callgraph`/`refs --json` 和 MCP `get_call_graph`/`find_references`。

"换了符号"= 回答的名字不是输入（`::` 规范化后）且不是精确限定名命中时的裸名。

| 语料 | 面 | 换了符号 | 其中不说 | 换对的（回答的裸名 == 本意） |
|---|---|---|---|---|
| flask | MCP get_call_graph | 31 | 31 | 31 |
| flask | MCP find_references | 22 | 22 | 22 |
| flask | CLI refs | 61 | 25 | 61 |
| flask | CLI callgraph | 70 | 0 | 70 |
| leveldb | MCP get_call_graph | 24 | 24 | 24 |
| leveldb | MCP find_references | 17 | 17 | 17 |
| leveldb | CLI refs | 51 | 19 | 51 |
| leveldb | CLI callgraph | 58 | 0 | 58 |

拼错和前缀这两类里，模糊步骤的唯一命中在样本里全部换对。换错的是另一种：整个带点名字参与编辑距离（`Flask.route` → `FlaskGroup`）。为量出这一种，另生成每个语料 600 个"没有任何定义叫这个名字"的带点输入（400 个 `类.方法` 组合、200 个成员拼错），只跑 MCP `get_call_graph`：

| 语料 | 模糊步骤给出回答 | 回答的裸名 == 本意 | 不是本意 | 拒绝 |
|---|---|---|---|---|
| flask | 9 / 600 | 4 | 5 | 591 |
| leveldb | 6 / 600 | 5 | 1 | 594 |

换对的都是类名只有 1~3 个字符（`X.register_template_test`、`DB.BloomHash`）：编辑距离正好吸收掉 `X.`。

## 设计

选 D#431 列的 (a)：保留回答，加一句说明。

- MCP `get_call_graph`、`find_references`：模糊步骤唯一命中、且命中的名字不是输入时，回答里加 `note`：`No exact match for '<输入>'; answering for '<命中>'.`（沿用这两个工具模糊分支已有的 `No exact match for` 说法）。
- CLI `refs`：同一情况在 stderr 打印 `Resolved '<输入>' → '<命中>'`，和 `callgraph` 一样；stdout 不变。

不选的：

- (c) 拒绝模糊命中：拼错和前缀的命中在样本里 100% 换对，拒绝会让这些输入多一轮往返；而且这改变已发布工具的默认行为。
- (b) 带点名字退回裸名（CLI 的做法）：0.152.0 的提交说明和 CHANGELOG 写了三个 MCP 工具都会退回裸名，但 0.152.0 的 MCP 代码里没有这段，至今三个工具都拒绝。补上它会改变约 95% 的"没有定义叫这个名字"的带点输入的回答（拒绝 → 回答），涉及 `get_ast_node` 在内三个工具和文件路径、尾部点号等写法，要先列形态表，单独做（D#435）。
- 只对带点名字跳过模糊步骤：上面 1200 个带点输入里它会拒掉 9 个换对的、6 个换错的，净效果是负的。

## 输入形态表

性质：改动后的回答 == 改动前的回答，唯一例外是"换了符号"时 MCP 多一个 `note` 键、CLI `refs` 多一行 stderr。

| 形态 | 例子 | 改动前 MCP | 改动后 MCP |
|---|---|---|---|
| 精确名、精确限定名 | `route`、`Scaffold.route` | 按本身回答 | 逐字节不变 |
| 拼错、前缀，模糊唯一命中 | `abot`、`get_ve` | 按 `abort`、`get_version` 回答，不说 | 同样回答 + `note` |
| 同上加 `file_path`，命中定义在该文件 | `abot` + `helpers.py` | 同上，不说 | 同上 + `note` |
| 带点，整串模糊唯一命中 | `Flask.route` | 按 `FlaskGroup` 回答，不说 | 同样回答 + `note` |
| 模糊多个命中 | `rout` | 候选列表 | 不变 |
| 候选全是输入本身（标题、构造函数） | `Flask` | 按本身回答 | 不变 |
| 找不到 | `zzqx` | not found | 不变 |
| 带点，没有定义叫这个名字，模糊也没命中 | `Blueprint.get_load_dotenv` | not found | 不变（CLI 退回裸名，见 D#435） |

## 验证记录

- RED：`test_a_fuzzy_answer_says_which_symbol_it_answers_for`（MCP，3 个工具/参数 × 拼错、前缀、整串编辑距离的带点名字，加 `file_path`）修复前失败（`note` 为 null）；`test_cli_refs_says_which_symbol_a_fuzzy_answer_is_for` 修复前失败（stderr 为空）。修复后都通过。
- 差分，改动前 P = `head-embed`（md5 `2677ac96…`），改动后 H = `/var/tmp/d426/bin/d431`（`cargo build --release --features embed-model`，md5 `e2fe74a0…`），上面那批生成输入：

| 语料 | 运行次数 | 逐字节相同 | 只多了 `note`（MCP，都在换了符号的回答上） | 只多了 `Resolved` 一行（CLI refs） | 其他差异 |
|---|---|---|---|---|---|
| flask | 656 | 578 | 53 | 25 | 0 |
| leveldb | 660 | 600 | 41 | 19 | 0 |

  H 上四个面"换了符号却不说"都是 0（P：MCP 31/22、24/17，CLI refs 25/19）。
- 1200 个带点输入（只跑 MCP `get_call_graph`）：flask 593 个相同、7 个只多了 `note`；leveldb 596 个、4 个；其他差异 0。
- flask `Flask.route`、`Flask::route `、` Flask.route` 经 `get_call_graph`/`find_references`：仍答 `FlaskGroup`，带 `note: No exact match for 'Flask.route'; answering for 'FlaskGroup'.`。
- `cargo clippy --all-targets -- -D warnings`（默认、embed-model 两条）通过；`cargo fmt --check` 通过。

## 决定

- 说明文字放在顶层 `note` 键：`get_call_graph`、`find_references` 的回答原来没有顶层 `note`；服务器包装层只加 `freshness`、`ignored_arguments`、`clamped_arguments`。
- 同一批名字两端加空白（尾空格、首空格、`\t…\n`、尾 NBSP，`in-pad-*.jsonl`，HEAD = `d429` 构建 md5 `f0f75cec…`）：flask 2,624 次、leveldb 2,640 次，与不加空白的 HEAD 回答逐字节相同 5,264/5,264；与 0.171.2 不加空白的回答相比，相同 2,312 + 2,400，只多了 `note`/`Resolved` 312 + 240。0.171.2 与改动前 HEAD 在不加空白的 1,316 次上逐字节相同（0 处差异），所以上面 P→H 的数字也是 0.171.2→H 的。
- flask `Flask.route`/`Flask::route` × 4 种填充 × 2 个工具：0.171.2 拒绝 16/16，HEAD 答 `FlaskGroup` 带 `note` 16/16；不填充的 4 次，0.171.2 答 `FlaskGroup` 不带 `note`，HEAD 带。

## 遗留

- 带点名字没有任何定义时，MCP 三个工具不退回裸名（0.152.0 的说明写了会退回）：D#435。
