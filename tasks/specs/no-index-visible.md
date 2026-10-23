# 无索引目录里的查询不再静默返回空（P0-b）
状态：已完成（只剩发版后的 AC5 监测）

## 背景与目标

2026-10-23 的使用审计（`docs/plugin-usage-audit-2026-10-23.md`）发现：子代理经常在仓库的副本里工作（scratchpad 里的克隆、`git archive` 解出的目录），这些目录没有索引。模型调用 code-graph 的习惯写法是：

```
code-graph-mcp show X 2>/dev/null | head -40 || grep -n "function X" -A40 src/
```

无索引时，错误只写在 stderr，被 `2>/dev/null` 丢掉；`||` 也不会执行，因为管道的退出码取的是 `head` 的 0。结果是一次没有任何输出的调用，模型接着再用 grep 查一遍。2026-10-16 到 10-23，中立项目子代理在副本里的 9 次 code-graph 调用中有 3 次是这样（gsd-lite 的审查子代理）。插件自己两个仓库里的 "No index" 大多是开发时有意在新建的测试夹具里运行 CLI，不计入。

0.177.0 的实际行为（在未建索引的 git 仓库里实测）：

| 命令 | stdout | stderr | 退出码 |
|---|---|---|---|
| `show` / `callgraph` / `impact` / `overview` / `search`（文本模式） | 空 | `Error: No index found at <path>. Run: code-graph-mcp incremental-index` | 1 |
| 同上加 `--json` | `{"error":"No index found at …"}` | 同上 | 1 |
| `grep` | 退回普通文本搜索的结果 | `[code-graph] No index found. Run: …` | 0 |

目标：文本模式下，没有索引时 stdout 上也能看到一行说明，即使 stderr 被丢掉。

## 范围

- 做：经过 `CliContext::open` 的查询命令，在文本模式下遇到"没有索引"的错误时，在 stdout 打印一行说明；退出码仍为 1；stderr 原来那一行保持不变。
- 不做：
  - 自动建索引。要么在用户的副本里写入 `.code-graph/`（0.164 刚因为往用户仓库写东西修过一轮副作用），要么新增一个放在缓存目录里的索引位置，后者是更大的架构改动。真正需要时，命令行提示里已经给出了 `incremental-index`（gsd-lite 的 180 个文件全量索引耗时 0.31 秒）。
  - `--json`：已经在 stdout 输出错误对象。
  - `grep`：已经会退回普通文本搜索。
  - MCP 服务：启动时会自己建索引，不走这条路径。

## 验收标准

- [x] AC1：在未建索引的 git 仓库里，对每个经过 `CliContext::open` 的查询命令执行 `code-graph-mcp <cmd> <arg> 2>/dev/null`，stdout 恰好一行，包含 `No index` 和 `incremental-index`，退出码 1。验证：新增 `tests/cli_e2e.rs` 用例，逐个命令检查。
- [x] AC2：`--json` 的 stdout 仍然只有那一个 JSON 对象，不多出一行文本。验证：同一用例里加断言。
- [x] AC3：有索引时，所有命令的输出不变。验证：`cargo test --no-default-features` 全部通过。
- [x] AC4：stderr 原有那一行不变，`--json` 的错误对象不变（第二轮评审后撤回无 `.git` 的另一套文字，才真正成立，见决策记录）。验证：`tests/cli_e2e.rs` 的 `test_cli_json_error_no_index_emits_error_object` 和 `claude-plugin/scripts/doctor.test.js` 的现有断言通过；新用例另外断言 stderr 那一行还在。
- [ ] AC5（发版后监测）：`python3 scripts/plugin_usage.py` 的 `empty` 列（只计没有任何输出的调用）在副本目录中下降，同样的调用改记在 `no-index` 计数里。

## 方案

- 出错点：`src/cli/mod.rs` 的 `CliContext::open_inner` 用 `anyhow::bail!("No index found at …")` 报错；`main.rs` 在命令执行完后有一个统一出口（`if let Err(e) = &result`），`--json` 模式就是在那里把错误打到 stdout 的。
- 改法：把"没有索引"做成一个具名错误类型（例如 `NoIndexError { db_path }`），main 里用 `e.downcast_ref::<NoIndexError>()` 判断，在文本模式下往 stdout 打一行。用类型判断，不按错误文字匹配。
- 同样的文字还出现在 `src/cli/health.rs:258`（health-check 自己的路径），实现时一并确认它走不走这个出口。
- 取舍：stdout 多了一行。按文本解析输出的脚本会读到这一行，但这种情况下原来的输出是 0 字节加退出码 1，脚本本来就该看退出码。在终端里，用户会看到 stdout 和 stderr 两行内容相近的文字。

## 计划

- [x] S1：先写测试（`tests/cli_e2e.rs`，逐个命令，包括 `--json` 不变）——覆盖 AC1、AC2——依赖：无——执行：主会话
- [x] S2：`NoIndexError` 加上 main 出口的改动——覆盖 AC1、AC3、AC4——依赖：S1
- [x] S3：CHANGELOG 的 Unreleased 写明 stdout 多一行——依赖：S2
- [x] S4：发版前评审，用一个新的子代理（`review.md`）——依赖：S3

## 决策与进展

- 2026-10-23：草稿。这一项改变已发布 CLI 的用户可见默认行为，按规范属于 L3，需要用户授权后才能实现。
- 2026-10-23：用户授权，在分支 `fix/no-index-visible` 上实现。实测范围：19 个命令（18 个查询命令加 `health-check`）都经过 `CliContext::open_inner`，无索引时原来都是 stdout 为空。
- 设计收窄：最初在 main 里还加了一段"`--format json` 时不打这一行"的判断，后来删掉了。只有 `health-check` 有 `--format`，而它的 JSON 分支自己生成 `no_index` 报告，不会产生 `NoIndexError`；删掉之后测试照样通过。
- 2026-10-23：同一轮审计里的 P1（把 SubagentStart 扩到所有代理类型）已撤回，不在本 spec 范围内。核实发现自定义代理会加载 CLAUDE.md（release-reviewer 45/45、executor 24/24、reviewer 30/30），而 daagu、gsd-lite、claudemd 的 CLAUDE.md 里都有 code-graph 的引导段落。所以 release-reviewer 31 个会话一次没用 code-graph，原因不是缺少上下文。

- 2026-10-23：评审（新的子代理，一轮）没有 HIGH，处理如下：
  - F1（MEDIUM）CHANGELOG 的 "three projects" 不对：那 9 次全部是 gsd-lite 一个项目的 3 个会话。已改。
  - F2（MEDIUM）没有 `.git` 的目录里，提示的 `incremental-index` 会跳过、退出 0，agent 会陷入循环。`NoIndexError` 加了 `git_anchor`：没有 `.git` 时改为提示 `rebuild-index --confirm`，有 `.git` 时文字不变。测试先失败后通过。
  - F3 读端提前关闭时 `println!` 触发 panic 钩子，退出码变 0、stderr 那行丢失。改为 `writeln!` 并忽略写入错误。测试先失败后通过。
  - F4 措辞、F6（首次建索引期间仍只写 stderr）写进 CHANGELOG；F5 加了反向对照（被拒绝的 `.code-graph` 不进 stdout），评审员给的变异 `.is_some() || true` 现在会被测出；F7 把 `plugin_usage.py` 的空输出和"无索引"拆开计数，AC5 改成可测的写法；F8 补跑了 `doctor.test.js`；F9 修正注释；F10 `plugin_usage.py` 不再把含 "No index found" 的错误结果算成 deny。
  - 变异：三处修复各有测试抓住。第一次对 `println!` 的变异因为留下了未使用的 `use` 而编译失败，被脚本误报成"存活"；把 `use` 一起替换后重做，确认被杀死。

- 2026-10-23：合入 main（快进到 133b3de7，之后是 D#560 的评审修复 2b12f713 和数字更正 732c6bff）。未发版。

- 2026-10-23：发版前第二轮评审（新的子代理，只审修复提交）没有 HIGH，与本 spec 相关的处理如下：
  - M-5：F2 的修复让无 `.git` 的目录都提示 `rebuild-index --confirm`，而 `$HOME`、装着多个仓库的目录也没有 `.git`，在那里照做会把整棵目录树建成一个索引。M-4：同一处改动也改了 stderr 和 `--json` 里的文字，CHANGELOG 和 AC4 说的"不变"因此不成立。
  - 处理：撤回 F2（`git_anchor` 删掉），三处文字恢复 0.177.0 的原文。无 `.git` 目录里照提示运行 `incremental-index`，它会跳过，并在 stderr 上列出 `git init`、换到仓库里、`rebuild-index --confirm` 三种做法，由读者判断。新用例 `test_cli_no_index_without_git_says_what_a_repository_gets` 在撤回前失败、撤回后通过。差分：41 个命令 × 10 个目录（含 `$HOME`）共 410 次运行，去掉新增的 stdout 行后，退出码、stdout、stderr 与 0.177.0 逐字节相同（410/410），新增行出现在其中 182 次。
  - L-1：`--json` 在读端关闭时退出码为 0、stderr 为空，0.177.0 也是如此，CHANGELOG 已写明。L-2：建索引期间 `health-check` 不拒绝请求，CHANGELOG 已写明。
  - L-4：`plugin_usage.py` 把所有含 "No index found" 的结果都记为 `cg_no_index`，包括 0.178 之前把 stderr 一并收进来的结果。所以 AC5 的基线要用新脚本对发版前的时段重新计算，不能沿用审计报告里的数字。
- dead-end: 在无 `.git` 的目录里让提示直接给出 `rebuild-index --confirm` —— 判断条件只看有没有 `.git`，区分不了项目副本和 `$HOME` —— 本任务不再尝试
- 2026-10-23：第三轮评审（新的子代理，只审 f3a6345c）没有 HIGH。处理如下：
  - F2（MEDIUM）：新用例拿 `project.path()` 拼期望的路径，而 CLI 给的是物理路径。macOS 的临时目录经过 `/var` → `/private/var` 符号链接，这个用例会在 CI 的 macOS 那一路失败。改为从 stderr 读回原文再比对。旧写法在符号链接的 TMPDIR 下复现失败；修复后 cli_e2e 全量 401/401 通过。
  - F1、F3、F6–F9：commit 说明、代码注释和 CHANGELOG 里共有几句不准确的话，已改正：`replace_all` 时主 checkout 的文本不唯一；Stop 检查的那条说法不该是全称；建索引期间带 `--json` 时，stdout 上是 JSON 错误对象；`incremental-index` 的跳过提示在 `$HOME` 里也会给出 `rebuild-index`（0.177.0 起就如此）；`health-check` 有竞态窗口；worktree 副本走样后，可能报出另一个同名定义的调用方。
  - F4：只在 worktree 下生效的那个条件原先没有测试钉住，补了主 checkout 的 `replace_all` 用例，变异 JM2 现在会被测出。
  - F5（LOW，未修）：钩子数文本出现次数时允许重叠，Claude Code 的 Edit 不允许重叠，结果是 `old_string = X+X` 落在 `X+X+X` 里时，worktree 中静默。这是罕见形态，记入待办。
  - 不再开第四轮评审：这一轮的修复只改了一个测试的断言、一处注释和若干措辞，产品行为没有变化。F2 的真正检验是 CI 的 macOS 那一路。

## 验证记录

- `cargo test --no-default-features --test cli_e2e test_cli_no_index_says_so_on_stdout`：改动前失败（`["show", "foo"]: one line on stdout, got ""`），改动后通过。
- `cargo test --no-default-features`：通过，2342 个通过、0 失败、7 个忽略。
- `cargo clippy --no-default-features --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：第一次失败（两处换行），`cargo fmt` 之后通过，相关测试重跑通过（3/3）。
- 评审修复后：`cargo test --no-default-features --test cli_e2e -- test_cli_no_index …`（6 个相关用例）通过；新用例 `test_cli_no_index_without_git_names_rebuild_index`、`test_cli_no_index_line_keeps_exit_code_on_a_closed_pipe_and_stays_alone` 修复前失败（提示仍是 incremental-index；退出码 0、stderr 为空）。
- `node --test claude-plugin/scripts/doctor.test.js`：通过（69/69）。
- 手动核对（debug 构建，未建索引的 git 仓库）：`show foo 2>/dev/null` 的 stdout 是 `[code-graph] No index found at …/.code-graph/index.db. Run: code-graph-mcp incremental-index`，退出码 1；stderr 是 `Error: No index found at …`；`health-check --format json` 的 stdout 仍然是 JSON。
