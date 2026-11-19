# 2026-11-19 D#898：一次运行新增的名字，未改动文件里对它的 references 边要等重建才出现

## 为什么

增量索引应与同状态重建一致。0.201.0 发版前实测（本仓库 v0.180.0 → v0.198.0 一次增量运行）：重建比增量多 2,398 条 `references` 边，增量独有 0 条；daagu 200 个提交多 2 条。用户在重建前看不到这些引用（`refs`、`impact`、死代码判断都读它们）。

用户授权：“继续完成剩余的工作，先深入思考给出科学准确的建议，然后按你的建议执行”（2026-11-19，接续 D#901 之后的计划：D#898 → D#897 → D#894 → D#891）。

## 级别

L2：修复增量路径，使其符合“增量 = 重建”的既有行为（bugfix，不改全量的边）；跨 `src/indexer`（解析、扇出）与 `src/storage`（待解析表的读写）两个模块。不改表结构；若全量的边有变化，升级并重新评估 INDEX_VERSION。

## 复现与分类（当前代码 f675077b，release 构建 md5 dccd74fd）

`tasks/d898-references-fanout/replay.py`：索引旧树 → 覆盖成新树 → 一次 `incremental-index` → 与新树的全新索引逐条比较边；`classify.py` 对照运行前的索引（old.db）给每条缺边分类。

| 回放 | 增量耗时 | 增量独有 | 重建独有 |
|------|------|------|------|
| 本仓库 v0.180.0 → v0.198.0 | 3.07 s | 0 | 2,398（references ambiguous 2,350、inferred 48） |
| daagu 10b3ce5d~200 → 10b3ce5d | 5.96 s | 0 | 2（references inferred） |

2,400 条全部是同一类：起点文件在这次运行里没改动；运行前，起点文件之外、同语言里这个名字的定义数是 0；运行前没有这条边；目标文件是这次运行新增的。

## 根因

`resolve_deferred_relations`（`index_files.rs`）对一个 `references` 关系，在本文件和同语言里都找不到目标时直接丢弃（“precision over recall”）。`calls` 在同样情况下会写进 `pending_unresolved_calls`，后来的运行里有了目标，清扫会补上边；导入、继承等结构关系会挂到 `<external>` 占位节点上，扇出轮靠这条边找回导入方。`references` 两样都没有：之后别的文件定义了这个名字，扇出轮（`bare_name_callers_of_new_duplicates`）只找已有 `ambiguous` 边的调用方，找不到这个文件，它要等自己被改动或重建才重新抽取。

删除方向同理：目标文件删除时入边经 `get_inbound_relations_for_requeue` 重新走延迟解析，找不到目标就同样被丢弃，之后重新出现时也不会补上。

## 附带发现（不在本次范围，记入遗留）

缺的 2,398 条里，大部分是全量一侧的错边：起点是推导式变量或生成器变量，被当成全局引用按名绑定。例如 `scripts/audit-metrics.py` 里的 `sum(len(m) for m in dup_marks)`，`m` 绑到 283 个夹具文件里的方法 `C.m`、`D.m`；`evals/_coding/grade.py` 里的 `for d, c, a, m in jobs` 也是这样。`py_enclosing_fn_local_names` 不收推导式的目标变量。这是精度问题，改它会改变全量结果（需要升级 INDEX_VERSION），与本条的一致性问题无关，另记延期项。

## 决定

- **记下被丢弃的引用，名字出现时重新抽取它的文件。**
  - 延迟解析丢弃 `references` 时，把（起点节点、名字、语言）写进现有的 `pending_unresolved_calls`，metadata 固定为 `PENDING_REFERENCE_MARK`（`{"rel":"references"}`）。
  - 扇出轮（`bare_name_callers_of_new_duplicates`）用 `cg_fanout_up` 与这些行按（名字、语言）连接，重新抽取持有行的文件。
  - 重新抽取与重建同一套规则，不在清扫里另写一套 references 的绑定规则；清扫“故意更窄”的规则曾让多批与单批结果不同（`index_files.rs` 的注释）。
- **为什么用现有的表，不建新表**：不改表结构（`SCHEMA_VERSION` 不变）。已有先例：`typed_callers_of_class_drift` 也把待解析行当作“谁要重新抽取”的线索。
- **这些行不参与清扫、老化与计数。**
  - `list_pending_unresolved_calls`、清扫预取名字的查询、`age_and_evict_pending_unresolved_calls` 都排除标记行。
  - `count_pending_unresolved_calls` 也排除，health-check 的“N pending”含义不变。
  - 不老化：清扫从不解析它们，老化计数没有意义。
  - 表的规模有上界：起点文件重新抽取时，行随源节点级联删除，行数等于当前代码里被丢弃的引用数。
- **磁盘内容已变的文件不在这一轮重新抽取**（与 D#543 对已删除文件依赖方的规则相同）。
  - 查询时的刷新（`apply_file_refreshes`）也走扇出轮。它若读了用户改过、尚未索引的文件，下一次增量运行会把这个文件看成没变，欠它的扇出就丢了。
  - 原型没有这一条时，`a_query_time_restore_leaves_an_edited_dependent_to_the_next_run` 失败：`c.rs` 的导入停在 `<external>`，缺 `user → work` 调用。
- **`INDEX_VERSION` 143 → 144**，全量的边不变，升级理由是新的表内容：
  - 旧索引没有这些行，名字后来出现时，自上次索引以来没动过的文件仍缺引用。
  - 0.201.0 会把标记行当成缓冲的调用，名字出现时绑成 `calls` 边。实测：新版建的索引，再用 0.201.0 增量，得到 `calls go → a`，重建是 `references go → a`。
  - 升级后 0.201.0 会拒绝写这个索引（见 Upgrading 实测）。

## 验证记录

- **RED（修前代码 f675077b）**：`d898_a_reference_reaches_a_name_a_later_run_defines`，5 个用例 × 2 条路径（增量运行、查询时刷新）共 10 处失败，每处都是重建独有一条 `references`。五个用例：装饰器 `@a`、回调 `key=a`、新文件里的方法 `C.m`、定义删掉后在别的文件重新出现、TypeScript 的 `wrap(session)`。修后通过。
- **原型的全量测试**：2,527 通过、4 失败。
  - `a_query_time_restore_leaves_an_edited_dependent_to_the_next_run`：查询时刷新的扇出读了磁盘上已改动的 `b.rs`。加上磁盘哈希检查后通过。
  - 两个 Rust 测试：辅助函数 `pending_set` 列出了全部待解析行，新的标记行也被列进来。它们测的是调用的缓冲，`pending_set` 改为只列调用行（注释本来就写“Every buffered call”）。
  - 抽取指纹：按规则重录，`INDEX_VERSION` 144。
- **降级实测**（决定升 `INDEX_VERSION` 的依据）：
  - 新版（未升版的原型 head2）建索引，`use.py` 的 `key=a` 留下标记行。
  - 加上 `def a`，用 0.201.0 增量，得到 `calls go → a inferred`；重建是 `references go → a`。
  - 升版后（head3）：0.201.0 对 v144 索引 `incremental-index` 退出 1，提示“index was built by a newer code-graph (index v144 > this binary v143); not writing to it”。新版对 v143 索引查询时提示“answers come from it until it is rebuilt — run: code-graph-mcp reindex”，增量时提示“Index version mismatch (stored v143 ≠ binary v144): clearing + rebuilding”。
- **新增测试**（共 3 个）：
  - `d898_the_fanout_takes_a_dropped_reference_of_the_language_from_an_indexed_file`：同语言才取；JS 的 `a` 不拉 Python 文件，Python 的 `b` 不拉 JS 文件；磁盘上已改动的文件不取。
  - `d898_a_dropped_reference_row_is_not_a_pending_call`：清扫不绑成 `calls`；老化 51 次后标记行仍在；计数不含标记行。
- **变异**（`mut898.py`，备份还原后 touch，每个都确认编译并跑了测试）：
  - 杀死：K1 不写标记行、K2 扇出查询不匹配、K3 不按语言连接、K4 去掉磁盘哈希检查、K6 标记行参与老化、K7 计数含标记行。
  - 单独存活：K5 清扫列表含标记行、K8 预取名字含标记行，二者互相掩护：任一过滤都使清扫绑不上。K5+K8 同时变异时 3 个测试失败。
- **回放**（`measure.sh` 顺序跑，空闲机器；base = head0 f675077b release md5 dccd74fd，head = head3 md5 8c2f9735；各 3 次，中位数）：

  | 回放 | 重建独有边 base → head | 增量耗时 base → head | 扇出重新抽取文件 base → head |
  |------|------|------|------|
  | 本仓库 v0.180.0 → v0.198.0 | 2,398 → 0 | 3.06 → 3.11 s（head 3.27、3.11、3.06） | 102 → 179 |
  | daagu 10b3ce5d~200 → 10b3ce5d | 2 → 0 | 5.99 → 5.93 s | 393 → 394 |

  两边增量独有边都是 0。
- **全量结果不变**（`fulldiff.py`，每个语料两边各全新索引一次，比较边的两端、关系、置信度、metadata，重复计数）：

  | 语料 | 边（两边相同） | 标记行 | 调用待解析行（不变） | 索引大小 MB | 全量耗时 s（各 1 次） |
  |------|------|------|------|------|------|
  | 标准库 | 113,891 | 3,786 | 8,642 | 68.7 → 69.3 | 2.93 → 2.99 |
  | nltk | 30,258 | 1,358 | 5,212 | 34.2 → 34.3 | 1.27 → 1.30 |
  | flask | 3,705 | 486 | 931 | 5.4 → 5.4 | 0.26 → 0.26 |
  | 本仓库 HEAD | 29,366 | 10,073 | 18,960 | 46.9 → 48.1 | 2.33 → 2.41 |
  | daagu | 77,250 | 7,896 | 25,560 | 94.2 → 95.2 | 3.85 → 3.88 |

  base-only、head-only 都是 0。
- **开销**（`cost.py`，标准库副本，两个二进制在同一轮里交替跑）：
  - 普通编辑（`shutil.py` 末尾加一个函数）7 次中位数：0.24 → 0.24 s，扇出 0 个文件。
  - 新定义一个很多文件“按名引用却没有定义”的名字，是这个改动的最坏情形。标准库里这种文件最多的名字是 `int`（72 个文件），其次如 `exc`（37 个）。
    - 新文件写 `def int()`：0.43 → 2.29 s，扇出 72 个文件，重建是 2.99 s。
    - 写 `def exc()`：0.45 → 1.36 s，扇出 37 个文件。
    - 删掉再加回时，扇出只有 5 个：其余文件由 D#543 的删除记录在运行内重新抽取，不经扇出。
  - 上界：一次运行重新抽取的文件，不超过同语言里引用了它新定义的名字、而当时没有定义的文件数；这些文件在重建里也会得到这些边。
- **提交 82173486**（pre-commit：`cargo test` 2,533 通过 / 0 失败 / 7 忽略，fmt、两条分支的 clippy 都通过）。改了 `domain.rs`，按 runbook 另跑 JS 套件：1,785 个，1,782 通过、0 失败、3 跳过。
- **Upgrading 实测**（head3 = 提交后的代码）：
  - 0.201.0 建的索引（`application_id` 143）：新版查询时给出旧版提示并照常回答；新版 `incremental-index` 清空重建。
  - MCP 服务器（`serve`，经 `tasks/d435-harness/mcpc.py`）启动后 `application_id` 变成 144，`find_references a` 列出 `go`：0.201.0 增量建的这个索引原来没有这条边。
  - 例子：`use.py` 里 `sorted(xs, key=a)`，之后加 `lib.py` 的 `def a()`。0.201.0 的 `refs a` 回答“No references found for 'a'”，新版列出 `[references] go (use.py:1) ~inferred`。
- **全量一侧的错边（D#905）**：重建独有的 2,398 条里，指向 `m` 的 1,415 条（5 个起点 × 283 个名为 `m` 的节点，其中 1,410 条指向方法）全部来自名叫 `m` 的变量，这些变量有三种：
  - 生成器或推导式的目标：`grade.py` 的 `for d, c, a, m in jobs`、`audit-metrics.py` 的 `for m in dup_marks`、`mutbin.py` 的 `{m[0]: m for m in …}`。
  - 模块级变量：`modmap.py` 的 `m = collections.defaultdict(set)`。
  - 元组解包的局部变量：`sim.py` 的 `q, m = q_of(meta)`。
  - 其余名字（`B`、`A`、`n` 等）没有逐个核对。
- **模糊测试**（`fuzz_all.sh`，每步比较；base head0、head head3；第一次跑在模板 `gen_multi.py` 处中止，它不是独立的生成器，脚本已跳过它，剩下 6 个用 `ONLY=` 补跑）：

  | 生成器（种子数） | base 分歧种子 / 行 | head | 其中 references 行 base → head | head 独有种子 / 不在 base 同一步的行 |
  |------|------|------|------|------|
  | 自己的（4,500，3 步） | 15 / 21 | 15 / 21 | 0 → 0 | 0 / 0 |
  | 评审（1,500，4 步） | 56 / 120 | 50 / 105 | 18 → 3 | 0 / 0 |
  | Python 移动（1,500） | 25 / 44 | 22 / 39 | 6 → 1 | 0 / 0 |
  | Go | 33 / 43 | 10 / 15 | 28 → 0 | 0 / 0 |
  | Go2 | 40 / 64 | 18 / 29 | 35 → 0 | 0 / 0 |
  | Java | 32 / 53 | 0 / 0 | 53 → 0 | 0 / 0 |
  | Rust | 75 / 271 | 69 / 259 | 9 → 0 | 0 / 0 |
  | JS | 88 / 143 | 88 / 143 | 0 → 0 | 0 / 0 |
  | TS | 6 / 17 | 6 / 17 | 0 → 0 | 0 / 0 |
  | PHP | 180 / 522 | 180 / 522 | 0 → 0 | 0 / 0 |
  | C++ | 1 / 1 | 1 / 1 | 0 → 0 | 0 / 0 |
  | C、C#、Dart、Kotlin、Ruby、Swift | 0 | 0 | — | 0 / 0 |

  D#901 记的两个 references 种子已与重建一致：88（`REFS = [a, b]`）、229（装饰器 `@a`）。
- **剩下的 4 行 references（不修，记 D#906）**：评审生成器 265、551，Python 移动生成器 1241，都是同一种形态。
  - 最小复现：`lib/k.py` 有 `def b`；`use.py` 写 `from lib.k import b`、`REFS = [b]`，边 `use.py → lib/k.py:b` 是 `inferred`（导入作证）。一次运行给 `app/n.py` 加 `def b`，重建多一条 `use.py → app/n.py:b ambiguous`，增量没有。
  - 原因：扇出只取 `ambiguous` 的 references 边，这条是 `inferred`。
  - 要修就得取任何置信度的 references 边：任何同名定义新增时，导入了这个名字的文件都要重新抽取。TS、Java、Go 的类型引用若严格跟导入走，这些重新抽取只有开销。开销没测；两段真实回放里这种形态出现 0 次。
  - 按“A LOW's repair is the riskiest code”的规则，写进 Not covered，记 D#906。
- **CHANGELOG 0.202.0** 的数字都来自上面各条：
  - 例子 `refs a` 的修前修后回答是实跑的。
  - `int` 的两种写法实测都会留下标记行：`map(int, xs)`、`isinstance(x, int)`，两个函数各一行。
  - 指向 `m` 的 1,415 条边的目标：项目里 283 个名为 `m` 的节点，都在 `tasks/d370g-kit/`，其中方法 282 个、函数 1 个。

## 发版前盲审第 1 轮（192fe243）与处理

评审是新开的子代理，空上下文，只拿到提交范围 f675077b..192fe243、合约、问题和要证伪的说法清单。它自己用 `git archive` 构建了 base（md5 b60419bc，0.201.0）和 head（md5 12719af8，0.202.0）。结论：HIGH 0、MEDIUM 0、LOW 5、NOTE 5。评审确认的事项：

- 9 种语言 25 个用例，增量运行与查询时刷新两条路径：base 分别差 20、21 个，head 差 0。
- 17 个生成器的增量模糊测试与本 spec 的数字一致；刷新路径的模糊测试（8 个生成器 × 750 个种子）references 行 68 → 3；25 组运行都没有 head 独有的分歧。
- 五个语料的全量结果不变。
- 生命周期：编辑 10 次、改名、5 次定义/删除循环、删除文件，都没有孤儿行。
- 升降级行为与 Upgrading 段一致；不升版时，base 会绑出 `calls go → a`。

逐条核对后的处理：

- **L1 CHANGELOG 的 `int` 开销偏低（成立）。** `cost.py` 的 define 场景把“删掉后再定义”的两次（扇出 5 个文件）与首次定义（72 个）混在一起取中位数。评审每次都从全新的已索引副本开始：base 0.50 s、head 3.11 s，略高于一次重建的 2.99 s；`exc` 是 0.38 → 1.69 s。修正：`cost.py` 改成 `new:` 场景，每次从全新副本开始，同时计删除那次运行；重测后改写 CHANGELOG（见下）。
- **L2 最坏情形不封顶，可超过重建（成立）。**
  - daagu 新增方法 `date`：234 个文件，3.59 s，重建 3.79 s。
  - 一个文件定义 50 个最常见的这类名字：942 个文件，8.92 s，是重建的 2.12 倍（构造出来的例子）。
  - CHANGELOG 的 change 一节改为如实写“没有上限、可超过一次重建”，并给出 `date` 的实测。
- **L3 刷新跳过已改动文件后，用户又撤销了修改，边一直缺（成立；base 一样缺）。** CHANGELOG 的“left to the next incremental update, which reads the edit”说过了头，改写，写进 Not covered，记 D#907。
- **L4 旧的歧义调用方那一支没有哈希检查，刷新时会读已改动的文件，进而让新分支找不回引用（成立；base 一样）。** 写进 Not covered，记 D#907。
- **L5 存活变异**：M2（新分支也取调用行，只多开销）、M9、M10 单独（与本 spec 的 K5、K8 相同，互相掩护；M9+M10 被 5 个测试杀死）、M11（近等价）、M13（只多开销）。不补测试，记录在此。
- **N1 全量一侧的另一种错边被复制到增量（成立）。** daagu 的 `from datetime import date` 加 `trade_date: date`，绑到后来新增的项目方法 `ZzProbe.date`（791 条，`inferred`）。写进 Not covered，记 D#908。
- **N2 回放耗时**：评审测得 2.93 → 3.08 s、5.88 → 6.06 s，与本 spec 的 3.06 → 3.11、5.99 → 5.93 都在波动内。CHANGELOG 改成用已提交代码构建的二进制重测后的数字。
- **N3 删除定义文件的开销**：head 2.26 s、base 0.14 s。base 删除时没有那 72 个文件的边要撤，从包含该文件的全新索引出发，base 删除也要 2.35–2.42 s。CHANGELOG 补上这一点。
- **N4** “one row per such reference” 实为每个（源节点、名字）一行。改写。
- **N5** 第二轮扇出的 5 个文件只是开销。不处理。

没有代码改动，只改 CHANGELOG、spec 和测量脚本，所以不开第 2 轮评审（与 0.201.0 第 2 轮的处理方式相同）。

**L1/L2/N2/N3 重测**（`measure2.sh`；二进制是评审从已提交代码构建的 base b60419bc、head 12719af8；`cost.py new:` 每次从全新的已索引副本开始，同时计删除那次运行）：

| 场景 | base | head | head 扇出文件 | 该状态的重建 |
|------|------|------|------|------|
| 标准库新增 `def int()`（5 次） | 0.55 s | 3.36 s | 72，再一轮 5 | 3.09 s |
| 再删掉它 | 0.15 s | 2.73 s | 5 | — |
| 标准库新增 `def exc()`（5 次） | 0.38 s | 1.74 s | 37 | — |
| 再删掉它 | 0.20 s | 1.11 s | 0 | — |
| daagu 新增类的方法 `date`（3 次） | 0.98 s | 3.69 s | 234 | 3.98 s |
| 再删掉它 | 0.35 s | 2.31 s | 0 | — |
| `shutil.py` 末尾加函数（7 次） | 0.25 s | 0.25 s | 0 | — |
| 本仓库回放（3 次） | 3.05 s | 3.18 s | 102 → 179 | — |
| daagu 回放（3 次） | 6.27 s | 6.25 s | 393 → 394 | — |

daagu 加 `date` 方法后的全量索引：234 个文件共 791 条 `inferred` references 指向 `ZzProbe.date`，234 个文件都有 `from datetime import … date`。CHANGELOG 的 change 一节与 Not covered 按此改写。

## 进度

- **已发版 0.202.0**（2026-11-19 UTC，tag v0.202.0 = 0bbca30b）。
  - CI 7/7 一次通过（13:32–13:49 UTC）；macOS 与 Windows 日志里 6 个新增或改过的测试都是 `ok`。
  - Release 10/10 加 3 个发布后冒烟（52 分 50 秒）；发版说明 = CHANGELOG 0.202.0 段；14 个资产。
  - 6 个 npm 包 `_npmUser` 都是 GitHub Actions；npm 实装的 linux-x64 `--version` 0.202.0（md5 40f49b2d）。
- **发布二进制与源码构建**（源码构建为评审从 192fe243 构建的 12719af8，与 0bbca30b 代码相同）：
  - 五个语料全量逐条相同：标准库 113,891、nltk 30,258、flask 3,705、本仓库 29,366、daagu 77,250 条边；调用待解析行、标记行数也相同。
  - 随机序列逐步哈希：自己的 4,500 个、评审 1,500 个、Java 1,500 个、Go2 1,500 个，与 head3 全部相同。
- D#898 关闭。新记延期：D#905（变量被当成引用绑定，全量一侧）、D#906（导入作证的引用不接新同名定义）、D#907（查询时刷新遇到已改动文件的两种缺口）、D#908（导入的库名被绑到项目同名定义，全量一侧）。
