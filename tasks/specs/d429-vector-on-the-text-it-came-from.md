# D#429：向量只存到它所依据的文本上

状态：已实现并验证（2026-10-17），未发版。级别 L2（`src/storage`、`src/indexer`；`fix:`，修的是存错的向量）。来源：0.171.2 发版前评审（已复现），0.171.2 的 Not covered。

## 问题

`nodes.id` 是没有 AUTOINCREMENT 的 `INTEGER PRIMARY KEY`。一个进程在嵌入时（读出 `(id, context_string)`，推理几秒，再写入），另一个进程重建了这个文件的节点；如果这些节点原本是 id 最大的那批，新节点会拿到相同的 id。`insert_node_vectors_batch` 只检查 id 是否还存在，于是旧文本算出的向量写到了新节点上。之后没有任何路径会重新嵌入一个已有向量的节点，直到文件再次变化。

触发需要两个进程：手动运行的 `incremental-index`（不带 `--no-embed`）旁边另有一次索引（另一个 `incremental-index`，或服务器的监视器/补算）。服务器进程内部靠 `embedding_yield` 避开了它。0.171.2 去掉了 CLI 之间"等对方嵌入完"的等待，所以更容易碰上。

## 审计：每条嵌入路径写入和嵌入的是不是同一段文本

修法要求"嵌入的文本 == 写入时节点的 `context_string`"。逐条核对（都汇到 `embed_and_store_batch`，再到 `insert_node_vectors_batch`）：

| 路径 | 文本来源 | 先写后嵌？ |
|---|---|---|
| Phase 3（`index_files.rs` `build_context_strings_and_embed`） | 本次构建的 `updates` | `update_context_strings_batch` 后提交，再嵌入同一个 `updates` |
| `regenerate_context_strings`（`context.rs`） | 本次构建的 `context_updates` | 同上 |
| `repair_null_context_strings`（`context.rs`） | 本次构建的 `context_updates` | 同上 |
| CLI `embed_missing_nodes`（`index_ops.rs`） | `UnembeddedQueue::next_chunk` 从 `nodes.context_string` 读出 | 读的就是已存的文本 |
| 服务器补算（`backfill.rs`） | 同上 | 同上 |

`update_context_strings_batch` 原样写入（`UPDATE nodes SET context_string = ?1`），没有截断或改写。

## 设计

- `insert_node_vectors_batch(conn, vectors, embedded_from)`：多一个"每个向量依据的文本"的映射；写入前的检查从 `id = ?1` 改成 `id = ?1 AND context_string = ?2`。映射里没有的 id 跳过。
- `embed_and_store_batch` 的三处写入（缓存命中、逐条回退、批量）都传入由 `context_updates` 建的同一个映射。
- 返回值语义写清：返回"处理完"的 id，包括因节点已删除或已被换成别的文本而跳过的。原来的代码对"已删除"就是这样做的（文档说"实际拿到向量的"，与代码不符）。被换掉文本的节点不算失败：补算循环把未返回的 id 记为失败，服务器还会把失败当作"嵌入不了的残留"钉住地板值、不再重试；不报失败，它仍是未嵌入的，队列重建时按新文本再取到它。
- 测试里给夹具塞向量的四处（`freshness_parity.rs`、`integration.rs`、`cli_e2e.rs` 两处）改用不带检查的 `insert_node_vector`。

## 验证记录

- RED：`embed::tests::a_vector_is_stored_only_on_the_text_it_was_computed_from`（复用 id：删掉文件的节点再插入，id 相同；旧文本的缓存命中）修复前失败：旧文本的向量被写到新节点上，且报告为已嵌入。
- 变异（把检查改成 `id = ?1 AND ?2 = ?2`）：默认 feature 下 `a_vector_is_stored_only_on_the_text_it_was_computed_from`、`vectors::tests::a_late_insert_skips_a_node_whose_text_changed` 变红；embed-model 下 `embed::model_tests::a_fresh_embedding_is_stored_only_on_the_text_it_was_computed_from`（真模型、批量路径，模型已加载，0.96 s）和后者变红。恢复后全绿。
- 端到端（`race429.sh`，评审的场景：a.py 110 个函数 + z.py 10 个函数，CLI `incremental-index` 嵌入到 64/123 时改写 z.py 并运行 `incremental-index --no-embed`；参照 = 同一棵最终树全新索引）：

| 二进制 | z.py 10 个节点 | 与新文本向量的余弦 | 结束时进度 |
|---|---|---|---|
| 0.171.2（`base`，md5 `4e287531…`） | 10 个旧向量 | 0.7936–0.8148 | 123/123 |
| 修复后（`d429`，md5 `f0f75cec…`） | 10 个新向量 | 1.0000 | 123/123 |

  修复后进度照样是满的：同一次运行里按新文本重新取到了这 10 个节点。
- 对照（检查不会丢掉正常的向量）：上表的参照索引（CLI 全量）两个二进制都是 123/123；经 MCP 服务器（`server429.py`，60 个函数，启动后补算，再追加 10 个函数让增量 + 补算再跑一遍）两个二进制都是启动 61/61、编辑后 71/71。
- `cargo clippy --all-targets -- -D warnings`（默认、embed-model 两条）、`cargo fmt --check` 通过。

## 遗留

- 逐条回退路径（`sp_embed_sequential`）只在批量推理报错时走到，测试里触发不了；它和另两处传的是同一个映射，靠读代码确认。

## 发版前评审（0.171.3 第 1 轮）

- F5 LOW（读代码）："同一次运行里按新文本嵌入"依赖时机：只有另一边在本次队列耗尽前写好 `context_string`，队列重建才会取到；否则留到下一次嵌入运行。CHANGELOG 已按这个说法改写。
- F6 LOW（已复现）：CLI 结尾的 `Embedded N nodes` 把被跳过、又重新嵌入的节点算了两次：race 里 r1 打印 `Embedded 133 nodes (123/123)`，0.171.2 打印 123。没改（只在这个竞态里出现），写进 Not covered。
