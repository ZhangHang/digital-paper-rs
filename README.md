# digital-paper-rs

Rust workspace for managing Sony Digital Paper devices.

This repo ships three applications:

- `digital-paper-cli` (CLI)
- `digital-paper-tui` (terminal UI)
- `digital-paper-gpui` (desktop app)

It is inspired by:

- [HappyZ/dpt-tools](https://github.com/HappyZ/dpt-tools)
- [janten/dpt-rp1-py](https://github.com/janten/dpt-rp1-py)

This repository is implemented as a Rust-first project for personal usage.

## Features

- Device connect/pair flows
- File browser and transfer (upload/download/delete/rename/move/copy)
- Sync and utility commands in CLI
- Mouse-capable TUI (Ghostty/iTerm-compatible)
- Desktop app for daily document management

## Project Scope

- Primary target: macOS
- Main reliable path: Wi-Fi
- USB transport is supported, but can still fail on some macOS/network setups
- This is personal software and intentionally pragmatic

## Repository Layout

- `apps/digital-paper-cli` CLI app
- `apps/digital-paper-tui` terminal UI app
- `apps/digital-paper-gpui` desktop app
- `crates/digital-paper-domain` shared types/constants/errors
- `crates/digital-paper-provider` provider interface layer
- `crates/digital-paper-rust-provider` native Rust implementation

## Quick Start

Requirements:

- Rust toolchain (stable)
- macOS

Run:

```bash
make check
make build
make run-cli ARGS="discover"
make run-tui
make run-gui
```

Direct cargo examples:

```bash
cargo run -p digital-paper-cli -- discover
cargo run -p digital-paper-tui
cargo run -p digital-paper-gpui
```

## Configuration

- `DPT_DEFAULT_ADDR`: default target address used by TUI/GPUI add-device flows (defaults to `digitalpaper.local`)

Bundle app:

```bash
make bundle
```

Output:

```text
target/release/bundle/osx/Digital Paper.app
```

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution workflow.

## License

Licensed under Apache-2.0. See [LICENSE](LICENSE).
