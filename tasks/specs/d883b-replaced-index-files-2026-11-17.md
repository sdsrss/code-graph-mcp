# 2026-11-17 D#883 剩余：索引文件在打开期间被替换后，本工具的连接不再读写它们

状态：**已发布 0.197.0（tag 在 400cda2b，2026-11-18，Release 10/10）**；剩余缺口转为延期条目 D#891（L3：跨 storage、mcp/server、cli 三个模块，且改变用户可见行为——文件被替换后服务改为拒绝）。授权：用户 2026-11-17“把剩余工作都做了，授权清理和发版”。仓库公开：**复现手法只在本地 gitignore 的 `docs/`（`docs/d883b-notes.md`、`docs/d883b-scripts/`），本文件和测试不写手法。**

## 背景

0.196.0 让 MCP 二级读者每条消息在同一个读事务里先查 schema 再读。主进程、CLI 命令、回填连接、晋升后的写连接仍只在打开时查一次。D#877 R1 的 HIGH 是这一类问题的触发方式：pull 在某个进程打开索引期间替换了 `.code-graph/index.db` 或 `index.db-wal`，而 `-shm` 是按路径共享的，于是旧文件上的进程和新文件上的进程用同一份 `-shm` 读写各自不同的文件。0.195.0 的 CHANGELOG Not covered 与 GHSA 的 Not fixed 已写明这一类。

## 实测（2026-11-17，发布版 0.196.0，md5 de89b792，scratchpad 沙箱）

| # | 情形 | 结果 |
|---|------|------|
| M1 | 旧文件上的写入方（替身进程）持续写入与 checkpoint，pull 替换 `-wal`，之后 CLI `health-check --json` 跑 100 次 | 3 次执行了植入的视图，89 次打开时被拒，4 次报 "views may not be indexed"（迁移语句编译时加载了植入的 schema） |
| M2 | 同上，`show helper_alpha` | 执行 4 次，打开时拒 71 次 |
| M3 | 同上但替换的是 `index.db` | `health-check` 执行 2 次 / 100；`show` 执行 1 次 / 100 |
| M4 | 真实主进程（`serve`），pull 带源码改动与替换文件，`health-check` 循环 15–20 秒 | 三轮共约 9,200 次，0 次执行；主进程只在工具调用或监视器触发时写入，这几轮里写入窗口为毫秒级，CLI 没碰上 |

结论：机制成立（M1–M3，CLI 是新打开的一方）；真实主进程下触发需要 CLI 的打开与后续语句恰好夹住主进程的一次写入（M4 未碰上，频率未知）。

## 核实过的事实（SQLite 3.54.1，`libsqlite3-sys` 0.39.1 内置）

| # | 事实 | 位置 |
|---|------|------|
| F1 | `SQLITE_FCNTL_HAS_MOVED` 比较打开时的 inode 与路径当前的 inode（路径不存在也算移动） | sqlite3.c:42277 `fileHasMoved`，44857 |
| F2 | 只有主库文件带 inode 信息；`-wal` 等非主库文件以 `UNIXFILE_NOLOCK` 打开，`pInode` 为空，对它们 `HAS_MOVED` 恒为 0 | sqlite3.c:47432–47433、46789 |
| F3 | commit hook 在 `vdbeCommit` 里、提交第一阶段之前调用；返回非 0 → `SQLITE_CONSTRAINT_COMMITHOOK`，事务回滚，不写 WAL 帧、不改 `-shm` | `vdbeCommit` 第 64–67 行 |
| F4 | 关闭时：主库文件已移动则跳过 checkpoint（`databaseIsUnmoved`）；`-wal` 移动不在此列。`SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` 让关闭既不 checkpoint 也不删除 `-wal`/`-shm` | sqlite3.c:63202，`sqlite3WalClose` |
| F5 | WAL 模式下每条连接一直持有库文件的 SHARED 锁，所以别的进程在本连接打开期间拿不到删 `-wal` 所需的 EXCLUSIVE；路径上的 `-wal` 消失只会是被替换或被外部删除 | `sqlite3WalClose`、pager WAL 锁 |
| F6 | 本工具自己也会在服务运行时替换 `index.db`：`rebuild-index --force`（先删旧 `-wal`/`-shm` 再 rename）；不带 `--force` 时它拒绝并说明"那个进程会写进已删除的文件，答案停在旧索引直到重启" | src/cli/index_ops.rs:369–394、701 |

## 决定

| 项 | 决定 | 依据 |
|----|------|------|
| 方向 | 修触发条件，而不是给每条连接的每个事务加 schema 检查：**本工具的任何连接，只要它打开的 `index.db` 或 `-wal` 已不是路径上的那个文件，就不再写、不再 checkpoint、关闭时不 checkpoint；服务进程在每条消息开始时发现这一点就拒绝（直到重启）** | M1–M3 的混读需要"旧文件上的进程继续写或 checkpoint"；它停下后，新打开的进程只看到彼此一致的新文件。每事务检查要改动所有读写路径（17 个 CLI 读命令、索引器、回填），代价和回归面都大得多 |
| 判定 | 主库：`SQLITE_FCNTL_HAS_MOVED`（F1）。`-wal`：打开结束时记录（打开路径取绝对路径）的 (dev, ino)，之后比较路径当前的 (dev, ino)；路径上没有 `-wal` 而连接仍写着打开时那个 WAL（`SQLITE_FCNTL_JOURNAL_POINTER` 指针相同）也算；打开时不存在则不比较 | F2：SQLite 不给 `-wal` 的身份 |
| 写 | 所有读写连接装 commit hook：判定为已替换 → 否决提交，并置粘滞标记 | F3：覆盖索引器、CLI 新鲜度写入、回填、晋升写连接的提交；例外见评审 R1（大事务溢出、VACUUM） |
| checkpoint | `run_optimize` 的 TRUNCATE checkpoint 前先判定 | 唯一的显式 checkpoint（live 索引上） |
| 关闭 | `Database` 析构时若已替换，先设 `NO_CKPT_ON_CLOSE` | F4：只靠 SQLite 自己的判定会在 `-wal` 被替换时照常 checkpoint |
| 服务 | 主进程、晋升后、二级：每条消息开始时判定；已替换 → 本条及以后的工具调用都报拒绝（说明原因与"重启会话"），不再读 | 二级本来每条消息查 schema，这里统一行为；粘滞避免一半旧一半新的状态 |
| `rebuild-index --force` 提示 | 改为"那个进程会拒绝使用索引直到重启" | F6 的旧说法在本改动后不成立 |
| Windows | `-wal` 身份只在 unix 记录（`cfg(unix)`）；钩子在 Windows 也装，但 Windows 的文件层不回答 HAS_MOVED，等于不起作用 | SQLite 在 Windows 打开文件不带 FILE_SHARE_DELETE，打开期间无法被替换；std 稳定版也拿不到 Windows 的文件 ID |
| 版本 | minor（用户可见：替换后从"继续用旧文件/混读"改为拒绝） | 发版手册"Choices" |

## 不覆盖（计划写进 CHANGELOG）

- 替换发生在一次已通过判定的提交或 checkpoint 进行之中：这一次仍会落到旧文件并移动 `-shm`。
- 替换发生在本连接打开期间（SQLite 打开 `-wal` 与本工具记录其身份之间）：之后判定不出来。
- 一条消息或一个 CLI 命令进行中被替换：本次剩余的读不再判定（下一条消息才判定）。
- 不是本工具的进程写旧文件：本改动只约束本工具的连接。
- 每个事务都检查 schema（D#883 的方向 1）：没有做；二级读者仍是每条消息检查。

## 计划

1. RED（`storage::db` 单元测试，两个 `Database` 在同一进程里按顺序操作，无竞态）：旧文件上的写连接在 `-wal` 被替换后仍能提交并让新打开的连接读到替换文件里的页；`index.db` 被替换后 `run_optimize` 的 checkpoint 让新连接读到替换后的库；`-wal` 被替换后关闭旧连接会 checkpoint。服务层：主进程在文件被替换后仍回答工具调用。
2. 实现：`storage::db` 加文件身份与判定、commit hook、`run_optimize` 判定、析构；`mcp/server` 每条消息开始判定并报拒绝；`cli/index_ops` 提示改写。
3. 全量测试、clippy（两条特性腿）、fmt；变异（独立 worktree + 独立 target）。
4. 端到端：发布版 0.196.0 对本次构建，真实主进程在 pull 之后收到会写入的工具调用：0.196.0 写入（`-shm` 的 mxFrame 前进），本次构建拒绝且 mxFrame 不动；本地 `docs/` 的场景脚本重跑。
5. 代价：每次提交两次 `stat`；测本仓库 `src/` 副本全量索引耗时与二级/主进程每次调用耗时，前后对比。
6. 独立评审（简报只查回归、说法与变异，不要求构造绕过检查的文件），然后发版。

## 实施中的修正

- **缺失的 `-wal` 不算替换**：第一版把“路径上没有 `-wal`”也算替换，snapshot 的测试夹具在打开的连接上执行 `PRAGMA journal_mode = DELETE`（SQLite 删掉自己的 `-wal`），之后写入被拒，6 个 snapshot 测试失败。产品代码只在打开时设一次 `journal_mode = WAL`（记录身份之前），其余都在测试里。改为路径上有另一个文件才算；被删除的 `-wal` 要等下一个进程在原路径建出新文件才判得出（写进 Not covered）。
- **去掉 `check_files` 里的语句缓存清空**：它没有可观察的效果（服务的回答无论如何都换成拒绝），变异测不出，按“只写守卫能验证的代码”删掉。
- **`run_optimize` 的检查**：变异 M4（去掉这条检查）第一次存活——替换之后 `PRAGMA optimize` 总会开写事务（没有统计的空表每次都重新分析），被 commit hook 否决，报的是 SQLite 的 "constraint failed"。这条检查实际起作用的是给出原因；测试改为钉住拒绝信息，M4 被杀。

## 评审 R1（新起的子代理，盲审收窄简报，对 385b1d9c）：HIGH 0 / MEDIUM 2 / LOW 3 / NOTE 6

没有发现合法流程被误判为替换（只有 N1 的 WAL→DELETE→WAL 来回）；端到端六种替换情形 20/20 拒绝；评审自造 17 个变异，杀 14、存活 3；Windows cfg 翻转后 check 与 clippy 干净。

| # | 结论 | 处理 |
|---|------|------|
| M1（实测） | 大于页缓存（64 MB）的事务在 COMMIT 之前就把页溢出写进 WAL（评审：替换后的 2 万行事务，WAL 0 → 119,405,872 字节，随后提交被拒）；“写 WAL 之前就拒绝”不成立。MCP `rebuild_index` 整个索引一个事务 | 改说法（CHANGELOG、代码注释），写进 Not covered：未提交的页在拒绝后回滚，其他进程只读到最后一次提交（从 SQLite 源码推导，未实测） |
| M2（实测） | 把“路径上没有 `-wal`”当作未替换：只删 `-wal` 后放行的一次提交让下一个打开者报 disk I/O error | 修：路径上没有 `-wal` 且连接仍写着打开时那个 WAL（`JOURNAL_POINTER` 指针相同）算替换。第一版读数据库文件头判 WAL 模式，snapshot 夹具的 `journal_mode = DELETE` 在同一语句里先删 `-wal` 后改文件头、提交钩子在中间被调用，误拒 12 个测试，改为指针判据。RED：主测试加“deleted index.db-wal”一例，修前写入放行 |
| L1（实测） | `VACUUM` 提交不经过 commit hook | 不改代码（LOW 的修复是一次发版里风险最高的代码）：产品里只有启动时的向量压缩在一次通过检查的提交之后紧接着 VACUUM，写进 Not covered |
| L2（实测） | 项目路径不是 UTF-8 时 `conn.path()` 为 None，`-wal` 检查失效 | 修：`-wal` 路径由打开路径取绝对路径后拼出。RED：`a_wal_replaced_under_a_non_utf8_path_is_noticed` 修前写入放行 |
| L3（实测） | 缓存的语句在替换后照样执行；二级的提前返回在 RESET 之前，丢掉了 0.196.0 让缓存语句重编译的效果 | 二级：先 RESET 再判定，缓存语句重编译时被拒。RED：`a_cached_read_on_a_secondary_is_refused_after_a_replacement` 修前读到 `Ok(1)`。主进程：说法改为“之后编译的语句被拒，之前编译的照样执行，回答一律是拒绝” |
| N1 | WAL→DELETE→WAL 来回后下一次提交被拒 | 产品代码不这样做，写进 Not covered |
| N2 | CLI 与后台的提交被拒只报 `constraint failed` | CHANGELOG 写明 |
| N3 | 3875a878 的提交说明说“`check_files` 不再清缓存”，但那个清空在 cb8b7096 之前就去掉了 | 不改写未推送的提交说明（保留评审轨迹），以本 spec 为准 |
| N4 | 决定表“Windows 不做”与钩子在 Windows 也装不符 | 决定表已改 |
| N5 | 变异存活：拒绝 savepoint（R7）、`-wal` 路径取调用方路径（R8）、`run_startup_tasks` 去掉检查（R15） | R7：事务控制一段加 `SAVEPOINT s; RELEASE s;`；R8：L2 的修复后 `-wal` 路径就是调用方路径的绝对形式，不再有两种来源；R15：去掉 `run_startup_tasks` 里的检查（消息开始时已查，二级的读在 `ReadGuard` 里查，主进程的写由 commit hook 查，没有可观察的效果） |
| N6 | 每次调用 3–7 µs 未被确认：评审在高负载下测得中位 +9.6 µs、最小值 +3.4 µs | CHANGELOG 两次测量都写上，“3 到 10 µs”（R2 NOTE-4 后改为在 12feb709 上重测，见验证记录） |

## 评审 R2（新起的子代理，只审修复提交 12feb709，盲审收窄简报）：HIGH 0 / MEDIUM 0 / LOW 1 / NOTE 5

三处修复（被删的 `-wal`、非 UTF-8 路径、二级的缓存语句）在父提交上各自复现了修复前的失败，修复后通过；产品流程里没有找到回归（切到 DELETE、别的进程做 TRUNCATE checkpoint、同进程/跨进程关闭、二级在时主连接重开、相对路径加 `chdir`、符号链接目录、`:memory:`、移动项目目录）。评审自造 11 个变异，杀 9、存活 2。按评审深度上限（2 轮），本轮只改说法、记录，不再改代码。

| # | 结论 | 处理 |
|---|------|------|
| LOW-1（实测） | `-wal` 不在时的判据比较的是 WAL 对象的堆地址：WAL→DELETE→WAL 来回后，默认情况下新 WAL 落在旧地址、下一次提交被拒；占住空闲块让地址不同时，删掉的 `-wal` 判为未替换、提交放行。与 CHANGELOG 第 102 行“来回后下一次提交被拒”不符 | 产品里唯一切 journal 模式的是 snapshot 的 staging 连接，切到 DELETE 后即关闭，不回到 WAL。不改代码；CHANGELOG 的 Not covered 改为写明两种实测结果 |
| NOTE-1（实测） | 去掉 `std::path::absolute` 的变异存活（只有相对路径打开后 `chdir` 的探针能杀） | 产品路径都来自 `getcwd`，本来就是绝对路径。不加测试：`chdir` 是进程级状态，会干扰同一测试进程里并行的其他测试。记为遗留 |
| NOTE-2（实测） | `-wal` 路径用调用方的写法，主库用 SQLite 解析后的名字：目录符号链接改指另一份副本时，12feb709 判为替换（385b1d9c 判未移动）；相对的 `file:x.db` 被 SQLite 当 URI 打开，`-wal` 检查静默关闭（385b1d9c 能查） | 两种都只出现在库调用方：服务与 CLI 的路径来自 `getcwd`，符号链接的 `.code-graph` 被 `reject_symlinked_dir` 拒绝。不改代码；评审建议的改法（把 `sqlite3_db_filename` 按字节读成 `OsStr`）记为遗留 |
| NOTE-3（读代码） | 去掉 `run_startup_tasks` 的检查后，只有一个极窄的时间窗（`notifications/initialized` 的检查与随后 `get_index_status` 之间被替换）日志行不同 | 两版都存在同样的竞争，只是窗口位置不同；不改 |
| NOTE-4（读代码） | CHANGELOG 的每次调用耗时是在每条消息检查两次的构建（cb8b7096：`handle_message` 与每条消息后的 `run_startup_tasks` 各一次）上测的 | 在 12feb709 的源码构建上重测，CHANGELOG 换成重测数字（见验证记录） |
| NOTE-5 | “64 MB”应为 64,000 KiB；R1 M1 的溢出说法“推导、未实测”现已由评审实测（rv20：替换后 2 万行事务提交前向旧 WAL 写了 119,393,512 字节，提交被拒并回滚，新连接读到 count=1，integrity ok）；二级上连 `BEGIN; ROLLBACK;` 也被拒（二级的 authorizer 在检查过的读事务之外一律拒绝，`src/storage/db.rs:556-562`） | CHANGELOG 三处改写 |

## 验证记录

- RED（实现前，`check_files` 为空实现）：4 个存储层测试失败——"index.db: the write reached the replaced files"、"the run_optimize went ahead"、"the close deleted the -wal now at the path"、只读连接 "read Ok(1)"；服务层 `test_primary_refuses_index_files_replaced_under_it` 失败于替换后的调用照常回答。两个对照测试（检查点与其他写入方不算替换、事务中 drop 连接）实现前后都通过。
- GREEN：`storage::`、`snapshot::`、主进程与二级服务测试 262/262；全量 `cargo test --no-default-features --no-fail-fast` 2508 / 0 / 7（31 个套件）；clippy 两条特性腿、fmt 通过；cb8b7096 的 pre-commit：lib 1657/1657 及其余套件通过。
- 变异（git archive 副本 + 独立 target，`/var/tmp/agent-tmp-d883b/`，每个变异都确认编译并跑了 29 个测试）：commit hook 不否决、不查 `-wal`、不查 HAS_MOVED、`run_optimize` 不查（测试加强后）、关闭时照常 checkpoint、替换后 authorizer 放行、authorizer 拒绝事务控制、`handle_message` 不查、只读检查忽略替换、缺失的 `-wal` 算替换、`take_read_refusal` 不报替换——11/11 被杀。
- 端到端（scratchpad 沙箱，真实主进程，pull 替换 `-wal` 或 `index.db` 并带源码，之后 20 次“改源码 + 工具调用”，同时循环跑 `health-check`）：发布版 0.196.0 两种变体 20 次调用都照常回答，`-shm` 的 mxFrame 从 2 走到 26；新构建（cb8b7096 的 embed release，md5 5d53eea9，其后只改了测试和 `check_files` 的缓存清空）pull 前 1 次正常、之后 20 次都报 "refusing to use …: its files were replaced"，mxFrame 停在 2。两边 CLI 都没有执行植入的 SQL（0.196.0 0/1049、0/1490；新构建 0/977、0/1251）。
- 说法逐条实测（cb8b7096 nodef release，md5 ed57c670，对 0.196.0）：主进程下 `rebuild-index --confirm --force` 之后，0.196.0 照常回答、新构建拒绝；主进程运行时移动项目目录，0.196.0 报找不到符号、新构建拒绝；二级服务（另一进程持锁）`index.db` 被替换后，0.196.0 照常回答、新构建拒绝。CLI 命令的写入只在数据库层测过（CHANGELOG 已写明）。
- 代价（两边源码构建：6e81e29b md5 7e1045a9 对 cb8b7096 md5 ed57c670，不带 embed，本仓库 `src/` 145 个文件 4,480 个节点）：全量索引 6 轮中位数 1,136 → 1,138 ms；主进程每次 `get_ast_node` 0.073/0.073/0.074 → 0.080/0.077/0.077 ms（3 轮×100）；二级 0.355/0.347/0.339 → 0.346/0.348/0.347 ms。cb8b7096 每条消息检查两次（R2 NOTE-4），这组数字已不进 CHANGELOG。
- 代价重测（R2 后；6e81e29b md5 7e1045a9 对 12feb709 md5 77d75f2e，不带 embed，同一份 `src/` 副本，`health-check --json` 145 个文件 4,480 个节点，两边交替各 6 轮，负载 0.95–1.31，脚本 scratchpad `d883b/perf/run_r2.sh`）：`rebuild-index --confirm --quiet` 中位 1,070 → 1,062 ms（1,013–1,086 对 998–1,100）；主进程每轮 300 次 `get_ast_node` 的中位数，6 轮的中位 0.0715 → 0.076 ms（0.071–0.079 对 0.075–0.105），+4.5 µs；二级每轮 100 次，0.320–0.348 → 0.321–0.351 ms，中位 0.335 → 0.339 ms。
- main CI（9241c967，run 44202175805）：macOS 上 `a_wal_replaced_under_a_non_utf8_path_is_noticed` 失败于建目录 `Os { code: 92, message: "Illegal byte sequence" }`（macOS 不允许非 UTF-8 文件名）；其余 6 个任务通过，macOS 上其他 8 个新测试都跑了且通过，Windows 上 2 个非 unix 新测试通过。按 `tests/cli_snapshot.rs:313` 的先例把该测试改为只在 Linux 跑。
