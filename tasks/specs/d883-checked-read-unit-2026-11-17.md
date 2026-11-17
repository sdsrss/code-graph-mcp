# 2026-11-17 D#883：只读连接的每次读取都落在查过 schema 的快照上（MCP 二级读者）

状态：**已随 0.196.0 发版（2026-11-17）**。提交 4f48c0c0、36fff7ef、9315eda4、3329e646、a0386127、a1694c01、610a4b9d，release 3394db38，tag v0.196.0 在 7a119fe7；main CI 44161026745 7/7 绿，Release 44164041373 10/10（36m17s）；npm 六个包 0.196.0 都由 GitHub Actions 经 trusted publishing 发布；GHSA-q67h-24gq-4263 正文已补 0.196.0。主进程、回填与 embedding 线程、晋升后的写连接、CLI 命令仍只在打开时检查（Not covered，D#883 延期条目保持打开）。仓库公开：**复现手法只在本地 gitignore 的 `docs/`（`docs/d784-r3-notes.md`、`docs/d877-r1-notes.md`）；本文件和测试都不写手法，也不写怎样让服务进入二级模式。**

## 背景

0.195.0（D#784）在打开索引时检查 schema，拒绝本工具不建的触发器、视图和虚表。MCP 服务的二级读者用一条只读连接读完整个会话，只在打开时查一次：之后这条连接的每个读事务都可能读到另一份 schema（R3 两种、D#877 R1 的 HIGH 两种，都实测过），SQLite 加载并执行它。D#877 的"只接受 WAL 模式"一稿已撤回（`tasks/specs/d877-secondary-reader-wal-2026-11-17.md`）。

## 核实过的事实（SQLite 3.54.1，`libsqlite3-sys` 0.39.1 内置；sqlite-vec 为仓库 vendored 版本）

| # | 事实 | 位置 |
|---|------|------|
| S1 | 语句遇到 NOMEM、IOERR、FULL（非只读语句还有 INTERRUPT）时，SQLite 回滚整个显式事务并回到 autocommit；之后的语句各自开新事务、读新快照 | sqlite3.c:90840–90869 |
| S2 | 上面的自动回滚会调用 rollback hook（判断时 `autoCommit` 仍为 0） | sqlite3.c:189907 |
| S3 | 加载 schema 期间不调用 authorizer | sqlite3.c:126744 |
| S4 | `sqlite3Select` 先做 `AuthCheck(SQLITE_SELECT)`，再展开表名、连接虚表；PRAGMA 同样先做 auth 检查 | sqlite3Select 函数内第 31 行 vs 第 66 行；sqlite3.c:145787 |
| S5 | `PRAGMA writable_schema=RESET` 丢弃本连接缓存的 schema，下次使用时重新加载（文档条款 R-60817-01178） | sqlite3.c:146498–146505 |
| S6 | `trusted_schema=OFF` 时，视图里用到非 innocuous 虚表、触发器对它做 DML 都在编译时报错；sqlite-vec 不调用 `sqlite3_vtab_config`，vec0 等都是默认风险；FTS5 标了 innocuous；xConnect 在这个检查之前运行 | sqlite3.c:155711–155734、133314；sqlite-vec.c 无 `vtab_config`；sqlite3.c:262294 |
| S7 | 本工具自己的触发器 `nodes_vectors_ad` 在触发器里对 vec0 做 DELETE | src/storage/schema.rs:564 |
| S8 | 主进程结束索引后的 TRUNCATE checkpoint 容忍读者推迟它 | src/storage/db.rs:1278–1280 |

## 决定

| 项 | 决定 | 依据 |
|----|------|------|
| 授权 | 用户 2026-11-17："继续完成剩余工作，先深入思考，给出科学准确的建议，然后按你的建议执行" | 改动在 storage、mcp/server 两个模块；不改 schema、依赖、CI；修的是 0.195.0 已公开的缺口 |
| 范围 | **只做 `Database::open_readonly` 打开的连接（MCP 二级读者，含晋升后仍用于读的那条）** | 实测过的四种变体都在这里；主进程、CLI 写入方、回填连接上的同类问题只是推导，继续写在 Not covered |
| 机制 | 每条消息里第一次取连接时：`PRAGMA writable_schema=RESET` → `BEGIN` → 在这个事务里查 schema（与打开时同一检查）；消息处理完（或晋升服务要写之前）`COMMIT` | 检查和读取同一个快照；RESET 让本次的 schema 缓存一定来自本次查过的快照（S5），不依赖"cookie 不变就不重载" |
| 兜底 | authorizer：只在"已查过的事务里"或"本工具自己的检查语句"时允许编译，否则拒绝；rollback hook 在事务被回滚时清掉"已查过"标记 | 自动回滚（S1、S2）后、或有代码绕过 `conn()` 时，新编译的语句都被拒；SELECT 在展开表名前就被拒（S4），植入的虚表连 xConnect 都到不了 |
| 被拒时 | 这条消息的工具调用报 D#784 的拒绝信息；下一条消息重新检查（不永久封死） | 每次都 RESET + 重查，缓存不会带着植入的 schema 进下一次 |
| `trusted_schema=OFF` | 不做 | 全局开会打断本工具自己的触发器（S7）；只对只读连接开的话被上面的检查覆盖，属于没有已知缺口时的额外防线 |
| 晋升后的读写 | `write_db()` 交出晋升后的写连接前结束读事务 | 晋升后写走另一条连接，读仍走只读连接；不结束的话本条消息里写完再读会读到旧快照 |
| 版本 | 随下一个版本发出；发版前独立评审 | — |

## 不覆盖（计划写进 CHANGELOG）

- 主进程、CLI 命令、回填线程的连接：它们跨多个事务时同样可能读到没查过的 schema（推导，未实测）。
- 已经编译好的语句在自动回滚后的新快照上执行：它的执行计划来自查过的 schema，读到的是新快照里本工具表的数据——属于 0.195.0 已写明的"伪造数据"一类。
- 只读连接在拒绝状态下编译 DML（本工具在二级模式不发 DML）：SQLite 在 auth 检查前查表，植入虚表的 xConnect 会先跑。

## 计划

1. RED（`storage::db` 单元测试）：只读连接打开后出现的植入视图，在下一次读取时被拒；事务被回滚后、新事务开始前编译的语句被拒；每次读取的事务之间能看到主进程的新写入。
2. 实现：`Database` 加只读守卫；`open_readonly` 安装；`McpServer::handle_message` 结束读事务；`write_db()` 晋升分支先结束读事务；工具调用被拒时报拒绝信息。
3. 全量测试、clippy、fmt；变异（独立 worktree + 独立 target）。
4. 端到端：发布版 0.195.0 对本次 release 构建，跑本地 `docs/` 里的四个复现脚本。
5. 每次工具调用的额外耗时（二级模式，中位数）。
6. 独立评审（盲审简报）。

## 实施中的修正

- **rollback hook 改为 authorizer 里查 autocommit**：第一版用 rollback hook 标记"事务已被回滚"，`storage::db` 里所有用 `open_readonly` 的测试都 SIGSEGV（连原有的 `test_open_readonly_rejects_writes`）。成因：rusqlite 0.41 把 hook 闭包存成 SQLite 的 client data，`sqlite3Close` 先释放 client data（sqlite3Close 内 `while( db->pDbData )`），之后 `sqlite3LeaveMutexAndCloseZombie` 才回滚仍开着的事务并调用 rollback hook（sqlite3.c:189770 一带）——调用的是已释放的闭包。读事务开着时 drop 连接就会触发。改为 authorizer 里调用 `sqlite3_get_autocommit`：事务被回滚后连接回到 autocommit，之后编译的语句都被拒；关闭时没有回调。
- **拒绝措辞**：第一版沿用打开时的 "refusing to open …"；会话中途服务并不在打开文件，改为 "refusing to read …"（打开时的拒绝不变），两个测试钉住开头。
- **未报告的拒绝随消息结束清掉**：预先推演的变异"结束读事务时不清 refusal"原有测试杀不掉（拒绝会漏到下一条消息的回答里）；`schema_that_arrives…` 加了一段"被拒但没人取走，下一条消息干净"。

## 评审 R1（新起的子代理，盲审简报，对 4f48c0c0）：HIGH 1 / MEDIUM 1 / LOW 5

| # | 结论 | 处理 |
|---|------|------|
| 1 HIGH（实测） | 延迟 BEGIN 不取快照；检查语句编译时 `sqlite3InitOne` 在自己开的读事务里解析 schema（sqlite3.c:148711–148718、148861），检查语句执行时才在更晚的快照上开本消息的事务；cookie 相同就不重载。评审：本构建 776/3000 次调用执行了植入视图（另一进程以相同 cookie 来回改 schema 行），父提交 0/3000（打开时解析一次、cookie 不变就不重载——正是 RESET 打开了这个窗口） | 修：BEGIN 后先读文件头（不需要 schema）打开快照，再检查。**第一次修法仍漏**：用 rusqlite 的 `pragma_query_value` 读 `user_version`，它先跑一条 `SELECT … FROM pragma_pragma_list`，编译时就在快照外解析了 schema（`trace_v2` 抓到的语句序列）；release 构建实测 793/3000、320/5000。改为直接 `query_row("PRAGMA user_version")`。回归测试改成在 BEGIN 之后守卫的每一条语句开始时各注入一次（第一次的测试只在检查语句处注入，没抓到上面的漏洞） |
| 2 MEDIUM（实测） | `main` 在每条消息之后调用 `run_startup_tasks`，二级分支里读索引，读事务一直开到下一条消息结束：另一进程的 TRUNCATE checkpoint 等满 busy timeout（WAL 1.6 MB 留着），第一次调用读到启动前的快照 | 修：`run_startup_tasks` 结束时结束读事务；服务层测试钉住 |
| 3 LOW（实测） | `sqlite_stat1` 定义里的虚拟生成列在每次解析 schema 时运行，现在每次调用都解析 | 不改代码：0.195.0 的 Not covered 已写生成列；CHANGELOG 写明现在每次调用都读 `sqlite_stat1` |
| 4 LOW（推导） | 二级进程里的 embedding 工作线程、晋升后的读写连接也是跨事务连接，CHANGELOG 没点名；`write_db()` 中途结束读事务会丢掉待报告的拒绝 | 修：拆成 `end_read`（保留拒绝）与 `end_message`（消息结束才清）；CHANGELOG 点名这些连接 |
| 5 LOW（推导） | 新版本主进程迁移 schema 后，旧版二级读者每次调用都报"删掉重建"，不报"请升级"、不出状态栏标记 | 修：每次检查先比 `user_version`；三处"schema 太新"共用 `schema_too_new`。这是本次改动引入的回归 |
| 6 LOW（推导） | `begin()` 若 unwind，状态停在允许一切的 CHECKING | 不改：`begin()` 里没有 panic 点（评审与我都查过），按"不为不会发生的情况加防御" |
| 7 LOW | 措辞超出代码（"重新解析"在 #1 下不成立；"一次调用一个快照"对晋升服务和启动不成立；提交说明说"不会在新快照上运行"） | CHANGELOG 改正；4f48c0c0 的提交说明不改写（未推送，但保留评审轨迹），以 CHANGELOG 为准 |

复审深度：R2 只审 R1 之后的增量；R2 再有 HIGH/MEDIUM 不打第三轮补丁，连同上下文交用户决定。

## 评审 R2（新起的子代理，只审 36fff7ef..9315eda4 与 CHANGELOG）：HIGH 1（增量之外）/ LOW 3

R1 的 #1、#2、#4、#5 在增量里确认关闭（评审独立重跑：4f48c0c0 竞态 475/2,000，9315eda4 0/5,000 快速、0/5,000 慢速；启动 checkpoint (1,400,0) → (0,0,0)）。

| # | 结论 | 处理 |
|---|------|------|
| F1 HIGH（实测，**0.195.0 起就在 D#784 的检查里，不在本次增量**） | 检查经查询优化器读 `sqlite_schema`；文件里一条 `CREATE INDEX … ON sqlite_master(…)`（以普通前缀开头，被放行）让优化器用它做覆盖索引，检查读到的是伪造者填的 b-tree，SQLite 却按表本身建 schema。仓库提交一个 `index.db` 即可，不需要竞态和写入方；0.195.0、本次构建的打开与每次调用、主进程、`health-check` 都执行了植入视图 | **作为范围扩展修了**（3329e646）：检查读 `NOT INDEXED`；`sqlite_` 表上的索引行一律算外来（SQLite 拒绝建这种索引，并在加载时核对 `type`、`tbl_name` 与 SQL 一致；`sqlite_stat1/4` 是表，第一版规则没限定 index 行，误拒了 7 个 snapshot 测试，已改）。理由：D#883 的保证建立在检查之上，F1 让会话中途的检查同样被绕过（评审实测），不修则本次 CHANGELOG 的说法不成立；不属于"第二轮还在修上一轮的修复"（停止线针对的情形）。测试的伪造索引覆盖检查读的全部列——第一版夹具只覆盖三列，加了 `tbl_name` 一列后优化器不再用它，"去掉 NOT INDEXED"的变异存活；改成覆盖四列后两条腿各自被杀 |
| F2 LOW（实测） | 每次调用先比版本、打开时先查外来对象，CHANGELOG 的"as an open does"不成立；植入的库可以把 `user_version` 设大，换成"请升级"的说法（仍被拒，不执行 SQL） | CHANGELOG 改正措辞；顺序不改（先比版本是为了不让新版本迁移过的索引被报成植入） |
| F3 LOW（推导） | Not covered 没点名 CLI 读命令（`health-check`、`show`、`grep`…），它们同样跨事务、打开时查一次 | CHANGELOG 补上，标题改为"connections other than a secondary's reads" |
| F4 LOW | "on the previous commit"指代不清（实为 6ac9b552） | 改为"built from the commit before this fix" |

F1 的修复没有经过独立评审（再起一轮即第三轮，按停止线交用户决定是否在发版前评审）。

## 评审 R3 与发版（2026-11-17，用户："按你的建议执行"）

- 决定：再起一轮盲审，范围只到没人审过的 20e1caea、3329e646 和整段 `## 0.196.0` 发版说明；审完作为 0.196.0（minor）发版。理由：3329e646 改的是 0.195.0 安全修复的核心检查，0.195.0 用户在新版本发出前一直暴露于 F1；发版和推送一起做，缩短手法公开而修复未发的窗口。
- 事先定下 R3 结论的处理（第三轮，不再打开放式补丁）：
  - 3329e646 或 20e1caea 相对 0.195.0 引入的回归（误拒合法索引、崩溃、措辞写错文件）→ 修，先写失败测试；修法不是只减不增或超出原提交的行 → 撤回该提交里出问题的部分。
  - 0.195.0 同样存在的另一种绕过检查的方式 → 本版不补，CHANGELOG Not covered 与 GHSA Not fixed 写明（不写手法），记延期条目；手法只进 gitignore 的 `docs/`。
  - 只涉及说法的 → 改说法。
  - 不属于以上的 → 停下问用户。
- R3 结果：子代理工作约 29 分钟后被 API 的安全策略（safeguards）终止，没有最终报告。只收到它的变异中间结果：M3（`type` 区分大小写比较）、M4（规则收窄为字面 `sqlite_master`）存活，lib 1649/1649 通过；M5（`&&`→`||`）被杀，lib 78 失败；M6（`of_users_file` 不去掉删除提示）只被 cli_snapshot 杀（1 失败）；M8 被杀；M7b 没跑完。问题 1、2 与 C1–C8 没有结论。
- M3/M4 是测试缺口：代码两处都用 `eq_ignore_ascii_case`，原测试只用小写 `sqlite_master`。补 `an_index_row_on_any_sqlite_table_is_refused_in_any_case`（`index`/`SQLITE_MASTER`、`INDEX`/`sqlite_master`、`index`/`sqlite_stat1`，不带植入视图），两个变异各自让它失败，代码不改。
- R3 的发布前检查另发现（我，不是评审）：`tool_text` 只被 `cfg(unix)` 测试调用，Windows 下 `warnings = "deny"` 会让测试构建失败；在 git archive 副本里把三个 unix 项翻成 `cfg(windows)` 复现（"function `tool_text` is never used"），a0386127 修。
- R4：按安全策略提示收窄范围另起一轮——只查回归、发版说明的说法与测试覆盖，不构造绕过检查的文件。R4 也被终止 → 评审在本环境无法运行，停下问用户。
- R4（新起的子代理，收窄简报，对 a1694c01；未被终止，完整报告）：HIGH 0 / MEDIUM 0 / LOW 2 / NOTE 3，没有回归。本机九个索引（INDEX_VERSION 71、92、124、139、140 写的，都有 `sqlite_stat1/4`、`sqlite_sequence`、autoindex、FTS5 与 vec0 影子表）在 0.195.0 与本版 `health-check --json` 整段 JSON 相同；HEAD 新建与 0.195.0 新建的索引、ANALYZE + `PRAGMA optimize` 后的副本都健康且计数相同；两个版本各做的 8 个快照 `snapshot inspect` 输出 md5 相同；二级服务三次调用两版都正常。变异 14 个被杀、3 个存活。

| # | 结论 | 处理 |
|---|------|------|
| F1 LOW（实测） | CHANGELOG "A refusal of `.code-graph/index.db` keeps the remedy" 对 `snapshot inspect .code-graph/index.db` 不成立：inspect 对任何文件都按用户文件措辞（WAL 索引走副本，`open_staged` 套 `of_users_file`） | 改说法：inspect 给任何文件（含 `.code-graph/index.db`）都这样措辞；把它当索引打开的命令（`health-check`、`show`、`incremental-index`、MCP 服务等，R4 实测）保留删除重建提示 |
| F2 LOW（变异 M14 存活） | 用户文件拒绝里的对象列表没有测试 | 610a4b9d：cli_snapshot 四条读法都断言 `view "planted"`；M14 现在失败（cli 1/14） |
| F3 NOTE（M16 存活） | "and N more" 截断无测试 | 不改：0.195.0 前就有的逻辑，20e1caea 只是挪进 `listed_objects` |
| F4 NOTE（M20 存活，4f48c0c0） | 二级每次调用的拒绝带删除提示，但没测试钉住 | 610a4b9d：二级测试断言提示（只比路径尾部，macOS 的临时目录经符号链接）；M20 现在失败 |
| F5 NOTE | CHANGELOG 的每次调用耗时是 3329e646 之前测的 | 重测（两边源码 release 构建：6ac9b552 对 a1694c01，本仓库 `src/` 副本 4,460 个节点，二级模式，`get_ast_node`，3 轮×100 次交替）：0.060/0.067/0.115 → 0.338/0.356/0.358 ms，每轮差 0.278/0.289/0.243 ms；CHANGELOG 换成这组数字 |

- 发版说明里"四种变化之后的调用都被拒""3,000 次 / 20,000 次竞态 0 次执行"是在 9315eda4 上测的，3329e646 之后没有重跑：3329e646 只增加拒绝（新规则）并把检查改读表本身，R4 用 `EXPLAIN QUERY PLAN` 看到改前改后都是 `SCAN sqlite_schema`，这些变体里没有建在 `sqlite_schema` 上的索引。所以这几条数字沿用 9315eda4 的实测结果，"本版同样成立"是推导，没有重测。
- 升级说明的实测：本机九个索引（`~/dev/*/.code-graph/index.db`）在 0.195.0 发布版和 3329e646 构建上 `health-check --json` 的 `healthy` 与文件数逐个相同（9/9）；系统 sqlite3 3.46.1 对 `sqlite_master`、`sqlite_schema`、`sqlite_stat1`、`sqlite_sequence` 建索引都报 "may not be indexed"。

## 验证记录

- RED（实现前，接口先写成空实现）：4 个 `storage::db` 新测试全部失败——植入视图执行了；同一消息里第二次读看到了别的连接的提交（left 1, right 0）；事务回滚后语句照样编译执行；**cookie 不变时 SQLite 用缓存里的植入 schema 执行了视图**（证明 RESET 是必需的，不是多余防线）。服务层新测试 `test_secondary_refuses_schema_that_arrives_after_it_opened` 失败于"第二次调用正常回答"（读事务没有在消息结束时关闭，读的还是植入前的快照）。GREEN：64/64（`storage::db` + `mcp::server::tests::test_secondary*`）。
- 全量（措辞修改前的源码；之后的改动只有拒绝措辞与测试断言，相关 64 个测试重跑通过，pre-commit 又跑了一次 lib 全量）：`cargo test --no-default-features --no-fail-fast` 2495 / 0 / 7（31 个套件；上一轮 2490 + 新增 5）；`--features embed-model` 2518 / 0 / 7；clippy 两种组合通过；fmt 通过。提交 4f48c0c0 的 pre-commit：lib 1644/1644 及其余套件、fmt、两条 clippy 通过。
- 端到端（scratchpad 重建本地复现环境，每个二进制用自己建的索引）：发布版 0.195.0 四种变体（R1 HIGH 两种、R3 两种）第一次调用正常，变化之后的调用都报 "malformed JSON"（植入视图执行）；最终构建（4f48c0c0 release，md5 bb459c5a）四种变化之后的调用都报 "refusing to read <index>: …"，第一次调用正常。
- 变异（独立 worktree + 独立 target，`/var/tmp/agent-tmp-d883/`，`cargo test --lib --no-default-features`，每个变异都确认编译通过且 1644/1645 个测试真的跑了）：
  - 第一轮（4f48c0c0）：去掉 RESET、外来对象也放行、不清未报告的拒绝、工具回答不报拒绝、晋升后写前不结束读事务——5 个被杀；检查出错仍放行（M9）、`resources/read` 不报拒绝（M10）存活；另 4 个因项目把 unused 当错误而没编译过，不算数。
  - 补测试（36fff7ef）：`a_check_that_fails_refuses_the_rest_of_the_message`（回滚日志模式下另一连接持 EXCLUSIVE 锁，检查 BUSY 失败，对方随后提交视图，同一消息里的读仍被拒）；服务层测试加 `resources/read` 断言。
  - 第二轮（36fff7ef，改写成能编译的形状）：authorizer 不看 autocommit、`end_read` 空操作、消息结束不结束读事务、M9、M10、不装守卫——6 个全被杀。合计 11/11。
- 耗时（二级模式，`get_ast_node`，两边都是源码 release 构建：6ac9b552 对 4f48c0c0 的早一版构建，差异只有措辞）：3 节点索引 5 轮×200 次中位数 0.040 → 0.160 ms；本仓库 `src/`（4,448 个节点）3 轮×100 次 0.064–0.114 → 0.339–0.343 ms。多出的是每条消息重新解析 schema、重新编译语句和检查本身。
- R1 修复之后（最终源码，release md5 963f13e8）：
  - RED：4 个新测试/改动先失败——注入测试 `Ok("ran")`；新版本 schema 报"delete … rebuild"；中途 `end_read` 丢了拒绝；启动后第一次调用 `files_count` 1（应为 2）。`pragma_query_value` 写法下加强后的注入测试失败于"statement 1 after BEGIN"。GREEN：68/68。
  - 竞态（数据库层探针，独立 worktree）：修复前 375–651/3000 次读到植入视图；修复后 20,000 次读、140,449 次切换，0 次。release（评审的脚本）：最终构建快速切换 0/3000、1 ms 切换 0/20,000；父提交对照 0/3000。
  - 启动（评审的脚本）：最终构建另一进程 checkpoint 立即 (0, 0, 0)、WAL 0 字节，第一次调用 `files_count` 2；4f48c0c0 为 (1, 400, 0)、1.6 MB、`files_count` 1。
  - 四种复现变体在最终构建上变化之后的调用都报 "refusing to read"。耗时 0.065–0.073 → 0.342–0.351 ms（3 轮×100 次）。
  - 全量（最终源码，不带 embed，由变异 worktree 的 pre-commit 跑出）：lib 1648/1648 及其余套件通过，fmt、两条 clippy 通过。
  - 变异第三轮（9315eda4 的源码）：`pragma_query_value` 写法、消息结束不清拒绝、`end_read` 丢拒绝、启动后不结束读、去掉版本检查——5/5 被杀。三轮合计 16/16。带 embed 全量 2522 / 0 / 7。
- R2 F1 修复之后（3329e646，release md5 bca7bc7e）：
  - RED：`an_index_on_the_schema_table_does_not_answer_the_check` 修复前失败于 "the index answered the check"（`reject_unowned_index` 返回 Ok）。GREEN：`storage::`、`snapshot::`、二级模式测试 254/254。变异：去掉 `NOT INDEXED`、去掉索引规则各被杀（夹具改为覆盖四列之后）。
  - 端到端（评审的伪造夹具）：0.195.0 与 9315eda4 的 `health-check --json` 都报 `files: 4242`、`reason` 为空，二级 MCP `get_index_status` 回答 `files_count 4242`；3329e646 的 `health-check` 报 `reason: refused`，二级服务在 initialize 前退出 1。
- 发版核验（2026-11-17）：main CI 44161026745 7/7（macOS 日志里 6 个新增或改动的测试都跑了，Windows 跑了不限 unix 的 4 个）；Release 44164041373 10/10，19:56:57→20:33:14Z；Release 非草稿、14 个资产，正文换成 CHANGELOG 0.196.0 段；npm 六个包 `_npmUser` 都是 GitHub Actions、`trustedPublisher.id` github；`npm install @sdsrs/code-graph@0.196.0` → `added 2 packages`，平台包自带二进制 `--version` 0.196.0，对 `sqlite_stat1` 上的索引行夹具 `health-check --json` 报 `reason: refused`、点名 `index "shadow_idx"`，对照索引健康（145 文件、4,460 节点）；二级模式 20 次 `get_ast_node` 正常（中位数 0.385 ms）。
