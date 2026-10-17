# D#426 (4)(7)：MCP impact 按节点取种子；符号参数去掉两端空白

状态：已实现并验证（2026-10-17），未发版。级别 L2（跨 `src/cli`、`src/mcp`、`src/resolve.rs`；两项都是 `fix:`，修的是答错的结果）。来源：D#426 第 4、7 项。

## 问题

### (4) 同一个节点，CLI 和 MCP 报的排除数量不同

`get_ast_node include_impact` 已经解析到一个 `node_id`，但 `append_impact_summary` 用"名字 + 文件"取种子，于是把同一文件里所有同名定义（cfg 双胞胎、C++ 重载）都算进来。`impact --node-id` 按节点取种子。

实测（leveldb，1152 个非测试函数/方法，`excluded_diff2.py` 节点模式，HEAD 1bdab5e6 的 release 二进制）：12 个节点不一致，全部是同文件同名的定义。

| 节点 | CLI 排除 | MCP 排除 | CLI 总调用方 | MCP 总调用方 |
|---|---|---|---|---|
| `Slice`（slice.h，5 个构造函数，各一行） | 104 | 520 | 0 | 0 |
| `InternalKey`（dbformat.h，2 个） | 9 | 18 | 0 | 0 |
| `ParsedInternalKey`（dbformat.h，2 个） | 11 | 13 | 1 | 1 |
| `Status`（status.h，3 个） | 76 | 76 | 198 | 196 |

最小复现：`src/a.rs` 里两个 cfg 双胞胎 `plat`，`src/b.rs` 一个 `plat`，`main` 裸调用 `plat()`。`impact --node-id 2` 报 1，MCP `get_ast_node {node_id:2, include_impact:true}` 报 2。

### (7) 带尾随空格的 `DBImpl::Write ` 在 MCP 下答成 `DBImpl::Writer`

符号参数两端的空白从不去掉。精确查找落空后，`get_call_graph` 和 `find_references` 进入模糊匹配，编辑距离回退找到唯一一个近似名 `DBImpl::Writer`（结构体），然后按它回答，exit 0。CLI 的 `callgraph / impact / refs / show` 和 MCP 的 `get_ast_node` 拒绝（exit 1 或报错），但也没答出 `DBImpl::Write`。

最小复现（C++）：`db.h` 声明 `class DBImpl { int Write(int); struct Writer; }`，`db.cc` 定义 `struct DBImpl::Writer` 和 `DBImpl::Write`，`Put` 调用 `d->Write(1)`。BASE 上 MCP `get_call_graph {"symbol_name":"DBImpl::Write "}` 答 `{"function":"DBImpl::Writer","callers":[]}`；`find_references` 同样答 `DBImpl::Writer`。

## 设计

(4)：`append_impact_summary` 改为接收种子（`CallGraphSeed`），每个入口传 `cmd_impact` 对同一请求用的那个：

- 显式 `node_id` → `Node(node_id)`（`impact --node-id`）；
- 只给名字（自动解析）和名字 + 文件 → 仍是名字 + 文件（`impact <name>`）。

第一版把所有入口都改成按节点取种子，leveldb 上按名字查 `Slice`（解析到类）从 624 掉到 104，而 CLI `impact Slice` 是 624（类和 5 个同名构造函数一起算）：修好一处，又造出一处 CLI/MCP 不一致。按名字查询并不总是拒绝同文件多定义：类和它的同名构造函数时，自动解析选类。

(7)：新增 `resolve::trim_symbol_arg`：去掉两端空白；去掉后什么都不剩时保持原样（这类输入各入口已经按原样拒绝，第 47、49 轮复审专门看过）。接入点：

- CLI：`callgraph / impact / refs / show / similar` 的 `symbol` 位置参数，用 clap `value_parser`，命令内所有用到 `args.symbol` 的地方都拿到去空白后的值；
- MCP：`dispatch_tool` 对 `symbol_name` / `function_name` 先去空白，再做已有的 `::` 规范化。

前提（已核实）：索引里没有名字以空白开头或结尾。leveldb、flask、本仓库共 11,548 个节点，`name` 和 `qualified_name` 两端带空白的都是 0；markdown 标题在解析时 `trim()`。

## 输入形态表

性质：HEAD(s) == BASE(trim(s))（trim 后非空），否则 HEAD(s) == BASE(s)。

| 形态 | 例子 | 预期 |
|---|---|---|
| 无空白 | `X`、`A::B` | 不变 |
| 两端空白 | `X `、` X`、`\tX\n`、`A::B ` | 等于 BASE 对 `X` / `A::B` 的回答 |
| 段内空白 | `A:: B`、`SkipList<Key, Comparator>::Insert` | 不变（第 47 轮的行为） |
| 只有空白 | `   `、`\t` | 不变 |
| `::` 加空白 | `:: `、`::\t`、`Flask:: ` | 等于 BASE(`::`) / BASE(`Flask::`)，仍被拒绝，不出现空名 |
| 空串 | `""` | 不变 |

## 验证记录

- (4) RED：`test_impact_summary_of_a_node_id_counts_that_node_only` 修复前失败（left 2, right 1），修复后通过。
- (4) `test_impact_summary_by_name_keeps_the_same_named_constructors`（C++ 类 + 2 个构造函数）：按名字 3、按 node_id 各 1。变异：把 `NameInFile` 分支改成按节点 → 该测试红；把 `Node` 分支改成名字 + 文件 → 两个新测试都红。
- (4) 差分，BASE = `/var/tmp/d426/bin/base`（md5 `4e287531…`，1bdab5e6 之后构建），HEAD = `fix4b`（md5 `369bf965…`）：

| 语料 | CLI vs MCP 节点模式不一致（BASE → HEAD） | MCP BASE vs HEAD，节点模式 | MCP BASE vs HEAD，名字模式 |
|---|---|---|---|
| leveldb（1152 个） | 12 → 0 | 12 个不同，正是那 12 个 | 0 |
| flask（392 个） | 13 → 0（嵌套的 `decorator`） | 13 个不同 | 3 个不同，只差"只读从实例"提示：两台服务器开在同一语料上，第二台是从实例。答案相同 |

- (7) RED：`test_symbol_args_answer_without_the_whitespace_around_them`（MCP，5 个工具/参数）修复前失败（`get_ast_node "DBImpl::Write "` 报 not found）；`test_cli_a_symbol_answers_without_the_whitespace_around_it`（CLI，5 个命令）修复前失败（`show` exit 1，提示 `DBImpl::Writer`）。修复后都通过；`trim_symbol_arg_drops_only_the_whitespace_around_a_name` 覆盖形态表。
- (7) 差分，HEAD = `fix7`（md5 `82140c5e…`）。名字集：每个语料随机 12 个（seed 426，带 `.` 的再加 `::` 写法）+ 手选 7~9 个 + 23 个敌意写法（`::`、`:: `、`Flask:: `、` ::Flask`、`A:: b`、空串、纯空白等）；5 种填充（无、尾空格、首空格、`\t…\n`、尾 NBSP）；10 个入口（CLI 5 个命令 `--json`，比较 exit、stdout、stderr；MCP 5 个工具/参数，比较整个 result）。MCP 两台服务器先后运行，不并发。

| 语料 | 运行次数 | HEAD(s) 与 BASE(trim(s)) 不同 | 对照：BASE(s) 与 BASE(trim(s)) 不同 | 有回答（BASE → HEAD） |
|---|---|---|---|---|
| leveldb | 2610 | 0 | 1877 | 568 → 1256 |
| flask | 2310 | 0 | 1742 | 326 → 937 |

- (7) 答成别的符号（MCP `get_call_graph` / `find_references`，填充过的输入，`wrongsym2.py`）：
  - 由错变对 16 次：leveldb `DBImpl::Write`/`DBImpl.Write` 加尾空格或 NBSP → `DBImpl::Writer`（8）；flask `about` → `abort`（6）、`route ` → `_method_route`（2）。
  - 由拒绝变错 16 次：flask `Flask.route` / `Flask::route` 的 4 种填充 × 2 个工具 → `FlaskGroup`。BASE 对不带空白的 `Flask.route` 本来就答 `FlaskGroup`，且不提示换了符号；CLI 对同一输入说 `No definition is named 'Flask.route'; answering for 'route'`。填充写法现在和不填充的一样，继承了这个已有缺陷。

- 同特性复跑：上面的 `fix4b`、`fix7` 编译时没带 `embed-model`，而 BASE 带了。用 `cargo build --release --features embed-model` 在 ef4dd425 重建 HEAD（`/var/tmp/d426/bin/head-embed`，md5 `2677ac96…`），所有数字原样复现：空白差分 0/2610、0/2310（有回答 1256、937），CLI vs MCP 0/1152、0/392，A/B 节点模式 12、13，名字模式 0、0，flask 填充 `Flask.route` 答成 `FlaskGroup` 仍是 16 次。

## 遗留

- MCP `get_call_graph` / `find_references` 的模糊步骤唯一命中时，按另一个名字回答且不提示（CLI 打印 `Resolved 'X' → 'Y'`，或对带点名字退回裸名并说明）。不带空白就能触发（flask `Flask.route` → `FlaskGroup`）。本任务不改，已立为 D#431。D#426 其余 6 项移到 D#432。

## 决定

- 只做 D#426 的 4 和 7（上次汇报的建议顺序），其余各项留在 D#426。
- (7) 选择"去空白后照常回答"而不是"拒绝"：两端空白不可能是名字的一部分（见前提），用户要的就是去掉空白后的那个符号。
- (7) 接受上面"由拒绝变错 16 次"：它们是不带空白的同名输入已有的错误回答，根因在模糊步骤，不在去空白。只在本 diff 里修这一类的唯一办法是"去空白后查不到精确定义就保持原样"，这是个只为保留拒绝而加的保守分支，而且不修不带空白的那一半。模糊步骤本身由 D#431 修。

## 发版前评审（0.171.3 第 1 轮）

- F4 LOW（平行路径）：`show --node-id N --impact` 仍按"名字 + 文件"取种子。修复 `27824988`：`--node-id` 时按节点取种子，按名字不变。RED：`test_cli_show_impact_of_a_node_id_counts_that_node_only`（同一文件两个 `fn new`）修复前 show 报 3、impact 报 1。全量对照（`showimpact.py`，每个非测试函数/方法，`show --node-id N --impact --json` 与 `impact --node-id N --json` 比直接调用方和总数）：0.171.2 不一致 leveldb 45/1152、flask 7/392；修复后 0/1152、0/392。按名字的 `show <name> --impact`：leveldb 随机 80 个名字，0.171.2 与修复后输出相同。
- 差量复审（第 2 轮）LOW（已复现）：上面的修复在"文件已在索引后改动"时比 0.171.2 差：`show --node-id N` 先用 `reresolve_node_by_identity` 重新找节点，它返回同一身份的第一个重载，按节点取种子后就数了那个重载的调用方（leveldb `Harness.ToString` 1696：4 个、MEDIUM；0.171.2 和 `impact --node-id` 是 3 个、LOW）。第 2 轮又在第 1 轮的修复里发现问题，按停止线撤回（`a10c3fc9` 回退 `27824988`），不再打第三个补丁；`show` 与 `impact --node-id` 的差异写进 Not covered，种子和重新定位一起移到 D#441。
