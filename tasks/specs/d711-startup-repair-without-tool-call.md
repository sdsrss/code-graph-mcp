# D#711：启动修复不再等 MCP 工具调用（2026-11-03）

状态：已实现并验证，未发版。来源：`docs/plugin-usage-audit-2026-11-03.md` F2 / D1，待办 D#711。

## 背景

`spawn_startup_repair`（NULL context_string 重建、孤儿向量清理、向量压缩、嵌入缓存校验/播种/回收）只从 `start_post_index_services` 调用，而它要等启动索引完成之后的下一条 MCP 消息（`consume_startup_index_result`）。Claude Code 的握手在启动索引完成前就发完，真实会话调用 code-graph 全走 CLI，所以修复从不运行。审计探针（`tasks/usage-audit-2026-11-03/startup_repair_probe.py`，claudemd 的 index.db 副本，0.184.0）：

| 臂 | 消息 | 向量槽 | 库大小 | `[startup-repair]` |
|---|---|---|---|---|
| A | 只握手 | 78,848 → 81,920 | 148 → 153 MB | 无 |
| B | 握手 + 1 次 tools/call | 78,848 → 4,096 | 148 → 38 MB | 有 |

## 规则

- 启动索引线程在索引成功、`IndexGuard` 释放之后运行启动修复，然后运行嵌入回填（embed-model 构建）。`startup_repair_done` 在决定运行时置位，保证每个进程最多一次。
- 修复和随后的回填在同一次 `embedding_in_progress` 占用下运行：
  - 修复是让步协议之外的写入者，与回填或增量索引并发会出 `SQLITE_BUSY_SNAPSHOT`（`project_mcp_first_call_concurrency`）。占着这个标志时，其他回填会跳过，增量索引会按让步协议等它（最多 `EMBEDDING_YIELD_WAIT_SECS`，超时则跳过并记为欠账）。
  - 一次占用一直延续到回填，中间没有空档让另一个回填插进来，和正在等的增量并发。
  - 标志被别的回填占着时（启动索引超过 60 秒、周期回填先开始），等它释放再运行，不跳过：跳过的话，没有工具调用的会话里修复没有别的触发点。
- 消息路径（`spawn_startup_repair`：提升为主实例、已索引的启动）保留，线程里走同一个函数。
- 索引线程只在本二进制刚成功写完索引后修复；新版本二进制的索引会让索引失败（`index_files` 拒写），所以不修复。消息路径仍由 `may_write_index` 在 spawn 时检查。
- 索引失败时不修复，回填照旧运行（与原来一致）。

## 不做

- 文件监听仍在第一次工具调用时启动：监听事件只在 `ensure_indexed` 里消费，不调用工具的会话里提前启动它没有读者；提前启动带来的首调用等待问题是 D#505。
- 增量索引运行期间没有任何标志，周期回填或等到标志的修复仍可能与之并发。这是现有问题，本次只让修复不再固定撞在第一次工具调用上。

## 验证

- 单元测试（`src/mcp/server/mod.rs`）：
  1. 不调用 `ensure_indexed`，启动索引线程结束后孤儿向量被清理（修复前红）。
  2. 启动索引结束时 `embedding_in_progress` 被占着：修复不运行；释放后修复运行。
- 探针 A 臂（握手）在 claudemd index.db 副本上出现 `[startup-repair]`，向量槽接近活向量数。
- 最大索引（daagu、claude-mem-lite 副本）上修复耗时，对照 `EMBEDDING_YIELD_WAIT_SECS` = 20 秒。
- `cargo fmt --check`、`cargo clippy`、`cargo test`（默认特性 + embed-model）。

## 决定记录

- 修复放在回填之前，而不是之后：它重建的 context string 能在同一轮回填里嵌入；升版本重建后向量已被清空，压缩时活向量为 0，比回填后再压缩少写一整代向量（daagu 实测压缩 0 个，1.06 秒）。
- 不在修复和回填之间单独再查一次 `embedding_yield`：回填循环开头本来就查，代价只是一次模型加载，已计入 `EMBEDDING_YIELD_WAIT_SECS` 的说明。
- 没有在修复函数里再查 `newer_index_version()`：索引线程只在本二进制刚写完索引后进入，消息路径保留 spawn 时的检查。
- 修改了 `src/indexer/pipeline/` 下两行注释（函数名），INDEX_VERSION 守卫要求记录：提取结果不变，不升版本，已更新 `tests/data/extraction_fingerprint.txt`。

## 验证记录

- 新测试修复前：`cargo test --lib -- test_startup_repair_runs_without_a_tool_call test_startup_repair_waits_for_a_running_backfill` → 2 失败（孤儿向量 30 秒内未清理）。修复后 3/3 通过（含原有的 newer-index 测试）。
- 变异：等待改成"被占就跳过" → `waits_for_a_running_backfill` 失败（释放后未运行）；去掉占用 → 同一测试失败（占用期间运行了）。两个变异都编译通过。
- `cargo clippy --locked --all-targets -- -D warnings` 两个特性腿 → 通过。
- `cargo test --locked --no-fail-fast` → 2,387 通过 / 0 失败 / 7 忽略；`--features embed-model` → 2,410 / 0 / 7。第一次运行时 embed-model 腿有 2 个 CLI 端到端测试失败，原因是我在它运行时用默认特性重建了 `target/debug/code-graph-mcp`；顺序重跑后通过。
- 探针 A 臂（只握手），claudemd index.db 副本（v125 → 重建）：
  - 0.184.0 发布二进制：槽 78,848 → 81,920，148 → 153 MB，无 `[startup-repair]`。
  - 本改动 release + embed-model：槽 78,848 → 4,096，148 → 32 MB，修复 0.42 秒；活向量 3,488（两臂相同），嵌入缓存 5,375 → 3,426（只去掉 1,949 条无对应节点的条目）。
- daagu 副本：
  - 升版本重建：槽 245,760 → 12,288，588 → 238 MB，修复 1.06 秒（探针 60 秒空闲内回填未跑完，活向量 11,895/20,540）。
  - 把副本的 `application_id` 改成 126（不重建，最坏情况）：压缩 12,364 个活向量，槽 245,760 → 19,456，588 → 246 MB，修复 1.49 秒，远低于 20 秒。两次都在 cargo test 并行占 CPU 时测得，是上界。
