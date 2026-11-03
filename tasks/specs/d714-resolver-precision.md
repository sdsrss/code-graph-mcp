# D#714：解析精度（同文件同名定义、内置方法名、barrel 导入）（2026-11-03）

状态：已分析，未实现。下一步：新会话实现 A3（下面的"设计"），A4 先按语言模拟。来源：`docs/plugin-usage-audit-2026-11-03.md` A2–A4，待办 D#714。

## A3：同一文件里多个同名定义，调用会连到每一个

### 语料（claude-mem-lite 副本，0.184.0 + D#712，`run.sh --dump-judged`）

- 默认下限精度：extracted 4,578/5,039 = 90.9%，inferred 4,370/4,396 = 99.4%。
- extracted 层错边 461 条，其中 459 条是"同文件、该名字定义不止一次"：388 条的这些定义不在任何具名函数节点里面，71 条的被调定义嵌套在具名函数里。
- 错边最多的名字：`tests/local-steering.test.mjs` 的 `app` 319 条、`block` 13 条、`commitOnly` 12 条、`rulesPath` 6 条；`install.mjs` 的 `ok` 23 条、`warn` 20 条、`log` 16 条、`fail` 8 条。
- 写法：
  - 测试文件里每个 `describe(…, () => { const app = () => …; const write = () => …app()… })` 回调各自定义一遍同名箭头函数。定义在匿名回调里，匿名回调不是节点。
  - `install.mjs`：顶层 `function ok(msg)`（231 行）和某个函数里的 `const ok = (msg) => …`（2443 行）。

### 模拟（`tasks/d714-resolver-precision/sim_span_scope.py`）

规则：同文件同名候选里，"所在作用域（包住它的最内层函数/方法/类节点，没有则为文件）包含调用方节点"的为可见，保留其中作用域最内层的；没有可见的保持原样。

- 195 组调用：75 组只留 1 个，120 组同层留 2–12 个。
- 结果：对的边 321 条全保留；错边去掉 76 条，留下 383 条。
- 留下的 383 条都在匿名回调里，只用节点行范围分不开。

### 设计（待实现）

- 解析器（内存中，不入库）：每个定义节点记录包住它的最内层函数类节点的行范围（匿名函数、箭头函数、lambda 也算）；每个调用关系记录调用点所在的最内层函数类节点的行范围。
- 解析（`index_files.rs` 批内 `same_file_targets`）：候选的作用域包含调用点作用域即为可见，保留作用域最内层的；没有可见的保持原样（不新增边，只从原来的集合里去掉）。同文件绑定只在批内决定，文件的解析结果都在内存里，所以不需要改表结构。
- 先列形态和夹具：JS `describe`/`it` 回调、闭包、遮蔽、函数表达式、类方法同名；Python 嵌套 `def`、`lambda`；Rust 闭包、cfg 孪生（已有 `source_line` 机制，不能回归）；C++ 成员。每种语言的函数类节点 kind 不同。
- 预期上限（模拟，非实测）：claude-mem-lite extracted 层错边 461 → 2（前提是 383 条全部能按匿名作用域分开）。
- 需要升 INDEX_VERSION。

## A4：内置方法名遇到类型未知的接收者，绑到项目里唯一的同名方法

- 审计：daagu inferred 层错边 225/245 是这一类（`dict.update`、`Path.exists` 绑到项目里唯一的 `update`/`exists` 方法）。
- 记忆 `feedback_simulate_a_relabel_per_language_first`、`feedback_narrow_the_result_not_the_pool`：先按语言列出内置类型的方法名表，用 judged dump 模拟"去掉这条边"对每种语言精度和召回的影响，再改；改结果不改候选池。

## A2 的剩余部分：非噪声名字经 barrel 的导入边

- gsd-lite：`init` 的导入边按名字连到了 `tests/round2-edge.test.js:48`，产生 42 条错边。
- D#712 已让 `js_import_targets` 跟进重导出，但导入边本身（`resolve_js_module_targets`）在指定文件里找不到名字时仍退回按名字解析。可以复用同一个跟进逻辑。

## 相关

- D#720：TypeScript `await f<T>(x)` 不产生调用关系。daagu 前端在 D#712 之后余下 40 条漏报，oracle 列出的 38 条全是这种写法。
