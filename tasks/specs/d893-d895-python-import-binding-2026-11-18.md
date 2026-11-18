# 2026-11-18 D#893 + D#895：Python 改名导入不再佐证原名；增量里重新出现的定义要刷新回退导入

状态：**D#893 第四版已发布 0.200.0（tag v0.200.0 在 04760d4e，2026-11-18；CI 7/7，Windows 首跑因 Chocolatey 取 ripgrep 失败重跑一次；Release 10/10 用时 33 分 45 秒；npm 6 个包发布者 GitHub Actions、trustedPublisher github、带签名证明；发布版 linux-x64 md5 75cebab3 在五个语料与 probe 树上与源码构建 head4 逐边相同）；D#895 已发布 0.199.0。** 授权：用户 2026-11-17“把剩余工作都做了，授权清理和发版”，2026-11-18“D#893按你建议执行，只记录、没有修的要修”。

## 为什么

### D#893
`from m import a as b` 之后再 `from n import a`，裸调用 `a()` 同时连到 `m.a`（错）和 `n.a`，两条都是 `inferred`。延期条目写的是“没有指向 `n.a` 的边”，实测（0.198.0 发布版与本分支，abs/rel/顺序颠倒/平铺模块/模块级调用 6 种写法）都是“多一条错边”，以实测为准。

根因（`src/indexer/pipeline/resolve.rs`）：后处理用的导入投影 `cg_imports` / `cg_unique_imports` 按 `COALESCE($.imported, 目标名)` 给导入取名，Python 改名导入在这里仍叫原名 `a`；`prune_import_contradicted_call_edges` 的“没有导入这个目标”和 `classify_edge_confidence` 的“导入佐证”只比目标 id、不比名字。于是 `m.a` 那条按名错边既没被“导入矛盾”剪掉，又被当成“有导入佐证”标成 `inferred`。

### D#895
评审 R3 NOTE-4：重导出链 `p → p.sub → p/sub/impl.py`，先把 `impl.py` 清空，再在下一次增量里重新定义 `a` 并删掉 `q/impl.py`，增量比重建少 `go_b1`/`go_b2` 两条 pyimp 边。

缩小（本分支构建的二进制，逐个序列对比）：与 `if True:` 无关，与 `q` 包无关；只要同一次运行里删掉**另一个定义了 `a` 的文件**（没人导入的 `r.py` 也算）就触发，只改 `r.py` 内容不触发。根因：`bare_name_callers_of_new_duplicates` 的 `cg_fanout_up`（“本次运行新增了定义的名字”）按 (名字, 语言) 把整次运行的计数相加，`impl.py` +1、`r.py` −1 相抵，`a` 不算新增，`module_fallback_sql` 就不刷新停在 `impl.py` `<module>` 节点上的 `p/sub/__init__.py`。与记忆里“增量过程要记集合，不要记计数”同类。

## 改动

- `IMPORT_EDGE_NAME`（新常量）：导入绑定的名字 = `COALESCE($.imported, $.as, 目标名)`；`cg_imports.nm` 与 `cg_unique_imports.import_name` 共用。`$.as` 在 imports 边上只有 Python 改名导入写（`imports.rs` `aliased_import`，别名与原名不同时）；`P.X()` 的合成导入只有 `python_module` + `attr`。
- 剪枝的 NOT EXISTS 与置信度的佐证都加 `(nm = 目标名 OR module IS NULL)`：Python 导入只为它绑定的名字作证；非 Python 导入照旧。
- `cg_fanout_before` / `cg_fanout_after` 按 (路径, 名字, 语言) 计数，`cg_fanout_up` 取任一路径计数上升的 (名字, 语言)，DISTINCT。

## 决定

- 只对 Python 生效（`OR module IS NULL`）：JS 有同类缺陷，两种形态都实测到（经改名再导出 `export {x as y}` + `import {x}`；直接 `import {x as z}` + `import {x}`，两版都是两条 `inferred`）。第二种 JS imports 边根本不记本地别名，要改解析器（抽取指纹），不在本次；第一种去掉限制即可修，但会改 JS 结果，本次不扩。记为 D#897。变异 M5/M6（去掉限制）因此存活，这是有意的。
- 按路径计数会让“同一次运行里定义在文件间移动”也触发一轮重新抽取（以前相抵不触发）。开销见验证记录。

## 验证记录

- RED（修前）：`test_python_renamed_import_does_not_bind_its_original_name`：views/rev 的 `go` 各多一条 `app/m.py:a inferred`，only.py 只有 `m.a inferred`（应为两条 `ambiguous`）。`test_python_reexport_chain_refilled_matches_rebuild`：两步后增量 `[]`，重建两条 pyimp。
- GREEN：两测试通过；D#893 测试含 7 种单步增量与“n.py 后到”“改名导入后到”两种序列，D#895 测试含评审序列、只删同名文件、同一定义在文件间移动三种。
- 变异（每个跑完整 lib 套件）：M1（`cg_imports` 不用别名）、M2（剪枝不比名字）、M3（佐证不比名字）、N0 对照（别名完全不进名字）均被杀；M4（`cg_unique_imports` 不用别名）首轮存活，补 tests/ 夹具（按名优先非测试文件，只有绑定阶段会加错边）后被杀；M5/M6 存活，见决定。D#895：P1（`cg_fanout_up` 改回整次运行求和）被杀；P2（去掉 DISTINCT）存活，下游是 EXISTS 或去重后的路径集合，输出等价，保留 DISTINCT。
- 语料（`/var/tmp/agent-tmp-d893/`，0.198.0 发布版 md5 606829a7 对本分支 nodef，各自 `incremental-index --no-embed`，calls 行含置信度与 metadata）：标准库 569 个 .py 100,389 → 100,352（−38/+1）；nltk 24,060、flask 3.1.0（ab814966）1,836、本仓库 3cc4f480（Rust/JS/Python）14,092 逐行相同；私有应用 daagu 10b3ce5d 55,351 → 55,348（−4/+1）。逐条读源码：删掉的 42 行都是按改名导入原名绑上的错边（argparse/getopt/optparse 的 `_` 36 行，`_pyrepl/readline.py` 的 `Console`，random 的 `mean` 两行，daagu 的 `AIProvider as AIProviderSchema`、`CL3Signal as CL3SignalModel` 4 行），新增 2 行都是别名指向的定义（`fmean as mean`、`load_active_config_standalone as load_active_config`）。v143 构建与 v142 构建在五个语料上逐行相同。
- 升级提示（v143 构建对 0.198.0 建的 flask 索引）：查询打印 “This index was built by an older code-graph (index v142, this binary v143) …”，`incremental-index` 打印 “Index version mismatch (stored v142 ≠ binary v143): clearing + rebuilding …”，与 CHANGELOG 一致。
- 增量开销（各一次）：daagu HEAD~200 → HEAD（526 个文件，20 个改名、17 个删除）5.964 → 5.706 s；本仓库 v0.180.0 → v0.198.0（669 个文件）3.049 → 2.878 s。两边与全量重建的差距相同（daagu 2 条、本仓库 2,389 条，全是 `references`，已有问题，记 D#898）。
- D#894 量级：四个语料里 Python imports 边绑到嵌套定义的：标准库 11 条（都是 `from os import fsencode/fsdecode`，os.py 用 `fsencode, fsdecode = _fscodec()` 把嵌套函数绑到模块级，这 11 条是对的），flask 12 条（测试函数里的 `class Module`，错），nltk、daagu 0。D#894 建议的“imports 边跳过嵌套节点”会丢掉标准库这 11 条，故不做，留延期。
- 函数内改名导入（新构建）：`def f(): from app.m import a as b; return b()` 与另一函数 `g` 里的 `b()` 都绑 `m.a`（0.198.0 都按名绑 `other.b`），写进 Not covered。

## 提交

- a0e73ed0：D#893（INDEX_VERSION 142 → 143，指纹按只含 D#893 的树重录）。
- 下一个提交：D#895（指纹按完整树重录，仍为 v143）。

## 评审 R1（新起的子代理，盲审 v0.198.0..44eebf41）：HIGH 0 / MEDIUM 4 / LOW 3 / NOTE 5

| # | 结论 | 处理 |
|---|------|------|
| MEDIUM-1（实测） | 绑定阶段（`cg_unique_imports`）按别名登记后，借一条“恰好有个无关的同名 `b`”留下的按名边把 `go → m.a` 绑上；那个 `b` 删掉或改名后增量留着这条边，重建没有（s8/s8c/s18/s22/s24，0.198.0 都 0/0） | 修（只减不增）：绑定阶段不读 Python 改名导入（`$.as IS NULL`）。改名导入的调用归 D#726 的 pyimp；剪枝与置信度仍按别名。评审 16 个序列在修复版上全部 0/0。测试加函数内改名、参数、try 里模块级三种写法及三个增量序列 |
| MEDIUM-2（实测） | CHANGELOG “`b` 是唯一导入名时 `b()` 绑 `m.a`，和普通导入一样”不成立：没有无关 `b` 时无边，有时是 `ambiguous`，三个阶段对别名边意见不一 | 随 MEDIUM-1 修复后该句删除；改写为实测：这些 `b()` 在 0.198.0 都按名绑到无关的 `other.b`，现在无边（fl/param/tried/另一个函数 4 处，两版实跑） |
| MEDIUM-3（实测） | D#895 的开销说法不成立：我引的两组运行扇出两版相同（改动没起作用），时间差是构建差异；移动文件时扇出 7 → 285、耗时 3.41 → 6.54 s | 修：只有相对导入回退到 `<module>` 的那一支按文件计数（`cg_fanout_up_file`），其余分支（按名调用方、Python/JS 导入、Rust use 两支、re-export）恢复整次运行求和。先试过“所有导入类分支按文件”（head2）：变异 RB3–RB6 存活，绝对导入 mi1/mi2、JS js1 在 0.198.0 上本来就与重建一致，只有相对回退 mi3 不一致，故收窄到这一支（head3）。head3：五个移动/互换场景扇出与 0.198.0 相同（7、21、2、9、2），标准库三包移动三次中位 3.472 → 3.431 s；评审 16 个序列 + mi1/mi2/mi3/js1 全部 0/0。测试 `an_ordinary_edit_does_not_drag_callers_into_a_fanout_round` 加“定义在文件间移动不重新抽取调用方”一臂 |
| MEDIUM-4（实测） | “36 条 `_()`”应为 35（argparse 19 + getopt 4 + optparse 12） | 改 CHANGELOG 与 INDEX_VERSION 注释 |
| LOW-1（实测） | 私有应用还有 4 条 `references` 从 inferred 改为 ambiguous，说明只写了 calls | 改为比较所有关系；写进 CHANGELOG（逐条核对都是改名导入原名上的错边，如 `DataSource as ProviderDataSource`） |
| LOW-2（实测/读码） | 审计守卫抓不住 `env: CARGO_HOME` + 仓库里的 audit.toml、`make audit`、`rustsec/audit-check` 的 `ignore:` | 开放域，不改；守卫覆盖 `--ignore ID`、`--ignore=ID`、项目 `.cargo/audit.toml` 与 ci.yml 里出现 `audit.toml` 字样，记在发版手册 |
| LOW-3（实测） | 变异 PM4（绑定阶段不读别名）、FM4（计数下降也扇出）存活 | PM4 即本次修复方向，现由新夹具区分；FM4 是开销，不改 |
| NOTE-1 | 参数调用 `def go(b): b()` 的 `ur` 边被别名绑定替换 | 随 MEDIUM-1 修复不再绑定；现在该按名边被剪掉（与普通导入同） |
| NOTE-2 | 按文件计数顺带消除了移动场景下与重建的大量差异（三包移动 873 行） | 修复后回到 0.198.0 的差异（三包移动 1,292/69、互换 132/5、nltk 14），写进 Not covered，记 D#899 |
| NOTE-3 | `CODE_GRAPH_PARSE_TIMEOUT_MS=1` 时 round-two 测试里一个小 `parse_tree` 失败 | 输入很小，不改 |
| NOTE-4 | `IMPORT_EDGE_NAME` 落在 `bind_calls_to_imported_targets` 游离文档块下面 | 常量移到游离块之上（游离块本身是旧问题） |
| NOTE-5 | daagu 增量比重建慢、`references` 差异 | 已有问题（D#898） |

修复后（cg-head2，与最终的 head3 在五个语料上逐行相同）对 0.198.0，所有关系：标准库 −38 calls（35 `_` + `Console` + 2 `mean`），+0；私有应用 −4 calls、4 条 references inferred→ambiguous；nltk、flask、本仓库逐行相同。`fmean`、`load_active_config_standalone` 两条新增边随绑定阶段收窄消失。

## 评审 R2（新起的子代理，只审修复提交 88a3895f）：HIGH 0 / MEDIUM 2 / LOW 4 / NOTE 3

| # | 结论 | 处理 |
|---|------|------|
| MEDIUM-1（实测） | D#893 的剪枝按别名认“矛盾导入”：函数内 `from pkg.m import a as b; b()` 的唯一按名边（无关的 `other.b`）被剪掉；之后 `m.py` 不再定义 `a`，重建按名绑 `other.b`，增量没有任何边可触发重新解析。随机 1,500 个序列里只有新版分歧 12 个（0.198.0 一致）；44eebf41 上已有 | 到停止线（R1、R2 各有 MEDIUM）：撤回 D#893，不再打补丁 |
| MEDIUM-2（实测） | 说明里“38 行”是按限定名去重的调用方–被调方对，按边是 108 条（`_common.py` 有 3 个 `_`） | 随 D#893 撤回，该段删除 |
| LOW-1（实测） | D#895 标题说“文件间移动的定义的导入会重新解析”过宽：绝对导入按名绑到别处的 `a`，一次运行把 `a` 从第三个文件移入所指模块，0.198.0 与本版都差 2/2 | 标题收窄为“相对导入”；该形状写进 Not covered（两版实跑 low1 都是 2/2） |
| LOW-2（实测） | 星号导入 + 改名导入 + 裸 `a()`：0.198.0 对，D#893 两边都 `ambiguous` | 随 D#893 撤回消失 |
| LOW-3（实测） | “`b()` 失去按名边”只在导入落到项目节点时成立 | 随 D#893 撤回，该句删除 |
| LOW-4（实测） | 存活变异：其余导入类分支按文件、回退分支不比语言、求和子查询不比语言、不 DROP `cg_fanout_up_file` | 不改：其余分支按文件属 R1 已撤的方向；求和子查询的 `b.lang IS a.lang` 与 0.198.0 的连接条件相同，同样没有测试钉住（同名在两种语言里同时变化的运行才会不同）；回退分支去掉语言条件只会多刷新导入方，结果相同；`cg_fanout_up_file` 在下一次 `drop_fanout_temps` 里删除 |
| NOTE-1 | 函数内改名导入 44eebf41 绑对、R1 修复后无边 | 随 D#893 撤回回到 0.198.0 |
| NOTE-2（实测） | 我的 3.431/3.472 s 是源码构建对发布版；源码对源码 +2.0% 以内 | 说明里删掉计时，只留扇出文件数 |
| NOTE-3 | 绑定阶段的游离文档块落在 `build_imports_temp` 上 | 随撤回回到 0.198.0 的原样 |

## 撤回后的验证（head4 = v0.198.0 + D#895 的 7 块，md5 见发版记录）

- resolve.rs 只反向应用 D#893 的 6 块（常量、导入投影、绑定两块、剪枝、置信度），domain.rs 与 v0.198.0 逐字相同（INDEX_VERSION 142），tests.rs 去掉 D#893 测试与夹具；抽取指纹在 142 下重录（D#895 只影响以后的增量运行，全量结果不变，不需要提高版本号）。
- 全量索引：五个语料每条边（关系、两端类型/限定名/起始行、置信度、metadata，保留重复）与 0.198.0 发布版逐行相同：标准库 113,891 条边 / 17,954 个节点，nltk 30,258 / 7,578，flask 3,705 / 1,796，私有应用 77,250 / 20,773，本仓库 28,605 / 10,907。
- 增量序列（评审 R1 的 16 个 + mi1/mi2/mi3/js1/low1/r2m1）：0.198.0 一致的，head4 全部一致；s11 3/4 → 0/0、mi3 1/1 → 0/0；low1 两版都 2/2。
- 五个移动/互换场景：扇出与 0.198.0 相同（7、21、2、9、2），与重建的差异逐项相同（1,292/69、0/0、14/0、0/0、132/5）。

## D#893 下一次的起点（留在延期条目）

- R1：绑定阶段按别名登记会借无关同名边绑定，增量分歧；R2：剪枝按别名认“矛盾导入”会剪掉调用唯一的边，之后无触发。
- 候选设计：改名导入对绑定、剪枝（矛盾与保护两边）、置信度三个阶段都不可见。它修 `from m import a as b; from n import a; a()`（`m.a` 不再受保护被剪），不会再按别名剪 `b()`；代价：只有改名导入时的裸 `a()` 会多出 `ambiguous` 的按名边。需单独发版、单独评审，先用评审的 1,500 序列模糊测试对照。

## D#893 第二版（2026-11-18，授权：用户“D#893按你建议执行”）

### 改动
- `cg_imports` 加一列 `alias = $.as`（只有 Python `from m import a as b` 且 b ≠ a 时写）。
- 剪枝两侧（“导入了同名的别的节点”的 EXISTS、“导入了这个目标”的 NOT EXISTS）都加 `alias IS NULL`；置信度的导入佐证加 `i.alias IS NULL`；绑定阶段 `cg_unique_imports` 加 `$.as IS NULL`。
- 不改：字段调用剪枝（`FIELD_CALL_PRUNE_PREDICATE`）与局部值调用（`py_local_value_call`）的“调用方文件从目标文件导入了任何东西”——改名导入仍是从那个文件导入，去掉会改别的行为。
- 与撤回的第一版的区别：第一版把改名导入按别名 `b` 登记（R1、R2 的分歧都来自按别名绑定/剪枝 `b()`）；这一版既不按 `a` 也不按 `b` 登记，`b()` 的结果与 0.199.0 相同（D#726 的 `pyimp`，否则按名）。
- INDEX_VERSION 142 → 143（全量结果变：私有应用 12 条）；抽取指纹重录 21812da4。

### 决定
- 代价：文件里只有改名导入、没有别的 `a` 导入时，裸 `a()` 不再被改名导入保护/佐证，回到按名猜测（两个同名定义时两条 `ambiguous`，0.199.0 是 `m.a` 一条 `inferred`）。Python 语义上 `a` 没被这个导入绑定，按名猜测是诚实的答案。
- 随机序列里新版独有的分歧全部是已有缺陷被改名导入掩盖（见验证），不在本次修；记为新延期条目（见遗留）。

### 验证记录
- RED（修前，HEAD 22314a2a 的 resolve.rs）：`test_python_renamed_import_does_not_bind_its_original_name` 失败：views/rev 的 `go` 多 `app/m.py:a inferred`，only.py 只有 `m.a inferred`。GREEN：两个测试通过（第二个是 13 个单步/两步增量对照重建，修前修后都通过，作回归护栏）。
- 变异（每个都由断言杀死）：M1 剪枝 EXISTS 读别名 → only.py 丢 `n.a`；M2 剪枝 NOT EXISTS 读别名 → views/rev 留 `m.a ambiguous`；M3 置信度读别名 → only.py `m.a inferred`，且增量与重建分歧；M4 绑定读别名 → tests/ 夹具多 `tests/helpers.py:a`。
- 三种 `b()`（函数内改名导入、参数、try 里模块级）两版实跑相同：`other.b` inferred / ambiguous(ur local) / inferred。
- 语料（0.199.0 发布版 md5 c67a84f2 对 head1，各自全量 `incremental-index --no-embed`，所有关系，含两端行号、置信度、metadata，保留重复）：标准库 113,891、nltk 30,258、flask 3.1.0（ab81496）3,705、本仓库 22314a2a 28,623 逐行相同；私有应用 daagu 10b3ce5d 77,250 → 77,246：删 4 条 calls（`llm_providers.py` 两个函数里 `AIProvider(...)` 指 db 模型，错绑到 `AIProvider as AIProviderSchema` 的 schema 类；两个测试里 `CL3Signal(...)` 指 types 的类，错绑到 `CL3Signal as CL3SignalModel` 的 db 模型），4 条 references inferred → ambiguous（同三对名字加 `DataSource as ProviderDataSource`），逐条读源码都是改名导入原名上的错边。
- 随机序列对照（脚本在 `tasks/d893-fuzz/`）：每个种子生成含改名/普通/相对/函数内导入、再导出、嵌套定义、赋值、星号导入、tests/ 目录的小项目，3 步随机修改，每步后增量索引与同状态全量索引比较所有边。种子 0–1499：0.199.0 分歧 171 个，新版 158 个；只有新版分歧 8 个、只有 0.199.0 分歧 21 个。8 个全部是“掩盖”：把各状态里的改名导入行换成 `pass` 后用 0.199.0 重放，在同一步出现同样的分歧行（7 个完全相同，1306 多一行同类）。机理（种子 189）：`from other import a` 所在模块失去 `a` 后改为按名绑定，导入方未改动，它此前被剪掉的裸调用边不会恢复；0.199.0 里改名导入保护着那条边所以看不出来。
- 种子 1500–4499：0.199.0 分歧 354 个，新版 343 个；只有新版分歧 13 个，全部“掩盖”（同一步、新版的分歧行都在去掉改名导入后 0.199.0 的分歧行里）；只有 0.199.0 分歧 24 个。合计 4,500 个：0.199.0 525（11.7%），新版 501（11.1%），只有新版 21、只有 0.199.0 45。
- 升级/降级提示（head2 = INDEX_VERSION 143 构建，md5 02650990，对 0.199.0 建的 flask 索引）：查询打印 “This index was built by an older code-graph (index v142, this binary v143): answers come from it until it is rebuilt — run: code-graph-mcp reindex”；`incremental-index` 打印 “Index version mismatch (stored v142 ≠ binary v143): clearing + rebuilding …”，退出 0；MCP 服务启动后 `application_id` 142 → 143 并重建（带一次查询的会话结束时 3,705 条边）。0.199.0 对 143 的索引：查询警告 “index built by a newer code-graph (v143 > binary v142); leaving it intact” 并照常回答；`incremental-index` 退出 1，提示先删 `.code-graph/index.db*`。

### 遗留
- D#901（新延期，取代 D#900）：0.199.0 已有的四种增量分歧（普通导入回退按名绑定后被剪的裸调用不恢复、求和计数抵消、`<external>` 占位导入丢 `as`、改名导入 + `m.a()` 在同名定义后到时缺按名边），每种都有复现；0.200.0 不改边，不再暴露它们。
- JS 同类（D#897）不在本次：JS 改名导入的 imports 边不记本地别名，要改解析器。

## 0.200.0 评审 R1（新起的子代理，盲审 v0.199.0..2b2d5478）：HIGH 0 / MEDIUM 1 / LOW 5 / NOTE 7

| # | 结论 | 处理 |
|---|------|------|
| MEDIUM-1（实测） | 新版在 0.199.0 与重建一致的树上分歧，成因不止说明里那一种：A 同一次运行新增一个 `a`、删另一个 `a`，求和计数抵消；B 名字哪里都没有时改名导入落在 `<external>` 占位节点上、metadata 为空、丢了 `as`，剪枝把它当普通导入。另：我的统计每个种子只看第一处分歧，“两版都分歧”的种子里新版多出的行没算（41 个种子，去掉改名导入能解释的只有 4 个） | 不再修剪枝（到停止线前换成只减不增的收窄）：第三版剪枝与绑定恢复成 0.199.0，只改置信度，边集合与 0.199.0 相同，暴露的已有缺陷回到被保护状态。D#900 仍是已有缺陷 |
| LOW-1（实测） | 置信度过滤对所有边生效，`from app import m` + 改名导入 + `m.a()` 的正确 `inferred` 变 `ambiguous`，callgraph 默认不显示 | 修：过滤只对裸边（metadata 空，或 `ur`）生效；测试 qual.py |
| LOW-2（实测） | `from m import a as _a; a = _a; a()` 0.199.0 对，新版变两条 ambiguous | 第三版：边不变（只有 `m.a`），标签 inferred → ambiguous；写进 Not covered |
| LOW-3（实测） | “两个定义 → 两条 ambiguous”不普遍（按名优先近处/非 tests） | 第三版说明不再写这句 |
| LOW-4（实测） | 审计守卫：`-   uses:`、`uses :`、流式映射、`continue-on-error`、`working-directory`、`env: HOME`、`--git` 安装都能绕过；`cargo install cargo-audit --locked`、`cargo-audit@0.22.2`、`--deny warnings`、带引号、行尾注释被误拒 | 修：作业里每行必须以允许的键开头（去掉序列短横及其后空白），install/audit 逐词白名单，接受引号与行尾注释、`--deny`；工作流顶层不许 `env:`/`defaults:`。评审的 14 个变体加 3 个（顶层 env HOME、顶层 defaults、第二个 checkout 拉 fork）：11 个绕过全拒，5 个正常写法通过，块标量仍拒（提示写成一行） |
| LOW-5（实测） | 存活变异 M6（绑定唯一性）、M7（非裸边佐证）、M8（references 佐证）、M9 | 第三版不改绑定（M6 不再适用）；M7/M8 由 qual.py、ref.py 断言杀死（MB、MC）；M9 只在 `self.x.a ()` 这种空格写法上不等价，不补 |
| NOTE-1 | `b` 本身是另一个改名导入的原名时 `b()` 标签变 | 第三版同样（裸调用指向改名导入的目标），说明按谓词写，不再说“`b()` 全部不变” |
| NOTE-2/3 | 测试注释与 resolve.rs 注释描述不准 | 随第三版重写 |
| NOTE-4 | spec 里“私有应用 12 条”是 diff 行数，边是 8 条 | 第三版：8 条边只改标签 |
| NOTE-6 | `an_incremental_run_leaves_no_fanout_tables` 只看 `cg_fanout%`，看不到 `cg_shape_before` | 断言改为同时查 `cg_shape_before` |

## D#893 第三版（只改标签）

- 相对 v0.199.0 的 resolve.rs 差异只有两处：`cg_imports` 多一列 `alias`；置信度的导入佐证对“裸边”（metadata 空或带 `ur`）不认 `alias` 非空的导入。剪枝、绑定与 0.199.0 逐字相同。置信度过程不增删边，所以全量边集合与 0.199.0 相同；增量方面标签会影响扇出（按 `ambiguous` 选调用方重新抽取），用随机序列逐步比较边集合（去掉置信度列的哈希）验证。
- 语料（head3 md5 见下）：五个语料去掉置信度列后与 0.199.0 逐行相同；标签变化只有私有应用 8 条（4 calls、4 references，inferred → ambiguous，就是第二版删掉/降级的那 8 条）。
- RED：v0.199.0 的 resolve.rs 上 `test_python_renamed_import_does_not_bind_its_original_name` 失败（views/rev/only 的 `m.a` 为 inferred）。变异 MA（去掉过滤）、MB（对所有边过滤，LOW-1）、MC（只对 calls 过滤）都被断言杀死。
- `qual.py`（改名导入 + `m.a()`）不放进增量对照树：`n.py` 后到时 `m.a()` 缺按名的 `n.a`，0.199.0 与新版同样分歧（已有缺陷，记入 D#900 说明）。
- 用户可见（实测，base 对 head3）：`callgraph go` 由两条变为只列 `n.a` 并提示 1 条 ambiguous 被隐藏；`impact a --file app/m.py` 由 1 个调用方变为 0（风险 UNKNOWN，提示排除了 1 条 ambiguous）；`refs` 仍列出，标 `~ambiguous`；MCP `get_call_graph` 默认 `ambiguous_edges_hidden: 1`。JS 两种形态两版相同。
- 随机序列（第三版 head3 md5 见发版记录；`fuzz_seq.py` 改为每步都比较、记录每步去掉置信度列的增量/全量边集合哈希；`rows_cmp.py` 按步比较两版的分歧行）：自己的生成器 4,500 个种子 0.199.0 分歧 525、新版 497；评审的生成器（`gen_review.py`，4 步）1,500 个种子 218 → 198。全量一侧的边集合在 6,000 个种子的每一步都与 0.199.0 相同（0 个种子不同）；增量一侧 38 + 23 个种子不同（标签影响扇出选择重新抽取的调用方）。新版独有的分歧行忽略置信度后 54 + 12 行，全部是“同一条边在增量与全量里标签不同”，且都出现在 0.199.0 同一步也与重建不一致的状态上（多为同一文件的 imports 边已分歧，改名导入的佐证原先把两边标签拉平）；新的边分歧 0。即新版与重建不一致的状态是 0.199.0 的子集。总分歧行（忽略置信度）2,155 → 2,090、894 → 843。
- 评审 R1 的复现：cause A（求和抵消）、cause B（`<external>` 占位）、种子 53 在第三版上都与重建一致；去掉改名导入的对照两版同样分歧（已有）。
- 提交：580e80bd（审计守卫键白名单）、6cef4145（第三版修复，CHANGELOG 重写）。

## 0.200.0 评审 R2（新起的子代理，只审 580e80bd / 6cef4145 / 7d32d0f8）：HIGH 0 / MEDIUM 1 / LOW 4 / NOTE 5

| # | 结论 | 处理 |
|---|------|------|
| MEDIUM-1（实测） | 相对导入的模块调用 `from . import m; m.a()` 带 `{"ur":"rel"}`，被当成裸调用，正确的 `m.a` inferred → ambiguous，callgraph 不列被调方；与 R1 LOW-1 同类，只修了绝对形式 | 到停止线（R1、R2 各有 MEDIUM）：只减不增的收窄——只对无 metadata 的边去掉佐证，任何带标记的边（`q`、`ur` rel/attr/local、`pyimp`）保持 0.199.0 标签。改标签的边集合是第三版的子集；用语料与随机序列证明（见下）。不送第三轮 |
| LOW-1（实测） | 星号导入 + 同模块改名导入：唯一且正确的 `m.a` 变 ambiguous | 写进 Not covered（五个语料都没有） |
| LOW-2（实测） | 注解 `x: m.A` 的点号尾巴记成无 metadata 的引用，正确引用变 ambiguous | 写进 Not covered |
| LOW-3（实测） | 存活变异 M4（去掉 ur 判断，非等价）、M13（改名导入挡住同目标普通导入的佐证，非等价）等 | 第四版即 M4 方向；测试 relq.py（相对模块调用保持 inferred）与 both.py（普通导入照常佐证）杀死 N1（把 ur 边重新纳入过滤）与 N2（M13） |
| LOW-4（实测） | 审计守卫 12 种新绕过（checkout `with: {repository/ref}`、续行、`--deny=x\|\|true`、双引号里 `\t`、顶层 `env :`/`"env":`/`defaults :`、`cargo-audit@…&&…`、node_modules 里的 audit.toml 配合 HOME） | 不再逐项堵：audit 作业与固定文本逐行（含缩进）比较，注释行跳过，动作的 `@<40 位十六进制>[ # tag]` 归一为 `@PIN`，`cargo audit` 行只能追加单空格的 `--ignore RUSTSEC-YYYY-NNNN`；顶层键闭集（name/on/permissions/concurrency/jobs）。评审 47 个变体：所有绕过都失败，现文件与升级 pin 通过，其余改动都失败并提示更新 AUDIT_JOB |
| NOTE-1 | 守卫误拒的常见写法 | 固定文本的预期代价：改作业就要改 AUDIT_JOB |
| NOTE-2 | 说明与注释少写了 references 与 `ur` 边；`ur: local` 是 D#789 不是 D10B | 第四版 `ur` 边不再变；CHANGELOG 升级段写“calls and references”，注释改写 |
| NOTE-3 | `global a; a = b`、try 回退里 `a = _a` 同样变 | 写进 Not covered（实测） |
| NOTE-4 | “边不变”只对全量成立，增量边集合会因扇出不同 | CHANGELOG 写“a full index” |
| NOTE-5 | `from app import n as m` + 改名导入 + `m.a()`：错边仍 inferred（两版相同） | 不变，不处理 |

## D#893 第四版（发版内容）

- resolve.rs 相对 v0.199.0：`cg_imports` 多 `alias` 列；置信度佐证 `AND (i.alias IS NULL OR (e.metadata IS NOT NULL AND e.metadata <> ''))`。
- 实测（base 对 head4）：probe 树与第三版相同（views/rev/only/ref/assign 的无标记边 inferred → ambiguous）；带标记的 relq/selfx/local 边保持 inferred，其无标记孪生边变 ambiguous，callgraph 仍列 `m.a`（隐藏计数 +1）；star/dotted/glob/tryfb 写进 Not covered。
- 收窄的对照证明（head4 md5 9be108b2，`--no-default-features`）：五个语料与 head3 逐行相同（0 行不同），对 0.199.0 只有私有应用 8 行标签变化，去掉置信度列后五个语料与 0.199.0 相同。随机序列（两个生成器共 6,000 个种子，每步比较）：全量一侧每步与 0.199.0 相同；只有新版与重建不一致的状态 0 个；分歧种子 525 → 497、218 → 198；分歧行 2,155 → 2,090、894 → 843（与 head3 相同）。
- 停止线：R1、R2 各有 MEDIUM；第四版是第三版改标签集合的子集（只减不增），由上面的差分证明，不送第三轮（发版手册“what closed every deep loop was withdrawal or decline-only narrowing proven by a differential”）。
