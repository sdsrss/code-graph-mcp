# TS 类型调用边降级 + MCP 监听晚启动补扫（2026-10-20，0.174.0 之后）

状态：D#370(d) 完成（`6654ee1f` + `87dffb58` 的索引侧修复，INDEX_VERSION 118，未发版）；D#436(5) 两轮评审后撤回（`54624b41`），重做记为 D#505。

## 背景与目标

用户要求"继续剩余的工作，先深入思考给出科学准确的建议，再按建议执行"。上一份汇报的批 A–D 排序重新核对后，本会话做两项：

1. **D#370(d) 重做**：TS 里指向 `interface` / `type` 节点的 `calls` 边标为 `ambiguous`（默认层隐藏），不删边。0.174.0 前的删边做法（批 4b）因增量路径问题（评审 F3）撤回；本做法在 `classify_edge_confidence` 里改标签，边保留，增量的入边恢复照常工作。
2. **D#436(5)**：MCP 文件监听在第一次索引类工具调用时才启动（`consume_startup_index_result` → `start_post_index_services`），启动扫描之后、监听启动之前的改动没有事件；补一次增量扫描，放在后台 embedding 启动之前（免得增量等 embedding 让路，最长 20 s）。

不做：worktree 披露（D#371(7)/D#379(1)）、批 B、批 C、D#476(a)、D#370(f)。不发版：INDEX_VERSION 升级等更多 D#370 子项一起发，用户只重建一次。

## 排序依据（实测 / 读码）

- D#370(d)：7 个语料（`/var/tmp/d446/corpora`，0.174.0 索引）里，hono 默认层（`inferred`）调用边 975 条，其中 82 条指向 TS `interface`/`type` 节点（8.4%），全部跨文件；`Response` 69（`new Response()` → hono 的 `interface Response`）、`Set` 10、`Router` 1、`Component` 1（另有同名 function）、`Event` 1。其余 6 个语料 0 条（gin 有 7 条指向 Go interface，不在本项范围）。类型声明在运行时不存在，这类边不可能是对的调用目标。
- D#436(5)：读码 `src/mcp/server/mod.rs`：`last_incremental_check` 初始化为"构造时刻 − 60 s"，有监听时的兜底间隔 `WATCHER_BACKSTOP_SECS = 300`，所以兜底在进程存活约 240 s 后才触发。影响面 = 启动扫描之后、首次工具调用之前的改动，且只在进程前约 4 分钟内看不到（上次汇报写"最长 300 秒"偏大）。
- worktree：读码 `project-root.js` / `cli/paths.rs`：在 worktree 里启动的会话由 MCP 服务器建自己的索引，回退只发生在建索引期间；永久回退的是 EnterWorktree 和 worktree 子代理（MCP 服务器根在 main）。要先核实 Claude Code 给这些 hook 的 cwd，涉及 9 个注入点，留给后续会话。
- D#290（npm trusted publishing）：`NPM_TOKEN` secret 更新于 2026-08-22；npm 自 2025-10/11 起写权限 token 最长 90 天，所以这个 token 最晚约 2026-11-20 失效（推算，token 的实际到期日只能在 npmjs.com 看）。CI 侧已就绪（Node 24、`id-token: write`、OIDC 优先、token 兜底）；需要维护者执行 `npm trust github <pkg> --file release.yml --repo sdsrss/code-graph-mcp --allow-publish`（6 个包）。0.174.0 的 `_npmUser` 是维护者账号（token 发布）。

## 设计

### D#370(d)

- `resolve::classify_edge_confidence` 的 `CONF_CASE` 加一个分支：`calls` 边、目标节点类型是 `interface` 或 `type`、目标文件语言是 `typescript` 或 `tsx` → `ambiguous`。`references` 边不动（类型被引用是正常的）。
- 作用域：沿用 `CONF_WHERE`（跨文件 + Rust/Python 的同文件类）。同文件的 TS 类型调用边是否存在先用夹具确认，再决定是否扩到同文件。
- INDEX_VERSION 117 → 118（已有索引里的标签要重算）。
- 验证：RED 夹具；增量 = 全量（评审的 tsA–tsG 场景移植成 Rust 测试 + `parity.py` 两臂）；全边差分（边集合不变，只有谓词命中的标签变）；7 个语料前后对比；SCIP oracle 在 hono 上"之前可见的正确边现在隐藏"计数。

### D#436(5)

- `consume_startup_index_result` 里监听是晚启动的：先启动监听，再跑一次增量（`run_incremental_with_cache_restore`），然后才启动 embedding 等后台服务。尊重 `may_write_index()`。
- 验证：RED 测试（启动索引完成 → 改文件 → 第一次 `ensure_indexed` 必须看到改动，兜底间隔保持测试默认的 3600 s）；大语料上的空增量耗时。

## 决策与进展

- 2026-10-20：选这两项的理由见上；分级 L2（修复错误的默认回答，不改契约）。
- 同文件也降级：夹具里 `type User = …` + 同文件 `User()` 原来是 `extracted`（值与类型同名、值没有节点时就会这样），与跨文件同理，一并标 `ambiguous`。hono 上没有这种边。
- 语言门：只对 `typescript`/`tsx`。Go 的类型转换 `HandlerFunc(f)` 也是指向 `type` 节点的调用边（gin 7 条），不在本项范围，作测试里的对照。Java 匿名类 `new Shape() {…}` 不产生调用边，不能当对照。
- D#436(5) 的其他启动监听路径：提升为主实例已经置 `pending_incremental`，不改；`initialized` 时"已索引"分支只在工具调用先于 `initialized` 时出现，间隔毫秒级，不改；监听启动失败走无监听的 30 s 去抖，不改。
- 提交：`6654ee1f`（D#370(d)，INDEX_VERSION 118）、`4724f4ac`（D#436(5)）、`87dffb58`（评审第 1 轮修复）、`54624b41`（撤回 D#436(5)）。
- 评审第 1 轮后改了 D#436(5) 的做法：原来监听晚启动时直接跑一次增量；增量要先等 embedding 回填在批次边界让路，而 embed-model 构建的启动线程自己就在跑回填，所以没有改动时第一次调用也要等一个批次（评审实测 +3.3 s、+6.6 s），等超时后同一次调用还会再跑一次（两次等待）。现在改为只读比较磁盘与索引（`index_differs_from_disk`，与增量共用扫描和差分，不写、不动目录缓存），有差异才置 `pending_incremental`，由 `ensure_indexed` 跑一次。`initialized` 时"已索引"分支（工具调用先于 `initialized` 时）同样处理；监听只启动一次。
- 有改动且回填在跑时，第一次调用仍要等一个批次（实测 6.0 s）。这是回填持有写路径、按批次让路（D1）的既有代价，每次监听触发的增量都一样；缩短批次另记 D#503(b)。
- 不做（记入 D#503）：F6 工具描述里对 `ambiguous` 的说明（LLM 可见元数据，L3）；Go 类型转换指向 type 节点的调用边。
- 2026-10-20 撤回 D#436(5)：评审第 2 轮在修复后的做法上又复现两个 MEDIUM（见验证记录）。两轮都在补扫设计上找到 MEDIUM，剩下的问题都伸进服务器既有的并发机制（兜底计时的含义、启动修复线程是让路协议之外的第二个写者、BUSY_SNAPSHOT、中断运行的升级），按"第 2 轮之后不再打补丁、第 3 次未过即上报"的规则撤回，`src/mcp/server/mod.rs`、`src/indexer/pipeline/mod.rs` 回到 0.174.0（`git diff 5e5cf754` 两个文件为空）。只读检查、6 个测试和探针留在 `87dffb58` 和 `tasks/ts-type-calls-and-late-watcher/`，重做方向记在 D#505。D#370(d) 两轮都通过，保留。

## 验证记录

### D#370(d)（`6654ee1f`）

- RED：`test_ts_call_into_an_interface_or_type_alias_is_ambiguous` 在 HEAD 的 `resolve.rs` 上失败：`Make`/`Response`/`Props` 为 `inferred`，同文件 `User` 为 `extracted`。GREEN 通过。去掉语言门的变异 → 失败（Go `HandlerFunc` 变成 `ambiguous`）。
- `test_ts_type_call_label_converges_incrementally_to_a_rebuild`（评审 tsA–tsF 的 6 个场景）改动前后都通过（守卫）。
- 全量：`cargo test --lib` 1,486 通过；pre-commit 全套 31 个套件 2,306 通过、0 失败、7 忽略；fmt、clippy 通过。
- 语料全边差分（`/var/tmp/ts118/tools/edgediff.py`，0.174.0 发布二进制 vs `6654ee1f` 源码构建，各自在语料副本上全量建索引）：7 个语料边集合（不含 confidence）全部相同；只有 hono 改标签，`calls` 82 条 `inferred → ambiguous`（目标 interface 80、type 2）；其余 6 个语料 0 条。建索引耗时两臂相同（0.1 s 精度）。
- 82 条是否都是错的：`Response` 是 `src/client/types.ts:103` 的模块内 `export interface Response extends ClientResponse<unknown> {}`，37 个调用文件里没有一个导入它（`grep` 导入语句 0 个），调用都是 `new Response(...)`（全局）；`Set` 是 `src/context.ts:101` 的 `interface Set<E>`。

### D#436(5)（`4724f4ac`）

- RED：`test_change_before_the_watcher_starts_is_seen_by_the_first_call`（兜底间隔设为生产值 300 s）在改动前失败：第一次调用后监听已启动，`late_fn` 不在索引里。GREEN 通过。
- 全量：`cargo test --lib` 1,487 通过；pre-commit 全套 31 个套件 2,307 通过、0 失败、7 忽略；clippy（含 embed-model）通过。
- 代价：3,661 个文件（hono 复制 10 份）上 CLI 空增量 141–189 ms（3 次，含进程启动）；服务器补扫用启动索引的目录缓存，这是上限。后台 embedding 在跑时，补扫要等一个批次（32 个节点）让路，最长 20 s 后跳过并留作 owed。（评审第 1 轮证明这条代价说低了，见下。）

### 评审第 1 轮（新开子代理，`5e5cf754..4724f4ac`）

0 HIGH、2 MEDIUM、7 LOW，我逐条对照代码核过。

- F1 MEDIUM：embed-model 构建里启动线程自己跑回填（mod.rs:1133），第一次调用的补扫要等它让路；10 份 hono、模型在场：第一次调用 0.174.0 13 ms → 4724f4ac 6,598 ms（一个 32 节点批次 6.8–10.3 s）。
- F2 MEDIUM：补扫等待超时后 owed 仍置位，`ensure_indexed` 同一次调用再跑一次，最坏 2 × 20 s。
- LOW：F3 64 个文件以内走 scoped 驱动，全局驱动里的规则没有测试钉住；F4 几个变异存活（`may_write_index` 守卫等）；F5 `surprising` 的理由文字对单候选的 TS 类型调用不实；F6 工具描述对 `ambiguous` 的定义（L3）；F7 监听启动失败时启动两次；F8 两处注释说多了；F9 `initialized` 时"已索引"分支同样没有补扫。
- 核实的作者说法：82 条标签、边集合相同（评审独立复现，二进制 md5 与我的相同）；82 条都不是正确的调用目标；6 个增量场景在被撤回的删边版本 a4f8890f 上有 4 个失败。

### 修复（`87dffb58`）

- F1/F2/F7/F9：见"决策与进展"。RED（在 4724f4ac 上）：没有改动时第一次调用等了 4.0 s（测试里让路上限 2 s，等了两次）；有改动时 4.0 s；非工具消息之后、`initialized` 之前的改动都没有置 owed。GREEN 5/5。变异：去掉 `<external>` 排除 → 只读检查测试失败；不检查就置 owed → "无改动不等待"测试失败；从不置 owed → 4 个测试失败；`initialized` 分支用旧函数 → 对应测试失败。（第一次写的两个变异因 `deny(warnings)` 编译不过，改写后才真正跑到测试。）
- F3：标签测试再跑一次 78 个文件（超过 `SCOPED_POST_PASS_MAX_FILES`）；只从全局驱动去掉规则的变异原来存活，现在失败。F5：理由文字改为 "ambiguous: bare-name target not decided (several candidates, or a TypeScript type)"。F8：两处注释改正。
- 全量：`cargo test --lib` 1,492 通过；pre-commit 全套 31 个套件 2,312 通过、0 失败、7 忽略；clippy（默认、embed-model）通过；指纹重新生成（索引输出不变，仍是 118）。
- 实测（`/var/tmp/ts118/tools/mcp_probe.py`，3,660 个文件，模型在场，各 1 次）：

  | 二进制 | 有改动：第一次调用 | 改动已索引 | 无改动：第一次调用 |
  |---|---|---|---|
  | 0.174.0 | 12 ms | 否 | 12 ms |
  | 4724f4ac | 5,648 ms | 是 | 5,628 ms |
  | 87dffb58 | 6,005 ms | 是 | 41 ms |

  87dffb58 有改动时日志是一个批次（6.6 s）、一次让路、一次增量（0.2 s）；无改动时没有增量也没有让路。

### 评审第 2 轮（新开子代理，只审 `87dffb58`）

0 HIGH、2 MEDIUM、5 LOW。评审确认：只读检查与增量的扫描和差分一致（`<external>`、删除、目录 mtime 不变的改动、D#136 都对得上）；没有路径在一次调用里跑两次增量；全量和 4 处改动后的增量索引输出与 4724f4ac 逐字节相同（转储 sha256 相同）；新的 4 个服务器测试在 4724f4ac 上都失败；锁顺序成立。

- F1 MEDIUM：无改动的检查不重置 `last_incremental_check`，进程起来超过 240 s 后的第一次调用仍走兜底增量、等回填让路（87dffb58 2,733 ms，0.174.0 2,404 ms；提交说明"没有改动就不等"只在 240 s 内成立）。
- F2 MEDIUM：owed 增量现在在 `spawn_startup_repair` 之后跑，启动修复线程是让路协议之外的写者；12 线程 × 50 次压力里 8/600、13/600 次增量中途 `SQLITE_BUSY_SNAPSHOT`（517）中止，留下 in-flight 标记，下一次调用全量重建（embed-model 还要全量重新 embedding）；关掉修复线程或用 4724f4ac 的顺序都是 0/600。`test_catch_up_after_a_non_tool_message_waits_for_the_next_call` 在 12 路并行下 4/180、3/180 失败。
- LOW：F3 有改动但没有回填在跑时，第一次调用新起的回填加载模型后才让路（+约 100 ms）；F4 检查看不到中断运行的升级和别的二进制的解析错误判定；F5 没有目录缓存时检查要哈希整棵树，且扔掉扫描缓存；F6 只读模式下也扫描并置一个永远不清的 owed；F7 两处性质没有测试钉住（检查必须在监听启动之后；检查出错算作"有差异"）。
- 作者说法：耗时形状相同、数值更大（无改动 57/86 ms，0.174.0 22 ms，4724f4ac 10,878 ms，有改动 8,400 ms）。

### 撤回（`54624b41`）

- 两个文件恢复为 `6654ee1f` 的版本（即 0.174.0 的这两个文件），去掉只读检查的单元测试；指纹重新生成。
- pre-commit 全套 31 个套件 2,306 通过、0 失败、7 忽略（与 `6654ee1f` 之后的数目相同）；clippy（默认、embed-model）通过。
