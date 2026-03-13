.PHONY: help check test build build-release run-cli run-tui run-gui bundle release clean

CLI_PKG := digital-paper-cli
TUI_PKG := digital-paper-tui
GUI_PKG := digital-paper-gpui

help:
	@printf "%s\n" \
		"Targets:" \
		"  make check         - cargo check the whole workspace" \
		"  make test          - run workspace tests" \
		"  make build         - build the whole workspace" \
		"  make build-release - build the whole workspace in release mode" \
		"  make run-cli       - run the CLI (append ARGS='...')" \
		"  make run-tui       - run the TUI" \
		"  make run-gui       - run the GUI app" \
		"  make bundle        - build the macOS app bundle" \
		"  make release       - release build + bundle" \
		"  make clean         - cargo clean"

check:
	cargo check

test:
	cargo test

build:
	cargo build

build-release:
	cargo build --release

run-cli:
	cargo run -p $(CLI_PKG) -- $(ARGS)

run-tui:
	cargo run -p $(TUI_PKG)

run-gui:
	cargo run -p $(GUI_PKG)

bundle:
	cargo bundle --release -p $(GUI_PKG)

release: build-release bundle

clean:
	cargo clean
