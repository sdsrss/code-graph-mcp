#!/usr/bin/env node
'use strict';
// adopt / unadopt — installs the code-graph steering into this project:
//   <cwd>/CLAUDE.md            — a concise, sentinel-bracketed managed block
//                                (always-loaded; the router/decision summary)
//   <cwd>/.claude/plugin_code_graph_mcp.md — the full decision table
//                                (on-demand; NOT auto-loaded each session)
// Idempotent. Replaces the pre-v0.74 "adopt into the auto-memory dir" scheme,
// which wrote into ~/.claude/projects/<slug>/memory/ (MEMORY.md sentinel +
// detail file) — equal weight to CLAUDE.md but polluting claude-mem-lite's
// index. migrateLegacyMemoryDir() cleans those legacy artifacts on upgrade.
const fs = require('fs');
const path = require('path');
const os = require('os');
const { PROJECT_MARKERS, isProjectRoot, isNonProjectCwd } = require('./project-detect');

// Managed-block sentinels. Bumped to v2 when the steering target moved from the
// auto-memory dir's MEMORY.md (v1) to the project's CLAUDE.md (v2). The strip
// regex matches ANY version so migration can remove the legacy v1 MEMORY.md block.
const SENTINEL_VERSION = 'v2';
const SENTINEL_BEGIN = `<!-- code-graph-mcp:begin ${SENTINEL_VERSION} -->`;
const SENTINEL_END = '<!-- code-graph-mcp:end -->';
// `[^>\n]*`, not `[^>]*`: an HTML comment marker cannot span lines, but the
// permissive class could — a marker truncated mid-write (`…:begin v2 --`) let
// the match run across newlines to whatever `-->` came next, swallowing every
// user line in between before any of the guards in stripSentinelBlock applied.
const SENTINEL_BEGIN_SRC = '<!-- code-graph-mcp:begin[^>\\n]*-->';
// Marker on the first line of the installed .claude/plugin_code_graph_mcp.md so
// unadopt/needsRefresh can distinguish our generated copy from a user's own file
// of the same name (and so needsRefresh strips it before the bytewise compare).
const MANAGED_BY = '<!-- managed-by: code-graph-mcp -->';
// Legacy first-line marker on the old memory-dir detail file; migration deletes
// only files carrying it (never a user file that happens to share the name).
const LEGACY_ADOPTED_BY = '<!-- adopted-by:';
// Atomic write (tmp in same dir → rename) so a crash mid-write can't leave a
// half-written MEMORY.md / detail file — the dir is shared with claude-mem-lite,
// which reads MEMORY.md on every keyword match. Mirrors lifecycle.js
// writeJsonAtomic / auto-update.js binary promote; accepts a string or Buffer.
// `followLink` is opt-in, and ONLY the CLAUDE.md read-modify-write passes it.
//
// `rename` REPLACES a symlink with a regular file. For CLAUDE.md that is wrong:
// we READ through the link, strip the block from what we read, and must write
// the result back to the same file — otherwise the shared target keeps the block
// while we report `blockPruned: true`, and the project silently gets a detached
// private copy.
//
// For every other caller it is the opposite. The detail file and the registry
// are whole-file REPLACEMENTS of content we own; following a link there turns
// "replace our symlink" into "overwrite whatever the user pointed it at". A
// first pass applied realpath to all four callers and created exactly that loss
// path — the fix for one caller became a bug in the others.
function writeFileAtomic(filePath, data, { followLink = false } = {}) {
  let target = filePath;
  if (followLink) {
    try { target = fs.realpathSync(filePath); } catch { /* new file — no link to follow */ }
  }
  const tmp = target + '.tmp.' + process.pid;
  fs.writeFileSync(tmp, data);
  try {
    fs.renameSync(tmp, target);
  } catch (e) {
    // rename can fail (ENOSPC / EACCES / EROFS on the dir). Don't orphan the
    // temp in the shared memory dir — mirror auto-update.js's binary promote,
    // which cleans its tmp on failure. Best-effort unlink, then rethrow original.
    try { fs.unlinkSync(tmp); } catch { /* already gone */ }
    throw e;
  }
}
// The managed block written into <cwd>/CLAUDE.md. Concise + always-loaded: a
// scannable trigger table that primes the right tool, ending with a pointer to
// the full table at .claude/plugin_code_graph_mcp.md (opened on demand, never
// auto-loaded). Project-type tailoring swaps a couple of rows (web → HTTP-route
// tracing; frontend → reference audits) — body of the detail doc is unchanged.
const BLOCK_HEADING = '## Code Graph (repo-wide AST index)';

function buildTriggerRows(projectType = 'generic') {
  const base = [
    ['Who calls X / what X calls', '`code-graph-mcp callgraph X`'],
    ['Impact before editing a fn', '`code-graph-mcp impact X`'],
    ['Unfamiliar dir / module', '`code-graph-mcp overview <dir>`'],
    ['Symbol source / signature', '`code-graph-mcp show X`'],
    ['Concept search (no exact name)', '`code-graph-mcp search "…"` (vector: MCP `semantic_code_search`)'],
    ['grep + AST context', '`code-graph-mcp grep "pat" [paths] [-t lang] [-g glob] [-c]`'],
  ];
  switch (projectType) {
    case 'web-rs':
    case 'web-node':
    case 'web-py':
    case 'web-go':
      // HTTP route → handler chain matters; insert after the impact row.
      return [base[0], base[1],
        ['HTTP route → handler chain', '`code-graph-mcp trace "GET /api/x"`'],
        ...base.slice(2)];
    case 'frontend':
      // Rename/refactor audits dominate; surface find-references explicitly.
      return [base[0],
        ['Rename / refactor audit (refs)', '`code-graph-mcp refs X`'],
        ...base.slice(1)];
    default:
      return base;
  }
}

// Build the full sentinel-wrapped managed block for a project type. Deterministic
// (same type → byte-identical) so needsRefresh can bytewise-detect drift.
function buildBlock(projectType = 'generic') {
  const rows = buildTriggerRows(projectType);
  const table = ['| Intent | Command |', '|--------|---------|']
    .concat(rows.map(([intent, cmd]) => `| ${intent} | ${cmd} |`))
    .join('\n');
  const body = [
    BLOCK_HEADING,
    '',
    'AST + FTS + vector index of the whole repo — prefer over multi-round Grep/Read for',
    'structural queries (LSP only sees open files; this sees everything). Fastest path = Bash CLI:',
    '',
    table,
    '',
    "Still use Grep for literal strings/regex in non-code files; still Read files you'll edit.",
    'Full command + MCP-tool table: `.claude/plugin_code_graph_mcp.md`',
  ].join('\n');
  return `${SENTINEL_BEGIN}\n${body}\n${SENTINEL_END}`;
}

// Project-type detection tailors the CLAUDE.md block's trigger rows (buildBlock):
// a Rust CLI never benefits from HTTP-route tracing, a React frontend cares more
// about find-references for rename audits. Detection is cheap substring-on-marker
// (no AST, no graph): one fs.readFileSync per cwd. Failure mode is silent
// fall-back to 'generic' — false-negatives are strictly safer than false-positives
// that promote the wrong tool.
function readFileQuiet(p) {
  try { return fs.readFileSync(p, 'utf8'); } catch { return ''; }
}

// Valid project type buckets — also serves as the allow-list for
// `CODE_GRAPH_PROJECT_TYPE` env override. Anything not in this set falls back
// to file-based detection (so a typo'd env var does not silently break).
const PROJECT_TYPES = new Set([
  'rust', 'web-rs', 'web-node', 'web-py', 'web-go',
  'frontend', 'python', 'go', 'node', 'generic',
]);

// Cargo.toml: strip `# ...` comment lines (line-leading or trailing). Then
// extract the contents of the [dependencies] section only — dev/build/target
// deps don't characterize the project's runtime web-vs-cli posture.
//
// Why a state machine and not a TOML parser: we have zero deps and don't
// want to add one for a coarse classification. This handles the >95% case
// (well-formed [dependencies] block); pathological TOML (e.g. inline-table
// dependencies) falls through to false-negative `rust` which is safer than
// false-positive `web-rs`.
function extractCargoRuntimeDeps(cargo) {
  const lines = cargo.split(/\r?\n/);
  const out = [];
  let inDeps = false;
  for (const raw of lines) {
    // Strip `# comment` (line-leading or trailing). Inside string literals
    // `#` is rare in Cargo dep specs; accept the false-strip risk.
    const line = raw.replace(/(^|\s)#.*$/, '$1').trim();
    if (line.startsWith('[')) {
      // New section heading — only `[dependencies]` (canonical, exact match)
      // gates web-framework detection. `[dev-dependencies]`,
      // `[build-dependencies]`, `[target.'...'.dependencies]` deliberately
      // skipped: a project that pulls in axum only for tests is not a web
      // project for routing purposes.
      inDeps = (line === '[dependencies]');
      continue;
    }
    if (inDeps && line) out.push(line);
  }
  return out.join('\n');
}

// pyproject.toml: same pattern as Cargo.toml — strip comments, scan only
// [tool.poetry.dependencies] (Poetry) or [project.dependencies] (PEP 621).
function extractPyRuntimeDeps(pyproj) {
  const lines = pyproj.split(/\r?\n/);
  const out = [];
  let inDeps = false;
  for (const raw of lines) {
    const line = raw.replace(/(^|\s)#.*$/, '$1').trim();
    if (line.startsWith('[')) {
      inDeps = (
        line === '[tool.poetry.dependencies]' ||
        line === '[project.dependencies]' ||
        line === '[project]'  // PEP 621 inline `dependencies = [...]` lives here
      );
      continue;
    }
    if (inDeps && line) out.push(line);
  }
  return out.join('\n');
}

// go.mod: skip `// indirect` lines (transitive deps) and `// comment` lines.
// Direct require blocks are what matter for "is this a web service?" — a
// project that transitively pulls gin via a CLI dep is still a CLI.
function extractGoDirectRequires(gomod) {
  const lines = gomod.split(/\r?\n/);
  const out = [];
  let inRequire = false;
  for (const raw of lines) {
    const trimmed = raw.trim();
    if (trimmed.startsWith('//')) continue;       // pure comment line
    if (/\/\/\s*indirect\b/.test(raw)) continue;  // indirect dep marker
    if (trimmed === 'require (') { inRequire = true; continue; }
    if (inRequire && trimmed === ')') { inRequire = false; continue; }
    if (trimmed.startsWith('require ')) out.push(trimmed.slice(8).trim());
    else if (inRequire && trimmed) out.push(trimmed);
  }
  return out.join('\n');
}

function detectProjectType(cwd = process.cwd(), env = process.env) {
  // 2D: env override beats file-based detection. Honors only valid bucket
  // names; invalid override silently falls through to detection (avoids a
  // typo'd env var silently classifying everything as 'generic'). Power
  // users / CI can pin without touching the heuristics.
  const override = env && env.CODE_GRAPH_PROJECT_TYPE;
  if (override && PROJECT_TYPES.has(override)) {
    return override;
  }

  const cargo = readFileQuiet(path.join(cwd, 'Cargo.toml'));
  if (cargo) {
    const deps = extractCargoRuntimeDeps(cargo);
    // Web-framework detection: match on the dep name token (start-of-line
    // or after whitespace/quote) to avoid hits inside path strings or
    // unrelated metadata. `hyper` deliberately omitted from web-rs — it is
    // also commonly used as a plain HTTP client in CLI tools (false-positive
    // risk too high). axum/actix-web/etc. are unambiguous server stacks.
    if (/^(actix-web|axum|rocket|warp|poem|tide|salvo)\s*=/m.test(deps)) {
      return 'web-rs';
    }
    return 'rust';
  }

  const pkgRaw = readFileQuiet(path.join(cwd, 'package.json'));
  if (pkgRaw) {
    let pkg = null;
    try { pkg = JSON.parse(pkgRaw); } catch { /* malformed → fall through */ }
    if (pkg && typeof pkg === 'object') {
      // Only `dependencies` matters — devDependencies are build/test only,
      // and a project with `react` only in devDependencies is likely a
      // component library, not a frontend app.
      const deps = pkg.dependencies && typeof pkg.dependencies === 'object'
        ? Object.keys(pkg.dependencies)
        : [];
      const has = (name) => deps.includes(name);
      if (has('next') || has('react') || has('vue') || has('svelte') ||
          has('@angular/core') || has('nuxt') || has('astro') ||
          has('remix') || has('solid-js')) {
        return 'frontend';
      }
      if (has('express') || has('fastify') || has('koa') || has('hono') ||
          has('@nestjs/core') || has('@hapi/hapi')) {
        return 'web-node';
      }
    }
    return 'node';
  }

  const pyproj = readFileQuiet(path.join(cwd, 'pyproject.toml'));
  if (pyproj) {
    const deps = extractPyRuntimeDeps(pyproj);
    if (/\b(django|flask|fastapi|starlette|sanic|tornado|quart)\b/i.test(deps)) {
      return 'web-py';
    }
    return 'python';
  }
  // requirements.txt fallback: line-format `pkg==ver` / `pkg>=ver`. Strip
  // `#` comment lines; scan remaining as a flat list (no section headers
  // in this format).
  const reqs = readFileQuiet(path.join(cwd, 'requirements.txt'));
  if (reqs) {
    const cleaned = reqs.split(/\r?\n/)
      .map(l => l.replace(/(^|\s)#.*$/, '$1').trim())
      .filter(Boolean)
      .join('\n');
    if (/^(django|flask|fastapi|starlette|sanic|tornado|quart)\b/im.test(cleaned)) {
      return 'web-py';
    }
    return 'python';
  }

  const gomod = readFileQuiet(path.join(cwd, 'go.mod'));
  if (gomod) {
    const direct = extractGoDirectRequires(gomod);
    if (/\b(gin-gonic|labstack\/echo|gofiber|go-chi|gorilla\/mux)\b/.test(direct)) {
      return 'web-go';
    }
    return 'go';
  }
  return 'generic';
}

const TEMPLATE_PATH = path.resolve(__dirname, '..', 'templates', 'plugin_code_graph_mcp.md');
const TARGET_NAME = 'plugin_code_graph_mcp.md';

// LEGACY (pre-v0.74) memory-dir path — now used ONLY by migrateLegacyMemoryDir
// to locate and remove the old MEMORY.md sentinel + detail file. Claude Code slug
// convention: every non-alphanumeric-non-hyphen char → `-`.
// `/mnt/data_ssd/dev/proj` → `-mnt-data-ssd-dev-proj`
// `/home/sds/.claude/x`   → `-home-sds--claude-x`  (double-dash from `/.`)
//
// `home` is the OS home dir (default `os.homedir()`). When `CLAUDE_CONFIG_DIR`
// is set it overrides `home/.claude`, so multi-account users (personal vs work)
// land in the directory Claude Code itself is using for `projects/`.
function memoryDir(cwd = process.cwd(), home = os.homedir()) {
  const slug = cwd.replace(/[^a-zA-Z0-9-]/g, '-');
  const claudeDir = process.env.CLAUDE_CONFIG_DIR || path.join(home, '.claude');
  return path.join(claudeDir, 'projects', slug, 'memory');
}

// New-scheme targets: steering lives in the project tree, not the memory dir.
// CLAUDE.md is auto-loaded each session (concise block); .claude/<detail> is
// opened on demand. Both keyed off the project cwd — no lossy slug, no collision.
function claudeMdPath(cwd = process.cwd()) { return path.join(cwd, 'CLAUDE.md'); }
function detailDir(cwd = process.cwd()) { return path.join(cwd, '.claude'); }
function detailPath(cwd = process.cwd()) { return path.join(detailDir(cwd), TARGET_NAME); }

function escapeRegex(s) {
  return s.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&');
}

// Remove every match of `re` and heal ONLY the seam each removal leaves behind.
//
// The whole-file `out.replace(/\n{3,}/g, '\n\n')` this replaces was the last
// unscoped edit in a function whose entire contract is "touch nothing but our
// block". It rewrote the user's prose: blank-line runs inside fenced code blocks
// collapsed, and because the collapse changed bytes even when no marker was
// present, `unadopt` reported "De-blocked" for files that never held our block
// — on every SessionStart, and across every registered project on uninstall
// (audit 2026-08-16 P1-15).
//
// Seam rule: the blank lines that end up adjacent BECAUSE the block between them
// was removed collapse to one blank line (the old behavior, now local); bytes
// anywhere else are copied through untouched. A text containing no match is
// returned identical, which is what makes "changed" mean "we removed something".
function stripAndHealSeams(text, re) {
  let out = '';
  let cursor = 0;
  re.lastIndex = 0;
  for (let m = re.exec(text); m !== null; m = re.exec(text)) {
    if (m[0] === '') { re.lastIndex++; continue; }  // zero-width guard: never loop forever
    out += text.slice(cursor, m.index);
    cursor = m.index + m[0].length;
    const before = /\n*$/.exec(out)[0].length;
    const after = /^\n*/.exec(text.slice(cursor))[0].length;
    if (before + after > 2) {
      out = out.slice(0, out.length - before) + '\n\n';
      // Skip the newlines we just absorbed. Safe for the scan: the skipped span
      // is newlines only, and every pattern here starts at a line's first
      // non-newline character.
      cursor += after;
      re.lastIndex = cursor;
    }
  }
  return out + text.slice(cursor);
}

// Strip our sentinel block — well-formed first, then self-heal orphan begin/end.
// Shared by adopt (so re-adopt rewrites a stale/malformed block) and unadopt.
//
// Every rule below exists because its absence deleted a user's prose. The file
// this runs against is the user's own CLAUDE.md, `unadopt` runs it over EVERY
// registered project via `uninstall({unadoptAll:true})`, and the only thing that
// distinguishes our block from their text is a string they are *invited* to
// write down — the block itself tells them not to edit inside it. So: never
// start a match at a marker we cannot prove we wrote, and when in doubt delete
// less. A stale block left behind is visible and removable; deleted prose is not.
function stripSentinelBlock(text) {
  // Line-anchored. A marker mentioned mid-sentence ("the block starts with
  // `<!-- code-graph-mcp:begin v2 -->` — don't edit inside it") is prose, not a
  // block opener; matching it ate everything from that sentence to the end of
  // the real block below it.
  const BEGIN_LINE = `^[ \\t]*${SENTINEL_BEGIN_SRC}[ \\t]*$`;
  const END_LINE = `^[ \\t]*${escapeRegex(SENTINEL_END)}[ \\t]*$`;

  // Match ANY begin version (v1 legacy MEMORY.md block, v2 CLAUDE.md block) so a
  // single strip handles both the new target and the legacy migration cleanup.
  //
  // `(?:(?!BEGIN)[\s\S])*?` — the body may not contain another BEGIN. Without it
  // a lazy `[\s\S]*?` still ANCHORS at the earliest BEGIN in the file, so a
  // marker quoted on its own line (a fenced example, a note to a teammate) made
  // the match start there and run through everything up to our real END.
  const wellFormed = new RegExp(
    `${BEGIN_LINE}(?:(?!${SENTINEL_BEGIN_SRC})[\\s\\S])*?${END_LINE}\\n?`,
    'gm'
  );
  let out = stripAndHealSeams(text, wellFormed);
  // Orphan BEGIN with no matching END (truncation / partial edit): remove the
  // MARKER LINE ONLY, never the content after it.
  //
  // This used to strip from the marker to the next blank line, on the theory
  // that the content below a stray begin marker was our truncated block. It is
  // not decidable. These two files are byte-for-byte the same shape:
  //
  //   BEGIN / "- stale partial block" / "" / "Also keep me."      <- our leftovers
  //   BEGIN / "KEEP: on-call rotation" / "" / "tail prose"        <- their notes
  //
  // and the second one is what a user writes after reading "the block opens
  // with <marker>". The blank-line bound protects nothing when their next line
  // is prose. Since the two cases cannot be told apart, the tie goes to the
  // outcome that is recoverable: a stale fragment left in the file is visible
  // and the user can delete it; prose we deleted is gone. `isAdopted` requires a
  // line-anchored BEGIN *and* END, so a leftover fragment does not block
  // re-adoption — the next adopt writes a fresh, well-formed block.
  const orphanBegin = new RegExp(`${BEGIN_LINE}\\n?`, 'gm');
  out = stripAndHealSeams(out, orphanBegin);
  // Orphan END line by itself — same rule, one line, nothing around it. Written
  // as the same line-anchored removal the other two passes use (the old
  // split/filter/join spelling was a third predicate for "is this our END line",
  // and every duplicated predicate in this file has drifted at least once).
  if (out.includes(SENTINEL_END)) {
    out = stripAndHealSeams(out, new RegExp(`${END_LINE}\\n?`, 'gm'));
  }
  // NOTE: no whole-file newline collapse here. Each removal above healed its own
  // seam; bytes the user wrote are returned exactly as they came in.
  return out;
}

function platformGuard() {
  if (process.platform === 'win32') {
    return { ok: false, reason: 'windows-not-supported' };
  }
  return null;
}

// Project-marker detection (PROJECT_MARKERS / isProjectRoot / isNonProjectCwd)
// now lives in project-detect.js — the single activation gate shared with
// mcp-launcher.js and session-init.js. Imported above and re-exported below.

// ── Adopted-projects registry ───────────────────────────────
// ~/.cache/code-graph/adopted-projects.json — every project adopt() has touched.
// Sole consumer is lifecycle.js uninstall(): without this list it cannot tell
// the user WHICH projects still carry a managed CLAUDE.md block + .code-graph/
// index dir (adoption state is otherwise only discoverable per-project).
// Best-effort: registry loss only degrades uninstall guidance, never adoption.

function adoptedRegistryFile(home) {
  return path.join(home || os.homedir(), '.cache', 'code-graph', 'adopted-projects.json');
}

// Read the registry keeping WHY it failed — the same one bit lifecycle.js's
// readJsonResult exists for. Only a genuinely ABSENT (or empty) file may be
// treated as "nothing here, safe to create": everything else (EACCES, EISDIR,
// truncated JSON, wrong shape) means the file EXISTS and holds entries we cannot
// read. The old lenient reader returned `[]` for all of them and the next
// recordAdopted persisted `[thisProject]` over it — dropping every other adopted
// project, which is exactly the list `uninstall --unadopt-all` iterates, so
// their managed CLAUDE.md blocks would be stranded (audit 2026-08-16 P1-12).
function readAdoptedResult(home) {
  let raw;
  try {
    raw = fs.readFileSync(adoptedRegistryFile(home), 'utf8');
  } catch (err) {
    const missing = Boolean(err) && err.code === 'ENOENT';
    return { list: [], missing, unusable: !missing };
  }
  if (raw.trim() === '') return { list: [], missing: true, unusable: false };
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return { list: [], missing: false, unusable: true };
    return { list: parsed.filter((p) => typeof p === 'string'), missing: false, unusable: false };
  } catch {
    return { list: [], missing: false, unusable: true };
  }
}

/** Read-side contract is unchanged: a list, never a throw. */
function readAdoptedProjects(home) {
  return readAdoptedResult(home).list;
}

/** @returns {boolean} true when the project is recorded (or already was). */
function recordAdopted(projectDir, home) {
  const res = readAdoptedResult(home);
  if (res.unusable) return false;   // never rebuild over entries we cannot read
  try {
    const file = adoptedRegistryFile(home);
    const abs = path.resolve(projectDir);
    if (res.list.includes(abs)) return true;
    fs.mkdirSync(path.dirname(file), { recursive: true });
    writeFileAtomic(file, JSON.stringify([...res.list, abs], null, 2) + '\n');
    return true;
  } catch { return false; }        // best-effort: registry loss only degrades guidance
}

/** @returns {boolean} true when the project is absent from the registry afterwards. */
function removeAdopted(projectDir, home) {
  const res = readAdoptedResult(home);
  if (res.unusable) return false;
  try {
    const abs = path.resolve(projectDir);
    const next = res.list.filter((p) => p !== abs);
    if (next.length === res.list.length) return true;
    writeFileAtomic(adoptedRegistryFile(home), JSON.stringify(next, null, 2) + '\n');
    return true;
  } catch { return false; }
}

function adopt({ cwd, templatePath, home } = {}) {
  const blocked = platformGuard();
  if (blocked) return blocked;

  const effectiveCwd = cwd || process.cwd();
  // Gate on a real-project cwd BEFORE touching the filesystem — Claude Code also
  // spawns headless /tmp sessions (claude-mem-lite); we must leave those alone.
  if (isNonProjectCwd(effectiveCwd)) {
    return { ok: false, reason: 'not-a-project', cwd: effectiveCwd };
  }
  const tpl = templatePath || TEMPLATE_PATH;
  if (!fs.existsSync(tpl)) {
    return { ok: false, reason: 'no-template', template: tpl };
  }

  // Every filesystem touch below is on files the USER owns and may have made
  // unreadable (a root-owned CLAUDE.md from a `sudo` session) or replaced with a
  // directory. Those throw EACCES/EISDIR, and this function is called bare from
  // maybeAutoAdopt → runSessionInit: one such file killed the whole SessionStart
  // hook, so binary verification, index freshness and the hook self-test never
  // ran (audit 2026-08-16 P1-16). Adoption is optional; the rest of the session
  // is not. Every arm below returns a REASON instead of throwing.

  // 1. Install the detail doc at <cwd>/.claude/plugin_code_graph_mcp.md.
  // First line is the MANAGED_BY marker (HTML comment → invisible in rendered
  // markdown) so unadopt/needsRefresh can tell our generated copy from a user
  // file of the same name. needsRefresh strips it before the bytewise compare.
  const dDir = detailDir(effectiveCwd);
  const dPath = detailPath(effectiveCwd);
  let detailWritten = false;
  let desiredDetail;
  try {
    desiredDetail = Buffer.concat([Buffer.from(`${MANAGED_BY}\n`), fs.readFileSync(tpl)]);
  } catch (e) {
    return { ok: false, reason: 'no-template', template: tpl, error: e.code || String(e) };
  }
  try {
    if (!fs.existsSync(dDir)) fs.mkdirSync(dDir, { recursive: true });
    // readFileSync on the existing copy can throw for the same reasons as
    // CLAUDE.md below; an unreadable one is "different from what we want", so
    // fall through to the write and let THAT report the real failure.
    let current = null;
    try { current = fs.readFileSync(dPath); } catch { current = null; }
    if (current === null || !current.equals(desiredDetail)) {
      writeFileAtomic(dPath, desiredDetail);
      detailWritten = true;
    }
  } catch (e) {
    return { ok: false, reason: 'detail-unwritable', detailPath: dPath, error: e.code || String(e) };
  }

  // 2. Ensure the managed block in <cwd>/CLAUDE.md. Create-if-missing, else
  // inject — only our sentinel block is managed; the user's own prose is never
  // touched. Per-project trigger rows tailored to the detected project type.
  const cPath = claudeMdPath(effectiveCwd);
  const block = buildBlock(detectProjectType(effectiveCwd));
  const exists = fs.existsSync(cPath);
  let current = '';
  if (exists) {
    try {
      current = fs.readFileSync(cPath, 'utf8');
    } catch (e) {
      // EACCES / EISDIR / EIO: the file is there and we cannot read it. Writing
      // anyway would replace content we never saw, so this project simply cannot
      // be adopted right now.
      return { ok: false, reason: 'claude-md-unreadable', claudeMdPath: cPath, error: e.code || String(e) };
    }
  }
  if (current.includes(block)) {
    const registryRecorded = recordAdopted(effectiveCwd, home);
    return { ok: true, detailPath: dPath, claudeMdPath: cPath, detailWritten, claudeMdWritten: false, created: false, healed: false, registryRecorded };
  }
  const cleaned = exists ? stripSentinelBlock(current) : '';
  const healed = exists && cleaned !== current;
  const base = cleaned.replace(/\n+$/, '');
  const prefix = base ? base + '\n\n' : '';
  try {
    // followLink: read-modify-write of a file the user may have symlinked.
    writeFileAtomic(cPath, prefix + block + '\n', { followLink: true });
  } catch (e) {
    return { ok: false, reason: 'claude-md-unwritable', claudeMdPath: cPath, error: e.code || String(e) };
  }
  const registryRecorded = recordAdopted(effectiveCwd, home);
  return { ok: true, detailPath: dPath, claudeMdPath: cPath, detailWritten, claudeMdWritten: true, created: !exists, healed, registryRecorded };
}

// "已 install" 判定：detail 文件在 + CLAUDE.md 内有我们的 sentinel 块（任意版本）。
// 用在 maybeAutoAdopt 里做幂等门，也用在 session-init 里推导 quietHooks。
function isAdopted({ cwd } = {}) {
  const effectiveCwd = cwd || process.cwd();
  const cPath = claudeMdPath(effectiveCwd);
  const dPath = detailPath(effectiveCwd);
  if (!fs.existsSync(dPath) || !fs.existsSync(cPath)) return false;
  // Unreadable (EACCES) or a directory (EISDIR) → "not adopted here". A throw
  // out of this predicate took SessionStart down with it (P1-16), and a file we
  // cannot read cannot be proven to hold our block anyway.
  let c;
  try { c = fs.readFileSync(cPath, 'utf8'); } catch { return false; }
  // Line-anchored for the same reason stripSentinelBlock is: a user quoting the
  // markers in prose would otherwise read as adopted, and this gates the
  // idempotent auto-adopt — so the block would never actually be written.
  return new RegExp(`^[ \\t]*${SENTINEL_BEGIN_SRC}[ \\t]*$`, 'm').test(c)
    && new RegExp(`^[ \\t]*${escapeRegex(SENTINEL_END)}[ \\t]*$`, 'm').test(c);
}

// shipped template / 管理块 与已落地版本出现漂移时返回 true。让已 install 的项目
// 在下次 SessionStart 自动对齐到插件最新决策表（含 v1→v2 升级、项目类型变更）。
// 手动编辑会被覆盖——锁定方式：CODE_GRAPH_NO_TEMPLATE_REFRESH=1。
function needsRefresh({ cwd, templatePath } = {}) {
  const effectiveCwd = cwd || process.cwd();
  const cPath = claudeMdPath(effectiveCwd);
  const dPath = detailPath(effectiveCwd);
  const tpl = templatePath || TEMPLATE_PATH;
  if (!fs.existsSync(dPath) || !fs.existsSync(cPath) || !fs.existsSync(tpl)) {
    return false;
  }
  // Detail-doc body drift — strip the leading MANAGED_BY marker line first.
  // Unreadable inputs → false: a refresh we cannot decide is one we must not
  // attempt (and adopt() would refuse the write anyway). Never throws — this
  // runs inside the SessionStart hook (P1-16).
  let shipped;
  let current;
  try {
    shipped = fs.readFileSync(tpl);
    current = fs.readFileSync(dPath);
  } catch { return false; }
  let body = current;
  const nl = current.indexOf(0x0a);
  // Equality on the trimmed line, matching the unadopt guard. `includes` here
  // was harmless in isolation (worst case: a spurious refresh) but it is the
  // third spelling of the same predicate in this file, and the other two both
  // turned out to be wrong — a loose one left behind is the next audit's finding.
  if (nl > 0 && current.subarray(0, nl).toString().trim() === MANAGED_BY) {
    body = current.subarray(nl + 1);
  }
  if (!shipped.equals(body)) return true;
  // CLAUDE.md managed-block drift. Detection is deterministic so adopt and
  // needsRefresh always agree on the variant — including when a project gains a
  // web-framework dep and switches type bucket, or on a sentinel version bump.
  const block = buildBlock(detectProjectType(effectiveCwd));
  try {
    return !fs.readFileSync(cPath, 'utf8').includes(block);
  } catch { return false; }
}

// 检测脚本是否从 Claude Code 插件 cache 运行。
// 走 __dirname 而非 CLAUDE_PLUGIN_ROOT — 后者在多插件共存时会互相污染
// （见 feedback_plugin_env_isolation.md）。
// 默认匹配 `.claude/plugins/` 路径；CLAUDE_CONFIG_DIR 自定义目录时
// 走 startsWith(CLAUDE_CONFIG_DIR/plugins/) 兜底。
function isPluginModeInstall(scriptPath = __dirname) {
  const sep = path.sep;
  if (scriptPath.includes(`${sep}.claude${sep}plugins${sep}`)) return true;
  const envDir = process.env.CLAUDE_CONFIG_DIR;
  if (envDir) {
    const marker = path.join(envDir, 'plugins') + sep;
    if (scriptPath.startsWith(marker)) return true;
  }
  return false;
}

// One-time per-project cleanup of the legacy memory-dir adoption (pre-v0.74):
// the v1 sentinel block in MEMORY.md + the adopted-by-marked detail file under
// ~/.claude/projects/<slug>/memory/. Touches only the CURRENT project's memory
// dir (a single known path derived from cwd) — no ~/.claude traversal (§8 SAFETY).
// Idempotent + guarded: only strips OUR sentinel, only deletes a file carrying
// the adopted-by marker. Safe to run every SessionStart.
function migrateLegacyMemoryDir({ cwd, home } = {}) {
  const result = { memoryIndexPruned: false, legacyDetailRemoved: false };
  if (platformGuard()) return result;
  const dir = memoryDir(cwd, home);
  const legacyDetail = path.join(dir, TARGET_NAME);
  if (fs.existsSync(legacyDetail)) {
    try {
      const head = fs.readFileSync(legacyDetail, 'utf8').split('\n', 1)[0];
      // Same guard as unadopt's (:636), and it must be — this is the MORE
      // frequently executed of the two: maybeAutoAdopt calls migrate on every
      // SessionStart, while unadopt is explicit. Tightening only unadopt's copy
      // left the hot path deleting any user file whose first line merely began
      // with `<!-- adopted-by:` — e.g. a note from someone else's tooling.
      // The legacy marker carries a payload after the prefix, so it is matched
      // as a whole-line HTML comment rather than by equality.
      const h = head.trim();
      if (h.startsWith(LEGACY_ADOPTED_BY) && h.endsWith('-->')) {
        fs.unlinkSync(legacyDetail);
        result.legacyDetailRemoved = true;
      }
    } catch { /* unreadable → leave it */ }
  }
  const legacyIndex = path.join(dir, 'MEMORY.md');
  if (fs.existsSync(legacyIndex)) {
    try {
      const before = fs.readFileSync(legacyIndex, 'utf8');
      const after = stripSentinelBlock(before);
      if (after !== before) {
        writeFileAtomic(legacyIndex, after, { followLink: true });
        result.memoryIndexPruned = true;
      }
    } catch { /* unreadable → leave it */ }
  }
  return result;
}

// 上下文感知默认：插件模式下首次 SessionStart 静默安装（创建/注入 CLAUDE.md 块 +
// .claude/ detail 文件）。/plugin install 本身已构成知情同意；npm / npx / 裸 checkout
// 保持 opt-in。退出：CODE_GRAPH_NO_AUTO_ADOPT=1。每次 SessionStart 先清理旧 memory-dir
// 制品（升级自动迁移），再安装/刷新。
function maybeAutoAdopt({ cwd, home, env, scriptPath } = {}) {
  env = env || process.env;
  // Consistent return shape: every path carries `migrated`. The two pre-gate
  // early returns run before migration, so they report the zero result.
  const noMigration = { memoryIndexPruned: false, legacyDetailRemoved: false };
  if (env.CODE_GRAPH_NO_AUTO_ADOPT === '1') {
    return { attempted: false, reason: 'opted-out', migrated: noMigration };
  }
  if (!isPluginModeInstall(scriptPath || __dirname)) {
    return { attempted: false, reason: 'not-plugin-mode', migrated: noMigration };
  }
  // Clean legacy memory-dir artifacts before installing the new CLAUDE.md scheme.
  const migrated = migrateLegacyMemoryDir({ cwd, home });
  if (isAdopted({ cwd })) {
    // shipped template / 管理块 漂移时重跑 adopt 对齐。
    // opt-out: CODE_GRAPH_NO_TEMPLATE_REFRESH=1（锁定手动编辑）。
    if (env.CODE_GRAPH_NO_TEMPLATE_REFRESH !== '1' && needsRefresh({ cwd })) {
      const result = adopt({ cwd, home });
      return { attempted: true, reason: 'refreshed', result, migrated };
    }
    return { attempted: false, reason: 'already-adopted', migrated };
  }
  const result = adopt({ cwd, home });
  return { attempted: true, reason: 'adopted', result, migrated };
}

function unadopt({ cwd, home } = {}) {
  const blocked = platformGuard();
  if (blocked) return blocked;

  const effectiveCwd = cwd || process.cwd();
  const cPath = claudeMdPath(effectiveCwd);
  const dPath = detailPath(effectiveCwd);
  let fileRemoved = false;
  let blockPruned = false;
  let claudeMdRemoved = false;

  // Detail file — guard on our marker so a user's same-named file is never deleted.
  if (fs.existsSync(dPath)) {
    let mine = false;
    try {
      const head = fs.readFileSync(dPath, 'utf8').split('\n', 1)[0];
      // `===`, not `includes`. adopt writes the marker as the ENTIRE first line
      // (`${MANAGED_BY}\n` + template, :403), so equality is exactly as capable
      // here — while `includes` unlinked any user file whose first line merely
      // quoted the marker, e.g. a note about what this plugin writes.
      // BOTH arms anchored. Tightening only the current marker left the legacy
      // one as a loose startsWith, so a user file opening `<!-- adopted-by: …`
      // was still unlinked — the same half-applied shape this batch keeps
      // producing. The legacy marker carries a payload after the prefix, so it
      // is matched as a whole-line HTML comment rather than by equality.
      const h = head.trim();
      mine = h === MANAGED_BY
        || (h.startsWith(LEGACY_ADOPTED_BY) && h.endsWith('-->'));
    } catch { mine = false; }
    // The unlink itself can fail (read-only dir, EPERM) — an uninstall sweep
    // must keep going for the remaining projects.
    if (mine) {
      try { fs.unlinkSync(dPath); fileRemoved = true; } catch { /* left behind, reported as not removed */ }
    }
  }

  // CLAUDE.md — strip only our block. If nothing else remains, remove the file
  // we created; otherwise preserve the user's prose.
  //
  // Unreadable / a directory / an un-writable dir: report it and move on. This
  // path runs over EVERY registered project from `uninstall --unadopt-all`, so
  // one bad file used to abort the whole sweep with a raw stack trace (P1-16).
  let claudeMdUnreadable = false;
  let claudeMdUnwritable = false;
  if (fs.existsSync(cPath)) {
    let before;
    try {
      before = fs.readFileSync(cPath, 'utf8');
    } catch { claudeMdUnreadable = true; }
    if (before !== undefined) {
      const after = stripSentinelBlock(before);
      if (after !== before) {
        try {
          // Only delete a file we could have created. A symlinked CLAUDE.md points
          // at something the user owns (a shared team file, a dotfiles repo);
          // unlinking it removes their link, and the "file we created" rationale
          // does not apply. Write the stripped text through the link instead.
          if (after.trim() === '' && !fs.lstatSync(cPath).isSymbolicLink()) {
            fs.unlinkSync(cPath);
            claudeMdRemoved = true;
          } else {
            writeFileAtomic(cPath, after, { followLink: true });
          }
          // Only after the write lands: `blockPruned` drives the "De-blocked"
          // line, and claiming it for a write that threw is the same class of
          // false success this batch keeps finding.
          blockPruned = true;
        } catch { claudeMdUnwritable = true; }
      }
    }
  }

  // Also sweep any legacy memory-dir remnants (uninstall before auto-migration ran).
  const migrated = migrateLegacyMemoryDir({ cwd, home });

  const registryUpdated = removeAdopted(effectiveCwd, home);
  return {
    ok: true, fileRemoved, blockPruned, claudeMdRemoved,
    claudeMdUnreadable, claudeMdUnwritable, registryUpdated,
    target: dPath, claudeMdPath: cPath, migrated,
  };
}

function formatResult(action, result) {
  if (!result.ok && result.reason === 'windows-not-supported') {
    return '[code-graph] adopt/unadopt are POSIX-only on this build. ' +
           'Edit CLAUDE.md manually to opt in.';
  }
  if (action === 'adopt') {
    if (!result.ok) {
      if (result.reason === 'not-a-project') {
        return `[code-graph] Not a project root: ${result.cwd}\n` +
               '  No project marker (.git, Cargo.toml, package.json, pyproject.toml, ...).\n' +
               '  cd into a real project before running adopt.';
      }
      if (result.reason === 'no-template') {
        return `[code-graph] Template missing: ${result.template}`;
      }
      // Name the file and the OS error: "adopt failed: claude-md-unreadable" is
      // not something a user can act on, and this is the arm a root-owned
      // CLAUDE.md lands in.
      if (result.reason === 'claude-md-unreadable' || result.reason === 'claude-md-unwritable') {
        const what = result.reason === 'claude-md-unreadable' ? 'read' : 'write';
        return `[code-graph] Cannot ${what} ${result.claudeMdPath} (${result.error || 'unknown error'}).\n` +
               `  Nothing was changed. Fix its permissions (or move it aside) and re-run;\n` +
               '  opt out entirely with CODE_GRAPH_NO_AUTO_ADOPT=1.';
      }
      if (result.reason === 'detail-unwritable') {
        return `[code-graph] Cannot write ${result.detailPath} (${result.error || 'unknown error'}). Nothing was changed.`;
      }
      return `[code-graph] adopt failed: ${result.reason || 'unknown'}`;
    }
    const lines = [];
    if (result.created) lines.push(`[code-graph] Created → ${result.claudeMdPath} (code-graph block)`);
    else if (result.claudeMdWritten) lines.push(`[code-graph] Updated code-graph block → ${result.claudeMdPath}`);
    else lines.push('[code-graph] CLAUDE.md block already up-to-date — no write');
    if (result.healed) lines.push('[code-graph] Healed a malformed prior block.');
    lines.push(`[code-graph] Detail table → ${result.detailPath}`);
    lines.push('[code-graph] CLAUDE.md is git-tracked — commit the block, or gitignore');
    lines.push('[code-graph]   .claude/plugin_code_graph_mcp.md (a generated copy) as you prefer.');
    lines.push('[code-graph] Reverse:  code-graph-mcp unadopt');
    lines.push('[code-graph] Opt out:  CODE_GRAPH_NO_AUTO_ADOPT=1 in ~/.claude/settings.json env');
    return lines.join('\n');
  }
  if (action === 'unadopt') {
    const lines = [];
    if (result.claudeMdRemoved) lines.push(`[code-graph] Removed → ${result.claudeMdPath} (was code-graph-only)`);
    else if (result.blockPruned) lines.push(`[code-graph] De-blocked → ${result.claudeMdPath}`);
    if (result.fileRemoved) lines.push(`[code-graph] Removed → ${result.target}`);
    if (result.claudeMdUnreadable || result.claudeMdUnwritable) {
      lines.push(`[code-graph] Could not ${result.claudeMdUnreadable ? 'read' : 'write'} ${result.claudeMdPath} — ` +
                 'the managed block (if any) is still there. Fix its permissions and re-run.');
    }
    const m = result.migrated || {};
    if (m.memoryIndexPruned || m.legacyDetailRemoved) {
      lines.push('[code-graph] Cleaned legacy memory-dir artifacts.');
    }
    if (!result.blockPruned && !result.fileRemoved && !result.claudeMdRemoved &&
        !result.claudeMdUnreadable && !result.claudeMdUnwritable &&
        !(m.memoryIndexPruned || m.legacyDetailRemoved)) {
      lines.push('[code-graph] Nothing to unadopt');
    }
    return lines.join('\n');
  }
  return '';
}

if (require.main === module) {
  const action = process.argv[2] === 'unadopt' ? 'unadopt' : 'adopt';
  const result = action === 'unadopt' ? unadopt() : adopt();
  process.stdout.write(formatResult(action, result) + '\n');
  process.exit(result.ok === false ? 1 : 0);
}

module.exports = {
  adopt, unadopt, memoryDir, formatResult, stripSentinelBlock,
  readAdoptedProjects, recordAdopted, removeAdopted, adoptedRegistryFile,
  isAdopted, isPluginModeInstall, maybeAutoAdopt, needsRefresh, isProjectRoot,
  detectProjectType, buildBlock, buildTriggerRows, migrateLegacyMemoryDir,
  claudeMdPath, detailDir, detailPath,
  extractCargoRuntimeDeps, extractPyRuntimeDeps, extractGoDirectRequires,
  SENTINEL_BEGIN, SENTINEL_END, SENTINEL_BEGIN_SRC, SENTINEL_VERSION,
  MANAGED_BY, TEMPLATE_PATH, TARGET_NAME,
  PROJECT_MARKERS, PROJECT_TYPES, isNonProjectCwd,
};
