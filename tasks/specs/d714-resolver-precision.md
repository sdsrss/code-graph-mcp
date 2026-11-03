# D#714：解析精度（同文件同名定义、内置方法名、barrel 导入）（2026-11-03）

状态：A3 已提交（a82c3b54）；A4 已提交（644e8640，见"A4 模拟与决定"）；A2 剩余已实现（见"A2 实现"）。来源：`docs/plugin-usage-audit-2026-11-03.md` A2–A4，待办 D#714。

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

### 设计

- 解析器（内存中，不入库）：每个定义节点记录包住它的最内层函数类节点的行范围（匿名函数、箭头函数、lambda 也算）；每个调用关系记录调用点所在的最内层函数类节点的行范围。
- 解析（`index_files.rs` 批内 `same_file_targets`）：候选的作用域包含调用点作用域即为可见，保留作用域最内层的；没有可见的保持原样（不新增边，只从原来的集合里去掉）。同文件绑定只在批内决定，文件的解析结果都在内存里，所以不需要改表结构。
- 先列形态和夹具：JS `describe`/`it` 回调、闭包、遮蔽、函数表达式、类方法同名；Python 嵌套 `def`、`lambda`；Rust 闭包、cfg 孪生（已有 `source_line` 机制，不能回归）；C++ 成员。每种语言的函数类节点 kind 不同。
- 预期上限（模拟，非实测）：claude-mem-lite extracted 层错边 461 → 2（前提是 383 条全部能按匿名作用域分开）。
- 需要升 INDEX_VERSION。

### A3 实现（2026-11-03）

- 解析器：`ParsedNode.lexical_scope`（`treesitter::lexical_scope`）——只给"词法绑定"的定义：JS/TS `function` 声明和 `const f = () => …` 声明符、Python 不在类里的 `def`、Rust 不在 `impl`/`trait`/`extern` 里的 `fn`。作用域取往上第一个函数类节点（具名或匿名，含箭头函数、函数表达式、lambda、闭包）的字节范围，没有则为整个文件；往上先碰到类、对象字面量、impl、trait 的是成员，记 None。`ParsedRelation.call_at`：裸调用（被调是标识符、无 import/包/`use` 路径/接收者元数据）的起始字节。都只在内存里。
- 解析阶段（`index_files.rs::visible_same_file_targets`）：同文件候选里，有作用域且作用域包含调用点的定义中，只留作用域最窄的；作用域不包含调用点的、被更内层同名定义遮蔽的去掉；没有作用域的候选（方法、成员）不动；一个可见的都没有时全部保留。只从原集合里去掉。
- 决定：
  - 不用"类也算作用域"：Python 方法里 `h()` 看不到类里的 `h`，Java/C++ 的隐式 `this` 能调到继承来的成员，按类算会删掉对的边。所以只覆盖 JS/TS、Python、Rust，且成员候选不参与。
  - Rust 的 `mod` 不算作用域：`use super::a::h` 能从兄弟模块引入，按 mod 算会删掉对的边。
  - JS 块作用域按函数算（更宽，只会少删）。
  - Python 的 `async_function_definition` 分支在 tree-sitter-python 0.23 里走不到（`async def` 解析成 `function_definition`），不在那里加代码。
- 测试：`a_bare_call_binds_the_same_file_definition_its_scope_sees`（14 行，正例 + 保持不变的行）、`a_same_name_member_is_left_to_the_other_rules`（6 行，改前的输出）、`a_scoped_same_file_bind_survives_an_incremental_run`。13 个单点回退里 10 个各让至少一行变红；没杀掉的 3 个：Python `async_function_definition` 分支（走不到，已删）、"一个都不可见时去掉全部"（被延迟阶段按名字重新绑定兜住）、Rust `impl` 成员判断（Rust 裸调用的形状规则已经先把关联函数排除了）。
- 实测（SCIP oracle，BEFORE = 1bcf622f，AFTER = 工作树，同一副本，`tasks/d714-resolver-precision/oracle_ab_lang.sh`），召回全部不变：

| 语料 | extracted 层 对/判定 | 默认下限精度 |
|---|---|---|
| claude-mem-lite | 4582/5043 → 4582/4588 | 94.84% → 99.64% |
| claudemd | 1072/1096 → 1072/1072 | 98.58% → 100.00% |
| gsd-lite | 586/588 → 586/586 | 96.47% → 96.62% |
| sgc | 614/618 → 614/616 | 99.45% → 99.57% |
| daagu 前端、hono、express | 不变 | 不变 |

- claude-mem-lite 剩下 6 条 extracted 错边不是词法孪生：`RealDate.now()` 绑到静态方法 `now`；裸调用 `read(rel)` 绑到 4 个对象字面量方法 `read() {}`（成员，规则不动它们）。JS 里裸调用永远调不到对象/类成员，可以另做。

## A4：内置方法名遇到类型未知的接收者，绑到项目里唯一的同名方法

- 审计：daagu inferred 层错边 225/245 是这一类（`dict.update`、`Path.exists` 绑到项目里唯一的 `update`/`exists` 方法）。
- 记忆 `feedback_simulate_a_relabel_per_language_first`、`feedback_narrow_the_result_not_the_pool`：先按语言列出内置类型的方法名表，用 judged dump 模拟"去掉这条边"对每种语言精度和召回的影响，再改；改结果不改候选池。

### A4 模拟与决定（2026-11-03）

- 模拟（A3 之后的 judged dump 加上索引里的边元数据；只看 `inferred` 层、接收者无类型的成员调用 `q:member` 或 `ur`）：

| 表 | daagu 后端 | nltk | stdlib | cloudinit（留出） | JS 5 个项目 |
|---|---|---|---|---|---|
| 全部内置类型方法名 | 去掉 223，对 5（`start`） | 去掉 219，对 206（`items`/`values`/`keys`/`sort`…） | 去掉 377，对 1 | 去掉 98，对 2（`items`） | 去掉 23，对 1 |
| 按原则选：`str`、集合运算、`pathlib.Path`、`re`、`datetime`，不含映射/序列/文件协议名和 `match`/`start`/`end` | 129，对 0 | 2，对 0 | 376，对 0 | 0 | 不做 |

- 决定：
  - 不删边，改标签（Phase 2e 标成 `ambiguous`）：还能用 `--min-confidence ambiguous` 看到；同 D#162 的做法。
  - 表按原则定，不按数据挑：从三个语料里挑"从没对过的名字"得到 41 个，在留出的 cloudinit 上一条都碰不到，说明不了什么。
  - 映射/序列/文件协议名不进表：nltk 的 `FreqDist.items()` 等是项目自己实现的（对 206 条），cloudinit 的 `items` 却错 96/98，取决于项目。daagu 的 80 条 `dict.update` 因此留着。
  - 只改 Phase 2e 已经重新判定的边（跨文件 + Python `ur`）：同文件 `extracted` 的这类边在 stdlib 里一半对（35 错 34 对，`pathlib` 调自己的 `joinpath`）。
  - JS 不做：数组/Map 方法名已经在噪声表里，剩下的只有 `toString` 21 条、`search` 1 条，收益小。
- 实测（BEFORE = a82c3b54 的源码树，AFTER = 工作树）与模拟完全一致：daagu `inferred` 7879/8126 → 7879/7997，默认下限精度 98.04% → 98.86%；stdlib 1469/2342 → 1469/1966，79.64% → 81.87%；nltk 1829/1984 → 1829/1982；cloudinit、claude-mem-lite、hono 不变；召回全部不变。
- 测试 `a_python_builtin_method_name_on_an_untyped_receiver_is_ambiguous`：`q:member`、`ur`、Python 限定三个条件各自回退都会让测试变红。INDEX_VERSION 130。

## A2 的剩余部分：非噪声名字经 barrel 的导入边

- gsd-lite：`init` 的导入边按名字连到了 `tests/round2-edge.test.js:48`，产生 42 条错边。
- D#712 已让 `js_import_targets` 跟进重导出，但导入边本身（`resolve_js_module_targets`）在指定文件里找不到名字时仍退回按名字解析。可以复用同一个跟进逻辑。

### A2 实现（2026-11-03）

- `resolve::js_export_through_reexports` 加一个"本文件里找名字"的参数：D#712 的调用仍只找导出的顶层函数（`JsFileExports::targets`），导入边找该文件里任何同名节点（和 `resolve_js_module_targets` 一样）。新函数 `js_import_edge_targets`。
- 延迟阶段第 3 步：具名导入（元数据没有 `q`）在指定文件里没找到名字、且指定文件在索引里时，先停放；其余关系都处理完后 `settle_js_reexports` 按轮绑定，直到一轮什么都没绑上（和 Python 的 `settle_python_reexports` 同一个做法：重导出的重导出要等里面那层的边先写进去）；没答上的再走一遍原来的按名字回退。
- 决定：重导出列表从数据库里重导出文件自己的 `imports` 边读，所以要等那些边存在；按轮重试 + 每绑一条就 `forget` 该文件的缓存。
- 测试 `a_js_import_through_a_reexport_binds_what_it_names`：具名重导出、`export *`、重导出的重导出（导入方在桶文件之前处理，要第二轮）、同目录的同名干扰项；全量索引后逐个文件改动再增量，结果必须不变。5 个单点回退（不停放、不 settle、不 forget、只一轮、不跟 `export *`）各自让测试变红。
- 实测（BEFORE = 644e8640 的源码树，AFTER = 工作树）：gsd-lite 召回 96.31% → 99.68%，默认下限精度 96.62% → 100.00%；hono 精度 97.34% → 97.44%，召回不变；其余 5 个不变。hono 去掉 134 条导入边、22 条调用边，gsd-lite 去掉 100 条、42 条，全部是同名多绑里错的那几个，没有哪个导入或调用变成一个目标都没有。
- INDEX_VERSION 131。

## 相关

- D#720：TypeScript `await f<T>(x)` 不产生调用关系。daagu 前端在 D#712 之后余下 40 条漏报，oracle 列出的 38 条全是这种写法。
