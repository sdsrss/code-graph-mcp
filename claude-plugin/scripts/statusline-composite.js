#!/usr/bin/env node
'use strict';
// FIRST statement, before this file's other requires (pre-tag review
// 2026-09-02): the handler installed after them could not catch a throw
// from `require('./lifecycle')` itself, which is exactly the broken-install
// case JS-12 exists for. Guarded on `require.main` so importing this module
// in a test does NOT install a process-wide handler that exits 0 — that
// would swallow the test's own failures.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('statusLine');

/**
 * Composite StatusLine — combines multiple statusline providers.
 * Reads stdin (JSON context from Claude Code), pipes to the primary
 * statusline (GSD), then appends code-graph status.
 */
const { execFileSync } = require('child_process');
const path = require('path');
const os = require('os');
const lifecycle = require('./lifecycle');
const { hidden } = require('./proc-opts');
const { readRegistry } = lifecycle;
const cleanupDisabledStatusline = lifecycle.cleanupDisabledStatusline || (() => ({ cleaned: false }));

const SEPARATOR = ' \x1b[2m|\x1b[0m ';

function main() {
  // Same reasoning as statusline.js: the teardown writes settings.json and the
  // registry, both of which throw on a read-only config dir, and this is THE
  // command Claude Code runs for the status line — an uncaught throw here blanks
  // every provider's segment, not just ours.
  let disabledCleanup = { cleaned: false };
  try {
    disabledCleanup = cleanupDisabledStatusline();
  } catch { /* teardown is optional; rendering is not */ }
  if (disabledCleanup.cleaned) process.exit(0);

  // Collect stdin (Claude Code pipes JSON context)
  let stdinData = '';
  let ran = false;
  const stdinTimeout = setTimeout(() => { if (!ran) { ran = true; run(''); } }, 2000);
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => { stdinData += chunk; });
  process.stdin.on('end', () => { clearTimeout(stdinTimeout); if (!ran) { ran = true; run(stdinData); } });
}

// Only run the statusline when invoked as a CLI; `require()` (tests) just imports helpers.
if (require.main === module) {
  main();
}

function run(stdin) {
  const registry = readRegistry();
  if (registry.length === 0) {
    // Fallback: no registry, run code-graph only
    const cg = runProvider(codeGraphCommand(), false, stdin, 'code-graph');
    if (cg) process.stdout.write(cg);
    return;
  }

  // Display order: pre-existing statuslines (_previous) first, then our providers.
  // This ensures plugins installed earlier appear before ours.
  const sorted = registry.slice().sort((a, b) => {
    if (a.id === '_previous') return -1;
    if (b.id === '_previous') return 1;
    return 0;
  });

  const outputs = [];
  for (const provider of sorted) {
    const out = runProvider(provider.command, provider.needsStdin, stdin, provider.id);
    if (out) outputs.push(out);
  }
  if (outputs.length > 0) {
    process.stdout.write(outputs.join(SEPARATOR));
  }
}

/// True when the command needs a shell to mean what it says.
///
/// Gated on the entry being `_previous`, and that gate is the whole point.
/// `_previous` IS the user's `statusLine.command`, which Claude Code runs
/// through a shell — so a captured pipeline is legitimate there, and a pipeline
/// cannot run through `execFileSync` under any splitting: it produces ENOENT and
/// a silently missing segment.
///
/// The other two registry classes were never shell strings. `codeGraphCommand()`
/// composes `node "<__dirname>/statusline.js"`, and third-party entries arrive
/// through `statusline-chain.js register`, whose only executor has ever been
/// `execFileSync`. Handing those to a shell imposes semantics they never had,
/// and OUR segment is the one that dies: measured, a plugin installed under a
/// directory named `dev$work` produced `node "…/dev$work/statusline.js"`, which
/// a shell reads as `…/dev/statusline.js` — segment gone, `catch` swallows it.
/// Inside double quotes only `$` and a backtick break, which is why this stayed
/// invisible until someone had one in an install path.
///
/// Trade-off, stated because it is a real one: through `sh -c`, the timeout's
/// SIGKILL reaches the SHELL, not necessarily a grandchild that traps signals
/// (the hazard the direct-exec path was hardened against). Confining the shell
/// to `_previous` also confines that loss to the entry that cannot work without
/// it. Windows keeps everything on the direct path — note that Claude Code
/// itself runs statusline commands through Git Bash there, so a `_previous`
/// pipeline works in Claude Code and still dies here; the fix is half-applied by
/// platform, which is a gap rather than a regression (it never worked here).
function needsShell(command, id) {
  return id === '_previous' && process.platform !== 'win32' && SHELL_METACHARS.test(command);
}

function runProvider(command, needsStdin, stdin, id) {
  if (!command) return null;
  try {
    // Parse command into executable + args
    const parts = needsShell(command, id) ? ['/bin/sh', '-c', command] : parseCommand(command);
    if (!parts) return null;

    // Claude Code runs statusLine.command through a shell, so a leading `~`
    // (e.g. `~/.claude/utils/statusline.sh`) is expanded natively. execFileSync
    // does NOT use a shell, so we must expand `~/` ourselves on every word —
    // otherwise a `_previous` command captured verbatim throws ENOENT and gets
    // swallowed below, silently dropping the user's original statusline.
    // `sh -c` does its own tilde expansion; expanding our own would corrupt the
    // script text (`~` inside a quoted string is not a home directory).
    const argv = needsShell(command, id) ? parts : parts.map(expandTilde);

    // Forward Claude Code's authoritative current dir (from the stdin payload) as
    // a plugin-scoped env var. The code-graph provider gates on it instead of its
    // own process.cwd(), which need not track the session's working dir. Harmless
    // to `_previous`/third-party providers, which ignore the unknown var. The
    // CODE_GRAPH_ prefix (not CLAUDE_) keeps it out of Claude Code's own namespace.
    const cwd = cwdFromStdin(stdin);
    const env = cwd ? { ...process.env, CODE_GRAPH_STATUSLINE_CWD: cwd } : process.env;

    const out = execFileSync(argv[0], argv.slice(1), hidden({
      timeout: 3000,
      // SIGKILL, not the SIGTERM default: a provider that traps SIGTERM makes
      // Node's timeout unreachable and hangs every render (audit P1-17).
      killSignal: 'SIGKILL',
      stdio: ['pipe', 'pipe', 'pipe'],
      input: needsStdin ? stdin : '',
      env,
    })).toString().trim();

    return out || null;
  } catch (err) {
    // EPIPE is OUR failure to deliver, not the provider's failure to answer.
    // `execFileSync` writes `input` into the child's stdin; a provider that
    // exits without reading it (a static `echo`, an env-var-only line) makes
    // that write fail — and by then the child has already produced its output
    // in full. Discarding it drops the user's original statusline for a reason
    // that has nothing to do with the provider. `_previous` is registered with
    // needsStdin unconditionally true (lifecycle.js), so every such user is
    // exposed. Measured: 10/10 EPIPE with a payload past the pipe buffer and
    // the child's stdout complete in 10 of 10; on ordinary payloads the same
    // discard happens as a timing race, which is what made
    // lifecycle.e2e.test.js's "issue #24" intermittent for two releases.
    // An empty stdout still falls through: nothing was produced, nothing to keep.
    if (err && err.code === 'EPIPE') {
      const salvaged = (err.stdout || '').toString().trim();
      if (salvaged) return salvaged;
    }
    // Swallowing is correct in production and stays the default: a third-party
    // provider that throws must not take the user's whole statusline with it.
    // But a swallowed error is also why `lifecycle.e2e.test.js`'s "issue #24"
    // has gone intermittently red with nothing to go on — a dropped provider and
    // a provider that printed nothing are the same observation from outside, so
    // every diagnosis of it so far has been a guess. Opt-in, off unless the env
    // var is set, so no shipped behavior changes.
    if (process.env.CODE_GRAPH_STATUSLINE_DEBUG) {
      // Its own try/catch: `process.stderr.write` throws on a closed stderr, and
      // this sits inside the catch that exists so a bad provider cannot take the
      // statusline down. Unguarded, the throw escapes runProvider and past the
      // provider loop in run(), which has none — a debug switch that kills the
      // whole line is worse than no debug switch.
      try {
        const why = err && (err.code || err.message) ? (err.code || err.message) : String(err);
        process.stderr.write(`[statusline] provider '${id}' dropped: ${why}\n`);
      } catch { /* diagnostics are best-effort; rendering is not */ }
    }
    return null;
  }
}

// Extract Claude Code's current working directory from the stdin JSON context.
// Prefer the top-level `cwd`, then `workspace.current_dir`; both track the
// session's working dir (after the model runs `cd`). Returns null for empty,
// non-JSON, or cwd-less payloads (e.g. the stdin-timeout fallback passes '').
// Only a non-empty STRING is accepted: a malformed `cwd` (number/object) would
// otherwise be coerced to a bogus env path that resolves nowhere and silently
// blanks the segment — null keeps the gate on the safe process.cwd() fallback.
function cwdFromStdin(stdin) {
  if (!stdin) return null;
  try {
    const ctx = JSON.parse(stdin);
    const v = ctx && (ctx.cwd || (ctx.workspace && ctx.workspace.current_dir));
    return typeof v === 'string' && v ? v : null;
  } catch { return null; }
}

/// Shell constructs a direct `execFileSync` cannot honour at all: pipelines,
/// redirection, sequencing, command substitution, backgrounding.
///
/// Deliberately NOT here: `\\` (every Windows path has them), `~` (expandTilde
/// handles it), and glob characters (far likelier to be a literal in a path
/// than an intended glob in a statusline command).
const SHELL_METACHARS = /[|&;<>()`$]/;

/// Split a command line into argv the way a shell would: double quotes, single
/// quotes, and backslash escapes of space / quote / backslash.
///
/// The old parser was a single regex that only understood ONE double-quoted
/// word immediately after the executable; everything else went through
/// `split(/\s+/)`. So a `_previous` command whose path contains a space —
/// `"C:\Program Files\tools\line.exe"`, `node "~/My Configs/line.js"` — was
/// torn into fragments, `execFileSync` threw ENOENT, and the catch swallowed
/// it. The user's original statusline vanished without a word, which is exactly
/// the case the `_previous` slot exists to protect (audit 2026-08-22 P2-9).
///
/// A backslash escapes only space, quote and backslash. Treating it as a
/// general escape would eat `C:\Users\me\bin` on Windows, turning a working
/// path into a broken one — a repair that breaks the platform it did not test.
///
/// Returns null for an unterminated quote: the caller then leaves the provider
/// alone rather than exec'ing a guess.
function tokenize(cmd) {
  const argv = [];
  let cur = '';
  let has = false;
  let quote = null; // '"' | "'" | null
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (quote === "'") {
      // Single quotes are literal, backslash included — POSIX rules.
      if (c === "'") quote = null;
      else { cur += c; has = true; }
      continue;
    }
    if (c === '\\' && (cmd[i + 1] === ' ' || cmd[i + 1] === '"' || cmd[i + 1] === '\\')) {
      cur += cmd[++i];
      has = true;
      continue;
    }
    if (quote === '"') {
      if (c === '"') quote = null;
      else { cur += c; has = true; }
      continue;
    }
    if (c === '"' || c === "'") { quote = c; has = true; continue; }
    if (/\s/.test(c)) {
      if (has) { argv.push(cur); cur = ''; has = false; }
      continue;
    }
    cur += c;
    has = true;
  }
  if (quote) return null; // unterminated quote — do not guess
  if (has) argv.push(cur);
  return argv.length > 0 ? argv : null;
}

function parseCommand(cmd) {
  return tokenize(cmd);
}

// Expand a leading `~` / `~/` to the home directory, mirroring shell tilde
// expansion (which Claude Code applies when it runs statusLine.command, but
// execFileSync does not). Only a bare `~` or a `~/`-prefixed word is expanded;
// `~user` and mid-string `~` are left untouched (we don't resolve other users).
function expandTilde(p) {
  if (p === '~') return os.homedir();
  if (p.startsWith('~/')) return path.join(os.homedir(), p.slice(2));
  return p;
}

function codeGraphCommand() {
  // Always derive from __dirname — CLAUDE_PLUGIN_ROOT can leak from other plugins
  return `node "${path.join(__dirname, 'statusline.js')}"`;
}

module.exports = { run, runProvider, parseCommand, tokenize, needsShell, expandTilde, cwdFromStdin };
