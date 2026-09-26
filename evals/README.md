# Plugin evals

Does the plugin make Claude answer structural questions better or more
cheaply? `claude plugin eval` runs each case with the plugin loaded and again
with no plugin at all. The difference, Δ, is what the plugin contributed.

```bash
evals/run.sh --case callers-direct --runs 1     # one trial, about $0.25
evals/run.sh --runs 3 -j 3 --max-cost-usd 30    # the whole suite
```

Every run is a paid model call on your account. Results go to
`evals/results/<timestamp>/`, which is gitignored. Always start the suite
through `run.sh`: it builds the environment described below, and a bare
`claude plugin eval` measures something else.

## Cases

Every case runs on the same workspace: this repo's `src/` at commit `6f0f6a2`
(134 Rust files), committed as a one-commit git repo (`_fixture/scaffold.sh`).
The answers are pinned to that snapshot. For the first eight cases, each answer
was taken from the index and then checked with grep and by reading the code.
The two `hard` cases have set answers too large to check that way, so their
answers come from rust-analyzer's SCIP index instead
(`_fixture/derive_gold.py`, which applies the SCIP oracle's call-site rules),
and a case cannot reward the plugin for sharing its own blind spots. The one
exception found by hand, `map_incoming_ref`, is passed as a function value, so
the call-site rule misses it. It became a grader that fails any reply listing
it.

| Case | Question shape | Why it is in the suite |
|---|---|---|
| `callers-direct` | direct callers of a unique name | grep alone can answer it |
| `callers-transitive` | every function that reaches X, up to the entry points | multi-hop. The index misses the two path-qualified calls in `main.rs` (the SCIP oracle's path-call recall gap), so the right answer needs them from another source |
| `impact-signature` | production files to update after a signature change | tests must be excluded |
| `rename-audit` | all 17 files that reference a struct | completeness; `grep -lw` also gets it |
| `concept-locate` | find a function from a description of what it does | the question names no symbol |
| `prod-or-test` | reached from production, or only from tests? | two hops through a private wrapper |
| `subagent-callers` | callers, delegated to an Explore subagent | the baseline for steering subagents (Explore skips CLAUDE.md) |
| `control-constant` | value and location of a constant | control: expect Δ ≈ 0; compare turns and cost to see the plugin's overhead |
| `transitive-wide` (hard) | all 19 production functions up to 7 hops above a helper | wide multi-hop closure; one grader per function (18; `main` is too common a word), so the score is recall |
| `never-called` (hard) | the 9 functions in `src/storage/queries/` no production code calls | needs a check per function; one grader per function, plus one against the function-value trap |

Each of the first eight cases has two graders:

- `answer`: a regex over the final reply, written as one lookahead per required
  item, so the reply fails if any item is missing. This is the scored grader.
  The all-or-nothing score is why the first baseline hit the ceiling. The
  `hard` cases grade each item separately instead (`item-*.md`), so a reply
  that finds 12 of 18 scores 0.67.
- `used-code-graph`: a regex over the trace that matches either a CLI query
  (`"command":"… code-graph-mcp callgraph …"`) or an MCP tool call. It is
  marked `arm: with-only`, so it reports whether the plugin was used and never
  counts toward the score.

Correctness alone can hide the plugin's value, because both arms can reach the
same answer at different cost. Read the per-run `turns` and `costUsd` in
`aggregate-result.json` next to Δ.

## Baseline (v0.157.0, 2026-09-26)

`evals/run.sh --runs 3 -j 3 --max-cost-usd 30`, Claude Code 2.1.283, the
account's default model. The model was not pinned, and the result file does
not record it; pass `--model` next time so runs can be compared. Total $7.30,
300 s, no partial or errored runs.

| Case | Score W / W/O | Cost/run W / W/O | Turns W / W/O | Used code-graph (W) |
|---|---|---|---|---|
| callers-direct | 1.00 / 1.00 | $0.104 / $0.124 | 2.0 / 4.3 | 3/3 |
| callers-transitive | 1.00 / 1.00 | $0.177 / $0.213 | 5.3 / 6.7 | 3/3 |
| concept-locate | 1.00 / 1.00 | $0.126 / $0.113 | 4.0 / 4.0 | 0/3 |
| control-constant | 1.00 / 1.00 | $0.090 / $0.079 | 2.0 / 2.0 | 0/3 |
| impact-signature | 1.00 / 1.00 | $0.115 / $0.110 | 3.0 / 3.3 | 3/3 |
| prod-or-test | 1.00 / 1.00 | $0.195 / $0.294 | 5.7 / 13.7 | 3/3 |
| rename-audit | 1.00 / 1.00 | $0.135 / $0.130 | 2.3 / 2.7 | 0/3 |
| subagent-callers | 1.00 / 1.00 | $0.207 / $0.221 | 2.3 / 3.0 | 2/3 |
| **all 24 runs per arm** | Δ 0.00 | $3.45 / $3.85 | 80 / 119 | 14/24 |

How to read it:

- **Correctness is at the ceiling.** Every run in both arms is right, so Δ is
  0.00 everywhere and cannot gate a change. The fixture is 134 files, and the
  default model finds these answers with grep. Cases that separate the arms
  need a larger corpus, or questions whose answer grep cannot enumerate.
- **The signal is cost and turns.** Summed over all 24 runs per arm, the plugin
  arm used 80 turns against 119 (−33%), $3.45 against $3.85 (−10%), and 400 s
  of run time against 482 s (−17%). The gap is concentrated in the multi-hop
  cases: `prod-or-test` took 5.7 turns against 13.7. With n = 3 per arm, a
  per-case difference under about one turn is noise.
- **Where the plugin went unused it cost a little**: +$0.013, +$0.011 and
  +$0.005 per run on the three cases where it was used 0/3 times
  (concept-locate, control-constant, rename-audit). That is the price of the
  steering text in context. Nothing routed the concept search or the
  all-references question to the plugin.

### Hard cases (v0.157.0, 2026-09-26, `--model claude-opus-5-5`)

`evals/run.sh --tag hard --runs 3 -j 3 --model claude-opus-5-5`: $4.80,
284 s, no partial or errored runs.

| Case | Score W / W/O (per run) | Cost/run W / W/O | Turns/run W / W/O | Used code-graph (W) |
|---|---|---|---|---|
| transitive-wide | 1.00, 1.00, 1.00 / 1.00, 0.89, 1.00 (Δ +0.04) | $0.392 / $0.417 | 10.0 / 12.7 | 3/3 |
| never-called | 0.90, 1.00, 1.00 / 1.00, 0.90, 1.00 (Δ 0.00) | $0.397 / $0.393 | 10.0 / 10.0 | 0/3 |

- Partial credit did not move the ceiling much: without the plugin, the model
  still recovered 18 of 18 closure members in 2 of 3 runs, by grep alone. The
  one miss (`dispatch_tool`, `refresh_result_set`) is the only correctness gap
  the plugin closed in the whole suite.
- In `never-called` the plugin arm never used code-graph. The model checked
  each function by grep, and `dead-code` was never reached for. Each arm lost
  `insert_node` once.
- **What this says about the gate:** on a 134-file corpus the model gets
  structural answers right without the plugin, so a correctness Δ cannot tell
  a better plugin from a worse one. Turns and cost per run can. To make
  correctness separate as well, the corpus has to be large enough that a grep
  walk of a closure runs out of turns or budget. That is the next step if Δ is
  to become a gate.

## What `run.sh` sets up, and why

Each eval run gets a temporary HOME, an empty workspace and a fresh Claude Code
config, and the agent's Bash runs in an OS sandbox. With a plain
`claude plugin eval`, the with-plugin arm differs from a real session in
several ways. `run.sh` closes the ones that would change what is measured:

1. **The plugin is loaded from a staged copy at a plugin-mode path**
   (`/var/tmp/code-graph-eval/stage/.claude/plugins/code-graph-mcp`), with the
   suite copied inside it. From the checkout, SessionStart does not adopt the
   project's CLAUDE.md, because `adopt.js isPluginModeInstall` keys on
   `/.claude/plugins/`. Every launcher would also resolve `target/release` as a
   dev build.
2. **The MCP server is started** (`--allow-real-servers`, plus a grant for its
   tools). The eval never starts a plugin's MCP server by default, and a real
   session always does. Without it, the server's `instructions` field and the
   index it builds at startup are both absent.
3. **`PATH` is rebuilt from scratch, with a shim standing in for the plugin's
   `bin/` entry.** The eval does not put `<plugin-root>/bin` on the Bash PATH
   (a normal `claude -p --plugin-dir` session does). It also passes this
   shell's PATH to BOTH arms, so an installed `code-graph-mcp` would leak into
   the no-plugin arm. The shim in `~/.cache/code-graph-eval/bin` runs the
   plugin's launcher only when `~/.cache/code-graph/install-manifest.json`
   exists. The plugin's SessionStart hook writes that file, so the shim works
   only in the with-plugin arm and answers 127 elsewhere. The directory is under
   `$HOME` because the sandbox can read a PATH entry only there. `node` is
   copied into it because the machine's `node` is a symlink into a directory
   the sandbox cannot read.
4. **The binary is placed where a real install keeps it**
   (`~/.cache/code-graph/bin/`, by the scaffold), in both arms. Nothing in the
   no-plugin arm's PATH or context points at it.

5. **Leftovers are reaped** (`_fixture/reap.sh`). SessionStart starts
   `auto-update.js check` detached, so it outlives the run. Once the eval has
   deleted the temporary HOME, the check finds the binary missing and downloads
   the release into it again. Each run then left a
   `/tmp/claude-eval-XXXXXX/home/` of about 41 MB, and the first full suite
   leaked 15 of them into a tmpfs. After the eval exits, `run.sh` waits for
   those processes and removes only the directories that are new and hold
   nothing but `home/`, so `--keep-temp` output survives. During a run the
   check leaves the binary alone, because the scaffold's copy already carries
   the latest release's version. A binary under test that is OLDER than the
   latest release would be swapped for the release mid-run.

Known remaining differences:

- No embedding model is installed, so search is FTS-only (the CLI is FTS-only
  anyway; MCP `semantic_code_search` loses its vector leg).
- **Every run starts cold, so the hooks miss the first few seconds.** The
  PreToolUse/PostToolUse hooks are written to settings.json at SessionStart,
  and they do take effect in that same session: a probe that ran
  `sleep 8` and then `grep -rn X src/` got the grep rewritten, and
  `.code-graph/recommendations.jsonl` recorded `deny … delivery:"rewrite"`.
  But every hook stays silent until `.code-graph/index.db` exists, and the MCP
  server builds it only after startup. A grep in the first seconds of a run
  runs unhooked. A user's second session already has the index, so the eval
  under-measures the steady state here.
- The grep hook only acts on a source path written with a trailing slash:
  `grep -rn X src/` is rewritten, while `grep -rn X src`, `grep -rn X .` and a
  bare `rg X` are not (`SRC_PATH` in `pre-grep-guide.js`). That is product
  behavior, not a harness gap, but it decides whether the hook fires in a run.
  Over 1,655 grep-like Bash commands in this machine's transcripts, 80 (4.8%)
  had the bare-directory or `.` shape.
- This is a first session: the CLAUDE.md block is written during SessionStart.
  In the trial the agent ran `code-graph-mcp … || ~/.cache/code-graph/bin/code-graph-mcp …`,
  which is the fallback that both the CLAUDE.md block and the MCP
  `instructions` field spell out. So at least one of them reached the context;
  the trial does not show which one.

## Machine prerequisites (Linux)

The Bash sandbox needs `bubblewrap` and `socat`. On Ubuntu 24.04+ with
`kernel.apparmor_restrict_unprivileged_userns = 1`, it also needs the AppArmor
profile from the Claude Code sandboxing docs. The profile Ubuntu ships,
`bwrap-userns-restrict`, lets bwrap create a namespace but not the nested one
the sandbox uses for seccomp. Every command then fails with
`apply-seccomp: write /proc/self/setgroups … Permission denied`, and the case
scores 0 for a reason that has nothing to do with the plugin.
