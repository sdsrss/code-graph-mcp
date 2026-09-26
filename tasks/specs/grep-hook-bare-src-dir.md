---
status: draft
revision: 4
---

# grep hook：接管不带结尾斜杠的源码目录（D#73）

## goal

`grep -rn "X" src`、`rg X tests`、`git grep X src` 这类命令，源码目录写成一个独立的词、后面没有 `/`，现在 hook 完全不接管：`SRC_PATH` 要求 `(src|tests|…)/`。让它们和 `src/` 写法走同一条路径：先提示，满足等价条件时再改写。

## non-goals

- `.` 和不写路径（`grep -rn X .`、`rg X`）：会搜到非源码文件；而且 `grep -r` 不看 .gitignore，`rg`/cg 看，两边搜的文件集合不一样，改写后结果对不上。维持原样。
- 语法解析不了的命令：未知 flag、两个及以上路径、`$(…)`、反引号、`$'…'`、flag 取值等。只要 `rewritePlan` 不能证明裸词是路径参数，就维持原样（r3）。
- 子目录 shell 里没有被 rebase 的裸目录：它指的是 `<cwd>/src`，不是根目录的 src，维持原样（r3）。
- `grep -n X src` 不带 `-r` 时原命令会报 "Is a directory"。`src/` 写法今天就被同样改写，这个问题是既有的，不在本次范围内（见 open-questions）。
- 前缀表 `SRC_PREFIXES` 不变。

## constraints

- 改写报告的是成功，必须和原命令等价（记忆 feedback_a_rewrite_that_reports_success_must_be_equivalent）。只放宽路径识别，`rewritePlan` 的语法和 `rewriteMatchesBlock` 的"同模式、同路径"检查不变。
- `countNamedPaths` 把语法证明的裸目录计为一个路径，和它的 `src/` 写法同样计数。
- 先写接受形态表和语料测试并确认失败，再改实现（记忆 feedback_corpus_first_and_a_review_stop_line）。评审第 3 轮仍在修上一轮的修复，就停下来重新设计。
- 属于 LLM 可见的 hook 行为变化，L3；发布时要 minor 版本 + CHANGELOG 说明 + 关闭开关（现有 `CODE_GRAPH_NO_BLOCK_GREP=1` / `CODE_GRAPH_QUIET_HOOKS=1` 即可）。

## 接受形态表

| 形态 | 现在 | 之后 |
|---|---|---|
| `grep -rn "X_y" src` | 不接管 | 接管（改写，target `src`） |
| `rg "X_y" tests` / `git grep "X_y" src` | 不接管 | 接管 |
| `grep -rn "X_y" ./src` / `./src/` | 不接管 | 接管（`./` 与 `rewriteMatchesBlock` 的规范化一致） |
| `grep -rn "X_y" src tests` | 不接管 | 不接管（语法只接受一个路径参数） |
| `grep -rn "X_y" src 2>/dev/null` | 不接管 | 只提示（与 `src/ 2>/dev/null` 相同：带重定向的命令从不拒绝） |
| `grep -rn "X_y" src \| head` | 不接管 | PostToolUse 注入，搜索范围 src（与 `src/ \| head` 相同） |
| 子目录 `xtask/` 下 `grep -rn "X_y" src` | 不接管 | 不接管（`xtask/src` ≠ 根目录 src） |
| `cd x && grep -rn "X_y" src \| head`、`echo; grep …` | 不接管 | 不接管（grep 不是第一段；r4） |
| `grep -n tasks "task_queue.py"` | 不接管 | 不接管（裸词是搜索词） |
| `ag "X_y" --ignore tests .` / `rg -t cmd "X_y" src/` | 不接管 / 改写 src/ | 不变（flag 取值不是路径） |
| `grep -rn "X_y" .` / `rg "X_y"` | 不接管 | 不接管 |
| `grep -rn "X_y" "src"` | 不接管 | 接管（处在路径参数的位置，shell 就是搜 src） |
| `grep -rn "src" tests/` | 按 tests/ | 不变（搜索词 src 带引号，不当作路径） |
| `grep -rn src_dir tests` / `grep -rn X_y tests` | 不接管 | 只提示（与 `tests/` 相同：不带引号的搜索词从不拒绝；`src_dir` 也不会被当成路径） |
| `grep -rn src tests` | 不接管 | 只提示，不改写：`extractSearchPath` 取到的是搜索词 `src`，与计划的 target `tests` 对不上，`rewriteMatchesBlock` 拒绝；`src` 也不像标识符，本来就不会被拒绝 |
| `grep -rn X srcs` / `src.rs` | 不接管 | 不接管 |
| `grep -rn "X_y" /tmp/clone/src` | 不接管 | 不接管 |

## success-criteria

1. 语料测试：表中每一行在改动前失败（新接受的行）或通过（维持原样的行），改动后全部通过；`pre-grep-guide.test.js` 与 `post-grep-inject.test.js` 全绿，数量与基线对比。
2. 有界差分：在从 transcript 收集的 grep 命令语料上，比较改动前后 `shouldHint` / `classifyDeny` / `rewritePlan+rewriteMatchesBlock` 的判定。新接受的每一条都必须带有裸目录词；除此之外不允许出现新接受的命令。
3. 覆盖率：transcript 语料中 hook 可接管的命令数从 675/1666 提高，提高量等于差分中新接受的条数。
4. 端到端：在一个带索引的临时仓库里，把 `grep -rn "X" src` 喂给 hook，得到改写（`delivery:"rewrite"`），结果与 `src/` 写法相同。

## open-questions

- `grep` 不带 `-r` 搜目录时，原命令报错而改写返回结果（`src/` 与 `src` 都是这样）。是否要在 `rewritePlan` 里对 plain grep + 目录路径要求 `-r`/`-R`？倾向于另开一项，因为它会**收窄**现有行为。

# Change log

- r1 2026-09-26：初稿。
- r4 2026-09-26：第 2 轮评审（有界：head 是否在裸目录参数这一类之外多接受了什么？308 万条组合中为 0）发现 `extractSearchPath` 的文本扫描把形似路径的搜索词当成了范围（B1）、带空格的引号路径被截断（B2）、命令内部的 `cd` 没有被守卫（B3），已修。第 3 轮增量评审确认修复没有产生错误的范围，但 `cd` 守卫可以被 19 种写法绕过（`builtin cd`、`if cd`、`{ cd …; }`、`popd`、`eval`…），于是把黑名单换成白名单：裸目录只在 grep 是整条命令的第一段时才注入。
- r3 2026-09-26：评审第 1 轮（新子代理，无上下文）复现 H1（子目录下裸 src 被改写成根目录的 src）、M1（`grep -n tasks "x.py"` 注入时搜索词和路径对调）、M2（`$(ls -d src tests)` 两个路径只回答一个）、M3（`ag --ignore tests` 把排除的目录当成搜索范围）、L2（`rg -t cmd … src/` 丢失改写）等。没有逐条打补丁，而是改变机制：由语法证明裸词是路径参数，并对子目录加守卫。带引号的 `"src"` 在路径参数位置时改为接受。`src/` 在子目录下的同类老问题另记 defer。
- r2 2026-09-26：两行预期改为"只提示"。r1 写成"改写"是错的：旧版本上 `src/` 写法对这两种形态同样只提示；测试改为与带斜杠写法的对等性断言。
