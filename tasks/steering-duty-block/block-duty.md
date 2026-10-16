<!-- code-graph-mcp:begin v2 -->
## Code Graph (repo-wide AST index)

Parsed index of the whole repo. Use it at these moments:

- **Before you change a function's signature, or rename or delete it**: run
  `code-graph-mcp impact X` and handle every caller it lists; for a rename or
  removal also `code-graph-mcp refs X`, then `grep -w X`.
- **After you edit source files, before you run tests**: run
  `code-graph-mcp affected <files>` and run the tests it names.
- **When you need who calls X, or what X calls**: `code-graph-mcp callgraph X`,
  one call instead of rounds of grep.
- **Before reading file after file in an unfamiliar directory**:
  `code-graph-mcp overview <dir>`.

Also: `code-graph-mcp show X` prints a symbol's source and signature;
`code-graph-mcp search "…"` finds code by concept; `code-graph-mcp grep "pat"
[paths]` is grep with each hit's enclosing function.

Unresolved calls (dynamic dispatch, reflection, unresolved imports) leave no
edge, so an empty answer is not proof: confirm with grep before deleting or
renaming.
<!-- code-graph-mcp:end -->
