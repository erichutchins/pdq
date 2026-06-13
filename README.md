# PDQ - Pretty Darn Quick

🚀 **Fast Parquet file search with FST indexing and DataFusion row-group optimization**

PDQ is a high-performance search engine for Parquet files that combines FST (Finite State Transducer) indexing with Apache DataFusion's advanced row-group pruning capabilities. Designed for cybersecurity log analysis and large-scale data search operations.

## ✨ Key Features

### 🎯 **Core Capabilities**

- **Exact-match queries** over Parquet via SQL/DataFusion, with microsecond FST lookups
- **Prefix and lexicographic range lookups** at the index level (`search` subcommand / Python API)
- **String-valued columns** are indexed (IPs, hashes, IDs, user agents, etc.)
- **CSV / JSONL / table output** with full Arrow type support
- **Cross-platform** (Windows, Linux, macOS)

> Note: PDQ indexes string (`Utf8`) columns. The `query` (SQL) path resolves
> **exact-match** equality predicates; prefix and range matching are available
> as index-level lookups via the `search` subcommand and the Python API.

### 🔥 **ParquetAccessPlan Integration**

- **True row-group level optimization** using DataFusion's latest APIs
- **Zero-I/O queries** for searches with no matches (returns without reading any Parquet data)
- **Multi-core parallel FST processing** for maximum throughput

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
instead of scanning every file. A query with no index matches returns without
touching the Parquet files at all.

Measured numbers on a local microbenchmark are in
[Baseline Benchmarks](#-baseline-benchmarks-simulated) below. Because query time is driven
by how much data is read rather than the total dataset size, the gap over a full
scan widens as datasets grow — but the figures that matter are the ones you can
reproduce, not extrapolations, so this README sticks to measured results.

## 🏗️ Architecture Overview

### Core Components

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   FST Indexes   │    │ AccessPlanBuilder│    │ DataFusion Exec │
│                 │    │                  │    │                 │
│ • Parallel scan │───▶│ • Row-group IDs  │───▶│ • Optimized I/O │
│ • Multi-core    │    │ • ParquetAccess  │    │ • Predicate push│
│ • Authoritative │    │ • Statistics     │    │ • Zero I/O path │
└─────────────────┘    └──────────────────┘    └─────────────────┘
```

### Data Flow

1. **Index Building**: FST indexes map `value → (file_hash, row_group_id)`
2. **Query Processing**: Parallel FST search across all CPU cores
3. **Access Plan Creation**: ParquetAccessPlan targets specific row groups
4. **DataFusion Execution**: Reads only the necessary data

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

### Zero I/O Optimization

```rust
// If index returns no matches, immediately return empty result
if file_row_groups.is_empty() {
    return Ok(empty_execution_plan); // No disk I/O needed!
}
```

## 📊 Baseline Benchmarks (Simulated)

The following benchmarks were generated using the provided `misc/fabricate_test_data.py` script on a local developer machine.

### Search Performance Comparison
**Dataset**: 100 Parquet files, 10,000,000 rows, nested in a 2x10 hierarchy.
**Tool**: Brute Force baseline using Polars (`pl.scan_parquet().filter().collect()`).

| Test Case | Polars Python (Brute) | PDQ Python Bindings | PDQ CLI (Native Rust) |
| :--- | :--- | :--- | :--- |
| **Match** (Target IP) | ~215 ms | ~112 ms (**2x fast**) | **~20 ms** (**10x fast**) |

> **Why the difference?** Polars is incredibly efficient at brute-forcing data that fits in memory/cache. However, its execution time scales linearly with the number of rows. PDQ's execution time is nearly constant because the FST index lookup determines exactly which row groups to read, skipping 99.9% of the Work.

> **Why the difference?** On small datasets, process startup and metadata overhead account for most of the time. PDQ's advantage grows exponentially with data volume as it skips nearly 100% of the I/O that a full scan must perform.

### Index Building
Building the `src_ip` index for the 160k row dataset takes **< 1 second** on modern NVMe drives, with an index size of approximately **2-5%** of the original Parquet data volume.

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
- TBD
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
