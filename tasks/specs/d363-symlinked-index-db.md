---
status: in progress
revision: 1
---

# D#363：`.code-graph` 里的 index.db（及 -wal/-shm）是符号链接或不是普通文件时，拒绝使用

## 背景

0.170.1（D#358）让每个读 `.code-graph` 的命令拒绝一个符号链接的 `.code-graph` 目录，并在 Not covered 里写明：真实目录里的 `index.db` 是符号链接时仍然会顺着链接打开并写入。`src/utils/owned.rs` 的模块文档也写明 index.db 不经过它（rusqlite 打开，跟随链接）。`.code-graph` 是普通的仓库内容，`git clone` 可以带进一个符号链接。

## 证据（2026-11-01，0.179.4 的 target/release 二进制，scratchpad 夹具）

| # | 形态 | 0.179.4 的表现 |
|---|---|---|
| S1 | `index.db -> ../../other/.code-graph/index.db`（另一个项目的索引） | `health-check --format json` 报 healthy，计数是那个项目的；`incremental-index` 把那个索引改写成本项目的 4 个文件（原来 5 个） |
| S2 | `index.db` 是悬空链接 `-> ../../outside-new.db` | `incremental-index` exit 0，在项目外新建 1,773,568 字节的数据库，并在它旁边建 `index-run.lock`（`in_flight::run_lock_path` 取的是 SQLite 解析后的路径） |
| S3 | `index.db -> ../../foreign.sqlite`（别的程序的库） | 那个库的 journal_mode 从 delete 变成 wal，sqlite_master 从 1 项变成 44 项（原表还在） |
| S4 | `index.db` 是 FIFO | exit 1，`disk I/O error`，不挂起 |
| S5 | `index.db` 正常，`index.db-wal -> ../../victim.txt` | 受害文件不变；exit 1，`unable to open database file` |

代码阅读（内置 SQLite 3.51.1，`libsqlite3-sys-0.36.0/sqlite3/sqlite3.c`）：`unixFullPathname`（:46210）逐级 lstat 并展开每个符号链接，打开的是解析后的路径，所以 wal/shm 也落在链接目标旁边；`unixOpen`（:45869）和 shm（:44233）带 `O_NOFOLLOW`，所以 `-wal`/`-shm` 本身是链接时 SQLite 拒绝打开而不是跟随（S5 实测一致）。`SQLITE_OPEN_NOFOLLOW`（:63712）拒绝路径上**任何一级**符号链接，包括项目根之上的目录（macOS 临时目录 `/var -> /private/var`、链接进来的 home），不能直接用。

## 方案

1. `owned.rs` 新增 `reject_unowned_db(db_path)`：对 `db_path`、`db_path-wal`、`db_path-shm` 各做一次 lstat（只看最后一级）。不存在 → 通过；符号链接（活的、悬空的）→ `refusing to use a symlinked <path> — remove it and re-run`；存在但不是普通文件（FIFO、目录、socket）→ `<path> exists and is not a regular file`。
2. 唯一入口：`Database::open_impl`（所有读写打开）和 `Database::open_readonly`（MCP 只读副实例）开头调用它。CLI、MCP 主/副实例、索引器、快照校验都经过这两处。
3. 需要正确消息和顺序的地方再显式调用一次（在 `exists()` 判断之前，否则悬空链接会被说成"没有索引"）：`CliContext::open_inner` / `try_open`、`health-check --format json` 的拒绝报告（`reason: "refused"`）、grep 回退时的说明、`incremental-index` 开头、MCP `from_project_root`。
4. 插件：statusline 收到 `reason: "refused"` 的报告时显示与 D#358 相同的 `code-graph: ✗ .code-graph refused (see health-check)`，而不是 `✗ 0 nodes | 0 files`；doctor 的修复说明不再只说 ".code-graph is a symlink"。
5. 不改：`rebuild-index`（临时库 rename 覆盖 index.db，替换的是链接本身）、`reindex`（按名 unlink）、快照安装（rename 覆盖、按名删 sidecar）——这三条都不跟随链接，正好是用户的补救路径。

### 决策：硬链接照常打开（不拒绝）

- git 不能带来硬链接；`tar` 的硬链接条目只能指向归档内的成员。
- 能产生的场景是用户自己 `cp -al` 复制项目：原项目和副本的 index.db 共享 inode，nlink 都是 2。按 nlink>1 拒绝会让**原项目**也打不开自己的索引。
- 共享 inode 的后果是写入会到达这个文件的每个名字：另一个副本的索引；sidecar 硬链接到无关文件时就是那个文件（评审实测：一个 5 字节的文件被 `-wal` 写成 12,392 字节）。git 带不来硬链接。
- 测试固定这个决定（硬链接的 index.db 照常可查询），并写进 CHANGELOG 的 Not covered。

### 不做

- lstat 和 SQLite 打开之间的竞争窗口（需要本机并发写入者；克隆来的仓库是静态的）。
- ~~`-journal`：WAL 模式下不创建；链接时 SQLite 自己带 `O_NOFOLLOW` 拒绝。~~ 评审 M-2 证伪：每次打开都会探测 hot journal，FIFO 会让 `open()` 一直阻塞（0.179.4 同样挂起）。已加入检查。
- JS 钩子对 `index.db` 的存在性/mtime 读取（D#365 的范围）。

## 验收

- AC1：S1–S5 各一个 CLI 端到端测试，修复前失败、修复后通过：命令退出非 0 并给出上面的消息；`health-check --format json`/`--json` 给 `reason: "refused"`；链接目标及其目录的文件列表和字节不变；悬空链接的目标不被创建。
- AC2：`Database::open` / `open_nondestructive` / `open_readonly` 单元测试：符号链接和悬空链接被拒绝，目标不变、不被创建；正常文件、硬链接照常打开。
- AC3：MCP `from_project_root` 对链接的 index.db 返回错误，链接目标不变。
- AC4：路径上层是符号链接（项目通过链接目录访问）照常工作。
- AC5：statusline / doctor 的 JS 测试覆盖 index.db 被拒绝的报告。
- AC6：全量 cargo test + JS 测试 + clippy 两个 feature 通过；发版前新子代理评审。

## 进展

- 2026-11-01：用户授权按建议执行（"需要我拍板的也按你的建议执行"），按 L3 处理（已发布产物的默认行为变化）。复现 S1–S5，见上表。
- 2026-11-01：实现并提交 36ede5af。6 个 Rust 测试 + 2 个 JS 测试修复前失败、修复后通过；8 个变异（两个打开入口、CLI 上下文、incremental-index 入口、health JSON、grep 说明、去掉 -wal、去掉非普通文件分支）各至少让 1 个测试失败。实现时删掉了 `try_open` 和 MCP `from_project_root` 两处检查：数据库打开那一层已经拒绝，变异显示它们没有可观察的作用。
- 2026-11-01：逐入口实测（0.179.4 发布二进制 md5 d5a6c861 对 HEAD 二进制 md5 f5cb6fa3，由之后提交为 64dfe9a1 的工作区构建），8 种形态 × show / health-check（两种格式）/ grep / incremental-index / MCP serve，外加 statusline、doctor、rebuild-index 补救；结果写进 CHANGELOG 0.180.0。与 D#611（64dfe9a1）一起发 0.180.0。
- 2026-11-01：发版前评审第 1 轮（HIGH 0 / MEDIUM 2 / LOW 11）。修复：M-1（`snapshot inspect` 拒绝用户自己给的符号链接 → 链接输入走复制分支）、M-2（`-journal` 加入检查）、L-6（`is_snapshot_meta` 真值表测试）、L-7（doctor 措辞）、L-8（rustdoc 位置）；其余 LOW 写进 CHANGELOG 的说明和 Not covered。修复的 6 个变异（去掉 -journal、链接不走复制、判定式 4 种）各让 1 个测试失败。
