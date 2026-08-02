# PDQ: Fast Parquet Query Engine

PDQ is a high-performance query engine for Parquet files that uses FST (Finite State Transducer) indices to dramatically speed up lookups. It's built in Rust for maximum performance with Python bindings for ease of use.

## Features

- **Lightning-fast queries**: Pre-built FST indices enable near-instant lookups
- **Row-group level pruning**: Only reads the specific parts of files that contain matching data
- **Zero-match optimization**: Skips file I/O entirely when index indicates no matches exist
- **Async/await support**: Non-blocking operations for high-performance applications
- **Pure Arrow/PyArrow interface**: All data transfer uses Arrow for zero-copy efficiency
- **SQL support**: Query your Parquet files using familiar SQL syntax
- **Multi-format output**: Seamlessly convert results to Polars, Pandas, or keep as PyArrow

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

Search results are returned as PyArrow tables for efficient processing:

````python
import asyncio
import pdq

async def search():
    # Find all files containing customer_id = "C123456"
    # Returns Arrow table with columns: file_path, row_group
    results = await pdq.search_files(
        column="customer_id",
        term="C123456",
        index_dir="./pdq-index"
    )

    # Convert to Pandas to view results
    if results is not None:
        df = results.to_pandas()
        print(df)
        # Output:
        #          file_path  row_group
        # 0  data/file1.parquet          0
        # 1  data/file1.parquet          3
        # 2  data/file2.parquet          1

asyncio.run(search())
```</parameter>
````

### Querying Data

Execute efficient queries using the indexed columns (async operation):

```python
import asyncio

async def query_data():
    # Query all records with customer_id = "C123456"
    # Returns PyArrow Table by default (zero-copy)
    table = await pdq.query(
        column="customer_id",
        term="C123456",
        data_dir="./data",
        index_dir="./pdq-index"
    )

    if table:
        # Convert to Pandas DataFrame
        df = table.to_pandas()
        print(df)
    else:
        print("No results found")

asyncio.run(query_data())
```

### Direct Format Conversion

Get results directly in your preferred format:

```python
async def query_polars():
    # Get results directly as Polars DataFrame
    df = await pdq.query(
        column="customer_id",
        term="C123456",
        data_dir="./data",
        index_dir="./pdq-index",
        format="polars"  # Options: "arrow", "polars", "pandas"
    )

    if df is not None:
        # Use Polars operations directly
        result = df.filter(pl.col("amount") > 100).sort("amount")
        print(result)

asyncio.run(query_polars())
```

### SQL Queries

Execute SQL queries against your indexed data:

```python
async def sql_query():
    result = await pdq.sql_query(
        sql="SELECT customer_id, AVG(amount) as avg_amount "
            "FROM pdq_data "
            "WHERE customer_id >= 'C000100' AND customer_id < 'C000200' "
            "GROUP BY customer_id "
            "ORDER BY avg_amount DESC "
            "LIMIT 10",
        data_dir="./data",
        index_dir="./pdq-index",
        format="polars"  # Get results as Polars DataFrame
    )

    if result is not None:
        print(result)

asyncio.run(sql_query())
```

### Parallel Searches

Leverage async/await for parallel operations:

````python
async def parallel_search():
    searcher = pdq.Searcher("./pdq-index")

    # Search for multiple values in parallel
    results = await asyncio.gather(
        searcher.exact_search("customer_id", "C000100"),
        searcher.exact_search("customer_id", "C000200"),
        searcher.exact_search("customer_id", "C000300"),
    )

    # Each result is an Arrow table
    for i, table in enumerate(results):
        if table is not None:
            print(f"Search {i+1}: Found {len(table)} match(es)")
            print(table.to_pandas())

asyncio.run(parallel_search())
```</parameter>
````

## API Reference

### Convenience Functions

#### `pdq.build_index(data_dir, column, index_dir="pdq-index")`

Builds an FST index for a specific column across all Parquet files in a directory.

**Parameters:**

- `data_dir` (str): Directory containing Parquet files
- `column` (str): Column name to index
- `index_dir` (str): Directory to store the index files

**Returns:** None

---

#### `async pdq.search_files(column, term, index_dir="pdq-index", format="arrow")`

Searches for files containing a specific value in a column.

**Parameters:**

- `column` (str): Column name to search
- `term` (str): Value to search for
- `index_dir` (str): Directory containing index files
- `format` (str): Output format ("arrow", "polars", "pandas")

**Returns:** PyArrow Table (or converted format) with columns:

- `file_path` (string): Path to the matching Parquet file
- `row_group` (uint64): Row group index containing the value

Each row represents one (file, row_group) match. The same file may appear multiple times if the value exists in multiple row groups.

**Note:** This is an async function and must be awaited.</parameter>

---

#### `async pdq.query(column, term, data_dir, index_dir="pdq-index", format="arrow")`

Queries Parquet files for records matching a specific value in a column.

**Parameters:**

- `column` (str): Column name to search
- `term` (str): Value to search for
- `data_dir` (str): Directory containing Parquet files
- `index_dir` (str): Directory containing index files
- `format` (str): Output format - one of:
  - `"arrow"` (default) - PyArrow Table (zero-copy)
  - `"polars"` - Polars DataFrame
  - `"pandas"` - Pandas DataFrame

**Returns:** Table in the requested format, or None if no matches found

**Note:** This is an async function and must be awaited.

---

#### `async pdq.sql_query(sql, data_dir, index_dir="pdq-index", format="arrow")`

Executes a SQL query against indexed Parquet files.

**Parameters:**

- `sql` (str): SQL query to execute. Use `pdq_data` as the table name.
- `data_dir` (str): Directory containing Parquet files
- `index_dir` (str): Directory containing index files
- `format` (str): Output format (same options as `query()`)

**Returns:** Query results in the requested format, or None if no matches found

**Note:** This is an async function and must be awaited.

---

### Classes

#### `pdq.Indexer(output_dir)`

Builds FST indices for Parquet files.

**Methods:**

- `build_index(data_dir, column)` - Build an index for a specific column

**Example:**

```python
indexer = pdq.Indexer("./pdq-index")
indexer.build_index("./data", "customer_id")
```

---

#### `pdq.Searcher(index_dir)`

Performs fast lookups against FST indices.

**Methods (all async, all return PyArrow tables):**

- `async exact_search(column, term)` - Exact match search, returns Arrow table with (file_path, row_group) columns
- `async prefix_search(column, prefix)` - Find all values starting with prefix, returns Arrow table
- `async range_search(column, start, end)` - Find all values in range [start, end], returns Arrow table

**Example:**

````python
searcher = pdq.Searcher("./pdq-index")

# All methods return PyArrow tables
results = await searcher.exact_search("customer_id", "C123456")
print(results.to_pandas())  # Convert to Pandas to view

prefix_results = await searcher.prefix_search("customer_id", "C0001")
range_results = await searcher.range_search("customer_id", "C000100", "C000200")
```</parameter>
````

---

#### `pdq.QueryEngine(index_dir, data_dir)`

Executes queries against Parquet files using FST indices. Reuses DataFusion SessionContext for efficiency.

**Methods (all async):**

- `async query(column, term)` - Query for records matching a value
- `async sql_query(sql)` - Execute a SQL query

**Example:**

```python
engine = pdq.QueryEngine("./pdq-index", "./data")

# Execute multiple queries with the same engine (efficient)
result1 = await engine.query("customer_id", "C123456")
result2 = await engine.sql_query("SELECT * FROM pdq_data WHERE amount > 100")
```

## ---</parameter>

## Performance Considerations

### Index Building

- Index building is a one-time operation that enables fast subsequent queries
- Columns with high cardinality (many unique values) benefit most from indexing
- The FST index size is typically a small fraction of the raw data size

### Query Performance

- **Row group precision**: PDQ eliminates I/O at the row group level, not just file level
- **Zero-match optimization**: When index indicates no matches, no I/O is performed
- **Async operations**: All search and query operations are non-blocking
- **Reusable contexts**: QueryEngine reuses DataFusion contexts for efficiency

### Memory Efficiency

- **Zero-copy**: Pure PyArrow interface avoids unnecessary data copies between Rust and Python
- **Arrow format**: All results use Arrow's columnar format for efficient memory usage
- **RecordBatch streaming**: Results use Arrow RecordBatches for efficient processing</parameter>

### Async Best Practices

```python
# Good: Run multiple searches in parallel
results = await asyncio.gather(
    searcher.exact_search("col1", "val1"),
    searcher.exact_search("col2", "val2"),
    searcher.exact_search("col3", "val3"),
)

# Good: Reuse QueryEngine for multiple queries
engine = pdq.QueryEngine(index_dir, data_dir)
result1 = await engine.query("customer_id", "C123")
result2 = await engine.query("customer_id", "C456")

# Avoid: Creating new engine for each query (wastes resources)
result1 = await pdq.QueryEngine(index_dir, data_dir).query("customer_id", "C123")
result2 = await pdq.QueryEngine(index_dir, data_dir).query("customer_id", "C456")
```

## Advanced Usage

### Custom Index Locations

```python
# Build indices in a custom location
pdq.build_index(
    data_dir="./data",
    column="user_id",
    index_dir="./custom-index-location"
)

# Query using custom index location
result = await pdq.query(
    column="user_id",
    term="user_123",
    data_dir="./data",
    index_dir="./custom-index-location"
)
```

### Multiple Column Indices

```python
# Build indices for multiple columns
for column in ["customer_id", "product_id", "category"]:
    pdq.build_index(data_dir="./data", column=column)

# Query using different columns
customers = await pdq.query(column="customer_id", term="C123", data_dir="./data")
products = await pdq.query(column="product_id", term="P456", data_dir="./data")
```

### Integration with Data Pipelines

````python
import asyncio
import pdq
import pyarrow.compute as pc

async def process_customer(customer_id: str):
    """Process a single customer's data"""
    # Get results as Arrow table
    table = await pdq.query(
        column="customer_id",
        term=customer_id,
        data_dir="./data",
        format="arrow"
    )

    if table is not None:
        # Use Arrow compute functions for efficient processing
        total_amount = pc.sum(table["amount"]).as_py()
        return {"customer_id": customer_id, "total": total_amount}
    return None

async def batch_process_customers(customer_ids: list):
    """Process multiple customers in parallel"""
    results = await asyncio.gather(*[
        process_customer(cid) for cid in customer_ids
    ])
    return [r for r in results if r is not None]

# Run batch processing
customer_ids = ["C123", "C456", "C789"]
results = asyncio.run(batch_process_customers(customer_ids))
```</parameter>
````

## Comparison with Alternatives

| Feature           | PDQ         | Scanning Parquet | DuckDB         | ClickHouse  |
| ----------------- | ----------- | ---------------- | -------------- | ----------- |
| Index Size        | Small (FST) | N/A              | Large (B-tree) | Large       |
| Row Group Pruning | ✅ Precise  | ❌ Full scan     | ✅ Approximate | ✅ Yes      |
| Zero-match I/O    | ✅ None     | ❌ Full scan     | ⚠️ Minimal     | ⚠️ Minimal  |
| Setup Complexity  | Low         | None             | Medium         | High        |
| Async Python API  | ✅ Native   | N/A              | ❌ Blocking    | ❌ Blocking |
| Memory Usage      | Low         | Medium           | High           | Medium      |

## Troubleshooting

### "No results found" when data should exist

- Verify the index was built for the correct column: `pdq.build_index(data_dir, column)`
- Ensure index_dir matches between indexing and querying
- Check that the search term exactly matches values in the data

### Poor performance

- Build indices for columns you frequently query
- Use `format="arrow"` (default) for best performance
- Reuse `QueryEngine` instances instead of creating new ones
- Consider running multiple searches in parallel with `asyncio.gather()`

### Memory issues with large results

- Use PyArrow format (default) which is most memory-efficient
- Consider adding additional filters in SQL queries to reduce result size
- Process results in batches if possible

## License

MIT License

## Contributing

Contributions are welcome! Please see our [GitHub repository](https://github.com/erichutchins/pdq) for details.
