# 2026-11-17 D#877（MCP 二级读者只在打开时检查索引）的修复尝试与撤回；D#878（D#784 的测试缺口与 inspect 提示）

状态：D#877 的修复**撤回，未发版**（尝试保留在本地分支 `d877-wal-attempt`，未推送）；D#878 完成，随下一个版本发出（CHANGELOG `## Unreleased`）。仓库公开：**未修问题的复现手法只在本地 gitignore 的 `docs/`（`docs/d775-vec0-method.md`、`docs/d784-r3-notes.md`、`docs/d877-r1-notes.md`），本文件和测试都不写；怎样让服务进入二级模式也不写。**

## 背景

0.195.0（D#784）在每次打开索引时检查 schema，拒绝本工具不建的触发器、视图和虚表。评审 R3 发现 MCP 服务的二级读者（另一个进程持有索引锁时，服务以只读连接读索引，整个会话不关）只在打开时查一次；0.195.0 把它写进 CHANGELOG 的 Not covered 和 GHSA 的 Not fixed（用户决定，2026-11-16）。

## 决定

| 项 | 决定 | 依据 |
|----|------|------|
| 授权 | 用户 2026-11-17："继续完成剩下的工作，先深入思考给出科学准确的建议，然后按你的建议执行" | — |
| 第一稿修法（已撤回） | 二级读者先读 `PRAGMA journal_mode`，只接受 WAL 模式，再在同一连接上查 schema；启动等待内重试 | R3 的两种复现都要求连接不在 WAL 模式（SQLite 只在非 WAL 模式下按路径找 `-wal` 和热日志，sqlite3.c:64287、64457）；WAL 模式下 pull 换掉文件时这个连接读的仍是它打开的那些（实测） |
| 发版前评审 R1 | HIGH 1 / MEDIUM 1 / LOW 3 | HIGH：多个进程共用同一个 `-shm`。pull 换掉文件后才打开的二级读者，在另一个进程继续写旧文件时，会读到检查时没读到的页，埋的 SQL 照样执行；0.195.0 与第一稿相同（我按评审的脚本复现了两种变体）。MEDIUM：第一稿会拒绝只读目录里回滚日志模式的索引，0.195.0 能读（回归）。细节只在本地 `docs/d877-r1-notes.md` |
| 处理 | **撤回第一稿，不发 0.196.0**；保留 D#878；在 CHANGELOG `## Unreleased` 和 GHSA 的 Not fixed 里把范围改准（不写手法） | WAL 要求只在没有别的进程写时有效，HIGH 在每个版本都存在，又带来一个实测回归；它也不是根本修法的一部分。评审规则：深循环靠撤回或只减不增的收窄收尾，不靠再打补丁 |
| 根本修法（待用户决定是否投入） | 方向一：每个跨多次读取保留的连接（二级读者、主进程、回填线程、CLI 写入）都在一个读事务内先查 schema 再读，并防住 SQLite 在 IOERR/NOMEM 时自动回滚该事务（sqlite3.c:90840）后后续语句读到未查过的快照。方向二：写入方发现自己打开的文件已被换掉（路径上的 inode 与打开时不同）就停止写入。两者都是重新设计，需单独的 spec、评审和版本 | — |
| D#878 | M2、M3b 各补一个能杀掉它的测试；`nodes_fts` 白名单补"本工具没写过的列""别的分词器"两例；`snapshot inspect` 读副本那一支被拒时，提示写用户给的文件（相对路径前加 `./`，与原地读一致）；补一个 CLI 测试钉住相对路径的写法（R1 F4：只用绝对路径的测试杀不掉"不加 `./`"的变异） | 0.195.0 R3 的 F3/F8 |
| 版本 | 不单独发版 | D#878 只有提示文本和测试；随下一个版本发出 |

## 不覆盖

- D#877 本身：见 CHANGELOG `## Unreleased` 的 "Not covered: wider than 0.195.0 said"。
- R1 的 F5（LOW）：`snapshot inspect` 拒绝快照时仍说 "delete … and re-run to rebuild the index"，这是写给索引的话；原地读那一支在 0.195.0 就这样说。本提交不改；之后在 20e1caea 里四种读法都改了。

## 验证记录

- 第一稿（分支 `d877-wal-attempt`）：RED/GREEN、11 个变异全杀、端到端都在该分支的 spec 版本里；撤回后不再适用。
- R1 的 HIGH 复现（我，发布版 0.195.0 与第一稿的 release 构建，用评审的脚本）：两种变体在两个版本里，第一次工具调用正常，另一个进程写入或检查点之后的调用都报 "malformed JSON"；不 pull 的对照没有执行。
- D#878：
  - RED 由变异给出（与本提交相同的 D#878 代码，在第一稿的变异运行里，独立 worktree + 独立 target，每次确认测试真的跑了）：M2（命名打开容忍一切读取错误）、M3b（容忍集合去掉 CORRUPT）、M5c（任何列名）、M5d（任何分词器）、F8（副本那一支写回副本路径）各被对应的新测试杀掉。
  - 新 CLI 测试 `cli_snapshot_inspect_refusal_names_a_relative_file_as_typed`：把 `naming` 改成不加 `./` 时失败（"refusing to open planted-copy.db"），还原后通过。
  - 实测（发布版 0.195.0 对本次构建，三种快照：WAL 模式、旁边有热日志、原地读）：0.195.0 前两种写 `/tmp/.tmpXXXX/snapshot.db`；本次三种都写 `./<文件名>`，退出 1。
  - `cargo test --lib -- snapshot storage::db` 119/119、`--test cli_snapshot` 14/14；`cargo fmt`；`cargo clippy --all-targets -D warnings` 默认与 `embed-model` 两种组合都通过。
