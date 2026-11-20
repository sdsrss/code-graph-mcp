---
status: approved
revision: 1
---
# 常驻文字里"Still use Grep"改成小写 grep

## 背景
claudemd 仓库 2026-11-20 的提示词审计（`docs/prompt-audit-2026-11-20.md` F14，10-17 那次起遗留）：
本插件写进各项目 `CLAUDE.md` 的托管块有一句 "Still use Grep for literal strings/regex in non-code files"，
MCP `instructions` 里有 "Still Grep for exact strings/regex"。大写的 Grep 指 Claude Code 的 Grep 工具；
本机 Claude Code 2.1.329 的会话里没有这个工具（带内嵌搜索工具的构建只给 Bash，shell 里的 `grep` 可用）。
小写 `grep` 同时指 shell 命令和工具，两种构建都成立。

用户 2026-11-20 在 claudemd 会话里要求把审计剩余各项做完（含这一条）。

## 目标
- `claude-plugin/scripts/adopt.js` 托管块那一行、`src/mcp/server/mod.rs` 的 `INSTRUCTIONS_NOISY` 那一句改成小写 `grep`。
- `tests/routing_bench.rs` 的 `STEERING_BLOCK_MIRROR` 同步。

## 不做
- "rounds of Grep + Read"（托管块第 2 行、`templates/plugin_code_graph_mcp.md` 的 description）：描述性的，不是指令，不改。
- `code-explorer` 子代理的 tools 清单（`Grep, Glob`）：那是 harness 的工具名，没有的构建里自然不存在。
- 发版：随下一次 code-graph-mcp 发版（见"决策"）。

## 验收
- [x] AC1 `adopt.test.js` 新用例：托管块含整行 "Still use grep for literal strings/regex in non-code files; still Read files you'll edit." — 旧文字下失败，新文字下通过
- [x] AC2 `tests/doc_cli_alignment.rs` 新用例 `resident_instructions_point_literal_search_at_grep` — 旧文字下失败，新文字下通过
- [x] AC3 提交钩子（cargo check、全量 cargo test、fmt、clippy 两条 feature 线）通过

## 决策
- 2026-11-20：路由基准（`tests/routing_bench.rs`，要付费 API key、默认 ignored）没有跑。改的是指向另一个工具的那一句里一个词的大小写，不涉及本插件 7 个工具的描述；为它花一次付费基准不相称。
- 2026-11-20：托管块文字一变，已采用的项目会在下一次会话开始看到一次"块已过期"的提示（steering-channel 的机制），所以这条随正式发版发出，发版说明里写明。
- 2026-11-20：使用者选择现在推送到 `main`、不单独发版（"2A"），随下一次 code-graph-mcp 发版发出。

## 验证记录
- `node --test --test-name-pattern="points literal-string search" claude-plugin/scripts/adopt.test.js`（旧文字）→ 失败（0/1）
- `node --test claude-plugin/scripts/adopt.test.js`（新文字）→ 通过（102/102）
- `cargo test -q --test doc_cli_alignment resident_instructions_point_literal_search_at_grep`（`mod.rs` 旧文字）→ 失败（0 passed; 1 failed）
- `cargo test -q --test doc_cli_alignment`（新文字）→ 通过（10 passed; 0 failed）
- `cargo fmt --check` 三个改动的 Rust 文件 → 通过
- 提交钩子（044a61b4 首次提交时）→ 通过（plugin JS tests、cargo check、cargo test 2540 passed / 0 failed、fmt、clippy、clippy --features embed-model）
