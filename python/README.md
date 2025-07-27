# PDQ: Fast Parquet Query Engine

PDQ is a high-performance query engine for Parquet files that uses FST (Finite State Transducer) indices to dramatically speed up lookups. It's built in Rust for maximum performance with Python bindings for ease of use.

## Features

- **Lightning-fast queries**: Pre-built FST indices enable near-instant lookups
- **Row-group level pruning**: Only reads the specific parts of files that contain matching data
- **Zero-match optimization**: Skips file I/O entirely when index indicates no matches exist
- **SQL support**: Query your Parquet files using familiar SQL syntax
- **Polars integration**: Seamlessly convert results to Polars LazyFrames

## Installation

```bash
pip install pdq
```

## Quick Start

### Building an Index

Before querying, you need to build an index for specific columns in your Parquet files:

```python
import pdq

# Build an index for the 'customer_id' column in all Parquet files in data_dir
pdq.build_index(data_dir="./data", column="customer_id", index_dir="./pdq-index")
```

### Finding Files with Matching Data

To quickly find which files contain a specific value (similar to `grep -l`):

```python
# Find all files containing customer_id = "C123456"
results = pdq.search_files(column="customer_id", term="C123456", index_dir="./pdq-index")

# Print matching files and row groups
for match in results:
    print(f"File: {match.file_path}, Row Group: {match.row_group}")
```

### Querying Data

Execute efficient queries using the indexed columns:

```python
# Query all records with customer_id = "C123456"
table = pdq.query(
    column="customer_id",
    term="C123456",
    data_dir="./data",
    index_dir="./pdq-index"
)

# Convert to Pandas DataFrame
df = table.to_pandas()
print(df)
```

### Integration with Polars

PDQ can directly return a Polars LazyFrame:

```python
# Query and get a Polars LazyFrame
lazy_frame = pdq.query(
    column="customer_id",
    term="C123456",
    data_dir="./data",
    index_dir="./pdq-index",
    to_polars=True
)

# Execute Polars operations
result = lazy_frame.filter(pl.col("amount") > 100).collect()
print(result)
```

## API Reference

### `pdq.build_index(data_dir, column, index_dir="pdq-index")`

Builds an FST index for a specific column across all Parquet files in a directory.

### `pdq.search_files(column, term, index_dir="pdq-index")`

Searches for files containing a specific value in a column, returning a list of `SearchResult` objects.

### `pdq.query(column, term, data_dir, index_dir="pdq-index", to_polars=False)`

Queries Parquet files for records matching a specific value in a column, returning either a PyArrow Table or a Polars LazyFrame.

### Advanced Usage with Direct Classes

For more control, you can use the underlying classes directly:

```python
from pdq import Indexer, Searcher, QueryEngine

# Create an indexer
indexer = Indexer("./pdq-index")
indexer.build_index("./data", "customer_id")

# Search using the searcher
searcher = Searcher("./pdq-index")
results = searcher.exact_search("customer_id", "C123456")

# Execute queries with the query engine
engine = QueryEngine("./pdq-index", "./data")
table = engine.query("customer_id", "C123456")

# Execute SQL queries
table = engine.sql_query("SELECT * FROM pdq_data WHERE customer_id = 'C123456'")
```

## Performance Considerations

- Index building is a one-time operation that enables fast subsequent queries
- Columns with high cardinality (many unique values) benefit most from indexing
- The FST index size is typically a small fraction of the raw data size
- The zero-match optimization eliminates I/O for queries that won't return results

## License

Apache License 2.0
