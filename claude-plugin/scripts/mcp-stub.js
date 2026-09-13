'use strict';
/**
 * Minimal MCP server used by mcp-launcher.js when the plugin should NOT run the
 * real binary — either the project already registers its own code-graph server
 * (dedup) or the cwd is not a project (e.g. /tmp headless calls).
 *
 * Two modes:
 *   serveEmptyMcpStub()               permanent 0-tool stub (dedup / genuine /tmp)
 *   serveEmptyMcpStub({ upgrade })    0-tool stub that UPGRADES in place
 *
 * The upgrade path closes the "stub latch" gap: the non-project gate is
 * evaluated once at launcher start, so a directory that becomes a project
 * mid-session (bare dir → `git init` + scaffold) would otherwise stay toolless
 * until a full Claude Code restart. With { upgrade } the stub advertises
 * `tools.listChanged:true`, polls `shouldUpgrade()`, and when the cwd becomes a
 * project spawns the real binary, proxies JSON-RPC to it, and emits
 * `notifications/tools/list_changed` so the client re-fetches the (now real)
 * tool list — no restart. Genuinely non-project /tmp callers never satisfy
 * shouldUpgrade(), so they stay cheap (never spawn the binary).
 *
 * Known limitation: the child's ENTIRE `initialize` result is swallowed (the
 * client keeps the stub's) — not just the code-graph `instructions` block but
 * all negotiated server capabilities and the protocolVersion. For a tools-only
 * server that's fine; tools work immediately, but the instructions steering and
 * any non-tool capability only appear after a normal restart. Acceptable: tools
 * are the point, and MCP has no post-init way to re-deliver `instructions`.
 *
 * Deps (input / output / spawn timing / exit) are injectable so the proxy
 * handoff is unit-testable without spawning a real server.
 */

const DEFAULT_POLL_MS = 4000;
// Bound the retry loop: a persistently unresolvable/broken binary would
// otherwise re-spawn every pollMs for the whole session (~60s at the default).
const MAX_UPGRADE_FAILURES = 15;
// Distinct from any id Claude Code uses (it increments from 1) so the child's
// reply to our replayed initialize is unambiguous to swallow.
const SENTINEL_ID = 2147483646;

function serveEmptyMcpStub(opts = {}) {
  const input = opts.input || process.stdin;
  const output = opts.output || process.stdout;
  const upgrade = opts.upgrade || null;
  const setIv = opts.setInterval || setInterval;
  const clearIv = opts.clearInterval || clearInterval;
  const exit = opts.exit || ((code) => process.exit(code));
  const canUpgrade = !!upgrade;

  let savedInitialize = null;   // client's initialize request, replayed to the child
  let child = null;             // real binary, once upgraded
  let childReady = false;       // child finished its (replayed) handshake
  const queuedForChild = [];    // client lines seen after spawn, before child is ready
  let poller = null;
  let upgradeFailures = 0;      // consecutive failed upgrade attempts (see noteUpgradeFailure)
  let backoffTicks = 0;         // poll ticks to skip before the next probe (upgrade.backoff)
  let backoffNext = 1;

  function writeCc(obj) { output.write(JSON.stringify(obj) + '\n'); }

  function stubInitializeResult() {
    if (canUpgrade) {
      // Only tools.listChanged is load-bearing (it lets the client honor our
      // later notifications/tools/list_changed). Deliberately NOT advertising
      // resources/prompts here so a genuine /tmp caller that never upgrades
      // stays as cheap as the permanent stub (no extra resources/prompts probes).
      return {
        protocolVersion: '2024-11-05',
        capabilities: { tools: { listChanged: true } },
        serverInfo: { name: 'code-graph-mcp (plugin stub, upgrading)', version: '0.31.1' },
      };
    }
    return {
      protocolVersion: '2024-11-05',
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: 'code-graph-mcp (plugin stub, dedup)', version: '0.31.1' },
    };
  }

  function answerAsStub(req) {
    if (typeof req.id === 'undefined') return; // JSON-RPC notification → no response
    const m = req.method;
    let result, error;
    if (m === 'initialize') result = stubInitializeResult();
    else if (m === 'tools/list') result = { tools: [] };
    else if (m === 'resources/list') result = { resources: [] };
    else if (m === 'prompts/list') result = { prompts: [] };
    // The reason a caller is talking to a 0-tool stub differs per gate, and this
    // string is the ONLY place it reaches the model — the launcher's stderr goes
    // to the Claude Code log, not into the tool result. One shared wording named
    // the non-project cause for both gates, so the missing-binary gate told a
    // caller sitting in a perfectly good git repo to make it a project.
    else error = {
      code: -32601,
      message: canUpgrade
        ? `method not found (plugin MCP stub; ${upgrade.hint || 'upgrades when cwd becomes a project'})`
        : 'method not found (plugin MCP is in dedup stub mode)',
    };
    writeCc(error ? { jsonrpc: '2.0', id: req.id, error } : { jsonrpc: '2.0', id: req.id, result });
  }

  // ---- client → stub (or → child once proxying) ----
  let buf = '';
  input.setEncoding('utf8');
  input.on('data', (chunk) => {
    buf += chunk;
    let nl;
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      if (!line) continue;
      if (child) {                                   // proxy mode
        if (childReady) {
          try { child.stdin.write(line + '\n'); }
          catch { /* child stream gone; 'error'/'exit' handler cleans up */ }
        } else {
          queuedForChild.push(line);
        }
        continue;
      }
      let req;
      try { req = JSON.parse(line); } catch { continue; }
      if (!req || typeof req.method !== 'string') continue;
      if (req.method === 'initialize') savedInitialize = req;
      answerAsStub(req);
    }
  });
  input.on('end', () => {
    if (child) { try { child.stdin.end(); } catch { /* ok */ } }
    else exit(0);
  });

  // ---- upgrade: poll, then hand the live connection to the real binary ----
  function noteUpgradeFailure(reason) {
    // After the cap, stop polling and surface a one-time actionable hint instead
    // of re-spawning a doomed binary forever.
    if (++upgradeFailures < MAX_UPGRADE_FAILURES) return;
    if (poller) { clearIv(poller); poller = null; }
    process.stderr.write(`[code-graph] plugin MCP could not upgrade after ${upgradeFailures} attempts (${reason}); restart Claude Code once this project is set up. Staying in 0-tool stub.\n`);
  }

  function attemptUpgrade() {
    if (child || !upgrade) return;
    if (!upgrade.shouldUpgrade()) {
      // Not upgradable yet — not a failure. But when the probe itself is
      // expensive (missing-binary: full discovery walk incl. `npm root -g`,
      // up to 2s), a flat 4s cadence for a whole offline session is pure
      // subprocess churn. With { backoff:true } skip a doubling number of
      // ticks between probes, capped near 60s; a manual attemptUpgrade()
      // nudge (install chain's onInstalled) still probes immediately.
      if (upgrade.backoff) {
        const pollMs = upgrade.pollMs || DEFAULT_POLL_MS;
        backoffTicks = backoffNext;
        backoffNext = Math.min(backoffNext * 2, Math.max(1, Math.floor(60000 / pollMs) - 1));
      }
      return;
    }
    // `spawnReal` may THROW rather than return null. node hands exactly FIVE
    // errnos to the async 'error' event that `beginProxy` listens on — EACCES,
    // EAGAIN, EMFILE, ENFILE, ENOENT ("Run-time errors should emit an error, not
    // throw an exception", internal/child_process.js) — and throws every other
    // one synchronously out of child_process.spawn. ETXTBSY is outside that set,
    // and it is the one this path invites: the missing-binary gate nudges
    // attemptUpgrade() from the install chain's onInstalled, so it execs a ~40MB
    // binary at the instant the installer finished writing it, and Linux answers
    // ETXTBSY for as long as any writer fd is still open. Uncaught, the throw
    // escaped this function, escaped the poll-timer callback that called it, and
    // killed the whole MCP server — at the moment the install had SUCCEEDED
    // (QA 2026-09-13: 3/3 against a cold npm cache, 0/3 warm).
    let spawned;
    try {
      spawned = upgrade.spawnReal();
    } catch (err) {
      const code = (err && err.code) || 'unknown';
      process.stderr.write(`[code-graph] plugin MCP upgrade spawn failed (${code}); staying in 0-tool stub, will retry\n`);
      // Clear the offline backoff. It models "the binary is not there yet", and
      // `shouldUpgrade()` just returned TRUE — the binary IS there, this is a
      // transient exec failure. Without this the throw returns into whatever
      // `backoffTicks` a whole offline stretch ratcheted up (capped at 14 ticks
      // = 56 s), and `pollTick` burns that down one tick at a time before it
      // will even probe again: measured 4 s / 16 s / 36 s / 52 s of 0-tool stub
      // after a 10 s / 30 s / 45 s / 90 s install (pre-ship review of 7dc7eb4).
      // The retry is what makes containment a fix rather than a downgrade, so it
      // has to be the next tick.
      backoffTicks = 0;
      backoffNext = 1;
      noteUpgradeFailure(`spawn-threw-${code}`);
      return;
    }
    if (!spawned) { noteUpgradeFailure('binary-unresolved'); return; }
    if (poller) { clearIv(poller); poller = null; }
    child = spawned;
    beginProxy();
  }

  // Poll-timer tick: honors the backoff skip counter; the exported
  // attemptUpgrade stays direct so external nudges are never delayed.
  function pollTick() {
    if (backoffTicks > 0) { backoffTicks--; return; }
    attemptUpgrade();
  }

  function fallBackToStub(reason) {
    // Child spawned but died/errored before it was ready: answer anything the
    // client queued (so it doesn't hang on those ids), resume polling, and count
    // the failure toward the retry cap.
    process.stderr.write(`[code-graph] plugin MCP upgrade aborted (${reason}); staying in 0-tool stub, will retry\n`);
    const requeued = queuedForChild.splice(0);
    child = null;
    childReady = false;
    for (const line of requeued) {
      try { const req = JSON.parse(line); if (req && typeof req.method === 'string') answerAsStub(req); }
      catch { /* ignore */ }
    }
    if (upgrade && !poller) poller = setIv(pollTick, upgrade.pollMs || DEFAULT_POLL_MS);
    noteUpgradeFailure(reason);
  }

  function beginProxy() {
    const thisChild = child;   // guard against terminal events from a superseded spawn
    thisChild.stdin.on('error', () => { /* EPIPE if the child died — 'error'/'exit' handle it */ });
    thisChild.stdout.on('error', () => {});

    const params = (savedInitialize && savedInitialize.params) || {
      protocolVersion: '2024-11-05', capabilities: {},
      clientInfo: { name: 'code-graph-plugin-launcher', version: '0' },
    };
    // Replay initialize under a sentinel id; the child's reply is swallowed
    // (the client already received OUR initialize result).
    thisChild.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: SENTINEL_ID, method: 'initialize', params }) + '\n');

    let cbuf = '';
    thisChild.stdout.setEncoding('utf8');
    thisChild.stdout.on('data', (chunk) => {
      cbuf += chunk;
      let nl;
      while ((nl = cbuf.indexOf('\n')) >= 0) {
        const line = cbuf.slice(0, nl);
        cbuf = cbuf.slice(nl + 1);
        if (!line.trim()) continue;
        let msg = null;
        try { msg = JSON.parse(line); } catch { /* forward non-JSON verbatim */ }
        if (msg && msg.id === SENTINEL_ID) {
          // Child handshake complete: finish MCP init, flush queued client
          // requests, and tell the client its tool list changed.
          thisChild.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
          childReady = true;
          for (const l of queuedForChild.splice(0)) thisChild.stdin.write(l + '\n');
          writeCc({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' });
          continue;                                  // swallow the sentinel reply
        }
        output.write(line + '\n');                   // forward child → client verbatim
      }
    });
    thisChild.on('error', () => {
      if (child !== thisChild) return;               // superseded spawn — ignore
      if (childReady) { exit(1); return; }           // a ready child broke → die like the binary died (avoid a flapping stub)
      fallBackToStub('spawn-error');
    });
    thisChild.on('exit', (code, signal) => {
      if (child !== thisChild) return;
      if (!childReady) { fallBackToStub('early-exit'); return; }
      if (signal) process.kill(process.pid, signal);
      else exit(code == null ? 1 : code);
    });
  }

  if (upgrade) poller = setIv(pollTick, upgrade.pollMs || DEFAULT_POLL_MS);

  return { attemptUpgrade, _state: () => ({ hasChild: !!child, childReady }) };
}

module.exports = { serveEmptyMcpStub, SENTINEL_ID, DEFAULT_POLL_MS, MAX_UPGRADE_FAILURES };
