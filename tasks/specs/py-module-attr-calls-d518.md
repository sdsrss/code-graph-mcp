# Python `import P` + `P.X()` 经导入边绑定（D#518，接 D#515，2026-10-21）

状态：已实现（INDEX_VERSION 123 → 124），待评审。

## 背景

`import flask` 之后的 `flask.Flask(...)` 现在按名字解析：flask 语料里 47 个这样的调用（31 个跨文件 `ambiguous`，16 个绑到 `tests/test_config.py` 测试函数里的局部 `class Flask`）。D#515（`d44798b1`、`c0e7f62b`）按模块属性直接绑定，两轮评审后撤回（`b41ee796`）：答案读别的文件的定义，增量运行没有机制在那些文件变化时重新检查调用。

## 设计：把 `P.X()` 当作 `from M import X` + `X()`

`from M import X; X()` 这条路径的增量一致性已经被现有机制覆盖（D#370(a) 两轮 65 个场景 64/65，剩下的 p15 是方法形状，既有）。所以：

1. 解析器：文件里的名字 `P` 只有在它的每一次出现都是"绑定一个模块路径的普通 import"或"属性访问的对象"时才算模块名。对每个直接调用 `P.X(...)`（`function` 是 `attribute`，对象是标识符 `P`），调用元数据在原来的 `{"q":"module","v":M}` 上加 `"attr":"X"`；同时为每个 `(M, X)` 生成一条合成导入 `<module> imports X {"python_module":M,"attr":"X"}`。`count_python_import_sources` 不计这些合成导入。
2. 合成导入的解析与 `from M import X` 相同（模块文件里的同名节点，没有就跟随再导出），只有两处不同：模块自己的节点里不取方法（经再导出得到的照 from-import 绑）；都找不到时绑到 M 的 `<module>`，不走按名链。M 不是项目模块时与 `import M` 共用同一条哨兵边（去重）。
3. 调用照旧先按名解析，然后在后处理里：调用者文件有对应 `(M, X)` 的合成导入且目标不是 `<module>` 时，绑到导入目标并删掉这个调用其余的边（与裸调用经导入绑定 + 剪枝同一模式，但同文件的同名定义不遮蔽 `P.X`）。导入回落到 `<module>` 时调用保持按名解析的结果。
4. 区分合成导入：`ReexportEdges`（文件的再导出）、裸调用的唯一导入表、裸调用剪枝不读它们；置信度豁免（"调用者文件导入了这个目标"）只对带 `attr` 的调用读它们。
5. 增量：合成导入是 `imports` 边、带 `python_module`，重新定根（D#517/D#520）、再导出漂移、结构依赖都会把它当作导入处理。目标文件重新抽取时：绑到 `X` 的按名回填；回落到 `<module>` 的，若该文件现在有名为 `X` 的节点就重新排队解析，否则按名回填到新的 `<module>`。`P.X()` 调用按名回填，回填不到时走 deferred 重新解析而不进 pending（pending 按 (调用者, 名字, 语言) 唯一，会与同一调用者的裸 `X()` 合并，s18）。
6. 用户面：`domain::synthetic_import_sql` 一处定义，11 处查询（refs 两个分支、上下文字符串出入两个方向、health 的按语言统计、边计数（`count_user_edges`，经 relation 索引减去）、report 的置信度、deps/affected 的闭包与符号计数、cycles、map 依赖、dead-code）都 `AND NOT` 它。snapshot 复制整表（内部数据，不过滤）。

## 形状（先写语料，再实现）

接受（绑定改变）：

| # | 形状 | 期望 |
|---|---|---|
| A1 | `import pkg` + `pkg.X()`，`pkg/__init__.py` 定义 `X` | 调用绑 `pkg/__init__.py:X`，`inferred` |
| A2 | `pkg/__init__.py` 只有 `from .core import X` | 调用绑 `pkg/core.py:X`（跟随再导出） |
| A3 | `import a.b as m` + `m.f()` | 绑 `a/b.py:f` |
| A4 | 调用者文件自己定义了同名 `X` | 仍绑模块的 `X`（同文件定义不遮蔽 `P.X`） |
| A5 | 别处有同名 `X`（诱饵） | 只绑模块的 `X`，诱饵的边被剪掉 |
| A6 | `import flask` + `class Flask(flask.Flask)` 的测试局部类（flask 的 `tests/test_config.py`） | 绑 `src/flask/app.py:Flask` |

不接受（与现在相同，按名解析）：

| # | 形状 | 原因 |
|---|---|---|
| D1 | `P` 在文件里被重新绑定（赋值、参数、`for`/`with`/`except` 目标、`def`/`class` 名、`global`、`del`） | 不是模块名 |
| D2 | `P` 作为值使用（`f(P)`、`x = P`） | 规则只接受"属性的对象" |
| D3 | `from x import P`、`from x import y as P` | 不是普通 import |
| D4 | `import a.b`（绑定 `a`） | 保守：导入的是 `a.b` |
| D5 | 两个普通 import 绑同一个名字到不同模块 | 不唯一 |
| D6 | `P.a.X()`（多级） | 只处理直接调用 |
| D7 | `M` 不是项目模块（`import click; click.echo()`） | 照旧丢弃 |
| D8 | 模块里没有 `X`，也没有再导出 | 导入回落到 `<module>`，调用按名解析 |
| D9 | 再导出改名（`from .core import _X as X`）、`"n"` 多来源 | 不跟随（与 from-import 相同）→ D8 |
| D10 | `P.X` 不是调用（`cb = P.X`） | 只处理调用 |

增量（`parity.py`，每个都要与重建一致）：D#370(a) 两轮的 65 个场景（`/var/tmp/d370a-review{,2}/scn`），加上：模块新增 `X`（回落 → 定义）、删除 `X`（定义 → 回落）、再导出改目标、`__init__.py` 删除/加回、调用者文件不变而模块文件改名。

## 决策

- 2026-10-21：分级 L2（修正错误的默认回答；跨解析器与索引管线，超过 50 行）。全量索引会变，INDEX_VERSION 升一次。
- 模块查找最初与 from-import 完全相同（含方法）：标准库 oracle 多 38 条错的 `inferred`（`textwrap.wrap()` 同时绑方法 `TextWrapper.wrap` 等），p02（模块新增同名方法）增量不一致。只过滤模块自身节点里的方法（不过滤再导出结果，否则 cases5 的 cy__* 共 14 步在 0.175.0 一致的地方新出现不一致）后：oracle 只多 1 条错的，p02 一致，1,789 步里没有新的不一致。与 D#517 撤回的方法规则不同：那条规则有条件（有非方法时才去掉方法）且作用于 from-import 的回填；这里无条件、只作用于合成导入的直接查找。
- 实现中发现并单独提交 `5d2cdc3c`：大运行里早批次导入者的导入在晚批次文件去掉名字后丢失（0.175.0 第 2 轮 MEDIUM 2 的形状），D#518 的大运行场景依赖它。
- 去掉的代码：外部模块的合成导入特判（与 `import M` 的哨兵边去重，变异等价）、删除路径按属性名重排（不可达：导入者总是被删文件的结构依赖）、删除路径的 `P.X()` 调用绕过 pending（同理不可达）。
- 保留但变异存活：deferred 里"模块不再是项目模块就丢弃"（只在重排队路径可达，场景里未触发）、绑到 `X` 的合成导入按名回填而非重排（结果相同，只省开销）、漂移快照排除合成导入（只少拉文件）。

## 验证记录

- RED：解析器测试 `a_call_on_a_python_module_name_names_the_module_attribute` 在实现前失败（没有 `attr`）。管线测试在实现过程中逐个写、逐个失败后修（s18 的 pending 合并、大运行早批次、`<module>` 回落的回填）。
- 变异（`/var/tmp/d520/mut`，过滤 python/attr 相关测试）：两轮共 35 个，见 `/var/tmp/d520/mutate518*.log`；表面过滤 11 处每处都被 `the_synthetic_imports_of_module_attribute_calls_reach_no_answer` 杀死（用 `(NOT … OR 1)` 形式，第 1 轮的 `length('…')` 形式造成 SQL 语法错误，作废）；存活的 3 个见决策。
- 场景（最终 release `cgm-b5` md5 `ed54e951…`）：D#370(a) 两轮 65 个 + `big_run_attr` 共 66 个，65 一致（p15 既有）；0.175.0 两轮 1,789 步，262 步不一致，与只含 `5d2cdc3c` 的构建（`cgm-A`）完全同样的步，没有新不一致。flask 随机编辑 3×30 次：两臂都是 2/0/1 次（改名 = LOW 7，`typing.py` 自导入 ×2）。
- 全量（对 `cgm-r1`，即 D#520 之后、`5d2cdc3c` 之前）：非 Python 六个语料不变；flask −218 +135（合成导入 53），networkx −5 +5（合成导入 3,732），标准库 −1,695 +478（合成导入 1,806）。`5d2cdc3c` 不改全量（flask、networkx 相同）。
- SCIP oracle（标准库）：`inferred` 1,235/2,107 → 1,469/2,342，+234 对、+1 错（`gettext` 的 `operator.index`），被判对的边一条没丢；195 条由 `ambiguous` 升为 `inferred` 且正确。
- 用户面（flask，16 个 CLI 面）：callgraph Flask 不再隐藏 31 条 ambiguous、测试调用者 50 → 81；impact Flask 受影响测试 27 → 74；health 边数 11,928 → 11,845；deps/affected/map/refs Flask 随调用边变化；cycles、dead-code、surprising、refs/impact/callgraph url_for、callgraph make_response 不变。
- 开销（7 次交替中位数，对 `cgm-A`）：全量 CPU networkx 3.73 → 3.96 s、标准库 5.47 → 5.67 s（解析器多一遍游标遍历约占一半：只遍历不写合成导入的变体 3.91 s）；networkx 编辑 `cycles.py` 124 → 141 ms、`__init__.py` 120 → 149 ms（回填前 342 ms，改为"有同名节点才重排"）、`classes/graph.py` 504 → 510 ms（`P.X()` 调用改为按名回填前 620 ms）；flask 普通编辑 58 vs 51 ms。
