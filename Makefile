.PHONY: build release install uninstall run test fmt fmt-check clippy check clean deps help

# 既定 target
.DEFAULT_GOAL := help

# 変数
BINARY_NAME := ai-usage
INSTALL_PATH := $(HOME)/.local/bin

## Setup

deps: ## ビルド前提(cmake。wreq/BoringSSL に必要)を導入
	@command -v cmake >/dev/null 2>&1 || brew install cmake
	@echo "cmake: $$(cmake --version | head -1)"

## ビルド

build: ## debug 版をビルド
	cargo build

release: ## 最適化 release 版をビルド(strip + LTO)
	cargo build --release

## Installation

install: release ## release をビルドして ~/.local/bin へインストール
	@mkdir -p $(INSTALL_PATH)
	cp target/release/$(BINARY_NAME) $(INSTALL_PATH)/
	@echo "Installed $(BINARY_NAME) -> $(INSTALL_PATH)/$(BINARY_NAME)"

uninstall: ## インストール済み binary を削除
	rm -f $(INSTALL_PATH)/$(BINARY_NAME)

## Development

run: ## ビルドして実行(例: make run ARGS="--only claude")
	cargo run -- $(ARGS)

test: ## テストを実行
	cargo test

fmt: ## code を format
	cargo fmt

fmt-check: ## file を変更せず format を確認
	cargo fmt -- --check

clippy: ## warning を error として clippy を実行
	cargo clippy --all-targets -- -D warnings

check: clippy fmt-check ## clippy + format check + cargo check を実行
	cargo check

clean: ## build artifact を削除
	cargo clean

## Help

help: ## この help message を表示
	@echo "$(BINARY_NAME) — make targets"
	@echo ""
	@echo "使い方: make [target]"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Release: make release または make install"
