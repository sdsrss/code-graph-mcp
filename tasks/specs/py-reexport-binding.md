# Python 包的再导出绑定（D#370(a)，2026-10-20，0.174.0 之后）

状态：(a1) 已提交 `b7424176` + `d523c852`（INDEX_VERSION 119）；(a2) 完成待提交（INDEX_VERSION 120）。

## 背景与目标

用户要求"继续剩余工作，按你的建议执行"；上一份汇报建议下一步做 D#370(a)：flask 的 `Flask` 只找到 3 个调用者，实际约 80 个。

实测（`/var/tmp/d370a/c/flask`，HEAD `30017f46` 的 release 构建 `/var/tmp/d370a/bin/base`，md5 `737a1bfb`）：82 条指向名为 `Flask` 的调用边里 79 条指向 `tests/test_config.py:202`，那是测试函数里的局部类 `class Flask(flask.Flask)`；只有 3 条指向 `src/flask/app.py:109`。另有 15 条 `imports` 边绑到同一个局部类。两种调用形状：

- **形状 A**（31 条，`inferred`）：`from flask import Flask` 后 `Flask()`。`src/flask/__init__.py` 只有 `from .app import Flask as Flask`，模块查找在 `__init__.py` 里找不到 `Flask` 节点，掉进按名链；`refine_ambiguous_targets` 对测试文件里的调用者按最长公共路径前缀选中 `tests/` 下的局部类。导入边改对以后，现有的 bind + prune 后处理会把裸调用改指过去。
- **形状 B**（47 条：同文件 16 条 `extracted`、跨文件 31 条 `ambiguous`）：`import flask` 后 `flask.Flask()`。`{"q":"module","v":"flask"}` 的调用对项目模块"按裸调用解析"，同文件层先选中本文件的局部类。

索引里已经有 `src/flask/__init__.py` 的再导出边（`<module> --imports--> app.py:Flask`，`{"python_module":".app"}`），没有人沿着它走。

SCIP oracle 只统计指向函数/方法的调用，`Flask(...)` 是类构造，oracle 不判；测试里对 `flask` 的引用 scip-python 也没解析（`call_sites_to_unresolved_import` 226）。所以证据是全边差分 + 逐条读代码。

## 范围

分两次提交：

- **(a1)** `from P import X`（P 是项目模块，P 的文件里没有 X 节点）：沿 P 的再导出绑定。
- **(a2)** `P.X()`，接收者正好是 `import P` / `import a.b as P` 绑定的名字：绑定模块 P 的属性 X（P 的文件里定义的 X，否则 P 的再导出），不再走同文件层；标签不因同名节点多而降为 `ambiguous`。

不做（记入遗留）：星号导入 `from .m import *`；P 里赋值得到的 X（`X = ...`，没有节点）；`from P import sub` 导入子模块（绝对形式）；`a.b.X()` 这类多段接收者；`__getattr__` 模块；函数内嵌定义的作用域规则（另一个改进：586/1175 个 flask Python 类/函数节点嵌在函数里，跨文件调用指向它们 55 条）。

## 接受的形状（先写语料，再实现）

(a1) 重导出者 F 是 P 解析到的文件，导入者写 `from P import X`：

| # | F 的内容 | 导入者 X 绑定到 | 现在 |
|---|---|---|---|
| A1 | `from .m import X` | m.X | 按名链 |
| A2 | `from .m import X as X` | m.X | 按名链 |
| A3 | `from pkg.m import X`（绝对） | m.X | 按名链 |
| A4 | `from .m import Y as X`（改名导出） | m.Y | 按名链（找 X） |
| A5 | `from .m import X as Z`（X 没导出） | 不经这条边，照旧 | 可能经名字碰到 m.X |
| A6 | 链：P/__init__ `from .sub import X`，sub/__init__ `from .impl import X` | impl.X | 按名链 |
| A7 | try/except 两处导入 X | 两个都绑 | 按名链 |
| A8 | `from .m import *` | 照旧 | |
| A9 | `X = ...`（无节点） | 照旧 | |
| A10 | `from . import X`，X 是子模块 | 照旧（D10） | |
| A11 | 循环：a 从 b 再导出 X，b 从 a | 照旧（没有可验证的链） | |
| A12 | F 的 X 来自外部包 | 照旧 | |
| A13 | F 对 X 的导入本身掉进按名链 | 照旧（不跟随未验证的边） | |
| A14 | 相对导入者 `from . import X`（在 pkg/sub.py），pkg/__init__ 再导出 X | m.X（先再导出，再 D10） | D10 绑 `__init__.py` |

"可验证"：F 的导入边 `F.<module> → T`（`python_module` = Q）只有在 T 的文件属于 Q 解析到的文件，或 T 在那些文件里又是一条可验证的再导出时才跟随；深度上限 8。按名链得到的边永远不被跟随，所以全量和增量看到的一样。

(a2) 调用 `R.X()`：

| # | 形状 | 绑定 |
|---|---|---|
| B1 | `import P; P.X()`，X 在 P 的文件里 | P.X |
| B2 | `import P; P.X()`，X 由 P 再导出 | 再导出目标 |
| B3 | `import a.b as m; m.X()` | a/b.py 的 X |
| B4 | `import a.b; a.X()` | 照旧（模块行指向 a.b，不是 a） |
| B5 | `from P import m; m.X()` | 照旧 |
| B6 | `import P; P.sub.X()` | 照旧 |
| B7 | `import P; P.X()`，P 里找不到 X | 照旧，标 `amb` |
| B8 | 调用者文件自己定义 X，写 `P.X()` | P.X，不是本地 |
| B9 | 库模块 `import os; os.path.join()` | 照旧丢弃 |

## 设计

### (a1)

- 解析器：`from .m import Y as X` 的导入行元数据加 `"as":"X"`（只在别名与原名不同时）。别名相同或无别名的元数据不变。
- `python_modules.rs`：`python_reexport_targets(db, module_map, files, name)`：F 的 `<module>` 出发的 `imports` 边（有 `python_module`、不是 `is_module_import`），导出名 = `as` 或目标名，等于 X；按上面的"可验证"规则递归，深度上限 8，访问集防循环。
- 延迟阶段步骤 2：模块解析到文件但没有 X 节点时，先查再导出。查不到的先挂起；非调用关系处理完后对挂起的反复查到不再有进展（链和顺序都由此得到），剩下的照旧走 D10 / 按名链。
- 增量：重导出者的再导出集合（导出名、目标文件、目标名）在运行前后变化时，重新提取它的导入者（有模块行导入边指向它 `<module>` 的文件），循环到不再变化（链），上限 16 轮。快照与 `snapshot_definition_counts` 同时取。

### (a2)

- 解析器：`PY_MODULE_NAMES`（每文件）：`import a`、`import a.b as m` 绑定的名字，减去任何 from-import 也绑定的名字。调用 `m.f()` 的接收者正好是这样的名字时，元数据加 `"attr":1`。
- `CalleeMeta::Module { module, attr }`。批处理阶段：项目模块的 `attr` 调用一律延迟（命名空间包也延迟，好让回落带上 `amb`）。
- 延迟阶段：`ModuleAttributes::targets`（模块文件里定义的同名节点，否则 `python_reexport_targets`；按 (module, name) 记忆）→ 有就绑，元数据去掉 `amb`；没有 → 照旧的按名链，元数据加 `amb`。
- 标签：`q=module` 且有 `attr` 且无 `amb` 的边不因同名多而降为 `ambiguous`。
- 扇出：新定义（D9）加一臂：`"attr":1` 的调用边指向名字新增定义的节点 → 重新提取调用者；再导出漂移加一臂：`attr` 调用边或待定行，名字在移动名单里、`v` 解析到移动的模块 → 重新提取。

## 决策与进展

- 2026-10-20：分级 L2（修复错误的默认回答，不改契约；改动跨解析器与索引管线，超过 50 行，所以写本 spec）。INDEX_VERSION 每次提交升一次（118 未发版，用户仍只重建一次）。
- 选再导出跟随而不是"排除函数内嵌节点"：前者是正向绑定，同时修好 `Blueprint`（绑到两个类）、`flask.redirect()`（绑到测试里的内嵌函数）；后者只换掉错误候选，`Blueprint` 这类两个都是模块级的仍错。内嵌作用域另记。
- (a1) 实现：挂起的导入在非调用关系之后、调用之前结算（`settle_python_reexports`），剩下的回到同一个循环体照旧绑定（指针集合 `waited` 标记，循环体不复制）。快照在 `index_files` 里 Phase 0-pre 之后取（这样因依赖出现/消失而被拉进来的重导出者也比较），扇出在 `fan_out_to_new_duplicate_definitions` 里读；导入者用自己导入边的 `python_module` 解析，相对导入者也算。链：之后每轮重读漂移，上限 8 轮。
- 查找不缓存跨分支的结果：被循环截断的答案在不成环的分支上是错的；链深 ≤ 8、分支数通常为 1，代价可忽略。
- 性能（networkx 693 个文件实测后改）：结算里每次查找各查一次库，840 个挂起导入 × 2 轮，593 ms（全量 2.3 s 里）；建索引时还没有统计信息，规划器从 `idx_edges_relation` 扫全部导入边（每个文件 0.34 ms）。改为每文件读一次的缓存（`ReexportEdges`，绑定后对该文件 `forget`）+ 固定连接顺序（`CROSS JOIN`），结算 1 ms。
- 扇出收窄：原来拉进模块的全部导入者（flask `__init__.py` 加一行再导出 → 52 个文件，352–368 ms）。改为只拉"可能改绑"的：有一条导入边指向被移动名字（导出名或目标名）的节点，或相对导入回落到某个 `<module>`（D10，包自己的或子模块的——新的再导出会遮住子模块，Python 也如此）。7–9 个文件，128–144 ms。三个条件各有一个变异测试。
- (a2) 待定扫描（sweep）不做属性查找：写了又删。变异 M12（去掉扫描的属性查找）在每个能想到的场景都存活——扫描按名绑定的 `attr` 边要么被标 `ambiguous` 后由 D9 第一臂拉回，要么是指向新增定义的 `attr` 边由新加的 D9 臂拉回，要么由再导出漂移臂拉回，最后都由延迟阶段决定。扫描里给 `attr` 行补 `amb` 的小臂（M13）同样存活（来自延迟回落的行已经带 `amb`），一并删掉。
- (a2) 扇出的名字匹配收紧到同一条边（该边的 `python_module` 解析到移动的模块）；哨兵边没有 `python_module`，按文件级判断；D10 回落到子模块时要求子模块名在移动名单里，回落到包自身（不记名字）时照旧拉。改名才把目标名算进名单。效果（flask `__init__.py`）：加一行同名再导出 0 个文件 32–36 ms（之前 7 个、130–139 ms），`abort as abort2` 5 个 220–224 ms，改指向 `redirect` 6 个 95–100 ms；base 25–30 ms。
- (a2) 性能：每个 `attr` 调用各查一次库，networkx 全量中位 +6%（10,700 个 `attr` 调用）；按 (module, name) 记忆后 CPU 中位 3.87 s vs base 3.83 s（7 次交替）。D9 新臂原用 `LIKE '%"attr"%'`，会匹配 D10B 的 `{"ur":"attr"}`，改为 `'%"attr":1%'`。
- A7（try/except）：导入两个都绑；裸调用的 bind 只跟随唯一导入，于是 prune 去掉错误的邻近候选、不绑任何一个。不扩 bind（范围外），CHANGELOG 记入 Not covered。

## 验证记录

### (a1)

- RED（HEAD `30017f46` 代码）：`a_python_from_import_follows_its_package_reexport` 失败，缺 13 条、错 11 条（导入与调用全落在 `aaa/decoys.py` 的局部定义上，`pkg/user.py` 落在 `__init__.py` 的 `<module>`）。`a_python_reexport_binding_converges_incrementally_to_a_rebuild`（8 个场景）在旧代码上通过（守卫：旧做法不读别的文件）。
- GREEN：两个测试通过。中间状态：只有延迟阶段的跟随时，S2（重导出改指向）增量 ≠ 全量；加上扇出后 8/8。
- 变异（每个都能编译、都实际跑到）：去掉验证 → A13 场景失败；去掉扇出 → S2 失败；结算只跑一轮 → 两个测试都失败；忽略 `as` → 形状测试失败。第一次写的"去掉验证"因 `deny(warnings)`（未用变量）编译不过，改写后重跑。
- clippy（默认、embed-model，`-D warnings`）通过；指纹重新生成，`index_version_guard` 7/7。
- 语料全边差分（`edgediff.py`，base `737a1bfb` vs a1 `325d7063`，两臂都是源码 release 构建 + embed-model，各自全量建索引）：cgm/express/gin/gson/hono/leveldb 边集合相同、无标签变化、建索引耗时相同（0.1 s 精度）。flask：11,941 → 11,927 条边。去掉元数据后 −63 / +49：31 条调用 + 15 条导入从 `tests/test_config.py` 的局部 `Flask` 改到 `src/flask/app.py`；`cli.py` 的 `from . import Flask` 从 `__init__.py` 的 `<module>` 改到 `app.py:Flask`（A14）；16 条指向 `sansio/blueprints.py:Blueprint` 的边（9 调用 + 7 导入）去掉，16 个调用者都保留指向 `src/flask/blueprints.py` 的边；2 条导入因 `as` 不同成为同一目标的第二条边（`app.py` 函数内的 `from .testing import FlaskClient as cls`）；8 条只是元数据加了 `as`。4 条指向局部 `Flask` 的 `references` 从 `inferred` 变 `ambiguous`（文件的导入不再指向它）。
- 性能与收窄（`a1c` md5 `263c3b3a`）：七个语料与 a1 的边集合、标签完全相同。networkx 全量 5 次交替：a1c 1,649–1,773 ms（中位 1,668），base 1,642–1,728（中位 1,680）；普通文件编辑（3 个文件 × 5 次）a1c 83–122 ms、base 81–121 ms，不触发扇出。flask 改 `__init__.py` 再导出：base 25–27 ms，收窄前 352–368 ms（52 个文件），收窄后 128–144 ms（7–9 个文件）。
- 新测试 `a_python_reexport_fanout_reaches_every_importer_shape`（相对导入回落到包、被新再导出遮住的子模块、`<external>` 哨兵、改名再导出的目标名、没人导入的新再导出）；变异：去掉"名字"条件 → 哨兵与改指向场景失败；去掉 `<module>` 条件 → 相对导入场景失败；只用导出名 → 改名场景失败。
- 指向 `src/flask/app.py:Flask` 的调用边 3 → 34（(a1) 之后；(a2) 之后 81）（全部 `inferred`）；`callgraph Flask --direction callers --include-tests --depth 1` 3 行 → 28 行；`impact Flask` 受影响测试 2 → 27。剩下的 31 条 `ambiguous` + 16 条同文件是形状 B（a2）。

### (a2)

- RED（`d523c852` 代码）：`a_python_call_on_an_imported_module_binds_the_module_attribute` 失败：缺 4 条（`pkg.Widget()`→core、`pkg.helper()`→`__init__`、`core.build()`、自定义 `Widget` 的文件里 `pkg.Widget()`），错 4 条（落在 `aaa/decoys.py` 局部定义和调用者自己的 `Widget`）。增量守卫在旧代码上通过；实现到一半时它在"重导出改指向"场景失败（扇出没拉 `attr` 调用者），补上后通过。
- 新测试：上面两个 + `a_buffered_module_attribute_call_is_swept_as_the_attribute`（待定行在包新增定义后的结果与重建相同，含元数据）。增量守卫 8 个场景（调用者编辑、包新增自己的 `Widget`、改指向、删掉再导出、目标改名、删掉 `helper`、新增模块+再导出、两个新 `Missing` 同一次运行）。
- 变异：解析器不标 `attr` → 形状测试失败；去掉标签豁免 → 形状测试（`ambiguous` 断言）失败；去掉 D9 `attr` 臂 → 两个测试失败；去掉漂移的 `attr` 臂 → 增量守卫失败；漂移名字条件、`<module>` 条件、哨兵条件、改名目标名、子模块名、包自身回落各自去掉 → 对应场景失败（子模块名那条原来因同文件另一条导入被掩盖而存活，把子模块导入单独放一个文件后失败）。
- 语料（`edgediff.py`，a1c vs a2；a2 vs a2c/a2e 输出相同）：六个语料边集合相同、无标签变化。flask：去掉元数据后 −135/+52：47 条调用从局部 `Flask` 到 `app.py:Flask`（31 跨文件 + 16 同文件），5 条 `flask.redirect()` 从测试里的内嵌函数到 `helpers.py:redirect`，83 条第二条边去掉（`sansio/blueprints.py:Blueprint` 48、方法 `Flask.url_for` 32、`Flask.make_response` 3），83 个调用者都保留正确目标且 `ambiguous → inferred`。指向 `app.py:Flask` 的调用边 81 条全 `inferred`；`callgraph Flask --direction callers --include-tests --depth 1` 75 行（a1c 28、base 3）；`impact Flask` 受影响测试 74（a1c 27、base 2）。
- networkx（base vs a2f）：只有 1 条边改目标（`test_threshold.py` 的 `nxt.triangles()`，从 `test_cycles.py` 的内嵌函数到 `threshold.py:triangles`，对）；10,744 条只改元数据（10,715 条 `attr` 调用 + 29 条导入的 `as`）；5 条调用 `ambiguous → inferred`。10,715 条 `attr` 调用里 67 条经模块属性绑定，10,648 条回落（星号再导出）。
- SCIP oracle（flask，a1c 与 a2）：精确率/召回完全相同，且与 base 相同（extracted 195/205、inferred 89/107、ambiguous 19/60；R@inferred 258/300）——变动的调用都不在金标里（scip-python 没解析测试里的 `import flask`）。
- 性能（a2f md5 `e0be3389`）：networkx 全量 CPU 3.84–4.07 s（中位 3.87），base 3.72–3.90（中位 3.83），各 7 次交替；普通编辑 83–137 ms（中位 104），base 89–121（中位 105），不触发扇出。
