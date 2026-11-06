SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := help
.DELETE_ON_ERROR:
MAKEFLAGS += --warn-undefined-variables --no-builtin-rules

CARGO ?= cargo
# Extra cargo flags for every invocation, e.g. `make test CARGO_FLAGS=--offline`.
CARGO_FLAGS ?= --locked
# Arguments forwarded to the binary by `make run`, e.g. `make run ARGS="--url http://127.0.0.1:9090"`.
ARGS ?=

# Run a command with config/state dirs redirected to a throwaway directory so
# nothing can ever read or write ~/.config/tabby (see AGENTS.md).
# usage: $(call isolated,<command>)
define isolated
tmp="$$(mktemp -d)"; trap 'rm -rf "$$tmp"' EXIT; \
XDG_CONFIG_HOME="$$tmp/config" XDG_STATE_HOME="$$tmp/state" XDG_DATA_HOME="$$tmp/data" $(1)
endef

.PHONY: help build release run fmt fmt-check lint test check install uninstall clean

help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Debug build
	$(CARGO) build $(CARGO_FLAGS)

release: ## Optimized build (target/release/tabby)
	$(CARGO) build --release $(CARGO_FLAGS)

run: ## Run with isolated config dirs; pass flags via ARGS="..."
	@$(call isolated,$(CARGO) run $(CARGO_FLAGS) -- $(ARGS))

fmt: ## Format sources
	$(CARGO) fmt

fmt-check: ## Verify formatting
	$(CARGO) fmt --check

lint: ## Clippy, warnings are errors (pedantic is on in Cargo.toml)
	$(CARGO) clippy --all-targets $(CARGO_FLAGS) -- -D warnings

test: ## Run tests with isolated config dirs
	@$(call isolated,$(CARGO) test $(CARGO_FLAGS))

check: fmt-check lint test ## Full per-commit gate: fmt + clippy + test

install: ## Install tabby into ~/.cargo/bin
	$(CARGO) install --path . $(CARGO_FLAGS)

uninstall: ## Remove the installed binary
	$(CARGO) uninstall tabby

clean: ## Remove build artifacts
	$(CARGO) clean
