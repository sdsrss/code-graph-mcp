# 2026-11-18 D#901：Python 增量索引与重建不一致（D#893 的模糊测试发现的已有分歧）

## 为什么

D#893 的随机序列模糊测试（`tasks/d893-fuzz/`）显示，0.199.0 在 4,500 个序列里有 525 个（11.7%）增量索引与同状态全量索引的边不同；0.200.0 是 497 个，评审生成器 1,500 个里 198 个。用户每次改文件走的是增量路径，分歧就是在重建前缺边、多边或标签不同。延期条目 D#901 记了四种复现：

1. 普通导入的模块不再定义该名、导入回退为按名绑定后，导入方此前被剪掉的裸调用边不恢复（导入方未改动、不重新抽取）。
2. 同一次运行一个文件新增 `a`、另一个文件删掉 `a`：按名求和的计数不升，裸调用方缺新定义的边。
3. 名字在哪里都没有时，导入落在 `<external>` 占位节点、metadata 为空；之后模块有了这个名字。
4. `from app import m; from app.m import a as b; m.a()`，`n.py` 后到时缺按名的 `n.a`。

用户授权：“继续完成剩余的工作，先深入思考给出科学准确的建议，然后按你的建议执行”（2026-11-18）。

## 级别

L2：修复增量路径使其符合“增量 = 重建”的既有行为（bugfix，不改全量结果），跨 `resolve.rs` 与 `index_files.rs`。若改动改变全量结果，升级并重新评估 INDEX_VERSION。

## 方法

1. 基线：HEAD a1007267（0.200.0）release 构建，两个生成器共 6,000 个种子，每步比较。
2. 按首个分歧的形态分桶，每桶取最小复现，找出漏掉的触发（哪个文件该重新抽取却没有）。
3. 每个修复先写失败测试（`assert_steps_match_rebuild`），再改；修后重跑同样 6,000 个种子，要求只减不增（新版独有分歧 0），并跑五个语料对照全量结果不变。

## 根因（最小化后的复现，`minimize.py` 从模糊测试种子缩出）

基线 HEAD a1007267 release（md5 4d56efc8）：自己的生成器 4,500 个种子 497 个分歧、评审生成器 1,500 个 198 个，与 0.200.0 发版记录一致。8 个种子缩成 2–4 个文件的复现，归成三类：

- **A 导入恢复**（Phase 0 `restore_inbound_edges`）：未改动的文件 S 的 Python `imports` 边指向被重新索引的文件 T，恢复时只按“T 里同名节点”挪过去，或在 T 没有同名节点时重新排队。重建对 S 的导入、以及按这个导入绑定或剪掉的调用，结果可能不同，但 S 不会被重新抽取：
  - 1749、1870：T 不再定义该名，导入回退为按名绑定；S 里先前因导入指向别处而被剪掉的裸调用（含 `ur:local` 的参数调用）不恢复。
  - 1755、1854、2634：T 新增同名方法 `K.a`；重建的 `from T import a` 绑定 T 里所有同名节点（`resolve_python_module_targets`），恢复只挪到函数上（D#192 的方法过滤）；2634 的调用跟着导入走。
  - 1621：回退绑定的导入，T 里的定义变成嵌套函数；恢复挪到嵌套函数上，重建的回退不选它。
- **B 扇出触发只看计数**（`bare_name_callers_of_new_duplicates` 的 `cg_fanout_up`）：同一名字的定义被替换（方法 ↔ 函数，同一文件或跨文件），总数不变，不触发：1213（导入停在 `<external>`，同名方法换成函数，即 D#901 第 3 种）、2012（成员调用的候选换成离调用方更近的方法）。
- **C 模块限定调用**：`m.a()`（`"q":"module"`）按名绑定所有同名节点，但 `a` 的导入为其中一个作证（`inferred`），只有一个候选时不是 `ambiguous`，扇出只看 `ambiguous`，新增的同名定义不触发（D#901 第 4 种；普通导入同样，不限于改名导入）。

## 决定

- A：恢复 Python `imports` 边时，只有 T 里恰好一个同名节点、在模块级、限定名与原来相同，才视为“原样恢复”；否则照旧恢复/排队，同时把 S 记入 `temp.cg_py_requeued`，扇出轮重新抽取 S（JS 已有同样做法：`requeued_walk_import`）。只对不在本次运行里的 S 记录；`attr`（D#518 的合成导入）与 `<module>` 不记录。
- B：候选两种（实验期用环境变量 `CG_D901_FANOUT` 切换，定稿后删除）：b1 按（文件、名字、类型、限定名）逐行比较，只要有一行变多就触发；b2 按（名字、类型、限定名）求和比较。b1 同时覆盖“同一次运行在一个文件加、另一个文件删”（D#901 第 2 种、D#899 的移动），代价是移动文件时重新抽取更多调用方（D#899 记过 285 对 7 个文件）。按模糊测试与开销实测选。
- C：扇出的调用方查询把带 `"q":"module"` 的 `calls` 边也算上，不论置信度。
- B 的补充（head1 剩余分歧最小化后，8 个里 6 个是这一种）：嵌套的 `def a` 变成模块级 `def a`，类型、限定名都不变（嵌套 def 的限定名就是裸名），b1 看不出；导入、按名回退、裸调用只认模块级定义，重建绑上新的。比较的键再加“是否嵌套在 def/类里”（只对 Python 算，用与 `module_level_definition_ids` 相同的行号包含判断）。RED：`d901_a_nested_def_made_module_level_reaches_its_callers` 2/2 失败，加上后通过。
- 不修（写进 Not covered，记新延期）：`m.a()` 在没有导入 `m` 时是成员调用（`"q":"member"`），只有一个候选方法时为 `inferred`，新增的同名方法（更近）不触发扇出（head1 剩余样本 122、230）。要覆盖得让任何新增同名方法都重新抽取所有 `.a()` 调用方，普通编辑（给类加一个常见方法名）也会付这个代价，需要单独测量。

## 验证记录

- RED（修前代码）：`d901_an_import_moved_by_a_run_restores_its_files_pruned_calls` 2/2 个用例失败，`d901_a_restored_import_binds_what_a_rebuild_binds` 4/4，`d901_a_same_name_replacement_reaches_its_callers` 2/3（第 3 个“一个文件加、另一个删”修前已通过，留作护栏）。A 之后前两个通过；b1、b2 下第三个通过。`d901_a_module_call_reaches_a_new_namesake` 去掉 C 的条件时 2/2 失败，加上通过。
- `an_incremental_run_leaves_no_fanout_tables` 加了一步（模块新增同名方法 → 记录导入方），并查 `cg_py_requeued`；变异“记录表永不删除”被它杀死；“`take` 不删表”存活，等价（`drop_fanout_temps` 在同一轮里删）。
- 模糊测试（exp1 = A + B 三种，未含 C，md5 5d04e169；每步比较，种子 0–4499 自己的生成器、0–1499 评审生成器 4 步）：

  | 版本 | 自己的生成器 分歧种子 / 分歧行 | 评审生成器 | 基线一致而新版分歧的种子 |
  |------|------|------|------|
  | 基线 a1007267 | 497 / 2,090 | 198 / 843 | — |
  | b0（只有 A） | 243 / 976 | 101 / 377 | 0 / 0 |
  | b1 | 43 / 140 | 24 / 72 | 0 / 0 |
  | b2 | 145 / 470 | 63 / 216 | 0 / 0 |

- 对 b0 剩下的 8 个最小种子用 b1 重新最小化：7 个不再复现；剩 299（嵌套 `def a` 变成模块级 `def a`，类型与限定名都不变，b1 看不出；导入停在 `<external>`）。
- 开销与正确性（exp2 = A + B 两种 + C，md5 eee250d7；`cost.py`，各 3 次，空闲机器；“错边”= 增量与同状态重建相比多/少的边）：

  | 场景 | 基线 秒 | b0 秒 | b1 秒 | 重建 秒 | 扇出重新抽取文件数 基线/b1 | 错边 基线 → b1 |
  |------|------|------|------|------|------|------|
  | 标准库改一个文件（`shutil.py` 末尾加函数） | 0.25 | 0.27 | 0.28 | 3.2 | 0/0 | imports 1/2 → 1/2（已有） |
  | 标准库 `mkdir moved; mv email json xml moved/` | 3.59 | 3.91 | 6.80 | 3.0 | 7/288 | calls 1,292/69 → 0/0 |
  | 标准库互换 `shutil.py` 与 `tempfile.py` | 1.10 | 1.06 | 3.71 | 3.1 | 2/141 | calls 132/4、references 0/1 → 0/0 |
  | nltk `mv nltk/corpus nltk/moved` | 1.18 | 1.24 | 2.18 | 1.3 | 2/87 | calls 14 → 0 |
  | 本仓库 v0.180.0 → v0.198.0 | 3.21 | 3.20 | 3.24 | 2.5 | 100/102 | references 0/2,399 → 0/2,398（D#898） |
  | daagu 10b3ce5d~200 → 10b3ce5d | 6.28 | 6.17 | 6.11 | 4.2 | 393/393 | references 0/2（D#898） |

- **测试框架缺陷（本次发现）**：`fuzz_seq.py --gen gen_review.py` 从来没生效。Python 3.14 的进程池默认 `forkserver`，工作进程重新导入模块，`main()` 里替换的 `initial/step` 到不了工作进程；“评审生成器”的结果其实是自己的生成器跑 4 步（结果文件里没有 `lib/k.py` 等评审生成器才有的文件）。0.200.0 发版记录里的“评审生成器 1,500 个 218 → 198”同样是自己的生成器。已修：生成器路径随任务参数传给工作进程，在工作进程里加载（`load_gen`）；上面表里“评审生成器”一列应读作“自己的生成器 4 步、种子 0–1499”，真正的评审生成器结果见下。
- 选 b1：两段真实历史回放的耗时差在三次之间的波动内；标准库改一个文件 0.25 → 0.28 s（三次都高 0.03 s，见下面 7 次复测）；移动/互换文件时增量比基线慢 1.9–3.4 倍、超过一次重建，但 D#899 记的三个场景错边全部归零（D#899 一并关闭）。正确性优先于速度；大批移动少见，错边却一直留到下一次重建。环境变量开关已删除。
- **定稿 head2**（A + B 的 b1 + 嵌套标记 + C，release，md5 9449942a），每步比较，评审生成器用修好的 `--gen`：

  | 版本 | 自己的生成器 4,500 个：分歧种子 / 分歧行 | 评审生成器 1,500 个 | 新版独有分歧种子 | 不在基线同一步的分歧行 |
  |------|------|------|------|------|
  | 基线 a1007267 | 497 / 2,090 | 278 / 1,417 | — | — |
  | head2 | 15 / 21 | 56 / 120 | 0 / 0 | 0 / 0 |

- **开销 head2**（`cost.py`，改一个文件 7 次、其余 3 次）：改一个文件中位数 0.26 s（基线 0.25 s，7 次 0.25–0.28）；标准库移动 6.90 s（重新抽取 288 个文件，重建 3.09 s）；互换 3.79 s（141 个）；nltk 移动 2.15 s（87 个）；本仓库回放 3.14 s（102 个，基线 3.21 s、100 个）；daagu 回放 6.18 s（393 个，基线 6.28 s）。错边与 exp2 相同：调用差异全部为 0，剩 imports 1/2（已有）、references 0/2,398 与 0/2（D#898）。
- **全量结果不变**（`fulldiff.py`，base 对 head2，比较边的两端、关系、置信度、metadata）：标准库 113,891 条边、nltk 30,258、flask 3,705、本仓库 28,804、daagu 77,250，两边逐条相同（base-only 0、head-only 0）。`INDEX_VERSION` 保持 143；抽取指纹按规则重录（内容不变，只是测试源码变了）。
- **适用范围**：A 有 `src_lang == "python"` 判断；C 的 `"q":"module"` 只由 Python 抽取器（`python_module_call_meta`）产出；B（`cg_fanout_up` 按文件、类型、限定名比较）对所有语言生效，嵌套标记只对 Python 算。本仓库（Rust、JS、Python）回放重新抽取 100 → 102 个文件，即非 Python 的实测；CHANGELOG 的 change 一节写明“对所有语言生效”。
- **剩余分歧的形态**（head2 剩余种子用 `minimize.py` 缩小，再用 base 与 head2 各复跑一次；每个用例两边差异相同，2707 基线多一条）：
  - inherits：`class W(B)` 在 B 后来才有时不加边（28、138），已有 B 时在更近的文件新增 B 不改指向（813）。
  - references：装饰器 `@a`（229）、值里的名字 `REFS = [a, b]`（88），后来新增的同名定义不加边（D#898 一类）。
  - 属性调用 `self.x.a()`：别的文件新增方法 `a`（247）。
  - 成员调用 `m.a()`（没有把 `m` 作为模块导入，旁边有 `from other import a as c`）：新增更近的同名方法（122、230）；去掉这行导入就不再分歧。
  - 删除定义：`a` 有三个定义时裸调用 `a()` 没有边，删掉一个后重建把剩下两个标为 `ambiguous`，增量没有（495、569）；`def go_p(b): return b()` 删除别处的嵌套 `def b` 时，重建多一条带 `"ur":"local"` 的同目标边（195，调用方列表不变，未写进 CHANGELOG）。
  - 新增定义：`from lib.k import a`，`lib/k.py` 有函数 `a` 和方法 `A.a`，调用方同目录新增 `a`，增量保留两条边，重建一条都没有（398、537）；参数调用同理（611）。
  - 再导出链：`app/sub/__init__.py` 从 `impl.py` 导入、`impl.py` 再从 `other.py` 导入，链尾新增 `a`（684）；同一次运行给 `impl.py` 加同名方法（2707）。
  - 166、25、2998 的最小化结果复跑两边都不分歧（最小化时接受了一次偶然通过），不作为证据；166 的原种子形态同 247。
- **新发现（全量一侧，不在本次范围）**：398 的最终状态直接全量索引，`from lib.k import a` 的调用 `a()` 没有任何边。探针（调用方 `tests/test_x.py`，base 与 head2 结果相同）：

  | 文件组合 | `go_a → a` 边数 |
  |------|------|
  | `lib/k.py` 只有函数 `a`，同目录 `tests/helpers.py` 也有 `a` | 1 |
  | `lib/k.py` 有函数 `a` 和方法 `A.a`，别处没有 `a` | 2 |
  | 同上，另一个 `a` 在根目录 `other.py` | 2 |
  | 同上，另一个 `a` 在同目录 `tests/helpers.py` | 0 |
  | `lib/k.py` 只有函数 `a`，`other.py` 与 `tests/helpers.py` 也有 | 1 |
  | `lib/k.py` 只有函数 `a`，`other.py` 有方法 `a` | 1 |

  即被导入模块里同名节点不止一个、调用方目录里另有同名定义时，显式导入的调用整个丢边。记新延期条目，写进 CHANGELOG 的 Not covered。
- **并行路径**：增量有两个入口，都走 `index_files`（A）和 `fan_out_to_new_duplicate_definitions`（B、C、`cg_py_requeued`）：`run_incremental_index_cached`（CLI `incremental-index`、MCP 启动漂移检查，mod.rs:707）和 `apply_file_refreshes`（MCP 查询时刷新 `ensure_file_indexed` / `resync_stale_files`，mod.rs:563）。原测试只走第一个；`assert_d901_cases` 改为每个用例两条路径各跑一次（第二条把每步改动的路径经 `plan_file_refresh` 合成一批交给 `apply_file_refreshes`，同 `resync_stale_files`）。当前代码 5/5 通过。
- 检查（改 tests.rs 之前的同一棵树）：`cargo fmt --check` 通过；抽取指纹重录（tests.rs 不在指纹范围内，pipeline 其余文件经 fmt 后内容变化，INDEX_VERSION 不变）后校验通过；`cargo clippy --all-targets -- -D warnings` 默认与 `--features embed-model` 两条都通过；`cargo test --no-fail-fast` 2,523 通过、0 失败、7 忽略。
- **RED（两条路径）**：把当前 tests.rs 放进 a1007267 的 `git archive` 副本，单独的 target 目录跑 `cargo test --lib d901_`：5/5 个测试失败，13 个用例 × 2 条路径共 26 处失败，增量运行与刷新一一对应。其中“一个文件加、另一个删”在两条路径上都失败，与上面 RED 记录的“修前已通过”不符；本次是在完整的 a1007267 源码上复跑，以本次为准，早先的出入原因未查。
- **提交**：84355db5 修复与测试（pre-commit：cargo check、`cargo test` 2,523 通过 / 0 失败 / 7 忽略、fmt、两条分支的 clippy 全部通过）；b8d3452b 模糊测试工具 `--gen` 修复。

## 发版前盲审第 1 轮（6d19ae93）与修复

评审：新开的子代理，空上下文，盲审说明只给提交范围 a1007267..6d19ae93、合约和问题；自己用 `git archive` 构建 base（a1007267，md5 648efa92）与 head（6d19ae93，md5 3618326a）。结论 HIGH 2、MEDIUM 1、LOW 2、NOTE 5。逐条对照代码核对后处理如下。

- **HIGH-1 Go 回归（成立）**：Phase 0 恢复入边时，Go 的 `px.Foo()`（`{"q":"gopkg"}`）被挪到文件里每个叫 `Foo` 的节点上，包括方法 `K.Foo`。`index_files.rs` 恢复过滤里排除方法那一条只对 Python 生效；全量和延迟、待解析三条路径都经过 `go_package_candidates`，后者用 `filter_out_method_ids` 去掉方法（`type <> 'method'`）。编辑定义文件本身时 0.200.0 就会这样；本版逐路径触发后，移动一个“该文件的歧义调用所指名字”的定义文件也会重新抽取它，于是多了一类触发。修：Go 包调用恢复时去掉 `method` 类型的候选，与 `go_package_candidates` 同一条规则。RED：`d901_a_go_package_call_is_not_restored_onto_a_method` 2 个用例 × 2 条路径共 4 处失败（多出 `py/u.go:Go → px/x.go:K.Foo`），修后通过；变异“去掉这条过滤”被它杀死。
- **HIGH-2 快照平方开销（成立）**：`nested` 用逐节点的 EXISTS 子查询扫同文件所有节点，只有 `idx_nodes_file` 可用，N 个节点 N² 次比较，每次增量运行和每次刷新都付。改为窗口函数：同文件按 `start_line` 排序，取起始行更早的 def/方法/类的最大 `end_line`，不小于本节点 `end_line` 即被包含——与“存在更早开始、结束不早于本节点的 def/类”逐条等价（整数行号，`RANGE … 1 PRECEDING` 即严格更早；无候选时 `IFNULL(…, -1)` 得 0，与 EXISTS 为假相同）。前后两半共用 `DEFINITION_ROWS_SELECT`，`<module>` 过滤仍只在前一半、在窗口之外。RED：`the_definition_snapshot_grows_near_linearly_with_a_python_files_nodes` 用 SQLite 进度回调数虚拟机步数（与机器无关），400 → 1,600 个 def 修前 44,449 → 696,181（×100 步，15.7 倍），修后低于 8 倍阈值通过；变异“窗口帧含本行”“去掉 nested 列”都被 D#901 测试杀死。
- **MEDIUM-3 改类名扇出（成立，修法与评审建议不同）**：比较键里的限定名让改类名时每个方法名都“上升”，所有 `ambiguous` 调用方被重新抽取，边一条不变。先按评审的 noqn 变体去掉限定名（head3），Python、JS 生成器结果与 head 逐种子相同，但 Rust 生成器有 18 个种子 head 已修好而 head3 又分歧（都在 base 上分歧，不是相对 0.200.0 的回归）。缩小种子 18：`impl K { fn a(&self) }` 变成自由 `fn a()`，Rust 没有 method 类型，两者都是 `function`，只有限定名 `K.a` / `a` 能区分，而裸调用 `a()` 只能到自由函数。定稿（head4）：不比较限定名本身，比较 `scoped`（限定名是否不同于名字，即是否是类型/模块的成员）。改类名时 `K.a → K2.a` 两边都是 scoped，不触发；`K.a → a` 触发。RED：`a_class_rename_pulls_no_caller_into_the_fanout_round`（调用方用 `return close` 这种值引用，对两个方法都是 `ambiguous`；`x.close()` 参数接收者不产生边，最初的写法对照组不成立，已改）修前失败；`d901_a_same_name_replacement_reaches_its_callers` 新增 Rust 用例在 head3 两条路径都失败，加 `scoped` 后通过；变异“去掉 scoped 列”被它杀死。
- **LOW-4 更早批次的导入方（成立，补测试）**：`done_paths` 那一支（导入方在同一次运行更早的批次里被重新抽取）没有测试；新增 `d901_an_importer_in_an_earlier_batch_reaches_a_new_namesake`（`BATCH_SIZE` 个改动的填充文件，导入方 `a_use.py` 与模块 `z_mod.py` 分在两批），当前代码通过，变异 M3（去掉这一支）被它杀死。
- **LOW-5 Upgrading 段（成立）**：“相关文件重新索引时消失”只对调用方文件成立。实测：0.200.0 留下过期状态后，用 head3 只编辑 `app/m.py` 或 `app/sub/impl.py`，`go` 的被调列表不变；编辑 `app/views.py` 后与重建一致。CHANGELOG 改为“the file the edge starts from is indexed again”，并补上“或缺边”。
- **NOTE-6**：“replaying 200 commits”实为一次增量运行从旧树更新到新树；CHANGELOG 改写。
- **NOTE-7**：Rust 种子 1120 第 2 步的一行是基线第 1 步报错掩盖的已有缺陷，不是回归；CHANGELOG 的“没有新分歧、每条剩余分歧都在基线同一步出现”在 Python 生成器那一段，其他语言的结果另写（见下）。
- **NOTE-8（更正本 spec）**：上文“`take` 不删表存活，等价”是错的。`drop_fanout_temps` 在 `bare_name_callers_of_new_duplicates` 里运行，位于第一次 `take`（mod.rs:855）与循环（mod.rs:904-921）之间；若 `take` 不删表，循环每轮都拿回同样的路径，扇出轮记录的导入方会被重复抽取至多 8 次——是开销缺陷，不是等价变异。代码本身会删表，这个变异仍无测试固定，记在下面的存活清单。
- **NOTE-9、NOTE-10**：临时表生命周期与 `--gen` 修复无问题，不需处理。

**变异（修复后，每个变异体确认编译并跑了测试，用备份文件还原）**：杀死——M3、Go 方法过滤、scoped 列、nested 列、窗口帧含本行；存活——`ty` 列（目前找到的类型变化用例里 scoped 同时变化，保留 `ty`，代价几乎为零，无法证明“同名同限定只有类型变化”的情形不存在）、包含类型去掉 `class`（Python 的嵌套类在回退绑定时本来就会被绑上，探针两边相同，视为等价）、M17（去掉 `f.language = 'python'`：窗口函数后对任何语言都是 O(N log N)，只多算一列）、M8（循环里的 `take`：只在刷新路径上扇出轮重新抽取的文件磁盘内容已变时影响正确性，其余为开销）、M10（见 NOTE-8，开销）、M1/M2（被其他检查掩盖）。

**检查（定稿 head4 的同一棵树）**：`cargo fmt --check` 通过；抽取指纹重录后复核通过（INDEX_VERSION 不变）；`cargo clippy --all-targets -- -D warnings` 默认与 `--features embed-model` 都通过；`cargo test --no-fail-fast` 2,527 通过、0 失败、7 忽略（原 2,523 + 新增 4 个测试函数）。

**全量结果不变**（`fulldiff.py`，base 对 head3、base 对 head4）：五个语料（标准库 113,891 条边、nltk 30,258、flask 3,705、本仓库 28,804、daagu 77,250）两边逐条相同。

**模糊测试**（每步比较；Python 三个生成器的 base 是本 spec 的 a1007267 构建 4d56efc8，Go/JS/Rust/Python 移动生成器是评审写的 `gens/gen_*.py`，base 与评审 head 的结果取自评审自建的二进制 648efa92 / 3618326a；head3 05531e57 = 去掉限定名，head4 980aabfc = 定稿）：

| 生成器（种子数） | base 分歧种子 / 行 | 评审 head | head3 | head4 | head4 独有分歧种子 |
|------|------|------|------|------|------|
| 自己的（4,500，3 步） | 497 / 2,090 | 15 / 21 | 15 / 21 | 15 / 21 | 0 |
| 评审生成器（1,500，4 步） | 278 / 1,417 | 56 / 120 | 56 / 120 | 56 / 120 | 0 |
| Python 移动/互换（1,500） | 420 / 1,862 | 25 / 44 | 25 / 44 | 25 / 44 | 0 |
| Go（1,500） | 285 / 578 | 176 / 322 | 40 / 64 | 40 / 64 | 0 |
| JS/TS（1,500） | 229 / 549 | 88 / 143 | 88 / 143 | 88 / 143 | 0 |
| Rust（1,500） | 205 / 719 | 75 / 271 | 93 / 298 | 75 / 271 | 0 |

head4 的自己生成器与评审生成器结果和 head2 逐种子相同（4,500 / 1,500 个）。“head4 有、base 同一步没有的行”只有 Rust 种子 1120 第 2 步一行；从第 1 步的干净索引复跑（`rs1120.json`），base 与 head4 都少这一行，是已有缺陷（评审 NOTE-7）。

**开销 head4**（`cost.py` 成对，空闲机器，改一个文件 7 次、其余 3 次，中位数）：改一个文件 0.26 → 0.25 s；标准库移动 3.43 → 6.81 s（7 → 288 个文件，重建 2.9 s）；互换 1.08 → 3.60 s（2 → 141）；nltk 移动 1.17 → 2.06 s（2 → 87）；改类名 `IMAP4` 1.11 → 1.10 s、`FakeSession` 0.52 → 0.51 s（都是 0 → 0）；本仓库一次更新 0.180.0 → 0.198.0 3.07 → 3.14 s（100 → 102）；daagu 一次更新 200 个提交 6.25 → 6.39 s（393 → 393）。大文件单行编辑（评审的模板）：2,000 个 def 0.15 → 0.14 s，10,000 个 0.80 → 0.84 s，20,000 个 1.60 → 1.73 s，numpy `__init__.pyi` 0.45 → 0.46 s（评审测的 head 分别是 0.55、10.8、42.7、0.60 s）。错边与 head2 相同：调用差异全部为 0，剩 imports 1/2（已有）、references 0/2,398 与 0/2（D#898）。

**Upgrading 段复核**（head3）：0.200.0 做一次“删 `other.py` 的 `a`、加 `app/m.py` 的 `a`”后，`go` 只列 `app/sub/impl.py`；head3 编辑 `app/m.py`、再编辑 `app/sub/impl.py` 都不变；编辑 `app/views.py` 后列出 `app/m.py`、`app/sub/impl.py`，与重建相同。

## 发版前盲审第 2 轮（3529f2ea，只审修复）与处理

评审：新开的子代理，空上下文，只给 3529f2ea 与对照 a1007267、6d19ae93；自建三方二进制，校验和与本 spec 一致（base 648efa92、parent 3618326a、head 980aabfc）。结论 HIGH 0、MEDIUM 1、LOW 2、NOTE 4；16 个生成器、14 种语言相对 0.200.0 无新增错边，五个语料全量不变，`cargo test --no-fail-fast` 2,527/0/7。逐条核对后处理如下（没有代码改动，修的是文字与测试，故不再开第 3 轮）。

- **MEDIUM-1 Upgrading 段（成立）**：第 1 轮改写的“起点文件重新索引时纠正”对经 `__init__.py` 再导出的 Python 导入不成立。自己复现（`upcase.py cases/upgrade_min.json`）：0.200.0 留下的过期状态缺两条 `imports → app/sub/impl.py:K.a`；head 只重索引 `app/__init__.py`，两条都还缺；只重索引 `app/sub/__init__.py`，它自己那条纠正；两者同一次运行则全部纠正。再加一个 `use.py`（`from app import a` 并调用 `a()`）：只重索引 `use.py`，它的导入与调用 `go_a → K.a` 都不纠正，链上每个 `__init__.py` 一起才纠正。自己重跑评审的 `upgrade_check2.py`（默认生成器，base 分歧的种子逐文件重索引）：682 对、1,454 条过期边，剩 21 条（14 个文件）；对照 `up_control.py`：这 14 对 head 从干净索引出发再重索引都与重建相同（head_own_divergence 0），剩下的来自其他文件的过期状态。CHANGELOG 改为“重新索引起点文件纠正大多数：1,454 条中 1,433 条；其余 21 条经过另一个文件的过期边，可能要等那个文件也重新索引——再导出的导入与经它绑定的调用，要等链上每个 `__init__.py` 都重新索引”（不写“仅当”：开放域，只写复现过的形态）。
- **LOW-2 三个存活变异（成立，补测试）**：M3（Go 过滤扩到所有 Go 调用）、M6（`>=` 改 `>`）、M7（去掉 `PARTITION BY`）。新增三个测试：`d901_a_go_method_call_is_still_restored_onto_a_method`（从评审 M3 变异体的 Go 种子 16 用 `minimize.py` 缩出，`pa/a.go` 在同一次运行加 `K.A` 与调用方 `x.A()`），`d901_a_nested_def_ending_on_its_outers_last_line_is_nested`、`d901_the_nested_flag_is_per_file`（评审写的用例）。head 上 3 个通过；三个变异体各被对应测试杀死、其他两个通过（`mut2.py`，备份还原后 touch）。评审的 Dart 用例杀不死 M10，不加。
- **LOW-3 改类名的注释（成立）**：“改类名不改任何调用方的边”在标准库外不对：Java `new K()`、PHP `use PA\K` 在类改名成它们已引用的名字时会绑上，parent 因方法限定名变化顺带重抽了这些调用方，head 不会。用评审的数据核对（`fuzzcmp.py` parent 对 head）：每 1,000 个种子 Java 5、PHP 23、TS 3 个是 parent 修好而 head 没修的，都在 0.200.0 同样分歧，head 没有 0.200.0 同一步没有的行——是没修到，不是回归。改注释如实写，不改代码（改回限定名会让 `IMAP4` 改名重抽 292 个文件）。记入 D#903。
- **NOTE-4 `CLASS_SHAPE_SELECT` 仍是平方**：发版前已有（5,000 个类的文件改一行 base 7.21 s、head 7.32 s）。提交标题的“linear snapshot”只指定义行。记为新延期项。
- **NOTE-5 `<module>` 注释**：注释说前后两半都排除，实际只有前一半排除，于是每个文件的 `<module>` 都“上升”。五个语料（标准库、nltk、flask、本仓库、daagu）实查指向 `<module>` 的边只有 imports（2,861 / 1,009 / 119 / 425 / 3,007），模块导入那一支本来就过滤 `<module>`，只多付探查开销。改注释，代码不动（0.200.0 相同）。
- **NOTE-6 移动比重建慢**：CHANGELOG 只给了标准库移动的重建时间。补上互换重建 3.0 s、nltk 移动重建 1.3 s（本 spec 的 cost-h4 实测：3.03 s、1.26 s），Upgrading 段改为“在下面测的三种情形里比重建慢”。
- **NOTE-7 等价或只影响开销的变异**：M16（scoped 取反，只用 `IS` 比较，等价）、M2（Go 过滤只留 function，requeue 路径会重解析）、M10（`ty` 列）、M11、M12、M15 存活，记录不处理。

**检查（含本轮改动的同一棵树）**：`cargo fmt --check` 通过；抽取指纹重录后复核 7/7 通过（INDEX_VERSION 不变）；`cargo clippy --all-targets -- -D warnings` 默认与 `--features embed-model` 都通过；`cargo test --no-fail-fast` 2,530 通过、0 失败、7 忽略（2,527 + 新增 3 个）。

## 进度

- 盲审第 2 轮处理完（上节），提交后 push、CI、tag、Release、发版说明、验证。
- 延期待记：D#901 与 D#899 关闭；新条目——Not covered 列出的剩余增量形态与改类名时 Java/PHP/TS 少数调用方（D#903）；全量一侧“导入模块有同名函数与方法、调用方目录另有同名定义时，显式导入的调用丢边”（D#902）；`CLASS_SHAPE_SELECT` 的逐类 EXISTS 平方开销与 `<module>` 在后一半不排除的探查开销；变异存活清单里的 M8、M10、`ty` 列无测试固定。
