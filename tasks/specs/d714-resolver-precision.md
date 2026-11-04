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
  - ~~JS 块作用域按函数算（更宽，只会少删）。~~ 0.185.0 预发评审 M3 证伪：块内定义被当成整个函数可见，块外的调用就绑到块内定义、丢掉它真正调用的外层定义，删得更多。改为：嵌套块（不是函数自己的函数体）里的 JS `const`/`let` 声明和 Rust 项，作用域取那个块；嵌套块里的 JS `function` 声明返回 None，不参与收窄（非严格模式下块执行后也绑到外层函数）；`var` 仍取函数。`for (const f = …)` 循环头的声明后来也按循环算（v134）。
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

## 0.185.0 预发评审第 1 轮（2026-11-03）

- H1（已修）：只改 barrel 时导入方不重抽，增量和重建永久不一致（评审 s1/s2/s7/s12/s13）。修法照 Python 的 D#370(a)：`index_files` 记下本轮 JS/TS 文件的重导出行（名字、规格、q、目标文件），扇出时比较，导入了变动名字（或该文件 `export *` 集合变动）的文件重抽，逐轮传播。测试 `a_barrel_only_edit_rebinds_its_importers_as_a_rebuild_does`（5 种编辑）。
- M2（已修）：重导出遍历的上限从"总共访问 8 个文件"改为"8 层深"。测试 `a_wide_export_star_barrel_is_followed_past_eight_sources`。
- M3（已修）：见上面"块作用域"一条。A3 表格新增 5 行。
- LOW 处理：JS 变异存活补测试并验证能杀（grep 模式守卫、`^ {2}` 锚点）；D#713 只有 agent_id 的读者补断言；两处注释更正；其余写进 CHANGELOG 的"未覆盖"或收窄说法（D#720 的 `1 + await f<T>()`，A4 的构造器接收者，改名重导出不跟进，Python `global`，grep 跳过把 `echo`/版本号行当命中，启动修复遇并发写不重试）。

## 0.185.0 预发评审第 2 轮（2026-11-03）——停在评审上限，等用户决定

- 评审范围 `64f06762..c9225678`。HIGH-1：H1 的扇出跨不过 `export *`。星号行的值 `<module>␟spec␟star␟下一文件` 在下层变化时不变，上层 barrel 不算漂移；星号导入边目标名是 `<module>`，和下层漂移的具名名字对不上。用评审的 38 种编辑形状（`/var/tmp/agent-tmp-review2-0185/cases`，复制在 `/var/tmp/agent-tmp-oracle-m3/kit`，`ivf.sh BIN CASE`）复现：0.184.0 0/38 不一致，85986fff 6/38（star_chain_mid、star_chain_mid_gains_named、star_cycle、star_then_named、barrel_deleted、dir_import_file_deleted）。
- MEDIUM-1：增删命名空间导入或整模块 `require` 时，漂移名是 `<module>`，所有命名空间导入方都被重抽（claude-mem-lite 那种编辑 99–112 → 266–284 ms，边不变）。
- LOW：删除 barrel 不重抽导入方（按本轮之后的文件集解析）；深度上限加"每次查找只访问一次"依赖顺序；TS 合并命名空间被当嵌套块、丢正确边；测试钉不住块收窄和深度边界；`for_in_statement` 条目是死代码；四条文档说法不准（已改：gsd-lite 句子、带注解局部变量、grep 命中形状、测试注释差一）。
- 计划中的修法（未动手）：漂移按 `export *` 边做传递闭包（经星号转出漂移文件的文件也算漂移，继承名字）；匹配时排除 `<module>` 名；被删文件按运行前的文件集解析；TS `internal_module` 的块不算嵌套块；补只含块内调用的测试行和深度边界测试；删 `for_in_statement`。验收：38 形状 0/38，12 语料边集不变，然后第 3 轮增量评审（越过两轮上限，需用户同意）。
- 备选：按现状发版，6 种形状和过度拉取写进"未覆盖"，0.185.1 单独修。

## 第 2 轮之后：用户选 A（2026-11-03）

- 决定：用户回复"A"——现在修，38 形状 0/38、语料边集不变，再做第 3 轮增量评审（越过两轮上限，用户已同意），然后发版。
- 实现（`resolve.rs`）：
  - `importers_of_js_reexport_drift`：漂移名不再收 `<module>`（命名空间导入、`require`、默认导入不是重导出，遍历不读它们）；按 `export *` 边做传递闭包，经星号转出漂移文件的文件继承名字和星号标记，直到不再增长；导入方只看具名导入边（命名空间成员调用 `ns.f()` 绑该文件自己的节点，不走遍历）；规格同时按当前文件集和本轮开始时的文件集解析（快照表加 `existed` 列：本轮路径开始时是否在索引里）。
  - `js_export_through_reexports`：`seen` 改为"每个文件走过的最小深度"，从更近处再遇到时重走（LOW-2）；两个 `export *` 源找到同一组节点算一个绑定（重走后菱形会被找两次）。
- `treesitter.rs`：TS `internal_module` / `module` / `ambient_declaration` 的块不算嵌套块（LOW-3）；删 `for_in_statement`（LOW-7）。
- INDEX_VERSION 135，指纹重生成。
- 测试（先红后绿，红的输出在本会话）：屏障测试新增 7 种编辑（星号链中间换源、中间加具名、星号下具名换目标、星号环、删 barrel、删遮住目录 index 的文件、删星号源；前 6 种在旧代码上红）；`the_js_reexport_fanout_pulls_only_importers_a_rebuild_rebinds`（旧代码拉 `ns.js`、`req.js`、`star.js`）；`a_reexport_chain_is_followed_eight_files_deep_and_no_further`（旧代码也绿，钉深度边界）；`a_reexport_lookup_does_not_depend_on_which_export_star_comes_first`（旧代码"长路径在前"红）；A3 表加 6 行（块内/块后各一个调用、switch、Rust，TS 合并命名空间——最后一行旧代码红）。
- 验证：评审矩阵 38/38 一致（`cg-r2fix`，md5 1cf75fba；同一工具对 `cg-85986fff` 在 star_chain_mid、barrel_deleted 上仍报 DIFF）；11 个语料目录全量重建 85986fff 对 r2fix 边集差 0 行。
- 计时（6 次，`--no-embed`，0.184.0 / 85986fff / r2fix）：claude-mem-lite 在 `lib/git-state.mjs` 增删 `import * as` 112–121 / 285–313（重抽 18 个）/ 114–128 ms（不重抽）；hono 新增删一条 `export *` 57–70 / 87–102 / 93–105；删再恢复已有 `export * from './ssg'` 52–66 / 155–199 / 152–175；`hono-base.ts` 增删 import 99–110 / 90–109 / 95–114。
- 另做 6 种形状（同 stem 换扩展名、星号菱形、菱形变歧义、三层星号链底层删除、改名导入经星号链、`require` 解构经星号链）：r2fix 全一致，85986fff 全不一致。
- 变异（归档副本，独立 target，15 个）：14 个被杀；M15（忽略 `existed`）起初存活，加"同 stem 换扩展名"一例后被杀。M2（漂移名保留 `<module>`）输出等价、存活：它只省掉"本轮只动了 `<module>` 行"时对全部导入边的扫描，留着并注释。
- 成熟方案核对：TypeScript 增量构建按"文件导出签名变了 → 重查引用它的文件，传递"做（exportedModulesMap / 签名传播；其 issue #63579、#64591 也是 `export *` 链上的增量漏传）。本实现同一形状，不另引库。
- 下一步：第 3 轮增量评审（新 spawn，盲简报 `review3-brief.md`），范围 a2b24cbe..文档提交。

## 0.185.0 预发评审第 3 轮（2026-11-03）——用户已同意的最后一轮

- 评审范围 `a2b24cbe..1b7c3cf3`。结论：0 HIGH、2 MEDIUM、5 LOW。
- F1（MEDIUM，已修）：遍历把"已走过而跳过"、"两个 `export *` 源冲突"和"没有"都当成空结果；具名重导出的目标被跳过后落到 `export *` 源上。第 2 轮的深度表让一个合法 JS 形状从 `a.f` 变成没有边（a2b24cbe 是对的），另有两种形状随 `export *` 行的顺序变。修法按 ECMAScript `ResolveExport`：结果分四种（找到 / 没有 / 别处已答 / 歧义）；具名重导出得到"别处已答"就是答案，不再问 `export *` 源；`export *` 源冲突为歧义，向上传播；目标里没有这个名字时仍落到 `export *`（这里文件的普通 import 也读成重导出，宽松处理保留）。测试 `a_named_reexport_answered_elsewhere_is_not_retried_through_export_star`（旧代码：两行无边、冲突程序两种顺序绑到 `y.f`）。
- F2（MEDIUM，未修，D#739）：`export * from './x'` 写在 `x` 存在之前，`x` 出现后不跟进；未解析的星号/命名空间导入不留记录，0.184.0 也缺那条 barrel 自身的边。写进"未覆盖"。
- F3（LOW，未修）：TS 命名空间成员全文件可见，加回 0.184.0 那几条多余边；写进"未覆盖"，测试行注释写明钉的是这个近似。
- F4（LOW）：补测试——三层 `export *` 闭包（上层先扫）、出现的文件遮住目录 index、旧式 `module M {}` 行。
- F5（LOW，未修）：`export *` 集合一变，所有经该 barrel 具名导入的文件都重抽；claude-mem-lite `utils.mjs` 加一行 `export *`：90 个文件，1,026–1,077 ms 对 157–169 ms，边只多 barrel 自己那条（本会话复测）。写进 CHANGELOG。
- F6（LOW）：`ns.f()` 的说法改为"不经模块的重导出绑定"。F7（LOW，未修）：遍历把文件的 import 读成重导出，写进"未覆盖"。
- 变异中发现：具名重导出没答上时按名字回退绑到几个同名定义，同一个规格就有几行，第二次访问读成"别处已答"，把第一次的"没有"盖掉（普通 import 加 `export *` 的文件因此不再问 `export *` 源；r2fix 是对的）。修法：具名目标去重。测试加一行（旧代码无边，期望 `c.js` 的 `f`）。
- 变异（10 个）：N1–N9 全部被杀（具名"别处已答"不终止、星号歧义不传播、具名歧义落到星号、星号冲突忽略、"别处已答"不向上传、深度截断当"没有"、闭包只跑一轮、丢当前文件集、旧式 `module` 当嵌套块）；N10（去掉去重）编译不过，不算杀，证据是去重前那次红。
- INDEX_VERSION 136。
- 验证：`cg-r3fix`（md5 be00796f）：38 形状 38/38 一致；评审的 61 种形状 57/61 一致（余下 4 种全是 F2；第 2 轮二进制 55/61）；我的 6 种额外形状 6/6；11 个语料目录 r2fix 对 r3fix 边集差 0 行。计时（6 次，`--no-embed`，0.184.0 对 r3fix）：hono 新增删 `export *` 52–54 / 87–105 ms；删再恢复 `export * from './ssg'` 51–66（6 次里 1 次 110）/ 125–129；`hono-base.ts` 增删 import 76–86 / 77–86；claude-mem-lite 增删 `import * as` 107–115 / 108–118。
- 最终二进制 `cg-r3final`（md5 b566cc37，含去重）：38 形状 38/38；评审 61 种 57/61（余 4 种为 F2）；额外 6 种 6/6。
- 最终二进制 11 个语料目录对 r2fix 边集差 0 行。计时（6 次，`--no-embed`，0.184.0 对 r3final）：hono 新增删 `export *` 52–62 / 89–95 ms；删再恢复 `export * from './ssg'` 52–54 / 126–137；`hono-base.ts` 增删 import 78–88 / 78–85；claude-mem-lite `utils.mjs` 增删 `export *` 161–171 / 1,026–1,102（重抽 90 个）；`lib/git-state.mjs` 增删 `import * as` 113–122 / 110–113。CHANGELOG 用这组数。
