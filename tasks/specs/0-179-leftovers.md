---
status: done (local commits on main, not released)
revision: 2
---

# 0.179.0 遗留：读法的平方耗时、两种丢摘要的写法、测试改写本机缓存

## 背景

0.179.0 的 CHANGELOG "Not covered" 和发版报告里留下四项（D#571、D#572 和两种写法）。都是相对 0.178.0 的退步或已有缺陷，修复目标是恢复 0.178.0 的行为，不加新功能。

## 证据（2026-10-24，修改前实测）

- **平方耗时**：HEAD 上 `definitionHeaders` 单次读取：100 KB 只含 `async` 的行 2,782 ms，320 KB 单行 `const a = (` 2,781 ms（钩子读新旧两次，所以整体 5.4 s / 5.3 s）。按倍增扫描（12 种扩展名 × 47 种重复单元，8 KB→32 KB）只有四类增长约 16 倍：JS/TS 修饰词行（`async\n`、`static\n`）、JS/TS `const a = (`、Go `func (`、Kotlin `fun <`；Go 和 Kotlin 那两类连按名字查的 `extractSignatures` 也慢（160 KB 1.3–1.5 s），编辑前记录基线（`baselineSignatures`）和 Stop 检查都走它，0.179.0 之前就有。
- **测试改写本机缓存**：把整个测试进程的 HOME 指向沙箱后逐文件跑 CI 的 49 个 JS 测试文件，6 个文件在这个"真实 HOME"里写出 `.cache/code-graph/binary-path`：doctor、hook-fire、incremental-index、mcp-launcher、session-init、stop-impact。doctor.test.js 的三个进程内 `runDiagnostics` 都写（两个改了 HOME，但 find-binary 加载时已按真实 HOME 算好路径；JS-04 那个没改 HOME）。49 个文件在沙箱 HOME 下全部通过、0 跳过。`tests/hardening.rs` 的 `js_test_suite_leaves_the_shared_tmp_dir_intact` 跑全量时 HOME 也是沙箱，但不检查沙箱 HOME 里的写入，所以看不见。
- **丢摘要的写法**：`onComment` 用一条对所有语言通用的正则判断"选中的名字在注释行上"，`#`、`--`、`;` 开头都算注释，`*` 后跟空白也算。于是 Rust 的 `#![allow(..)] fn f(`、JS 的私有方法 `#parse(`、StandardJS 的行首分号 `;(function main () {` 和生成器方法 `* entries (prefix) {` 都被当成注释，丢了 0.178.0 的摘要。

## 方案

1. 读法（`stop-impact.js`）每个起点扫描的长度有上界：
   - 修饰词最多 4 个 `(?:…\s+){0,4}`（合法 TS 方法最多 4 个：访问修饰、`static`、`override`、`async` 或 `get`/`set`），跨行写法照旧能读；
   - `const f = (…)`、Go 接收者 `(…)`、Kotlin 类型参数 `<…>` 的内容最多 `MAX_SIGNATURE_CHARS`（600）个字符。超过 600 的签名本来就读不出（`readHeader` 返回 null）；差别只在这种绑定从"整段读不出"变成"不算定义"。
2. ~~注释判断按语言~~（rev 2 收窄，见决策）：`#![` 和 `#[` 一样不算注释；`*` 加空白后紧跟选中的名字（`* entries (`）是生成器方法，不算注释。其余照旧，JS 行首 `#`、`;` 仍按注释处理。
3. 测试：8 个文件（rev 1 的 6 个，加 install-e2e、auto-update）在模块作用域、加载会解析缓存路径的模块之前把 HOME、USERPROFILE 指向 `mkdtempSync` 沙箱，`test.after` 删除；不在模块级删 `CLAUDE_CONFIG_DIR` 的文件同块里把它指到沙箱（`js_test_files_neutralize_claude_config_dir` 要求成对）。`js_test_suite_leaves_the_shared_tmp_dir_intact` 在沙箱 HOME 里预置一个哨兵 `.cache/code-graph/binary-path`，跑完要求目录里只有它、内容不变。

### 形态与预期

| # | 形态 | 0.178.0 | 0.179.0 | 新 |
|---|---|---|---|---|
| T1 | 100 KB `async` 行 / 320 KB `const a = (` 单行，钩子整体 | < 40 ms | 5.4 s / 5.3 s，无摘要 | 线性 |
| T2 | Go `func (`、Kotlin `fun <` 单行重复，Stop 检查 / 基线 | 平方 | 平方 | 线性 |
| T3 | 跨行修饰词 `static\n  foo() {`、4 个修饰词的方法 | 读得到 | 读得到 | 读得到 |
| C1 | JS `* entries (prefix) {` 改参数 | 出 | 不出 | 出 |
| C2 | Rust `#![allow(dead_code)] fn helper(` 改参数 | 出 | 不出 | 出 |
| C3 | JS 私有方法 `#parse(text) {` 改参数 | 出（问 `parse`） | 不出 | 不出（rev 2：索引里叫 `#parse`） |
| C4 | JS `;(function main (argv) {` 改参数 | 出（问 `main`） | 不出 | 不出（rev 2：具名函数表达式没有节点） |
| C5 | Python `# def helper(a):` 注释行改了 | 出 | 不出 | 不出 |
| C6 | JS `// a function wrapping …`、`/** … this function already … */` | 出 | 不出 | 不出（S2、S9 不变） |
| C7 | JSDoc 里 ` * entries(prefix) {` 这样的代码示例行 | 出 | 不出 | 出（与 0.178.0 相同，写进不覆盖） |
| H1 | 全量 JS 测试跑完，真实 HOME 的 `.cache/code-graph` | 被改写 | 被改写 | 不变 |

## 验收标准

- AC1：T1/T2 的增长比例测试（16 KB→64 KB，CPU 时间）修改前红、修改后绿；钩子整体在 100 KB / 320 KB 输入上的耗时有修改前后数字。
- AC2：C1–C4 修改前红、修改后绿；C5、C6、T3 修改前后都绿。
- AC3：H1 的 Rust 断言修改前红（列出写入者）、修改后绿。
- AC4：语料差分（`tasks/pre-edit-pick/hook_diff.js`，4,118 个片段，HEAD 对修改后）逐条列出，变化只来自上面几类。
- AC5：JS 全量、`cargo test --test hardening` 相关用例通过；CHANGELOG（Unreleased）写明。

## 决策与进展

- 2026-10-24：开始。npm 可信发布（D#290）需要用户在 npmjs.com 操作，0.179.0 使用一周后的监测需要等时间，都不在本任务内。
- 2026-10-24（rev 2）：用 0.179.0 的二进制给真实小项目建索引后核对：私有方法的节点名是 `#parse`，`impact parse` 返回 "Symbol not found"（同文件另有 `parse` 时会问到那一个）；`;(function main (argv) {` 的 `main` 没有节点。所以 C3、C4 恢复 0.178.0 的选法只会问到"找不到"或问错，0.179.0 不出摘要是对的结果。方案 2 收窄为只放开 C1、C2，C3、C4 用测试固定为不出，代码注释写明原因。C1 的 `entries`、C2 的 `helper` 在真实索引里都能查到调用方。
- 2026-10-24：修好 rev 1 的 6 个文件后 Rust 守卫仍红：`scripts/install-e2e.test.js`（CI 的 JS 列表排除了它，守卫会跑）里继承环境的 4 处子进程也写；hook-fire.test.js 的沙箱放在了 `require('./lifecycle')` 之后，加载时已按真实 HOME 定好路径，移到前面。随后在真实 HOME 下跑 CI 列表，`binary-path` 被删掉了：`auto-update.test.js` 进程内的自愈测试调用 `clearCache()`。沙箱里只查"多出来的文件"看不见删除，所以守卫改为预置哨兵、要求不变。被删的文件随后由本会话已安装插件的钩子按原内容（md5 e0831da4…）写回。

## 验证记录

- 修改前失败：5 个读法增长测试 15.6–16.6 倍（16 KB→64 KB）；C1、C2 两个测试（以及 rev 1 的 C3、C4）失败；Rust 守卫报 `binary-path`；预置哨兵后，去掉 auto-update.test.js 的沙箱时守卫报"目录为空、binary-path 不见了"（变异后按备份拷回，cmp 一致）。
- 修改后：`pre-edit-guide.test.js` 90/90，`stop-impact.test.js` 108/108，`auto-update.test.js` 99/99，hook-fire 12/12；`cargo test --test hardening js_test` 2/2。
- v0.178.0 上跑新测试：C1、C2 出摘要（与表一致）；"不出摘要"组在 Python 注释处失败（0.178.0 会选 `helper`）。
- 读法耗时（一次读取）：100 KB `async` 行 2,782→2.0 ms；320 KB `const a = (` 2,781→25.7 ms；160 KB Go `func (` 1,278→21.6 ms，Kotlin `fun <` 1,541→24.9 ms。倍增扫描（12 种扩展名 × 47 种单元，8→32 KB）修改后没有超过 20 ms 的组合。`extractSignatures` 160 KB：v0.178.0 1.10–1.48 s，HEAD 1.13–1.49 s，修改后 1.3–25.7 ms。
- 钩子整体（冷进程、存根二进制、3 次中位数）：100 KB `async` 行 HEAD 5,305 ms 无摘要 → 31 ms 出摘要（v0.178.0 27 ms）；320 KB `const a = (` 5,327 ms 无摘要 → 78 ms（v0.178.0 30 ms）；40 KB 876→29 ms；160 KB 1,348→52 ms。
- 等价性：语料 5,193 段不同文本（.mjs 1,818、.js 1,368、.rs 1,054、.py 893、.ts 38、.cjs 18、.c 4）与 16,076 个真实源文件（cargo 注册表的 .rs 6,000，本机 node_modules 的 .js 6,778、.ts 2,806、.cjs 309、.mjs 183）上，HEAD 与修改后的 `definitionHeaders` + `allSignatures` 输出 0 处不同。Go、Kotlin 没有语料，只有单元测试里的形式。
- 真实钩子差分（hook_diff.js，HEAD 对修改后）：4,159 个不同片段，same_none 4,053、same_summary 106，新增 / 去掉 / 换名 0，非零退出 0，stderr 0。
- 全部 51 个 JS 测试文件（含 install-e2e）在同一个沙箱 HOME 下：1,829 个，1,825 通过、0 失败、4 跳过。
- 真实 HOME 下跑 CI 的 JS 列表（所有修复之后）：1,786 个，1,783 通过、0 失败、3 跳过；`~/.cache/code-graph` 下 14 个文件路径、修改时间、大小前后一致。`os.tmpdir()` 下沙箱目录残留 0。
- `cargo fmt --check` 通过；`git ls-files '*.js' | xargs -n1 node --check` 通过。
