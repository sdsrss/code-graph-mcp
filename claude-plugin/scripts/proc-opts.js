'use strict';
/**
 * Child-process option defaults shared by every spawn/exec in this plugin.
 *
 * Windows creates a NEW visible console window for every console-subsystem
 * child whose parent has no console of its own — and none of our parents do:
 * the MCP server (`node mcp-launcher.js`), the hooks and the statusline are all
 * launched hidden by Claude Code. Node's `windowsHide` defaults to `false` on
 * EVERY child_process API (spawn/spawnSync/exec/execSync/execFile/execFileSync),
 * so each `where` / `curl` / `tar` / `npm` child flashed a console window for
 * ~1s and stole keyboard focus. Reported as 5–7 flashes per session start
 * (issue #40); the auto-update treadmill fixed alongside it made that per
 * session, forever.
 *
 * `windowsHide: true` maps to CREATE_NO_WINDOW, which only stops the child from
 * ALLOCATING a console — inherited stdio handles still work, so an interactive
 * `doctor` run in a real terminal is unaffected. No-op on non-Windows.
 *
 * `killSignal` is deliberately NOT defaulted here. Node's `timeout` option
 * sends SIGTERM and then WAITS, so a child that traps SIGTERM makes the
 * timeout unreachable (audit 2026-08-16 P1-17: one deaf third-party statusline
 * provider blanked the status line on every frame). The two statusline call
 * sites pass `killSignal: 'SIGKILL'` themselves — they run UNTRUSTED provider
 * commands / a possibly-wedged binary on the render hot path, and nothing
 * there shuts down gracefully at timeout anyway. It is not a global default
 * because our other timed children DO need SIGTERM's grace: a timed-out
 * `git pull` hard-killed mid-write leaves `.git/index.lock` behind and every
 * later marketplace refresh then fails silently; npm has equivalent lock
 * files (batch review of the P1-17 fix). New call sites that run untrusted or
 * hang-prone children with a timeout should opt in the same way.
 * (Caveat SIGKILL does not fix: it reaches the direct child only. A grandchild
 * holding the same stdout pipe can still stall a *Sync call until it exits.)
 *
 * Every child_process call site under claude-plugin/scripts/ must route through
 * here (or set windowsHide itself); `windows-hide.test.js` fails the build on a
 * new call site that doesn't.
 */
function hidden(opts = {}) {
  return { windowsHide: true, ...opts };
}

module.exports = { hidden };
