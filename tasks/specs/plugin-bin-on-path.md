---
status: draft
revision: 1
---

# Issue #41's actual fix: put the name on PATH instead of apologising for it

## Goal

`code-graph-mcp <subcommand>`, typed exactly as every steering surface prints
it, runs on a plugin-only install. Not "runs after the reader substitutes a
path" — runs.

## Evidence (verified at `63b63d2`, v0.143.0)

- **The defect is still live.** In this repo, with the plugin installed:
  `command -v code-graph-mcp` → nothing. The reported symptom
  (`/bin/bash: line 1: code-graph-mcp: command not found` on
  `code-graph-mcp grep …`) reproduces on an unmodified HEAD.
- **Claude Code already opens the door.** `claude` 2.1.263 builds, for every
  enabled non-builtin plugin, `join(plugin.path, "bin")` and puts it on the
  Bash tool's PATH (function `P6n` in the bundle; the entry is added whether or
  not the directory exists, and is dropped only if the path contains shell
  metacharacters). This session's PATH carries
  `…/plugins/cache/code-graph-mcp/code-graph-mcp/0.142.0/bin` — and that
  directory does not exist. Of 12 installed plugins only `claudemd` ships a
  `bin/` at all.
- **v0.141.0 and v0.142.0 fixed the wrong layer.** Both releases rewrote
  printed strings — `adopt.js buildBlock`'s fallback line, the SessionStart
  announcement, `adopt.js`'s own result printer, the uninstall sweep's notice,
  `templates/plugin_code_graph_mcp.md`. The bare name was never made runnable.
- **One surface was never touched at all**: `src/mcp/server/mod.rs:34`
  `INSTRUCTIONS_QUIET` and `:42-52` `INSTRUCTIONS_NOISY`, the MCP `instructions`
  field. It is injected into the system prompt of every session in every adopted
  project, it opens with ``Fastest path is the CLI via Bash … `code-graph-mcp
  callgraph X` ``, and it carries no fallback. It is the highest-traffic copy of
  the unrunnable instruction and it survived both fixes.
- **A launcher on PATH collides with binary discovery.**
  `find-binary.js:193 isNativeBinary()` decides "is this the native binary?" by
  `path.basename(fs.realpathSync(candidate)) === BINARY_NAME` — a name test, by
  design (it is what rejects the npm shim, whose realpath basename is
  `cli.js`). A launcher that must be *named* `code-graph-mcp` passes it. Two
  tiers then offer the launcher as the binary: the PATH tier (`:554`, `which
  code-graph-mcp`) and the bundled-`bin/` tier (`:520-528`), which
  `mcp-launcher.js:19` points at the plugin root. `findBinary()` would return
  the launcher, the launcher would exec the launcher.

## Approach

1. Ship `claude-plugin/bin/code-graph-mcp` (mode 755) plus a `.cmd` sibling for
   Windows PATHEXT. Claude Code puts that directory on PATH; the name resolves.
2. Both command-name entry points — the npm bin (`bin/cli.js`) and the new
   plugin launcher — dispatch through one module,
   `claude-plugin/scripts/cli-entry.js`. Two printers of the same `unadopt`
   string already drifted once (v0.142.0); two dispatchers would be the same
   defect with more surface.
3. `isNativeBinary()` rejects anything whose realpath sits in the plugin's own
   `bin/`. One predicate, so every tier that consults it — PATH, bundled,
   `isCachedBinaryFresh` on a poisoned cache entry — is covered at once.
4. The MCP `instructions` field gains the fallback sentence the other three
   surfaces have carried since v0.141.0.
5. (r2) `doctor` joins `adopt` / `unadopt` / `uninstall` as a JS-intercepted
   subcommand, through the exported `runDoctorCli` the other two doctor entry
   points already share. Forwarding it to the binary is what leaves it broken on
   the install this whole spec is about.

## Non-goals

- **Do not retreat from CLI-first.** Unchanged and unrelated; the ranking was
  measured (v0.49) and the defect is the invocation string.
- **Do not remove the `~/.cache/code-graph/bin/…` fallback line** from the
  CLAUDE.md block or the detail doc. The PATH entry is dropped by Claude Code
  when the plugin path contains shell metacharacters, and older Claude Code
  versions do not add it at all. Removing the line would also rewrite the
  managed block in every adopted project for no user-visible gain.
- **Do not add a magic-byte check to `isNativeBinary`.** It would be the more
  general guard and it would break the fixture strategy of the whole
  `find-binary.test.js` suite, which plants text files (`'stub'`) and copies of
  `process.execPath` named `code-graph-mcp`. The launcher lives at one known
  path; test that path.
- Not shipping a `code-graph` alias launcher. The npm package has both bin
  names; no steering surface spends the short one.

## Constraints

- The launcher must not set `_FIND_BINARY_ROOT`. `find-binary.js` already
  derives the right root from its own `__dirname` in all three install shapes,
  and pointing the variable at the plugin root would add the launcher's own
  directory to the bundled-`bin/` tier.
- Executable bit must survive packaging. Both channels preserve mode: the
  marketplace source is the `./claude-plugin` directory of a git clone, and
  `release.yml:604` is `tar czf … claude-plugin`.
- No new dependency, no new subprocess on the hook path. `isNativeBinary` gains
  one memoized `realpathSync` of a constant directory.
- L3 by core §2: LLM-visible metadata (MCP `instructions`) plus a new
  executable shipped in a released artifact.

## Success criteria

1. `claude-plugin/bin/code-graph-mcp` exists, is mode-executable, and running it
   dispatches the JS-only subcommands without a native binary present.
2. `createVersionGate(null).consider(<plugin>/bin/code-graph-mcp)` is `null` —
   asserted after asserting the launcher exists, so the guard cannot pass
   vacuously on a missing file.
3. `bin/cli.js` and the launcher produce identical behaviour for
   `adopt --help`, `unadopt --help`, and unknown-flag rejection, because they
   are the same code.
4. Both `INSTRUCTIONS_*` name a runnable spelling, and
   `tests/doc_cli_alignment.rs` still passes on them.
5. `INSTRUCTIONS_NOISY` stays inside the 1500-byte compile-time budget
   (951 bytes at r1; 549 free).

## Open questions

- None blocking. One judgment recorded: the launcher is a Node script rather
  than a symlink to the cached binary, because the cached binary is downloaded
  at runtime into a directory the plugin does not own at install time, and a
  dangling symlink on PATH is a worse failure than a launcher that can explain
  itself.

# Change log

- r1 (2026-09-08): drafted from the field report at `63b63d2`, after confirming
  the PATH mechanism in the Claude Code bundle.
- r2 (2026-09-08): `doctor` folded in. Testing it honestly needed a real
  plugin-only sandbox — the plugin copied where no `Cargo.toml` sits above it,
  HOME redirected, and a stub as the only reachable binary — because in this
  repo discovery finds `target/release/`, which CAN re-exec `doctor.js`, so the
  obvious test passes at every value of the bug. Two repo-wide guards caught the
  sandbox before review did: the HOME/USERPROFILE two-name rule and
  `js_test_files_neutralize_claude_config_dir`. Both were right.
