# Contributing

Thanks for your interest in contributing.

## Development Setup

1. Install Rust stable.
2. Clone the repo.
3. Run checks:

```bash
make check
make test
```

## Workflow

1. Create a feature branch.
2. Make focused changes.
3. Ensure all workspace checks pass:

```bash
make check
make test
```

4. Open a PR with:
- clear summary
- rationale
- screenshots for UI changes
- test notes

## Project Notes

- Primary support target is macOS.
- Keep CLI, TUI, and GUI behaviors aligned where possible.
- Prefer configuration/constants over hardcoded values.

## Code Style

- Keep changes minimal and scoped.
- Avoid unrelated refactors in the same PR.
- Add tests for behavior changes when practical.
