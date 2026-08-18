# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-17

First release. PDQ builds FST secondary indexes over directories of Parquet
files so that exact-match, prefix, and range lookups read only the row groups
that can contain a hit.

### Added

- **Row-group-precision indexing.** One immutable `fst::Set` per (file, column),
  mapping each value to the specific row groups containing it. Indexing walks a
  directory tree and is incremental — unchanged files are skipped on re-index,
  and `--prune` drops indexes whose source file is gone.
- **DataFusion integration.** `PdqTableProvider` turns index matches into a
  `ParquetAccessPlan` per file and hands the scan to a stock `ParquetSource`, so
  projection, predicate pruning, and page-index pruning all work unmodified.
  Row-group counts come from index metadata, so planning does no Parquet footer
  I/O, and a query with no index matches short-circuits to an empty plan without
  touching the data files at all.
- **CLI** (`pdq`) with `index`, `search`, and `query` subcommands. `search`
  reports matching files and row groups from the index alone; `query` runs the
  full DataFusion query with jsonl, csv, or table output.
- **Python bindings** (`Indexer`, `Searcher`, `QueryEngine`, plus the
  `build_index` / `search_files` convenience wrappers). Data crosses the
  Rust/Python boundary as Arrow with zero copy, async methods run on tokio, and
  results convert to pandas, polars, or narwhals on request.
- abi3 wheels for CPython 3.10+ on Linux (x86_64, aarch64), macOS (x86_64,
  arm64), and Windows (x64).

[0.1.0]: https://github.com/erichutchins/pdq/releases/tag/v0.1.0
