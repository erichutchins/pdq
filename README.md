# PDQ - Pretty Darn Quick

🚀 **Fast Parquet file search with FST indexing and DataFusion row-group optimization**

PDQ is a high-performance search engine for Parquet files that combines FST (Finite State Transducer) indexing with Apache DataFusion's advanced row-group pruning capabilities. Designed for cybersecurity log analysis and large-scale data search operations.

## ✨ Key Features

### 🎯 **Core Capabilities**

- **Exact match searches** with microsecond-level FST lookups
- **Prefix searches** for pattern matching (e.g., IP subnets)
- **Range queries** for numerical and lexicographic ranges
- **AND/OR query logic** for complex search conditions
- **Native JSONL output** with full Arrow type support
- **Cross-platform compatibility** (Windows, Linux, macOS)

### 🔥 **ParquetAccessPlan Integration**

- **True row-group level optimization** using DataFusion's latest APIs
- **Zero I/O queries** for searches with no matches (< 2ms response time)
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
# 1. Index your Parquet files
./target/release/pdq index --path ./logs/ --column src_ip

# 2. Search with ParquetAccessPlan optimization
./target/release/pdq query --column src_ip --term 192.168.1.100 --output jsonl
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
# Index multiple columns for comprehensive search
./target/release/pdq index --path ./logs/ --column src_ip
./target/release/pdq index --path ./logs/ --column dst_ip
./target/release/pdq index --path ./logs/ --column user_agent
./target/release/pdq index --path ./logs/ --column status_code
```

### Complex Queries

```bash
# Exact match with statistics
./target/release/pdq query --column src_ip --term 192.168.1.100 --stats

# Prefix search for IP subnet
./target/release/pdq query --column src_ip --prefix 192.168.1 --output csv

# Range query for status codes
./target/release/pdq query --column status_code --range 200:299 --limit 1000
```

### Output Formats

```bash
# JSONL (default) - best for log analysis
./target/release/pdq query --column src_ip --term 192.168.1.100 --output jsonl

# CSV - best for spreadsheet analysis
./target/release/pdq query --column src_ip --term 192.168.1.100 --output csv

# Table - pretty printed for humans
./target/release/pdq query --column src_ip --term 192.168.1.100 --format table
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

```bash
$ ./target/release/pdq query --column src_ip --term 192.168.1.100

{"timestamp":"2024-01-01T10:00:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.1","bytes":1024,"status":"200"}
{"timestamp":"2024-01-01T10:01:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.2","bytes":2048,"status":"404"}
{"timestamp":"2024-01-01T10:02:00Z","src_ip":"192.168.1.100","dst_ip":"10.0.0.3","bytes":3072,"status":"200"}

[INFO] PDQ Optimization: Scanning 2/100 files, 4/2000 row groups (99.8% I/O reduction)
[INFO] Query completed in 47ms
```

### Query with No Results

```bash
$ ./target/release/pdq query --column src_ip --term 192.168.999.999

[INFO] PDQ Optimization: Zero I/O - No matches found in index
[INFO] Query completed in 1ms
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

### Cybersecurity Log Analysis

```bash
# Find all connections from suspicious IP
./target/release/pdq query --column src_ip --term 192.168.1.100

# Investigate HTTP 4xx/5xx errors
./target/release/pdq query --column status_code --range 400:599

# Track user agent patterns
./target/release/pdq query --column user_agent --prefix "Mozilla/5.0"
```

### Network Traffic Analysis

```bash
# Find high-bandwidth connections
./target/release/pdq query --column bytes --range 1000000:999999999

# Track specific ports
./target/release/pdq query --column dst_port --term 443

# Analyze protocol distribution
./target/release/pdq query --column protocol --term TCP
```

### Application Log Analysis

```bash
# Find error patterns
./target/release/pdq query --column level --term ERROR

# Track user sessions
./target/release/pdq query --column session_id --term abc123

# Monitor API endpoints
./target/release/pdq query --column endpoint --prefix /api/v1/
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
