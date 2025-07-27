"""
PDQ: Fast Parquet Data Query using FST-based index

PDQ provides efficient querying of Parquet files by building and using
FST (Finite State Transducer) indices for specific columns.
"""

__version__ = "0.1.0"

# Import main components from Rust implementation
from .pdq import (
    Indexer,
    Searcher,
    QueryEngine,
    SearchResult,
)


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


def search_files(column, term, index_dir="pdq-index"):
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
    return searcher.exact_search(column, term)


def query(column, term, data_dir, index_dir="pdq-index", to_polars=False):
    """
    Query Parquet files for records matching a specific value in a column.

    Args:
        column (str): Column name to search
        term (str): Value to search for
        data_dir (str): Directory containing Parquet files
        index_dir (str): Directory containing index files
        to_polars (bool): Convert result to Polars LazyFrame if True

    Returns:
        ArrowTable or polars.LazyFrame: Query results
    """
    engine = QueryEngine(index_dir, data_dir)
    result = engine.query(column, term)

    if to_polars and result is not None:
        import polars as pl

        return pl.LazyFrame(result)
    return result
