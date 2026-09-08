'use strict';
// `CLAUDE_CONFIG_DIR` is dropped before anything runs. The sandboxes below
// redirect HOME and pass `{...process.env}` to their children, and
// `claudeHome()` is `CLAUDE_CONFIG_DIR || homedir/.claude` — so for a developer
// who exports it (the documented multi-profile setup) the variable WINS over the
// redirected HOME and these tests would act on the LIVE config. Pinned by
// `js_test_files_neutralize_claude_config_dir` in tests/hardening.rs.
delete process.env.CLAUDE_CONFIG_DIR;

/**
 * The two command-name entry points and the one dispatcher behind them.
 *
 * Issue #41's real fix: Claude Code puts `<plugin-root>/bin` on the Bash tool's
 * PATH for every enabled plugin, and this plugin shipped no such directory — so
 * every steering surface that printed the bare name printed a command the
 * user's shell answered with "command not found". The launcher is what makes
 * the printed spelling true; these tests are what keep it shipped, executable,
 * and identical to the npm entry point it shares a dispatcher with.
 */
const test = require('node:test');
const assert = require('node:assert');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { execFileSync } = require('child_process');

const PLUGIN_ROOT = path.resolve(__dirname, '..');
const LAUNCHER = path.join(PLUGIN_ROOT, 'bin', 'code-graph-mcp');
const LAUNCHER_CMD = path.join(PLUGIN_ROOT, 'bin', 'code-graph-mcp.cmd');
// The npm package's bin entry, one level above the plugin directory.
const NPM_ENTRY = path.resolve(PLUGIN_ROOT, '..', 'bin', 'cli.js');

function mkDir(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cli-entry-'));
  t.after(() => { try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* gone */ } });
  return dir;
}

// Run an entry point in a throwaway cwd. Returns { status, stdout, stderr }.
// Never runs in the repo: `adopt` writes CLAUDE.md, and a guard that regressed
// would do it here.
function run(t, entry, args) {
  const cwd = mkDir(t);
  try {
    const stdout = execFileSync(process.execPath, [entry, ...args], {
      cwd, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
    });
    return { status: 0, stdout, stderr: '' };
  } catch (e) {
    return { status: e.status, stdout: e.stdout || '', stderr: e.stderr || '' };
  }
}

test('the plugin ships a launcher at the path Claude Code puts on PATH', () => {
  assert.ok(
    fs.existsSync(LAUNCHER),
    `${LAUNCHER} is missing — Claude Code adds <plugin-root>/bin to PATH, so ` +
    'without this file every printed `code-graph-mcp …` is "command not found" ' +
    'on a plugin-only install (issue #41)'
  );
  assert.ok(fs.existsSync(LAUNCHER_CMD), `${LAUNCHER_CMD} is missing — Windows PATHEXT needs it`);
});

// Round 2: nothing pinned the exit-code propagation, so deleting the line was a
// green mutation. A source assertion rather than an execution one — this suite
// runs on POSIX and cmd.exe is not available to prove it the honest way; what
// CAN be pinned is that the line is present and last, which is where cmd.exe
// requires it.
test('the .cmd launcher propagates the exit code, on its last line', () => {
  const lines = fs.readFileSync(LAUNCHER_CMD, 'utf8')
    .split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  assert.equal(lines[lines.length - 1].toLowerCase(), 'exit /b %errorlevel%',
    'without this a non-zero `doctor` or a failed search looks like success to a wrapper');
  assert.ok(lines.some((l) => /^node "%~dp0code-graph-mcp"/.test(l)),
    'the .cmd must run the launcher beside it, not a second copy of its logic');
});

test('the launcher is executable', { skip: process.platform === 'win32' && 'POSIX mode bits' }, () => {
  const mode = fs.statSync(LAUNCHER).mode;
  assert.ok(
    (mode & 0o111) !== 0,
    `${LAUNCHER} is mode ${(mode & 0o777).toString(8)} — a file PATH cannot execute is not on PATH`
  );
});

// The working-tree mode above is not what ships. Both delivery channels copy the
// mode git recorded — the marketplace clones this repo and points at
// `./claude-plugin`, and release.yml tars the same directory — so a launcher
// that is +x here and 100644 in the index arrives unexecutable, the PATH entry
// resolves to a file the shell refuses to run, and issue #41 is back with no
// local symptom to notice it by.
test('git records the launcher as executable', {
  skip: !fs.existsSync(path.resolve(PLUGIN_ROOT, '..', '.git')) && 'not a git checkout',
}, () => {
  const mode = execFileSync('git', ['ls-files', '-s', '--', LAUNCHER], {
    cwd: PLUGIN_ROOT, encoding: 'utf8',
  }).split(/\s/)[0];
  assert.equal(mode, '100755', `git has the launcher at mode ${mode || '(untracked)'}`);
});

test('the launcher dispatches JS-only subcommands with no native binary present', (t) => {
  const r = run(t, LAUNCHER, ['adopt', '--help']);
  assert.equal(r.status, 0, `adopt --help exited ${r.status}; stderr=${r.stderr}`);
  assert.match(r.stdout, /install the code-graph steering block/);
});

// The v0.142.0 defect was two printers of one `unadopt` string drifting apart.
// Two dispatchers would be that defect with more surface, so the entry points
// are asserted byte-identical rather than merely both-working.
for (const args of [['adopt', '--help'], ['unadopt', '--help'], ['uninstall', '--help']]) {
  test(`launcher and npm entry agree byte-for-byte on \`${args.join(' ')}\``, (t) => {
    const a = run(t, LAUNCHER, args);
    const b = run(t, NPM_ENTRY, args);
    assert.equal(a.status, b.status);
    assert.equal(a.stdout, b.stdout);
  });
}

test('the launcher rejects unknown flags instead of performing the side effect', (t) => {
  const r = run(t, LAUNCHER, ['adopt', '--helpp']);
  assert.equal(r.status, 2, `expected exit 2, got ${r.status}`);
  assert.match(r.stderr, /unknown argument\(s\): --helpp/);
});

// ── A faithful plugin-only install ──────────────────────────────────────────
//
// Running the repo's own launcher proves nothing about the users this exists
// for: discovery's dev-repo tier finds `target/release/code-graph-mcp`, which
// has the whole `claude-plugin/` tree two levels above it and can therefore
// re-exec any JS-dispatched subcommand. A plugin-only install has neither. So
// the plugin is copied somewhere with no Cargo.toml above it, HOME is
// redirected, and the only binary discovery can reach is a stub in the cache
// directory that refuses every call with a sentinel.
//
// The sentinel is the point. `doctor` on a real plugin-only install reaches the
// CACHED binary, which has no `doctor.js` beside it to re-exec — verified
// directly: `~/.cache/code-graph/bin/code-graph-mcp doctor` answers `doctor.js
// not found. Looked in: …/bin/../../claude-plugin/scripts/doctor.js`. Any
// subcommand that consults the binary at all is broken for these users, and
// this fixture is what makes "consulted the binary" observable instead of
// accidentally working off whatever the host machine has installed.
function mkPluginOnly(t) {
  const root = mkDir(t);
  fs.cpSync(PLUGIN_ROOT, path.join(root, 'claude-plugin'), { recursive: true });
  const home = path.join(root, 'home');
  const cacheBin = path.join(home, '.cache', 'code-graph', 'bin');
  fs.mkdirSync(cacheBin, { recursive: true });
  const stub = path.join(cacheBin, 'code-graph-mcp');
  // Answers --version above the package version so the discovery chain's gate
  // accepts it outright rather than filing it as a stale fallback.
  fs.writeFileSync(stub,
    '#!/bin/sh\n' +
    'if [ "$1" = "--version" ]; then echo "code-graph-mcp 999.0.0"; exit 0; fi\n' +
    'echo "REACHED_THE_BINARY $*"\nexit 3\n');
  fs.chmodSync(stub, 0o755);
  // USERPROFILE alongside HOME: `os.homedir()` reads USERPROFILE on Windows and
  // ignores HOME, so a one-name redirect points the sandbox at the developer's
  // real home there (the two-name rule, pinned by tmpdir-drift-guard.test.js).
  const env = { ...process.env, HOME: home, USERPROFILE: home, PATH: '/usr/bin:/bin' };
  delete env._FIND_BINARY_ROOT;
  return { launcher: path.join(root, 'claude-plugin', 'bin', 'code-graph-mcp'), env, cwd: root };
}

function runPluginOnly(box, args) {
  try {
    const stdout = execFileSync(process.execPath, [box.launcher, ...args], {
      cwd: box.cwd, env: box.env, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
    });
    return { status: 0, stdout, stderr: '' };
  } catch (e) {
    return { status: e.status, stdout: e.stdout || '', stderr: e.stderr || '' };
  }
}

const posixOnly = { skip: process.platform === 'win32' && 'sh stub binary' };

// The control that keeps the two tests below from passing vacuously: if the
// sandbox silently resolved some other binary — or none — the sentinel would be
// absent from THIS result too, and "doctor never reached the binary" would be
// true for the wrong reason.
test('plugin-only fixture control: a forwarded subcommand does reach the binary', posixOnly, (t) => {
  const r = runPluginOnly(mkPluginOnly(t), ['callgraph', 'someSymbol']);
  assert.match(r.stdout, /REACHED_THE_BINARY callgraph someSymbol/,
    `sandbox did not route to the stub; status=${r.status} stderr=${r.stderr}`);
  assert.equal(r.status, 3);
});

test('plugin-only: doctor is answered without consulting the binary', posixOnly, (t) => {
  const r = runPluginOnly(mkPluginOnly(t), ['doctor', '--help']);
  assert.doesNotMatch(r.stdout + r.stderr, /REACHED_THE_BINARY/,
    'doctor was forwarded to the binary — on a real install that binary is the ' +
    'cached one, which has no doctor.js beside it to re-exec');
  assert.equal(r.status, 0, `stderr=${r.stderr}`);
  assert.match(r.stdout, /USAGE:\n\s+code-graph-mcp doctor/);
});

test('plugin-only: doctor rejects unknown flags with its own parser', posixOnly, (t) => {
  const r = runPluginOnly(mkPluginOnly(t), ['doctor', '--check-onlyy']);
  assert.doesNotMatch(r.stdout + r.stderr, /REACHED_THE_BINARY/);
  assert.equal(r.status, 2, `expected doctor's own exit 2, got ${r.status}`);
  assert.match(r.stderr, /doctor: unknown argument\(s\): --check-onlyy/);
});

// Pre-ship review: the self-exec hazard the isNativeBinary directory check
// exists to prevent was pinned only by a unit test — the sandbox above installs
// a minimal PATH, so the launcher's own directory was never a discovery
// candidate end to end. Here it is FIRST on PATH, which is exactly what Claude
// Code does. `which code-graph-mcp` therefore answers with the launcher, and the
// discovery chain must still reach the real binary past it.
//
// The timeout is the anti-recursion assertion: a launcher that resolves itself
// re-execs forever and this call never returns.
test('plugin-only: the launcher first on PATH still resolves past itself', posixOnly, (t) => {
  const box = mkPluginOnly(t);
  const binDir = path.dirname(box.launcher);
  let r;
  try {
    r = {
      status: 0,
      stdout: execFileSync(process.execPath, [box.launcher, 'callgraph', 'someSymbol'], {
        cwd: box.cwd, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
        env: { ...box.env, PATH: `${binDir}:${box.env.PATH}` },
        timeout: 30000,
      }),
    };
  } catch (e) { r = { status: e.status, stdout: e.stdout || '', stderr: e.stderr || '' }; }
  assert.match(r.stdout, /REACHED_THE_BINARY callgraph someSymbol/,
    `the launcher did not reach the stub; if it resolved ITSELF this is the symptom. stdout=${r.stdout}`);
  assert.equal(r.status, 3);
});

test('plugin-only: adopt is answered without consulting the binary', posixOnly, (t) => {
  const r = runPluginOnly(mkPluginOnly(t), ['adopt', '--help']);
  assert.doesNotMatch(r.stdout + r.stderr, /REACHED_THE_BINARY/);
  assert.equal(r.status, 0);
  assert.match(r.stdout, /install the code-graph steering block/);
});
