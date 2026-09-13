'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { PassThrough } = require('stream');
const { EventEmitter } = require('events');
const { serveEmptyMcpStub, SENTINEL_ID, MAX_UPGRADE_FAILURES } = require('./mcp-stub');

const tick = () => new Promise((r) => setImmediate(r));

// A fake child_process: collect what the launcher writes to child.stdin, and
// let the test push child.stdout lines + emit exit/error.
function makeFakeChild() {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  const ee = new EventEmitter();
  const linesToChild = [];
  let sbuf = '';
  stdin.setEncoding('utf8');
  stdin.on('data', (c) => {
    sbuf += c;
    let nl;
    while ((nl = sbuf.indexOf('\n')) >= 0) { linesToChild.push(sbuf.slice(0, nl)); sbuf = sbuf.slice(nl + 1); }
  });
  return {
    stdin, stdout,
    on: (e, cb) => ee.on(e, cb),
    emit: (e, ...a) => ee.emit(e, ...a),
    linesToChild,
  };
}

function makeRig(upgrade) {
  const input = new PassThrough();
  const out = [];
  const output = { write: (s) => { out.push(s); return true; } };
  let exitCode = null;
  const handle = serveEmptyMcpStub({
    input, output,
    setInterval: () => 0, clearInterval: () => {},   // no real timer; drive attemptUpgrade() manually
    exit: (c) => { exitCode = c; },
    upgrade,
  });
  const send = (obj) => input.write(JSON.stringify(obj) + '\n');
  return { input, out, handle, send, getExit: () => exitCode };
}

const INIT = { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'cc', version: '1' } } };

test('permanent stub: 0 tools, listChanged:false, unknown method → -32601', async () => {
  const { out, send } = makeRig(null);
  send(INIT);
  send({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  send({ jsonrpc: '2.0', id: 3, method: 'nope/nope', params: {} });
  await tick();
  const initResp = JSON.parse(out[0]);
  assert.equal(initResp.result.capabilities.tools.listChanged, false);
  assert.match(initResp.result.serverInfo.name, /dedup/);
  assert.deepEqual(JSON.parse(out[1]).result.tools, []);
  assert.equal(JSON.parse(out[2]).error.code, -32601);
});

test('upgradeable stub advertises tools.listChanged:true', async () => {
  const { out, send } = makeRig({ shouldUpgrade: () => false, spawnReal: () => null });
  send(INIT);
  await tick();
  assert.equal(JSON.parse(out[0]).result.capabilities.tools.listChanged, true);
});

test('upgradeable stub does NOT spawn while shouldUpgrade() is false', async () => {
  let spawnCalls = 0;
  const { handle } = makeRig({ shouldUpgrade: () => false, spawnReal: () => { spawnCalls++; return null; } });
  handle.attemptUpgrade();
  await tick();
  assert.equal(spawnCalls, 0);
  assert.equal(handle._state().hasChild, false);
});

test('upgrade handoff: proxies to real binary + emits tools/list_changed + forwards real tools', async () => {
  const child = makeFakeChild();
  let allow = false;
  const { out, send, handle } = makeRig({ shouldUpgrade: () => allow, spawnReal: () => child });

  // 1. client initialize + tools/list while still a non-project stub
  send(INIT);
  send({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  await tick();
  assert.equal(JSON.parse(out[0]).result.capabilities.tools.listChanged, true);
  assert.deepEqual(JSON.parse(out[1]).result.tools, []);          // stub → empty
  const outLenBeforeUpgrade = out.length;

  // 2. cwd becomes a project → upgrade fires
  allow = true;
  handle.attemptUpgrade();
  await tick();
  assert.equal(handle._state().hasChild, true);
  // launcher replayed initialize to the child under the sentinel id
  const replay = JSON.parse(child.linesToChild[0]);
  assert.equal(replay.method, 'initialize');
  assert.equal(replay.id, SENTINEL_ID);

  // 3. child answers the sentinel initialize
  child.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: SENTINEL_ID, result: { capabilities: { tools: { listChanged: false } }, instructions: 'real server' } }) + '\n');
  await tick();
  // launcher must: send initialized to child, and tools/list_changed to client
  assert.ok(child.linesToChild.some((l) => JSON.parse(l).method === 'notifications/initialized'));
  const newClientMsgs = out.slice(outLenBeforeUpgrade).map((s) => JSON.parse(s));
  assert.ok(newClientMsgs.some((m) => m.method === 'notifications/tools/list_changed'),
    'client must be told tools changed');
  // the sentinel init reply must NOT leak to the client
  assert.ok(!newClientMsgs.some((m) => m.id === SENTINEL_ID), 'sentinel reply must be swallowed');

  // 4. client re-requests tools/list → forwarded to child (not answered empty)
  const childLinesBefore = child.linesToChild.length;
  send({ jsonrpc: '2.0', id: 3, method: 'tools/list', params: {} });
  await tick();
  const forwarded = child.linesToChild.slice(childLinesBefore).map((l) => JSON.parse(l));
  assert.ok(forwarded.some((m) => m.id === 3 && m.method === 'tools/list'), 'tools/list forwarded to real binary');

  // 5. child returns real tools → forwarded verbatim to client
  child.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: 3, result: { tools: [{ name: 'get_call_graph' }, { name: 'semantic_code_search' }] } }) + '\n');
  await tick();
  const toolResp = out.map((s) => JSON.parse(s)).find((m) => m.id === 3);
  assert.ok(toolResp, 'client received a tools/list response');
  assert.deepEqual(toolResp.result.tools.map((t) => t.name), ['get_call_graph', 'semantic_code_search']);
});

test('upgrade with unresolvable binary stays a stub and keeps answering', async () => {
  let allow = true;
  const { out, send, handle } = makeRig({ shouldUpgrade: () => allow, spawnReal: () => null });
  send(INIT);
  await tick();
  handle.attemptUpgrade();               // spawnReal returns null → no child
  await tick();
  assert.equal(handle._state().hasChild, false);
  send({ jsonrpc: '2.0', id: 9, method: 'tools/list', params: {} });
  await tick();
  const resp = out.map((s) => JSON.parse(s)).find((m) => m.id === 9);
  assert.deepEqual(resp.result.tools, []);   // still served by the stub
});

// `spawnReal` returning null was covered; `spawnReal` THROWING was not, and
// node reserves the async 'error' event for ENOENT/EAGAIN/EMFILE/ENFILE only —
// every other errno is thrown synchronously out of child_process.spawn. ETXTBSY
// is the one that matters: the missing-binary gate nudges attemptUpgrade() from
// the install chain's onInstalled, i.e. it execs a 40MB binary at the instant
// the installer finished writing it, and Linux answers ETXTBSY while any writer
// fd is still open. Uncaught, that throw left the poll-timer callback and killed
// the whole MCP server — at the moment the install SUCCEEDED. Reproduced 3/3
// against a cold npm cache (QA 2026-09-13).
test('upgrade: a spawnReal that THROWS (ETXTBSY) is contained — stub survives and retries', async () => {
  const boom = Object.assign(new Error('spawn ETXTBSY'), { code: 'ETXTBSY', errno: -26, syscall: 'spawn' });
  let throwNext = true;
  const child = makeFakeChild();
  const { out, send, handle } = makeRig({
    shouldUpgrade: () => true,
    spawnReal: () => { if (throwNext) throw boom; return child; },
  });
  send(INIT);
  await tick();

  // The defect: this call propagated the throw to the caller (the poll timer).
  assert.doesNotThrow(() => handle.attemptUpgrade(), 'spawnReal throwing must not escape attemptUpgrade');
  await tick();
  assert.equal(handle._state().hasChild, false, 'no child adopted after a failed spawn');

  // The connection must still be alive and answering as a stub.
  send({ jsonrpc: '2.0', id: 9, method: 'tools/list', params: {} });
  await tick();
  assert.deepEqual(out.map((s) => JSON.parse(s)).find((m) => m.id === 9).result.tools, []);

  // ETXTBSY clears in milliseconds, so the retry must still be able to win.
  throwNext = false;
  handle.attemptUpgrade();
  await tick();
  assert.equal(handle._state().hasChild, true, 'a later attempt still upgrades');
});

// The -32601 text is the only diagnosis a MODEL ever sees — stderr goes to the
// Claude Code log, not into the tool result. Both upgradeable gates shared one
// string that named the non-project cause, so the missing-binary gate (the gate
// every new install passes through) answered a caller standing in a perfectly
// good git repo with "upgrades when cwd becomes a project".
test('upgrade: the -32601 reason reflects the gate that armed the stub', async () => {
  const { out, send } = makeRig({
    shouldUpgrade: () => false, spawnReal: () => null,
    hint: 'binary still installing',
  });
  send(INIT);
  send({ jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'project_map', arguments: {} } });
  await tick();
  const err = out.map((s) => JSON.parse(s)).find((m) => m.id === 4).error;
  assert.equal(err.code, -32601);
  assert.match(err.message, /binary still installing/);
  assert.doesNotMatch(err.message, /cwd becomes a project/);
});

test('upgrade: default reason is still the non-project wording', async () => {
  const { out, send } = makeRig({ shouldUpgrade: () => false, spawnReal: () => null });
  send(INIT);
  send({ jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'project_map', arguments: {} } });
  await tick();
  assert.match(out.map((s) => JSON.parse(s)).find((m) => m.id === 5).error.message, /cwd becomes a project/);
});

test('upgrade: repeated spawn throws hit the retry cap instead of looping forever', async () => {
  const boom = Object.assign(new Error('spawn EACCES'), { code: 'EACCES' });
  const { send, handle } = makeRig({
    shouldUpgrade: () => true,
    spawnReal: () => { throw boom; },
  });
  send(INIT);
  await tick();
  for (let i = 0; i < MAX_UPGRADE_FAILURES + 3; i++) {
    assert.doesNotThrow(() => handle.attemptUpgrade(), `attempt ${i} escaped`);
  }
  await tick();
  assert.equal(handle._state().hasChild, false);
});

test('upgrade: a request in the handoff window is queued then flushed to the child in order', async () => {
  const child = makeFakeChild();
  const { send, handle } = makeRig({ shouldUpgrade: () => true, spawnReal: () => child });
  send(INIT);
  await tick();
  handle.attemptUpgrade();                     // child spawned; sentinel initialize sent; NOT ready yet
  await tick();
  // client sends a call while the child is mid-handshake → must be queued, not forwarded
  send({ jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'x' } });
  await tick();
  assert.equal(child.linesToChild.length, 1);
  assert.equal(JSON.parse(child.linesToChild[0]).id, SENTINEL_ID);
  // child completes its handshake → queued request flushes AFTER initialized
  child.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: SENTINEL_ID, result: { capabilities: {} } }) + '\n');
  await tick();
  const methods = child.linesToChild.map((l) => { const m = JSON.parse(l); return m.method || `id:${m.id}`; });
  assert.deepEqual(methods, ['initialize', 'notifications/initialized', 'tools/call']);
});

test('upgrade: child that exits before ready falls back to stub and answers queued requests', async () => {
  const child = makeFakeChild();
  const { out, send, handle } = makeRig({ shouldUpgrade: () => true, spawnReal: () => child });
  send(INIT);
  await tick();
  handle.attemptUpgrade();                     // child spawned, not ready
  await tick();
  send({ jsonrpc: '2.0', id: 8, method: 'tools/list', params: {} });   // queued during handoff
  await tick();
  child.emit('exit', 1, null);                 // child dies BEFORE the sentinel reply
  await tick();
  assert.equal(handle._state().hasChild, false);                       // reverted to stub
  const resp = out.map((s) => JSON.parse(s)).find((m) => m.id === 8);
  assert.ok(resp, 'queued request got a stub answer (client not left hanging)');
  assert.deepEqual(resp.result.tools, []);
});

test('upgrade: error on a READY child exits instead of reverting to a flapping stub', async () => {
  const child = makeFakeChild();
  const { send, handle, getExit } = makeRig({ shouldUpgrade: () => true, spawnReal: () => child });
  send(INIT);
  await tick();
  handle.attemptUpgrade();
  await tick();
  child.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: SENTINEL_ID, result: { capabilities: {} } }) + '\n');
  await tick();
  assert.equal(handle._state().childReady, true);
  child.emit('error', new Error('boom'));      // a READY child errors
  await tick();
  assert.equal(handle._state().hasChild, true, 'must NOT revert to stub (that would flap)');
  assert.equal(getExit(), 1, 'ready-child error exits the launcher like the binary died');
});

test('poller is wired to attemptUpgrade at the default interval', () => {
  const input = new PassThrough();
  const out = [];
  let captured = null, spawnCalls = 0;
  serveEmptyMcpStub({
    input, output: { write: (s) => out.push(s) },
    setInterval: (fn, ms) => { captured = { fn, ms }; return 1; },
    clearInterval: () => {}, exit: () => {},
    upgrade: { shouldUpgrade: () => true, spawnReal: () => { spawnCalls++; return null; } },
  });
  assert.ok(captured, 'poller started via setInterval');
  assert.equal(captured.ms, 4000);             // DEFAULT_POLL_MS
  captured.fn();                                // simulate one poll tick
  assert.equal(spawnCalls, 1);                  // tick attempted the upgrade
});

test('backoff: missing-binary probes thin out toward the 60s cap; manual nudge stays direct', () => {
  const input = new PassThrough();
  let captured = null, probes = 0;
  const stub = serveEmptyMcpStub({
    input, output: { write: () => true },
    setInterval: (fn, ms) => { captured = { fn, ms }; return 1; },
    clearInterval: () => {}, exit: () => {},
    upgrade: { backoff: true, shouldUpgrade: () => { probes++; return false; }, spawnReal: () => null },
  });
  // 40 ticks at 4s each = 160s of session time. Without backoff that is 40
  // full discovery walks; with doubling skip (1,2,4,8,14-cap) it must be far
  // fewer while never stopping entirely.
  for (let i = 0; i < 40; i++) captured.fn();
  assert.ok(probes <= 8, `expected ≤8 probes over 40 ticks with backoff, got ${probes}`);
  assert.ok(probes >= 3, `backoff must keep probing, got ${probes}`);
  // External nudge (install chain onInstalled) bypasses the skip counter.
  const before = probes;
  stub.attemptUpgrade();
  assert.equal(probes, before + 1, 'manual attemptUpgrade probes immediately');
});

test('no backoff flag → every tick probes (non-project gate keeps 4s responsiveness)', () => {
  const input = new PassThrough();
  let captured = null, probes = 0;
  serveEmptyMcpStub({
    input, output: { write: () => true },
    setInterval: (fn, ms) => { captured = { fn, ms }; return 1; },
    clearInterval: () => {}, exit: () => {},
    upgrade: { shouldUpgrade: () => { probes++; return false; }, spawnReal: () => null },
  });
  for (let i = 0; i < 10; i++) captured.fn();
  assert.equal(probes, 10);
});
