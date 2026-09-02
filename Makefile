# One-command entry points for the checks CI runs (.github/workflows/ci.yml)
# and the audit baseline (docs/audit/). Every target is a thin wrapper — the
# real commands are visible in the recipe so nothing here can drift from CI
# without the diff showing it.
#
#   make check        cargo check, both feature legs
#   make lint         clippy -D warnings (both legs) + JS syntax gate
#   make fmt          rustfmt in place
#   make fmt-check    rustfmt --check (what CI runs)
#   make test         Rust (no-embed) + JS test suites
#   make test-embed   Rust with the embed-model feature (what release.yml ships)
#   make test-js      JS plugin/script tests, same discovery rule as CI
#   make test-js-e2e  install-e2e (needs target/release binary; pre-commit-only in CI)
#   make metrics      static metrics + lint + tests as one Markdown report
#   make metrics-full same, plus coverage (instrumented rebuild; slow)
#   make ci           local approximation of ci.yml: fmt-check + lint (both legs) +
#                     test (no-embed) + test-js. NOT run here: the embed test leg
#                     (`make test-embed`, ~8 min), the criterion bench smoke, and
#                     `cargo audit` — CI still owns those.

.PHONY: check lint lint-js fmt fmt-check test test-rs test-embed test-js test-js-e2e \
        metrics metrics-static metrics-full coverage ci

# Incremental artifacts across feature sets balloon target/ (memory:
# feedback_disk_fill_embed_builds). CI runs with it off too.
export CARGO_INCREMENTAL = 0

# Same discovery rule as ci.yml: every *.test.js under the two script trees
# minus install-e2e (needs a built release binary). `find`, not a glob —
# a glob is non-recursive and would skip a test in a subdirectory silently.
JS_TESTS = $(shell find claude-plugin/scripts scripts -type f -name '*.test.js' \
             | grep -vE '(^|/)install-e2e\.test\.js$$' | sort)

check:
	cargo check --no-default-features --all-targets
	cargo check --features embed-model --all-targets

lint: lint-js
	cargo clippy --no-default-features --all-targets -- -D warnings
	cargo clippy --features embed-model --all-targets -- -D warnings

# No JS linter is configured in this repo (see docs/audit); `node --check`
# is the syntax gate. Fails on the first file that does not parse.
lint-js:
	@git ls-files '*.js' | xargs -n1 node --check && echo "node --check: all tracked .js parse"

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

test: test-rs test-js

test-rs:
	cargo test --no-default-features

# CODE_GRAPH_DISABLE_MODEL_DOWNLOAD: the spawned `serve` tests would otherwise
# background-fetch the ~90 MB model (same env as ci.yml embed-check).
test-embed:
	CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1 cargo test --features embed-model

# --test-concurrency=1: cg-answer + find-binary share on-disk resolution cache
# state and flake under per-file parallelism (ci.yml has the same note).
test-js:
	node --test --test-concurrency=1 $(JS_TESTS)

test-js-e2e:
	node --test scripts/install-e2e.test.js

metrics-static:
	python3 scripts/audit-metrics.py

metrics:
	scripts/audit-metrics.sh

metrics-full:
	scripts/audit-metrics.sh --coverage --embed

# Rust line coverage on its own (tarpaulin lives in target/tools so the
# sandboxed dev box can run it: `cargo install cargo-tarpaulin --locked --root target/tools`).
coverage:
	target/tools/bin/cargo-tarpaulin tarpaulin --engine llvm --no-default-features \
	  --skip-clean --target-dir target/tarpaulin --timeout 600

ci: fmt-check lint test
