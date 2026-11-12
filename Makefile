.PHONY: help fmt fmt-check lint test test-rust-selected engine-bins test-node test-bun build check desktop-dist test-price-script

NODE_TESTS := $(filter-out $(NODE_TESTS_EXCLUDE),$(shell find apps crates scripts \( -name node_modules -o -name target -o -name dist \) -prune -o \( -name '*.test.mjs' -o -name '*.test.cjs' \) -type f -print | LC_ALL=C sort))
BUN_TESTS := $(filter-out $(BUN_TESTS_EXCLUDE),$(shell find scripts -name '*.test.ts' -type f -print | LC_ALL=C sort))
ENGINE_TESTS := $(filter-out $(ENGINE_TESTS_EXCLUDE),$(basename $(notdir $(wildcard crates/crazyagent/tests/*.rs))))
ROWS = $(foreach test,$(1),$(if $(findstring ::,$(test)),$(test),$(if $(findstring /,$(test)),$(notdir $(test)))))
ENGINE_ROWS_EXCLUDE := $(call ROWS,$(ENGINE_TESTS_EXCLUDE))
DESKTOP_TESTS := $(filter-out $(DESKTOP_TESTS_EXCLUDE),$(basename $(notdir $(wildcard apps/desktop/src-tauri/tests/*.rs))))
DESKTOP_ROWS_EXCLUDE := $(call ROWS,$(DESKTOP_TESTS_EXCLUDE))
BUN ?= $(shell command -v bun)
RCLI_SDK ?= $(CURDIR)/.local-state/tools/rcli-source

DEBUG_DIR := $(or $(CARGO_TARGET_DIR),$(CURDIR)/target)/debug

test test-rust-selected: export CRAZYAGENT_TEST_NATIVE_AUTH_HOST = $(DEBUG_DIR)/crazyagent

help:
	@printf '%s\n' 'make check      Format check, lint, test, build, test again' 'make test       Run the real Rust, Node and Bun test suites' 'make test-rust-selected  Run the engine and desktop unit tests and test targets minus ENGINE_TESTS_EXCLUDE and DESKTOP_TESTS_EXCLUDE (targets, module::rows and target/rows)' 'make test-node  Run every first-party node:test file' 'make test-bun   Run the rcli host bun:test files' 'make build      Build the Rust workspace offline' 'make desktop-dist  Build the desktop frontend the desktop crate embeds' 'make fmt        Format Rust source' 'make lint       Check Rust warnings'

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint: desktop-dist
	cargo clippy --workspace --all-targets --locked --offline -- -D warnings

test: desktop-dist engine-bins
	cargo test --workspace --locked --offline
	$(MAKE) test-node
	$(MAKE) test-bun

engine-bins:
	cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked --offline
	mkdir -p $(DEBUG_DIR)/deps
	install -m 0755 $(DEBUG_DIR)/crazyagent-landlock $(DEBUG_DIR)/deps/crazyagent-landlock

test-rust-selected: desktop-dist engine-bins
	cargo build -p crazyagent-desktop --bin crazyagent-desktop --locked --offline
	status=0; \
	cargo test -p crazyagent --locked --offline --no-fail-fast --lib --bins $(foreach test,$(ENGINE_TESTS),--test $(test))$(if $(ENGINE_ROWS_EXCLUDE), -- --exact $(foreach row,$(ENGINE_ROWS_EXCLUDE),--skip $(row))) || status=$$?; \
	cargo test -p crazyagent-desktop --locked --offline --no-fail-fast --lib --bins $(foreach test,$(DESKTOP_TESTS),--test $(test))$(if $(DESKTOP_ROWS_EXCLUDE), -- --exact $(foreach row,$(DESKTOP_ROWS_EXCLUDE),--skip $(row))) || status=$$?; \
	exit $$status

test-price-script:
	python3 -B -I scripts/usage/test_refresh_prices.py

test-node:
	CRAZYAGENT_BUN="$(BUN)" CRAZYAGENT_RCLI_SDK="$(RCLI_SDK)" node --test --test-timeout=15000 $(NODE_TESTS)

test-bun:
	@test -n "$(BUN)" || { echo 'test-bun: unavailable: bun is not on PATH; set BUN=/path/to/bun' >&2; exit 2; }
	@test -d "$(CURDIR)/.local-state/tools/rcli-source/node_modules" || { echo 'test-bun: unavailable: .local-state/tools/rcli-source is not the pinned Backboard-R-CLI checkout with installed node_modules' >&2; exit 2; }
	"$(BUN)" test $(BUN_TESTS)

build: desktop-dist
	cargo build --workspace --locked --offline

desktop-dist:
	npm --prefix apps/desktop run build

check:
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test-price-script
	$(MAKE) test
	$(MAKE) build
	$(MAKE) test
