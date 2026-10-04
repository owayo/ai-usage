# ai-usage の開発用タスク。引数なしの `make` でターゲットを一覧表示する。
#
# ツールの版は mise.toml で固定する。mise があれば `mise exec --` 経由で実行し、
# IDE や GUI から起動した場合など、シェルで mise を有効化していなくても固定版を使う。
# SYSTEM_TOOLS=1 を指定すると PATH 上のツールを使うため、版は保証しない。
#
# wreq の BoringSSL が呼ぶ CMake も mise で固定する。SYSTEM_TOOLS=1 の場合のみ、
# make deps が未導入の CMake を Homebrew で入れる。
#
# macOS 標準の GNU Make 3.81 に対応するため、.ONESHELL、.SHELLFLAGS、$(file ...)、!= は使わない。

.DEFAULT_GOAL := help

BINARY_NAME := ai-usage
INSTALL_PATH ?= /usr/local/bin
# コミット済みの Cargo.lock を使い、CI と同じ依存に解決する。
CARGO_FLAGS ?= --locked

# ---- ツールチェーン ------------------------------------------------------------------
# GUI からはシェルの PATH を継承しないことがあるため、PATH と一般的な導入先から mise を探す。
# make MISE=/path/to/mise で上書きできる。mise が無い場合の確認には MISE_CANDIDATES= を使う。
MISE_CANDIDATES ?= $(HOME)/.local/bin/mise /opt/homebrew/bin/mise /usr/local/bin/mise
ifeq ($(SYSTEM_TOOLS),1)
RUN :=
else
ifndef MISE
MISE := $(firstword $(shell command -v mise 2>/dev/null) $(wildcard $(MISE_CANDIDATES)))
endif
ifeq ($(MISE),)
ifneq ($(filter-out help,$(or $(MAKECMDGOALS),help)),)
$(error mise was not found. Install it from https://mise.jdx.dev, or add SYSTEM_TOOLS=1 to use the tools on PATH)
endif
endif
RUN := $(if $(MISE),$(MISE) exec --,)
endif

.PHONY: help setup deps build release run test lint clippy fmt fmt-check check ci install uninstall clean

## Setup

setup: ## Install the toolchain (mise) and dependencies
	@if [ -n "$(MISE)" ]; then "$(MISE)" install; fi
	$(MAKE) deps
	$(RUN) cargo fetch $(CARGO_FLAGS)

deps: ## Ensure CMake is available for the BoringSSL build in wreq
ifeq ($(RUN),)
	@command -v cmake >/dev/null 2>&1 || brew install cmake
else
	@"$(MISE)" install cmake
endif
	@$(RUN) cmake --version | head -1

## Build

build: ## Build a debug binary
	$(RUN) cargo build $(CARGO_FLAGS)

release: ## Build a release binary
	$(RUN) cargo build --release $(CARGO_FLAGS)

run: ## Run the debug binary (arguments via ARGS="...")
	$(RUN) cargo run $(CARGO_FLAGS) -- $(ARGS)

## Checks

test: ## Run the tests
	$(RUN) cargo test $(CARGO_FLAGS)

lint: ## Run clippy with warnings as errors
	$(RUN) cargo clippy $(CARGO_FLAGS) --all-targets -- -D warnings

clippy: lint ## Same as lint (kept for existing habits)

fmt: ## Format the code (rewrites files)
	$(RUN) cargo fmt --all

fmt-check: ## Check the formatting (no changes)
	$(RUN) cargo fmt --all -- --check

check: fmt-check lint ## Run fmt-check and lint (no changes)

ci: check test ## Run the same checks as CI (no changes)

## Install

# macOS は inode ごとにコード署名の検証結果を保持するため、直接上書きすると起動直後に
# SIGKILL (exit 137) となることがある。同じディレクトリの一時ファイルへコピーし、
# rename で inode を置き換えてから使う。
install: release ## Install the release binary to INSTALL_PATH (default /usr/local/bin)
	@mkdir -p "$(INSTALL_PATH)"
	cp "target/release/$(BINARY_NAME)" "$(INSTALL_PATH)/$(BINARY_NAME).new"
	mv -f "$(INSTALL_PATH)/$(BINARY_NAME).new" "$(INSTALL_PATH)/$(BINARY_NAME)"

uninstall: ## Remove the binary from INSTALL_PATH
	rm -f "$(INSTALL_PATH)/$(BINARY_NAME)"

clean: ## Remove build artifacts
	$(RUN) cargo clean

## Help

help: ## Show this help
	@echo "Development tasks for $(BINARY_NAME)"
	@echo ""
	@echo "Usage: make <target>"
	@echo ""
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Tool versions are pinned in mise.toml. Run make setup first."
	@echo "Release: GitHub Actions > Release > Run workflow"
