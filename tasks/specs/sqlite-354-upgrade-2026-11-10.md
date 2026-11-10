# 2026-11-10 SQLite 安全升级（D#753）与旧 Release 页面（D#361 剩余）

状态：完成，已发版 0.190.1（2026-11-10）。

## 背景

0.190.0 发版时，main 的 CI 里 Cargo Audit 报了新公告 RUSTSEC-2026-0459（CVE-2026-119375）：SQLite 3.54.0 之前的版本，打开一个数据库和它旁边页大小不同的 `-wal` 时，会把未初始化的页缓存内存当查询结果返回。libsqlite3-sys 0.36.0 内置 SQLite 3.51.1，在受影响范围内；修复版本是 libsqlite3-sys ≥0.39.0（随 rusqlite 0.41）。本工具会打开 `.code-graph/index.db` 和它的 `-wal`，克隆来的仓库可以放进这两个文件，所以受影响路径是可达的。

用户 2026-11-10 授权：升级 rusqlite 0.38.0→0.41.1、libsqlite3-sys 0.36.0→0.39.1（连带 hashlink 0.11→0.12.2、uuid 1.22→1.29.2、新增 hashbrown 0.17.1），单独发 0.190.1；同时把 23 个旧 Release 页面里的 `cargo install code-graph-mcp --version X` 改成 git tag 写法。

## 决定

| 项 | 决定 | 理由 |
|----|------|------|
| 版本号 | 0.190.1（补丁） | 目标是行为不变；差分若发现行为变化，再改为次版本并写进说明 |
| 单独发版 | 只放依赖升级和两处测试适配 | 出现行为差异时能归因 |
| `trace_v2` 测试 | 加一个仅测试用的 `conn_mut` | rusqlite 0.41 的 `trace_v2` 要 `&mut Connection`（修了 use-after-free，#1899） |
| super-journal 测试 | 夹具改成 3.54 仍会删除的形状 | 3.54 只删名字像 super-journal（`-mj` + 9 位十六进制，倒数第 3 位是 `9`）且内容列出本日志路径的文件；对照必须仍能触发，否则断言空过 |
| 旧 Release 页面 | 只替换那一行，先存原文 | 对外修改，要可还原 |

## 风险与验证设计

- 查询规划器（3.53、3.54 都有改动）可能改变没写 ORDER BY 的查询的行序 → 语料差分：v0.190.0 和升级版各自从源码构建（各用自己的 target），在多份语料上逐字节比较索引表内容和 CLI 查询输出。
- 浮点转文本默认改为 17 位有效数字（3.53）→ 代码里没有在 SQL 中把 REAL 转成文本的地方（已 grep）。
- rusqlite 0.41 校验 PRAGMA 名称 → 代码用到的 8 个 PRAGMA 都是标准名称；全套测试会覆盖。
- 修复前证据：CI 42717894698 的 Cargo Audit 失败；修复后证据：新提交的 CI audit 通过。

## 验证记录

- 旧 Release 页面（3887591c）：23 页逐页改写，每页只有那一行变化，改后读回线上正文与预期逐字节一致，23/23；全量扫描后只剩 v0.190.0 自己的描述句含该短语。原文与还原方法在 `tasks/release-pages-2026-11-10/`。v0.190.0 的 CHANGELOG 条目与线上说明同步改为"已于 2026-11-10 改写"。
- 更正：libsqlite3-sys 0.36.0 内置的是 SQLite 3.51.1（`sqlite3.h` 的 `SQLITE_VERSION`），不是之前写的 3.53。
- 3.54 的 super-journal 加固（`pager_delsuper`，sqlite3.c 61542–61651；`pagerIsSuperJrnlName` 60257）：只删除名字像 super-journal 且内容列出本日志路径的文件。旧夹具（名为 `victim`、内容任意）在 3.54 下对照不触发；新夹具对照触发；把 inspect 的原地只读打开改成读写打开（变异）→ 测试在 tests.rs:1575 失败（R1 修复给注释加了 5 行后是 1580）（"inspect deleted the file the journal named"），还原后 `git diff` 干净。
- 变异无法覆盖的一点：`stage_copy` 若重新复制 `-journal`（D#630 被撤回的做法），在 3.54 下受害文件必须列出暂存目录里的日志路径（随机临时目录名），攻击者无法预知，所以这个形状在 3.54 下本身不会删除文件；`stage_copy_takes_the_main_file_alone` 里的受害文件断言在 3.54 下因此是空过的，它的主断言（暂存目录只有主文件）仍有效。
- 两条测试腿：默认特性 pre-commit 2428 通过、0 失败、7 忽略；`--features embed-model` 2451 通过、0 失败、7 忽略。
- 语料差分（v0.190.0 与升级版都从源码构建、默认特性；base 内置 SQLite 源码 ID 2025-11-28 即 3.51.1，head 2026-10-16 即 3.54.1）：8 份语料（flask、gin、gson、leveldb、express、hono、本仓库 v0.190.0、Python 3.14 标准库），7 张表逐行比较 56/56 相同（含 id；标准库 114,900 条边）；CLI 3,088 次运行只有 1 次不同（leveldb `health-check --json` 的 `last_indexed_at`/`index_age`，两次建索引相差 1 秒的墙钟时间）；MCP 2,060 次调用 0 次不同，抽查 flask 257 次调用（原写 312，算错）0 次报错、平均 0.9–8.8 KB。脚本在 `/var/tmp/agent-tmp-sqlite-diff/drive.py`、`mcp_diff.py`（任务结束时删除）。
- 版本 0.190.1；embed-model release 构建 `--version` 0.190.1，health-check 为 hybrid；提交 74cc94bb（pre-commit 通过）。
- 干净克隆插件清点：0.190.1，Skills 2 · Agents 1 · Hooks 6 · MCP 1。
- 发版前评审第 1 轮（新子代理，盲审，v0.190.0..74cc94bb）：HIGH 0、MEDIUM 1、LOW 7、NIT 7。评审自己的差分更大（9 份语料含 tokio、django，CLI 17,736 次、MCP 8,276 次，含增量编辑和二级只读实例、embed 构建的向量搜索），只有时间戳和版本号不同；性能无退化（tokio/pystd/django 全量索引 −3.0%/−2.2%/−0.2%）；cargo-audit：HEAD 退出 0，BASE 报 RUSTSEC-2026-0459。处理：
  - M1：0.190.0 会删除 `.code-graph/index.db-journal` 里点名的任意文件（SQLite 3.51.1 把它当 super-journal 删掉），0.190.1 不会。我在 gin 索引上复现：0.190.0 一次 `health-check` 删掉项目外的文件，0.190.1 不删。插件的 session-start 钩子和状态栏都会跑 `health-check`。→ 说明里新增一节安全说明，pin-back 处写明会把两个问题都带回来。不另加代码防护：3.54 只删名字像 super-journal 且内容列出本日志的文件，攻击者无法让用户已有的文件满足这两个条件。
  - L1、L2（rusqlite 0.41 让两处报错文案在损坏的数据库上变了）：我复现后写进 Not covered，不改代码（前提是手工改过或损坏的库）。
  - L3：测试注释写明它现在只抓"原地读写打开"，暂存日志的形状由 `stage_copy_takes_the_main_file_alone` 抓（评审变异 s-M2 在那个测试被杀）。
  - L4（Windows 上 `canonicalize` 给 `\\?\` 形式，对照不会触发）：测试设计本来就在非 Unix 上只打印说明；不改。
  - L5（扇出测试没有下限，7fc590cd 以来就有）：与本次无关，记入待办。
  - L6：去掉"`snapshot inspect` 打开给定文件"这条暴露路径（inspect 遇到非空 `-wal` 走只复制主文件的暂存，不会和 `-wal` 一起打开）。
  - L7：v0.190.0 页面也在当天重新生成过，README 写明原文在 tag 的 CHANGELOG 里。
  - N1（CLI 命令的文本/JSON 说法）、N3（"测试以外没改代码"）、N5（源码构建需要 Rust 1.89）：改说明。N2：差分脚本收进 `tasks/sqlite-354-upgrade-2026-11-10/`。N6：backlog spec 的版本已更正。N4、N7：不改（N7 在推送并核对 CI 后成立）。
- 第 1 轮修复提交 db7fa644（pre-commit 2428/0/7），已推送；main CI 42811973060 7/7 通过，Cargo Audit 由红转绿（扫描 425 个依赖，无公告）；两个改过的测试在 macOS、Windows、with-embed 日志里都是 ok（macOS 上对照是硬断言，所以 `/private/var` 下也触发了）。
- 发版前评审第 2 轮（新子代理，只审 db7fa644）：HIGH 0、MEDIUM 1、LOW 4、NIT 7，全部是说明/注释问题；评审照 README 重跑差分，数字全部复现（56/56、3,088、2,060）。处理：
  - M1（删除问题不只两条插件路径：所有打开索引的命令都会触发，`serve` 一启动就会）：说明改为"谁先打开索引谁回滚"，列出 MCP 服务启动和 CLI 命令。
  - L1（"源码构建需要 Rust 1.89"是从声明推出来的；实测两个版本都要 1.91，因为 `round_char_boundary`）：删掉这句，这次发版不改变最低版本。
  - L2（"更早版本内置更旧的 SQLite"不准）：改为"更早版本都内置早于 3.54 的 SQLite，行为相同"。
  - L3（相对路径也能删）：我复现（`../outside/victim.txt`，0.190.0 删、0.190.1 不删）后写进说明。
  - L4（植入的日志仍会被回滚进索引，可把 `index.db` 撑到 8,589,934,592 字节表观大小）：我复现（两版都从 3,751,936 变为 8,589,934,592，Linux 上稀疏、实际 3,668 KiB）后写进 Not covered，记入待办。
  - N1（会话启动钩子不总跑 health-check）：说明不再点名具体钩子。N2（UTF-8 文案在其他读该节点的命令里也变；inspect 的变化只在原地回滚模式的原始文件）：改说明。N3：测试注释补上兜底暂存分支由哪两个测试拦。N4、N5：spec 行号和次数更正。N6：`build.sh` 收进 `tasks/sqlite-354-upgrade-2026-11-10/`，README 写明标准库依赖本机 Python 补丁版本。N7：说明改为"除版本号外"。
- 停止线：两轮评审已到上限；第 2 轮修复只改说明、测试注释和 tasks 文件，二进制与 db7fa644 相同，不派第 3 轮，由我逐条复现后改。
- 发版：tag v0.190.1 在 3470df79（CI 42816635653 7/7，Cargo Audit 通过）；Release 42818413497 10/10（25 分 27 秒）；Release 正文 = CHANGELOG 0.190.1 段（与线上逐字相同，只差末尾换行）；非草稿，14 个附件；npm `@sdsrs/code-graph` 0.190.1，六个包 `_npmUser` 都是 GitHub Actions（`trustedPublisher` github）；临时目录真实安装 added 2 packages，平台二进制 `--version` 0.190.1，内含 SQLite 源码 ID 2026-10-16 555e07ef（3.54.1）；用发布的二进制实跑：植入日志的 gin 项目 `health-check` 后项目外文件仍在；在 gin、flask、hono 上与源码构建的 1,132 次 CLI 输出逐字节相同。
- 清理：/var/tmp/agent-tmp-sqlite-diff（818M）、两轮评审目录（30G、22G）、scratchpad 临时目录都已删除；`~/.cache/code-graph` 4 小时内只有插件自己的 update-state.json（12:40Z，早于本次命令且本次所有二进制运行都设了 CODE_GRAPH_NO_AUTO_UPDATE）；我的一次 `npm view` 留下的 npm 日志已删除。
- 待办：D#753 关闭；新增 D#758（扇出测试没有下限）、D#759（植入的日志仍会被回滚进索引）。
