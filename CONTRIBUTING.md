# Contributing to PDQ

Thanks for your interest in improving PDQ! This is a small project, so the process is light.

## Development setup

PDQ is a Rust crate with Python bindings (PyO3 + maturin). You'll need a recent Rust toolchain and [`uv`](https://github.com/astral-sh/uv).

```bash
# Rust
cargo build
cargo test

# Python bindings (rebuild after any Rust change)
uv sync
uv run maturin develop
uv run pytest tests/test_pdq.py
```

See [CLAUDE.md](CLAUDE.md) for the full set of commands and an architecture overview.

## Before opening a pull request

- `cargo fmt` and `cargo clippy --all-targets` are clean.
- `cargo test` passes (Rust unit + integration tests).
- `uv run maturin develop && uv run pytest` passes (Python bindings).
- `uv run ruff check` is clean for Python changes.

## Guidelines

- Keep the core invariant intact: pruning happens at the **row-group** level, not just the file level.
- Prefer DataFusion's standard APIs and patterns over reimplementing engine internals.
- Open an issue first for larger changes so we can agree on the approach.

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
