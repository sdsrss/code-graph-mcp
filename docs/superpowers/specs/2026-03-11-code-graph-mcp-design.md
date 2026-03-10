# code-graph-mcp 设计文档

> 本地零配置代码分析 MCP 服务器 — 语义搜索 + 知识图谱 + 增量索引

## 1. 概述

### 目标

构建一个本地运行的 MCP (Model Context Protocol) 服务器，为 Claude Code 提供智能代码索引和查询能力：

- **语义代码搜索**：自然语言查询代码，混合全文+向量+图关系
- **知识图谱**：函数调用链、继承关系、路由映射的图查询
- **增量索引**：Merkle 树变更检测，毫秒级增量更新
- **Context 虚拟化**：大结果集自动压缩为摘要+按需读取指针
- **零配置**：单二进制文件，内嵌 ML 模型，自动解析 .gitignore

### 设计原则

- **隐私优先**：全部本地运行，无云依赖，无 API Key
- **Token 高效**：图查询替代全文件读取，沙箱压缩大结果集
- **极致性能**：毫秒级查询，<200ms 增量索引
- **零配置**：即插即用，无需任何设置

### 技术灵感来源

| 项目 | 提取的能力 |
|------|-----------|
| zilliztech/claude-context | Merkle Tree 增量索引 + Hybrid Search (BM25 + 向量) |
| DeusData/codebase-memory-mcp | 知识图谱，关系>原始代码，省 99% token |
| johnhuang316/code-index-mcp | 零配置 + .gitignore 自动解析 + 精确符号搜索 |
| mksglu/context-mode | Context 虚拟化/沙箱 (315KB → 5.4KB) |
| danielbowne/claude-context | 零依赖本地向量存储 (sqlite-vec) |
| Augment (商业产品) | 实时增量同步 + 自定义嵌入 + 架构理解 |

## 2. 技术栈

| 层级 | 技术 | 用途 |
|------|------|------|
| 语言 | **Rust** | 单二进制，极致性能，candle 生态 |
| MCP 协议 | serde_json + tokio | JSON-RPC 2.0 over stdio |
| 代码解析 | tree-sitter + 语言 grammars | AST 解析，支持 TS/JS/Go/Python/Rust/Java/C/C++/HTML/CSS |
| 存储 | rusqlite (bundled + fts5) | SQLite，内嵌 FTS5 全文搜索 |
| 向量搜索 | sqlite-vec (vec0) | 本地 384 维向量相似度搜索 |
| 嵌入模型 | candle + all-MiniLM-L6-v2 | 本地推理，~22MB 模型，384 维输出 |
| 文件监听 | notify | 跨平台文件系统 watcher |
| 哈希 | blake3 | Merkle 树构建，最快的哈希函数 |
| .gitignore | ignore crate | 自动跳过 node_modules、dist 等 |

## 3. 系统架构

```
┌─────────────────────────────────────────────────────┐
│                   MCP Protocol Layer                 │
│              (JSON-RPC 2.0 over stdio)               │
│  Tools: semantic_search | call_graph | find_route    │
│         | get_ast_node | read_snippet                │
│         | start_watch  | stop_watch                  │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────────┐
│                  Query Engine                         │
│  ┌─────────┐  ┌──────────┐  ┌────────────────────┐  │
│  │ FTS5    │  │ Vec0     │  │ Graph (递归CTE)    │  │
│  │ BM25搜索│  │ 向量搜索  │  │ 调用链/继承/路由   │  │
│  └────┬────┘  └────┬─────┘  └────────┬───────────┘  │
│       └────────┬───┘                 │               │
│           RRF Fusion ◄───────────────┘               │
│                │                                      │
│       Context Sandbox                                │
│       (大结果→摘要+指针)                               │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────────┐
│                 Storage Layer                         │
│           SQLite (.code-graph/index.db)               │
│  Tables: files | nodes(FTS5) | edges | node_vectors  │
│          | context_sandbox | merkle_state            │
└──────────────────────┬──────────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────────┐
│              Indexing Pipeline                        │
│  ┌──────────┐  ┌───────────┐  ┌──────────────────┐  │
│  │ Watcher  │  │ Parser    │  │ Embedding Engine │  │
│  │ notify + │→│ Tree-sitter│→│ candle +         │  │
│  │ Merkle   │  │ AST分块    │  │ MiniLM-L6-v2    │  │
│  │ (blake3) │  │ +关系提取  │  │ +图上下文注入     │  │
│  └──────────┘  └───────────┘  └──────────────────┘  │
└─────────────────────────────────────────────────────┘
```

### 数据流

1. **索引流**：文件变更 → Merkle diff → Tree-sitter AST 解析 → 提取节点+关系 → 图上下文注入 → candle embed → SQLite 写入
2. **查询流**：MCP 工具调用 → 按需增量检查 → FTS5/Vec0/Graph 三引擎并行 → RRF 融合 → Context Sandbox 压缩 → 返回

### 模块划分

```
src/
├── main.rs              // 入口，初始化
├── mcp/                 // MCP 协议层
│   ├── mod.rs
│   ├── protocol.rs      // JSON-RPC 解析/序列化
│   ├── tools.rs         // 7个工具的 handler 注册
│   └── types.rs         // 请求/响应类型定义
├── parser/              // AST 解析
│   ├── mod.rs
│   ├── treesitter.rs    // Tree-sitter 核心封装
│   ├── languages.rs     // 多语言 grammar 加载
│   ├── chunker.rs       // AST 节点分块策略
│   └── relations.rs     // 调用/继承/路由关系提取
├── graph/               // 知识图谱
│   ├── mod.rs
│   ├── store.rs         // edges 表 CRUD
│   └── query.rs         // 递归 CTE 查询
├── embedding/           // 嵌入引擎
│   ├── mod.rs
│   ├── model.rs         // candle 模型加载+推理
│   └── context.rs       // 图上下文注入
├── search/              // 混合搜索
│   ├── mod.rs
│   ├── fts.rs           // FTS5 全文搜索
│   ├── vector.rs        // vec0 向量搜索
│   └── fusion.rs        // RRF 排序融合
├── sandbox/             // Context 虚拟化
│   ├── mod.rs
│   └── compressor.rs    // 大结果→摘要+指针
├── indexer/             // 增量索引管线
│   ├── mod.rs
│   ├── merkle.rs        // blake3 Merkle 树
│   ├── watcher.rs       // notify 文件监听
│   └── pipeline.rs      // 编排：diff→parse→embed→store
├── storage/             // SQLite 存储层
│   ├── mod.rs
│   ├── db.rs            // 连接管理、迁移
│   ├── schema.rs        // 建表 DDL
│   └── queries.rs       // 预编译 SQL 语句
└── utils/               // 通用工具
    ├── mod.rs
    ├── gitignore.rs     // ignore crate 封装
    └── config.rs        // 运行时配置
```

## 4. 数据库 Schema

### 核心表：文件追踪

```sql
CREATE TABLE files (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    blake3_hash TEXT NOT NULL,
    last_modified INTEGER NOT NULL,
    language    TEXT,
    indexed_at  INTEGER NOT NULL
);
```

### 核心表：AST 节点

```sql
CREATE TABLE nodes (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    type        TEXT NOT NULL,       -- 'function'|'class'|'method'|'interface'|'struct'|'enum'|'route'|'module'
    name        TEXT NOT NULL,
    qualified_name TEXT,             -- 完整限定名
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    code_content TEXT NOT NULL,
    signature   TEXT,
    doc_comment TEXT,
    context_string TEXT NOT NULL     -- 图增强上下文（路由+调用链+继承）
);

CREATE INDEX idx_nodes_file ON nodes(file_id);
CREATE INDEX idx_nodes_type ON nodes(type);
CREATE INDEX idx_nodes_name ON nodes(name);
```

### FTS5 虚拟表

```sql
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    name, code_content, context_string, doc_comment,
    content='nodes', content_rowid='id'
);

-- 同步触发器（INSERT/UPDATE/DELETE）
CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, name, code_content, context_string, doc_comment)
    VALUES (new.id, new.name, new.code_content, new.context_string, new.doc_comment);
END;
CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, code_content, context_string, doc_comment)
    VALUES ('delete', old.id, old.name, old.code_content, old.context_string, old.doc_comment);
END;
CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, code_content, context_string, doc_comment)
    VALUES ('delete', old.id, old.name, old.code_content, old.context_string, old.doc_comment);
    INSERT INTO nodes_fts(rowid, name, code_content, context_string, doc_comment)
    VALUES (new.id, new.name, new.code_content, new.context_string, new.doc_comment);
END;
```

### 向量表

```sql
CREATE VIRTUAL TABLE node_vectors USING vec0(
    node_id INTEGER PRIMARY KEY,
    embedding float[384]
);
```

### 关系表：知识图谱边

```sql
CREATE TABLE edges (
    id          INTEGER PRIMARY KEY,
    source_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation    TEXT NOT NULL,    -- 'calls'|'inherits'|'implements'|'imports'|'routes_to'|'contains'
    metadata    TEXT,             -- JSON
    UNIQUE(source_id, target_id, relation)
);

CREATE INDEX idx_edges_source ON edges(source_id);
CREATE INDEX idx_edges_target ON edges(target_id);
CREATE INDEX idx_edges_relation ON edges(relation);
```

### Context Sandbox

```sql
CREATE TABLE context_sandbox (
    id          INTEGER PRIMARY KEY,
    query_hash  TEXT NOT NULL,
    summary     TEXT NOT NULL,
    pointers    TEXT NOT NULL,    -- JSON: [{node_id, snippet_range, relevance}]
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);

CREATE INDEX idx_sandbox_query ON context_sandbox(query_hash);
```

### Merkle 树状态

```sql
CREATE TABLE merkle_state (
    dir_path    TEXT PRIMARY KEY,
    tree_hash   TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
```

### SQLite 配置

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -64000;      -- 64MB
PRAGMA mmap_size = 268435456;    -- 256MB
PRAGMA temp_store = MEMORY;
PRAGMA foreign_keys = ON;
```

## 5. MCP 工具 API

### 5.1 semantic_code_search

语义代码搜索。混合 BM25 全文 + 向量语义 + 图关系，返回最相关的 AST 节点。

**输入**:
- `query: string` (必填) — 自然语言查询
- `top_k: number` (可选, 默认5) — 返回结果数
- `language: string` (可选) — 限定语言
- `node_type: string` (可选) — 限定节点类型

**输出**: `[{name, qualified_name, type, file_path, lines, signature, code_snippet, relevance_score}]`

**Context Sandbox 触发逻辑**:
- 结果集 token 数 <= 2000 → 直接返回完整代码
- 结果集 token 数 > 2000 → 返回摘要 + pointer_id，Claude 用 `read_snippet` 按需读取

### 5.2 get_call_graph

查询函数的上下游调用链。递归 CTE 遍历知识图谱。

**输入**:
- `function_name: string` (必填)
- `direction: string` (可选, 默认'both') — 'callers' | 'callees' | 'both'
- `depth: number` (可选, 默认2)
- `file_path: string` (可选) — 同名函数消歧

**输出**: `{root: {name, file, line}, callers: [{name, file, line, depth}], callees: [...]}`

**核心 SQL**:
```sql
WITH RECURSIVE call_chain(node_id, name, file_path, depth) AS (
    SELECT n.id, n.name, f.path, 0
    FROM nodes n JOIN files f ON n.file_id = f.id
    WHERE n.name = ?1
    UNION ALL
    SELECT n2.id, n2.name, f2.path, cc.depth + 1
    FROM call_chain cc
    JOIN edges e ON e.source_id = cc.node_id AND e.relation = 'calls'
    JOIN nodes n2 ON n2.id = e.target_id
    JOIN files f2 ON n2.file_id = f2.id
    WHERE cc.depth < ?2
)
SELECT * FROM call_chain ORDER BY depth;
```

### 5.3 find_http_route

从路由路径追踪到后端处理函数。

**输入**:
- `route_path: string` (必填) — 如 '/api/users' 或 'POST /api/login'
- `include_middleware: boolean` (可选, 默认true)

**输出**: `{route, method, handler: {name, file, line, code}, middleware: [{name, file}], downstream_calls: [...]}`

### 5.4 get_ast_node

精确提取某个文件中的代码符号。

**输入**:
- `file_path: string` (必填)
- `symbol_name: string` (必填)
- `include_references: boolean` (可选, 默认false)

**输出**: `{name, type, file_path, lines, signature, doc_comment, code, references?: [{name, file, line, relation}]}`

### 5.5 read_snippet

根据 Context Sandbox 指针按需读取原始代码。

**输入**:
- `pointer_id: number` (必填)
- `context_lines: number` (可选, 默认3)

**输出**: `{node_name, file_path, lines, code_content, surrounding_context}`

### 5.6 start_watch

启动文件系统实时监听。

**输入**: 无
**输出**: `{status: 'watching', watched_dirs: number, ignored_patterns: [string]}`

### 5.7 stop_watch

停止文件系统实时监听。

**输入**: 无
**输出**: `{status: 'stopped', indexed_since_start: number}`

## 6. 索引管线

### 完整流程

```
文件变更检测 → Merkle Diff → 清理已删除文件(CASCADE)
→ Tree-sitter 解析(检测语言→加载grammar→提取节点→提取关系)
→ 两阶段关系解析(Phase1: 节点入库 → Phase2: 跨文件关系匹配写入edges)
→ 图上下文构建(拼接路由+调用链+继承链 → context_string)
→ candle 批量嵌入(每批32个节点)
→ 单事务写入(nodes + FTS5触发器 + node_vectors + edges + merkle_state)
```

### 图增强嵌入 (Graph-Augmented Embedding)

核心差异化特性：不对原始代码做嵌入，而是对「带图上下文的语义描述」做嵌入。

**context_string 模板**:
```
{type} {name}
in {file_path}
signature: {sig}
routes: {routes}
calls: {callees}
called_by: {callers}
inherits: {parents}
doc: {doc_comment}
```

**示例** — 原始代码:
```typescript
async function validateToken(token: string): Promise<User | null> {
  const decoded = jwt.verify(token, SECRET);
  return await UserRepo.findById(decoded.userId);
}
```

**图增强后的 context_string**:
```
function validateToken
in src/auth/middleware.ts
signature: (token: string) -> Promise<User | null>
routes: POST /api/login, GET /api/profile (via middleware chain)
calls: jwt.verify, UserRepo.findById
called_by: authMiddleware, handleLogin, handleRefreshToken
doc: Validates JWT token and returns the associated user
```

搜索"用户认证流程"时，即使代码不含"认证"二字，通过路由和调用链上下文也能匹配。

### Tree-sitter 查询策略

| 语言 | 节点类型 | 关系提取 |
|------|---------|---------|
| TypeScript/JS | function_declaration, class_declaration, method_definition, arrow_function (named), interface_declaration | call_expression→calls, extends_clause→inherits, import_statement→imports, decorator(route)→routes_to |
| Go | function_declaration, method_declaration, type_declaration | call_expression→calls, qualified_type→implements, import_declaration→imports, http.HandleFunc→routes_to |
| Python | function_definition, class_definition | call→calls, bases→inherits, import_statement→imports, @app.route→routes_to |
| Rust | function_item, impl_item, struct_item, enum_item, trait_item | call_expression→calls, impl trait→implements, use_declaration→imports |
| Java | method_declaration, class_declaration, interface_declaration | method_invocation→calls, superclass/interfaces→inherits/implements, @RequestMapping→routes_to |
| C/C++ | function_definition, class_specifier, struct_specifier | call_expression→calls, base_class_clause→inherits, #include→imports |

### 增量更新的边界处理

文件 A 变更可能影响引用 A 节点的其他文件的 context_string。

**延迟传播 + 脏标记策略**:
1. 文件 A 变更 → 重新解析 A 的节点和边
2. 查 edges 表：A 的节点被谁引用
3. 将引用方节点标记为 context_dirty
4. 重新生成脏节点的 context_string + 重新 embed
5. 传播深度限制为 1 层（避免级联爆炸）

### 混合搜索 RRF 融合

```
Step 1: FTS5 BM25 搜索 → 取 top 20
Step 2: Vec0 向量搜索 → 取 top 20
Step 3: RRF 融合: score(d) = Σ 1/(k + rank_i(d)), k=60
Step 4: 按 RRF 总分排序，取 top_k
```

## 7. 错误处理

| 场景 | 处理方式 |
|------|---------|
| Tree-sitter 解析失败 | 跳过该文件，记录 warning，下次变更时重试 |
| candle 推理失败 | 节点入库但不生成向量，退化为纯 FTS5，标记 `[vector_unavailable]` |
| SQLite 写入冲突 | WAL 模式读写并发，写操作单线程事务 |
| sqlite-vec 加载失败 | 禁用向量搜索，仅 FTS5 + Graph |
| 文件删除残留 | Merkle diff 检测 + CASCADE 删除，启动时全量 diff 兜底 |
| 超大仓库首次索引 | 分批 100 文件，进度通过 MCP notification 推送，支持中断续建 |
| 同名函数消歧 | 支持 file_path 参数，多匹配时返回全部并提示 |

## 8. 性能约束

| 指标 | 目标 |
|------|------|
| 启动时间（已有索引） | < 50ms |
| 增量索引（单文件） | < 200ms |
| 首次全量索引（50k 行） | < 60s |
| semantic_code_search | < 100ms |
| get_call_graph | < 10ms |
| get_ast_node | < 5ms |
| 内存占用 | < 200MB（含模型） |
| 二进制体积 | < 80MB（含模型） |

## 9. Rust 依赖

```toml
[dependencies]
# MCP 协议
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

# SQLite
rusqlite = { version = "0.31", features = ["bundled", "fts5", "functions"] }

# Tree-sitter
tree-sitter = "0.22"
tree-sitter-typescript = "0.21"
tree-sitter-javascript = "0.21"
tree-sitter-go = "0.21"
tree-sitter-python = "0.21"
tree-sitter-rust = "0.21"
tree-sitter-java = "0.21"
tree-sitter-c = "0.21"
tree-sitter-cpp = "0.21"
tree-sitter-html = "0.20"
tree-sitter-css = "0.21"

# 嵌入引擎
candle-core = "0.6"
candle-nn = "0.6"
candle-transformers = "0.6"
tokenizers = "0.19"

# 增量索引
blake3 = "1"
notify = "6"
ignore = "0.4"

# 工具
tracing = "0.1"
tracing-subscriber = "0.3"
anyhow = "1"
clap = { version = "4", features = ["derive"] }
```

## 10. 数据存储

- 位置：项目根目录 `.code-graph/index.db`
- 自动追加 `.code-graph/` 到 `.gitignore`
- 项目删除时自然清理，无孤儿数据问题

## 11. 索引触发机制

混合模式：
- **默认**：按需索引 — 每次 MCP 工具调用时 Merkle diff 检查变更
- **可选**：`start_watch` / `stop_watch` 启停实时监听

## 12. 项目初始化流程

```
首次 MCP 工具调用
→ .code-graph/ 不存在
→ 创建目录 + 初始化 index.db (建表 + PRAGMA)
→ 全量索引 (分批, 带进度)
→ 追加 .code-graph/ 到 .gitignore
→ 执行查询
```
