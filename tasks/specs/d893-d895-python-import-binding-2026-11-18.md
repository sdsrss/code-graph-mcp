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
- 变异（每个跑完整 lib 套件）：M1（`cg_imports` 不用别名）、M2（剪枝不比名字）、M3（佐证不比名字）、N0 对照（别名完全不进名字）均被杀；M4（`cg_unique_imports` 不用别名）首轮存活，补 tests/ 夹具（按名优先非测试文件，只有绑定阶段会加错边）；M5/M6 存活，见决定。
