# Code-Graph Invariance Toolkit — Design & Integration Plan

> How code-graph-mcp connects to the 3-layer invariance tower in cli-ops
> and what additional ratchets we can build against the SDK/spec/litellm
> sources that are already indexed.

## 1. What exists today — the verification pipeline

```
cli-ops/refs/api-references/
├── Makefile                 ← `make verify` = THE drift gate
├── 29 gen-*.py scripts      ← AST-walk 4 source families → 28 .md files
├── check-cross-wire-coverage.py  ← L1 coverage advisor
├── gen-harness-invariants-from-tests.py  ← L2 autogen from Vitest titles
├── gen-harness-invariants-response-item.py  ← L2 RI catalog
├── gen-harness-invariants-pi-reference.py   ← L2 Anthropic state machine
├── gen-harness-invariants-gemini-reference.py ← L2 Gemini state machine
└── check-cross-wire-coverage.py  ← L1 advisory gap finder

26 git submodules feed the generators:
├── refs/anthropic/spec/          ← anthropic-sdk-typescript (api.md, types)
├── refs/openai/spec/             ← openai-openapi (81K-line YAML)
├── refs/gemini/spec/googleapis/  ← proto definitions
├── upstream-infrastructure/litellm/  ← AST-walked for transformations
└── upstream-infrastructure/llm-proxy/  ← AST-walked for routes/aliases
```

### What each source family gives us

| Source | `make` target | What it produces | Extraction method |
|---|---|---|---|
| **anthropic-sdk-typescript** (`refs/anthropic/spec/`) | `anthropic/*` | wire-endpoints, wire-events, wire-schemas, models | TS→Python AST walk of `api.md` + source files |
| **openai-openapi** (`refs/openai/spec/`) | `openai/*` | wire-endpoints, wire-events, wire-schemas, models | OpenAPI YAML parse (81K lines) |
| **googleapis protobuf** (`refs/gemini/spec/`) | `gemini/*` | wire-endpoints, wire-events, wire-schemas, models | `.proto` file parse |
| **litellm** (`upstream-infrastructure/litellm/`) | `*/litellm-*.md` | base transform, vertex/azure transform, model lists | Python AST walk of litellm source |
| **llm-proxy** (`upstream-infrastructure/llm-proxy/`) | `llm-proxy/*` | routes, transformations, model-aliases, middleware | Python AST walk (VPN-gated) |
| **apex-ontap tests** (spoke) | `harness-invariants-autogen.md` | 314 Vitest it() titles = enforcement evidence | Regex extraction from *.test.ts |
| **XLI ResponseItem catalog** (spoke) | `harness-invariants-response-item.md` | 36 RI grain invariants | JSON source + generator |
| **Anthropic PI reference** (spoke) | `harness-invariants-pi-reference.md` | 21 state machine branches | SHA-stamped pi/index.ts walk |
| **Gemini reference** (spoke) | `harness-invariants-gemini-reference.md` | 18 state machine rows | SHA-stamped geminiChat.ts walk |

### What `make verify` actually checks

1. Regenerates all 28 .md files from source
2. `git diff --exit-code` — any change = FAIL (source drifted)
3. Runs `--verify` on 4 harness invariant generators (row ID stability)
4. Runs `check-cross-wire-coverage.py` (advisory — doesn't fail)

## 2. What `cgm-invariant-audit` adds (shipped today)

The `make` pipeline checks **document drift** (did the generated markdown change?).
`cgm-invariant-audit` checks **type existence drift** (did the source types
that the documents describe still exist at the expected paths in the actual code?).

Different failure modes:
- `make verify` fails → a spec submodule or litellm advanced
- `cgm-invariant-audit` fails → a spoke refactored internally (type renamed/moved)

Both must pass for the invariance docs to be authoritative.

## 3. Ratchets we CAN build but haven't

### 3a. SDK type ratchet — `make verify-sdk-types`

**Source:** `refs/anthropic/spec/src/resources/messages.ts` (canonical Anthropic types)

**What it would do:** AST-extract every exported type from the Anthropic SDK
(`MessageCreateParams`, `ContentBlock`, `ToolUseBlock`, `ThinkingBlock`, etc.),
then cross-reference against the converter types in apex-ontap's
`AnthropicContentConverter` and XLI's `messages_wire.rs`.

**What it catches:** SDK adds a new field (e.g. `citations_delta` event) that
neither harness handles yet → null-space gap auto-detected before it hits prod.

**Effort:** ~1 day. The `_ast_helpers.py` and `gen_ref.py` infrastructure
already does Python AST walks; TypeScript AST walk via the code-graph index
would be faster (the apex-ontap index already has `AnthropicContentConverter`
at L47-1244 with all its methods).

### 3b. litellm transformation ratchet — `make verify-litellm-surface`

**Source:** `upstream-infrastructure/litellm/litellm/llms/anthropic/chat/transformation.py`

**What it would do:** AST-extract the `map_openai_params()` method and its
per-field conditional branches. For each field it handles, verify that:
1. Our proxy config (`config_seclab*.yaml`) passes the field through
2. Our harness wire (`buildRequest` / `build_messages_request`) sends it
3. Our `settings-invariants.md` documents it

**What it catches:** litellm adds support for a new param (e.g.
`frequency_penalty` on Anthropic) that our proxy passes through but our
settings-invariants.md says is "dropped" → stale doc, silent behavioral change.

**Effort:** ~2 days. `gen-anthropic-litellm-base.py` already walks this file;
extend it to emit a field-level manifest instead of just a summary doc.

### 3c. OpenAPI field ratchet — `make verify-openai-fields`

**Source:** `refs/openai/spec/openapi.yaml` (81K lines)

**What it would do:** Extract every field in `CreateChatCompletionRequest`,
`CreateResponseRequest`, and their response schemas. Cross-reference against
the code-graph index of `ResponsesPipeline.convertGeminiContentsToResponsesInput`
and `build_responses_request` to verify each field is handled.

**What it catches:** OpenAI adds a new response field or request param that
our harness doesn't translate → L1 null-space gap.

**Effort:** ~1 day. `_openapi_helpers.py` already parses the YAML; the
field-level cross-reference is new.

### 3d. Proxy model alias ratchet — `make verify-proxy-models`

**Source:** `upstream-infrastructure/llm-proxy/app/api/config_seclab*.yaml`

**What it would do:** Extract every model alias and verify it appears in:
1. `settings-invariants.md` §3b downgrade matrix (reasoning-capable models)
2. apex-ontap's `system-settings.json` model catalog
3. XLI's `ModelInfo` catalog in `protocol/openai_models.rs`

**What it catches:** Proxy adds a new model that neither harness knows about
→ users can select it in the picker but reasoning/effort/caching behavior
is undefined.

**Effort:** ~4 hours. `gen-llm-proxy-aliases.py` already extracts this; the
cross-reference is the new part.

### 3e. Test-title → type cross-reference — `make verify-test-coverage`

**Source:** `harness-invariants-autogen.md` (314 test titles)

**What it would do:** For each test title that references a type name (e.g.
"buildRequest includes thinking config for opus-4.7"), verify via code-graph
that the referenced type still exists and the test file's test function still
exists.

**What it catches:** Test renamed but autogen not regenerated → stale H-INV
row that references a nonexistent test.

**Effort:** ~4 hours. `gen-harness-invariants-from-tests.py` already has the
test list; code-graph `show` verifies existence.

## 4. Priority ranking for additional ratchets

| Ratchet | Sources needed | Effort | What it catches | Priority |
|---|---|---|---|---|
| **SDK type ratchet (3a)** | anthropic-sdk-ts + code-graph apex/xli | 1 day | New Anthropic fields not handled | 🔴 P0 |
| **litellm transform ratchet (3b)** | litellm source + settings-invariants.md | 2 days | Proxy silently passes new fields | 🔴 P0 |
| **Proxy model alias ratchet (3d)** | proxy config + both harness catalogs | 4 hrs | New models with undefined behavior | 🟡 P1 |
| **OpenAPI field ratchet (3c)** | openai-openapi + code-graph | 1 day | New /responses fields not translated | 🟡 P1 |
| **Test-title cross-ref (3e)** | autogen rows + code-graph | 4 hrs | Stale H-INV rows | 🟠 P2 |

## 5. The full ratchet cascade (target state)

```
make verify                         ← current: doc drift gate
  ├── regenerate 28 .md files from 5 source families
  ├── git diff --exit-code
  ├── 4× --verify harness invariant row-ID stability
  └── check-cross-wire-coverage.py (advisory)

cgm-invariant-audit                 ← shipped: type existence gate
  └── 22 types × 2 spokes verified in code-graph indexes

make verify-sdk-types               ← planned: SDK field coverage gate
  └── every Anthropic SDK export → converter cross-ref

make verify-litellm-surface         ← planned: litellm transform field gate
  └── map_openai_params() fields → proxy + harness + settings-invariants

make verify-proxy-models            ← planned: model alias coverage gate
  └── config_seclab models → both harness catalogs

make verify-openai-fields           ← planned: OpenAPI field coverage gate
  └── CreateResponseRequest fields → wire converter cross-ref

make verify-test-coverage           ← planned: test-title existence gate
  └── H-INV row → code-graph symbol existence
```

When all 6 pass, every layer of the tower is mechanically verified against
its ground-truth sources. No silent drift in any direction.

## 6. Integration with upstream merge workflow

```
git fetch upstream/main (openai/codex)
git merge upstream/main into dev2
  ↓
make -C refs/api-references verify    ← spec/litellm drift?
cgm-invariant-audit --drift           ← type existence drift?
make verify-sdk-types                 ← new SDK fields?
npm run test (apex-ontap)             ← tests still pass?
cargo test (xli)                      ← tests still pass?
  ↓
All green? → merge safe
Any red? → investigate before merging
```
