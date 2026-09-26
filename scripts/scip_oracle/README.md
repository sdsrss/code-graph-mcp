# SCIP call-edge oracle

Measures how right code-graph's Rust `calls` edges are, per confidence tier,
against the compiler-grade name resolution in rust-analyzer's SCIP index.

The differential guards in `tests/` prove a change *agrees with the previous
version*. This proves it *agrees with the compiler*. Use it for any change to call
resolution: report precision and recall before and after, not an edge total
(a total that does not move can hide a migration between tiers).

## Run

```bash
rustup component add rust-analyzer    # once
scripts/scip_oracle/run.sh            # ~35 s on this repo
scripts/scip_oracle/run.sh --json-out /tmp/after.json --samples 50
```

`run.sh` reads the index and never writes it: a running MCP server keeps it
fresh, and indexing from here would race that server. With no server running,
run `code-graph-mcp incremental-index` first. To score a dev build, index with
that build (`target/release/code-graph-mcp incremental-index`) before running.

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
