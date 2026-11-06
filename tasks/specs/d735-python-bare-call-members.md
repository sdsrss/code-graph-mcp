# D#735：Python 函数体里的裸调用不绑定同文件的类成员

状态：进行中（2026-11-05）。级别 L2（解析器 + 索引两个模块，改边，INDEX_VERSION 137 → 138，minor 版本）。来源：待办 D#735（D#714 A3 的 Python 一半）。

## 问题

`98dc4d7b` 让 JS/TS 的裸调用在看得见词法定义时不再绑定同文件的 `method`。Python 当时没做，因为类体里的裸调用确实能调到类体里先定义的函数。可是 Python 函数体（`def`、`lambda` 的体）里的名字只按 局部 → 外层函数 → 模块 → 内置 查找，类作用域不在其中，所以方法里的 `h()` 永远调不到 `C.h`。现在 `g@9 -> h@6` 这类边全是错的。

上限（2026-11-03 oracle dump，"同文件调用方→方法、且调用方同时绑了同名函数"）：nltk 13 条，全错；stdlib 43 错、1 对、52 未判定；daagu 0。

## 规则

裸调用（`call_at` 有值）在 Python 里额外带一个解析期标志：调用是否运行在某个类的类体里。从调用节点往上走：
- 先经 `function_definition` 或 `lambda` 的 `body` 字段到达 → 在函数体里，碰不到类成员；
- 先经 `class_definition` 的 `body` 字段到达 → 在类体里，保持原样；
- 走到模块 → 碰不到类成员。

参数默认值、返回注解、装饰器、类的基类列表都不在 `body` 里，按外层作用域算（Python 就在外层求值）。

解析阶段（`visible_same_file_targets`）：调用看得见词法定义时，碰不到类成员的调用去掉同文件 `method` 候选。JS/TS 一律碰不到（与现状相同），Rust 不变。看不见任何词法定义时全部保留。只从原集合里去掉。

## 形状表

去掉方法（目标：只留词法定义）：
1. 方法体里调用，模块函数 + 同类方法同名。
2. 方法体里调用，同名方法属于文件里另一个类。
3. 方法里的嵌套函数里调用。
4. 函数里定义的类有同名方法，在函数体里调用。
5. 类体里的 `lambda` 体里调用。
6. 模块顶层调用。
7. `async def` 方法、`@staticmethod` 方法里调用。

保持原样（刻意不动）：
- K1 类体里直接调用（类体能看到之前定义的方法）。
- K2 方法参数默认值里调用（在类作用域求值）。
- K3 方法装饰器里调用。
- K4 类体里推导式里调用：Python 3 的推导式体看不到类作用域，严格说应去掉；保守不动，推导式不算函数作用域。
- K5 方法里调用、文件里只有同名方法、没有可见的词法定义。
- K6 `lambda` 默认参数里调用（在外层求值）。

## 验证计划

- 先红：两张表（`a_bare_call_binds_the_same_file_definition_its_scope_sees` 加 Python 行；`a_same_name_member_is_left_to_the_other_rules` 两行 Python 期望改为去掉方法，加 K1–K6）。
- oracle A/B（`tasks/d714-resolver-precision/oracle_ab_lang.sh`，BEFORE = 30bb2aa1 源码构建，AFTER = 工作树）：stdlib、nltk。看默认下限精度、召回、"丢掉的对边"逐条核对（先看 stdlib 那 1 条对边是不是 K1 形状）。
- 增量 = 重建：编辑方法体、类体，`ivchain` 风格比较。
- 全量 `cargo test`、两条 clippy 腿、fmt。

## 决定

- 推导式不算函数作用域（K4）：只会少删，规则保持"只减不增"；要精确得区分第一个 `for` 的可迭代对象（它在外层求值）。
- 标志放进 `ParsedRelation.call_at` 的值里（`BareCall { at, reaches_members }`），不加新字段：其余 20 多处 `call_at: None` 不用改。
- 标志不在 `bare_call_at` 里按调用往上找祖先算：tree-sitter 的 `Node::parent` 从根往下找，每个调用的代价是深度的平方。第一版这样写，900 KB、调用嵌套 200 层的文件全量索引 3.0 s → 58–61 s。改为每个 Python 文件抽完关系后走一遍树收集 `def`/`lambda`/`class` 的 body 范围，按字节序扫一遍调用（`treesitter::mark_python_class_body_calls`），线性：同一输入 2.84–2.85 s。

## 验证记录

- 先红（改实现前的代码，本会话）：`a_bare_call_binds_the_same_file_definition_its_scope_sees` 新增 7 行 Python 全红（如 `py: method beside a module function: got ["g@9 -> h@1", "g@9 -> h@6"]`）；`a_same_name_member_is_left_to_the_other_rules` 的 6 行保持行在旧代码上的输出就是期望（只改了调用方名字：类体里的调用记在 `<module>@1` 名下）。实现后两张表和 `a_scoped_same_file_bind_survives_an_incremental_run` 通过。
- 成本测试 `test_python_class_body_marking_stays_linear`（300 KB，调用嵌套 200 层，60 s 上限）：线性版 debug 3.3 s；变异为逐调用找祖先 → 超过 60 s 失败。
- 语义变异（表格）：类体当函数（MA）、用整个 def 范围代替 body（MB，默认参数行红）、lambda 不算函数（MC）、任一外层是类就算（MD）——4 个都被杀。
- 计时（release，`--no-embed`，各 3 次）：deep 900 KB BASE 3,022–3,340 / HEAD 2,839–2,852 ms；嵌套 lambda 900 KB 138–153 / 155–157 ms；wide 1 MB（30,000 个方法）3,085–3,151 / 2,840–3,356 ms；stdlib 全量 3,099–3,127 / 3,085–3,121 ms（交替跑，负载 0.84）。
- oracle（BASE = 30bb2aa1 源码构建 md5 839d70bf，HEAD = 工作树 e9c8ff84，同一副本）：
  - stdlib：默认下限 10,971/13,400 = 81.87% → 10,971/13,356 = 82.14%，错边 2,429 → 2,385；召回 10,759/12,375 不变；判定边去掉 44 条全是 wrong_target，新增 0、换层 0。全部边（含未判定）114,843 → 114,773：去掉 70 条，全是指向 `method` 的 `calls`，新增 0。未判定的 26 条逐类读源码：`webbrowser.open_new/open_new_tab/main` 调模块函数 `open`（去掉的是 8 个浏览器类的 `open` 方法）；tkinter `_substitute` 里 `getboolean = self.tk.getboolean` 是局部变量——这 25 条的方法都碰不到。第 26 条 `mimetypes._read_windows_registry(cls, add_type)` 的 `add_type` 是参数，调用方传入的是 `self.add_type`（或调用它的 lambda），去掉的正是运行时会执行的那条（第 1 轮评审 LOW-1，写进 Not covered）。
  - nltk：默认下限 5,782/6,475 = 89.30% → 5,782/6,465 = 89.44%，错边 693 → 683；召回 4,984/5,838 不变；去掉 10 条全是判定错边，全部边 30,414 → 30,404，新增 0。
  - 判定为对的边一条没丢（两个语料的"去掉"里没有 correct），D#735 上限里那 1 条对边不在去掉的边里。
- 增量 = 重建：5 步编辑链（方法体调用 → 类体调用 → 删模块函数 → 加回并加另一文件的 `h` → 改成 lambda 体调用），BASE、HEAD 每步边集和 pending 行都与重建相同。
- `cargo test --no-fail-fast`（sweep 改写前的第一版）：2419 通过、0 失败、7 忽略。`cargo clippy --all-targets -D warnings` 两条 feature 腿：通过。

## 预发评审

- 第 1 轮（新 subagent，盲简报，范围 30bb2aa1..eb76f0a0）：HIGH 0、MEDIUM 1、LOW 2、NIT 3。生成器 16,148 个形状对照 CPython 3.14.4：Python 会调到方法的 5,688 个里去掉 0 个；oracle 数字逐项复现；JS/TS/Rust 4 个语料边集不变。逐条核实：
  - MEDIUM-1（复现）：`node_type_of` 每次扫描整个文件的节点列表，新规则让 Python 每个裸调用的每个无作用域候选都走它：N 个调用 × N 个同名方法 × F 个节点（805 KB：21 s → 162 s）。JS/TS 早就这样。改为 `FileParsed.method_ids` 集合查找。先红：`a_python_call_among_many_same_named_methods_stays_linear`（1,500 × 1,500，2 万填充函数，60 s 上限）在修复前超时，修复后 debug 4 s 内。
  - LOW-1（复现）：mimetypes 那条去掉的是运行时调用（参数持有方法）；参数/局部别名/`global` 都属于名字解析看不到的值流，写进 Not covered，CHANGELOG 测量段改为"25 条碰不到、第 26 条是 Python 会运行的"。
  - LOW-2（复现）：MU16（只认模块顶层的类）、MU7（类用整个节点范围）、MU10（不排序 body）存活。表里各加一行（函数里的类体调用、函数里的类的基类、方法默认参数里的 lambda），三个变异各被一行杀掉。
  - NIT-1：类体里的调用保留文件里所有同名方法（别的类、之后定义的、外层类的），写进 Not covered，正文改措辞。NIT-2：3.14 的注解是惰性求值、作用域能看到类体，改措辞。NIT-3（Rust 标志无可观察效果）：保持"Rust 不变"的写法，不改。
  - 修复后二进制（工作树 md5 e3f4f484）：stdlib 114,773、nltk 30,404、本仓库 26,581 条边与 eb76f0a0 构建逐行相同；评审的最坏输入（widecalls 6,000/20,000，805,815 B）BASE 20,866–20,968 ms，修复后 2,297–2,319 ms（修复前评审测 162.5 s）；stdlib 全量 BASE 3,021–3,065 / 修复后 3,027–3,054 ms（交替各 3 次，负载 0.94），CHANGELOG 计时句换成这组数。Not covered 新增的形状逐个跑过：`global h` 的类体 def 被去掉，别的类体、方法之前、嵌套类体里的调用都保留方法。
- 第 1 轮修复提交 57efd309（pre-commit：cargo test 2421 通过、0 失败、7 忽略；fmt；两条 clippy 腿）。提交说明里"N x N x F（805 KB：0.186.0 21 s）"把 0.186.0 的 21 s 记错了来源：0.186.0 不对 Python 调 `node_type_of`，它的 21 s 是 6,000 个调用 × 6,001 个保留目标的插边（第 2 轮 NIT-3，另记 D#745）。
- 第 2 轮（只看 57efd309，新 subagent）：HIGH 0、MEDIUM 0、LOW 5（1 条既有）、NIT 3。9 个语料（Python 3、JS 2、TS 1、Rust 3）57efd309 与 eb76f0a0 边集逐字节相同；R1–R5 成立。逐条核实：
  - LOW-1（复现）：stdlib 唯一调用方走默认 `strict=True`，参数里是调用 `self.add_type` 的 lambda，不是方法本身。CHANGELOG 两处改为"调用方传入调用 `self.add_type` 的 lambda（`strict=False` 时传方法本身）"。
  - LOW-2（复现）：新成本测试的 TempDir 在发送结果后才在分离线程里释放，进程先退出就留下 /tmp/.tmp*（每次约 9 MB）。改为发送前释放；单跑一次后 /tmp/.tmp* 数量 17 → 17，没有新目录。删掉本任务（我和两轮评审）留下的 14 个目录（index.db 对或 `def h(*a)` 开头的 m.py，逐个看过）；另外 3 个（11-03、11-05 05:07 的）不是本任务的，没动。
  - LOW-3（复现）：S16（带装饰器的类体不算类体）、S17（嵌套类体不算）、F9（类也算进 method_ids）存活。保持表加 3 行，各被一行杀掉。
  - LOW-4（复现）：JS/TS 自 0.185.0 起同样的 N×N×F 代价被这次修复一并去掉，CHANGELOG 加一节，数字本机实测。
  - LOW-5（既有）：保留全部候选的调用按 调用数 × 候选数 插边，登记 D#745。
  - NIT-1：`global h` 那句只在模块也定义 `h` 时成立，改措辞。NIT-2：函数里的类，类体调用记在外层函数名下，改措辞。NIT-3：见上。
  - LOW-4 实测（本机，release，`--no-embed`）：评审的 588 KB 输入，JS 0.186.0 164,387 ms → 57efd309 构建 2,233–2,246 ms（3 次），TS 161,148 → 2,216–2,291 ms（2 次），调用边都是 1 条。

