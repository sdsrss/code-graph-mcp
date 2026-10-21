# 0.175.0 两轮评审剩下的 LOW（D#520，2026-10-21）

状态：LOW 5、6、11 已实现（全量索引不变，INDEX_VERSION 不升）；LOW 7 已授权（2026-10-21），已实现，随 0.176.0 发布。

## 背景

用户要求"继续完成剩余的工作"。D#520 记的是 0.175.0 发版前两轮评审的 LOW。方法规则（D#517）在第 2 轮后撤回，所以先在 0.175.0 发布的二进制（`~/.cache/code-graph/bin/code-graph-mcp`，md5 `f42613e2…`）上逐条重跑评审的场景（`/var/tmp/d175-review/harness/s1–s8.py`，复制到 `/var/tmp/d520/harness/`），再用 flask 随机编辑 30 次（`fuzz.py`，seed 1）找同类。

## 逐条结论（0.175.0 上重跑）

| # | 内容 | 0.175.0 | 处理 |
|---|---|---|---|
| 3 | 嵌套 `def` 算作模块级（`from m import register` 绑到 `make()` 里的 `register`） | 绑方法和嵌套函数两个（与 0.174.0 相同） | 不做：与撤回的方法规则同一类，回填/分批都要跟着改，答案依赖目标文件的状态 |
| 8 | `__init__.py` 函数内的 `from .heavy import X` 被当作再导出 | 绑 `heavy.X` 一个（0.174.0 绑两个） | 不做：去掉跟随后落回按名链，绑两个，更差；正确答案要改"模块里没有这个名字"的回落 |
| 5 | 删掉再加回 `pkg/__init__.py` 后 `from .. import X` 仍是 `<external>/..` | 增量多 1 少 2 | 修 |
| 5' | （fuzz 新发现）删掉 `task_app/__init__.py` 后 `from . import tasks` 仍绑 `tasks.py` | 增量多 1 少 1 | 修，同 5 |
| 6 | 根目录新增 `helpers.py`，`tests/test_a.py` 的 `import helpers` 没加上它 | 增量少 2 | 修 |
| 7 | 文件改名后 6 条 `calls[ambiguous]` 缺失 | 增量少 6 | 需要授权（改表索引），见下 |
| 7' | （fuzz）删掉再加回 `src/flask/__init__.py` 后 `typing.py` 自己的 `import typing` | 增量少 1 | 不做：自导入不留边，要么重抽整个根下的文件 |
| 10 | `query_row` 没有 `.optional()` | 代码已随方法规则撤回 | 无需处理 |
| 11 | `surprising` 的原因文字对所有语言都写 TypeScript | 成立 | 修 |
| 第 2 轮 LOW | 方法规则相关 | 已撤回 | 无需处理 |

## 设计

1. LOW 6：`python_root_change_importers` 的门槛从"有 `__init__.py` 出现或消失"放宽为"或有本次新出现的非 `__init__` Python 模块"。之后的计算不变（`changed_module_paths` 比较前后两张模块表）。新文件只会改变它自己的模块路径：新根目录只能来自它自己的上级目录，别的文件经过新根的前提是它们已经让这个目录成为根。删除不用管：绑到被删文件的导入者已经是结构依赖。
2. LOW 5/5'：本次出现或消失的 `D/__init__.py`（`D` 非空），拉取相对导入解析到 `D` 的 Python 文件，包括已绑定的边（`python_module` 以 `.` 开头）和 `<external>` 哨兵（名字以 `.` 开头）。解析与 `project_module_files_from` 共用一个函数，只算路径。
   实现时去掉了"`changed` 为空但有包变化"的分支：`__init__.py` 出现或消失时它自己的模块路径就变了，`changed` 不会为空（变异 M5 存活，是等价变异）。
3. LOW 11：原因文字按边判断：调用、目标是 `interface`/`type`、目标文件是 TypeScript 时写 TypeScript 类型，其余写"several candidates"。判断与 `classify_edge_confidence` 的 `TS_TYPE_CALL` 一致。
4. LOW 7（2026-10-21 授权）：`pending_unresolved_calls` 唯一索引是 `(source_id, target_name, source_language)`，同一调用者两个同名、元数据不同的调用（flask `test_blueprint_renaming` 里 `bp.register_blueprint` 是 `{"q":"member"}`，`app.register_blueprint` 是 `{"q":"module","v":"blueprintapp"}`）缓冲时只留第一条。改成 `(source_id, target_name, source_language, COALESCE(metadata, ''))` 需要迁移旧库的索引。
   实现：`create_tables_sql` 的定义改为带 `COALESCE(metadata, '')`（与 `idx_edges_unique` 同形）；打开数据库时 `schema::widen_pending_unique_index` 读 `sqlite_master` 里的定义，没有 `metadata` 才在一个事务里 DROP + CREATE，所以只写一次。不升 SCHEMA_VERSION：新索引接受旧索引接受的每一组行，0.175.0 打开迁移后的库时自己的 `CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_unique` 按名字不做事，照常工作；升 SCHEMA_VERSION 会让旧版本拒绝打开、health-check 报 UNHEALTHY（`test_open_adds_composite_file_name_index_to_an_older_db` 的同一理由）。同时覆盖 D#518 第 1 轮的 LOW-1（同一函数里 `m.X()` 与裸 `X()`）。

## 决策

- 2026-10-21：分级 L2（恢复增量 = 重建；改动在 `src/indexer/` 一个模块内，加上 `src/graph/surprising.rs` 的文字）。全量索引不变，INDEX_VERSION 不升。
- LOW 3、8 不做，理由见上表；写进 CHANGELOG 的 Not covered。
- 2026-10-21：LOW 7 获授权，分级 L3（索引结构迁移）。D#519 按方案 (a) 结案：保留已知问题（用户回复"把后面的做了"，按报告里的推荐理解）。

## 验证记录

- RED：`a_new_python_module_on_a_module_path_rebinds_its_importers`（增量只绑 `tests/helpers.py`）、`a_package_init_that_appears_or_vanishes_rebinds_the_relative_imports_of_it`（删除时 `views.py` 仍绑 `tasks.py`）、`a_python_package_change_pulls_only_the_importers_it_reroots` 新断言（`[]`）、`the_ambiguous_reason_names_a_typescript_type_only_for_a_call_into_one`（Python 边写了 TypeScript）。GREEN 后全部通过。
- 变异（`/var/tmp/d520/mut`，独立 target）：M1 去掉新模块门槛、M2 相对判断恒假、M3 绑定臂不判相对、M4 哨兵臂不判相对、M6 去掉语言条件、M7 去掉类型条件、M8 原因文字忽略类型调用 → 都被杀死；M5 等价（见设计 2）。
- 场景（release 构建 `cgm-r1` md5 `dceb6c33…` 对 0.175.0）：第 1 轮 s1–s8 加 4 个改名场景 124 步，0.175.0 不一致 4 步、新版 2 步；第 2 轮 cases1–6 加 s3 共 1,665 步，331 → 328；新版没有一步比 0.175.0 差，剩下的都是方法 `Y` 与函数 `Y` 并存、类改名两类（0.175.0 的 Not covered）。D#370(a) 两轮的 65 个 `parity.py` 场景：两臂都是 64/65（p15，既有）。
- 全量：flask、express、hono、gin、gson、leveldb、本仓库、networkx（`git archive` 的干净副本）、标准库，两臂边集合、标签、元数据完全相同（标准库 116,373，networkx 36,776）。
- flask 随机编辑（`fuzz.py` seed 1–3，各 30 次）：0.175.0 不一致 6 次，新版 3 次（改名 1 次 = LOW 7；`typing.py` 自导入 2 次 = 7'）。
- 开销（7 次交替的中位数）：新增模块 标准库 113 → 117 ms、networkx 85 → 91 ms；删 `networkx/utils/__init__.py` 1340 vs 1302 ms（5 次，都重抽 186 个文件）；flask 删 `task_app/__init__.py` 36 vs 33 ms（2 → 3 个文件）；flask 普通编辑 48 vs 51 ms。
- `surprising --include-tests`：hono 1,748 条 `ambiguous` 中 73 条改为 TypeScript 类型的文字，其余 1,675 条为 several candidates；flask 883 条都是 several candidates。
- LOW 7：RED `two_unresolved_calls_of_one_name_are_buffered_apart`（修前增量缺 `app.py.f --calls[inferred]--> m.py.X`，即裸 `X()`）；`opening_an_older_db_puts_metadata_in_the_pending_unique_key`。变异：去掉打开时的调用 → 行数 1 ≠ 2；去掉"已是新定义就返回" → 第二次打开 schema_version 35 ≠ 33。两个都被杀死。
- LOW 7 差分（`/var/tmp/d521`，release 构建 `cgm-pre176` = f31bbf5e md5 `e3c2482c…`，`cgm-low7` = 8f3f1670 md5 `b17a79c3…`；第一次构建 low7 因 `git archive` 的 mtime 被 cargo 复用、md5 相同，touch 后重建）：
  - 全量：十个语料（d446 的七个、networkx、标准库、本仓库 8f3f1670 树）两臂边逐行相同。pending 行数 flask 921 → 923、hono 3,005 → 3,026、networkx 8,834 → 8,848、标准库 8,556 → 8,560。
  - D#518 第 1 轮 994 个场景 1,205 步：0.175.0 362 步、pre176 276 步、low7 270 步不一致；low7 没有一步是 pre176 或 0.175.0 一致的；修好 a11 第 1–3 步、d05 第 3、4 步、d370a-review2 s23 第 1 步。剩下与本项相关的 d05 第 2 步只差一条 pending 行（边相同），a09 第 2 步 0.175.0 起就有。
  - 0.175.0 两轮 1,789 步：两臂都是 262 步，逐步裁决相同。D#370(a) 65 个 + big_run_attr：两臂都是 65/66（p15）。第 2 轮 twofile2–4：两臂相同（D#542 的形状）。
  - flask 随机编辑 3×30：pre176 2/0/1，low7 1/0/1；seed 1 op6 改名 `sansio/blueprints.py` 少 6 条（`tests/test_blueprints.py` 六个测试函数进 `Blueprint.register_blueprint`）修好；剩下 2 次是 `typing.py` 自导入（7'）。
  - 降级：low7 建的 flask 索引，0.175.0 `health-check`、`callgraph` 退出码 0（照旧打印"newer index"警告），`rebuild-index --confirm` 建出旧定义的索引；low7 再打开（health-check，读者路径）即改为新定义，增量运行后 INDEX_VERSION 124、pending 923。

## 0.176.0 发版前评审（第 1 轮，新子代理，盲审 `f6e8c932..d7a18c5d`：HIGH 0 · MEDIUM 3 · LOW 6）

逐条复核后的处理：
- F1 MEDIUM 成立（复现：0.175.0 建的 flask 索引 `chmod 444` 后 low7 的 `health-check`/`callgraph`/`show` 退出码 1，`attempt to write a readonly database`；另一连接持 `BEGIN IMMEDIATE` 8 秒时 low7 `health-check` 退出码 1，`database is locked`；f31bbf5e 都是 0）。修：只在索引器打开（`revalidate`）时转换；读者打开不写。RED：db 测试加"读者打开后 schema_version 不变"（修前 33 ≠ 31）。`03b01c4f` 构建（`cgm-fix2` md5 `36e0f6c2…`）上同样的复现全部退出码 0，读者不改定义，`incremental-index` 后转换，0.175.0 仍能读、能 `rebuild-index --confirm`。
- F2 MEDIUM 成立：改名那条腿在改名前对 `q.py` 没有边，什么也没钉住。改为 `q.py` 里的方法 `K.X`（`m.X()` 与 `o.X()` 两条边都进它，即 flask 的形状）；单独跑这条腿，旧键下失败（缺 `{"q":"member"}` 那条）。
- F3 MEDIUM 成立：flask 随机编辑的数字是"引入新差异的编辑数"（0.175.0 6、f31bbf5e 3、本版 2），差异会延续，"之后不一致的编辑数"是 33 / 32 / 20（自己从日志重算，与评审一致）。两处 CHANGELOG 都写明两种数。
- F4 LOW 成立：pending 行数补全十个语料（本仓库 12,181 → 13,320 最多），并写开销：本仓库编辑 `src/storage/db.rs` 中位数 306 → 307 ms（15 次）、300 → 308 ms（41 次），自己测。
- F5 LOW 成立：第 2 轮 101 步里本版在 0.175.0 一致的 6 步不一致（q03、r11 = Not covered 的形状；q20、q23、q24、r06 = 多留一条合成导入，D#542 (2)，f31bbf5e 起就有）。q20 自己复现：增量 2 条合成导入、重建 1 条，7 个命令输出逐字节相同。CHANGELOG 句子改为写明这两个例外，Not covered 加一条。
- F6 LOW 成立：1,789 行里 863 行是第 0 步（与自身比较），比较步是 926（不含改名 922）；CHANGELOG 分母改为 926 / 922 / 改名 4。
- F7 LOW 成立（并发首次打开各自重建，结果相同）：只改文档措辞，不改代码。
- F8 LOW 成立：三处注释改为历史原因。
- F9 LOW：M6b 成立，加 `a_fresh_db_buffers_a_call_without_metadata_once`（M6b 下 2 ≠ 1）。M13（编辑路径重排按调用者和名字去重）是否有可达形状未验证：登记待办。
- 范围外：本仓库 Rust 随机编辑 30 次里 16 次增量与重建不一致（f31bbf5e 与本版相同；恢复删除的文件后带路径的 calls/references 缺失，如恢复 `src/indexer/pipeline/mod.rs` 后少 96 条）：登记待办，不在本版修。
