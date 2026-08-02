# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What PDQ Is

PDQ ("Pretty Darn Quick") is a secondary indexing system for fast exact-match/prefix/range searches of cybersecurity indicators (IPs, hashes, emails) across Parquet log files. It builds FST (finite state transducer) indexes that map values to specific **row groups** within Parquet files, then uses DataFusion's `ParquetAccessPlan` to read only those row groups — including a zero-I/O fast path when the index has no matches.

It is a hybrid Rust crate + Python package (PyO3/maturin) with a CLI binary.

## Commands

### Rust
```bash
cargo build --release          # CLI at target/release/pdq
cargo test                     # all Rust tests
cargo test --test integration_tests   # one integration test file (tests/*.rs)
cargo test test_name           # single test by name
cargo clippy
cargo fmt
```

### Python bindings (uv + maturin)
```bash
uv sync                        # set up .venv (Python 3.13 in devcontainer)
uv run maturin develop         # rebuild bindings — required after ANY Rust change
uv run pytest tests/test_pdq.py            # Python test suite
uv run pytest tests/test_pdq.py -k name    # single Python test
uv run ruff check              # lint (line-length 90, target py310)
```

Note: maturin is configured with `locked = true`, so `Cargo.lock` must be up to date. Python tests import the compiled `pdq.pdq` module, so `maturin develop` must run before pytest after Rust edits.

### Test data + CLI workflow
```bash
uv run misc/fabricate_test_data.py --out ./sample_data --depth 2 --breadth 10 --rows 100000
./target/release/pdq index --path ./sample_data --column src_ip   # incremental; add --prune to drop orphan indexes
./target/release/pdq search --column src_ip --term 192.168.133.7  # index-only lookup (file + row group)
./target/release/pdq query --column src_ip --term 192.168.133.7   # full DataFusion query, jsonl/csv/table output
```

## Architecture

Data flows through three layers, all keyed on the FST index format:

1. **Indexer** (`src/index.rs`) — walks a directory of Parquet files and builds one immutable `fst::Set` per (file, column). Keys are `value\x00rgN` (constants in the `key_format` module of `src/lib.rs`). Indexes live at `<index-dir>/<file-path-hash>/<column>.fst` with a `metadata.txt` storing (line by line) the original path, mtime, size, and the file's total row-group count. `calculate_file_hash` (src/lib.rs) hashes the file *path*, not contents. Indexing is incremental (skips unchanged files).

2. **IndexQueryEngine** (`src/query.rs`) — mmaps FSTs and runs exact/prefix/range searches in parallel (rayon) across all indexed files, returning `FileMatches { file_path, row_groups }`. Also exposes `num_row_groups(file_hash)` (read from `metadata.txt`) so the provider can build access plans without touching the Parquet footer. Range semantics rely on the key format: e.g. exact match is `ge("value\x00").lt("value\x01")`.

3. **PdqTableProvider** (`src/provider.rs`) — a DataFusion `TableProvider` (built via `PdqTableProviderBuilder`) that converts FST matches into a `ParquetAccessPlan` per file, attached via `PartitionedFile::with_extension`, then hands the scan to a stock `ParquetSource`. This is DataFusion's documented secondary-index pattern (see its `advanced_parquet_index` example), so projection, predicate pruning, and page-index pruning all work without custom code. The row-group count for `ParquetAccessPlan::new_none(count)` comes from the index, so `scan()` does no Parquet footer I/O at planning time. Empty index results short-circuit to an empty plan.

The CLI (`src/bin/pdq.rs`, clap) and Python bindings (`src/py_module.rs`, exposing `Indexer`, `Searcher`, `QueryEngine`) are thin wrappers over these layers. Python convenience functions (`build_index`, `search_files`, ...) live in `python/pdq/__init__.py`; all Rust↔Python data transfer is Arrow (zero-copy via arrow-pyarrow), async methods use pyo3-async-runtimes on tokio, and results convert to pandas/polars/narwhals on request.

DataFusion is pinned (currently 54.0.0, `default-features = false`); major upgrades can require rework in `src/provider.rs`, though relying on the stock `ParquetSource` + `ParquetAccessPlan` pattern keeps that surface small.

### Repository layout notes

- `src/` and `tests/` are the real code; `tests/` mixes Rust integration tests (`*.rs`) and the pytest suite (`test_pdq.py`).
- `misc/` holds uv single-file helper scripts (`fabricate_test_data.py`, `brute_force_polars.py`, `search_pdq.py`, `test_incremental.py`) — not part of the build.
- Row-group-level pruning is the core invariant of the project: changes should preserve "which row groups, not just which files" precision.
