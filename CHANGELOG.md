# Changelog

## Unreleased

### Four MCP tools published a schema that required nothing, and rejected calls that gave nothing (CON-13)

`find_references` declares seven optional properties and no `required` array,
while `refs.rs:35` rejects any call carrying neither `symbol_name` nor
`node_id`. The client builds the model's picture of a tool from that schema, so
the model was being told every argument is optional by the same surface that
then answered with an error it had no way to anticipate.

The audit named one tool and reported that "the six siblings all declare
`required`". Both halves were off, and checking is what found it:

- **Five** declare it, not six. `get_call_graph` had the identical omission
  (`symbol_name` or `route_path`, `callgraph.rs:95`).
- Two more — `get_ast_node` and `ast_search` — declare `"required": []`, which
  the finding would count as compliant. In JSON Schema an empty `required` is
  indistinguishable from no `required` at all, so those two were making exactly
  the same silent promise. That also rules out the obvious fix: adding
  `"required": []` to the two tools missing it would have changed nothing while
  looking like a repair.

All four now publish the disjunction their handler enforces:

```json
"anyOf": [{ "required": ["symbol_name"] }, { "required": ["node_id"] }]
```

Property descriptions are untouched — the model's routing text is byte-identical
to 0.129.0, so this does not need a routing-bench baseline (which is dark until
`OPENROUTER_API_KEY` returns).

Two guards, because either alone is satisfiable by a lie. One reads the registry
and fails if a listed tool appears in neither the disjunctive table nor the
"`{}` is a valid call" table — a new tool has to declare which it is. The other
reads the bytes `tools/list` actually returns, and separately asserts each
handler still rejects a call that omits every arm: a schema promising a
requirement nothing enforces is the same defect pointing the other way. Both go
red under a dropped `anyOf`, a changed arm, a deleted handler check, and an
emptied table.

### `dead-code --ignore` was root-relative while the path next to it was not (CON-11)

One invocation carried two readings of a path. The scan path goes through
`normalize_user_path` — relative arguments resolve against your shell's cwd, the
way `grep` and `ls` do — while `--ignore` was only separator-normalized. So from
`src/`:

```
code-graph-mcp dead-code . --ignore generated
```

scoped the scan to `src/` and then excluded **nothing**, because the index
stores `src/generated/…` and the prefix stayed `generated`. Exit 0, full report,
no disclosure — a filter that silently did not run. An absolute prefix
(`--ignore /home/me/repo/src/generated`, the spelling an editor pastes) failed
the same way, for the same reason: prefixes are matched against project-relative
keys.

`--ignore` now resolves like the path it sits next to. Three details worth
naming, because each is a way the fix could have been worse than the defect:

- **The defaults stay root-relative.** `claude-plugin/` and `benches/` are the
  tool's own list, not user input; resolving them against the cwd would make
  them mean `src/claude-plugin/` for anyone standing in `src/`.
- **A trailing separator survives.** `--ignore tmp/` must not widen into a
  prefix that also matches `tmpfiles/`.
- **`--ignore .` now errors instead of reporting clean.** It normalizes to the
  empty string, and every path starts with that — the whole report would vanish
  behind an exit-0 "No dead code found", which is the false clean this command
  already refuses for a misspelled `--type`.

The MCP twin (`find_dead_code`'s `ignore_paths`) is unaffected: its arguments
are root-relative by contract, and its empty report already discloses
`ignored_count` and `ignore_paths_applied`.

### `show`'s "that's a file, not a symbol" hint was dead from every subdirectory (CON-12)

`show` nudges you toward `overview` when the positional argument names a file.
The probe was `project_root.join(arg).is_file()` while everything else in the
command resolves against the cwd, so from `src/`, `show auth.ts` fell through to
symbol resolution and answered `Symbol not found: auth.ts` — for the one input
where the tool knows exactly which command you wanted. The probe now resolves
the argument the same way the rest of the command does. A path that fails
normalization (a `..` escape, a drive letter) is not a file worth hinting about
and still takes the ordinary symbol path.

## 0.129.0

**Upgrading:** two defaults change in ways you can feel.

1. **MCP numeric arguments are now type-checked.** A wrong-typed number is
   rejected instead of silently becoming the default — `semantic_code_search
   {"limit": "50"}` used to return 20 results and say nothing; it now returns an
   error naming the argument. If a client of yours passes numbers as JSON
   strings, it will start seeing errors where it previously got quietly-wrong
   results. That is the fix, not a regression: the schema always declared these
   as integers. No opt-out — restoring the old behaviour means restoring a silent
   wrong answer. To defer, pin `0.128.0`.
2. **SessionStart hooks now run on resumed sessions.** The plugin's matcher
   excluded `resume`, so statusline self-heal, update checks, index-freshness
   probes and recent-impact injection were all dark there. Resuming a session
   will now do that work — and print the same one-time lines a fresh start does.

Nothing else changes shape; the rest of this entry is behaviour that was already
supposed to work.

### SessionStart never fired on a resumed session (JS-04)

`hooks.json` matched `startup|clear|compact`. `resume` was not in the list, and
`session-init.js` handles that source explicitly — it is named in the stdin
contract at the entry point and drives the branch that decides whether the
last-commit reminder is worth showing. So every resumed session ran with no
statusLine self-heal, no forced update check, no index-freshness probe and no
recent-impact injection, silently, for anyone whose workflow leans on resume.

The matcher now spells all four. The guard reads the source list out of
`session-init.js`'s stdin comment rather than restating it in the test, and a
parse failure there is a hard failure, not a silent skip. Stated with its limit,
because pre-tag review pushed back on the first wording: a hand-maintained
comment is still a copy — only two of the four sources (`startup`, `compact`)
have any code branch keyed on them, so nothing mechanically ties that comment to
behaviour. What the arrangement buys is one home instead of two, not a derived
truth. The count floor is pinned at the current cardinality so a comment
truncated to two sources trips it instead of passing.

### The snapshot trust gate ran after the network call it reads as gating (SEC-07)

`gate_origin_url(resolve_from_github(root), origin_trusted)` evaluates its
argument first, so `git remote get-url origin` plus a `gh api` round-trip
happened on **every project open**, regardless of trust; the gate only suppressed
the install afterwards. The module's own comment described that gate as deciding
whether to fetch. It now takes the resolver as a closure and does not call it
without trust — untrusted is the default, so this also drops a pair of
subprocesses from the overwhelmingly common startup path.

What it costs is stated rather than hidden: resolving first was deliberate, so
the opt-in hint could name the actual snapshot URL and stay silent for repos that
publish none. A gate that declines to look cannot know either, so that hint drops
to `debug` and the opt-in stays documented in README's env-var table. The guard
counts resolver calls (0 untrusted, 1 trusted) — the return value is `None` under
both the old and the new code, so nothing weaker can see the difference.

Second half: `parse_github_remote` accepted anything in `owner`/`repo`, and
`repo` is the tail of a `splitn(2, '/')`, so embedded `/` and `..` went into the
`gh api` path. Blast radius was small and is worth recording as such — `endpoint`
always starts with the literal `repos/`, and the GET's response never flows back
to whoever wrote the remote — which makes this a missing guard rather than a
proven hole. Names outside GitHub's alphabet are now refused, `.` and `..`
included.

### Numeric MCP arguments were silently downgraded to their defaults (CON-15)

`semantic_code_search {"limit": "50"}` succeeded, returned 20 results, and
disclosed nothing. Every numeric argument on every tool read as
`as_u64().unwrap_or(default)`, which cannot distinguish "absent" from "sent as
the wrong type" — and `as_u64` answers `None` for `-3` exactly as it does for
`"3"`, so `min_lines: -3` quietly became 3, twice over on the
`module_overview` → `find_dead_code` path.

The string-enum half of this defect class was already fixed at every entry; this
is the numeric half, which had been fixed at none. All seventeen sites now go
through `arg_i64` / `arg_u64` / `arg_f64` / `arg_opt_i64`, which default only on
a genuinely absent value and otherwise name the argument and what arrived.
Rejecting rather than coercing is what keeps the two halves symmetric: an error a
model can read beats a silent answer to a question it did not ask. The new errors
classify as `bad_param`, so the metrics bucket whose job is "the model is calling
this tool wrong" sees the misuse this made visible.

`module_overview` validates `deps_depth` and `dead_min_lines` at entry rather
than inside their `include_*` blocks, matching what the file already does for
`deps_direction` and for the same reason. The guard is a parity table over
(tool, argument) because the site list *is* the unguarded axis — a single-site
test would have passed for either state of the other fifteen.

### Guard scan surfaces were narrower than what they guard (ENG-04/05/06, JS-08)

Four instances of one shape, all found by reading guards rather than by a red
test — which is the point: a scanning guard's blind spot is invisible in a corpus
that contains no offender.

* The tmp three-name matcher recognized `process.env.NAME =` and `NAME:` only, so
  the bracket spelling `process.env['TMPDIR'] = x` counted 0/0/0 — and zero
  equals zero. It now recognizes every spelling, and the matcher itself is
  exercised against each one, since the corpus cannot tell a working matcher from
  a broken one.
* HOME/USERPROFILE is the same axis one directory over and had no guard at all:
  `os.homedir()` reads USERPROFILE on Windows, and 13 test files redirected HOME
  while 4 of them also spelled USERPROFILE. All 104 sites now spell both, with
  the same both-directions count rule. Two mitigations existed and neither covers a
  developer running `node --test` on a Windows machine.
* Three JS hygiene guards enumerated only the top level while all three discovery
  chains have been recursive since the fix `test-discovery-drift-guard.test.js`
  pins — so the first nested test file would have RUN in CI while escaping every
  guard that grades the corpus. All three now walk.
* `detectHookDark` read `process.cwd()` while every writer of
  `recommendations.jsonl` records into the resolved root. In a subdirectory or
  worktree session — exactly what the subdir-cwd fix exists for — the detector
  for dark hooks was itself dark.

DOC-07 is a bet, not a determination: the `code-explorer` agent's tool allowlist
now carries both the bare `mcp__code-graph__*` and the plugin-hosted
`mcp__plugin_code-graph-mcp_code-graph__*` spellings. A plugin-hosted server has
been observed using the second form, but which one resolves for THIS plugin could
not be confirmed without a live install. The asymmetry settles it: an entry that
matches no tool is inert, a missing entry silently strips the agent to
Read/Grep/Bash with no error anywhere. Delete at most one half if a future
harness makes it definitively wrong.

Adding `USERPROFILE` next to `HOME` immediately reddened two correctly-sandboxed
files, because `js_test_files_neutralize_claude_config_dir` looked exactly ONE
line ahead for the paired `CLAUDE_CONFIG_DIR` — the same magic-number mistake its
own neighbour documents three versions of. Its bound is now the redirect block.
Also closed the last vacuous assertion in the tree,
`assert.ok(existsSync(...) === true || true)`, which was true for every input.

### `uninstall` was a documented command that did not exist in half the builds (DOC-03)

It lives only in the npm wrapper, and README sent every reader to it — so a
from-source user following the documented teardown got a bare "unknown
subcommand" for the one command whose whole job is leaving no residue. The Rust
binary now answers for it: names the npx invocation that works, points at
`unadopt` for the part it *can* do, and exits 1 because the requested work did
not happen. README gains a from-source teardown section, and the subcommand joins
the typo table and the doc-alignment guard.

The README performance table was three versions stale and is re-measured on
v0.128.0 (median of three runs, idle machine). Correcting it, not improving it:
P50 655us vs the 575us it claimed, P99 2.1ms vs 1.9ms, full index 2.0s vs 1.9s —
on a corpus that also grew, 278 → 283 files and 5,065 → 5,311 nodes.

### Five items from the 2026-08-29 audit

Five more items from the 2026-08-29 audit: the MCP stub that had none of the
hardening the real server loop got, a ~40 MB binary re-downloaded on every
repeated update round, a test whose deadline was shorter than the interval it
was waiting on, a skill document teaching a destructive path plus the guard hole
that hid it, and the last forked copy of the ambiguity renderer.

No published contract changes shape. The one observable difference is on the
non-project MCP stub, and it is the fix: an oversized message is now answered
with `-32600` and the session continues, where before the session ended.

### The non-project MCP stub had neither hardening the main loop has (CON-05)

`serve_non_project_stub` — what every headless `/tmp` session actually connects
to — read requests with a bare `read_line`. That is the exact call the main loop
documents why it does *not* use: `read_line` validates UTF-8, so one malformed
byte returns `Err(InvalidData)`, and the `?` carried it straight out of the serve
loop, ending the session. It also had no size cap, so a single unterminated line
allocated without bound.

Both loops now share `utils::stdio::read_frame`, which reads raw bytes under a
10 MiB cap, fully drains an oversized line through its newline, and decodes
lossily. It lives in `utils`, not in `mcp::protocol`, because `src/cli ->
crate::mcp` is a forbidden edge — the two published surfaces must not borrow from
each other — and the arch-lock guard in `tests/hardening.rs` caught the first
attempt at exactly that. The JSON-RPC error codes moved to `domain` for the same
reason and are re-exported from `mcp::protocol`, so every existing
`protocol::JSONRPC_*` path is unchanged. The reply wording and the blank-line skip
stay at each call site, so neither command's output changed. Reverting the stub to
`read_line` turns the two new tests red —
the invalid-UTF-8 one dies at the call, the oversized one comes back with the
rejection missing — while the pre-existing stub test stays green either way,
which is why this went unnoticed.

### An already-current binary was re-downloaded on every repeated round (JS-03)

`downloadAndInstall` called `downloadBin(latest)` unconditionally at Step 2, and
again from each of its two early-return arms, while `downloadBinary` had no
"already latest" check. On the normal update path the binary really is behind, so
nothing looked wrong. On a *repeated* round it re-fetched and re-promoted a
~40 MB binary that was already current — which is what sat underneath the
repoint-blocked treadmill (JS-02), one full download roughly every 30 minutes.
`downloadBinary` is now gated on `cachedBinaryNeedsUpdate`, so a current cache
performs zero curl calls; missing, unreadable and older binaries all still
download, since the predicate returns true for each.

### A test deadline shorter than the interval it waited on (ENG-02, D#166)

The periodic-backfill test polled for 30s for a driver that sleeps
`PERIODIC_BACKFILL_SECS` between passes. That constant is 1s under `cfg(test)`
and 60s otherwise — and this test drives `CARGO_BIN_EXE_…`, a binary built
*without* `cfg(test)`, so the server under test slept the production 60s. The 30s
window could therefore not contain a tick on its own: it passed only when the
startup drain happened to finish 29–59s in, so that the single tick at t=60s fell
inside it. Measured on a quiet 24-core box before the change, it passed at 61.4s.
A faster drain makes that worse, not better, which is the inversion D#166
described.

All three embedding polls now share a 300s `EMBED_POLL_BUDGET`, spanning five
ticks regardless of drain time. Each poll breaks on its hit, so the green path
pays nothing. Acceptance: three rounds of two concurrent `mcp_stdio_integration`
processes with all 24 cores saturated — 0 of 6 processes failed, 185–186s per
round.

### A skill taught a non-atomic rebuild, in a directory no guard could see (DOC-02, DOC-08)

`claude-plugin/skills/index.md` taught deleting `.code-graph/` by hand and then
re-indexing, bypassing `rebuild-index --confirm`'s temp-build-and-atomic-rename —
the path that exists precisely so a live server and its open WAL never observe a
half-built index.

Its front matter also said these commands are not exposed over MCP. The first
repair overcorrected — `dispatch_tool` does route `get_index_status` and
`rebuild_index`, so the sentence read as false — but dispatchable is not the same
as reachable: both are deliberately withheld from `tools/list` to save tokens
(`src/mcp/tools.rs`, asserted there), and a model builds its callable set from
that list. Telling it to "use whichever surface is at hand" would have sent it
looking for a tool it does not have. The skill now states the CLI is the surface
it has, and why. Caught in pre-tag review, not by the author.

The reason this could drift at all is DOC-08: `claude-plugin/skills/**` and
`agents/**` were outside every scan surface `doc_cli_alignment` had. They are now
enumerated from the directory rather than a hardcoded list — a hardcoded list is
itself the unguarded axis, and the next skill added would land back in the blind
spot — and each directory must be non-empty, so a move that empties one fails
loudly instead of silently checking nothing. Widening the surface reddened the
guard immediately on the real defect.

### The last forked copy of the ambiguity renderer (ARC-01)

`callgraph` and `refs` are the only two fuzzy-Ambiguous sites, and their
hand-written copies had drifted into different key names and different envelope
shapes for one concept. The sibling exact-ambiguity path has shared a renderer
since it was written; this was the leg that stayed forked, and that is where the
drift grew.

Both now call `emit_fuzzy_ambiguity`. The envelope and the two message suffixes
are parameters, **not** unified: both spellings are published CLI contract, so
`callgraph` still emits `{results, error, candidates}` and `refs` still emits
`{error, suggestions}`. Verified by diffing both commands' `--json` output and
their stderr rendering against the pre-change binary — byte-identical on all
four, with exit code 1 unchanged.

## v0.128.0 (2026-09-01)

Eighteen items from the 2026-08-29 audit: the two that made the index diverge
from a rebuild or grow without bound, two answers that were wrong rather than
terse (a source window cut at pre-edit offsets, and a compact map with no URLs in
it), a benchmark that had reported success for a month without measuring
anything, the guard holes that let those classes come back, and a batch of
hook-latency, determinism and diagnosis fixes. Other items from that report
remain open.

Minor bump rather than patch: three surfaces change behavior by default.

**What you may have to do:**

| Change | Action |
|---|---|
| INDEX_VERSION 69 → 70 | Nothing — every index rebuilds once, automatically, on first use. |
| `get_ast_node(node_id)` / `show --node-id` now re-index the node's file before answering, and may return a **different `node_id`** for the same symbol | Only if you cache node_ids across edits: read `node_id` back off each response (`node_id_renumbered: true` marks the ones that moved) rather than reusing the one you sent. To keep the old behavior — pre-edit line numbers, no re-index — pass `skip_indexing: true`. |
| compact `project_map` gained `entry_points.route`, `module_dependencies.imports` and, when the list is cut, `hot_functions_truncated` / `hot_functions_total` | Nothing — additive fields. Compact is ~931 B larger on this repo (8799 → 9730 B against an 11197 B full map). |
| The `Routing Bench` workflow now **fails** on schedule and release tags when `OPENROUTER_API_KEY` is missing | Nothing on a fork — the gate is scoped to the upstream repo, so a fork without the secret keeps its benign no-op. On a fork of the *workflow*, either set the secret or drop the `GITHUB_REPOSITORY` arm. |

### A file appearing or vanishing left every file that depends on it stale

Only a file whose *content* changed re-emits its import relations, so a file's
*existence* changing left its dependents pinned to whatever they resolved to
before. The recovery channel for a deleted file's inbound relations re-resolves
them by the target node's **name**, and an import's identity is its
**specifier** — which the edge row no longer holds. For Python `from a import
target`:

| after `rm a.py` | edge |
|---|---|
| incremental | `b.<module> --imports--> <external>.target` |
| fresh rebuild | `b.<module> --imports--> <external>.a` |

So the intermediate state diverged too, in the opposite direction, and the
module-level edge (target name `<module>`, which resolves to nothing) was
dropped outright. Restoring `a.py` healed neither, because nothing re-emitted
`b.py`'s imports — and the stale sentinel then satisfied the import-contradiction
prune, which deleted the call edge the pending sweep had just recovered. Every
later run replayed it, so `deps` / `impact` / `callgraph` answered from a graph
that diverged from a rebuild permanently. Branch switches are the everyday way
to hit this.

The indexer now re-extracts a changed file's dependents in the same run — the
same code path a full rebuild runs, which is what makes both states agree with
one. On a 2,000-file corpus with 400 importers of the module that disappears,
both states are now byte-identical to a fresh rebuild of the same tree (before:
800 differing lines, then 800 edges permanently missing). The extra pass costs
0.05–0.09s there; a no-op reindex is unchanged.

### index.db grew forever, and VACUUM could not reclaim it

sqlite-vec only ever inserts into the newest chunk and never reuses a deleted
slot, so every re-index, re-embed and version bump stranded its vectors' space
inside live chunk rows. This repo's index reached **259 MB holding ~7.7 MB of
vectors** — 5,044 live against 130,048 allocated slots. `rebuild-index` reset it
as a side effect of its temp-build, so it only accumulated where the MCP server
rebuilds in place.

Startup repair now rewrites the vector table when the allocator is mostly dead
slots (below 25% occupancy with at least 16 MB claimed) and VACUUMs after. On
this index: 259.0 MB → 73.7 MB, chunks 190.8 MB → 7.5 MB, vector coverage
unchanged, 0.55s. It copies vectors that already exist, so it needs no model and
no embedding-cache coverage, and it never re-embeds anything.

### Two blocking hooks stalled for seconds on ordinary repository content

`pre-edit-guide` matches eight function-signature patterns against whatever you
are editing. Three of them ran an unbounded `\w` / `\S` run in front of a
required literal, which on a long bracket-free run backtracks from every start
position — quadratic. Measured: 100 KB of such text took 2.8s, 200 KB 11.0s,
400 KB 43.4s, all of it blocking the Edit. No malice needed: a base64 asset, a
hex dump or a minified bundle does it. Real code never paid it (225 KB of this
repo's own source: 0.3ms).

The three runs are now capped at 128 characters, which makes the curve linear,
and the hook matches only the first 8 KB, where a signature lives. A 400 KB
`old_string` now costs 1.81ms instead of 43,351ms.

`pre-grep-guide`'s `rebaseRelativePaths` made one `fs.existsSync` call per token
and ran before every length gate in the file, so a very long command line meant
tens of thousands of syscalls inside a blocking hook (100k tokens = 100,001
probes, 2.2s). It now returns early past 2000 characters — the loosest bound the
file already applies elsewhere.

### `stats` printed differently every run, and `--quiet` was never quiet

`stats` text output sorted HashMap-collected rows by count alone, so tied groups
came out in per-process random order: five runs over an unchanged `usage.jsonl`
produced five different outputs. `--json` was unaffected, which is exactly why
the JSON regression tests never saw it.

Separately, `main` installs a stderr tracing subscriber for every CLI
subcommand, but eight sites still wrote both a `tracing::warn!` and the same
sentence through `eprintln!`, on the older premise that the CLI has no
subscriber. So every one of those warnings printed twice, and `--quiet` — which
suppressed only the manual half — left the tracing lines on stderr.
`incremental-index --quiet` is what the PostToolUse hook runs, and its whole
contract is silence.

`--quiet` now reaches the log filter (`RUST_LOG` still overrides it), and each
site keeps one channel — the one written for a human, where the two had drifted
apart: the lock warning now names which processes hold it and what to do.

### Smaller fixes

- A non-object `arguments` in a `tools/call` — `"arguments": "src/main.rs"` —
  was answered as "Error: Missing path", pointing at a parameter you did pass.
  It is now an invalid-params error naming the envelope.
- `centrality_limit` was the only numeric MCP parameter with no upper bound. It
  clamps to 1-100 like every sibling, and a new guard fails the build on the
  next unbounded one.
- A malformed `installed_plugins.json` entry (an empty array) made auto-update
  report "Plugin download/extract failed" — a download diagnosis for a local
  file — and, once guarded, would have silently succeeded forever: a refused
  repoint left the registry pointing at the old version while the update was
  recorded as done, re-running a full download roughly every 30 minutes. A
  refused repoint is no longer counted as success.
- Uninstall no longer reports projects whose managed block you had already
  removed by hand as "Could NOT clean".
- A snapshot-supplied commit id is validated before it reaches `git cat-file`.

### The compact-key drift guard could not see the keys it most needed to

`compact_allowlist_covers_all_result_keys` scanned `module_overview`'s producer
for `result["k"] =` assignments only — but six of its top-level keys come from
the `json!({…})` seed, so a seventh could have been added, silently dropped in
`compact: true` mode, and left the guard green. The scan now covers the seed
literal and `insert()` as well, and reads the compactor's `full["k"]` accesses as
coverage so renamed-but-forwarded keys are not reported as holes. Both halves are
mutation-verified.

### `project_map`'s description told the model it already had the map

The description ended "SessionStart already injected; recall after major
refactor." That injection has been off by default since v0.17.0 (opt in with
`CODE_GRAPH_VERBOSE_HOOKS=1`, and even then only for adopted projects), and the
shipped detail doc says so — so the two steering surfaces contradicted each
other, and the one the model reads was the false one.

The timeline is the interesting part. v0.17.0 removed the dump and named its
replacement in as many words: *"The decision table + the on-demand `project_map`
MCP tool + the per-tool descriptions cover every workflow that the SessionStart
map dump used to support."* The clause was then added three patch releases later,
at v0.17.3, telling the model the dump still happens — suppressing exactly the
call v0.17.0 had designated as the dump's replacement. It was recorded at the
time as saving ~33K tokens/month of "redundant" `project_map` calls at zero
routing regression. The tokens were real; the calls were not redundant, because
the thing they supposedly duplicated had already been switched off.

Now a positive cue rather than a qualified one — negative "don't call this
unless…" framing measured 20pp worse in this repo's own bench. Re-benched per the
tool-description contract (sonnet-4.5, tool-only, Backend): baseline 22/22 =
100.0%, after 22/22 = 100.0%. That shows no triage regression; it does not show
the intended gain, since tool-only P@1 measures which tool is picked once the
model has decided to pick one, not whether it decides to.

### The routing bench reported success for four weeks without measuring anything

Its `OPENROUTER_API_KEY` secret went missing on 2026-08-02. The step handled that
by writing a step summary saying "no P@1 was measured" and exiting 0 — so every
weekly schedule and all 14 release tags through v0.126.2 reported **success**,
because nobody opens a green run to read its summary. A disclosure you have to
click through is not a signal.

The secret is restored, and the first real run since is `P@1 = 22/22 = 100.0%`
(threshold 70%, `anthropic/claude-sonnet-4.5`, 125.7s of live calls). The
unattended triggers — the weekly schedule and release tags — now **fail** when
the key is missing, so a dark measurement is visible in the run list rather than
green. Manual dispatch still exits 0 (a human is reading that output), and the
whole gate is scoped to the upstream repo, so a fork without the secret keeps its
documented benign no-op.

`scripts/routing-bench-dark-run-guard.test.js` extracts the step's real shell
script out of the workflow and runs it under each trigger, asserting exit codes —
a textual "does the source contain exit 1" check would pass for a gate keyed on
an event name this workflow never receives. It carries a negative control that
disarms the gate and requires the run to go green again.

### `get_ast_node` by node_id could return a different symbol's code as yours

`get_ast_node(node_id)` and `show --node-id` are the only read paths that cut a
source *window* out of the live file — `read_source_context` opens the current
bytes and takes `start_line − context .. end_line + context` — and both took
those offsets from the index without refreshing it first. `context_lines`
defaults to 3 on exactly this branch and nowhere else.

Neither freshness mechanism reached it: MCP's `ensure_file_fresh_opt` fires only
on the `file_path` branch (the comment there reasoned a node_id lookup "has no
path to refresh against" — the row it is about to read carries the path), and the
CLI's resync sits in the `else` arm, so `show <symbol>` resynced and
`show --node-id` did not. With ten lines inserted above a function, asking for it
by id returned:

```json
{"name": "con10_target", "signature": "() -> i32",
 "code_content": "// pad 0\n// pad 1\n// pad 2\n// pad 3\n// pad 4"}
```

Five comment lines, under the right name and signature. Silent, because the
fallback to the stored `code_content` fires only when the file cannot be read at
all — and a shifted file reads fine.

Both surfaces now refresh the node's file first, then re-resolve **by identity**
(file, type, qualified name), never by id, through one shared helper. `nodes.id`
is a bare `INTEGER PRIMARY KEY` — a rowid alias, no `AUTOINCREMENT` — and a
re-index deletes and re-inserts the file's rows, so SQLite hands freed ids back
out. Mutating the fix to re-resolve by id and asking for a *deleted* symbol's id
returns the neighbouring function's full body and signature with no error; the
regression test is red under exactly that mutation. When a refresh does renumber,
the response says so (`node_id_renumbered`) instead of leaving you holding a dead
id; when the symbol is gone, it says that. `skip_indexing` still suppresses all
of it.

### compact `project_map` answered "what HTTP endpoints?" with no URLs

Compact mode hand-rebuilds the envelope, and two of its nested arrays had drifted
from the full one. `entry_points` dropped `route` — so a compact map replied to an
HTTP-surface question with handler names, file paths and no endpoints, the one
field the question asks for. `module_dependencies` dropped `imports`, the edge
weight, which is what makes the list rankable at all. Neither was documented as
trimmed. `hot_functions` *is* trimmed on purpose (15 → 10) but said nothing about
it, so a short list read as the whole list; the trim stays and now discloses via
`hot_functions_truncated` + `hot_functions_total`. CLI `map --compact --json` cut
the same rows with no marker while its text mode printed "... and N more", so it
gets the same two keys.

Two guards already covered this envelope's top-level keys and its `modules[]`
entries, after that drift recurred three times; the three other nested arrays had
none. They do now, with a negative control that mutates both key sources — the
`json!` literal and the conditional `obj["k"] =` assignment — for each array.

## v0.127.0 (2026-08-31)

**Security release.** Two ways an untrusted repository could act on the machine
that opened it, plus the data-correctness batch from the 2026-08-29 audit. Minor
bump rather than patch: three defaults change, each with a stated escape hatch.

**What you may have to do:**

| Change | Action |
|---|---|
| `CLAUDE_PROJECT_DIR` is no longer a trusted binary source | Running a source checkout against a marketplace-installed plugin? `export CODE_GRAPH_DEV=1` to keep using `<checkout>/target/release/code-graph-mcp`. Everyone else: nothing. |
| A symlinked `.code-graph` is refused (exit 1) | Only if you had deliberately symlinked the data directory. Move it back and re-run. |
| INDEX_VERSION 68 → 69 | Nothing — every index rebuilds once, automatically, on first use. |

### Query-time freshness: a declared switch with no reader, and four commands never swept

Four defects in the same family — a freshness contract that was documented,
tested, or listed in a guard, and not actually honoured.

- **`skip_indexing` was ignored by eight tools.** `HONORED_UNDECLARED_ARGS` says
  the flag is read by every tool through `should_skip_indexing`, and every tool's
  own dispatch arm does read it — but result-set freshness (FRS-2) arrived later
  and wraps those arms from outside. `semantic_code_search`, `ast_search`,
  `project_map`, `find_similar_code`, `trace_http_chain`, `find_http_route`,
  `get_call_graph` and `find_references` still took a write handle, ran a resync
  and re-dispatched. Measured with `ast_search` after moving a symbol from line 1
  to line 4: `skip_indexing:true` answered 4. It answers 1 now.
- **`module_overview` and `find_dead_code` refreshed nothing.** Both called a
  *file* refresher with a *directory* path, which is classified fresh and returns
  — so the 60s cache was never evicted and both could answer from a pre-edit
  index with no `freshness` disclosure. They were the only two MCP read surfaces
  that could do that. Both now use result-set refresh.
- **The architecture-level CLI commands were never swept.** `affected` was the
  damaging one: its whole contract is a list a CI hook acts on, and it classified
  caller-supplied paths through a stale gate, so a file the branch had just added
  landed in `not_indexed` and printed "0 test file(s) to re-run". `deps`
  disagreed with its own MCP twin `dependency_graph` about the same file while
  the CLI accepted `dependency_graph` as an alias for it. `affected` and `deps`
  now refresh the paths you name; `map`, `tour` and `centrality` refresh the files
  their answers name. `cycles` and `surprising` are deliberately unchanged —
  neither emits a file path, so there is no result set to refresh, and a
  whole-index scan is the cost this budgeted mechanism exists to avoid.
- **Two guard meta-holes.** `freshness_parity.rs` listed `module_overview` and
  `find_dead_code` as covered on the strength of a call that did nothing, and its
  CLI list omitted `cmd_callgraph` and `cmd_report` — both long since wired, so
  their resync could have been deleted with every guard still green. The new
  guards read the production list out of the source and are mutation-verified.

`affected` gained one more property along the way, and lost it again before
release: refreshing its inputs made it drop the dependents of a *deleted* file,
because the refresh retires a vanished path's rows and the command then reads
them. Deletions are the case whose dependents most need re-testing, so paths that
no longer exist are excluded from the refresh.

### The UserPromptSubmit hook had never fired in production

`user-prompt-context.js` read the user's text from `input.message`. Claude Code
sends it as `prompt` — and this repo's own payload constructor
(`lifecycle.js:hookFirePayload`) has always said so. So in production the hook
parsed its payload, found nothing, and exited 0 in silence. The entire
intent-driven injection surface — impact / callgraph / overview / search results
plus the symptom hint — was dead from the day it shipped, and its adoption
metrics read as a structural zero rather than a low number.

Three things hid it, and each one alone would have been enough:

- **The test suite fed `{message:…}`** — a self-consistent copy of the defect.
  Flipping the spawn payloads to the real shape turned three e2e tests red
  before any production line changed.
- **`verifyHooksFire` asserts exit 0**, which is exactly what a hook that reads
  nothing does. It already records `emitted` (non-empty stdout) in the same
  result object; nothing ever read it.
- **The probe payload could not emit either.** `'where is the parse function
  defined'` yields `determineQueryType(...) === null` — no query, no output — and
  the paths that would emit need a real indexed binary the throwaway fixture does
  not have. It is now a symptom-flavoured prompt, which reaches the prose-only
  `symptom-hint` path, for the same reason the Bash probe uses a quoted
  identifier.

The production tell was outside all three: zero `.code-graph-ctx-*` cooldown
flags in a heavily dogfooded tmp directory holding 93 flags from the sibling
hooks.

`prompt` is read first, `message` kept as a fallback. New coverage asserts
non-empty stdout plus a dropped cooldown flag on the documented shape, keeps the
`message` arm alive on its own sandbox (the first run's cooldown would otherwise
silence the second), and pins `{}` to silence. `verifyHooksFire` now asserts the
UserPromptSubmit probe emits, the sibling of the assertion the grep hook already
had. Reverting the field makes four tests fail.

Note for anyone reading adoption numbers: this surface's historical zero is not
a signal about the feature's usefulness. Nothing has been measured yet.

### A read command that mentioned a deleted file destroyed its callers' edges (INDEX_VERSION 69)

`apply_file_refreshes` — the query-time freshness path every read command runs —
deleted a vanished file's rows with a bare `delete_files_by_paths` and then handed
`index_files` an **empty** `delete_paths`. That parameter is what runs Phase 0's
`buffer_then_delete_files`, the mechanism added in v59 for exactly this hole, so
the cascade delete took every inbound call and import with it and nothing buffered
them.

Reproduced as a differential, because the report's own acceptance criterion is
that the two delete paths agree:

```
control  (run_incremental_index after rm a.py):  pending = ["caller->target"]
subject  (a read command's resync, same fixture): pending = []
```

Same fixture, same deletion, one path rescues the inbound call and the other
loses it permanently — until the caller's own file is edited or the whole index
is rebuilt. v0.124's resync unification is how it spread: the hole lived only in
`ensure_file_indexed`, and unifying the three copies carried the one that was
missing the shared mechanism to every read command (CLI `show`/`search`/`refs`/
`grep` plus the MCP tools).

The fix is to stop hand-rolling the delete: `drop_rows` now goes through
`index_files`' own `delete_paths`.

**INDEX_VERSION 68 → 69, so every index rebuilds once on upgrade.** Extraction
semantics did not move — a full index of a 2,000-file Python corpus produces
2001 files / 4001 nodes / 6000 edges before and after, byte-for-byte the same
counts. The bump is for the *installed* indexes: one that already took this path
holds edges that no other trigger will ever restore, and INDEX_VERSION is the
only rebuild trigger. (The audit recommended a CHANGELOG notice instead; a notice
leaves a silently-wrong index wrong for everyone who does not read it.)

**Cost.** A read command that touches a deleted file now pays the same global
post-pass a dirty-file refresh already paid. Measured on the 2,000-file corpus,
release build: a clean query is 5-6 ms, the drop-refresh was 5-7 ms before and is
29-37 ms after — roughly +25 ms, once, on the first query that notices the
deletion. Queries with nothing stale are untouched (early return).

### A repo-supplied symlink in `.code-graph/` redirected every write out of the tree

**Security.** `.code-graph/` is ordinary repo content: one `git clone` can carry
a symlink where this tool expects its own file. `fs::write`, `OpenOptions::append`
and `File::set_len` all follow symlinks, so every writer that opened a fixed name
in that directory was operating on the **link target** — a file the tool never
chose and the user never named. Measured, on a clone that ships nothing but the
links:

| Writer | Effect on the link target |
|---|---|
| `utils::telemetry::rotate_jsonl_if_over` (`recommendations.jsonl`, `usage.jsonl`) | a 1.2 MB file outside the project truncated to its last ~512 KB — 687,756 bytes destroyed, first line included |
| `indexer::lock` (`index.lock`) | `set_len(0)` + PID write left a 47-byte config holding the two digits of a PID |
| `utils::gitignore` (`.gitignore`) | `.code-graph/` appended into an unrelated file |
| `create_dir_all(.code-graph)` | a `.code-graph -> ../outside` link made the call a silent success, putting `index.db` (1.7 MB) and every telemetry file outside the project root |

No prompt, no `--confirm`, no stderr. The sharp contrast that names this as a
blind spot rather than an oversight: the **read** side already refuses to follow
symlinks *into* the tree (`WalkBuilder` runs `follow_links(false)`), while the
write side followed them *out* of it.

One helper closes the class rather than four call sites patching it separately —
`src/utils/owned.rs`, two layers on purpose: `refuse_non_regular` gives a message
naming the path and the reason (and is the only layer Windows has), and
`O_NOFOLLOW` on the open closes the check-then-open race on Unix. `append_owned`
/ `rewrite_owned` / `hold_owned` / `probe_owned` / `ensure_owned_dir` now back
`usage.jsonl`, `recommendations.jsonl`, `.gitignore`, `index.lock` and every
`.code-graph` creator.

**The first pass of this fix was incomplete, and the pre-tag review caught it.**
Both file-level layers judge only the *final* path component, so a symlinked
`.code-graph` **directory** holding perfectly ordinary files walks through both —
the write lands on a real regular file that simply is not where the caller thinks
it is. The directory guard existed but was called only from the three places that
CREATE the directory, and `reindex`, `rebuild-index` and `snapshot::try_install`
all do destructive work before reaching any of them. Measured on the original
fix: `reindex --from-snapshot` against `.code-graph -> ../outside` cut a 54-byte
config to 1 byte (a PID digit) and deleted an `index.db` outside the project —
and then printed the refusal, after the fact. Those three now guard first, and
`cleanup_legacy_db_files` guards inside itself rather than at its four call
sites. The regression test uses the shape the original PoC missed: the directory
linked, the files inside it ordinary.

Two details worth naming. The rotator now stats with `symlink_metadata`, so a
symlinked target is never even **read** — the guard is not merely a write guard.
And the read-only lock *probe* is guarded too: it writes nothing, but it would
`flock` whatever the link points at, and an external inode somebody else holds
would read back as "our index is locked" and refuse a rebuild that was safe.

Behavior change: a symlinked `.code-graph` is now refused with
`refusing to use a symlinked <path> — remove it and re-run` (exit 1) instead of
silently relocating the index. Symlinked telemetry files, `.gitignore` and
`index.lock` are skipped with a warning; indexing continues.

Verified end to end on a sandbox clone: the 1.2 MB target stays byte-identical at
1,200,020 bytes with its first line intact, `outside/` stays empty, and both
`victim.conf` and the symlinked `.gitignore` are unchanged. Six tests reproduce
the six shapes (each with a positive control, so "the writer stopped working"
cannot pass as a fix) plus three on the helper.

### The opened project could hand the plugin a binary — and get it run

**Security.** `findBinary()` resolved the developer's binary by walking a list of
"dev roots", and `CLAUDE_PROJECT_DIR` — the arbitrary directory Claude Code was
opened on — was one of them. A root qualified on the strength of a `Cargo.toml`
existing; the tier sat *above* the version gate, and `isNativeBinary` checked
only that the resolved file's basename was `code-graph-mcp`. Resolution then ran
the file: `findBinary` → `writeCacheEntry` → `readBinaryVersion` spawns it with
`--version`.

So cloning an untrusted repository — reviewing a PR, running someone's example,
pulling a scaffold — and opening it in Claude Code executed whatever that
repository shipped at `target/release/code-graph-mcp`, at SessionStart, with the
developer's full permissions, *before any file was read or any tool approved*.
`.gitignore` does not stop a **tracked**, mode-755 file from arriving intact in a
fresh clone, so supplying one costs an attacker nothing. Every consumer is
downstream of the same call (`session-init.js`, `statusline.js`,
`pre-edit-guide.js`, `mcp-launcher.js`, `doctor.js`, `cg-answer.js`), and
`mcp-launcher.js` spawns it as a long-lived server.

The two other roots are derived from the plugin's *own* install and stay as they
were. This one is now behind an explicit opt-in:

```sh
CODE_GRAPH_DEV=1     # same spelling version-utils.js:isDevMode already uses
```

**Who feels this.** Only a developer running a source checkout of this repo
against a *marketplace-installed* plugin: their hooks previously picked up
`<checkout>/target/release/code-graph-mcp` and will now use the released binary
from `~/.cache/code-graph/bin/` instead. Export `CODE_GRAPH_DEV=1` to restore the
old resolution. Nothing changes for `npm link` / source-tree invocations (the
plugin-derived root already covers those) or for end users (no `Cargo.toml`
beside a marketplace install).

Regression coverage lands where there was none: `find-binary.test.js` now runs
the resolver from a plugin tree copied *outside* any cargo repo — running
in-process would resolve `__dirname/../..` to this repo and mask the finding —
and asserts on **execution**, not on the return value: the fixture binary writes
a marker when it runs, and the marker must stay absent. A second arm asserts
`CODE_GRAPH_DEV=1` still resolves the checkout build, so deleting the tier
outright cannot pass as a fix.

## v0.126.2 (2026-08-26)

### The tmp sandbox that only held on two of three platforms

Contributor-facing. v0.126.1's new residue assertion did its job on its own
release commit: `Check & Test (windows-latest, no-embed)` went red while every
other job stayed green, and the failure named the leftover.

```
the JS test suite left 1 entries in ...\Temp\.tmpsKpjuu\code-graph-mcp
Leftovers: .cg-impact-bb0926fb4271-processPayment
```

`pre-edit-guide.test.js` spawns the real hook to check the PreToolUse envelope,
and sandboxed the child's tmp with one name:

```js
env: { ...process.env, HOME: home, TMPDIR: path.join(home, 'tmp'), … }
```

node's `os.tmpdir()` reads `TMPDIR` first on POSIX. On Windows it reads `TEMP`
then `TMP` and ignores `TMPDIR` outright, so the redirect held on Linux and
macOS and fell through to the *inherited* tmp on Windows — which, for anything
under `cgTmpDir()`, is the one machine-global directory the developer's live
hooks are also reading. The hook's `.cg-impact-<cwdHash>-<symbol>` cooldown flag
landed there and stayed, reclaimed only by `pruneCgTmp`'s 24 h sweep.

Every other tmp redirect in the tree already spelled all three names — all
thirteen of them, in both of the shapes this repo writes: seven module-scope
`process.env.TMPDIR/TMP/TEMP` blocks and six hand-built spawn-env literals. So
this was not a shape a sweep could not see; six siblings share its exact shape
and are correct. It was simply the one site that got written with one name, and
nothing checked.

It reached a release tag because the property had only a behavioural guard, and
a behavioural guard's visibility is platform-dependent — two of three platforms
are green no matter what. So this release adds the static half:
`tmpdir-drift-guard.test.js` now fails on any file whose `TMPDIR` count differs
from its `TMP` and `TEMP` counts. Counted per file rather than matched within a
window: the two shapes above are written with the names on three consecutive
lines or all three inline, and in two different orders, so any "look N lines
down" rule is a magic number waiting to be wrong.
Equality is checked both directions: `TMP`/`TEMP` without `TMPDIR` is the mirror
bug, inert on POSIX instead of Windows.

Reproduced on Linux by running the file under Windows' resolution order
(`env -u TMPDIR TMP=$S TEMP=$S`, with the `TMPDIR:` key stripped): before the
fix the run leaves `.cg-impact-<hash>-processPayment` in the stand-in shared
dir, after it leaves nothing, with the subprocess test that writes the flag
still passing — so the empty result is a fix and not a test that stopped
running. Mutating the fix back out reddens the new guard with
`pre-edit-guide.test.js: TMPDIR×1, TMP×0, TEMP×0`.

No runtime change: both files are tests.

## v0.126.1 (2026-08-24)

### The test suite stops littering your live hook directory

Contributor-facing, and the other half of a fix v0.126.0 only did one side of.

That release stopped three test files from *deleting* `<os.tmpdir()>/code-graph-mcp`
— the one machine-global directory holding live hook cooldown flags — and added a
guard that plants a sentinel there and asserts it survives a full suite run. The
guard could not see the opposite failure: writing into it. Three *other* hook test
files did exactly that, and a sentinel that is still present says nothing about the
14 files that appeared next to it.

Measured on the commit before this one, per full `node --test` run:

| file | entries left behind |
|---|---|
| `post-grep-inject.test.js` | 10 × `.code-graph-postinject-<cwdHash>-<cmdHash>` |
| `pre-read-guide.test.js` | 3 × `.code-graph-readfan-<cwdHash>.json` |
| `pre-grep-guide.test.js` | 1 × `.code-graph-readfan-<cwdHash>.json` |

Every fixture root is a fresh `mkdtempSync`, so the `cwdHash` half of each flag
name never repeats and nothing is ever overwritten — the directory only grows,
reclaimed 24 h later by `pruneCgTmp`'s sweep. On a box where
the suite runs dozens of times a day it accumulates faster than the sweep clears
it, in a directory the developer's own live hooks read.

`post-grep-inject.test.js` had a cleanup helper for exactly this, and it had been
deleting nothing since the flag became project-scoped: it spelled
`.code-graph-postinject-<cmdHash>` while production writes
`.code-graph-postinject-<cwdHash>-<cmdHash>`, and `unlinkSync` inside a `try`
cannot tell a miss from an already-gone file. `pre-grep-guide.test.js` hit the
identical bug, fixed it by matching the command-hash tail, and added a negative
control that asserts the flag EXISTS before cleanup runs — the sibling was left on
the old spelling. Both the fix and its negative control are now in place here too.

All three files redirect `TMPDIR`, `TMP` and `TEMP` into a `mkdtempSync` sandbox at
module scope, before the require that pulls in `tmp-dir.js` — that module resolves
`CG_TMP_DIR` from `os.tmpdir()` at require time, so a later assignment is inert.
The e2e paths still exercise the real flag mechanism; it just happens inside a
directory that goes away with the run.

The guard now asserts both directions and is renamed for it
(`js_test_suite_leaves_the_shared_tmp_dir_intact`). It reads the directory rather
than counting, so a failure names the offenders. Verified by mutation: with the
three files reverted it fails and lists all 14; with them fixed the directory holds
the sentinel and nothing else. Full JS suite 1161 tests / 0 failures, `cargo test
--test hardening` 23 / 0.

### The same guard now catches a config-directory leak, for free

`js_test_files_neutralize_claude_config_dir` scans every `*.test.js` for a
module-scope `delete process.env.CLAUDE_CONFIG_DIR`. The variable outranks a
redirected `HOME` (`claudeHome()` is `CLAUDE_CONFIG_DIR || homedir/.claude`), so
for a developer who exports it — the documented multi-profile setup — a test that
sandboxes only `HOME` operates on their live config. That scan runs on CI like any
other test; what CI never sees is the *leak*, because the runners leave the
variable unset and every such write lands harmlessly in the redirected `HOME`.

The suite guard above already spawns the whole suite under a sandbox, so pointing
`CLAUDE_CONFIG_DIR` at a canary inside that same sandbox costs no extra runtime
and puts the behavioural form of the property on CI for the first time — the
static form was already there. The two are complementary rather
than redundant: the scanner catches a file that omits the line even when no test
exercises a leaking path; the canary catches a neutralization that parses but does
not work, or a new leak reached through a helper the scanner cannot follow — the
shape this repository has already watched a source-scanning guard miss across a
refactor.

Measured before adding it: a full run leaves the canary byte-identical, so it
lands green and is a regression guard, not a bug fix. Mutation-verified in both
directions that reproduce — deleting the neutralizer from `lifecycle.test.js`
leaks `statusline-providers.json`, and from `adopt.test.js` leaks a whole
`projects/<slug>/memory/` tree; both fail the new assertion by name. The
second assertion, covering an existing file rewritten in place, is defensive:
none of the three mutations tried produced that shape.

## v0.126.0 (2026-08-24)

### One index: a stale-file query 11.4 s → 2.0 s, and rebuilds get faster too

v0.125.0 took the one-stale-file query from 7.2 s to 2.1 s by materializing the
projection three global post-passes were re-deriving. What stayed was the shape
of the work in one of them: `bind_calls_to_imported_targets` still asks, once
per candidate edge, "is this name defined in the caller's own file?" — and
`nodes` had no index that answers it. (Its two siblings ask their questions of
the `cg_imports` temp table instead, which carries its own indexes, so this
change does nothing for either. That is why the whole win below lands on one
pass.)

`idx_nodes_name` alone makes that a name-bucket probe followed by a table fetch
per row to test `file_id`, and in real code the hot names (`get`, `run`,
`__init__`) have buckets hundreds of rows deep. The whole fix is a composite:

    CREATE INDEX IF NOT EXISTS idx_nodes_file_name ON nodes(file_id, name);

The query planner shows exactly what changes, and it is one line:

    before:  SEARCH ln USING INDEX idx_nodes_name (name=?)
    after:   SEARCH ln USING COVERING INDEX idx_nodes_file_name (file_id=? AND name=?)

Covering — the row fetch disappears entirely. Measured on 4,736 files of
third-party Python (77,121 nodes / 611,486 edges), two samples per figure:

| | before | after |
|---|---|---|
| query after one file changed | 11.42 s | **1.98 s** |
| full rebuild | 35.6 s | **26.0 s** |
| `bind_calls_to_imported_targets` alone | 28.5 s | 0.34 s |

The rebuild getting *faster* is the answer to the obvious objection: an extra
index means extra maintenance on every node insert, so it should cost something
on the write path. It does — and the post-pass saving is an order of magnitude
larger, so the net is 27% off a full build. Index size is 1.9 MB on a 490 MB
index; creating it on an existing one takes 47 ms.

**No SCHEMA_VERSION bump, deliberately.** `create_tables_sql()` runs on every
writable open, so its `CREATE INDEX IF NOT EXISTS` already reaches every index
built before this release — a migration rung would be inert, which is also
true of the v5→v6 index rung, verified by deleting it and watching nothing go
red. And bumping would be actively harmful: `open_impl_inner` BAILS when
`user_version` exceeds the binary's SCHEMA_VERSION, so every older binary would
refuse to open a database it can read perfectly well, over an added index. This
project has a documented failure mode where the plugin shell updates while the
binary stays pinned; INDEX_VERSION drift only warns, SCHEMA_VERSION drift is
fatal, and an additive index does not belong on the fatal gate. A test pins the
version as unchanged so a future rung has to come past it.

Equivalence is asserted where the pass actually fires, not where it is quiet.
The corpus A/B that motivated the index compared two runs that both inserted
zero edges — a vacuous equality. The committed fixture builds the case the pass
exists for (a bare call resolving to the wrong same-name node while the caller's
file uniquely imports the right one), asserts it binds exactly one edge, and
compares the full edge set with the index present and dropped.

### Running the test suite no longer deletes your live hook state

Contributor-facing, and the root cause of two flakes filed separately.

`lifecycle.js` `uninstall()` removes `<os.tmpdir()>/code-graph-mcp` — the one
machine-global directory holding hook cooldown flags and interrupted `update-*`
download staging. That is right in production: after an uninstall nothing is
left to own it. Every *other* path that function deletes is derived from `HOME`,
so a test that redirects `HOME` into a sandbox looked fully isolated while this
single `rmSync` reached straight out to the real directory. Three test files did
exactly that, which meant `npm test` on a developer's machine silently wiped
their own live cooldown flags — measured, every run.

Inside a parallel `node --test`, the casualties were whichever sibling file was
mid-flight: a cooldown flag written milliseconds earlier disappeared and the
re-grep denied instead of observing, and an `update-<ms>` staging directory
vanished between extract and copy so a plugin update reported failure. Both had
been filed as unrelated mysteries, one of them with a mechanism that the unique
per-test command names make arithmetically impossible.

The three files now redirect `TMPDIR`, `TMP` and `TEMP` at module scope. The
guard against a repeat is behavioural rather than a source scan — it points
`TMPDIR` at a throwaway, plants a sentinel where `cgTmpDir()` will resolve, runs
the suite and asserts the sentinel survives, so it has no file list to go stale
and sees through helpers that build a child env out of sight of the spawn site.
Measured after: 40 consecutive full-suite runs, zero failures.

## v0.125.0 (2026-08-22)

### A query that finds one stale file: 7.2 s → 2.1 s

Editing one file in a 2,385-file project made the next query take **7.2
seconds**. With nothing stale it takes 5 ms — `resync_stale_files` returns
before any indexing work when no file changed — so the whole cost lands on the
one path where a user is waiting for an answer about the file they just edited.

Timers inside `index_files` put 96% of it in three global post-passes:
`bind_calls_to_imported_targets` (1.75 s), `prune_import_contradicted_call_edges`
(2.88 s) and `classify_edge_confidence` (2.56 s). Each evaluates every edge in
the index — 397,119 of them — and re-derives the caller file's imports as a
correlated subquery per candidate. The prune spent 3.6 s to delete zero rows.

Materializing those derivations once per pass, into indexed temp tables, turns
each per-edge re-derivation into an index probe. Measured on that corpus:
prune 3.97 s → 0.44 s, classify 2.71 s → 0.51 s, bind 2.59 s → 1.63 s, and the
end-to-end one-stale-file query 7,191 ms → 2,138 ms. A full rebuild also drops
from 27.2 s to 22.2 s. Nothing-stale queries stay at 4 ms.

No semantics change: each materialized table is its subquery's own FROM clause
verbatim. Verified rather than argued — both binaries indexed 2,385 files of
third-party Python and this repository, and the full edge sets (source, target,
relation, metadata AND confidence) came back byte-identical at 397,119 and
10,842 rows. Because a large corpus can pass an equality check vacuously, each
rewrite was also checked against a fixture where the pass actually fires: the
prune deletes one edge either way, the bind inserts one edge either way, and
classify's 397,119-row comparison spans a real distribution (218,950 ambiguous
/ 110,778 inferred / 67,391 extracted). No INDEX_VERSION bump.

Two things measured and NOT the cause, recorded so the next round does not
re-suspect them: `collect_crate_root_names` (1.68 ms) and
`build_python_module_map` (0.60 ms) together are 0.03% of the query — a
shape-based reading of the code had named them as the likely cost. And writing
less does not help: guarding `classify` to skip no-op updates saved 0.2 s of
2.6 s, because the cost is evaluating rows, not writing them.

The remaining 2.1 s is the same three passes still evaluating every edge, just
faster per edge. Getting below that means scoping them to the files that
changed plus the names whose counts moved, which is a correctness-sensitive
change (an import in one file binds a call sourced in another) and stays open.

### A half-applied plugin update now says so

`lifecycle.js` reads `installed_plugins.json` through the three-way read that
tells ENOENT from unreadable. `auto-update.js` writes the same file and did
not: it used the lenient `readJson`, whose `null` covers ENOENT, EACCES and
unparseable alike, and the `if (installed && …)` guard then skipped the repoint
in silence — after the plugin copy had landed and while the install manifest
was about to be advanced to the new version.

The result is a split-brain: Claude Code keeps launching the previous install
directory while state reads "up to date", which is the shape the binary-pin
incident was made of, with nothing on screen to connect the two. Nothing is
written now either — bytes we could not read are not ours to guess at — but the
user is told, so `/plugin update` stays reachable as the way out. The write
failure, previously swallowed by a bare `catch`, reports the same way.

The two remaining lenient reads in that file are read-only version lookups with
no write-back, so they are not the same defect.

Two follow-ups from the pre-tag review, both about the arm that refuses:

The report was a **one-shot**. `checkForUpdate` treats `readManifest().version`
as the authoritative installed version, and the manifest was advanced whether or
not the repoint had landed — so the next session computed "up to date", never
retried, and never printed again. One line of hook stderr was the entire notice.
The manifest now advances only when nothing is left pointing at the old version,
which puts the ordinary check interval behind the message: the install retries,
the report recurs, and the repoint lands by itself once the file is repaired. An
absent `installed_plugins.json` blocks nothing — there is no entry to repoint.

And **`lossy` is not `corrupt`**. `readJsonResult` reports `lossy` for a file
that parses fine but carries a byte that will not survive a rewrite — a cp1252
byte in a path, the shape a non-ASCII Windows username leaves. It returns a
usable value and no `error`, so folding it into the corrupt arm both refused a
repoint that v0.124.0 performed and called the file "unparseable" while holding
its parsed contents. It now takes `lifecycle.js`'s own preserve-then-proceed
route: the original bytes are copied to a `.corrupt-<stamp>` sidecar, the repoint
proceeds, and the message names the byte problem. If even the copy fails, the
repoint is refused — destroying the bytes silently is the worse outcome.

### Two more `--json` legs stop wearing the success shape

Both were carried as unverified notes from an earlier round. Reproduced against
HEAD, both were real.

`grep --json` emitted the success-shaped `[]` on the one error leg the previous
round of this fix did not reach. `emit_grep_json_error` exists precisely so a
failed run never looks like a zero-match run, and its own doc comment says
"every error leg used to print `[]`" — but the zero-match branch printed `[]`
BEFORE testing `partial_error`, so a path that does not exist inside the
project, or a flag-shaped token that displaces the real pattern into the path
slot, still produced `[]` with only the exit code to distinguish it. Under the
`grep … --json 2>/dev/null` shape the agent-facing docs themselves suggest,
that reads as "this repo has no matches" for a search that never ran. The check
now runs first and the error carries ripgrep's own message; the genuine
zero-match leg keeps its `[]`.

`stats --json` is an object-envelope command, so the same contract requires the
same shape when there is nothing to report. It emitted three of eleven keys
when `usage.jsonl` was missing, and two — with no disclosure at all — when the
file existed but held no sessions. A consumer reading `total_tool_calls` got a
missing key on exactly the projects where it should read zero. Both legs now
build the full envelope through `build_stats_json` and attach `note` alongside
it, so the shape is a property of construction rather than a list somebody has
to remember to extend.

The new guard derives its expected key set from a populated run rather than
listing the keys, since a hand-maintained list is what goes stale when
`build_stats_json` gains a field. Both fixes are mutation-verified.

Also corrected, from the same batch of notes: `stats --json` with an unknown
flag was recorded as printing the clap error twice and leaving the output
unparseable. It does not. The JSON error object goes to stdout and the
human-readable render to stderr — the same split every other subcommand uses,
and `--json` output stays parseable.

### `get_call_graph` no longer names a tool the client cannot call

`NON_LISTED_MCP_TOOLS`'s doc comment has said for several releases that
"anything the model READS (prompt text, tool descriptions, `instructions`) must
name only LIVE_MCP_TOOLS". One of those three surfaces was checked. The other
two were not, and one had drifted: `get_call_graph`'s `tools/list` description
carried "(folds the old trace_http_chain)" — a name `tools/list` never
advertises, so an MCP client cannot offer it and the model reads a call it
cannot make. The parenthetical was history, not instruction, so it is simply
gone.

The new guard reads the SHIPPED responses, not the source: it walks every
`tools/list` object whole (an enum label or a parameter doc teaches a name just
as well as a description does) and both `instructions` variants BY NAME. That
last part is the non-obvious half — `initialize` returns the quiet or the noisy
text depending on `CODE_GRAPH_QUIET_HOOKS`, so a check that only reads the
response covers whichever the ambient environment selects and leaves the other
permanently unguarded. The plugin sets that variable, which makes the quiet text
the one most users actually read; a first draft of this guard checked only the
response and would have missed it. All three arms were verified by mutation.

The doc comment now names both guards. A sentence claiming coverage is read as
coverage, so it may not outrun the tests that provide it.

Not measured: `tests/routing_bench.rs` needs an `ANTHROPIC_API_KEY` or
`OPENROUTER_API_KEY`, and neither was available, so there is no before/after
trigger-rate number for this description edit.

### The heritage and export axes are tables, finishing the walk conversion

`walk_for_relations` had three of its five relation axes as tables and two as
hand-written `match` arms. The arms' own comment named the cost: "adding a
language here still means adding an arm, and a missing one is a
silently-dropped edge rather than a compile error" — the exact shape that
produced the v0.83.0 per-language gaps and the 2026-08-16 heritage gaps, where
a Java `interface`, a Kotlin `object` and a Swift `protocol` emitted no
inheritance edges at all and nothing failed.

Two things were said to block the conversion, and only one was real. Heritage
was described as dispatching on the `is_heritage_decl` PREDICATE rather than a
fixed kind list — but that predicate was `HERITAGE_DECL_KINDS.contains`, a kind
list wearing a function, so the row just points at the same const. The C#
`base_list` arm inspecting its PARENT node kind is real, and it is why
`HeritagePass` carries `not_under`: one field, not a new dispatch mechanism. It
is what keeps `enum Level : byte` from emitting `Level inherits byte`.

The conversion changes one semantic that has to be paid for rather than
assumed. A `match` is first-match-wins, and the C++/Rust/Go/C# arms depended on
it — `HERITAGE_DECL_KINDS` deliberately omits `class_specifier` precisely
because the C++ arm sat later in the same match. The tables run EVERY matching
row, so `no_node_kind_reaches_two_heritage_or_export_rows` now asserts what
`match` used to enforce: two rows may share a node kind only when their
language gates cannot both admit the same language. All three of its rejection
paths were verified by mutation (an ANY_LANG overlap, a raw-vs-family key
mismatch, and two same-key rows sharing a language).

Verified as edge-neutral rather than argued to be: both binaries indexed this
repository (36,547 files — Rust `impl_item`, JS/TS ESM and CommonJS exports)
and 2,385 files of third-party Python (the `class_definition` heritage row),
and the full edge sets — source, target, relation, metadata and confidence —
came back byte-identical, 10,834 and 397,119 rows. Go `type_spec` and C#
`base_list` have no corpus on this machine; they are covered by the existing
unit tests, including the `enum Level : byte` negative control for `not_under`.
No INDEX_VERSION bump: extraction output does not move.

The `routes` axis keeps one arm in the walk — Python's `decorated_definition`,
because Flask/FastAPI spell a route as a decorator while Express and axum spell
it as a call and arrive through `CALL_PASSES`. A one-row table is a shape
without evidence for it.

### A Python package's non-package subdirectories are no longer import roots

`import_roots` decided whether a directory was importable-from by asking
whether that directory itself held an `__init__.py`. It never asked whether an
ANCESTOR did. Since PEP 420 made `__init__.py` optional, a package routinely
contains subdirectories without one — vendored trees, test-data trees, example
dirs — and every one of them became a top-level import root, so any module
inside it answered to its bare name.

That is the phantom class v0.124.0's sibling fix was written to remove,
rebuilt one level down: a phantom bound to a real node, which nothing in the
answer marks as wrong. Measured by indexing 2,385 files of third-party Python
with both binaries and diffing the edge sets: `import random` bound to
`numpy/typing/tests/data/pass/random.py` from 15 files across PIL, pandas,
pyarrow and numpy's own tests, and `from numba.extending import
register_jitable` bound to `numpy/random/_examples/numba/extending.py`. 17
`imports` edges removed — the 15 module bindings plus 2 symbol edges that
existed only because the module had resolved to a project file — and 15
re-bound to `<external>`, which is where the same statement spelled `from
typing import IO` already goes. `calls`, `inherits` and `references` were
byte-identical, so the change is confined to the axis it was aimed at.

The rule is now stated the way Python states it: a directory is
importable-from only when neither it nor any ancestor is a package. `src/` above
a `src/myapp/` package stays a root, which is what keeps `from db import save`
in a `src/` layout working; the ancestor test only ever removes roots that sit
INSIDE a package tree, where you are reached by dotted path.

INDEX_VERSION 67 → 68: existing indexes carry the phantoms until they rebuild.

### The pre-grep e2e cleanup had been deleting nothing

`cleanupFixture` in `pre-grep-guide.test.js` removed
`.code-graph-bash-<commandHash>`. The cooldown flag gained a `<cwdHash>-`
segment when cooldowns became project-scoped, and this line did not follow, so
it addressed a name production had stopped writing. A miss is indistinguishable
from "already gone" through `unlinkSync` plus a swallowing catch, so nothing
ever reported it: the comment promised "remove so reruns stay deterministic"
and bought none of it, and every e2e run left its flag in the real
`cgTmpDir()` until `pruneCgTmp`'s 24-hour sweep collected it.

Cleanup now matches on the command-hash TAIL, which is independent of both the
cwd and the prefix spelling, and a new test asserts the flag exists before
cleanup and is gone after — restoring the old derivation turns it red.

This also settles one open question about the `pre-grep-guide` cooldown flake
(D#156) in the negative: the leading hypothesis was a sibling test's cleanup
deleting this test's flag under parallel load, and a cleanup that deleted
nothing cannot have done that. The flake's mechanism remains unidentified.

### The statusline shell route now belongs to `_previous` alone

v0.124.0 sent any registry command containing a shell metacharacter through
`sh -c`. That is right for `_previous` — it IS the user's `statusLine.command`,
which Claude Code runs through a shell — and wrong for the other two classes,
which we build ourselves: `codeGraphCommand()` composes
`node "<plugin-dir>/statusline.js"`, and third-party entries arrive through
`statusline-chain.js register`, whose only executor has ever been
`execFileSync`. Neither was ever a shell string, so handing them to one imposes
semantics they never had.

Our own segment is the casualty. Measured with the plugin under a directory
named `dev$work`: the generated `node "…/dev$work/statusline.js"` returned a
segment before, and `null` at v0.124.0 — a shell expands `$work` to nothing
even inside the double quotes, the exec fails, and the catch swallows it. The
trigger is narrow (inside double quotes only `$` and a backtick break; `&`,
`;`, `|`, `(`, `)`, `<`, `>` are all literal there), which is why it took an
install path with a `$` in it to show up.

Confining the shell to `_previous` also confines the tradeoff that came with
it: through `sh -c` the timeout's SIGKILL reaches the shell rather than a
grandchild that traps signals, and now only the entry that cannot work without
a shell pays that price.

### Determinism, honest diagnostics, three dead symbols

`module_overview`'s `inactive_summary` was grouped through a `HashMap`, so the
same binary over the same index emitted a different group order on every run —
irreproducible LLM-visible output. It is a `BTreeMap` now, so the order is
structural rather than a sort that can be forgotten.

The index lock reported every `flock` failure as "another instance holds the
index lock", so a filesystem without flock support sent the reader hunting for
a process that does not exist; and the PID written for diagnostics landed on
top of the previous one without truncating, so `123456` followed by `999` read
back as `999456`. Both paths still fall back to secondary mode — only the
diagnosis changes.

`cg-answer`'s hard truncation decoded its bytes as latin1, one character per
byte, so a single oversized line of CJK came back as mojibake instead of a
shortened line. It now backs the cut off to a UTF-8 character boundary.

Removed `try_install_snapshot`, the `"file-impact"` telemetry arm, and
`project_root_canonical` — all three reserved by an `#[allow(dead_code)]` for
work that landed elsewhere. `FORBIDDEN_EDGES` gained a completeness check that
asks the filesystem what the module roots are, so the next `src/` addition
fails loudly instead of going unscanned.

## v0.124.0 (2026-08-22)

> **Upgrade note — your index rebuilds once.** INDEX_VERSION moves 65 → 67:
> Rust calls qualified with the crate's own package name now produce edges they
> used to drop, and Python `from X import Y` now records its module dependency.
> The rebuild is automatic on first use.

Audit `docs/audit/audit-2026-08-22-01.md`: all P1 and all P2 items.

### `mycrate::module::f()` calls no longer drop on the floor

A Rust call written with the crate's OWN package name — `code_graph_mcp::cli::cmd_grep()`,
which is how every `src/main.rs` reaches its library — produced no `calls` edge.
The parser strips the reserved roots `crate`, `super` and `self` from a
qualified path, so the surviving segments name real directories; a package name
is a crate root too, but it is not one of those three literals, so it stayed in
the qualifier. `code_graph_mcp/cli` matches no path under `src/`, the candidate
set emptied, and the Path arm's drop-on-empty discipline threw the edge away.

The blast radius is the three features that read that edge, on the most common
Rust layout there is. On this repository: `dead-code src/` reported 23 of 23
candidates, every one of them a `cmd_*` that `main.rs` calls; `impact cmd_grep`
answered "Risk: LOW, 0 callers" for a 768-line function; `refs cmd_map` printed
nothing while `src/main.rs:279` called it.

The resolver now strips a leading segment equal to one of this project's Cargo
package names (`-` normalized to `_`), which it reads from the `Cargo.toml`
files at and under the project root. Keyed on the real package names, so
`other_crate::cli::f()` — a genuinely foreign path — still drops rather than
binding to a same-named local symbol. Measured on this repository's own tree:
7,053 → 7,151 `calls` edges, no edge removed and no confidence changed, the
same result at `BATCH_SIZE` 500 and 25; `dead-code src/` now finds nothing.

The strip is a fallback, not a rewrite: the qualifier as written gets the first
say, and the crate-root reading applies only to a chain that matches nothing.
Stripping unconditionally would cost more than it bought, because a package name
is often also an ordinary directory name — `core`, `utils`, `parser`, `config`
in any Cargo workspace. `utils::helper::go()` in a package named `utils` would
degrade from `utils/helper`, which matches one directory, to `helper`, which
matches every `helper/` in the tree: one correct edge becoming two `inferred`
edges whose metadata still read `v:"utils::helper"`, a path only one of the two
targets has.

And a root with nothing after it — `my_crate::run()` — leaves no path constraint
at all. A path qualifier is exempt from the ambiguous-confidence downgrade, on
the premise that it binds by module path rather than by a bare-name guess; on
that branch the premise is false, so a two-way ambiguity would ship as two
`inferred` edges, one of them a phantom bound to a real node and sitting above
the default confidence floor where `dead-code`, `impact` and `callgraph` read it
as fact. A single candidate is still an unambiguous answer and resolves; several
candidates now drop. No answer beats a wrong one.

### Python imports resolve the way Python resolves them

`from pkg.mod import Helper` recorded `pkg.mod` as a plain symbol rather than a
module: the module is reached through the `module_name` FIELD, which left the
positional flag false, so the module's own node fell into the imported-symbol
branch. Resolution then looked `pkg.mod` up in the symbol pool, found nothing,
and the module dependency that `from X import Y` expresses — the dominant
Python import form — never reached the graph at all.

Marking that row exposed the larger problem underneath. The module map was keyed
by path SUFFIX: `src/myapp/utils.py` answered to `src.myapp.utils`,
`myapp.utils` AND `utils`, on the argument that over-connecting is the safer
failure without `sys.path` context. Measured against 1,763 files of third-party
Python, that argument does not survive contact — `import logging` bound to
`accelerate/logging.py`, `import json` to `rich/json.py`, `import math` to
`pygments/lexers/math.py`. 886 of 1,451 module bindings pointed at a real node
the import does not name, and `deps`, `cycles` and `map` consumed every one of
them as fact. Marking the module rows alone would have taken that to 1,729.

The map is now keyed by IMPORT ROOT: a dotted path resolves relative to the
project root plus every directory that is not itself a package. Inside a
package, `import logging` means the standard library — PEP 328 has made that the
only reading since Python 3 — while a `src/` layout, a `tests/` tree or a plain
script directory still puts its own modules on the path. On the same corpus:

| | module bindings | resolved to the named module | basename coincidences |
|---|---|---|---|
| before | 1,451 | 565 | 886 |
| marker only | 4,364 | 2,635 | 1,729 |
| now | 2,864 | 2,864 | 0 |

(The 228 that do not anchor at the project root anchor at
`setuptools/_vendor/`, which carries no `__init__.py` and so is a real import
root — that is the vendoring mechanism working, not a miss.)

528 call edges went with them, and their targets say what they were:
`islice` → `anyio/itertools.py`, `log` → `accelerate/tracking.py`,
`glob` → `setuptools/glob.py`. Those were confident answers pointing at the
wrong function. What replaces them is a bare-name fan-out at `ambiguous`
confidence, which the default floor hides — no answer instead of a wrong one.
In this repository the whole change moves exactly 10 edges, all of them the
benchmark scripts' `from eval_ranking import …` finally binding to
`scripts/embedding_benchmark/eval_ranking.py`.

### The auto-updater no longer hangs when a connection dies mid-response

`requestJson` listened for `data` and `end` on the response and nothing else.
`req.setTimeout` is an inactivity timer that lives on the socket, so a
connection dropped part-way through the body produced no `end`, no error and no
timeout: the promise stayed pending forever. The per-session update check runs
detached, so on a flaky link or a resetting proxy it left a zombie `node`
process behind for every session; run from a terminal, it hung the terminal.

The response now rejects on `error` and on `aborted`, and an overall watchdog
(four times the inactivity budget, so a slow-but-progressing fetch is not
failed by it) backstops any other never-settles shape. Both settle paths are
single-shot and clear the timer, so a normal response cannot be re-settled by a
late event or hold the event loop open.

Settling the promise turned out to be only half of it. An undestroyed request is
an active handle: the caller's `await` returns while the socket stays open, the
event loop still has a reason to live, and the detached check stays resident —
the same zombie process, reached through the other half of the problem. Every
handle that can outlive the promise (the request, and in the proxy branch the
CONNECT request plus the tunnelled socket, which outlives it) is now torn down
when the promise rejects. The watchdog timer is `unref`'d for the same reason:
if the request constructor throws outright on a malformed URL, the timer alone
would have held the process up for the full overall budget — twelve seconds at
the shipped setting.

### One query-time freshness resync instead of three copies of it

`show`/`refs`/`search`/…, `grep`'s AST annotations, and the MCP server's
result-set refresh each carried a line-for-line transcription of the same
rule — look up the stored hash, hash the bytes, re-index on mismatch,
decrement a budget — and the budget knob had split into two environment
variable names along the way. They now share one implementation in
`src/indexer/resync.rs`; `CODE_GRAPH_RESYNC_BUDGET` tunes every surface, and
`CODE_GRAPH_GREP_SYNC_BUDGET` keeps working for `grep`.

The copies also re-indexed one file at a time, and each of those calls re-hashed
the file the caller had just hashed and then paid a whole-graph name-map load
plus the global edge post-passes. A query touching the eight-file budget cost
eight whole-graph sweeps. The dirty set is now classified once and re-indexed in
a single batch. No output shape changes.

### Smaller fixes from the same audit

**Freshness reached the last two read paths.** `callgraph` had no
query-time resync on either surface, and MCP's `get_call_graph` /
`find_references` had a subtler version: both accept a `file_path`, so
they looked covered, but that argument is an optional disambiguator and
the ordinary call passes a bare symbol name. What goes stale there is not
a line number — it is the caller set, and a call added since the last
index was simply missing.

**Two prompts named tools the client cannot call.** `understand-module`
pointed at `dependency_graph` and `trace-request` at `trace_http_chain`.
Both still dispatch server-side, but neither appears in `tools/list`, and
a client only offers the model what that list returned. They now teach
`module_overview include_deps` and `get_call_graph route_path`, and a
guard walks every prompt to keep it that way.

**The statusline no longer loses a quoted `_previous` command.** Provider
commands were split with a regex that understood one double-quoted word
after the executable; a path containing a space was torn apart and the
segment vanished silently — the one thing the `_previous` slot exists to
prevent. Commands with shell constructs now go through `sh -c`.

**A PR past 100 comments stops collecting sticky comments.** `gh api
--paginate` emits one JSON document per page; the parse threw and every CI
run posted a new comment instead of patching the old one.

**Plugin housekeeping.** `migrateOldPluginIds` no longer throws a bare
stack out of doctor's repair arms when `~/.claude` is unwritable, and
`find-binary` no longer runs the binary to check its version on every
cache hit — the cache entry carries the version and a file stamp, so a
session-start that calls it four times spawns at most twice, and once the
entry is stamped, not at all.

**A full index reads each file once.** `scan_directory` hashed every file
and the pipeline then read it again to parse it; on 1,763 files that was
7,781 read syscalls where 3,535 suffice. Wall clock is unchanged — the
files were in page cache and parsing dominates — so this is I/O, not speed.

**Documentation says where its numbers come from.** The Performance table
now lists exactly what `code-graph-mcp benchmark` prints, measured on this
repository; two unsourced efficiency claims are gone and the third names
the test that produces it. All 35 `CODE_GRAPH_*` environment variables are
documented, with a test that fails when a new one is not.

**Long functions and one long file split**, with no behaviour change:
`index_files` 1,242 → 516 lines, `cmd_grep` 767 → 528,
`tool_semantic_search` 705 → 477, `cmd_stats` 500 → 64, and the MCP
server's `mod.rs` gives up its freshness and backfill halves to their own
files. Each verified by comparing real output — byte-identical CLI output,
byte-identical MCP responses, and identical edge sets over two corpora.

## v0.123.0 (2026-08-22)

> **Upgrade note — your index rebuilds once.** INDEX_VERSION moves 64 → 65
> because `doc_comment` values change for source you have already indexed: a
> comma-separated declaration used to hand its one documentation comment to
> every name it declared. The rebuild is automatic on first use. The shape is
> uncommon — a regex sweep of an external 2,900-file TypeScript/JavaScript
> corpus found zero occurrences — so most projects will see the rebuild and no
> content change. Nothing else to do.

### A shared documentation comment now belongs to the first declarator only

`/** DOC */ export const a = 1, b = 2;` gave DOC to both `a` and `b`. The
comment sits above the *statement*, which owns every declarator, so each name
resolved to the same block. The same held for `export let` and for
arrow-valued declarators.

This is not cosmetic mislabelling. `doc_comment` outranks `code:` when the
embedding context for a symbol is built, precisely because it is the densest
description a symbol has. A duplicated comment therefore made `b` retrievable
under a description of `a` — a phantom bound to a real node, which is worse
than a missing field, because nothing in the answer tells you it is wrong.

The rule now applied is the one this codebase already used for the same
construct in another language: Go's `// GROUP_DOC` above `type ( Alpha …; Beta
… )` documents `Alpha` and stops there. A comma-separated JavaScript
declaration is that construct with different punctuation, and it never reached
the check — the statement is its parent's first named child however many
declarators hang off it, so the check passes and cannot discriminate between
them.

The doc is claimed by position rather than by whether the leading declarator
produces an indexed symbol, so a first declarator that emits nothing cannot
slide the comment onto a later name. Names bound by a single destructuring
declarator (`export const { host, port } = getConfig()`) still share that
declarator's comment: the split is between declarators, not within one.

### `trace --json` reports a miss the way every other command does

A route that matched nothing emitted `{"handlers": [], "message": "…"}`. Every
other JSON surface in the CLI follows a three-tier rule where a miss carries a
self-describing `error` beside the empty collection — `show --json` returns
`{"candidates": [], "error": "Symbol not found", "symbol": …}` and `callgraph
--json` the same with its own identifier. A consumer that branched on `error`,
as the documented contract tells it to, read a no-match as a clean success with
zero handlers.

The envelope now emits `{route, handlers: [], error, hint}`. `route` is the
same key the success leg carries, so one shape reads both. The framework
coverage limit moved out of the error text into `hint`: it is a disclosure
about the extractor, not a description of this particular miss, and folding it
into the error made a coverage gap read as "no such route". An actix or Spring
project has real routes this extractor never sees.

### The MCP server can finally say that a file parsed with syntax errors

`get_index_status` already reported files the indexer *skipped*, including the
ones that failed to parse outright. The other counter was missing from this
surface entirely: a file that parses *with* syntax errors keeps whatever
tree-sitter's error recovery salvaged, and those partial symbols sit in the
index looking exactly like symbols from a clean file. That is the more
misleading of the two — a skipped file is visibly absent, while a half-parsed
one answers queries with a thin result set and no way to tell
thin-because-broken from thin-because-that-is-the-code.

The CLI has disclosed this since the counter existed, on both legs. Nothing
under the MCP server said it at all, so an agent driving the server was the one
caller that could not find out. Both surfaces that report indexing now carry
it, with deliberately different rules about zero: `rebuild_index` states it
unconditionally, because the rebuild ran in-band and a zero is earned;
`get_index_status` states it only when non-zero, because its statistics are
per-process and a server that started against an already-fresh index holds
zeros it never earned.

### Skipped context injections name the mode that spent the attempt

The PostToolUse hook that offers a structural answer alongside a grep records
the attempts it does not deliver. Those records carried the reason but not the
mode, so the funnel could see *that* injections were failing without seeing
*which* mode was failing — the attribution the work needed most.

The record now names the mode it tried, charged to the last one attempted, so a
call-graph miss that fell through to the grep echo is charged to the echo.
`stats` reports the two mixes separately: the payload mix keeps its meaning as
what was delivered, and skips get their own breakdown. Folding them together
would have quietly redefined the delivered-payload share into an attempted
share.

### `project_map`'s compact mode no longer advertises a number it never met

The `compact` option claimed it saved about half the tokens. Measured on this
repository, the full map is 5,445 bytes and the compact one 4,305 — 20.9%. No
implementation ever matched the advertised figure, and reaching it is not a
tuning question: compact spends 1,098 bytes on key symbols and 1,033 on hot
functions, so dropping the symbols alone lands at 41%. Those symbol names are
kept on purpose, because a map without them costs the caller a second request
to become useful.

The five sibling options in the same schema all say plainly that they save
tokens, without a figure. This one now matches them. Writing the measured 20.9%
in its place was the alternative and was rejected: the ratio moves with module
count and symbol density, so any figure would be a claim that goes stale on the
next repository.

### Internal: the import axis is a table

Relation extraction dispatched two of its four axes from tables and the rest
from a long `match`. `imports` is the axis where grammars disagree most about
spelling — `import_declaration` alone means two different shapes in Swift and
Java — and a run of match arms is a shape where a missing language is not a
compile error but an edge that silently never appears. Twelve table rows now
carry that mapping, with two guards: one asserting each language actually
emits its import targets, one asserting no extractor exists without a row
naming it.

No behaviour change, and verified as such rather than asserted: every relation
extracted from 2,927 external Python/TypeScript/JavaScript/Go/Java files was
compared before and after, metadata included — 397,088 rows, byte-identical.
Heritage and exports stay as match arms; their dispatch is not uniform in the
same way, and converting them is a different change.

An independent review before this tag found no correctness defects in the six
commits and seven places where a guard was weaker than it read — a duplicate
table row would not have failed anything, the coverage check listed its
languages by hand, the extractor scan would have missed a signature rustfmt
wrapped, and the parity rows could not see a phantom target. All seven are
closed. Tightening the parity rows to set equality immediately surfaced one
pre-existing oddity worth naming here even though it is not fixed in this
release: `from pkg.mod import Helper` in Python records `pkg.mod` itself
alongside `Helper`, without the `is_module_import` marker that `import os`
carries, so nothing downstream can tell that module from a symbol of the same
name. Correcting it changes the edge set of every indexed Python project, so it
belongs with an index-version bump of its own rather than inside this one.

## v0.122.1 (2026-08-19)

> **Nothing changes for you.** This release carries no source change — the
> binary is identical to v0.122.0 — and no index rebuild (INDEX_VERSION stays
> 64). It exists to cut a release through the repaired pipeline described below.
> If you are on v0.122.0 you can stay there.

### CI: an apt call can no longer hang a job for six hours

Cutting v0.122.0 ran into an apt mirror that stopped answering mid-`update`.
Three workflows stalled on the same step simultaneously — the release gate (29
minutes, with the tag already pushed and every later job blocked behind it), the
`with-embed` test leg (40 minutes) and the cache-priming job — and all three had
to be killed by hand. Nothing was published, because the gate is the first job
and everything else waits on it; the release went out cleanly on a re-trigger of
the same tag. This repository had already lost one run to the six-hour job
ceiling the same way.

The guard those steps carried was not guarding anything. Its comment said the
`command -v rg` short-circuit "keeps an apt mirror hiccup off this critical
path", but no GitHub runner ships ripgrep — a fact stated two comments further
up in the same file — so on Linux the check always falls through to apt. It only
ever helped a runner that already had it.

Every apt call now carries a step-level timeout, and the Linux paths retry three
times with a per-attempt bound so an ordinary hiccup heals itself. Failure still
fails loudly rather than being swallowed: the grep-backed tests self-skip when
ripgrep is missing, so ignoring an install error would trade a visible hang for
43 tests quietly not running.

The same install had been copy-pasted into four places and the cross-compiler
install into two more, so a test now walks every workflow and fails on any apt
step without a timeout. It found a sixth site on its first run — one on the
release critical path that the manual sweep had missed.

## v0.122.0 (2026-08-19)

> **Upgrade note — your index rebuilds once, and more of your symbols come back
> documented.** INDEX_VERSION moves 63 → 64 because `doc_comment` values change
> for source you have already indexed: a declaration carrying a decorator,
> attribute or annotation used to lose its documentation entirely. The rebuild is
> automatic on first use. Measured on this repository's own Rust source, the
> documented-symbol count went from 786 to 1100 (+39.9%) across an unchanged 2459
> nodes, with all 6004 edges unchanged — comparing the two indexes' full
> (source, target, relation) sets gives a symmetric difference of zero rows, so
> nothing but `doc_comment` moved. Nothing else to do.

### A decorator between a declaration and its doc comment no longer hides it

Documentation lookup walks backwards from a declaration to the comment above it.
That walk stopped at the first node that was not a comment, and the wrapper climb
above it insisted the declaration be its parent's literally-first named child. So
anything sitting in between — a decorator, an attribute, an annotation — cut the
channel, and the symbol was indexed with no documentation at all.

Which languages this hit is a property of their grammars rather than of their
decoration syntax, which is why it stayed hidden: Java, Kotlin and Swift park
annotations inside the declaration's own `modifiers`, C# and PHP inside an
`attribute_list` field, and Python has a `decorated_definition` wrapper that was
already handled. In all of those the comment stays the declaration's immediate
previous sibling and nothing was ever wrong. The four that spell the decoration
as a SIBLING all lost their docs:

- **TypeScript and JavaScript** (`decorator`). `@Component({}) export class C {}`
  puts the decorator inside the export statement ahead of the declaration, and
  `@Get() findAll() {}` sits directly between the comment and the method. That is
  the Angular and NestJS shape — in those codebases it is most of the documented
  declarations in the project.
- **Rust** (`attribute_item`). Every `#[derive]`d struct and every `#[inline]` or
  `#[test]` function, this repository's own included.
- **Dart** (`annotation`).

Because a JSDoc or `///` block lives OUTSIDE the node, `code_content` does not
carry it either — so a phrase that appeared only in the documentation of a
decorated symbol was unreachable through every channel at once: no FTS hit, no
vector, nothing to show in `get_ast_node`. Search for a concept that your team
only ever wrote down in a docblock above `@Injectable()` and the answer was
silence.

One mis-attribution goes the other way in the same pass: a Rust `//!` or `/*!`
block documents the module that contains it, and it was being handed to that
module's first declaration as if the declaration had written it. That predates
this release — the walk cannot tell `//!` from `///` by node kind — but stepping
over attributes widened it to the `//!` header + `#[derive(…)]` + type layout
that is ordinary Rust, so it is fixed here.

The node shapes were read off real parse trees rather than inferred from the
grammar documentation, which is also how two of them turned out NOT to be gaps
after all. The parity table guarding this now has a second axis — 13 rows across the 10
languages that have decoration syntax at all (Go and Ruby have none), keeping the
already-working languages as controls so the table covers the whole axis instead
of the half that broke.

### The server no longer dies on a deeply nested source file

`walk_for_relations` recurses once per AST level, so an 800-byte file of nested
parentheses drives it to its depth cap. The thread the MCP server runs startup
indexing on took `std::thread::spawn`'s 2 MiB default, and the peak for that
input measures 2–4 MiB unoptimized against 256–512 KiB optimized. A stack
overflow is an abort rather than a panic, so it walks straight past the serve
loop's per-request `catch_unwind` and takes the whole session with it — which is
what a development build did: the server died with SIGABRT and left the index at
zero files. Release builds survived on a roughly fourfold margin, but that margin
was something the optimizer happened to buy rather than anything the code asked
for, and the walker's frame width changes whenever its dispatch is refactored.
The thread is now sized explicitly and a test holds the size to the measurement.

A failed thread spawn also used to strand the server: the guard that clears the
"indexing in progress" flag and wakes waiters lives inside the closure, which a
failed spawn drops, so the flag stayed set and every later query waited on an
index run that would never start. That path now cleans up after itself.

### Only a real conflict means the index lock is held (Unix)

The non-Unix lock probe has always treated exactly one condition as "somebody
holds this" and everything else as a non-answer, because callers turn a "held"
into a refusal. The Unix probe never got that treatment: it read ANY `flock`
failure as held. `flock` reports a genuine conflict as `EWOULDBLOCK` and nothing
else does — its other failures are a signal arriving mid-call, the kernel running
out of lock records, or a filesystem that does not implement `flock` at all,
which some network and FUSE mounts do not. On any of those, `rebuild-index`
refused to run and told you another process held a lock that nobody held.

### `project_map` in compact mode stops calling populated modules empty

The full module envelope grew an `other` count because a docs-only or types-only
module read as `functions: 0, classes: 0` — empty — while `module_overview`
listed its symbols perfectly well. Compact mode kept only path, files and
functions, so it went on saying exactly that, and said it for any package that is
entirely classes or constants too. Compact now carries the counts when they are
non-zero, so a plain code module pays nothing and a directory full of classes
stops looking like a directory worth skipping.

## v0.121.0 (2026-08-18)

> **Upgrade note — the `outcome` numbers move, because they were wrong.** Five
> defects in the retrieval-adoption metric are fixed here and three of them change
> figures you have already seen: adoption goes DOWN (a file read in the SAME
> assistant message as the query no longer counts as having adopted its result),
> the field-MRR denominator goes UP (CLI `ast-search` calls now enter it at all),
> and calls made through the short `code-graph` bin name or a Windows `.cmd` shim
> now count where they were invisible. Numbers from before this release are not
> comparable with numbers after it. Nothing to do — and **no index rebuild**:
> INDEX_VERSION stays at 63.

### The Windows index lock is a handle now, not a PID file

The non-Unix lock was the lock FILE's existence plus a recorded PID. Acquisition
`create_new`'d it, and on `AlreadyExists` read the PID, probed it with
`tasklist`, and deleted the file if the holder looked dead. Two processes
starting together both read the same dead PID, both decided to reclaim, and the
second one's `remove_file` deleted the lock the first had just created: both then
held "the lock" and indexed one database as two primaries. The delete was
unconditional, so no amount of re-checking before it closed the window — there is
no atomic "unlink only if this is still the file I inspected".

Mutual exclusion is the OS's job now. The lock is an open handle with
`share_mode(FILE_SHARE_READ)`: a second acquisition asks for write access, which
that share mode refuses, and the kernel drops the handle when the holder dies
however it dies. The race goes away with its whole supporting cast — no liveness
probe, no stale lock to reclaim, and no permanent-secondary mode after an unclean
exit, which until now meant a crashed server left every later instance for that
project read-only (no indexing, no watcher, `rebuild-index` refusing) until
someone deleted `.code-graph/index.lock` by hand. A leftover lock file is inert.

`release_index_lock` is a no-op on both platforms now, for the same reason stated
two ways: on Unix the flock lives on the inode, so unlinking hands the lock to a
different inode; on Windows the delete WAS the racy reclaim step. Two smaller
things fall out — a lock held by THIS process used to read as free on Windows
(the probe compared PIDs), and the `tasklist` probe with its timeout plumbing and
five tests is gone, having guarded a mechanism that no longer exists.

### Five ways the adoption metric was lying

Each of these kept the retrieval-adoption numbers plausible while wrong, which is
the worst failure mode a metric has: a shrunken denominator still renders as a
confident percentage.

**CLI `ast-search` never entered the field-MRR denominator.** The ranked-tool
list was hand-written in two spellings, and a CLI event is named
`<canonical>_cli` — where the canonical form of `ast_search` is the HYPHENATED
`ast-search`. The entry `ast_search` therefore covered the MCP call and missed
every `ast-search_cli` one, dropping those calls from MRR and discarding their
rank. The list now holds MCP spellings only and derives each CLI twin through
`canonical_query_cmd`, the one table both surfaces already share.

**A file touched in the same assistant message as the call counted as adoption.**
The model batched them; the Read was decided before the result existed. It
inflated adoption and, because such a touch is always the first one after the
call, piled into the `d1` bucket — corrupting the very histogram used to argue
the attribution window is tight. `FileTouch` now carries its turn, and a
same-turn touch is skipped entirely.

**Calls through the short bin name or a Windows shim were invisible.**
`package.json` publishes TWO bins, `code-graph` and `code-graph-mcp`; npm writes
`.cmd`/`.ps1` wrappers beside them on Windows. Only the long name and `.exe` were
recognised, so `code-graph callgraph X` — the spelling people actually type —
never reached the conversion metric.

**`--project /repo/` answered `state: absent` with exit 0.** The trailing
separator survives normalization and slugifies to a transcript directory Claude
Code never created — a typo's worth of difference between "no data" and "you
asked wrong", and shell tab-completion supplies that slash for free.

**Transcripts that could not be read were skipped in silence.** N shrank and the
run still printed its rates as findings. They are counted now and disclosed on
their own line BEFORE the numbers, with an `unreadable` field in `--json` — and
so is a transcript DIRECTORY that cannot be enumerated at all, the same silent
zero one level up, which the pre-tag review caught still open after the per-file
fix.

### The `calls` axis is a table

Twelve arms of `walk_for_relations`'s giant `match` moved into
`src/parser/relations/calls.rs` as `CALL_PASSES`, one row per (language, node
kind). That match is where this crate's top recurring bug class lives: one arm
per language per relation, where a missing arm is not a compile error but an edge
that is silently never emitted — and tree-sitter guarantees the arms, because no
two grammars agree on what a call node is called (`call_expression` / `call` /
`method_invocation` / `invocation_expression` / three PHP kinds sharing one arm /
a Dart `selector` / a Bash `command`). As data the mapping is enumerable: a new
language's absence is a visible empty slot, `call_passes_wire_every_extractor`
fails on an extractor no row names, and `table_tests` rejects two rows claiming
one (language, kind) slot — the duplicate a `match` would have refused to
compile. `walk_for_relations` drops from 1,236 to 647 lines; the recursion and
its scope/class/impl propagation stay exactly where they were.

**This changes nothing about what gets extracted**, which is the whole claim and
was verified rather than assumed: 4,732,129 relations over 48,539 files — this
repo plus six third-party checkouts covering Go, Ruby, PHP, Java, C#, Dart,
JavaScript, TypeScript, Python and Bash — are byte-for-byte identical before and
after. Hence no INDEX_VERSION bump; the extraction fingerprint is re-recorded at
63.

## v0.120.1 (2026-08-17)

> **Correction to the v0.120.0 upgrade note.** That note told you to pin
> `@sdsrs/code-graph@0.119.0` to keep the old leave-it-behind uninstall
> behavior. That does not work and never did: the teardown runs from the
> plugin-cache scripts wired into `settings.json`, not from the npm global
> package, so pinning npm changes nothing for a plugin user — and an npm-only
> user never reaches this code path at all. There is no opt-out; what there is
> instead is the notice below, so you can see exactly what was touched.

Repairs found by an independent review of the v0.120.0 diff. The review
completed before the tag was pushed; its report did not reach the author until
after publish, so these ship as a patch rather than as part of v0.120.0.

**The uninstall sweep deregistered projects it had failed to clean.**
`unadopt()` called `removeAdopted()` unconditionally — after the "could not
rewrite CLAUDE.md" flag was already set. On a project whose file cannot be
written (root-owned from a stray `sudo`, a read-only mount, an EPERM directory)
the managed block stayed put *and* the registry entry went away. Harmless while
the only caller passed the current directory; load-bearing since v0.120.0, where
the uninstall sweep walks the whole list: every failure emptied the registry a
little more, and `removeCacheResidue()` only preserves a NON-EMPTY one, so the
file died with the cache directory and `code-graph-mcp uninstall --unadopt-all`
— the documented recovery — had nothing left to find. A project is now
deregistered only when it was actually cleaned.

**An unusable registry is no longer read as an empty one.** The sweep called
`readAdoptedProjects()`, which collapses unreadable / truncated / wrong-shape
into `[]` — indistinguishable from "nothing to do". A corrupt registry therefore
swept nothing, said nothing, and was then deleted along with the cache. It now
reads through `readAdoptedResult()`, skips the sweep on `unusable`, and
`removeCacheResidue()` preserves the bytes: the file we can least reconstruct is
exactly the one that must survive.

**The sweep says what it did.** Rewriting `CLAUDE.md` across several
repositories with no message meant the first sign was unexplained diffs in
`git status`. It now prints one stderr notice naming the count and the paths it
cleaned, plus any it could not, with the manual command for those. Both callers
`process.exit(0)` immediately afterwards, so this line is the only channel there
is.

## v0.120.0 (2026-08-17)

> **Upgrade note — `/plugin uninstall` now cleans up after itself.** Uninstalling
> the plugin strips the managed `<!-- code-graph-mcp:begin -->` block from every
> project's `CLAUDE.md` and deletes the generated
> `.claude/plugin_code_graph_mcp.md`, instead of leaving both behind forever.
> Your own prose outside the sentinel is untouched, and a `CLAUDE.md` that holds
> nothing but our block is removed (the plugin created it). A temporary
> **disable** in `/plugin` still changes nothing — the block, the 40MB binary
> cache and the embedding model all survive, so re-enabling costs no download.
> No index data is touched: `.code-graph/` stays. Nothing to configure; to keep
> the old leave-it-behind behavior, pin the previous version:
> `npm install -g @sdsrs/code-graph@0.119.0`.

A sandbox lifecycle pass driven from a real user's path — a throwaway `HOME` +
`CLAUDE_CONFIG_DIR`, the plugin laid out the way `/plugin marketplace add` and
`/plugin install` lay it out, a JSON-RPC stdio client where Claude Code would be,
and real network for the release binary, the embedding model and `npm install -g`.
Install, auto-update, self-heal, disable, uninstall, reinstall, each verified by
what was left on disk afterwards.

**npm does not always hoist the platform binary, and we only looked where it
sometimes is.** `npm install -g @sdsrs/code-graph` puts
`@sdsrs/code-graph-<plat>-<arch>` in one of two places — hoisted next to the
shell package, or nested inside it — and which one you get is an npm
implementation detail (npm 12 nests; both layouts were live on one machine).
`find-binary.js` probed only the hoisted spelling, so a **successful** install
read as "npm install did not yield a binary". Two consequences, one of which
outlived the session:

- The launcher fell through to the GitHub release download and pulled ~41MB it
  already had. Cold-start handover from the 0-tool stub to real tools: **17.7s →
  3.0s** measured in the sandbox.
- `recordGlobalInstall()` never fired, so `global-install-marker.json` was never
  written — and that marker is the only thing that proves the plugin, not the
  user, installed those global packages. Without it `code-graph-mcp uninstall`
  refuses to remove them and `doctor` reports "no plugin-install marker;
  uninstall leaves them", which is now "plugin-installed; … removes them".

Discovery probes both layouts (global prefix and npx cache alike). The global
package *inventory* deliberately still looks only at the top level: a nested
optional dependency is the shell package's private business, and `npm install -g`
/ `npm uninstall -g` on its name would create or orphan a top-level global the
user never asked for.

**The uninstall teardown was wired to the branch that no longer runs.** Unadopting
a project lived in `session-init.js`'s inactive branch — whose own comment
concedes that after a real `/plugin uninstall` "this SessionStart usually never
runs again", because Claude Code stops loading the plugin's `hooks.json` the
moment the install record disappears. What *does* still run is the composite
statusline render, and it only cleaned `settings.json` plus `~/.cache/code-graph`.
Measured with two adopted projects: 129MB of cache reclaimed, hooks and statusline
gone — and both `CLAUDE.md` blocks still sitting there, steering Claude at a CLI
that had just been deleted. `cleanupDisabledStatusline()` now sweeps the whole
adopted-projects registry *before* wiping the cache the registry lives in, with
per-project and overall error containment (it runs inside a statusline render,
where a throw blanks the user's status bar).

**Two messages that described the wrong situation.**

- The first SessionStart after `/plugin install` said "Binary not found — MCP
  server cannot start. Install: npm install -g @sdsrs/code-graph". A missing
  binary is the *normal* first-session state — nothing ships the engine with the
  plugin — and both automatic install paths are already running when that prints.
  It now says it is fetching in the background; the manual instruction is kept
  for `CODE_GRAPH_NO_AUTO_UPDATE=1`, where nothing else will fetch it.
- `auto-update check` printed `Up to date (v<installed>)` when the GitHub fetch
  had failed or another session held the install lock — telling a user who ran it
  *because* they were stuck on an old version that the old version is current
  (observed: "Up to date (v0.118.0)" with v0.119.0 published). Both paths now
  return a reason and print "update status UNKNOWN" or "another session is
  installing". The exit code stays 0: an unreachable GitHub is not a failure of
  the command, and both the launcher and `doctor` spawn it.

Two long-standing tests turn out to have been passing for the wrong reason. Both
assert that discovery finds nothing, and both were only true because the probe
missed a global install that was really there; `globalNodeModulesCandidates()`
derives one root from `process.execPath`, which no env var can mask. They now
skip with the offending path printed, so a developer machine cannot quietly
report a green that a CI runner (no global install) still earns.

## v0.119.0 (2026-08-17)

> **Upgrade note — your index rebuilds once.** `INDEX_VERSION` moves 62 → 63
> because doc-comment extraction changed, so the first `code-graph-mcp` run after
> upgrading re-indexes the project from scratch (the usual one-time cost; queries
> stay available throughout — structure lands first, embeddings backfill after,
> and that backfill now prints progress instead of sitting silent). Nothing to
> configure. To stay on the old graph, pin the previous version:
> `npm install -g @sdsrs/code-graph@0.118.0`.
>
> One behavior change can break a script: `--type module` / `--node-type module`
> now **exits 1** with a pointer to `map` / `overview` / `tour` instead of exiting
> 0 with no results. It never matched a row on any surface — see below.

End-to-end dogfood pass driven from a fresh user's path — index a polyglot repo,
then run every subcommand and the MCP stdio surface against it — plus the edge
shapes a real checkout has (no git, docs-only, monorepo with `.gitignore`,
symlinks, CRLF, unicode and spaced paths, a 20k-symbol file, concurrent runs).

**Filters that accepted a value and then could not honor it.** The `--type` /
`node_type` vocabulary is one constant (`domain::TYPE_FILTER_VOCAB`), and a
2026-08-16 guard already pinned "every advertised word parses" — which three of
them did while resolving to node types no row carries:

- `--type type` mapped to `type_alias`; the extractor writes `type` for a TS
  `type X = …`. Every `ast-search --type type` / `search --node-type type` /
  MCP `ast_search {type:"type"}` was a guaranteed zero-hit on an index holding
  the aliases.
- `--type var` mapped to `variable`, which nothing emits — a top-level
  `export var`/`let` binding is stored as `constant`. Now maps to both.
- `--type module` is no longer advertised: `is_skippable_result` (search /
  ast-search / similar) and the dead-code SQL's `n.name != '<module>'` drop the
  placeholders unconditionally, so it could only return nothing. It is rejected
  with a pointer to `map` / `overview` / `tour`, which do list modules
  (`domain::type_filter_note`). The MCP `ast_search` schema stopped advertising
  it to the model too, and now derives that description from the constant.

The guard now walks through to the extractor's own node-type list instead of
stopping at "the parser accepted it".

**Output that misread as two problems, or as a hang.**

- The filter-emptied disclosure ("N candidate(s) matched … removed by the active
  filter") went to stdout *and* stderr in human mode — one terminal, two copies,
  in two different wordings. stderr now carries it only under `--json`, where
  stdout is a machine envelope.
- The embedding backfill ran with no output at all: on a large repo the CLI
  printed its "Incremental index: N files updated" summary and then sat silent
  for minutes, which is what a hang looks like. It now announces the backlog and
  ticks every 3s once there are ≥500 nodes to embed.
- `dead-code` disclosed the `--min-lines` cut only when the result set was empty,
  so a report listing three candidates silently omitted every shorter one. Both
  surfaces name the active threshold.
- `similar` called the candidates past `--max-distance` "nearer", the opposite of
  what the count measures.
- `affected --depth 0` / `--depth 999` clamped in silence while `callgraph`
  warned about the identical clamp.
- `refs` and `callgraph` printed the internal `<module>` sentinel as if it were a
  symbol in the user's file; human output now reads `(file top level)`. `--json`
  and MCP keep the raw name.
- Hot-function, centrality, report and impact counts said "1 callers" — the
  `plural` helper exists for exactly this and four sites did not call it.

**`--help` and usage lines that were not runnable commands.**

- `parse_args_json_aware` handed clap `args.iter().skip(1)`, so the *subcommand*
  token became clap's program name: every subcommand rendered
  `Usage: search [OPTIONS] <QUERY>`, silently overriding the
  `name = "code-graph-mcp <sub>"` all two dozen Args structs set. Same in
  `parse_grep_args`.
- `print_help` listed subcommand-local flags under a bare `OPTIONS:`, which reads
  as "placeable before the subcommand" — and `code-graph-mcp --json search foo`
  answered "Unknown subcommand: --json". The heading says where they go, and a
  flag in the subcommand slot now gets told so (exit 2).

**Machine contracts.** `snapshot inspect` always prints JSON and takes no
`--json`, which excluded it from the tier-3 error leg: a corrupt or missing
snapshot — the one thing the command exists to detect, from a script — answered
with zero bytes on stdout. It now emits `{"error": …}` there. MCP `ast_search`
also accepts `node_type` as an alias for `type` (its sibling
`semantic_code_search` spells it that way), and no longer reports the argument as
ignored in the same response that applied it.

**`map` counted only four node-type buckets**, so a markdown-only or types-only
module read as `0 symbols` in the project map while `overview <path>` listed its
symbols. The census now includes everything outside the named buckets.

### Doc comments were missing for most of the supported languages (INDEX_VERSION 63)

`doc_comment` had no per-language guard, and the languages that DID work made the
column look populated. Four independent gaps, found by sweeping every supported
language against a real parse tree:

- **TS/JS `export`.** A JSDoc block precedes `export function f(){}` as a whole,
  so it is a sibling of the `export_statement`, while the sibling walk started at
  the inner declaration and found the `export` keyword. Exported symbols were
  undocumented; plain functions and class methods (unwrapped) kept their docs. In
  TypeScript the exported symbols are the documented ones — and unlike a Python
  docstring the block sits OUTSIDE the node, so `code_content` did not carry it
  either: a phrase living only in a JSDoc was unreachable by every channel
  (`search "issuer allowlist"` → no results). **Still open:** a comment above a
  DECORATED export (`/** … */ @Component(…) export class X`, the Angular/NestJS
  idiom) is separated from the declaration by the decorator node and remains
  undocumented.
- **Python.** Documents with a docstring, not a preceding comment, so the column
  was empty for any def whose docstring was its only documentation. (A def with
  an adjacent `#` comment always carried that comment, and still does — the
  docstring now takes precedence over it, so a file-level copyright or
  `# pylint:` header no longer lands in the `doc:` slot of the first function in
  the file.) The docstring text is inside `code_content` so FTS still reached it,
  but the embedding context builder ranks `doc:` above `code:` precisely because
  code is what gets truncated at 512 tokens — a long function's docstring was the
  first thing dropped from its vector.
- **Dart.** Its grammar calls the `///` block `documentation_comment`, a spelling
  the three-name allowlist did not carry, so every Dart symbol was undocumented.
  The check is a `*_comment` suffix match now.
- **Go `type`/`const` and Ruby methods.** Two more wrapper shapes: the extractor
  sees Go's inner `type_spec` (whose only preceding sibling is the `type`
  keyword) and Ruby's `method` inside a `body_statement`.

A comment that TRAILS code (`func F() {} // note`, `class R # note`) is no longer
read as the next declaration's documentation — widening the walk to wrappers had
made Go's trailing comment the doc of the following `type`, and Ruby's
`class X # note` the doc of that class's first method.

Measured on a 95-file third-party TS/Vue checkout: documented symbols
**264 → 335 (+71, +26.9%)**, with the edge set byte-identical at 36,671 rows —
this changes `doc_comment` only. `test_doc_comment_parity_across_languages` pins
the (language, declaration form) axis, and mutation testing confirms it fails
per-row when either mechanism is reverted. INDEX_VERSION bumped to 63: existing
indexes carry the empty column until they rebuild.

Known gaps this does NOT close, found by the pre-tag review and left open
deliberately: the decorated-export case above; MCP `project_map {compact: true}`
still reports only `{path, files, functions}` per module, so a docs-only or
types-only directory reads as `functions: 0` there even though the non-compact
envelope now carries `other` (the compact whitelist already dropped `classes`,
so this is the pre-existing trim, not a new gap).

## v0.118.0 (2026-08-16)

Continuation of the 2026-08-16 audit remediation — the §十 "中期" tier, then the
§四 P2 tail.

**§四 P2 tail — all seven clusters closed.** Roughly forty items, each verified
against HEAD before being touched; eleven turned out to be already fixed and are
recorded as such rather than re-fixed. Highlights, by what they cost a user:

- *Silent wrong answers.* `similar new` answered about one arbitrary definition
  out of five while `callgraph new` reported the ambiguity; `get_ast_node` kept
  its own ambiguity copy that never filtered the `<external>` sentinel;
  `show --impact` and `impact` reported different RISK levels for the same
  symbol; MCP `trace` put inline `#[cfg(test)]` helpers in a route's production
  call chain.
- *Machine contracts that lied.* A clap parse error under `--json` produced zero
  bytes on stdout; `grep --json` used the success-shaped `[]` on all five of its
  error legs; `search --json` never disclosed AND→OR degradation; `search x`
  reported "no results" for a query that never reached SQL;
  `health-check --format jsonn` printed prose and exited 0.
- *Ranking.* A `--language` filter made a query 4× more likely to discard its
  precise results, because the AND→OR threshold keyed on the internal
  over-fetch pool rather than on what the user asked for.
- *Index/storage.* Unix lock release no longer unlinks the lock file (inode-scoped
  flock — deleting it allows a second primary); `open_readonly` refuses a future
  schema with the marker the statusline keys on; the query-time refresh stops
  re-hashing files the indexer will never parse; the `<external>` reaper's gate
  now matches the prune that orphans sentinels.
- *Plugin.* `doctor` no longer exits 1 for the life of an install over advisory
  warnings; injected context is capped at the one place all three hooks emit
  through; a reinstall no longer resurrects the previous install's statusline
  entry; `prepare` stopped writing `.git/config` on every `npm pack --dry-run`.
- *Tests.* Two tests that were vacuous in every CI leg — measured, not inferred —
  now assert what they are named for.
- *Vocabularies.* `exports`/`routes_to` are filterable (they were visible in
  results and refused by name), `module` is advertised as the type filter it has
  always been, and `--min-confidence ""` means the same thing on every surface.

Fourteen new guards ship with these so the same drift cannot recur; three caught
real problems on their first run, including in this batch's own new code.

Deliberately not done, with reasons recorded in the code: the unbounded deferred
buffer, the per-batch name-map clone (unmeasurable on a 234-file repo), the
array-shaped commands' `freshness_partial`, the Windows hook existence guard, a
declared `rust-version`, and table-driving the calls axis — a parity probe found
all sixteen call-bearing languages already emit the edge, so the refactor would
close no gap and a guard was added instead.

**Upgrade notes**

- **Requires an index rebuild** (`INDEX_VERSION` 61 → 62, automatic on the next
  server start, one-time). Existing indexes are missing every inheritance edge
  listed under "heritage axis" below, carry unqualified Go method names, and hold
  a phantom `enum inherits <integral type>` edge for each C# enum. The version
  bump is the only rebuild trigger, so nothing else corrects them.

- **Three output shapes changed. If you script against them, read this.**
  - `grep --json` now emits `{"error": "…"}` on its FAILURE legs (unsupported
    flag, path outside the project, ripgrep missing, invalid pattern) instead of
    the success-shaped `[]`. A genuine zero-match run still returns `[]` with
    exit 1 — that part is unchanged. A consumer doing `JSON.parse(out).length`
    now gets `undefined` on an error rather than `0`, which is the point: the
    two were byte-identical before, so `--json 2>/dev/null` reported "no matches
    in this repo" for a typo'd flag.
  - `health-check --format <unknown>` now fails with exit 1 instead of silently
    printing the human one-liner and exiting 0. A script asking for JSON and
    getting prose with a success code had no way to tell.
  - A clap argument error under `--json` now writes a JSON error object to
    stdout (it previously wrote zero bytes and exited 2).
  - `doctor`'s exit code no longer counts advisory rows — a binary deliberately
    built without `embed-model`, or npm relics under a node version the tool
    cannot reach, no longer pin it at 1 forever. If you gate automation on
    `doctor && …`, it will now proceed in those states; genuinely broken checks
    still exit 1.

- **Reverting**: nothing here changes on-disk formats other than the index,
  which is a rebuildable cache. To go back, pin the previous version
  (`npm i -g @sdsrs/code-graph@0.117.0`, or `npx @sdsrs/code-graph@0.117.0`) and
  run `code-graph-mcp rebuild-index --confirm` once; the older binary refuses a
  newer schema by design, and this release does not move `SCHEMA_VERSION`
  (still 10), so the downgrade is clean.

### Fixed
- **The heritage axis matched three hard-coded node kinds, so six languages
  emitted ZERO inheritance edges for anything that is not spelled `class`.** A
  Java `interface`/`enum`/`record`, a TypeScript `interface`, a PHP `interface`,
  a Kotlin `object`, a Swift `protocol` and a Dart `enum` all carry heritage and
  all produced nothing — nothing failed, the graph was simply incomplete, so
  `find_dead_code` reported an interface's implementers as unused and every
  heritage traversal under-reported. Declaration kinds now come from a table
  (`HERITAGE_DECL_KINDS`) whose every row was read off a real parse, and three
  heritage-child spellings no extractor read (`extends_interfaces`,
  `extends_type_clause`, Dart's `interfaces`) are handled.

  Measured on external corpora, not fixtures: **okio +14 `inherits`, gson +3
  `implements`, moshi +1 — and 0 edges removed in all three.** Each new edge was
  confirmed against the source (`public enum FieldNamingPolicy implements
  FieldNamingStrategy`, `object NodeJsFileSystem : FileSystem()`, …).
- **Go methods did not carry their receiver, so two types' same-named methods
  were one indistinguishable symbol.** Go declares methods at file scope with the
  owner in a receiver rather than by nesting, so `qualified_name` was always just
  the bare name and `callgraph Start` silently merged the callers of
  `Server.Start` and `Client.Start` — with the ambiguity folded out by the
  default confidence floor, so the answer looked clean. Measured on gorilla/mux:
  **17 bare method names were shared by more than one method before; 0 qualified
  names are shared after**, with the edge set byte-identical (0 added, 0 removed)
  — this is a naming fix, not a graph change.
- **A C# `enum E : byte` emitted a phantom `E inherits byte`.** C# spells an
  enum's underlying integral type with the same `base_list` syntax a class uses
  for its base type. A phantom edge bound to a real node is worse than a missing
  one; the arm is now gated on the parent declaration kind.

The cross-batch differential fixture gained a pair for each newly-covered
language, and the equality check is backed by positive presence assertions —
without them a new axis compares empty to empty and reads as verified.

### Fixed
- **`impact <Class>.<method> --file <path>` endorsed a class that does not exist.**
  `--file` is a narrowing flag, but the resolver returns early when it is present
  and hands the rest of the command the bare method name — so `impact Gamma.run
  --file two.ts` matched `Alpha.run` living in the same file and answered
  `{"risk":"LOW"}` exit 0. That is the same safety-endorsement-for-a-typo shape
  P1-9 closed for wrong *paths*, still reachable through a wrong *qualifier*.
  A qualified input is now checked against `qualified_name`, the miss echoes what
  the user typed rather than the stripped name, and when the file legitimately
  defines the bare name more than once the answer says so — the qualifier narrows
  the lookup, not the caller traversal.
- **`dead-code src` reached into the sibling directory `src2/`.** The report
  filtered with an unanchored `f.path LIKE 'src%'` while the empty-result probe
  (`unindexed_path_prefix`) used a `/`-boundary match, so the two halves of one
  command disagreed about what "under this path" means. Both are boundary-anchored
  now (`f.path = 'src' OR f.path LIKE 'src/%'`); an exact file path still matches.
- **One unreadable cache file switched off all three of the updater's give-up
  budgets.** `readState()` was `readJson(STATE_FILE) || {}` — the lossy-read
  shape the audit swept for on settings.json, still live on the file that holds
  the update suspension, the binary self-heal budget and the GitHub rate-limit
  backoff. Collapsing "could not read it" into "fresh install" re-armed all
  three at once, so a corrupt or `chmod 000` state file silently restored the
  unbounded retry loops each of them exists to stop. Only ENOENT (or an empty
  file) now reads as fresh; anything else skips the session's update work and
  rewrites a clean state file, so recovery is bounded rather than immediate.
- **`doctor` could not name the one parked state a broken binary comes from.**
  `autoUpdateNoOpReason` covered the opt-out, the update suspension and the
  rate-limit backoff, but not an exhausted *binary* self-heal — so a user with a
  `binary-broken` row was told to update manually with no hint that the automatic
  repair had already given up. It now reports that too, via the updater's own
  predicate, which re-arms when a newer release appears.
- **Two disclosures were being computed and thrown away.** `install()`/`update()`
  have returned `manifestUnwritable` and `adopt()` has returned
  `registryRecorded` since each stopped throwing, and nothing read either.
  Neither is cosmetic: an unwritten manifest makes `syncLifecycleConfig` re-run
  install() on every SessionStart forever, and an unrecorded adoption means
  `/plugin uninstall` never strips the managed block from that project's
  CLAUDE.md — with no plugin code left to do it later. Both are reported now,
  each with the consequence, and a control test proves the lines stay quiet on a
  clean run.
- **`ast_search` dropped a disclosure whenever two hints applied at once, and the
  two surfaces dropped different ones.** Each surface assigned its `hint` field
  from several independent `if` blocks, so the last one executed won: for a result
  set that is both truncated and answered by the name-substring fallback (reachable
  — the fallback path carries its own `truncated`), the CLI kept only the fallback
  note and MCP kept only the truncation notice. Whichever surface dropped the
  truncation notice let a cut answer read as complete. Both now build `hint` from
  one ordered builder (`search::ast_query::hints`): why-it-is-empty, then the cut,
  then provenance — surface-specific wording (`--limit 20` vs `` `limit` ``),
  shared order. MCP's generic-fallback suggestion prepends instead of clobbering.

### Changed
- **The two upward module dependencies the audit found are gone, and a table now
  forbids them coming back.** `cli` borrowed the index-lock infrastructure from
  `mcp::server` (~285 lines with nothing to do with the MCP protocol) and
  `outcome` borrowed three generic helpers from `cli`; a third,
  `storage → search`, had grown since the last audit. Relocated downward:
  - `src/indexer/lock.rs` — `IndexLockGuard`, `acquire_index_lock_guard`,
    `other_process_holds_index_lock`, `try_acquire_index_lock` and the non-Unix
    PID-liveness probe, with their unit tests.
  - `src/utils/telemetry.rs` — `iso8601_now`, `rotate_jsonl_if_over`, the
    `JSONL_ROTATE_*` thresholds and `canonical_query_cmd` (shared funnel
    vocabulary: the CLI writes those names, `outcome` parses them back).
  - `src/utils/paths.rs` — `home_dir`.
  - `src/utils/{tokenizer,acronyms}.rs` — moved out of `search/`, which did not
    use them; the only consumers were `storage` and `indexer`.
  - `indexer::merkle` — `normalize_path_display_on`, now beside the separator
    rewrite it delegates to, so path spelling has one home.

  Public paths change accordingly (`code_graph_mcp::cli::home_dir` →
  `utils::paths::home_dir`, `cli::canonical_query_cmd` →
  `utils::telemetry::canonical_query_cmd`, `mcp::server::other_process_holds_index_lock`
  → `indexer::lock::other_process_holds_index_lock`, `search::tokenizer` →
  `utils::tokenizer`). No CLI flag, MCP tool schema or on-disk format is affected.

- **`src/cli.rs` (9,308 production lines) is now a module tree.** Pure mechanical
  relocation, no behaviour change: `cli/paths.rs` (project-root + user-path
  normalization), `cli/symbols.rs` (fuzzy/qualified symbol resolution),
  `cli/index_ops.rs`, `cli/health.rs`, `cli/usage.rs`, `cli/grep.rs`,
  `cli/freshness.rs` (the `refresh_files_if_stale` resync nine commands share)
  and `cli/commands/<name>.rs` — one file per subcommand. Largest file is now
  1,362 lines (`grep.rs`); `cli/mod.rs` is 170. Three source-scanning drift
  guards had to learn the new shape and were re-verified by mutation, not by
  their green result: `reader_nondestructive`'s destructive-constructor scan and
  `freshness_parity`'s resync scan now walk the whole tree, and the
  `cli → mcp` forbidden-edge row targets the directory.

### Added
- **`tests/hardening.rs` layering guard is now a 30-row forbidden-edge table**
  instead of the single `storage → graph` assertion it had been since M9a. That
  one edge stayed green while three others grew back unnoticed — a table makes the
  next one fail a test rather than an audit. Two supporting details, both learned
  the hard way in this change: the matcher strips string literals as well as
  comments (a parser fixture embeds `"… crate::snapshot::create() …"` as test
  data), and it anchors the module name at its end (a bare prefix match made
  `use crate::clippy_helper::x;` an offender for the `cli` row — caught by the new
  negative-control test, not by review). The guard is mutation-verified: injecting
  `crate::graph::…` into `src/storage/db.rs` turns it red.

## v0.117.0 (2026-08-16)

Remediation batch for the 2026-08-16 full audit (report kept locally — `docs/` is
gitignored in this repo): both P0s and the immediate/short-term P1 tiers, plus the
repairs two independent reviews of the batch itself turned up.

**Upgrade notes**

- **Requires an index rebuild** (`INDEX_VERSION` 60 → 61, automatic on the next
  server start, one-time). Existing indexes may carry the permanent edge loss
  P0-1 created, so the rebuild is the fix being delivered, not a side effect.
- **`semantic_code_search` now always answers an object.** The confident-hybrid
  path previously returned a bare JSON array; it is now
  `{"results": [...], "search_mode", "vector_available", "match_confidence"}`,
  matching what its other branches already emitted. A consumer doing
  `Array.isArray(payload)` must read `payload.results` instead. Nothing in this
  repo relied on the array shape.
- **Reverting:** pin the prior release — `npm install -g @sdsrs/code-graph@0.116.0`
  (or `cargo install code-graph-mcp --version 0.116.0`). Downgrading never wipes
  an index: the `INDEX_VERSION` comparison is directional, so a 0.116.0 binary
  reads a 61-stamped index without destroying it, and only re-stamps on upgrade.

### Fixed
- **P0-1: a file crossing the 1 MB / parse-timeout threshold aborted the whole
  index run and permanently destroyed its inbound edges.** Skipped files had
  their nodes purged but never left `global_name_map`, so the deferred pass
  resolved requeued relations to dead ids and the FK abort rolled back every
  other file's cross-file edges — while the batch commit had already stamped the
  file's new hash, so nothing ever retried (incremental permanently ≠ rebuild).
  Skipped files now prune the name map exactly like reindexed files, the
  deferred pass screens both id sources against live nodes instead of letting
  one dead id kill the savepoint, and a deferred-only run no longer skips
  confidence classification. Reproduced with two files pre-fix (`FOREIGN KEY
  constraint failed / 787`); regression fixtures carry ~50 filler files so
  SQLite rowid reuse cannot mask a dangling reference.
- **P0-2: the Edit-hook impact summary auto-approved the Edit it commented
  on.** `pre-edit-guide` delivered its context with `permissionDecision:
  'allow'`, which the hooks doc defines as "skip the interactive permission
  prompt" — on machines that ask before Edit, the plugin silently granted the
  write whenever the edited symbol had callers and the cooldown was cold. The
  hook now emits `additionalContext` with no permission decision (the doc's
  neutral path); read-only `pre-read-guide` keeps `allow`, and a drift test
  pins the allowlist to exactly that one file.
- **A killed or failed index run could leave file hashes durably current while
  their cross-file edges were never committed.** Cross-file relations buffer in
  memory until one deferred savepoint after the batch loop; a kill in between
  lost them with no detector. Runs now set an `index_run_in_flight` meta marker
  before the first commit and clear it after the deferred commit; a surviving
  marker escalates the next incremental to a full re-index. Deliberate
  amplification: an ordinary mid-run error also buys one full pass — the
  conservative answer when edge durability cannot be proven.
- **Snapshot install could silently resurrect the previous index.** `try_install`
  renamed a complete DB over `index.db` without clearing the destination's
  stale `-wal`/`-shm`, so SQLite replayed the old WAL over the fresh install
  (`integrity_check` still `ok`); the two CLI callers compensated, the MCP
  server path did not. The sidecar cleanup now lives inside `try_install`, so
  all three callers inherit it.
- **`affected` spent 14.7 s at its default depth to return the same answer the
  1 s depth-6 run gave.** Both recursive CTEs (file closure and call graph)
  guarded recursion with a per-path visited string — enumerating every simple
  path, exponential in depth (a synthetic 66-node graph never finished at
  depth 10). Both traversals are now an iterative BFS with a global visited
  set; 18/18 CLI probes byte-identical against the pre-fix binary, with the
  old CTEs kept verbatim as in-test differential oracles. `affected` default:
  12.2 s → 0.01 s on this repo.
- **`impact <symbol> --file <file-not-defining-it>` certified LOW risk, exit
  0.** The existence check ignored the file filter and the ambiguity guard was
  skipped whenever `--file` was given — a typo'd path produced a positive
  safety claim on the one command the steering table says to run before
  editing. Now errors with candidates + exit 1, matching its three siblings.
- **`semantic_code_search` swallowed its own disclosures on the most common
  response shape.** The confident-hybrid path returned a bare JSON array, which
  cannot carry `ignored_arguments` (a misspelled parameter vanished silently)
  or the `freshness` staleness note. Every path now answers the same
  `{results, search_mode, vector_available, ...}` envelope the other arms
  already used. **Consumers that assumed the bare-array shape must read
  `.results`** — in-repo consumers already handled both.
- **`search "db.execute"` (and every punctuation-joined query) was a hard
  zero.** The FTS sanitizer deleted punctuation inside a word, gluing
  `db.execute` into the nonexistent token `dbexecute`; the empty response then
  blamed the user's spelling. Punctuation now splits terms. The batch review
  caught the first version's regression — OR-fallback flooding garbage for
  single flag-shaped tokens like `--no-default-features` — so a single-token
  multi-fragment query gets one relaxed-AND retry (fragments absent from the
  index are dropped) and otherwise stays an honest empty instead of OR noise.
  A retry that dropped a fragment reports itself as a widened match, so
  `db.migratoin` takes the usual confidence penalty and prints the "AND match
  insufficient" note rather than reading as a precise hit.
- **`semantic_code_search` silently threw away its candidate pool.** The
  always-on module/external/test filter dropped up to 20 of 20 fetched
  candidates with no counter, no pool compensation, and an empty answer that
  suggested rebuilding the index. Triad drops are now counted, a widened
  second fetch runs when the filter starved the pool, and the empty message
  names what was actually filtered.
- **`ast_search` showed 3 of 39 real matches at the default limit and told the
  user to broaden the filter (the wrong remedy).** Both surfaces fetched a
  fixed `limit*4` then filtered; the MCP side also had a fallback the CLI
  lacked, so the two surfaces contradicted each other on the same query. One
  shared core now serves both, pool-sized by the same filter-aware widening
  semantic search uses, reporting `matched_total` + "raise the limit".
- **MCP `find_references` mixed ambiguous by-name collisions into rename
  audits with no way to tell.** It dropped the `confidence` tier the query
  already returned and offered no `min_confidence`; both added (additive
  schema change), with a `confidence_filtered` disclosure mirroring CLI
  `refs`.
- **MCP `find_dead_code` issued a clean bill of health for a path matching
  nothing in the index.** The CLI probe was lifted into a shared helper both
  surfaces call; the MCP tool now errors like its siblings instead of
  reporting "no dead code" for a directory it never examined.
- **The path-traversal guard test never called the guarded function.** It
  re-implemented the check inline (a tautology about `std::fs::canonicalize`);
  deleting the real guard left it green. Rewritten to drive `read_snippet`
  through the real dispatch with an out-of-root node and a positive control;
  mutation-verified red with the guard disabled.
- **The release gate ran a strict subset of CI.** The tag path never executed
  the `--no-default-features` test suite (what `cargo install` users build)
  before an irreversible npm publish; added to the gate and to
  `cache-warm.yml`'s warm job, with the drift guard extended to pin the new
  step. The gate's missing macOS/Windows legs are now an explicitly documented
  acceptance, not an accident.
- **Statusline registry destruction on an unreadable file — both halves.** The
  register path rebuilt the registry from `[]` when the file was unreadable
  (EACCES/corrupt ≠ absent) and persisted the wipe over the primary AND the
  durable backup, unrecoverably losing the user's previous statusline and any
  third-party providers; and the review caught the surviving sibling: the
  detach path made the same lenient read and deleted the user's `statusLine`
  slot. Both now refuse to act on an unusable registry. Same sweep converted
  `installed_plugins.json` and the adopted-projects registry off the lenient
  reader.
- **`doctor` emitted a repair id it had no repair for.** A broken binary — the
  most common real failure — printed "1 issue(s) found. Fixing..." then
  "0/1 addressed" with nothing in between. `binary-broken` now has a real arm
  (dev checkouts rebuild from source, end users re-download via the verified
  promote path), and a meta-guard asserts every emitted fixId has a handler.
- **A failed binary self-heal re-downloaded ~40 MB every session, forever.**
  The stale-binary path cleared its own throttle unconditionally and counted
  nothing; it now has a per-version attempt budget — while a MISSING binary is
  deliberately exempt (review catch: bounding the only recovery path would
  park a dead install permanently after five offline session starts).
- **`adopt`/`unadopt` rewrote user prose outside the managed block.** A
  whole-file `\n{3,}` collapse reflowed blank lines inside users' code fences
  on every SessionStart and reported "De-blocked" success on files that never
  held our block. Seam-healing is now anchored to the block's own removal
  site; untouched files come back byte-identical.
- **An unreadable or directory-shaped CLAUDE.md silently killed the rest of
  SessionStart.** `adopt()` threw uncaught (EACCES/EISDIR) with no top-level
  catch, so binary verification, index freshness and the hook self-test never
  ran. Adoption now reports instead of throwing, and `session-init` wraps its
  run so a failure in any one step cannot dark the rest.
- **One SIGTERM-trapping statusline provider hung the whole status line
  forever.** Node's `timeout` sends SIGTERM and waits. The statusline provider
  spawn, the statusline health-check and `doctor`'s health-check now kill with
  SIGKILL — scoped to those deliberately rather than defaulted globally, since
  hard-killing a timed-out `git pull` would orphan `.git/index.lock` and
  silently break marketplace refreshes.
- **`pr-impact-comment` reported a timed-out analysis as "covered".** A failed
  or timed-out `affected` spawn took the same branch as "zero affected tests";
  failures now render as their own "Not analyzed" section, and
  `CODE_GRAPH_FAIL_ON_RISK` also fails on unanalyzed files (an unmeasured file
  is unquantified risk).
- **README/steering drift.** Removed five slash commands deleted ~109 releases
  ago (`/understand` …), corrected schema v6→v10, the untestable "Rust 1.75+"
  claim (lockfile v4 requires newer; CI pins 1.95.0), the auto-update cadence,
  and the shipped steering template's claim that `impact_analysis` is still
  callable (it returns Unknown tool). The shipped CI template's actions are
  now SHA-pinned like the repo's own workflows, and the pin guard covers
  templates so they cannot float again.

### Added
- `tests/index_version_guard.rs`: a fingerprint trip-wire over the extraction
  and schema sources. Editing them without deciding about `INDEX_VERSION` /
  `SCHEMA_VERSION` now fails with instructions; regenerating the baseline is a
  visible act (the update run panics after writing, so a leftover env var
  cannot silently re-baseline). Coverage is the main extraction surface plus
  the file-selection seams — documented as a net, not a proof.

## v0.116.0 (2026-08-13)

The six items the v0.115.0 post-release review left open, in the order it ranked
them, plus the repairs an independent pre-tag review of the batch turned up.
Every entry carries a regression test that fails on the pre-fix code.

### Fixed
- **`trace 'DELETE /x'` claimed a Flask route that answers 405.** v0.115.0 made
  the stored `ANY` verb a matching wildcard, which fixed the false negative on
  `GET` by buying a false positive on every other verb. A bare `@app.route('/x')`
  now stores `GET` at extraction — Flask's and Starlette's own default — so the
  route matches the verb it serves and no others. **Requires an index rebuild**
  (`INDEX_VERSION` 59 → 60, automatic on next server start); old indexes keep the
  over-matching `ANY` rows. HEAD/OPTIONS on a bare `@app.route` remain unmodelled,
  as `methods=['POST','PUT']` → `POST` already was: the metadata schema holds one
  verb. No extractor emits `ANY` any more; `route_method_matches` still accepts it
  for Go `net/http`'s genuinely verb-agnostic `ALL`.
- **Two concurrent `rebuild-index --confirm` runs collided with a bare SQLite
  `disk I/O error`.** The pre-rebuild gate only *probed* the index lock (to catch
  a running MCP server), so two CLI rebuilds both read it free, both entered the
  `index.db.rebuild-*` temp sweep — which clears any other run's in-progress temp
  by design — and the loser died with an error nothing in it could be acted on.
  No data was ever at risk (the atomic rename saw to that). `rebuild-index` now
  holds that lock for its whole run, so the second one gets the existing
  explanatory refusal — whose text, along with the `incremental-index` warning
  that shares the probe, now names the concurrent-CLI case instead of blaming
  only the MCP server. `reindex --from-snapshot` holds it across the destructive
  window (the unlink plus the snapshot install) and releases before the indexing
  pass, which probes the same lock and would otherwise warn about this very
  process; a rebuild racing that last phase is still possible and is not claimed
  fixed here. A lock that cannot be *opened* (read-only dir, exotic FS) still
  proceeds unlocked, exactly as before — this gate must not be why a rebuild that
  used to work stops. On Windows, where the lock is a PID file rather than an
  flock, the CLI now removes it on release: a stranded dead-PID lock file would
  have refused every later rebuild and pushed every server start into secondary
  read-only mode.
- **`health-check` told offline users to restart the MCP server when a restart
  could not help.** The probe behind that advice only asked "is there a
  `model.safetensors`", but weights hand-placed in the *platform cache* dir carry
  no current `.model-id`, so the server re-downloads them on next start rather
  than adopting them. `health-check --json` gains `model_files_state`
  (`absent` / `unverified` / `ready`), and both the text arm and `doctor` route
  `unverified` to the advice that actually works: point `CODE_GRAPH_MODEL_DIR` at
  the weights. Weights found through `CODE_GRAPH_MODEL_DIR`, `cwd/models` or
  `exe/models` — the documented offline routes — are ungated and stay `ready`.
  The probe is O(1) (marker + companion `exists()`, never a hash), because
  health-check runs from hooks. `model_files_present` keeps its old meaning, so
  older plugin copies are unaffected.

### Added
- **MCP tools name back arguments they do not declare.** `ast_search
  {"language": "banana"}` silently dropped the filter and returned the whole
  repo; the caller is an LLM that could not tell that apart from a
  language-scoped answer, and it reported the wrong scope downstream. Results now
  carry `"ignored_arguments": ["language", …]` when a call passes members the
  tool's schema does not define. The call still succeeds — extra members are
  conventionally tolerated, and refusing would turn a mislabelled answer into no
  answer — but nothing is dropped in silence. Covers the seven schema-carrying
  tools plus the `read_snippet` alias; the hidden backends declare no properties
  anywhere, so they are skipped rather than checked against an empty allowlist.
  The published schema is not by itself the honored set, and the pre-tag review
  caught the inversion that follows from assuming it is: `get_call_graph`'s legacy
  `function_name` alias and the universal `skip_indexing` are both honored while
  undeclared, so the first version of this reported the argument that had just
  selected the caller's answer as ignored. Both are now exempt, and a drift guard
  (`test_no_new_undeclared_mcp_args`) pins the read-but-undeclared set so a new
  one has to be classified rather than silently mislabelled.
- **`--json` on `incremental-index`, `rebuild-index` and `reindex`.** It used to
  be a clap parse error, so CI had no structured way to learn what an index run
  did. One object on stdout per successful run — `mode`, `files_indexed`,
  `files_deleted`, `nodes_created`, `edges_created`, `files_with_parse_errors`,
  `elapsed_ms` — with progress and warnings left on stderr. `mode` names the path
  that actually ran (`full` / `incremental` / `rebuild`), not the subcommand
  typed, since `incremental-index` is a full index on a fresh checkout. The
  no-`.git`-anchor guard, which exits 0 without indexing, reports the same shape
  with zeroed counters plus a `skipped` reason rather than leaving stdout empty;
  failures keep the established `{"error": …}` object.

### Internal
- The all-commands EPIPE contract test now runs on Windows too, not just Unix.
  `std::io::pipe()` is cross-platform, and the Windows half — closed-pipe os
  errors 232/109, which the panic hook matches precisely because the message text
  is localized — had been reasoned about but never executed. Windows is in the CI
  matrix, so both arms now really run.

## v0.115.0 (2026-08-13)

Nine fixes from a three-round autonomous QA loop (every fix carries a
regression test that fails on the pre-fix code; the full evidence ledger,
replay transcripts and issue↔commit mapping live in the session's
`docs/looptesting/` records). Minor bump because two JSON surfaces gained
additive fields.

### Fixed
- **Piping a command into `head` or quitting `less` mid-stream panicked or
  printed a spurious error.** The EPIPE-silent contract (exit 0, no stderr
  noise) existed for `grep` and `stats` but nothing else: `health-check | head`
  died with a `failed printing to stdout: Broken pipe` panic, and
  `map | head` printed `Error: Broken pipe (os error 32)` through the anyhow
  return path. Two central hooks in `main()` now extend the same contract to
  every command — a panic hook that recognizes std's stdout-print failure
  (matching the Unix "Broken pipe" rendering and the Windows closed-pipe os
  error codes 232/109, which survive message localization), and a BrokenPipe
  check on the error chain that runs *before* the `--json` error leg (which
  would otherwise write into the same closed pipe). Scope note: the contract
  covers the stdout pipe; a *stderr* pipe closing early still reports, by
  design — stderr failures can signal real problems (full disk) that silence
  would hide.
- **`trace 'GET /health'` found nothing for routes that match every verb.**
  Flask `@app.route` without `methods=` is stored as `ANY` and Go `net/http`
  `HandleFunc` as `ALL`, but both trace surfaces (CLI and MCP) filtered by
  exact equality — so adding the verb you knew the route served made the route
  disappear, and the no-match hint then wrongly blamed framework coverage.
  A shared `route_method_matches` treats stored `ANY`/`ALL` as wildcards,
  case-insensitively on both surfaces (the MCP side had also drifted into
  case-sensitive comparison). Stored metadata is unchanged — no reindex needed.
  An explicit-method route still filters: `GET /submit` does not match a
  `methods=['POST']` route. Known approximation: Flask's real default for a
  bare `@app.route` is GET-only, so `trace 'DELETE /x'` now over-matches such a
  route (405 at runtime) — trading the old false negative on the verb the route
  *does* serve for a false positive on verbs it doesn't. Storing `GET` at
  extraction is the precise fix and needs an index-format bump; deferred.
- **`map` said "3 symbols" above a line naming four.** The module header's
  total counted functions, classes and interfaces/traits, but `key_symbols`
  also lists exported constants (`export const db`), so the count could be
  smaller than the list printed right under it. A `constants` bucket now feeds
  the total.
- **`health-check` claimed "no download has been attempted on this machine"
  while the model sat fully installed in `~/.cache/code-graph/models`.** The
  npm plugin installs weights without writing the binary's download marker, so
  the marker-based message contradicted the filesystem. A cheap fs-only
  presence probe now reports "model files present but not loaded in this
  process" — and the same fact travels through `health-check --json` as
  `model_files_present`, because `doctor` classifies from the JSON arm and was
  still printing the contradiction one surface over (the sibling-hole pattern
  this changelog keeps documenting).
- **`similar` on an embed-capable binary advised rebuilding with
  `--features embed-model`.** The empty-embeddings remedy now matches the
  running binary: an embed build is told to start the MCP server (which
  backfills embeddings); only the FTS-only build keeps the rebuild advice.
- **A symbol added after the last index read as nonexistent with no way out.**
  Query-time freshness can only re-sync files a symbol is already indexed in,
  so `show brandNewFn` printed a bare "Symbol not found". The no-candidate miss
  paths of `show`, `impact`, `similar`, `refs` and `callgraph` now add one
  stderr line pointing at `incremental-index` — on `callgraph` the hint fires
  only when the symbol is genuinely absent, not when it merely has no edges.
- **`deps` printed "(1 symbols)".** The existing `plural()` helper was never
  wired into the two per-file dependency lines.
- **`--help` overpromised `--json` "available on all commands".** It is not
  accepted by the index commands (`incremental-index`, `rebuild-index`,
  `reindex`), `doctor`, `adopt` or `unadopt`; the help now names the actual
  scope.

### Added
- `map --json` modules now carry a `constants` count (always); MCP
  `project_map` emits it when non-zero, same as `interfaces_traits`.
- `health-check --json` always emits `model_files_present` (bool).

## v0.114.1 (2026-08-03)

Documentation only — no behaviour change. The compiled binary's `.text` section
is byte-identical to v0.114.0's (the two binaries differ in 28 bytes total: the
ELF build-id, a metadata hash, and four embedded panic line numbers shifted by
the 40 comment lines added). The only non-comment edits are inside a test.
Released so the changelog that ships in the package is the accurate one.

v0.114.0 described the statistics change's downside vaguely, as "a synthetic
repository built for maximum same-name fan-out". That was too imprecise to act
on. The trigger has since been isolated: it is import **density**, not
repository size or fan-out on its own. The entry below now carries the measured
boundary, and the same measurements plus the root cause are recorded next to the
code so the trade-off does not have to be re-derived — including why
`PRAGMA analysis_limit`, the obvious-looking insurance, measures 13× *worse*
than what ships.

## v0.114.0 (2026-08-02)

Follow-up to the v0.113.0 audit remediation: the two places that batch left
unfinished, plus the reader-side half of an invariant this project has claimed
since the daagu incident.

### Fixed
- **A read-only command could delete your index.** `health-check`, which the
  statusline polls on every render, opened the database through a constructor
  documented as non-destructive — but that promise was only enforced for the
  `INDEX_VERSION` sweep. The two other wipes in the same function ran for
  readers as well, so an index whose header had been damaged (a crash mid-write,
  a bad sector) was deleted and replaced with an empty one by a status poll.
  Nothing rebuilds after a poll, so the index simply stayed empty; worse, the
  integrity probes added in v0.113.0 then reported `quick_check: ok`, because
  they were inspecting the blank replacement. Destroying the file is now
  confined to callers that rebuild it in the same breath. Readers report the
  corruption instead, and the message carries the one command that fixes it.
  `grep` no longer claims "No index found" about an index that is still there —
  that sentence used to be true only because the open had just deleted it.
- **`doctor` blamed the binary for a corrupt index.** `health-check --json`
  prints its full report and *then* exits non-zero when the index is unhealthy;
  doctor treated the exit code as a crash and discarded the report, showing
  `Schema: error — health-check failed` with no repair offered. It now reads the
  report, shows an `Integrity` row (page-level corruption, FTS drift, orphaned
  vectors — each at its own severity), and can rebuild a corrupt index, counting
  it fixed only after re-checking.
- **The version gate could not see a version site that had stopped being
  updated.** Every site is rewritten by a rule that assumes the file still looks
  a certain way; when it does not, the rewrite matches nothing and "unchanged"
  is indistinguishable from "already correct" — on both the write and `--check`
  paths. For the shipped CI template fixed in v0.113.0 that mattered: reverting
  it to the unscoped package name would have passed every check and shipped.
  Each site now asserts its expected end state, and a site that can no longer be
  written fails with its own exit code instead of reporting agreement.

### Performance
- **Indexing a large repository is ~35% faster.** The global edge post-passes are
  the first large joins to run over a freshly written graph, and the statistics
  refresh happened at the very end of the run — after them — so on a new index
  they planned against SQLite's built-in guesses. On a 2,052-file TypeScript
  repository that cost the import-contradiction prune 5.14s of a 13.5s full
  index; the identical statement against the identical database takes 0.187s
  once statistics exist. Building them first costs ~10-30ms. Full index of that
  repository: 13.16s → 8.52s. Single-file refreshes are excluded, so the
  query-time path is unaffected.

  Statistics change which plan SQLite picks, and that is not universally a win.
  The one measured counter-case needs a repository whose files almost never
  import anything: with 10 import edges across 605 TypeScript files — plus 60
  files exporting one name that everything calls bare — indexing went 0.36s →
  0.76s. Restoring imports to even 10% of those callers already flips it back to
  a win, and at 100% it is 6.7× faster. Real repositories are far on the winning
  side. If indexing gets slower for you on this release, that is the shape to
  look for; please report it, since the trigger is import density and the gate
  keys on files-touched.

## v0.113.0 (2026-08-02)

**Migration note: this release bumps INDEX_VERSION (58 → 59).** The MCP server wipes and
rebuilds the index automatically on its first start; a CLI-only setup rebuilds on
the next `incremental-index` / `rebuild-index`.

Remediation of the indexing/database/binary-chain domain audit
(`docs/AUDIT-INDEXING-DB-2026-08-02.md`).

### Security
- **The shipped GitHub Actions template told users to `npx -y code-graph-mcp`,
  which is a DIFFERENT publisher's package.** This project publishes
  `@sdsrs/code-graph` (`code-graph-mcp` is only a bin name inside it); the
  unscoped `code-graph-mcp` on npm belongs to someone else entirely. Anyone who
  copied `claude-plugin/templates/code-graph-snapshot.yml` into their repo — the
  documented way to publish snapshots — had a release job installing and
  executing a stranger's package, with `contents: write` in hand, on every
  release. The command failed afterwards on unrecognized arguments, so the only
  visible symptom was a red job, long after the third-party install script had
  run. The template now uses the scoped name pinned to the released version,
  `sync-versions.js` keeps that pin current (a 10th version site), and the
  release test asserts the template can never name the unscoped package again.

### Fixed
- **Deleting a file silently destroyed the non-`calls` edges pointing into it,
  and the index never recovered.** Phase 0 buffered only `calls` before the
  cascade delete, because the buffer table it writes to is calls-only. So
  `imports` / `implements` / `inherits` / `references` / `exports` / `routes_to`
  edges from files that had NOT changed were cascaded away with no recovery
  channel — and since those source files' hashes still matched, nothing ever
  re-extracted them. A full rebuild of the same final tree kept the edges (they
  re-resolve onto the `<external>` sentinel once the target file is gone), so
  `deps` / `cycles` / `project_map` answered differently depending on whether the
  index had been grown incrementally or rebuilt. Those edges now go through the
  same post-batch deferred pass the edit path uses. This is the delete-side
  sibling of the edit-side requeue added in v0.112.0.
- **A file that grew past the size limit (or stopped parsing) kept answering with
  symbols it no longer contains, forever.** Both `upsert_file` and
  `delete_nodes_by_file` live on the parsed path, so a skipped file kept its old
  nodes AND its old hash: `show` / `callgraph` / `dead-code` reported symbols that
  had been renamed or deleted, and `compute_diff` re-reported the file as changed
  on every single run (with `ensure_file_indexed` re-running the whole pipeline on
  every query that touched it). Oversize and unparsable files are now recorded
  with their current hash and their stale symbols purged; their inbound edges go
  through the same buffer the delete path uses. Read and hash failures are
  deliberately excluded — those are the transient, environmental failures, and
  purging a file's symbols because one read failed would be destructive on
  exactly the states that recover by themselves.

- **A stale `index.lock` left every future MCP server on Windows read-only,
  permanently.** The non-Unix liveness probe returned "alive" unconditionally, so
  after any unclean exit the recorded PID always looked live and every later
  instance became a secondary: no indexing, no watcher, `rebuild_index` refusing
  with `{"status":"secondary"}`, and nothing telling the user that deleting one
  file would fix it. Windows now really probes the PID, and only a positive "dead"
  verdict releases someone else's lock — an undecidable probe stays conservative
  and keeps treating the holder as alive. Two caveats, both Windows-only and both
  known: this is not verified on real Windows (the decision logic is
  platform-independent and unit-tested, the `tasklist` invocation is not), and
  reclaiming a stale lock is not itself atomic — two processes that judge the same
  dead PID simultaneously can both proceed, because the `remove_file` before the
  exclusive create discards the very atomicity that create was providing. That
  path was unreachable before (the probe always answered "alive"), so this trades
  a certain permanent failure for a rare race; the Unix path is unaffected, it
  holds a real `flock`.
- **A secondary instance never re-competed for the lock, so it answered from a
  frozen index for the rest of the session.** Two windows on one repo: close the
  first and the second kept serving whatever the index held at its own startup,
  with no disclosure on any query that returned results. It now retries promotion
  (throttled) and, on success, starts indexing and the watcher. Promotion also had
  to solve a problem the audit did not see: a secondary's connection is opened
  `query_only`, and SQLite cannot clear that on a live connection, so a promoted
  instance now writes through a connection opened at promotion time.
- **`rebuild-index` renamed a fresh index over one a running server had open**,
  stranding that server's writes on a deleted inode — its watcher and embedding
  work vanished silently and its queries kept serving the pre-rebuild snapshot
  until restart. It now refuses when another process holds the index lock, with
  `--force` to override; `reindex --from-snapshot` got the same gate (it had no
  warning at all), and `--quiet` no longer skips the probe itself.
- **`health-check` reported `healthy: true` on a corrupt database.** Its verdict
  was schema version plus non-zero counts, so page-level corruption, an FTS index
  that had stopped tracking `nodes`, and orphaned vectors were all invisible — and
  the human and JSON faces disagreed about a version-stale index. All three probes
  are now reported on both faces, and `healthy` accounts for corruption. Note for
  anyone extending this: `COUNT(nodes_fts)` cannot detect FTS drift — it is an
  external-content table and the count reads through to `nodes`, so it is equal by
  construction; the shadow table `nodes_fts_docsize` is what actually holds the
  index's own rows.
- **`.code-graph/` went un-ignored on CLI-only installs**, so `git add -A` would
  commit a multi-hundred-MB index. The `.gitignore` write lived only in the MCP
  server's startup path; it is now shared with the CLI index commands.
- **The watcher woke on its own writes.** It watched the whole project root with
  no path filter and the drain discarded event payloads, so `.code-graph/`'s own
  usage log and SQLite WAL kept `has_changes` true — every MCP tool call paid a
  full-tree stat walk, and the 30 s debounce was unreachable because it was gated
  on there being no watcher at all. Events under `.code-graph/` and `.git/` are now
  filtered at the source, and a watcher that has gone deaf (inotify limits,
  network filesystems, container mounts — all silent) is bounded by a backstop
  rescan instead of leaving the session stale forever.
- **Five MCP tools answered from pre-edit line numbers with no disclosure.** The
  six tools that take a `file_path` were refreshed; `semantic_code_search`,
  `ast_search`, `project_map`, `find_similar_code` and `trace_http_chain` were
  not, so a query issued right after an edit could report stale positions. They
  now refresh the files their own results name, under the same budget the CLI
  uses, and disclose when something was left stale rather than dropping it.
- `refs`/`show`/`deps` read source bytes from the worktree while taking line
  numbers from the main checkout's index, printing the wrong lines once the two
  diverged.
- A transient `SQLITE_BUSY` during promotion is now classified as retry-later
  rather than surfacing as a tool error.

### Changed
- `health-check` skips the page-scanning integrity pragma above 32 MB and says so
  (`"skipped_large"`), because the statusline polls this command on every render
  under a 1.5 s budget and a full scan costs ~2.4 ms/MB warm — more when the page
  cache is cold, which is exactly the first render after boot. `--deep` runs it
  regardless. Measured on this repo's 110 MB index: 280 ms → ~25 ms polled, 207 ms
  with `--deep`.
- The auto-update rate-limit backoff now actually applies to a stale binary. The
  "binary is stale" and "binary is missing" bypasses were evaluated *around* the
  throttle rather than inside it, so a suspended updater still spent one GitHub
  API call per session start doing nothing.
- `doctor` no longer counts a no-op auto-update as a repair. Its CLI has no
  non-zero exit path, so suspended, rate-limited, offline and opted-out runs all
  looked like success; doctor now re-checks the diagnostic afterwards and names
  the real reason. This closes a loop where the suspension notice told the user to
  run doctor and doctor replied with a checkmark.
- Binary downloads use `curl -f` (the sidecar fetch already did), and the size
  gate, version gate and outer failure path in the promotion step now say why they
  rejected a download instead of returning silently.
- `bind_calls_to_imported_targets` binds in one set-based statement instead of
  round-tripping every candidate pair through Rust. The predicate is unchanged
  (same SELECT, same `INSERT OR IGNORE` dedup), so the edges created are
  identical; it removes work that was re-proving already-existing edges — on the
  audit's 1,456-file measurement, 10,960 round trips to create 3 edges.

## v0.112.1 (2026-08-02)

### Fixed
- **`grep` on Windows returned absolute paths — and with them lost the AST
  annotations, `-c` zero-fill, and dedup.** Latent on every prior version;
  surfaced the moment v0.112.0's CI installed ripgrep and the 43 grep
  end-to-end tests actually ran on windows-latest for the first time (10
  failed). ripgrep echoes back the path spelling each operand was handed, and
  the operands came in TWO spellings: explicit search paths canonicalized to
  the long form, the default walk operand in the raw project-root spelling —
  which on Windows can be an 8.3 short name (`…\RUNNER~1\…`). No single
  lexical root string can equate the two, so output rows kept their absolute
  paths and everything keyed on repo-relative paths downstream missed. The
  default walk operand is now canonicalized like every explicit path, the
  path-traversal guard and `-c` scoping accept either root spelling for the
  nonexistent-path fallback (rg then reports the honest "No such file"
  partial instead of a bogus outside-project rejection), and output
  relativization tries the canonical root first, the raw root second.
  Verified where it can only be verified — the windows CI leg: 10 failures →
  0 across the two fixes. Linux/macOS output is unchanged (the fallback fires
  only when the canonical root misses, which requires the two spellings to
  differ).

## v0.112.0 (2026-08-02)

**Migration note: this release bumps INDEX_VERSION (57 → 58). The MCP server
wipes and rebuilds the index automatically on its first start; a CLI-only
setup rebuilds on the next `incremental-index` / `rebuild-index`. No action
needed beyond letting that rebuild run. If you need to stay on the old index
format, pin `@sdsrs/code-graph@0.111.1`.** For repositories over 500 files
the rebuild is the point of this release: the old index deterministically
carried phantom and missing cross-batch edges (below).

A full audit of v0.111.1 (7 parallel reviewers + main-thread cross-verification,
report in `docs/AUDIT-REPORT-2026-08-02.md`) found one P0 and nine P1s. All are
fixed here, in four commits, each with regression tests that were verified to
go red against the pre-fix code.

### Fixed — index correctness (the P0)
- **A fresh index of any repository larger than one batch (500 files) silently
  corrupted cross-batch relations — deterministically, and rebuilds reproduced
  it byte-for-byte.** Phase 2 resolved every relation against a pool that could
  not contain later batches' nodes: `implements`/`imports` whose target sat
  batches ahead bound to `<external>` phantoms, `inherits`/`exports`/
  `routes_to`/`references` dropped outright, and bare names wrong-bound to
  whichever same-name twin an earlier batch happened to hold. Measured: the
  same four files that produce four true edges alone produced two phantoms
  plus one missing edge with 600 filler files. Batch-time resolution now
  decides only pool-complete facts (same-file binds, noise drops, resolved
  file-constrained binds); everything else is re-resolved once after the batch
  loop against the complete name map, branch-for-branch identical to the
  batch-time chain. Verified on this repository's own tree: indexing at
  BATCH_SIZE 25 versus 500 now diffs to **zero nodes and zero edges**, and the
  new binary is byte-identical to v0.111.1 on single-batch trees, where the
  bug could not fire.
- **Renaming a symbol permanently orphaned its callers' edges under
  incremental indexing.** The by-name edge restore had nothing to bind after a
  rename and silently dropped; a full rebuild would have re-resolved the
  surviving caller against the whole tree. Misses are now requeued through the
  normal resolution channels, and a new whole-graph parity test pins
  incremental == full rebuild for the rename case.
- **`<external>` sentinel nodes were never garbage-collected**, lingering in
  the name-resolution pool forever and making incrementally-grown indexes
  diverge from fresh rebuilds. Zero-edge sentinels are now reaped every run,
  and a sentinel first minted as `module` upgrades to `trait` when an
  `implements` claim arrives later.

### Fixed — reads that wrote, and answers that disagreed
- **A read command run inside a linked git worktree wrote the worktree's file
  content into the MAIN checkout's index** (hash swapped, line numbers
  shifted, files absent on the branch cascade-deleted) whenever the branch had
  diverged. All nine freshness call sites now use the worktree-fallback root;
  a new end-to-end test diverges a branch first — the old worktree test used
  the same commit, so every hash matched and this write path was structurally
  unreachable.
- **`refs` silently merged every same-name definition's references into one
  total** while `callgraph` and the MCP tool errored `Ambiguous` on the same
  input. It now takes the same shared ambiguity gate, and `--json` discloses
  the `--min-confidence` hidden count the human format already printed.
- **With `--json`, any pre-handler error (no index yet, path outside root)
  left stdout at zero bytes** on 19 of 22 commands — a JSON parse failure for
  machine consumers on the most common error path a fresh checkout hits. Every
  command now emits an `{"error": ...}` object with exit 1.
- `health-check --json` answered `no_index` from a linked worktree whose main
  checkout had a healthy index (the human format said OK — doctor consumed the
  broken one). A dead-code candidate hidden by `--ignore` and `--min-lines`
  together was counted by neither disclosure and answered a bare `[]` false
  clean. `find_dead_code` (MCP) gains the query-time file refresh every
  sibling path tool had. `project_map` module counts use the shared test
  filter (a `tests/` directory no longer shows as a 154-function module with
  an empty symbol list).

### Fixed — plugin
- **Corrupt-tolerant settings writes reached only half the write sites.**
  `cleanupDisabledStatusline` and `uninstall` still round-tripped a
  lossy-decoded settings.json straight back to disk, permanently replacing
  invalid bytes with U+FFFD — the exact destruction the v0.108.1 guarded
  reader was built to stop. Both now probe first and take the guarded
  read+write only when a write is certain; a read-only `~/.claude` no longer
  crashes the statusline process.
- **The prompt-context injection hook could never find the binary on a
  plugin-marketplace-only install.** It spawned `code-graph-mcp` by bare PATH
  name with an empty catch; installs whose binary lives in
  `~/.cache/code-graph/bin/` got zero injections, forever, silently. It now
  resolves through `findBinary()` like every sibling hook, reads stdin from
  fd 0 (the lone `/dev/stdin` holdout — inert under the plugin's own hook-fire
  self-test), and stamps its cooldown on attempt so a slow index cannot re-run
  the query on every prompt.
- Hook cooldown flags are now project-scoped (a grep in one repository no
  longer suppressed another repository's hooks for 60 seconds); the shared
  tmp directory gets a 24-hour prune wired to SessionStart and uninstall
  (675 stale flag files on the dev machine → 27); the global-npm heal
  counter survives the update-available save path instead of resetting its
  3-attempt cap forever; `windowsHide` now also covers `bin/cli.js` (the
  `npx` entry point), doctor's injected-seam call, and `scripts/`, with the
  guard scanning all three roots.

### Changed — two answers that scripts may have depended on
- `refs <name>` on a symbol with multiple non-test definitions now **exits 1
  with an `Ambiguous` error object** (same answer `callgraph` and the MCP tool
  already gave) instead of returning every definition's references merged into
  one total with exit 0. Pass `--file` or `--node-id` to disambiguate. If a
  script consumed the merged total, it was summing references of unrelated
  symbols that happen to share a name.
- `project_map` / `map` module counts now apply the full test filter, so
  per-directory function/class counts no longer include `test_*` symbols and
  test-path files. Directories of tests stop appearing as large production
  modules with an empty key-symbol list.

### Fixed — CI actually runs what the gate runs
- Every CI job that runs `cargo test` now installs ripgrep: 43 grep
  end-to-end tests were silently skipping on all three runner images — the
  whole `cmd_grep` surface had zero executed CI coverage, release gate
  included — while passing on dev machines that have `rg`. The skip now
  asserts CI is unset, so this class cannot go dark again. Both CI clippy
  legs and the embed `cargo check` gain `--all-targets`, closing the gap
  where a tests/benches violation passed every PR and reddened the gate only
  after the tag existed.

### Docs
- The README's MCP tool table now lists the seven tools `tools/list` actually
  advertises (`impact_analysis` was removed long ago; calling it returns
  `Unknown tool`), documents the still-dispatchable hidden aliases, and maps
  CLI commands to their real MCP equivalents. Language table: Rust drops
  `inherits` (never emitted) and gains `routes_to` (axum); C++ gains
  `.hh`/`.hxx`; the `references` axis and CommonJS `exports` are documented;
  test-marker claims are scoped to what the AST layer actually detects.

## v0.111.1 (2026-07-30)

An independent review of v0.111.0 landed after that release went out. It found
no Critical and no High, but two of the things v0.111.0 *said* were not true,
and one of them hid a real gap. Corrections first.

### Fixed
- **A failing update could park itself for days.** v0.111.0 stopped retrying
  after 5 failed installs of the same release and only re-armed when a NEWER
  release appeared. But the five causes are indistinguishable at the failure
  site — a briefly-missing `.sha256` sidecar, a captive portal, a temporarily
  full disk all burn the budget as fast as a genuinely broken `tar` — and
  SessionStart forces a check with only a 2-minute floor, so roughly five
  Claude Code restarts inside ten minutes exhaust it. A ten-minute outage could
  therefore park auto-update until the next release. It now retries once a day
  while suspended, which keeps the per-session treadmill dead and still
  self-heals. (The retry deliberately is NOT keyed to `--force`: session-init
  passes `--force` on every session start, so re-arming there would restore the
  exact treadmill the cap exists to stop.)
- **A suspended update was invisible.** v0.111.0's CHANGELOG said users "will
  see a one-line stderr notice"; **that was false**. The notice is written by
  the updater process, which session-init spawns `detached` with
  `stdio: 'ignore'` — nothing reads that stderr — and the statusline goes quiet
  at the same threshold, so the only remaining signal was running
  `code-graph-mcp doctor` by hand. The statusline now shows `⚠ update stuck`,
  on the healthy line as well as the degraded ones.
- **The windowsHide guard was weaker than v0.111.0 claimed.** It said a new
  spawn "fails the build"; that held only for the direct-call spelling. The
  review got four genuinely unguarded calls past the scanner:
  `require('child_process').execSync(…)`, `cp.spawn(…)` via a namespace
  binding, a renamed destructure (`const { execFileSync: run } = …`), and an
  inner shadowed options variable hiding behind an outer guarded one. All four
  are now caught and pinned as regression tests. (Nothing was actually shipping
  unguarded — the sweep itself was complete; the guard's promise was not.)
- **`quoteCmdArg` mishandled a trailing backslash.** `"C:\x\"` is read by the
  receiving program's MSVCRT argv parser as an escaped quote, swallowing the
  rest of the command line. Trailing backslash runs are now doubled. Not
  reachable from any current call site — all args are flags and package specs —
  but the helper reads as general-purpose.
- **A bad `version` string could kill the MCP server instead of failing an
  install.** `npmInvocation` throws on an unquotable argument, and the
  launcher's cold-start path called it outside any `try`, so the throw escaped
  and took down a server that was already serving the 0-tool stub. Now caught
  and reported as a failed install step.

## v0.111.0 (2026-07-30)

Windows-only fixes, all from one field report ([#40]). Nothing here changes
behaviour on macOS/Linux except the new opt-out and the retry cap.

**No action required to upgrade** — the plugin auto-updates as usual, and this
release changes no CLI flag, tool schema, or on-disk format. Windows users
stuck on the flashing-console workaround `CODE_GRAPH_DEV=1` should drop it: it
also rewires binary resolution. Use `CODE_GRAPH_NO_AUTO_UPDATE=1` if the intent
was only to stop auto-update. Users whose updates have been failing repeatedly
will stop seeing a silent retry every session.

> **Correction (v0.111.1):** this entry originally claimed those users "will see
> a one-line stderr notice naming the manual update command". They do not — that
> stderr is discarded by the detached updater process. v0.111.1 makes the state
> visible in the statusline instead.

### Fixed
- **Auto-update flashed 5–7 console windows per session start and stole
  keyboard focus.** Node's `windowsHide` defaults to `false` on every
  `child_process` API, and Windows gives any console-subsystem child of a
  console-less parent a NEW visible console window — our parents (MCP server,
  hooks, statusline) are all launched hidden by Claude Code, so every `where` /
  `curl` / `tar` / `npm` child flashed one. All ~30 child-process call sites in
  `claude-plugin/scripts/` now route through `proc-opts.hidden()`;
  `windows-hide.test.js` re-derives the call-site list from source on every run,
  so a new spawn fails the build instead of shipping a flash.
- **`tar` could never extract the plugin asset under GNU tar**, which is first on
  PATH for anyone with git-for-Windows/MSYS: `tar xzf C:\Users\...\claude-plugin.tar.gz`
  reads `C:` as a REMOTE HOST (same colon-parsing family as #34/#35). It now
  passes a relative archive name with `cwd` set — the spelling both GNU tar and
  Windows' bundled bsdtar accept. This was the step that made plugin updates
  permanently unachievable on those machines, which is what kept the whole
  download chain re-running.
- **A permanently-failing update retried forever.** `updateAttempts` was
  counted but never read: the field report showed it at 8 and climbing, with the
  full download chain (and its console flashes) repeating every session. The
  counter is now scoped to the target version and the chain stops after 5 failed
  installs of the same release, going check-only until a newer release is
  published. A missing binary is still downloaded — that one is existential.
  `doctor` reports the suspension with the manual update command instead of
  "up-to-date", and deliberately offers no auto-fix for it.
- **`DEP0190` deprecation warning on every npm spawn** (Node 24 runtime
  deprecation): npm needs a shell on Windows because it is `npm.cmd`, and Node
  space-joins `args` into the command line unescaped in that mode. npm calls now
  go through `npmInvocation()`, which pre-quotes the whole command and passes an
  empty `args` array — refusing outright any argument that cannot be safely
  quoted for `cmd.exe`.

### Added
- **`CODE_GRAPH_NO_AUTO_UPDATE=1`** — a documented auto-update opt-out. Until
  now the only working one was the accidental `CODE_GRAPH_DEV=1`, which also
  rewires binary resolution. No version check, no download, and no updater
  process is spawned; a missing binary is still installed so the MCP server
  cannot be left with no engine.

[#40]: https://github.com/sdsrss/code-graph-mcp/issues/40

## v0.110.0 (2026-07-30)

Two defects, both found by checking a claim instead of quoting one. A third fix
was written, reviewed, rewritten, reviewed again, and then **removed** — see
"Known gap" below; that story is the most useful thing in this entry.

**`INDEX_VERSION` 56 → 57**, so upgrading rebuilds the index once.

### Fixed
- **`import mod, * as ns from './m'` emitted two identical `imports` rows**, one
  per binding, where each spelling alone emits one. `idx_edges_unique` includes
  `metadata` on purpose (multiple route edges per file), so the differing `q`
  marker kept both. The namespace marker wins: it also feeds `ns_module_map` for
  `ns.foo()` member calls, while the default marker deliberately feeds nothing
  else and is pure duplication once a namespace binding has claimed the edge.
  Note the originally reported impact did not hold — `deps` counts
  `COUNT(DISTINCT nb.id)` and was never affected; edge totals and per-language
  relation stats were. An independent review measured the risky direction
  (does the guard drop a real edge?) as clean, including for unresolvable
  specifiers such as `import React, * as ReactNS from 'react'`, where neither
  marker produces an edge in the first place.
- **The cached directory scan treated an unchanged mtime as proof of freshness.**
  `file_needs_hashing` compared mtimes alone, so a content edit landing inside
  one filesystem timestamp granule was skipped no matter how much the content
  moved — ordinary on HFS+/ext3 (1s), exFAT (2s) and several network
  filesystems. It now compares mtime **and** size, both already carried by the
  one `metadata()` call. The CLI path was never affected
  (`run_incremental_index` re-hashes everything); this is the MCP server's
  resident-cache path, which every tool reaches through `ensure_indexed`.

  Residual, and more reachable than "same granule AND same byte length" sounds:
  equal-length edits are ordinary (renaming `foo` to `bar`, flipping `>` to
  `<`). The only backstop is `ensure_file_indexed`, which fires just for the file
  being queried, so structural queries keep serving stale symbols until something
  moves the mtime. Kept anyway — it is the rsync quick-check tradeoff, and
  closing it means hashing every file on every scan.

### Changed
- The existing content-change tests for the cached scan all slept 50 ms before
  rewriting, which guarantees a fresh mtime and so could never exercise the case
  above. A new test freezes the mtime explicitly and asserts on the file's
  PRESENCE in the returned hash map — a skipped file is simply absent, so the
  natural `got != expected` comparison reads `None != Some(h)` and passes for
  exactly the failure it is meant to catch.
- A second test drives the same defect end to end through
  `run_incremental_index_cached` and asserts the user-visible outcome
  (`files_indexed == 1`, the new symbol present, the old one gone). The scan-level
  test pins the decision; only this one shows the consequence. Under the
  mtime-only mutation it fails `left: 0, right: 1`.

### Known gap — dynamically built include paths still produce phantom edges
`extract_string_from_subtree` returns the first string literal anywhere in the
subtree and discards the rest, so `require_once "config" . $env . ".php"` binds a
real `imports` edge to a real `config.php` — a file that statement never includes
at runtime. `require("./x" + suffix)` is the same shape in JS. A phantom aimed at
a real node is worse than a missing edge, because `deps` / `cycles` / `affected` /
`impact` all consume it as fact. This is **unchanged and still open.**

Two attempts to close it were made in this batch and both were removed, each
after an independent review measured it as a NET LOSS of true edges on ordinary
idioms:

1. *"Every operand after the first must be a literal."* Deleted
   `__DIR__ . DIRECTORY_SEPARATOR . "bootstrap.php"` and
   `ROOT_PATH . DS . "helper.php"` (the house style of a generation of PHP
   frameworks), and deleted `require(a || b || "./fallback.js")` while keeping
   the two-operand form — because the test matched the node KIND, and
   `binary_expression` is also `||`, `&&`, `??` and every comparison. It also
   failed to remove the `$dir . "lib.php"` phantom, which is the headline shape,
   because the exemption was positional and that operand sits at position 0.
2. *"Read right to left; the basename after the last separator must be known."*
   Fixed all of the above, and introduced its own: it deleted a fully static
   `require(("./par") + ".js")` (a parenthesized operand is not a string literal
   node), deleted `"$base/dir/" . "x.php"` (an interpolated literal contaminates
   even when it sits left of a separator, contradicting the rule's own stated
   principle), still dropped routes whose path ends in a variable
   (`@app.get("/g/" + VERSION)`), and manufactured a NEW phantom —
   `"vendor/" . $pkg . "/init.php"` bound to an unrelated `src/init.php`, because
   a known basename with an unknown directory is still a guess.

The transferable lesson is about measurement, not about strings: both times the
fixture used to validate the change happened not to contain the shapes it broke,
and both times the full-repo edge diff was zero because this repository contains
none of these spellings. A zero diff on one corpus is not evidence.

Whoever picks this up: the extractor is likely the wrong layer. It must answer
with a single string, which forces it to choose between "guess" and "give up"
before the resolver — the component that actually knows what files exist — ever
sees the expression. Validate with a FLAT fixture where targets can bind, and
include rows for the parenthesized operand, the interpolated literal left of a
separator, the `||` chain at two and three terms, and the route path ending in a
variable.

## v0.109.0 (2026-07-29)

Audit 2026-07-27 P2 batch: 20 of the ~29 observations, chosen for the ones whose
failure mode is silence, plus the seven findings an independent review raised
against the batch itself.

**`INDEX_VERSION` 53 → 56, so upgrading rebuilds the index once.** (The 53 → 54
step landed with the second sub-batch; this line said "no `INDEX_VERSION` change"
until 55, which was wrong from that commit onward.) Two changes alter what the
indexer stores for identical source: bare Rust callees that name a local binding,
and pattern-position identifiers inside `matches!`.

### Added
- **`.gitattributes` (`* text=auto eol=lf`)** — pins LF in the working tree on
  every platform, removing the CRLF class that cost v0.108.0 a full
  push/CI/fix round on an otherwise green release. `git add --renormalize .`
  changed nothing else: the repo was already all-LF, so this is purely
  preventive for Windows checkouts. It had to be negated in `.gitignore` first —
  the `.*` blanket rule silently refuses every new repo-root dotfile, and
  `git add` reports that as a hint rather than a failure.

**`INDEX_VERSION` 55 → 56**: two import forms that emitted no edge at all now
emit one (see the PHP and ESM entries below), so the index changes for any repo
containing either.

### Fixed
- **Five defects the pre-release review found, repaired before the tag.** Two
  independent fresh-context reviewers were run over the release diff; neither
  found a blocker, both found things worth stopping for.
  - **`settings.json` containing a non-UTF-8 byte was silently rewritten with
    no backup.** The byte-exactness work earlier in this batch covered only the
    *corrupt* branch. A file that is valid JSON but carries an invalid UTF-8
    byte — a latin-1 byte in a path, which a non-ASCII username on a legacy
    code page produces — was classified clean, and then round-tripped through
    `toString('utf8')` → `JSON.stringify` → atomic write, replacing every such
    byte with U+FFFD permanently, with nothing on stderr. Detected now by
    re-encoding the decoded text and comparing to the original bytes: a
    lossless decode round-trips, a lossy one cannot. The true bytes are copied
    aside first, and if the copy fails we refuse to touch the file.
  - **The statusline slot was never healed on Windows** (a regression from this
    batch). `cacheDirVersion`'s pattern was `/`-only, but the command it parses
    is built with `path.join`, so on Windows it returned null for every
    plugin-cache path and `compositeSlotIsStale` answered "not stale"
    unconditionally. The repo's own "an older plugin-cache version dir must
    still be healed" test could not catch it — `plugin-tests` is ubuntu-only.
    Now separator-agnostic, with a test that asserts both spellings from any
    platform.
  - **The JS-test gate guarding `npm publish` was still on the non-recursive
    glob.** Two of three gates were converted this batch; the one left behind
    was the last gate before an irreversible publish. It also, unlike ci.yml,
    must keep running `install-e2e` — the drift guard now covers release.yml
    and pins both properties.
  - **Rust local-binding call exclusion dropped real edges.** The exclusion
    added earlier in this batch tested a whole-function name set with no scope
    and no ordering, so a binder that cannot shadow the call suppressed it
    anyway: a `let` *after* the call, a binder in a sibling block, a `for` /
    `match` / `if let` binder with the call after the construct, a closure
    parameter with the call outside it. A dropped `calls` edge is the dangerous
    direction — dead-code reads exactly that edge. The memoized set is now only
    a cheap over-approximation; a hit runs a precise walk of the call's own
    ancestor chain. Edge-neutral and cost-neutral on this repository (0
    restored, 0 lost, 4038 ms → 4060 ms).
  - **Two CommonJS export forms bound the wrong node.** `(function (module,
    exports) { exports.x = y })` — the UMD/webpack wrapper — was treated as a
    module export because the object was matched by text, so it marked the
    wrapped function exported; and a pair whose value is not a symbol
    (`module.exports = { keyed: 42 }`) fell back to the KEY and marked a
    same-named real function exported.
- **CommonJS exports were invisible, so `dead-code` called them orphans.** An
  incoming `exports` edge is what makes `find_dead_code` report an unused symbol
  as `exported_unused` ("public surface, something outside may use it") rather
  than `orphan` ("nothing references this"). Only the ESM `export` keyword
  produced one. Identical dead code therefore got opposite verdicts by module
  system — and CommonJS got the stronger, more dangerous one, the verdict a user
  reads as *safe to delete*, on a module's public API. `module.exports = { f }`,
  `module.exports = f`, `exports.f = g` and `module.exports.f = g` all emit the
  edge now, targeting the identifier that names the real symbol rather than the
  export key. Measured on this repository, whose entire plugin is CommonJS:
  **+241 `exports` edges, 0 removed** (batch size 500; +233 at 25).
- **PHP `require_once "lib.php"` emitted no import edge at all** — the
  double-quoted spelling of all four include keywords (`require`,
  `require_once`, `include`, `include_once`), while the single-quoted spelling
  worked. tree-sitter-php gives a double-quoted string its own node kind,
  `encapsed_string`, because that form can interpolate; the extractor only knew
  `string`. Double quotes are the more common spelling
  (`require_once "vendor/autoload.php"`), so PHP file-level dependencies were
  largely invisible to deps / cycles / affected / project_map. An interpolated
  path (`require_once "$dir/lib.php"`) is still skipped deliberately: its value
  is not known statically, and guessing the stem would bind a real edge to
  whatever file shares the literal tail.
- **`import mod from './m'` emitted no import edge either** — the ESM default
  binding, the most common ESM form there is. It is a bare `identifier` under
  `import_clause`, so it was neither an `import_specifier` (all the specifier
  walk looks for) nor a direct child of the statement (what the identifier arm
  handles), and fell between the two. `import mod, { y } from './m'` emitted
  only the named half. It now binds module-level, like the namespace form,
  rather than as a symbol edge under the local name — the local name is
  arbitrary (`import anything from './m'`) and the default export's own node is
  usually called something else, so a name-based edge would either miss or bind
  a same-named symbol elsewhere. It carries its own `q` marker rather than
  reusing `ns_import`, because `mod.foo()` after a default import is a member of
  the default-exported value, not a top-level symbol of that module, and must
  not feed the namespace member-call map.

  Both were found by the new import parity table below, on its first run. The
  four `q` markers themselves were string literals written out twice — once at
  the parser that stamps them, once at the Phase-2 branch that reads them — and
  are now `domain::IMPORT_Q_*` constants.

### Added
- **Per-language, per-spelling import parity table** and a **per-language
  inheritance parity table** (`tests/edge_coverage.rs`). The `calls` axis got
  its 12-language table last batch; imports had six languages with one spelling
  each, and the inheritance axis had no table at all — only scattered
  single-language tests, leaving C#, Kotlin, Swift, Python, TypeScript and
  JavaScript able to lose their arm with nothing going red. Mutation-verified:
  disabling the C# `base_list` arm names `csharp inherits` and
  `csharp implements`; disabling the Kotlin import arm names `kotlin import`.

  The inheritance table records the modeling per (language, relation) because it
  is not uniform — Kotlin and Swift fold interface/protocol conformance into
  `inherits`, Go emits it for struct embedding only (interface satisfaction is
  structural, so there is nothing to extract), Rust has `implements` and no
  inheritance — and it asserts the zeroes too, so a future change that starts
  emitting `implements` for every Kotlin supertype fails instead of
  double-counting.

- **Three more parity tables**, closing the axes that had no numeric guard:
  the **method-call spelling** per OO language, **`exports` across module
  systems** (which is what surfaced the CommonJS defect), and the **`references`
  axis one fixture per PASS**. That last granularity was itself found by
  mutation: a first version asserted "each reference-capable language emits ≥ 1"
  and survived deleting Go's `type_identifier` row, because Go's other pass kept
  the count above zero — a language with two passes made the guard vacuous for
  both.

  The call axis was swept the same way the import axis was, and came back
  clean: 46 spellings across 15 languages (receiver calls, qualified/static
  calls, chained calls, optional chaining, `Self::assoc`, `super()`, Kotlin
  extension functions, C++ out-of-class definitions) all resolve. It is the
  most-worked axis in the crate, which is the likeliest explanation. Only the
  receiver spelling is pinned, since that is the one that has shipped broken
  before.

  All tables assert `files_with_parse_errors == 0`. tree-sitter recovers from a
  syntax error by returning a damaged tree, so a bad fixture still yields
  symbols and a missing edge would be ambiguous between "the arm is gone" and
  "this fixture never parsed" — not hypothetical: a single-line Kotlin class
  body (`class C { fun f(): Int = 1 }`) errors under the pinned grammar while
  the identical code across three lines does not.

### Changed
- **The additive `references` passes are a table, not eleven hand-written
  `if`s** (P1-9's other half). `walk_for_relations` carried one
  `if config.name == "…" && kind == "…" { … }` block per language per relation
  — the exact shape this crate's audits name as its top recurring bug class,
  because a missing block is not a compile error but an edge that is silently
  never emitted. The eleven are now rows in `REFERENCE_PASSES`, which makes
  them enumerable, and `tests/reference_pass_wiring.rs` asserts that every
  `extract_*_reference` defined under `src/parser/relations/` is wired to a
  row. Writing the extractor and forgetting the wiring now goes red instead of
  quietly indexing nothing.

  Two distinctions the blocks encoded implicitly and the table now states:
  some passes key on the file's raw language (where `typescript` and `tsx` are
  different strings) and some on the `LanguageConfig` family name, which is not
  interchangeable; and Rust's two `identifier` extractors are an either/or
  chain while Python's two are independent passes that share a node kind.
  `walk_for_relations` 1334 → 1216 lines. Edge sets identical before and after
  at both batch sizes (8708 and 7998, zero line diff, 823 / 719 of them
  `references`).

### Fixed
- **The `<external>` sentinel's node type was decided by hash-map iteration
  order.** A name reachable from both channels in one batch — `impl Write for …`
  (implements → `trait`) and an unresolved `use std::io::Write` (imports →
  `module`) — was stamped by whichever relation happened to be pushed last, so
  re-indexing an unchanged tree could flip it. Reproduced on this repo: three
  rebuilds with the pre-fix binary disagreed on `Write` (runs 2 and 3 both
  differed from run 1); the same three with the fix agree exactly. Two causes,
  both closed. Every caller derives its file list from `HashMap::keys()` —
  `run_full_index` from `scan_directory`'s map, both incremental entries from
  `compute_diff` — so `index_files` now sorts and dedups the list at the one
  choke point all four entry points funnel through. And the sentinel's type now
  follows a fixed precedence (implements is the specific claim and beats an
  import) rather than last-write-wins.

  Measured in both regimes, because they behave differently. Within one batch
  (this repo, 228 files against `BATCH_SIZE` 500) the edge set is unchanged,
  8665 ↔ 8665 with zero line diff — only the sentinel's type moved. **Across
  batches — which is every repo over 500 files — the ordering was costing
  edges, not just stability.** With `BATCH_SIZE` forced to 25 over the same
  tree, three pre-fix runs produced 7487, 7619 and 7891 edges (2402 and 2880
  differing lines against the first run); three post-fix runs produced 7997
  every time, zero diff. Sorted order keeps a directory's files in one batch,
  where same-batch resolution can bind them; arbitrary order scattered them
  across batches and the cross-batch bindings were simply lost.

  "More edges" is not by itself "better", so the sets were scored against the
  single-batch run of the same tree (8697 edges — every file in one batch, so
  nothing is lost to a batch boundary). Three pre-fix multi-batch runs landed
  6960 / 7487 / 7042 real edges alongside 535 / 350 / 456 wrong ones (439 / 330
  / 405 of those `imports → <external>` phantoms, minted when the defining file
  happened to sit in a later batch). The post-fix run: 7817 real, 170 wrong,
  162 phantom. Both directions move — recall against the single-batch reference
  goes 80.0–86.1% → 89.9%, and the phantom count halves at worst. So a
  multi-batch repo does get a different index out of this — covered by the
  53 → 56 `INDEX_VERSION` step this batch already carries, which forces the
  rebuild that picks it up.
- **SQL `LIKE` treated `_` as a wildcard in the test-source filter** (P2-10), so
  `latest.cs` (`%`=`l`, `_`=`a`, then `test.`) and `attest.py` were classified as
  tests and dropped from every production-caller count. The `'test\_%'` leg one
  line above was already escaped, in the same string literal. Fixing it exposed
  two more divergences in the same direction: the infix leg was a *contains* over
  any extension while `is_test_path` is an *ends-with* over four, and the name leg
  was ASCII-case-insensitive while `is_test_symbol` is `starts_with`, so
  `Test_Signup` was excluded too. Both legs are now GLOB, and the infix leg is
  generated from `INFIX_TEST_EXTS`. This was the fifth copy of the
  test-classification rule and the only one with no mechanical guard; it has one
  now, asserting the one-directional invariant that actually holds — the filter
  may be narrower than `is_test_symbol` (an accepted recall gap), never broader.
- **The release workflow's `concurrency.group` had no dispatch-tag fallback**
  (P2-21) — the one site of seven missing it. A re-release dispatched from `main`
  grouped under `refs/heads/main` while the tag-push run for the same version
  grouped under `refs/tags/vX.Y.Z`, so the two did not serialize: exactly the
  race the comment above that block says it prevents. A drift guard now covers
  every `github.ref` expression in the file.
- **The 24h GitHub rate-limit backoff was dead code** (P2-19). `checkForUpdate`
  snapshotted state before the fetch and wrote it back on the null return,
  erasing the `rateLimited: true` that the 403 handler had just set — so every
  403 cleared the flag it raised, and polling continued on the ordinary interval.
- **Both fail-open links in the download chains are closed.** The binary chain
  (P2-24) treated a missing sha256 sidecar as permission to install: it now
  retries once, then refuses. The plugin chain (P2-23) had no checksum available
  at all — it pulled GitHub's auto-generated source tarball, for which nothing
  publishes a digest, and it is the one chain whose payload becomes *executed
  code* (the extracted JS is copied into the plugin cache and run as hooks).
  `release.yml` now publishes `claude-plugin.tar.gz` with a `.sha256` sidecar,
  and the client refuses to extract without a match. The binary update still
  proceeds in every refusal case, so a bad plugin asset strands no one.
- **All 21 first-party `actions/*` uses were on mutable major tags** (P2-25)
  while all 20 third-party sites (3 distinct actions) were SHA-pinned — the harder half to justify,
  since `actions/checkout` runs first in every job including the one holding
  `NPM_TOKEN`. Every `uses:` is now pinned to a verified 40-hex commit, with a
  drift guard.
- **`similar --json` answered an exit-1 miss with a bare `[]`** (P2-14), the last
  CLI command doing so while `impact` / `callgraph` / `trace` / `deps` all answer
  `{error, symbol}`. Once stderr is dropped that array is indistinguishable from
  a successful empty result. The test meant to enforce the contract was pinning
  its violation. The sqlite-vec-absent case now discloses too: "similarity could
  not be computed" is not "nothing is similar".
- **`dead-code <path-with-nothing-indexed> --json` returned `[]` and exit 0**
  (P2-15) — a clean bill of health for a path the index has never heard of, while
  `overview` answers the same input with an error object and exit 1.
- **`get_ast_node` did not treat an empty `file_path` as absent** (incremental
  audit Δ1) — the one of five tools missing the filter its siblings carry, and
  that this same function applies to `symbol_name` forty lines below. An LLM
  client that fills every declared field with a placeholder got "File '' not
  found" for a request naming a real symbol.
- **`git rev-list` gained its `--` separator** (P2-26), matching the `ls-files`
  sibling 40 lines away that already carried the comment explaining why.
- **A lowercase tuple-variant pattern inside `matches!` produced a fake call
  edge** (P2-3). The token-soup pass tells patterns from calls by CamelCase
  convention, which `#[allow(non_camel_case_types)]` code (bindgen output, C-ABI
  enum mirrors) breaks: `matches!(x, ok(v))` emitted `calls → ok`, aimed at
  whichever same-language `fn ok` the resolver picked. Position inside a
  `matches!` / `assert_matches!` / `debug_assert_matches!` argument list is now
  decided structurally — everything after the first top-level `,` is a pattern —
  while a top-level `if` returns to expression state so guard calls keep their
  edges. Verified by rebuilding this repo's index on both sides: 5954 `calls`
  edges before and after, nothing lost. The same comment also now records the
  second half of the convention's cost, which it had left unstated: a type
  constructed ONLY inside macros gets no inbound edge at all, so `find_dead_code`
  reports it dead.
- **One failed `stat` could delete a live file from the index** (P2-5).
  `scan_directory_cached` derived "does this file still exist?" from its mtime
  map, which only holds files whose `metadata()` call succeeded — so a transient
  EACCES/EMFILE/NFS hiccup on a file the walker had just listed dropped it from
  the carry-forward, and the diff reported it DELETED, taking its nodes and edges
  with it. Existence now comes from the walk (which saw the file) and freshness
  from the stat; a failed stat also stops meaning "unchanged", which had left an
  edited file stale for as long as the failure lasted.
- **Idle ticks aged the pending-call buffer** (P2-6). A buffered forward
  reference gets 50 attempts before eviction, and every index pass spent one —
  including watcher flushes and periodic rescans whose diff was empty, where no
  node could have appeared and the sweep provably resolves nothing. Measured on
  this repo: every row sat at attempts = 4 after 26h/4 scans, ~2 weeks to the
  ceiling with the code untouched; an evicted row only returns if the *caller*
  file is re-indexed. The sweep now runs only for a batch that parsed files, so
  attempts count resolution opportunities.
- **Betweenness centrality had no scale bound and a quadratic scratch reset**
  (P2-12). Each BFS source cleared all *n* scratch entries whether or not the
  search reached them — 10^10 writes on a 100K-node graph to answer all zeros.
  The reset now touches only visited nodes, and above 5000 graph nodes the run
  switches to a deterministic strided Brandes–Pich sample, scaled back and
  announced on stderr rather than presented as exact. This repo (2770 non-test
  nodes) stays exact.
- **The fuzzy-name edit-distance fallback pooled 5000 rows with no `ORDER BY`**
  (P2-12) — on a larger repo the `LIMIT` silently decides which names get a
  typo-correction chance, and the planner decides the `LIMIT`. Pinned to `n.id`.
- **One file could be a test to `dead-code`/`affected` and production to
  `project_map` at the same time** (incremental audit Δ3). The edge-level source
  filter carried only the anchored `tests/` prefix and the `_test.<ext>` leg,
  while the node-level classifier had the full `is_test_path` set — so an xUnit
  (`src/Tests/Api/AuthTests.cs`), Maven (`src/test/java/…`) or JS
  (`foo.test.js`) layout was counted as a production caller in hot-function
  ranking and as a test everywhere else. Measured on this repository: **792
  `calls` edges** were classified both ways at once. Both surfaces now generate
  their path legs from one `test_path_legs_sql`, with a differential test over
  the parity corpus. The NAME half stays deliberately narrower (no
  `*Test`/`*Tests` symbol suffix) and keeps its one-directional guard.
- **`grep` blamed a missing `ripgrep` for a missing working directory**
  (incremental audit Δ5). `current_dir` is applied during the spawn, so a
  project root that has been moved or deleted fails with the same
  `ErrorKind::NotFound` as an absent binary — and the message sent the user to
  install a tool already on their PATH.
- **`.EXE` was not recognized as the binary suffix** (incremental audit Δ5) —
  PATHEXT is upper-case by default on Windows and `cmd.exe` echoes what it
  resolved, so those invocations went unrecorded in the conversion metric. Only
  the extension is case-folded; the stem stays exact.
- **Two copies of the plugin took the statusLine slot from each other every
  session** (P2-17). `install()` rewrote the slot whenever the command string
  differed from the one it would write — but a plugin-cache copy and a global-npm
  copy (or a dev checkout) derive different absolute paths for the same current
  composite, so each run undid the other and settings.json was rewritten on every
  SessionStart. The slot now uses the version/surface-tolerant staleness rule the
  hooks already had: only a dead path, an unparseable command, or an older
  plugin-cache version dir is healed. `session-init.js` carried the same
  exact-match test and was changed with it — otherwise it would have reported
  `self-healed-stale-statusline` every session while `install()` quietly did
  nothing. This is the sibling of the hook ping-pong fixed in v0.104.x, whose
  regression test asserted `settings.hooks` and nothing else.
- **Statusline stand-down never re-armed** (P2-22). After three displacements the
  plugin releases the slot and stops competing — correct — but the counter was
  write-only, so when the competing provider was uninstalled and the slot went
  EMPTY, the plugin stayed statusline-less for the life of the manifest with only
  an undocumented env var to recover. An empty slot now re-arms it; an occupied
  one still doesn't (both directions mutation-tested).
- **`selfHealGlobalPkgs` treated npm's exit code as proof** (P2-22). `npm i -g`
  installs into whichever prefix the current node resolves, which under nvm — or
  an `npm --prefix` in the user's npmrc — need not be where the stale copy lives.
  npm exited 0, the stale package stayed exactly where it was, and the reset
  counter meant the retry budget could never be spent: one npm install per
  throttle window, forever. Success is now "the stale copies are gone", re-read
  after the install.
- **The model download's TLS fallback had three loose ends** (incremental audit
  Δ4). (a) The OS-trust-store retry fired on ANY failure, so a dead mirror or a
  404 cost a second full 600s attempt — with the caller's three tries, close to an
  hour of `doctor` reporting "download in flight" for something that failed in the
  first minute. It now fires only for failures a different root store could
  explain. (b) Extraction runs before the blake3 pin can be checked and the only
  size cap was on the COMPRESSED body, so a hostile gzip ratio could write
  gigabytes of unverified data to the user's disk; the unpacked side is now bounded
  from the tar header, before any member is written. (c) The trust path that
  produced the bytes is recorded in `model-download.json`, so "did this model come
  through the OS certificate chain?" is answerable after the fact. Content
  integrity was never affected — the blake3 pin is checked either way.
- **Fourteen of twenty languages had no numeric guard on the call axis**
  (P2-27). The edge baseline asserted `calls` for three languages and imports for
  six; everything else could lose its `walk_for_relations` arm silently, which is
  this repo's most-repeated bug shape. One table now covers every call-capable
  language (JS, Go, Java, C#, Kotlin, Ruby, PHP, Swift, Dart, C, C++, Bash) with a
  plain handle→helper fixture; disabling the Ruby arm turns it red and names Ruby.
  Markdown/HTML/CSS/JSON are excluded in writing — they have no call axis.

### Fixed in review (defects this batch introduced)
- **`dead-code . --json` and `dead-code <dir>/ --json` hard-errored whenever the
  answer was legitimately clean.** The new unindexed-path probe compared against
  root-relative stored paths, and `.` normalizes to `""` while a tab-completed
  `src/` keeps its slash — neither equals a stored path nor prefixes one with
  `/`. So the fix inverted exactly the case it set out to make honest: a clean
  repo answered "no indexed files" and exit 1, breaking anything gating CI on the
  exit code the day it went green. The original negative control used bare `src`,
  the one spelling of four that worked; it now covers all four, and forces the
  report empty with `--min-lines 999` so the probe is actually reached — without
  that the loop passed no matter what the probe did.
- **The `concurrency.group` fix did not serialize the two runs it named.** A
  group key is compared as a literal string, so the plain `inputs.tag ||`
  spelling used everywhere else in the file yields `release-v0.109.0` for a
  dispatch and `release-refs/tags/v0.109.0` for the tag push — still two keys.
  It now rebuilds the ref with `format('refs/tags/{0}', …)`. The other six sites
  are correct with the plain form because they feed `ref:` / `tag_name:`, where
  both spellings resolve to the same object.
- **`project_map`'s two inline copies of the test rule were left a fix behind.**
  Inside one `hot_functions` query the source rows were judged by the new
  anchored, case-sensitive GLOB and the target rows by the old unanchored,
  case-insensitive LIKE, so `Test_Signup` and anything in `*_test.ts` /
  `*_test.java` / `*_test.rb` vanished from `project_map` while `callgraph`
  listed all their callers. Both now splice `domain::prod_filter_and`, which
  takes the alias pair.
- **Eight JS test files wrote into the real Claude config.** v0.108.1 closed
  this in two files and missed a third; the fix for that missed seven more,
  including `adopt.test.js`, which redirects `HOME` in-process rather than
  spawning. Measured against a canary config dir: five `projects/<slug>/memory/`
  trees and a fabricated `9.9.9` plugin version landed in it. Now neutralized at
  module load, with a guard covering both the spawn and in-process shapes — the
  first version of that guard checked only spawns, and a second version stayed
  green when the statement was commented out because it used `contains` on the
  whole file rather than matching a line.
- **The 24h rate-limit backoff became load-bearing without ever having run.**
  Fixing the state clobber (P2-19) activated a constant written for a dead path.
  GitHub's unauthenticated quota resets *hourly*, and the backoff arm outranks
  `--force`, so one 403 froze every update check — force included — for a full
  day. Now one hour, with the recovery direction asserted.
- The plugin sidecar fetch gets the same single retry the binary sidecar has, and
  the plugin asset excludes `*.test.js`, matching the npm `files` allowlist.
- The `uses:` vacuity floor in the pin guard was 21 against a real count of 41,
  so a parse regression halving it would have passed.
- **A doubled separator in a path argument corrupted the index through MCP, and
  read as a clean answer on the CLI.** `src//a.ts` survived normalization intact:
  `dead-code src//` returned `[]` at exit 0 on a directory with real dead code,
  and `get_ast_node {file_path: "src//a.ts"}` did something worse than miss — the
  freshness path indexed the file a SECOND time under the non-canonical key, so
  `files` gained a `src//a.ts` row and one symbol became two nodes, each
  reporting a different path for the same source line. Measured both ways.
  Repeated separators now collapse in `merkle::normalize_rel_str_on`, the crate's
  single separator-normalizing implementation, so the CLI, the MCP tools and the
  write path agree. A leading `//` is preserved — that is a UNC host root, which
  `normalize_path_display_on` asserts survives; index keys are relative and can
  never take that branch. Known limit, stated in the test: `.//src/foo.rs` still
  errors, because stripping `./` leaves a leading `/` — pre-existing, and not
  reachable from tab completion.

  Existing indexes may already carry a `//` row written by an earlier MCP call;
  the next full reindex clears it, and nothing reads it in the meantime.
- **The `CLAUDE_CONFIG_DIR` guard had two vacuity holes**, both found by planting
  a leaking canary test file rather than by reading it. It matched
  `...process.env` and `HOME:` on the *same line*, so the Prettier-formatted
  spelling of the very lines it was written for went unseen; and it accepted a
  `delete` anywhere in the file, including inside a function body that never
  runs. It now scans a window and requires the statement at module scope, before
  the first `test(`. Widening the window then broke the single-line case it
  already caught — `take_while` on the closing brace dropped the first line — so
  that is pinned too.

### Internal
- **`index_files` was decomposed** (P1-9). 1705 → 1050 lines and max brace
  depth 13 → 10, measured on the function body at `origin/main` and at HEAD,
  across four commits: Phase 3
  (context strings + embeddings) and the 2d-bind / 2d-prune / 2e trio out first
  as `build_context_strings_and_embed` and `run_global_edge_post_passes`; then
  Phases 0 / 1a / 1b / 2b / 2b-ext / 2c as `buffer_then_delete_files`,
  `pre_parse_batch`, `insert_batch_nodes`, `mint_external_sentinels` and
  `restore_inbound_edges`, with the six mutable accumulators that used to live
  across 1700 lines collected into `SkipCounters` and `BatchInserted`; then the
  Phase-2 loop's own duplication — twelve copies of the source × target edge
  insert into `insert_relation_edges`, three copies of the `<module>`-of-file
  lookup into `module_node_of`, and five import branches that each re-parsed the
  same metadata JSON down to one parse. Each step states its phases' inputs and
  outputs in a
  signature instead of inferred from 1700 lines of shared mutable state. Both
  were chosen because they touch none of the caller's six accumulators, so the
  extraction is behaviour-preserving by construction. Verified that way too:
  indexing an identical snapshot with the before and after binaries produced
  8665 `calls`/`imports`/… edges on both sides with a zero-line diff (relation
  and confidence included) and 4450 nodes on both sides. The rest of the
  decomposition debt — the batch loop, the six accumulators, brace depth 13 —
  is untouched.

### Changed
- `project_map`'s `key_symbols` now excludes `*/tests.rs`, a side effect of
  converging it onto the shared filter (that leg was in the shared rule and not
  in the inline copy). `is_test_symbol` does classify that path as a test, so
  this is convergence rather than regression, but it narrows `key_symbols` for
  any Rust repo using the `mod tests` file layout.

## v0.108.1 — post-release review of v0.108.0

No index rebuild: `INDEX_VERSION` is unchanged at 53, and nothing here changes
the shipped binary's answer for a project-relative path. The two
developer-machine findings are the ones that mattered.

### Fixed
- **`scripts/install-e2e.test.js` acted on the developer's real Claude config.**
  Every child it spawned redirected `HOME` but spread `process.env` — and
  `claudeHome()` is `CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var wins.
  For anyone who exports it (the documented multi-account setup), this file —
  run by pre-commit whenever `claude-plugin/` or `scripts/` JS is staged, and
  deliberately excluded from CI — operated on their live config. Measured against
  a canary config dir: `uninstall --unadopt-all` deleted
  `<canary>/plugins/cache/code-graph-mcp/` and wrote `statusline-providers.json`
  into it, and nine §1 assertions failed because they were reading a stranger's
  settings. All 19 spawn sites now go through one `sandboxEnv()` that pins both
  names; under the same canary the run is 42/42 with the canary byte-identical.
  Third sibling of an escape hatch v0.108.0 closed in `tests/cli_e2e.rs` and
  `doctor.test.js` — and the review that found it named only one of the 19 sites.
- **The v0.108.0 guard against exactly that was itself inert.**
  `test_cli_doctor_rejects_an_unknown_flag_instead_of_repairing` set
  `CLAUDE_CONFIG_DIR=<home>` while asserting on `<home>/.claude/settings.json` —
  a path nothing in the program can create. A repair run in that sandbox writes
  `<home>/settings.json`, so the assertion the comment calls "what catches it"
  could never fire. Verified by running a real repair in a sandbox: it produces
  `<CLAUDE_CONFIG_DIR>/settings.json` and `statusline-providers.json`, which is
  what the test now watches, derived from the same variable it hands the child.
- **`module_overview` refused legal filenames.** Its "outside the project root"
  check keyed on a colon at byte 1 with no separator requirement — the over-broad
  predicate `src/cli.rs` copied and then fixed on its own side in v0.108.0,
  leaving the two surfaces disagreeing. `a:b.rs` is an ordinary POSIX filename;
  `src/cli.rs` now asserts it must survive normalization while this entry
  rejected it. The drive form now requires `C:`, `C:/…` or `C:\…`, and UNC roots
  are named explicitly.
- **Two Windows path spellings stopped being rejected on Windows.** The CI fix
  that scoped the lexical drive/UNC check to `!cfg!(windows)` handed back the two
  roots `Path::is_absolute` misses *there* too: bare `C:` is drive-relative and
  `\\server` has no share. Both fell through to the relative branch and came back
  as an ordinary empty result. The predicate now takes `is_absolute` as a
  parameter instead of reading `cfg!(windows)`, so the Windows branch is
  executable — and asserted — on the Linux leg.
- **A `grep` hint told users to type what they had just typed.**
  `had_literal_separator` located the pattern with `position(|a| *a == pattern)`,
  which finds the first token spelling that string — an earlier flag's *value*.
  `grep -t rust -- rust` computed pattern-index 1 against separator-index 2 and
  concluded there was no separator.
- **Three test files leaked a temp directory into `os.tmpdir()` on every run.**
  `tmp-dir.test.js` (52 accumulated) and `adopt.test.js` (159) both took a
  module-load `mkdtempSync` with no owner; `lifecycle.e2e.test.js` (154) cleaned
  each home in `t.after` but one was re-created afterwards by a child outliving
  the run that spawned it, so a run-end sweep was added as a second net. Under
  Claude Code `os.tmpdir()` is `~/.claude/tmp/`, the same accumulation
  `install-e2e.test.js` documents at 223. Measured after: 0 leaked across 2 full
  JS-suite runs and 4 isolated runs, down from +2 per run. Only the first of the
  three was in the review; the other two were found by counting what the
  directory actually held. `tmp-dir.test.js` also set only `TMPDIR`, which
  `os.tmpdir()` ignores on Windows (`TEMP`/`TMP`), leaving the isolation its ~5%
  flake fix depends on inert there; all three names are now set.

## v0.108.0 — audit 2026-07-27 remediation (12 P1s, eight review rounds)

Upgrade notes: **index rebuild required** — `INDEX_VERSION` 52 → 53. The server
wipes and rebuilds automatically on first start; a CLI-only setup rebuilds on the
next `code-graph-mcp incremental-index`.

### Fixed
- **A typo in `doctor --check-only` ran the repairs it was meant to prevent.**
  The CLI tested `args.includes('--check-only')` and discarded every other
  argument, so `--check-onlyy` / `--checkonly` / `--check_only` — and any other
  unrecognized flag — fell through to the default mode and performed the full
  repair pass, rewriting `settings.json` and `MEMORY.md`, while the user believed
  they had asked for the read-only one. The read-only contract this release
  otherwise hardens was one keystroke from being inverted. Unknown arguments now
  exit 2 before any diagnosis runs, and `--help` prints usage without acting.
  All **five** entry points, found one at a time and each by a different means:
  `doctor.js` (the original), `node lifecycle.js doctor …` (its own copy of the
  same `includes('--check-only')` line, found by checking which callers the new
  allowlist could reject), and `code-graph-mcp doctor …` — the surface users
  actually install — whose Rust dispatch filtered argv down to the single literal
  `--check-only` and dropped everything else, so the typo never reached the
  validation at all. The two JS entry points now share one `runDoctorCli`, which
  also keeps them from drifting on the exit-code rule (issues *unresolved after
  repair*, not issues found); the binary passes its argv tail through verbatim
  and propagates the exit code. The fourth is `bin/cli.js`, the npm/npx wrapper,
  which intercepts adopt / unadopt / uninstall before the binary: it guarded
  `--help` but ignored every other token, so `code-graph-mcp adopt --helpp` ran
  adopt and wrote the user's CLAUDE.md — one keystroke from the side effect the
  `--help` guard exists to prevent. The fifth, found by the pre-tag review after
  the other four were closed, is `src/main.rs`'s own **adopt / unadopt** arms —
  they discarded the argv tail exactly as its doctor arm used to, so
  `code-graph-mcp adopt --helpp` wrote CLAUDE.md for anyone on `cargo install` or
  the direct binary (npm/npx users were already covered by `bin/cli.js`). Passing
  the tail through is not the fix there: `adopt.js` reads only `argv[2]` as the
  action and parses no flags, so a typo would become the action name — those two
  subcommands reject any argument beyond `--help` instead. Regression tests run
  against all five, and the `doctor --help` text is byte-identical between
  `src/main.rs` (which intercepts `--help` so it stays side-effect-free) and
  `doctor.js`.
- **The npm surface described a scheme it had not touched since v0.74.**
  `code-graph-mcp adopt --help` / `unadopt --help` via npm/npx still said
  "install the code-graph memory file + MEMORY.md sentinel" — the pre-v0.74
  memory-dir target — while the same command on the binary described the
  project's CLAUDE.md managed block. Two texts for one command; npm users got
  the stale one. The same pre-v0.74 description survived in **`README.md`**,
  which ships inside the npm tarball — so the entry claiming this was fixed would
  itself have shipped alongside the text it says is gone. Both corrected.
- **`grep` explains a flag-shaped pattern instead of just failing on it.** A long
  flag the subcommand does not implement (`grep --quiet foo`) is bound to the
  *pattern* positional — deliberately, so that a term like
  `--no-default-features` stays searchable without an escape — which pushes the
  real pattern into the path list, and ripgrep then reports the user's search
  term as a missing file. Accurate and unreadable. The behavior is unchanged
  (this is a published CLI surface and `grep -- --quiet` must keep working); what
  is new is a note naming the token, why it was read as a pattern, and the `--`
  escape. It fires only on the pairing that is actually ambiguous — a `--word`
  pattern *and* a missing-path error — so a genuine literal search that fails for
  another reason is not lectured.
- **`unadopt` deleted the user's own prose from CLAUDE.md.** Stripping the
  managed block matched from the *first* occurrence of the begin marker anywhere
  in the file through the real block's end marker — and the block we write
  invites the user to mention that marker ("do not edit inside this block"). One
  sentence quoting it cost a 1078-byte CLAUDE.md all but 43 bytes, at exit 0,
  printing `De-blocked →`. `uninstall({unadoptAll:true})` runs this over every
  registered project, and the SessionStart teardown runs it on the current one.
  Four separate amplifiers, all fixed: the match is now line-anchored (a
  mid-sentence mention is prose, not a block opener) and refuses to span another
  begin marker (so a marker quoted on its own line no longer anchors the match);
  the marker pattern excludes newlines, so a marker truncated mid-write can no
  longer run across lines to the next `-->`; the orphan-marker self-heal fires
  only when no end marker survives anywhere; and the detail-file delete requires
  the managed-by marker to *be* the first line rather than appear in it. A
  CLAUDE.md that is a symlink is now written through rather than replaced by the
  atomic rename — which had silently detached the link, left the real file's
  block in place, and still reported `blockPruned: true` — and is never unlinked,
  since "delete the file we created" does not apply to a link the user made.
  `isAdopted` is line-anchored for the same reason: a quoted pair read as
  installed, which gates auto-adopt, so the block was never written.

  **Behavior change:** the orphan-marker self-heal no longer removes the content
  under a stray begin marker — only the marker line. It used to strip to the next
  blank line on the theory that what followed was our truncated block, and that
  is not decidable: a leftover fragment and a user's notes under a marker they
  quoted from our own instructions are byte-for-byte the same shape. In a repo
  that had never been adopted, the heal took a 221-byte CLAUDE.md to 100 bytes,
  and `adopt` runs the same strip, so it fired on auto-adopt every SessionStart.
  The cost of the narrower rule is that a genuinely truncated block leaves a
  visible fragment; `isAdopted` requires a well-formed begin *and* end, so the
  next adopt still writes a clean block, and the fragment is inert.
- **The `.corrupt-*` backup was not the original bytes.** It was written from a
  UTF-8-decoded string, so every invalid byte became U+FFFD before the original
  was overwritten — the backup is the user's only copy, and for a settings.json
  containing any non-UTF-8 byte it was a lossy transcription (measured 50 B → 56 B).
  The file is now read as a Buffer and copied verbatim; only the parse decodes.
  The shipped test asserting "the backup must be the original bytes, verbatim"
  passed throughout because its fixture was pure ASCII.
- **`find_references` told an LLM client to bypass a filter that does not
  exist.** The `<external>` binding below is deliberately excluded from symbol
  *resolution*, so an import-only name reaches the not-found arm, which
  re-queried unfiltered, found the sentinel, and reported "all match(es) are in
  test/bench paths (`<external>`) … bypass the test filter" — a path that is not
  on disk, a filter that is not applied, and recovery advice that cannot work.
  The MCP surface now answers with the `imports` rows like the CLI already did,
  so this release's "both refs surfaces answer for imported std names" is true of
  both rather than half.
- **`sync-versions.js --check` reported agreement it had not checked.** A version
  site that did not exist was recorded as `SKIP (not found)` and did not set the
  drift flag, so deleting a platform `package.json` produced "All version sites
  agree" and exit 0 — eight sites checked, nine claimed. A corrupt, empty or
  unreadable site was worse: `JSON.parse` threw out of the loop before a single
  table row printed, every site after it went unexamined, and with real drift
  elsewhere the crash hid it entirely while exiting 1 (node's uncaught-throw
  code) indistinguishably from a clean drift report. Missing is now `MISSING` and
  counts as drift; unreadable sites are caught per-site, printed as `UNREADABLE`
  with the reason, and exit **2**, so a caller can tell "versions disagree"
  (fixable by re-running this script) from "a site could not be read at all"
  (not). Note that the write path already exits 2 on a cargo-build failure; the
  two modes are disjoint and the caller picks one, but the code is not unique
  across the script. `--check` still never writes. It is not currently wired
  into pre-commit or CI — both roll their own comparison — so this hardens a
  gate that its own docstring invites you to adopt rather than one in use.
- **A non-object `hooks` value made `install` a silent no-op that reported
  success.** `settings.hooks || {}` accepted an array, and the named properties
  assigned onto it are dropped by `JSON.stringify` — so `{"hooks": []}` came back
  out unchanged with zero of the six hooks registered, while `install` printed
  `settings=true` and `health` printed `OK — all paths valid`. A string or number
  threw an uncaught "cannot create property" instead. Any non-object value is now
  replaced, exactly as a missing key is.
- **An unwritable `~/.claude` escaped as a raw stack trace, then reported
  healthy.** A settings.json we can read but not write (read-only home, EROFS, a
  container mount) threw out of `fs.writeFileSync` with no `[code-graph]` line;
  the follow-up `health` then said `OK — all paths valid`, because it validates
  the paths it finds and it had registered none. It now reports the cause,
  changes nothing, exits non-zero, and — critically — does **not** stamp the
  install manifest, which would have told the next run "already installed" and
  left the plugin inert after the user fixed the permissions. `doctor` reports it
  too: the first version of this fix wired the new state into `lifecycle`'s CLI
  only, so `doctor` still printed "install reported no change (settings already
  had entries)" about a file it had just failed to write.
- **A read-only `~/.claude` was diagnosed as a missing npm package.** `doctor`'s
  `hooks-invalid` repair arm never learned the unwritable-settings state that its
  sibling arm had just been taught, and `scanForBrokenPaths` cannot surface it
  (the file reads fine), so a `chmod` on the config directory produced
  "plugin scripts may be missing — reinstall: npm install -g …". Both arms now
  report the same cause for the same failed `install()` call.
- **The legacy-migration delete guard was the loose copy.** `unadopt`'s
  detail-file guard was tightened to require a whole-line HTML comment, but
  `migrateLegacyMemoryDir` kept a bare untrimmed `startsWith` — and migration is
  the hot path, running on every SessionStart via auto-adopt, while `unadopt` is
  explicit. A user file whose first line merely began `<!-- adopted-by:` was
  still deleted there.
- **Uninstall stranded every adopted project's CLAUDE.md block.** The
  adopted-projects registry lives inside the cache directory, and the SessionStart
  teardown wiped that directory *before* unadopting. Only the current project was
  unadopted; every other repo's entry went with the wipe, so a later
  `uninstall --unadopt-all` read an empty registry, reported `unadopted: []`, and
  the blocks stayed behind with nothing left that knew where they were. The
  teardown now unadopts first, and the wipe itself preserves a registry that
  still names projects — a registry that is empty or absent strands nothing and
  is removed as before, so the common single-project case still leaves no residue.
  Same capture-before-cleanup ordering `uninstall()` already had to learn; this
  was its sibling.
- **`similar` destroyed a version-lagging index** — it was the one read command
  still opening the database through the *indexer* constructor
  (`open_with_vec`), which performs the destructive `INDEX_VERSION`
  revalidation. After upgrading the binary, a single `code-graph-mcp similar foo`
  in a project with no MCP server running wiped the index to 0 nodes and nothing
  rebuilt it — the daagu failure that `open_nondestructive` exists to prevent,
  reached through the one door still left open. The wipe happened before the
  `vec_enabled()` check, so builds without `embed-model` — which bail out of
  `similar` immediately — destroyed the index just the same. `similar` now goes
  through `CliContext`, which also gives it the worktree read-side fallback the
  other 19 commands already had.
- **An unusable `~/.claude/settings.json` was silently overwritten** — the
  plugin's `readJson` collapsed *every* failure into the same `null`, so
  `readJson(settingsPath()) || {}` handed `install()`/`update()` an empty object
  and the next atomic write replaced the whole file. One trailing comma cost the
  user their `model`, `env`, `permissions`, `enabledPlugins` and their own hooks,
  with no copy left anywhere. Only a genuine `ENOENT` now counts as "absent";
  a file that exists but yields no settings object is never rebuilt over
  in place. Where it can be read — unparseable text, or valid JSON that isn't an
  object (`null` / `[]` / `123` / `"str"`) — it is preserved as
  `settings.json.corrupt-<timestamp>` first. Where it cannot (**unreadable** after
  a stray `sudo` leaves it root-owned `0600`, `EPERM`, `EIO`, or the path is a
  directory), no copy is possible, so the original is left untouched and
  `install()`/`update()` **do nothing at all** — no hooks, no statusline, no
  manifest stamp — leaving the plugin inert until the file is repaired.

  Every surface that used to paper over that now reports it: the CLI says
  "Not installed … Nothing was changed." and exits non-zero instead of printing
  "Installed", `doctor` reports `settings.json unusable` instead of
  `all paths valid`, and **both** of its repair arms name the real cause instead
  of claiming the hooks were already registered or that an npm reinstall is
  needed. An empty or whitespace-only settings.json — what a crash mid-write
  leaves — counts as absent and is rebuilt with no backup litter, while a
  BOM-prefixed but otherwise valid file (PowerShell's default encoding) is now
  parsed rather than misfiled as corrupt.
- **A read-only query could delete part of the index** — `<external>` is a
  pseudo-file with no on-disk counterpart, so the query-time freshness resync
  classified it as a *deleted* file and dropped its row, CASCADE taking every
  sentinel node and every import edge into them; a later incremental pass did not
  restore them, because only a file whose content changed re-emits its import
  relations. Any read command that displayed or resolved an external name reached
  it — `show HashMap` destroyed them while printing `Symbol not found`, i.e. a
  query that reported failure still damaged the index. Pre-existing, but the
  `<external>` binding below puts far more nodes behind it, so it is fixed here.
- **`module_overview`'s 60-second cache swallowed `include_deps` / `include_dead`**
  — the flags are not part of the cache key and the folding happened *after* the
  cache early-return, so once any call warmed a path (SessionStart injection
  does), an `include_dead:true` call came back byte-identical to a plain one: no
  `dead_code` section and no `dead_code_unavailable` marker either, which is
  indistinguishable from "nothing dead here".
- **`find_dead_code` reported a false clean on Windows** — it was the sixth
  path-taking MCP tool and the only one still reading `args["path"]` raw. A
  client passing `src\parser` produced a LIKE prefix that matched no row, and the
  tool answered "No dead code found". A source-scanning drift guard now fails on
  any tool that reads a path argument without normalizing it, replacing the
  hand-maintained tool list that let this one through.
- **Windows adoption metrics were structurally zero** — `paths_match` split only
  on `/`, so a recorded `D:\repo\src\Foo.cs` became one opaque component and
  never matched a repo-relative path. v0.107.0 fixed the call-recognition half of
  the conversion metric (`.exe` tokens) and left this half dark.
- **`.\src\foo.rs` produced a bad index key** — the `"."` / `"./"` prefix tests in
  `normalize_user_path_from` were spelled Unix-only, so PowerShell's default
  tab-completion spelling fell through to `./src/foo.rs`, a key the index never
  contains. Separator normalization now happens before the prefix tests, and
  `is_cwd_anchored` recognizes `.\` / `..\` — so its "cwd-anchored paths never
  rebase" promise holds on Windows too. The branch is parameterized by platform,
  so the Linux CI leg executes it.
- **Windows-absolute paths given to a non-Windows binary answered "no results"
  instead of erroring** — `Path::is_absolute` is irreducibly platform-native, so
  `D:\repo\src\Foo.cs` and `C:/repo/src` were not absolute on a Unix host and
  fell through to the relative branch, emerging as the index key
  `D:/repo/src/Foo.cs`. A drive prefix or UNC root is now rejected by spelling on
  every platform, matching what the MCP entry already did.
- **cfg predicates were extracted as function calls (`INDEX_VERSION` 53)** —
  `#[cfg(not(windows))]` and `cfg!(any(unix))` put `not(…)` / `any(…)` in a
  token tree byte-identical to a call, and every predicate name is lowercase, so
  the CamelCase pattern guard passed them through. A project defining `fn any` or
  `fn not` had those calls bound to the wrong symbol.

### Changed
- **Rust `use std::…` now binds the `<external>` sentinel (`INDEX_VERSION` 53)**
  — v0.107.0 dropped statically-external imports entirely to stop the phantom
  `imports → fn fs` edges. Binding them explicitly stops the same phantoms *and*
  lets the existing import-contradiction prune remove the sibling **call**
  phantom that `use std::mem::swap; swap(&mut a, &mut b)` fabricates against a
  project `swap` (risk names: swap / replace / take / min / max / read / write /
  spawn / exit / sleep). Two shapes that bypassed the root check entirely are
  covered in the same pass: a leading `::` (`use ::std::mem::swap`) and a
  root-level use-list (`use {std::io::Read, crate::a::cb}`, now flattened so a
  mixed list gets a mixed verdict).
- **`resolve_fuzzy_name` is single-sourced** in `src/resolve.rs`. The CLI copy
  was hand-written from the MCP one and had zero tests — the same shape as the
  2026-06-03 incident that module exists to prevent.
- **`find_references` / `refs` now answer for imported std names.** A consequence
  of the `<external>` binding above: `refs HashMap` returns the `imports` edges
  that bind it, where it previously answered `Symbol not found`. The rows are
  import sites, not uses of the type.

  Symbol *resolution* is unaffected. `<external>` sentinels are excluded at the
  by-name query layer (`get_nodes_by_name`, `get_nodes_with_files_by_name`,
  `get_first_node_id_by_name`, `find_functions_by_fuzzy_name`, and the call-graph
  CTE seed), so `show` / `impact` / `callgraph` / `similar` / MCP `get_ast_node`
  all behave as they did before v53 for a name that only exists as an import, and
  a project `fn take` in a repo that also does `use std::mem::take` still
  resolves to the project symbol. Filtering at the query rather than per surface
  is deliberate: an earlier attempt patched two call sites and left `show
  HashMap` printing `module <external>/HashMap` with exit 0.

### CI
- **A tag push could publish to npm without ever running fmt, clippy, or one
  Rust test.** `release.yml`'s only trigger is `tags: ['v*']` and its chain was
  build → publish → smoke — no `needs: ci`, no `workflow_run`. `bump-version.sh`
  instructs `git push && git push --tags` back to back, so ci.yml's verdict on
  the commit does not exist yet when the tag fires; publish's own gates (artifact
  `--version` matches the tag, full JS suite) cover zero Rust. The 07-24 audit
  found clippy RED on already-committed code — under that topology it would have
  reached npm, which is irreversible. A `gate` job (fmt, clippy on both feature
  sets, `cargo test --features embed-model`) now blocks `build`.
- **`cache-warm.yml` primes that gate.** Actions caches are ref-scoped, so a
  cache the gate saved under `refs/tags/vX` would be invisible to the next tag
  while still consuming the repo's LRU budget. The gate is therefore
  restore-only (`save-if: false`) and a new main-branch `warm-gate` job is the
  sole writer of the `release-gate` key, mirroring the gate's commands
  byte-for-byte — `-D warnings` included, since lint flags participate in
  cargo's fingerprint and a cheaper variant would warm a cache the gate then
  misses. It needs its own key rather than sharing `warm`'s: the gate compiles
  the dev profile with `--all-targets`, a different artifact set from the
  `--release` host build. On a cache miss the gate just compiles cold, as it did
  before.
- **Both commit-gate scripts were non-executable in the git index.**
  `scripts/githooks/pre-commit` and `scripts/pre-commit.sh` were mode `100644`;
  git silently ignores a hook without the exec bit and the commit goes straight
  through, so the gate was inert on every fresh clone. It looked healthy on the
  author's box only because the working tree carried `rwx` — a bit that was never
  committed, which is why `ea0166d`'s "any machine that cuts a release has the
  gate active" was false. Both are now `100755`, and the `fmt` job asserts the
  index mode so it cannot silently revert.

  That assertion earned itself before this release shipped. The repo sets
  `core.fileMode = false`, so git ignores the working tree's exec bit entirely:
  `git add -A` can never stage a mode change, and only an explicit
  `git update-index --chmod=+x` sets it. A `git stash push -- scripts/` +
  `git stash pop` round-trip during this batch silently reverted both files to
  `100644` while the working tree kept showing `rwx` — the same invisible state
  the original defect lived in, restored by an operation that looks unrelated.
  Nothing but the index check found it. Generalise: under `core.fileMode=false`,
  an explicitly-set mode is not durable across stash/checkout, so assert it in CI
  rather than assuming a one-time `--chmod=+x` holds. The assertion now also runs
  in `release.yml`'s gate: `ci.yml` never fires on a tag, so before this a
  reverted exec bit would have published silently with the local commit gate
  inert.

### Internal
- **A new e2e test wrote to the real `~/.claude`.** The doctor flag guard's
  regression test inherited the ambient `HOME`, and its RED state — "doctor
  performed the repairs", the exact regression it exists to catch — ran the full
  repair pass over the developer's own settings.json, statusline registration,
  binary pin and npm cache. The mutation run recorded in this batch's own commit
  message did precisely that. `HOME` and `CLAUDE_CONFIG_DIR` are now sandboxed
  per-invocation (the latter because `claude-config.js` honours it ahead of
  `os.homedir()`), the test asserts nothing landed in the sandbox home, and it
  pins `_FIND_BINARY_ROOT` so a redirected `CARGO_TARGET_DIR` — this repo's own
  documented mitigation for target-dir growth — no longer fails it with
  "must name the offending token" while the real cause ("doctor.js not found")
  sits in stderr. Its JS sibling in `doctor.test.js` sandboxed only `HOME` — the
  same half-applied shape one file over, and `claude-config.js` reads
  `CLAUDE_CONFIG_DIR` ahead of `os.homedir()`, so a developer who exports it had
  the full repair pass land in their real config. Verified closed with a canary
  config dir: byte-identical after the suite runs. Verified: with the fix in place, the RED run still reddens and
  still takes ~89 s, and settings.json, the binary pin and the npm log count are
  all byte-identical afterwards.
- **A fourth inert negative control, deleted rather than relabelled.** The
  `Debug` trait-sentinel block added to the MCP find_references test was labelled
  "the live half of the control"; instrumenting both branches showed the payload
  byte-identical with and without the `<external>` exclusion, because
  `find_references` answers from EDGE rows, which never enter the by-name lookups
  the exclusion filters — as that constant's own doc comment says. An inert block
  invites the next reader to trust it.
- **`is_selectable_definition` had zero live coverage across 1346 tests**, while
  a comment asserted the reader guard "does go red under both mutations". Only
  the SQL mutation reddens it: the two guards sit in series with the SQL one
  first, so no reachable input carries an `<external>` path into the Rust
  predicate. The claim is corrected in place and the predicate now has a direct
  unit test, labelled as covering the function and not its reachability — it
  becomes load-bearing the moment the SQL exclusion is relaxed for the `deps`
  disclosure its doc comment contemplates.
- **Two independent test flakes, both root-caused.** `trackReadAndMaybeHint` failed
  about one full-suite run in seven: it spawns a real `node` stub through
  `cg-answer`, whose 2 s timeout is a product decision for a PreToolUse hook, so
  under load cold node startup exceeded it. A test-only `_CG_ANSWER_TIMEOUT_MS`
  seam (next to the `_CG_ANSWER_BINARY` one already there) insulates the tests
  without lowering the hook's bar. What remained after that was a DIFFERENT
  test — `cgTmpDir() returns the same path and creates the directory` — at 2
  failures in 40 instrumented runs: it wipes the process-wide `CG_TMP_DIR` and
  asserts it is absent, while `node --test` runs test files in parallel and any
  sibling calling `cgTmpDir()` re-creates it inside that window. That file now
  owns a private `TMPDIR`. Measured 0 failures in 40 runs afterwards, with the
  failing test names recorded per run rather than only a pass count — the earlier
  "30 clean runs" claim could not distinguish the two flakes because it did not
  capture names.
- **Three shipped promises verified against behavior, two already guarded.** A
  sweep had listed the v0.85.4 `doctor` exit-code semantics, the read-only
  secondary MCP explanation, and the `application_id` downgrade preservation as
  unclassified. All three hold. Two turned out to have live tests — mutation runs
  confirm `test_downgrade_open_never_wipes_newer_index` reddens when the version
  check is made symmetric again, and `test_secondary_not_found_includes_stale_hint`
  reddens when the hint text is removed. The third had a live PREDICATE test
  (`unresolvedCount`) but no wiring test, which is the gap this repo already
  named in v0.45.3; the exit code is now produced by three entry points, so a
  report-vs-exit-code consistency test runs against each. Its limits are recorded
  in the test itself rather than implied.
- **The `<external>` query-layer exclusion had no live guard.** Round-6 mutation
  runs showed that both tests believed to cover it — `external_sentinel_tests` in
  `src/resolve.rs` and the "negative control" in the MCP integration test —
  survive deleting `EXCLUDE_EXTERNAL_BY_NAME` *and* neutering
  `is_selectable_definition`. The reason is structural, not a fixture slip: the
  by-name fuzzy path already carries `AND n.type != 'module'`, and a sentinel is
  typed non-`module` only when no project symbol shares its name — exactly the
  case where there is nothing to discriminate. Three attempts to make that
  control live failed; the claim has been removed from the test rather than
  replaced with a fourth inert one. The real guard,
  `show_does_not_resolve_a_name_that_exists_only_as_an_import`, drives the binary
  at the surface where the defect was observed (`show HashMap` printing
  `module <external>/HashMap` at exit 0) and does go red under the mutation.
- **Workflow drift guard** (`release_and_cache_warm_workflows_do_not_drift`) pins
  what had only ever been a comment: one toolchain pin and one rust-cache pin
  across both files, every `shared-key` release.yml restores is written by a
  cache-warm.yml job, the gate is restore-only while `warm-gate` is the writer,
  the two jobs run the same cargo commands, the gate still runs fmt + both clippy
  passes + the test suite, and `build` still declares `needs: gate`. The comment
  version of this rule was already in `cache-warm.yml` when v0.101.0 shipped a
  `key`/`shared-key` mismatch that made all five release builds cold. Each
  assertion was checked at development time by mutating the workflow file and
  confirming the guard goes red (nine such mutations); those checks are not
  themselves committed, so treat the guard the way you would any other — the
  first draft of it passed against the *prose* explaining `save-if: false`
  rather than the setting itself, and only a mutation run caught that.
- **Read commands can no longer reach the destructive constructor**
  (`tests/reader_nondestructive.rs`). The `similar` fix above shipped with a
  regression pin hardcoded to `similar`, which read command #26 would walk
  straight past — the same shape that let `similar` itself survive four audits.
  Two class-level guards replace it: a behavioural sweep that re-stamps a stale
  `INDEX_VERSION` before each of 21 read subcommands and asserts the node count
  survives, and a source scan that fails the moment any `cmd_*` outside the two
  indexer entry points types `Database::open_with_vec`. A negative control
  (pointing one read command at the destructive constructor) reddens all three.
- Cross-language drift guard (`tests/predicate_parity.rs`) runs a shared corpus
  through all four `is_test_path` mirrors (Rust, SQL, JS, two Python) and diffs
  them. Only the Rust↔SQL pair had a mechanical differential before.

### Errata (v0.106.0)
- The `ureq` `platform-verifier` feature — a *functional* dependency change, not
  a release chore — was committed inside `chore(release): v0.106.0`. Without it
  the TLS fallback added in `5739dad` would have panicked on
  `RootCerts::PlatformVerifier`, unwinding the download thread and leaving
  `model-download.json` permanently `in_flight`. That combination was never
  published, so no released version was affected; recorded here because
  `git blame` points at a release commit for a runtime fix.

## v0.107.0 — Windows MCP path lookup, test-classifier parity across its four mirrors

Upgrade notes: **no index rebuild required** — `INDEX_VERSION` is unchanged at
52. Two classification changes apply at query time and take effect immediately:
files named `*Spec.cs` / `*Spec.java` / `*Spec.php` / `*Spec.swift` are now
treated as **production** code again (they were misclassified as tests in
v0.106.0), and `Test_Foo.py` / `Conftest.py` are production too, matching what
pytest actually collects. If you relied on the v0.106.0 behavior, pin
`0.106.0` — there is no flag for it, because both shapes were defects.

Library consumers: `domain::PASCAL_TEST_STEMS` is **removed**, replaced by
`domain::PASCAL_TEST_STEM_EXTS: [(&str, &[&str]); 3]` (per-stem extension sets).
The flat `PASCAL_TEST_STEMS × PASCAL_TEST_EXTS` cross-product it existed for is
what produced the `Spec` defect. Binary and MCP users are unaffected.

Follow-up to the v0.106.0 field reports
([#34](https://github.com/sdsrss/code-graph-mcp/issues/34),
[#36](https://github.com/sdsrss/code-graph-mcp/issues/36)) — the fixes there were
correct but incomplete, and this release closes the halves they left open.

### Fixed
- **MCP tools still missed the index on Windows (#34, second half)** — v0.106.0
  taught `ensure_file_fresh_opt` to normalize separators, but that helper returns
  `Result<()>`, so the normalized path never left it. All five tools taking a
  path (`get_ast_node`, `find_references`, `dependency_graph`, `get_call_graph`,
  `module_overview`) went on to hand the **raw** argument to the index. A client
  echoing back `src\Foo.cs` therefore refreshed the right file and then answered
  `File 'src\Foo.cs' not found in index` for a file that was indexed. Normalization
  now happens once at tool entry, before both the freshness call and the lookup —
  which also covers `skip_indexing:true`, where the freshness helper never runs.
- **`affected` and the SQL surfaces disagreed about pytest files** — `is_test_path`
  lower-cased before its `test_*.py` / `conftest.py` legs while the SQL mirror
  used case-sensitive `GLOB`, so `api/Test_Signup.py` counted as a test file in
  `affected` and as production in dead-code and search filtering. Both sides are
  case-sensitive now, matching pytest itself (`fnmatch_ex` does not normcase, and
  conftest is discovered by literal basename).
- **`*Spec.cs` / `*Spec.java` / `*Spec.php` / `*Spec.swift` were classified as
  tests** — `Spec` is a suite name in ScalaTest/Kotest but an ordinary production
  noun elsewhere, so `src/Contracts/OpenApiSpec.cs` had its symbols dropped from
  `search` entirely and appeared under "test file(s) to re-run" in `affected`.
  The stem is now scoped to `scala`/`kt`.
- **`affected` depth-group headers contradicted their own listing** — with 300
  files at depth 1 and a display cap of 40, the header read `depth 1 (300 file(s)):`
  above 40 paths. Truncated groups now print `depth 1 (40 of 300 file(s) shown):`.
- **Model-download diagnostics test wrote to the real user cache on macOS and
  Windows** — the test redirected `dirs::cache_dir()` via `XDG_CACHE_HOME`, which
  `dirs` honors on Linux/BSD only (macOS is unconditionally `$HOME/Library/Caches`;
  Windows resolves `SHGetKnownFolderPath`, not `%LOCALAPPDATA%`). The redirect was
  a no-op there: the test clobbered the developer's own `model-download.json` and
  then failed its "never attempted" assertion on the next run. The state-file path
  is injected now, which also removes an `env::set_var` racing sibling tests.

## v0.106.0 — Windows grep correctness, xUnit/JVM/pytest test detection, model-download diagnosability

Upgrade notes: **no index rebuild required** — `INDEX_VERSION` is unchanged at
52. The widened test-file classification is applied at query time
(`domain::is_test_node` ORs the stored parser flag with the path heuristic), so
existing indexes pick it up immediately. No config changes required.

Field reports from a Windows 11 + C#/TypeScript monorepo (3,819 indexed files):
issues [#34](https://github.com/sdsrss/code-graph-mcp/issues/34),
[#35](https://github.com/sdsrss/code-graph-mcp/issues/35),
[#36](https://github.com/sdsrss/code-graph-mcp/issues/36).

### Fixed
- **`grep` unusable on Windows at repo scale (#34)** — one root cause behind all
  three reported defects: path spellings were compared without normalizing
  separators. `rg --files` emits `src\foo.rs`, `git ls-files` emits `src/foo.rs`,
  and `canonicalize()` emits `\\?\D:\…`, so `tracked_files_missed_by_walk`'s
  `walked.contains(t)` matched **nothing** on Windows and the "supplement" became
  the entire tracked set — 3,284 absolute paths on one argv, i.e. the reported
  `os error 206` (Windows caps a command line at 32,767 chars). The same
  mismatch made every file get scanned twice (walk + supplement → duplicated
  matches in two different path spellings), leaked the `\\?\` extended prefix to
  stdout, and broke AST annotation entirely, because the index stores
  `/`-relative paths and the lookup key never matched — which is why Defect 3
  ("no containing function/class in the output") appeared on Windows only.
  `relativize_path` now normalizes both sides (`\\?\`/`\\?\UNC\` stripped, `\`→`/`,
  drive-letter case-insensitive) and is the single key producer for output,
  dedup, and index lookup.
- **`grep` silently searched only the first 500 supplement files (#34)** — the
  cap bounded the file *count* while the real constraint is argv *bytes*, so a
  deep layout blew the limit well before 500 and a shallow one dropped files
  without saying which. The supplement is now passed as relative paths (dropping
  the repeated root prefix) and split into argv-budgeted batches — 24 KB on
  Windows, 512 KB elsewhere, overridable via `CODE_GRAPH_RG_ARGV_BUDGET` — with
  results merged and globally sorted/deduped as before. No tracked file is
  dropped.
- **`affected` reported "0 test file(s) to re-run" for C#/Java/Python suites
  (#36)** — `is_test_path` only knew JS/Rust/Go conventions, so
  `src/Tests/<Project>/<Name>Tests.cs` (xUnit/NUnit/MSTest) and
  `src/test/java/…` (Maven/Gradle) matched no leg. A silent false negative in
  the one output a CI or pre-commit integration acts on. Added: case-insensitive
  `test`/`tests` path segments, the PascalCase `*Test`/`*Tests`/`*Spec` class
  convention across 8 extensions, `_test.<ext>` beyond Go/Rust, and pytest
  `test_*.py` / `conftest.py`. The Rust predicate and its SQL mirror
  (`is_test_node_sql`) are now generated from shared constants so they cannot
  drift; the parity test covers 16 new cases including near-misses
  (`src/latest.cs`, `src/protest/api.cs`, `src/testing/api.cs`) that must stay
  production.
- **Embedding-model download failed invisibly (#35)** — the background download
  logged to `tracing` only, so a permanently-degraded install was
  indistinguishable from one that simply hadn't finished, and `doctor` printed
  the same "auto-downloads in background … retry shortly" forever. Each attempt
  now records its outcome to `<platform cache>/code-graph/model-download.json`;
  `doctor` and the FTS5-only search note report "no download has been attempted"
  vs "download in flight (attempt N)" vs "download FAILED after N attempt(s):
  <error>". `doctor --json` gains an additive `model_download` field.
- **Model download now falls back to the OS certificate store (#35)** — ureq's
  bundled webpki roots do not include a corporate MITM proxy's private root, so
  on a TLS-inspecting network every fetch failed while `curl` (schannel)
  succeeded. Bundled roots are still tried first; on failure the request is
  retried with `RootCerts::PlatformVerifier` (new `ureq` `platform-verifier`
  feature) and both errors are reported together.
- **Model download timeout was sized for the wrong archive (#35)** — the 120s
  global timeout and its "~30MB compressed" comment predate an 83 MB tarball,
  silently requiring ~700 KB/s sustained. Raised to 600s.
- **Windows: a file given by absolute or backslash-typed path was reported "not
  in index"** — found by auditing with the new skill, same class as #34.
  `cli::normalize_user_path_from` returns the key that `affected` / `deps` /
  `trace` / `show` look up, and two of its branches returned
  `strip_prefix(root).to_string_lossy()` verbatim — which keeps the native
  separator. On Windows `affected D:\repo\src\Foo.cs` therefore produced the key
  `src\Foo.cs` against an index storing `src/Foo.cs`, and a present, indexed file
  was silently dropped. (The subdirectory branch was already correct: it goes
  through `collapse_within_root`, which decomposes into `Component`s and re-joins
  with `/`.) The MCP freshness entry point (`ensure_file_fresh_opt`) now
  normalizes the same way, which also makes a trailing `src\` register as the
  directory it is.
- **Windows: CLI invocations were not counted toward the conversion metric** —
  `outcome::cli_call_in_line` matched the binary with
  `t == "code-graph-mcp" || t.ends_with("/code-graph-mcp")`, missing the
  `…\code-graph-mcp.exe` form the plugin actually resolves on Windows — both the
  separator and the `.exe` suffix. The metric read zero and `doctor` reported the
  funnel DARK with nothing broken. The JS delivery surface (`find-binary.js`,
  `auto-update.js`) had handled `.exe` all along; the Rust side had no `.exe`
  awareness anywhere.
- **`grep` mangled Unix filenames containing a backslash** — caught before
  release, introduced by the #34 fix itself. `\` is a legal filename character on
  Unix (only `/` and NUL are illegal), so rewriting separators unconditionally
  renamed a real `src/od\bc.rs` to `src/od/bc.rs` in output and produced a lookup
  key that missed the indexed path — the same failure mode as #34, in the
  opposite direction (`merkle::normalize_rel_path` also rewrites only under
  `#[cfg(windows)]`). The rewrite is now gated on whether `\` is a separator on
  the target platform.
- **The test-file predicate had five copies and only two were widened (#36
  follow-up)** — `domain.rs` carries a "Five sites must agree" note, but the
  inventory document it points at no longer exists, so the first pass updated
  only the Rust predicate and its SQL mirror. Three ports were left on the old
  narrow rules: `claude-plugin/scripts/pr-impact-comment.js::isTestPath` (whose
  own test is named `isTestPath mirrors domain::is_test_path patterns`), and the
  Python ports in `scripts/embedding_benchmark/build_tier3_slice.py` and
  `diag_retrieval_drop.py` (both docstringed "Port of src/domain.rs"). The JS
  gap made the PR "test gaps" comment report every Java/C# test file as
  uncovered production code; the Python gap inflated the retrieval benchmark's
  measured miss rate. All five now generate from the same constant lists, and
  agreement is checked by running one 31-path corpus through every executable
  implementation rather than by maintaining parallel test tables. The two
  deliberately divergent sites (`PROD_SOURCE_FILTER_AND`/`TEST_SOURCE_FILTER_OR`
  and the closure in `pipeline::resolve::refine_ambiguous_targets`) are
  unchanged, as their own comments require.
- **Windows: `outcome` looked in the wrong transcript directory for a
  canonicalized project path** — `project_slug` maps every non-alphanumeric byte
  to `-`, which makes it immune to the separator itself (`D:\dev\r`, `D:/dev/r`
  and `D:\dev/r` all slugify identically) but not to the extended-length prefix:
  `\\?\D:\dev\r` became `----D--dev-r` instead of `D--dev-r`. Since
  `canonicalize()` prints the `\\?\` form and `outcome --project` takes a
  user-supplied path, pasting one back yielded a directory Claude Code never
  created and the command reported "absent" with nothing actually wrong — the
  same silent-zero failure mode as the `.exe` defect above. The prefix is now
  stripped through the crate's existing normalizer before slugging (a no-op on
  Unix, where it cannot occur). Deliberately not case-folded: Windows
  filesystems are case-insensitive but case-*preserving*, and this slug must
  match a directory name Claude Code chose from its own spelling.

### Changed
- **Separator normalization has one implementation crate-wide.**
  `merkle::normalize_rel_str_on` is now the only place `\` becomes `/`;
  `merkle::normalize_rel_path`, the new `merkle::normalize_rel_str` (for input
  that never went through `Path`), and `cli::normalize_path_display_on` (which
  adds the `\\?\` strip) all delegate to it. It takes the platform as a
  parameter instead of an inline `#[cfg(windows)]`, so the Linux and macOS CI
  legs exercise the Windows branch — the property that was missing when the #34
  defects shipped past an existing `windows-latest` job.
- **`affected` text output groups the blast radius by depth** and shows the
  nearest 40, stating how many it withheld and at what depths. A flat
  path-sorted dump buried the depth-1 dependents worth inspecting among hundreds
  of depth-8..10 transitive hits (454 of 3,819 files on the reporting repo).
  The `--depth` default (10) and the `--json` envelope are unchanged, so
  scripted consumers see no difference.

### Documentation
- README: how to install the model by hand via `CODE_GRAPH_MODEL_DIR`, why a
  hand-populated *default* cache dir is rejected (no `.model-id` marker), the
  Windows `tar -C C:\…` trap, and how to diagnose a model that never downloads.
- **New contributor skill `docs/skills/windows-compat/`** — an Agent Skill
  encoding the root causes above so the next path-handling change starts from
  them. Carries a `scripts/audit.sh` scan for the offending patterns, a
  `references/failure-modes.md` catalogue of each shipped defect with its fix,
  and the testability rule the whole batch turns on: take the platform as a
  *parameter*, never an inline `cfg!(windows)`, so the Linux and macOS CI legs
  exercise the Windows branch. `windows-latest` was already in the CI matrix and
  caught none of these — they were pure string logic that nothing asserted on.
  Install with:
  `mkdir -p .claude/skills && cp -r docs/skills/windows-compat .claude/skills/`

## v0.105.0 — macro-call edges + std-import phantom fix (IDX v52), audit-batch hardening

Upgrade notes: first query/index after upgrade triggers a one-time full index
rebuild (INDEX_VERSION 51→52 — two Rust edge-shape changes below). No config
changes required. Full audit trail: `docs/AUDIT-REPORT-2026-07-24.md`
(production-readiness audit, 6 dimensions + same-day disposition log).

### Added
- **Rust macro token-tree call extraction** (`parser/relations/rust.rs`,
  IDX v52a): calls made only inside macro args / `macro_rules!` bodies
  (`assert_eq!(foo(x), y)`, fn-local `sout!` bodies) now emit `calls` edges.
  tree-sitter parses macro interiors as opaque `token_tree`s — no
  `call_expression` exists — so such calls were invisible: targets
  false-flagged dead, impact/callgraph missed the calling fn (field failure:
  `impact grep_exit` missed `cmd_stats`). Heuristic: identifier directly
  followed by a `(…)` token_tree; excludes `.`/`::`/`$`/definition-keyword
  prev-tokens, no-scope top level, and **uppercase-initial names** — tuple
  patterns (`matches!(x, Some(y))`) are token-identical to calls, and
  variant/type names are CamelCase while the fn calls this pass recovers are
  snake_case (audit-reproduced false `calls→Some` edge without the guard).
- **CI formatting gate**: new `fmt` job (`cargo fmt --check`); repo-wide
  one-time `cargo fmt` sweep landed alongside (style-only commit).
- **Release gate: pre-publish version assertion** (`release.yml`): the publish
  job now execs the built linux-x64 artifact and asserts `--version` == tag
  BEFORE any publish step — previously a lagging Cargo.toml at tag time was
  caught only by post-publish smoke, after the packages were public.
- **mcp-launcher tests rejoined CI + release gates**: their exclusion reason
  (dedup test depending on the gitignored dev `.mcp.json`) went stale when the
  test became self-contained; on a bare checkout the binary-forwarding test
  self-skips, and in release.yml (artifact present) it runs for real — the
  stub→binary handover finally has automated coverage.

### Fixed
- **Phantom cross-module import edges from `use std::…`** (`parser/relations/
  rust.rs`, IDX v52b): the bare trailing segment of a std-rooted `use`
  (`use std::fs;` → "fs") entered global bare-name resolution with no
  qualifier metadata and bound to whatever single same-family project symbol
  shared the name — every `use std::fs;` in this repo fabricated an
  `imports → fn fs` edge onto a `#[cfg(test)]` helper, polluting 4
  `module_dependencies` pairs in `map` (one 100% phantom: src/embedding →
  src/indexer/pipeline). Statically-external roots (`std`/`core`/`alloc`/
  `proc_macro`) are now skipped whole; verified gone after rebuild.
- **CI clippy gate was red on committed code**: two `let_and_return` sites in
  `pipeline/resolve.rs` failed the exact CI command (`clippy -- -D warnings`,
  exit 101); inlined. Also cleared the `const_is_empty` warning in
  `effectiveness_bench.rs` so `--all-targets -D warnings` is clean too.
- **Symlinked source files were silently never indexed** (`indexer/merkle.rs`):
  the walkers run `follow_links=false`, so a symlinked file failed the
  `is_file()` guard on the ONLY skip path with no log — monorepo shared-package
  symlinks vanished with zero observability. Now: one aggregate warn per scan
  (count + example) + a behavior-pinning test. Following links (cycle/escape
  protection) is tracked separately (D#15).
- **Near-miss rebase logic deduplicated** (`cli.rs`): the subdir-cwd
  path-doubling fix's two hand-copied arms (`normalize_user_path_from` /
  `cmd_grep`) now share `is_cwd_anchored` + `note_root_rebase` — single source
  for the exclusion list and disclosure wording; added a same-name-collision
  regression test pinning the documented existence-heuristic tradeoff.
- **mtime same-tick blind spot documented** (`scan_directory_cached`): a
  content edit landing within the same filesystem timestamp tick as the prior
  scan is invisible to the cached path (interactive flow covered by
  `ensure_file_indexed`'s full re-hash); now stated at the definition.

## v0.104.1 — staleness-heal gap from npm/dev authority + release gate hardening

Upgrade notes: no action required. Follow-up to v0.104.0's hook-registration
repair: closes a residual case where a hook pinned at an old plugin-cache
version dir could not be healed, and hardens the release pipeline that let
v0.104.0 ship with a red e2e test.

### Fixed
- **Old cache-pinned hooks unhealable from the global-npm/dev authority**
  (`lifecycle.js`): v0.104.0's surface-tolerant staleness compared cache
  versions only when BOTH the present and the desired hook paths carried a
  cache version dir (`pv && dv`). The global-npm CLI and dev checkouts derive
  version-less desired paths, so from those authorities a hook pinned at an
  old plugin-cache version dir was never flagged stale and kept running old
  code. Staleness now falls back to the plugin's own version for the compare;
  a registration NEWER than the running authority still stays (downgrade-war
  guard, §1.11).
- **Release gate actually gates** (`release.yml`, `scripts/githooks/`,
  `package.json`, `bump-version.sh`): `install-e2e.test.js` §1.9 shipped red
  in v0.104.0 because its only gate — the local pre-commit hook — is installed
  by npm `prepare`, which a pure-Rust checkout never runs. (a) release.yml now
  runs the FULL JS suite (incl. install-e2e, fed the built linux-x64 artifact
  via `target/release/`) before any publish step; (b) the pre-commit hook moved
  to a committed `scripts/githooks/` dir activated via `core.hooksPath`, wired
  from both npm `prepare` and `bump-version.sh` so any machine that cuts a
  release has the gate active (covers all worktrees of a clone).

### Added
- **Field-shape regression smoke** (`hook-orphan-dedup.test.js`): one
  settings.json mixing an old cache-version block, bare npm blocks from two
  node versions, and a foreign plugin's hook must converge in a single
  registration pass to exactly one current entry per (event, script), leave
  the foreign hook untouched, and be a no-op on the second pass.

## v0.104.0 — hook-registration dedup + global-shell drift repair

Upgrade notes: no action required. If you switch nvm/node versions, code-graph
now evicts global-npm-delivered hooks stranded under the old node prefix instead
of firing them as stale duplicates, and `doctor` gained a **Global npm relics**
check that surfaces our packages left under a non-active node version. Root-cause
of an audit finding where a global `code-graph-mcp` CLI shim sat two+ versions
behind the native binary (RCA 2026-07-24).

### Fixed
- **Orphan hook accumulation across node/version switches** (`lifecycle.js`):
  `isOurHookEntry` recognized only the marketplace-cache delivery path (dir
  `code-graph-mcp`); hooks delivered via `npm i -g` live under the package name
  `@sdsrs/code-graph` (no `-mcp`) and were never evicted. Every node-version
  switch or reinstall left a stale bare `node "…"` hook firing beside the current
  one — Edit/Read/Bash/prompt hooks ran 2–3×, some executing code dozens of
  versions old. Eviction now recognizes both delivery surfaces.
- **settings.json hook-registration ping-pong** (`lifecycle.js`):
  `surveyHookCoverage` computed staleness by exact command-string compare, so the
  plugin-cache session-init and the global-npm CLI `doctor` — which derive
  different absolute script paths — each rewrote the other's valid, current entry
  on every alternating run. Staleness is now version/surface-tolerant (dead path
  OR an older plugin-cache version dir), and `registerHooksToSettings` is a true
  no-op when a valid current set is already present on either delivery surface.
- **Global-npm shell shim stranded at an old version** (`auto-update.js`): the
  global-package self-heal never ran on the throttle early-return, and the only
  context that can SEE the user's nvm/global prefix (a CLI run under that node)
  short-circuits there — so a global `code-graph-mcp` shim could sit at an old
  version while the native binary self-healed to current. The throttle path now
  attempts the (lock-guarded, targeted `npm i -g pkg@ver`) heal when a cheap
  local check finds a stale global.

### Added
- **`doctor`: "Global npm relics" check** (`doctor.js`, `find-binary.js`,
  `auto-update.js`): enumerates every nvm-managed node version's global prefix
  and reports our packages stranded under a non-active node — invisible to the
  active-node self-heal, yet able to seed stale settings.json hooks. Report-only
  with exact remediation (`nvm use <ver> && npm rm -g <pkg>`); `npm i -g` cannot
  target another node's prefix.

## v0.103.0 — statusline coexistence + uninstall sweep (audit remainder)

Upgrade notes: no action required. New uninstall flag `--unadopt-all` cleans
every adopted project in one pass. If another plugin (or you) takes over the
statusLine slot repeatedly, code-graph now stops re-claiming it after 2
attempts — re-claim explicitly with `node lifecycle.js install` or
`CODE_GRAPH_FORCE_STATUSLINE=1`.

### Added
- **`uninstall --unadopt-all`** (CLI + lifecycle): removes the managed
  CLAUDE.md block + generated detail file from every project in the
  adopted-projects registry, with a per-project result line. `.code-graph/`
  index dirs are project data — listed for manual removal, never auto-deleted.
- **Statusline slot stand-down**: install() tracks how often a foreign command
  displaces our composite from `settings.statusLine` (a peer plugin's
  self-heal, or your own choice). After >2 displacements it stops re-claiming
  — ending both the plugin-vs-plugin ping-pong and the fight against a user's
  manual statusline change.
- **Third-party provider survival**: providers registered through the
  composite registry (e.g. gsd) are no longer orphaned — a genuine uninstall
  hands them the statusLine slot (our runner dies with the plugin cache); a
  temporary disable keeps the composite rendering them.
- **doctor**: warns when the global-package self-heal has exhausted its 3
  attempts (previously silent until the next release re-armed the counter),
  with the exact manual `npm install -g` command.

### Fixed
- **Post-uninstall hook error spam**: settings.json hook commands are
  existence-guarded on POSIX (`if [ -f … ]; then node …; fi`) — the window
  where Claude Code deletes the plugin cache before our teardown strips the
  entries no longer errors on every Edit/Bash/Read/prompt. Node's own exit
  codes (PreToolUse deny = 2) still pass through.
- **Truncated npm platform binaries**: candidates under 1MB are rejected
  (a torn `npm install` could leave a partial file the npm tier accepted and
  cached; the GitHub path already had size/sha/exec gates).
- **Offline poll churn**: the missing-binary stub's 4s upgrade poll — each
  probe a full discovery walk incl. `npm root -g` — now backs off
  exponentially toward 60s; the background install's completion nudge still
  upgrades instantly.
- **compareVersions unified** into version-utils.js and made
  pre-release-aware ("1.2.3-rc1" < "1.2.3"); the two prior divergent copies
  disagreed silently on pre-release ordering.
- **Template drift-refresh notice** now names the overwritten
  `.claude/plugin_code_graph_mcp.md` too, not just the CLAUDE.md block.

## v0.102.0 — install/update/uninstall hardening (full-chain audit)

Upgrade notes: no action required. Uninstall behavior is additive — global npm
packages are removed only when a marker proves the plugin installed them, or
with the new `--purge-global` flag; a user's own `npm install -g` is never
touched. Pin `@sdsrs/code-graph@0.101.0` to stay on the prior behavior.

### Fixed
- **Marketplace installs ran with the version gate disarmed**: `getPackageVersion()`
  only read `../../package.json`, which does not exist in the plugin-cache
  layout — every gate (disk-cache freshness, relic shadowing) silently accepted
  the first candidate for marketplace users, re-opening the stale-binary
  connect-timeout incident that d578d99 fixed for npm installs only. Now falls
  back to `.claude-plugin/plugin.json` (regression-tested in the cache layout).
- **Windows npm was ENOENT-dark**: every bare `spawn('npm', …)` (launcher
  install, global self-heal, `npm root -g` discovery) failed on Windows —
  `npm.cmd` is not spawnable without a shell. All npm calls now route through
  `npm-exec.js` (`shell:true` on win32).
- **musl/Alpine futile 40MB re-download every session**: the download path
  ignored libc, fetched the glibc build, and the exec-based promote check
  rejected it forever while `binaryMissing` bypassed the throttle.
  `getPlatformAssetName()` now returns null under musl and the launcher
  surfaces the cargo-install/glibc-image hint instead.
- **`--version` parser loops**: the fully-anchored regex turned any benign
  output variation (v-prefix, build-metadata suffix, extra line, >2s cold
  exec) into "broken binary" → permanently stale → re-download every session.
  Regex is now tolerant, timeout 2s → 5s; `cachedBinaryNeedsUpdate` /
  `cachedBinaryStaleVsState` use ordered version compare so a newer-than-latest
  (dev) binary is never downgraded to an older release.

### Added
- **Uninstall completeness**: the launcher's background `npm install -g` writes
  `global-install-marker.json`; `lifecycle.js uninstall` (and
  `code-graph-mcp uninstall`) removes the global shell + platform packages when
  the marker proves plugin ownership or `--purge-global` is passed, reports
  anything left, and lists every adopted project (new adopted-projects
  registry maintained by adopt/unadopt). `doctor` gains a global-npm-residue
  check naming who owns cleanup.
- **Post-uninstall cache reclaim**: after `/plugin uninstall`, Claude Code
  stops loading the plugin's hooks.json, so the SessionStart teardown never
  ran and the ~40MB cached binary leaked. `cleanupDisabledStatusline()` — the
  one code path that still fires (composite statusline) — now removes
  `~/.cache/code-graph` on genuine uninstall (never on a temporary disable).
- **Inter-process install lock** (`install-lock.js`, O_EXCL + dead-pid/age
  reclaim): concurrent cold sessions and auto-update no longer run parallel
  `npm install -g` against one global prefix (npm staging is not
  concurrency-safe) or clobber each other's update-state counters.

### Also first shipped in this release (committed post-0.101.0)
- **Stub-first MCP handshake**: a missing binary no longer blocks `initialize`
  behind a synchronous npm(60s)+GitHub(90s) chain (presented as the 30s MCP
  connect timeout) — an upgradeable 0-tool stub answers instantly, the install
  runs in the background, and the live connection hands over to the real
  binary without a restart (f490a58).
- **Version-gated binary discovery**: every discovery tier below the
  auto-update cache now rejects candidates older than the plugin version
  (newest-stale as last resort), and the `NPM_CONFIG_PREFIX` env prefix
  outranks the execPath-derived global root (d578d99).
- **Global npm self-heal**: stale globally-installed `@sdsrs/code-graph` CLI
  shims and platform-package relics are refreshed with a targeted
  `npm install -g pkg@version` (immune to unrelated `npm update -g` failures
  like EALLOWGIT), bounded to 3 attempts per release (e2d9042).
- **Statusline**: phantom "indexing" display unstuck (progress-file liveness by
  mtime heartbeat, finalizing heartbeats, nested-timeout budget) (3803e28).

## v0.101.0 — instrumentation fixes + bounded pending retention (roadmap Phase 3)

### Added
- **Release build-cache warming** (roadmap §3.5, D#73): new `cache-warm.yml`
  workflow builds the exact 5-target release matrix on main (Mon+Thu schedule,
  manual dispatch, and on Cargo.lock pushes). Actions caches are ref-scoped —
  a cache saved by one tag's run is invisible to the next tag — and no
  main-branch run ever used release.yml's cache key, so every release
  recompiled ~397 deps cold on all 5 platforms (4–9 min each, 21 min worst
  observed). The warm main-scoped cache is readable from any tag ref.

### Changed
- **Global settings self-heal now runs in non-project cwds too** (roadmap
  §3.4): `session-init` used to return at the non-project gate before
  `syncLifecycleConfig`, so a lost/stale hook registration in the user-global
  settings.json never healed while sessions started in marker-less cwds
  (e.g. headless `claude -p` fleets in /tmp) — the structural residue of the
  weeks-dark bash-guard incident. The heal (a few idempotent JSON reads,
  claudeHome-only writes) now precedes the gate; project footprint in
  non-project cwds stays zero (no index, no adoption, no map injection).
- **`pending_unresolved_calls` is now bounded** (roadmap §3.2, D#77;
  SCHEMA_VERSION 9 → 10): each resolution sweep ages surviving rows by one
  `attempts`; rows failing 50 consecutive sweeps are evicted. ~99% of buffered
  rows are never-resolvable external/builtin calls (require/Some/Ok/…) that
  previously accumulated until the next INDEX_VERSION wipe (2909 rows on this
  repo). The incremental-edge-timing guarantee is preserved below the
  threshold — a row one sweep from eviction still resolves when its callee
  arrives (resolution drains before aging), and a caller-file re-parse resets
  the clock via cascade + re-buffer (verified live on a real binary).
  Upgraded DBs migrate in place (guarded ALTER + attempts backfilled to 0,
  verified against a real v9 index copy); after upgrading, older binaries
  refuse the migrated DB with the standard schema-too-new marker until they
  update ("↻ updating" statusline window).

### Fixed
- **`outcome` under-counted batched cg calls** (roadmap §3.1) — two cg calls
  issued in ONE assistant turn gave the first call a zero-width adoption
  window (the forward scan broke at the very next `CgCall`), so its result
  could never earn credit. Events now carry the transcript-line turn id;
  batch-mates share the forward window, which still ends at the first call
  from a later turn. Current 30-day windows across 3 projects show no delta
  (the batched pattern hadn't occurred), but the class is closed.
- **`outcome` read callgraph/impact-style CLI calls as returning zero files**
  (roadmap §3.3) — their human output prints `symbol (path)` with no
  `path:line` token, so `returned_files` was always empty and adoption was
  structurally impossible (the callgraph_cli 0/7 reading was this artifact).
  Extraction now falls back to parenthesized-path tokens when a stdout has no
  `path:line` hits at all; outputs with real `path:line` hits are unaffected.

### Added
- **`outcome` adoption-window calibration** (roadmap §3.1): each adoption
  records its distance (Nth file-touch after the call); human output and
  `--json` gain an `adoption_distance_histogram`. Real-data read (59
  adoptions, 3 projects, 30d): 78% adopt on the very next touch, 98% within
  6 — the unbounded until-next-call window is insensitive; no cap needed.

## v0.100.2 — Windows worktree-fallback hotfix

### Fixed
- **Worktree read-side fallback was dead on Windows** — git writes `gitdir` with
  forward slashes even on Windows; the marker search used the native separator
  and never matched, so v0.100.1's main-checkout fallback silently no-opped
  there (caught by CI windows-latest). Marker search is now separator-agnostic;
  returned paths keep native separators. Linux/macOS behavior unchanged.

## v0.100.1 — worktree read-side + MCP centrality (roadmap Phase 2b/2c)

### Added
- **MCP surface for centrality** (roadmap §2.4): `project_map` gains
  `include_centrality` (+ `centrality_limit`, default 10) attaching the
  CLI-only betweenness-centrality chokepoint ranking as a `centrality`
  array; compact mode forwards it trimmed (name + file + score). Computed
  per call outside the 60s project-map cache; test callers excluded like
  the CLI default. routing_bench unchanged (66 passed) after the tool-
  description update.

### Fixed
- **Query commands inside a linked git worktree read the main checkout's
  index** (roadmap §2.2, Rust read-side of the v0.99.0 JS fix) — every
  CLI reader behind `CliContext` (search/show/callgraph/grep/…) used to
  error "No index found" (or cold-build a duplicate index) in a Claude Code
  `.claude/worktrees/<slug>` checkout. A worktree with no own index now
  falls back to the main checkout's index (own index still wins; submodule
  `.git` pointers remain a hard boundary; write side — index/serve/rebuild —
  still builds locally). Paths and line numbers in answers are the main
  checkout's, same contract as the JS hooks/statusline.

## v0.100.0 — axum routes + namespace/star-barrel edges (roadmap Phase 2a)

### Added — INDEX_VERSION 51 (existing indexes auto-rebuild once on upgrade)

- **Rust axum route extraction** (roadmap §2.1 — `trace` was blind on Rust, this
  product's own language): `.route(path, get(h).post(h2))` builder chains emit
  one `routes_to` edge per (method, handler); inline `.nest("/prefix", …)`
  composes path prefixes (ancestor walk); handlers resolve by name incl.
  cross-file (`use handlers::list_users`) via the existing routes_to recovery;
  `axum::routing::get` scoped forms and `any` (→ ALL) supported. Scoped
  strictly to `.route` links — bare `.get()` (reqwest/HashMap) cannot
  fabricate routes. Non-goals (documented): closures as handlers, nest of a
  router variable built elsewhere (needs dataflow), actix/rocket/Spring —
  the `trace` empty-result hint names the coverage.
- **ESM namespace imports + star barrels form real edges** (roadmap §2.3,
  closes the v0.92.0 known limitation): `import * as ns from './m'` and
  `export * from './m'` (incl. `export * as ns`) now bind a module-level
  `imports` edge to the resolved file's `<module>` node (the PHP-/C-include
  pattern), so namespace-only and star-barrel dependencies are visible to
  `deps`/`affected`/`cycles`/`map`; CJS `const m = require('./m')` markers get
  the same module-level edge (previously skipped entirely). The ESM namespace
  alias also feeds the member-call binder, so `ns.fmt()` resolves cross-file
  exactly like the CJS require-namespace path. Residual (documented):
  name-level resolution *through* a star barrel still uses the name-based
  fallback — star-chain following is a future enhancement.

## v0.99.1 — disclosure batch (roadmap Phase 1)

### Fixed — disclosure batch (roadmap 2026-07-18 §1: honest info must reach the consumer)

All CLI query commands whose discriminating information lived only on stderr (or
nowhere) now put it in-band on stdout/JSON, where an LLM consumer running with
`2>/dev/null` actually reads. JSON shape notes below; every change is on an
empty/miss/truncated path — populated success outputs are unchanged.

- **`search` / `ast-search`: filter-emptied results are self-describing** — when
  the query HAD hits but `--language`/`--node-type`/`--returns`/`--params`
  removed them all, JSON was a bare `[]` (or `{results:[],count:0}`),
  byte-identical to a true zero-hit. Now an object:
  `{"results":[],"filtered_out":N,"filter":"language: python"}` (search keeps
  `query`; ast-search keeps `count`). Text mode prints the same line on stdout.
  True zero-hits keep the old shapes.
- **`dead-code`: hidden-candidate empties are self-describing** — empty because
  `--ignore` suppressed candidates → `{"results":[],"ignored_count":N}`; empty
  because all orphans sit below `--min-lines` → `{"results":[],
  "below_threshold_count":N,"min_lines":M}`. True clean keeps `[]`. Text mode
  puts the rerun hint on stdout.
- **`show` / `overview` / `callgraph`: misses carry an in-band error object**
  (exit codes unchanged, still 1) — `show` emits `{"error":"Symbol not found",
  "symbol":…,"candidates":[…]}` with the fuzzy "Did you mean" list in-band
  (was stderr-only); `show --node-id` emits `{"error":"Node ID not found",
  "node_id":…}`; `overview` emits `{"error":"No symbols found","path":…}`
  (was `[]`); `callgraph` adds `error`+`symbol` to its `{"results":[]}` object.
  Matches the `impact`/`refs`/`trace` in-band miss contract.
- **Partial freshness resync is disclosed in JSON** — the "N file(s) changed
  since indexing; line numbers may be stale" note was stderr-only. Object-shaped
  outputs (`ast-search`, `impact`, `trace`, `refs`) now carry
  `"freshness_partial":true` when it applies. Array-shaped outputs
  (`search`/`show`/`overview`/`similar`/`dead-code`) cannot carry a top-level
  field without breaking their success shape — stderr remains their channel.
- **`cycles`: truncation is disclosed** — `--limit` used to shrink the printed
  "(N found)" to the truncated length with no marker. Text now prints
  "(showing L of N found)"; JSON becomes `{"results":[…],"total_found":N,
  "truncated":true}` when (and only when) truncated — the untruncated array
  shape is unchanged.
- **`map`: silent list caps get "+N more" markers** — dependencies (top-30
  non-compact / top-10 compact) and compact hot-functions (top-5) now say
  "... and N more", matching the modules cap.
- **post-grep-inject records its non-delivering path** — the PostToolUse
  compound-grep hook recorded nothing when cg had no answer, so telemetry could
  not distinguish "hook dark (binary missing)" from "ran, genuinely nothing".
  Skips now log `answered:false` + `fallthrough`/`reason` (status), surfaced as
  `inject_skipped` in `stats --json`; they do not arm the conversion funnel.

### Fixed
- **Compound-command denies now flag the dropped tail on the FIRST line** — the
  pre-grep hook's re-issue NOTE (`the rest of this compound command did NOT run`)
  sits at the end of a long deny message, which Claude Code's transcript view folds;
  a human reading the truncated view saw a clean "answered" deny and misread the
  hook as broken. All three deny builders (answered / show / static) now append
  `(compound tail NOT run — see NOTE at end)` to their head line. Model-visible
  content is otherwise unchanged.

## v0.99.0 — worktree-aware root resolution + grep zero-hit disclosure

Behavior change (plugin JS resolver, `project-root.js` — shared by the statusline and
every hook gate): sessions inside a **linked git worktree** (e.g. Claude Code's
`.claude/worktrees/<slug>` branch checkouts) now resolve to the **main checkout's
index** instead of going dark. Numbers/answers reflect the main checkout's content;
a worktree that builds its own index (run any `code-graph-mcp` command in it) takes
precedence over the fallback. Revert path: pin the previous plugin version.

### Fixed — grep zero-hit disclosure
- **`grep -c` zero matches on a named file printed nothing on stdout** — GNU grep
  prints a `0` count per named file and exits 1; ours emitted a stderr-only note, so
  `grep "pat" file.py -c 2>/dev/null` was total silence (field failure). Named FILE
  args now get a `path:0` row (also filled for non-matching named files when other
  files match, per GNU); the `--json` shape gains the same `{file, count: 0}` rows.
  Deliberate GNU deviation kept: dir/repo-wide args do not enumerate zero rows
  (GNU `-rc` prints every scanned file — repo-scale noise).
- **BRE-style escape zero-hits now disclose the regex dialect** — `\|` is alternation
  in GNU BRE but a LITERAL pipe in ripgrep's Rust regex, so grep-habit patterns like
  `protocol\|proto` silently zero-hit and an LLM consumer concludes "no such code".
  The no-match path (all modes) now appends a one-line hint naming the escapes found;
  suppressed under `-F` where backslashes are genuinely literal.

### Fixed
- **Statusline + all hooks were dark in linked worktrees** — the worktree's `.git`
  FILE hit the hard submodule boundary (`hasGit → no index → null`). Worktree
  `gitdir: …/.git/worktrees/<name>` now resolves to the main checkout; submodule
  `gitdir: …/.git/modules/…` remains a hard boundary (different codebase, not a
  branch copy).
- **Worktree root vs subdir contradiction** — subdirs of a worktree escaped through
  the `.git` boundary to the main index while the worktree root showed nothing.
  Both now resolve identically (main checkout, or the worktree's own index if built).
- **Stray `~/.code-graph` leaked into every un-indexed dir under `$HOME`** — the
  ancestor walk honored an index at home itself ("checked, not crossed"), so an
  accidental home-dir index made unrelated directories show its statusline and
  activate hooks. Home is now a pure stop; resolving from home itself still honors
  a deliberate home index (own-index rule).
- **Subdirs of an un-indexed nested repo escaped to the outer project's index** —
  the ancestor walk now stops at `.git` boundaries, matching the root's own
  no-escape rule. The legit inverse (only `repo/packages/foo` indexed inside an
  un-indexed repo, resolving from below it) keeps working and is pinned by a test.
- **SessionStart + UserPromptSubmit gates used the bare session cwd** — sessions
  launched in a worktree or subdir skipped index-freshness, the project-map/recent-
  impact injections, and prompt hints entirely (sibling of the v0.48 pre-*-guide
  subdir-cwd dark class). Both now resolve the canonical root; recent-impact keeps
  git WIP detection in the session dir (the worktree's branch state) while querying
  the canonical index.

## v0.98.1 — grep partial results on path errors

### Fixed
- **`grep` no longer discards partial results when ripgrep exits 2** — a multi-path
  grep with one missing/unreadable path (`grep "pat" scripts parse -c` where `parse`
  doesn't exist) printed nothing and exited 2, silently eating the matches rg had
  already produced from the readable paths. GNU-grep parity now: matches from readable
  paths print in all three output modes (default/-l/-c), the path error is surfaced on
  stderr, and the exit code stays 2. Edge covered: `rg --json` emits a summary line
  even with zero matches, so an error-only run (single bad path) is classified as
  error (exit 2), not no-match (exit 1).

## v0.98.0 — audit-v0971 fix batch (INDEX_VERSION 49→50)

Fixes every actionable finding from the 2026-07-17 full audit (docs/AUDIT-2026-07-17.md, local).

### Fixed — extraction (INDEX_VERSION 49→50; old indexes rebuild automatically)
- **Constructor instantiations now produce `calls` edges for JS/TS/TSX (`new_expression`),
  C# and PHP (`object_creation_expression`)** — previously these five languages emitted
  ZERO edges for `new Foo()`, so a class only ever instantiated had no callers in
  callgraph/impact and was a dead-code false positive. Sibling enumeration verified the
  other languages were already covered (Java via type-reference `references`; Kotlin/
  Swift/Python/Ruby constructor calls are plain call expressions; Rust struct
  expressions; Go composite literals) and pinned Java's coverage with a matrix guard.
- **C# local functions (`local_function_statement`) are now extracted as symbols** — the
  default top-level `Program.cs` shape since .NET 6 declares functions this way, so the
  v49 `<module>` caller edge finally has a resolvable target instead of dangling.
- **Signature fields strip NUL bytes** (`return_type`/`param_types`/`signature`, feeding
  `context_string`) — an embedded NUL made SQLite `LIKE` filters (`ast-search
  returns=`/`params=`, fuzzy-name) silently miss everything after it (FTS was unaffected;
  sibling of the v48 `code_content` / v49 `doc_comment` strips).
- **The two residual `format!`-built edge-metadata sites (rtype, impl_method) now use
  `serde_json`** — byte-identical for today's identifier-only inputs, hostile-name safe
  (matches the v49 `serialize_callee_qualifier` migration).

### Fixed — MCP compact contract
- **`module_overview compact:true` no longer silently drops `dependencies`,
  `dependencies_unavailable`, and `dead_code_unavailable`** — an agent requesting a
  compact overview *with* deps got a complete-looking result that had lost the data it
  asked for, with no disclosure. A source-scanning parity guard now asserts every
  top-level key the tool sets is either forwarded by compact mode or explicitly listed
  as deliberately compacted (third recurrence of this allowlist bug class).

### Added — observability & guards
- **Tree-sitter parse errors are now visible**: files whose syntax tree contains ERROR
  recovery nodes (which silently drop all symbols after the error point) log a warning
  and the index summary reports `N file(s) parsed with syntax errors (symbols may be
  incomplete)` — previously this entire failure class had zero signal.
- **CLI↔MCP freshness parity drift-guard** (`tests/freshness_parity.rs`): all nine
  line-number-emitting CLI commands must call the shared resync path and all five
  file-path MCP tools must call `ensure_file_fresh_opt` — with permanent negative
  controls proving the guards fire on an omission.
- **Five missing `--json` empty-contract tests** (impact / ast-search / centrality /
  map / affected) — the commands already emitted same-shape JSON on empty input; the
  regression guards now exist like the other fifteen.
- **`sync-versions.js --check`**: read-only drift detection across all nine version
  sites (exit 1 on drift); write mode unchanged.

### Fixed — CLI hardening
- `git ls-files` / `rg --files` supplement spawns now pass `--` before path arguments
  (a `-`-prefixed path can no longer be misparsed as a flag).

### CI
- SHA-pinned the eight remaining floating third-party action refs in ci.yml /
  pr-impact-review.yml (release.yml was already pinned) and added a `concurrency`
  group to release.yml so a tag-push run and a `workflow_dispatch` re-run on the same
  tag serialize instead of racing.

### Docs
- README known-limitations: Kotlin/Swift interface conformance is recorded as
  `inherits` (single `: Type` grammar) so `implements`-filtered queries return empty
  for those two languages; cross-file dead-code may false-positive a type whose only
  cross-file reference sits beyond the 4096-byte stored-content cap (accepted, v0.97.1).

## v0.97.1 — fix dead-code over-suppression regression

### Fixed — cross-file dead-code detection restored
- **v0.97.0 silently disabled cross-file dead-code detection for edgeless kinds**
  (constant/struct/enum/type_alias/interface/trait). The truncation keep-bias added to
  the cross-file probe (`src/storage/queries/dead_code.rs`) was name-independent: a
  single truncated node anywhere in the project (and `code_content` caps at 4096 bytes,
  so every real repo has one) satisfied the `NOT EXISTS` subquery for **every**
  candidate, so nothing was ever reported dead cross-file. Caught by a post-release
  code review (the v0.97.0 negative-control test had no truncated node, so it never
  exercised the over-suppression path). The cross-file truncation term is removed; the
  **same-file** probe keeps its co-signal (there the truncated node shares the
  candidate's file — high correlation, one-file blast radius, no over-suppression).
  Regression test `test_find_dead_code_cross_file_unrelated_truncated_node_does_not_suppress`.
  This reopens the original (narrow) audit finding — a struct whose *sole* cross-file use
  sits past the 4096-byte cap of an importing file may be reported dead — as an accepted
  documented limitation, far rarer than the feature-nullifying false-negative it caused.

## v0.97.0 — audit-v0961 remediation batch (INDEX_VERSION 48→49)

Fixes every actionable finding from the 2026-07-16 full audit (docs/AUDIT-2026-07-16.md, local).
Also supersedes the unpublished v0.96.1 tag (its GitHub Release/npm publish was lost to a
GitHub API outage; the `show` freshness fix ships here).

### Fixed — extraction (INDEX_VERSION 48→49; old indexes rebuild automatically)
- **C# top-level statement calls were silently dropped** — a method invoked only from a
  top-level statement (the default `Program.cs` shape since .NET 6) had zero incoming
  `calls` edges: missing callers in callgraph/impact and a dead-code false-positive
  candidate. The `invocation_expression` arm now attributes such calls to `<module>`,
  like the Python/Ruby/PHP/bash/JS siblings. Kotlin, Swift, and Dart got the same
  fallback for library-level top-level calls (Rust/Go/Java/C remain intentionally
  excluded — negative control pinned).
- **Dart `mixin M {}` declarations produced no symbol node**, so `class D … with M`
  inheritance edges were extracted but dropped at resolution (the v32 fix was
  edge-only). `mixin_declaration` is now extracted as a class-kind node; `with`
  inheritance survives end to end.
- **Call-qualifier metadata is now real JSON** (`serde_json::json!` instead of a raw
  `format!`) — byte-identical for today's identifier-only inputs, but a future
  extractor feeding quotes/backslashes can no longer produce malformed rows that
  would abort the confidence-classification UPDATE.
- **`doc_comment` NUL bytes are stripped** (NUL→space) at the single assembly point —
  an embedded NUL made FTS silently unable to search the rest of that comment
  (the `code_content` path was already fixed in v48).
- **Pending-call sweep: `Self::`/`self.`/path-qualified calls with an empty type filter
  now bind nothing and drain** (Phase-2 parity). The previous bare-set fallback — the
  exact false-sibling shape H1 fixed — was latent-only (unreachable from production
  buffering) but documented as parity while behaving as its opposite.

### Fixed — CLI freshness parity (sibling sweep of the v0.96.1 `show` fix)
- **`refs`/`overview`/`search`/`ast-search`/`trace`/`similar`/`impact`/`dead-code` no
  longer print stale line numbers after post-index edits.** All eight now run through a
  shared resync orchestration (hash-compare displayed files, reindex dirty ones, re-run
  the query once) with the same bounds as `show`: 8-file budget, 250 ms busy-timeout,
  keep-stale on contention. The MCP surface already refreshed; the CLI surface — the one
  the steering recommends — was the stale one.
- **Partial freshness is now disclosed**: when the budget is exhausted or a resync fails,
  a stderr note says how many files may still be stale instead of silently mixing fresh
  and stale line numbers (`show` inherits this).

### Fixed — server robustness
- **The per-request panic defense is now real in release builds.** `[profile.release]`
  set `panic = "abort"`, which made the serve loop's `catch_unwind` (turn a handler
  panic into JSON-RPC `-32603`, keep the session alive) unreachable in the shipped
  binary while dev-profile tests kept it green. The abort setting is removed (default
  unwind) and a drift-guard test pins the profile. `run_startup_tasks()` — the most
  panic-prone per-iteration code — is now wrapped in the same guard.
- **Oversized-line drain is fully bounded**: a single line larger than 2× the 10 MiB
  message cap used to leave a tail that was misparsed as the next message (one spurious
  error response). The drain now loops until the terminating newline or EOF.

### Fixed — storage correctness
- **LIKE patterns now escape the backslash itself** (then `%`/`_`) via a shared
  `escape_like()` used by all 9 sites — under `ESCAPE '\'`, a literal `\` in a query
  acted as an escape character (`a\b` matched `ab`; a trailing `\` matched nothing).
- **Dead-code cross-file reference probe honors content truncation**: a symbol whose
  only cross-file reference sits beyond another node's 4096-byte `code_content` cap is
  no longer reported dead (same keep-bias co-signal the same-file probe already had).

### Docs / tooling
- README language table corrected (19 languages incl. Bash/Markdown/JSON; HTML/CSS
  consistently "file-FTS only"; HTTP-route claim scoped to TS/JS + Go + Python).
- Snapshot trust controls (`CODE_GRAPH_SNAPSHOT_TRUST_URL` / `TRUST_ORIGIN` / `PIN`) and
  offline model controls (`CODE_GRAPH_MODEL_DIR`, `CODE_GRAPH_DISABLE_MODEL_DOWNLOAD`)
  are now documented in the README; the stale pre-M11 "fail-open" doc comment on
  `verify_checksum_impl` now describes the actual fail-closed behavior; all `unsafe`
  blocks carry `// SAFETY:` comments.
- pre-commit version guard now also checks Cargo.lock's own `code-graph-mcp` entry
  (a `SYNC_VERSIONS_SKIP_BUILD=1` bump could leave it stale).

## v0.96.1 — `show` reports post-edit line numbers

### Fixed — query-time freshness for the `show` command
- **`code-graph-mcp show <symbol>` reported pre-edit line numbers after a file was edited
  post-index.** `show` read `start_line`/`end_line` (and the stored body) straight from the
  index with no freshness check, so a file edited since the last index made it print stale
  boundaries — landing a follow-up `sed`/read off by the number of lines inserted or deleted
  above the symbol. Its siblings already refreshed: the MCP `get_ast_node` tool via
  `ensure_file_fresh_opt`, and CLI `grep` via its own lazy resync (v0.18.0) — `show` was the
  gap. It now hash-compares each file the symbol resolves into, re-indexes the dirty ones via
  `ensure_file_indexed`, and re-resolves before printing. Bounded (8-file budget, 250ms
  busy-timeout); on write contention or a parse failure it keeps the stale-but-present node —
  never worse than before. With `--file`, the named file is refreshed even when the symbol
  does not resolve yet, so a symbol ADDED after the last index is picked up too. Symbol
  resolution was factored into `resolve_show_nodes` to re-run cleanly after the resync.
  Regression test: `test_cli_show_resyncs_after_edit`.

## v0.96.0 — grep-guard answers the file you actually grepped

### Fixed — compound-command clause scoping in the grep guard
- **The `grep` PreToolUse guard could deny a grep and hand back an AST answer for a
  *different* file than the one grepped.** When a grep's own path argument was not on the
  source-prefix allowlist but a later, non-grep part of the same compound command named an
  allowlisted path, the hook scanned the whole command string: it fired on the tail's path
  and scoped its "already ran for you" answer to that tail file. Real miss (2026-07-13):
  `grep -n "VERSION" skills/moa/scripts/moa.py | head; …; python3 … scripts/bump-version.sh`
  denied with a `code-graph-mcp grep "VERSION" scripts/bump-version.sh` answer — a file the
  user never searched. Classification now scopes to the grep's **own clause** (up to the
  first top-level control operator — `;`/`|`/`&&`/`||`/newline, quote-aware) via a shared
  `firstShellClause` helper: `shouldHint`'s source-path gate, `extractSearchPath`,
  `extractPatterns`, and `classifyBlock`'s flag checks all stop at the separator, matching
  the scope guard `countNamedPaths` already had (v0.70). A source path — or a
  `-v`/`--exclude` flag — in a non-grep tail no longer makes the hook fire or mis-scope its
  answer. Redirects (`>` `<`, incl. `2>&1` and process substitution `-f <(cat pats) src/`)
  are **not** treated as boundaries — grep path args legitimately follow them — so a
  `grep -rn "X" 2>&1 src/` still fires and scopes to `src/`.
- **Quote-parser family unified.** `firstShellClause`, `countNamedPaths`, and
  `extractUnansweredTail` now all honor the POSIX rule that inside double quotes `\"` does
  not close the string, so a `grep "a\";b" src/foo.rs; sed …` no longer garbles the
  "re-issue the tail" NOTE by splitting inside the pattern.
- **`skills/` is now a recognized source-prefix.** Claude Code plugin / agent monorepos
  keep source under `skills/<name>/…`; a grep there was previously invisible to the guard,
  so it could never be scoped to the real target.

### Changed — steering-doc / CLI-help accuracy (LLM-visible metadata)
- **The detail-doc CLI cheatsheet now spells out each enum flag's full valid-value set**,
  so a refinement flag no longer has to be probed by tripping a `--X must be one of: …`
  error first: `--relation ∈ calls|imports|inherits|implements|references|all`,
  `--min-confidence ∈ extracted|inferred|ambiguous` (shared by callgraph/impact/trace), and
  `impact --change-type ∈ signature|behavior|remove` (default `behavior`). Values are
  guarded against drift by `tests/doc_cli_alignment.rs`.
- **`adopt` / `unadopt` `--help` (and the top-level command list) no longer claim they write
  a `MEMORY.md` sentinel.** Since v0.74 adopt installs a managed block into the project
  `CLAUDE.md` plus the `.claude/plugin_code_graph_mcp.md` detail doc (the memory-dir path was
  removed); the Rust CLI help text still described the old MEMORY.md behavior. Corrected to
  describe the CLAUDE.md managed block + detail doc.

## v0.95.1 — MCP rebuild_index atomicity (P2 L6)

### Fixed — MCP rebuild atomicity (P2 L6)
- **`rebuild_index` (MCP tool) is now atomic and failure-safe.** It cleared the live
  index with a committed `DELETE FROM files` and then rebuilt in place, so for the whole
  (multi-second) rebuild an external fresh-connection reader — a CLI `grep`, a secondary
  MCP instance — saw an empty/partial index, and a rebuild that failed after the DELETE
  left no index at all. The DELETE and the full re-index now run inside one outer
  transaction: WAL readers keep seeing the old, complete index until it commits, and any
  failure rolls the DELETE back too, leaving the old index intact. (Unlike the CLI's
  temp-file + atomic-rename swap — the server holds a persistent connection whose fd would
  keep pointing at the old unlinked inode after a rename, so atomicity comes from one
  transaction on the live connection instead.) The indexer's phase transactions became
  nestable `SAVEPOINT`s (`Database::savepoint`) so they compose inside the outer
  transaction; behavior is unchanged when the pipeline runs standalone (CLI / incremental).
  `INDEX_VERSION`/`SCHEMA_VERSION` unchanged.

## v0.95.0 — architecture lock-in (M8/M9a + drift-guards) & robustness sweep (P2)

Continues the v0.93.1 audit remediation (`docs/OPTIMIZATION-ROADMAP.md`): the ARCH
"lock-priority" milestone — dual-surface de-duplication, a layer-decoupling, and six
parallel-path drift-guard tests — followed by nine low-risk P2 robustness fixes.
Every change ships with a regression test that fails on the pre-fix code.
**`INDEX_VERSION` 46 → 48** — existing indexes auto-rebuild on first open (call-edge
pruning and `code_content` sanitization changed). `SCHEMA_VERSION` unchanged (9).

### Architecture (behavior-preserving, byte-identical output)
- **Dead-code analysis: a single `dead_code_report` drives both surfaces.** The CLI
  (`cmd_dead_code`) and MCP (`tool_find_dead_code`) each carried a full copy of the
  validate → find → ignore-filter → hidden-threshold-probe → classify orchestration,
  already drifting in wording. Both now format the output of one shared report.
- **Broke the storage→graph dependency cycle.** `get_callers_with_route_info` (the
  repo's only reverse edge) moved up into `graph/routes.rs`, composing the graph
  traversal with a pure-SQL `fetch_route_metadata_map` that stays in storage.
- **Six parallel-path drift-guard tests.** One consistency test + a proven-RED
  negative control per "sibling-hole" class (qualifier full-vs-incremental parity,
  import extraction across languages, no bare `os.tmpdir()`, Rust↔JS project-root
  parity, deps symlink-escape confinement, CLI↔MCP dead-code single-source).

### Fixed — graph correctness (indexes rebuild)
- **Call edges from large callers are no longer false-pruned.** The import-contradiction
  prune trusts an `instr(code_content, '.name(')` guard, but `code_content` is capped
  at 4096 bytes with a `...` sentinel, so a qualified call beyond the cap was sliced
  off → the guard misfired → a real edge dropped. Truncated callers now keep the edge.
- **Source NUL bytes no longer truncate FTS.** FTS5 tokenizes stored TEXT as a
  C-string and stops at the first NUL, so a file with an embedded NUL (mis-detected
  binary / generated blob) left everything after it unsearchable. Extracted
  `code_content` now replaces NUL with a space; normal source is byte-identical.

### Fixed — search & determinism
- **RRF ranking is deterministic at exact score ties.** Fused scores were collected
  from a per-instance-seeded `HashMap` then stable-sorted by score, so tied results —
  and the `truncate(top_k)` cut straddling a tie — varied run to run. Ties now break
  by ascending `node_id`.
- **Fuzzy-name ordering escapes SQL wildcards.** The `find_functions_by_fuzzy_name`
  ORDER BY prefix bucket used the raw query, so a `%`/`_` in the input mis-bucketed
  names; it now uses the escaped form with `ESCAPE`. Result set unchanged.
- **Corrected the vector-similarity label.** The score is `1 − L2_distance`
  (`node_vectors` is a plain vec0 table), not cosine; comments and the debug probe
  field were relabeled.

### Fixed — robustness
- **The MCP stdio session survives a poisoned stdout lock.** A background
  notification write that panics while holding the shared stdout mutex poisoned it;
  the main loop's writes (outside the per-request `catch_unwind`) then panicked and
  tore down the long-lived session. Locks now recover the poisoned guard.
- **auto-update honors `HTTP(S)_PROXY`.** The release-metadata fetch used a bare
  `https.request` that ignores `*_PROXY` (binary downloads via curl already honored
  it); it now tunnels over an HTTP `CONNECT` when a proxy applies (with `NO_PROXY`).
- **CLI `rebuild-index`/`incremental-index` warn on a lock-holding server.** A running
  MCP server holds `index.lock` for its lifetime; the CLI now detects this and warns
  before racing its writes (best-effort, non-blocking, silenced by `--quiet`).

### Build / packaging
- **npm tarball excludes `*.test.js`.** 66 → 38 files, 777 KB → 384 KB unpacked; no
  runtime surface change.

## v0.94.0 — audit-driven correctness & supply-chain hardening (P0 + P1)

Thirteen fixes from the v0.93.1 architecture audit (`docs/AUDIT-2026-07-13.md`),
tracked in `docs/OPTIMIZATION-ROADMAP.md`. Every fix ships with a regression test
that fails on the pre-fix code. **`INDEX_VERSION` 41 → 46** — existing indexes
auto-rebuild on first open (parser/resolution output changed). `SCHEMA_VERSION`
unchanged (9).

### Fixed — graph correctness (indexes rebuild)
- **Incremental indexing no longer silently corrupts the call graph.** The
  pending-call sweep (`resolve_pending_calls`, run on watcher / branch-switch /
  incremental passes) resolved buffered calls by bare name only, ignoring the
  callee qualifier Phase 2 applies. A Python `w.write()` whose receiver type was
  inferred as `DataWriter` bound to *every* same-language `write` — false callers
  that persisted until `rebuild-index`. The sweep now applies the same qualifier
  filtering (additive: precise when the qualifier matches, bare fallback otherwise).
- **Qualifier-resolved call edges stay visible.** `classify_edge_confidence`
  downgraded `self.method()` / `Alpha::foo()` calls to `ambiguous` (hidden by the
  default confidence floor) whenever the bare name was duplicated, even though the
  edge was precisely resolved by a type/path qualifier. They now keep `inferred`.
- **Java `import` statements are extracted.** `import_declaration` was matched only
  for Swift, so every Java project had zero import edges, no `<external>` nodes, and
  imported classes false-flagged as dead-code — contradicting the documented Java
  Full tier. Now emits `imports` to the imported type (wildcard imports skipped).
- **PHP top-level calls attribute to `<module>`.** A bare `greetPhp();` at script
  top level was dropped (the arm required an enclosing scope), so the callee looked
  dead. Matches the python/ruby/bash `<module>` fallback.
- **C/C++ `#include "own.h"` resolves to the indexed header.** The include emitted
  only a bare stem, so it fell to `<external>` and local header dependencies were
  invisible to deps/cycles/affected. Now binds to the header file's `<module>`,
  mirroring PHP/JS require resolution.

### Fixed — robustness, ranking & CLI
- **A 10 MB+ multi-byte (CJK) MCP request no longer kills the session.** The serve
  read loop's `read_line` UTF-8-validated a `take`-truncated buffer and propagated
  the error outside the per-request `catch_unwind`, tearing down the whole stdio
  session — a single-request DoS. Now reads raw bytes + lossy-decodes.
- **RRF fusion no longer flips adjacent ranks on negative vector scores.** sqlite-vec
  L2 distance yields scores in `[-1, 1]`; a negative score produced an unbounded
  blend term. The numerator is now clamped (non-negative scores unchanged).
- **`deps` refuses symlinks that escape the project root.** The barrel-pattern
  scanner followed a symlink out of the repo with no confinement — a restricted
  file-read oracle. Now canonicalizes + confines like `read_source_context`.
- **Project-root resolution matches the JS hooks.** When cwd sat below a stray
  nested `.code-graph` under the real indexed `.git` root, the CLI resolved to the
  stray index while the hooks used the root — a split-brain double-DB. The CLI now
  prefers the indexed `.git` root.

### Fixed — supply chain, storage & dev/CI
- **Snapshot install fails closed with no integrity sidecar.** With no
  `CODE_GRAPH_SNAPSHOT_PIN`, a snapshot whose `.blake3` sidecar 404'd / was blocked /
  downgraded was installed *unverified*. It is now refused (escape hatches: a pin, or
  the publisher serving the sidecar).
- **A pre-v3 database no longer bricks after a crash mid-migration.** The v2→v3
  migration's `INSERT ... SELECT *` failed with "5 columns but 6 values" when a crash
  left `user_version` behind a schema that already had `edges.confidence`, with no
  self-heal. It now names the 5 columns explicitly and converges on re-run.
- **pre-commit JS tests no longer leak `GIT_*` into git fixtures** (dev-only; the
  v0.80.3 fix's sibling hole in the JS section).
- **CI runs every plugin `*.test.js`.** The curated list had drifted to silently
  miss four passing test files; CI now globs all of them minus two documented
  exclusions, so new test files are never skipped.

## v0.93.1 — `stats` no longer panics on a broken pipe

No `INDEX_VERSION` / `SCHEMA_VERSION` change — this is a CLI output-path bugfix
only; existing indexes are unaffected.

### Fixed
- **`code-graph-mcp stats | head` (or piping into a pager quit early) no longer
  crashes with SIGABRT.** `stats` wrote its table through 36 raw `println!` calls;
  because Rust's stdout is line-buffered, once the reader hangs up the pipe the next
  flush hit `EPIPE` and `println!` **panicked**, aborting the process with exit 134
  and a `failed printing to stdout: Broken pipe` message. Every other subcommand
  already routes stdout through a fallible path — `grep` in particular handles the
  broken pipe silently (`Err(BrokenPipe) => exit 0`), the contract locked by
  `test_cli_grep_sigpipe_graceful`. `stats` now mirrors that contract via a `sout!`
  macro, so an early-closing reader exits 0 cleanly instead of panicking. Found by
  an autonomous loop-testing QA pass over v0.93.0.

## v0.93.0 — grep-hook stops denying when it can't answer; `stats` error telemetry is now classifiable

No `INDEX_VERSION` change — this batch touches the PreToolUse grep hook and usage
telemetry only, not extraction, so existing indexes are unaffected.

Two dogfood-driven changes: one removes a friction case in the grep-redirect hook,
the other makes the `stats` error column actionable instead of an opaque count.

### Changed
- **The `grep`-redirect hook now allows the raw grep through when `code-graph-mcp`
  cannot answer.** When `pre-grep-guide` intercepted a symbol-search `grep`/`rg` on
  indexed source but the inline `code-graph-mcp grep` came back `unavailable` (binary
  ran but failed) or `no-binary` (binary not found), it still emitted a *static* deny —
  blocking the grep while handing the model **nothing**. For a compound command
  (`grep … ; python3 …`) that also dropped the tail, so the whole command half-ran.
  A no-value deny is pure friction that teaches the `CODE_GRAPH_NO_BLOCK_GREP` bypass
  (a dogfood project's only non-converting denies were exactly this). Now, when the
  hook cannot deliver a real answer, it **allows the raw grep** so the command runs
  intact (mirroring the existing `no-hits` fallthrough); the event is still logged with
  a `fallthrough` reason so the conversion funnel keeps its `no-hits`/`unavailable`/
  `no-binary` split. Denies are now emitted **only when the AST answer actually has
  hits**. Setting `CODE_GRAPH_NO_ANSWER_IN_DENY=1` still forces the old static deny.

### Added
- **`stats` now breaks the per-tool `err` count down by kind.** The error column was a
  single opaque number, so a tool showing a 32% error rate was indistinguishable
  between a real bug and benign model name-misses. `stats` now prints an *Error kinds*
  section (`not_found` / `bad_param` / `ambiguous` / `timeout` / `empty_input` / `fk` /
  `other`) per tool, most-errors-first, plus an `unrecorded` remainder for pre-feature
  sessions that logged `err` without a breakdown. (`err_kinds` was already persisted to
  `usage.jsonl`; only the display was missing.)
- **New `bad_param` error bucket splits parameter-validation errors out of `other`.**
  Invalid-value errors (`direction must be one of …`, `min_confidence must be one of …`,
  mutually-exclusive params, unknown relation filter) previously all fell into the
  catch-all `other` bucket — which is why `other` dominated `get_call_graph`'s errors.
  They now classify as `bad_param` (a wrong *value*), distinct from `empty_input` (a
  *missing* required param, which now also captures `… is required`). A high `bad_param`
  count is a signal that a tool's schema/description should make its valid values
  clearer, not that the tool is broken.
- **The `other` bucket now carries one representative message.** Because `other` is the
  residual catch-all, `stats` samples the first `other`-classified error string per tool
  (`↳ other e.g. "…"`) so an unexpected error is self-describing instead of requiring a
  source dive. Local-only diagnostic (`usage.jsonl` is gitignored).

## v0.92.0 — TS/JS destructuring exports and barrel re-exports are now graphed

`INDEX_VERSION` 40 → 41 (existing indexes rebuild on next open).

Two TS/JS extraction gaps found by QA, both breaking cross-module dependency
resolution for extremely common patterns (Redux/React destructured exports and
`index.ts` barrel files).

### Fixed
- **Destructuring exports now extract one symbol per bound name.** `export const {
  host, port } = getConfig()` and `export const [a, b] = getPair()` were captured as a
  *single* `constant` node named after the literal pattern text (`{ host, port }`) —
  not a valid identifier, so `import { host }` dangled to the `<external>` sentinel and
  the destructured symbols were unusable by name and invisible to `show`/`callgraph`/
  `find-references`. The v0.90.0 const-export import-edge fix silently missed every
  destructuring form. Each bound name is now its own `constant` node, so the import
  edge resolves. Common in the wild: Redux `export const { actions, reducer } = slice`,
  React `export const { Provider } = createContext()`, RTK Query hook exports. Renamed
  `{ key: local }` binds the local name; defaults (`{ x = 1 }`), rest (`{ ...r }` /
  `[...r]`), and nested patterns recurse to leaf identifiers.
- **Barrel re-exports now create dependency edges.** `export { X, Y } from './mod'` (the
  `index.ts` barrel pattern) produced **zero** graph edges — the file had no tracked
  dependency, so `deps` showed nothing, `find-references` missed it, and `affected`/
  `impact`/`cycles`/`tour` could not traverse *through* it. Each re-exported name now
  emits a `REL_IMPORTS` dependency edge stamped with the same `js_module` specifier a
  regular `import { X } from './mod'` carries, so Phase-2 resolves it to the source
  file. (`export * from './mod'` wildcards stay module-level-unresolved — a shared
  limitation with namespace imports `import * as ns`.)

## v0.91.0 — C++ header classes are graphed; test symbols stop leaking into dead-code / ast_search / surprising

`INDEX_VERSION` 39 → 40 (existing indexes rebuild on next open).

A QA-sweep batch: one new extraction capability plus a set of correctness fixes for
surfaces that were misclassifying test symbols or leaking graph-internal nodes.

### Added
- **C++ classes declared in a `.h` header are now extracted.** `.h` is C-vs-C++
  ambiguous by extension, so it was parsed as C — whose grammar can't parse `class`/
  `namespace`. In the most common C++ layout (declaration in `.h`, definition in
  `.cpp`) that meant the header's class **symbols never existed as nodes**, and their
  base-class `inherits` edges were never emitted; `overview`/`callgraph`/`dead-code`/
  `find_references` were blind to them. A `.h` whose content contains C++ markers
  (`::`, `public:`/`private:`/`protected:`, `class `, `namespace `, `template<`) is now
  parsed as C++. Gated on those markers so a pure-C header (`#define`/`#ifndef`/plain
  `struct`) still parses as C; a false positive is low-harm because the C++ grammar is a
  near-superset of C. `detect_language` itself is unchanged (path-only; the upgrade
  happens at the parse point where source is available).

### Fixed
- **`dead-code` and `surprising` no longer misclassify integration tests.** Both
  filtered tests by the raw AST `is_test` flag only, which the parser sets for
  `#[cfg(test)]`/`@Test`/gtest markers — but **not** for the most common test shape, a
  `def test_foo()` / `.test.ts` file whose test-ness is name/path-based. So `dead-code`
  reported pytest tests as orphans (inviting deletion of a live test) and `surprising`
  flagged a `test_foo → foo` call as an "unexpected coupling". Both now apply the full
  `is_test_node` predicate (a new `domain::is_test_node_sql` GLOB helper mirroring
  `is_test_symbol`) so they classify tests exactly like `callgraph`/`show`/`centrality`.
- **`ast_search` no longer leaks `<module>`/`<external>` placeholder nodes or test
  symbols into results.** Both the CLI (`ast-search`) and MCP (`ast_search`) surfaces —
  and both the FTS-query and filter-only paths — skipped the `is_skippable_result`
  triad that `search`/`similar` apply, so `ast-search <extern-name>` returned an
  `<external>:0-0` stub and `<module>` file nodes next to real symbols, and
  `ast-search --type function` listed test functions. Now filtered on every path.
- **A C++ class's inline constructor no longer receives a bogus `inherits` edge.** A
  class with an inline constructor (`Circle(double){}`) produces a `method Circle` node
  sharing the class name; the relation-source resolver matched an `inherits`/`implements`
  relation to *all* same-named nodes, so `Circle inherits Shape` attached to both the
  class and the constructor method. Inheritance sources are now restricted to type
  nodes (a `function`/`method` can never be a supertype).
- **`tour` now appears in `code-graph-mcp --help`.** The command (dependency-ordered
  reading order) shipped with its own `--help`, JSON output, and a dispatch arm, but was
  never listed in the top-level command help — it was undiscoverable.

### Changed
- **`dead-code` no longer reports a false-clean "No dead code found" when candidates sit
  below `--min-lines`.** At the default `min-lines` 3, a shorter dead function was
  silently hidden and the message read as "clean" — misleading, especially for an LLM
  caller that won't think to widen the threshold. The empty message (CLI stderr + MCP
  `summary`) now names how many shorter symbols the threshold hid and how to see them,
  probing at `min_lines=1` so the hint fires only when it actually hid something.
- **CLI `trace` discloses its route-framework coverage on an empty result.** Route
  extraction covers Express/Connect (JS/TS), Go `net/http`, and Flask/FastAPI (Python)
  only; a Rust (axum/actix) or Java (Spring) project has real routes the extractor never
  sees, so a bare "No routes matching" read as "no such route". The CLI message now
  mirrors the MCP trace path's framework-coverage note.

## v0.90.0 — TS/JS exported value constants are now graphed

`INDEX_VERSION` 38 → 39 (existing indexes rebuild on next open).

### Added
- **Top-level `export const/let X = <value>` in TS/JS/TSX is now extracted as a
  `constant` symbol.** Previously only arrow-function consts (`export const f = () =>
  {}`) became nodes; a value const — a config literal (`export const API_URL = "…"`),
  a route/config table, or a widely-imported singleton (`export const store =
  defineStore(…)`, `const logger = createLogger(…)`, `const svc = new Service()`) — was
  not a symbol, so `import { X } from './mod'` bound to the `<external>` sentinel and
  the cross-module dependency was **invisible** to `tour`, `affected`, `impact`, and
  `project_map`. The import now resolves to the real node and forms a `REL_IMPORTS`
  edge. A measurement across four TS/JS projects found 66 module dependencies that
  existed *only* through such value-const imports and were therefore entirely absent
  from the graph. This brings TS/JS to parity with the existing Rust
  `const`/`static` extraction.

  Scope and safeguards: only **exported, top-level** declarations are extracted — a
  function-local `const x = 5` cannot be imported cross-file, so it is deliberately not
  a symbol (no noise). Arrow/function-valued consts are unchanged (stay `function`). An
  exported-but-unused constant is reported by `dead-code` as *exported-unused* (the
  softer category, same as a `pub` Rust item), never as a hard orphan; one-line consts
  fall below the default `min-lines` and are not reported at all. This release is
  scoped to TS/JS/TSX — Go package-level `const`/`var`, Python module constants, and
  Java `static final` fields have different import idioms and are not addressed here.

## v0.89.0 — semantic-search confidence warning fires on an honest signal

The `low_confidence_warning` on `semantic_code_search` was a false alarm on the
tool's primary use case. This release retriggers it on a signal we can actually
trust, backed by a new regression-tracked calibration benchmark. No
`INDEX_VERSION` change (query-time only).

### Changed
- **`low_confidence_warning` now fires only when a result set has no text anchor
  (vector-only), not on a `match_confidence` threshold.** A new calibration bench
  (`scripts/embedding_benchmark/eval_confidence.py`, 36-query corpus over 5 classes)
  measured that `match_confidence` pins at ≈0.45 for essentially every multi-word
  natural-language query — good and nonsense alike (`OR-fallback 0.6 × intersection
  0.75`) — and that **no available signal separates a good NL query from nonsense**:
  `match_confidence` (good−nonsense = 0.023), RRF relevance, and raw top-1 vector
  cosine (0.016, with full overlap — the good query "normalize a user supplied file
  path" scored the global-minimum cosine, below every nonsense query). The old
  `match_confidence < 0.5` trigger therefore fired on **100% of good NL queries**
  (which retrieve relevant results 82% of the time in the corpus), telling the caller
  the results were "largely vector-similarity noise" — false, and it pushed the model
  to distrust correct answers. The warning now fires only when FTS returns no hits at
  all (results ranked by vector similarity alone — the one case where that claim is
  literally true), and its wording states the mechanic without claiming the results are
  wrong. Bench impact: good-NL false-alarm rate 100% → 0%, overall calibration accuracy
  0.484 → 0.871, exact-identifier and code-vocabulary queries unchanged (0% warned).
  `match_confidence` is still returned as a rough query-shape signal; the removed
  `CONF_WARNING_THRESHOLD` constant is gone.
- **New `CODE_GRAPH_EMIT_CONFIDENCE` env seam (internal, off by default).** When set,
  `serve` emits a `[CONF_PROBE]` line per search to stderr (`match_confidence`, raw top
  cosine, FTS/vector hit counts) so the calibration bench can measure signals the JSON
  response does not expose. No effect on the response contract.

## v0.88.0 — correctness fixes: call resolution, JSON-RPC errors, search confidence

Three correctness fixes surfaced by end-to-end QA. One `INDEX_VERSION` bump
(37 → 38; existing indexes rebuild on next open).

### Fixed
- **Receiver-typed call resolution no longer wildcard-matches sibling types
  (`INDEX_VERSION` 38).** `filter_method_ids` gated method candidates with a raw
  `qualified_name LIKE 'Type.%'`, so a receiver/impl type name containing a `_` or `%`
  — legal identifiers like `my_widget` or `Foo_Bar` — was matched as a SQL wildcard:
  `w = My_Widget(); w.run()` also bound `MyXWidget.run` (the `_` matched `X`), forging a
  false cross-type call edge. The type name is now escaped (`… ESCAPE '\'`), so the
  type-restricted resolution paths — Rust `self.method()` (SelfType) and Python
  constructor-inferred `recv.method()` (rtype) — bind only to the genuine type's method.
  Existing indexes carry the rare stale edge until rebuilt.
- **Malformed JSON-RPC requests now return the spec-correct error.** A message that
  parsed as JSON but was not a conforming Request (missing `method` or `jsonrpc`)
  returned `-32700 Parse error` — the code reserved for invalid JSON — and dropped the
  request `id`. It now returns `-32600 Invalid Request` and echoes the recoverable `id`
  so a client can correlate the failure; a JSON-RPC batch array is rejected cleanly
  (`-32600`, "batch requests are not supported") instead of leaking a serde type error;
  and a malformed message with no `id` (a notification) correctly receives no reply.
  Well-formed requests are unaffected — the diagnosis only runs on a deserialization
  failure, keeping the happy path a single allocation-free parse.
- **`semantic_code_search` surfaces its confidence signal on every result size.** The
  `match_confidence` value and the `low_confidence_warning` (which flags queries whose
  hits are largely vector-similarity noise) were attached only to the compressed
  response returned for large result sets; a low-confidence query small enough to skip
  compression returned a bare array with no confidence signal at all. Both paths now
  route through a shared `finalize_search_results` helper, so the caller can judge
  trustworthiness regardless of result count. A confident hybrid result still returns a
  bare array (unchanged contract) and an exact-identifier match stays exempt from the
  warning.

## v0.87.1 — steering doc ↔ CLI alignment guard (tests + internal)

No runtime behavior change. Adds a test that keeps the plugin's tool-steering
surfaces honest against the live CLI, closing a latent doc-drift gap.

### Internal
- **Steering doc ↔ CLI alignment test (`tests/doc_cli_alignment.rs`).** The three
  steering "sync faces" — the `.claude/plugin_code_graph_mcp.md` detail doc, the MCP
  `instructions` string, and the generated `CLAUDE.md` managed block (all three
  project-type variants) — are now asserted, via clap introspection
  (`XxxArgs::command()`, recursive into `snapshot` subcommands), to reference only
  real subcommands and real flags, with per-command flag attribution. Previously the
  block was only snapshot-guarded (`steering_block_drift_check` byte-compares the
  `generic` render; `adopt.test.js` matches variant-row literals) and the detail doc /
  `instructions` had no guard at all — a CLI flag rename or a hand-edit typo could
  silently stale them. Includes a meta-test proving the checker rejects fabricated
  commands, fabricated flags, and flags attributed to the wrong command. This is the
  first CLI-validity coverage for the web/frontend variant rows (`trace` / `refs`).
- The MCP `instructions` strings were hoisted from a function-local `const` in
  `handle_initialize` to module-level `pub const INSTRUCTIONS_{NOISY,QUIET}` so the
  test can scan them; the value is byte-identical and the ≤1500-byte compile-time
  budget assert moved with them.

## v0.87.0 — Python indexing accuracy: decorators, framework dead-code, receiver-type resolution

Fixes two reported Python-indexer issues (GitHub #31, #32) with one `INDEX_VERSION` bump
(35 → 37; existing indexes rebuild on next open).

### Fixed
- **Decorators are no longer stripped from Python symbols (#31).** `get_ast_node` bound each
  `def`/`class` to the inner tree-sitter node, so its `start_line` / `code_content` began at
  `def`/`class` and dropped the whole decorator stack. For Pydantic v2 the decorator *is* the
  contract — `@field_validator("lat", mode="before")` carries the validated field and mode — so
  semantic search and impact analysis were blind to it (a plain `@property` was also stripped).
  Symbols now bind to the enclosing `decorated_definition` wrapper (functions, methods, async
  methods, and classes), so the extent and stored source include the full decorator stack;
  name / signature still come from the inner definition.
- **Framework-decorated methods are no longer false-positive dead code (#32, dominant cause).**
  Methods registered or accessed via a decorator — pydantic `@field_validator` / `@model_validator`
  / `@computed_field` / serializers, pytest `@fixture`, stdlib `@property` / `@cached_property` /
  `@abstractmethod` / `@overload`, NiceGUI `@ui.refreshable` / `@ui.page` — are dispatched
  dynamically, so they carry no static call edge and were reported as orphans (≈83 of 86 in the
  reporter's pydantic + NiceGUI codebase). `find_dead_code` now excludes them (single SQL source,
  CLI + MCP), the same class as constructors and dunder methods. Enabled by #31 placing the
  decorator text into `code_content`.

### Added
- **Python receiver-type call resolution (#32, second cause).** A call `recv.method()` whose
  receiver type is fixed by a single local `recv = ClassName(...)` constructor assignment — or by
  an explicit parameter annotation `def f(recv: ClassName)` — now resolves to `ClassName.method`
  instead of dropping the ambiguous by-name fan-out when the method name is shared across classes
  (`writer.write()` with `write` defined on three classes previously orphaned all three).
  Conservative by construction: only a provably-single assignment / explicit annotation infers a
  type, and an inherited or mis-inferred method falls through to the existing bare resolution — so
  it adds precision without ever creating a false cross-type edge. Brings to Python the
  type-qualified receiver resolution PR #19 shipped for Rust.

`INDEX_VERSION` 35 → 37 — existing indexes rebuild on next open. No `SCHEMA_VERSION` change.

## v0.86.1 — vector-layer race + cache-fingerprint parity fast-follow

A post-v0.86.0 code review surfaced two latent concurrency/parity defects the shipped tests
missed. Neither corrupts data under normal single-model operation; both undermine invariants
v0.86.0 claimed, and the fixes are cheap.

### Fixed
- **Orphan reap / cache GC can no longer delete a live entry under concurrency.**
  `reap_orphan_vectors` and `gc_embedding_cache` enumerated stale rows in autocommit, then
  deleted inside a *deferred* transaction whose write snapshot is taken only at the first
  delete — a window in which a concurrent writer could insert a node that reuses an enumerated
  orphan's rowid (`nodes.id` is a plain `INTEGER PRIMARY KEY`, so rowids restart after a wipe)
  and have its fresh vector deleted. Both now hold an `IMMEDIATE` write lock across
  enumerate+delete, so the set cannot change under them.
- **Every embed path now validates the model fingerprint before reusing the cache.**
  The same-dim model-change guard previously ran only in the MCP backfill; CLI `rebuild-index`,
  foreground indexing, and the incremental path reused `embedding_cache` unchecked, so a future
  same-dim model swap could serve stale embeddings with no self-heal on a CLI-only install. The
  guard now runs at the shared `embed_and_store_batch` chokepoint and before the startup cache
  seed (the pre-work-check guard is retained so an all-already-embedded index still invalidates).
- **Coverage counting propagates transient read errors instead of reporting 0%.**
  `count_nodes_with_vectors` swallowed a `SQLITE_BUSY` on its coverage JOIN as a misleading
  `0/total`; it now probes the table then propagates real errors (matching `count_unembedded_nodes`).

No `INDEX_VERSION` or `SCHEMA_VERSION` change.

## v0.86.0 — embedding cache: skip re-embed on version bumps; vector-layer correctness

Investigating a daagu `1% vec` statusline — the embedding backfill restarting from ~0% on
every `INDEX_VERSION` bump — surfaced three vector-layer defects plus the root-cause fix.
`% vec` is embedding-backfill coverage, not AST re-indexing; it restarts because a version
bump wipes nodes and re-embeds. This release makes that correct, observable, and — via a
content-hash cache — no longer a full re-embed.

### Added
- **Content-hash embedding cache (`embedding_cache`) reuses embeddings across rebuilds.**
  Embeddings are keyed by `blake3(context_string)` in a table that survives the version-bump
  wipe, so a rebuild with unchanged content is a byte copy instead of minutes of candle
  re-inference. Every embed path (foreground indexing, `rebuild-index`, the background
  backfill) reuses through it, and an existing index is seeded from its current vectors on
  startup so the very next bump reuses. Created idempotently — no schema-version bump, no
  migration, downgrade-safe. On daagu's ~14.5k embeddings a bump now reuses 14485/14510 with
  zero model calls.

### Fixed
- **Orphan vectors from the async backfill race no longer accumulate.** The backfill computes
  embeddings on a separate connection over a seconds-long window; if a node was deleted
  meanwhile, a late vector insert created a permanent orphan (vec0 has no FK, and the delete
  trigger only reaps a matching-node delete — orphans are sticky). Guarded at the insert site;
  existing orphans are swept by a startup reap.
- **Embedding coverage can no longer exceed 100% or falsely report "complete".**
  `count_nodes_with_vectors` counted raw `node_vectors` rows (including orphans), inflating the
  numerator past the embeddable total and masking genuinely unembedded nodes. It now counts
  embeddable nodes that actually have a vector, so CLI and MCP status agree and `complete` means
  all-embeddable-embedded.
- **A same-dim embedding-model change now invalidates stale vectors.** The model content
  fingerprint is tracked; on a change the cache and `node_vectors` rebuild (previously a
  same-dim weight swap left stale vectors that the dim check missed).

No `INDEX_VERSION` or `SCHEMA_VERSION` change; existing indexes are cleaned and seeded on the
next server start.

## v0.85.9 — grep: de-duplicate output when path arguments overlap

`grep` no longer prints a match more than once when a file is reachable through
more than one path argument (`grep pat . src`, or the same file passed twice).
CLI-only (`src/cli.rs`); no schema or index change.

### Fixed
- **Overlapping or repeated `grep` path arguments no longer double the output.**
  ripgrep scans each path argument independently, so `grep pat . src/parser`
  emitted every match under `src/parser/` twice, and the v0.85.3 determinism sort
  made the duplicates adjacent. Content mode doubled each match line and its `→`
  AST annotation, `-l` listed the file twice, and `-c` printed two `path:N` rows
  for one file. All three modes now collapse exact-duplicate rows after the global
  sort (the `-c` count stays correct — one row per file). Real grep/ripgrep
  duplicate here; this AST-context grep de-duplicates to match what an agent
  passing overlapping scopes expects.

## v0.85.8 — auto-update: check on session start; close the release-publish throttle race

The auto-updater now re-checks on every session start / reload, and no longer
latches a stale "up to date" answer when a check races just ahead of a release.
Plugin-only (`claude-plugin/scripts`); no schema or index change.

### Changed
- **A session start / reload now forces an immediate update check.** The
  SessionStart hook already spawned a background check, but it went through the
  6h throttle — so a recently-throttled check silently skipped, and a freshly
  opened session would not pick up an available update. A session start /
  resume / clear / reload now forces the check, bypassing the soft throttle down
  to a 2-minute anti-hammer floor (GitHub rate-limit backoff still wins).
  Automatic mid-session compaction keeps the gentle background cadence. The
  update installs in the background and activates on the *next* session (Claude
  Code loads plugin hooks at startup).

### Fixed
- **A release that publishes seconds after an update check is no longer hidden
  for 6 hours.** The flat 6h throttle latched the stale "up to date" answer when
  a check raced just ahead of a release (observed live: v0.85.7 published 8s
  after a check pinned v0.85.6, so every session reopen for the next 6h
  re-reported "up to date"). An "up to date" result is now re-verified on a
  30-minute cadence, bounding the blind window; a pending update keeps the 6h
  interval and rate-limiting keeps its 24h backoff.

## v0.85.7 — output polish: `surprising`/`report` drop `<module>`; `show --impact` discloses test callers

Two low-severity dogfooding output fixes; no schema or index change.

### Fixed
- **`surprising` / `report` no longer surface the synthetic `<module>` scope node.** A
  top-level call is attributed to a `<module>` pseudo-node, which is not an actionable
  symbol — `project_map`, `dead_code`, and `module_exports` all exclude it, but
  `surprising_connections` did not, so a `<module> → target` coupling could show up in the
  `surprising` list and the `report` summary (both call it). It is now filtered there too,
  matching the sibling queries. Query-only; no schema or index change.
- **`show --impact` now discloses filtered test callers.** `show --impact` reported
  prod-only `direct_callers` / `transitive_callers` but not how many test callers were
  excluded from the risk count, while MCP `get_ast_node`, `callgraph`, and `project_map`
  all disclose their test-caller counts. It now emits `test_callers_filtered` in the
  `--json` impact object (exact parity with `get_ast_node`) and a `(N test callers
  excluded from the risk count)` line in the text output. Data was already correct — this
  closes a CLI/MCP disclosure gap.

## v0.85.6 — `overview` / `show` output: owner-qualified members, no doubled signature parens

Three dogfooding output-quality fixes; no schema or index change.

### Fixed
- **`overview` / `module_overview`: two exported classes in one file no longer hide
  each other's same-named method.** v0.85.5 started surfacing methods of exported
  classes, but `get_module_exports` deduped on `(name, file_path)` — so if a
  TypeScript/JavaScript file exported two classes that each defined, say, `render()`,
  the two `render` methods collided and only one appeared in the module summary. The
  dedup now keys on `(qualified_name, file_path)`, so `Animal.render` and
  `Widget.render` stay distinct. Top-level symbols (whose `qualified_name` equals their
  `name`) still collapse feature-gated duplicates as before. No schema or index change.
- **`overview` / `module_overview` now qualify class/impl members by their owner.** Once
  the dedup above stopped dropping same-named methods, they surfaced as indistinguishable
  bare rows (two `render`s). Any symbol whose `qualified_name` differs from its bare
  `name` now renders as `Owner.member` in the text outline and the inactive-symbol
  summary. This is repo-wide and LLM-visible, not TS-only: it also covers Rust/Python
  impl & associated functions (`ToolRegistry.new`, `EmbeddingModel.load`,
  `McpServer.tool_project_map`), which are stored as `function` nodes with a dotted
  `qualified_name` — so a real module overview relabels every such row. It disambiguates
  same-named members (two classes each with `render()`, or many `new`s) and matches what
  `show` already prints. Structured output (CLI `overview --json`, MCP `active_exports` /
  `hot_paths`, compact) also gains an additive `qualified_name` field, emitted only when
  it differs from `name`; the `name` field itself stays a bare identifier for existing
  consumers, and top-level functions/consts are unchanged.
- **`show` / `search` / `ast_search` no longer double-wrap function signatures.** The
  human output rendered `fn foo  loc  ((a, b)) -> R` — and `(())` for no-arg functions —
  because `format_node_compact` wrapped `param_types` in parentheses even though the
  parser already stores it parenthesized (structurally: it is the `parameters` node's
  text, which includes its delimiters). It now appends the stored value verbatim, so the
  signature reads `(a, b) -> R`. `--json` was already correct and is unchanged. The unit
  test's fixture had used a paren-less `param_types` that never matched real parser output
  and so hid the bug; it now mirrors the parser and guards against re-introducing it.

## v0.85.5 — `overview` surfaces TS/JS methods; case-insensitive `direction`/`relation`

Two dogfooding fixes, no schema or index change.

### Fixed
- **`overview` / `module_overview` now list methods of exported classes.** For a
  TypeScript/JavaScript file, `get_module_exports` contributed only symbols with their
  own `export` edge — but ESM emits an export edge for a class, never its methods, so
  every method of an exported class was dropped from the module summary, while
  Python/Rust/Go methods (files with no export edges) showed. Methods of an exported
  class are public API too and now appear. Non-exported classes' methods stay hidden,
  and files with no exports are byte-for-byte unchanged; the added lookup is kept off
  the hot path — it runs only for the few non-exported rows in export-bearing files —
  so pure Python/Rust/Go repos see no query slowdown.
- **`--direction` / `--relation` (CLI and MCP) accept case variants.** `--direction BOTH`
  or `relation: "CALLS"` were rejected as invalid while the sibling filters
  (`--node-type`, `--min-confidence`, `--language`) already accepted any case. All three
  now normalize through a shared canonicalizer, so case no longer matters; cross-vocabulary
  values (a `deps` word on `callgraph`, or vice versa) and unknown values are still
  rejected loudly.

## v0.85.4 — `doctor` exit code reflects what it couldn't fix

`code-graph-mcp doctor` exited 1 even after repairing every issue it found, so a
successful auto-repair looked like a failure to `doctor && …` chains and self-heal
automation. A bug fix; no schema or index change.

### Fixed
- **`doctor` exits 0 after a fully-successful repair.** The exit code keyed off
  issues *found* (`issueCount > 0`) instead of issues left *unresolved*, so a run
  that fixed everything ("N/N addressed") still exited 1 — breaking `doctor && …`
  and any self-heal automation that reads a nonzero exit as failure. It now exits 0
  when every found issue was resolved, and 1 only when something could not be fixed.
  `--check-only` is unchanged (still exits nonzero if any issue exists). The same
  exit logic lived in two entry points (the `doctor.js` CLI and the `lifecycle.js
  doctor` dispatch); both were fixed. The `hooks-invalid` repair now re-scans after
  `install()` and reports success only if the paths are actually clean, so a repair
  that cannot restore missing plugin scripts stays exit 1 instead of falsely
  reporting healthy.

## v0.85.3 — Deterministic `grep`; `--language` validation; CLI consistency

`grep` printed the same matches in a different file order on every run — the same
determinism class as v0.85.1/.2, but a different root cause (ripgrep's parallel walk,
not a HashMap). Alongside it, a batch of CLI/MCP correctness fixes surfaced by
dogfooding. All bug fixes; no schema or index change.

### Fixed
- **`grep` output is deterministic and globally path-sorted.** ripgrep parallelizes
  the file walk and emits results in worker-completion order, so the same `grep`
  (`-l` / `-c` / default / `--json`) shuffled file order on every run — observed as a
  distinct output on nearly every run — defeating diffs, caching, and trust. The
  collected result set is now sorted by path (and line) in every mode. Sorting in
  post-processing rather than via `rg --sort path` keeps ripgrep parallel (faster),
  folds the supplement (git-tracked-but-unwalked files) and multi-path input into a
  true global order, and imposes no minimum ripgrep version.
- **`search --language` / `semantic_code_search` reject an unknown language.** An
  unknown or mistyped language (`--language pyton`) was silently swallowed and
  reported as a too-narrow filter ("Broaden or clear the filter"), implying the value
  was valid. It now fails at entry — `Unknown language filter: 'X'. Valid: …` —
  mirroring the existing `--node-type` guard, on both the CLI and the MCP tool.
  Normalizing to the canonical language also fixes the MCP tool's case-sensitive
  match, so `language="Rust"` now works.
- **`similar --limit` is accepted** as an alias of `--top-k`, so the `--limit`
  learned from `search` / `ast-search` / `centrality` no longer hits a cryptic
  "unexpected argument" on `similar`.
- **`trace` prints a clean not-found message.** A no-match route reported the
  double-prefixed `Error: [code-graph] No routes matching`; it now uses the same
  clean `[code-graph] …` + exit 1 as `refs` / `impact` / `show`.

## v0.85.2 — Deterministic `overview` and `map`

`overview` / `module_overview` and `map` / `project_map` printed the same results in
a different order on every run — the same HashMap-ordering class as v0.85.1's
call-graph fix, in two more surfaces. A whole-project `overview .` also listed the
synthetic `<external>` import pseudo-symbols. All bug fixes; no schema/index change.

### Fixed
- **`overview` / `module_overview` output is deterministic.** `get_module_exports`
  deduped through a HashMap and returned `into_values()`, discarding the SQL
  `ORDER BY caller_count DESC` — so the same directory printed its symbols shuffled
  every run. It now sorts by a total order (caller_count DESC, file, line, name), and
  excludes the synthetic `<external>` pseudo-file (unresolved imports like `numpy` /
  `std::io::Write`, which are not project symbols and, all sharing
  caller_count=0/file/line, made `overview .` non-deterministic even under the tuple
  sort).
- **`map` / `project_map` output is deterministic.** Modules and dependencies were
  sorted by a single key (function count / import count) with no tiebreaker, so
  equal-count entries shuffled every run; the module list also came from a HashMap.
  Sorting now carries unique tiebreakers (path; (from, to)), and every LIMIT-bounded
  section (hot functions, key symbols, entry points) gained a unique `ORDER BY` tail
  so truncation drops a deterministic subset.

## v0.85.1 — Deterministic `callgraph`, clearer degenerate-input errors

`callgraph <symbol>` (the default `--direction both`, on both the CLI and the MCP tool)
printed the same caller/callee set in a different order on every run, because the merge
step iterated a `HashMap`. Two smaller CLI commands blamed the graph for a degenerate
argument. All three are bug fixes with no schema or index change.

### Fixed
- **`callgraph` output is deterministic across runs.** The `direction=both` merge
  collected `HashMap::into_values()` (per-instance random iteration order) and then sorted
  by depth only, so same-depth callers/callees came back shuffled on every invocation — the
  same query produced a different order each time (CLI text and MCP `results[]` alike),
  defeating diff and reproducibility. The merge now preserves each direction's existing
  `(depth, caller_count DESC, node_id)` relevance order, and the call-graph SQL gained a
  unique `node_id` tiebreaker so the row-limit truncation on a wide fan-out drops a
  deterministic subset. The caller set is unchanged; only the order is now stable.
  Query-time only — no `INDEX_VERSION` bump.
- **`centrality --limit 0` no longer claims the graph has no chokepoints.** `--limit 0`
  returned an empty ranking and printed "No chokepoints found (graph has no multi-hop call
  paths)", blaming the graph for a user-supplied zero. `--limit` now clamps to 1 (matching
  `callgraph --depth`), so `--limit 0` yields the top chokepoint.
- **`deps <dir>` points at `overview` instead of "File not found".** Passing a directory
  hit the missing-file branch (`is_file()` is false for a directory that plainly exists).
  It now reports that the path is a directory and suggests `overview`, in both the text and
  `--json` error paths.

## v0.85.0 — Surfaces stop hiding real results (mixed-language overview, import-resolved calls, `affected` input)

Three independent surfaces were silently dropping results a user had every reason to
expect. `overview` on a directory hid every non-export-language file whenever a sibling
had ESM `export`s; `callgraph`/`impact` hid a cross-file call that an explicit import had
already resolved, for any function name defined in two or more files; and a bare
`affected` printed "0 tests to re-run" when the real cause was a forgotten argument. None
reproduced on this repo's own dogfood layout, which is why they survived. The
call-visibility fix reclassifies existing edges, so it bumps `INDEX_VERSION` (34 → 35) —
existing indexes rebuild once on upgrade.

### Fixed
- **`overview` / `module_overview` show every file in a mixed-language directory.**
  `get_module_exports` ran a global two-phase fallback: explicit ESM `export` edges first,
  all-top-level-symbols only if that returned nothing. A directory mixing a `.ts` file
  (with `export`s) and `.py`/`.rs`/`.go` files therefore surfaced only the TypeScript and
  silently dropped every non-export-language sibling — even though those symbols were
  indexed and findable via `search`/`show`. The decision is now made per file: a file that
  declares exports contributes its exports (the public-API view is preserved); a file with
  none contributes every top-level symbol. The export-bearing file set is computed once, so
  it stays a single scan.
- **Import-resolved cross-file calls stay visible in `callgraph` / `impact`.** When a file
  explicitly imports a symbol (`import { process } from './helpers'`), a call to it is
  bound to that exact definition — but the confidence classifier then relabeled the edge
  `ambiguous` purely because the name `process` also existed in another file, and the v0.76
  confidence floor hid it by default. So `callgraph`/`impact` showed no callee/caller for a
  call the resolver had pinned exactly, for any name defined in ≥2 files
  (`process`/`handler`/`run`/`init`/`save`/…). An edge whose target is imported by the
  caller's file is now `inferred` (visible by default), while genuine bare-name fan-out
  with no corroborating import stays `ambiguous`. On this repo the rebuild reclassifies 173
  of 328 previously-`ambiguous` edges to `inferred`; 155 genuinely ambiguous ones stay
  hidden — no over-promotion.
- **`affected` with no input explains itself.** `affected` takes an explicit changed-file
  list (positional or `--stdin`); it does not auto-diff git. A bare invocation had no input
  and printed "0 test file(s) to re-run" — indistinguishable from "nothing is affected" and
  easy to misread as "no tests needed". It now prints a stderr hint pointing at the intended
  pipe (`git diff --name-only HEAD | code-graph-mcp affected --stdin`); stdout and exit code
  are unchanged, and a correctly-used empty `--stdin` pipe stays silent.

### Migration
- `INDEX_VERSION` 34 → 35 (call-edge confidence reclassification). Existing indexes rebuild
  automatically on first use after upgrade; no action required.

## v0.84.1 — Plugin MCP auto-upgrades a non-project stub when the cwd becomes a project

The plugin's MCP launcher serves a 0-tool stub in a non-project cwd (no `.git`/manifest
— e.g. the `/tmp` headless calls that never use code-graph), which avoids a throwaway
index and an empty tool catalog. That verdict was latched for the launcher's lifetime:
opening Claude Code in a bare directory and *then* `git init`-ing / scaffolding it left
the MCP server stuck at `connected · no tools` (and no statusline, no index) until a full
restart. The stub now re-checks and upgrades itself in place. Launcher logic only — no
index or schema change.

### Fixed
- **Non-project MCP stub now upgrades in place.** The non-project stub advertises
  `tools.listChanged`, polls the cwd, and when it becomes a real project (with no local
  code-graph server) it spawns the real binary, proxies the live JSON-RPC connection to
  it, and emits `notifications/tools/list_changed` so the client re-fetches the now-real
  tool list — no Claude Code restart required. Genuinely non-project `/tmp` callers never
  satisfy the upgrade condition, so they stay as cheap as before (no binary spawn, no
  index created). A persistently unresolvable binary is retried a bounded number of times,
  then logs a one-time restart hint. The stub logic moved to `mcp-stub.js` with unit
  coverage of the handoff, queue-during-handoff, failure-fallback, and poller paths.

## v0.84.0 — Compound-grep inject: grep-response gate + callgraph widening

The PostToolUse compound-grep inject now fires only when it adds something the model
does not already have. Injects were consistently ignored when they re-stated hits the
model's own grep had already surfaced; the hook now reads the executed command's output
and suppresses the redundant inject, keeping it for the case where the grep found
nothing (cg's structural answer is then genuinely new). Hook logic only — no index or
schema change.

### Changed
- **grep-response gate on the compound-grep inject.** `post-grep-inject` reads the
  executed command's stdout (`tool_response.stdout`) and skips the AST inject when the
  grep already surfaced the searched symbol, recognizing the common grep hit formats
  (`path:content`, `path:line:content`, single-file `line:content`, and `grep -l` bare
  path). It injects only when the grep found nothing, or the output is unreadable (no
  regression). The hit-shape scan is bounded to a line prefix so untrusted, unbounded
  grep stdout on the blocking hook cannot stall. Opt out with `CODE_GRAPH_NO_INJECT=1`.

### Added
- **Callgraph inject for multi-symbol grep patterns.** An alternation / multi-identifier
  grep (e.g. `foo|bar`) now resolves its identifier tokens and injects the cross-file
  callgraph for the first one with real edges, instead of falling back to the redundant
  grep echo.
- **`inject_by_mode` in `stats`.** `code-graph-mcp stats` (text and `--json`) now breaks
  inject events down by payload mode (callgraph / grep / show), so the high-value
  callgraph share is directly visible.

## v0.83.0 — Go, Dart & C++ inheritance extraction

Inheritance edges are now extracted for three more languages, closing a long-standing
per-language parity gap (Go and Dart were documented as supporting inheritance but did
not fully extract it; C++ produced none). Upgrading rebuilds the index once
(`INDEX_VERSION` 31→34).

### Added
- **Go struct & interface embedding → `inherits`.** `type Dog struct { Animal }` now
  records `Dog inherits Animal` (embedding is Go's idiomatic method promotion), and
  interface composition (`type ReadWriter interface { Reader; Writer }`) records
  `ReadWriter inherits Reader`/`Writer`. Pointer (`*Base`), qualified (`pkg.Type`), and
  generic (`Base[int]`) embedded types all bind on the base type name. Go previously
  produced no inheritance edges at all. A normal named field (`f Foo`) stays has-a.
- **Dart mixins → `inherits`.** `class C extends Base with M, N` now records
  `C inherits M` and `C inherits N` (mixin application injects the mixin's methods),
  alongside the existing `extends`/`implements` edges. A `with`-only class no longer
  emits a malformed target.
- **C++ base classes → `inherits`.** `class Dog : public Animal, private Trackable`
  now records `Dog inherits Animal` and `Dog inherits Trackable`. Multiple, `struct`,
  qualified (`ns::Base`), and template (`Tmpl<int>`) bases are all handled; access
  specifiers (`public`/`private`/`protected`) are ignored since C++ has no separate
  interface concept. C has no inheritance and is unchanged.

### Fixed
- **Go 1.18 generics no longer confuse inheritance extraction.** An interface type-set
  constraint (`interface { Signed | Unsigned }`) is no longer misread as embedding — it
  previously emitted a bogus `inherits` edge to the first union term. Embedded generic
  types (`struct { Base[int] }`, `interface { Container[T] }`) are now extracted instead
  of silently dropped.

## v0.82.1 — accurate `reindex` help, read-only `doctor --check-only`

Two user-facing message corrections. No behavior change, no index rebuild, no schema change.

### Fixed
- **`reindex` help no longer claims "Reset index".** Plain `reindex` (without `--from-snapshot`)
  runs an incremental refresh via `cmd_incremental_index` — it does not drop the index; only
  `--from-snapshot` does, and `rebuild-index` is the unconditional rebuild. The main help and the
  clap `about` (`reindex --help`) overstated it as "Reset index", which would mislead a user
  trying to recover a stale index into thinking a no-op incremental pass had rebuilt it. Both now
  describe the incremental behavior and point at `rebuild-index` for a full rebuild.
- **`doctor --check-only` no longer prints "Fixing...".** `--check-only` is read-only (it never
  reaches the repair path), but `formatReport` appended "Fixing..." whenever fixable issues
  existed — so `doctor --check-only` announced a fix while changing nothing, contradicting its
  documented "report issues without changing anything" contract and implying `settings.json` /
  `MEMORY.md` had just been rewritten. The report now reads "Run without --check-only to fix." in
  check-only mode.

## v0.82.0 — cwd-relative CLI paths, prune-safe plugin updates, bounded dev disk

Three independent changes: relative CLI path arguments now resolve against your working
directory (closing a path-traversal hole along the way), plugin auto-update no longer
deletes a cache version a live MCP server is still running from, and a dev-only script
bounds `target/debug` growth. No index rebuild, no schema change.

### Changed
- **Relative path arguments to the CLI now resolve against the current directory** (like
  grep/ls/cat) instead of always against the project root — so `code-graph-mcp deps main.rs`
  works from `src/`. Programmatic callers (hooks, cg-answer) always spawn with cwd == project
  root, so for them this is byte-identical to the previous behavior; only a human running the
  CLI from a subdirectory sees the change. `affected` and the MCP freshness path stay
  root-relative by design (git / schema contracts).

### Fixed
- **Absolute path arguments could escape the project root.** `normalize_user_path`'s
  absolute branch used `Path::strip_prefix`, which matches components and does not collapse
  `..`, so `deps <root>/../../secret` stripped to a remainder that still climbed out and
  leaked an out-of-root file's import/re-export lines — the absolute-path sibling of the
  previously fixed relative `..` traversal. The stripped remainder is now re-validated, with
  a canonicalize fallback for symlinked-but-in-root paths.
- **Plugin auto-update no longer breaks `/mcp` reconnect with `-32000`.** `lifecycle.js`
  pruned old cache versions purely by recency (keep latest N), which could delete the version
  directory a live MCP server was launched from; Claude Code caches that launcher path for
  the session, so a subsequent reconnect failed with `MODULE_NOT_FOUND` (`-32000`). Pruning
  now skips any version still referenced by a running process (it scans process command
  lines, falling back to recency-only where it cannot enumerate) and keeps the latest 5.

### Added
- **`scripts/cap-target-debug.sh`** (dev tooling, not shipped): clears `target/debug` once it
  exceeds `CG_DEBUG_CAP_GB` (default 25 GiB), skipping while a compile is active and never
  touching `target/release`. rust-analyzer plus cargo's lack of stale-fingerprint GC can
  balloon `target/debug` to tens of GB; this bounds it without manual cleanup.

## v0.81.1 — `outcome` hardening: trustworthy field-MRR, correct replay labels

Follow-up to v0.81.0 from a code review of the `outcome` reader. Three correctness fixes
(none crash — each silently skewed a measurement) and two output-completeness gaps. Still
read-only; no index rebuild, no schema change.

### Fixed
- **field-MRR is no longer presented as confident off a single ranked sample.** The
  low-confidence flag was keyed on total cg calls, but the field-MRR denominator is the
  ranked-tool calls only — so a run with plenty of adoption samples but one ranked call
  (e.g. a single `search`) printed `field-MRR 1.00` with no caveat. A separate
  `field_mrr_low_confidence` (ranked N < 20) now gates it, and both the human and JSON
  output show the ranked adopted/total counts.
- **`--emit-labels` no longer mislabels the adopted file.** The adopted rank is the index
  into the *original* result array, but the emitted file list is compacted (items without
  a `file_path` are dropped), so indexing that list by rank could point at the wrong file
  or none. The adopted path is now captured during the adoption scan, and is populated for
  structural (unranked) adoptions too, not just ranked ones.
- **Multi-line Bash `code-graph-mcp` calls are now counted.** Command tokenization
  collapsed newlines, so a `cd …` on one line followed by `code-graph-mcp callgraph …` on
  the next was missed (the binary no longer sat at a command head). Each line is now
  scanned independently.

### Changed
- **`--json` output adds `n_sessions`, `since_days`, `first_ts`, `last_ts`** (the
  transcript window was parsed but never reported); the human view gains a `Window:` line.
- **CLI-via-Bash replay labels now carry the query.** A `code-graph-mcp search "…"` call
  emitted an empty query, so its ranked `--emit-labels` row wasn't usable as a
  (query → adopted-file) pair; the quoted/positional query argument is now recovered.

## v0.81.0 — `outcome`: does retrieval actually get used?

A new read-only `code-graph-mcp outcome` reads your Claude Code session transcripts and
measures whether the code-graph results the model retrieved were *adopted* — a later Read/Edit
landed on a file the tool returned — rather than just counting how often the tools were called.
It also reports a rank-aware field-MRR for the ranked search tools, so a ranking change can be
judged on real adoption instead of a synthetic oracle. Read-only; no index rebuild, no schema change.

### Added
- **`code-graph-mcp outcome [--project <path>] [--since <days>] [--json] [--emit-labels <path>]`.**
  Pairs each model-initiated code-graph call — MCP `tool_use` *and* CLI-via-Bash
  `code-graph-mcp <subcmd>` — with the files it returned, then scores adoption (a subsequent
  Read/Edit on a returned, previously-untouched file) plus a dual field-MRR (adopted-only vs
  all-ranked) for the ranked search tools. `--emit-labels` writes `(query → adopted-file, rank)`
  rows for offline ranking evaluation. Anchors on real `tool_use` only; a small-N guard flags
  results below 20 calls as low-confidence.

## v0.80.3 — test/prod caller accuracy across callgraph, trace, show & get_ast_node

The graph surfaces now trust the AST-level `is_test` flag (not just a name heuristic)
when separating test callers from production, so inline unit tests no longer pollute
"who calls X" or inflate impact risk. No index rebuild.

### Fixed
- **callgraph, trace, `show`, and `get_ast_node` now exclude inline `#[cfg(test)]`
  unit tests from the default production view.** They partitioned callers with a
  name/path heuristic (`test_`-prefix / `tests/` path), so a Rust inline unit test
  with a descriptive snake_case name leaked in as a production caller and inflated
  `--impact` risk — the same inversion the v0.80.0 `impact` fix addressed, on the
  parallel surfaces. They now use the authoritative `is_test` flag with the heuristic
  as a fallback (`get_ast_node`'s impact summary and `show --impact` also route
  through the shared `classify_impact`, gaining caller dedup + test-route exclusion).
  `--include-tests` still shows them. No `INDEX_VERSION` bump — query-path only.
- **The MCP server survives a handler panic.** The stdio request loop wraps message
  handling in `catch_unwind`, so a single tool's panic returns a JSON-RPC internal
  error instead of tearing down the whole session.
- **`ast_search` type / name / return-type filters treat `_` and `%` literally.**
  They built a SQL `LIKE` pattern without escaping, so an underscore in an identifier
  (e.g. `get_node`) matched any character; the filters now escape LIKE wildcards.

### Internal
- Schema drift-guard test now covers the `meta` and `pending_unresolved_calls` tables.
- Pre-commit hook strips `GIT_*` env before `cargo test`, so fixture git tests are
  hermetic (they were silently breaking `.rs` commits under the hook). Added an
  invariant test that a node delete reaps its vector via the delete trigger on both
  the FK-cascade and direct paths — no orphan vectors (a prior audit suspicion,
  disproven).

## v0.80.2 — downgrade-safe index open (no index rebuild)

A version-mismatch index open is now directional: an older binary never wipes an
index built by a newer one. Runtime-only — no `INDEX_VERSION` bump, upgrades still
rebuild as before.

### Fixed
- **An older code-graph binary can no longer wipe a newer index.** The
  `application_id` (`INDEX_VERSION`) check in `Database::open` was symmetric, so a
  stale server on an older binary would `DELETE` an index a current binary had just
  built — and the two then cleared each other on every open, leaving it stuck at 0
  nodes (the version "ping-pong", seen after a plugin update + dev rebuild when an
  old MCP server process lingers). The check is now directional: only an *upgrade*
  (stored `<` binary) wipes-and-rebuilds on an indexer open; a *downgrade* (stored
  `>` binary) leaves the data and `application_id` intact, flags the index stale,
  and warns on indexer/server-startup opens. Readers were already non-destructive.
  A deliberate permanent downgrade still rebuilds via `rm .code-graph/index.db*`.

## v0.80.1 — statusline + release-tooling robustness

Follow-up fixes from the v0.80.0 audit's remaining cluster (no index rebuild).

### Fixed
- **Statusline no longer pins "↻ updating" forever when an auto-update keeps
  failing.** A consecutive-failure counter caps the optimistic state; past the cap
  the statusline surfaces the real status instead of asserting a self-heal that
  isn't happening.
- **"updating" (post-update window) vs "offline" now keys on a stable marker, not
  translatable prose.** The binary's "schema is newer than this binary supports"
  error carries a fixed token the statusline matches (old phrase kept as fallback).
- **The pre-commit version-drift guard now checks every version location** — the 5
  platform npm packages, marketplace `plugins[0].version`, and package.json's
  optionalDependencies pins, not just 4. A partial bump could otherwise ship a
  release npm/marketplace can't resolve.
- **`bump-version.sh` defaults to an embed-model dev rebuild**, so a local bump no
  longer silently produces a no-embed binary that disables vector search.

## v0.80.0 — audit remediation: edge-resolution, impact, path & snapshot hardening

A full-codebase audit (6 parallel reviewers) plus an adversarial pre-landing
review of the fixes. **Bumps INDEX_VERSION (30→31): existing indexes rebuild once
on first use after upgrade.** One default-behavior change — snapshot auto-install
is now opt-in (see Changed).

### Fixed
- **`impact` / `get_ast_node` no longer miscounts inline unit tests as production
  callers.** Inline Rust `#[cfg(test)] mod tests` functions with descriptive
  (non-`test_`) names were counted as prod callers — inverting the risk level
  (e.g. `impact find_cycles` → HIGH / "0 tests affected" when the callers were
  unit tests) and emptying the covering-test suggestion. The authoritative AST
  `is_test` flag now drives the prod/test partition on all impact surfaces.
- **Cross-language phantom structural edges removed.** `imports` / `inherits` /
  `implements` / `exports` / `routes_to` no longer fall through to a global
  all-language name pool: a Rust `use anyhow::Result` could bind to a markdown
  "Result" heading and `require('fs')` to a Rust `fs` symbol — at max confidence,
  so `--min-confidence` couldn't filter them. Edges bind within a language family
  (js/ts/tsx and c/cpp still cross-reference); genuine externals reach the
  `<external>` sentinel.
- **Incremental re-index no longer over-creates cross-file edges.** The Phase-2c
  inbound-edge restore rebound a saved edge to every same-name node in the batch
  (cross-file / cross-language); it now rebinds only to the same-name node in the
  file the edge originally pointed into, matching a full rebuild.
- **CLI path-traversal escape closed.** An absolute path beginning with the
  project root then climbing out via `..` (`<root>/../../etc/passwd`), and the
  `./../…` shortcut, bypassed the relative-path escape check — letting `deps`
  read a file outside the project. All CLI paths now route through one escape guard.

### Changed
- **Snapshot auto-install from a repo's own GitHub release is now opt-in.** Set
  `CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN=1` (or a `CODE_GRAPH_SNAPSHOT_PIN`) to enable
  it. Without it, opening an untrusted repo no longer auto-fetches that repo's
  published code-graph snapshot (which used same-origin TOFU verification and
  could seed a misleading graph). Mirrors the existing `.code-graph.toml` url
  override gate.
- **Plugin activates in monorepo subdirectories.** Launching from a marker-less
  subdirectory (`.git` only at the repo root) now resolves the project root by
  walking up (matching the binary) instead of serving a zero-tool stub.

### CI / internal
- embed-model is now compiled, clippy'd, and unit-tested in pre-merge CI (it was
  never actually built before — the matrix leg built the empty default). Release
  publishes npm only after the model tarball + GitHub Release exist (no
  half-release on a transient model fetch), and the model fetch now retries.
- Hook cooldown / restart-notice temp files moved out of the bare temp dir.

## v0.79.1 — grep: `-t`/`-g`/`-c` now constrain the git-grep supplement

Follow-up to v0.79.0 from a code review.

### Fixed
- **`-t`/`--type`, `-g`/`--glob`, and `-c`/`--count` no longer leak git-tracked
  files the rg walk misses.** ripgrep does not apply `--type`/`--glob` to files
  passed explicitly on the command line, so the git-grep *supplement*
  (tracked-but-gitignored / hidden-tracked files, appended as explicit args) was
  searched/counted even when it didn't match the active filter. The supplement is
  now re-filtered through ripgrep's own `ignore` matchers (rg-identical) before
  being appended. Only affected repos with force-tracked-into-gitignored or
  hidden-tracked files when using `-t`/`-g`/`-c`.
- The unsupported-flag error's "Supported:" list now includes `-c -t -g -M`.

### Changed
- `grep --json` `text` omits the trailing newline (uniform with the
  `-M`/`--max-columns` path); now covered by a test.

## v0.79.0 — grep: scope filters (`-t`/`-g`), count mode (`-c`), line-width cap (`-M`)

Higher-leverage `code-graph-mcp grep` for agent workflows — scope a search, count
matches, and stop one long line from flooding output. Aligns the AST-aware grep
with the filters Claude Code's built-in Grep already exposes, so scoping no longer
means dropping the `→ fn`/`→ class` annotation. Steering (CLAUDE.md managed block,
`.claude/plugin_code_graph_mcp.md`, MCP `instructions`) updated; routing_bench
re-run (context-rich, backend): Recall 22/22, FP-rate 0/10, Overall 100%.

### Added
- **`-t` / `--type <lang>`** — restrict to a ripgrep file type (e.g. `rust`, `py`,
  `ts`, `go`). An unknown type is surfaced as an error (exit 2), not swallowed.
- **`-g` / `--glob <pat>`** — include/exclude by path glob; repeatable; `!`-prefix
  excludes (e.g. `-g '!*test*'`).
- **`-c` / `--count`** — print `file:count` per file. Exhaustive: the per-file
  `--max-count` cap does not apply. `--json` emits `[{"file","count"}]`.
- **`-M` / `--max-columns <N>`** — truncate displayed lines to N characters
  (default **512**; `0` = unlimited). Text output appends ` … [+K chars]`; `--json`
  carries `"line_truncated": <K>`. Keeps a long minified/generated line from
  flooding output (and an agent's context).

### Changed
- The empty-pattern usage hint and `grep --help` now list the new flags.

`code-graph-mcp grep` parity + honesty fixes from a command audit.

### Added
- **`-m` / `-m N` short alias for `--max-count`** — grep and ripgrep both use
  `-m` for the per-file match cap, but the subcommand only accepted the long
  `--max-count`. Attached (`-m2`), separated (`-m 2`), and bundled (`-nm2`) forms
  all work.

### Fixed
- **Unsupported short flags fail with a clear message instead of a cryptic "No
  such file"** — the pattern positional's `allow_hyphen_values` silently bound any
  unknown short flag (`-v`, `-c`, `-o`, `-e`, …) as the search pattern, pushing the
  real pattern into the path list → `rg: No such file or directory: <pattern>`,
  exit 2. Such flags now report `unsupported flag: -X` with the `-- -X` escape hint
  (and still emit `[]` under `--json`). Dash-then-symbol patterns (`->`, `-1`,
  `-.*`) and the `--`-escaped literal form are unaffected.

### Changed
- **`grep --json` marks truncated results** — each match in a file that hit the
  per-file cap now carries `"truncated": true`. The cap warning was previously
  stderr-only, so a `--json` consumer parsing stdout saw silently truncated
  results (the default cap of 100 could drop hundreds of matches). The top-level
  array shape and empty-result `[]` contract are unchanged.

## v0.77.2 — status line cwd-bridge hardening (code-review follow-up)

Defensive hardening of the v0.77.1 stdin-cwd bridge, from a fresh-context code review.

### Fixed
- **`cwdFromStdin` accepts only a non-empty string cwd** — a malformed stdin payload
  (`cwd` as a number/object) was coerced into a bogus env path that resolved nowhere
  and silently blanked the code-graph status-line segment. It now falls back to
  `process.cwd()`.

### Changed
- **Forwarding env var renamed `CLAUDE_STATUSLINE_CWD` → `CODE_GRAPH_STATUSLINE_CWD`**
  so the plugin's internal composite→provider signal doesn't squat on Claude Code's
  `CLAUDE_` namespace. Internal channel only — no user-facing surface or behavior change.

## v0.77.1 — status line tracks Claude Code's working dir, not the spawn's cwd

The code-graph status line vanished whenever the shell sat in a project
subdirectory whose `process.cwd()` didn't resolve to the project root. The gate
now starts from Claude Code's authoritative current dir (forwarded from the stdin
payload), falling back to `process.cwd()` only when that is absent.

### Fixed
- **Status line no longer disappears in project subdirectories** — the composite
  statusline forwards Claude Code's stdin `cwd` / `workspace.current_dir` to every
  provider as `CLAUDE_STATUSLINE_CWD`, and the code-graph segment resolves the
  project root from that instead of the spawned process's `process.cwd()` (which
  need not track the session's working directory). The code-graph provider
  registers with `needsStdin=false`, so it cannot read the stdin payload directly —
  the env bridge is what carries the authoritative cwd to it. Falls back to
  `process.cwd()` for direct invocation, so existing behavior is unchanged when the
  env var is absent.

### Internal
- Removed the orphaned `impact_analysis` MCP tool — it was unadvertised with zero
  post-fold calls across 194 dogfood sessions and no advertised tool delegated to
  it. Full impact stays on the CLI (`impact --json`); compact impact on `get_ast_node
  include_impact`. No advertised MCP surface changed.

## v0.77.0 — `trace` inherits the v0.76 confidence floor

`trace` was the one call-graph surface left at rank-0 show-all when callgraph and
impact gained the default `inferred` confidence floor in v0.76. So a route handler
that made an **ambiguous by-name call** (one name resolving to many same-language
defs — e.g. `.execute()` resolving to every `execute`) splattered every tied edge
into both the recursive call chain and the one-hop downstream list. This completes
that work: `trace` now applies the same floor on both the CLI and MCP surfaces.

### Changed
- **`trace` hides the `ambiguous` by-name fan-out by default** — on both the
  recursive call chain and the one-hop downstream list, on the CLI (`code-graph-mcp
  trace`) and MCP (`get_call_graph` `route_path` mode, plus the legacy
  `trace_http_chain` / `find_http_route` names). A handler's genuine, uniquely-named
  calls are unaffected; only the by-name fan-out is folded.
- **The hidden count is disclosed, never silently dropped**: `ambiguous_edges_hidden`
  in `--json` / MCP responses (including the compressed-chain response), and a
  `(N direct ambiguous by-name edge(s) hidden — use --min-confidence ambiguous to
  show)` line in the human CLI output.
- **Opt out** with `--min-confidence ambiguous` (CLI) / `min_confidence: "ambiguous"`
  (MCP) to restore every edge, or `extracted` for same-file-precise only. The MCP arg
  was already advertised on `get_call_graph`'s schema; `route_path` mode now honors it
  (it was previously parsed-then-dropped on that path). `routes` / `show` are
  unchanged (still rank-0 show-all).

Query-layer only — reads the existing edge `confidence` column; no `INDEX_VERSION`
bump, no reindex.

## v0.76.4 — SQLite variable-cap crash fix (issue #30) + cycle-detection noise

### Fixed
- **Large repos no longer abort indexing with a SQLite variable-cap error
  (issue #30).** Several `IN`/`NOT IN` lookups bound one parameter per id in a
  single clause and could exceed `SQLITE_MAX_VARIABLE_NUMBER` once the id list
  scaled with the repo — the pending-call `source_id` list reaches ~2× the node
  count. This aborted every `ensure_indexed()` sweep, so `incremental-index` and
  all MCP tools (including `semantic_code_search`) failed on big projects. The
  affected lookups are now deduped and chunked under a 500-parameter cap.
- **`cycles` no longer reports a Rust crate's own module tree as circular
  dependencies.** Intra-crate `.rs`↔`.rs` `use` cycles are idiomatic (a crate
  compiles as a unit; Cargo forbids cross-crate cycles), so they are dropped
  before detection; cross-language cycles are kept. On this repo: 4 → 1.

## v0.76.3 — Backfill robustness follow-ups (code review)

Follow-ups to v0.76.2 from a code review. No schema, index-format, or
public-interface change.

### Fixed
- **`code-graph-mcp incremental-index` no longer hangs when a node can't be
  embedded.** Both embedding-backfill loops (the long-lived server's and the CLI's)
  now skip past a node that fails to vectorize instead of re-fetching it forever: the
  CLI loop, which only stopped on an empty result, previously spun indefinitely on a
  single un-embeddable node, and the server loop starved the embeddable nodes queued
  behind it.

### Internal
- FK-recovery detection now matches the full error cause chain (guarded by a test),
  so a future error-wrapping change can't silently bypass the index rebuild.
- Plugin auto-update tests no longer depend on the host having `curl`/`tar`.

## v0.76.2 — Indexing & embedding robustness (audit + code review)

Robustness hardening of the incremental-index / embedding / install-update
subsystem, from a full audit + two rounds of code review. All fixes restore
intended behavior; no schema, index-format, or public-interface change —
existing indexes keep working without a rebuild.

### Fixed
- **Embedding backfill no longer strands at 0% vectors on a fresh install.** The
  periodic backfill driver stopped advancing its "un-embeddable" floor when the
  model was merely still downloading at the first tick (or hit a transient embed
  stall) — which pinned the floor and left the index vector-less until a restart.
  It now keeps re-attempting until the model lands and bounded-retries stalls.
- **Edits made while the model is embedding are no longer lost.** A watcher-
  triggered incremental that was skipped because a background embedding held the
  write path had already consumed the change signal, stranding the change until an
  unrelated edit or a restart. The incremental is now re-armed and runs on the
  next tool call.
- **A killed or interrupted model download can no longer pin a broken cache.**
  Model extraction is now atomic (staging dir + verified rename), and the cache is
  treated as ready only when the tokenizer/config companion files are present — so
  a partial download self-heals (re-downloads) instead of leaving the server
  silently FTS5-only. Orphaned staging dirs from abnormal termination are GC'd.
- **Plugin auto-update no longer repoints at a missing install dir** when the
  plugin copy is skipped (tarball version drift): the installed-plugins/manifest
  repoint is now guarded on the copy actually landing.

## v0.76.1 — Confidence-floor disclosure follow-ups (code review)

Follow-ups to v0.76.0 from a code review. The default floor is unchanged; these
tighten the *disclosure* so the fold is never silently lossy on the risk surface.

### Fixed
- **Impact now discloses folded ambiguous callers across the whole returned
  frontier, not just the seed's direct callers.** A uniquely-named symbol with a
  clean (inferred) direct caller but an ambiguously-named *transitive* caller
  previously folded that caller from the risk count with zero disclosure
  (`ambiguous_callers_excluded` was seed-direct-only → could read 0). It now also
  counts sub-floor edges into the entire returned caller set, so a folded real
  caller never silently under-states risk.
- **`get_ast_node include_impact` now emits `ambiguous_note`** alongside the count,
  matching the `impact` CLI and `impact_analysis` tool (was count-only).
- **`min_confidence` is validated at tool entry on `impact_analysis` and
  `get_ast_node`**, before any index/freshness work, so a bad value errors cleanly
  instead of after a possible reindex (enum-validate-at-entry).

### Changed
- CLI `callgraph`'s hidden-edge line now says "direct ambiguous … edge(s)" (the
  callgraph count is seed-direct), and `--min-confidence ""` is treated as the
  default on the CLI, matching the MCP surface.

## v0.76.0 — Call graph & impact: hide ambiguous by-name fan-out by default

`get_call_graph` / `callgraph` and impact analysis now apply a confidence floor of
`inferred` by default, hiding the `ambiguous` by-name fan-out — the false-positive
class where a method/function name shared by many definitions (e.g. a `.execute()`
call) resolves to *every* same-named def. On a real Python monorepo this class was
~64% of all `calls` edges (one `execute` name absorbed 11,850 phantom edges across
56 definitions), drowning the real call relationships.

### Changed (user-visible default)
- **`callgraph` / `get_call_graph` traversal now follows only `inferred`+`extracted`
  edges by default.** Sub-threshold edges are pruned inside the recursive CTE — before
  they expand — so the depth-N blowup is stopped at the source. The count of hidden
  direct seed edges is disclosed (`ambiguous_edges_hidden` in JSON; `(N ambiguous
  by-name edge(s) hidden …)` in text).
- **`impact` analysis folds ambiguous callers out of the risk count by default**, but
  always discloses the excluded count (`ambiguous_callers_excluded` + a note) so a
  folded real caller never silently under-states risk.

### Opt-out / revert
- CLI: `--min-confidence ambiguous` on `callgraph` / `impact` restores every edge
  (`extracted` = same-file-precise only).
- MCP: `min_confidence: "ambiguous"` on `get_call_graph`, `get_ast_node`
  (`include_impact`), and `impact_analysis`.
- No reindex required — query-layer change; the stored confidence tiers are unchanged,
  so the flag flips behavior back instantly. No `INDEX_VERSION` bump.

### Unchanged
- `show` and other bare caller listings still show all edges.

## v0.75.4 — Periodic backfill hardening (code-review follow-ups)

Follow-ups to v0.75.3's periodic embedding-backfill driver, from a code review.
No behavior change for a healthy single-binary install; these close edge cases
around contention, version skew, and no-embed builds.

### Fixed
- **The driver's un-embedded count now propagates read errors instead of masking
  them as zero.** `count_unembedded_nodes` previously swallowed any query error to
  `0`, so under writer contention (`SQLITE_BUSY`) the driver could read "0 un-embedded",
  reset its residue floor, and on the next tick futilely reload the embedding model. It
  now returns `0` only when the vector table is genuinely absent and propagates real
  errors, so the driver keeps its floor across a transient read glitch.
- **The driver's per-tick count opens the index non-destructively.** It previously
  used the revalidating `open_with_vec`, which can wipe the index during an
  INDEX_VERSION skew window (a downgraded sibling binary) — a standing hazard for a
  60s poller. The count is read-only and sqlite-vec is registered process-globally, so
  `open_nondestructive` is both safer and cheaper.
- **The driver is gated behind the `embed-model` feature at its spawn sites,** matching
  the sibling embedding services, so a `--no-default-features` build never starts an
  idle polling thread.

## v0.75.3 — Periodic embedding backfill for no-tool-call sessions

Fixes the "✓ N nodes … 99% vec, never finishes" symptom in a session that drives
code-graph only through the PreToolUse CLI hooks and never sends an MCP tool call.

### Fixed
- **Embedding backfill now self-drives on a timer, not only on a tool call.** The
  pass that vectors un-embedded nodes previously fired only from the startup-index
  thread, an MCP tool call (`ensure_indexed`), or `rebuild-index`. The file watcher
  and other post-index services are deferred to the first tool call
  (`consume_startup_index_result`), so a pure hook/CLI session — where edited files
  are re-indexed by `ensure_file_indexed` with `model=None` and never embedded —
  left the new nodes stranded below 100% vector coverage until restart. A new
  periodic driver, spawned at startup from `run_startup_tasks` (not the tool-call-
  gated `start_post_index_services`), polls for un-embedded nodes every 60s and
  drains them, so coverage reaches 100% within a minute of an out-of-band edit with
  no tool call required. It tracks the un-embeddable residue as a floor and only
  re-measures it after a backfill it actually ran, so it never spins re-loading the
  model on nodes that can't be embedded. Gated on vector storage only — not the
  lazily-loaded in-process model — since that model stays unloaded in exactly the
  no-tool-call sessions this targets.

## v0.75.2 — Resolver parity: home boundary + indexed-ancestor preference

Code-review follow-ups to v0.75.1's stray-index fix, closing two ways the Rust
and JS resolvers could still disagree.

### Fixed
- **The Rust resolver now stops at `$HOME`, matching the JS one.** Without a home
  bound, a machine where `~` is itself a git repo (`~/.git`) *and* was indexed
  once (`~/.code-graph`) would resolve any non-git project beneath it (e.g.
  `~/proj` with its own index) up to `~`, so the statusline / CLI read `~`'s DB
  instead of the project's. The walk now stops below `$HOME` so an unrelated
  `~/.code-graph` / `~/.git` never poisons a project under it.
- **The Rust resolver prefers the nearest INDEXED ancestor, then a `.git` root,
  matching the JS resolver.** Previously it returned the nearest `.git` ancestor
  even when that dir had no index (showing an empty `✗ 0 nodes`) and did not skip
  a stray subdir index in a non-git-rooted monorepo. Both resolvers now agree on
  every layout (git-rooted, non-git-rooted, submodule, `~`-indexed). The JS reader
  additionally treats a cwd with its own `.git` but no index as `null` (a distinct
  project with nothing to show) rather than escaping to an ancestor index.

`resolve_project_root_from` is split into a `home`-injectable core so the
boundary is unit-tested without mutating the process environment.

## v0.75.1 — Stray nested indexes no longer hijack a monorepo

### Fixed
- **Project-root resolution now skips a STRAY nested `.code-graph` index inside an
  already-indexed repo.** In a monorepo whose subdirs carry their own markers
  (`backend/requirements.txt`, `frontend/package.json`), an older binary could
  create a separate `.code-graph` index in those subdirs. The `priority-1` rule
  ("a cwd-local index wins") then pinned it forever, so every tool — statusline,
  CLI, MCP server — read a *different* database depending on which subdir the
  shell sat in: the statusline appeared to "oscillate" (root 15k nodes / backend
  14k / `✗ 0 nodes` in an empty subdir index), and the subdir tree got needlessly
  re-indexed and re-embedded. `resolve_project_root_from` (Rust) and
  `resolveProjectRoot` (JS) now treat a cwd-local index as authoritative only when
  it is NOT nested under another indexed dir within the same `.git` boundary (a
  real submodule with its own `.git` still keeps its index). The walk stops at the
  `.git` root so an unrelated `~/.code-graph` never poisons a project beneath it.
- **The statusline walks up to the canonical project root** instead of keying on
  the bare `process.cwd()`, and runs `health-check` from that root — so it tracks
  one DB (the project root) from any subdir, with or without a stray relic present.

Existing stray subdir indexes are now inert (ignored), so no manual cleanup is
required; they can be deleted to reclaim disk.

## v0.75.0 — Cross-file call graph in the grep inject

The PostToolUse compound-grep inject now delivers a grepped symbol's cross-file
caller/callee tree instead of re-echoing the grep's own hits.

### Changed
- **`post-grep-inject` prefers the call graph over the grep echo.** A 2026-06-26
  audit of the non-blocking inject (13 delivered events, 0 consumed by the model)
  found the prior payload redundant — it re-stated the hits the model's own grep
  had already returned. When a compound grep targets a single clean identifier the
  hook now runs `code-graph-mcp callgraph <symbol>` and injects the cross-file
  caller/callee tree (the one structural fact a raw grep cannot surface), falling
  back to the AST-aware grep echo only when the symbol has no call edges
  (leaf/absent) or the pattern is not a bare identifier. New `runCallgraphAnswer`
  (`cg-answer.js`) keeps the same bounded, best-effort, `CODE_GRAPH_INTERNAL`-marked
  posture as the sibling answer runners; the inject records `mode:'callgraph'` so
  the conversion funnel can segment call-graph injects from echo injects. Hook
  logic only — no INDEX_VERSION/schema bump.

## v0.74.8 — Bounded embedding backfill

A code review of v0.74.7 caught a latent loop hazard in the now-eager embedding
backfill.

### Fixed
- **The background embedding backfill can no longer spin on an un-embeddable node.**
  `embed_and_store_batch` returns success even when an individual node's inference
  deterministically fails (it drops that node rather than tanking the whole batch),
  so the backfill's "loop until no unembedded nodes remain" could hand the same
  failing node back forever — pinning a CPU at 100% and holding the
  `embedding_in_progress` flag for the rest of the session (which in turn makes
  incremental re-index skip). The loop now tracks vectors actually written and
  stops, with a warning, when a non-empty batch produces none. v0.74.7's eager
  startup backfill is what made this reachable in every session; the loop itself
  predates it. In no-embed builds (`default = []`) the startup backfill is now
  skipped outright rather than attempting a model-load no-op each session.

## v0.74.7 — Background embedding completes without a tool call

A project whose semantic index sat partially embedded — the statusline showing a
stuck `N% vec` that never advanced — turned out to be a server-lifecycle gap, not
a slow or failed embed.

### Fixed
- **The background embedding backfill no longer stalls in a session that issues no
  code-graph tool call.** After the startup index finished, the embedding pass was
  kicked only by the index-result consumer, which runs on an incoming MCP message
  (i.e. a tool call). An "edit-only" session that never queried the graph left the
  freshly-indexed nodes' vectors stranded at whatever a prior search had embedded
  (e.g. a `2% vec` statusline that never moved). The startup-index thread now runs
  the backfill itself, once the index is committed and the indexing flag is
  cleared, so the index embeds to completion on its own — guarded so it never
  double-runs with a search-triggered embed, and a no-op when no embedding model
  is present locally. The file watcher's start remains driven by the first tool
  call (a narrower, self-healing gap: each new session's startup index catches up).

## v0.74.6 — Non-destructive reads + structure-first indexing

Debugging a persistent `code-graph: ↻ updating` / `offline` statusline in a
subdirectory project surfaced a chain: a read-only status poll was *destroying*
the index, and the statusline could not tell an empty index from a dead binary.

### Fixed
- **A read-only consumer no longer wipes the index on an INDEX_VERSION mismatch.**
  `Database::open` cleared all nodes/edges/files whenever the on-disk index was
  built by an older extractor generation — intended to force a rebuild, but it
  fired on *every* writable open, including the statusline's `health-check` poll
  and one-off `grep` / `show`. In a project where no MCP server is running (e.g. a
  subdirectory you only occasionally touch), nothing rebuilt the index afterward,
  so a single status poll left it permanently empty. Reader opens (`CliContext` →
  new `open_nondestructive`) now leave the data intact and report
  `index_version_stale` ("rebuild pending"); only an indexer open
  (`incremental-index` / `reindex` / server startup) performs the clear + rebuild.
- **The statusline distinguishes an empty/unhealthy index from an offline binary.**
  `health-check` exits non-zero on an empty index but still emits its full JSON
  report; the statusline discarded that and showed the alarming `offline` (or
  `↻ updating` during an update window). It now recovers the report from the
  non-zero exit and renders `✗ 0 nodes | 0 files`; `offline` is reserved for a
  binary that genuinely cannot produce a report.

### Added
- **`--no-embed` on `incremental-index` / `reindex` / `rebuild-index`** for a
  fast, query-ready structural index (nodes/edges/FTS) that skips the slow
  embedding pass. AST / grep / callgraph work immediately; vectors backfill in the
  background (the MCP server's embedder fills any node lacking a vector, resumably)
  or on a later run. The default still embeds, so existing behaviour is unchanged.
- **Statusline embedding coverage.** A structurally-complete but partially embedded
  index now reads as `… | 60% vec` / `… | vec pending` so the background vector
  backfill is visible, and a version-stale index shows `… | ↻ rebuilding`.
  `health-check --format json` gains an `index_version_stale` boolean.
- **Background rebuild for dormant projects.** SessionStart's index-freshness check
  now also triggers a detached background `incremental-index` when `health-check`
  reports a version-stale index (previously only git-vs-mtime drift triggered it),
  so a post-upgrade index self-heals even where the MCP server isn't running.

## v0.74.5 — Cycle labelling + meaningful `signature` impact

E2E dogfooding of the analysis commands surfaced two output-correctness bugs.

### Fixed
- **`cycles` / `report` no longer mislabel a large import SCC as an "N-file
  cycle".** A strongly-connected component of 12 files was printed as
  `12-file cycle: cpp.rs → mod.rs → cpp.rs` — the count (SCC size) and the arrow
  path (a 2-file shortest loop) contradicted each other, and `report` did not even
  list the members. A component now reads as `N-file cyclic group (shortest loop:
  …)` with the full member list when its shortest loop visits fewer files than the
  component holds; a genuine N-file loop still reads as `N-file cycle`. JSON output
  (`size` / `files` / `cycle`) is unchanged.
- **`impact --change-type signature` is no longer a silent alias of `behavior`.**
  Risk only branched on `change_type == "remove"`, so a `signature` change — which
  breaks every call site exactly as a removal does — was scored like a behaviour
  change (e.g. LOW for a single-caller symbol). Both `signature` and `remove` are
  now treated as breaking and pin the result to HIGH; `behavior` still scales by
  caller count. Applies to both the CLI and the MCP `get_ast_node` impact path
  (shared `classify_impact`). The default `change_type` is `behavior`, so default
  output is unchanged.

## v0.74.4 — Symmetric uninstall teardown

A sandbox lifecycle E2E (isolated `HOME` + `CLAUDE_CONFIG_DIR`, real project) found
that uninstall was far less automated than install. Install is one SessionStart
(`install()` + auto-adopt); uninstall via Claude Code's `/plugin uninstall` fires no
hook, so the only automated cleanup was the SessionStart settings self-heal — leaving
`~/.cache/code-graph` (the ~40MB binary + state), the `CLAUDE.md` adoption block, and
the `.claude/plugin_code_graph_mcp.md` detail doc behind in every adopted project.

### Fixed
- **SessionStart now fully tears down a genuine uninstall.** When the plugin is gone
  from `installed_plugins.json` (not merely toggled off), the inactive-branch self-heal
  also removes `~/.cache/code-graph` and unadopts the current project (strips its
  `CLAUDE.md` block + detail doc, preserving user content) — symmetric to install's
  auto-adopt. A temporary **disable** (`enabledPlugins[id]=false`) is deliberately left
  untouched so re-enabling doesn't force a binary re-download + re-adopt. Other adopted
  projects self-clean when next opened, or via `code-graph-mcp unadopt`.

### Added
- **`code-graph-mcp uninstall` CLI** — one-shot local teardown: restores the prior
  statusline, strips code-graph hooks from `settings.json`, deletes
  `~/.cache/code-graph`, and unadopts the current project. `--help` is side-effect-free.

## v0.74.3 — Statusline: distinguish "updating" from "offline"

### Fixed
- **Statusline no longer shows a misleading `offline` during the post-update
  binary-download window.** After a plugin update, the npm package version jumps
  immediately but the platform binary is still being fetched in the background
  (`session-init.js` → detached download). During that window `find-binary.js`'s
  version gates reject the stale cached binary and fall through to an older one
  (e.g. a leftover `~/.cargo/bin` install) that can't read the newer DB schema and
  exits with `Database schema version vN is newer than supported vM`. The statusline
  collapsed every health-check failure into `code-graph: offline`, so users saw a
  broken-looking state for minutes until the download finished. It now shows
  `code-graph: ↻ updating` when the failure is a schema-too-old error or an update
  is pending (`~/.cache/code-graph/update-state.json`); genuine failures still show
  `offline`. Display-only change — no index, schema, or CLI/MCP contract impact.

## v0.74.2 — Polyglot extraction fixes (INDEX_VERSION 30)

A dogfood sweep across languages surfaced six real extraction bugs (all `fix:`).
Existing indexes auto-rebuild on upgrade (INDEX_VERSION 28→30).

### Fixed
- **Cross-file call-noise filter is now language-aware.** The
  `CROSS_FILE_CALL_NOISE` skip-list is Rust/collection-stdlib flavored
  (`Vec::insert`, `HashMap::remove`) but was applied to every language, silently
  dropping legitimate method-call edges — JS/TS `db.insert()`/`cache.remove()`/
  `set.contains()` (not ECMAScript builtins) and **all** PHP `$o->method()` calls
  (PHP array ops are global functions, never methods). Those methods were reported
  as orphan dead code and their callers hidden from callgraph/impact/refs. Genuine
  ECMAScript builtins (`push`/`pop`/`get`/`map`...) still drop; Rust/Python/Ruby/
  Java/Kotlin/Swift/C++ unchanged.
- **Express routes with an imported handler now resolve.** `import { getUser }
  from './ctrl'; app.get('/x', getUser)` (the routes-file + controller-file
  layout) produced no `routes_to` edge — the handler was matched only against the
  route file's own nodes, so trace/find_http_route/impact saw no route. Now
  resolved cross-file (generalizes to Go `HandleFunc`).
- **C/C++ types are no longer reported as dead code.** C/C++ extraction emits no
  inheritance/type-reference edges, so every class/struct/enum was orphaned and
  flagged dead (a guaranteed false positive that drowned real findings). Excluded,
  like markdown headings and constructors; genuinely-unused functions still report.
- **`.hh`/`.hxx` C++ headers are now indexed** (`.hxx` pairs with the already-
  supported `.cxx`); previously skipped, leaving their symbols/includes invisible.
- **Dart calls now resolve in all positions.** Calls were only extracted from
  `expression_statement`, dropping `return foo()`, `var x = foo()`, `obj.run()`,
  nested args, and arrow bodies — the majority. Now dispatched on the
  `selector(argument_part)` node.
- **Dart top-level functions are now extracted as symbols** (they parse as a bare
  `function_signature` sibling under `program`, previously matched by no arm).

## v0.74.1 — Post-release review fixups (no behavior change)

### Fixed
- **`maybeAutoAdopt` return shape is now consistent across all paths.** The two
  pre-gate early returns (`CODE_GRAPH_NO_AUTO_ADOPT=1`, non-plugin-mode) now carry
  the `migrated` field like every other path (session-init already defended with a
  `|| {}` fallback, so no behavior changed). Locked by two new assertions.
- Refreshed two stale `session-init.js` comments that still described the old
  MEMORY.md adoption — the project-map / recent-impact injection gates key off the
  CLAUDE.md block as of v0.74.0.

Validated by a real plugin-cache install e2e (fresh install, legacy-user upgrade
with claude-mem-lite coexistence, idempotency, CRLF, unadopt, marker-guard): 36/36.
Full plugin suite 708/708. MCP instructions live-checked (noisy 916B, quiet 167B),
both point to CLAUDE.md.

## v0.74.0 — Steering moved from the auto-memory dir to the project's CLAUDE.md

### Changed
- **The tool-usage steering now installs into the project's `CLAUDE.md`, not the Claude Code
  auto-memory dir.** Pre-v0.74, plugin-mode SessionStart "adopted" a project by writing a
  sentinel block into `~/.claude/projects/<slug>/memory/MEMORY.md` (the claude-mem-lite index)
  plus the full decision table beside it. That dir is **equal weight** to `CLAUDE.md` — not
  higher — so the steering belonged in `CLAUDE.md`, and seeding `MEMORY.md` polluted an index
  meant for the user's own memories. Now `adopt` installs:
  - a concise, sentinel-wrapped **managed block** (`<!-- code-graph-mcp:begin v2 -->` … `:end`)
    into `<project>/CLAUDE.md` — created if missing, injected if present; **only the block is
    managed, your own prose is never touched**. A project-type-tailored trigger table (web →
    HTTP-route tracing, frontend → reference audits) ending in a pointer to the detail doc.
  - the full decision table at `<project>/.claude/plugin_code_graph_mcp.md` (a generated copy,
    opened on demand — **not** auto-loaded each session; safe to gitignore).
- **MCP `instructions` pointer** updated: `CLAUDE.md → .claude/plugin_code_graph_mcp.md`.

### Migration (automatic, zero action)
- On the first post-upgrade SessionStart **per project**, the plugin cleans the legacy
  memory-dir artifacts — strips the old `MEMORY.md` sentinel block (preserving every other
  memory) and deletes the `adopted-by`-marked detail file — then installs the new CLAUDE.md
  scheme. Touches only the current project's memory dir (a single known path; no `~/.claude`
  traversal). Idempotent and guarded: never removes a user file lacking our marker.
- Opt-outs unchanged: `CODE_GRAPH_NO_AUTO_ADOPT=1` (block install), `CODE_GRAPH_NO_TEMPLATE_REFRESH=1`
  (lock manual edits), `code-graph-mcp unadopt` (full reverse — removes block + detail, deletes a
  CLAUDE.md that held only our block, sweeps any legacy remnants).

## v0.73.1 — doctor: dev-mode rebuild preserves the embed-model feature set

### Fixed
- **`doctor` no longer silently downgrades a hybrid dev binary to FTS5-only.** When a stale or
  missing binary triggered the auto-fix **in the source repo (dev mode)**, `doctor` hardcoded
  `cargo build --release --no-default-features` — silently dropping `embed-model` (so semantic
  search degraded to FTS5) and ping-ponging against any manual `cargo build --release --features
  embed-model`. It now probes the existing binary's compiled feature via `health-check --json`
  (`model_available`) and rebuilds to match — `--features embed-model` for a hybrid binary,
  `--no-default-features` for an FTS5 one; when it can't detect (binary missing/broken) it builds
  FTS5 but prints the hybrid command instead of silently choosing. Build timeout raised 5→10min for
  the slower Candle build. **Dev-only — end users are unaffected:** a stale binary triggers
  auto-update to the published hybrid release binary, and a missing one prints install instructions
  (both never hit the local rebuild path).

## v0.73.0 — Compound-grep answer injection + hints that reach the model

### Added
- **`grep` inside a compound command now gets the AST-aware answer too.** The PreToolUse grep
  guard only fires when the command *head* is `grep`/`rg`/`ag`, so the dominant real shape —
  `echo "=== X ===" && grep -rn "Sym" tests/`, `git diff && grep "Sym" src/`, `for s in …; do
  grep "$s" src/` — sailed past it with no inline answer. A new **PostToolUse(Bash) hook**
  (`post-grep-inject`) splits the command, finds the foldable source-grep segment, runs the same
  `code-graph-mcp grep`/`show` the deny path uses, and injects the result **alongside** the grep's
  own output. It is **permission-neutral** (`additionalContext` with no permission decision): it
  never blocks the command and never auto-approves the bundled work (e.g. a `git push` in the same
  line still goes through your normal permission flow). Opt-out: **`CODE_GRAPH_NO_INJECT=1`** (or
  silence all hooks with `CODE_GRAPH_QUIET_HOOKS=1`).

### Fixed
- **Hint / impact output now actually reaches the model.** The PreToolUse grep hint, the read-fanout
  hint (`pre-read-guide`), and the edit-impact summary (`pre-edit-guide`) were written to plain
  stdout on exit 0 — which Claude Code routes to the *debug log only*, never into the model's
  context. They now deliver via `additionalContext` (the same channel the deny reason already used),
  so the read-fanout overview and the pre-edit impact summary are seen on the next model turn instead
  of being silently dropped. The dead, model-invisible grep hint-tier emission was removed.

> **Migration:** behavior-only change, no action required and no re-index (no schema/index-version
> bump). After this upgrade, a Bash command that searches the source tree gets one extra AST-aware
> context block after it runs, and Read/Edit nudges that were previously dark now appear. Opt out of
> the grep injection with `CODE_GRAPH_NO_INJECT=1`; opt out of all code-graph hooks with
> `CODE_GRAPH_QUIET_HOOKS=1`.

## v0.72.0 — Edit-time covering-test targeting

### Added
- **`impact` now surfaces the tests that cover a symbol.** Both impact surfaces — the
  `impact <symbol> --json` CLI subcommand and the `impact_analysis` MCP tool — now include a
  `test_callers` array (`{name, file}`) alongside the existing `tests_affected` count: the actual
  test/bench functions whose execution reaches the symbol, not just how many. Pure query-time over the
  existing reverse call graph — no re-index, no schema change.
- **The edit hook turns that into a runnable test command.** When you edit a function, the PreToolUse
  impact injection (`pre-edit-guide`) now lists the covering tests and — for Rust — a targeted
  `cargo test <names>` to run exactly the tests that exercise your change, instead of guessing a test
  name or running the whole suite. A widely-tested symbol (>6 covering tests) collapses to a count plus
  the suite command; non-Rust projects get the covering-test list without a fabricated command (a wrong
  command is worse than none). The edit injection is disabled, as before, by `CODE_GRAPH_QUIET_HOOKS=1`.

## v0.71.0 — Grep-deny: cover `git grep`

### Fixed
- **`git grep` now folds to the AST-aware equivalent.** The PreToolUse grep guard caught
  `grep`/`rg`/`ag` on the indexed source tree but missed `git grep` — its command head is `git`, so it
  leaked past the matcher and ran as a raw search with no inline answer. `git grep` carries the same
  foldable intent, and `code-graph-mcp grep` is a superset (it finds tracked AND gitignored files), so
  `git grep` on source now denies-with-answer exactly like plain grep. A single shared verb fragment
  drives the command-head, pattern-strip, and pipe-filter matchers so the parse sites stay in sync, and
  the BRE→rust-regex translation covers `git grep` too (it speaks basic-regex like grep). Output-filter
  pipes (`… | git grep X`), multi-file named greps (downgrade to hint, v0.70 parity), and the `--`
  pathspec separator are all handled. Searches whose scope the working-tree answer can't honor —
  `git grep --cached` (staged index) and treeish-scoped greps (`git grep "X" HEAD~3 -- src/`) — are
  deliberately left alone (no deny), since folding them would substitute current-tree hits for a
  different revision. +12 regression tests including an end-to-end deny.

## v0.70.0 — Grep-deny: don't deny what the inline answer can't fully cover

### Fixed
- **Multi-file grep deny → hint.** The deny-with-answer scopes its inline `cg grep` to a single
  path (the first source-prefixed token), so a grep naming ≥2 files (`grep "X" scripts/setup.sh
  hook-shared.mjs`) got an answer covering only the first — an incomplete substitute that rationally
  pushed the model to set `CODE_GRAPH_NO_BLOCK_GREP=1` and bypass the hook for the rest of the session
  (a 2026-06-23 reach audit found this was the dominant real bypass). Such greps now downgrade to a
  HINT (which still nudges) instead of an incomplete deny, so the model's complete grep runs.
  Single-file and directory/recursive greps — which the inline answer fully covers — still deny. The
  path count is scoped to the grep's own segment, so a path in a compound `… | sed … file` tail is not
  mistaken for a second grep target. +7 regression tests.

## v0.69.0 — Grep-deny floor hardening: never deny a non-foldable grep

### Fixed
- **Deny-precision floor.** The PreToolUse grep guard could, in edge cases, deny a grep that
  code-graph has no structural answer for — friction that teaches the model to set
  `CODE_GRAPH_NO_BLOCK_GREP=1` and bypass the hook entirely. Two gaps closed: (a) the non-source
  extension skip-list now covers `.ini/.conf/.xml/.log/.csv` (so `grep "FooBar" src/fixtures/app.log`
  is no longer a deny), and (b) the config-target strip is now global, so a multi-file config grep
  (`grep "X" src/a.json src/b.json`) peels off ALL the data files before the source-path re-check
  instead of only the first (the second's `src/` prefix used to false-match and fire). Precision-only —
  a mixed target that includes a real source file still fires. A 2026-06-23 reach audit found grep
  foldability (~24%) ≈ interception (24%), so not denying the non-foldable ~75% is the lever, not reach
  expansion; +6 regression tests lock pipe / external-dir / data-ext / multi-config / mixed.

## v0.68.0 — Honest adoption metric: hook deliveries no longer counted as model CLI use

### Fixed
- **Phantom CLI conversions.** Two hooks ran the `code-graph-mcp` CLI to build the context they push,
  but without the `CODE_GRAPH_INTERNAL=1` marker that tells `record_cli_use` a run is a *delivery*, not
  a model-initiated query: `user-prompt-context.js` (UserPromptSubmit intent injection —
  impact/callgraph/overview/search) and `session-init.js` (`map --compact` project-map injection).
  Their own injections were logged as `cli/use` events, so `stats` read them back as genuine adoption.
  On a heavy consumer project this showed up as "100 model CLI calls / deny→use 86%" — while
  cross-checking the session transcript found the model itself invoked the CLI **once**; the rest were
  the hooks crediting their own deliveries (same-second bursts trailing each grep deny). Both call sites
  now carry the marker (matching `cg-answer.js` / `pre-edit-guide.js`), so only real model-initiated CLI
  queries count toward the deny→use / `CLI uses` funnel. Adds a `buildRunEnv` helper + regression tests
  (`user-prompt-context.test.js`, `session-init.test.js`).

## v0.67.0 — Hook firing self-test: catch a registered-but-inert hook automatically

### Added
- **Hook firing self-test.** Registration checks prove a hook is *wired* into `settings.json`; they
  don't prove the script actually *runs*. A new self-test spawns each registered hook the way Claude
  Code does (a synthetic tool-call payload, in a throwaway fixture) and confirms it executes —
  catching the "registered but silently inert" class (a broken require-chain, an incompatible Node, a
  corrupt install) that path/string checks can't see. Surfaced three ways: a `Hook firing` line in
  `code-graph-mcp doctor`, a `code-graph-mcp verify-hooks-fire` command, and an automatic once-a-day
  background check at session start that prints a one-line warning (pointing at `doctor`) if any hook
  failed — so a dark hook surfaces on its own instead of being found weeks later. (It proves the
  script runs; only a live session proves Claude Code dispatches to it — see the canary below.)
- **Dispatch canary.** When the edit hook has fired repeatedly in a project but the grep/read hooks
  have recorded nothing, session start now warns that grep/read interception may be dark (the
  subdir-cwd class of bug). Conservative sibling comparison → low false-positive.
- **Static hook-registration gates (CI).** Tests now assert every registered hook script exists on
  disk and parses (`node --check`), and pin the exact matcher surface, so a rename/typo or an
  accidental matcher change can't silently disable a hook.
- **A way to register demand for non–Claude-Code agents.** A new GitHub issue template lets users
  request first-class support for other agents (Cursor / Codex / Gemini / Cline / …). The MCP server
  and CLI already work with any MCP-capable client today.

### Fixed
- **Hook-firing self-test fixture is race-safe.** It uses a unique temp directory rather than a
  shared one, so a concurrent process clearing the shared temp dir can't pull the fixture out from
  under an in-flight check.

## v0.66.0 — Fall-through metric excludes inconclusive follow-ups; recovers the failed v0.65.0 release

> **Note:** v0.65.0's Release workflow failed at the Publish step (a transient crates.io error during
> an unnecessary in-CI rebuild), so v0.65.0 was tagged but never published — v0.64.0 remained the
> latest release. v0.66.0 contains all v0.65.0 changes plus the fixes below and publishes cleanly.
> No user action needed; auto-update moves you straight from v0.64.0 to v0.66.0.

### Fixed
- **The Release Publish job no longer rebuilds the binary.** The `Update versions` step ran
  `sync-versions.js`, whose local "rebuild release binary" convenience ran `cargo build --release`
  inside CI — pointless (the 5 platform binaries are already built by the matrix jobs) and fragile:
  a transient `download of float8 failed` crates.io error failed the entire publish *after* every
  binary had built. The step now sets `SYNC_VERSIONS_SKIP_BUILD=1`; downstream steps use the
  pre-built artifacts + npm packages, never `target/release/`.
- **`code-graph-mcp stats` no longer counts inconclusive follow-ups as fall-through.** A follow-up
  search after an answered deny that itself found nothing (`no-hits` — cg ran the next grep and got
  0 hits) or could not run (`unavailable` — cg CLI down) carries no signal about whether the inline
  answer was sufficient, yet was scored as "answer insufficient". Such follow-ups now go to a
  separate `followup_inconclusive` bucket, excluded from the fall-through rate (same over-count
  class as the v0.64.0 drill-down/observe fix). A verbatim same-pattern re-grep still scores as
  fall-through.

### Added
- **Opt-in `.code-graph/.no-metrics` sentinel.** A development/dogfood checkout that runs the tool's
  own CLI/hooks for functionality testing or simulations can drop this file under `.code-graph/` to
  stop `record_cli_use` (Rust) and `recordRecommendation` (JS) from appending self-generated
  `use`/hook events to its own `recommendations.jsonl` (which otherwise read back as genuine
  adoption). Does not affect `usage.jsonl` (MCP tool metrics still flow). Delete the file to re-enable.

### Internal
- `aggregate_recommendations_jsonl` adds the `followup_inconclusive` counter (no-hits / unavailable
  follow-ups); `stats` text + `--json` surface it so the components of `researched_after_answer`
  stay legible. No index/schema bump (telemetry + output only).

## v0.65.0 — Pattern-fingerprint tightens the fall-through metric (a verbatim re-grep no longer counts as a win)

### Fixed
- **`code-graph-mcp stats` no longer scores a verbatim re-grep of an answered query as "drill-down
  sustained".** v0.64.0 split the follow-up after an answered deny into sustained (cg answered the
  next step too — a win) vs fall-through (cg couldn't satisfy it). But it had no way to tell a
  *deeper* search from the model re-running the *same* grep it was just answered, so a verbatim
  re-grep (the inline answer was ignored) landed in sustained and inflated the win count. The hook
  now records the denied search pattern, and the funnel scores a same-pattern follow-up as
  fall-through ("the inline answer didn't end the hunt for that query"), not sustained. The stats
  line is reworded to "inline answer didn't end the hunt (verbatim re-grep or a search cg couldn't
  satisfy)".

### Added
- **The two PreToolUse grep emit points (`deny` and the cooldown `observe`) now record a `pattern`
  field in `recommendations.jsonl`** — the post-translation search term. Omitted when the grep has
  no identifier-like pattern, so events without a pattern keep the v0.64.0 sustained/observe split
  (back-compatible).

### Internal
- `aggregate_recommendations_jsonl` tracks the armed answered-deny's pattern; a same-pattern
  follow-up takes precedence over the observe/answered split and scores as fall-through. The
  `is_some()` guard keeps absent-pattern events (all pre-v0.65 data) on the old behavior, so the
  fall-through rate on existing data is unchanged until new pattern-tagged events accrue. No
  index/schema bump (the telemetry field is additive; `stats` is output-only).

## v0.64.0 — Honest "fall-through" adoption metric (the re-search rate was over-counting)

### Fixed
- **`code-graph-mcp stats` no longer reports a misleading "re-search after cg answer" rate.**
  The old metric counted every grep/read that followed an answered deny as "kept searching = the
  inline answer failed" — but that lumps in healthy drill-down (the model greps the next related
  symbol and cg answers that one too) and file-reads that act on the delivered answer. On this
  repo's own data that read 61%, while the rate at which cg answered and then genuinely could not
  satisfy the next search was 7%. `stats` now leads with **`Fall-through after cg answer`** (the
  follow-up was a search cg could not satisfy — the only signal that the inline answer was
  insufficient), reports **drill-down sustained** (follow-ups cg also answered — a win, not a
  miss) separately, and labels the old raw "any follow-up" count as "NOT a failure rate".

### Added
- **`stats --json` gains `recommendations.sustained_after_answer`, `.fallthrough_after_answer`, and
  `.fallthrough_rate`.** `fallthrough_rate` is the honest "inline answer insufficient" fraction;
  `re_search_rate` is kept for back-compat but over-counts — read `fallthrough_rate`.

### Internal
- `aggregate_recommendations_jsonl` now splits the follow-up after an answered deny into sustained
  (cg answered it too) / fall-through (cg couldn't) / observe (a file read acting on the answer,
  excluded), instead of counting all three as "re-search". No index/schema bump (output-only).

## v0.63.0 — SessionStart live blast-radius context, edit-impact salience, project-map binary fix

### Added
- **SessionStart now injects the recent-change blast radius from the AST index.** On session
  start / resume, the working-tree changes (staged, unstaged, and untracked) — or, on a clean
  tree, the last commit — are run through `affected`, and a compact summary (files impacted,
  direct dependents, tests to re-run) is surfaced as context. Unlike the opt-in static project
  map, this is git-delta-derived: it changes every session and is not duplicated by `MEMORY.md`,
  so it is on by default for adopted projects. Self-selecting — a commit that touched no indexed
  source (a deps/release bump) or a cold start on a clean tree injects nothing. A high-fanout
  change (a constants/util module that "touches everything") collapses to a risk + test-count
  line instead of a noisy dependent list. Opt out with `CODE_GRAPH_NO_RECENT_IMPACT=1` (or the
  existing `CODE_GRAPH_QUIET_HOOKS=1`).
- **The pre-edit impact hook asks for an explicit per-caller verdict.** When you edit a function
  with callers, the injected impact summary now ends by asking you to confirm each caller still
  holds with the change, or note why it is unaffected — so the blast radius is reconciled against
  the edit rather than skimmed.

### Fixed
- **The SessionStart project map no longer shows "(empty project)" when the index is populated.**
  It invoked the binary by bare name on `PATH`, so a stale/global install pointing at a different
  index returned empty; it now resolves the binary the same way every other hook does.

### Internal
- New `live_impact` metric: the SessionStart blast-radius injection is recorded to
  `recommendations.jsonl` and surfaced by `code-graph-mcp stats`, so its effect (and the model's
  subsequent search fan-out) can be measured rather than assumed.

## v0.62.0 — dead-code false-positive sweep, atomic rebuild, path-traversal fix, script-language call recall

### Fixed
- **`dead-code` no longer reports implicitly-invoked methods as dead.** Constructors
  and magic/dunder methods are dispatched by the language runtime and never carry an
  incoming call edge, so every one was flagged as an orphan — the most damaging false
  positive (it invites deleting a live constructor). Excluded per each language's
  convention across nine language families: Python `__init__`/`__str__`/… (`__x__`),
  PHP `__construct`/`__toString` (`__x`), JS/TS `constructor`, Ruby `initialize`,
  Java/C#/Dart constructors (a function sharing the class name), and C++ constructors
  plus destructors (`~Class`). Normal-name methods (`toString`, `to_s`) stay as candidates.
- **`dead-code` no longer reports Markdown headings (`h1`–`h6`) as dead.** Headings are
  document structure, never callable, so every README heading was flagged. HTML/CSS/JSON
  contribute only an (already-excluded) `<module>` node; their content stays grep-searchable.
- **Security: relative paths that escape the project root are rejected.** `normalize_user_path`
  validated absolute paths but passed relative paths through unchanged, so `deps ../../secret.js`
  read a file *outside* the project root (the barrel-scan `join`s the raw path) and leaked that
  file's import/re-export lines. Relative `..` escapes are now rejected, consistent with the
  existing absolute-path check, across `overview` / `dead-code` / `deps` and every `--file` flag.
- **`rebuild-index` is now atomic.** It removed `index.db` and rebuilt in place, so for the
  whole (multi-second on large repos) rebuild a concurrent reader opening a fresh connection saw
  an empty/partial index and got false "no results". It now builds into a temp file and
  atomically renames it over `index.db` — readers always see a complete index. A failed rebuild
  also no longer destroys the existing index.
- **`--help` / `-h` is side-effect-free for `doctor`, `adopt`, and `unadopt`.** These
  subcommands bypass clap, so `--help` *ran* them instead of printing usage — `doctor --help`
  rewrote `~/.claude/settings.json` and `adopt --help` rewrote `MEMORY.md`. Fixed for both the
  native binary and the npm/npx wrapper, and the previously-undocumented `doctor --check-only`
  diagnose-only mode is now shown in the usage.

### Added
- **Top-level (entry-point) calls are tracked for bash, Python, and Ruby.** A function invoked
  only at script/module top level (`run_app "$@"`, a bare `main_entry()`) was dropped — only
  JS/TS attributed top-level calls — so it had no incoming edge and `dead-code` reported the
  entry point as dead. External commands / undefined callees still drop at Phase-2 resolution,
  so only calls to defined functions add edges.
- **Ruby bare (parens-less) method calls are now extracted.** `helper` (no parens/receiver)
  parses identically to a local-variable read, so idiomatic Ruby produced no call edges. A
  scope-aware pass replicates Ruby's own rule — a bare statement-position name is a method call
  unless it is bound as a local (assignment / parameter / block param / `for` / `rescue`) in the
  enclosing scope — biased to false-negatives so a local never invents a spurious caller.

### Internal
- `INDEX_VERSION` 25 → 28 (bash/Python/Ruby top-level call attribution; Ruby bare calls).
  Existing `.code-graph/` indexes rebuild automatically on first open after upgrade.

## v0.61.0 — grep flag parity, PHP include deps, Flask route methods

### Fixed
- **`grep` accepts grep/ripgrep's attached and bundled context flags** (`-A2`,
  `-C2`, `-nA2`, `-niB3`). The `pattern` positional's `allow_hyphen_values`
  previously bound an attached short value like `-A2` as the search pattern and
  pushed the real pattern into the path list → a cryptic `rg: No such file`
  exit 2 on one of grep's most common forms. Separated `-A 2`, literal
  `--no-default-features` patterns, and unknown-flag errors are unchanged.
- **File counts exclude the synthetic `<external>` pseudo-file.** `health-check`,
  `report`, and the MCP `get_index_status` tool/resource counted the
  unresolved-import bucket as a real file, inflating the reported count by 1
  whenever a project has external imports (e.g. "5 files" for 4 source files).
- **Flask `@app.route('/x', methods=['GET'])` routes carry the declared HTTP
  method.** The verb was read only from the decorator name, so every Flask route
  was recorded as "ANY" and `trace 'GET /x'` (exact-method filter) matched
  nothing. The `methods=` kwarg is now parsed (first verb when several are
  listed; no `methods=` stays "ANY").

### Added
- **PHP `require` / `require_once` / `include` / `include_once` file imports are
  extracted and resolved.** PHP was the only supported language whose
  file-include dependencies produced no edge — `deps` / `cycles` / `affected` /
  `project_map` now see PHP cross-file includes. The include path resolves
  against the importing file's directory to the included file's module node;
  unindexed/vendored paths fall through to `<external>`.

### Internal
- `INDEX_VERSION` 23 → 25 (PHP file-include imports; Flask route method
  metadata). Existing `.code-graph/` indexes rebuild automatically on first open
  after upgrade.

## v0.60.0 — accurate duplicate-route handling + CLI/MCP parity

### Fixed
- **Duplicate inline route handlers for the same METHOD+path in one file now
  resolve to distinct nodes.** Previously two `app.get('/x', …)` registrations in
  one file collapsed onto a single synthetic `GET /x` node, so name-based edge
  resolution cross-linked their calls and fanned `routes_to` into a cartesian
  product — inflating trace/impact/call-graph for those routes. Handler nodes now
  carry a per-occurrence line suffix (`GET /x#Lstart`) so each resolves 1:1.
  Route lookup via `trace` is unaffected (it matches the route metadata path, not
  the node name). Existing `.code-graph/` indexes rebuild automatically on first
  open after upgrade.
- **`impact` route count is now consistent** across the CLI and MCP surfaces, and
  matches the reported risk level even when a route's metadata is malformed.

### Added
- **`impact` reports `value_references` in the CLI too** (callback / function-
  pointer / type-position couplings the call graph misses), matching the MCP tool.
- **`trace --include-tests`**: the call chain now hides test symbols by default
  (matching the MCP `trace` tool); pass `--include-tests` to show them.

### Internal
- Indexer skips the cross-file bind/prune/classify passes on no-op incremental
  updates — no wasted full-graph scans on empty-diff watcher ticks.
- SQLite memory-mapping is pinned off on every connection-open path (the
  read-only secondary and the snapshot producer, not just the primary), and
  adopt/unadopt atomic writes clean up their temp file if a rename fails.

## v0.59.0 — import-aware call resolution (calls bind to the imported definition, not a path-proximity guess)

### Improved
- **Call edges now resolve to the symbol the caller actually imports**, instead
  of guessing the path-closest same-name definition. When several files define
  the same name, an explicit import/require disambiguates the call — cutting
  false callers/impact inflation and dead-code false positives. Four resolution
  paths now consult the import binding:
  - **bare calls bound by an in-file import** (`from x import foo; foo()`)
    resolve to that import's target — insert the correct edge, drop the
    contradicted proximity edge (any language whose imports already resolve);
  - **JS/TS ES imports** (`import { foo } from './rel'`) resolve via the module
    specifier (relative-path resolution + `.ts/.tsx/.js/.jsx/.mjs/.cjs` /
    `/index.*` probing) rather than name proximity;
  - **destructured CommonJS require** (`const { foo } = require('./rel')`)
    resolves each name to the required file's export;
  - **namespace-require member calls** (`const m = require('./rel'); m.foo()`)
    resolve to the required module's export.
  Bare / external / unresolved specifiers fall through to the previous
  name-based behavior. Index format bumped (18 → 22); existing `.code-graph/`
  indexes rebuild automatically on first use.

### Fixed
- **Plugin: atomic writes in `adopt`/`unadopt`** so a concurrent session can't
  observe or clobber a half-written shared `MEMORY.md`.

## v0.58.0 — snapshot integrity pin (CODE_GRAPH_SNAPSHOT_PIN); freshness path-traversal guard

### Security
- **`CODE_GRAPH_SNAPSHOT_PIN` — out-of-band snapshot integrity pin.** Set it to a
  snapshot artifact's blake3 hex digest and the auto-installer verifies the
  download against it instead of the `<url>.blake3` sidecar. Because the pin lives
  in the environment (not in the repo's `.code-graph.toml`), it holds even when
  `CODE_GRAPH_SNAPSHOT_TRUST_URL=1` trusts a custom url — closing the residual gap
  where an attacker who controls the url also controls its sidecar. When set it is
  the sole integrity authority (no network sidecar fetch) and applies to `file://`
  sources too. Unset → behavior is unchanged.
- **`ensure_file_indexed` path-traversal guard** — the query-time freshness leaf
  now refuses an absolute or `..`-escaping `rel_path`, so an MCP `file_path`
  argument forwarded without normalization can't make the server hash/index a
  file outside the project root. Legitimate relative paths are unaffected.

## v0.57.0 — inline route handlers; SIGBUS/flake fixes; CLI/MCP classification unified

### Added
- **Inline route handlers are materialized as nodes** — Express/Fastify/Koa inline
  arrow / function-expression handlers become function nodes named `"METHOD path"`,
  so trace / impact / overview / map give correct per-route results instead of
  collapsing onto the file `<module>` node (INDEX_VERSION 17→18; existing
  `.code-graph` indexes auto-rebuild on next open).
- **Search-decay outcome metrics** — observe logging + a re-search-rate proxy.
- **answer-in-deny observability** — the grep/read deny hooks report a distinct
  `no-binary` status, and `doctor` reports when a missing binary has disabled
  answer-in-deny.

### Changed
- **MCP `impact_analysis` output values corrected** — callers are now deduplicated
  by (name, file, depth) and routes counted from production callers only, so
  test-only endpoints no longer inflate `affected_routes` / `risk_level`. The tool
  schema is unchanged; numeric values may change for affected symbols.
- CLI `search` / `similar` now filter `<external>` stubs, matching the MCP surface.

### Fixed
- **`snapshot_integration` SIGBUS in concurrent install** — `try_install` no longer
  re-opens the shared `index.db` to write meta after the atomic rename; a
  concurrent installer's rename could replace the open WAL database and corrupt the
  `-shm` index (SIGBUS in `walIndexAppend`). Meta is now written to the per-thread
  partial and checkpointed before the rename.
- **SQLite mmap disabled (`mmap_size=0`)** — removes the documented
  mmap-on-truncation SIGBUS hazard (VACUUM/checkpoint shrinking a mapped DB); the
  page cache keeps reads fast at index sizes.
- **`ensure_indexed` spurious-wakeup** — the startup-indexing grace wait now loops
  on an `Instant` deadline, so a spurious condvar wakeup can't make it fall through
  and start indexing instead of staying non-blocking.
- **`CODE_GRAPH_QUIET_HOOKS=1`** now also silences the fresh-install restart notice
  (it leaked on a checkout with no install manifest).
- Audit remediation: `grep -n` parity, binary checksum on download, CLI tracing;
  honest `stats` metric naming.

### Internal
- CI runs the full Node test suite (serially, to avoid a find-binary cache race).
- Migration parity test also diffs column defaults; CLI/MCP dead-code + impact
  classification unified into shared helpers (`domain::is_dead_code_exported`,
  `graph::impact::classify_impact`).

## v0.56.2 — fix: code-explorer sub-agent referenced a folded MCP tool

The shipped `code-explorer` sub-agent (`claude-plugin/agents/code-explorer.md`)
listed `mcp__code-graph__trace_http_chain` in its tool frontmatter and body, but
that tool was folded into `get_call_graph route_path` back in v0.18.4 and no longer
dispatches — so the agent carried a dead tool and was missing four exploration tools
it should have had. Plugin-shell-only fix; no `INDEX_VERSION` / schema change, no
binary change.

- **fix(plugin): drop stale `trace_http_chain` ref from code-explorer agent.** The
  agent now lists exactly the 7 live MCP tools (adds `project_map` / `module_overview`
  / `ast_search` / `find_references`, which its module-architecture remit needs), and
  the body strategy uses `get_call_graph route_path='GET /api/x'` for HTTP flow. Guarded
  by `tests/integration.rs::test_code_explorer_agent_references_only_live_tools`, which
  asserts every `mcp__code-graph__<name>` the agent lists is a live tool in the registry.

## v0.56.1 — fix: chained statusline dropped when the previous command used a leading `~`

The composite statusline runs each provider via `execFileSync` (no shell), so a
`_previous` command captured verbatim with a leading `~` (e.g.
`~/.claude/utils/statusline.sh`) threw `ENOENT` and was silently swallowed — the
user's original statusline vanished, leaving only the `code-graph` line. No
`INDEX_VERSION` / schema change.

- **fix(statusline): expand a leading `~` before exec** (#24). `runProvider` now
  expands `~`/`~/` to the home directory on every command word, mirroring the shell
  tilde expansion Claude Code applies when it runs `statusLine.command` directly.
  The `_previous` command is still stored verbatim (correct for the shell-based
  restore to `settings.json` on uninstall); expansion happens only at exec time, so
  users already affected are fixed on upgrade without re-running install. A regression
  test spawns the composite with a `~`-prefixed provider and asserts it renders.

## v0.56.0 — vector-availability visibility + model/release hardening + platform hints

Install-integrity pass: make a silent FTS5-only degradation visible, harden the
model download + release gate, and turn unsupported-platform install failures into
actionable hints. No `INDEX_VERSION` / schema change.

- **feat(search): surface vector availability.** `semantic_code_search` now reports
  `search_mode`/`vector_available` when the embedding model isn't loaded (the hybrid
  path stays a bare array — unchanged). `health-check` text gains a `Search:` line,
  and `doctor` warns on a vector-inactive index instead of reporting it green.
- **feat(indexer): retry model download.** The background model download retries 3×
  with backoff, so a transient first-session failure no longer strands the whole
  session on FTS5-only.
- **ci(release): vector-integrity gate.** Post-publish smoke now installs the
  published model and asserts it loads + embeds (`search_mode=hybrid`) — a
  missing/corrupt/unloadable model release fails the gate instead of shipping green.
- **feat(plugin): unsupported-platform hints + libc metadata.** Alpine/musl and
  native Windows-on-ARM get an actionable install hint (build-from-source /
  x64-emulation) instead of a misleading suggestion to install a nonexistent
  platform package. The `linux-x64`/`linux-arm64` npm packages declare
  `"libc": ["glibc"]` so npm cleanly skips them on musl.
- **fix(bench): tier3 slice excludes test symbols** (internal) — the "14.6% retrieval
  miss" was a benchmark artifact (test-classifier mismatch), not a pipeline issue.

## v0.55.0 — code-health commands (cycles / surprising / report) + routing

Three new CLI-only analysis commands, with routing-template wiring so Claude Code
auto-routes to them. All read-time over the existing graph; no `INDEX_VERSION` /
schema change.

- **feat(cli) `cycles`**: detect circular import dependencies — strongly-connected
  components of the file-level `imports` graph, with a representative shortest loop.
  Imports-only (a `calls` cycle is recursion, not a smell). Most actionable for
  JS/TS/Python/Go; Rust intra-crate module cycles are often benign.
- **feat(cli) `surprising`**: surface unexpected cross-module couplings — cross-file
  `calls`/`references` ranked by resolution confidence (ambiguous > inferred >
  extracted, reusing v0.54 `edges.confidence`) + crosses-modules + sole-bridge.
- **feat(cli) `report`**: consolidated code-health overview — summary (counts +
  edge-confidence breakdown) + hot functions + betweenness chokepoints + import
  cycles + surprising connections + dead code. `--json` emits an object envelope.
- **docs(adoption)**: the shipped routing template
  (`claude-plugin/templates/plugin_code_graph_mcp.md`) lists all three in its CLI
  decision table + cheat-sheet (CLI-only, like `centrality`). Adopted project
  copies refresh from the template on next SessionStart.

## v0.54.2 — routing template: centrality + refs --min-confidence

- **docs(adoption)**: the shipped routing template (`claude-plugin/templates/plugin_code_graph_mcp.md`)
  now lists `centrality` (architectural chokepoints, CLI-only) and
  `refs --min-confidence` (filter low-confidence by-name edges) in its decision
  tables + CLI cheat-sheet, so Claude Code auto-routes to the v0.53/v0.54 commands.
  Adopted project copies refresh from the template on next SessionStart.

## v0.54.1 — edge confidence polish (post-review follow-ups)

Two non-blocking items from the v0.54.0 integration review. No schema change,
no `INDEX_VERSION` bump.

- **fix(cli)**: `refs` dedup keeps the **lowest** confidence among collapsed
  siblings. The dedup key is `(name, file_path, relation)` — two edges from one
  source to different same-name targets collapse to one row; the displayed
  confidence now shows the most conservative tier instead of an arbitrary
  first-wins value, so it can't understate a hidden sibling's ambiguity.
- **perf(indexer)**: Phase 2e (`classify_edge_confidence`) is now skipped when an
  index pass indexed and deleted nothing — the full-graph classification UPDATE
  was a guaranteed no-op there (e.g. query-time freshness checks where the file
  hash matched). It still runs globally whenever anything changed, since a
  duplicate-named node added/removed in one file flips cross-file edge ambiguity.

## v0.54.0 — edge confidence tiers

Edges now carry a **resolution confidence** so consumers can tell precise edges
from heuristic by-name guesses. **Schema migration** — `INDEX_VERSION 16→17`
forces a one-time index rebuild on first run.

### New: `edges.confidence` (extracted | inferred | ambiguous)

- A single post-resolution classification pass (`classify_edge_confidence`,
  Phase 2e) assigns confidence — **not** threaded through the ~10 insert sites.
  Purely additive metadata: **no edge is added or removed**, no default behavior
  change to `impact`/`dead-code`/`affected`.
  - `extracted`: same-file resolution, or a structural relation (imports /
    inherits / implements / routes_to / exports). Precise.
  - `inferred`: a cross-file `calls`/`references` edge resolved by bare name,
    target name unique among same-language nodes.
  - `ambiguous`: cross-file by-name where >1 same-language node shares the name
    — the class behind known false positives (bare-name calls, method-call drops,
    value-reference flood).
- The classification is idempotent and recomputed every index pass (an `inferred`
  edge becomes `ambiguous` when a duplicate-named sibling is later added, and back).

### `refs` exposes + filters by confidence

- `code-graph-mcp refs <symbol>` annotates non-`extracted` refs (`~inferred` /
  `~ambiguous`) and accepts `--min-confidence extracted|inferred|ambiguous` to
  hide lower-confidence edges (default: show all). JSON gains a `confidence` field.
- (MCP `find_references` output is unchanged in this release — CLI-first; a future
  release may surface confidence on the published tool schema.)

## v0.53.0 — `centrality`: architectural chokepoints

New CLI command that ranks functions by **betweenness centrality** over the call graph.
Read-only over the existing index — no `INDEX_VERSION` bump, no breaking changes.

### New: `code-graph-mcp centrality [--limit N] [--include-tests] [--json]`

- Surfaces the *structural bridges* — functions lying on the most shortest call paths
  between other functions. This is orthogonal to `map`'s `caller_count` "hot functions"
  (degree centrality): a chokepoint can have **few callers** yet route most cross-cluster
  traffic. (On this repo, `walk_for_relations` has 1 caller but ranks #3 by betweenness.)
- Exact **Brandes' algorithm** (O(V·E)) over the directed `calls` edge set; directed
  scores are not halved. Reports raw betweenness, a size-normalized `(n-1)(n-2)` figure,
  and `caller_count` side-by-side. Top `--limit` (default 15).
- Test symbols are excluded from the graph entirely (endpoints **and** intermediate hops)
  unless `--include-tests`, reusing the canonical `is_test_symbol` classifier rather than
  a parallel SQL heuristic.
- **CLI-only by design** — the model reaches it via Bash; no new MCP tool, keeping the
  LLM-visible instructions budget and tool-routing surface unchanged. The algorithm core
  lives in `src/graph/centrality.rs` (pure, unit-tested).

### New: PR impact review GitHub Action

- `.github/workflows/pr-impact-review.yml` posts a **sticky comment** on each PR with a
  code-graph `affected` analysis: test files to re-run, the blast radius, and changed
  **production files with no covering test** ("test gaps"). Productizes the already-shipped
  `affected` command — no new graph logic.
- Comment rendering + the gh sticky-upsert (find-by-marker → PATCH-or-POST) live in
  `claude-plugin/scripts/pr-impact-comment.js`; the render path is pure and unit-tested
  (`pr-impact-comment.test.js`, in the CI plugin-test set). Binary calls carry
  `CODE_GRAPH_INTERNAL=1` so CI runs never inflate the deny→use conversion funnel.
- **Opt-in merge gate**: repo variable `CODE_GRAPH_FAIL_ON_RISK=true` fails the job when a
  changed production file has no test in its reverse-dependency closure (default: off).
- Minimal permissions (`contents: read`, `pull-requests: write`); builds `--no-default-features`
  (impact analysis needs no embeddings), reusing the pinned `1.95.0` toolchain + rust-cache.

## v0.52.0 — `tour`: dependency-ordered reading order

New CLI command that answers "where do I start reading this repo?" deterministically.
Read-only over the existing index — no `INDEX_VERSION` bump, no breaking changes.

### New: `code-graph-mcp tour [PATH] [--json]`

- Lists modules in dependency order — prerequisites before the modules that build on
  them — via a Kahn topological sort over the project-map import edges. Each module is
  annotated with a role (`entry` / `foundational` / `core` / `mid`), its depended-on-by
  count, its in-scope imports, and key symbols. Optional `PATH` scopes the tour to a
  subtree; `--json` emits a `{"reading_order": [...]}` envelope.
- **Cycle-tolerant**: import cycles are broken deterministically (smallest unresolved
  in-degree, then lexical order) and flagged, so the output is stable for a fixed index.
- **CLI-only by design** — the model reaches it via Bash; no new MCP tool, keeping the
  LLM-visible instructions budget and tool-routing surface unchanged.
- Reuses the existing project-map graph (`get_project_map`); the ordering core lives in
  `src/graph/reading_order.rs` (pure, unit-tested).

## v0.51.1 — `affected` correctness fixes (post-release code review)

Fix-forward for defects found by a max-effort code review of v0.51.0's `affected`
command. No breaking changes; no `INDEX_VERSION` bump.

### `affected` correctness

- **fix(cli)**: the reverse-dependency closure now walks **all** dependency relations
  (imports ∪ calls ∪ references ∪ implements ∪ inherits) via a new
  `get_reverse_dependents`, not just imports ∪ calls. Test files that only `reference`
  (or implement/inherit) a changed symbol were silently dropped from the "tests to
  re-run" set — e.g. `affected src/domain.rs` went from 11 → 15 test files.
- **fix(cli)**: apply the cross-language compatibility filter (now shared by `affected`,
  `deps`, and the dependency-graph tool) so bare-name resolution false positives in a
  different language no longer leak into the blast radius.
- **fix(cli)**: `affected_files` is disjoint from `changed`; inputs normalizing to `""`
  (`.`/root) are skipped; a nonexistent test-path input is reported in `not_indexed`
  only (never both `not_indexed` and `tests`); `not_indexed` reports the raw input
  consistently; `--stdin` reads bytes lossily so a non-UTF-8 path can't break `--json`.
- **fix(domain)**: `is_test_path` now matches `.spec.tsx`/`.spec.jsx`.

### Honesty / consistency

- **fix(health)**: the text `health-check` prints the `Resolution:` line even on an
  unhealthy index, matching the unconditional `--json` block.
- **docs**: corrected the `is_test_path` "single source of truth" overclaim (the SQL
  filters and a `resolve.rs` closure are intentionally divergent copies) and the
  resolution-block relation list (it counts every relation, not just calls/imports/references).

### Known backlog (not in this release)

- `resolution_stats` runs a `GROUP BY` over edges on every `health-check` (perf on huge repos).
- a negative `--depth` clamps to 1 + exits 0 (consistent with `impact`'s flag form,
  inconsistent with the positional-numeric exit-2 audit convention).

## v0.51.0 — `affected` command (changed files → impacted tests) + resolution-coverage metric

Additive minor release. No breaking changes. No `INDEX_VERSION` bump (both features
read existing edge data; no extraction change). Adopted from a competitive scan of
[colbymchenry/codegraph](https://github.com/colbymchenry/codegraph).

### `affected` — reverse-impact test selection

- **feat(cli)**: new `code-graph-mcp affected [files...]`. Given changed files
  (positional or `--stdin` from `git diff --name-only`), it walks the reverse
  import ∪ call closure and reports the **test files to re-run** (primary) plus the
  full affected-file blast radius with depths (secondary). Flags: `--stdin`,
  `--depth N` (default 10), `--json`. Reuses the existing `get_import_tree(incoming)`
  traversal, inheriting its cycle-guarding and import-only-dependent coverage. Inputs
  go through `normalize_user_path`; unindexed inputs are reported in `not_indexed`
  (the `--json` envelope is same-shape on every path). Registered as an adoption-funnel
  conversion. Example: `git diff --name-only | code-graph-mcp affected --stdin`.

### Graph resolution-coverage metric

- **feat(health)**: `health-check --json` gains a `resolution` block —
  `pending_unresolved_calls` + per-language edge counts grouped by relation (calls,
  imports, references, implements, inherits, …), a pure aggregate over existing edges
  via a single `GROUP BY`. The text output gains
  a `Resolution:` summary line. Makes silent edge-resolution regressions observable
  instead of human-discovered after shipping.

### Internal

- **refactor(domain)**: extracted `is_test_path` (file-level test predicate) from
  `is_test_symbol`, which now delegates — single source of truth, behavior preserved.
- **test(coverage)**: new `tests/edge_coverage.rs` — multi-language (TS/Python/Rust)
  per-language edge-count baseline + a file-scoped method→sibling-method resolution
  invariant (guards the `method_call_edge_drops` class fixed at `INDEX_VERSION` 16).

## v0.50.0 — grep parity (BREAKING: exit codes) + auto-update chain audit

**Migration note (breaking)**: `code-graph-mcp grep` now uses grep-compatible exit
codes — **0 = matched, 1 = no match, 2 = error/usage** (previously: 0 on no-match,
1 on most errors). Scripts that treated any nonzero exit as failure must distinguish
1 (no match) from 2 (trouble). Revert path: pin the previous version
(`npm i @sdsrs/code-graph@0.49.0` / `cargo install code-graph-mcp --version 0.49.0`).
The bundled hook consumer (`cg-answer.js`) is updated in the same release and accepts
both old and new shapes.

### Auto-update chain audit (plugin shell / marketplace clone / binary / model)

- **fix(plugin)**: stale-relic downgrade war — a still-running Claude Code process
  fires SessionStart from the plugin-cache dir it loaded at startup; after
  auto-update installed vN+1, the vN scripts' `syncLifecycleConfig` saw
  `manifest.version !== currentVersion` and (direction-blind) called `update()`,
  dragging the install manifest and all six settings.json hook/statusline paths
  back to vN — upgrade↔downgrade ping-pong until the next auto-update re-ran
  (observed live 2026-06-12→13: manifest 0.49.0 → 0.48.0 fifteen minutes after a
  successful update; settings re-downgraded again after doctor re-registered).
  `syncLifecycleConfig` now defers to `installed_plugins.json` as the authority:
  a script running from a cache dir that is NOT the active installPath returns
  `deferred-to-active-install` and touches nothing. Deliberate downgrades via
  /plugin keep self-heal rights (installPath then IS the old dir); dev checkouts
  and npm installs are exempt. (`isStaleRelicContext` in lifecycle.js +
  install-e2e §1.11.) A relic SessionStart also skips the auto-adopt template
  refresh (it would "refresh" MEMORY.md back to the OLD shipped table), and
  doctor's settings-writing repairs (`hooks-invalid` / `missing-hooks-in-settings`)
  get the same guard with a redirect to the active copy (`relicRepairGuard`).
- **fix(plugin)**: marketplace clone staleness — auto-update wrote the plugin
  cache + installed_plugins.json but never touched
  `~/.claude/plugins/marketplaces/code-graph-mcp`, so its marketplace.json stayed
  at whatever version the last manual /plugin command saw (observed: 0.48.0 four
  days after 0.49.0 shipped), making the /plugin UI lie and letting Claude Code
  reinstall old plugin files from it. After a successful shell update, auto-update
  now fast-forwards the clone (`git pull --ff-only`, silent no-op on dirty/
  diverged/missing-git). State gains `marketplaceRefreshed`.
- **fix(embedding)**: model cache was existence-only and unverified — the
  embedding model (`models.tar.gz` → `~/.cache/code-graph/models/`) downloaded
  once and was never version-checked (same fault class as the v0.45.x native-
  binary pin) nor integrity-checked (the published `.sha256` only self-validates
  the bundle). The binary now pins the expected `model.safetensors` content
  (`MODEL_CONTENT_BLAKE3`); downloads verify-or-reject (mismatched weights are
  deleted, not loaded), the cache check is identity-aware (`.model-id` marker,
  one-time hash migration for pre-existing caches), and a future model change
  re-downloads automatically instead of pinning forever. Offline/stale caches
  still load (graceful degradation unchanged).
- **ci(release)**: the "Package model files" step now `sha256sum -c` pins all
  three HF files against known-good hashes of the pinned revision, so a
  compromised HF response or a revision bump without updating the client-side
  `MODEL_CONTENT_BLAKE3` fails the release instead of shipping mismatched weights.

- **fix**: leading-dash patterns work — `grep "--no-default-features"` no longer
  parses as a flag (clap `allow_hyphen_values` + `--` separator before the rg pattern).
- **fix**: per-file truncation is surfaced — files hitting the match cap are listed on
  stderr; new `--max-count N` flag controls the cap (`0` = unlimited, default 100).
- **fix**: repo-wide searches now include tracked-but-gitignored files (`git ls-files
  -ci` supplement) — `git grep` semantics; previously a `docs/` ignore rule silently
  hid git-tracked docs. Untracked ignored files stay skipped.
- **fix**: `... | head` no longer prints `Error: Broken pipe (os error 32)` — EPIPE
  ends output silently with exit 0, like grep.
- **feat**: `-i/--ignore-case`, `-w/--word-regexp`, `-F/--fixed-strings` (literal
  search — regex-hostile patterns like `res.json(` work with `-F`), multiple path
  arguments.
- **feat**: `-l/--files-with-matches` (bare paths; `--json` → array of strings) and
  `-A/-B/-C N` context lines (grep-style `:` match / `-` context separators with
  `--` between groups; AST annotation stays on match lines only; `--json` context
  entries carry `"context": true`).
- **perf**: `--json` mode reuses the per-file AST-node cache (was one DB query per
  match).
- **fix(plugin)**: grep denies on compound commands (`grep …; sed -n 1,60p f` /
  `grep … && wc`) now flag the unanswered `;`/`&&` tail with a verbatim re-issue
  line — on all three deny paths (grep-answer, show-answer, and the static
  fallback). Previously the whole command was blocked but the deny copy ("use
  these results directly instead of re-running") silently swallowed the tail's
  intent (2026-06-13 mem-project deny dropped a `sed` read of the file's first
  60 lines). `||` tails stay unflagged: with hits delivered, the on-failure
  branch would not have run anyway. Quote-aware — separators inside pattern
  quotes don't trigger. Deny records in `recommendations.jsonl` gain `tail: true`
  when a note was carried, so the funnel can segment re-issue behavior
  (`jq 'select(.tail)'` until `stats` breaks it out).
- **feat**: query-time freshness for AST annotations — before annotating, each
  matched file is hash-compared and lazily re-indexed when dirty (parity with the
  MCP tools' `ensure_file_indexed`), bounded by a sync budget
  (`CODE_GRAPH_GREP_SYNC_BUDGET`, default 8 files) and a 250ms SQLite busy
  timeout; beyond budget or under write contention annotations carry `[stale]`
  (JSON: `"stale": true`) with a stderr hint. Cost: +6.7ms on a repo-wide search
  (29.6→36.3ms avg).

## v0.49.0 — feat: answer-delivery upgrade — every guidance surface delivers results, and the funnel finally sees failures

First valid funnel reading (daagu, 6 sessions / ~3.5h real coding, 2026-06-12: 53 hook
events, 0 MCP calls, 3 post-deny CLI conversions) showed which levers work: delivered
answers satisfy in place (5/5), advice converts ~0 (0/40 hints), and the only conversions
were CLI invocations seconds after a deny. This release rebuilds around that evidence.

**What changes for users** (migration: nothing to do; opt-outs below):

- **Deny tier broadened, intent-aware** (`pre-grep-guide.js`): declaration-anchor greps
  with context flags (`rg "def X|class Y" -A 25`) are denied WITH the symbol bodies via
  `code-graph-mcp show`; `-l` / `--include` symbol greps are denied with the grep answer.
  `-L` / `-v` / `--exclude*` (intents the answer can't honor) stay hint. Replay on the
  daagu night: deny-class coverage 20 → 35 of 128 head-greps, hint 28 → 13.
- **Deny copy never mentions the escape env again** — the v0.48 "THIS command only"
  scoping was adopted as a permanent prefix within 8 seconds and reused 11× that night.
  `CODE_GRAPH_NO_BLOCK_GREP=1` still works; it is documented here, not taught in-band.
- **BRE→rust-regex bridge**: plain-grep patterns (`a\|b`) are unescaped before the
  answer runs — both `answered:false` denies that night were dialect/path-shape misses.
- **Read-fanout hint now DELIVERS the module overview** (`pre-read-guide.js`): on the
  5th same-dir read the hint embeds `code-graph-mcp overview <dir>` output. The read
  hook also gets the v0.48 subdir-cwd fix it missed (it had recorded ZERO events in
  daagu history) plus fd-0 stdin. `sed -n X,Yp` source reads now count toward fanout.
- **Edit→impact injection un-darkened** (`pre-edit-guide.js`): same subdir-cwd + stdin
  fixes (daagu: 115 edits, zero impact injections); impact CLI call uses the resolved
  binary; injections are recorded to the funnel.
- **Funnel instrument fixes (Rust)**: 0-tool-call sessions with in-window hook traffic
  now flush a usage record — previously the deny→use denominator could only contain
  converted sessions, making 0% conversion structurally unobservable. Model-initiated
  CLI queries record `{hook:"cli",action:"use"}` (hook-internal runs carry
  `CODE_GRAPH_INTERNAL=1` and are excluded). `stats` segments denies answered/static,
  reports CLI uses, and funnel conversion is any-use (mcp OR cli) with separable legs.
- **Guidance surfaces lead with the CLI** (MCP `instructions`, adopt index line,
  decision-table template): in Claude Code the MCP tools are deferred behind ToolSearch
  while Bash is always live. Adopted projects realign at next SessionStart.

**Opt-out / revert**: `CODE_GRAPH_QUIET_HOOKS=1` silences hooks; `CODE_GRAPH_NO_BLOCK_GREP=1`
downgrades block→hint per command (still measured); `CODE_GRAPH_NO_ANSWER_IN_DENY=1`
restores advice-only denies AND advice-only read hints; or pin `@sdsrs/code-graph@0.48.0`.
usage.jsonl / recommendations.jsonl shapes are additive — no schema bump, no index rebuild.

## v0.48.0 — fix: grep guard survives `cd` into subdirs; deny stops teaching its own bypass

Field reading of the v0.46 deny→use funnel on a real consumer project (daagu, 4 sessions /
~3h real coding, 2026-06-11) found the instrument itself mostly dark: of ~98 grep-bearing
commands the hook engaged with only 3. Four independent causes, all fixed:

- **Subdir-cwd dark (38/40 head-greps)**: the hook's `process.cwd()` follows the
  persistent shell, so after the model ran `cd backend/`, the `.code-graph/index.db`
  gate failed silently for the rest of the session. Now `resolveProjectRoot` walks up
  to the nearest ancestor holding the index (stops at `$HOME`), and bare
  subdir-relative path args (`app --include=*.py` from `backend/`) are rebased onto
  the project root with existence-verified probing. Recommendations are always
  recorded at the root; no `.code-graph` is ever created in subdirs.
- **Deny no longer advertises its own escape**: the answered deny used to end with
  "re-run with `CODE_GRAPH_NO_BLOCK_GREP=1` prepended" — one deny taught the model a
  permanent prefix within 5 seconds (14 subsequent greps that night, including symbol
  searches). The answered deny now carries no escape line (the results are already
  there); the static deny scopes it to "THIS command only — a per-command escape,
  not a default prefix".
- **Bypassed greps are now visible to the funnel**: `CODE_GRAPH_NO_BLOCK_GREP=1 grep …`
  (bare KEY=VALUE prefix, exactly what the deny taught) failed the command-head regex
  and was invisible — not even recorded. The head regex now accepts bare assignment
  prefixes and a bypassed source-grep records `{action:"bypass"}` then stays silent.
- **Glob search paths no longer break answer-in-the-deny**: the hook spawns the CLI
  without a shell, so a literal `backend/…/llm_engine/*.py` reached ripgrep as a
  nonexistent file → exit 1 → static deny with `answered:false` (the night's only
  deny failed exactly this way — on a query that should have been the honest
  no-hits→allow). Glob segments are now truncated (`…/llm_engine/*.py` →
  `…/llm_engine`) before both the run and the displayed command.

**Opt-out / revert**: unchanged — `CODE_GRAPH_QUIET_HOOKS=1` silences the hook,
`CODE_GRAPH_NO_BLOCK_GREP=1` downgrades block→hint (now per-command and measured),
`CODE_GRAPH_NO_ANSWER_IN_DENY=1` restores the static deny, or pin `@sdsrs/code-graph@0.47.1`.
Rust/SQLite surface untouched (the stats aggregator accepts the new `bypass` action
generically); no index rebuild needed.

## v0.47.1 — fix: grep guard now matches absolute paths (it was missing ~97% of real traffic)

The deny/hint tier of `pre-grep-guide.js` only matched **relative** source paths
(`grep -rn "X" backend/app/…`), but Claude Code's harness explicitly steers Bash toward
**absolute** paths — so `grep -rn "X" /abs/project/backend/app/…`, the dominant real-world
shape, never fired. Field replay (daagu, 3 real coding sessions, 2026-06-11): 42/42 raw
greps used absolute paths → 1 hint / 0 blocks as-is vs **30 hints / 16 blocks** after this
fix. The v0.47.0 answer-in-the-deny feature was unreachable on consumer projects until now.

- Fix: strip `<cwd>/` (the hook's cwd IS the project root) from the command before
  matching — absolute paths under the root now behave exactly like their relative
  spelling; paths outside the project still never fire (conservative edge preserved).
  The inline answer's CLI scope argument is passed in relative form.
- Replay methodology added to tests: real transcript commands asserted in both spellings.
- Known limitation (pre-existing, unchanged): the CLI `grep` shells to ripgrep, whose
  gitignore handling can diverge from git on `dir/` + `!negation` whitelists (observed:
  rg 14.1.0 prunes a git-whitelisted directory when walking from above). Worst case is
  the honest no-hits fallthrough: the raw grep is allowed through and finds the truth.

## v0.47.0 — feat: answer in the deny — denied greps now return the actual results

**What changes for users**: when the PreToolUse hook denies a symbol-shaped raw grep, the
deny message now CONTAINS the results of the AST-aware equivalent (`code-graph-mcp grep
"<pattern>" [path]`, run synchronously inside the hook, ~20ms warm / 2s timeout) instead of
only suggesting the command. Rationale: measured recommend→use transfer of suggestion-style
interventions is ~0% — the model rarely initiates a new tool call because a message told it
to, but it will use results already in front of it.
**Opt-out / revert**: `CODE_GRAPH_NO_ANSWER_IN_DENY=1` restores the v0.46 static deny;
`CODE_GRAPH_NO_BLOCK_GREP=1` still downgrades the whole block tier to hint.

- **Three deny outcomes** (new `cg-answer.js`, all failure modes degrade, never break the
  tool call): ≥1 hit → deny with embedded results (truncated at line boundary to ≤4KB);
  CLI missing/error/timeout → v0.46 static deny; **0 hits → the raw grep is ALLOWED** with a
  one-line FYI (regex-dialect differences — BRE `\|` vs ripgrep — mean 0 hits is not proof
  of absence, so a hard deny could mislead).
- **Funnel semantics**: deny records gain `answered: true|false`; no-hit fallthroughs record
  `{action:"hint", fallthrough:"no-hits"}`. Rust readers ignore the extra fields (verified:
  CLI `grep` does not write `usage.jsonl`, so hook-initiated runs cannot inflate deny→use).
  **Reading note**: an answered deny satisfies the need in-place, so `Deny→use` will read
  LOW even when this feature works — segment by `answered` when reading Piece 3.
- **Hook stdin hardening**: hooks now read fd 0 directly instead of `/dev/stdin` (the path
  form fails silently when stdin is a socketpair, e.g. under `spawnSync({input})` test
  harnesses; real Claude Code pipes were unaffected).

## v0.46.0 — feat: measure whether the DENY stick converts + honest conversion metric

The recommend→use conversion metric (v0.39.0) was producing **zero usable data** in this repo
and failing silently. This release makes it honest and adds the first real attribution of the
PreToolUse **DENY** intervention — without adding or broadening any intervention itself.

- **Honest, visible metric.** `stats` (text + JSON `recommendations.state`) and `health-check`
  now surface `absent | empty | live` instead of silently skipping the block — "hooks not
  recording here" can no longer be mistaken for "feature absent".
- **Test-leak fixed.** `resolve_project_root_from` walks up to the nearest ancestor `.git`;
  `plugin_e2e::spawn_server` wrote only a `Cargo.toml`, so a fixture nested under the repo
  flushed test metrics (`nonexistent_tool`, `dur_s:0`) into the **real** `usage.jsonl`,
  corrupting the conversion denominator. Fixed by test isolation (a `.git` marker) — prod
  resolution is unchanged (subdir-CLI behavior is intended). Regression-tested.
- **Per-session deny→use funnel.** `SessionMetrics` stamps a wallclock start; flush
  window-joins in-session `deny`/`hint` events from `recommendations.jsonl` into the usage
  record (`recs`, additive — no schema change). `stats` prints `Deny→use: M/N = X%` (+ hint);
  JSON gains `recommendations.funnel`.
- **doctor stale-path detection.** A registered-but-stale-version PreToolUse hook (e.g. an old
  plugin-cache path) fires but may run pre-`recordRecommendation` code, keeping the metric
  silently dark. `doctor` now flags such entries and routes to the existing re-register fix.

## v0.45.4 — fix: intra-class method calls were dropped from the graph in OO languages

The call graph silently omitted every **method → sibling-method** edge for class-based
languages (TypeScript, JavaScript, Python, Java, Ruby). Only Rust (impl methods carry a bare
scope) and Go (receiver methods carry a bare qualified name) were unaffected — which is why
self-dogfooding on this Rust codebase never surfaced it. Two independent root causes:

- **Qualified scope vs. bare source lookup.** A class method's enclosing scope is recorded as
  `Class.method`, but Phase-2 source resolution matched the relation's `source_name` only
  against each node's bare `name` (`method`), so the source never resolved and the edge was
  dropped. Resolution now also matches `qualified_name`.
- **Java calls were never extracted.** tree-sitter-java emits `method_invocation` (not
  `call_expression`), which had no dispatch arm — so *all* Java call edges were missing despite
  Java being a documented Full-tier language. Added the `method_invocation` arm.

Impact: `get_call_graph` / impact analysis / `find_references` undercounted, and
`find_dead_code` produced false positives (a method called only by its siblings looked like an
orphan). `INDEX_VERSION` is bumped 15 → 16, so existing indexes auto-rebuild on first use.

Also: a read-only **secondary** MCP instance (a second editor window on the same project) now
explains a stale "symbol not found" — it does not reindex on its own, so a just-edited symbol
may not be present yet — instead of a bare not-found that's indistinguishable from a typo.

## v0.45.3 — chore: regression-test the binary self-heal wiring (no behavior change)

Hardening follow-up to v0.45.1/v0.45.2. Those two patches both broke in the *orchestration*
that calls `downloadBinary` on a present-but-stale binary — the shell-matches-latest branch of
`checkForUpdate` — while the underlying decision predicate (`cachedBinaryNeedsUpdate`) was
correct and unit-tested the whole time. Predicate tests don't cover whether anything actually
*calls* the predicate, so the exact glue that regressed twice had no test. Extracted that glue
into `selfHealStaleBinary(latest, { needsUpdate, download })` with injected dependencies and a
wiring test (download-invoked when stale / no-op when current / returns false so the next
session retries when the download fails). No runtime behavior change — the self-heal path is
identical; it is now guarded against a third silent regression.

## v0.45.2 — fix: stale binary self-heals on the next session, not up to 6h later

Follow-up to v0.45.1. The binary self-heal added there only ran after the time-based
update throttle (`shouldCheck`, 6h) had elapsed, so a present-but-stale binary could stay
pinned for up to a full check interval after the plugin shell updated. The throttle now
also yields to a present-but-stale binary (compared against the last known release version
— no extra network call), so the self-heal runs on the next session instead of waiting out
the window. Binary still loads into the running MCP server on the next Claude Code restart.

## v0.45.1 — fix: plugin auto-update could pin the native binary at an old version

The plugin shell would update while the cached native binary (`~/.cache/code-graph/bin/`)
stayed stuck at whatever version was first installed, with the updater reporting "up to
date". Three compounding defects in `claude-plugin/scripts/auto-update.js`:

- **Binary download never succeeded.** `promoteVerifiedBinary` read the downloaded
  binary's version (which runs `<binary> --version`) *before* `chmod +x`. `curl -o` writes
  the temp file as `0644`, so the exec failed with `EACCES`, the version read as `null`, and
  verification rejected every download. The `chmod` now happens before the version read.
- **Self-heal was existence-only.** The "no update needed" path re-downloaded the binary
  only when the file was *missing*, never when it was present-but-stale. It is now
  version-aware (new `cachedBinaryNeedsUpdate`): a cached binary whose version differs from
  the latest release self-heals even when the plugin shell version already matches.
- Together these caused a permanent deadlock once the shell version caught up to the
  release while the binary lagged. Existing installs heal automatically on the next update
  check after upgrading to this version.

## v0.45.0 — value references Phase 3b: JSX, Go composites, tuples, primitive paths

More value-reference coverage and precision. The index format bumped (`INDEX_VERSION`
14 → 15), so the first run after upgrade rebuilds the index once.

- **JSX attribute callbacks (JS/TSX).** `<Button onClick={handleClick} />` now emits a
  `references` edge to `handleClick`. Inline-arrow attributes (`onClick={() => …}`) and
  JSX child expressions are not treated as bare references.
- **Go composite-literal field values.** `Handler{ OnEvent: handler }` (and positional
  `[]fn{a, b}`) now reference the field-value function; the field-name key does not.
- **Python tuple return / RHS.** `return f, g` and `a, b = f, g` now reference both `f`
  and `g` (previously only single-value forms were tracked).
- **Primitive-type-head paths suppressed.** `str::trim` / `u32::MAX` and similar no longer
  emit a `references` edge to the bare tail — like the existing PascalCase-type-head rule,
  these associated items can't be resolved and would bind a wrong same-named local.

## v0.44.0 — value references Phase 3a: C / C++ function pointers

Extends callback / function-pointer reference tracking to C and C++ — where function
pointers are the primary callback mechanism. The index format bumped (`INDEX_VERSION`
13 → 14), so the first run after upgrade rebuilds the index once.

- **C/C++ value references.** A function named by a bare identifier now emits a
  `references` edge when passed as a call argument (`qsort(a, n, s, compare)`), taken by
  address (`signal(2, &handler)`), used as a designated or positional initializer value —
  the vtable idiom `struct ops o = { .read = my_read }` — assigned (`ops->read = my_read`),
  bound (`fn_t cb = handler`), or returned.
- **Precision.** A C/C++ local — function parameters and body declarations — is excluded
  by resolving the declared name from the declarator chain (handling
  `int *x` / `void (*cb)(int)` / multi-declarator forms), never from an initializer value,
  so a local passed by name does not fabricate an edge to a same-named global function.
  References still resolve same-file or same-language only.

## v0.43.0 — value references Phase 2: more positions + Python/Go

Extends callback / function-pointer reference tracking (v0.41.0) to more syntactic
positions and two more languages. The index format bumped (`INDEX_VERSION` 11 → 13), so
the first run after upgrade rebuilds the index once.

- **More value positions (Rust + JS/TS).** Beyond call arguments, a function named by a
  bare identifier now emits a `references` edge when it is: a binding RHS (`let cb =
  handler` / `const cb = handler`), a return value (`return handler`, Rust tail
  expression `{ … handler }`, JS arrow body `() => handler`), or a struct / object field
  value (`Config { cb: handler }`, `{ onClick: handler }`).
- **Python value references.** Python now tracks callbacks passed by bare name in call
  arguments (`register(handler)`), keyword arguments (`sorted(xs, key=my_key)`),
  assignment RHS (`cb = handler`), `return`, and dict values — in addition to the
  existing type-annotation references.
- **Go value references.** Go now tracks callbacks in call arguments
  (`http.HandleFunc(p, handler)`), `:=` / `=` / `var` right-hand sides, and `return`.
- **Precision.** Each language excludes local bindings (parameters, `let`/`const`/`:=`/
  `var` declarations, and Rust `if let` / `match` / `for` patterns, Python assignment /
  `for` targets, Go `:=` / `range` targets) so a bare name that is a local — not a global
  function — does not emit a false reference. References still resolve same-file or
  same-language only.

## v0.42.0 — reference precision: drop macro-path and type-associated false positives

Follow-up to v0.41.0. The `references` edge extractor for Rust path-qualified values
(`extract_rust_path_reference`) emitted two classes of false positive that predate
v0.41.0; both are now suppressed. The index format bumped (`INDEX_VERSION` 10 → 11), so
the first run after upgrade rebuilds the index once.

- **Macro paths.** `tracing::error!(…)` / `serde_json::json!(…)` parse as a macro
  invocation whose macro name is a scoped path (`tracing::error`). The extractor treated
  the tail (`error`) as a value reference, colliding with same-named functions. Macro
  paths no longer emit a reference.
- **Type-associated paths.** `String::as_str` passed as a function pointer (e.g.
  `.map(String::as_str)`) emitted a reference to the bare tail `as_str`, which then
  bound to an unrelated local function — the associated item can't be resolved (std
  methods aren't indexed). A PascalCase path head (a type) now suppresses the reference;
  lowercase module heads (`crate::domain::SHARED`) still emit. Primitive-type heads
  (`str::trim`) are a documented residual.

On this repo these removed 14 phantom reference edges (504 → 490) with zero genuine
references lost.

## v0.41.0 — callback / function-pointer references (Phase 1)

A function passed as a *value* — a callback or function pointer — is referenced by a
bare identifier in a non-call position (`register(handler)`, `xs.iter().map(map_row)`,
`addEventListener('click', handler)`, `signal(&shutdown)`). The parser only emitted a
`calls` edge for actual call expressions, so these usages produced no edge:
`find_references`, `impact_analysis`, and dead-code all missed them. This release tracks
them as `references` edges (Rust + JS/TS/TSX). The index format bumped
(`INDEX_VERSION` 9 → 10), so the first run after upgrade rebuilds the index once.

- **Bare-identifier value references.** A bare function name in call-argument position
  (or Rust address-of `&fn`) now emits a `references` edge to the function, distinct
  from `calls` — a registered callback is coupling, not a synchronous call. They surface
  in `find_references` and as a new `value_references` count in `impact_analysis`, but
  stay out of the call graph and the calls-based hot-function / caller counts.
- **Precision gates.** Bare identifiers are only emitted as references when they are not
  a local binding: enclosing-function parameters, `let` bindings, and `if let` /
  `while let` / `match`-arm / `for` pattern bindings are all excluded (they shadow
  same-named accessor functions like `db` / `conn` / `node`). Resolution requires a
  same-file or same-language target — a reference is dropped rather than bound to a
  cross-language same-named function.
- **Cross-language resolution fix.** The Phase-2 edge resolver previously let non-`calls`
  relations fall through to a global, cross-language name pool. `references` now drops on
  no same-language match (like `calls`), so a Rust value-reference can never bind a
  same-named JavaScript function.

## v0.40.0 — dogfooding fixes: project_map accuracy, trace/grep, build-dir exclusion

End-to-end dogfooding pass. The index format bumped (`INDEX_VERSION` 8 → 9, for the
build-dir exclusion below), so the first run after upgrade rebuilds the index once
automatically.

Read-side query fixes for `map` / `project_map`. Dogfooding surfaced three
count/labeling defects in the project map:

- **Synthetic `<external>` bucket excluded.** Unresolved external-import targets
  (e.g. `os`, and external traits like `Drop` / `std::io::Write` / `Default`) live
  in a virtual `<external>` file. These leaked into the module map as a phantom
  `<root>` module ("0 symbols, external") and were counted as project symbols, and
  every external/builtin import surfaced as a misleading `→ <root>` dependency
  (dominating the Dependencies section). `<external>` is now excluded from the
  module list, symbol counts, key-symbols, and the cross-module dependency graph —
  Dependencies now shows genuine internal module coupling only.
- **Methods counted toward the symbol total.** The per-module "N symbols" count
  summed only functions + classes/structs/enums + interfaces/traits, silently
  dropping `method` nodes. For OO modules this undercounted, and contradicted the
  `key_symbols` list (which includes methods) — a class file could read "3 symbols"
  while listing 4. Methods now count, matching `overview`.
- **`deps` counts distinct symbols, not edges.** File-level dependencies labeled
  "N symbols" actually counted cross-file edges, so a symbol both imported and
  called inflated the count (2 symbols → "4 symbols"). Now counts distinct target
  symbols.
- **`trace` renders the route label, not raw JSON.** The CLI `trace` text output
  printed the raw `routes_to` metadata blob (`{"handler_end_line":10,...}`) as the
  route label instead of `GET /users` like the map already does. Now formatted as
  `METHOD path`.
- **`grep` exits non-zero on a ripgrep error.** An invalid regex (e.g. an
  unescaped `(` in `res.json(`) or unreadable path printed the error to stderr but
  still exited 0, hiding the failure from scripts. ripgrep's error exit code (2) is
  now surfaced as a non-zero CLI exit; a valid no-match still exits 0.
- **Build/dependency dirs excluded without a `.gitignore`.** Indexing relied on
  the `ignore` crate's gitignore rules, so a project with no `.gitignore` (or that
  isn't a git repo) indexed `node_modules/` (JS/TS source), `target/` (Rust/Maven
  build), and nested copies — bloating the index and polluting the graph with
  dependency/build code. These (plus `vendor/`, `bower_components/`) are now
  excluded by a hardcoded safety net, matched on whole path segments at any depth
  so a directory `target/` is skipped while a file `target.rs` is still indexed.
  Hidden dirs (`.git`, `.venv`, `.code-graph`, …) were already skipped.

## v0.39.0 — C++ method scope, real-session conversion metric, adopted-only map

Parser + metrics release. The index format bumped (`INDEX_VERSION` 7 → 8), so the
first run after upgrade rebuilds the index once automatically.

**C++ `Class::method` scope.** C/C++ method extraction was previously bare-name
only — in-class methods, out-of-class `Type::method` definitions, and qualified
`Foo::bar()` calls all lost their type scope, and qualified calls produced no
edge at all. Now:

- In-class methods (`class Foo { void bar(){} }`) and out-of-class definitions
  (`void Foo::bar(){}`) carry `node_type: "method"` + `qualified_name: "Foo.bar"`.
- Call sources inside C/C++ functions/methods now attribute to the function
  (`Foo.bar`) instead of `<module>` (`scope_name` had no `name` field for C/C++).
- Qualified calls `Foo::bar()` now produce a `calls` edge (resolved by rightmost
  name, same-language) — they were silently dropped before.

So call-graph / impact / dead-code for C and C++ are materially more accurate.

**Real-session conversion metric.** The PreToolUse hooks now record each
recommendation (raw-grep hint/deny, read-fanout hint) to
`.code-graph/recommendations.jsonl`, and `code-graph-mcp stats` reports the
field conversion signal — cg tool calls vs recommendations emitted — that the
synthetic routing benchmark can't see. `--json` gains a `recommendations` block
(`total` / `by_action` / `by_hook` / `cg_tool_calls` / `conversion_ratio`). The
recorder is append-only and never creates `.code-graph`, so non-project / tmp
cwds leave zero footprint.

**SessionStart map injection is adopted-only.** On top of the existing
quiet-by-default behavior (the map is opt-in via `CODE_GRAPH_VERBOSE_HOOKS=1`),
the project-map dump now also requires the project to be adopted into your
MEMORY.md workflow. Unadopted projects get no map even under verbose — the dump
was measured to be zero-referenced there.

## v0.38.0 — call-graph precision: prune import-contradicted edges

Graph-output release: `get_call_graph` / `get_ast_node include_impact` / dead-code
results change for repos with same-named functions across files. The index format
bumped (`INDEX_VERSION` 6 → 7), so the first run after upgrade rebuilds the index
once automatically.

**Call-graph false positives removed.** When a file makes a bare call `save()`
whose name matches several functions across files, the resolver used to fan the
edge out to every candidate (to protect dead-code precision when it had no way to
choose). When the caller's file has an explicit `imports` edge binding that name
to one node — e.g. Python `from db import save` — the bare call resolves to the
imported node, so the other same-name edges are false callers. Those are now
pruned: impact/call-graph no longer lists phantom callers, and the genuinely
uncalled sibling is correctly reported by dead-code. The prune is conservative —
it only touches bare-name edges contradicted by an import, never qualified calls
(`cache.save()`, `crate::x::foo()`), same-file targets, or the no-import tie case
(so Rust scoped-call dead-code precision is preserved). Languages whose imports
themselves fan out (JS/TS, Rust `use`) safely no-op.

**Other fixes:**

- **CLI `--json` empty/error paths** now always emit success-shaped JSON: `refs`
  (all three not-found branches), `similar` (no-embeddings / missing `--node-id`),
  and `search` no longer under-returns below `--limit` when the always-on
  test/module filter drops top rows.
- **`stats` version list** sorts numerically (major.minor.patch) instead of
  lexically, so `0.5.40` no longer sorts after `0.32.2`.
- **Index version-mismatch is now visible.** When two binaries of different
  `INDEX_VERSION` share one `.code-graph/index.db` and clear each other's data,
  the cause is printed to stderr (was a silent "index is empty").
- **Grammar.** Module-overview and dead-code entries print `1 file` / `1 line`
  instead of `1 files` / `1 lines`.

## v0.37.0 — CLI argument parsing migrated to clap-derive

All 22 `code-graph-mcp` CLI subcommands moved from a hand-rolled argv parser to
clap-derive; the hand parser is gone entirely. Every success path is preserved —
queries, filters, JSON envelopes, and the found/not-found exit codes are
unchanged. The changes are confined to argument-error handling and help, where
clap now owns parsing. **No MCP tool behavior changes** (the MCP server was never
hand-parsed).

**User-visible CLI changes** (see `code-graph-mcp <cmd> --help` for each surface):

- **Per-command help.** Every subcommand now has clap-generated `--help`/`-h`
  (exit 0). Top-level `--version`/`-V` unchanged.
- **Stricter argument errors → exit 2.** Unknown flags, extra positionals, a
  missing required argument, and non-numeric or negative-numeric flag values
  (e.g. `--depth -5`) now error with exit code 2. The old parser silently ignored
  unknown flags / extra args and clamped-or-defaulted bad numbers (exit 0/1).
- **`--flag=value` now honored.** `--limit=5`, `--direction=callees`,
  `--relation=calls`, etc. now parse correctly. The old exact-token parser
  silently ignored the `=value` form and fell back to defaults — sometimes
  reporting false success (e.g. `refs --relation=calls` returned *all* references).
- **`trace`.** The advertised-but-non-functional `--include-middleware` flag is
  removed; use `--no-middleware` to hide downstream middleware (shown by default),
  which is what the command always actually did.
- **`snapshot`** is now a real `create` / `inspect` subcommand pair; an unknown or
  missing subcommand errors (exit 2).

In-handler validation messages and exit codes for the enum flags (`--direction`,
`--change-type`, `--relation`) and the symbol-not-found / ambiguous-symbol guards
are unchanged. clap is added as a runtime dependency.

## v0.36.0 — Audit remediation: snapshot supply-chain integrity, CLI/MCP consistency, honest dead-code

Remediation of the 2026-06-03 architecture/security audit. Headline items: two
supply-chain hardenings on the snapshot install path, and a correctness fix that
makes the CLI and MCP give the **same** verdict on ambiguous symbols.

**Security — snapshot supply-chain integrity.** First-run snapshot install
downloads a `.db.zst` that becomes the entire code graph. It now verifies the
artifact against a published `<url>.blake3` sidecar **before** decompressing
(hard-fail on mismatch), caps the compressed side against a zip-bomb, and refuses
to honor a `.code-graph.toml [snapshot] url` override unless the developer opts in
out-of-band via `CODE_GRAPH_SNAPSHOT_TRUST_URL=1` — so a committed url in a
malicious repo/PR can no longer silently redirect the graph to an attacker-chosen
database. The artifact and sidecar fetches also reject HTTPS→HTTP redirect
downgrades, closing a plaintext-substitution path.

**Behavior change — `callgraph`/`impact` on same-file overloads.** A bare symbol
name resolving to ≥2 non-test definitions in the **same file** (e.g. two
`fn new()` in different impl blocks) is now reported as ambiguous instead of
silently merging their call graphs. Previously the CLI merged them (exit 0, a
conflated answer) while the MCP tool refused — same input, opposite answers; both
now refuse consistently via a shared resolver. **Migration:** disambiguate with
`--file` + `show --node-id <N>` (CLI) or `file_path` + `node_id` / `get_ast_node`
(MCP). Cross-file collisions still take `--file`/`file_path` as before.

**Output change — dead-code reports "candidates", not "results".** `find_dead_code`
(CLI + MCP) now frames output as candidates needing human verification and notes
that receiver-method calls (`obj.method()`) and cross-file const/type uses aren't
edge-tracked, so a flagged symbol may still be live. The MCP
`module_overview.include_dead` schema description carries the same caveat. Scripts
or agents that string-matched the old `results` wording should update.

**Fixes.**
- Vectors no longer go stale after an incremental edit: a cross-file dirty node
  whose context changed while the embedding model wasn't loaded now has its vector
  invalidated so the background embedder regenerates it.
- Enum arguments (`direction`, `change_type`, `relation`, `deps_direction`) are
  validated at the tool/command entry on both CLI and MCP, so a typo errors
  cleanly instead of surfacing as a confusing downstream/freshness error.
- CLI `--ignore` (dead-code) parses as a value flag, and a misspelled `--type` is
  rejected loudly instead of silently matching zero rows.

**Internal / hardening.**
- WAL bounded (`journal_size_limit` + checkpoint TRUNCATE in `run_optimize`).
- Rerank multipliers extracted to named `domain.rs` constants (value-preserving).
- `build.rs` pins the vendored `sqlite-vec` C source against a blake3 checksum and
  fails the build on tamper.
- Session metrics can tag dogfood traffic via `CODE_GRAPH_DOGFOOD=1`; the
  `TriggerRate` routing metric gains a soft floor.

## v0.35.0 — Reference edges + receiver-call resolution sharpen dead-code & find_references

**Feature (additive).** A new `references` graph relation captures edgeless
symbol *usages* — types used only in annotation/type position (`field: Foo`,
`func g() Bar`, `List<Foo>`) and Rust path-qualified const references
(`crate::a::FOO`). It is extracted for **Rust, TypeScript/TSX, Python, Go, and
Java**, each gated to that language's real type-position AST nodes and filtered
against per-language builtin/JDK noise sets so it points only at project
symbols. `find_dead_code` now counts incoming `references` edges as usage, and
`find_references` gains a `relation: "references"` filter (additive enum value
on the tool schema + CLI `--relation references`).

Why: a type/interface/enum/const that is defined and used **only** as a type
annotation or a path-qualified constant produced no `calls`/`imports` edge, so
`find_dead_code` reported it as dead (an agent acting on that could delete live
code) and `find_references`/`impact` were blind to it. Reference edges are
produced at parse time from the full source, so they are immune to the
`code_content` truncation that the same-file `instr` fallback suffered.

**Fix — receiver-method calls.** `obj.method()` calls whose receiver type can't
be statically inferred were dropped entirely, marking uniquely-named live
methods (e.g. `file_exists`, `validate`) as dead and hiding their callers from
`impact`/`callers`. They now resolve to a real `calls` edge **only when
unambiguous** — exactly one same-language method of that name (non-stdlib),
preferring a same-file match. Ambiguous or stdlib-noise names still drop, so
`impact` cannot fan out across unrelated modules.

**Fix — dead-code false positives.** The dead-code reference fallback now also
probes other files for edgeless node kinds (const/struct/enum/type/interface/
trait, name length ≥ 5, delimiter-aware) and scans same-file declaration bodies
(not just function/method bodies), rescuing cross-file path-qualified consts and
same-file struct-field type usages.

### Migration

- `INDEX_VERSION` is bumped 5 → 6: the server detects the mismatch on first run
  and automatically clears + rebuilds `.code-graph/index.db` so the new edges
  are present. No action needed.
- The `references` relation is additive; existing `find_references` calls
  (`calls`/`imports`/`inherits`/`implements`/`all`) are unchanged.

## v0.34.0 — Rust binary no-ops in non-project directories

**Behavior change (opt-out available).** `code-graph-mcp serve` now serves a
0-tool stub when launched directly in a non-project working directory (no
`.git`/manifest marker), mirroring the v0.33.0 MCP-launcher gate. It opens no
database, loads no embedding model, creates no `.code-graph/`, and emits no
tool-decision `instructions`.

Why: v0.33.0 gated the JS launcher path, but a binary invoked directly —
bypassing the launcher (a dev `.mcp.json`, or any MCP config pointing straight
at the binary) — still half-activated in a marker-less cwd. This closes that
parallel path so the activation boundary holds at the Rust layer regardless of
entry point. Detection is marker-based (`.git`, `package.json`, `Cargo.toml`,
`pyproject.toml`, `go.mod`, `pom.xml`, `build.gradle`), not a literal path
check — a real git repo under `/tmp` still activates.

### Migration / opt-out

- No action needed; real projects (with a project marker) serve the full tool
  catalog exactly as before.
- To force the full plugin MCP in a marker-less cwd, set
  `CODE_GRAPH_FORCE_PLUGIN_MCP=1` (same override as the launcher gate).

## v0.33.0 — plugin no-ops in non-project directories

**Behavior change (opt-out available).** The plugin now fully no-ops in working
directories that are not a project — i.e. that carry none of the recognized
project markers (`.git`, `package.json`, `Cargo.toml`, `pyproject.toml`,
`go.mod`, `pom.xml`, `build.gradle`). In such a cwd the MCP launcher serves a
0-tool stub instead of spawning the binary, and the SessionStart hook +
auto-adopt short-circuit.

Why: a cross-project audit found code-graph half-activating on the ~2035
headless `claude -p` calls claude-mem-lite spawns with `cwd=/tmp` ("Return ONLY
valid JSON") — none of which ever use code-graph. Each one paid an MCP-server
spin-up, a ~780-byte `instructions` block injected into a JSON-only task, a
SessionStart map probe, and an empty `/tmp/.code-graph/index.db`, plus an
adopted decision-table sentinel in `~/.claude/projects/-tmp/memory/MEMORY.md`.
Net waste, zero usage. A real git repo cloned under `/tmp` still activates —
detection is marker-based, not a literal path check.

### Migration / opt-out

- No action needed for normal use; real projects (with a project marker)
  activate exactly as before.
- To force the full plugin MCP in a marker-less cwd, set
  `CODE_GRAPH_FORCE_PLUGIN_MCP=1` (the same override that bypasses the
  tool-catalog dedup gate).
- `.code-graph` is no longer treated as a project marker for activation — a
  directory whose only marker is a previously-created `.code-graph` is now
  considered non-project (prevents a polluted `/tmp` from self-certifying).

### Changed

- New `claude-plugin/scripts/project-detect.js` centralizes project detection
  (`isProjectRoot` / `isNonProjectCwd`, marker set sans `.code-graph`); the MCP
  launcher, the SessionStart hook, and `adopt()` all gate on it.
- `adopt()` now refuses a non-project cwd **even when the memory dir already
  exists** — the prior guard sat inside `if (!fs.existsSync(dir))` and was
  bypassed because Claude Code pre-creates `~/.claude/projects/<slug>/memory`
  for every session (including the headless `/tmp` calls).

## v0.32.3 — CLI path normalization + enum-arg early validation

Three-fix patch bundling end-to-end dogfood findings. Two of the three
were *silent wrong-answer* bugs (worst kind: no error, no exit code,
just incorrect output). The third tightens MCP/CLI error attribution
so a single typo doesn't surface as two cascading errors.

### Fixed

- **CLI commands accept absolute paths under the project root.** The
  indexed `file_path` column is project-relative; users who pasted
  absolute paths from an IDE used to get:
  - `overview /abs/path` → "No symbols found" (silent wrong, exit 1)
  - `dead-code /abs/path` → exit-0 "No dead code found" (most dangerous —
    user trusts the wrong answer)
  - `deps /abs/path` → bogus `barrel_scan` fallback with empty
    `depended_by`/`depends_on` (user thinks file has no deps)
  - `callgraph X --file /abs/path` (also `impact`/`show`/`refs`) → empty
    filter, no edges, false "Symbol not found"
  v0.32.3 introduces `normalize_user_path(project_root, raw)` and
  routes all positional path args (`overview`, `deps`, `dead-code`)
  plus all `--file` flags through it. Behavior:
  - `.` → `""` (whole project, matches MCP `module_overview` semantics)
  - `./foo` → `foo`
  - absolute under root → relative portion (lexical strip; canonicalize
    fallback covers symlinks)
  - absolute outside root → explicit error, *not* silent wrong
  - relative path → unchanged
  Distinct from MCP-side `tool_module_overview` which *rejects* absolute
  paths outright — CLI is more lenient because real humans paste from
  IDE. Regression coverage in `tests/cli_e2e.rs::*absolute_path*`.

- **`get_call_graph` / `dependency_graph` / `module_overview` validate
  enum args at tool entry (MCP + CLI).** Previously a bogus
  `direction` / `deps_direction` value first hit the ambiguity check
  (which echoed the bogus value back in the JSON), and only after the
  user disambiguated with `file_path` would the underlying graph layer
  reject the enum — two errors for one mistake. Worse,
  `module_overview deps_direction=bogus include_deps=true` swallowed
  the downstream `tool_dependency_graph` error into a
  `dependencies_unavailable` text field and returned
  `isError=false`, so agents/scripts treated the call as successful.
  v0.32.3 gates each enum-valued arg at the tool/cmd entry with
  `matches!()`, matching the schema enum exactly and echoing the bad
  value in the error message. Affected:
  - MCP `get_call_graph` `direction`
  - MCP `dependency_graph` `direction`
  - MCP `module_overview` `deps_direction`
  - CLI `callgraph --direction`
  Regression coverage in
  `tests/mcp_stdio_integration.rs::mcp_enum_args_validated_at_tool_entry`
  and `tests/cli_e2e.rs::test_cli_callgraph_invalid_direction_errors_early`.

- **`show <Class.method>` falls back to base-name match when the DB
  doesn't store the qualified prefix.** Parsers populate
  `qualified_name` inconsistently (Rust `impl` blocks: yes; free
  functions: no). The old `show` fallback required an exact qualified
  match and silently returned "Symbol not found" when the DB only had
  the base name — even though `callgraph <Class.method>` resolved the
  same input fine via prefix-strip. v0.32.3: prefer exact qualified
  matches when any exist; otherwise fall back to all base-name nodes.
  No regression for the qualified case (`show Database.open` still
  uniquely matches the Rust `impl Database::open`). Regression
  coverage in
  `tests/cli_e2e.rs::test_cli_show_qualified_falls_back_to_base_name`.

- **`stats --last <non-integer>` now warns instead of silently
  showing all sessions.** Previously `--last abc` parsed through
  `.ok()` and fell through to "no filter" — exactly opposite of the
  user's stated intent. Aligned with `parse_flag_or`'s pattern
  (warn-and-default) used by other numeric flags.

### Tests

11 new regression tests across `cli_e2e` (+8: abs-path on
`overview`/`deps`/`dead-code`, callgraph invalid-direction,
show qualified fallback, plus 5 `normalize_user_path` unit tests in
`src/cli.rs`) and `mcp_stdio_integration` (+1 enum-arg validation).
Full suite: 595 passing.

## v0.32.2 — tmp-dir containment + healthCheck repair verification

Three-fix patch bundling reviewer follow-ups from v0.32.1 (M3/M4) and a
session-dogfood discovery (M6/tmp-dir). None changes the public contract
or schema; `healthCheck()` return shape gets one additive field
(`remaining`) and one tightened invariant (`repaired:true` requires a
post-install re-scan to be clean).

### Fixed

- **`healthCheck()` `repaired:true` is now verified by a post-install
  re-scan (M3)** — previously the function ran `install()` whenever a
  flagged path was found and unconditionally returned `repaired:true`,
  even if `install()` couldn't actually resolve the issue (binary
  permanently gone, third-party registry entry not ours to touch,
  etc.). `doctor.js` then printed "N issue(s) auto-repaired ✅" —
  Iron Law #2 style honesty violation. v0.32.2 extracts
  `scanForBrokenPaths()` as a pure function (also exported for direct
  unit testing); `healthCheck()` runs it once, then if issues found
  runs `install()` and re-runs the scan; `repaired:true` only when the
  post-repair scan is clean. Otherwise `repaired:false` plus a new
  `remaining[]` field listing what's still broken. `doctor.js` Hooks
  row consumes the new shape: `repaired:true` → `ok` /
  "N issue(s) auto-repaired"; `repaired:false` → `warn` /
  "N invalid path(s) — auto-repair did not resolve" + `fixId:'hooks-invalid'`
  so the user can still try the manual repair path.
- **`doctor.js` `hooks-invalid` repair output aligned with sibling
  message (M4)** — `runRepairs('hooks-invalid')` previously logged
  "✅ Hooks repaired" but the sibling `missing-hooks-in-settings` case
  already said "✅ Hooks repaired — restart Claude Code to apply".
  `settings.json` changes only land after Claude Code restarts the hook
  dispatcher, so both messages now say the same thing.
- **Hook + auto-update tmp artifacts no longer collide with Claude
  Code's transcript directory (M6)** — `pre-grep-guide.js` cooldown
  flags (`.code-graph-bash-<hash>`), `pre-edit-guide.js` impact cache
  (`.cg-impact-<symbol>`), `pre-read-guide.js` readfan state
  (`.code-graph-readfan-<hash>.json`), and `auto-update.js` staging dir
  (`code-graph-update-<ts>`) previously called `os.tmpdir()` directly.
  Claude Code overrides `$TMPDIR` to `~/.claude/tmp/` (to capture
  process output for transcript replay), so these artifacts landed
  alongside 9000+ transcript subdirs. Two failure modes:
  (a) **diagnostic blindness** — every doc / memory / debug query that
  checked `/tmp/.code-graph-bash-*` for hook firing returned empty even
  when the hook worked correctly (the v0.32.0 "PreToolUse dark under
  green health" investigation burned ~2 hours chasing this red
  herring); (b) **§8 SAFETY recursive-traversal trap** — scattering
  0-byte flag files alongside transcript subdirs amplifies the
  "`grep -r ~/.claude/tmp/`" footgun. Fix: new `tmp-dir.js` helper pins
  all hook + auto-update artifacts to `${tmpdir}/code-graph-mcp/`.
  Legacy orphans in the parent age out naturally (0-byte, no behavior
  impact). Diagnostic memory (`feedback_pretooluse_dark_under_green_health.md`)
  updated to point at the new location.

### Internal

- `lifecycle.js` now exports `scanForBrokenPaths()` as a pure function
  for unit testing.
- New `claude-plugin/scripts/tmp-dir.js` module — single source of
  truth for hook + auto-update tmp paths.

### Testing

- 448/448 plugin script tests pass (29 new on v0.32.0 baseline):
  - `healthCheck` clean-state → `healthy:true`, no `remaining` field
  - `healthCheck` post-repair re-scan clean → `repaired:true`,
    `remaining:[]`
  - `healthCheck` post-repair re-scan still broken → `repaired:false`,
    `remaining` lists what `install()` couldn't fix
  - `scanForBrokenPaths` exported and returns the expected issue
    structure
  - `update()` from v0.31.2 manifest + empty settings.json — reviewer
    Rec #2 covered the actual production migration path
  - `tmp-dir.js`: `CG_TMP_DIR` resolves to
    `${os.tmpdir()}/code-graph-mcp`, `cgTmpDir()` creates the dir on
    demand and is idempotent, regression guard that hook flag basenames
    never leak into the parent root (`os.tmpdir()` itself)

### Process

M3/M4 driven by `/superpowers:requesting-code-review` follow-ups on the
v0.32.1 patch. M6 surfaced by an end-of-cycle user dogfood pass —
direct invoke of `pre-grep-guide.js` showed HINT firing but zero
`/tmp/.code-graph-bash-*` flag files; `$TMPDIR` probe revealed the
override.

## v0.32.1 — block-tier false-positive fixes + foreign-strip risk reduction

Four-finding code-review patch on v0.32.0. All findings were `Important`
(not Critical, not data-loss), but they hit exactly the spec the v0.32.0
block tier was designed to satisfy ("narrowest case"), so they get fast
follow-up rather than a v0.33 bundle.

### Fixed

- **Block tier no longer false-positives on identifier-shaped PATH ARGUMENTS** —
  v0.32.0's `shouldBlock` ran `IDENTIFIER_LIKE.test(cmd)` against the full
  command, so `grep -rn "abc" src/EmbeddingModel.rs` blocked because the
  filename contains CamelCase. Same problem for snake_case directory names
  (`src/some_module/...`). v0.32.1 adds `extractPatterns(cmd)` that pulls
  quoted arguments out of the command and tests `IDENTIFIER_LIKE` against
  the patterns only. Unquoted-pattern usage (`grep foo src/`) falls back to
  hint behavior — conservative.
- **`type` dropped from declaration-anchor keyword list; remaining anchors
  pinned to pattern start** — `\btype\s+\w` matched "# type checking",
  "some type X", and other comment-string scans. v0.32.1 drops `type`
  (too common in English prose) and anchors `fn` / `def` / `class` /
  `function` / `struct` / `impl` / `trait` to `^\s*` so they only fire
  when the user is actually searching for a declaration.
- **`isOurHookEntry` script-name fallback tightened** — v0.32.0 used
  `cmd.includes('code-graph')` which would claim a user's own
  `~/code-graph/foo.js`. v0.32.1 uses `MARKETPLACE_NAME` (`code-graph-mcp`)
  instead. Foreign-entry strip risk eliminated.
- **`doctor.js` report no longer prints two `'Hooks'` rows** — check 6
  (healthCheck path validation) keeps `'Hooks'`; check 7 (settings.json
  coverage) renamed to `'Hook coverage'`.

### Internal

- Dropped a `pluginRootDir()` function that duplicated the module-level
  `PLUGIN_ROOT` constant.
- Cleaned up `doctor.js` unused imports (`removeHooksFromSettings`,
  `writeJsonAtomic`) that became dead when the v0.32.0 inversion replaced
  the legacy strip path with `install()`-as-repair.

### Testing

- 437/437 plugin script tests pass (18 new). New coverage:
  - `extractPatterns` matrix: single/multi quoted, env-prefixed verb,
    `rg` / `ag` heads, no-quote conservative fallback, empty/null inputs
  - I1 regressions: `"abc"` against CamelCase-named file, `"x"` against
    snake_case directory, English-prose pattern with identifier-name
    file path, unquoted-pattern hint fallback, inverse sanity (CamelCase
    pattern + plain path still blocks)
  - I4 regressions: `"# type checking"`, `"some type X"`, `"the def keyword"`
    all hint (not block); real `"def calc_total"` and `"fn render"` still
    block
- E2E smoke (three inputs through the actual hook script, sandboxed
  `.code-graph/index.db`): I1 case emits hint text, I4 case emits hint
  text, real CamelCase symbol search emits JSON block envelope with
  `permissionDecision: "deny"`. cargo check green.

### Process

Driven by `/superpowers:requesting-code-review` subagent on the
`b5c907f..a25bee9` (v0.31.2..v0.32.0) range. Verdict was "With fixes
— needs v0.32.1 patch"; this is that patch.

## v0.32.0 — re-route PreToolUse/PostToolUse/UserPromptSubmit via settings.json (root-cause fix for "hook never fired since v0.25.0")

Architecture-level fix for a silent failure that turned every v0.25.0+ release
into theater. The plugin's PreToolUse hooks (`pre-edit-guide.js`, `pre-grep-guide.js`,
`pre-read-guide.js`), the PostToolUse incremental indexer, and the UserPromptSubmit
context push **never actually fired** since v0.25.0. Only SessionStart worked.

### The bug

`claude-plugin/hooks/hooks.json` registered every event type (PreToolUse,
PostToolUse, UserPromptSubmit, SessionStart). Empirical 2026-05-24 diagnostic in
a real Claude Code session:

- 25+ Bash tool calls produced **zero** `PreToolUse:Bash` events in the session jsonl
- **Zero** `/tmp/.code-graph-bash-*` cooldown flags (the hook scripts were never spawned)
- `.code-graph/index.db` mtime was the SessionStart timestamp — `incremental-index.js`
  (PostToolUse:Edit handler) never ran despite 5+ Edits

Cross-validated by inspecting `~/.claude/plugins/cache/sdsrss/claude-mem-lite/*/hooks/hooks.json`
— claude-mem-lite ships `"hooks": {}` with the note "Hooks managed by install.mjs
in settings.json — this file cleared to prevent duplicates". The pattern is well-known
in the ecosystem; code-graph-mcp was the outlier.

The v0.31.1 "PreToolUse hooks never fired since v0.25.0" fix corrected the matcher
DSL (`tool == "Edit"` → `"Edit"`) — the matcher was right but the entries
themselves were never loaded by CC for non-SessionStart events.

### Fixed (root cause)

- `claude-plugin/scripts/lifecycle.js` — new `registerHooksToSettings(settings)`
  actively writes PreToolUse / PostToolUse / UserPromptSubmit entries into
  `~/.claude/settings.json` on `install()` and `update()`. Two-pass: evict our
  entries across every event (catches legacy v0.7/v0.8 entries with stale paths),
  then append fresh v0.32+ entries for the events we own. Description markers
  carry `[code-graph-mcp v0.32+] …` for cleanup. Hook command paths are
  absolute, derived from `__dirname` — no `${CLAUDE_PLUGIN_ROOT}` (immune to
  cross-plugin env leak per `feedback_plugin_env_isolation.md`).
- `claude-plugin/hooks/hooks.json` — SessionStart only. `_note` explicitly
  documents that other event types would be dead config if added back.
- `claude-plugin/scripts/doctor.js` — `legacy-hooks-in-settings` →
  `missing-hooks-in-settings`. Semantics inverted: presence in settings.json
  is now correct; missing is the bug. Repair runs `install()` instead of strip.
- `claude-plugin/scripts/session-init.js` — new self-heal path detects missing
  settings.json coverage and triggers `install()` (catches manual settings
  edits and third-party settings.json rewriters that don't preserve markers).

### Added

- **PreToolUse Bash block tier** in `pre-grep-guide.js`. Raw `grep -r{n}` /
  `rg` / `ag` against an indexed source tree with an identifier-shaped
  pattern is now blocked via `{ hookSpecificOutput: { hookEventName:
  "PreToolUse", permissionDecision: "deny", permissionDecisionReason:
  "<cg equivalent>" } }` (current Claude Code schema — the
  `hookEventName` discriminator is required for CC to recognize the
  permission verdict). Bash is the comfort-zone leak — 15d audit measured 429 raw
  grep vs 191 functional CLI (~13× preference). Block tier targets the
  narrowest "I'm searching for a symbol" subset: bare flags (no `-l`,
  `--include`, `-A`/`-B`/`-C`), identifier-shaped pattern (CamelCase ≥4ch,
  snake_case with `_`, or declaration anchor `fn X` / `class X` / `def X`),
  not a marker word (TODO/FIXME/XXX). Marker-only and precision-flag scans
  downgrade to the v0.25.0 informational hint. Escape hatch: prepend
  `CODE_GRAPH_NO_BLOCK_GREP=1` to fall back to hint behavior; independent
  of `CODE_GRAPH_QUIET_HOOKS=1` which silences entirely.

### Testing

- 419/419 `claude-plugin/scripts` tests pass — 165 net new across `hooks.test.js`,
  `lifecycle.test.js`, `pre-grep-guide.test.js`. New coverage includes:
  - settings.json install writes the expected matchers (`Edit`, `Bash`, `Read`
    for PreToolUse, `Write|Edit` for PostToolUse, UserPromptSubmit), every
    entry carries a description marker, paths are absolute (no
    `${CLAUDE_PLUGIN_ROOT}`)
  - install is byte-idempotent across re-runs
  - foreign plugins' settings.json entries survive install/uninstall (no
    collateral strip)
  - legacy v0.8.x SessionStart entries with stale paths get evicted on install
  - hooks.json contract: only `SessionStart` key present; cross-validates that
    `lifecycle.js` `buildSettingsHookEntries()` covers the matchers removed
    from hooks.json
  - `shouldBlock(cmd)` matrix: 22 cases covering CamelCase / snake_case /
    declaration anchors / precision-flag downgrades / marker-only downgrades

### Migration

Automatic via `lifecycle.js update()`. v0.31.x users upgrading to v0.32.0:
- `session-init.js syncLifecycleConfig` detects version mismatch → calls `update()`
- `update()` calls `registerHooksToSettings(settings)` → fresh entries land in
  `~/.claude/settings.json`
- Plugin-cache `hooks.json` arrives without the dead non-SessionStart entries
- No user action required

### Caveat

This fix has been verified at the unit + e2e level (fake-HOME install + JSON
shape inspection) but **not yet** in a real Claude Code session where the
upgrade path runs end-to-end. The bench evidence supporting the diagnostic
(zero PreToolUse:Bash events, zero cooldown flags, stale `.code-graph/index.db`
mtime) was collected before this fix — a successor session will be the first
real-world confirmation that the registered settings.json entries actually
trigger CC's hook dispatcher. The hook scripts themselves are unchanged from
the pre-fix versions verified in v0.25.0 through v0.31.2; only the
registration channel changed.

## v0.31.2 — dedup plugin MCP catalog when project provides its own + regression gate

Follow-up to v0.31.1's PreToolUse repair. Four small functional cleanups:

### Fixed
- `claude-plugin/scripts/mcp-launcher.js` — when the user's project has its
  own `.mcp.json` registering a `code-graph*` server (the recommended setup
  for dev work on this repo, so usage telemetry lands in the project's
  `.code-graph/usage.jsonl`), the plugin's MCP server now serves a minimal
  "0 tools" JSON-RPC stub instead of registering a duplicate 7-tool catalog.
  Saves ~4-8 KB of context per session and removes the AI's ambiguity
  between the two equivalent namespaces. Env override
  `CODE_GRAPH_FORCE_PLUGIN_MCP=1` bypasses the dedup gate.
- `claude-plugin/scripts/pre-edit-guide.js` — `code-graph-mcp` binary is now
  resolved via `findBinary()` (consistent with `mcp-launcher.js` and
  `incremental-index.js`) instead of a bare PATH lookup, so npm-global
  installs on systems where the global bin dir isn't on PATH for non-login
  shells (the failure mode behind mem #8187) no longer leave the hook
  silently inert.

### Added
- `claude-plugin/scripts/hooks.test.js` (5 tests) — regression gate that
  parses `hooks/hooks.json` and rejects matchers containing the expression
  DSL tokens (`==`, `tool `, `||`, `&&`, `"`) that caused the v0.25.0 →
  v0.31.0 silent breakage. Verified by a negative test (re-injecting the
  broken matcher makes the gate fail with a concrete diagnostic). Wired
  into `.github/workflows/ci.yml`'s `plugin-tests` job alongside
  `pre-edit-guide.test.js`, which was also missing from the CI matrix.

## v0.31.1 — fix: PreToolUse hooks never fired

Two compounding bugs caused `PreToolUse:Edit`/`Bash`/`Read` hooks and the
`PostToolUse:Write|Edit` incremental-index hook to be **registered but
silently inert** since v0.25.0. Replaying 147 historical `.rs` Edits across
5 sessions confirms the regression — none of them produced hook output,
even on functions with 30+ direct callers (`conn`, `lock_or_recover`).

### Fixed
- `claude-plugin/hooks/hooks.json` — matchers used the expression-style
  syntax `"tool == \"Edit\""`, but Claude Code's `matcher` field is a literal
  string / pipe-list / regex matched against the tool name. Switched to
  `"Edit"`, `"Bash"`, `"Read"`, and `"Write|Edit"`.
- `claude-plugin/scripts/pre-edit-guide.js` — `code-graph-mcp impact` was
  called without `--file`, so common short symbol names (`open`, `new`,
  `from`, `parse`, `init`) hit the ambiguous-symbol code path and returned
  `{"error": "..."}` instead of caller data. The hook then read
  `direct_callers || 0` from the error object and silently exited at the
  `< 1` gate. Now passes `--file <relative-path>` (the indexer stores
  repo-relative paths; absolute paths return 0 matches) and explicitly
  handles `error`-keyed responses.

Measured before/after on the same 147 historical Edits, impact-injection
fires went from **0 / 147 (0%)** to **37 / 147 (25%)**. Test-only sessions
(routing_bench.rs heavy) stay low by design — bench helpers have no
production callers, so silent-skip is correct there.

## v0.31.0 — Multi-account isolation: honor `CLAUDE_CONFIG_DIR`

Closes [#20](https://github.com/sdsrss/code-graph-mcp/issues/20). Users with
multiple Claude Code accounts (personal vs work) set `CLAUDE_CONFIG_DIR` to
keep their `settings.json`, `plugins/`, and `projects/` separate. The plugin
previously hardcoded `~/.claude/` across ~15 call sites, so for any account
running with the override:

- hook registrations were written to a file Claude Code did not read,
- adoption files / MEMORY.md sentinels landed in the wrong project dir,
- cache cleanup and `installed_plugins.json` writes pointed at the default
  install, not the configured one.

Net effect: the plugin was effectively broken under multi-account isolation.

### Fixed
- New shared helper `claude-plugin/scripts/claude-config.js` exposes
  `claudeHome()` (returns `process.env.CLAUDE_CONFIG_DIR || ~/.claude`,
  re-read on every call).
- `lifecycle.js`, `auto-update.js`, `doctor.js`, `session-init.js`,
  `adopt.js` now route all `~/.claude/...` paths through the helper.
- `adopt.js: memoryDir()` keeps its `(cwd, home)` signature for back-compat;
  `CLAUDE_CONFIG_DIR` simply overrides the `home + .claude` join.
- `adopt.js: isPluginModeInstall()` matches both the legacy
  `~/.claude/plugins/` marker and `CLAUDE_CONFIG_DIR/plugins/`.

### Tests
- New `claude-config.test.js`: env-var resolution + empty-string fallback.
- `adopt.test.js`: `memoryDir` + `isPluginModeInstall` honor the override.
- `lifecycle.e2e.test.js`: full install subprocess writes into
  `CLAUDE_CONFIG_DIR` and never touches default `~/.claude/`.

No default-behavior change: when `CLAUDE_CONFIG_DIR` is unset, every path
resolves exactly as before.

## v0.30.0 — UX pass: 16 silent-failure / misleading-feedback fixes

Four rounds of end-to-end dogfooding (fresh-project workflows, MCP stdio
fuzz, IO edge cases) surfaced 16 places where the tool silently swallowed
errors, gave misleading guidance, or returned empty results indistinguishable
from a successful no-op. All four commits are pure UX/correctness — no API
removals, no behavior changes for happy paths.

### Fixed — CLI feedback honesty
- **`incremental-index` now reports file deletions** (`src/indexer/pipeline/`):
  `IndexResult` gained `files_deleted`; the summary line reads "N files
  updated, M files removed, K nodes created" when M > 0. Previously a
  delete-only incremental said "0 files updated, 0 nodes created" — looked
  like a no-op even when nodes/edges were cascade-deleted.
- **`deps <file>` distinguishes missing-file from no-imports**
  (`src/cli.rs`): pre-check `project_root.join(file_path).is_file()` and
  report "File not found" before the barrel-scan fallback fires.
- **`deps <file>` surfaces unresolved imports** (`src/cli.rs`): when all
  edges point to `<external>` or cross-language targets and get filtered
  out, render "(no resolved deps; N unresolved outgoing/incoming)" instead
  of just printing the bare filename. JSON gets matching `unresolved_*`
  fields.
- **`callgraph` suppresses no-op fuzzy resolve** (`src/cli.rs`): no longer
  prints `[code-graph] Resolved 'X' → 'X'` when fuzzy matched the same
  input verbatim.
- **`incremental-index` in non-git dir prints why it skipped** (`src/main.rs`):
  the silent-bail guard for multi-repo workspace parents now emits "Skipping
  index: no .git anchor or existing .code-graph/ at …" in non-quiet mode.
  `--quiet` (hook path) remains silent — PostToolUse contract preserved.
- **`map` on empty project replaces dangling header** (`src/cli.rs`):
  "(empty project — no indexed source files)" instead of a lone "Modules:".

### Fixed — `health-check` / `doctor` contract
- **`health-check --json` emits valid JSON even with no index** (`src/cli.rs`):
  returns `{healthy:false, reason:"no_index", issue:...}` + exit 0 instead
  of bailing with stderr "No index found" + exit 1. Non-JSON mode keeps the
  stderr+exit-1 contract for interactive callers.
- **`doctor` routes "No index found" to the index-empty fix path**
  (`claude-plugin/scripts/doctor.js`): old behavior labeled it `binary-broken`
  (no fix handler) and reported "Fixing… 0/1 addressed". Now detects the
  `reason:"no_index"` JSON flag and runs `incremental-index` automatically.

### Fixed — MCP empty-string args
- **`get_call_graph` / `find_references` / `get_ast_node` / `find_similar_code`
  reject empty/whitespace `symbol_name`**: previously `symbol_name=""` fell
  through to fuzzy resolve and silently matched a random Unique() candidate
  (seen returning `function:"x"` from a DB with one fn called `x`).
- **`get_call_graph` / `find_references` treat empty `file_path` as absent**:
  `Some("")` used to filter to a nonexistent path, producing "Symbol 'x'
  not found in file ''" or silent empty-edge results.
- **`trace_http_chain(route_path="")` rejects upfront**: empty pattern used
  to substring-match every route, returning "no routes found" indistinguishable
  from a typo.
- **`dependency_graph(file_path="")` rejects upfront**: empty path used to
  trigger the "looks like a directory" hint at "", giving wrong guidance.

### Fixed — path & filesystem hardening
- **`module_overview` rejects absolute paths / `../` traversal / Windows
  drive letters** (`src/mcp/server/tools/overview.rs`): the index stores
  relative paths from project root, so `/etc`, `../foo`, `C:\Windows` will
  never match. Old behavior silently returned `0 files`; now errors with
  actionable message.
- **`snapshot create --out` pre-flights the target path** (`src/cli.rs`):
  rejects dir-as-out (`/tmp/`) and missing-parent (`/nonexistent/snap.db`)
  before SQLite VACUUM INTO leaks its raw "unable to open database file"
  error chain.
- **`scan_directory` tolerates per-entry walk errors**
  (`src/indexer/merkle.rs`): a single unreadable subdir (`chmod 000` build
  artifact, restricted mount, broken symlink target) used to abort the
  whole rebuild with `Permission denied (os error 13)`. Now skip-and-warn
  via tracing — readable files still get indexed.

### Notes
- 526 tests pass, `cargo +1.95.0 clippy --all-targets -- -D warnings` clean
  on both default and `--no-default-features`.
- No public API removals. New `IndexResult.files_deleted` field is purely
  additive. MCP tool schemas unchanged.

## v0.29.0 — edge resolution precision pass (12 silent-failure fixes)

Five rounds of end-to-end dogfooding surfaced 12 silent-failure / mis-attribution
bugs across the parser → resolver → MCP/CLI surface. Net effect on the project's
own index: **dead-code false positives dropped 21 → 6** (all remaining 6 are
documented design limitations: receiver `obj.method()` calls, type-as-field
references, cross-file constant access), **edges restored 3030 → 3266+** (+8%
real call relations recovered). The user-visible behavior shift is the
TypeScript `return_type` shape (no longer leaks `": "` prefix) — minor bump
flags that.

### Fixed — parser
- **TypeScript `return_type` strips leading `:`** (`src/parser/treesitter.rs`):
  `extract_signature_info` read the tree-sitter `return_type` field verbatim,
  which on TS/JS is a `type_annotation` node whose text starts with `:`. Stored
  values were `": string"` not `"string"`; signatures rendered `(name: string)
  -> : string`. Now trimmed at extraction — Python / Rust / Go produce clean
  values unchanged (no-op when first char isn't `:`).
- **Rust generic trait impl emits no method edges** (`src/parser/relations/rust.rs`,
  `src/parser/treesitter.rs`): `impl<'a, W: Write> Write for CapWriter<'a, W>`
  stored `source_name = "CapWriter<'a, W>"` from `type_node` verbatim; Phase 2
  source resolution exact-name-matched against `"CapWriter"` and dropped every
  method edge. Every generic trait impl's methods looked dead. Now strips
  generic params at both extraction sites (relations + treesitter qualified_name).
- **Method-level implements edges fan out within a file**
  (`src/parser/relations/mod.rs`, `src/indexer/pipeline/index_files.rs`): N
  structs each `impl Trait for StructN` in one file produced N×N×methods edge
  combinations because the resolver matched bare method names against every
  same-name node in the file. Parser now stamps `{"q":"impl_method","v":"<Type>"}`;
  resolver filters method candidates by `qualified_name LIKE "<Type>.%"` via
  the existing `self_filter_candidates`.

### Fixed — resolver
- **Same-file targets dropped under Path qualifier**
  (`src/indexer/pipeline/index_files.rs`): the `CalleeMeta::Path` branch
  excluded `local_ids` before applying the path filter, contradicting the
  spec's "same-file matches take precedence". `Foo::helper()` in the same
  file as `impl Foo { fn helper }` produced zero call edges. Now includes
  same-file candidates in the path-filtered pool.
- **`path_filter_candidates` misses single-file Rust mods**
  (`src/indexer/pipeline/resolve.rs`): only matched `/<seg>/` or `<seg>/`
  directory boundaries, so `crate::domain::foo()` resolving into `src/domain.rs`
  (single-file mod, no `domain/` directory) dropped. Now also accepts
  `path.ends_with("/<last_seg>.rs")`. **This single fix eliminated 14 of the
  20 dead-code false positives** (`normalize_type_filter`, all `migrate_v*_to_v*`,
  `create_tables_sql`, etc.).

### Fixed — MCP / CLI surface validation
- **`ast_search` / `semantic_code_search` invalid `type` filter silently empty**
  (`src/mcp/server/tools/ast_search.rs`, `src/mcp/server/tools/search.rs`,
  `src/cli.rs`): `normalize_type_filter("INVALID")` returns empty `Vec`;
  `.any()` on empty returns false → every node filtered out → "No results"
  with exit 0. Now bails up-front with the valid-values list.
- **`find_references` invalid `relation` silently falls back to `all`**
  (`src/mcp/server/tools/refs.rs`): `match relation { "calls"=>..., _=>None }`
  treated `"call"` (typo) identical to `"all"`. Now explicit `"all" => None`
  + Err on anything else.
- **`get_call_graph` `symbol_name` + `route_path` silently uses `route_path`**
  (`src/mcp/server/tools/callgraph.rs`): schema marks them mutually exclusive
  but impl preferred route_path silently. Now errors with the conflict.
- **`module_overview path=""` returns the whole project**
  (`src/mcp/server/tools/overview.rs`, `src/cli.rs`): empty string normalized
  to the same "match all" prefix as `"."`. Common variable-substitution bug
  (`process.env.X || ""` → dumps entire repo). Now errors; `"."` still works
  as the documented match-all alias.

### Fixed — FTS5 / snapshot / concurrency
- **FTS5 reserved-word queries leak raw syntax error**
  (`src/storage/queries/search.rs`): `semantic_code_search query="NOT"`
  returned `Error: fts5: syntax error near "NOT"`. Each sanitized token is
  now wrapped in `"…"` (FTS5 phrase syntax) — equivalent for normal tokens,
  defuses the NOT/AND/OR/NEAR keywords.
- **`snapshot inspect` silently succeeds on truncated SQLite**
  (`src/snapshot/mod.rs`): a 100-byte file starting with `"SQLite format 3\0"`
  magic passed the header check; `Database::open` initialized empty schema;
  every meta lookup returned None → defaults. Inspect returned a fake "empty
  valid snapshot" with zeroed fields. Now bails when all meta is missing.
- **Concurrent `incremental-index` shows cryptic `Error code 5: database is locked`**
  (`src/cli.rs`): two CLI processes racing on `.code-graph/index.db` got raw
  rusqlite SQLITE_BUSY. New `wrap_busy()` translates to "Another `code-graph-mcp`
  process is writing... Wait for it to finish, then retry." while keeping the
  original error for debug.

### Regression coverage
- `tests/cli_e2e.rs::test_cli_ast_search_invalid_type` /
  `test_cli_search_invalid_node_type` / `test_cli_overview_empty_path_errors`.
- `tests/integration.rs::test_module_overview_empty_path_errors` /
  `test_find_references_invalid_relation_errors` /
  `test_get_call_graph_symbol_and_route_mutually_exclusive` /
  `test_fts5_keyword_query_does_not_leak_syntax_error`.
- `tests/integration_call_qualifier.rs::path_qualifier_keeps_same_file_target` /
  `path_qualifier_resolves_single_file_rust_mod` /
  `same_file_generic_impl_method_edges_dont_fan_out`.
- `src/snapshot/tests.rs::inspect_rejects_truncated_sqlite_header`.
- `src/parser/relations/tests.rs::test_extract_rust_impl_trait_generic_type_strips_params`.

353+67+50+9+19 tests pass; `cargo +1.95.0 clippy --no-default-features` and
`--all-targets` both clean under `-D warnings`.

### Known limitations preserved
Six dead-code entries remain after this pass and are documented design gaps,
not bugs:
- Receiver method calls (`obj.method()` where receiver type isn't statically
  inferable): `validate`, `file_exists`, `db()`.
- Type-as-field references (`pub foo: SomeStruct`): `SnapshotConfig`.
- Cross-file constant access via Path qualifier (extractor doesn't emit edges
  for non-call identifier references): `PROD_SOURCE_FILTER_AND`,
  `TEST_SOURCE_FILTER_OR`.

Fixing these requires either a Rust type inferencer or extending edge
extraction to non-call identifier references — out of scope for this release.

## v0.28.0 — trigger-rate gate + hook coverage expansion

Data-driven release based on a 7-day usage audit (2026-05-12 → 2026-05-14,
141 main sessions): **1972 raw `bash grep` vs 47 code-graph calls** (the 47
all came from one dogfooding session — zero organic cg invocations across
20 sampled real sessions). Recall bench has stayed at P@1=100% throughout
2026, so the gap isn't "which tool" but "is the model reaching for a tool
at all". This release adds a measurement axis for that and widens three
hook-layer interventions that were missing real-world surface.

### Added
- **`BenchMode::TriggerRate`** (`tests/routing_bench.rs`): new bench mode with
  `tool_choice: auto` (recall benches use `required` / `any`). Surfaces the
  "model returned no tool / picked Bash" failure mode that forced-tool benches
  hide. 12-entry hard `TRIGGER_ORACLE` (10 `__CG__` + 2 `__DECOY__`) covering
  pure symptom prompts ("今天的报告数据不准"), misleading-grep framing
  ("用 grep 找一下…"), answer-flavored guesses ("应该是 cleanup 那段没等完吧"),
  and generic feature refs. New `bash_decoy()` (TriggerRate-only — kept out of
  ContextRich so v0.17.x baselines stay comparable), `compute_trigger_metrics`,
  `matches_trigger_class`, `DECOY_NAMES`. Baseline on Sonnet 4.5: **60.0%
  trigger (6/10) · 30.0% no-tool (3/10) · 100% decoy boundary · 0% leak**.
  Run with `ROUTING_BENCH_MODE=trigger-rate`.
- **PreToolUse:Read fanout detector** (`claude-plugin/scripts/pre-read-guide.js`):
  new hook fires on the 5th Read into the same source dir within a 30-min
  window. 7d audit found 16 sessions with 5+ Reads into one dir (top: 13
  reads into `backend/app/services/`) — Claude burns ~500-2000 tokens per
  Read when one `module_overview path=X/` returns symbols + caller counts in
  ~600. Per-cwd state in `/tmp/.code-graph-readfan-<hash>.json`, 5-min
  per-dir cooldown, 30-min state TTL. One-line hint suggests
  `code-graph-mcp overview <dir>/` or MCP `module_overview path=<dir>`. New
  `tool == "Read"` matcher in `claude-plugin/hooks/hooks.json` (3s timeout).
- **UserPromptSubmit symptom-hint fallback** (`claude-plugin/scripts/user-prompt-context.js`):
  24-entry `SYMPTOM_PATTERNS` (`/bug/i` · `/crash/i` · `/not work/i` ·
  `/why does/i` · `挂了` · `失败了` · `卡死` · `不准` · `缺失` · `又失败` ·
  `为什么` · `哪里[\s\S]{0,5}(?:错|有问题|不对)` · …). When the 4 existing
  channels (intent / qualified symbol / file path / any symbol) all return
  no actionable query AND the message has symptom phrasing AND a 10-min
  cooldown is cold, `determineQueryType` returns `{ type: 'symptom-hint' }`
  and `runMain` emits ONE LINE of prose (NO CLI execution — Phase A's lesson:
  heavy structured injection backfires on borderline prompts). Hint format:
  `[code-graph:hint] indexed repo — for vague-symptom prompts, try
  \`semantic_code_search "<symptom>"\` or \`module_overview <suspected-dir>\`
  to surface candidate code structurally. Skip if not searching code.`
  Actionable paths (impact / overview / callgraph / search) still take
  precedence; signature gains a 5th `message = ''` param keeping all legacy
  callers backward-compat.

### Changed
- **`pre-grep-guide.js` SRC_PATH expansion** (`claude-plugin/scripts/pre-grep-guide.js`):
  added 20 backend / DDD / web convention prefixes — `backend frontend services
  models domain controllers views handlers middleware routes repositories
  entities migrations tasks jobs workers features modules api web`. Root cause:
  the v0.21+ regex required prefix terms in `(src|tests|lib|…|app|server|
  client)/` to be preceded by whitespace / quote / start-of-string, so the
  dominant daagu-style miss `backend/app/services/…` never fired (`app/` sat
  after `backend/`). Audit shows 5 of the worst-offender sessions used exactly
  this layout. Generic terms (`core` / `utils` / `shared` / `common` / `types`)
  deliberately omitted — too many non-code contexts. 10 new positive regression
  tests pass; 3 precision guards (`web.config` / `node_modules/` / `docs/*.md`)
  stay false.

### Fixed
- `doc_lazy_continuation` lint on `tests/routing_bench.rs:704` —
  `Grep \n /// + Read decoys` wrapped such that the continuation line began
  with `+`, which clippy 1.95 reads as a markdown bullet. Reworded to
  `the Grep/Read decoys alone don't model…`. Both clippy passes
  (`--no-default-features` and `--all-targets`) clean.

### Failed experiment (reverted in-session, kept for lesson)
- **Phase A — "DO NOT use when X (Grep/Read/Bash)" steering in tool descriptions**:
  Hypothesis (mem #8234 / `feedback_negative_steering_backfire.md`): mem-style
  negative steering should lift trigger rate. **Bench falsified the hypothesis
  before commit**: TriggerRate baseline **60% → 40%** (3-run unanimous), two
  brand-new misses (`今天的报告数据不准 → None`, `test 又挂了 → Bash`), and the
  target miss (`用 grep 找一下…` → Grep) wasn't fixed. Root cause: clauses like
  "`INSTEAD OF Grep`" + "`DO NOT for: literals (Grep)`" are self-contradictory
  — when the user hints "grep" the second clause licenses the wrong tool. New
  rule in `feedback_negative_steering_backfire.md`: negative steering is safe
  only between cg-vs-cg, never pointing at native decoys. Revert via
  `git restore`, no commit hit `main`.

### Regression coverage
- `tests/routing_bench.rs::scoring_tests::compute_trigger_metrics_*` (4
  tests covering all-correct / mixed / empty-oracle / missing-pick), plus
  `decoy_tests::bash_decoy_has_required_fields_and_anchor`,
  `mode_tests::detect_mode_trigger_rate`, `trigger_oracle_well_formed`.
- `claude-plugin/scripts/pre-read-guide.test.js` — 34 tests covering source
  extension whitelist, dir extraction, cooldown / threshold logic, state
  load+save round-trip, TTL pruning, malformed-JSON tolerance, hint shape,
  silenced env, integrated 5-read flow.
- `claude-plugin/scripts/pre-grep-guide.test.js` — 10 new positive prefix
  regressions (`backend/app/services/`, `services/scheduler/`, `models/`,
  `controllers/`, `domain/`, `handlers/`, `migrations/`, `features/`,
  `api/`, `frontend/`) + 3 precision guards. 52/52 pass.
- `claude-plugin/scripts/user-prompt-context.test.js` — 18 new tests
  (`hasSymptom` positive + precision, `determineQueryType` symptom-fallback
  with cooldown + precedence + backward-compat, integration via `analyze`).
  99/99 pass.

### Rationale anchor
The strategy: don't try to argue Claude into picking better tools at the
description level — bench has already saturated there. Instead measure
"trigger rate" as a separate axis (Phase 0/F), widen the existing Bash and
Read hooks to fire on real-world layouts (Phases C/D), and add a low-noise
symptom-only fallback to the UPS hook for the bug-shaped prompts that
slipped through the existing 4 channels (Phase E). Phase A demonstrated
that re-trying the description angle backfires. Phase B (MCP `instructions`
symptom-mapping line) deliberately deferred until the C/D/E real-world
fire-rate data justifies it.

Surface: plugin-shipped behavior — users with adopted CG indexes will see
new `[code-graph]` hints on `bash grep -rn backend/services/...`, 5+ Reads
into the same source dir, and bug-flavored prompts that previously got
zero UPS injection. Escape hatch unchanged: `CODE_GRAPH_QUIET_HOOKS=1`
(env in `~/.claude/settings.json`).

Pre-push parity: `cargo +1.95.0 clippy --no-default-features --all-targets
-- -D warnings` clean on both targets; `node --test claude-plugin/scripts/
*.test.js` 251 tests pass (52 pre-grep + 34 pre-read + 99 user-prompt +
66 other); `cargo test` full suite passes.

## v0.27.0 — Python call relations + dead-code truncation guard

### Fixed
- **Python call-edge extraction (P0)**: `src/parser/relations/mod.rs` only
  matched the `call_expression` arm plus a Ruby-guarded `call` arm;
  tree-sitter-python emits `call` nodes that fell through to a no-op, so
  every `.py` file produced **0 call edges** despite README and `CLAUDE.md`
  documenting Python as Full tier. Knock-on effects: `module_overview`
  showed `caller_count=0` for every Python symbol, `find_dead_code`
  over-reported orphans, and `impact_analysis` / `get_call_graph` /
  `find_references` returned wrong results for any Python query. New
  `"call" if config.name == "python"` arm, plus `helpers::extract_callee_name`
  now treats Python `attribute` (field name `attribute`, not `property` /
  `field`) the same as JS `member_expression`. Reindexing this repo:
  **0 → 2969 total edges**; `scripts/analyze-search-queries.py` now
  produces non-zero caller counts.
- `cmd_overview` JSON empty contract (`src/cli.rs`): `overview . --json`
  returned `[]` on stdout but exited 1 with `Error: [code-graph] No symbols
  found under: .` smeared on stderr by `anyhow::bail!`, breaking log
  consumers piping stdout to `jq`. Now `.` normalizes to project root
  (mirroring MCP `tool_module_overview`); JSON-mode empty path emits a
  clean `eprintln!` + `exit(1)` so stderr stays free of the anyhow `Error:`
  prefix.
- `find_dead_code` truncation guard (`src/storage/queries/dead_code.rs`):
  when `CODE_GRAPH_MAX_CODE_LEN` caps a long function's stored body,
  references in the truncated tail were invisible to the SQL `instr`
  fallback, falsely flagging callback targets as dead. New OR clause
  detects truncated hosts via **two co-signals** — trailing `...` sentinel
  **and** declared-span > stored-newline-count by 5+ lines — and gives
  same-file names benefit of the doubt. Single signal alone is rejected
  (Python `def stub(): ...` has the sentinel without a span gap; compact
  test fixtures have a gap without the sentinel). Default 4 KB limit means
  real-world activation is rare.
- `snapshot::{mod,install}.rs`: three best-effort `git` invocations
  (`rev-parse HEAD` / `remote get-url origin` / `cat-file -e`) now redirect
  stderr to `Stdio::null()`. Previously `fatal: not a git repository`
  leaked into `cargo test` output and `snapshot create` runs on non-git
  roots.

### Removed
- `LanguageConfig.call_node_kind` field — defined but never read. The call
  dispatcher in `relations/mod.rs` uses hardcoded literal match arms because
  per-language call shapes diverge too far for one string to drive them
  (Ruby's `call` doubles as `require`; PHP splits into three kinds; C# uses
  `invocation_expression`; Bash uses `command`; Python's `call` carries an
  `attribute` field for method names). Keeping the field misled contributors
  into thinking new languages could be added by editing config alone — that
  was the Python regression's root cause. Field + assertions removed; the
  dispatcher entry now carries a comment enumerating every language's
  call-node kind so the trap is visible at the dispatch site.

### Regression coverage
- `src/parser/relations/tests.rs::test_extract_python_bare_call`,
  `::test_extract_python_method_call` — Python `call` arm + `attribute`
  callee.
- `src/storage/queries/dead_code.rs::tests::test_find_dead_code_skips_when_caller_content_truncated`
  — truncation guard.
- `tests/cli_e2e.rs::test_cli_overview_dot_means_project_root`,
  `::test_cli_overview_json_empty_no_anyhow_prefix` — overview path
  normalization + JSON empty-stderr cleanliness.

### Rationale anchor
Autonomous iteration loop (4 rounds): 1 P0 + 4 P1 + 1 P2 surfaced, all
fixed. Each change stays inside internal surface — no Δ-contract on MCP
tool schemas, CLI flags published to npm users, or SQLite schema.
Pre-push parity: `cargo +1.95.0 clippy --no-default-features --all-targets -- -D warnings`
clean on both targets; 467 tests pass.

## v0.26.0 — UserPromptSubmit context push default ON + trigger hints

### Changed
- `claude-plugin/scripts/user-prompt-context.js`: `computeQuietHooks` default
  flipped back to **noisy** (push ON). The v0.21 opt-in flip cited routing-bench
  P@1=100% as evidence the agent already picks tools correctly without push,
  but that bench measures *triage accuracy once the agent has decided to query
  a tool* — not the prior question of whether the agent reaches for a tool at
  all. The real counter-evidence is in `pre-grep-guide.js`'s 15-day baseline:
  **429 raw `grep` vs 191 functional CLI calls on the same indexed source tree
  (~13× pre-training bias toward grep)**. Push is the corrective. Per-type
  cooldowns (impact 30s / overview 5min / callgraph 60s / search 60s) cap
  frequency; the 8-char message floor + `shouldSkip` filter keep confirmation
  chatter silent. Escape hatch: `CODE_GRAPH_QUIET_HOOKS=1`.
- `claude-plugin/scripts/pre-edit-guide.js`: caller threshold lowered from
  `directCallers < 2` to `< 1`. Editing any function with one or more callers
  now surfaces the one-line impact summary; the per-symbol 2-minute cooldown
  is unchanged so the noise floor stays the same.
- SessionStart `project_map` injection (`session-init.js`) **stays default
  OFF** — that hook is a static dump duplicated by `MEMORY.md`'s decision
  table; this hook is a reactive trigger reminder. The two defaults are
  intentionally asymmetric.

### Added
- `src/mcp/server/mod.rs` MCP `instructions` field gains one line of explicit
  scenario triggers: `"who calls X?" → get_call_graph; "impact of X?" or
  before editing a fn → get_ast_node include_impact=true; concept search
  without an exact symbol → semantic_code_search`. Compile-time
  `assert!(NOISY.len() <= 1500)` budget guard unchanged (now 772 / 1500 bytes).
- Project `CLAUDE.md` "Code Graph Integration" section replaced with a 5-row
  trigger table (who calls / impact / module overview / concept search / HTTP
  route) — `CLAUDE.md` is loaded every session, higher priority than the
  invited-memory path in `MEMORY.md`.
- `claude-plugin/templates/plugin_code_graph_mcp.md` clarifies the asymmetric
  hook defaults and lists `CODE_GRAPH_QUIET_HOOKS=1` as the context-push
  escape hatch alongside the existing `VERBOSE_HOOKS` / `QUIET_HOOKS=0` flags.

### Rationale anchor
- mem #8234 documents that hook content has **bounded leverage** when the
  current bench corpus is saturated (Sonnet 4.5 hits P@1=100%); bench is the
  right oracle for tool-description boundary disambiguation, not for
  server-prelude / hook-content tuning. This release therefore lands without
  a fresh routing-bench cycle — the changes are all hook-content surface.

### Verification
- `cargo check`: clean (compile-time `assert!(len <= 1500)` on
  `NOISY` instructions string holds; final length ~772 bytes).
- `node --test claude-plugin/scripts/user-prompt-context.test.js`:
  77/77 pass — six `computeQuietHooks` priority-chain cases rewritten for
  the default-noisy invariant; one e2e check kept on the `=1` escape hatch.
- No change to `routing_bench.rs` corpus; intentionally skipped per mem #8234.

### Migration
- Existing users on default env will start seeing `[code-graph:impact|
  overview|callgraph|search]` push lines on intent-matching prompts. Set
  `CODE_GRAPH_QUIET_HOOKS=1` in `~/.claude/settings.json` env to opt out.
- Adopted projects: the `plugin_code_graph_mcp.md` template auto-refreshes on
  next SessionStart (unless `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` is set).
- No data-migration, no schema change, no MCP tool API change.

## v0.25.1 — findBinary disk cache version-check

### Fixed
- `claude-plugin/scripts/find-binary.js`: disk cache (`~/.cache/code-graph/binary-path`)
  now validates the cached binary's `--version` against the package
  version before returning it. Previously the cache short-circuit at
  `findBinary()` entry only checked `isNativeBinary(cached)` (file
  exists + right basename) — once a stale path got written, it
  shadowed every newer binary on the system **forever**. Symptom on
  this dev machine: cache pinned `bin/code-graph-mcp` v0.5.28 (the
  un-tracked `scripts/copy-binary.js` artifact from March 17) while
  `~/.cargo/bin/code-graph-mcp` was the freshly installed v0.25.0,
  causing `incremental-index.test.js` to fail mid-pre-commit hook with
  the older binary's pre-v0.16.9 hard-bail behavior.

### How the bug bites end users
- Asymmetric version-check coverage. Auto-update cache at
  `find-binary.js:184-188` was already version-gated (mem #8187 fixed
  three install-chain bugs but landed only on the `~/.cache/.../bin/`
  branch). Disk cache `binary-path` — the entry-level fast-path that
  runs on **every** hook tick — had no equivalent gate. After
  `npm install -g` of an updated platform pkg, or any path drift in
  the platform-pkg layout, the disk cache would keep returning the
  pre-update binary until a user manually `rm`-ed the cache file.
- New `isCachedBinaryFresh(cachedPath, pkgVersion)` helper. Permissive
  on unknown values (missing pkg version, unreadable binary `--version`
  output) → trust the cache (don't refuse the only path we know
  about). Strict only when both versions parse and cached < pkg.

### Verification
- `node --test find-binary.test.js`: 19/19 pass — 11 existing +
  8 new covering THE BUG case (cached `0.5.28` vs pkg `0.25.0` →
  invalidate), equal versions, newer cache, missing pkg version
  permissive, unreadable binary permissive, non-existent path,
  null/undefined input, basename mismatch.
- `node --test lifecycle.test.js`: 12/12 — schema regression-clean.
- `cargo +1.95.0 clippy --no-default-features --all-targets -D warnings`: 0.

### Migration
- No user action needed. First findBinary call after upgrade detects
  stale cache (older than 0.25.1 cached binary) → invalidates →
  falls through to the rest of the discovery chain (target/release →
  auto-update cache → platform pkg → bundled → cargo install → PATH).
- For users on the dev branch with manually-recorded cache paths:
  `rm ~/.cache/code-graph/binary-path` triggers the same fresh walk.

## v0.25.0 — PreToolUse:Bash hint hook (raw-grep → cg CLI nudge)

### Added
- `claude-plugin/scripts/pre-grep-guide.js`: new PreToolUse:Bash hook that
  detects raw `grep`/`rg`/`ag` invocations on the indexed source tree
  (`src/`, `tests/`, `lib/`, `scripts/`, `claude-plugin/`, `tools/`, `pkg/`,
  `cmd/`, `internal/`, `app/`, `components/`, `server/`, `client/`,
  `crates/`, `packages/`) and emits a 6-line hint pointing at
  `code-graph-mcp grep / ast-search / callgraph / show`. Fires only on
  bare grep at command HEAD (pipe-greps like `cargo test | grep FAILED`
  are output filters and skipped). Per-command-hash cooldown 60s prevents
  repeat noise. Registered in `claude-plugin/hooks/hooks.json` with
  3s timeout.

### Motivation
- 15-day session telemetry (78 sessions / 13.5K assistant turns) showed
  429 raw `grep -rn` calls on source trees vs 437 `code-graph-mcp`
  invocations — ~1:1 overall but with severe variance (3 work days at
  10:0 or worse against `code-graph-mcp`, today's 05-11 at 39:10).
  Pre-training bias gives `grep -rn pattern src/` an enormous default
  weight; tool descriptions alone can route correctly (routing_bench
  Opus 4.7 P@1=95.5% in tool-only mode) but don't surface the indexed
  alternative when Claude isn't already deciding between tools. This
  hook closes the loop at the Bash entry point — same shape as the
  existing PreToolUse:Edit (`pre-edit-guide.js`) impact-summary hook.

### Verification
- `node --test claude-plugin/scripts/pre-grep-guide.test.js`: 35/35 pass.
  Covers fire cases (grep/rg/ag on src + tests + lib + claude-plugin,
  alternation patterns, env-prefixed, head/tail pipes downstream),
  skip cases (pipe-grep output filters, code-graph-mcp self-invocation,
  config-only targets like Cargo.toml/.gitignore/CHANGELOG.md, non-search
  tools like ls/cat/git/find), and 5 regression cases lifted verbatim
  from 2026-05-11 session telemetry.
- `node --test claude-plugin/scripts/lifecycle.test.js`: 12/12 pass —
  hooks.json schema change accepted by lifecycle's hook-identity matcher.
- E2E sanity: piping `{"tool_input":{"command":"grep -rn ... src/storage/"}}`
  through `pre-grep-guide.js` emits the 6-line hint on first invocation,
  silent on repeat (cooldown verified), silent on `cargo test | grep FAILED`
  (pipe-grep correctly skipped).
- Bench unaffected: routing_bench is tool-only mode (forced
  `tool_choice=any`), Bash hook injection happens outside that path —
  no P@1 regression possible.

### Migration
- Plugin SessionStart auto-updates the hook registration via
  `${CLAUDE_PLUGIN_ROOT}` path indirection. Disable per-session with
  `CODE_GRAPH_QUIET_HOOKS=1` (already gates the whole hook tier).
  No `.code-graph/index.db` in CWD → hook exits silently regardless.

## v0.24.1 — Adoption tag specificity fix

### Fixed
- adopt: MEMORY.md index-line tags renamed to MCP-tool-aligned multi-word
  form (`impact-analysis`, `find-references`, `module-overview`,
  `semantic-search`, `dependency-graph`, `trace-http-chain`, `http-route`,
  `find-similar-code`). Previous single-word tags (`impact`, `refs`,
  `overview`, `semantic`, `deps`, `trace`, `route`, `similar`) collided
  with release-notes and commit-message prose under the claudemd §11
  `read-the-file` hook's word-boundary + 0-2 char declension regex,
  producing false-positive denies on prose like "fail-open semantics" or
  "overview of changes". `callgraph`, `ast-search`, `dead-code` retained
  (already multi-word). Affects four index-line variants in
  `claude-plugin/scripts/adopt.js` (generic + web-* / frontend /
  rust-go-python-node) and the Rust drift mirror in
  `tests/routing_bench.rs`.

### Migration
- Existing adopted projects auto-refresh on next plugin SessionStart:
  `needsRefresh` does bytewise compare of MEMORY.md against the new
  `desiredBlock`; `stripSentinelBlock` cleans the old block (still v1
  sentinel — no version bump needed) and the new block is written in
  place. Lock manual edits with `CODE_GRAPH_NO_TEMPLATE_REFRESH=1`.

### Verification
- Hook-regex stress prose: OLD tags 3 FP (`impact`, `overview`,
  `semantics`) → NEW tags 0 FP; legitimate references still match.
- `adopt.test.js`: 66/66 pass. New regression case `stale INDEX_LINE →
  adopt rewrites in place without duplicating sentinel blocks` covers
  the bump-without-strip-extension failure mode (would otherwise leave
  orphan v1 + new v2 blocks).
- `routing_bench index_line_drift_check`: pass (Rust mirror byte-aligned
  with JS source).
- routing_bench context-rich (2026-05-11, OpenRouter sonnet-4.5,
  domain=all, 3-run majority vote, 382s): Recall 41/42 = 97.6%,
  FP-rate 0/10 = 0%, Overall 51/52 = 98.1% — **zero regression** vs
  v0.17.3+pm-desc-dedup baseline (Backend 22/22 = 100% kept; Frontend
  19/20 = 95% kept; same residual path-anchored `src/components/` miss
  unrelated to this change). Confirms tag-rename preserves routing
  signal.

## v0.24.0 — Bare-name call qualifier (Rust)

### Fixed
- callgraph: Rust qualified calls (`Type::method`, `crate::path::fn`,
  `self.method`, `Self::method`, builder chains like `OpenOptions::new().create()`)
  no longer route to unrelated project functions sharing the rightmost name.
  Eliminates phantom callers in `impact_analysis` and `find_dead_code` for
  short-named functions (`new`/`create`/`open`/`from`).
- parser: `impl crate::path::Type { ... }` impl-block type now strips the
  leading path so qualified_name and SelfRecv payloads match (was producing
  `crate::path::Type.method` qualified_names that broke same-type LIKE
  matching).

### Migration
- Existing `.code-graph/` databases keep working (qualifier-aware resolution
  is a no-op when `edges.metadata IS NULL`). Run `code-graph-mcp index --rebuild`
  to populate qualifier metadata on existing Rust files; incremental indexing
  picks it up automatically as files change.

### Verification
- `impact run_full_index`: 36 → 33 transitive callers; the 3 documented
  phantoms (decompress_with_cap, try_acquire_index_lock, from_project_root)
  no longer appear.
- routing_bench P@1: 22/22 (no regression).
- 558 tests pass with default + `--no-default-features`. Clippy clean with
  `--all-features`.

## v0.23.1 — snapshot UX + FTS garbage-query guard

Follow-up enhancements to v0.23.0 snapshot work plus an unrelated
search-quality fix.

`snapshot create --out <path>` now auto-zstd-compresses when `<path>`
ends in `.db.zst` (level 9, matching the producer workflow template).
Raw `.db` output unchanged — the existing two-step `--out foo.db &&
zstd -9 foo.db` flow still works.

`snapshot inspect <file>` now accepts both `.db` and `.db.zst` (format
detected from magic bytes, not extension), so first-time users who run
`snapshot create --out foo.db && snapshot inspect foo.db` get sensible
output instead of zstd's cryptic "Unknown frame descriptor". Garbage or
wrong-format files now produce: "X is not a code-graph snapshot —
expected zstd-compressed (.db.zst) or raw SQLite (.db)". `snapshot
inspect <typo>` also surfaces the file path in the error chain instead
of bare "No such file or directory (os error 2)".

Non-https `[snapshot] url` in `.code-graph.toml` now writes to stderr in
addition to `tracing::warn!`, so users see the rejection on CLI startup
paths that don't install a tracing subscriber.

`fts5_search` no longer OR-fallbacks when the user's single-word query
has zero AND-mode hits AND the original token doesn't appear anywhere
in the FTS index. This was returning noise via camelCase token splits —
a query like `ZzzzNoMatchXyzzz` matched any code containing the literal
`--no-default-features` (split on `-`) or the Rust `match` keyword.
Acronyms like `RRF` are unaffected: the original token *is* indexed, so
OR-fallback runs as before for legitimate recall expansion. Multi-word
queries are unchanged.

## v0.23.0 — shared graph snapshot

Team-shared graph artifact via GitHub Releases. New CLI subcommands
`snapshot create` and `snapshot inspect`. MCP server auto-fetches the
latest published snapshot on first start (when no local index exists) and
falls through to the existing full-index path on any failure — snapshot is
an optimization, not a dependency. Workflow template shipped at
`claude-plugin/templates/code-graph-snapshot.yml`. New CLI
`reindex --from-snapshot` forces a re-fetch. Snapshot status surfaces in
`health-check --json`. Snapshot file is symbols+edges+FTS5 only (no
`node_vectors`) to decouple from embedding model choice. Spec:
`docs/superpowers/specs/2026-05-10-shared-graph-snapshot-design.md`.

## v0.22.2 — index.db sub-header size guard

Defensive hardening for `Database::open` recovery. The existing
`is_corruption_error` retry branch covers files that error on open, but a
main DB file shorter than the SQLite header (100 bytes) can land in
SQLite-version-dependent territory — sometimes treated as fresh, sometimes
silently combined with stale `.wal/.shm` residue from a prior crashed
indexing pass.

The new `sub_header_size_guard` runs at the top of `open_impl` and wipes
the entire main+wal+shm triple whenever the main file exists but is < 100
bytes, so every recovery path starts from the same blank state.

### Why now

Round 2 of the v0.22.x dogfood loop surfaced `health-check` exit codes that
varied across repeated runs against an interrupted indexing state. The
existing recovery branch was deterministic-by-luck — relying on a
particular SQLite version's tolerance for sub-header files. The guard
makes recovery deterministic-by-design.

### Tests

Four new unit tests in `src/storage/db.rs::tests` document the safety
contract: 0-byte main alone, 0-byte main + stale wal/shm, partial-write
under header size, and the regression guard for valid databases. Full
suite: 303 lib + 198 integration = 501 passed, 0 failed.

### Also in this release

- `fix(cli): preserve user --depth in callgraph requested_max_depth`
  (`73cd954`) — CLI no longer clamps `--depth` before passing to the engine;
  the engine's own `CALL_GRAPH_MAX_DEPTH` cap and the `requested_max_depth`
  / `effective_max_depth` envelope fields surface truncation truthfully.

## v0.22.1 — dogfood loop fixes (test/prod boundary + truncation bias)

Five bug fixes from a 5-round structured dogfood pass. All fixes converge on
one root pattern: the test/prod source classification was implemented in five
sites independently, and result truncation in `centralized_compress` was
biased against production callers when source data ordering put tests at
the array head/tail.

### Fixes

- **`get_ast_node` `called_by` post-truncation bias** (`src/mcp/server/tools/ast_node.rs`)
  When `include_references=true include_tests=true`, SQL row order without
  `ORDER BY` clustered test callers at array start/end, and `centralized_compress`
  kept first 10 + last 5 — leaving zero production callers visible for
  test-heavy targets like `conn` (49 prod / 76 test). Stable-sort prod-first
  inside the tool before emitting.

- **`find_references` references post-truncation bias** (`src/mcp/server/tools/refs.rs`)
  Same pattern as above, but worse because `find_references` defaults
  `include_tests=true` (rename audits need test sites). 125-caller targets
  collapsed to a 10-prod-of-cli + 5-tests-of-tests/ window with all
  `src/indexer/`, `src/mcp/`, `src/storage/` prod callers silently dropped.
  Same prod-first stable sort inside the tool.

- **`module_overview` `caller_count` includes test sources** (`src/storage/queries/routes.rs`)
  `get_module_exports` `cc` LEFT JOIN counted every incoming `calls` edge —
  did not filter source-side `is_test`. `parse_code` showed `caller_count=39`
  while `find_references include_tests=false` / `get_ast_node impact` /
  `project_map hot_functions` all reported 0 prod. Aligned with the four
  other prod-only counts via the same source-side filter pattern.

- **`ast_search` ranking includes test sources** (`src/storage/queries/nodes.rs`)
  `get_nodes_with_files_by_filters` `ORDER BY (SELECT COUNT(*) FROM edges …)`
  ranked test-only utility wrappers (e.g. `extract_relations` 0 prod / 64 test)
  above genuinely hot prod symbols. Same source-side filter applied.

- **`find_references` "Symbol not found" for test/bench symbols** (`src/mcp/server/tools/refs.rs`)
  `resolve_fuzzy_name` filters test/bench candidates upstream; previous error
  said "not found" even when the symbol was present. Re-query without the
  filter to detect the "found-but-filtered" case and surface a bypass hint
  with the actual file paths. Unblocks the dead-code → find_references
  reverse-trace flow.

### Internal refactor

`src/storage/queries/{routes,nodes,project_map}.rs` now share a single
SQL filter via `src/domain.rs::prod_source_join_sql()` +
`PROD_SOURCE_FILTER_AND` / `TEST_SOURCE_FILTER_OR`. Five duplicate `LIKE`
chains collapsed to one canonical source. New test/harness directory
conventions only need a single edit going forward.

### Tests

- `tests/mcp_stdio_integration.rs` (new, 245 LOC) — three end-to-end JSON-RPC
  stdio tests against a real spawned `code-graph-mcp serve` subprocess.
  Covers prod-first sort survival across centralized_compress truncation,
  caller_count prod-only correctness, and the new explanatory error message.
  Caught a real gap in the error-message fix during authoring (the
  `FuzzyResolution::NotFound` branch needed the same treatment as
  `Unique`).
- `cargo test --release`: 299 lib + 3 new mcp_stdio_integration + ~194 other
  integration = 496 total, 0 failed (1 pre-existing `#[ignore]`).
- `cargo +1.95.0 clippy --no-default-features -- -D warnings` clean.
- `cargo +1.95.0 clippy --all-targets -- -D warnings` clean.

## v0.22.0 — 三巨头 source-file split (queries / relations / pipeline)

Pure refactor release — zero behavior change, public surface preserved across
all three splits. The three biggest source files (8049 lines as monoliths) are
now decomposed into 26 per-concern submodules so future edits don't need to
load 2000+ lines of context per touch.

### What moved

| Original | Lines | New tree | Files |
|---|---:|---|---:|
| `src/storage/queries.rs` | 2892 | `src/storage/queries/` | 10 |
| `src/parser/relations.rs` | 2783 | `src/parser/relations/` | 9 |
| `src/indexer/pipeline.rs` | 2374 | `src/indexer/pipeline/` | 7 |

Submodule items use `pub(super)`; mod.rs re-exports the items external callers
already depend on. External call sites in `cli.rs`, `mcp/server`, `tests/`,
`benches/`, and `claude-plugin/` need zero edits — paths like
`crate::storage::queries::upsert_file`, `crate::parser::relations::ParsedRelation`,
`crate::indexer::pipeline::run_full_index` continue to resolve.

The three orchestrator-style functions stay whole in their respective `mod.rs`
or `index_files.rs` — `walk_for_relations` (~650 lines) and the Phase-0..3
indexer dispatch (~770 lines) share local state across their match arms /
phases that splitting would either duplicate or thread back via large arg
lists. Splitting per-language inside `walk_for_relations` would lose the
shared `current_scope` / `current_class` propagation; splitting per-phase
inside `index_files` would break the shared `tx` / atomics / `batch_parsed`
/ `name_to_ids` / `global_name_map` state. Both are kept whole deliberately.

### Verification

- `cargo check` clean
- `cargo +1.95.0 clippy --no-default-features -- -D warnings` clean
- `cargo +1.95.0 clippy --all-targets -- -D warnings` clean
- `cargo test --release`: 292 lib + 129 integration = 421 tests, 0 failed
  (1 pre-existing `#[ignore]`)
- Pre-merge CI green on all three PRs (#15, #16, #17)
- Independent code-reviewer subagent passed each split with zero Critical /
  Important issues

### Commit references

- queries.rs: 657a1f9 (#15)
- relations.rs: 2dfbab9 (#16)
- pipeline.rs: aef55b2 (#17)

## v0.21.0 — Opt-in plugin hooks (token discipline) + callgraph caller_count ordering + multi-model routing bench

### Migration notes (read first)

**Two LLM-visible default behaviors flipped to opt-in.** Both have explicit env
opt-out paths; existing users who set the legacy `CODE_GRAPH_QUIET_HOOKS=1` see
no change. Users on default settings will feel the new behavior on next session.

- **`user-prompt-context.js` (UserPromptSubmit hook) — default-quiet.** Per-prompt
  CLI exec was costing 200–500 tokens/turn injecting outline/callgraph context
  the agent would have asked for via MCP itself. v0.20.0 routing-bench backend
  P@1 = 100% on Sonnet 4.5 proves the agent picks the right tool without
  push-injection. Restore the v0.20.0 noisy default: set
  `CODE_GRAPH_VERBOSE_HOOKS=1` in `~/.claude/settings.json` env block. Legacy
  `CODE_GRAPH_QUIET_HOOKS=0` still forces noisy for back-compat.
- **`incremental-index.js` (PostToolUse Edit/Write hook) — default-off.**
  v0.18.0 added query-time `ensure_file_indexed` (single-file hash + sync
  reindex) inside MCP tools that take `file_path`, so the PostToolUse hook
  spawning a fresh process per edit was redundant for the MCP-driven workflow
  and burnt ~80ms cold-start per edit. CLI-only workflows (running
  `code-graph-mcp search` after Bash-side edits without going through MCP)
  need the hook for freshness — opt back in with `CODE_GRAPH_HOOK_INDEX=on`.

The two knobs are independent: setting one does not affect the other. CLI-only
users typically want `CODE_GRAPH_HOOK_INDEX=on` only; users who relied on
per-prompt outline injection want `CODE_GRAPH_VERBOSE_HOOKS=1` only.

One internal-but-user-perceptible change: `get_call_graph` (and the underlying
`get_call_graph_query`) now orders results within each depth by `caller_count
DESC`. Previously ties broke by row order, which silently dropped the most-
relevant subtree under `CALL_GRAPH_ROW_LIMIT` truncation. Hot functions like
`conn` (51 callers + 72 test in this repo) are now guaranteed to surface
their high-connectivity subtrees first. No JSON shape change — only ordering.

### Plugin hook default flips (the headline)

`claude-plugin/scripts/user-prompt-context.js` — replaced 6 mixed-language
intent regex piles with per-keyword weighted patterns under `INTENT_PATTERNS`.
Each (regex, weight) row is testable in isolation; threshold 0.5 + uniform
weight 1.0 preserves the original OR-of-alternatives behavior 1:1. Future
tuning can downweight noisy short keywords (`bug`, `什么`) once false-positive
data accumulates. Maintenance cost: ~150 lines of table vs 6 × 200-char
regexes — the regex form had two prior silent-bug regressions (#5754, #7713).

`computeQuietHooks(env)` priority chain (high → low):

1. `CODE_GRAPH_QUIET_HOOKS=0` → forced noisy (legacy)
2. `CODE_GRAPH_QUIET_HOOKS=1` → forced quiet (legacy)
3. `CODE_GRAPH_VERBOSE_HOOKS=1` → opt-in noisy (new)
4. default → quiet (v0.21 flip)

`claude-plugin/scripts/incremental-index.js` — pure passthrough refactor
behind `shouldRun(env)` gate. `CODE_GRAPH_HOOK_INDEX=on|1|true` opts in;
default and any other value skip the binary exec. `module.exports =
{ shouldRun }` exposes the gate for the test file.

Both hook scripts gain dedicated `*.test.js` files: 91 new lines of tests
on user-prompt-context.js (covers the env-precedence chain + per-keyword
intent table) and 55 new lines on incremental-index.js (covers the env
gate + idempotent skip).

### Callgraph: caller_count DESC tie-breaker (`src/graph/query.rs`)

The recursive CTE in `query_direction` gained a `caller_counts` CTE
(non-correlated `GROUP BY target_id` over `edges WHERE relation = ?4`,
covered by `idx_edges_target_rel`) and a `LEFT JOIN` into the outer SELECT.
Final `ORDER BY` is now `depth ASC, caller_count DESC`. When the result set
saturates `CALL_GRAPH_ROW_LIMIT`, high-connectivity subtrees survive the
truncation instead of being silently dropped. Test:
`test_callees_ordered_by_caller_count` (3 callees, 5/1/0 external callers,
asserts the depth-1 ordering matches caller-count rank).

`caller_count` is computed for every node in the result, not just the
truncation boundary — small CPU overhead, big interpretability win for
`module_overview` and `find_references` consumers downstream that read
the same field for sort ordering.

### Routing-bench multi-model dispatch (`tests/routing_bench.rs`)

New `ROUTING_BENCH_MODELS` env var accepts a comma-separated model list
(`sonnet-4.5,sonnet-4.6,opus-4.7,haiku-4.5`) and dispatches one Backend
per name, sharing a single API key. Single-model `ROUTING_BENCH_MODEL`
still works (legacy callers unchanged). When more than one backend ran,
the bench prints a multi-model summary table:

```
=== Multi-model P@1 summary (threshold 70%) ===
  sonnet-4.5      backend  recall 22/22 (100.0%)  fp 0/10
  sonnet-4.6      backend  recall 22/22 (100.0%)  fp 0/10
  opus-4.7        backend  recall 21/22 ( 95.5%)  fp 0/10
  haiku-4.5       backend  recall 18/22 ( 81.8%)  fp 0/10
```

Use case: weekly CI cron walking the Anthropic family to catch routing
regression when Claude Code rotates the default model. v0.20.0 measured
100% P@1 on Sonnet 4.5 only — the rest of the family had no signal until
this hook existed.

`detect_backend()` (legacy single-model) is preserved and still backs
the default `ROUTING_BENCH_MODEL` path. New `detect_backends()` returns
a `Vec<Backend>`; pure helpers `parse_models_env(s)` and
`build_backends(models, anthropic_key, openrouter_key)` are unit-tested
without API keys (4 new tests under `multi_model_dispatch_tests`).

### Effectiveness benchmark harness (`tests/effectiveness_bench.rs`, new)

Turns the README's "40-60% session token savings" vibe-claim into a
regression-tracked number. For each navigation task in the corpus, runs
the equivalent `code-graph-mcp` CLI command on a fixture project and
compares the byte count of the response to a hardcoded `baseline_bytes`
representing the historical Grep+Read approach. Asserts the overall
ratio stays ≤ 0.60 (matches the headline claim's worst case).

Bytes are a token proxy; for English / TS source they correlate ~1:3
with BPE tokens, so a 50% byte reduction maps to a 50% token reduction
at the same ratio. The harness intentionally avoids a tokenizer
dependency — bytes-as-proxy is good enough for tracking trend over
releases. Run with:

```
cargo build --no-default-features
cargo test --test effectiveness_bench --no-default-features -- --ignored --nocapture
```

`#[ignore]`-gated like `routing_bench`, so it doesn't fire on default
`cargo test` — opt in with `--ignored`. New tasks added by hand-counting
(or by running grep/Read for the same intent and summing the bytes
touched), set `baseline_bytes` once, commit. Subsequent regressions move
the ratio without touching the baseline.

## v0.20.0 — Adversarial tool descriptions + single-file outline + project-typed memdir + 100% routing P@1

### Migration notes (read first)

**No breaking changes.** All edits are LLM-visible metadata, additive output
fields, or new feature gates that fall back to the v0.19.0 behavior when not
opted into. Three behaviors users feel automatically on next session:

- 7 MCP tool descriptions rewritten in adversarial style ("INSTEAD OF Grep",
  "Replaces N rounds of grep+Read") to compete with Claude Code's first-class
  Grep/Read/LSP tool prompts.
- `module_overview` (and CLI `overview`) on a single file path now emits an
  outline view: `L<start>-<end>  type  name (callers×)  signature` per symbol,
  sorted by line number. Replaces Read on 3000+ line source files.
- `code-graph-mcp adopt` now detects project type (Rust/web-rs/web-node/web-py/
  web-go/frontend/python/go/node/generic) and writes a per-type MEMORY.md
  index line — Web projects get HTTP-route-tracing priming, Rust CLIs get
  callgraph/impact priming, frontend projects get rename-audit priming.

To pin the generic INDEX_LINE behavior of v0.19.0, set
`CODE_GRAPH_PROJECT_TYPE=generic` in `~/.claude/settings.json` env block.
To pin tool descriptions, downgrade — there is no env opt-out by design,
since LLM-visible metadata changes are the headline feature here.

### LLM-visible metadata revamp (the headline)

`src/mcp/tools.rs` — all 7 visible tool descriptions rewritten following the
sdscc reference ("MCP tool description should compete with Grep/Read/LSP for
the same query"). Pattern: lead with the trigger phrase users actually type,
then state the alternative-tool replacement, then the boundary. Examples:

- `get_call_graph`: "Multi-hop call chain. Replaces N rounds of `grep \"X(\"` +
  Read. Pass route_path='GET /api/x' to trace HTTP handler → downstream."
- `module_overview`: "Symbols in a directory or file, grouped by type +
  caller count. Replaces Glob + Read×N for big dirs / huge files. Single
  file: include_deps=dep graph, include_dead=unreferenced."
- `find_references`: "Rename/remove audits — every site that imports/inherits/
  implements/calls a symbol. Repo-wide cross-language (LSP needs file open).
  Literals → Grep; 'who calls X?' → get_call_graph."
- `ast_search`: "Enumerate symbols by typed filters (type/returns/params)
  Grep can't express. Use for 'all fns returning Result<T>' / 'all structs
  implementing X'. ONE known symbol → get_ast_node."

Server `instructions` field gained one line: `"Repo-wide AST index (LSP only
handles open files; we don't). Replaces multi-round Grep+Read for structural
queries."` Compile-time `assert!(NOISY.len() <= 1500)` budget unchanged.

`test_descriptions_are_concise` (≤200 char) still passes for all 7 tools.

### Single-file outline format (cmd_overview / module_overview)

`ModuleExport` struct gained `start_line` + `end_line` fields, plumbed
through both SQL queries (sql_exports + sql_fallback). When `overview` /
`module_overview` resolves to exactly one file path, output switches from
"by-type compact list" to outline:

```
src/mcp/server/mod.rs
  L1213-1254  fn  handle_initialize  fn handle_initialize(&self, ...)
  L1256-1265  fn  handle_tools_list (3×)  fn handle_tools_list(&self, ...)
  ...
```

MCP `module_overview.active_exports[]` JSON gained `start_line` + `end_line`
(additive — existing clients ignore unknown keys).

### Project-typed memdir adoption (memdir L1 升格)

`claude-plugin/scripts/adopt.js` gains `detectProjectType(cwd, env)` and
`buildIndexLine(projectType)`. Detection state machine:

- **Cargo.toml**: strips `# ...` comments, scans only `[dependencies]`
  section (skips `[dev-dependencies]` / `[build-dependencies]` / target deps).
  Web frameworks: actix-web, axum, rocket, warp, poem, tide, salvo. (`hyper`
  excluded — too commonly a CLI HTTP client.)
- **package.json**: `JSON.parse` + checks only `dependencies` field (skips
  `devDependencies` to avoid false-promoting React component libraries).
  Frontend: next/react/vue/svelte/nuxt/astro/remix/solid-js. Web-node:
  express/fastify/koa/hono/@nestjs/core/@hapi/hapi.
- **pyproject.toml**: scans `[tool.poetry.dependencies]` + `[project.dependencies]`
  + `[project]` (PEP 621 inline). Web: django/flask/fastapi/starlette/sanic/
  tornado/quart.
- **requirements.txt fallback** with comment-strip.
- **go.mod**: skips `// indirect` deps and `//` comment lines. Web:
  gin-gonic/labstack-echo/gofiber/go-chi/gorilla-mux.

Per-type INDEX_LINE primes the right tools and demotes irrelevant ones —
e.g. a Rust CLI's INDEX_LINE no longer mentions `trace_http_chain`, freeing
attention budget for callgraph/impact/dead-code routing.

### CODE_GRAPH_PROJECT_TYPE env override

`detectProjectType(cwd, env)` honors `CODE_GRAPH_PROJECT_TYPE` env var when
set to a valid bucket name (`PROJECT_TYPES` Set is the allow-list).
Invalid/typo'd values silently fall through to file-based detection (so a
typo doesn't classify everything as `generic`). Use cases: power users who
want to pin a non-default classification, CI runs that want deterministic
typing across mixed repos, or opting out via `=generic`.

### routing-bench: oracle alternates (test infra)

`tests/routing_bench.rs` ORACLE entries can now express "either of these
tools is correct" via `|`-separated expected: e.g.
`("Who calls X?", "get_call_graph|find_references")`. New helper
`matches_expected(picked, expected)` splits on `|` and accepts membership.
Wired through `compute_recall`, `compute_overall`,
`assert_oracle_covers_registry`, and the main benchmark miss-detection.

Why: at depth=1, `find_references` with `relation=calls` returns the same
caller list as `get_call_graph`. Pinning a single answer over-fitted the
oracle to a stylistic preference rather than measuring real routing capability.

Result: routing-bench P@1 went from **95.5% (21/22)** → **100% (22/22)** on
the Backend oracle (OpenRouter Sonnet 4.5, ToolOnly mode).

### Test coverage

- `claude-plugin/scripts/adopt.test.js`: 43 → 65 tests (+22). Covers project-typed
  INDEX_LINE roundtrip + 12 detection-hardening tests (commented dep,
  dev-deps only, build-deps only, devDependencies, `// indirect`, malformed
  JSON, PEP 621, requirements.txt, env override valid/invalid/empty/forced-generic).
- `tests/routing_bench.rs` scoring tests: 40 → 43 (+3 alternates path coverage).
- Rust suite: 469 → 470 passed, 0 failed, 1 ignored (routing_bench API key gate).

### Internal: storage struct + clippy

- `ModuleExport` struct: 2 new fields (`start_line`, `end_line`). SQL touched
  in 2 places (sql_exports + sql_fallback). All 5 ModuleExport call sites
  in cli.rs / overview.rs read the new fields.
- One pre-existing clippy `iter_cloned_collect` lint cleaned up
  (`.iter().copied().collect()` → `.to_vec()` in the new outline branch).
- `cargo +1.95.0 clippy --all-targets -- -D warnings` clean on both
  `--no-default-features` and default builds.

## v0.19.0 — Tier-aware language support: bash/json + C/C++ #include/gtest + Dart top-level fix

### Migration notes (read first)

**No breaking changes.** All additions are backward-compatible. Existing indexes
pick up new edges and test markers on the next incremental update — no rebuild
required. Users feel three new behaviors automatically:

- New file extensions are now indexed: `.sh` / `.bash` (Bash), `.json` (JSON, file-FTS only).
- C/C++ `#include` directives now produce IMPORTS edges in the dependency graph.
- gtest macro invocations (`TEST` / `TEST_F` / `TEST_P` / `TEST_CASE` / `TYPED_TEST` / `TYPED_TEST_P`) are now marked `is_test=true` and named `Suite.Name`.

To revert any individual feature, pin to v0.18.4 (`cargo install code-graph-mcp@0.18.4`
or downgrade the npm-installed binary). No env-flag opt-out — the additions are
graph data shape, not behavior toggles.

### New language coverage

- **Bash** (`tree-sitter-bash 0.23.3`) — function definitions, command-style
  calls (with static-identifier filter rejecting `$VAR` / `$(...)` / shell
  built-ins like `[` and `:`), and IMPORTS edges from `source <file>` / `. <file>`
  (path prefix and `.sh` / `.bash` extension stripped; dynamic paths skipped).
- **JSON** (`tree-sitter-json 0.24.8`) — file-FTS indexing only. No AST symbols
  extracted by design (JSON has no function/class concepts); files are searchable
  via FTS5 like any other indexed text.

### C/C++ improvements

- `#include "foo/bar.h"` and `#include <stdio.h>` now emit IMPORTS edges from
  `<module>` to the bare module name. Path prefix and `.h` / `.hpp` / `.hxx` / `.hh`
  extensions stripped so cross-file resolution can match header file nodes.
  Closes a long-standing gap where C/C++ projects had near-empty import graphs.
- gtest macros parsed by tree-sitter as `function_definition` now extract
  `Suite.Name` (e.g. `MathSuite.Addition`) instead of colliding under the macro
  name (`TEST`), and force `is_test=true` on the resulting node. Six macros
  covered: `TEST`, `TEST_F`, `TEST_P`, `TEST_CASE`, `TYPED_TEST`, `TYPED_TEST_P`.

### Bugfixes

- **Dart top-level function scope** (silent call-graph hole): the `function_body`
  scope_name arm in `relations.rs` previously only matched `method_signature`
  prev-siblings. Top-level Dart functions wrap as `declaration > function_signature
  + function_body` — that AST path was silently dropped, so every call inside any
  top-level Dart function was missing from the call graph. Now both top-level
  and class-method shapes resolve correctly.

### Tier-aware language support docs

README and project `CLAUDE.md` previously claimed "16 languages" as a flat list.
Reality is a continuum of extraction depth. Updated to a 5-tier breakdown:

- **Full** (calls + imports + inheritance + HTTP routes + test markers):
  TS/TSX, JS, Go, Python, Rust, Java
- **Smoke-tested** (calls + imports + inheritance): C#, Kotlin, Ruby, PHP, Swift, Dart
- **Limited** (functions + calls + `#include` imports + gtest test markers;
  `Class::method` scope qualification still deferred): C, C++
- **Scripting**: Bash (with `source`/`.` imports), Markdown (headings)
- **File-FTS only** (no AST symbols extracted): HTML, CSS, JSON

### Test coverage

Parser test suite: 65 → 87 (+22). New tests:

- 6 inheritance smoke tests for C#/Kotlin/Ruby/PHP/Swift/Dart (audit confirmed
  baseline shapes work for `delegation_specifiers` / `inheritance_specifier` /
  `base_clause` / `class_interface_clause` / `superclass` / `base_list` with
  IFoo heuristic).
- 12 calls + imports smoke tests for the same 6 languages (Tier 2).
- 3 tests for C/C++ `#include` IMPORTS + gtest macro detection.
- Test infrastructure now provides regression protection for the 6 Tier 2
  languages that had zero specific tests before this release.

## v0.18.4 — Hidden-5 fold + tools.rs split + Cargo default lite + routing-bench CI

### Migration notes (read first)

**Cargo default features changed** (direct `cargo install code-graph-mcp` users):
the default build is now FTS5-only (~10 MB binary). Opt back into the full
hybrid (FTS5 + vector) with `cargo install code-graph-mcp --features
embed-model`. **npm/npx/plugin users see no change** — `release.yml` now passes
`--features embed-model` explicitly, so shipped binaries keep the same
capabilities they had in v0.18.3.

**MCP `instructions` shrunk from ~700 B to ~330 B** (visible in `initialize`
response). Removes the "5 advanced tools CLI-only" caveat that was the
v0.18.3 reality but is no longer true after the fold below. Compile-time
guard at 1500 B is unchanged.

**Hidden 5 names still callable** (`impact_analysis`, `find_similar_code`,
`dependency_graph`, `find_dead_code`, `trace_http_chain` + alias
`find_http_route`). Dispatcher entries kept for raw JSON-RPC / SDK clients
and existing integration tests. Claude Code is expected to use the new flag
forms below.

### Fold: hidden 5 → core 7 flags

The 5 niche tools that were registered-but-hidden from `tools/list` (and
therefore unreachable from Claude Code, which derives its callable set from
`tools/list`) are now reachable as flags on the core 7. Same backing logic;
new entry path:

| Old (hidden, still callable as alias) | New flag form (preferred) |
|---|---|
| `impact_analysis symbol_name=X` | `get_ast_node symbol_name=X include_impact=true` |
| `find_similar_code node_id=N` | `get_ast_node node_id=N include_similar=true` |
| `dependency_graph file_path=F` | `module_overview path=F include_deps=true` |
| `find_dead_code path=P` | `module_overview path=P include_dead=true` |
| `trace_http_chain route_path="GET /x"` | `get_call_graph route_path="GET /x"` |

`get_ast_node include_impact` was already in v0.18.3 — the other four flags
are new. CLI subcommands (`code-graph-mcp impact|similar|deps|dead-code|trace`)
are unchanged for Bash workflows.

### Refactor: `src/mcp/server/tools.rs` split (no behavior change)

The 2354-line tool dispatch file is now 9 focused modules under
`src/mcp/server/tools/`:

```
tools/
├── search.rs        — semantic_code_search
├── callgraph.rs     — get_call_graph + format helpers + truncation flags
├── ast_node.rs      — get_ast_node + ast_node_by_id + impact summary + similar attach
├── ast_search.rs    — ast_search
├── refs.rs          — find_references
├── overview.rs      — module_overview + compact + dep/dead fold
├── project_map.rs   — project_map
├── advanced.rs      — backing logic for the folded 5 (still pub(in server))
└── management.rs    — start/stop watch, get_index_status, rebuild_index
```

Visibility for handler methods is now `pub(in crate::mcp::server)` so the
dispatcher in `mod.rs` can still reach them across the new module boundary.
No public API change; the matching commit is the bisect target if you're
cherry-picking.

### CI: weekly routing-bench tracking

New `.github/workflows/routing-bench.yml` runs `tests/routing_bench.rs`
weekly (Sunday 03:17 UTC), on every release tag, and on manual dispatch.
Asserts P@1 ≥ 0.70 against the live 7-tool MCP schema using OpenRouter
(Claude Sonnet 4.5 default, override via workflow input). Cost ~$0.10 per
run. Requires `OPENROUTER_API_KEY` repo secret; without it the job no-ops
gracefully. Per-release P@1 lands in the GitHub Actions step summary +
artifact retention 90 days.

### Adoption template refresh

`claude-plugin/templates/plugin_code_graph_mcp.md` reflects the fold —
core-7 decision table now shows the new `include_*` and `route_path` flags
inline, and the legacy "进阶 5 走 CLI" section is rewritten as "old names
still work; prefer flag form in Claude Code". Adopted projects with
`CODE_GRAPH_NO_TEMPLATE_REFRESH` unset will pick this up on next
SessionStart.

## v0.18.3 — release-pipeline supply-chain hardening + pending-sweep narrow

Maintenance release. No public-API or schema changes; CLI flags, MCP tool
shapes, and SQLite schema all unchanged from v0.18.2. Output of comprehensive
gstack audit run (/cso, /review, /retro) on v0.18.2 — every finding actioned
or explicitly accepted with rationale.

**Release pipeline** — third-party action SHA pins, model revision pin

- `release.yml`: `dtolnay/rust-toolchain@stable` → `@e081816` (1.95.0 branch
  SHA-pinned). Closes the asymmetry where `release.yml` built shipped
  binaries with whatever the latest stable Rust was at build time, while
  `ci.yml` tested with `1.95.0`. Also closes the supply-chain window where
  a moved `@stable` tag would have silently affected every release.
- `release.yml`: `Swatinem/rust-cache@v2` → `@e18b497`,
  `softprops/action-gh-release@v3` → `@b430933`. Both third-party,
  in the release path; cache-action poisoning could exfiltrate `NPM_TOKEN`,
  release-action substitution could swap GH Release artifacts. Floating
  major-version tags are mutable; commit SHAs aren't.
- `release.yml` model-bundle step: HuggingFace `resolve/main/$f` →
  `resolve/c9745ed1d9f207416be6d2e6f8de32d1f16199bf/$f` for
  `sentence-transformers/all-MiniLM-L6-v2`. Plus `curl --fail` so a 404
  HTML page can no longer masquerade as `model.safetensors` (the bundle's
  downstream sha256 only validated the bundle against itself, not against
  a known-good upstream).

**Supply-chain CVE coverage** — new CI job + 6 RUSTSEC fixes

- New `audit` job in `ci.yml` runs `cargo audit` against `Cargo.lock` on
  every push/PR. cargo-audit pinned to `^0.22` because `0.21.x` panics on
  RustSec advisories using CVSS 4.0 (e.g. `RUSTSEC-2026-0066`) — fetch
  fails before any finding can be reported. Default behavior fails on
  vulnerabilities; informational `unmaintained`/`unsound` advisories print
  but don't block (most are transitive and not under our control until
  upstream replacements ship).
- `cargo update -p rustls-webpki -p tar` resolved the 6 advisories the
  new audit job surfaced on a v0.18.2 baseline:
  - `rustls-webpki 0.103.9 → 0.103.13` (RUSTSEC-2026-0099 wildcard cert
    name acceptance, RUSTSEC-2026-0098 URI name constraint, RUSTSEC-2026-0104
    CRL parsing panic, RUSTSEC-2026-0049 CRL distribution-point matching).
    Used transitively via `reqwest`/`quinn`.
  - `tar 0.4.44 → 0.4.45` (RUSTSEC-2026-0067 `unpack_in` symlink chmod,
    RUSTSEC-2026-0068 PAX size header). Direct dep behind `embed-model`
    feature, used to unpack the bundled HF model tarball.

**Indexer perf** — pending sweep no longer full-scans nodes

`resolve_pending_calls` in `src/indexer/pipeline.rs` previously did
`SELECT n.id, n.name, ... FROM nodes n JOIN files f ...` over the full
nodes table to build the in-memory `name → [(node_id, language)]` map for
resolution. Even a 1-row pending table triggered a full scan on every
incremental pass. Narrowed by adding
`AND n.name IN (SELECT DISTINCT target_name FROM pending_unresolved_calls)`
to the SELECT — scope drops to ≤ |pending| names per sweep. All 5
v0.18.2 regression tests still pass; resolution semantics unchanged.

## v0.18.2 — incremental dropped-edge root-cause fix (both directions)

Closes the bug documented in memory `feedback_incremental_edge_timing.md`:
incremental indexing silently dropped REL_CALLS edges in two symmetric
scenarios that only `rebuild-index` recovered. v0.18.1 query-time
freshness was a band-aid for the file_path-aware tools; this is the
underlying fix, in both directions.

**The bug, both directions**

*Direction A — callee added later*: file B has `caller_b() { foo(); }`.
At B's Phase 2, `foo` has no same-file or same-language target → REL_CALLS
dropped (memory `feedback_edge_resolution_same_language.md` correctly
forbids cross-language fallback for calls). Later, file A is added with
`function foo() {}`. Incremental index reindexes A only; B isn't in
`changed_paths`, so B's bare-name `foo()` is never re-resolved. Edge
`caller_b → foo` permanently missing until full rebuild.

*Direction B — callee removed*: same setup but A is *deleted*. Cascade
delete on `target_id` FK strips B's edge to A.foo automatically; B isn't
in `delete_paths`, so Phase 2 doesn't re-extract it. If A is then re-added
later, B has neither a stale edge nor a way to know it should re-resolve.

**The fix (schema v8)**

New `pending_unresolved_calls` table buffers REL_CALLS that Phase 2 can't
resolve at extraction time, plus inbound REL_CALLS edges Phase 0 is about
to cascade-strip. The post-Phase-2 sweep promotes pending rows to real
edges as soon as a same-language target appears.

- `(source_id REFERENCES nodes ON DELETE CASCADE, target_name,
  source_language, metadata)` with unique index on the triple — keeps
  inserts idempotent across repeated Phase 2 invocations.
- `ON DELETE CASCADE` makes the table self-cleaning: when caller B is
  reindexed (Phase 1 deletes its old nodes), pending rows for B's old
  source_ids drain automatically.
- Sweep scope is **same-language only** — cross-language is never
  promoted (the canonical false-positive class from
  `feedback_edge_resolution_same_language.md`). When multiple
  same-language candidates exist, the existing `refine_ambiguous_targets`
  applies (path-proximity + non-test preference), so dense-fanout cases
  don't regress dead-code precision.

**Direction A wiring (commit `d172cae`)**: at Phase 2's REL_CALLS drop
point in `pipeline.rs`, instead of silent `continue` we
`insert_pending_unresolved_call`. End of `index_files` runs
`resolve_pending_calls` which builds name → [(node_id, language)] and
node_id → path maps from current DB state (one indexed SELECT, not
per-row), iterates pending rows, applies `refine_ambiguous_targets`
where ambiguous, inserts edges, drops rows.

**Direction B wiring (commit `9c27739`)**: Phase 0 in `pipeline.rs` now
resolves file_ids before `delete_files_by_paths` drops them, calls new
`queries::get_inbound_calls_for_pending` to fetch inbound REL_CALLS
edges from non-deleted files, and writes pending rows for each before
letting cascade fire. Same post-Phase-2 sweep then handles the resolution.

**Migration**: SCHEMA_VERSION 7 → 8. INDEX_VERSION unchanged — existing
edges remain valid; the pending table starts empty and fills naturally
on next index pass. Migration is transactional (matches the pattern of
every prior migration). Existing v0.18.1 DBs auto-upgrade on first open.

**Test coverage** (5 new pending-resolution tests + 1 migration test):
- `test_pending_unresolved_call_resolves_when_callee_added_later`
  (direction A round-trip)
- `test_pending_buffers_on_callee_file_deletion` (direction B
  round-trip — edge → delete → buffered → re-add → edge restored)
- `test_pending_unresolved_call_does_not_cross_language` (TS pending
  vs Rust definition stays buffered; cross-language refused)
- `test_pending_resolves_multiple_calls_in_same_caller` (3 undefined
  calls in one caller → 3 pending rows → all drain on single sweep)
- `test_pending_cascade_deletes_when_caller_file_reindexed`
  (load-bearing schema FK behavior — explicit guard so a future
  migration weakening the FK fails loudly here)
- `test_v7_to_v8_migration_adds_pending_table` (asserts table + both
  indexes after v7-shape DB opened via Database::open)

**Bonus**: 2 new plugin-script test files
- `scripts/sync-versions.test.js` — 4 tests, fixture-copy strategy, locks
  release-tooling contract (`feedback_version_sync.md`). Includes
  `(9 files updated)` count assertion to catch silent target drops.
- `claude-plugin/scripts/mcp-launcher.test.js` — 3 tests, end-to-end MCP
  initialize via launcher + 2 static-grep checks for plugin-env
  isolation (`feedback_plugin_env_isolation.md`) and macOS quarantine
  hint surface.

**Test count**: 272 default-features (was 265 in v0.18.1), 265
no-default-features (was 261), 182 JS tests (was 175). All clippy 1.95
clean on both feature profiles.

**Compatibility**: All 16 MCP tool schemas unchanged. CLI flags
unchanged. Output JSON unchanged (no shape additions). Schema migration
is transparent to consumers — query results match v0.18.1 plus
previously-missing edges that should have been there all along.

## v0.18.1 — query-time freshness + call-graph truncation provenance

Three additive improvements to MCP tool surfaces, no breaking changes.
All output-shape changes are strictly additive — non-truncated /
non-edit-aware paths return the exact prior shape.

**1. Query-time freshness for file_path-aware tools** (commits `30678d6`,
`82f1526`).

When an MCP tool receives an explicit `file_path` argument, the agent is
signaling "I just edited this; please answer against the current bytes."
The 30s `last_incremental_check` debounce in the server is too coarse for
tight Edit→search loops — agents would see pre-edit call edges right after
saving a file.

- New `pipeline::ensure_file_indexed(db, root, rel_path, model)`: single-file
  hash-compare reindex, no-op when on-disk hash matches stored hash. Drops
  stale rows when the file is gone. Skips files we wouldn't index in the
  first place (binary / unrecognized language). Cross-file dirty-edge
  handling mirrors `run_incremental_index` (collect dirty node IDs BEFORE
  re-indexing so cascade delete doesn't strip the context-string
  regeneration target set).
- New `McpServer::ensure_file_fresh_opt(path)`: server-side wrapper that's
  a no-op on read-only secondaries, on missing/empty/directory paths, and
  when the embedding lock is contended. Invalidates `project_map` and
  `module_overview` caches only when a reindex actually fired.
- Wired into 6 file_path-aware tools: `get_call_graph`, `get_ast_node`,
  `module_overview`, `find_references`, `dependency_graph`,
  `impact_analysis`. Agents no longer have to remember which tools
  auto-refresh and which don't.

`test_ensure_file_indexed_picks_up_post_edit_changes` covers the no-op /
post-edit-pickup / repeat-no-op / file-deleted paths.

**2. Call-graph truncation provenance** (commit `fd168fd`).

The recursive CTE in `get_call_graph` silently caps at depth 10 and 200
rows. Agents reading partial results couldn't tell when truncation fired
vs. when the graph genuinely ended — a common failure mode for "who calls
X?" on hot functions where the real answer is "200+ across the codebase,
you're seeing a slice."

- `graph::query::CallGraphResult` wraps `Vec<CallGraphNode>` with
  `limit_hit` / `depth_capped` / `effective_max_depth` /
  `requested_max_depth` flags.
- `CALL_GRAPH_MAX_DEPTH` (10) and `CALL_GRAPH_ROW_LIMIT` (200) are now
  public constants — single source of truth (was a magic number in two
  places).
- `query_direction` returns `(nodes, limit_hit)` so the `direction="both"`
  merge can OR-combine saturation across both call directions.
- New JSON fields appear **only** when truncation fires: `limit_hit`,
  `depth_capped`, `effective_max_depth`, `requested_max_depth`,
  `truncation_warning`. The warning text gives the agent a recovery move
  ("pick a leaf node_id and re-query from there, or narrow with file_path").
- Wired into MCP `get_call_graph` (incl. rollup branch) +
  `trace_http_chain` (`call_chain_truncated` flag per handler), and CLI
  `code-graph-mcp callgraph` / `trace`.

`test_depth_capped_signal` verifies clamp + flag wiring with a depth=99
request.

**3. CHARS_PER_TOKEN clarified as bytes/token + CJK regression test**
(commit `6dc10ff`).

The constant has always been used with `s.len()` (UTF-8 byte count in
Rust), not Unicode char counts. The historical name suggested otherwise
and tempted "fixes" to char-count, which would silently halve the CJK
budget — one CJK char = 3 bytes ≈ 1 BPE token, so `bytes/3 ≈ chars ≈
tokens` (accidentally correct under the bytes interpretation).

- Doc on `CHARS_PER_TOKEN` now explains ASCII vs CJK behavior and the
  conservative-overestimation property that makes earlier-fire
  compression the safe error direction.
- `estimate_tokens` local rename `total_chars → total_bytes` to match.
- `test_estimate_tokens_cjk_byte_based`: 1000 CJK chars (3000 bytes) must
  estimate ~1000 tokens; ASCII 1000 chars (1000 bytes) must estimate
  lower, confirming the divisor is bytes-based. Regression guard against
  someone "fixing" the estimator to char-count.

No behavior change in this commit — doc + test only.

**Test count**: 265 default-features (was 264), 258 no-default-features
(was 257). 3 new tests across the three changes.

**Compatibility**: All 16 MCP tool schemas unchanged. CLI flags
unchanged. Output JSON additive only. Zero breaking changes for plugins
or downstream consumers.

## v0.18.0 — routing_bench frontend domain + project_map dedup hint

Two changes driven by the v0.17.3 30-day usage audit. The audit found
that **all 728 code-graph MCP/CLI calls in 30 days came from the
plugin's own repo** — frontend / non-Rust workflows had zero coverage
in the routing benchmark, so we couldn't tell whether tool descriptions
activated for them. It also found that `project_map` was being invoked
~11 times/30d via MCP **after** SessionStart had already injected the
same map at boot — pure redundancy.

**1. routing_bench frontend domain** (`tests/routing_bench.rs`).

Adds a second 20-query oracle (`FRONTEND_ORACLE`) covering the same 7
core tools with JS/TS/Vue/React phrasing (component / hook / Promise /
useEffect / Redux dispatch). Selectable via new env:

- `ROUTING_BENCH_DOMAIN=backend` (default — preserves v0.17.2/v0.17.3
  baselines comparable; runs only the original 22-query Rust pool).
- `ROUTING_BENCH_DOMAIN=frontend` — runs only `FRONTEND_ORACLE`.
- `ROUTING_BENCH_DOMAIN=all` — both pools (42 q), with separate
  `Backend recall` / `Frontend recall` buckets in the report so
  frontend regressions don't hide behind backend wins.

The bench helpers (`compute_recall`, `compute_overall`, `build_oracle`)
were refactored to accept an oracle slice instead of hardcoding `ORACLE`,
so the same scoring path covers both domains. `oracle_well_formed`
guards backend coverage; new `frontend_oracle_well_formed` and
`frontend_oracle_distinct_from_backend` guard the frontend pool.

15 new tests added (42 total, was 27); no API key required for any of
them. Test count: `cargo test --test routing_bench` 42 passed,
1 ignored (the API-gated `routing_recall_benchmark` itself).

**First frontend baseline** (sonnet-4.5, 3-run majority vote,
domain=all mode=context-rich, ~$0.80/run):

- Backend recall: **22/22 = 100%** (was 21/22 in v0.17.2 — the historic
  `EmbeddingModel struct definition` miss did not recur this run; v0.17.3's
  description tightening of `get_ast_node` and `semantic_code_search`
  appears to have stuck on sonnet-4.5).
- Frontend recall: **19/20 = 95.0%**.
- FP-rate: 0/10 = 0%.
- Overall: 51/52 = 98.1%.

The single frontend miss is `"List all React components in src/components/"` →
routes to `module_overview` (3-run unanimous), expected `ast_search`. This
is borderline-by-design: the query contains a module-path prefix
(`src/components/`) which triggers the v0.17.0 description rule "if module
path is known, prefer module_overview" — the same rule guarded by backend
ORACLE's `"How does the embedding pipeline work in src/embedding/?"`
regression case. Two valid routings; model picked the path-anchored one.
Future regression gate: frontend recall ≥ 19/20.

**Conclusion**: frontend pool achieves near-backend recall with vanilla
sonnet — tool descriptions already activate on JS/TS/Vue/React
vocabulary. The "frontend project shows zero MCP calls" observation
from the usage audit is workflow/install shortfall (the audited project
hadn't enabled the plugin in `.mcp.json`), not a routing-description
failure.

**2. project_map description: explicit dedup hint** (`src/mcp/tools.rs`).

Description rewritten from
`"Project architecture map. Use when: starting work on unfamiliar
code, finding which module owns functionality, or needing cross-module
dependency overview."`
to
`"Project architecture map. SessionStart hook already injects this at
boot. Call only if structure changed mid-session: major refactor,
rebuild-index, or many new modules."`

170 bytes, fits the 200-byte per-description cap asserted by
`mcp::tools::tests::test_descriptions_are_concise`.

**Re-bench (same methodology) post-description-change**: zero
regression. Backend 22/22, Frontend 19/20, FP 0/10, Overall 51/52
unchanged. Same single frontend miss. Conclusion: explicit
"do-not-call-redundantly" framing in tool descriptions is regression-
safe and reusable for any other MCP tool that already has SessionStart-
hook coverage.

**Expected impact**: ~11 redundant `project_map` MCP calls/month
eliminated (~33K tokens/month saved) without any routing precision
trade-off. Will be visible in the next 30-day window's
`code-graph-mcp stats tools.project_map.n` count.

## v0.17.3 — get_ast_node disambiguation (description tightening)

Bench-driven fix on **published tool descriptions** for the
"named-symbol queries leak to semantic_code_search" boundary.

**Background.** v0.17.2's context-rich bench (haiku-4.5 stress test)
surfaced 3 systematic misses: `EmbeddingModel struct definition`,
`weighted_rrf_fusion signature`, `format_call_graph_response
implementation` — all routing to `semantic_code_search` instead of
`get_ast_node`. The MEMORY.md hook already had `看 X 源码/签名 →
get_ast_node` but weak models ignored it at tool-selection.

**Diagnosis.** `semantic_code_search`'s description led with "Search
code by concept" with no explicit handoff to `get_ast_node` for
named-symbol queries. v0.17.0 added analogous redirects for
`semantic_code_search → module_overview` and `find_references → Grep`;
the named-symbol boundary was the missing one.

**Fix.** Two description edits in `src/mcp/tools.rs`:

- `semantic_code_search` (197 chars): rewritten to "Concept search
  when no symbol/module is named. If a symbol is named (e.g., 'show
  X struct'), use get_ast_node; if module path is known, use
  module_overview. Use when grep is noisy."
- `get_ast_node` (200 chars): "Inspect ONE named symbol: signature,
  full source, optional references/impact. Use when: query names a
  symbol asking for its definition/body/signature/implementation.
  PREFER over semantic_code_search."

Both fit the project's 200-char-per-description cap (asserted by
`mcp::tools::tests::test_descriptions_are_concise`). Tighter
example-list patterns were tested first but exceeded the cap.

**Bench results** (3-run majority vote on each model):

- **Sonnet 4.5 context-rich**: 22/22 / 0/10 / 32/32 = 100%
  pre-fix and post-fix. Zero regression.
- **Haiku 4.5 context-rich**: 19/22 → **20/22** (Recall 86.4% →
  90.9%, Overall 90.6% → 93.8%). `weighted_rrf_fusion signature`
  recovered to `get_ast_node`. Two queries still miss
  (`EmbeddingModel struct`, `format_call_graph_response
  implementation`) — they need stronger anchor patterns than fit in
  the 200-char budget; tracked as a follow-up.

**Iteration history (recorded for future tuning).** A higher-budget
description with three named example phrasings ('show X struct
definition', 'signature of Y', 'implementation of Z') recovered
`EmbeddingModel struct` on haiku but exceeded the 200-char cap and
caused a `List all structs in storage module → module_overview`
regression on the same model. Compressing to fit the cap dropped
the EmbeddingModel recovery but eliminated the regression. Net
haiku improvement: +1 query.

**Drift test still passes** — `INDEX_LINE_MIRROR` byte-equal to
`adopt.js` (no MEMORY.md hook change in this commit). Per-tool
descriptions are LLM-visible metadata (L3 published surface);
content is description-only with bench-verified outcomes on both
strong and weak models.

## v0.17.2 — routing_bench context-rich mode

Adds a measurement capability the existing bench architecture lacked:
grading the MEMORY.md hook line quality (added in v0.17.1). The
existing `tests/routing_bench.rs` only consumed tool descriptions;
it could not detect routing changes from MEMORY.md, the adoption-
memory file, or MCP `instructions`. Stage 3 hook content tuning
needed an oracle.

**`ROUTING_BENCH_MODE=context-rich` mode.** Adds:

- `INDEX_LINE_MIRROR` Rust constant + drift-detection test that
  spawns Node and asserts byte-equality with `adopt.js`'s
  `INDEX_LINE` export. Drift fails on every `cargo test`.
- Decoy `Grep` and `Read` tools added to the API call's `tools`
  array (descriptions calibrated with "Prefer over code-graph"
  anchors to be measurement-fair).
- 10-entry `FP_ORACLE` of strict-boundary queries (literal text,
  file reads by path, doc/config content) that should route to
  decoys, not code-graph.
- 3-run majority-vote aggregation per query (tie-break: first run);
  applied to both modes.
- Three reported metrics: Recall (out of 22 ORACLE), FP-rate (out
  of 10 FP_ORACLE), Overall (out of 32, loose summary).

**`temperature: 0` added to both backends** (Anthropic + OpenRouter)
in tool-only mode too. Pre-existing latent ±3-5pp single-run noise
masked Stage 3-level differences. Reproducible from this version on.

**First baselines** (2026-04-30, OpenRouter `anthropic/claude-sonnet-4.5`):

- Tool-only: **21/22 = 95.5%** (178s, 60 calls). Same residual miss
  as v0.16.7 (`Show me the EmbeddingModel struct definition` →
  `ast_search` instead of `get_ast_node` — pre-existing semantic
  borderline). Note: `feedback_routing_bench.md` had been tracking
  19/20 — that was against the 20-entry pre-v0.17.0 oracle; v0.17.0
  added 2 regression-guard queries, real total is 22.
- Context-rich: **Recall 22/22 = 100%** · **FP-rate 0/10 = 0%** ·
  **Overall 32/32 = 100%** (255s, 90 calls). The historically-stuck
  `EmbeddingModel struct` query routes correctly here — the MEMORY.md
  hook + Grep/Read decoys provide enough disambiguation context to
  flip it. Caveat: single bench run; Stage 3 will tell us how robust
  this is to hook-content variations.

**Default mode unchanged.** With `ROUTING_BENCH_MODE` unset or any
value other than `context-rich`, the bench behaves identically to
v0.17.1 except for `temperature: 0` and 3-run aggregation. The
`oracle_well_formed` and `index_line_drift_check` tests run on
every `cargo test`; the live benchmark stays `#[ignore]`'d.

## v0.17.1 — adoption-memory hook line: spec compliance

Single-file structural fix. `claude-plugin/scripts/adopt.js`
`INDEX_LINE` changes from an 11-line `array.join('\n')` block to a
single-line string, complying with the MEMORY.md spec ("each entry
should be one line"). The sentinel block written to
`~/.claude/projects/<slug>/memory/MEMORY.md` shrinks 11 lines → 1
line at next SessionStart per the v0.11.0 template-refresh
contract.

**No behavior change.** All 12 tool names (7 core + 5 hidden), all
6 中文 scene phrases (改 X 影响面 / 谁调用 X / X 被谁用 / 看 X 源码 /
Y 模块长啥样 / 概念查询), the `优先于 Grep` anchor, and the
`字面匹配走 Grep` reverse signal are kept verbatim. New:
spec-canonical tag syntax
`[impact, callgraph, refs, overview, semantic, ast-search, dead-code, similar, deps, trace]`
for explicit keyword matching. Reduces always-loaded MEMORY.md
context by ~366 chars per session.

The adoption-memory detail file (`plugin_code_graph_mcp.md`) is
unchanged — it already holds the full decision table the
multi-line block was duplicating.

**Bench scope.** `tests/routing_bench.rs` only consumes tool
`name + description + input_schema` (verified at
`tests/routing_bench.rs:224-233` + `:50-52`); it does not consume
MEMORY.md, the adoption-memory file, or MCP `instructions`. So it
cannot grade adoption-memory hook quality. A context-rich bench
(MEMORY.md in system prompt + Grep-decoy false-positive corpus) is
a separate change. Existing routing_bench is unaffected by this
PR.

**Tests.** `cargo test` 26/26, `node --test claude-plugin/scripts/*.test.js` 132/132,
`adopt.test.js` 43/43.

## v0.17.0 — quiet by default + tighter routing instructions

Two-part SessionStart context-cost reduction. The plugin used to inject
a ~2.3 KB `project_map` every session for un-adopted projects, plus a
1418-byte MCP `instructions` block packing 10 per-tool decision rules.
Both are redundant with what already exists: each tool's own
`description` carries its routing hint, and `MEMORY.md →
plugin_code_graph_mcp.md` already holds the full decision table for
adopted projects. v0.17.0 cuts both, and tightens the two tool
descriptions whose phrasing demonstrably mis-routed in benchmarks.

**1. SessionStart `project_map` injection: OFF by default.**
Old contract (v0.9.0): adopted → quiet, un-adopted → noisy. The
assumption was that adoption installed the MEMORY.md decision table
so the dump only became redundant *after* adopt.

New contract: quiet unconditionally. The decision table + the
on-demand `project_map` MCP tool + the per-tool descriptions cover
every workflow that the SessionStart map dump used to support, so
paying ~2.3 KB of context per session is wasteful — even pre-adopt.

- `CODE_GRAPH_VERBOSE_HOOKS=1` opts in to the dump (new).
- Legacy `CODE_GRAPH_QUIET_HOOKS=0` (force noisy) / `=1` (force quiet)
  still wins, preserving the v0.9.0 escape hatches.
- `computeQuietHooks({ adopted, env })` accepts but ignores `adopted`
  — kept only to avoid breaking call-sites.

**2. MCP `instructions` field trimmed 1418 → ~700 B.**
The old noisy block packed all 10 routing rules with CLI aliases
inline. Compile-time guard was 1500 B, against an observed Claude
Code truncation cutoff of ~2048 B. The 10 rules now live in
per-tool `description` strings (where clients actually read them to
pick a tool) and in the adopted-project decision table.

What remains in `instructions` is the boundary signal — which 5
advanced tools are CLI-only (MCP integration can't call them by
name), what to still use Grep / Read for, and where to find the
adopted decision table.

**3. Tool description tightening (LLM-visible).**

- `semantic_code_search` now adds *"If module path is known, prefer
  module_overview"* — closes the "I know the path AND a concept word"
  ambiguity that previously routed to semantic search and burned a
  vector lookup.
- `find_references` now adds *"For plain literals (string/regex),
  prefer Grep"* — `find_references` only tracks defined-symbol usage
  sites, not raw text. Before tightening it caught literal-string
  queries that should have gone to Grep.

**4. routing_bench: +2 regression guards.**
Two new oracle items in `tests/routing_bench.rs` directly probe the
tightened phrasings:

- *"How does the embedding pipeline work in src/embedding/?"* →
  expects `module_overview` (path > concept tie-breaker).
- *"I need to rename parse_tree to parse_ast — find every place I'd
  update."* → expects `find_references` (rename-audit intent
  preserved despite the new "prefer Grep for literals" line).

**Verification.** `OPENROUTER_API_KEY=… cargo test --release --test
routing_bench -- --ignored` against `anthropic/claude-sonnet-4.5`
returned **P@1 = 21/22 = 95.5%**, up from baseline v0.16.7's
19/20 = 95.0%. Both new guards passed. The single miss is *"Show me
the EmbeddingModel struct definition"* routing to `ast_search`
instead of `get_ast_node` — pre-existing oracle item, semantically
defensible (`ast_search` returns nodes by name+kind), not introduced
by this release.

## v0.16.9 — install/uninstall lifecycle hardening + MCP/CLI parity

Audit-driven fixes after sandboxed end-to-end testing of the install,
adopt, update, and uninstall flows. Three real bugs surfaced that the
existing 97-test suite couldn't see because none of them tested the
*real* user path: `npm uninstall`, post-upgrade binary resolution, and
adopt-from-fresh-clone. Plus a parity sweep on the MCP↔CLI surface.

**1. `npm uninstall` left dangling hooks in `~/.claude/settings.json`.**
The package shipped a full `lifecycle.js uninstall` that strips our
hook entries from settings.json — but nothing wired it to npm. After
`npm uninstall -g @sdsrs/code-graph` the package files were gone but
`settings.json` still pointed PostToolUse / SessionStart hooks at the
deleted scripts. Claude Code subsequently failed to fire those hooks
or surfaced ENOENT spam.

**Fix:** added `"preuninstall": "node claude-plugin/scripts/lifecycle.js
uninstall || true"` to `package.json`. npm now invokes the existing
uninstall path before removing files. The `|| true` ensures a
lifecycle failure never blocks the uninstall itself. Verified end-to-end
in a sandboxed HOME: settings.json hooks containing `code-graph` paths
get stripped; foreign hooks and `otherKey` configuration are preserved
byte-for-byte.

**2. `find-binary` cache shadowed fresh `npm update` binaries.** The
cache priority was: dev mode → auto-update cache (`~/.cache/code-graph/
bin/`) → platform npm pkg. After `npm update -g 0.16.7→0.16.8` the
platform-pkg binary was refreshed, but the auto-update cache still
held 0.16.7. find-binary returned the stale cache because it only
verified the binary was *executable*, never that the version matched.
Users kept running 0.16.7 until auto-update fired (up to 6h later).

**Fix:** when the auto-update cache hits, read its `--version` and
compare against the npm pkg version (`require('../../package.json').
version`). Cache wins when `cache.ver >= pkg.ver` (legitimate case:
auto-update fetched a newer release than npm has shipped). Cache loses
when older — find-binary falls through to platform-pkg. Includes a
3-digit semver compare helper that tolerates short / non-numeric input.

**3. `adopt` couldn't bootstrap a fresh clone.** The path required
`~/.claude/projects/<slug>/memory/` to already exist (created by
Claude Code on first session that writes memory). Fresh-cloned project
with no memory dir → `adopt` errored `no-memory-dir` and told the user
to "run claude at least once". CI / scripted setup / first-time users
on a new project all hit the wall.

**Fix:** introduced a project-marker check (`.git`, `Cargo.toml`,
`package.json`, `pyproject.toml`, `go.mod`, `pom.xml`, `build.gradle`,
`.code-graph`). Memory dir missing AND cwd has any marker → `mkdir -p`
and proceed. No marker → return `not-a-project` with a clearer error
("cd into a real project before running adopt"). The slug-pollution
guard remains in place for `/tmp` / `$HOME` accidents.

### Slug collision marker

Claude Code's slug encoding (`[^a-zA-Z0-9-]→'-'`) is lossy: `/foo/bar`
and `/foo bar` resolve to the same memory dir. Two projects can
silently share state with no signal. Added: `adopt` writes
`<!-- adopted-by: <abs-cwd> -->` as the first line of
`plugin_code_graph_mcp.md`. Re-adopt from a different cwd surfaces
`result.collisionWith` and a stderr warning. `needsRefresh`'s
bytewise compare strips the marker line first, so the marker doesn't
cause false-positive drift detection on every SessionStart.

### MCP↔CLI parity sweep

Drove every MCP tool against its CLI counterpart on the same query
and compared output. Three real divergences fixed:

- **`hot_functions`**: CLI used `callers` / `test_callers`, MCP used
  `caller_count` / `test_caller_count`; CLI cap=15, MCP cap=10. Both
  now use `caller_count` / `test_caller_count`. CLI honors `--compact`
  for top-10 cap (matching MCP `compact:true`); default returns top-15
  (the underlying SQL `LIMIT 15`).
- **`module_overview` compact**: MCP renamed `caller_count` → `callers`
  in compact mode but kept `caller_count` in full mode. Aligned both
  to `caller_count`.
- **`get_call_graph` self-edge**: CLI included the queried symbol
  itself with `direction=callers` AND `direction=callees` (count off
  by 2 for `direction=both`). MCP filtered `depth > 0`. CLI now
  filters seed in JSON output too. Human renderer keeps the seed for
  the tree root.
- **`project_map` compact `type` field**: MCP non-compact had `type`
  on each hot_function, compact dropped it. Both surfaces now keep
  `type` for parity.

### CLI accepts MCP tool names as aliases

Real-world friction observed in another project where Claude typed
`code-graph-mcp project_map --compact` (the MCP tool name) verbatim
into Bash and hit "Unknown subcommand: project_map". The MCP
`instructions` had `Start: project_map --compact` without the
parens-form CLI alias hint that the other 10 rules use. Two-layer fix:

- Fixed the instructions text: `Start: project_map (map --compact)`
  follows the existing `MCP-name (CLI-alias)` convention.
- Defense in depth: CLI dispatch now accepts MCP tool names directly.
  `project_map` / `module_overview` / `get_ast_node` / `find_references`
  / `get_call_graph` / `impact_analysis` / `find_similar_code` /
  `dependency_graph` / `trace_http_chain` / `find_dead_code` /
  `ast_search` / `semantic_code_search` all map to existing short-name
  handlers. `code-graph-mcp project_map --compact` now works. Typo
  suggester also learned the MCP names so `project_mapp` →
  `project_map`.

### Opt-in real-network auto-update test

`scripts/release-smoke.test.js` gained `auto-update parses real GitHub
releases/latest shape`, gated on `CODE_GRAPH_AUTO_UPDATE_E2E=1`. The
existing 10 auto-update unit tests are all mocked — there's no
guardrail against GitHub API shape regression. Run once per release
to validate `parseLatestRelease` against the real payload.

### Validation

- 165 node tests pass + 1 opt-in skip across 12 suites
- 391 cargo tests pass + 1 ignored (routing_bench needs API key)
- Sandbox lifecycle E2E: 16/16 pass with HOME-isolated mkdtemp
  (binary smoke / adopt / re-adopt / status / check / session-init /
  unadopt / residue audit, no orphan plugin file)
- A end-to-end: realistic settings.json with `code-graph` hook paths
  → `lifecycle uninstall || true` strips ours, preserves foreign
  hooks + `otherKey`

## v0.16.8 — callgraph tree, JSON contracts, dead-code defaults, E2E hardening

End-to-end usability pass: simulated a Claude Code session driving every
MCP tool and CLI subcommand on real symbols. Five independent fixes for
issues that surfaced — none blocking on their own, but each was eroding
the trust-layer agents need to act on tool output.

**1. `callgraph` rendered depth>1 nodes under the wrong parent.** The
recursive CTE was collapsing duplicates with `GROUP BY MIN(depth)`,
which lost the actual traversal parent and made every depth-N node
appear nested under the *last* depth-(N-1) sibling. So `A→B→C` plus
`D→B` printed as if `D` lived under `A` once `B` was already shown.

**Fix:** the CTE now tracks `parent_id` (the cg row that produced each
new node) on each inductive step, and dedup uses
`ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY depth)` so the
shortest-path parent survives. CLI renderer builds a `parent_id →
children` map per direction and recurses, so callers/callees subtrees
stay separate under `--direction=both`. JSON output now includes
`parent_id` (null for the root) for any consumer that wants to rebuild
the tree.

**2. `similar` and `deps` violated the `--json` empty-result contract.**
Both subcommands had paths that wrote nothing to stdout and exited
with stderr only — breaking machine consumers per
`feedback_cli_json_empty_contract`. Added: `similar --json` writes
`[]` when vector search returns no neighbors; `deps --json` writes a
JSON error object `{"file":..., "depends_on":[], "depended_by":[],
"error":"..."}` when the file has no tracked imports. Two new
regression tests guard these paths.

Bonus: `similar 1010` (digits as positional) used to print the
unhelpful "Symbol not found: 1010". Now nudges toward
`similar --node-id 1010`. And `similar` with an existing symbol that
hasn't been embedded yet ("No embedding for node_id 342") explains
*why* (`(1033/1321 nodes embedded — embeddings still generating; try
again shortly or pick a node with --node-id from \`show X\`)`).

**3. MCP tool descriptions misled agents on subtle defaults.** Two
tools had descriptions that didn't match their actual behavior, so
agents made decisions on stale info:

- `module_overview` — caller counts include test callers, but the
  description didn't say so; agents reading "5 callers" couldn't tell
  if a function was prod-hot or only test-driven. Description now
  states "callers count includes tests" so the LLM picks a different
  tool when it actually needs prod-only callers.
- `find_references` — for constants, only `imports` edges are
  recorded; usage sites where the const is read don't appear because
  Rust grammar emits them as identifiers without an import-context.
  Description now says "consts: imports only, not value-uses" so the
  agent escalates to grep when auditing a const for rename.

Also added one line to the MCP `instructions` payload telling the
agent that `impact_analysis`/`find_dead_code`/`find_similar_code`/
`dependency_graph`/`trace_http_chain` are CLI-only after the v0.10.0
core/advanced split — Claude Code only sees the 7 core tools, so
agents trying to invoke the advanced 5 directly via MCP would 404.

**4. E2E suite was passing on dead queries.** `scripts/e2e-validate.js`
called `get_call_graph(handle_call_tool)`, `impact_analysis(
handle_call_tool)`, and `dependency_graph(src/mcp/server.rs)` —
all three symbols/paths had been renamed/moved sessions ago. The
assertions only checked "response contains non-empty text", so
"`[code-graph] Symbol not found: handle_call_tool`" passed as
success. 24/24 green, but actually testing zero-result paths. Real
response sizes told the story: get_call_graph 221 bytes (now 2628),
impact_analysis 220 bytes (now 498), dependency_graph 304 bytes
(now 2291).

**Fix:** swapped the queries to stable hot symbols (`handle_message`,
`conn`, `src/mcp/server/mod.rs`) and added two stricter assertions:
`assertNotEmptyResult(resp, label)` rejects 6 known empty-result
patterns ("Symbol not found", "No callers found", etc.); the MCP
`dependency_graph` returns JSON, not the human "Depends on" text, so
its assertion now `JSON.parse`s and checks `depends_on` is a non-empty
array.

**5. `dead-code` falsely flagged Criterion benchmarks as orphan.**
`benches/indexing.rs` defines three bench functions, all referenced
only via `criterion_group!(benches, bench_full_index, ...)`. The AST
relation extractor doesn't parse macro arguments as references, so
the benches showed up as ORPHAN every time — drowning out the four
real `EXPORTED-UNUSED` results worth attention.

**Fix:** added `benches/` to `domain::default_dead_code_ignores()`,
mirroring the existing `claude-plugin/` exclusion for shell-invoked
hook scripts. The rule generalizes: any directory whose entry points
are reached through tokens the AST can't resolve (macro arguments,
shell command strings, settings.json hook definitions) belongs in
the default ignore list. CLI `--no-ignore` still surfaces them. New
unit test pins the policy.

Together these don't change any external schema, but they materially
improve the signal an agent gets per tool call — fewer phantom
orphans, a callgraph tree that reads like one, and an E2E suite that
actually fails when a hot symbol moves.

## v0.16.7 — install reliability: 3 independent failure paths fixed

Reported on a fresh `/plugin install code-graph-mcp` on another
machine: MCP couldn't connect, the binary was nowhere to be found.
Triage found three independent breakages along the launcher chain;
each is fixed and tested separately so the chain is fault-tolerant
on first install.

**1. `find-binary.js`: didn't search npm global `node_modules`.**
`require.resolve('@sdsrs/code-graph-{platform}-{arch}/package.json')`
only walks the `node_modules` chain rooted at the requiring file —
it does NOT search global installs, because nvm and standard Unix
prefixes don't set `NODE_PATH`. So a working `npm install -g
@sdsrs/code-graph-linux-x64` was previously invisible to the
launcher even when the binary was sitting at
`~/.nvm/.../lib/node_modules/@sdsrs/code-graph-linux-x64/code-graph-mcp`.

**Fix:** new `globalNodeModulesCandidates()` probes 4 prefix
sources — `process.execPath`-derived (Linux/macOS:
`<prefix>/lib/node_modules`; Windows: next to `node.exe`),
`NPM_CONFIG_PREFIX` env, `~/.npm-global/lib/node_modules`, and
`npm root -g` (last resort, ~50-200ms). New `findPlatformBinary()`
combines fast-path (`require.resolve`) + slow-path (global probe).

**2. `auto-update.js`: trusted state file over filesystem.** When
`installedVersion === latestVersion`, `checkForUpdate` short-circuited
to the no-update branch without verifying that
`~/.cache/code-graph/bin/code-graph-mcp` actually exists. Once the
state file recorded "installed v0.16.6", a wiped cache or a
silently-failed prior download would never be repaired. Real-world
artifact: `update-state.json` says "Up to date" while the cache
directory is empty.

**Fix:** new `downloadBinary()` helper extracted from
`downloadAndInstall` so the binary download can run in either
context. Throttle bypassed when cache binary is missing (a hard
failure overrides the 6h check window). No-update branch
self-heals by calling `downloadBinary(latest)` when binary is
absent. `cachedBinaryPath()` exported for test harnesses.

**3. `mcp-launcher.js`: only one fallback strategy.** When
`findBinary()` returned null, the launcher tried `npm install -g
@sdsrs/code-graph` once and gave up if that didn't yield a binary.
But npm's `optionalDependencies` failure mode is to silently
accept partial installs (an OS-mismatch tolerance feature that
also masks transient registry/network errors), so the wrapper
package would install successfully while the platform binary
package was dropped.

**Fix:** second-stage fallback runs `auto-update.js --silent`
which downloads the platform binary directly from the GitHub
release into `~/.cache/code-graph/bin/`. Bypasses npm registry
entirely. Final error message also names the platform-specific
package (`@sdsrs/code-graph-{platform}-{arch}`) for manual
recovery.

**Tests:** 7 new (`find-binary.test.js` × 4 covering candidate
derivation + dedup + integration; `auto-update.test.js` × 3
covering `cachedBinaryPath` + `downloadBinary` null safety).
117 plugin JS + 385 Rust = 502 total green.

## v0.16.6 — semantic_code_search: doc demotion + find_references: include_tests

Two MCP tool UX bugs surfaced during a user-simulation pass
over the core 7 toolset on this very repo:

**semantic_code_search: README headings outranked code.** Query
`merkle tree change detection` returned `README.md` `License`
(h2, 0.45) / `Features` (h2, 0.44) / `Build` (h3, 0.42) ahead
of `DirectoryCache` struct in `src/indexer/merkle.rs` (0.37).
Root: markdown heading nodes get respectable vector-similarity
scores for unrelated queries (short heading text embeds close
to many concepts), and the re-ranker (`name_boost` /
`size_factor`) had no doc-tier preference. The tool is
`semantic_code_*search*`; for code-intent queries, prose should
not dominate.

**Fix (`src/mcp/server/tools.rs:193-209`):** `doc_penalty = 0.4`
multiplier applied when the candidate's language is `markdown`
AND the caller did not pass `language="markdown"`. Same query
after fix: TOP 6 all from `merkle.rs` / `watcher.rs`, first
result `DirectoryCache` rose to 0.60. Explicit
`language="markdown"` bypasses the penalty (verified
`Installation` h2 comes back at 0.59 for "installation
instructions" queries).

**find_references: no test-filter opt-out.** `upsert_file`
query returned 27 references, 24 of them `test_*` callers,
drowning the 3 production usage sites. Inconsistent with
`get_call_graph` and `get_ast_node include_impact=true`, which
already default to hiding test callers.

**Fix:** new `include_tests` boolean parameter (default `true`
to preserve rename-audit semantics — tests ARE usage sites),
plus `test_references_filtered` count in the response when
callers opt out. Schema published in `src/mcp/tools.rs:131`.
Call with `include_tests=false` to get production-only refs;
call without the flag (or `true`) for the pre-v0.16.6
behavior.

## v0.16.5 — impact_analysis: UNKNOWN risk for non-function symbols

Three impact-analysis paths (`cmd_impact`, `tool_impact_analysis`,
`append_impact_summary`) each maintained their own inline list of
"non-function" node types to flag as UNKNOWN. The lists had drifted:
two only matched `struct|class|enum|interface|type_alias` (missing
`constant` and `trait`), and `append_impact_summary` — the path
reached by the core-7 `get_ast_node include_impact=true` that Claude
Code actually uses — had no type check at all.

Symptom: `code-graph-mcp impact REL_CALLS` returned
`risk_level: LOW, 0 callers` even though 16 importers touch the
constant. An LLM acting on that signal would confidently change the
string and break every importer.

**Fix (`src/domain.rs`):** single source of truth
`is_function_node_type()` + `NON_FUNCTION_IMPACT_WARNING` constant.
All three paths share them. Non-function symbols with zero call-graph
callers now return `risk_level: UNKNOWN` plus an explicit warning
directing to `find_references` / `code-graph-mcp refs <symbol>`.
Function / method impact behavior is unchanged; `HIGH`/`MEDIUM`/`LOW`
still flow from `compute_risk_level` as before.

## v0.16.4 — watcher canonicalize: cfg-gate off Windows (UNC path trap)

v0.16.3 canonicalized the watcher root on every platform to fix
macOS FSEvents; on Windows that regressed the watcher because
`std::fs::canonicalize` there returns UNC paths (`\\?\C:\...`) while
the ReadDirectoryChangesW backend emits plain `C:\...` — the same
`strip_prefix` silently-drop-all-events failure as before, mirrored.
The canonicalize step is now cfg-gated to non-Windows only.

Windows Release workflow (build + npm publish + smoke test) was
always green because the watcher unit tests don't run there; this
only surfaced on the CI matrix.

## v0.16.3 — macOS FSEvents root canonicalization

Follow-up to v0.16.2. After the path-normalization fixes landed,
Windows CI turned green but the two macOS watcher tests still
timed out. Root cause: FSEvents emits every event path via realpath,
so a watch registered on a non-canonical root like
`/var/folders/xx/T/foo` (the `tempfile::TempDir` default on macOS)
could never produce a prefix match against realpath output
`/private/var/folders/...` — every event was silently dropped at
`strip_prefix`.

**Fix (`src/indexer/watcher.rs`):** `FileWatcher::start` canonicalizes
the root path before passing it to notify. No-op on systems without
symlinks in the path; unblocks macOS CI and also hardens production
against project roots with symlinked ancestors (home-dir on systems
where `/home` is a symlink to `/usr/home`, chrooted containers, etc.).

## v0.16.2 — cross-platform path normalization + watcher test stability

Follow-up to v0.16.1. That release fixed Clippy on the 1.95 toolchain,
which let the `Test` step run for the first time on macOS and Windows
in this repo's CI matrix — and immediately surfaced a set of
pre-existing cross-platform bugs the previous red baseline had been
hiding. v0.16.2 addresses them.

**Path normalization (fixes Windows runtime + tests):**
- `src/indexer/merkle.rs` — new internal `normalize_rel_path(&Path)`
  helper converts `\` to `/` on Windows. All relative paths that land
  in the DB, CLI/MCP output, and gitignore-prefix checks now use `/`
  on every platform. Without this, `starts_with(".git/")` style
  filters only fired when the OS used `/`, and Windows users saw
  `pkg\scripts\foo.js` in every tool response.
- `src/indexer/watcher.rs` — notify events go through the same
  normalizer before emission.
- Fixes 4 pipeline tests and 2 merkle tests that were red on
  `windows-latest` in v0.16.1 CI.

**macOS FSEvents flake:**
- `src/indexer/watcher.rs::tests::test_watcher_detects_file_changes`
  — recv_timeout raised from 5s to 15s. macOS FSEvents coalescing on
  loaded GH runners routinely exceeded 5s.
- `src/mcp/server/tests::test_watcher_detects_changes_and_reindexes`
  — replaced fixed 300ms sleep with bounded polling (40 × 200ms
  ≈ 8s total), which is correct on slow hosts and instant on fast.

**CI:**
- `.github/workflows/release.yml` — post-publish smoke now reads
  `map.json` via `fs.readFileSync('map.json',...)` instead of
  `require('$tmpdir/map.json')`. On Git Bash under Windows,
  `mktemp -d` returns a POSIX-looking `/tmp/tmp.XXXX` that Node.js
  on Win32 cannot resolve; the `require` was failing despite the
  file existing.

## v0.16.1 — JS edge resolution precision + CI clippy component fix

**Parser / indexer correctness (JS/TS):**
- `src/parser/relations.rs` — `walk_for_relations` no longer tags
  anonymous arrow functions (`test(() => {...})`, `[1,2].map(x => x)`)
  with the sentinel scope `<anonymous>`, which resolved to no source
  node and silently dropped every call inside such callbacks. Arrows
  without a `variable_declarator` parent now inherit the enclosing
  scope; JS/TS/TSX calls at module top-level fall back to `<module>`
  so they produce resolvable same-file edges. Test-file helpers like
  `writeJson`, `mkHome`, `readCargoVersion` that are referenced only
  from inside `test(...)` callbacks are no longer reported as orphan
  dead code.
- `src/indexer/pipeline.rs` — cross-file same-language resolution used
  to fan out an edge to every same-name target whenever no same-file
  match existed, turning a single `readJson()` call into N phantom
  edges across unrelated modules. New `refine_ambiguous_targets`
  prefers non-test candidates (when the caller is non-test code) and
  the candidate with the longest byte-common path prefix; keeps the
  remaining pool on true ties so Rust bare-name `crate::x::foo()`
  calls that always tie on prefix don't get dropped.

Before v0.16.1 this project indexed 28 cross-file JS `calls`
fan-out edges, all of them pointing at the wrong target in at least
one leg; after, 7 edges, each single-target and correct. `refs
writeJson` rose from 2 → 5 (the 3 real test-callback callers
previously lost).

**CI:**
- `.github/workflows/ci.yml` — `dtolnay/rust-toolchain@1.95.0` now
  installs the `clippy` component explicitly. Without this, the
  Clippy step failed with `'cargo-clippy' is not installed for the
  toolchain '1.95.0'` on every OS/feature-matrix cell in v0.16.0.

## v0.16.0 — production hardening pass (RRF math, schema v7 dim guard, readonly secondary, bounded watcher, CI matrix)

Architecture audit surfaced nine correctness / safety gaps — this
release addresses all of them plus four items flagged in a follow-up
code review. Schema bump auto-migrates; no user action required.

**Algorithmic correctness:**
- `src/search/fusion.rs` — `SCORE_BLEND_FACTOR = 0.1` silently dominated
  RRF by ~100× at k=30 (rank-0 RRF ≈ 0.0164 vs. max blend = 0.1),
  inverting the docstring's own "doesn't override rank ordering"
  contract and effectively converting RRF into per-source-raw-score
  ranking. Replaced with adaptive `blend_scale = 0.5 / ((k+1)(k+2))` —
  mathematically half the smallest adjacent-rank RRF gap. Semantic
  search results will shift (for the better) on queries where one
  source returns a high-raw-score item at a late rank.

**Data safety — schema v7 embedding-dim guard:**
- `SCHEMA_VERSION` 6 → 7. New `meta` table records embedding_dim. On
  open, mismatch → atomic DROP + rebuild `node_vectors` at current
  `EMBEDDING_DIM`. Prevents silent crash-on-INSERT when a user rebuilds
  the binary at a different dim (e.g., swaps embedding model).
- v6 → v7 upgrade path introspects the on-disk vec0 DDL via
  `sqlite_master.sql` (`float[N]` regex) and rebuilds if the existing
  table's dim ≠ current — the adversarial case where `meta` is empty
  but a pre-existing vec0 is present.

**Concurrency hardening:**
- `src/indexer/watcher.rs` — bounded `sync_channel(4096)` with
  overflow-drop policy (warn!). Unbounded channel had no cap on memory
  during bulk fs events (branch switches, IDE reformats). Merkle
  rescan is idempotent so dropped events don't lose data.
- `src/storage/db.rs` + `src/mcp/server/mod.rs` — secondary instances
  (flock denied) now open DB with `SQLITE_OPEN_READ_ONLY | query_only=ON`.
  Eliminates race where a secondary could run migrations +
  `INDEX_VERSION` DELETE sweep against the primary's DB. Secondary
  polls up to 3s for the primary's bootstrap then bails with a clear
  error rather than falling through to read-write.

**Contract strengthening:**
- `src/parser/relations.rs` — `ParsedRelation` carries `source_language`,
  stamped by `extract_relations_from_tree`. Resolver at
  `src/indexer/pipeline.rs` hard-errors on mismatch (bail!, not
  debug_assert!) so parser regressions fail in release builds too.
- `src/mcp/server/mod.rs` — `start_post_index_services` spawns a
  once-per-process Phase-3 repair thread before background embedding.
  README's "Startup repair for incomplete indexing" claim was
  documentation-only until now; `repair_null_context_strings` now
  actually fires on every session start (primary-only, idempotent).

**Documentation accuracy:**
- `README.md` — HTTP route tracing previously claimed Express, Flask/
  FastAPI, Go, ASP.NET, Rails, Laravel, Vapor (8 frameworks). Only 3
  are actually implemented in `extract_route_pattern`. Corrected.

**CI + release:**
- `.github/workflows/ci.yml` — matrix {ubuntu, macos, windows} × {no-embed,
  with-embed} (was ubuntu-only), toolchain pinned `@1.95.0`.
- `.github/workflows/release.yml` — new `smoke-verify` job runs after
  `publish` on all 3 OSes: npm install with retry-backoff, `--version`
  exact match, `incremental-index` + `map --json` on a tmp git repo.
  Catches missing platform binaries / `find-binary.js` regressions /
  version-sync drift before users hit them.

**Test delta:** +18 unit tests (RRF invariants ×4, schema v7 paths ×5,
readonly ×2, source_language stamp ×1, etc.). 250 unit + 56 integration
+ 44 hardening + 19 parser + 6 cli + 6 plugin + 1 routing = 382 tests
pass. Clippy 1.95 clean on both feature modes.

**Deferred to a later release (L3 refactor):** `tools.rs` (2236 LOC),
`relations.rs` (2174), `queries.rs` (2783) file splits — flagged in the
audit but require a dedicated session with plan-mode review.

## v0.15.2 — ast_search ranking + dead-code --json empty contract

User-driven QA pass exercising every MCP tool + CLI subcommand surfaced
two bugs whose contract violations were silent — both regressions guard
against recurrence.

Fixes:
- `src/storage/queries.rs` — `get_nodes_with_files_by_filters` (the SQL
  backing `ast_search` / `ast-search`) ordered by `f.path ASC` only, so
  the `LIMIT` clause silently truncated alphabetically-late files
  (`src/storage/queries.rs` itself, with 54 `Result`-returning fns) out
  of the top-N. New ordering is `caller_count DESC, path ASC, line ASC`
  so high-value symbols surface first regardless of file path.
- `src/cli.rs:2655` — `dead-code --json` returned only stderr (no stdout)
  when all results were filtered by `--ignore`, breaking JSON consumers
  piping stdout. Now emits `[]` to stdout before the human stderr
  message, matching the established empty-result contract used by
  `search` / `grep` / `callgraph` / `show` / `trace` / `overview`.

New regression tests:
- `test_get_nodes_with_files_by_filters_ranks_by_caller_count`
  (`src/storage/queries.rs`) — alphabetically-first low-caller fn must
  not outrank alphabetically-last high-caller fn at any `LIMIT`.
- `test_cli_json_empty_dead_code` (`tests/cli_e2e.rs`) — stdout must be
  `[]` and stderr must still surface "No dead code" when --ignore filters
  all results.

371 tests pass (was 369). Clippy 1.95 clean on both feature combos.

## v0.15.1 — TSX parity: LanguageConfig + require() + Express routes

v0.15.0 audit of JS/TS support surfaced a silent breakage for `.tsx`
files: `LanguageConfig::for_language("tsx")` hit the default arm where
`_ => "unknown"`, so every `config.name == "tsx"` branch was dead code.
Ripple effect: the describe/it `is_test` propagation added in v0.15.0
(scoped `matches!(config.name, ... | "tsx")`) silently skipped TSX.

Fixes:
- `src/parser/lang_config.rs` — add `"tsx" => "tsx"` to the static-name
  match so `config.name` is preserved through the default-config branch.
- `src/parser/relations.rs:101` — `require()` arm now matches
  `"javascript" | "typescript" | "tsx"` (was js/ts only).
- `src/parser/relations.rs:1172` — `extract_route_pattern` now routes
  `"tsx"` through `extract_express_route` alongside js/ts.

Two new regression tests: `test_extract_tsx_commonjs_require_and_route`
(parser) and `test_parse_tsx_describe_it_marks_nested_as_test`
(treesitter). 369 total tests pass.

C/C++ coverage audit surfaced three parallel gaps — `#include`
not extracted, GoogleTest `TEST`/`TEST_F`/`TEST_P` macros not
recognized, no scope qualification for `Class::method` / `obj.method` /
`obj->method`. Tracked for v0.16.0.

## v0.15.0 — same-language edge resolution, JS require() imports, markdown indexing, JS test-block detection

Multi-front accuracy pass motivated by user feedback that code-graph was
useful in Rust projects but under-utilized in JS / mixed / claudemd
projects. Traced to four compounding issues; all four fixed in this
release with regression tests.

### Same-language edge resolution — eliminates cross-language phantom edges

`src/indexer/pipeline.rs` resolved call/implements/imports target names
via a flat global bare-name lookup. In mixed-language projects this
produced catastrophic false positives: the Rust `hasher.update(&buf)`
call in `src/indexer/merkle.rs:hash_file` was resolving to the JS
`function update()` in `claude-plugin/scripts/lifecycle.js`, pulling
11 phantom Rust→JS edges into `callgraph hash_file` (verified via
dogfood before/after). Each same-named method (`update`, `open`,
`init`, `run`, `read`, `write`, etc.) was a collision vector.

Fix: edge resolution now uses a three-tier cascade — `same-file` →
`same-language` → (for calls: drop; for imports/implements: global
fallback to preserve the existing `<external>` sentinel path).
Non-call relations keep cross-language fallback because sentinel
nodes carry language `"external"` by design.

Mechanically, `get_all_node_names_with_ids` and the per-batch
`node_id_to_path` map now carry each node's `language`, enabling the
filter. Public type alias `NameEntry = (i64, String, Option<String>)`
added to keep clippy `type_complexity` happy.

Regression test `test_cross_language_bare_name_call_resolution`
plants an `update` collision across a Rust file and a JS file and
asserts that Rust `caller_rs` does not resolve any call edge to the
JS file.

### CommonJS `require()` — JS import edges appear for the first time

`src/parser/relations.rs` handled ES module `import` statements but
had no branch for `require(...)` calls, the canonical CommonJS form.
Consequence: Node.js code bases (including this repo's own
`claude-plugin/scripts/*.js`) had 3 total `imports` edges across 19
JS files before the fix. After the fix: 286 edges (path 27, fs 24,
child_process 18, os 17, plus local modules).

Require detection inserted into the existing `call_expression` arm;
handles `node:fs` scheme normalization and strips `.js`/`.ts`/`.mjs`/
`.cjs` suffixes so `require('./utils/version-utils.js')` resolves to
the same target as an ES `import` binding named `version-utils`.
Unresolved imports flow into the existing Phase 2b-ext external-
sentinel mechanism (previously only wired for implements), so
`<external>/fs` nodes now exist and are discoverable via `deps <file>`
dependency graphs.

Two new tests: `test_extract_js_commonjs_require` (parser level,
covers node scheme + extension stripping + relative paths) and
`test_js_require_creates_external_import_edges` (pipeline level,
end-to-end DB assertion).

### Markdown heading indexing — claudemd / docs projects become navigable

Added `tree-sitter-md = "0.3"` (pinned to 0.3 because 0.5.x ships
tree-sitter ABI 15 and this repo still runs tree-sitter 0.24 / ABI 14).
`detect_language` accepts `.md` / `.mdx`; `LanguageConfig` exposes
"markdown" for the default-config fallthrough; `extract_nodes` new arms
for `atx_heading` (walks marker children to infer level 1–6) and
`setext_heading` (paragraph + `setext_h{1,2}_underline`). Heading text
becomes the node name, `h1`..`h6` the node type. Searchable via FTS;
visible in `module_overview` and `project_map`.

Dogfood: this repo's README, CHANGELOG, and 4 plugin docs now yield
145 heading nodes. `code-graph-mcp search "Installation"` returns
`h2 Installation README.md:117` as the top hit.

Shell and JSON indexing deferred — tree-sitter-bash adds real value
for hook-script projects; JSON alone is low-yield because the useful
relations (hook → script name) cross file formats. Both tracked as
follow-up.

### JS `describe` / `it` / `test` AST blocks mark nested code as test

`LanguageConfig::has_test_attributes = false` for JS/TS because the
test framework is function-call-driven, not attribute-driven. The
existing `is_test_symbol` file-path heuristic caught `.test.js` /
`.spec.js` / `__tests__/` patterns but missed **in-source** test code
(Vitest in-source testing, Jest co-location without the suffix, or
any file that mixes prod + test definitions).

`extract_nodes` now intercepts `call_expression` nodes whose function
head is one of `describe`, `it`, `test`, `suite`, `context`,
`beforeEach`, `beforeAll`, `afterEach`, `afterAll`, `before`, `after`,
`fdescribe`, `xdescribe`, `fit`, `xit` (both bare and `.only` / `.skip`
/ `.each` member forms). Child argument nodes recurse with
`in_test_context = true` which flows into the existing `is_test` field
on every nested function / class / method.

Regression: `test_parse_js_describe_it_marks_nested_as_test` plants
6 definitions across `describe` / `it` / `it.skip` / `beforeEach`
nesting and asserts the `is_test` propagation is correct (plus a
top-level prod function stays `is_test=false`).

### Test + dogfood summary

367 total tests pass (+4 net new). `cargo +1.95.0 clippy --all-targets
-- -D warnings` clean. Full rebuild on this repo: 84 files → 1295
nodes → 2590 edges (was 1068 / 2300 pre-release). Net per-dimension:
- phantom Rust→JS call edges: 11 → 0
- JS imports edges: 3 → 286
- markdown heading nodes: 0 → 145
- indexed languages: 16 → 17

## v0.14.5 — FK-recovery fix, rebuild_index busy-timeout relief, error-kind telemetry

Patch release. Drops six observed bug classes surfaced by a full-fleet
error-rate audit over 156 MCP sessions + 55 Claude Code transcripts.

### Incremental-index FK recovery now truncates before rebuild

Historical transcripts showed 6 agent-side `FOREIGN KEY constraint failed`
errors on `project_map` (4), `module_overview` (1), and
`semantic_code_search` (1). Root cause: `run_incremental_with_cache_restore`
caught FK violations and fell back to `run_full_index`, but the latter
only does per-file upsert — orphan rows from the failed incremental
survived and re-triggered FK on the retry, bubbling the raw SQLite
error to tool handlers.

Fix (`src/mcp/server/mod.rs:987`): the FK branch now `DELETE FROM files`
in a transaction before re-running full_index. CASCADE chains nodes →
edges → node_vectors via the schema's existing `ON DELETE CASCADE`.
Pattern lifted verbatim from `tool_rebuild_index`.

Regression test (`test_fk_fallback_truncate_purges_stale_state_and_rebuild_recovers`)
injects a phantom file + node + edge via `PRAGMA foreign_keys = OFF`
and asserts truncate + full_index purge it while restoring on-disk
symbols. Guards against future removal of the truncate step.

### `rebuild_index` 10s "busy" cliff relaxed to 30s

`usage.jsonl` showed `rebuild_index` err-rate 5/9 = 55%, with all 5
failures hitting `max_ms ≈ 10009` — i.e. the `embedding_in_progress`
wait deadline, returning `{status:"busy"}` which session metrics count
as errors. Not a real failure mode; 30s accommodates larger projects
whose embedding pass exceeds 10s.

### `find_dead_code` excludes anonymous `_` constants

`const _: () = assert!(...)` and `let _ = ...` patterns are
compile-time-only bindings, never callable. They were being reported
as dead code. New filter in `find_dead_code` SQL: `n.name != '_'`.

### Canonical error-kind telemetry in `usage.jsonl`

`SessionMetrics::record_tool_call` now classifies failures into
`ErrKind { Timeout, NotFound, Ambiguous, FkConstraint, EmptyInput, Other }`
and emits per-tool breakdowns as `tools.<name>.err_kinds`:

```json
"get_ast_node": {"n": 69, "ms": 4630, "err": 12, "max_ms": 2003,
                 "err_kinds": {"timeout": 7, "ambiguous": 3, "not_found": 2}}
```

Additive — readers that only consume `n/ms/err/max_ms` are unaffected.
Success-only tools omit the `err_kinds` field entirely for compact
output. Unlocks post-hoc error analysis via `jq` instead of manual
transcript grep.

### Dev tooling: `scripts/analyze-search-queries.py`

Persistent sampler that classifies `code-graph-mcp search` queries
issued by the agent (extracted from Claude Code transcripts) into
keyword-like vs concept-like. Used to validate decisions about
MCP-vs-CLI routing trade-offs without needing a round-trip through
`routing_bench`.

---

## v0.14.4 — CLI `impact`/`callgraph` ambiguous-symbol guard (parity with MCP)

Patch release. Closes a CLI/MCP behavior gap discovered in the same
end-to-end audit that produced v0.14.3.

### Bare-name queries on overloaded symbols now prompt for disambiguation

MCP `get_call_graph` and `get_ast_node` already returned an
`Ambiguous symbol` error with suggestion list when a bare name
resolved to ≥2 non-test definitions in different files. The CLI
counterparts (`callgraph`, `impact`) did not — they silently merged
call graphs / caller lists across all same-named definitions,
misreporting risk_level and blast radius.

Example: this repo has two `open` functions (`Database::open` in
`src/storage/db.rs` and `CliContext::open` in `src/cli.rs`). Before
the fix:

```
$ code-graph-mcp impact open
Impact: open — Risk: HIGH
  26 direct callers, 31 total, 9 files ...
```

The 26 callers are a union of both `open`s. After the fix:

```
$ code-graph-mcp impact open
[code-graph] Ambiguous symbol 'open': 2 matches in different files.
Specify --file or --node-id:
  open (function) in src/storage/db.rs [node_id 5717]
  open (function) in src/cli.rs [node_id 7055]
```

Exit code 1 signals script-level callers that disambiguation is
required. Qualified names (`Database.open`), `--file`, and `--node-id`
paths still work unchanged.

### Implementation

New helper `detect_exact_ambiguity` in `src/cli.rs` queries
`get_nodes_with_files_by_name`, filters non-test definitions, and
returns `Some(candidates)` only when ≥2 distinct files are present
(multiple definitions in one file, e.g. overloads, stay
non-ambiguous). Shared `emit_exact_ambiguity` formatter handles both
`--json` and human modes.

Both `cmd_callgraph` and `cmd_impact` gain a `file_filter.is_none()`
guard that invokes the helper before the downstream query runs.

### Verified

`cargo test` 235/235, `cargo +1.95.0 clippy --all-targets` clean.

## v0.14.3 — module_overview compact truncation fields + CLI deps `<external>` parity

Patch release. Two UX bugs found during end-to-end tool audit.

### MCP `module_overview` compact mode — surface truncation metadata

Full mode already set `active_capped`/`showing`/`total_active`/`hint`
when a module had >30 active exports, but `compact_module_overview`
rebuilt the response by cherry-picking known fields and silently
dropped the conditional truncation fields. Users calling with
`compact=true` on a large module (e.g. `src/parser/` with 54 active
exports) saw `"summary": "54 active + 2 inactive"` and 30 items — no
signal that 24 were missing.

Fix: forward the four conditional fields at the end of
`compact_module_overview` with a `.get().cloned()` loop so any future
addition of a conditional field stays forwarded by default.

### CLI `deps` — filter synthetic `<external>` bucket like MCP does

`dependency_graph` in the MCP handler filters the `<external>` pseudo-
file (a container for unresolved third-party imports) from outgoing
deps. The CLI `deps` subcommand had the language-compat filter but not
the `<external>` guard, so CLI output at depth ≥2 could show
`<external>` as a fake file dependency.

Fix: add the one-line guard to `cmd_deps`'s `is_compatible_lang` so
both entry points apply the same filter.

### Verified

`cargo test` 235/235, `cargo +1.95.0 clippy --lib -- -D warnings`
clean. Before/after:

- `module_overview(path="src/parser/", compact=true)` now returns
  `active_capped: true, showing: 30, total_active: 54, hint: "..."`
- `deps src/mcp/server/tools.rs --json` depends_on no longer contains
  `{"file":"<external>","depth":2}`

## v0.14.2 — MCP init instructions fit Claude Code truncation budget

Patch release. Fixes observed silent truncation of the MCP `initialize`
response `instructions` field at Claude Code's ~2KB harness boundary — the
last 4 of 10 routing decision rules were being dropped, making Claude
fall back to Grep/Read where code-graph tools should have been invoked.

### MCP `instructions` — pack 10 decision rules under 1500-byte budget

Old noisy-mode instructions were ~2.5KB with three section headers and
verbose workflow tips. Claude Code's `initialize` handler truncated near
~2048 bytes, cutting `modifying a function signature`, `find_dead_code`,
`find_similar_code`, `dependency_graph`, and the `get_ast_node` row — all
critical routing signals.

Rewrite compresses to **1292 bytes** (~48% of original) while preserving
all 10 decision rules verbatim. Each rule now carries its CLI alias
inline (e.g. `get_call_graph (callgraph X)`), so the LLM learns the CLI
invocation from the same line it learns the routing intent — no separate
MEMORY.md cross-reference needed for the base case.

Also re-adds a `Prompts:` line enumerating the three registered MCP
prompts, and replaces the misleading `"5 CLI-only tools"` phrasing with
`"5 advanced tools"` — the hidden 5 are still callable via raw MCP
`tools/call`, they are just off `tools/list` by default to preserve
startup-token budget.

### Compile-time budget guard

`const _: () = assert!(NOISY.len() <= 1500, ...)` added in
`src/mcp/server/mod.rs`. Any future edit that blows the budget fails
`cargo check` with `rustc E0080: evaluation panicked` — catches the
regression at build time, not debug-build test time. Verified by
tightening the cap to 1000 and observing the compile break.

### CLI `search` — stderr hint directing concept queries to MCP

CLI `code-graph-mcp search <q>` is FTS5-only; the MCP
`semantic_code_search` tool adds vector similarity + RRF fusion. On
non-JSON success paths, a stderr tip now points concept-query users to
the MCP tool. `--json` mode is untouched so script consumers still see
clean stdout.

### Tests

366 tests pass across integration suites (v0.14.1 baseline + compile-time
assert test exercised via intentional budget-cap inversion). Clippy 1.95
clean on both `--no-default-features` and `--all-targets`. Routing bench
(`tests/routing_bench.rs` via OpenRouter `anthropic/claude-sonnet-4.5`):
**P@1 = 19/20 = 95.0%** — unchanged from the v0.14.1 baseline, confirming
the compression did not degrade routing quality. Single miss remains the
known-borderline `ast_search` vs `get_ast_node` on a struct-def lookup.

---

## v0.14.1 — semantic search UX + find_references type hint

Patch release. Six targeted accuracy/UX fixes to MCP tool responses surfaced by a
3-round smoke test. All changes are additive or remove false-positive warnings;
no schema changes, no behavior regressions.

### `semantic_code_search` — compression estimator aligned to actual output

The compression trigger estimated token cost from `context_string` (can exceed
2000 chars) but the actual result JSON only carries `code_content` capped at
`MAX_SEARCH_CODE_LEN = 500`. Small `top_k` queries (3, 5) were being forced into
`compressed_nodes` mode unnecessarily, losing `relevance` and `signature` fields.

Estimator now mirrors the output: it measures truncated `code_content` +
signature + name + path + ~80 chars JSON framing per result. Small `top_k`
responses return full arrays again.

### `semantic_code_search` — `match_confidence` + `low_confidence_warning`

Compressed responses (`compressed_nodes` / `compressed_files` /
`compressed_directories`) now include a rounded `match_confidence` float. When
`< 0.5`, a `low_confidence_warning` string explains that FTS found few matches
and results are likely vector-similarity noise, with advice to use concrete
identifiers or `ast_search`.

The FTS sparsity and source-intersection penalties used to over-fire on
precision queries (single-identifier FTS hits). The penalty now requires
`fts_search.len() >= 5`; below that, the query is treated as precision-mode
and not penalized.

Exact-name-match exemption: when any top-5 candidate's `name` or
`qualified_name` equals the query (case-insensitive), the warning is
suppressed. `match_confidence` is still returned so callers can judge.

### `find_references` — `type_definition_note` for type symbols

When the target is a `struct` / `enum` / `trait` / `type` / `interface` /
`class`, the response now includes a `type_definition_note` explaining that
the edge index captures explicit imports/inherits/implements and
struct-literal instantiation, but NOT method-qualified calls
(`Type::method()`), field access, or type annotations. Guides the caller to
query each method via `module_overview` for a complete rename audit.

### `get_index_status` — `embedding_coverage_pct` floor

When embedding is in progress with a small fraction done (e.g. 2/1052),
integer percent rounded to 0 and looked stuck. Now floors to 1 whenever
`vectors_done > 0`, so `embedding_status: in_progress` stays consistent with
the percentage.

### `get_ast_node(node_id)` — explanatory not-found error

`Node N not found` replaced with a message that explains node_ids are
rebuild-scoped and suggests re-resolving via `get_ast_node(symbol_name,
file_path)` or `semantic_code_search`.

### Tests

43 `mcp::server` unit tests remain green. Routing bench
(`tests/routing_bench.rs` via OpenRouter `anthropic/claude-sonnet-4.5`):
**P@1 = 19/20 = 95.0%** (threshold 70%). Single miss is a semantic-neighbor
pick (`ast_search` vs `get_ast_node` for a struct-def lookup) unrelated to
this release.

---

## v0.14.0 — durable statusline-provider chain + public register CLI

Minor release. Addresses a long-standing fragility in the composite statusline
integration: when the user cleaned `~/.cache/code-graph/`, the `_previous`
snapshot (pre-install statusline, e.g. GSD) was lost, leaving only code-graph
visible on the status bar.

### Durable backup for `statusline-registry.json`

`writeRegistry()` in `claude-plugin/scripts/lifecycle.js` now mirrors the
registry to `~/.claude/statusline-providers.json` on every write. This file
lives outside the `~/.cache/` hierarchy, so routine cache cleanup no longer
strands third-party provider entries.

`readRegistry()` self-heals: if the primary `~/.cache/code-graph/statusline-registry.json`
is missing or empty, it falls back to the durable backup and rewrites the
primary. No user action needed on upgrade — the first `writeRegistry()` call
after install writes both files; recovery from a prior cache wipe happens
automatically on next SessionStart.

Clearing the registry (e.g. during uninstall) clears both files.

### New public CLI: `statusline-chain.js`

`claude-plugin/scripts/statusline-chain.js` exposes a documented registration
surface for third-party plugins that want to coexist with code-graph's
composite statusline:

```
node <plugin-cache>/scripts/statusline-chain.js register <id> <command> [--stdin]
node <plugin-cache>/scripts/statusline-chain.js unregister <id>
node <plugin-cache>/scripts/statusline-chain.js list
```

Reserved ids (`_previous`, `code-graph`) are rejected with exit code 2. The
CLI uses existing `registerStatuslineProvider` / `unregisterStatuslineProvider`
so writes land in both primary + durable backup.

**Motivating use case:** GSD currently owns `settings.json.statusLine`
directly and is captured as `_previous` when code-graph installs. With this
CLI, GSD's install hook can instead call `statusline-chain.js register gsd
"<gsd-statusline-command>" --stdin` and become a first-class provider in the
composite, independent of install order. Fallback path (call without `--stdin`
if the command doesn't read stdin; skip call entirely if code-graph isn't
installed) keeps standalone operation working.

### Tests

Four new cases in `lifecycle.test.js`:

- `writeRegistry` mirrors to durable backup
- `readRegistry` self-heals primary from backup after simulated cache wipe
- `writeRegistry([])` clears both files
- `statusline-chain.js` CLI register/list/unregister + reserved-id guard

12/12 lifecycle tests pass; 228/228 Rust lib tests green; clippy 1.95 clean on
both `--no-default-features` and `--all-targets`.

## v0.13.0 — `stats` CLI + rebuild_index busy semantics + CLI/MCP search disambiguation

Minor release. Three changes driven by real-usage-data review:

### `stats` subcommand (new)

`code-graph-mcp stats` aggregates `.code-graph/usage.jsonl` across sessions
and prints per-tool counts (`n`, `avg_ms`, `err`, `max_ms`), search totals
(queries, zero-result ratio, hybrid/FTS split, avg quality), and index
activity (full vs incremental, avg full-rebuild time). Flags: `--last N`
limits to the most recent N sessions, `--json` emits structured output.

Motivation: the metrics module has been writing JSONL for months (1MB
rotation), but there was no reader. Running on this repo's own history
surfaced the `rebuild_index` error pattern that motivates change #2.

### `rebuild_index` MCP tool — busy signal is no longer an error

When the server rejects a rebuild request because background embedding is
still running, it now returns `Ok({status: "busy", retry_after_ms: 2000})`
instead of `Err("Background embedding still in progress")`. This matches
the precedent in `run_incremental_with_cache_restore` (which returns
`Ok(())` on the same condition) and keeps the usage-metrics `err` counter
from inflating on legitimate retry signals.

**Contract change** — SDK/script clients of the `rebuild_index` MCP tool
must now distinguish `status: "busy"` success payloads from actual errors.
JSON-RPC-level errors on `rebuild_index` now indicate real failures only
(missing `confirm`, no project root, DB error).

### CLI ↔ MCP search disambiguation

`plugin_code_graph_mcp.md` template previously listed `search "Z"` and
`semantic_code_search` as equivalent intents. They are not: the CLI
`search` command is **FTS5-only** (`src/cli.rs:710` → `fts5_search`), while
the MCP `semantic_code_search` tool performs **RRF fusion** of FTS5 + vector
similarity (`src/mcp/server/tools.rs:42 → 101`). The template now states
this explicitly in the core-7 decision table and the CLI cheat sheet.

Adopted memory files auto-refresh from the template on the next
SessionStart (v0.11.0+ behavior).

### Clippy 1.95 parity

Four `clippy::manual_checked_ops` and one `clippy::unnecessary_sort_by`
flagged by the 1.95 toolchain in the new `cmd_stats` code path are fixed
before push (local baseline: `cargo +1.95.0 clippy --no-default-features
-- -D warnings && cargo +1.95.0 clippy --all-targets -- -D warnings`,
both green).

## v0.12.1 — incremental-index skips non-project directories

Bugfix release: the PostToolUse `incremental-index` hook no longer creates
`.code-graph/` in directories that are not project roots. In multi-repo
workspace layouts (one parent dir containing N independent git repos, parent
not itself a repo), the hook previously materialized a stray 16 MB+ index at
the workspace parent, overlapping every child repo.

### What changes

`src/main.rs` incremental-index arm now bails silently when the resolved
project root has neither a `.git` anchor nor an existing
`.code-graph/index.db` (the index check preserves the explicit per-dir index
case where a user deliberately ran `incremental-index` in a non-git folder).

Silent-skip matches the prevailing hook-layer convention:
`incremental-index.js` swallows errors, `CliContext::try_open` returns `None`,
`session-init.js` returns `'skipped'`.

### Test coverage

`claude-plugin/scripts/incremental-index.test.js` — two cases:
- non-git tmpdir → exit 0, `.code-graph/` not created
- fake `.git/` tmpdir → exit 0, guard does not block

### Credits

Reported + fixed by @jgangemi (issue #8, PR #9). Re-landed on top of current
`resolve_project_root_from` helper with doc-comment scope creep removed.

## v0.12.0 — Scenario-keyed MEMORY.md index (auto-adopt template refresh)

Auto-adopt (`claude-plugin/scripts/adopt.js`) now seeds MEMORY.md's sentinel
block with a 5-row scenario→tool table in addition to the existing tool-name
list. The always-loaded context gap this closes: Claude Code knew the 7+5 tool
names but not the natural-language triggers ("who calls X?", "改 X 影响面")
that should route to them, so sessions silently slid to `Grep` / `Read` when a
code-graph tool would be more precise. The scenario phrases now live in the
200-line-capped MEMORY.md itself, not a second-hop `plugin_code_graph_mcp.md`.

### What changes

Sentinel `<!-- code-graph-mcp:begin v1 -->...<!-- code-graph-mcp:end -->` grows
from 3 lines to 9. Added block (nested under the existing index entry):

    - 场景速查（优先于 Grep）：
      - 改 X 影响面 → `get_ast_node symbol=X include_impact=true`（或 CLI `code-graph-mcp impact X`）
      - 谁调用 X / X 被谁用 → `get_call_graph X` 或 `find_references X`
      - 看 X 源码 / 签名 → `get_ast_node symbol=X`
      - Y 模块长啥样 → `module_overview` 或 CLI `code-graph-mcp overview Y/`
      - 概念查询（不知精确名）→ `semantic_code_search "Z"`；字面匹配用 Grep

### Migration — existing adopted projects

`needsRefresh()` detects INDEX_LINE drift automatically; the sentinel block
rewrites once on next SessionStart. No user action required.

### Opt-out

- Lock current MEMORY.md block against this refresh: `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` (shipped in v0.11.0)
- Disable auto-adopt entirely for new projects: `CODE_GRAPH_NO_AUTO_ADOPT=1` (shipped in v0.9.0)
- Downgrade: reinstall `0.11.6` to restore the 3-line INDEX_LINE

### Verification

- `adopt.test.js`: 37/37 green — tests reference the `INDEX_LINE` constant, so the content extension is transparent.
- `routing_bench`: 19/20 = 95.0% on `anthropic/claude-sonnet-4.5` via OpenRouter — unchanged from v0.11.6. This release doesn't touch `ToolRegistry` descriptions, which is what the bench measures; the adopted MEMORY.md lives outside the oracle's prompt.

## v0.11.6 — Tool-description tightening (+5% routing P@1) + OpenRouter backend

First run of the routing-recall benchmark landed v0.11.4 at **P@1 = 18/20 = 90.0%**
(`anthropic/claude-sonnet-4.5` via OpenRouter). The two misses were both semantic
overlaps between adjacent tools. This release tightens 4 tool descriptions and
re-runs the bench: **P@1 = 19/20 = 95.0%**, a net +5.0 points with one miss
remaining (borderline — "show me the EmbeddingModel struct" routes to `ast_search`
with `type=struct`, which returns the right answer albeit via the "enumerate"
tool rather than the "inspect ONE" tool).

### Tool-description changes (`src/mcp/tools.rs`)

All stay under the 200-char registry limit.

- **`get_call_graph`** — leads with `"Who calls X, what X calls"` + `"Returns a
  graph (not a flat list)"`. Fixed routing for "Who calls ensure_indexed?"
  (was → `find_references`, now → `get_call_graph`).
- **`find_references`** — leads with `"Flat enumeration of all usage sites"` +
  explicit deflection: `"For 'who calls X?', use get_call_graph."`.
- **`get_ast_node`** — leads with `"Inspect ONE named symbol"` + `"you have a
  symbol name (or node_id) and want its definition/body"` to claim the
  "show me X / signature of Y" intent.
- **`ast_search`** — leads with `"Enumerate MULTIPLE symbols by structural
  criteria"` + deflection: `"For ONE known symbol, use get_ast_node."`.

Pattern: each description now leads with a shape verb (`who calls`, `flat
enumeration`, `inspect ONE`, `enumerate MULTIPLE`) and points at the
adjacent tool when a query drifts into overlap.

### Routing-bench OpenRouter backend (`tests/routing_bench.rs`)

Auto-detects `ANTHROPIC_API_KEY` (native Messages API) or `OPENROUTER_API_KEY`
(OpenAI-compatible `/chat/completions`). Tool schemas re-packaged as
`{type: "function", function: {...}}` for the OpenRouter path. Model default
`anthropic/claude-sonnet-4.5`; override with `ROUTING_BENCH_MODEL`. Anthropic
wins if both keys present.

### Baseline measurement (published)

| Run | Backend / Model | P@1 |
|-----|-----------------|-----|
| v0.11.4 baseline | openrouter / anthropic/claude-sonnet-4.5 | 18/20 (90.0%) |
| v0.11.6 post-tightening | openrouter / anthropic/claude-sonnet-4.5 | 19/20 (95.0%) |

Cost ≈ $0.10/run. Threshold stays at 0.70; consider raising to 0.85 after two
more releases confirm 95% as stable baseline (20-query sample is within model
stochasticity range).

## v0.11.5 — Hotfix: clippy 1.95 parity (`unnecessary_sort_by`)

`-D warnings` on stable clippy 1.95 flagged the two `sort_by(|a, b| b.0.cmp(&a.0))`
calls added in v0.11.4 rollup. Local clippy (0.1.91, ~4 months behind stable)
accepted them. Functional behavior unchanged.

### Fix

- `src/mcp/server/tools.rs:503-504`: `sort_by(|a, b| b.0.cmp(&a.0))` →
  `sort_by_key(|e| std::cmp::Reverse(e.0))` (applied exactly as clippy suggested).

### Why v0.11.4 shipped red

Local pre-push ran `cargo clippy --all-targets -- -D warnings` — passed on 0.1.91.
CI uses `dtolnay/rust-toolchain@stable` which pulls whatever's latest
(1.95.0 at ship time), catching `clippy::unnecessary_sort_by` which landed post-0.1.91.
Functional code from v0.11.4 is unaffected; only the `-D warnings` gate broke.
v0.11.4 tag + release left pointing at the failing commit as a historical artifact.

## v0.11.4 — Integration-friction fixes: ast_search hint + acronym expansion + call graph rollup

Integration-test pass against Claude Code found three specific friction points
where tool responses forced a second round-trip or missed relevant nodes.
All three fixed. Additive — no schema change, no re-index.

### Fixes

1. **`ast_search` generic-fallback hint.** When `returns="Vec<Relation>"` yields
   zero hits because the codebase uses `Vec<ParsedRelation>`, the response now
   carries `hint` + `suggested_query` instead of a bare `count: 0`. Example:
   `{ "count": 0, "hint": "No match for returns='Vec<Relation>'. Substring
   'Relation' has 7 matches — try that.", "suggested_query": {"returns":
   "Relation", "type": "fn"} }`. Strip rule: innermost `<…>` wins; multi-param
   types take the last comma-separated param. See
   `src/mcp/server/helpers.rs::strip_outer_generic`.

2. **Acronym query expansion.** `fts5_search` preprocessing now expands
   common CS/IR/DB acronyms into full-form terms alongside the original:
   `RRF` → `RRF` + `reciprocal` + `rank` + `fusion`; same for `BM25`, `FTS`,
   `AST`, `LSP`, `MCP`, `RPC`, `SQL`, `ORM`, `CTE`, `JWT`, `TTL`, `DAG`,
   `RBAC`, `CRUD`, `CORS`. Benchmark before/after on query `"RRF fusion BM25"`:
   `weighted_rrf_fusion` now appears at rank 3 (previously absent from top-5).
   New static dict in `src/search/acronyms.rs`; expansions deduped via the
   existing BTreeSet pass.

3. **`semantic_code_search` acronym-heavy FTS bias.** Queries that are entirely
   short uppercase tokens (≤3 tokens, each ≤5 chars, all `[A-Z0-9]`) now run
   with `fts_weight=2.0, vec_weight=0.8` instead of the default `1.0/1.2`.
   Rationale: embeddings handle letter-exact acronyms poorly while FTS5's
   token-exact match is reliable; shift the weight toward the precise channel.

4. **`get_call_graph` file-level rollup replaces `compressed_call_graph`.**
   When the flat node list exceeds `COMPRESSION_TOKEN_THRESHOLD` (previously
   this mode dumped the raw list anyway), group by `(file_path, direction)`
   and emit `{file, count, names[], node_ids[], min_depth, max_depth}` sorted
   by count desc. New mode string `"rollup_call_graph"`. Measured on
   `ensure_indexed` (86 nodes): previously 86 flat entries → now 2 caller
   rollups + 5 callee rollups, preserving `node_ids` for `get_ast_node`
   drill-down. Contract Δ: consumers matching on
   `mode == "compressed_call_graph"` must update to `"rollup_call_graph"`.

### Tests

- `strip_outer_generic` unit tests (4/4) cover `Vec<T>`, nested generics,
  multi-param (`Result<T, E>`), and no-bracket cases.
- `acronyms::expand_acronym` unit tests (4/4) cover case-insensitivity,
  unknown tokens, `BM25` numeric acronym, and an FTS-length-filter guardrail.
- 230 lib tests + 44 integration tests all green.

### Internal

New module `src/search/acronyms.rs`. `strip_outer_generic` in
`src/mcp/server/helpers.rs`. All other edits localized to `tool_ast_search`,
`tool_semantic_search`, and `format_call_graph_response` in
`src/mcp/server/tools.rs`, plus one flat_map augmentation in
`storage::queries::fts5_search_impl`.

### Routing-recall benchmark (new)

`tests/routing_bench.rs` — turns "does Claude Code naturally call our tools
for the right intents?" from vibe-check into a P@1 number. 20 oracle queries
(3 per tool for 6 tools + 2 for `find_references`), each sent to the Claude
API with the live 7-tool schemas from `ToolRegistry`; asserts the picked
tool matches the oracle expectation.

- `oracle_well_formed` runs in default `cargo test` and verifies every
  oracle entry references a real tool *and* every registered tool has at
  least one oracle query — catches drift when tools are renamed/added.
- `routing_recall_benchmark` is `#[ignore]` (requires `ANTHROPIC_API_KEY`).
  Run locally: `ANTHROPIC_API_KEY=sk-... cargo test --test routing_bench -- --ignored --nocapture`.
  Cost ≈ $0.10/run with `claude-sonnet-4-6` (20 queries × ~1.2K in + ~150 out).
  Threshold starts at P@1 ≥ 0.70; tighten as descriptions improve.
- New dev-dep `reqwest` (blocking + rustls-tls, no TLS-OpenSSL pulled in).
- CI wiring deliberately not added yet — run manually or add a gated step
  (`env: ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}`) when ready.

## v0.11.3 — Doc: "hidden but callable" clarified (Claude Code vs. raw MCP)

User-facing: no behavior change; corrects a misleading claim in the adopted
plugin memory after a 12-tool UX audit.

### Context

v0.10.0 trimmed `tools/list` to 7 core tools and documented the other 5
(`impact_analysis`, `trace_http_chain`, `dependency_graph`, `find_similar_code`,
`find_dead_code`) as "hidden but callable by name". UX audit found this holds
only for clients that invoke `tools/call` with a literal tool name (raw JSON-RPC,
MCP SDKs, CLI). **Claude Code's MCP integration derives its callable set from
`tools/list`** — `ToolSearch` returns `No matching deferred tools found` for the
hidden 5, and direct invocation errors with `No such tool available`.

### Fixes

1. **`claude-plugin/templates/plugin_code_graph_mcp.md` "进阶 5" table
   reworded**: CLI is now the primary column for Claude Code users; raw MCP
   name calls annotated as SDK/scripts-only. v0.11.0 template auto-refresh
   pushes this to previously-adopted projects on next SessionStart.
2. **`src/mcp/tools.rs` doc comment**: spells out which MCP clients can reach
   hidden tools and points to CLI fallback for Claude Code.

### Why this matters

Misleading docs caused agents to attempt `mcp__…__impact_analysis` /
`mcp__…__trace_http_chain` and hit a terminal "No such tool available" error
instead of routing to `code-graph-mcp impact|trace|deps|similar|dead-code`
via Bash.

## v0.11.2 — Post-audit follow-up: 4 residual precision fixes

Follow-up audit on top of v0.11.1. All additive/tightening — no schema breakage.

### Fixes

1. **`module_overview` no longer leaks inline `#[cfg(test)]` test fns.**
   Name-heuristic `is_test_symbol` couldn't catch `#[cfg(test)] mod tests { #[test] fn anything_goes }`
   whose names don't prefix `test_`. Root fix: `get_module_exports` SQL now
   `WHERE n.is_test = 0` on both the explicit-exports (JS/TS) path and the
   fallback (Rust / Go / Python) path — AST-level flag propagates through.

2. **Disambiguation suggestions carry `node_id` + `start_line`.**
   `resolve_fuzzy_name` and `disambiguate_symbol` suggestions now include
   both fields so callers can pick a specific definition when multiple
   same-name functions live in one file (e.g. two `fn new()` in different
   `impl` blocks of the same module). `disambiguate_symbol` also fires on
   same-file multi-def, not just cross-file collisions.

3. **`find_references` gains `node_id` parameter.** Lets callers pass the
   `node_id` from a suggestion directly, skipping the ambiguous name-lookup
   step. When a name is ambiguous within one file, the tool now returns
   a per-definition suggestion list (with `start_line`) instead of silently
   merging refs across defs.

4. **`find_dead_code` gets `ignore_paths` (MCP) / `--ignore` (CLI).**
   Shell-invoked plugin entry points (lifecycle/hook scripts in
   `claude-plugin/`) are not in the static AST call graph, so they surfaced
   as false-positive orphans. Added prefix-match exclusions with a sensible
   default (`["claude-plugin/"]`). Pass `ignore_paths: []` or
   `--no-ignore` to see the full list. Response carries `ignored_count`,
   `ignore_paths_applied`, `ignore_paths_defaulted` for transparency.

### Docs

- `plugin_code_graph_mcp.md`: hidden-5 tools now have an explicit
  required/optional parameter table (notably `trace_http_chain` takes
  `route_path`, not `route`) — users calling by name no longer need to
  trigger the error message to discover arg names.

### Tests

+4 new (+1 unit in `queries.rs`, +3 integration covering Bug #1 / Issue #3 /
Bug #2). Full suite: **347 passed / 0 failed** default features,
**340 passed / 0 failed** `--no-default-features`; clippy
`-D warnings` clean under both feature configs.

## v0.11.1 — 12-tool accuracy audit: 1 critical bugfix + 5 precision improvements

Post-audit fixes for tool output correctness. All changes are additive/tightening —
no consumer schema breakage.

### Fixes

1. **`find_references` — critical bugfix for exact-name resolution.**
   `resolve_fuzzy_name` was matching substrings before exact names, so
   `find_references("handle_tool")` falsely reported ambiguity with
   `handle_tools_list` / `handle_tools_call`. Now exact-name matches win first;
   same-name-in-multiple-files still produces `Ambiguous` but scoped to exact
   matches only. Same fix benefits `impact_analysis` and `get_call_graph`
   fuzzy-fallback paths.

2. **Centralized truncation keeps arrays homogeneous.** The
   `centralized_compress` pipeline used to splice a string sentinel
   (`"... [N items truncated]"`) into the middle of object arrays, breaking
   type consistency for strict JSON consumers and hiding how much was dropped.
   Arrays now truncate silently to `first-10 + last-5` (15 homogeneous items),
   and a new `_array_truncations: {<field>: {original, kept}}` sibling records
   the true pre-truncation length so callers can reconcile `count`/`total`
   siblings against what was actually returned.

3. **`project_map` schema sharpened.**
   - `hot_functions` SQL tightened to `n.type IN ('function','method')` so
     structs/classes no longer leak into the "hot functions" bucket.
   - `entry_points[].kind` added: `"main"` for program entry points, `"http_route"`
     for framework-registered handlers. Lets LLMs skip `main` when scanning the
     HTTP surface without sniffing the `route` string.

4. **`dependency_graph` filters the `<external>` sentinel.** The synthetic
   bucket for unresolved imports now no longer surfaces as a fake file dependency.

5. **`find_similar_code` reports cutoff-driven shortfalls.** When
   `max_distance` drops candidates below `top_k`, the response now carries
   `cutoff_applied: true`, `cutoff_dropped: N`, and a `hint` suggesting the
   user widen `max_distance`. Also echoes `top_k` and `max_distance` in every
   response for transparency.

6. **`impact_analysis` on types returns `risk_level: "UNKNOWN"`.** When the
   target is a struct/class/enum/interface/type_alias and the call graph finds
   zero callers, the risk level is now `UNKNOWN` instead of `LOW` — so LLMs
   don't mistake "call graph can't see type usage" for "no one uses this".
   The existing type_warning still explains why and points to
   `semantic_code_search` for broader coverage.

### Test coverage

- +2 unit tests in `src/mcp/server/helpers.rs` (truncation homogeneity,
  no-op when arrays < 20).
- +6 integration tests in `tests/integration.rs` covering each fix above.
- Full suite: lib 221 + integration 41 + cli_e2e 50 + parser 19 + plugin 6 +
  hardening 6 = 343 passed, clippy clean.

## v0.11.0 — auto-refresh stale decision table on plugin upgrade

### Migration note

v0.10.0 shipped the 7-core/5-hidden tool surface in the Rust binary **but left the adopted `plugin_code_graph_mcp.md` decision table file — and the `MEMORY.md` sentinel block — stuck at the v0.8.x/v0.9.x 12-tool content** for any project that had already auto-adopted. The plugin's `maybeAutoAdopt()` short-circuited on `isAdopted() == true` and never refreshed the template. Two related holes were also fixed:

1. The shipped source template (`claude-plugin/templates/plugin_code_graph_mcp.md`) was not updated in v0.10.0 — **new** `/plugin install` + first-adopt users were also getting the stale 12-tool table.
2. The `INDEX_LINE` constant in `adopt.js` (which drives the `MEMORY.md` sentinel block) was likewise still the v0.8.x 12-tool line.

### What changes on upgrade

- **Source template synced** to match the 7-core / 5-hidden surface. Fresh `/plugin install` gets the correct decision table on first adopt.
- **`INDEX_LINE` synced** to the v0.10.0 wording.
- **Auto-refresh on drift**: when a project is already adopted but the shipped template hash ≠ the project's copy (or the `MEMORY.md` sentinel block's content ≠ current `INDEX_LINE`), the next plugin SessionStart refreshes both silently. One-time stderr notice: `[code-graph] Refreshed decision table to latest shipped version.`
- Hand-edited decision tables are overwritten by default. To lock: `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` in `~/.claude/settings.json` env.

### Opt-out

- `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` — preserves your local edits of `plugin_code_graph_mcp.md`; also pins `MEMORY.md` sentinel to whatever it was. Does not affect first-adopt (only the refresh path).
- `CODE_GRAPH_NO_AUTO_ADOPT=1` — still gates the first-adopt path as in v0.9.0.
- `code-graph-mcp unadopt` — unchanged; strips sentinel + deletes target file.

### Why this matters

Without this fix, an already-adopted v0.8.x/v0.9.x user who upgrades to v0.10.x gets mixed state: the Rust binary serves 7 tools in `tools/list` but the MEMORY.md index + decision-table file still instruct the LLM to route through the full 12-tool surface as if they were peers. Functionally nothing breaks (hidden tools remain callable by name), but the decision guidance is misaligned. v0.11.0 closes the loop so the three surfaces — binary, index pointer, decision table — all move together on upgrade.

## v0.10.0 — tools/list surface trimmed to 7 core tools

### Migration note

MCP `tools/list` now advertises 7 tools instead of 12. The 5 hidden tools remain fully callable by name (aliases preserved) — only their visibility to the LLM at session start is removed, to shrink tools/list payload (~40% reduction) and cut decision fatigue in daily coding flows.

**Core 7 (exposed in tools/list)**:
`semantic_code_search`, `get_call_graph`, `get_ast_node`, `module_overview`, `project_map`, `find_references`, `ast_search`.

**Hidden but callable by name / CLI (backward-compatible aliases)**:
`impact_analysis`, `trace_http_chain`, `dependency_graph`, `find_similar_code`, `find_dead_code`.

**Rationale**: these 5 are niche (cleanup, duplicate detection, HTTP routing, file-level imports, blast-radius pre-check) — high value when needed, low daily frequency. For the primary blast-radius use case, prefer `get_ast_node symbol_name=X include_impact=true` which is in the core 7.

**Reverse / opt-out**: call any hidden tool by name via MCP `tools/call` or the matching `code-graph-mcp <subcommand>` CLI. All handlers, schemas, and CLI paths unchanged — only the tools/list catalog shrunk.

**Memory sync**: projects that auto-adopted v0.9.x will see updated `plugin_code_graph_mcp.md` decision tables on next session.

## v0.9.1 — Rust 1.95 clippy cleanup

CI-only cleanup; no runtime behavior changes, no user-visible differences. Fixes 9 clippy errors surfaced by Rust 1.95.0's stricter lints (pre-existing since ~v0.8.1, was shipping with red CI):

- `collapsible_match` (4): merge `match arm => if cond` into `match arm if cond =>` in `src/parser/relations.rs` C# arms + Python decorator scan.
- `unnecessary_sort_by` (4): `.sort_by(|a,b| b.x.cmp(&a.x))` → `.sort_by_key(|e| Reverse(e.x))` in `src/mcp/server/tools.rs` and `src/storage/queries.rs`.
- `useless_conversion` (1): drop redundant `.into_iter()` in a chained iterator in `src/graph/query.rs`.

Verified with `cargo +1.95.0 clippy -- -D warnings` on both `--no-default-features` and default feature sets.

## v0.9.0 — Context-aware auto-adopt (C')

### Migration note

Plugin-mode installs (`/plugin install` in Claude Code) now **auto-adopt** into the project's `MEMORY.md` on first `SessionStart`. Previously adoption required running the adopt script manually, which most users never discovered — so the tool-invocation contract never got loaded and MCP tools stayed underused.

**What changes on first upgrade (plugin mode)**:

1. `~/.claude/projects/<slug>/memory/plugin_code_graph_mcp.md` is written (tool-decision rules).
2. A sentinel-bracketed pointer line is appended to `MEMORY.md`.
3. `quietHooks` flips to `true` automatically — per-session `project_map` injection (~60 lines) is skipped; tools are loaded on-demand instead.
4. A single stderr notice fires on the first adoption showing how to opt out or reverse.

**Opt-outs** (in `~/.claude/settings.json` → `env`):

- `CODE_GRAPH_NO_AUTO_ADOPT=1` — prevents future auto-adoption; does not affect already-adopted projects.
- `CODE_GRAPH_QUIET_HOOKS=0` — forces `project_map` injection back on, even if adopted.
- `CODE_GRAPH_QUIET_HOOKS=1` — forces silent mode, even if not adopted.

**Reverse adoption**: `code-graph-mcp unadopt` (now a real CLI subcommand — see below).

**What does NOT auto-adopt**:

- npm global installs (`npm install -g @sdsrs/code-graph`)
- `npx ./tarball.tgz` invocations
- Bare dev checkouts / test fixtures
- CI / agent short-session contexts

Detection uses the script's `__dirname` (checks for `~/.claude/plugins/` prefix), not `CLAUDE_PLUGIN_ROOT` — the env var leaks across concurrent plugins.

### New

- **`code-graph-mcp adopt` / `unadopt` CLI subcommands**: previously only callable via `node claude-plugin/scripts/adopt.js`. Now uniform across plugin / npm / npx installs via `bin/cli.js` interception.
- **`CODE_GRAPH_NO_AUTO_ADOPT=1`**: explicit opt-out env for auto-adopt.

### CLI polish

- **`code-graph-mcp show <file-path>` nudge**: when the positional argument is an existing code file on disk, emit a clear pointer to `overview <file>` instead of silently returning no rows. `show` is for symbols; `overview` is for files.
- **`code-graph-mcp deps` barrel fallback**: files with no tracked dependency edges (Rust `mod.rs`, `index.ts` barrels, Python `__init__.py`) now scan source for language-appropriate re-export / import lines and surface them — previously a hard error.
- **Impact / references filter `<external>` placeholders**: stub nodes synthesized for unresolved external symbols no longer surface in `impact_analysis` / `find_references` results.

### Breaking (semantic default change)

The default meaning of "plugin installed but not adopted" changed from *"inject project_map every session, user must find /adopt to opt into the contract"* to *"adopted implicitly from the install action, quiet by default"*. Hence the minor bump. Users who preferred the v0.8.x noisy default can pin it with `CODE_GRAPH_QUIET_HOOKS=0`.

---

## v0.8.4 — `.code-graph` pollution + test leak cleanup

See [release notes](https://github.com/sdsrs/code-graph-mcp/releases/tag/v0.8.4).

## Older releases

See [GitHub Releases](https://github.com/sdsrs/code-graph-mcp/releases).
