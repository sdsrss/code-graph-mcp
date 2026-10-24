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
