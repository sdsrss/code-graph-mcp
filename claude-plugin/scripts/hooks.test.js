'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

// Regression gate for v0.31.1: hooks.json matchers must be Claude Code's
// literal/regex form, NOT the expression DSL `tool == "X"`. The earlier
// matchers parsed as regex against tool names, never matched anything,
// and left every PreToolUse hook silently inert from v0.25.0 through
// v0.31.0. The bug was invisible to the existing unit tests because they
// spawn the hook scripts directly via stdin, bypassing Claude Code's
// matcher dispatch.

const HOOKS_JSON = path.resolve(__dirname, '..', 'hooks', 'hooks.json');

function loadHooks() {
  const raw = fs.readFileSync(HOOKS_JSON, 'utf8');
  return JSON.parse(raw);
}

function* iterMatchers(hooksByEvent) {
  for (const [event, entries] of Object.entries(hooksByEvent || {})) {
    if (!Array.isArray(entries)) continue;
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i];
      yield { event, idx: i, matcher: e && e.matcher };
    }
  }
}

test('hooks.json: file parses as JSON', () => {
  assert.doesNotThrow(loadHooks);
});

test('hooks.json: every entry has a string matcher', () => {
  const cfg = loadHooks();
  let count = 0;
  for (const { event, idx, matcher } of iterMatchers(cfg.hooks)) {
    assert.equal(typeof matcher, 'string',
      `hooks.${event}[${idx}].matcher should be a string, got ${typeof matcher}`);
    count++;
  }
  assert.ok(count > 0, 'expected at least one matcher in hooks.json');
});

// The actual regression gate. Each banned token reflects a specific
// failure mode we hit and want to keep out forever.
const BANNED_TOKENS = [
  // The original v0.25.0 → v0.31.0 bug: expression-style matcher treated
  // as regex against tool name → never matched.
  { token: '==', why: 'expression DSL (e.g. `tool == "Edit"`) is not supported; use literal tool name' },
  // `tool ==` or `tool name == "X"` — same family, different spelling.
  { token: 'tool ', why: 'expression DSL with `tool` variable is not supported' },
  // Boolean ORs as expression operators (regex uses `|`, not `||`).
  { token: '||', why: 'use `|` for pipe-list (e.g. `Write|Edit`), not `||`' },
  // Boolean AND has no meaning in tool-name matching.
  { token: '&&', why: '`&&` has no meaning in matchers' },
  // Double-quotes inside the matcher are a strong hint of expression DSL
  // (the broken syntax was `"tool == \"Edit\""`).
  { token: '"', why: 'literal double-quote in matcher is almost always a copy-paste of expression DSL' },
];

test('hooks.json: matchers avoid banned expression-DSL tokens', () => {
  const cfg = loadHooks();
  const offenders = [];
  for (const { event, idx, matcher } of iterMatchers(cfg.hooks)) {
    for (const { token, why } of BANNED_TOKENS) {
      if (matcher.includes(token)) {
        offenders.push(`hooks.${event}[${idx}].matcher = ${JSON.stringify(matcher)} — contains banned ${JSON.stringify(token)} (${why})`);
      }
    }
  }
  assert.deepEqual(offenders, [],
    'hooks.json matcher syntax regression — see v0.31.1 CHANGELOG:\n  ' + offenders.join('\n  '));
});

// v0.32.0 architecture: plugin-cache hooks.json ONLY carries SessionStart.
// PreToolUse / PostToolUse / UserPromptSubmit are registered into
// ~/.claude/settings.json by lifecycle.js (current Claude Code silently
// ignores plugin-cache hooks.json entries for those events — confirmed
// 2026-05-24 via session jsonl, see feedback_pretooluse_dark_under_green_health.md).
test('hooks.json: contains SessionStart only (v0.32.0)', () => {
  const cfg = loadHooks();
  assert.deepEqual(Object.keys(cfg.hooks || {}), ['SessionStart'],
    'plugin-cache hooks.json must contain only SessionStart; other events go via settings.json. ' +
    'Adding entries here for PreToolUse/PostToolUse/UserPromptSubmit would be dead config — CC does not load them.');
});

test('hooks.json: SessionStart wires session-init.js', () => {
  const cfg = loadHooks();
  const entries = (cfg.hooks && cfg.hooks.SessionStart) || [];
  assert.ok(entries.length > 0, 'SessionStart entry missing');
  const cmd = entries[0].hooks && entries[0].hooks[0] && entries[0].hooks[0].command;
  assert.match(cmd || '', /session-init\.js/);
});

// JS-04 (audit 2026-08-29). The matcher shipped as `startup|clear|compact` and
// silently excluded `resume` — every resumed session ran with NO statusLine
// self-heal, no forced update check, no index-freshness probe and no recent-impact
// injection, even though session-init.js handles that source explicitly.
//
// The expected set is READ OUT OF session-init.js rather than duplicated here:
// a test that carries its own copy of the production list goes green while the
// two drift apart (the same shape as the pre-edit-guide regex copy). Parse
// failure is a hard failure, not a silent skip — a vacuous guard is worse than
// no guard.
function documentedSessionStartSources() {
  const src = fs.readFileSync(path.resolve(__dirname, 'session-init.js'), 'utf8');
  const line = src.split('\n').find((l) => l.includes('SessionStart passes {source:'));
  assert.ok(line,
    'could not find the `SessionStart passes {source:...}` comment in session-init.js — ' +
    'this guard derives its expectation from it; re-point the guard rather than deleting it');
  const sources = [...line.matchAll(/"([a-z]+)"/g)].map((m) => m[1]);
  // Pinned at the CURRENT cardinality, not at some low floor. Pre-tag review
  // caught the first version at `>= 2`, which would accept a comment truncated
  // to {source:"startup"|"clear"} and then pass while the matcher was missing
  // `resume` — the exact bug this guard exists for. If Claude Code adds a fifth
  // source, this fails loudly and both the comment and the matcher get updated.
  assert.ok(sources.length >= 4,
    `parsed only ${sources.length} source(s) from ${JSON.stringify(line)} — guard would be vacuous`);
  return sources;
}

test('hooks.json: SessionStart matcher covers every source session-init.js handles', () => {
  const cfg = loadHooks();
  const matcher = cfg.hooks.SessionStart[0].matcher;
  const alternatives = matcher.split('|');
  const documented = documentedSessionStartSources();

  const missing = documented.filter((s) => !alternatives.includes(s));
  assert.deepEqual(missing, [],
    `SessionStart matcher ${JSON.stringify(matcher)} does not fire for ${missing.join(', ')} — ` +
    'session-init.js handles those sources, so the hook is dark on exactly those sessions (JS-04)');

  // Both directions (pre-tag review). An alternative with no documented source
  // behind it is either a typo that will never match, or a real fifth source
  // nobody wrote down — both worth failing on, and neither visible one-way.
  const undocumented = alternatives.filter((s) => !documented.includes(s));
  assert.deepEqual(undocumented, [],
    `SessionStart matcher ${JSON.stringify(matcher)} lists ${undocumented.join(', ')}, which ` +
    "session-init.js's stdin contract does not mention — a typo matches nothing silently");
});

// Cross-validate that lifecycle.js's buildSettingsHookEntries covers the
// matchers we removed from hooks.json — keeps the migration whole. If a
// future refactor accidentally drops a matcher in one place, this fails.
test('lifecycle.buildSettingsHookEntries covers PreToolUse Edit/Bash/Read', () => {
  const { buildSettingsHookEntries } = require('./lifecycle');
  const desired = buildSettingsHookEntries();
  const ptu = (desired.PreToolUse || []).map(e => e.matcher);
  for (const tool of ['Edit', 'Bash', 'Read']) {
    assert.ok(ptu.includes(tool), `lifecycle.js PreToolUse missing matcher: ${tool}; got ${JSON.stringify(ptu)}`);
  }
});

test('lifecycle.buildSettingsHookEntries covers PostToolUse Write|Edit + UserPromptSubmit', () => {
  const { buildSettingsHookEntries } = require('./lifecycle');
  const desired = buildSettingsHookEntries();
  const postMatchers = (desired.PostToolUse || []).map(e => e.matcher);
  assert.ok(postMatchers.some(m => m === 'Write|Edit'),
    `PostToolUse must have 'Write|Edit' matcher; got ${JSON.stringify(postMatchers)}`);
  const upsMatchers = (desired.UserPromptSubmit || []).map(e => e.matcher);
  assert.ok(upsMatchers.length > 0, 'UserPromptSubmit must have at least one matcher');
});

// ── JS-03 (audit 2026-09-05): one budget, known to both halves ─────────────
//
// The registered `timeout` is the number Claude Code kills the hook at, and the
// hook's own internal timeouts are what it spends against it. They were written
// in two places that never referenced each other, and the sums did not fit —
// pre-edit-guide could spend 12.5 s of a 4 s budget. Both now read
// HOOK_TIMEOUT_SECONDS, and these pin every registration site to it: a bump
// applied to only one of them fails here rather than in somebody's session.
test('registered PreToolUse/PostToolUse/UserPromptSubmit timeouts come from HOOK_TIMEOUT_SECONDS', () => {
  const { HOOK_TIMEOUT_SECONDS } = require('./hook-fail-open');
  const { buildSettingsHookEntries } = require('./lifecycle');
  const desired = buildSettingsHookEntries();
  let checked = 0;
  for (const [event, entries] of Object.entries(desired)) {
    for (const entry of entries) {
      for (const h of entry.hooks) {
        const script = (h.command.match(/([a-z-]+\.js)/) || [])[1];
        assert.ok(script, `${event}: no script name in command ${h.command}`);
        assert.equal(h.timeout, HOOK_TIMEOUT_SECONDS[script],
          `${event}/${script} registers timeout ${h.timeout}s but the table says ` +
          `${HOOK_TIMEOUT_SECONDS[script]}s — the hook would spend against the wrong number`);
        checked++;
      }
    }
  }
  assert.equal(checked, 6, `expected all six settings.json hooks; checked ${checked}`);
});

// The coupling the whole deadline mechanism rests on, and the one that can
// break in silence: `armHookDeadline` looks the budget up by
// `basename(process.argv[1])` and RETURNS QUIETLY on a table miss. So a renamed
// hook file, a launcher wrapper, or a symlink with a different basename leaves
// every child back on its own unclamped timeout with the whole suite green.
// Both halves — the name a hook is invoked as, and the key it looks itself up
// by — are asserted here against each other (pre-ship review 2026-09-05).
test('every registered hook script is a HOOK_TIMEOUT_SECONDS key and arms a deadline', () => {
  const { HOOK_TIMEOUT_SECONDS } = require('./hook-fail-open');
  const { buildSettingsHookEntries } = require('./lifecycle');
  const SCRIPT = /scripts[/\\]([A-Za-z0-9_-]+\.js)/;

  const registered = new Set();
  for (const entries of Object.values(buildSettingsHookEntries())) {
    for (const entry of entries) {
      for (const h of entry.hooks) {
        const m = SCRIPT.exec(h.command);
        assert.ok(m, `no script name in registered command: ${h.command}`);
        registered.add(m[1]);
      }
    }
  }
  // SessionStart comes from the plugin manifest, not from lifecycle.js.
  const manifest = fs.readFileSync(HOOKS_JSON, 'utf8');
  for (const m of manifest.matchAll(new RegExp(SCRIPT.source, 'g'))) registered.add(m[1]);
  assert.ok(registered.size >= 7, `only ${registered.size} hook scripts found: ${[...registered]}`);

  for (const script of registered) {
    assert.ok(
      HOOK_TIMEOUT_SECONDS[script],
      `${script} is registered as a hook but has no HOOK_TIMEOUT_SECONDS entry — ` +
      `armHookDeadline would no-op for it and every child would run unclamped`
    );
  }

  // No exemptions. `session-init.js` held the last one until audit 2026-09-05
  // NEW-05 wired it: it predated the helper, wrapped its own main in a
  // try/catch, and ran 21.5 s of serial children against a 5 s budget — the
  // largest overrun of the seven. An empty whitelist is the point; re-adding a
  // name here means re-accepting an unclamped hook.
  for (const script of registered) {
    const src = fs.readFileSync(path.join(__dirname, script), 'utf8');
    assert.match(
      src, /installHookFailOpen|armHookDeadline/,
      `${script} is registered with a ${HOOK_TIMEOUT_SECONDS[script]}s budget but never arms ` +
      `a deadline, so its children cannot be clamped to it`
    );
  }
});

test('hooks.json SessionStart timeout matches HOOK_TIMEOUT_SECONDS', () => {
  // SessionStart is the one event Claude Code loads from plugin-cache
  // hooks.json, so its budget cannot be written by lifecycle.js — but the table
  // is still the place the number is decided, and this is what keeps the two
  // files from drifting apart the way the hooks' internal timeouts had.
  const { HOOK_TIMEOUT_SECONDS } = require('./hook-fail-open');
  const cfg = loadHooks();
  const entry = cfg.hooks.SessionStart[0].hooks[0];
  assert.match(entry.command, /session-init\.js/);
  assert.equal(entry.timeout, HOOK_TIMEOUT_SECONDS['session-init.js'],
    'hooks.json and the budget table disagree about how long session-init.js gets');
});

test('lifecycle.buildSettingsHookEntries: every entry carries description marker', () => {
  // Description marker is the primary cleanup discriminator (immune to
  // path/env pollution per feedback_plugin_env_isolation.md). If an entry
  // lacks a description, isOurHookEntry falls back to path-fragment match
  // which is less reliable. Force every entry to have one.
  const { buildSettingsHookEntries } = require('./lifecycle');
  const desired = buildSettingsHookEntries();
  for (const [event, entries] of Object.entries(desired)) {
    for (let i = 0; i < entries.length; i++) {
      assert.ok(entries[i].description && entries[i].description.includes('[code-graph-mcp'),
        `${event}[${i}] missing or malformed description marker`);
    }
  }
});

test('lifecycle.buildSettingsHookEntries: hook commands use absolute paths (no env vars)', () => {
  // settings.json hook commands run with env pollution risk
  // (feedback_plugin_env_isolation.md). Paths MUST be absolute, derived
  // from __dirname, never from ${CLAUDE_PLUGIN_ROOT}.
  const { buildSettingsHookEntries } = require('./lifecycle');
  const desired = buildSettingsHookEntries();
  for (const entries of Object.values(desired)) {
    for (const e of entries) {
      for (const h of e.hooks) {
        assert.ok(!h.command.includes('${CLAUDE_PLUGIN_ROOT}'),
          `command must not use \${CLAUDE_PLUGIN_ROOT}: ${h.command}`);
        // POSIX commands are existence-guarded (`if [ -f "…" ]; then node "…"; fi`),
        // so assert on the extracted node-invocation path, not a string prefix.
        const m = h.command.match(/node "([^"]+)"/);
        assert.ok(m && (m[1].startsWith('/') || /^[A-Z]:\\/.test(m[1])),
          `command path must be absolute: ${h.command}`);
      }
    }
  }
});

// v0.67.0 hook-reliability Layer 1 (static firing invariants):
// The tests above inspect matcher STRINGS but never the target script file. A
// renamed/typo'd/moved hook script makes Claude Code unable to run it → the hook
// is SILENTLY inert (the "dark hook" class — feedback_pretooluse_dark_under_green_health.md).
// This collects every script CC will actually load — both registration channels —
// and asserts each exists and parses. Cheapest possible guard against silent dark.
const PLUGIN_ROOT = path.resolve(__dirname, '..'); // claude-plugin/

function resolveHookScript(cmd) {
  // command form: node "<path>"  (<path> may contain ${CLAUDE_PLUGIN_ROOT})
  const m = (cmd || '').match(/"([^"]+\.js)"/);
  return m ? m[1].replace('${CLAUDE_PLUGIN_ROOT}', PLUGIN_ROOT) : null;
}

function allRegisteredHookCommands() {
  const commands = [];
  // (1) settings.json side — lifecycle.buildSettingsHookEntries (PreToolUse/PostToolUse/UserPromptSubmit)
  const { buildSettingsHookEntries } = require('./lifecycle');
  for (const entries of Object.values(buildSettingsHookEntries())) {
    for (const e of entries) for (const h of e.hooks || []) commands.push(h.command);
  }
  // (2) plugin-cache hooks.json side — SessionStart (the only event CC loads from here)
  for (const entries of Object.values(loadHooks().hooks || {})) {
    if (!Array.isArray(entries)) continue;
    for (const e of entries) for (const h of e.hooks || []) commands.push(h.command);
  }
  return commands;
}

test('every registered hook script exists on disk', () => {
  const commands = allRegisteredHookCommands();
  // 3 PreToolUse + 2 PostToolUse (incremental-index + compound-grep inject) + 1 UserPromptSubmit + 1 SessionStart = 7
  assert.ok(commands.length >= 7, `expected >=7 registered hook commands, got ${commands.length}`);
  for (const cmd of commands) {
    const p = resolveHookScript(cmd);
    assert.ok(p, `could not extract a .js path from hook command: ${JSON.stringify(cmd)}`);
    assert.ok(fs.existsSync(p),
      `hook script missing on disk: ${p}\n  (from command ${JSON.stringify(cmd)})\n` +
      `  A renamed/typo'd/moved script makes the hook silently inert — Claude Code cannot run a missing file.`);
  }
});

test('every registered hook script parses (node --check)', () => {
  for (const cmd of allRegisteredHookCommands()) {
    const p = resolveHookScript(cmd);
    assert.doesNotThrow(
      () => execFileSync(process.execPath, ['--check', p], { stdio: 'pipe' }),
      `hook script has a syntax error (node --check failed): ${p}`);
  }
});

// Pin the EXACT matcher surface, not just "covers". The earlier tests assert the
// set INCLUDES Edit/Bash/Read etc.; this asserts it EQUALS the intended set, so
// adding/dropping a matcher must update this test — a deliberate decision, never a
// silent coverage drift. A PreToolUse hook fires only on the literal tool name.
// Deliberate exclusions (verified 2026-06-23; revisit if either premise changes):
//   - MultiEdit: NOT a tool in current Claude Code (absent from the tool surface;
//     the plugin targets recent CC per the v0.32.0 settings.json architecture), so
//     a matcher for it would be dead config. Re-add only if CC (re)introduces it.
//   - NotebookEdit: a real tool, but code-graph does NOT parse .ipynb (no jupyter
//     support in the parser / supported-language set), so both pre-edit-guide
//     (needs graph symbols) and incremental-index (needs to re-index the file)
//     would no-op on a notebook. Prerequisite is .ipynb PARSING support (a parser
//     feature); add the matcher as PART of that work, never before it.
test('buildSettingsHookEntries: matcher surface is exactly the intended set', () => {
  const { buildSettingsHookEntries } = require('./lifecycle');
  const desired = buildSettingsHookEntries();
  const setOf = (event) => (desired[event] || []).map(e => e.matcher).sort();
  assert.deepEqual(setOf('PreToolUse'), ['Bash', 'Edit', 'Read'],
    'PreToolUse matcher set changed — update this gate intentionally (does the new tool need a guide hook?)');
  assert.deepEqual(setOf('PostToolUse'), ['Bash', 'Write|Edit'],
    'PostToolUse matcher set changed — incremental-index (Write|Edit) + compound-grep inject (Bash) trigger surface must be deliberate');
  assert.deepEqual(setOf('UserPromptSubmit'), [''],
    'UserPromptSubmit matcher set changed unexpectedly');
  assert.deepEqual(Object.keys(desired).sort(), ['PostToolUse', 'PreToolUse', 'UserPromptSubmit'],
    'a new top-level hook event is registered into settings.json — confirm it is intended (SessionStart belongs in hooks.json)');
});

test('settings hook commands are existence-guarded on POSIX (dead path silent-0, exit codes preserved)', (t) => {
  if (process.platform === 'win32') { t.skip('POSIX-only guard form'); return; }
  const fs2 = require('fs');
  const os2 = require('os');
  const path2 = require('path');
  const { spawnSync } = require('child_process');
  const { buildSettingsHookEntries } = require('./lifecycle');

  const cmd = buildSettingsHookEntries().PreToolUse[0].hooks[0].command;
  assert.match(cmd, /^if \[ -f "/, 'POSIX hook command carries the existence guard');

  // Post-uninstall window: plugin-cache dir deleted before teardown strips the
  // hooks — the guard must turn "error on every tool call" into a silent 0.
  const dead = 'if [ -f "/nonexistent/cg-hook.js" ]; then node "/nonexistent/cg-hook.js"; fi';
  const r1 = spawnSync('sh', ['-c', dead], { encoding: 'utf8' });
  assert.equal(r1.status, 0, 'missing script exits 0');
  assert.equal((r1.stderr || '').trim(), '', 'missing script is silent');

  // Live script: node's own exit code must pass through — PreToolUse deny
  // semantics (exit 2) would be destroyed by an `|| exit 0` style guard.
  const dir = fs2.mkdtempSync(path2.join(os2.tmpdir(), 'cg-hookguard-'));
  t.after(() => fs2.rmSync(dir, { recursive: true, force: true }));
  const script = path2.join(dir, 'deny.js');
  fs2.writeFileSync(script, 'process.exit(2);');
  const guarded = `if [ -f "${script}" ]; then node "${script}"; fi`;
  const r2 = spawnSync('sh', ['-c', guarded], { encoding: 'utf8' });
  assert.equal(r2.status, 2, 'live script exit code passes through the guard');
});
