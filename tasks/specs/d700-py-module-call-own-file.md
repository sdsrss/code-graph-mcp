# D#700(2b)：Python 模块限定调用不再绑到调用方自己的文件（2026-11-02）

状态：`3fc7cdee` 发版前评审第 1 轮：1 MEDIUM（F1）、1 LOW（F2）、4 NIT，正在修。父项：待办 D#700（D#370 在 0.183.0 之后的剩余部分）。

## 背景

用户要求：对 0.183.0 之后的遗留工作给出有依据的建议并执行。开工前用 SCIP oracle 在 0.183.0（`~/.cache/code-graph/bin` 的发布二进制，md5 `a5641033…`）上重测，工作目录 `/var/tmp/d700`：

- 标准库默认层（extracted + inferred）10,996/14,329（76.7%），错 3,333，与发版记录相同。
- 按调用形态分（`tasks/d370g-kit/tools/classify.py`），模块限定调用绑进调用方自己文件的几类（`ext/module*/same/*`）合计错 478、判对 35。
- 判对的逐条看过源码，都不是这次调用的真实目标：
  - `logging.FileHandler.emit(self, record)` 绑到 handlers.py 的 8 个同名 `emit`，oracle 因覆盖关系记为正确，Python 执行的是 `logging/__init__.py` 的那个。
  - 其余的同一对（调用方, 被调方）另有一行边，来自裸调用、`super()` 或类调用。edges 的唯一键含 metadata，那一行不受影响。
- 其他候选：
  - Go 外部包限定调用：gin 45、cobra 3，全是标准库包。下一项，单独做。
  - 同文件未定型 `x.m()`：错 1,305 / 对 795，不能只收窄，留在待办。
  - 跨文件未定型成员调用：错 406 / 对 48，同上。

## 语言事实

`R.a.b.m()` 中 `R` 是名字时，运行时取 `R` 在该作用域的绑定。Python 语言参考 §4.2.1 列出了能绑定名字的结构：

- 形参、`def`、`class`、赋值目标（含增量赋值和带注解的赋值）、`for` 目标、`with … as`、`except … as`；
- 海象赋值、`match` 的捕获和 `as` 模式、`import`、`type`、类型参数、`del`、`global`/`nonlocal`。

如果文件里 `R` 的绑定只有 import（含 `from … import`；`import *` 绑定的也是导入对象），`R` 就是别的模块里的对象，`R.….m` 在那个模块的命名空间里查找。这时目标是调用方文件自己的定义只有三种可能：

- 调用方本身就是模块 V；
- V 经循环的重新导出指回调用方；
- 文件给 `R` 的某个属性赋值，例如 `R.m = 本地函数`。

## 规则

- 解析器（`relations/member.rs`）：模块限定调用 `{"q":"module","v":V}` 的根名字 `R`，只要有一次出现既不在 import 语句里，也不是允许清单里的读取（`py_class_name_is_read`，0.183.0 的同一份清单），或者以 `R` 起头的属性链处在非读取位置（赋值、`del` 等的目标），就给调用加 `"rb":1`。没有这个键表示"只由 import 绑定、没被修改"，所以常见调用的 metadata 不变。
- 解析结果（`pipeline/index_files.rs` 批内默认链）：调用带 `{"q":"module"}` 且没有 `"rb"`，按名字只找到本文件的目标，且调用方路径不可能是模块 V（点分路径不等于 V，也不以 `.V` 结尾）时，丢弃这次调用：不连边，也不转去跨文件解析。其余情况与 0.183.0 相同。
- 带 `"attr"` 的直接调用 `P.X()`（D#518）不在本规则内：Phase 2d-attr 要以调用按名字连出的边为起点，把它改指向模块里的 `X` 并剪掉其余边。批内一旦丢掉这条边，它就没有起点，正确的边也连不上（实现时由索引测试发现）。模块属性能解析时，同文件那条边本来就会被剪掉；解析不到时保持原样（标准库 `module.attr/same` 15 错）。
- 新边集 ⊆ 旧边集：只去掉同文件绑定，不新增。

## 接受与拒绝的形态

不再绑本文件（规则生效）：

- `import V` / `import V as R` / `import V.x`（绑定 `V`）/ `from P import V`，之后 `R.f()`、`R.x.f()`；
- `from V import C`，之后 `C.m()`；
- `R` 的其他出现都是读取：`getattr(R, …)`、`f(R)`、`isinstance(x, R.T)`、`class D(R.Base)`、注解、`except R.E`、`with R.cm()` 等。

保持 0.183.0 的解析（不生效）：

- `R` 在任何作用域另有绑定：`def R`、`class R`、`R = …`、形参、`for R`、`with … as R`、`except … as R`、海象、`del R`、`global R`、match 捕获、`type R`；
- `try: from _x import C` / `except ImportError: class C: …`（本地兜底定义）；
- `R.x = …`、`R.x.m = …`、`del R.x`、`R.x += …` 等写入以 `R` 起头的属性链；
- 调用方点分路径等于 V 或以 `.V` 结尾（可能就是模块 V 自己）；
- 允许清单之外的位置。

已知不覆盖（写进 CHANGELOG）：

- `setattr(R, 'm', f)`、`globals()`、`sys.modules` 改写；
- V 循环地重新导出调用方的定义，例如 `posixpath.py` 里调用 `os.path.join`。

## 决策

- 标记例外（`"rb"`）而不是标记常见情况：已有测试和下游按完整字符串比较 `{"q":"module",…}`，常见调用的 metadata 不变，改动面最小。
- 在结果上收窄，不在候选池上收窄（见记忆 "Narrow the result, not the pool"）：同文件目标被拒后不转去跨文件解析，否则会新增未经检验的边。
- 路径判断用后缀匹配，不用模块映射：只依赖调用方自己的路径，增量和全量的结果一致；比按映射判断更保守。
- 复用 `py_class_name_is_read`，不另写读取清单（该清单已对 CPython 做过生成式校验）。
- `INDEX_VERSION` 125 → 126。

## 验证计划

1. 解析器表驱动测试和索引测试先写，确认失败。
2. 生成器：按"绑定形式 × 作用域 × 属性写入 × 路径"组合生成文件，用 CPython 跑出每个调用实际执行的函数。要求规则拒绝的调用在 CPython 下都不执行本文件的定义，且 HEAD 边集 ⊆ BASE 边集。
3. 六个语料差分：只少不多；oracle 数字；逐条看"丢失的正确边"。
4. 增量与全量一致（parity 场景）；全量索引耗时交替对比。
5. 全量测试、clippy（两个特性组合）、fmt。
6. 新开子代理盲评。

## 验证记录

- RED：解析器表驱动测试 `a_python_module_call_marks_a_root_the_file_rebinds` 在实现前失败（`module assignment: {"q":"module","v":"m"}`）；索引测试 `a_python_module_call_does_not_bind_the_callers_own_definition` 实现前多出 `app.py.go`、`app.py.m` 两条边。
- 变异（每个都确认测试实际跑了、不是编译失败）：去掉 `rb` 判断、自路径的 `==` 腿、`.V` 后缀腿、属性链写入检查、`attr` 豁免，都被抓到；属性链检查的第一次变异编译不过（未使用函数），改写成 `false && …` 后两个测试失败。
- 默认参数值读取：先加 3 个解析器形态（RED：`parameter defaults: {"q":"module","rb":1,"v":"m"}`），再实现；两个变异（去掉这条读取、不要求是 `value` 字段）都被抓到。
- 全量 `cargo test --no-fail-fast`：2,382 通过、0 失败、7 忽略；clippy 两种特性组合通过；pre-commit 通过。
- 默认参数值（`def f(x=m.X)`）算读取（第二步加入，只用于本规则，不改 `C.m()` 共用的清单）：标准库里 `logging/handlers.py` 的 `flushLevel=logging.ERROR`、`tempfile.py` 的 `unlink=_os.unlink`、`zipfile` 的 `whence=os.SEEK_SET` 原先让整文件的调用都保守地保留旧解析（logging/handlers.py 一个文件 149 条边）。用切分法定位：把 6 个仍带 `rb` 的文件拆成 835 段分别建索引，带 `rb` 的 6 段里 3 段是默认参数，另 3 段是真实的重新绑定或修改（`gzip = None`、`warnings.showwarning = …`、类体绑定）。
- 二进制（都从提交的树 `git archive` 构建，release，默认特性）：BASE `2866280f` md5 `b5d409c8…`，HEAD `3fc7cdee` md5 `8616574d…`；对比用的 0.183.0 发布版 md5 `a5641033…` 与源码构建在 0.183.0 发版时已核对过边集逐条相同。
- 生成器（`tasks/d700-kit`，CPython 3.14 判定）：
  - 种子 1：2,999 个用例，declined-ok 1,168，kept-correct 538，kept-wrong 992，无同文件边 251，WRONG 50，ADDED 0；
  - 种子 2：2,996 个用例，declined-ok 1,149，kept-correct 550，kept-wrong 978，无同文件边 266，WRONG 53，ADDED 0；
  - 103 个 WRONG 都含 `setattr`、`globals()[…] =` 或 `vars(…)[…] =`（已知不覆盖）；只含绑定、属性写入或读取的用例 WRONG 为 0。
- 语料差分（BASE = 0.183.0 发布版，HEAD = `8616574d`）：
  - 标准库删 497 条边，全是同文件 extracted 调用，新增 0，改置信度 0；
  - flask 删 7 条，同上；
  - hono、leveldb、cobra、gin 逐条相同。
- 标准库 oracle 默认层：10,996/14,329（76.7%）→ 10,971/13,879（79.0%），错边 3,333 → 2,908（−425）。
  - 判对的 pair 少 25 个：logging/handlers.py 的 `logging.FileHandler.emit(self, record)` 16 个、`logging.Handler.close(self)` 8 个、tkinter/dialog.py 的 `Widget.destroy(self)` 1 个。三处都是显式基类调用，Python 执行的是基类方法，oracle 因子类覆盖记为正确（已知的记分误差）。
  - flask 的 oracle 数字不变（那 7 条在测试文件里，oracle 判定不了）。
- 增量与全量（`parity.py`，d370g 的 88 个场景加本项新增的 5 个，BASE 和 HEAD 各跑一遍）：186 行里 182 行相等。不相等的 2 个场景在两个二进制上的差异逐字相同，与本项无关：
  - `p15`：D#370 时已记录的老问题；
  - `q03`：新出现 `src/ext/__init__.py` 时，`lib/ext.py` 的 `import ext` 这条 imports 边只在全量时连上，是本项之前就有的增量缺口。
- 对抗输入（约 1 MiB，交替各 3 次，取 CPU 中位数）：
  - 大量出现：0.845 → 0.901 s（+6.7%）；
  - 括号：1.016 → 1.077 s（+6.0%）；
  - 写入：0.469 → 0.504 s（+7.5%）；
  - 默认参数：2.399 → 2.270 s（−5.4%）；
  - 26 万段属性链：0.238 → 0.310 s（+30%），RSS 89 → 117 MB。长度增到 1.5 倍时 HEAD 为 ×1.45，BASE 为 ×1.42，线性。

## 发版前评审第 1 轮（`3fc7cdee`，新开子代理，盲评）

结论：C1、C3–C6 成立，C2 被证伪。HIGH 0、MEDIUM 1、LOW 1、NIT 4。

- F1 MEDIUM（复现）：自路径判断只用了根名字的导入来源 `v`，没有用调用实际到达的模块。`a/mod.py` 里 `import a; a.mod.go()`、`from c import mod; mod.go()`、`from e.f import mod as m; m.Klass.go()` 等 5 种写法，CPython 执行的都是本文件的定义，HEAD 却拿掉了边。nltk 3.9.2 里有 7 条（`classify/util.py`、`parse/pchart.py`、`sem/cooper_storage.py`）；标准库和 flask 为 0。
- F2 LOW（复现）：同一个名字多次导入时 `v` 只记最后一次（`import pkg.mod as m` 加上函数里的 `import ext as m`）。
- N1：变异 M7（`.pyi` 调用方）、M22（从属性名开始往上爬链）、M23（后缀判断丢了点号）存活；增量测试只比较增量与全量，不检查规则本身。
- N2：`want` 数组排序后，注释和下面的行对不上。N3：测试钉住了一条已知错误的边（`ext.gone()`，D#518 的范围，行为未变）。
- N4：已知缺口在真实代码里出现（nltk `chunk/util.py` 2 条模块回传）；`mod.__dict__[k] = f`、`mock.patch` 属于同类，CHANGELOG 没写。标准库丢的是 517 行边、497 个（调用方, 被调方）对，CHANGELOG 写成了 "497 call edges"。

## 第 1 轮修复的设计

- 解析器拿到文件路径（`extract_relations_from_tree` 加一个参数；测试入口 `extract_relations` 传"未知"，不做自路径判断）。
- 每个导入绑定的名字收集全部导入目标：`import a.b` 记 `a`，`import a.b as m` 记 `a.b`，`from p import n [as m]` 记 `p.n`，相对导入记"未知"。
- 名字在文件里每次作为属性链的根出现时（任何位置），目标拼上链即可能到达的对象路径。路径的任一段前缀可能就是调用方的模块（调用方点分路径等于它或以 `.它` 结尾；`.py`、`.pyi`、`/__init__` 去掉后比较），或者目标"未知"，就给这个名字加标记。这一条覆盖 F1，也覆盖原来的 `v` 判断（`v` 总是某个目标的前缀）；所有导入都参与，F2 也因此解决。
- 按名字标记，不按调用标记：同一调用方经同一名字的几次调用 metadata 一致，不会在 BASE 只有一行的地方拆出两行。
- 标记键改名为 `"loc"`（可能在本文件解析），因为它不再只表示"重新绑定"；尚未发布，改名没有代价。
- 解析阶段只剩"`q` 为 module、没有 `loc`、没有 `attr`"一条，路径判断删掉，不保留两处来源。

## 第 1 轮修复的实现（`33f5bbb6`）

- RED：新索引测试 `a_python_module_call_into_the_callers_own_module_keeps_its_bind`（F1 的 5 种写法、F2、`next.py`、两个 `.pyi`）在 3fc7cdee 上失败：F1、F2 全部被拿掉，`stubs/other.pyi` 被保留（M7）。
- 带路径的解析器测试 `a_python_module_call_that_may_reach_its_own_module_is_marked`：保持 7 种、标记 10 种，外加 `__init__.py`、`.pyi`、无模块路径、无路径。它写在实现之后，所以逐条做了变异：未知目标、星号导入、链长上限、`ends_with`、只比全长、`/__init__.py` 后缀、`.pyi` 后缀、从属性名起算的链（两处）都被抓到；`from` 分支的 `text(c)` 是等价变异（`from p import n` 的 `n` 只能是单个标识符），已删掉那一支。
- 全量 `cargo test --no-fail-fast` 2,384 通过、0 失败、7 忽略；clippy 两种特性组合通过；pre-commit 通过。
