# PDQ - Pretty Darn Quick

🚀 **Fast Parquet file search with FST indexing and DataFusion row-group optimization**

PDQ is a high-performance search engine for Parquet files that combines FST (Finite State Transducer) indexing with Apache DataFusion's advanced row-group pruning capabilities. Designed for cybersecurity log analysis and large-scale data search operations.

## ✨ Key Features

### 🎯 **Core Capabilities**

- **Exact-match queries** over Parquet via SQL/DataFusion, backed by sub-millisecond FST index lookups
- **Prefix and lexicographic range lookups** at the index level (`search` subcommand / Python API)
- **String-valued columns** are indexed (IPs, hashes, IDs, user agents, etc.)
- **Incremental indexing** — re-indexing only touches new or modified files; `--prune` drops orphan indexes
- **Python bindings** (`Indexer`, `Searcher`, `QueryEngine`) with zero-copy Arrow transfer to pandas/polars
- **CSV / JSON / JSONL / NDJSON / table output** with full Arrow type support
- **Cross-platform** (Windows, Linux, macOS)

> Note: PDQ indexes string (`Utf8`) columns. The `query` (SQL) path resolves
> **exact-match** equality predicates; prefix and range matching are available
> as index-level lookups via the `search` subcommand and the Python API.

### 🔥 **Row-group precision, not just file precision**

PDQ's invariant is **which row groups, not just which files**. The FST index maps
each value to the exact `(file, row_group)` set that can contain it, and that set
drives every layer below:

- **Zero-footer-I/O planning** — row-group counts for the `ParquetAccessPlan` come
  from the FST index metadata, so query *planning* reads no Parquet footers at all.
- **Zero-I/O on no-match** — if the index has no hits, the query returns immediately
  without opening a single Parquet file (authoritative from the index).
- **Cached metadata** — a long-lived engine parses each matched file's footer at
  most once (a shared `ParquetMetaData` cache), instead of re-parsing per query.
- **Row-filter pushdown** — the equality predicate is applied *during* Parquet decode
  (late materialization), so only matching rows are materialized rather than whole
  row groups.
- **Exact, no false positives** — unlike probabilistic filters, the FST returns the
  precise row-group set, so there is no false-positive read tail.
- **Multi-core FST search** — index lookups fan out across all CPU cores (rayon).

## 🚀 Quick Start

### Installation

```bash
# Clone the repository
git clone https://github.com/erichutchins/pdq.git
cd pdq

# Build the project
cargo build --release
```

### Basic Usage

```bash
# 1. Index a column across your Parquet files (index stored in ./pdq-index)
./target/release/pdq index --path ./logs/ --column src_ip

# 2. Query the data for an exact match (returns matching rows)
./target/release/pdq query --column src_ip --term 192.168.1.100 \
    --data-path ./logs/ --format jsonl
```

## 📊 Performance

PDQ's advantage comes from reading less data. An FST lookup identifies the exact
row groups that can contain a value, so a query reads only those row groups
instead of scanning every file, and a query with no index matches returns without
touching the Parquet files at all.

PDQ is tuned for the **large-corpus, rare-needle** workload — finding a handful of
indicators across many Parquet log files. The honest, reproducible numbers live in
**[BENCHMARKS.md](BENCHMARKS.md)**, a scaled head-to-head against Parquet-native
bloom filters on a dedicated EC2 box over a 10 / 100 / 1000-file ladder. In short:

- **Pruning decision (which row groups):** PDQ wins decisively at every scale, with
  **zero false positives** — an mmap'd FST traversal vs. opening and footer-parsing
  every file.
- **End-to-end, warm cache:** PDQ pulls ahead as the corpus grows; the crossover sits
  between small and large ladders, and the lead widens at 1000 files.
- **End-to-end, cold cache:** a compact embedded bloom can win first-touch I/O — PDQ's
  end-to-end edge is a warm, high-QPS, long-lived-service phenomenon.
- **Cost:** the FST index is a separate side structure (larger on disk than embedded
  blooms) with a one-time build cost.

See [BENCHMARKS.md](BENCHMARKS.md) for the full methodology, the corrected results,
and an adversarial review of the limitations.

## 🏗️ Architecture Overview

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

1. **Indexer** (`src/index.rs`) — walks a directory of Parquet files and builds one
   immutable `fst::Set` per `(file, column)`. Keys are `value\x00rgN`. Indexes live at
   `<index-dir>/<file-path-hash>/<column>.fst` with a `metadata.txt` recording the
   original path, mtime, size, and the file's total row-group count. Indexing is
   incremental; `--prune` drops indexes for files that no longer exist.
2. **IndexQueryEngine** (`src/query.rs`) — mmaps the FSTs and runs exact/prefix/range
   searches in parallel (rayon) across all indexed files, returning the matching
   `(file, row_groups)`. It also serves `num_row_groups` straight from `metadata.txt`,
   so the provider can size access plans without touching a Parquet footer.
3. **PdqTableProvider** (`src/provider.rs`) — a DataFusion `TableProvider` that turns
   FST matches into a `ParquetAccessPlan` per file and hands the scan to a stock
   `ParquetSource`. This is DataFusion's documented secondary-index pattern, so
   projection, predicate/statistics pruning, and page-index pruning all work for free —
   plus a shared metadata cache and row-filter pushdown (see the deep dive below).

## 🔧 Advanced Usage

### Multi-Column Indexing

```bash
# Index multiple string columns for comprehensive search
./target/release/pdq index --path ./logs/ --column src_ip
./target/release/pdq index --path ./logs/ --column dst_ip
./target/release/pdq index --path ./logs/ --column user_agent
./target/release/pdq index --path ./logs/ --column session_id
```

### Index-level lookups (`search`)

The `search` subcommand resolves a term against the index and prints the matching
files and row groups (no data is read). It supports exact, prefix, and
lexicographic range lookups via `--type`:

```bash
# Exact match (default)
./target/release/pdq search --column src_ip --term 192.168.1.100

# Prefix match — e.g. an IP subnet
./target/release/pdq search --column src_ip --term 192.168.1 --type prefix

# Lexicographic range starting at a term
./target/release/pdq search --column user_agent --term Mozilla --type range
```

### Querying data (`query`)

The `query` subcommand runs a DataFusion exact-match query and returns the
matching rows. It requires `--data-path` (where the Parquet files live) and
chooses output via `--format` (`table`, `csv`, `jsonl`); `--output <file>` writes
to a file instead of stdout.

```bash
# Pretty table (default)
./target/release/pdq query --column src_ip --term 192.168.1.100 --data-path ./logs/

# JSONL — best for log pipelines
./target/release/pdq query --column src_ip --term 192.168.1.100 \
    --data-path ./logs/ --format jsonl

# CSV to a file
./target/release/pdq query --column src_ip --term 192.168.1.100 \
    --data-path ./logs/ --format csv --output results.csv
```

## 🧪 Simulation & Testing

To simulate a realistic environment with nested directories and "needle in a haystack" scenarios:

### 2. Fabricate Test Data
Use the provided `uv` script to create a nested hierarchy of Parquet files:

```bash
uv run misc/fabricate_test_data.py --out ./sample_data --depth 2 --breadth 10 --rows 100000
```
This generates ~10 million rows across 100 files and hides a specific "needle" IP (`192.168.133.7`) in one of the files.

### 3. Repeatable Testing Workflow
If you are using an agentic assistant that supports workflows, you can run:
```bash
/simulate-nested-search
```
This will automate the generation, indexing, and verification of the search capabilities.

### 4. Incremental Indexing & Maintenance
PDQ supports incremental indexing. If you run the index command again, it will only re-index files that have been modified or are new:

```bash
./target/release/pdq index --path ./sample_data --column src_ip
```

If you delete Parquet files, you can prune the orphan indices using the `--prune` flag:

```bash
./target/release/pdq index --path ./sample_data --column src_ip --prune
```

Queries will gracefully skip any missing Parquet files that are still present in the index.

## 🧪 Example Output

### Query with Results

PDQ prints which files and row groups the index selected, then the matching rows
in the requested format:

```bash
$ ./target/release/pdq query --column src_ip --term 192.168.1.100 \
      --data-path ./logs/ --format jsonl

📊 Index Results:
   Found 4 matching row groups across 2 files
🎯 Query Complete!
{"timestamp":"2024-01-01T10:00:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.1"}
{"timestamp":"2024-01-01T10:01:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.2"}
```

### Query with No Results

When the index has no matches, the query returns immediately without reading any
Parquet data (the zero-I/O fast path):

```bash
$ ./target/release/pdq query --column src_ip --term 192.168.999.999 --data-path ./logs/

⚡ ZERO-MATCH OPTIMIZATION TRIGGERED!
   Result: No matches found (authoritative from index)
```

## 🛠️ Development Setup

### Prerequisites

- A recent Rust toolchain (edition 2024; Rust 1.85+)
- [`uv`](https://github.com/astral-sh/uv) for building/testing the Python bindings

DataFusion, Arrow, and Parquet are pulled in as crate dependencies — nothing to install separately.

### Building from Source

```bash
# Clone and build the CLI
git clone https://github.com/erichutchins/pdq.git
cd pdq
cargo build --release

# Run the Rust tests
cargo test

# Build and test the Python bindings
uv sync
uv run maturin develop
uv run pytest tests/test_pdq.py
```

### Project Structure

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

## 📈 Use Cases

Columns are indexed as strings, so numeric fields should be stored/queried as
their string representation. Exact match uses `query`; subnet/path prefixes use
`search --type prefix`.

### Cybersecurity Log Analysis

```bash
# Find all connections from a suspicious IP (exact match)
./target/release/pdq query --column src_ip --term 192.168.1.100 --data-path ./logs/

# Look up a specific HTTP status code
./target/release/pdq query --column status_code --term 404 --data-path ./logs/

# Find which files/row groups hold a user-agent prefix (index lookup)
./target/release/pdq search --column user_agent --term "Mozilla/5.0" --type prefix
```

### Network Traffic Analysis

```bash
# Track a specific port
./target/release/pdq query --column dst_port --term 443 --data-path ./traffic/

# Analyze a protocol
./target/release/pdq query --column protocol --term TCP --data-path ./traffic/

# Find destinations in a subnet (prefix index lookup)
./target/release/pdq search --column dst_ip --term 10.0.0 --type prefix
```

### Application Log Analysis

```bash
# Find error-level logs
./target/release/pdq query --column level --term ERROR --data-path ./app-logs/

# Track a user session
./target/release/pdq query --column session_id --term abc123 --data-path ./app-logs/

# Find endpoints under a path prefix (index lookup)
./target/release/pdq search --column endpoint --term /api/v1/ --type prefix
```

## 🔬 Technical Deep Dive

### FST Index Structure

Each FST index stores sorted keys in the format:

```
value\x00rg{row_group_id}
```

Example:

```
192.168.1.100\x00rg0    # IP found in row group 0
192.168.1.100\x00rg5    # IP found in row group 5
192.168.1.101\x00rg2    # Different IP in row group 2
```

### ParquetAccessPlan Integration

PDQ uses DataFusion's standard secondary-index pattern: build a `ParquetAccessPlan`
and attach it to the file via `PartitionedFile` extensions, then let the stock
`ParquetSource` run the scan.

```rust
// Build an access plan that scans only the matched row groups
let mut access_plan = ParquetAccessPlan::new_none(total_row_groups);
for &row_group_idx in &matched_row_groups {
    access_plan.scan(row_group_idx);
}

// Attach it to the file; DataFusion's ParquetSource honors it at execution time
let file = PartitionedFile::new(path, size).with_extension(access_plan);
```

Because the row-group count for `ParquetAccessPlan::new_none(total_row_groups)` comes
from the FST index metadata (not the Parquet footer), planning performs **no Parquet
footer I/O**.

### Execution-path optimizations

On top of the stock `ParquetSource`, the provider wires in two DataFusion 54 features
(adapted from its `parquet_advanced_index` example) that matter for a long-lived,
high-QPS service:

- **Cached Parquet metadata** — a `ParquetFileReaderFactory` serves `ParquetMetaData`
  from a process-lifetime cache, so each matched file's footer is parsed at most once
  for the engine's lifetime instead of on every query. A footer size hint lets the
  first (cold) read fetch the footer in a single shot.
- **Row-filter pushdown** — `with_pushdown_filters(true)` applies the equality predicate
  as a row filter *during* decode (late materialization). On an unsorted corpus,
  min/max zonemaps can't prune within a row group, so without this the whole matched
  row group is decoded and a `FilterExec` above the scan throws most of it away; with
  it, only the matching rows are materialized.

### Zero I/O Optimization

```rust
// If index returns no matches, immediately return empty result
if file_row_groups.is_empty() {
    return Ok(empty_execution_plan); // No disk I/O needed!
}
```

## 📊 Benchmarks

The canonical, reproducible benchmark is **[BENCHMARKS.md](BENCHMARKS.md)** — a scaled
FST-vs-bloom-filter shootout (10 / 100 / 1000 files, warm + cold, Layer-1 pruning and
Layer-2 end-to-end) run on a dedicated EC2 box, including an adversarial review of its
own limitations. Read that for any number you intend to quote.

### Quick local sanity check

```bash
uv run misc/fabricate_test_data.py --out ./sample_data --depth 2 --breadth 10 --rows 100000
./target/release/pdq index --path ./sample_data --column src_ip
./target/release/pdq query --column src_ip --term 192.168.133.7 --data-path ./sample_data
```

At small scale, process startup and footer metadata dominate, so the win over a Polars
brute-force scan is modest; the advantage grows with corpus size because query time is
driven by how much data is read, not by the total dataset size. PDQ's clearest,
scale-independent edge is the **pruning decision** (which row groups, exactly, with no
false positives) — see Layer 1 in [BENCHMARKS.md](BENCHMARKS.md).

### Index footprint

The FST index is a **separate side structure** whose size is cardinality-driven: a
high-cardinality column (IPs, hashes) produces a larger FST than a low-cardinality one,
which can collapse to near-zero. It is larger on disk than embedded Parquet bloom
filters — a deliberate trade for exactness and faster, footer-free pruning. See the
measured storage footprint table in [BENCHMARKS.md](BENCHMARKS.md).

## 🤝 Contributing

We welcome contributions! Please see our [Contributing Guide](CONTRIBUTING.md) for details.

### Development Workflow

```bash
# Fork and clone
git clone https://github.com/erichutchins/pdq.git

# Create feature branch
git checkout -b feature/your-feature

# Make changes and test
cargo test
cargo fmt
cargo clippy

# Submit PR
git push origin feature/your-feature
```

## 📚 Documentation
- [BENCHMARKS.md](BENCHMARKS.md) — the canonical FST-vs-bloom shootout (methodology, results, limitations)
- [misc/shootout/README.md](misc/shootout/README.md) — runbook to reproduce the shootout
- [API Documentation](https://docs.rs/pdq)

## 🙏 Acknowledgments

- **Apache DataFusion** for the high-performance query engine
- **FST crate** for the excellent finite state transducer implementation
- **Apache Arrow** for efficient columnar data processing
- **Rust Community** for the amazing ecosystem

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🔗 Related Projects

- [Apache DataFusion](https://github.com/apache/datafusion) - Query engine
- [FST](https://github.com/BurntSushi/fst) - Finite state transducers
- [Apache Arrow](https://github.com/apache/arrow) - Columnar data format
