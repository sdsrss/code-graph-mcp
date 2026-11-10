# SQLite 3.51.1 → 3.54.1 差分脚本（D#753，0.190.1）

`drive.py`（索引表与 CLI 输出）和 `mcp_diff.py`（MCP 工具调用）比较两个构建：`bin/base`（v0.190.0）和 `bin/head`（升级版），都从源码以默认特性构建。根目录写死为 `/var/tmp/agent-tmp-sqlite-diff`，布局：`corpora/<名字>`（每份是 git 仓库；提交见 `tasks/var-tmp-kits/README.md`，本仓库用 `git archive v0.190.0` 加一次提交，标准库是 `/usr/lib/python3.14` 去掉 `test/`、`idlelib/idle_test` 后加一次提交）、`bin/{base,head}`、`run-{base,head}/`（脚本复制并建索引）。

    ./build.sh base & ./build.sh head & wait   # src-base = git archive v0.190.0, src-head = the release commit
    python3 drive.py index && python3 drive.py tables && python3 drive.py queries
    python3 mcp_diff.py

结果见 `tasks/specs/sqlite-354-upgrade-2026-11-10.md` 的验证记录和 CHANGELOG 0.190.1。

标准库语料取自本机 `/usr/lib/python3.14`，结果依赖本机 Python 的补丁版本（2026-11-10 为 3.14.4）。
