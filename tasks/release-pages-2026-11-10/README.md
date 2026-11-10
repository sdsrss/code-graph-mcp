# 2026-11-10 旧 Release 页面改写（D#361 剩余）

`originals.json` 是 23 个 GitHub Release 页面（v0.143.0–v0.163.0 含 v0.145.1，加 v0.166.0）改写前的正文（`tag`、`id`、`body`）。每页只把 `cargo install code-graph-mcp --version X` 改成 `cargo install --git https://github.com/sdsrss/code-graph-mcp --tag vX`（crates.io 上没有这个 crate），改后逐页比对线上正文与预期一致。

还原某一页：

    python3 -c "import json,sys; p={x['tag']:x for x in json.load(open('originals.json'))}[sys.argv[1]]; open('/tmp/body.md','w',newline='').write(p['body'])" v0.150.0
    gh release edit v0.150.0 --notes-file /tmp/body.md

另外，v0.190.0 的页面在同一天（2026-11-10）随 CHANGELOG 0.190.0 段的改动重新生成过（那一条说明从"仍写着"改为"已改写"），它的原文没有收进 `originals.json`：原文等于 tag v0.190.0 上 CHANGELOG 的 `## 0.190.0` 段（`git show v0.190.0:CHANGELOG.md`）。
