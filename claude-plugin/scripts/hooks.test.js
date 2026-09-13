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

// Relocatability of the PLUGIN-SIDE config — the two files Claude Code reads out
// of the plugin root and expands `${CLAUDE_PLUGIN_ROOT}` in. Note the opposite
// rule below at "hook commands use absolute paths (no env vars)": that one is
// about the entries `lifecycle.js` writes into `~/.claude/settings.json`, where
// nothing expands the variable. Same-looking strings, opposite requirements,
// which is why each needs its own guard.
//
// This is the property `claude --plugin-dir <path>` exercises: the same tree
// loaded from a source checkout instead of `~/.claude/plugins/cache/<m>/<p>/<v>/`.
// It is asserted here rather than end-to-end because `--plugin-dir` needs
// credentials, and running it against a real home would have this plugin's own
// SessionStart hook rewrite that machine's live settings.json (QA 2026-09-13).
// A version literal is the specific way this breaks: the cache path carries the
// version, so a hardcoded one survives exactly until the next release.
const PLUGIN_ROOT_DIR = path.resolve(__dirname, '..');
const RELOCATABLE_CONFIG = ['hooks/hooks.json', '.mcp.json'];

test('plugin-side config is relocatable: ${CLAUDE_PLUGIN_ROOT}, no absolute paths, no ../, no version literal', () => {
  const version = JSON.parse(
    fs.readFileSync(path.join(PLUGIN_ROOT_DIR, '.claude-plugin', 'plugin.json'), 'utf8')
  ).version;
  assert.match(version, /^\d+\.\d+\.\d+/, 'plugin.json must carry a version to check against');

  let checkedPaths = 0;
  for (const rel of RELOCATABLE_CONFIG) {
    const raw = fs.readFileSync(path.join(PLUGIN_ROOT_DIR, rel), 'utf8');
    assert.doesNotThrow(() => JSON.parse(raw), `${rel} must parse`);

    // Every path-shaped token that names a file inside the plugin.
    for (const m of raw.matchAll(/[^"\s]*\/scripts\/[A-Za-z0-9_.-]+\.js/g)) {
      const p = m[0];
      checkedPaths++;
      assert.ok(p.startsWith('${CLAUDE_PLUGIN_ROOT}/'),
        `${rel}: ${p} must be rooted at \${CLAUDE_PLUGIN_ROOT}`);
      assert.ok(!p.includes('..'),
        `${rel}: ${p} escapes the plugin root with ".."`);
      assert.ok(fs.existsSync(p.replace('${CLAUDE_PLUGIN_ROOT}', PLUGIN_ROOT_DIR)),
        `${rel}: ${p} does not exist on disk`);
    }

    assert.ok(!/"\/(?:home|Users|var|opt|usr)\//.test(raw),
      `${rel} contains an absolute path — it would not survive being installed anywhere else`);
    assert.ok(!raw.includes(version),
      `${rel} hardcodes the version ${version}; the plugin-cache path carries it, ` +
      'so this breaks on the next release');
  }
  // Anti-vacuity: both files DO name scripts, so a regex that stopped matching
  // would otherwise leave this test green while checking nothing.
  assert.ok(checkedPaths >= 2,
    `expected at least 2 script paths across ${RELOCATABLE_CONFIG.join(' + ')}, found ${checkedPaths}`);
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

// v0.145.1: Claude Code validates this file against a closed key set and warns
// at startup for anything outside it — the banner reads `code-graph-mcp:
// hooks.json: unknown key "_note" ignored` (the debug log prefixes `Plugin ` and
// appends the source), on every session, in red. We had carried a `_note` key
// here since v0.32.0 as a maintainer comment (JSON has none), and it became
// user-visible noise the moment CC 2.1.268 added the check. The allowlists below
// are read out of that binary's validator, which collects
// `Object.keys(manifest).filter(k => !ALLOWED.has(k))` plus the same pass over
// every event entry, and concatenates both into one message. The warning is
// emitted AFTER the schema parse has already succeeded and its return value is
// discarded at the call site, which is why the hooks themselves were never
// affected — `_note` cost us a red line, not a dark hook.
// Comments belong in this file (a .js), never in that one.
const ALLOWED_MANIFEST_KEYS = new Set(['description', 'hooks', 'modules', 'surface']);
const ALLOWED_ENTRY_KEYS = new Set(['matcher', 'hooks']);
// One level deeper, Claude Code does NOT warn: the hook object is parsed in
// strip mode, so a typo'd `timeOut` or `typ` is dropped in total silence —
// quieter than the `_note` case above, and it would leave the hook on a default
// budget or unable to run at all. CC gives us no signal there, so this file is
// the signal. `command` and `timeout` are additionally value-pinned by the
// HOOK_TIMEOUT_SECONDS test below; `type` is pinned only here.
const ALLOWED_HOOK_KEYS = new Set(['type', 'command', 'timeout']);
const REQUIRED_HOOK_KEYS = ['type', 'command', 'timeout'];

test('hooks.json: no key outside the schema Claude Code accepts', () => {
  const cfg = loadHooks();
  // Collected into ONE list and reported once, the way Claude Code reports it:
  // its validator concatenates the top-level and per-entry misses into a single
  // message. Two asserts would stop at the first, hiding the rest behind a
  // re-run (pre-ship review).
  const unknown = Object.keys(cfg)
    .filter((k) => !ALLOWED_MANIFEST_KEYS.has(k))
    .map((k) => `"${k}" (top level)`);

  for (const [event, entries] of Object.entries(cfg.hooks || {})) {
    if (!Array.isArray(entries)) continue;
    entries.forEach((e, i) => {
      for (const k of Object.keys(e || {})) {
        if (!ALLOWED_ENTRY_KEYS.has(k)) unknown.push(`"${k}" in hooks.${event}[${i}]`);
      }
      (e && e.hooks ? e.hooks : []).forEach((h, j) => {
        for (const k of Object.keys(h || {})) {
          if (!ALLOWED_HOOK_KEYS.has(k)) unknown.push(`"${k}" in hooks.${event}[${i}].hooks[${j}]`);
        }
      });
    });
  }

  assert.deepEqual(unknown, [],
    `hooks.json carries ${unknown.length} key(s) outside the schema: ${unknown.join(', ')}. ` +
    'Claude Code drops the top-level and per-entry ones while warning about them at every ' +
    'session start, and drops anything deeper WITHOUT warning. Put maintainer notes in the ' +
    '`description` string or in this test file, never in a new key.');

  // The other direction. An allowlist alone accepts a hook object that is
  // missing `command` entirely — which is exactly what a rename leaves behind.
  const incomplete = [];
  for (const [event, entries] of Object.entries(cfg.hooks || {})) {
    if (!Array.isArray(entries)) continue;
    entries.forEach((e, i) => (e && e.hooks ? e.hooks : []).forEach((h, j) => {
      for (const k of REQUIRED_HOOK_KEYS) {
        if (!(k in (h || {}))) incomplete.push(`hooks.${event}[${i}].hooks[${j}].${k}`);
      }
    }));
  }
  assert.deepEqual(incomplete, [],
    `these required hook fields are missing: ${incomplete.join(', ')} — Claude Code would ` +
    'silently run the hook wrong rather than tell you');
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
// Both budget guards below read the same set, so it is built once. A second
// hand-rolled copy of this walk is how the two halves would drift apart.
function registeredHookScripts() {
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
  return registered;
}

test('every registered hook script is a HOOK_TIMEOUT_SECONDS key and arms a deadline', () => {
  const { HOOK_TIMEOUT_SECONDS } = require('./hook-fail-open');
  const registered = registeredHookScripts();
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

// The other half of the deadline mechanism, and the queue item the JS-18 /
// JS-23 / JS-32 round left open (audit 2026-09-07). The guard above proves a
// registered hook ARMS a budget; nothing proved it SPENDS one. All three of
// those defects had the identical shape — a hook that armed a deadline and then
// handed its child a literal `timeout: 8000` the budget could not shrink — and
// each was found by reading, one at a time, because arming is what was checked.
//
// The discrimination the audit flagged as the expensive part: three registered
// hooks (`pre-grep-guide`, `pre-read-guide`, `post-grep-inject`) spawn nothing
// themselves and delegate to modules that spend the budget, so requiring a
// budget call in every registered file would fail on three correct ones. The
// trigger is therefore "does THIS file start a child", and only then is the
// literal forbidden.
//
// NOT a duplicate of `incremental-index.test.js`'s "no child of this hook
// carries a hard-coded timeout (JS-32)". That one scans one file, and was added
// with the fix for that file; this one generalises it over the registered set,
// which is the part that was missing — each of JS-18, JS-23 and JS-32 was found
// by hand, after the previous one, because nothing asked the question of every
// hook at once. Keep both: the per-file guard also pins the positive wiring
// (`timeout: budget`, `budget === null` skips), which a corpus-wide scan cannot
// assert without knowing each hook's own budget helper by name.
test('a registered hook that spawns a child must spend the budget, not a literal', () => {
  const registered = registeredHookScripts();

  // Line-prefix stripping only, like the sibling guard in
  // tmpdir-drift-guard.test.js. Two registered hooks carry `timeout: 0` inside
  // PROSE explaining that node reads it as no timeout at all; a scanner that
  // cannot tell code from commentary fires on both of them today. Trailing `//`
  // removal can also truncate a line at a `//` inside a string literal, which
  // can only hide an offender, never invent one.
  // Line numbers are carried from the ORIGINAL file, not from the filtered
  // array: dropping comment lines and then counting the survivors reports a
  // number that does not exist in the file, which is how you get a reader
  // doubting the guard instead of the offending line.
  const codeOf = (script) =>
    fs.readFileSync(path.join(__dirname, script), 'utf8')
      .split('\n')
      .map((l, i) => [i + 1, l])
      .filter(([, l]) => !/^\s*(?:\/\/|\*|\/\*)/.test(l))
      .map(([n, l]) => [n, l.replace(/\/\/.*$/, '')]);

  // The prefix class excludes `_` and alphanumerics but ALLOWS `.`, so a dotted
  // receiver still counts: the first version used `[^A-Za-z0-9_.]` and read
  // `cp.spawnSync(…)` / `require('child_process').execFileSync(…)` as
  // "delegates" — the single likeliest way to write a spawn in this codebase and
  // the single likeliest way past the guard. The async pair is listed too.
  const SPAWNS =
    /(?:^|[^A-Za-z0-9_])(?:spawnSync|execFileSync|execSync|execFile|exec|spawn)\s*\(/;
  // `"timeout": 8000` and `timeout : 8000` are the same defect with different
  // whitespace and quoting; all three spellings are one regex.
  const LITERAL_TIMEOUT = /["']?\btimeout["']?\s*:\s*\d/;

  let spawners = 0;
  for (const script of registered) {
    const lines = codeOf(script);
    if (!lines.some(([, l]) => SPAWNS.test(l))) continue; // delegates; the callee owns the budget
    spawners++;
    const offenders = lines
      .filter(([, l]) => LITERAL_TIMEOUT.test(l))
      .map(([n, l]) => `${script}:${n}: ${l.trim()}`);
    assert.deepEqual(
      offenders, [],
      `these lines hand a child a literal timeout the hook's own budget cannot shrink — ` +
      `spend the budget instead (remainingMs / a childBudgetMs-style helper), and return ` +
      `without running when it is gone:\n${offenders.join('\n')}`
    );
  }

  // Anti-vacuity floor, absolute rather than derived from the set it guards: if
  // every registered hook stopped spawning directly, the loop above would assert
  // nothing at all and stay green. Four is exactly today's count
  // (incremental-index, pre-edit-guide, session-init, user-prompt-context), so
  // this catches a total collapse of the detector, NOT four-of-eight going dark
  // — raise it alongside any hook that starts spawning.
  assert.ok(
    spawners >= 4,
    `expected at least 4 registered hooks to start a child directly; saw ${spawners}. ` +
    `Either the corpus shrank or the spawn detector stopped matching — both make this guard vacuous`
  );
});

// Known residue, recorded rather than implied. This scan cannot see
// `timeout: SOME_CONST` — the likeliest reaction to being told not to write a
// literal — because deciding whether a named constant is budget-derived needs
// the value, not the text. The per-hook tests are what cover that half
// (`incremental-index.test.js` pins `timeout: budget` and `budget === null`
// positively), and `codeOf` does not strip string CONTENTS, so a literal
// `timeout: 30…` inside a string would false-positive. Neither shape exists in
// the registered set today; both are cheap to diagnose because the failure
// message quotes the offending line.

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
