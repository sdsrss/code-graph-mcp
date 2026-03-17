#!/usr/bin/env node

/**
 * E2E Validation Script for code-graph-mcp
 *
 * Spawns the MCP server as a child process over stdio and exercises
 * every JSON-RPC method: initialize, tools/list, tools/call (9 visible + 5 hidden tools),
 * resources/list, resources/read, prompts/list, prompts/get.
 *
 * Usage:
 *   CODE_GRAPH_BIN=./target/release/code-graph-mcp node scripts/e2e-validate.js
 *
 * Exit code 0 = all tests passed, 1 = at least one failure.
 */

const { spawn } = require("child_process");
const readline = require("readline");
const path = require("path");

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const BIN = process.env.CODE_GRAPH_BIN || path.join(__dirname, "..", "target", "release", "code-graph-mcp");
const REQUEST_TIMEOUT_MS = 30_000;
const STARTUP_WAIT_MS = 10_000; // wait for background indexing after initialized

// ---------------------------------------------------------------------------
// JSON-RPC transport
// ---------------------------------------------------------------------------

let nextId = 1;
/** @type {Map<number, {resolve: Function, reject: Function, timer: NodeJS.Timeout}>} */
const pending = new Map();
/** @type {import("child_process").ChildProcess} */
let child;
/** @type {readline.Interface} */
let rl;

function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    const msg = JSON.stringify({ jsonrpc: "2.0", id, method, params });

    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`Timeout (${REQUEST_TIMEOUT_MS}ms) waiting for response to ${method} (id=${id})`));
    }, REQUEST_TIMEOUT_MS);

    pending.set(id, { resolve, reject, timer });
    child.stdin.write(msg + "\n");
  });
}

function notify(method, params = {}) {
  const msg = JSON.stringify({ jsonrpc: "2.0", method, params });
  child.stdin.write(msg + "\n");
}

function onLine(line) {
  const trimmed = line.trim();
  if (!trimmed) return;

  let obj;
  try {
    obj = JSON.parse(trimmed);
  } catch {
    // Not JSON — ignore (tracing noise that leaked, etc.)
    return;
  }

  // Notifications from server (no id) — ignore
  if (obj.id == null) return;

  const entry = pending.get(obj.id);
  if (!entry) return;

  clearTimeout(entry.timer);
  pending.delete(obj.id);
  entry.resolve(obj);
}

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

const results = []; // { name, ok, bytes, approxTokens, error? }

function logResult(name, ok, responseStr, error) {
  const bytes = responseStr ? Buffer.byteLength(responseStr, "utf8") : 0;
  const approxTokens = Math.ceil(bytes / 4);
  results.push({ name, ok, bytes, approxTokens, error });

  const status = ok ? "\x1b[32mOK\x1b[0m" : "\x1b[31mFAIL\x1b[0m";
  const detail = error ? ` — ${error}` : "";
  console.log(`  ${status}  ${name.padEnd(30)} ${String(bytes).padStart(6)} bytes  ~${String(approxTokens).padStart(5)} tokens${detail}`);
}

async function testRequest(name, method, params, validate) {
  let resp;
  let raw;
  try {
    resp = await send(method, params);
    raw = JSON.stringify(resp);
  } catch (e) {
    logResult(name, false, null, e.message);
    return null;
  }

  if (resp.error) {
    // Some tools are expected to return errors (e.g. missing routes).
    // Let the validate callback decide.
    if (validate) {
      try {
        validate(resp);
        logResult(name, true, raw);
        return resp;
      } catch (e) {
        logResult(name, false, raw, e.message);
        return resp;
      }
    }
    logResult(name, false, raw, `JSON-RPC error: ${resp.error.message}`);
    return resp;
  }

  if (validate) {
    try {
      validate(resp);
      logResult(name, true, raw);
    } catch (e) {
      logResult(name, false, raw, e.message);
    }
  } else {
    logResult(name, true, raw);
  }
  return resp;
}

async function testToolCall(name, toolName, args, validate) {
  return testRequest(name, "tools/call", { name: toolName, arguments: args }, validate);
}

function assertDefined(val, msg) {
  if (val === undefined || val === null) throw new Error(msg || "Expected defined value");
}

function assertNoError(resp) {
  if (resp.error) throw new Error(`Unexpected JSON-RPC error: ${resp.error.message}`);
}

function assertToolContent(resp) {
  assertNoError(resp);
  const content = resp.result?.content;
  if (!Array.isArray(content) || content.length === 0) {
    throw new Error("Expected non-empty content array");
  }
}

function assertToolText(resp) {
  assertToolContent(resp);
  const text = resp.result.content[0].text;
  if (typeof text !== "string" || text.length === 0) {
    throw new Error("Expected non-empty text in content[0]");
  }
  return text;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function main() {
  console.log(`\n=== code-graph-mcp E2E Validation ===\n`);
  console.log(`Binary: ${BIN}`);
  console.log(`CWD:    ${process.cwd()}\n`);

  // Spawn the server
  child = spawn(BIN, ["serve"], {
    cwd: process.cwd(),
    stdio: ["pipe", "pipe", "pipe"], // stdin, stdout, stderr
  });

  // Suppress stderr (tracing logs)
  child.stderr.on("data", () => {});

  child.on("error", (err) => {
    console.error(`Failed to spawn server: ${err.message}`);
    process.exit(1);
  });

  child.on("exit", (code, signal) => {
    // If child exits unexpectedly during tests, reject all pending
    for (const [, entry] of pending) {
      clearTimeout(entry.timer);
      entry.reject(new Error(`Server exited (code=${code}, signal=${signal})`));
    }
    pending.clear();
  });

  rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });
  rl.on("line", onLine);

  // -----------------------------------------------------------------------
  // 1. Initialize
  // -----------------------------------------------------------------------
  console.log("--- Protocol handshake ---");

  await testRequest("initialize", "initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "e2e-validate", version: "1.0.0" },
  }, (resp) => {
    assertNoError(resp);
    const sv = resp.result?.serverInfo?.version;
    assertDefined(sv, "Missing serverInfo.version");
    console.log(`    Server version: ${sv}`);
  });

  // 2. Send initialized notification (triggers startup indexing)
  notify("notifications/initialized");

  // 3. Wait for startup indexing
  console.log(`\n    Waiting ${STARTUP_WAIT_MS / 1000}s for startup indexing...`);
  await new Promise((r) => setTimeout(r, STARTUP_WAIT_MS));

  // -----------------------------------------------------------------------
  // 4. tools/list — verify 9 visible tools (5 hidden: start_watch, stop_watch, get_index_status, rebuild_index, find_http_route)
  // -----------------------------------------------------------------------
  console.log("\n--- tools/list ---");

  let toolNames = [];
  await testRequest("tools/list", "tools/list", {}, (resp) => {
    assertNoError(resp);
    const tools = resp.result?.tools;
    if (!Array.isArray(tools)) throw new Error("Expected tools array");
    if (tools.length !== 9) throw new Error(`Expected 9 tools, got ${tools.length}`);
    toolNames = tools.map((t) => t.name);
    console.log(`    Tools (${tools.length}): ${toolNames.join(", ")}`);
  });

  // -----------------------------------------------------------------------
  // 5. Tool calls
  // -----------------------------------------------------------------------
  console.log("\n--- tools/call (14 tools, 9 visible + 5 hidden) ---");

  // 5.1 semantic_code_search
  await testToolCall("semantic_code_search", "semantic_code_search",
    { query: "handle tool call" },
    (resp) => { assertToolText(resp); }
  );

  // 5.2 get_call_graph
  await testToolCall("get_call_graph", "get_call_graph",
    { symbol_name: "handle_call_tool", direction: "both", depth: 2 },
    (resp) => { assertToolText(resp); }
  );

  // 5.3 find_http_route (expect empty / no match — still a success response)
  await testToolCall("find_http_route", "find_http_route",
    { route_path: "/api/test" },
    (resp) => { assertToolContent(resp); }
  );

  // 5.4 trace_http_chain (expect empty / no match)
  await testToolCall("trace_http_chain", "trace_http_chain",
    { route_path: "/api/test", depth: 3 },
    (resp) => { assertToolContent(resp); }
  );

  // 5.5 get_ast_node
  let nodeId = null;
  await testToolCall("get_ast_node", "get_ast_node",
    { file_path: "src/mcp/server.rs", symbol_name: "McpServer" },
    (resp) => {
      const text = assertToolText(resp);
      // Try to extract a node_id from the JSON response
      try {
        const parsed = JSON.parse(text);
        nodeId = parsed.node_id || parsed.id || null;
        // Also check if it's in a nested structure
        if (nodeId == null && parsed.nodes && parsed.nodes.length > 0) {
          nodeId = parsed.nodes[0].node_id || parsed.nodes[0].id;
        }
      } catch {
        // text might not be JSON — that's OK for validation
      }
    }
  );

  // 5.6 read_snippet (use node_id from get_ast_node, or fallback to 1)
  await testToolCall("read_snippet", "read_snippet",
    { node_id: nodeId || 1, context_lines: 3 },
    (resp) => { assertToolContent(resp); }
  );

  // 5.7 impact_analysis
  await testToolCall("impact_analysis", "impact_analysis",
    { symbol_name: "handle_call_tool" },
    (resp) => { assertToolText(resp); }
  );

  // 5.8 module_overview
  await testToolCall("module_overview", "module_overview",
    { path: "src/mcp" },
    (resp) => { assertToolText(resp); }
  );

  // 5.9 dependency_graph
  await testToolCall("dependency_graph", "dependency_graph",
    { file_path: "src/mcp/server.rs" },
    (resp) => { assertToolText(resp); }
  );

  // 5.10 find_similar_code
  await testToolCall("find_similar_code", "find_similar_code",
    { symbol_name: "compress_if_needed" },
    (resp) => { assertToolContent(resp); }
  );

  // 5.11 start_watch
  await testToolCall("start_watch", "start_watch", {}, (resp) => {
    assertToolContent(resp);
  });

  // 5.12 stop_watch
  await testToolCall("stop_watch", "stop_watch", {}, (resp) => {
    assertToolContent(resp);
  });

  // 5.13 get_index_status
  await testToolCall("get_index_status", "get_index_status", {}, (resp) => {
    assertToolText(resp);
  });

  // 5.14 rebuild_index
  await testToolCall("rebuild_index", "rebuild_index",
    { confirm: true },
    (resp) => { assertToolContent(resp); }
  );

  // -----------------------------------------------------------------------
  // 6. Resources
  // -----------------------------------------------------------------------
  console.log("\n--- resources ---");

  await testRequest("resources/list", "resources/list", {}, (resp) => {
    assertNoError(resp);
    const resources = resp.result?.resources;
    if (!Array.isArray(resources) || resources.length === 0) {
      throw new Error("Expected non-empty resources array");
    }
    console.log(`    Resources: ${resources.map((r) => r.uri).join(", ")}`);
  });

  await testRequest("resources/read", "resources/read",
    { uri: "code-graph://project-summary" },
    (resp) => {
      assertNoError(resp);
      const contents = resp.result?.contents;
      if (!Array.isArray(contents) || contents.length === 0) {
        throw new Error("Expected non-empty contents array");
      }
    }
  );

  // -----------------------------------------------------------------------
  // 7. Prompts
  // -----------------------------------------------------------------------
  console.log("\n--- prompts ---");

  let promptNames = [];
  await testRequest("prompts/list", "prompts/list", {}, (resp) => {
    assertNoError(resp);
    const prompts = resp.result?.prompts;
    if (!Array.isArray(prompts) || prompts.length === 0) {
      throw new Error("Expected non-empty prompts array");
    }
    promptNames = prompts.map((p) => p.name);
    console.log(`    Prompts: ${promptNames.join(", ")}`);
  });

  await testRequest("prompts/get (impact-analysis)", "prompts/get",
    { name: "impact-analysis", arguments: { symbol_name: "handle_tool" } },
    (resp) => {
      assertNoError(resp);
      const messages = resp.result?.messages;
      if (!Array.isArray(messages) || messages.length === 0) {
        throw new Error("Expected non-empty messages array");
      }
    }
  );

  // -----------------------------------------------------------------------
  // Summary
  // -----------------------------------------------------------------------
  console.log("\n=== Summary ===\n");

  const passed = results.filter((r) => r.ok).length;
  const failed = results.filter((r) => !r.ok).length;
  const totalBytes = results.reduce((sum, r) => sum + r.bytes, 0);
  const totalTokens = results.reduce((sum, r) => sum + r.approxTokens, 0);

  console.log(`  Total:  ${results.length}`);
  console.log(`  Passed: \x1b[32m${passed}\x1b[0m`);
  console.log(`  Failed: ${failed > 0 ? `\x1b[31m${failed}\x1b[0m` : "0"}`);
  console.log(`  Bytes:  ${totalBytes.toLocaleString()}`);
  console.log(`  Tokens: ~${totalTokens.toLocaleString()}`);

  if (failed > 0) {
    console.log("\n  Failed tests:");
    for (const r of results.filter((r) => !r.ok)) {
      console.log(`    - ${r.name}: ${r.error || "unknown"}`);
    }
  }

  console.log("");

  // Cleanup
  child.stdin.end();
  rl.close();

  // Give the child a moment to exit gracefully
  await new Promise((r) => setTimeout(r, 500));
  if (child.exitCode === null) {
    child.kill("SIGTERM");
  }

  process.exit(failed > 0 ? 1 : 0);
}

main().catch((err) => {
  console.error(`\nFatal error: ${err.message}`);
  if (child && child.exitCode === null) {
    child.kill("SIGTERM");
  }
  process.exit(1);
});
