# D#712：导入函数的裸调用不再被噪声表丢掉（2026-11-03）

状态：已实现并验证，未发版。来源：`docs/plugin-usage-audit-2026-11-03.md` A1（及 A2 中经 barrel 的部分），待办 D#712。

## 背景

`CROSS_FILE_CALL_NOISE`（`get`、`read`、`write`、`join`、`close`、`push` 等）是给"接收者类型未知的方法调用"用的。批内解析（`index_files.rs`）对没有同文件目标的裸调用也直接按这张表丢弃，不管名字是不是从项目文件导入的：

- daagu 前端：`import { get } from '../request'` 后的 `get(url)`，`request.ts` 的 `get` 83 个调用方全丢（同文件的 `post` 有边）。
- gsd-lite：`import { read } from '../state/index.js'`（barrel：`export { read } from './crud.js'`），`read` 少 53 条。

夹具复现（`tasks/specs` 同日）：`get`/`post` 写法相同，`post` 有边，`get` 没有；`update`/`read` 经 barrel，`update` 有边，`read` 没有。

## 规则

- 解析器（`parser/relations/member.rs`）：JS/TS 裸调用 `N()`，名字在 JS 噪声表里（`is_cross_file_call_noise(N, "javascript")`），按作用域找到的绑定是顶层 `import { N } from 'S'`（不改名、非 `import type`、`S` 为相对路径），就像 D#120 的改名导入一样记成 `{"q":"imp","js_module":S,"v":N}`。
  - 只限噪声名字：其他名字今天按名字解析、再由 2d-bind/2d-prune 按导入改指，已经能连上；全部改成 `imp` 会改变大量边的元数据，并让经 barrel 的调用依赖下面的跟进规则。
  - 只限相对路径：`resolve_js_specifier_path` 只解析相对路径；包导入（`import { join } from 'path'`）保持原样，结果都是没有边。
  - 默认导入、`require` 解构、动态导入不在本规则内（保持原样，即仍被丢弃）。
- 解析（`resolve::js_import_targets`，D#120 的批内、延迟阶段、pending 清扫、增量重排都走它）：在 `S` 指向的文件 F 里找不到导出的顶层函数 N 时，跟进 F 的重导出：
  1. F 的 `<module>` 有一条目标名为 N、带 `js_module` 的 `imports` 边（`export { N } from './x'`、`import { N } from './x'; export { N }`）：到该路径指向的文件继续找。只用路径，不用这条边已连上的目标，导入解析自身的错误（A2 的 `init`）不会传进来。
  2. 否则 F 的 `star_reexport` 边（`export * from './x'`）：逐个跟进，恰好一个来源找到 N 才用它（多个来源同名时 JS 不导出该名字）。
  3. 最多 8 跳，访问过的文件不再进入。
- 这同时作用于原有的改名导入调用（D#120）：经 barrel 的 `import { a as b } from './index'` 原来留在 pending 里永不绑定，现在能绑定。
- 升 INDEX_VERSION 126 → 127。

## 不做

- Python 的同类调用（`from x import get` 后 `get()`）：审计没测，留作遗留。
- 导入边本身经 barrel 的解析（A2 的 `init` 错边、80 条经 index.js 的非噪声调用）：属于 D#714。
- barrel 文件改动（去掉重导出）后，已连到 barrel 背后文件的调用边要等调用方重建才会消失；此时调用方的导入在运行时本身就会报错。

## 验证

- 解析器表测试：噪声名字 + 相对具名导入 → `imp`；非噪声名字、包路径、`import type`、参数/局部遮蔽、默认导入 → 不变。
- 索引测试：直接导入、命名重导出、`export *`、两个 `export *` 来源同名（无边）、改名导入经 barrel；每个文件依次修改后与重建一致。
- SCIP oracle：daagu 前端、gsd-lite 召回上升，精度不降；hono、express 精度不降（新增边必须是对的）。
- `cargo fmt --check`、clippy 两条特性腿、`cargo test --no-fail-fast` 两条特性腿。

## 决定记录

- 复用 D#120 的 `imp` 标记，而不是在批内、延迟阶段、pending 清扫、增量重排 4 条路径上分别改噪声判断：`imp` 调用在这 4 条路径上已经统一为"只在路径指向的文件里找导出"，增量与重建一致的测试也都现成。
- barrel 跟进只用重导出边上的路径（`js_module`），不用这条边按名字连上的目标；`star_reexport` 恰好一个来源有该名字才绑定。
- 重导出查询与导出查询同样钉住执行计划（`test_js_export_map_query_seeks_the_module_node_edges` 扩展为两条查询）。
- 验证中发现的独立缺陷：TypeScript `await f<T>(x)` 不产生调用关系（与名字无关，`post` 也丢），登记为 D#720，不在本次改动里修。
- `/var/tmp/d712`（语料副本、scip-typescript、release 构建）保留给 D#714 的 oracle 对比，整轮结束时删除。

## 验证记录

- 修复前：解析器表测试 4 行失败（应打 `imp` 的行）；索引测试全量即丢 4 条边（直接导入的 `get`、命名重导出、`export *`、改名导入经 barrel）。
- 修复后：`test_js_noise_named_import_call_shapes`、`test_js_noise_named_import_calls_bind_through_barrels`、`test_js_noise_named_import_through_barrel_matches_rebuild` 及 D#120 原有 10 个测试通过。
- 变异：去掉 `export *` 多来源判断 → 多出 `close` 错边，测试失败；去掉命名重导出跟进 → 丢边，测试失败。
- `cargo clippy --all-targets -D warnings` 两条特性腿 → 通过；`cargo test --no-fail-fast` → 默认 2,390 / 0 / 7，embed-model 2,413 / 0 / 7（D#711 后为 2,387 / 2,410，新增 3 个测试）。
- SCIP oracle（`tasks/d712-noise-named-import-calls/oracle_ab.sh`，同一份副本，0.184.0 对本次 release 构建）：

  | 语料 | 金标准对 | 召回 | 精度（默认下限） | 错边 |
  |---|---|---|---|---|
  | daagu 前端 | 575 | 83.5% → 93.0% | 99.6% → 99.3% | 2 → 4 |
  | gsd-lite | 1,247 | 92.1% → 96.3% | 96.3% → 96.5% | 44 → 44 |
  | hono | 1,145 | 80.4% → 80.9% | 97.3% → 97.3% | 26 → 26 |
  | claudemd / sgc / claude-mem-lite / express | 1,663 / 1,641 / 8,993 / 69 | 不变 | 不变 | 不变 |

  daagu 新增的 2 条"错边"（`getAIQuota`、`getStockDetail` → `get`）读源码确认是真实调用（`return get<{…}>(…)` 跨多行），oracle 判"无此名调用"是误判。daagu 余下 40 条漏报，oracle 列出的 38 条全是 `await get<T>(…)`（D#720）。
- `impact get --file frontend/src/api/request.ts`（daagu 副本）：0 个调用方 → 56 个直接调用方。
- `~/.cache/code-graph` 残留：0 个新文件。
