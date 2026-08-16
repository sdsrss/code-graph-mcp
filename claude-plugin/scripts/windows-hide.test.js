'use strict';
// Drift guard for issue #40: on Windows every console-subsystem child spawned
// from a console-less parent (our MCP server, hooks and statusline are all
// launched hidden by Claude Code) gets a NEW visible console window unless the
// spawn passes `windowsHide: true` — which Node defaults to FALSE on every
// child_process API. The field report was 5–7 console windows flashing and
// stealing keyboard focus on every session start.
//
// One-off fixes rot: this file re-derives the call-site list from source on
// every run, so a NEW spawn added tomorrow fails here instead of shipping a
// flash. It is a static check by necessity — the behavior itself only
// reproduces on Windows, which CI does not run for the JS suite.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');

// Names that reach child_process. `spawnFn` is the injected-seam spelling used
// by launcher-install.js / session-init.js — its default IS spawn, so its
// options object needs the same treatment.
const CALL_NAMES = ['spawnSync', 'spawn', 'execFileSync', 'execFile', 'execSync', 'exec', 'spawnFn'];
const CALL_RE = new RegExp(`(?<![.\\w$])(${CALL_NAMES.join('|')})\\s*\\(`, 'g');

// MEMBER-expression spellings — `cp.spawn(...)`, `require('child_process').execSync(...)`.
// The direct-call regex above deliberately excludes anything after a `.` so that
// `RE.exec(raw)` is not a call site, and that exclusion silently covered the
// member form too: an independent review fed this scanner
// `require('child_process').execSync('npm root -g')` and got back "clean". Since
// `.exec(` is overwhelmingly a regex, the bare `exec` member form only counts
// when the receiver is a binding this file took from child_process.
const MEMBER_RE = /(?<![\w$])([\w$]+|\)|['"]\s*\))\s*\.\s*(spawnSync|spawn|execFileSync|execFile|execSync|exec)\s*\(/g;
const NEVER_REGEX_METHODS = new Set(['spawnSync', 'spawn', 'execFileSync', 'execFile', 'execSync']);

/**
 * Identifiers in `src` bound to the child_process module or to one of its
 * functions under another name — `const cp = require('child_process')`,
 * `const { execFileSync: run } = require('child_process')`.
 *
 * Takes RAW source, not masked: the module name it keys on lives inside a
 * string literal, and the masker blanks string CONTENTS — running this on the
 * masked text made every binding invisible (`require('               ')`), which
 * is why the alias form was still slipping through after the member-call fix.
 * @param {string} src - raw, unmasked source
 * @returns {{namespaces: Set<string>, aliases: Set<string>}}
 */
function childProcessBindings(src) {
  const namespaces = new Set();
  const aliases = new Set();
  const req = String.raw`require\(\s*['"](?:node:)?child_process['"]\s*\)`;
  for (const m of src.matchAll(new RegExp(String.raw`(?:const|let|var)\s+([\w$]+)\s*=\s*${req}`, 'g'))) {
    namespaces.add(m[1]);
  }
  for (const m of src.matchAll(new RegExp(String.raw`(?:const|let|var)\s*\{([^}]*)\}\s*=\s*${req}`, 'g'))) {
    for (const part of m[1].split(',')) {
      const [orig, alias] = part.split(':').map((s) => s.trim());
      if (alias && CALL_NAMES.includes(orig)) aliases.add(alias);
    }
  }
  // Injected-seam spelling: `function detectEmbedModel(binary, run = execFileSync)`.
  // The parameter's DEFAULT is the real child_process function, so every `run(...)`
  // in that body spawns — and the scanner saw only a local identifier it had no
  // reason to care about. doctor.js's detectEmbedModel was unguarded behind
  // exactly this shape while the sweep reported the directory clean. `spawnFn` is
  // in CALL_NAMES by hand for the same reason; this generalizes it so the next
  // seam does not need a hand-edit.
  // The `(?!\s*\()` matters: `const result = spawnSync('cargo', …)` assigns the
  // RESULT, not the function, and treating `result` as a spawn alias would make
  // the scanner flag unrelated calls that merely reuse the name.
  for (const m of src.matchAll(/([\w$]+)\s*=\s*(execFileSync|execFile|execSync|spawnSync|spawn|exec)\b(?!\s*\()/g)) {
    if (CALL_NAMES.includes(m[1])) continue;
    aliases.add(m[1]);
  }
  return { namespaces, aliases };
}

// Accepted spellings inside the argument list: the shared helper, the npm
// invocation builder (which folds windowsHide in), or an explicit literal.
const GUARDED_RE = /(?<![.\w$])(hidden|npmInvocation)\s*\(|windowsHide/;

/**
 * Replace the CONTENTS of strings, template literals, regex literals and
 * comments with spaces, preserving offsets and length. Lets the scanner treat
 * `'exec('` inside a string, or a call named in a comment, as what they are:
 * not call sites.
 *
 * Regex literals are NOT optional here. Skipping them looked harmless until
 * user-prompt-context.js's ``/`([a-zA-Z_]\w{2,})`/g`` — a regex containing
 * backticks — was read as the start of a template literal, which put every
 * quote after it out of phase and hid that file's one execFileSync from the
 * sweep. A guard that silently stops seeing part of the tree is worse than no
 * guard; `masker sees every file that spawns` below is the cross-check.
 * @param {string} src
 * @returns {string}
 */
function maskLiterals(src) {
  const out = src.split('');
  let i = 0;
  let prev = ''; // last significant (non-space) code char, for regex-vs-divide
  const blank = (from, to) => { for (let k = from; k < to && k < out.length; k++) if (out[k] !== '\n') out[k] = ' '; };
  while (i < src.length) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '/' && n === '/') {
      const end = src.indexOf('\n', i);
      blank(i, end === -1 ? src.length : end);
      i = end === -1 ? src.length : end;
    } else if (c === '/' && n === '*') {
      const end = src.indexOf('*/', i + 2);
      blank(i, end === -1 ? src.length : end + 2);
      i = end === -1 ? src.length : end + 2;
    } else if (c === '/' && (prev === '' || '(,=:[!&|?{};+-*%~^<>'.includes(prev))) {
      // Regex literal (the only `/` that can legally follow those); anything
      // else — an identifier, `)`, `]`, a digit — is division.
      let j = i + 1;
      let inClass = false;
      while (j < src.length && src[j] !== '\n') {
        if (src[j] === '\\') { j += 2; continue; }
        if (src[j] === '[') inClass = true;
        else if (src[j] === ']') inClass = false;
        else if (src[j] === '/' && !inClass) break;
        j++;
      }
      blank(i + 1, j);
      prev = '/';
      i = j + 1;
    } else if (c === '"' || c === "'" || c === '`') {
      let j = i + 1;
      while (j < src.length && src[j] !== c) {
        if (src[j] === '\\') j++;
        j++;
      }
      blank(i + 1, j);
      prev = c;
      i = j + 1;
    } else {
      if (!/\s/.test(c)) prev = c;
      i++;
    }
  }
  return out.join('');
}

/**
 * Text of the argument list starting at the `(` index, without the outer parens.
 * @param {string} masked
 * @param {number} openIdx
 * @returns {string|null} null when unbalanced (should not happen in valid JS)
 */
function argText(masked, openIdx) {
  let depth = 0;
  for (let i = openIdx; i < masked.length; i++) {
    const c = masked[i];
    if (c === '(' || c === '[' || c === '{') depth++;
    else if (c === ')' || c === ']' || c === '}') {
      depth--;
      if (depth === 0) return masked.slice(openIdx + 1, i);
    }
  }
  return null;
}

/**
 * Unguarded child_process call sites in one source text.
 * @param {string} src
 * @returns {Array<{name: string, line: number}>}
 */
function findUnguarded(src) {
  const masked = maskLiterals(src);
  const { namespaces, aliases } = childProcessBindings(src);
  const bad = [];
  const hits = [];

  const directRe = aliases.size
    ? new RegExp(`(?<![.\\w$])(${[...CALL_NAMES, ...aliases].join('|')})\\s*\\(`, 'g')
    : CALL_RE;
  directRe.lastIndex = 0;
  for (let m = directRe.exec(masked); m; m = directRe.exec(masked)) {
    hits.push({ name: m[1], index: m.index, nameLen: m[1].length });
  }
  MEMBER_RE.lastIndex = 0;
  for (let m = MEMBER_RE.exec(masked); m; m = MEMBER_RE.exec(masked)) {
    const [receiver, method] = [m[1], m[2]];
    if (!NEVER_REGEX_METHODS.has(method) && !namespaces.has(receiver) && !/\)$/.test(receiver)) continue;
    hits.push({ name: `${receiver}.${method}`, index: m.index, nameLen: m[0].length - 1 });
  }

  for (const hit of hits) {
    const open = masked.indexOf('(', hit.index + hit.nameLen);
    const args = argText(masked, open);
    if (args === null) continue;
    let ok = GUARDED_RE.test(args);
    if (!ok) {
      // Options passed as a variable (`gitOpts`, `npm.opts`): resolve the
      // binding in the same file and test its initializer instead. EVERY
      // declaration of that name must be guarded, not the first one found —
      // a shadowed inner `const o = { timeout: 1 }` used to hide behind an
      // outer `const o = hidden({})` (stricter reading wins).
      const last = args.split(',').pop().trim().replace(/\.opts$/, '');
      if (/^[A-Za-z_$][\w$]*$/.test(last)) {
        const decls = [...masked.matchAll(new RegExp(`(?:const|let|var)\\s+${last}\\s*=([^;]*)`, 'g'))];
        ok = decls.length > 0 && decls.every((d) => GUARDED_RE.test(d[1]));
      }
    }
    if (!ok) bad.push({ name: hit.name, line: src.slice(0, hit.index).split('\n').length });
  }
  return bad;
}

// ── Negative control: the checker must actually reject an unguarded call ──
// Without this, a scanner that silently matched nothing (bad regex, masking
// bug) would report a clean sweep over the whole plugin.
test('the guard rejects an unguarded spawn (negative control)', () => {
  assert.deepEqual(
    findUnguarded("execFileSync('curl', ['-sL', url], { timeout: 1000 });").map((b) => b.name),
    ['execFileSync']);
  assert.deepEqual(findUnguarded("const o = { timeout: 1 };\nspawnSync('npm', a, o);").map((b) => b.name),
    ['spawnSync']);
  // ...and must ACCEPT each guarded spelling.
  assert.deepEqual(findUnguarded("execSync(cmd, hidden({ cwd }));"), []);
  assert.deepEqual(findUnguarded("spawn(f, a, { windowsHide: true, cwd });"), []);
  assert.deepEqual(findUnguarded("const npm = npmInvocation(['root','-g']);\nspawn(npm.file, npm.args, npm.opts);"), []);
  assert.deepEqual(findUnguarded("const gitOpts = hidden({ cwd });\nexecSync('git log', gitOpts);"), []);
  // Not call sites: a regex .exec, a call named inside a string or comment.
  assert.deepEqual(findUnguarded("const m = RE.exec(raw);\n// spawn('x', a, {})\nconst s = \"execSync('y', {})\";"), []);
  // A regex literal containing a quote/backtick must not throw the masker out
  // of phase and blind it to everything after — this exact shape (a backtick
  // inside a regex in user-prompt-context.js) hid a real call site.
  assert.deepEqual(
    findUnguarded("const s = m.match(/`(\\w+)`/g);\nexecFileSync(bin, a, { timeout: 1 });").map((b) => b.name),
    ['execFileSync']);
  // ...and division must still be division, not an unterminated regex.
  assert.deepEqual(findUnguarded("const half = total / 2;\nspawn(f, a, hidden({}));"), []);
});

// ── Spellings an independent review got past the first version of this scanner ──
// Every one of these is a genuinely unguarded child_process call that the
// scanner reported as clean, which made the guard's promise ("a new spawn fails
// the build") weaker than it was stated to be. The member-expression form is
// the likeliest way someone actually adds one.
test('the guard catches indirect spawn spellings, not just the direct call', () => {
  const caught = (src) => findUnguarded(src).length > 0;
  assert.ok(caught("require('child_process').execSync('npm root -g');"),
    'member call straight off require()');
  assert.ok(caught("const cp = require('child_process');\ncp.spawn(f, a, { stdio: 'pipe' });"),
    'namespace binding');
  assert.ok(caught("const { execFileSync: run } = require('child_process');\nrun('curl', a, { timeout: 1 });"),
    'renamed destructure');
  assert.ok(caught("const o = hidden({});\nfunction g(){ const o = { timeout: 1 }; spawnSync(x, [], o); }"),
    'inner shadowed options binding must not hide behind an outer guarded one');
  assert.ok(caught("const cp = require('child_process');\ncp.exec('git status', { timeout: 1 });"),
    'bare .exec counts when the receiver is a child_process binding');
  assert.ok(caught("function probe(bin, run = execFileSync) { return run(bin, ['x'], { timeout: 1 }); }"),
    'injected seam whose parameter DEFAULT is the child_process function (doctor.js detectEmbedModel)');

  // Guarded form of that same seam stays clean...
  assert.deepEqual(findUnguarded("function probe(bin, run = execFileSync) { return run(bin, ['x'], hidden({})); }"), []);
  // ...and assigning a spawn RESULT must not turn the variable into a spawn
  // alias: `result(1)` below is an unrelated call and flagging it would be
  // noise, which is how a guard gets muted.
  assert.deepEqual(findUnguarded("const result = spawnSync('cargo', a, hidden({}));\nresult(1);"), []);

  // The reason `.exec(` is not simply added to the call names: it is
  // overwhelmingly a regex method, and flagging those would make the guard
  // noise that gets muted.
  assert.deepEqual(findUnguarded("const m = RE.exec(raw);\nconst n = /x/.exec(s);"), []);
  // Guarded spellings of the same indirect forms stay clean.
  assert.deepEqual(findUnguarded("const cp = require('child_process');\ncp.spawn(f, a, hidden({}));"), []);
});

// Every shipped JS directory, not just this one. `bin/cli.js` is the npm
// package's entry point — the single most-executed script we ship — and it
// spawned the MCP binary with no windowsHide at all while this guard reported
// the tree clean, because the guard only ever looked at its own directory.
// scripts/ (e2e-validate.js, sync-versions.js) had the same exemption.
const REPO_ROOT = path.resolve(__dirname, '..', '..');
const SCAN_DIRS = [__dirname, path.join(REPO_ROOT, 'bin'), path.join(REPO_ROOT, 'scripts')];

/** Every scannable source file across SCAN_DIRS, as {label, src} sorted by label. */
function scanTargets() {
  const out = [];
  for (const dir of SCAN_DIRS) {
    let names;
    try { names = fs.readdirSync(dir); } catch { continue; }
    for (const f of names.filter((n) => n.endsWith('.js') && !n.endsWith('.test.js')).sort()) {
      out.push({
        label: `${path.relative(REPO_ROOT, dir)}/${f}`,
        file: f,
        src: fs.readFileSync(path.join(dir, f), 'utf8'),
      });
    }
  }
  return out;
}

test('the masker sees every file that spawns (guard-of-the-guard)', () => {
  // Cross-check with a dumb detector: any file that routes options through
  // hidden()/npmInvocation() must yield at least one call site to the scanner.
  // Zero would mean the masking silently swallowed that file's code — the
  // failure mode that shipped in the first version of this test.
  const blind = [];
  for (const { label, file, src } of scanTargets()) {
    if (file === 'proc-opts.js' || file === 'npm-exec.js') continue; // define the helpers, don't spawn
    if (!/(?<![.\w$])(hidden|npmInvocation)\s*\(/.test(src)) continue;
    CALL_RE.lastIndex = 0;
    if (!CALL_RE.test(maskLiterals(src))) blind.push(label);
  }
  assert.deepEqual(blind, [], 'the scanner found no call sites in files that demonstrably spawn');
});

test('every child_process call site in the shipped JS dirs sets windowsHide', () => {
  const failures = [];
  let scanned = 0;
  for (const { label, src } of scanTargets()) {
    const masked = maskLiterals(src);
    CALL_RE.lastIndex = 0;
    scanned += (masked.match(CALL_RE) || []).length;
    for (const bad of findUnguarded(src)) failures.push(`${label}:${bad.line} — ${bad.name}(...)`);
  }
  // Positive control on the scan itself. A near-exact bound, not a loose floor:
  // the previous `>= 25` was so far under the real 47 that a rewrite could stop
  // seeing HALF the call sites and still pass. Counted at the time of writing;
  // -2 of slack absorbs an incidental removal without hiding a scanner that has
  // gone blind. Adding call sites raises it — update the number deliberately.
  const EXPECTED_CALL_SITES = 50;
  assert.ok(scanned >= EXPECTED_CALL_SITES - 2,
    `expected the scanner to still find ~${EXPECTED_CALL_SITES} call sites, found ${scanned} — ` +
    'a drop this large means the scanner stopped seeing code, not that the code went away');
  assert.deepEqual(failures, [],
    'these child_process calls will flash a console window on Windows — wrap the options in hidden({...}) from ./proc-opts');
});

test('proc-opts.hidden defaults windowsHide on and preserves caller options', () => {
  const { hidden } = require('./proc-opts');
  assert.equal(hidden().windowsHide, true);
  assert.deepEqual(hidden({ cwd: '/x', timeout: 5 }),
    { windowsHide: true, cwd: '/x', timeout: 5 });
  // An explicit caller value wins — nothing in-tree needs it, but silently
  // overriding a deliberate `false` would be a surprise.
  assert.equal(hidden({ windowsHide: false }).windowsHide, false);
});

// killSignal is deliberately NOT a hidden() default: SIGKILL is right for the
// untrusted/hang-prone statusline children (P1-17), but hard-killing a timed
// `git pull` or npm child orphans their lock files (.git/index.lock) and
// silently breaks every later marketplace refresh. The two statusline call
// sites opt in explicitly; this pins both halves of that decision.
test('killSignal SIGKILL is opted in at the statusline call sites, not defaulted globally', () => {
  const { hidden } = require('./proc-opts');
  assert.equal(hidden().killSignal, undefined,
    'a global SIGKILL default would orphan git/npm lock files on timeout');
  assert.equal(hidden({ killSignal: 'SIGKILL' }).killSignal, 'SIGKILL');
  // Anchored to the OPTION OBJECT, not the file: a file-scoped match stays
  // green when the line is commented out or drifts away from its call (proven
  // in the pre-tag review). Each `timeout:` in these files must have a SIGKILL
  // within the same options literal — i.e. before the object closes.
  for (const file of ['statusline-composite.js', 'statusline.js']) {
    const src = fs.readFileSync(path.join(__dirname, file), 'utf8');
    const uncommented = src.replace(/^\s*\/\/.*$/gm, '');
    const timeouts = [...uncommented.matchAll(/timeout:\s*\d+/g)];
    assert.ok(timeouts.length > 0,
      `${file}: expected at least one timed spawn — this guard would be vacuous`);
    for (const m of timeouts) {
      // Options-literal window: from this `timeout:` to the first `}))` or `})`
      // that closes the call, whichever comes first.
      const rest = uncommented.slice(m.index);
      const end = rest.search(/\}\s*\)/);
      const window = end === -1 ? rest : rest.slice(0, end);
      assert.ok(/killSignal:\s*'SIGKILL'/.test(window),
        `${file}: the timed spawn at offset ${m.index} runs an untrusted or ` +
        `hang-prone child without killSignal: 'SIGKILL' — Node sends SIGTERM and ` +
        `then WAITS, so a deaf child makes the timeout unreachable (audit P1-17)`);
    }
  }
});
