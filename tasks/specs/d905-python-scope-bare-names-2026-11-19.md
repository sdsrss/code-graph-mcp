# 2026-11-19 D#905+：Python 裸名按 Python 的作用域规则判断，局部值、模块变量、未被遮蔽的内建名不再绑到项目里的同名定义

## 为什么

用户授权：“继续完成剩余的工作，先深入思考给出科学准确的建议，然后按你的建议执行”（2026-11-19，0.202.0 发版之后）。

上一轮计划的顺序是 D#897 → D#894 → D#891。动手前先量了它们和全量一侧精度问题在真实代码里各出现多少次：

- D#897、D#894、D#906 都是随机序列或评审构造出来的形态；两段真实回放（本仓库 v0.180→v0.198、daagu 200 个提交）里出现 0 次。
- 全量索引一侧，用 Python 自己的作用域规则（`ast` 写的判定器，`tasks/d905-scope-oracle/reforacle.py`）逐条检查 0.202.0 全新索引里的 Python 边，错边数以千计（下表）。

所以先做全量精度：每个用户、每次查询都会读到这些边。

### 判定器的结果（0.202.0，5 个语料全新索引）

`references`（Python 起点的全部边）：

| 语料 | 边 | 正确 | 局部值（D#905） | 模块变量 | 内建名 | 同文件嵌套同名 | 外部导入（D#908） |
|------|------|------|------|------|------|------|------|
| 标准库 | 5,732 | 2,869（50.1%） | 690 | 202 | 1,422 | 214 | 24 |
| nltk | 2,118 | 882（41.6%） | 289 | 61 | 804 | 28 | 6 |
| flask | 652 | 352（54.0%） | 4 | 10 | 0 | 269 | 3 |
| 本仓库 | 7,351 | 98（1.3%） | 2,008 | 5,186 | 21 | 26 | 0 |
| daagu | 4,278 | 3,197（74.7%） | 189 | 179 | 3 | 626 | 5 |

裸名 `calls`（`f(...)` 形态；`x.f()` 不在判定器范围内）里，内建名绑到项目同名定义：标准库 8,140、nltk 3,855、flask 19、本仓库 71、daagu 2,588。默认可见（`inferred`/`extracted`）的例子：daagu 313 个 `set()` 绑到 `TTLCache.set`，nltk 289 个 `list()` 绑到 `Downloader.list`、235 个 `range()` 绑到 `Alignment.range`，标准库 182 个 `str()` 绑到 `locale.str`。

## 级别

L2：修复全量结果里的错边（恢复“边是解析出来的调用/引用”这一既有含义，`fix:`），只改解析器（`src/parser/relations`）一个模块；全量的边变化，`INDEX_VERSION` 144 → 145。

## 决定

- **规则只看这个文件自己的文本**：作用域链、模块级绑定、星号导入、内建名表都在本文件里定。别的文件怎么变都改变不了结论，所以不需要新的扇出触发，增量与重建按构造一致。
- **只删不加（输出 ⊆ 0.202.0）**：
  - `references` 保留旧的排除（`py_enclosing_fn_local_names`），新规则只是再多排除一些。
  - 旧排除收得过宽：参数的类型名、默认值、属性赋值 `self.x = …` 里的 `x` 都会被排除，所以有些对的边原来就没有。这一次不改，记入遗留。
- **references 的新规则**：对一个候选名，按 Python 的作用域链从里往外找：
  - 推导式：第一个可迭代对象在外层求值，不算推导式的作用域。
  - lambda：只在函数体里。
  - 函数：只在函数体里；参数默认值和注解在外层求值。
  - 类体：名字必须直接写在类体里，中间没有函数、lambda 或推导式。
  - 找到的绑定是值（参数、赋值、`for`、`with … as`、`except … as`、`:=`、推导式目标）→ 不发引用（D#905 的局部值）。
  - 找到的是定义（嵌套 `def`/`class`、导入、`f=f`）或类体绑定 → 与现在相同。
  - 到了模块级：
    - 模块有这个名字的 `def`、`class` 或导入，或者文件里任何位置有同名的 `def`/`class` → 与现在相同。
    - 模块只有值绑定（赋值等，或某个函数里 `global` 了它）→ 不发（模块变量没有节点，按名绑到的必是别处的同名定义）。
    - 模块没有绑定它：文件有星号导入 → 与现在相同；是内建名 → 不发；否则与现在相同（按名）。
  - 值引用和类型注解两条路径都用这条规则。
- **calls 的新规则只有内建名**：裸调用 `N(...)` 满足以下全部条件时不发调用关系：
  - `scope_scan` 判定它到达模块级；
  - 若直接写在类体里，类体不绑定 `N`；
  - 模块不绑定 `N`，也没有 `global N`；
  - 文件里没有同名 `def`/`class`；
  - 没有星号导入；
  - `N` 在内建名表里。
  - 局部值调用已经标 `ambiguous`（D#789/#1446），不动；模块变量的调用量小（默认可见：标准库 71），不动。
- **内建名表**：Python 3.14 `dir(builtins)` 里不以 `_` 开头的 150 个名字（含 `exit`、`quit`、`help` 等 site 注入的名字），写成 `domain::PYTHON_BUILTINS`。Python 2 的名字（`unicode`、`xrange`）不收：在 Python 3 里它们没有绑定，按名绑定与现在相同。
- **删除而不是降级为 `ambiguous`**：
  - 这与已有做法一致。引用的局部名排除、D#789 列表元素规则、类型注解的内建名表，都是在抽取时就不发。
  - 降级的先例（D#789 的局部值调用）是因为那类边有时是对的，比如 `getregentry` 分发器。内建名、模块变量、推导式变量，按 Python 语义永远不会指向项目里的那个定义。
  - 风险是有些定义失去仅有的入边，变成“没有入边的定义”。按 `feedback_count_orphans_before_pruning_guess_edges` 的做法，量出每个语料零入边定义数的变化，逐个看样本。

## 接受形状表（先写测试）

references，值位置与注解位置：

| 形状 | 期望 |
|------|------|
| `sum(len(m) for m in xs)`、`{m[0]: m for m in xs}`、`[f(m) for m in xs]` | 不发 `m` |
| `[g(y) for y in make(y)]`：第一个可迭代对象里的 `y` | 按外层判断（模块级 `def y` → 发） |
| `lambda cb: run(cb)` | 不发 `cb` |
| `lambda cb=handler: cb`：默认值 | 按外层判断（`def handler` → 发） |
| `with open(p) as fh: use(fh)`、`except E as exc: log(exc)`、`if (w := get()): use(w)` | 不发 |
| 函数里 `a, m = pair(); use(m)` | 不发（旧规则已排除） |
| 嵌套函数用外层函数的局部值 `def o(): v = 1; def i(): use(v)` | 不发 |
| `nonlocal v` 后使用 | 跟到外层：外层是值 → 不发 |
| `global g` 后使用，模块有 `def g` | 发 |
| 模块级 `m = defaultdict(set)`，函数里 `use(m)` | 不发 |
| 模块级 `try: from x import f` / `except ImportError: f = None` | 发（有导入） |
| `isinstance(x, int)`、`map(str, xs)`、`@property`、`x: memoryview` | 不发 |
| 文件有 `def str(...)`（任何层级）时 `map(str, xs)` | 发（与现在相同） |
| 文件有 `from m import *` 时 `map(str, xs)` | 发（与现在相同） |
| 模块级 `from m import set` 时 `use(set)` | 发 |
| 类体里 `def property(self)` 之后的 `@property` | 发（类作用域，与现在相同） |
| 类体里 `x = staticmethod(helper)`，模块 `def helper` | 发 |
| 未绑定、非内建的名字 `register(handler)` | 发（与现在相同） |
| 函数参数默认值里的名字 `def f(cb=handler)` | 与现在相同（旧排除把参数里的标识符都收了） |

calls：

| 形状 | 期望 |
|------|------|
| `set()`、`range(3)`、`open(p)`、`print(x)`、`super()` 在不绑定它的文件里 | 不发调用 |
| 文件模块级 `def open` / `from gzip import open` / `open = io.open` / `global open` | 发（与现在相同） |
| 文件里嵌套 `def set`（任何层级） | 发（与现在相同） |
| 有星号导入 | 发 |
| 类体里 `def set(self)` 之后、类体里直接 `x = set()` | 发（类作用域） |
| 方法体里 `set()`，类体有 `def set` | 不发（方法看不到类体） |
| 参数 `set` 上的调用 `set()` | `{"ur":"local"}`，与现在相同 |
| lambda/推导式里的 `set()` | 不发 |

## 验证计划

- RED：上面两张表写成 `src/parser/relations/tests.rs` 的两个表驱动测试，在 0.202.0 代码上失败。
- 判定器 A/B：用同一份语料副本，0.202.0 与新版各建一次全新索引，逐条比较边。
  - 新版独有边应为 0。
  - 每条只在 0.202.0 里有的边，判定器的结论必须是错边；判定为“正确”的数目必须是 0，不是 0 就逐条解释。
- 零入边定义数：5 个语料两边各数一次，变化逐个看样本。
- 全量耗时与索引大小：两边各跑 1 次，记数字。
- 增量与重建一致：17 个生成器的随机序列（`tasks/d898-references-fanout/fuzz_all.sh`），新版独有的分歧种子必须是 0。
- 全套 `cargo test`、fmt、clippy；按 runbook，改了 `domain.rs` 要另跑 JS 套件。

## 实现中的修正（都先写测试，确认去掉修正时测试失败）

- **模块变量的两个例外。** 第一版 A/B 里，判定器说“不确定”的边经人工核对，有两种其实是对的：
  - `concurrent/futures/__init__.py` 的 `__getattr__` 里 `global ProcessPoolExecutor` 加延迟导入：模块变量持有的正是同名类。
  - `_pyrepl/reader.py` 的 `Command = commands.Command`：别名。
  - 修正：模块级 `X = X` / `X = a.b.X` 记为定义；被任何函数 `global` 声明的名字按原来的方式按名解析。与参数默认值 `f=f`、`heappush=heapq.heappush` 的别名规则相同。
  - 修正后，标准库里 0.202.0 独有的 references 从 2,131 降到 2,101：30 条别名边留下了。
- **注解里 `a.b` 的 `b` 不是裸名。** `x: enum.property` 的 `property` 是 `enum` 的属性。第一版把它当内建名丢掉了。测试是 `annotation attribute, built-in name` 和 `annotation attribute, module variable name`。
- **作用域判断的开销。**
  - 第一版对每个候选名沿祖先节点向上走（`Node::parent` 从根往下找，所以是深度的平方）。
  - 对抗输入（release 构建）：
    - 480 KB、150 层嵌套泛型的注解：0.87 s → 8.06 s；
    - 120 KB、150 层嵌套参数：11.5 s → 17.8 s。
  - 改为复用 `scope_scan` 的作用域表：每个作用域一个字节区间，附带父作用域。候选名二分查找最内层作用域，再沿父链往外。代价只取决于作用域嵌套层数，和语法深度无关。
  - 改后与 0.202.0 持平：0.86 → 0.99 s、11.37 → 11.57 s。
  - 调用一侧也换成同一张表，判断规则不变（`ScopeScan` 的三个既有测试和全套测试都通过）。
  - 成本守卫 `d905_a_scope_read_costs_the_scope_nesting_not_the_syntax_depth`：900 KB 的输入在 debug 构建下要 6.9 s；在读取里注入一次爬到根的祖先遍历，超过 60 s 失败。

## 验证记录

- **RED**（代码 0580ad88）：`d905_a_python_reference_follows_the_scope_chain`、`d905_a_python_bare_call_of_a_builtin_is_no_call` 都失败，失败信息见上文；改后两个都通过。
  - 后补的形状各自做了变异：别名（`got: []`）、`global`（`got: ["pe"]`）、注解属性尾（`got: []`），每个都先确认变红再还原。
- **全套 `cargo test --no-fail-fast`**：2,535 通过，0 失败，7 忽略。
  - 被改动的钉子测试：`edge_coverage` 里有两处原来记着 `caller.py:call_it -> shadow.py:open`，这是 Python 永远不会走的边（`caller.py` 没有导入 `open`，调用的是内建）。
  - 改为：加了 `caller_imp.py`，用 `from shadow import open`；收敛测试改用非内建名 `fetch`，并断言没有任何边指向 `shadow.py:open`。
  - `index_version_guard` 的抽取指纹按规则重录，`INDEX_VERSION` 145。
- **fmt、clippy（两条特性分支）**：通过。
- **A/B**（`tasks/d905-scope-oracle/abdiff.py`；base = 0580ad88 源码构建 md5 12719af8，head = 工作树 md5 dac88a40；每个语料两边各从同一份副本全新索引）：

  | 语料 | 边 base → head | head 独有 | base 独有（calls / references） | 其中默认可见 | Python 零入边定义 | 索引 MB | 全量 s（各 1 次） |
  |------|------|------|------|------|------|------|------|
  | 标准库 | 113,891 → 104,087 | 0 | 9,804（7,673 / 2,131） | 1,918 | 5,169 → 5,237 | 69.3 → 67.3 | 2.82 → 2.66 |
  | nltk | 30,258 → 25,613 | 0 | 4,645（3,697 / 948） | 881 | 2,169 → 2,182 | 34.3 → 33.3 | 1.14 → 1.10 |
  | flask | 3,705 → 3,682 | 0 | 23（19 / 4） | 8 | 745 → 745 | 5.4 → 5.4 | 0.18 → 0.19 |
  | 本仓库 | 29,502 → 22,331 | 0 | 7,171（71 / 7,100） | 273 | 239 → 471 | 47.4 → 46.0 | 2.31 → 2.20 |
  | daagu | 77,250 → 74,477 | 0 | 2,773（2,559 / 214） | 348 | 10,600 → 10,681 | 95.2 → 93.8 | 3.61 → 3.62 |

  - base 独有的边全部过判定器：判为“正确”的 0 条。
  - 判定器拿不准的 62 条逐条看过源码，都是错边：
    - 标准库 calls 2 条：`formatargvalues` 参数默认值里 lambda 中的 `repr(value)`，内建。
    - references 6 条“不确定”：doctest 的局部 `encoding = save_stdout.encoding` 5 条；turtle 模块级 `except ImportError as msg` 1 条。
    - references 47 条“没找到出现位置”：
      - 方法上的 `@property` 装饰器 41 条；
      - `field(default_factory=list)` 4 条；
      - typing 的 `T = TypeVar('T')` 1 条，加上“其他”2 条，这 3 条都绑到 `encodings/punycode.py` 的函数 `T`。
    - nltk calls 5 条：参数默认值里的 `dict()`、`set()`。
  - head 的正确边数与 base 相同：标准库 references 2,889、calls 5,676；nltk 882、2,799。
  - Python references 的正确率：标准库 50.4% → 80.2%，nltk 41.6% → 75.4%。
- **零入边定义与 dead-code**：
  - 新增的零入边定义丢掉的都是上面这些错边。例：nltk 的 `Downloader.list` 原来有 315 条 `calls` 边，几乎都来自内建 `list()`。
  - `dead-code` 一次最多返回 200 条（`DEAD_CODE_SCAN_LIMIT`），这个窗口里的变化：
    - 标准库换进 3 个（`Executor.map`、`Random.sample`、ElementTree 的 `indent`）、换出 3 个；
    - nltk 换进 2 个、换出 2 个；
    - daagu 不变。
  - 换进的都是项目里没有调用方的公开 API，原来靠内建调用或局部变量的错边“活着”。
- **用户可见的例子**：
  - nltk `callgraph Downloader.list --direction callers --depth 1`：197 个调用方（`limit_hit`）→ 24 个。剩下的 24 个都在有星号导入的文件里。
  - 标准库 `callgraph str --file locale.py --direction callers --depth 1`：199（`limit_hit`）→ 47。
  - `refs prefix`：3 → 1，去掉的两条是 `_py_warnings.py:423`、`configparser.py:586` 的推导式变量。
  - `refs T`：9 → 4，去掉 typing.py 的 5 条注解。
- **增量与重建一致**：
  - `parity905.py`：4 个 D#905 形状、13 步，两边都是 0 个分歧步。
  - 随机序列（`tasks/d893-fuzz/fuzz_seq.py`，同一批种子，base 与 head 各跑一次）：

    | 生成器 | 种子 | base 分歧种子 / 行 | head 分歧种子 / 行 | head 独有种子 | head 有、base 同一步没有的行 |
    |------|------|------|------|------|------|
    | 自己的（3 步） | 4,500 | 15 / 21 | 15 / 21 | 0 | 0 |
    | 评审（4 步） | 1,500 | 50 / 105 | 50 / 105 | 0 | 0 |
    | Python 移动 | 1,500 | 22 / 39 | 22 / 39 | 0 | 0 |

    这些生成器只用名字 a/b/c，不生成内建名和推导式。它们说明的是“没有新分歧”；新规则自身的形状由 `parity905.py` 覆盖。
- **升降级实测**：
  - head 查询 v144 索引时提示 "This index was built by an older code-graph (index v144, this binary v145): answers come from it until it is rebuilt — run: code-graph-mcp reindex"。
  - head 增量时提示 "Index version mismatch (stored v144 ≠ binary v145): clearing + rebuilding"。
  - 0.202.0 对 v145 索引 `incremental-index` 退出 1，提示 "index was built by a newer code-graph (index v145 > this binary v144); not writing to it"。
- **既有的开销问题（不在本次范围，新记延期）**：旧的 `py_enclosing_fn_local_names` 对每个候选名都收集最近函数体的全部赋值目标。0.202.0 上：
  - 900 KB 文件、60 层嵌套函数、每行 10 个引用：113 s；
  - 120 KB、150 层嵌套参数：11.4 s，按大小大致 2.5 倍增长。

## 进度

- [x] RED 测试
- [x] 实现（含上面三处修正）
- [x] 判定器 A/B、零入边、耗时、对抗输入
- [x] 随机序列（Python 三个生成器，两边）
- [ ] 提交、CHANGELOG
- [ ] 发版前盲审
