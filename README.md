# PDQ -- Pretty Darn Quick

**Fast, exact Parquet search with FST sidecar indexes and DataFusion row-group pruning.**

PDQ is a search layer for Parquet files built for the large-corpus, rare-needle workload:
finding a handful of cyber security indicators (IPs, hashes, domains, emails, IDs) across many
Parquet log files. It builds a compact FST (finite state transducer) sidecar index next to
your Parquet files that maps each value to the exact `(file, row_group)` set containing it,
then feeds that set into Apache DataFusion so a query reads only the row group(s) that can
match.

## Why PDQ

- **The index answer is authoritative on its own.** A single index lookup tells you an
  indicator is **present** (with the exact files and row groups) or **absent from the entire
  corpus**: no false positives, no candidate list to adjudicate, no Parquet data read. For
  threat hunting and incident response, a fast, _certain_ "not here" is itself the deliverable,
  and that's something a probabilistic index cannot provide.
- **Flat query latency as the corpus grows.** PDQ touches only the matched file and, via
  row-filter pushdown, materializes only the matching row. Query engines scale linearly with
  file count; PDQ stays flat. On a 10/100/1000-file ladder its end-to-end latency moves from
  ~5 ms to ~7 ms warm while DuckDB grows to ~200 ms; see [BENCHMARKS.md](BENCHMARKS.md).
- **Exact, not probabilistic.** The FST returns the precise row-group set, so there's no
  false-positive read tail to clean up.
- **Range and prefix, not just equality.** Because the FST is ordered, it serves prefix (subnet,
  path) and lexicographic range lookups, which a bloom filter can't express at all.
- **Non-invasive.** The index is a separate sidecar; your Parquet files are never modified.

### How it compares to built-in Parquet pruning

Parquet ships two pruning mechanisms, and both fall short on exactly this workload:
high-selectivity predicates on high-cardinality, unsorted columns.

![Comparison of Parquet min/max statistics, bloom filters, and PDQ's FST sidecar for row-group pruning. On an unsorted high-cardinality column, min/max ranges all span the target value so nothing is pruned; a bloom filter prunes most row groups but admits false positives and answers only "probably"; the FST returns the one exact row group with no false positives.](docs/index-comparison.svg)

- **Min/max statistics** only help when a column is sorted or clustered. On an unsorted
  high-cardinality column, nearly every row group's `[min, max]` spans the whole domain, so the
  target sits inside every range and nothing is pruned, even though the value is really in just
  one row group.
- **Bloom filters** fixes equality on unsorted columns, but they're equality-only (no range or
  prefix), probabilistic (a tunable false-positive tail), and answer "definitely no" or
  "probably yes", never "definitely yes." A "probably yes" still has to be confirmed by reading
  the data. (They're also write-only in much of the Python ecosystem: PyArrow, pandas, and
  Polars can _write_ them but don't _prune_ with them on read; only engines like DuckDB and
  DataFusion do.)
- **PDQ's FST** is ordered and exact, so it serves equality, prefix, and range with no false
  positives, and returns the precise row-group set rather than a per-group maybe.

### What it takes to reach a confident "not present"

This is the asymmetry that matters most for hunting. A negative finding (_this indicator is
nowhere in these logs_) is only as trustworthy as the work behind it.

![The path each mechanism takes to an authoritative negative finding. Min/max stats must decode and scan all data on an unsorted column before NO is sound; a bloom filter answers "definitely no" cheaply but any "maybe" forces a data read to clear the false-positive tail; PDQ's FST reaches a conclusive NO in a single index lookup with zero Parquet I/O, because key absence in the automaton is exact rather than probabilistic.](docs/negative-finding.svg)

With min/max stats on an unsorted column, _no_ is only sound after a full scan. With a bloom
filter, any single "maybe" forces a data read to rule it out before a corpus-wide _no_ holds.
With PDQ, absence of the key in the automaton is conclusive in one lookup: zero footers, zero
data reads.

## Quick Start

```bash
git clone https://github.com/erichutchins/pdq.git
cd pdq
cargo build --release
```

```bash
# 1. Index a column across your Parquet files (index stored in ./pdq-index)
./target/release/pdq index --path ./logs/ --column src_ip

# 2. Query for an exact match (returns matching rows)
./target/release/pdq query --column src_ip --term 192.168.1.100 \
    --data-path ./logs/ --format jsonl
```

## Key Features

- **Exact-match queries** over Parquet via SQL/DataFusion, backed by sub-millisecond FST lookups
- **Prefix and lexicographic range lookups** at the index level (`search` subcommand / Python API)
- **String-valued columns** indexed (IPs, hashes, IDs, user agents, etc.)
- **Incremental indexing**: re-indexing only touches new or modified files; `--prune` drops orphans
- **Python bindings** (`Indexer`, `Searcher`, `QueryEngine`) with zero-copy Arrow transfer to pandas/polars
- **CSV / JSON / JSONL / NDJSON / table output** with full Arrow type support
- **Cross-platform** (Windows, Linux, macOS)

> PDQ indexes string (`Utf8`) columns; store/query numeric fields as their string
> representation. The `query` (SQL) path resolves **exact-match** equality predicates; prefix
> and range matching are available as index-level lookups via `search` and the Python API.

### Row-group precision, not just file precision

PDQ's invariant is **which row groups, not just which files**. The FST maps each value to the
exact `(file, row_group)` set, and that set drives every layer below:

- **Zero-footer-I/O planning**: row-group counts for the `ParquetAccessPlan` come from FST index
  metadata, so query _planning_ reads no Parquet footers.
- **Zero-I/O on no-match**: if the index has no hits, the query returns immediately without
  opening a single Parquet file (authoritative from the index).
- **Cached metadata**: a long-lived engine parses each matched file's footer at most once.
- **Row-filter pushdown**: the equality predicate is applied _during_ decode (late
  materialization), so only matching rows are materialized rather than whole row groups.
- **Exact, no false positives**: the FST returns the precise row-group set; no false-positive tail.
- **Multi-core FST search**: index lookups fan out across all CPU cores (rayon).

## Performance

PDQ's advantage comes from reading less data: an FST lookup identifies the exact row groups
that can contain a value, so a query reads only those row groups, and a query with no index
matches returns without touching Parquet at all.

The honest, reproducible numbers live in **[BENCHMARKS.md](BENCHMARKS.md)**: a scaled
head-to-head against Parquet-native bloom filters and the query engines (DuckDB, DataFusion,
Polars) on a dedicated EC2 box, over a 10 / 100 / 1000-file ladder, warm and cold, with the
engines _tuned_ and running a matched query shape. In short:

- **Pruning decision (which row groups):** PDQ wins at every scale with **zero false
  positives**, an mmap'd FST traversal vs. opening and footer-parsing every file.
- **End-to-end, warm:** PDQ is flat (~5-7 ms across a 100x corpus growth) while the engines
  scale with file count; it is 28x faster than DuckDB at 1000 files. Tuning DuckDB's metadata
  cache moves it only -12%, so the gap is structural (PDQ is O(matches), engines are O(files)),
  not a footer-parsing artifact.
- **End-to-end, cold:** PDQ is flat (~15-17 ms) and wins at every scale. The headline is 16x
  vs DuckDB at 1000 files, the cache-symmetric comparison, where the residual asymmetry
  favors DuckDB. (The multiples vs DataFusion/Polars are larger but are upper bounds; see
  the cold-cache notes in BENCHMARKS.md.)
- **Cost:** the FST index is a separate sidecar (larger on disk than embedded blooms, ~6x) with
  a one-time build cost, the deliberate trade for exactness and footer-free pruning.

## Architecture Overview

Data flows through three layers, all keyed on the FST index format:

```
┌────────────────────┐   ┌────────────────────┐   ┌────────────────────────┐
│ Indexer            │   │ IndexQueryEngine   │   │ PdqTableProvider       │
│ (src/index.rs)     │   │ (src/query.rs)     │   │ (src/provider.rs)      │
│                    │──▶│                    │──▶│                        │
│ • walks Parquet    │   │ • mmaps the FSTs   │   │ • FST matches →        │
│   dirs, builds one │   │ • exact/prefix/    │   │   ParquetAccessPlan    │
│   fst::Set per     │   │   range search,    │   │ • stock ParquetSource  │
│   (file, column)   │   │   parallel (rayon) │   │   scan (cached meta,   │
│ • incremental      │   │ • row-group count  │   │   row-filter pushdown) │
│   (skips unchanged)│   │   from index meta  │   │ • empty plan on        │
│ • key: value\x00rgN│   │   (zero footer I/O)│   │   no-match (zero I/O)  │
└────────────────────┘   └────────────────────┘   └────────────────────────┘
```

1. **Indexer** (`src/index.rs`): walks a directory of Parquet files and builds one immutable
   `fst::Set` per `(file, column)`. Keys are `value\x00rgN`. Indexes live at
   `<index-dir>/<file-path-hash>/<column>.fst` with a `metadata.txt` recording the original
   path, mtime, size, and the file's total row-group count. Indexing is incremental; `--prune`
   drops indexes for files that no longer exist.
2. **IndexQueryEngine** (`src/query.rs`): mmaps the FSTs and runs exact/prefix/range searches
   in parallel (rayon) across all indexed files, returning the matching `(file, row_groups)`. It
   serves `num_row_groups` straight from `metadata.txt`, so the provider can size access plans
   without touching a Parquet footer.
3. **PdqTableProvider** (`src/provider.rs`): a DataFusion `TableProvider` that turns FST matches
   into a `ParquetAccessPlan` per file and hands the scan to a stock `ParquetSource`. This is
   DataFusion's documented secondary-index pattern, so projection, predicate/statistics pruning,
   and page-index pruning all work for free, plus a shared metadata cache and row-filter
   pushdown (see the deep dive).

## Advanced Usage

### Multi-column indexing

```bash
./target/release/pdq index --path ./logs/ --column src_ip
./target/release/pdq index --path ./logs/ --column dst_ip
./target/release/pdq index --path ./logs/ --column user_agent
./target/release/pdq index --path ./logs/ --column session_id
```

### Index-level lookups (`search`)

The `search` subcommand resolves a term against the index and prints the matching files and row
groups (no data is read). It supports exact, prefix, and lexicographic range lookups via
`--type`:

```bash
# Exact match (default)
./target/release/pdq search --column src_ip --term 192.168.1.100

# Prefix match, e.g. an IP subnet
./target/release/pdq search --column src_ip --term 192.168.1 --type prefix

# Lexicographic range starting at a term
./target/release/pdq search --column user_agent --term Mozilla --type range
```

### Querying data (`query`)

The `query` subcommand runs a DataFusion exact-match query and returns matching rows. It
requires `--data-path` and chooses output via `--format` (`table`, `csv`, `jsonl`); `--output
<file>` writes to a file instead of stdout.

```bash
# Pretty table (default)
./target/release/pdq query --column src_ip --term 192.168.1.100 --data-path ./logs/

# JSONL, best for log pipelines
./target/release/pdq query --column src_ip --term 192.168.1.100 \
    --data-path ./logs/ --format jsonl

# CSV to a file
./target/release/pdq query --column src_ip --term 192.168.1.100 \
    --data-path ./logs/ --format csv --output results.csv
```

### Incremental indexing & maintenance

Re-running `index` only touches new or modified files. `--prune` drops indexes for deleted
files; queries gracefully skip missing Parquet files still present in the index.

```bash
./target/release/pdq index --path ./sample_data --column src_ip            # incremental
./target/release/pdq index --path ./sample_data --column src_ip --prune    # drop orphans
```

## Example Output

### Query with results

```bash
$ ./target/release/pdq query --column src_ip --term 192.168.1.100 \
      --data-path ./logs/ --format jsonl

📊 Index Results:
   Found 4 matching row groups across 2 files
🎯 Query Complete!
{"timestamp":"2024-01-01T10:00:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.1"}
{"timestamp":"2024-01-01T10:01:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.2"}
```

### Query with no results (zero-I/O fast path)

```bash
$ ./target/release/pdq query --column src_ip --term 192.168.999.999 --data-path ./logs/

⚡ ZERO-MATCH OPTIMIZATION TRIGGERED!
   Result: No matches found (authoritative from index)
```

## Use Cases

Columns are indexed as strings, so numeric fields should be stored/queried as their string
representation. Exact match uses `query`; subnet/path prefixes use `search --type prefix`.

```bash
# Cybersecurity log analysis
./target/release/pdq query  --column src_ip      --term 192.168.1.100  --data-path ./logs/
./target/release/pdq search --column user_agent  --term "Mozilla/5.0"  --type prefix

# Network traffic analysis
./target/release/pdq query  --column dst_port    --term 443            --data-path ./traffic/
./target/release/pdq search --column dst_ip      --term 10.0.0         --type prefix

# Application log analysis
./target/release/pdq query  --column session_id  --term abc123         --data-path ./app-logs/
./target/release/pdq search --column endpoint    --term /api/v1/       --type prefix
```

## Technical Deep Dive

### FST index structure

Each FST index stores sorted keys in the format `value\x00rg{row_group_id}`:

```
192.168.1.100\x00rg0    # IP found in row group 0
192.168.1.100\x00rg5    # IP found in row group 5
192.168.1.101\x00rg2    # Different IP in row group 2
```

### ParquetAccessPlan integration

PDQ uses DataFusion's standard secondary-index pattern: build a `ParquetAccessPlan`, attach it
to the file via `PartitionedFile` extensions, and let the stock `ParquetSource` run the scan.

```rust
// Build an access plan that scans only the matched row groups
let mut access_plan = ParquetAccessPlan::new_none(total_row_groups);
for &row_group_idx in &matched_row_groups {
    access_plan.scan(row_group_idx);
}

// Attach it to the file; DataFusion's ParquetSource honors it at execution time
let file = PartitionedFile::new(path, size).with_extension(access_plan);
```

Because `total_row_groups` comes from FST index metadata (not the Parquet footer), planning
performs **no Parquet footer I/O**.

### Execution-path optimizations

On top of the stock `ParquetSource`, the provider wires in two DataFusion 54 features (adapted
from its `parquet_advanced_index` example) that matter for a long-lived, high-QPS service:

- **Cached Parquet metadata**: a `ParquetFileReaderFactory` serves `ParquetMetaData` from a
  process-lifetime cache, so each matched file's footer is parsed at most once for the engine's
  lifetime. A footer size hint lets the first (cold) read fetch the footer in one shot.
- **Row-filter pushdown**: `with_pushdown_filters(true)` applies the equality predicate as a
  row filter _during_ decode (late materialization). On an unsorted corpus, min/max zonemaps
  can't prune within a row group, so without this the whole matched row group is decoded and a
  `FilterExec` throws most of it away; with it, only the matching rows are materialized.

### Zero-I/O on no-match

```rust
// If the index returns no matches, return an empty plan with no disk I/O
if file_row_groups.is_empty() {
    return Ok(empty_execution_plan);
}
```

## Simulation & Testing

Fabricate a nested hierarchy of Parquet files with a planted needle (`192.168.133.7`):

```bash
uv run misc/fabricate_test_data.py --out ./sample_data --depth 2 --breadth 10 --rows 100000
./target/release/pdq index --path ./sample_data --column src_ip
./target/release/pdq query --column src_ip --term 192.168.133.7 --data-path ./sample_data
```

This generates ~10 million rows across 100 files. If your agentic assistant supports workflows,
`/simulate-nested-search` automates generation, indexing, and verification.

## Development Setup

### Prerequisites

- A recent Rust toolchain (edition 2024; Rust 1.85+)
- [`uv`](https://github.com/astral-sh/uv) for building/testing the Python bindings

DataFusion, Arrow, and Parquet are pulled in as crate dependencies; nothing to install
separately.

### Building from source

```bash
git clone https://github.com/erichutchins/pdq.git
cd pdq
cargo build --release          # CLI
cargo test                     # Rust tests
uv sync && uv run maturin develop && uv run pytest tests/test_pdq.py   # Python bindings
```

### Project structure

```
pdq/
├── src/
│   ├── lib.rs             # Crate root, key-format constants, file hashing
│   ├── index.rs           # FST index builder
│   ├── query.rs           # Multi-core FST search engine
│   ├── provider.rs        # DataFusion TableProvider (ParquetAccessPlan pruning)
│   ├── parquet_filter.rs  # Output formatting
│   ├── py_module.rs       # PyO3 Python bindings
│   └── bin/pdq.rs         # CLI application
├── python/pdq/            # Python package (convenience wrappers)
├── tests/                 # Rust integration tests + pytest suite
└── misc/                  # uv helper scripts (test-data fabrication, benchmarks)
```

## Contributing

Contributions welcome; see the [Contributing Guide](CONTRIBUTING.md).

```bash
git checkout -b feature/your-feature
cargo test && cargo fmt && cargo clippy
git push origin feature/your-feature
```

## Documentation

- [BENCHMARKS.md](BENCHMARKS.md): the FST-vs-bloom shootout (methodology, results, limitations)
- [misc/shootout/README.md](misc/shootout/README.md): runbook to reproduce the shootout
- [API Documentation](https://docs.rs/pdq)

## Acknowledgments

Built on [Apache DataFusion](https://github.com/apache/datafusion) (query engine),
[FST](https://github.com/BurntSushi/fst) (finite state transducers), and
[Apache Arrow](https://github.com/apache/arrow) (columnar data).

## License

MIT. See [LICENSE](LICENSE).
