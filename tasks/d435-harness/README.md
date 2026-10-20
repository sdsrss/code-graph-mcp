# D#435 差分工具

0.172.0（D#435/D#445/D#447）发版用的语料差分脚本，原件在 `/var/tmp/d435/harness`，发版后删除了临时目录，脚本留在这里。路径写死为 `/var/tmp/d435`，复用时先改根目录（`drive.py`/`diff.py` 有 `--root`；`mutate.py` 的 target 目录现在是 `/var/tmp/d446/target-mut`）。

- `gen.py`：从各语料的 `.code-graph/index.db`（只读）生成输入，seed 435；类别见 `tasks/specs/d435-mcp-selects-like-cli.md` 的"先测"和第 1、2 轮评审。
- `drive.py <binary> <arm> [--workers N] [--classes ...]`：MCP 经 `mcpc.py`（stdio JSON-RPC 客户端）、CLI `callgraph`/`refs`/`impact` 逐个跑，结果写 `results/<arm>/<corpus>/<surface>.jsonl`。
- `diff.py arms A B` / `diff.py oracle A`：版本间逐字节比较；同一版本 MCP 对 CLI 的裁决比较。
- `transitions.py`、`node_score.py`、`dc_score.py`：变化分类、按输入所指节点打分（参照 `find_references(node_id)` / `callgraph --node-id`）。
- `decline.py`：收窄后每次运行是否等于某个已评审版本的回答。
- `mutate.py <tree> <test> <mutations.json>`：在副本里逐个打变异、跑一个测试、还原。
- `d446.py <armA> <armB> [--root R]`（D#446，0.173.0）：两个版本间每个改变的运行按（面、前裁决、后裁决）分类，列出不在预期集合里的改变、仍是测试路径拒绝的输入、`get_ast_node` 回答的节点是否为测试代码里叫这个名字的节点；改变的输入写到 `results/d446-changed-<A>-<B>-<corpus>.json`。
- `d446_score.py <armA> <armB> <binary>`：对上面改变的输入，`find_references(name)` 与 `find_references(node_id=get_ast_node 回答的节点)` 比较，歧义时比较两个工具的候选 node_id。
- D#446 加了五个语料（express、hono、gin v1.10.0、gson gson-parent-2.11.0、本仓库 `src/`+`tests/` 的 `git archive`，后者要 `git init` 并提交一次）；`gen.py` 对没有 class/method 的语料跳过 D1/DF。
- `rel174.py <armA> <armB> [--root R]`（0.174.0）：两臂间每个改变的运行归入三类——调用图面变成只有外部行时的新提示且名字只有 `<external>` 行、改变前不是回答这个名字本身（外部名）；leveldb 由 ambiguous_exact 变为回答或候选更少的 ambiguous_exact（构造函数）；CLI `refs` 回答→回答，并与同一臂的 MCP `find_references` 比较列表/总数（refs）。其余打印为 UNEXPECTED。
