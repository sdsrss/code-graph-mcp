# Python 增量与重建不一致的三个形状（D#517，2026-10-21）

状态：已实现并提交（`bcba08f9`），INDEX_VERSION 122 → 123；随 0.175.0 发版。

## 背景

用户要求"把剩下能做的做了后发版"。上一份汇报的遗留第 5 项：D#370(a) 两轮评审的增量场景里，p15、p28、p29 在 BASE（`752afb86`）上就已经增量 ≠ 重建。用评审留下的 `parity.py`（全量 → 逐步增量 → 对最终树重新全量，比较全部边、元数据和标签）在 HEAD（`b41ee796` 的 release 构建 w1）上三个都复现。

## 形状（先写语料，再实现）

| # | 形状 | 重建 | HEAD 增量 | 原因 |
|---|---|---|---|---|
| p28 / I1 | `tests/` 新增 `__init__.py`；`tests/test_a.py` 有 `import helpers` | `<external>:helpers` | 仍指向 `tests/helpers.py` | `tests/` 成为包，不再是导入根；导入者文本没变，没人重新提取 |
| I2 | 反过来：删掉 `tests/__init__.py` | 指向 `tests/helpers.py` | 仍是 `<external>` | 同上，方向相反 |
| p29 / I3 | `src/` 新增 `__init__.py`；`app.py` 有 `import pkg`（`src/pkg/`） | `<external>:pkg` | 仍指向 `src/pkg/` | src 布局的根消失 |
| I4 | 反过来：删掉 `src/__init__.py` | 指向 `src/pkg/` | 仍是 `<external>` | 同上 |
| I5 | 包内的 `pkg/sub/__init__.py` 删掉 | 不变 | 不变 | 包内的普通目录不是根，模块表不变 |
| p15 | `from .m import Y as X`，`m.py` 新增 `class C: def Y` | 导入同时绑 `def Y` 和方法 `C.Y` | 只有 `def Y` | 重建一侧错：方法不是模块属性 |
| q3 | 同 p15，普通 `from pkg.m import Y` | 同上 | 同上 | 同上 |
| q1 | `m.py` 原来只有方法 `C.Y`，新增 `def Y` | 改后：只有 `def Y` | 回填保留 `C.Y` 并加上 `def Y` | 回填规则对方法按限定名保留 |
| q4 | `m.py` 只有两个方法 `C.Y`、`D.Y` | 两个方法都绑（照旧） | 同 | 只有方法时不变，避免掉进按名链 |

## 设计

1. `python_modules::changed_module_paths(before, after)`：两组 Python 路径各建一次模块表，返回文件列表不同的点号模块路径。
2. `index_files::python_root_change_importers(db, files, delete_paths)`：只在本次运行有 `__init__.py` 出现（索引里还没有）或消失时才计算；之前的路径取自 `files` 表，之后的 = 之前 − 删除 + 本次的 `.py`。拉取 (a) 有 `imports` 边且其 `python_module` 在变动集合里的 Python 文件，(b) `<external>` 哨兵名（即模块路径）在变动集合里的 Python 导入者。接在 `existence_change_dependents`（Phase 0-pre）末尾，排除本次运行和正在删除的文件。
3. `resolve_python_module_targets`：`from m import Y` 在模块文件里有多个候选、其中有非方法时，去掉方法。只有方法时照旧绑定（不让它掉进按名链，那里 C4 会排除方法，转去绑别处的同名函数）。候选不止一个时才查库（每个候选一次 `SELECT type = 'method'`）。
4. `restore_inbound_edges`：Python 来源的回填候选里有非方法时去掉方法，与第 3 条一致（q1）。

## 决策

- 2026-10-21：分级 L2（恢复增量 = 重建这一既定行为，同时修正重建侧对方法的错误绑定；改动在 `src/indexer/` 一个模块内，超过 50 行，所以写本 spec）。
- 方法过滤只在"有非方法候选"时生效：与 C4（v107，按名链里 Python 导入从不绑方法）方向一致，但不把只有方法的导入推进按名链。
- 门槛（只有 `__init__.py` 出现/消失才计算）只影响开销：去掉它结果不变（变异 M6 存活，预期）。
- D#515（`P.X()` 经导入边重做）本次不做。唯一能让增量依赖由现有机制跟踪的做法，是为每个 `P.X()` 写一条合成的 `imports` 边（未解析时回落到 P 的 `<module>`，P 的文件重新提取时重新解析而不是按名回填，结果变化才重新提取导入者）。代价是 `imports` 边进入约 30 处查询（`src/` 下 12 个文件：refs、死代码、map、project_map、上下文字符串与向量文本、surprising、MCP 工具），每一处都是用户能看到的回答，要逐个过滤或测量；同时 `ReexportEdges`、再导出漂移快照、`bind_calls_to_imported_targets`、`prune_import_contradicted_call_edges`、Phase 2e 的导入豁免都要区分合成边。不写边的方案就是两轮评审后撤回的那一版。按"语料先行、评审停止线、一任务一会话"，放到单独会话做；设计要点已记入延期项 D#515。
- D#370(c)（Go）同样不做：gin 上非测试文件 → `_test.go` 的调用边只有 2 条且都是 `ambiguous`；Go 调用没有任何元数据（4278 条全为空），标准库限定调用要先改解析器记录包限定符。

## 验证记录

- RED（桩函数返回空）：4 个新测试全部失败——`a_package_change_changes_the_module_paths_below_it`（`[]` vs `["helpers","test_a","tests"]`）、`a_python_package_change_pulls_only_the_importers_it_reroots`、`a_python_package_change_rebinds_the_imports_it_reroots`（p28：增量仍绑 `tests/helpers.py`）、`a_python_from_import_binds_the_module_level_name_not_a_method`（重建绑了函数和方法）。
- GREEN：4/4；q1 一开始失败（回填同时保留方法和函数），加上回填规则后通过。
- 全量 `cargo test`（pre-commit）：lib 1497/1497，其余测试二进制全绿；clippy 默认与 embed-model 两条腿通过。
- 变异（`git archive` 副本，独立 target，过滤 `python` 的 73 个测试）：M1 去掉拉取、M3 去掉哨兵臂、M4 去掉绑定臂、M5 哨兵不限 Python、M7 不过滤方法、M8 只有方法也去掉、M9 回填不过滤、M10 所有路径都算变动、M11 删除不算 → 各自有测试失败；M6（门槛恒开）存活，只影响开销。M8 第一次存活：只有方法的夹具只有一个候选、过滤分支没跑到，改成两个方法后失败。
- 增量场景（`parity.py`，debug 构建 `9a1e1f1f` vs HEAD w1）：两轮评审的 65 个 + p15/p28/p29 副本 + 新增 q1–q4、i2、i4、i5，共 71 个场景 74 次运行：新代码全部一致；HEAD 不一致 9 次（p15、p28、p29 各两次，i2、i4、q3）。
- 语料全边差分（`edgediff.py`，两臂都是已提交树的 release + embed-model 构建：w1 `02175983`（`b41ee796`）对 a `abb76729`（`bcba08f9`），各自全量）：七个语料与 networkx 边集合相同、无标签变化；标准库（569 个文件）去掉 19 条（14 条导入 + 5 条调用），全部指向方法，每个导入者/调用者都保留指向同名模块级函数的边，没有新增。
- SCIP oracle（标准库，两臂都重跑）：w1 `inferred` 1235/2107（与记录的基线相同），a 1235/2104；判定边的差别只有 3 条 `inferred` 错边被去掉（`pydoc.html_error`、`random._test_generator`、`xmlrpc/server.docroutine`）。
- 语料增量一致性（`corpus_parity.py`：索引 → 改动 → 增量，对比同一棵树的全新索引）：flask 新增 `tests/__init__.py`，w1 增量多 152 条、少 6 条，a 完全一致（重新提取 5 个文件）；networkx 删除 `networkx/utils/__init__.py`，两臂都一致（186 个文件，由已有的删除依赖覆盖）。
- 开销（D#516 循环暂停时测）：flask 新增 `tests/__init__.py` 91–160 ms（中位 105），w1 37–45 ms；networkx 删除 `utils/__init__.py` 两臂 1.27–1.39 s；普通编辑 flask 54 vs 57 ms、networkx 128 vs 125 ms（各 5 次）；全量 CPU networkx 中位 3.99 vs 4.22 s（7 次交替），标准库 5.55 vs 5.59 s（15 次交替）。

## 发版前评审第 1 轮（0.175.0，新开子代理，盲审 `v0.174.0..9f834cc3`）

1 HIGH、1 MEDIUM、9 LOW；场景与工具在 `/var/tmp/d175-review/harness/`。

- HIGH（成立，我用 `parity.py` 复现）：`restore_inbound_edges` 的方法过滤对所有 Python 关系生效，方法所在文件同时有同名函数（或嵌套闭包）时，指向方法的调用和引用（`self.store.fetch_rows()`、`m.fetch_rows()`、裸调用）在该文件重新索引后不回填；标准库 10 个文件各加一行注释缺 624 条，flask 改 `__init__.py` 的再导出缺 1 条（`View.as_view → AppGroup.decorator`）。修复 `d45dad19`：只对 `imports` 生效。RED：新测试 `an_edit_keeps_the_calls_into_a_method_beside_a_namesake_function` 在修复前失败；GREEN 后，标准库 10 文件实验 0/0、flask 场景一致。变异：去掉语言条件（M19）、恢复成所有关系（即本 HIGH）都被新测试杀死（加了一个 TypeScript 场景钉住语言条件）。全量索引不变（9 个语料 a vs b 边集合相同），INDEX_VERSION 不升（123 未发版），只更新指纹。
- MEDIUM：CI 红是三条 2026-10-19/20 的 RustSec 公告（rustls-webpki、webpki-roots、webpki-root-certs），与代码无关；升级生产依赖需要用户授权，已提问。
- LOW 3（嵌套函数算作模块级）、8（`__init__.py` 函数内的导入被当作再导出）、5/6（既有的增量缺口）：写入 CHANGELOG 的 Not covered，不修（LOW 的修复是发版里风险最高的代码）。
- LOW 4（删 `__init__.py` 时再导出扇出多 23 个文件，说明里没写）、9（元数据新增 `as`/`n` 没写）：按实测补进说明。基准改用 v0.174.0 源码构建（评审的 `aef147c7`），networkx 改用干净副本（`/var/tmp/d370a/nx/A` 的 `__init__.py` 有一行未提交的再导出，评审指出）。
- LOW 10（`query_row` 对已删除的 id 会中止运行，评审找不到触发方式）、11（`surprising` 的原因文字对所有语言都写了 TypeScript）：记入遗留。

### 修复后的测量（最终构建 `925c1488`，`d45dad19`）

- 全量：七个语料 + 干净 networkx + 标准库，与 `abb76729`（`bcba08f9`）边集合完全相同。
- 对 0.174.0（`aef147c7`）：flask 新增 `tests/__init__.py` 88–109 ms vs 34 ms；删 `networkx/utils/__init__.py` 中位 1249 vs 1018 ms（186 + 23 个文件 vs 186 个）；普通编辑 flask 52 vs 49 ms、networkx 113 vs 117 ms；全量 CPU networkx 3.68 vs 3.61 s（9 次交替，每组都更高），标准库 5.40 vs 5.32 s。
- 元数据：0.174.0 不写 `as`/`n`；最终版 flask 10/3、标准库 51/18、networkx 29/0。

## D#516（同一会话）

- hono 全量循环约 17,910 次：4 次 `Invalid column type Blob`（其中 1 次 `integrity_check` 报索引缺行）+ 1 次段错误，全部在 04:53–05:35；同时段内核有 virtio-gpu vblank 超时（vCPU 停顿）。此后各臂（w1、0.174.0、`RAYON_NUM_THREADS=1`、glibc `malloc.check=3`、`malloc.perturb`）约 15,500 次 0 失败。
- networkx：0.174.0 源码构建全量一次失败（07:3x，`code_content` 成 BLOB）——问题在已发布版本里就有。
- 特征：一个节点行的一列被存成 BLOB，字节或与原文本相同（只有类型变了），或是残留 C 字符串 `"typescript\0"`、零字节、含堆指针的垃圾；与 `Mem` 标志被改写一致。插入走缓存的 `INSERT … RETURNING id`，表上有 FTS5 AFTER INSERT 触发器。
- 补救（同版本实测）：值损坏、索引结构完好 → 再跑索引或 `rebuild-index --confirm` 都能修好；表索引也坏了 → 再跑两次都报 malformed，`rebuild-index --confirm` 能修好。写入 CHANGELOG 的 Known issue。
