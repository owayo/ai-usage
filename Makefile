.PHONY: build release install uninstall run test fmt fmt-check clippy check clean deps help

# Default target
.DEFAULT_GOAL := help

# Variables
BINARY_NAME := ai-usage
INSTALL_PATH := $(HOME)/.local/bin

## Setup

deps: ## Install build prerequisites (cmake, needed by wreq/BoringSSL)
	@command -v cmake >/dev/null 2>&1 || brew install cmake
	@echo "cmake: $$(cmake --version | head -1)"

## Build Commands

build: ## Build debug version
	cargo build

release: ## Build optimized release version (strip + LTO)
	cargo build --release

## Installation

install: release ## Build release and install to ~/.local/bin
	@mkdir -p $(INSTALL_PATH)
	cp target/release/$(BINARY_NAME) $(INSTALL_PATH)/
	@echo "Installed $(BINARY_NAME) -> $(INSTALL_PATH)/$(BINARY_NAME)"

uninstall: ## Remove the installed binary
	rm -f $(INSTALL_PATH)/$(BINARY_NAME)

## Development

run: ## Build and run (e.g. make run ARGS="--only claude")
	cargo run -- $(ARGS)

test: ## Run tests
	cargo test

fmt: ## Format code
	cargo fmt

fmt-check: ## Check formatting without modifying files
	cargo fmt -- --check

clippy: ## Run clippy with warnings as errors
	cargo clippy --all-targets -- -D warnings

check: clippy fmt-check ## Run clippy + formatting check + cargo check
	cargo check

clean: ## Clean build artifacts
	cargo clean

## Help

help: ## Show this help message
	@echo "$(BINARY_NAME) — make targets"
	@echo ""
	@echo "Usage: make [target]"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Release: GitHub Actions > Release > Run workflow (workflow_dispatch)"
