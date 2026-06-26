---
name: invariance-tower
description: "3-layer invariance tower: type audit (L1-L3) + 4 ratchets against SDK/litellm/proxy/tests"
---

# Invariance Tower — Verification Toolkit

## What this verifies

The invariance tower ensures that the 3 layers of cross-wire documentation
in `cli-ops/refs/api-references/` remain mechanically grounded in the actual
source code across both harness spokes (apex-ontap + xli).

```
L3  settings-invariants.md     S ──π_wire──→ S_wire ──π_model──→ S_effective(m)
L2  harness-invariants.md      IR^(n) ──τ──→ IR^(n+1)
L1  cross-wire.md              IR ──ε──→ Wire ──δ──→ IR
```

## Gate 1: Type Audit (`invariance_check action=audit`)

**Script:** `~/Projects/code-graph-mcp/scripts/cgm-invariant-audit`

Verifies 22 invariant-critical types exist at their expected file paths
across both spokes using the code-graph-mcp `show` command.

### Three layers × two spokes = 22 checkpoints

| Layer | Apex types | XLI types |
|---|---|---|
| **L3 settings** | WireCapabilities, resolveSamplingParams, ResolvedSamplingParams, buildSamplingParameters, buildRequest, resolveEffort, ModelConfig, ModelCapabilities | ConfiguredModelProvider, AnthropicMessagesProvider, GeminiModelProvider |
| **L1 wire** | AnthropicContentConverter, convertGeminiContentsToResponsesInput, convertResponsesEventToGemini, ResponsesPipeline | build_messages_request, build_responses_request |
| **L2 harness** | AnthropicContentGenerator, OpenAIResponsesContentGenerator | AnthropicMessagesProvider, build_messages_request, build_responses_request, ConfiguredModelProvider |

### Interpreting results

- `✅` = type found at expected file:line
- `❌ MISSING` = type not found → it was renamed or deleted; update the manifest
- `⚠️ MOVED` = type found but at a different file → file was moved; update the manifest

### Flags

```bash
cgm-invariant-audit                    # all 3 layers, both spokes
cgm-invariant-audit --layer settings   # L3 only
cgm-invariant-audit --layer wire       # L1 only
cgm-invariant-audit --drift            # compare against previous snapshot
cgm-invariant-audit --json             # machine-readable output
```

### Snapshots

Saved to `~/Projects/cli-ops/refs/api-references/.audit-snapshots/<timestamp>.txt`.
`--drift` compares against the most recent snapshot.

### When to run

- After every upstream merge (openai/codex, google-gemini/gemini-cli)
- After any sortie that touches wire converter files
- After refactors that rename or move types

## Gate 2–5: Ratchets (`invariance_check action=ratchet`)

**Location:** `~/Projects/cli-ops/refs/api-references/`
**Generate:** `make ratchets` (or `make -C ~/Projects/cli-ops/refs/api-references ratchets`)

### Ratchet 1: SDK Type Coverage (`name=sdk-types`)

**Script:** `scripts/gen-sdk-type-ratchet.py`
**Source:** `refs/anthropic/spec/src/` (anthropic-sdk-typescript submodule)
**Cross-ref:** `cross-wire.md` concept column

Extracts every exported TypeScript type/interface from the Anthropic SDK
and checks which appear in cross-wire.md. Growth in gap count = new Anthropic
feature that neither harness maps yet.

### Ratchet 2: litellm Transform Coverage (`name=litellm-params`)

**Script:** `scripts/gen-litellm-transform-ratchet.py`
**Source:** `upstream-infrastructure/litellm/litellm/llms/anthropic/chat/transformation.py`
**Cross-ref:** `settings-invariants.md` §3 manifest

AST-walks litellm's `map_openai_params()` method — extracts the 18 params
it handles for Anthropic. Cross-references against settings-invariants.md.
Gaps = params litellm passes through that our settings doc doesn't cover.
Asymmetric = params we doc as "dropped" that litellm correctly doesn't handle.

### Ratchet 3: Proxy Model Coverage (`name=proxy-models`)

**Script:** `scripts/gen-proxy-model-ratchet.py`
**Source:** `upstream-infrastructure/llm-proxy/app/api/config_seclab*.yaml`
**Cross-ref:** `~/Projects/apex-ontap/canonical/live/system-settings.json`

Every model alias the proxy serves vs what the apex catalog knows. Gaps =
models users can route to but apex doesn't know their capabilities
(reasoning, effort, caching behavior undefined).

### Ratchet 4: Test Title Coverage (`name=test-titles`)

**Script:** `scripts/gen-test-title-ratchet.py`
**Source:** `harness-invariants-autogen.md` (388 H-INV rows)
**Cross-ref:** actual test files on disk

Verifies every H-INV row's source test file still exists and still contains
the test title string. MISSING = test file deleted (invariant chain broken).
DRIFTED = test title changed (H-INV row stale).

## Gate 6: `make verify` (existing pipeline)

**Location:** `~/Projects/cli-ops/refs/api-references/Makefile`

Regenerates all 28 .md files from 26 git submodules (3 SDK specs, litellm AST,
llm-proxy AST, harness test extraction) and `git diff --exit-code`. Any change
= spec submodule or generator drifted.

```bash
make -C ~/Projects/cli-ops/refs/api-references verify     # strict
make -C ~/Projects/cli-ops/refs/api-references verify-xli  # XLI-only gate
make -C ~/Projects/cli-ops/refs/api-references coverage    # advisory gap finder
```

## Full verification cascade

```bash
# Quick health check (reads existing reports, no regeneration)
invariance_check(action="status")

# Deep verification (runs scripts, ~30s)
invariance_check(action="audit")          # 22 types verified
invariance_check(action="ratchet")        # 4 ratchets, gaps only

# After upstream merge (full pipeline, ~2min)
make -C ~/Projects/cli-ops/refs/api-references verify
cgm-invariant-audit --drift
make -C ~/Projects/cli-ops/refs/api-references ratchets
```

## Key file paths

| What | Path |
|---|---|
| Type audit script | `~/Projects/code-graph-mcp/scripts/cgm-invariant-audit` |
| Init script | `~/Projects/code-graph-mcp/scripts/cgm-init` |
| Ratchet scripts | `~/Projects/cli-ops/refs/api-references/scripts/gen-*-ratchet.py` |
| SDK type ratchet | `~/Projects/cli-ops/refs/api-references/scripts/gen-sdk-type-ratchet.py` |
| Make pipeline | `~/Projects/cli-ops/refs/api-references/Makefile` |
| Audit snapshots | `~/Projects/cli-ops/refs/api-references/.audit-snapshots/` |
| Ratchet reports | `~/Projects/cli-ops/refs/api-references/{sdk-type-coverage,litellm-transform-ratchet,proxy-model-coverage,test-title-coverage}.md` |
| settings-invariants.md | `~/Projects/cli-ops/refs/api-references/settings-invariants.md` |
| cross-wire.md | `~/Projects/cli-ops/refs/api-references/cross-wire.md` |
| harness-invariants.md | `~/Projects/cli-ops/refs/api-references/harness-invariants-curated.md` |
| Design doc | `~/Projects/code-graph-mcp/docs/INVARIANCE-TOOLKIT-DESIGN.md` |
