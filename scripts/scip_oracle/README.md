# SCIP call-edge oracle

Measures how right code-graph's `calls` edges are, per confidence tier, against
the compiler-grade name resolution in a SCIP index: rust-analyzer for Rust,
scip-typescript for JavaScript/TypeScript, scip-python for Python, scip-clang for C++.

The differential guards in `tests/` prove a change *agrees with the previous
version*. This proves it *agrees with the compiler*. Use it for any change to call
resolution: report precision and recall before and after, not an edge total
(a total that does not move can hide a migration between tiers).

## Run

```bash
rustup component add rust-analyzer    # once
scripts/scip_oracle/run.sh            # Rust, ~35 s on this repo
scripts/scip_oracle/run.sh --json-out /tmp/after.json --samples 50
scripts/scip_oracle/run.sh --dump-judged /tmp/judged.json   # every judged edge + verdict

# JS/TS and Python: pinned indexers in a private prefix (not project deps)
npm install --prefix /var/tmp/scip-tools --save-exact \
  @sourcegraph/scip-typescript@0.4.0 @sourcegraph/scip-python@0.6.6
export SCIP_TOOLS_BIN=/var/tmp/scip-tools/node_modules/.bin
scripts/scip_oracle/run.sh --language javascript   # ~4 s
scripts/scip_oracle/run.sh --language python       # ~4 s

# C++: scip-clang is a standalone binary (on PATH or in SCIP_TOOLS_BIN)
curl -sLo /var/tmp/scip-tools/scip-clang \
  https://github.com/sourcegraph/scip-clang/releases/download/v0.4.0/scip-clang-x86_64-linux
chmod +x /var/tmp/scip-tools/scip-clang
COMPDB=build/compile_commands.json scripts/scip_oracle/run.sh --repo DIR --language cpp

# Another repo, indexed by code-graph first (writes DIR/.code-graph, DIR/.gitignore)
scripts/scip_oracle/run.sh --repo DIR --language python

# The pinned external corpora below: clone, index, score (~10 min, mostly embedding)
scripts/scip_oracle/corpora.sh /var/tmp/scip-corpora && rm -rf /var/tmp/scip-corpora
```

For JS and Python, `run.sh` copies the working tree's files of that language
(tracked + untracked, minus `.gitignore`d) into a temp dir, writes a tsconfig
there for JS (the repo has none; `--infer-tsconfig` writes a `{}` tsconfig.json into
the project root and then indexes 0 files), and
scores against that copy. Nothing is written into the repo. The tsconfig uses
`"module": "preserve"`, which resolves both `require()` and extensionless ESM
imports (`from './hono-base'`); `nodenext` refuses the latter and every such call
became external. On this repo the two give identical results.
For C++, scip-clang reads a compilation database and indexes the repo in place.

`run.sh` reads the index and never writes it: a running MCP server keeps it
fresh, and indexing from here would race that server. With no server running,
run `code-graph-mcp incremental-index` first. To score a dev build, index with
that build (`target/release/code-graph-mcp incremental-index`) before running.
A stale index shows up as `unmapped_definitions` and `call_sites_in_unmapped_caller`
(a source file edited after the last index): read those before the precision.

Tests (synthetic SCIP + synthetic index, no rust-analyzer needed):

```bash
python3 -m unittest discover -s scripts/scip_oracle -p 'test_*.py'
```

No third-party Python packages: `scip_decode.py` reads the protobuf wire format
directly, for the handful of fields used here.

## Method

1. **Gold set.** Every SCIP reference to a function/method symbol (descriptor
   ending in `(…).`) is a *call site* when the name is followed by `(` or by a
   turbofish and then `(`. Anything else (`use` items, `.map(foo)`, `let f = foo;`,
   intra-doc links) is a `non_call_reference`. The caller is the innermost SCIP
   function definition whose `enclosing_range` contains the site. A pair
   (caller, callee) is gold when both ends are in-repo definitions.
2. **Alignment.** A SCIP definition maps to the index node with the same file
   and name whose line span contains the definition's name line (innermost wins).
3. **Judging.** Our `calls` edges are deduplicated to one per (source, target)
   pair at the pair's highest tier, then judged only when both endpoints align.
4. **Metrics.**
   - `precision(tier)`: judged edges of that tier that are gold, divided by the judged edges of that tier.
   - `recall(floor)`: gold pairs we have at that tier or better, divided by all gold pairs.

   `callgraph`/`impact` default to the `inferred` floor. `recall_by_call_shape`
   splits recall by the site's syntax: `bare` `f()`, `method` `x.f()`, `path` `A::f()`.

Every step that drops something is counted in `funnel`, so a denominator can
never shrink without it showing up.

### JavaScript and Python

The same method, with what the other two indexers do differently (each rule
has a synthetic test in `test_oracle.py`, and a mutation of each turns it red):

- **Columns are UTF-16.** scip-typescript and scip-python leave
  `position_encoding` unset and count UTF-16 units; they are converted to UTF-8
  bytes on load. Read as bytes they cut 22 of 787 names wrong on this repo's
  non-ASCII JS lines.
- **Callables.** A method descriptor `name().`, or a local / term that aligns to a
  function node (`const f = () => {}` is the term `f.`, a nested function is a
  `local N`). Locals are keyed per document.
- **Callers.** A nested function (a local) has no `enclosing_range` in
  scip-typescript; its node's line span stands in (`enclosing_from_index_span`).
  A call inside no named function (top level, or an anonymous callback there,
  which is most JS test code) belongs to the file's `<module>` node, where the
  index puts it (`call_sites_attributed_to_module`).
- **Export aliases.** `require('./b').f()` references the export property `f0:`;
  in `module.exports = { f }` that property's definition shares a range with a
  reference to `f().`, and the call is resolved through it (`call_sites_via_export_alias`).
- **Unknown bindings are not judged.** A call to a symbol that is not a
  function defined here and not a method descriptor (a parameter, a const bound
  to an expression, a destructured factory result, an export property with no
  alias, a class, an import scip-python could not resolve) has no known callee.
  It is not gold, and our edge from that caller to that name is counted in
  `unjudged_edges_unknown_binding`, not as wrong. Likewise `x.name(` with no SCIP
  occurrence at all, which is a call on an untyped receiver
  (`unjudged_edges_untyped_call`: no `@types/node` in the copy, so Node API
  objects are untyped).
- **Call shape.** `f?.()` is a call. `a.f()` is `member`, not `method`: in JS and
  Python it is as often a module or namespace member.

| funnel key | meaning |
|---|---|
| `call_sites_to_external` | callee is std or a dependency: out of scope |
| `self_calls_excluded` | recursion; the indexer drops self-edges by design (`index_files.rs`) |
| `colliding_definitions` | one SCIP symbol defined in several files (see below) |
| `ambiguous_definitions_in_one_file` | colliding symbol defined twice in the same file (nested fns): dropped |
| `unmapped_definitions` | SCIP definition with no matching node (e.g. `extern` fns) |
| `call_sites_to_unmapped_callee` / `call_sites_in_unmapped_caller` / `call_sites_without_enclosing_fn` | a call site lost on one end (the last: calls in `const`/`static` initialisers) |
| `unjudged_edges_non_function_endpoint` | our edge ends at a struct node (tuple-struct construction); SCIP has no callable there |
| `unjudged_edges_unmapped_function` | our edge touches a function SCIP never defined: mostly cfg-inactive code (below) |
| `wrong_<tier>_wrong_target` | the caller does call something by that name, and the edge picked the wrong definition |
| `wrong_<tier>_no_call_by_that_name` | the caller calls nothing by that name |
| `call_sites_attributed_to_module` | JS/Python: a call in no named function, credited to the file's `<module>` node |
| `call_sites_to_non_function_symbol` / `call_sites_to_unresolved_import` | JS/Python: a call whose callee SCIP does not know (see above) |
| `call_sites_via_export_alias` | JS: a call through `module.exports = { f }`, resolved to `f` |
| `enclosing_from_index_span` | JS: a nested function's span taken from its node |
| `unjudged_edges_unknown_binding` / `unjudged_edges_untyped_call` | JS/Python/C++: our edge to a name the caller calls through an unknown binding or an untyped receiver |
| `call_sites_in_ambiguous_definition` | a call inside one of several same-file definitions of one symbol (overloads): no caller |
| `call_sites_to_declaration_only_method` | a call to a pure virtual / abstract method: no callee, but its overrides are credited |
| `edges_credited_via_override` | our edge to an override of the method the call names (virtual dispatch): correct |
| `edges_credited_to_constructor_class` | our edge to the class of the constructor the call runs: correct |

### Rules for every language: dispatch, constructors, overloads

Found on the external corpora below, each with a synthetic test and a mutation
that turns it red:

- **Virtual dispatch.** `processor.getPath()` names the base method; the index
  may bind the call to the overrides instead. SCIP records an override as an
  `is_implementation` relationship; an edge to any (transitive) override of the
  called method counts as correct, and as found for recall. A pure virtual or
  abstract method has no definition, so a call to it is not gold, but edges to
  its overrides are still credited (`call_sites_to_declaration_only_method`).
- **Constructors.** `new Box()` references the constructor (TS `<constructor>`,
  C++ `Box#Box(…)`); the index resolves it to the class node. An edge to the
  constructor's class counts as the constructor. Recall splits these out as
  the `constructor` call shape.
- **Overloads.** `@overload` stubs and TS overload signatures share one symbol
  with the implementation. A call in any of their bodies has no known caller and
  is dropped (`call_sites_in_ambiguous_definition`), not credited to the
  enclosing scope.
- **Duplicate occurrences** (same range, symbol and role) are counted once.

### C++ (scip-clang)

- **Columns are UTF-8 bytes** although `position_encoding` is unset (measured:
  `callee` after `"é😀"` is at column 48, the byte offset, not 45).
- **Declarations are references.** A header's `Status Get(...);` is a role-0
  occurrence shaped like a call. Outside every function body a call-shaped
  reference is a declaration or an initializer: `call_sites_without_enclosing_fn`.
- **No enclosing ranges.** Every caller's span is its node's
  (`enclosing_from_index_span`), and no call is credited to `<module>`.
- **Names.** scip-clang's name range stops at `~` and at `operator`, and a gtest
  body's range is the `TEST_F` macro; they align to the index's `~Impl`,
  `operator==` and `Suite.Case`.
- **Type arguments.** `helper<int>(1)` is a call (so is TS `f<T>()`).
- **One worker.** `run.sh` passes `--jobs=1`: with parallel workers a header is
  indexed by whichever translation unit reaches it first, and leveldb's gold
  moved between 3345 and 3394 pairs over identical input.
- A `.h` file is indexed as `c`; `--language cpp` scores both.

### Blind spots of the oracle

- **One cfg per run.** rust-analyzer analyses the default features on the host
  platform. Code behind `#[cfg(windows)]`, `#[cfg(not(unix))]` or
  `feature = "embed-model"` has no occurrences, so edges touching it are
  unjudged, not scored.
- **Colliding test-crate symbols.** Each `tests/*.rs` file is its own crate, but
  rust-analyzer gives all of them the same package prefix. When a helper such as
  `setup_indexed_project` is defined in two test files, a reference binds to the
  definition in its own file, because a crate can only call its own copy.
- **Call shape is lexical.** A name followed by `(` on the *next* line is not
  counted. Macro-generated calls count only where rust-analyzer maps them back to
  a source token.
- **JS/Python: unknown bindings shrink the judged set.** 58 JS and 29 Python
  edges are unjudged because the caller calls that name through a parameter,
  a factory result or an unresolved import. Some are right (a destructured
  `makeCooldown()` result bound to the closure the edge points at), some wrong
  (a parameter `install = npmInstallGlobal` bound to `lifecycle.install`); the
  oracle cannot tell which.
- **C++: one configuration per run**, like Rust's cfg: code behind an `#if` the
  compilation database turns off has no occurrences. A template's dependent call
  has none either: `t.f()` on a `T` is `unjudged_edges_untyped_call`, but the
  untyped-receiver check reads only `.`, so `p->f()` on a dependent `p` is judged.
- **Python: the import heuristic is one line.** A local counts as an unresolved
  import when one of its occurrences is on a line starting with `import`/`from`;
  a name inside a parenthesised multi-line import is not recognised.

## Baseline: v0.157.0 (`e14008a`, 2026-09-26)

`results/baseline-2026-09-26-v0.157.0.json`. There are 6,792 gold pairs out of
47,707 call sites (37,777 of those sites call std or a dependency).

| tier | precision | recall at this floor |
|---|---|---|
| extracted | 3404/3454 = 98.6% | 3404/6792 = 50.1% |
| inferred | 3105/3142 = 98.8% | 6509/6792 = 95.8% (default floor) |
| ambiguous | 5/13 = 38.5% | 6514/6792 = 95.9% |

Recall by call shape: bare 4325/4331 (99.9%), method 967/1082 (89.4%), path
1222/1379 (88.6%).

The measured errors, which are the work items for call-resolution changes:

- **Missed path calls (157).** `Type::f()` 102, `crate::…::f()` 42,
  `module::f()` 8, `super::…::f()` 5. For example,
  `crate::domain::type_filter_note(input)` produces no edge.
- **Missed method calls (115).** The receiver is not typed, and the method name
  is not unique enough to bind (`conn` 14, `flush` 9, `is_empty` 8, …).
- **Wrong target: std name shadowed by a project name.** `drop(x)` calls
  `std::mem::drop` but binds to a project `impl Drop` (27 extracted edges).
  `.as_str()` on a `String` binds to a project enum's `as_str` (10 extracted).
- **Wrong target: an explicit import ignored.** `src/graph/routes.rs` imports
  `crate::storage::queries::helpers::test_db`, but its `test_db()` calls bind
  to another file's `test_db` (9 inferred).
- **Same-name twins share edges.** Two definitions with one qualified name in one
  file (cfg twins such as `try_acquire_index_lock` for unix and non-unix, or the
  feature-off `EmbeddingModel::embed` stub) each receive the other's callees,
  because the source end of a call is resolved by name
  (`index_files.rs::resolve_batch_relations`). This repo has 11 such groups.

## Baseline: JavaScript and Python, v0.157.0 (2026-09-26)

`results/baseline-2026-09-26-v0.157.0-javascript.json` and `-python.json`. The index
was snapshotted at `8505631`, with SCIP built from a copy of the same tree and no file
changed between the two, so `unmapped_definitions` is 1 (JS) and 0 (Python).
scip-typescript 0.4.0, scip-python 0.6.6.

| tier | JS precision | JS recall at this floor | Python precision | Python recall at this floor |
|---|---|---|---|---|
| extracted | 789/796 = 99.1% | 789/1289 = 61.2% | 159/159 = 100.0% | 159/197 = 80.7% |
| inferred | 495/516 = 95.9% | 1284/1289 = 99.6% (default floor) | 38/43 = 88.4% | 197/197 = 100.0% |
| ambiguous | 1/134 = 0.7% | 1285/1289 = 99.7% | 0/0 | 197/197 = 100.0% |

Gold pairs: 1289 JS (4196 of the 6405 JS call sites sit in no named function and
are credited to `<module>`), 197 Python.

The measured errors:

- **A std method binds to a nested project arrow function (JS, 156 edges).**
  `words.push(x)` in 7 functions of `pre-grep-guide.js` binds, at the
  **extracted** tier, to `const push = () => …` nested inside another function
  (`pre-grep-guide.js:943`). `set.has(x)` binds to `const has = (name) => …`
  (`adopt.js:267`, 21 inferred), and `JSON.parse(…)` to `const parse = (v) => …`
  (`version-utils.js:45`, 128 of the 133 wrong ambiguous edges). A nested local
  helper is not visible outside its function, and a member call is not a call
  of a bare name.
- **Python std names shadowed (5 inferred).** `re.search(…)` binds to a
  project `search` method (4), `.encode()` to `Backend.encode` (1).
- **Missed: a renamed destructuring (JS, 4).**
  `const { clearCache: clearBinaryCache } = require('./find-binary')`: calls to
  `clearBinaryCache()` produce no edge to `clearCache` (`doctor.js` ×3,
  `auto-update.js` ×1).
- Everything else found: JS recall at the default floor is 1284/1289; Python
  197/197.

## Baseline: external corpora, v0.157.0 and after the v73 fixes (2026-09-26)

`corpora.sh` pins hono v4.6.14 (TypeScript), express 4.21.2 (JavaScript),
flask 3.1.0 (Python) and leveldb 1.23 (C++; googletest/benchmark are compiled
against but excluded from the index). Both arms were indexed the same way and
scored with the same oracle. Precision / recall at the `inferred` floor:

| corpus | v0.157.0 | after |
|---|---|---|
| hono | P 471/571, R 780/896 | P 471/503, R 780/896 |
| express | P 5/9, R 63/68 | P 5/5, R 63/68 |
| flask | P 69/237, R 233/273 | P 69/101, R 233/273 |
| leveldb | P 641/682, R 1358/3356 | P 792/838, R 2094/3394 |

leveldb extracted: 822/966 → 1469/1617. The five errors behind the change
are in CHANGELOG.md (INDEX_VERSION 73). What is left, by category:

- **Member call on another object bound to a same-name method** (no receiver
  types): leveldb 107 of 405 same-file member-call edges; hono `c.text()` /
  `req.arrayBuffer()`, flask `ctx.pop()` / `dict.get()`.
- **A call through an external module bound to a project function**: flask
  `click.echo()` (16 edges) reaches flask's own `echo`, because `click` is an
  import binding and module calls are left unrestricted.
- **Calls a SCIP indexer cannot name**: parameters, factory results, dependency
  objects with no type information (`unjudged_edges_unknown_binding` /
  `unjudged_edges_untyped_call`) — neither right nor wrong here.
- **express gold is small (68 pairs)**: its methods are assigned
  (`res.send = function send()`), which scip-typescript records as properties,
  not callables.
