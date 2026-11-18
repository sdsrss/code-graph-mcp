# 2026-11-17 D#726：Python 经改名导入的调用绑定到导入的那个定义

状态：**已实现，分支 `d726-d884`，变异与四个语料差分已做，待发版前评审、发 0.198.0**。授权：用户 2026-11-17“把剩余工作都做了，授权清理和发版”。

## 问题（2026-11-03 实测，见 D#726 延期条目）

`from m import a as b` 之后的 `b()` 不绑定任何东西：解析器把它记成对 `b` 的裸调用，项目里没有名为 `b` 的节点；Phase 2d-bind（`bind_calls_to_imported_targets`）只会改指已经存在的边。daagu backend（SCIP 金标 16,082 对，0.184.0+D#712+D#720）：360 对漏掉的里 49 对是裸调用，其中 29 对是改名导入（`parse_date_param as _parse_date_param` ×15、`wilson_ci` ×3、`is_limit_up/down` ×4 等）。

## 方案：照 JS D#120 的形状

JS 的改名导入调用（`member::js_renamed_import_call`）被记成导出名的调用，元数据 `{"q":"imp","js_module":s,"v":a}`，解析器在批次、延后、pending 清扫、inbound 恢复四条路径上都只按“该模块导出的 a”绑定，从不按名字到别处找（`CalleeMeta::Import`）。Python 加一个平行的变体：

| 层 | 改动 |
|----|------|
| domain | 新常量 `CALL_Q_PY_IMPORT = "pyimp"`，文档说明元数据 `{"q":"pyimp","py_module":m,"v":a}` |
| 解析器（Python 调用抽取） | 裸调用 `b(...)`，若 `b` 的最近绑定是模块级 `from m import a as b`（`a != b`）：记成对 `a` 的调用并带上述元数据。最近绑定：从调用往外，任一外层函数作用域绑定了 `b`（参数、赋值、def、import、`global`/`nonlocal` 另算）就不是；类体直接绑定了 `b` 也不是；模块级 `b` 的所有绑定都必须是同一个 `(m, a)` 的改名导入，另有 `def b`、`class b`、赋值、`for`/`with` 目标、其他导入，或任何函数里 `global b`，都不算 |
| resolve.rs | `CalleeMeta::PyImport { module, export }`；pending 名 `export\u{1f}module`（同 JS，pending 表按 (caller, name, lang) 唯一） |
| 批次（index_files.rs ~2427） | `PyImport` 推迟到延后阶段（模块文件可能在后面的批次） |
| 延后阶段（~4517） | 目标 = `project_module_files_from(module, 调用方路径, map)` 里名为 `a` 的非方法节点（`module_attribute_ids`），再加 `python_reexport_targets` 跟随转导出；外部模块（None）→ 不建边、不进 pending；项目模块但没有 `a` → pending |
| pending 清扫（resolve.rs ~353） | 同一个解析函数；仍无 → 留在 pending；外部 → 删行 |
| inbound 恢复（~4010） | 与 JS `Import` 一样走 `renamed_import` 分支：不按名字恢复，交给 pending 重新解析 |
| INDEX_VERSION | 141 → 142（解析器输出变了，需重建） |

## 不覆盖（计划）

- 直接写在类体里的调用按模块级判断（与 `local_value_calls` 一致），类体自己绑定同名时也按导入绑定。
- 星号导入（`from x import *`）不读：它在改名导入之后重新绑定同名时，调用仍按导入绑定。
- 改名导入的是模块级变量（`handler = make()` 后 `from m import handler as h; h()`）：只绑 `def` 与类，不建边。
- 函数内的改名导入（`def f(): from m import a as b; b()`）。
- `import m as n; n.f()`、`from m import a` 后 `a()` 等已有路径不变。
- 名字噪声表上的非改名导入（D#712 的 Python 半边，2026-11-03 实测 0/360，不做）。
- 成员调用（360 对里的 311 对），另一类问题。

## 计划

1. RED：解析器形状表测试（接受与拒绝的各种绑定形状）；解析器端到端测试：全量索引后 `b()` 有边指向 `m.a`；同文件也定义了 `a` 时不绑本地 `a`；外部模块不建边；增量：改名/删除 `m.a` 后边消失并进 pending，再加回来绑上；模块文件后建（pending 清扫）。
2. 实现上表。
3. 全量测试、clippy 两腿、fmt；变异。
4. 语料：本机能找到的 Python 语料（daagu 若在 `~/dev/` 下）0.197.0 对本次构建的 calls 边差异，逐条看新增边是否都是改名导入且目标正确，删掉的边为 0。

## 实施中的修正

- **按文件一次扫描，不逐调用找祖先**：`python.rs` 写明 `Node::parent` 从根往下找，逐调用找祖先是深度平方（曾有测试 60 秒超时）。改为扩展已有的 `local_value_calls` 一次扫描（改名为 `scope_scan`），多收集“最近绑定在模块级”的调用与全文件的 `global` 名，再与模块级改名导入表求交。
- **Python 调用走 `extract_python_call`**：第一稿插在通用调用路径（JS 的 D#120 所在处），Python 不经过那里；改到 `extract_python_call` 开头。
- **守卫的预期变化**：`edge_coverage` 的 Python 层级清单多了 `calls inferred use_alias.py:build -> cache.py:Cache`（夹具 `from cache import Cache as NewCache`），pending 积压 1 → 0，两处按守卫自己的说明更新；`index_version_guard` 的抽取指纹重录（INDEX_VERSION 已提到 142）。

## 验证记录

- RED（385b1d9c 的 archive 副本只放入新测试）：`test_python_renamed_import_binds_the_export` 失败于 go_date / go_ci / go_h 都是 `[]`；`test_python_renamed_import_changes_match_rebuild` 失败于不空断言（`[]` 对 `["app/pkg/impl.py:function:load"]`）。GREEN：两个都通过。
- 全量 `cargo test --no-default-features --no-fail-fast`：2508 通过、2 失败（上面两个守卫的预期变化），更新后 `edge_coverage` 14/14、`index_version_guard` 7/7。
- 语料差分（daagu f32defc5 的 archive 副本，1,487 个文件；0.197.0 代码 cb8b7096 nodef md5 ed57c670 对本分支 nodef md5 d47ac9a2）：Python calls 边 54,445 → 54,479；新增 35 条全部带 `pyimp`，逐条核对目标文件就是导入语句的模块对应的文件且在模块顶层定义了该名（35/35）；删掉 1 条：`run_stock_picker.py main -> config.py load_active_config`，源码是 `load_active_config_standalone as load_active_config`，旧边按名字绑到了另一个同名函数，现在绑到 `data_providers.load_active_config_standalone`。
- 语料差分（0.198.0 前；0.197.0 nodef 源码构建 12feb709 md5 77d75f2e 对本分支 nodef md5 2f7315ff，各自 `incremental-index --no-embed`，按 caller/callee 比较 calls 对，`/var/tmp/agent-tmp-d726/c2/`）：
  - CPython 3.14 标准库副本（569 个 .py，按 stdlib 语料的打包方法）：99,552 → 99,536 对；新增 27 对全部 `pyimp`、`inferred`，逐条 grep 导入行核对（`_strptime` 的 `re_compile/re_sub/re_escape`、`collections` 的 `_itemgetter/_recursive_repr`、`random` 与 `_pydatetime` 的 `_index`、`smtplib` 的 `encode_base64` 等）都指向导入所指的定义；删掉 43 对（8 `inferred`、35 `ambiguous`）：42 对是旧的按名错边（`math`/`_thread`/`itertools` 的 C 模块函数被改名后绑到项目里同名的 `Logger._log`、`Lock`、`Barrier._count` 等；`smtplib` 的 `encode_base64` 原绑 `email.encoders`），1 对是 `ntpath._readlink_deep → genericpath.islink`：`ntpath.py:23` 先 `from genericpath import *`，`:851` 在 try 里 `from nt import _path_islink as islink`，星号导入不读，现在只绑 `nt`（外部）→ 无边；`nt` 没有该函数时运行的正是 `genericpath.islink`。写进 CHANGELOG Not covered。
  - nltk 3.9.2（`/usr/lib/python3/dist-packages/nltk`，358 个 .py）：新增 8 对，导入行逐条核对；删除 0。
  - flask d086db85：无变化。
  - daagu f32defc5（1,255 个 .py）：新增 35 对，与此前逐条核对过的 35 对集合相同；删除 1 对（同前）。
- 版本不一致提示（flask 副本）：新二进制在 v141 索引上查询打印 "This index was built by an older code-graph (index v141, this binary v142) …"，`incremental-index` 打印 "Index version mismatch (stored v141 ≠ binary v142): clearing + rebuilding"；0.197.0 在 v142 索引上 `incremental-index` 退出 1 并提示删除 `.code-graph/index.db*`；`serve` 收到 `notifications/initialized` 后重建（日志 "Index version changed (141 → 142)"，meta 里 v 变为 142）。
- Not covered 各条在小夹具上实测（`/var/tmp/agent-tmp-d726/fx`）：类体里 `y = x()` 在类属性 `x = 3` 下仍绑到导入；函数内的改名导入无边；模块级变量导出无边（Python 不建变量节点：标准库索引里只有 class/function/method/module 四种节点）；`from m import b as a` 后接 `from n import *`（n 定义 `a`）绑到 `m.b`，0.197.0 按名绑 `m.a` 与 `n.a`（ambiguous）。
- 变异（`/var/tmp/agent-tmp-d726/mutate_d726.py`，git archive 副本 + 独立 target，跑 lib 里名字含 python 的 96 个测试和 `edge_coverage` 14 个）：第一次驱动脚本按 `test … FAILED` 行判死，`-q` 下没有这种行，19 个全报存活，作废重跑。19 个变异：初版测试杀死 P1（模块级 def 不阻断）、P2（global）、P3（两个改名导入）、P7（批次不延后）、P10（pending 清扫按名）、P12（不跟重导出）、P13（同时记按名调用，`python_resolution_tier_inventory_is_pinned` 杀）；P14–P19（模块级 for/with/海象/普通导入/`import … as`/赋值不算绑定）存活，补夹具后全部杀死（3b1eaa1c）。仍存活：
  - P4（`from m import a as a` 也走改名路径）：有意的范围（同名别名留给普通导入路径）；能区分两者的输入要用到 D#298 的已知缺陷（外部包的名字按名绑到项目同名函数），不把缺陷写进测试。
  - P5（外部模块也写 pending）：等价——同一次索引的 pending 清扫对外部模块删行（`resolve.rs:415-417`）。
  - P6（不按节点类型过滤）：等价——Python 只建 class/function/method/module 节点，method 的限定名带类名，被 `qualified_name = name` 排除。
  - P8（inbound 恢复按名）：恢复对带元数据的调用已按限定名过滤；补了“重导出删除且目标文件同时改动”一例仍相等，未找到能区分的输入。
  - P9（pyimp 不豁免置信度降级）：试了一层、两层包重导出加同名函数，导入表已为目标作证，走不到豁免；保留与 JS `imp` 对称。
  - P11（延后阶段空结果不进 pending）：未找到能区分的输入（“export 回来后重新绑上”那一例在变异下也通过）；推测调用方在模块文件改动时经由别的路径被重新解析，未核实。
