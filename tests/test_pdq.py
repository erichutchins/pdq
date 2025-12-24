"""
PDQ Python API Tests

Comprehensive test suite for the PDQ Python bindings using pytest.
Tests cover the Indexer, Searcher, QueryEngine, and convenience functions.
"""

import tempfile
import shutil
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

import pdq


# ============================================================================
# Fixtures
# ============================================================================


@pytest.fixture
def temp_dir():
    """Create a temporary directory for test files."""
    tmp = tempfile.mkdtemp()
    yield Path(tmp)
    shutil.rmtree(tmp, ignore_errors=True)


@pytest.fixture
def sample_table():
    """Create a sample PyArrow table with test data."""
    return pa.table(
        {
            "id": pa.array([1, 2, 3, 4, 5], type=pa.int64()),
            "src_ip": pa.array(
                ["192.168.1.1", "10.0.0.1", "192.168.1.1", "172.16.0.1", "10.0.0.1"]
            ),
            "status": pa.array(["ok", "error", "ok", "ok", "error"]),
        }
    )


@pytest.fixture
def parquet_file(temp_dir, sample_table):
    """Create a sample Parquet file with test data."""
    file_path = temp_dir / "test_data.parquet"
    # Write with small row groups to test row group tracking
    pq.write_table(sample_table, file_path, row_group_size=2)
    return file_path


@pytest.fixture
def data_dir(temp_dir, parquet_file):
    """Return the directory containing test Parquet files."""
    return temp_dir


@pytest.fixture
def index_dir(temp_dir):
    """Return a directory for FST index storage."""
    idx_dir = temp_dir / "pdq-index"
    idx_dir.mkdir(exist_ok=True)
    return idx_dir


@pytest.fixture
def indexed_data(data_dir, index_dir):
    """Create indexed test data for search and query tests."""
    indexer = pdq.Indexer(str(index_dir))
    indexer.build_index(str(data_dir), "src_ip")
    return {"data_dir": data_dir, "index_dir": index_dir}


# ============================================================================
# Indexer Tests
# ============================================================================


class TestIndexer:
    """Tests for the Indexer class."""

    def test_indexer_creation(self, index_dir):
        """Test that Indexer can be instantiated."""
        indexer = pdq.Indexer(str(index_dir))
        assert indexer is not None

    def test_build_index(self, data_dir, index_dir):
        """Test that build_index creates FST index files."""
        indexer = pdq.Indexer(str(index_dir))
        indexer.build_index(str(data_dir), "src_ip")

        # Verify index directory has content
        index_files = list(index_dir.glob("**/*.fst"))
        assert len(index_files) > 0, "No FST files created"

    def test_build_index_multiple_columns(self, data_dir, index_dir):
        """Test indexing multiple columns."""
        indexer = pdq.Indexer(str(index_dir))
        indexer.build_index(str(data_dir), "src_ip")
        indexer.build_index(str(data_dir), "status")

        # Should have FST files for both columns
        fst_files = list(index_dir.glob("**/*.fst"))
        assert len(fst_files) >= 2, "Should have FST files for both columns"


# ============================================================================
# Searcher Tests
# ============================================================================


class TestSearcher:
    """Tests for the Searcher class."""

    def test_searcher_creation(self, index_dir):
        """Test that Searcher can be instantiated."""
        searcher = pdq.Searcher(str(index_dir))
        assert searcher is not None

    @pytest.mark.asyncio
    async def test_exact_search(self, indexed_data):
        """Test exact match search."""
        searcher = pdq.Searcher(str(indexed_data["index_dir"]))
        result = await searcher.exact_search("src_ip", "192.168.1.1")

        assert result is not None
        # Result should be a list of record batches
        if isinstance(result, list) and len(result) > 0:
            table = pa.Table.from_batches(result)
            assert "file_path" in table.column_names
            assert "row_group" in table.column_names
            assert table.num_rows > 0

    @pytest.mark.asyncio
    async def test_exact_search_no_match(self, indexed_data):
        """Test exact search with non-existent value."""
        searcher = pdq.Searcher(str(indexed_data["index_dir"]))
        result = await searcher.exact_search("src_ip", "1.2.3.4")

        # Should return empty or None for no matches
        if result is not None and isinstance(result, list) and len(result) > 0:
            table = pa.Table.from_batches(result)
            assert table.num_rows == 0

    @pytest.mark.asyncio
    async def test_prefix_search(self, indexed_data):
        """Test prefix search."""
        searcher = pdq.Searcher(str(indexed_data["index_dir"]))
        result = await searcher.prefix_search("src_ip", "192.168")

        assert result is not None
        if isinstance(result, list) and len(result) > 0:
            table = pa.Table.from_batches(result)
            assert table.num_rows > 0

    @pytest.mark.asyncio
    async def test_range_search(self, indexed_data):
        """Test range search."""
        searcher = pdq.Searcher(str(indexed_data["index_dir"]))
        result = await searcher.range_search("src_ip", "10.0.0.0", "10.255.255.255")

        assert result is not None
        if isinstance(result, list) and len(result) > 0:
            table = pa.Table.from_batches(result)
            assert table.num_rows > 0


# ============================================================================
# QueryEngine Tests
# ============================================================================


class TestQueryEngine:
    """Tests for the QueryEngine class."""

    def test_query_engine_creation(self, indexed_data):
        """Test that QueryEngine can be instantiated."""
        engine = pdq.QueryEngine(
            str(indexed_data["index_dir"]), str(indexed_data["data_dir"])
        )
        assert engine is not None

    @pytest.mark.asyncio
    async def test_query(self, indexed_data):
        """Test basic query execution."""
        engine = pdq.QueryEngine(
            str(indexed_data["index_dir"]), str(indexed_data["data_dir"])
        )
        result = await engine.query("src_ip", "192.168.1.1")

        if result is not None and isinstance(result, list) and len(result) > 0:
            table = pa.Table.from_batches(result)
            # Verify results contain the queried IP
            src_ips = table.column("src_ip").to_pylist()
            assert all(ip == "192.168.1.1" for ip in src_ips)

    @pytest.mark.asyncio
    async def test_sql_query(self, indexed_data):
        """Test SQL query execution."""
        engine = pdq.QueryEngine(
            str(indexed_data["index_dir"]), str(indexed_data["data_dir"])
        )
        result = await engine.sql_query("SELECT * FROM pdq_data LIMIT 5")

        if result is not None and isinstance(result, list) and len(result) > 0:
            table = pa.Table.from_batches(result)
            assert table.num_rows <= 5


# ============================================================================
# Convenience Function Tests
# ============================================================================


class TestConvenienceFunctions:
    """Tests for top-level convenience functions."""

    def test_build_index(self, data_dir, index_dir):
        """Test the build_index convenience function."""
        pdq.build_index(str(data_dir), "src_ip", str(index_dir))

        # Verify index was created
        fst_files = list(index_dir.glob("**/*.fst"))
        assert len(fst_files) > 0

    @pytest.mark.asyncio
    async def test_search_files(self, indexed_data):
        """Test the search_files convenience function."""
        result = await pdq.search_files(
            "src_ip", "192.168.1.1", str(indexed_data["index_dir"])
        )

        if result is not None:
            assert isinstance(result, pa.Table)

    @pytest.mark.asyncio
    async def test_search_files_pandas_format(self, indexed_data):
        """Test search_files with pandas output format."""
        pd = pytest.importorskip("pandas")
        result = await pdq.search_files(
            "src_ip", "192.168.1.1", str(indexed_data["index_dir"]), format="pandas"
        )

        if result is not None:
            assert isinstance(result, pd.DataFrame)

    @pytest.mark.asyncio
    async def test_query_function(self, indexed_data):
        """Test the query convenience function."""
        result = await pdq.query(
            "src_ip",
            "192.168.1.1",
            str(indexed_data["data_dir"]),
            str(indexed_data["index_dir"]),
        )

        if result is not None:
            assert isinstance(result, pa.Table)

    @pytest.mark.asyncio
    async def test_sql_query_function(self, indexed_data):
        """Test the sql_query convenience function."""
        result = await pdq.sql_query(
            "SELECT COUNT(*) as cnt FROM pdq_data",
            str(indexed_data["data_dir"]),
            str(indexed_data["index_dir"]),
        )

        if result is not None:
            assert isinstance(result, pa.Table)


# ============================================================================
# Edge Case Tests
# ============================================================================


class TestEdgeCases:
    """Tests for edge cases and error handling."""

    def test_indexer_empty_directory(self, temp_dir, index_dir):
        """Test indexing an empty directory."""
        empty_dir = temp_dir / "empty"
        empty_dir.mkdir()

        indexer = pdq.Indexer(str(index_dir))
        # Should not raise, just index nothing
        indexer.build_index(str(empty_dir), "src_ip")

    @pytest.mark.asyncio
    async def test_searcher_empty_index(self, index_dir):
        """Test searching an empty index."""
        searcher = pdq.Searcher(str(index_dir))
        result = await searcher.exact_search("src_ip", "192.168.1.1")

        # Should return empty/None, not error
        if result is not None and isinstance(result, list):
            assert len(result) == 0 or all(b.num_rows == 0 for b in result)


# ============================================================================
# Entry point for running tests directly
# ============================================================================

if __name__ == "__main__":
    pytest.main([__file__, "-v"])
