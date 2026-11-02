# D#370(g)：Python `C.m()` 绑到本文件 `class C` 自己的方法（2026-11-02）

状态：实现完成，验证中（数字见“验证记录”）。

## 背景

用户要求：遗留工作里挑最值得做的，先给出有依据的建议再执行。上一会话的遗留是 D#370（默认置信度层的调用边有 13–40% 是错的，2026-10-16 在 v0.170.1 上每个语料人工抽 30 条判的）。开工前用 SCIP oracle 在 0.182.0 上重测（`scripts/scip_oracle/run.sh`，只计 oracle 能判定的边，默认层 = extracted + inferred）：

| 语料 | 语言 | 默认层判定边 | 错 | 正确率 |
|---|---|---|---|---|
| hono v4.6.14 | TS | 971 | 26 | 97.3% |
| flask 3.1.0 | Python | 295 | 32 | 89.2% |
| Python 3.14 标准库 | Python | 15,827 | 4,824 | 69.5% |
| leveldb 1.23 | C++ | 4,171 | 69 | 98.3% |
| gin v1.10.0 / cobra v1.8.1 | Go | 无 oracle | 机械检查：stdlib 包限定调用绑到项目符号 45/1,504、3/1,475 | — |

结论：10-16 的抽样数字对 TS/C++ 已不成立（之后 (a)(d) 已修，oracle 还把虚函数分派到覆盖方法算作正确）。错边集中在 Python：标准库一个语料的默认层错边是其余语料合计的 30 倍以上。

标准库默认层错边按调用形态分（`/var/tmp/d370/classify.py`）：最大一类是 `ext/member/same/method` 2,801 错 / 1,010 对，其中 2,057 错来自 `C.m(...)`，`C` 是本文件模块级的类（`Widget.__init__(self, …)` 842 条、`Error.__init__` 181 条……）。接收者是类名，但被当成无类型对象（`{"q":"member"}`），于是按名字绑到本文件所有同名方法。

## 语言事实与做法

`C.m` 是对类对象 `C` 的属性查找：按 C 的 MRO 找，C 自己定义了就是 C 的，否则是最近的祖先的；不会是本文件另一个类的同名方法，也不会是子类的覆盖（显式类调用不分派）。这正是 `super()` 限定符已有的绑定（`recv_type_targets(…, dispatch=false)`：自己的方法，否则最近祖先的；都没有则按旧的成员调用解析）。所以只改解析器：符合条件的 `C.m()` 带 `{"q":"super","v":"C"}`。批处理延后、延后遍、pending 清扫、类结构变化的漂移重解析（`typed_callers_of_class_drift` 按 `rtype`/`super` 查）、Phase 2e 置信度豁免都已认 `super`，不需要新代码路径。

前提（每条一个夹具，`test_python_call_on_a_module_level_class_carries_that_class`）：`C` 是模块级 `class C`（可在 `if`/`try`/装饰器下，不在 `def`/`class`/`lambda` 里），且文件里 `C` 的每一次出现都只是读取。判定只读调用方自己的文件，增量与全量不会因它而分歧。

## 接受与拒绝的形态

接受（读取，C 仍是这个类）：类定义本身；属性对象 `C.x` / `C.x = 1`；被调用 `C()`；参数与基类 `f(C)` `class D(C)`；赋值右边 `E = C`；注解 `x: C`、`def f(a: C) -> C`；运算数、`return`、装饰器、f-string、字典键值、`*C`/`**C`、`assert`/`raise`/`if`/`while` 条件；`except C:`、`except C as e`（C 读，e 绑定）；`case C():`；`from x import *` 在类定义之前。元组/列表/括号先向上越过，再按语句判定。

拒绝（保持旧解析）：任何赋值目标（含元组、列表、星号、带注解、增量赋值）；各种形参（含 lambda、仅关键字、带默认值、带注解）；`for`/推导式目标；`with … as C`、`except E as C`；任何 import 绑定；`global`/`nonlocal`；`del`（含 `del a, C`）；海象；同名 `def`（含方法）；同名的嵌套类或类内类；类属性 `C = …`；捕获模式 `case C:`；类型参数、`type C = …`；类定义之后的 `from x import *`；接收者不是 C 本身（`self.C.m()`、`C.D.m()`、`C().m()`、`x.C.m()`）；本文件没有定义的类（`B.m()`，B 只作基类）。允许清单之外的位置一律拒绝。

不变的形态（已知仍错，本项不处理）：`C` 自己和项目内祖先都没有 `m`（来自外部基类，如 `Exception`）时退回旧的成员调用解析，仍按名字绑本文件同名方法；`from x import C` 导入的类上的 `C.m()` 走模块限定调用（`{"q":"module"}`），另一类问题（标准库 453 错 / 33 对，见“遗留”）。

## 决策

- 复用 `super` 限定符而不是新增一个：所有消费者已经按“不分派的类型化调用”处理它；新增一个值要改 ≥6 处按 `IN ('rtype','super')` 写的 SQL 和匹配，漏一处就是增量与全量分歧。代价是名字不够贴切，在 `python_class_call_meta` 和 `CalleeMeta::SuperType` 的文档里写明。
- 只认模块级的类：函数内的类（flask 测试里 `class Index(View)` 定义在测试函数里）要按作用域判定，先不做；这些调用今天大多已对（唯一方法规则）。
- `INDEX_VERSION` 124 → 125。

- 第二步（同一提交）：`recv_type_targets` 在 `dispatch == false`（`super()` 与 `C.m()`，只有 Python 产生）且调用方文件定义了同名顶层类时，只认那个类的方法，未命中再从那个类往上找。原因见验证记录的 HEAD1：只改解析器时，按名字认类会把 tkinter 的 `Widget.__init__` 绑到 ttk.py 里另一个 `Widget` 的 `__init__`（18 条），把 xmlrpc 的 `Error.__init__` 绑到 configparser 的（2 条）。这一步也改了 `super()`：第一基类是本文件的类时同样只认它。

## 验证记录

工作目录 `/var/tmp/d370`（语料、二进制、脚本；用完删除）。二进制：BASE = `git archive 6f4e17de` 源码构建（md5 `08afea01…`），与 0.182.0 发布版在四个语料上边集逐条相同；HEAD1 = 只改解析器；HEAD2 = 解析器 + 锚定（md5 `16cce968…`）。

- RED：解析器形态测试改动前在第一个接受形态失败（实际 `{"q":"member"}`）；管线测试在 BASE 源码副本上失败（`Button.__init__` 绑 `[BaseWidget.__init__, Label.__init__]`）；加入 ttk 同名类后在 HEAD1 上失败（绑 `ttk.py:Widget.__init__`）。GREEN：三个新测试通过。
- oracle（默认层，只计判定边），BASE → HEAD1 → HEAD2：
  - 标准库：错 4,824 → 2,815 → 2,824；对 11,003 → 10,975 → 10,994；正确率 69.5% → 79.6% → 79.6%。HEAD1 少的错边多 9 条，代价是丢了 18 条真正对的（tkinter 继承的 `BaseWidget.__init__`）并新增 20 条错边（上面两类）；HEAD2 新增错边 0。
  - HEAD2 相对 BASE 丢失的“正确”10 条：全部是显式基类调用（`ExpatBuilder.install(self, parser)`、`Node.unlink(self)` 等）到子类覆盖的边，oracle 对任何方法调用都给覆盖方法记正确；逐条核对 HEAD2 仍绑到被点名的类的方法。另有 1 条 inferred 跨文件边被替换：asyncio `super().__init__(extra)` 原绑 xmlrpc 的 `Transport.__init__`，现绑 `BaseTransport.__init__`。
  - flask、hono、leveldb、gin、cobra：HEAD2 边集与 BASE 逐条相同（全部关系、去重后的端点对）；flask 的边行数 9,859 → 9,860，多出的一行是同一端点对的第二个调用点（两个调用的限定符不同，不再合并）。
- 增量 = 全量：`/var/tmp/d370/parity`，D#370(a) 两轮评审的 65 个场景 + 本项 4 个（同名类出现/消失/丢方法、跨文件基类增删方法、调用方文件重新绑定类名、三层继承），BASE 与 HEAD2 都是 68/69 相等；唯一不等的 p15 两边相同（已记为 D#517）。单元测试另有 8 个增量场景。
- 全量测试：`cargo test` 31 个套件 2,374 通过、0 失败、7 忽略；`cargo clippy --all-targets -- -D warnings` 通过；`cargo fmt --check` 通过；`index_version_guard` 指纹已按 INDEX_VERSION 125 重新记录。
- 性能：标准库全量索引（569 个文件，`--no-embed`，各 3 次交替）用户态 CPU 中位数 5.516 s → 5.545 s（+0.5%），墙钟 3.357 s → 3.506 s，两组范围重叠（3.257–3.455 / 3.270–3.527）。

## 遗留

- 标准库默认层剩 2,824 条错边。最大几类（BASE 数字）：未定型 `x.m()` 绑本文件同名方法（去掉本项那 2,057 条后约 740 错）；`from x import C` 或模块限定 `m.f()` / `m.x.f()` 绑进调用方自己的文件（453 错 / 33 对，其中 33 多为 oracle 的覆盖记分）；跨文件未定型成员调用 406 错 / 48 对。方法名同时是内建类型方法名（`get`/`append`/`close`…）时，同文件未定型成员调用在标准库 272 对 / 2,564 错、flask 2 对 / 2 错，可作重新标注的依据，要先按语言模拟。
- oracle 对显式类调用 `Base.m(self)` 也给子类覆盖记正确，会把本项这类修复算成“丢正确”；`scripts/scip_oracle/oracle.py` 的覆盖记分可以排除这种调用形态。
- 函数内定义的类上的 `C.m()` 仍按旧规则（flask 测试里的 `Index.as_view`，今天多数已对）。
