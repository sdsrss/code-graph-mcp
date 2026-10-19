# D#435 差分工具

0.172.0（D#435/D#445/D#447）发版用的语料差分脚本，原件在 `/var/tmp/d435/harness`，发版后删除了临时目录，脚本留在这里。路径写死为 `/var/tmp/d435`，复用时先改根目录。

- `gen.py`：从各语料的 `.code-graph/index.db`（只读）生成输入，seed 435；类别见 `tasks/specs/d435-mcp-selects-like-cli.md` 的"先测"和第 1、2 轮评审。
- `drive.py <binary> <arm> [--workers N] [--classes ...]`：MCP 经 `mcpc.py`（stdio JSON-RPC 客户端）、CLI `callgraph`/`refs`/`impact` 逐个跑，结果写 `results/<arm>/<corpus>/<surface>.jsonl`。
- `diff.py arms A B` / `diff.py oracle A`：版本间逐字节比较；同一版本 MCP 对 CLI 的裁决比较。
- `transitions.py`、`node_score.py`、`dc_score.py`：变化分类、按输入所指节点打分（参照 `find_references(node_id)` / `callgraph --node-id`）。
- `decline.py`：收窄后每次运行是否等于某个已评审版本的回答。
- `mutate.py <tree> <test> <mutations.json>`：在副本里逐个打变异、跑一个测试、还原。
