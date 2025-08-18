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

import narwhals as nw


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


async def search_files(column, term, index_dir="pdq-index"):
    """
    Search for files containing a specific value in a column.

    Args:
        column (str): Column name to search
        term (str): Value to search for
        index_dir (str): Directory containing index files

    Returns:
        list: List of SearchResult objects with file_path and row_group
    """
    searcher = Searcher(index_dir)
    return await searcher.exact_search(column, term)


async def query(column, term, data_dir, index_dir="pdq-index", as_type=None):
    """
    Query Parquet files for records matching a specific value in a column.

    Args:
        column (str): Column name to search
        term (str): Value to search for
        data_dir (str): Directory containing Parquet files
        index_dir (str): Directory containing index files
        as_type (str, optional): Output type. Supported:
            - "arrow"   → PyArrow Table (default)
            - "polars"  → Polars DataFrame
            - "pandas"  → Pandas DataFrame
            - "narwhals"→ Narwhals backend-agnostic frame

    Returns:
        Table in the requested format
    """
    engine = QueryEngine(index_dir, data_dir)
    result = await engine.query(column, term)

    if result is None:
        return None

    if as_type is None or as_type == "arrow":
        return result

    # Wrap in Narwhals
    nw_frame = nw.from_arrow(result)

    if as_type == "narwhals":
        return nw_frame
    elif as_type == "polars":
        return nw_frame.to_polars()
    elif as_type == "pandas":
        return nw_frame.to_pandas()
    else:
        raise ValueError(f"Unsupported as_type: {as_type}")
