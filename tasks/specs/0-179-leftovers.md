---
status: released in 0.179.1 (tag v0.179.1 at f8cc5843, 2026-10-25)
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
- 2026-10-24：用户同意按建议发版，发 0.179.1（patch：相对 0.179.0 的行为变化都是恢复 0.178.0 的输出，或在超过 600 字符上限的签名上不再给出定义）。发版前补测 Go、Kotlin 语料（见验证记录）。发版提交 20df22cb（未推送），等待新子代理的发版前评审；之后 push → CI → tag → Release → 说明 → 三项核验。第 4.5 步（干净克隆的插件清单）跳过：自 v0.179.0 起 hooks/skills/agents/清单只改了版本号。
- 2026-10-24（R1）：发版前评审（新子代理，盲审 v0.179.0..20df22cb）0 HIGH / 0 MEDIUM / 10 LOW，没有违反契约的行为。我复现了 1、2、3、7、8：(1) 超过 600 字符的括号段 0.179.0 并非"本来就读不出"——Go `a(); g := func (<650 x>) Name() {` 读出 `a()`，2,000 字符以上的行让整个文件读出 null；(2)(3) Python 注释 `#![x] def helper(a):`、JSDoc 方法形状的行（` * entries(prefix) {`、` *   handleRequest(req) {`、` *\thelper(a) {`）0.179.1 都出摘要，与 0.178.0 相同、与 0.179.0 不同；(7) 160 KB `async` 行的 `allSignatures`（Write / `sed -i` 的基线）0.178.0 6.9–8.4 s、0.179.0 8.1–8.7 s，不是 1.1–1.5 s；(8) 字面 `fun <` 是 1,167 行（1,174 是 `fun\s*<`）。处理：只改文字——CHANGELOG 与 stop-impact.js 注释按复现结果改写（1、7、8、9、10、5 的守卫说法收窄），2、3 写进 CHANGELOG（恢复 0.178.0 的输出，不改代码）；4、5、6 是测试/守卫缺口，写进 Not covered 并延后为 D#575（LOW 的修复是发版里风险最大的代码）。
- 2026-10-24（R2）：修复提交 a2de8a56 的增量评审（新子代理）0 HIGH / 1 MEDIUM / 7 LOW，全是文字；确认只改了注释和文档。MEDIUM：语料比较里最长的括号段只有 125 字符（Go 接收者；Kotlin 类型参数 77），"600 以内由语料比较检查"说过了头——我复现：上限改成 130 时 Go/Kotlin 比较仍 0 处不同，改成 60 时读法测试仍 198/198。LOW：耗时范围随负载变（改用空闲机器上重测的数字）、注释里截断字符的列举不全、Kotlin 的括号段写法、403 的退避是"距上次检查满一小时"不是"一小时"、1,167 是三个库合计、升级说明的"两种写法"与正文列出的行不一致、一行过长。按 runbook 的停止线（第二轮）只收窄文字，不做 R3。
- 2026-10-25（D#575）：补上 R1 留下的三处测试缺口，只改测试（174f698d、f6a285fe、1ef4da15）。守卫另外预置 `bin/sentinel` 和 `update-state.json`（`{}`，与文件不存在读法相同），比较 mtime；Linux 上检查前等待环境里带沙箱路径的进程退出（最多 30 秒，超时报错）。用户要求推送发版，发 0.179.2（patch：发布出去的二进制和插件脚本除版本号外与 0.179.1 相同）。
- 2026-10-25（0.179.2 R1）：发版前评审（新子代理，盲审 6151b740..5c0b65f3）0 HIGH / 1 MEDIUM / 7 LOW。MEDIUM：等待的匹配条件是"环境变量的值落在沙箱路径之下"，各测试自己的沙箱都嵌在守卫的 TMPDIR 下，lifecycle.e2e 的更新器（下载二进制）在 runner 退出后还活了 17.6 s，网络慢时会让干净运行失败。我在 HEAD 上复现了误报（子进程自带 HOME/TMPDIR、存活 40 s → 失败 95.22 s），修复 0cb8d983：只匹配守卫自己设的 6 个变量的原值（`guard_env`，同时传给 `.envs`）。修复后 M11 通过、M9/M9b 失败、干净运行通过。LOW：改名再改回看不见（写进 Not covered）、自带沙箱的子进程迟到写入（写进 Not covered）、断言文字和注释说过头（随修复改）；doctor 测试在版本号已升、尚未发布的窗口里留下 `update-<ms>`、运行时间依赖网络与 API 配额，都是改动之前就有的问题，不在本次范围内。
- 2026-10-25（0.179.2 R2）：修复提交的增量评审（新子代理）0 HIGH / 1 MEDIUM / 7 LOW，合约 1–3 成立，Linux 上 4 成立。MEDIUM 是 CHANGELOG"存活 45 s 的子进程会失败"没写前提（继承了原环境）。关键 LOW：更新器只在没有 `target/` 的副本里发请求（`isDevMode()`，我核实主检出返回 true），正常检出和 CI 的 Rust 任务里不下载。按停止线只改文字：CHANGELOG、注释、断言的补救办法（删去"给子进程自己的沙箱"，那样写入仍落在真实 tmp 目录）；值写法不同（`<沙箱>/`）和守卫删沙箱后的迟到写入并入 Not covered。不做 R3。

## 验证记录
- R2 复核（2026-10-24，空闲，负载 0.07→1.56）：160 KB、每次新进程、各 5 次。旧版（0.178.0 / 0.179.0）`extractSignatures` / `allSignatures` 1,115–7,089 ms，其中 `async` 行的 `allSignatures` 6,732–7,089 ms；新版 Go 21.6–21.9、Kotlin 25.2–26.6、async 1.4–3.4 ms。CHANGELOG 用这组数字（1.1–7.1 s、6.7–7.1 s、1.4–27 ms）。
- R1 复核（2026-10-24，机器上另有其他项目的 vitest 在跑，负载 14）：160 KB 单次读取、每次新进程。旧版（0.178.0 / 0.179.0）`extractSignatures` 1,128–2,898 ms，`allSignatures` 1,408–8,749 ms；新版五次中位数 `extractSignatures` Go 40.4 / Kotlin 29.6 / async 2.7 ms，`allSignatures` 24.5 / 28.8 / 5.7 ms，`definitionHeaders` 28.1 / 28.6 / 3.9 ms。脚本 /var/tmp/r1791/verify/time1.js、f1.js、cases.js（评审的 pick.js 驱动）。

- 修改前失败：5 个读法增长测试 15.6–16.6 倍（16 KB→64 KB）；C1、C2 两个测试（以及 rev 1 的 C3、C4）失败；Rust 守卫报 `binary-path`；预置哨兵后，去掉 auto-update.test.js 的沙箱时守卫报"目录为空、binary-path 不见了"（变异后按备份拷回，cmp 一致）。
- 修改后：`pre-edit-guide.test.js` 90/90，`stop-impact.test.js` 108/108，`auto-update.test.js` 99/99，hook-fire 12/12；`cargo test --test hardening js_test` 2/2。
- v0.178.0 上跑新测试：C1、C2 出摘要（与表一致）；"不出摘要"组在 Python 注释处失败（0.178.0 会选 `helper`）。
- 读法耗时（一次读取）：100 KB `async` 行 2,782→2.0 ms；320 KB `const a = (` 2,781→25.7 ms；160 KB Go `func (` 1,278→21.6 ms，Kotlin `fun <` 1,541→24.9 ms。倍增扫描（12 种扩展名 × 47 种单元，8→32 KB）修改后没有超过 20 ms 的组合。`extractSignatures` 160 KB：v0.178.0 1.10–1.48 s，HEAD 1.13–1.49 s，修改后 1.3–25.7 ms。
- 钩子整体（冷进程、存根二进制、3 次中位数）：100 KB `async` 行 HEAD 5,305 ms 无摘要 → 31 ms 出摘要（v0.178.0 27 ms）；320 KB `const a = (` 5,327 ms 无摘要 → 78 ms（v0.178.0 30 ms）；40 KB 876→29 ms；160 KB 1,348→52 ms。
- 等价性：语料 5,193 段不同文本（.mjs 1,818、.js 1,368、.rs 1,054、.py 893、.ts 38、.cjs 18、.c 4）与 16,076 个真实源文件（cargo 注册表的 .rs 6,000，本机 node_modules 的 .js 6,778、.ts 2,806、.cjs 309、.mjs 183）上，HEAD 与修改后的 `definitionHeaders` + `allSignatures` 输出 0 处不同。Go、Kotlin 当时没有语料，只有单元测试里的形式。
- 等价性补测（2026-10-24，发 0.179.1 前；v0.179.0 对 HEAD 26fd43d8 的 `stop-impact.js`，md5 e9cc0b20… / b95a0277…）：Go 10,088 个文件（gin 75ccf94d、golang/go go1.23.0 6885bad7；含 `func (` 的行 24,012），Kotlin 3,670 个文件（kotlinx.coroutines 1.9.0 d8d6f8f3、ktor 3.0.0 6d55f8b4、okhttp c3179301；含 `fun <` 的行 1,174）。`definitionHeaders` + `allSignatures` 逐文件 0 处不同（读出定义 Go 88,911、Kotlin 24,094）；对两边读出的每个名字再调 `extractSignatures`，Go 82,525 次、Kotlin 20,822 次，0 处不同。脚本：`tasks/pre-edit-pick/reader_diff.js`（两边的 scripts 目录取自 `git archive`）。
- 真实钩子差分（hook_diff.js，HEAD 对修改后）：4,159 个不同片段，same_none 4,053、same_summary 106，新增 / 去掉 / 换名 0，非零退出 0，stderr 0。
- 全部 51 个 JS 测试文件（含 install-e2e）在同一个沙箱 HOME 下：1,829 个，1,825 通过、0 失败、4 跳过。
- 真实 HOME 下跑 CI 的 JS 列表（所有修复之后）：1,786 个，1,783 通过、0 失败、3 跳过；`~/.cache/code-graph` 下 14 个文件路径、修改时间、大小前后一致。`os.tmpdir()` 下沙箱目录残留 0。
- `cargo fmt --check` 通过；`git ls-files '*.js' | xargs -n1 node --check` 通过。
- 发版（2026-10-25 UTC）：main CI 7/7（run 40165003815；新 JS 测试在 Plugin 任务里通过，`js_test_suite_leaves_the_shared_tmp_dir_intact` 在 Linux/macOS/Windows 均 ok）；Release 10/10（run 40165909584，34 分钟）；说明 5,224 字符，14 个附件；6 个 npm 包 0.179.1，`_npmUser` 仍是 `sdsrs`；实装 2 个包，linux-x64 二进制 44,497,432 B、`--version` 0.179.1；npm 包里 claude-plugin 47 个文件与 tag 逐字节一致；用已发布的包跑 C1/C2 等 9 个写法与表一致，160 KB 读取 async 3.3 ms、Go 21.8 ms。
- D#575（2026-10-25）：旧守卫同时打开 M3（改写后写回原字节）、M4（删 `bin/` 和 `update-state.json`）、M9（runner 退出 1 秒后 detached 子进程写入）→ 1/1 通过；新守卫逐个打开 → 都失败，M9b（子进程活 45 秒）也失败；不带变异 70.65 s 通过。改守卫前实测：runner 退出到其后 41 秒，没有带沙箱路径的进程。release-smoke：替身 HOME + 403 预加载，修复前 `update-state.json` 变成 `{"rateLimited":true}`，修复后不变；真实网络 4/4。`BRACKETED` 改为 `{0,60}`、`{0,585}`、`{0,601}` 时 198/201（只有新测试失败），去掉上限 195/201，原样 201/201。CI 的 JS 列表 1,789 个，1,786 通过、0 失败、3 跳过；`cargo test` 全量通过（pre-commit）。
