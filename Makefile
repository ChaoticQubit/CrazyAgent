.PHONY: help fmt fmt-check lint test test-rust-selected test-node test-bun build check

NODE_TESTS := $(filter-out $(NODE_TESTS_EXCLUDE),$(shell find apps crates scripts \( -name node_modules -o -name target -o -name dist \) -prune -o \( -name '*.test.mjs' -o -name '*.test.cjs' \) -type f -print | LC_ALL=C sort))
BUN_TESTS := $(filter-out $(BUN_TESTS_EXCLUDE),$(shell find scripts -name '*.test.ts' -type f -print | LC_ALL=C sort))
ENGINE_TESTS := $(filter-out $(ENGINE_TESTS_EXCLUDE),$(basename $(notdir $(wildcard crates/crazyagent/tests/*.rs))))
DESKTOP_TESTS := $(filter-out $(DESKTOP_TESTS_EXCLUDE),$(basename $(notdir $(wildcard apps/desktop/src-tauri/tests/*.rs))))
BUN ?= $(shell command -v bun)
RCLI_SDK ?= $(CURDIR)/.local-state/tools/rcli-source

help:
	@printf '%s\n' 'make check      Format check, lint, test, build, test again' 'make test       Run the real Rust, Node and Bun test suites' 'make test-rust-selected  Run engine and desktop test targets minus ENGINE_TESTS_EXCLUDE and DESKTOP_TESTS_EXCLUDE' 'make test-node  Run every first-party node:test file' 'make test-bun   Run the rcli host bun:test files' 'make build      Build the Rust workspace offline' 'make fmt        Format Rust source' 'make lint       Check Rust warnings'

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --locked --offline -- -D warnings

test:
	cargo build -p crazyagent --bin crazyagent --locked --offline
	CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$${CARGO_TARGET_DIR:-$(CURDIR)/target}/debug/crazyagent" cargo test --workspace --locked --offline
	$(MAKE) test-node
	$(MAKE) test-bun

test-rust-selected:
	cargo build -p crazyagent --bin crazyagent --locked --offline
	CRAZYAGENT_TEST_NATIVE_AUTH_HOST="$${CARGO_TARGET_DIR:-$(CURDIR)/target}/debug/crazyagent" cargo test -p crazyagent --locked --offline --bins $(foreach test,$(ENGINE_TESTS),--test $(test))
	cargo test -p crazyagent-desktop --locked --offline --lib --bins $(foreach test,$(DESKTOP_TESTS),--test $(test))

test-node:
	CRAZYAGENT_BUN="$(BUN)" CRAZYAGENT_RCLI_SDK="$(RCLI_SDK)" node --test --test-timeout=15000 $(NODE_TESTS)

test-bun:
	@test -n "$(BUN)" || { echo 'test-bun: unavailable: bun is not on PATH; set BUN=/path/to/bun' >&2; exit 2; }
	@test -d "$(CURDIR)/.local-state/tools/rcli-source/node_modules" || { echo 'test-bun: unavailable: .local-state/tools/rcli-source is not the pinned Backboard-R-CLI checkout with installed node_modules' >&2; exit 2; }
	"$(BUN)" test $(BUN_TESTS)

build:
	cargo build --workspace --locked --offline

check:
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) build
	$(MAKE) test
