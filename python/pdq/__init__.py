"""
PDQ: Fast Parquet Data Query using FST-based index

PDQ provides efficient querying of Parquet files by building and using
FST (Finite State Transducer) indices for specific columns.
"""

# Import the Rust module before accessing its version
from .pdq import __version__

# Import main components from Rust implementation
from .pdq import (
    Indexer,
    Searcher,
    QueryEngine,
)

import pyarrow as pa


# Provide top-level convenience functions
def build_index(data_dir, column, index_dir="pdq-index"):
    """
    Build an FST index for a specific column in Parquet files.

    Args:
        data_dir (str): Directory containing Parquet files
        column (str): Column name to index
        index_dir (str): Directory to store the index files

    Returns:
        None
    """
    indexer = Indexer(index_dir)
    indexer.build_index(data_dir, column)


async def search_files(column, term, index_dir="pdq-index", format="arrow"):
    """
    Search for files containing a specific value in a column.

    Returns an Arrow table with columns:
    - file_path (string): Path to the matching file
    - row_group (uint64): Row group index containing the value

    Args:
        column (str): Column name to search
        term (str): Value to search for
        index_dir (str): Directory containing index files
        format (str, optional): Output format:
            - "arrow" (default) - PyArrow Table (zero-copy)
            - "pandas" - Pandas DataFrame
            - "polars" - Polars DataFrame
            - "narwhals" - Narwhals wrapper (for library authors writing framework-agnostic code)

    Returns:
        Search results in the requested format

    Examples:
        >>> # Get Arrow table with file_path and row_group columns
        >>> results = await pdq.search_files("user_id", "12345")
        >>> print(results.to_pandas())

        >>> # Get as Polars DataFrame
        >>> df = await pdq.search_files("user_id", "12345", format="polars")
    """
    searcher = Searcher(index_dir)
    result = await searcher.exact_search(column, term)

    if result is None:
        return None

    # Convert list of record batches to table
    if isinstance(result, list) and len(result) > 0:
        table = pa.Table.from_batches(result)
    else:
        # Empty result - create empty table with proper schema
        schema = pa.schema([("file_path", pa.string()), ("row_group", pa.uint64())])
        table = pa.table({}, schema=schema)

    return _convert_format(table, format)


async def query(column, term, data_dir, index_dir="pdq-index", format="arrow"):
    """
    Query Parquet files for records matching a specific value in a column.

    Args:
        column (str): Column name to search
        term (str): Value to search for
        data_dir (str): Directory containing Parquet files
        index_dir (str): Directory containing index files
        format (str, optional): Output format. Supported:
            - "arrow"    → PyArrow Table (default, zero-copy)
            - "pandas"   → Pandas DataFrame
            - "polars"   → Polars DataFrame
            - "narwhals" → Narwhals wrapper (for library authors writing framework-agnostic code)</parameter>

    Returns:
        Table in the requested format, or None if no matches found

    Examples:
        >>> # Get results as PyArrow table (default)
        >>> table = await pdq.query("user_id", "12345", "./data")
        >>>
        >>> # Get results as Polars DataFrame
        >>> df = await pdq.query("user_id", "12345", "./data", format="polars")
        >>>
        >>> # Get results as Pandas DataFrame
        >>> df = await pdq.query("user_id", "12345", "./data", format="pandas")
    """
    engine = QueryEngine(index_dir, data_dir)
    result = await engine.query(column, term)

    if result is None:
        return None

    # Convert list of record batches to table
    if isinstance(result, list) and len(result) > 0:
        table = pa.Table.from_batches(result)
    else:
        return None

    return _convert_format(table, format)


async def sql_query(sql, data_dir, index_dir="pdq-index", format="arrow"):
    """
    Execute a SQL query against indexed Parquet files.

    Args:
        sql (str): SQL query to execute. Use 'pdq_data' as the table name.
        data_dir (str): Directory containing Parquet files
        index_dir (str): Directory containing index files
        format (str, optional): Output format ('arrow', 'pandas', 'polars', 'narwhals')</parameter>

    Returns:
        Query results in the requested format, or None if no matches found

    Examples:
        >>> # Execute SQL query
        >>> result = await pdq.sql_query(
        ...     "SELECT * FROM pdq_data WHERE user_id = '12345'",
        ...     "./data"
        ... )
        >>>
        >>> # Get results as Polars DataFrame
        >>> df = await pdq.sql_query(
        ...     "SELECT user_id, COUNT(*) as count FROM pdq_data GROUP BY user_id",
        ...     "./data",
        ...     format="polars"
        ... )
    """
    engine = QueryEngine(index_dir, data_dir)
    result = await engine.sql_query(sql)

    if result is None:
        return None

    # Convert list of record batches to table
    if isinstance(result, list) and len(result) > 0:
        table = pa.Table.from_batches(result)
    else:
        return None

    return _convert_format(table, format)


def _convert_format(table, format):
    """
    Convert Arrow table to requested format using native conversions.

    Uses direct conversions (Arrow->Pandas, Arrow->Polars) rather than
    intermediate steps for maximum efficiency. This is faster and more
    memory-efficient than going through intermediate conversion layers.

    Args:
        table: PyArrow Table to convert
        format: Target format ('arrow', 'pandas', 'polars')

    Returns:
        Converted table in the requested format
    """
    # Default: return Arrow table (zero-copy)
    if format == "arrow":
        return table
    elif format == "pandas":
        # Direct Arrow -> Pandas (zero-copy where possible)
        return table.to_pandas()
    elif format == "polars":
        # Direct Arrow -> Polars (zero-copy)
        try:
            import polars as pl
        except ImportError:
            raise ImportError(
                "Polars is required for format='polars'. Install with: pip install polars"
            )
        return pl.from_arrow(table)
    else:
        raise ValueError(
            f"Unsupported format: {format}. Must be one of: 'arrow', 'pandas', 'polars'"
        )


__all__ = [
    "__version__",
    "Indexer",
    "Searcher",
    "QueryEngine",
    "build_index",
    "search_files",
    "query",
    "sql_query",
]
