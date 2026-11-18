# 2026-11-18 D#893 + D#895：Python 改名导入不再佐证原名；增量里重新出现的定义要刷新回退导入

状态：**实现中**。授权：用户 2026-11-17“把剩余工作都做了，授权清理和发版”，2026-11-18“继续完成剩下的任务”。

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
