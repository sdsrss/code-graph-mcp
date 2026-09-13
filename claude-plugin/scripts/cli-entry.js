'use strict';
/**
 * The dispatcher behind every `code-graph-mcp <subcommand>` a user can type.
 *
 * Two files put that name on a PATH: the npm package's bin entry
 * (`bin/cli.js`, from `npm i -g @sdsrs/code-graph`) and this plugin's launcher
 * (`claude-plugin/bin/code-graph-mcp`, which Claude Code puts on PATH for every
 * enabled plugin). Both are three lines that call `main()` here.
 *
 * Why one module and not two entry points: v0.142.0 shipped a fix for two
 * printers of the same `unadopt` string that had drifted apart, and the release
 * before it shipped a fix for the first of them. Two dispatchers for the same
 * six subcommands would be that defect with a much larger surface — `adopt`
 * writes the user's CLAUDE.md and `uninstall` deletes their cache, so a drift
 * between them is a drift between two destructive paths.
 *
 * `adopt` / `unadopt` / `uninstall` are intercepted here rather than forwarded:
 * they write `<cwd>/CLAUDE.md`, `<cwd>/.claude/` and `~/.cache/code-graph`, and
 * have no counterpart in the Rust binary. Everything else is forwarded verbatim.
 */
const { spawn } = require('child_process');
const path = require('path');
const { hidden } = require('./proc-opts');

/**
 * Reject unknown flags on an intercepted subcommand BEFORE doing its work.
 *
 * `--help` was already guarded, but every OTHER token was ignored, so
 * `code-graph-mcp adopt --helpp` ran adopt and wrote the user's CLAUDE.md — a
 * typo away from the very side effect the --help guard exists to prevent. This
 * is the fourth entry point onto the same "ignore what you don't recognise"
 * idiom (doctor.js, lifecycle.js doctor, src/main.rs were the first three); it
 * is the surface both PATH names reach, so it is the one most users hit.
 */
function rejectUnknownFlags(argv, name, known) {
  const unknown = argv.slice(3).filter((a) => !known.has(a));
  if (unknown.length) {
    process.stderr.write(
      `code-graph-mcp ${name}: unknown argument(s): ${unknown.join(" ")}\n` +
      `Run \`code-graph-mcp ${name} --help\` for usage.\n`);
    process.exit(2);
  }
}

/**
 * @param {object}   [opts]
 * @param {string[]} [opts.argv]           process.argv-shaped: [node, entry, sub, ...]
 * @param {string}   [opts.findBinaryRoot] package root for find-binary's dev-mode and
 *   bundled-`bin/` tiers. The npm entry passes its package root. The plugin
 *   launcher passes NOTHING on purpose: find-binary derives the right root from
 *   its own `__dirname` in every install shape, and pointing this at the plugin
 *   root would add the launcher's own directory to the bundled-binary tier.
 */
function main({ argv = process.argv, findBinaryRoot = null } = {}) {
  if (findBinaryRoot) process.env._FIND_BINARY_ROOT = path.resolve(findBinaryRoot);

  const sub = argv[2];

  if (sub === "adopt" || sub === "unadopt") {
    // `--help`/`-h` must be side-effect-free: adopt() writes the managed block
    // into CLAUDE.md, unadopt() removes it. The Rust binary guards this for
    // direct invocation, but both PATH names route through this wrapper, which
    // intercepts adopt/unadopt *before* the binary — so the guard must be
    // repeated here, or `code-graph-mcp adopt --help` adopts the project (the
    // common new-user path).
    if (argv.slice(3).some((a) => a === "--help" || a === "-h")) {
      process.stdout.write(sub === "adopt"
        // Kept in sync with src/main.rs's adopt/unadopt help. This text described
        // the pre-v0.74 scheme (a sentinel in the ~/.claude memory dir) for three
        // releases after the target moved to the project's own CLAUDE.md, so npm
        // users were told this command edits a file it has not touched since.
        ? "code-graph-mcp adopt — install the code-graph steering block into the project CLAUDE.md\n\n" +
          "USAGE:\n    code-graph-mcp adopt\n\n" +
          "Writes a sentinel-wrapped managed block into <cwd>/CLAUDE.md plus a\n" +
          "<cwd>/.claude/plugin_code_graph_mcp.md detail doc, so Claude Code loads the\n" +
          "decision table each session. Run `code-graph-mcp unadopt` to remove it.\n"
        : "code-graph-mcp unadopt — remove the code-graph steering block\n\n" +
          "USAGE:\n    code-graph-mcp unadopt\n\n" +
          "Reverses `code-graph-mcp adopt`: strips the managed block from\n" +
          "<cwd>/CLAUDE.md and deletes <cwd>/.claude/plugin_code_graph_mcp.md.\n" +
          "User content outside the sentinel is kept.\n");
      process.exit(0);
    }
    rejectUnknownFlags(argv, sub, new Set(["--help", "-h"]));
    const { adopt, unadopt, formatResult } = require("./adopt");
    const result = sub === "unadopt" ? unadopt() : adopt();
    process.stdout.write(formatResult(sub, result) + "\n");
    process.exit(result.ok === false ? 1 : 0);
  }

  // Full local teardown (restore prior statusline, strip code-graph hooks from
  // settings.json, delete ~/.cache/code-graph, and unadopt the CURRENT project).
  // Node-only; no Rust counterpart. Claude Code's `/plugin uninstall` fires no
  // uninstall hook, so this is the user's one-shot CLI teardown. Guard `--help`
  // before the destructive work (same discipline as adopt/unadopt above).
  if (sub === "uninstall") {
    if (argv.slice(3).some((a) => a === "--help" || a === "-h")) {
      process.stdout.write(
        "code-graph-mcp uninstall — remove code-graph config + cache from this machine\n\n" +
        "USAGE:\n    code-graph-mcp uninstall [--unadopt-all] [--purge-global]\n\n" +
        "Restores your prior statusline, strips code-graph hooks from settings.json,\n" +
        "deletes ~/.cache/code-graph, and removes this project's CLAUDE.md adoption\n" +
        "block. --unadopt-all also removes the managed block + detail file from every\n" +
        "registered adopted project; --purge-global removes the globally-installed\n" +
        "@sdsrs npm packages even without the plugin-install marker.\n\n" +
        "This also removes Claude Code's own registration, so `claude plugin uninstall\n" +
        "code-graph-mcp` afterwards answers \"not found\" — expected. Only a Claude Code\n" +
        "session still listing the plugin needs `/plugin uninstall code-graph-mcp`.\n");
      process.exit(0);
    }
    rejectUnknownFlags(argv, "uninstall", new Set(["--help", "-h", "--unadopt-all", "--purge-global"]));
    const lifecycle = require("./lifecycle");
    const { unadopt } = require("./adopt");
    // Unadopt THIS project BEFORE the teardown, not after.
    //
    // `lifecycle.removeCacheResidue` (step 6) holds two rules: PRESERVE a
    // non-empty adopted-projects registry — those projects still carry a managed
    // block someone has to be able to find (JS-17) — and never re-create
    // CACHE_DIR merely to hold `[]`, which its own comment calls "just new
    // residue". Unadopting afterwards produced exactly that `[]`, one step too
    // late for anything to sweep it, so the file step 6 had carefully preserved
    // for us became the residue step 6 exists to prevent. Unadopt first and the
    // registry is already empty when step 6 reads it, so the whole cache
    // directory goes. It is also why that comment describes "the normal
    // SessionStart teardown order, which unadopts first" — this was the one
    // caller that did not.
    //
    // Deliberate knock-on: `r.adoptedProjects` (captured at step 5.5) and the
    // `--unadopt-all` sweep no longer see this project, so it is reported once,
    // on the `this project unadopted=` line, instead of being counted twice.
    // `otherAdopted` below already filtered `process.cwd()` out either way.
    let ua = { ok: false };
    try { ua = unadopt(); } catch { /* best-effort — the teardown below still runs */ }
    const projectUnadopted = !!(ua && (ua.blockPruned || ua.fileRemoved || ua.claudeMdRemoved));
    const r = lifecycle.uninstall({
      purgeGlobal: argv.slice(3).includes("--purge-global"),
      unadoptAll: argv.slice(3).includes("--unadopt-all"),
    });
    let out =
      `Uninstalled code-graph-mcp | settings cleaned=${r.settingsChanged}` +
      ` | this project unadopted=${projectUnadopted}\n`;
    if (r.globalPkgsRemoved.length) {
      out += `  Removed global npm package(s): ${r.globalPkgsRemoved.join(", ")}\n`;
    }
    if (r.globalPkgsRemaining.length) {
      out += `  Global npm package(s) still installed: ${r.globalPkgsRemaining.join(", ")}\n` +
        `    Remove with: npm uninstall -g ${r.globalPkgsRemaining.join(" ")}` +
        (r.pluginInstalledGlobals ? "\n" : "   (or re-run with --purge-global)\n");
    }
    if (r.unadopted.length) {
      const cleaned = r.unadopted.filter((u) => u.cleaned).length;
      out += `  Unadopted ${cleaned}/${r.unadopted.length} registered project(s) (--unadopt-all).\n`;
    }
    const otherAdopted = r.adoptedProjects.filter((p) => p !== process.cwd());
    if (otherAdopted.length) {
      out += "  Other adopted project(s) — re-run with --unadopt-all, or in each:" +
        " `code-graph-mcp unadopt` + `rm -rf .code-graph`\n" +
        otherAdopted.map((p) => `    ${p}\n`).join("");
    }
    for (const line of lifecycle.POST_TEARDOWN_UI_NOTE) out += `  ${line}\n`;
    process.stdout.write(out);
    process.exit(0);
  }

  // `doctor` is the third JS-dispatched subcommand, and the one that used to be
  // reached the long way round: forwarded to the binary, which re-execs a
  // `doctor.js` it looks for beside itself. That works for a source build and
  // for the npm package; it does NOT work for the install this launcher exists
  // for, where the binary is the downloaded one in `~/.cache/code-graph/bin/`
  // and answers `doctor.js not found. Looked in: …`. The script is right here,
  // so ask it directly.
  //
  // `runDoctorCli` rather than a private re-parse: it is the function
  // `node doctor.js …` and `node lifecycle.js doctor …` already share, and it
  // exists as one function because the first version of doctor's unknown-flag
  // guard lived in only one of them and a typo'd flag kept running the repair
  // pass on the other. A third entry point re-implementing the parse would
  // restore exactly that.
  if (sub === "doctor") {
    const { runDoctorCli } = require("./doctor");
    process.exit(runDoctorCli(argv.slice(3)));
  }

  const { findBinary, unsupportedPlatformHint } = require("./find-binary");

  const binary = findBinary();

  if (!binary) {
    const hint = unsupportedPlatformHint();
    console.error(
      "Error: code-graph-mcp binary not found.\n\n" +
      (hint ? hint + "\n\n" : "") +
      "To install:\n" +
      "  npm install -g @sdsrs/code-graph\n\n" +
      "To build from source:\n" +
      "  cargo install code-graph-mcp --features embed-model\n"
    );
    process.exit(1);
  }

  // Forward stdio so MCP JSON-RPC (`serve`) and interactive output both work.
  const child = spawn(binary, argv.slice(2), hidden({
    stdio: "inherit",
    env: process.env,
  }));

  child.on("error", (err) => {
    console.error(`Failed to start code-graph-mcp: ${err.message}`);
    // A glibc binary installed on musl (older npm ignores the `libc` field) is present
    // but fails to exec — surface the actionable platform hint instead of a bare error.
    const hint = unsupportedPlatformHint();
    if (hint) console.error("\n" + hint);
    process.exit(1);
  });

  child.on("exit", (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
    } else {
      process.exit(code ?? 1);
    }
  });
}

module.exports = { main, rejectUnknownFlags };
