# 0.175.0 两轮评审剩下的 LOW（D#520，2026-10-21）

状态：LOW 5、6、11 已实现（全量索引不变，INDEX_VERSION 不升）；LOW 7 待授权。

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
4. LOW 7（待授权）：`pending_unresolved_calls` 唯一索引是 `(source_id, target_name, source_language)`，同一调用者两个同名、元数据不同的调用（flask `test_blueprint_renaming` 里 `bp.register_blueprint` 是 `{"q":"member"}`，`app.register_blueprint` 是 `{"q":"module","v":"blueprintapp"}`）缓冲时只留第一条。改成 `(source_id, target_name, source_language, COALESCE(metadata, ''))` 需要迁移旧库的索引。

## 决策

- 2026-10-21：分级 L2（恢复增量 = 重建；改动在 `src/indexer/` 一个模块内，加上 `src/graph/surprising.rs` 的文字）。全量索引不变，INDEX_VERSION 不升。
- LOW 3、8 不做，理由见上表；写进 CHANGELOG 的 Not covered。

## 验证记录

- RED：`a_new_python_module_on_a_module_path_rebinds_its_importers`（增量只绑 `tests/helpers.py`）、`a_package_init_that_appears_or_vanishes_rebinds_the_relative_imports_of_it`（删除时 `views.py` 仍绑 `tasks.py`）、`a_python_package_change_pulls_only_the_importers_it_reroots` 新断言（`[]`）、`the_ambiguous_reason_names_a_typescript_type_only_for_a_call_into_one`（Python 边写了 TypeScript）。GREEN 后全部通过。
- 变异（`/var/tmp/d520/mut`，独立 target）：M1 去掉新模块门槛、M2 相对判断恒假、M3 绑定臂不判相对、M4 哨兵臂不判相对、M6 去掉语言条件、M7 去掉类型条件、M8 原因文字忽略类型调用 → 都被杀死；M5 等价（见设计 2）。
- 场景（release 构建 `cgm-r1` md5 `dceb6c33…` 对 0.175.0）：第 1 轮 s1–s8 加 4 个改名场景 124 步，0.175.0 不一致 4 步、新版 2 步；第 2 轮 cases1–6 加 s3 共 1,665 步，331 → 328；新版没有一步比 0.175.0 差，剩下的都是方法 `Y` 与函数 `Y` 并存、类改名两类（0.175.0 的 Not covered）。D#370(a) 两轮的 65 个 `parity.py` 场景：两臂都是 64/65（p15，既有）。
- 全量：flask、express、hono、gin、gson、leveldb、本仓库、networkx（`git archive` 的干净副本）、标准库，两臂边集合、标签、元数据完全相同（标准库 116,373，networkx 36,776）。
- flask 随机编辑（`fuzz.py` seed 1–3，各 30 次）：0.175.0 不一致 6 次，新版 3 次（改名 1 次 = LOW 7；`typing.py` 自导入 2 次 = 7'）。
- 开销（7 次交替的中位数）：新增模块 标准库 113 → 117 ms、networkx 85 → 91 ms；删 `networkx/utils/__init__.py` 1340 vs 1302 ms（5 次，都重抽 186 个文件）；flask 删 `task_app/__init__.py` 36 vs 33 ms（2 → 3 个文件）；flask 普通编辑 48 vs 51 ms。
- `surprising --include-tests`：hono 1,748 条 `ambiguous` 中 73 条改为 TypeScript 类型的文字，其余 1,675 条为 several candidates；flask 883 条都是 several candidates。
