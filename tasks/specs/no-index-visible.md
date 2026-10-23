# 无索引目录里的查询不再静默返回空（P0-b）
状态：进行中（已实现，待评审）

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
- [x] AC4：stderr 原有那一行不变，`--json` 的错误对象不变。验证：`tests/cli_e2e.rs` 的 `test_cli_json_error_no_index_emits_error_object` 和 `claude-plugin/scripts/doctor.test.js` 的现有断言通过；新用例另外断言 stderr 那一行还在。
- [ ] AC5（发版后监测）：`python3 scripts/plugin_usage.py` 的 `empty` 列里，副本目录中的调用不再是空输出。

## 方案

- 出错点：`src/cli/mod.rs` 的 `CliContext::open_inner` 用 `anyhow::bail!("No index found at …")` 报错；`main.rs` 在命令执行完后有一个统一出口（`if let Err(e) = &result`），`--json` 模式就是在那里把错误打到 stdout 的。
- 改法：把"没有索引"做成一个具名错误类型（例如 `NoIndexError { db_path }`），main 里用 `e.downcast_ref::<NoIndexError>()` 判断，在文本模式下往 stdout 打一行。用类型判断，不按错误文字匹配。
- 同样的文字还出现在 `src/cli/health.rs:258`（health-check 自己的路径），实现时一并确认它走不走这个出口。
- 取舍：stdout 多了一行。按文本解析输出的脚本会读到这一行，但这种情况下原来的输出是 0 字节加退出码 1，脚本本来就该看退出码。在终端里，用户会看到 stdout 和 stderr 两行内容相近的文字。

## 计划

- [x] S1：先写测试（`tests/cli_e2e.rs`，逐个命令，包括 `--json` 不变）——覆盖 AC1、AC2——依赖：无——执行：主会话
- [x] S2：`NoIndexError` 加上 main 出口的改动——覆盖 AC1、AC3、AC4——依赖：S1
- [x] S3：CHANGELOG 的 Unreleased 写明 stdout 多一行——依赖：S2
- [ ] S4：发版前评审，用一个新的子代理（`review.md`）——依赖：S3

## 决策与进展

- 2026-10-23：草稿。这一项改变已发布 CLI 的用户可见默认行为，按规范属于 L3，需要用户授权后才能实现。
- 2026-10-23：用户授权，在分支 `fix/no-index-visible` 上实现。实测范围：19 个命令（18 个查询命令加 `health-check`）都经过 `CliContext::open_inner`，无索引时原来都是 stdout 为空。
- 设计收窄：最初在 main 里还加了一段"`--format json` 时不打这一行"的判断，后来删掉了。只有 `health-check` 有 `--format`，而它的 JSON 分支自己生成 `no_index` 报告，不会产生 `NoIndexError`；删掉之后测试照样通过。
- 2026-10-23：同一轮审计里的 P1（把 SubagentStart 扩到所有代理类型）已撤回，不在本 spec 范围内。核实发现自定义代理会加载 CLAUDE.md（release-reviewer 45/45、executor 24/24、reviewer 30/30），而 daagu、gsd-lite、claudemd 的 CLAUDE.md 里都有 code-graph 的引导段落。所以 release-reviewer 31 个会话一次没用 code-graph，原因不是缺少上下文。

## 验证记录

- `cargo test --no-default-features --test cli_e2e test_cli_no_index_says_so_on_stdout`：改动前失败（`["show", "foo"]: one line on stdout, got ""`），改动后通过。
- `cargo test --no-default-features`：通过，2342 个通过、0 失败、7 个忽略。
- `cargo clippy --no-default-features --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：第一次失败（两处换行），`cargo fmt` 之后通过，相关测试重跑通过（3/3）。
- 手动核对（debug 构建，未建索引的 git 仓库）：`show foo 2>/dev/null` 的 stdout 是 `[code-graph] No index found at …/.code-graph/index.db. Run: code-graph-mcp incremental-index`，退出码 1；stderr 是 `Error: No index found at …`；`health-check --format json` 的 stdout 仍然是 JSON。
