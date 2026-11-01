# pre-edit pick corpus (spec: tasks/specs/pre-edit-pick-by-reader.md)

The corpus is every Edit/MultiEdit hunk in this machine's Claude Code
transcripts. It holds users' code, so it stays out of the repository: build it
into a scratch directory.

    python3 -I edit_corpus.py <scratch>/edits.jsonl     # all hunks, one JSON line each
    python3 -I delivered.py <scratch>/delivered.json    # toolUseID -> delivered [code-graph:impact] symbol
    node edit_diff.js <scratch>/edits.jsonl <scratch>/samples.json   # current rule vs the Stop check's reader
    node adis.js <scratch>/samples.json pick_not_a_definition         # list one disagreement class
    node delivered_cur.js <scratch>/delivered.json <scratch>/edits.jsonl  # delivered summaries the current rule reproduces
    node sim_new.js <scratch>/edits.jsonl               # current vs proposed pick, every changed decision listed
    node show_added.js <scratch>/edits.jsonl <name>...  # reader signatures before/after for named definitions

`miss_dump.py` / `miss_detect.js` replay the 26 cross-file signature edits of
`tasks/usage-audit-2026-10-23/sig_edits.py` through the current pick.
The scripts read `claude-plugin/scripts/pre-edit-guide.js` and
`stop-impact.js` from /home/ai/dev/code-graph-mcp; `sim_new.js` and
`edit_diff.js` copy the current rule out of pre-edit-guide.js, so run them
against the BASE tree when comparing after the change.

`hook_diff.js` runs the real pre-edit-guide.js of two script trees on every
hunk with a stub binary and classifies each hunk by the name each asks about
(same / added / removed / switched; where a name is placed is not compared);
`hunk_lines.js` prints the pick's line for rows of it.

    node hook_diff.js <scratch>/edits.jsonl <treeA>/claude-plugin/scripts <treeB>/claude-plugin/scripts <scratch>/work
    node hunk_lines.js <scratch>/edits.jsonl <treeA>/claude-plugin/scripts/pre-edit-guide.js <name>@<id:k>...

`reader_diff.js` compares the reader (`definitionHeaders`, `allSignatures`,
and `extractSignatures` per name found) of two script trees on every source
file of the given extensions under the given roots (0.179.1: Go and Kotlin):

    node reader_diff.js <treeA>/claude-plugin/scripts <treeB>/claude-plugin/scripts .go <root>...

## Field check on real sessions (D#574, 2026-11-01)

Transcripts do not record the plugin version a hook ran (`hook_success.command`
keeps `${CLAUDE_PLUGIN_ROOT}` unexpanded). Assign it per process: a Claude Code
process keeps the version installed when it started until `/reload-plugins`;
a session that opens with `SessionStart:clear` continues the latest earlier
session's process in the same cwd (`startup` / `resume` start a new one); the
installed version at time t is the newest
`~/.claude/plugins/cache/code-graph-mcp/code-graph-mcp/<v>` directory born
(`stat -c %w`) at or before t. Live processes list themselves in each version
dir's `.in_use`, which confirmed the rule.

Result on 0.179.0–0.179.4 (466 hunks out of 3,784): `hook_diff.js` v0.178.0 →
HEAD gave 446 same-none, 19 same-summary, 1 added (a destructured parameter,
spec S3); 0 non-definitions named by either rule; 11 of 11 delivered summaries
reproduced. Too small to judge the fix: the in-sample error shapes (Rust call
sites, strings and comments; JS comment lines) did not occur. Transcripts age
out (684 left on 2026-11-01, against 1,057 on 2026-10-23), so the original
corpus cannot be rebuilt.
