'use strict';
// SessionStart keeps the steering text in <git top-level>/.claude/rules/code-graph.md
// (tasks/specs/steering-channel.md, design r4 and shapes 1–24).
//
// Why a file at all: the MCP `instructions` reach the model in every session,
// yet a 2026-10-06 A/B in real `claude -p` sessions found the same text in a
// launch-loaded rules file raised code-graph use on structural questions from
// 19/30 to 26/30 runs (evals/README.md). Why this file: CLAUDE.md and
// CLAUDE.local.md both make Claude Code stop reading the repo's AGENTS.md by
// default; `.claude/rules/*.md` does not. Why it stays out of the user's way:
// git-excluded (`git status` never lists it — the side effect that stopped
// SessionStart writing CLAUDE.md, decision D4), never written where npm would
// publish it, never through a link, and swept at uninstall through the
// adopted-projects registry like the CLAUDE.md block.
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execFileSync } = require('child_process');
const {
  buildRulesFile, detectProjectType, rulesFilePath, removeRulesFile, RULES_REL,
  recordAdopted, isPluginModeInstall, platformGuard, writeFileAtomic,
  claudeMdPath, SENTINEL_BEGIN_SRC, MANAGED_BY,
} = require('./adopt');
const { hidden } = require('./proc-opts');

// In .code-graph/: "this plugin created the file here". A file that is gone
// while this is present was removed by the user, and stays removed.
const RULES_MARKER = 'rules-file';
// Exclude pattern, anchored at the top-level like the `.code-graph/` one.
const EXCLUDE_LINE = '/' + RULES_REL.split(path.sep).join('/');

function git(cwd, args) {
  return execFileSync('git', args, hidden({
    cwd, encoding: 'utf8', timeout: 2000, stdio: ['ignore', 'pipe', 'ignore'],
  })).trim();
}
function gitOk(cwd, args) {
  try { git(cwd, args); return true; } catch { return false; }
}
function realOrSelf(p) { try { return fs.realpathSync(p); } catch { return path.resolve(p); } }

// npm decides what `npm publish` ships from package.json `files` (else
// .npmignore / .gitignore); `.git/info/exclude` is git-only. So a root npm could
// publish keeps no file of ours: no `"private": true` and no `files` array, or
// a `files` entry that can reach `.claude/` (a `.claude…` path, any glob that
// starts with `*`, `.`, or an empty entry). Unreadable or unparsable counts as
// publishable — "could not tell" is not "private".
function npmPublishable(cwd) {
  let raw;
  try { raw = fs.readFileSync(path.join(cwd, 'package.json'), 'utf8'); } catch (e) {
    return !(e && e.code === 'ENOENT');
  }
  let pkg;
  try { pkg = JSON.parse(raw); } catch { return true; }
  if (!pkg || typeof pkg !== 'object') return true;
  if (pkg.private === true) return false;
  if (!Array.isArray(pkg.files)) return true;
  return pkg.files.some((f) => {
    if (typeof f !== 'string') return true;
    const e = f.trim().replace(/^\.\/+/, '').replace(/^\/+/, '');
    return e === '' || e === '.' || e.startsWith('.claude') || e.startsWith('*');
  });
}

function anySymlink(cwd) {
  for (const rel of ['.claude', path.join('.claude', 'rules'), RULES_REL]) {
    try { if (fs.lstatSync(path.join(cwd, rel)).isSymbolicLink()) return true; } catch { /* absent */ }
  }
  return false;
}

function claudeMdHasBlock(cwd) {
  try {
    return new RegExp(`^[ \\t]*${SENTINEL_BEGIN_SRC}[ \\t]*$`, 'm').test(fs.readFileSync(claudeMdPath(cwd), 'utf8'));
  } catch { return false; }
}

/** Append our line to the repo's info/exclude unless git already ignores the path. */
function ensureExcluded(cwd) {
  if (gitOk(cwd, ['check-ignore', '-q', '--no-index', '--', RULES_REL])) return true;
  try {
    // --git-path resolves info/exclude to the common dir for a linked worktree.
    const file = path.resolve(cwd, git(cwd, ['rev-parse', '--git-path', 'info/exclude']));
    let cur = '';
    try { cur = fs.readFileSync(file, 'utf8'); } catch (e) { if (!e || e.code !== 'ENOENT') throw e; }
    if (cur.split('\n').some((l) => l.trim() === EXCLUDE_LINE)) return true;
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.appendFileSync(file, (cur && !cur.endsWith('\n') ? '\n' : '') + EXCLUDE_LINE + '\n');
    return true;
  } catch { return false; }
}

/**
 * Create, refresh or leave the rules file for `cwd`. Never throws.
 * @returns {{action: 'created'|'updated'|'unchanged'|'skipped'|'refused', reason?: string, text?: string}}
 *   `text` on 'created': the session that wrote the file has already read its
 *   rules, so the caller hands it this text once.
 */
function maybeWriteRulesFile({ cwd, home, env, scriptPath } = {}) {
  try {
    return writeRulesFile({
      cwd: cwd || process.cwd(), home: home || os.homedir(), env: env || process.env, scriptPath,
    });
  } catch (e) {
    return { action: 'skipped', reason: 'error', error: (e && e.message) || String(e) };
  }
}

function writeRulesFile({ cwd, home, env, scriptPath }) {
  const skip = (reason) => ({ action: 'skipped', reason });
  const refuse = (reason) => ({ action: 'refused', reason });
  if (platformGuard()) return skip('platform');
  if (env.CODE_GRAPH_NO_AUTO_ADOPT === '1') return skip('opted-out');
  if (!isPluginModeInstall(scriptPath || __dirname)) return skip('not-plugin-mode');
  const marker = path.join(cwd, '.code-graph', RULES_MARKER);
  try { if (!fs.statSync(path.join(cwd, '.code-graph')).isDirectory()) return skip('no-index'); } catch { return skip('no-index'); }
  let top;
  try { top = git(cwd, ['rev-parse', '--show-toplevel']); } catch { return skip('not-git'); }
  if (realOrSelf(top) !== realOrSelf(cwd)) return skip('not-top-level');
  const real = realOrSelf(cwd);
  if (real === realOrSelf(home) || real === path.parse(real).root) return skip('home-or-root');
  if (claudeMdHasBlock(cwd)) return skip('claude-md-block');
  if (anySymlink(cwd)) return refuse('symlink');

  const p = rulesFilePath(cwd);
  let current = null;
  try { current = fs.readFileSync(p, 'utf8'); } catch { /* absent */ }
  if (current !== null && current.split('\n', 1)[0].trim() !== MANAGED_BY) return refuse('foreign-file');
  if (npmPublishable(cwd)) {
    // Ours and now publishable (the root lost `"private"`): take it out, and
    // forget we made it — this is not the user removing it.
    if (current !== null && removeRulesFile(cwd).removed) {
      try { fs.unlinkSync(marker); } catch { /* absent */ }
    }
    return refuse('npm-publishable');
  }
  if (current === null && fs.existsSync(marker)) return skip('removed-by-user');

  const text = buildRulesFile(detectProjectType(cwd));
  if (current === text) return { action: 'unchanged' };
  if (gitOk(cwd, ['ls-files', '--error-unmatch', '--', RULES_REL])) return refuse('tracked');
  if (!ensureExcluded(cwd)) return refuse('exclude-unwritable');
  fs.mkdirSync(path.dirname(p), { recursive: true });
  writeFileAtomic(p, text);
  try { fs.writeFileSync(marker, new Date().toISOString() + '\n'); } catch { /* re-created next time if deleted */ }
  recordAdopted(cwd, home);
  return current === null ? { action: 'created', text } : { action: 'updated' };
}

module.exports = { maybeWriteRulesFile, npmPublishable, RULES_MARKER, EXCLUDE_LINE };
