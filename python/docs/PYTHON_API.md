# PDQ Python API Documentation

## Overview

PDQ (Parquet Data Query) is a high-performance query engine for Parquet files that uses FST (Finite State Transducer) indices to dramatically speed up data access. The PDQ Python package provides bindings to the Rust implementation, enabling Python users to leverage the performance benefits of PDQ with a simple, familiar interface.

## Core Concepts

PDQ operates on three key principles:

1. **Indexing**: Build FST indices for specific columns in Parquet files.
2. **Row Group Pruning**: Use indices to identify and read only the relevant row groups, eliminating unnecessary I/O.
3. **Zero-Match Optimization**: Skip file I/O entirely when indices indicate no matches exist.

## Installation

```bash
pip install pdq
```

## Module Structure

The PDQ Python package consists of:

- High-level convenience functions
- Core classes for direct access to PDQ functionality
- PyArrow integration for handling result data
- Optional Polars integration for further data processing

## API Reference

### High-Level Functions

#### `pdq.build_index(data_dir, column, index_dir="pdq-index")`

Builds an FST index for a specific column across all Parquet files in a directory.

**Parameters**:
- `data_dir` (str): Directory containing Parquet files to index
- `column` (str): Column name to index
- `index_dir` (str, optional): Directory to store the index files. Defaults to "pdq-index".

**Returns**: None

**Example**:
```python
import pdq

# Build an index for the 'customer_id' column
pdq.build_index(data_dir="./data", column="customer_id")
```

#### `pdq.search_files(column, term, index_dir="pdq-index")`

Searches for files containing a specific value in a column, returning file paths and row groups.

**Parameters**:
- `column` (str): Column name to search
- `term` (str): Value to search for
- `index_dir` (str, optional): Directory containing index files. Defaults to "pdq-index".

**Returns**: List of `SearchResult` objects with `file_path` and `row_group` attributes.

**Example**:
```python
# Find files containing customer_id = "C12345"
results = pdq.search_files(column="customer_id", term="C12345")
for match in results:
    print(f"File: {match.file_path}, Row Group: {match.row_group}")
```

#### `pdq.query(column, term, data_dir, index_dir="pdq-index", to_polars=False)`

Queries Parquet files for records matching a specific value in a column.

**Parameters**:
- `column` (str): Column name to search
- `term` (str): Value to search for
- `data_dir` (str): Directory containing Parquet data files
- `index_dir` (str, optional): Directory containing index files. Defaults to "pdq-index".
- `to_polars` (bool, optional): Convert result to Polars LazyFrame. Defaults to False.

**Returns**:
- If `to_polars=False`: PyArrow Table or None if no matches found
- If `to_polars=True`: Polars LazyFrame or None if no matches found

**Example**:
```python
# Query and get PyArrow table
table = pdq.query(column="customer_id", term="C12345", data_dir="./data")
if table:
    df = table.to_pandas()
    print(df)

# Query and get Polars LazyFrame
lf = pdq.query(column="customer_id", term="C12345", data_dir="./data", to_polars=True)
if lf:
    result = lf.filter(pl.col("amount") > 100).collect()
    print(result)
```

### Core Classes

#### `pdq.Indexer`

Class for building FST indices for Parquet files.

##### Constructor

`Indexer(output_dir)`

**Parameters**:
- `output_dir` (str): Directory to store the index files

##### Methods

`build_index(data_dir, column)`

**Parameters**:
- `data_dir` (str): Directory containing Parquet files to index
- `column` (str): Column name to index

**Returns**: None

**Example**:
```python
indexer = pdq.Indexer("./pdq-index")
indexer.build_index("./data", "customer_id")
```

#### `pdq.Searcher`

Class for searching FST indices.

##### Constructor

`Searcher(index_dir)`

**Parameters**:
- `index_dir` (str): Directory containing index files

##### Methods

`exact_search(column, term)`

**Parameters**:
- `column` (str): Column name to search
- `term` (str): Exact value to search for

**Returns**: List of `SearchResult` objects

`prefix_search(column, prefix)`

**Parameters**:
- `column` (str): Column name to search
- `prefix` (str): Prefix to search for

**Returns**: List of `SearchResult` objects

`range_search(column, start, end)`

**Parameters**:
- `column` (str): Column name to search
- `start` (str): Start of range (inclusive)
- `end` (str): End of range (exclusive)

**Returns**: List of `SearchResult` objects

**Example**:
```python
searcher = pdq.Searcher("./pdq-index")

# Exact match
exact_results = searcher.exact_search("customer_id", "C12345")

# Prefix search
prefix_results = searcher.prefix_search("customer_id", "C123")

# Range search
range_results = searcher.range_search("customer_id", "C1000", "C2000")
```

#### `pdq.QueryEngine`

Class for executing queries against Parquet files using FST indices.

##### Constructor

`QueryEngine(index_dir, data_dir)`

**Parameters**:
- `index_dir` (str): Directory containing index files
- `data_dir` (str): Directory containing Parquet data files

##### Methods

`query(column, term)`

**Parameters**:
- `column` (str): Column name to search
- `term` (str): Value to search for

**Returns**: PyArrow Table or None if no matches found

`sql_query(sql)`

**Parameters**:
- `sql` (str): SQL query to execute

**Returns**: PyArrow Table or None if no matches found

**Example**:
```python
engine = pdq.QueryEngine("./pdq-index", "./data")

# Simple query
table = engine.query("customer_id", "C12345")

# SQL query
sql_result = engine.sql_query(
    "SELECT * FROM pdq_data WHERE customer_id = 'C12345' AND amount > 100"
)
```

#### `pdq.SearchResult`

Class representing a search result.

##### Attributes

- `file_path` (str): Path to the file containing the match
- `row_group` (int): Row group index within the file

## PyArrow Integration

PDQ returns query results as PyArrow Tables, which can be easily converted to pandas DataFrames:

```python
table = pdq.query(column="customer_id", term="C12345", data_dir="./data")
if table:
    # Convert to pandas
    df = table.to_pandas()

    # Access columns directly
    customer_ids = table.column("customer_id").to_numpy()

    # Get schema
    schema = table.schema
    print(schema)
```

## Polars Integration

PDQ supports direct conversion to Polars LazyFrames for advanced data processing:

```python
# Get results as a Polars LazyFrame
lf = pdq.query(
    column="customer_id",
    term="C12345",
    data_dir="./data",
    to_polars=True
)

if lf:
    # Use Polars' powerful data processing capabilities
    result = (
        lf.filter(pl.col("amount") > 100)
          .groupby("category")
          .agg([
              pl.col("amount").sum().alias("total_amount"),
              pl.count().alias("transaction_count")
          ])
          .sort("total_amount", descending=True)
          .collect()
    )
    print(result)
```

## Performance Considerations

### Indexing Strategy

- Index columns that are frequently used in filters
- Columns with high cardinality (many unique values) benefit most from indexing
- Index size is typically a small fraction of the original data size

### Query Optimization

- Always filter on indexed columns for best performance
- Complex queries with multiple filter conditions still benefit if at least one column is indexed
- The zero-match optimization completely eliminates I/O for queries that return no results

### Memory Usage

- PDQ uses memory-mapped files for efficient index access
- Result tables are loaded into memory as PyArrow Tables
- For very large result sets, consider adding LIMIT clauses to queries

## Advanced Usage Patterns

### Building Multiple Indices

```python
# Index multiple columns
for column in ["customer_id", "product_id", "transaction_date"]:
    pdq.build_index(data_dir="./data", column=column)
```

### Combining with SQL

```python
# Use SQL for complex queries while leveraging PDQ's optimizations
engine = pdq.QueryEngine("./pdq-index", "./data")
result = engine.sql_query("""
    SELECT
        category,
        COUNT(*) as transaction_count,
        SUM(amount) as total_amount,
        AVG(amount) as avg_amount
    FROM pdq_data
    WHERE customer_id = 'C12345'
    GROUP BY category
    HAVING COUNT(*) > 1
    ORDER BY total_amount DESC
""")
```

### Processing Large Datasets with Polars

```python
# Use Polars for processing large result sets efficiently
lf = pdq.query(
    column="category",
    term="electronics",
    data_dir="./data",
    to_polars=True
)

if lf:
    # Efficient processing with Polars
    result = (
        lf.filter(pl.col("amount") > 1000)
          .select([
              pl.col("customer_id"),
              pl.col("amount"),
              pl.col("transaction_date"),
              (pl.col("amount") * 0.1).alias("tax")
          ])
          .collect()
    )
```

## Troubleshooting

### Common Issues

1. **IndexError: No results found**
   - Ensure the column is properly indexed
   - Verify the exact spelling and case of the search term

2. **FileNotFoundError: Index directory not found**
   - Check that the index directory exists and has the correct path
   - Ensure the indexing process completed successfully

3. **No results returned for a query**
   - Verify the column name and search term
   - Check if the Parquet files contain the expected data
   - Try using the `search_files` function to verify index contents

## API Stability

PDQ follows semantic versioning:
- Major version changes may include breaking API changes
- Minor version updates add functionality in a backward-compatible manner
- Patch releases include backward-compatible bug fixes

## Further Resources

- [Project GitHub Repository](https://github.com/yourusername/pdq)
- [Rust Documentation](https://docs.rs/pdq)
- [Issue Tracker](https://github.com/yourusername/pdq/issues)
