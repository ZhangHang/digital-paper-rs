# Digital Paper

Personal Sony Digital Paper manager for macOS.

Repo name: `digital-paper-rs`

This repo ships three Rust targets:

- `digital-paper-cli`: command-line tool
- `digital-paper-tui`: terminal UI
- `digital-paper-gpui`: macOS desktop app

It is inspired by and based on parts of [HappyZ/dpt-tools](https://github.com/HappyZ/dpt-tools), but this repo is a separate Rust-first implementation for my own usage.

This codebase is entirely vibe coded. It is pragmatic, personal software, not a polished general-purpose product.

## Scope

- personal-use project
- only tested on macOS
- Wi-Fi is the reliable path today
- USB may still fail depending on device state and macOS USB networking behavior

## What Is In The Repo

- `apps/digital-paper-cli`: CLI for discovery, pairing, sync, and file operations
- `apps/digital-paper-tui`: mouse-capable terminal UI
- `apps/digital-paper-gpui`: macOS GUI app
- `crates/digital-paper-domain`: shared data models and errors
- `crates/digital-paper-provider`: provider traits and shared wiring
- `crates/digital-paper-rust-provider`: native Rust device transport and auth

## What Is Not Shipped

The old Python `dpt-tools` reference code is kept only as a local development reference. It is not part of the shipped product surface and should not be treated as a supported runtime path.

## Naming

- repo/workspace name: `digital-paper-rs`
- product name: `Digital Paper`
- internal crate and binary prefixes: `digital-paper-*`

The repo name is intentionally different from the app name. The repo describes the Rust workspace; the shipped app stays `Digital Paper`.

## Running

CLI:

```bash
cargo run -p digital-paper-cli -- discover
```

TUI:

```bash
cargo run -p digital-paper-tui
```

GUI:

```bash
cargo run -p digital-paper-gpui
```

Build the macOS app bundle:

```bash
cargo bundle -p digital-paper-gpui --release
```

Bundle output:

```text
target/release/bundle/osx/Digital Paper.app
```

## Notes

- pairing and daily use are expected to happen over Wi-Fi
- USB support is still under active experimentation
- the TUI supports keyboard, mouse, recursive search, and context menus in modern terminals like Ghostty
