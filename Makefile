.PHONY: help check test build build-release run-cli run-tui run-gui bundle release clean dpt-cli dpt-tui dpt-gpui

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
		"  make dpt-cli       - legacy alias for run-cli" \
		"  make dpt-tui       - legacy alias for run-tui" \
		"  make dpt-gpui      - legacy alias for run-gui" \
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

# Legacy aliases from the old dpt-* package names.
dpt-cli:
	@echo "Using renamed package 'digital-paper-cli' (legacy alias: dpt-cli)"
	@$(MAKE) run-cli ARGS="$(ARGS)"

dpt-tui:
	@echo "Using renamed package 'digital-paper-tui' (legacy alias: dpt-tui)"
	@$(MAKE) run-tui

dpt-gpui:
	@echo "Using renamed package 'digital-paper-gpui' (legacy alias: dpt-gpui)"
	@$(MAKE) run-gui

bundle:
	cargo bundle --release -p $(GUI_PKG)

release: build-release bundle

clean:
	cargo clean
