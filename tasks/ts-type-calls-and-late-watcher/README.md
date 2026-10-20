# 2026-10-20 批次的两个工具

- `edgediff.py <binA> <binB> <work>`：把 `/var/tmp/d446/corpora` 的每个语料（不含 `.code-graph`）复制两份，分别用两个二进制全量建索引（沙箱 HOME），比较每条边：边集合（不含 confidence）是否相同，以及每个标签变化按（旧、新、目标类型、目标语言、关系）计数。D#370(d) 用它得出"只有 hono 82 条 `inferred → ambiguous`"。
- `mcp_probe.py <binary> <corpus> <home> <delay_s> <label>`（评审子代理写，`RV_LOGDIR` 指定 stderr 目录，`RV_NOCHANGE=1` 不写改动文件）：启动 `serve`，等启动索引结束，`delay_s` 秒后在语料里新建一个 TS 文件，再发第一次 `get_call_graph`，打印第一次调用耗时和新函数是否已入索引。语料每次都要用不含 `.code-graph` 的新副本；HOME 里放模型（`.cache/code-graph/models`）才会有后台 embedding。D#436(5) 的两轮评审和 D#505 的重做用它测第一次调用的等待。
