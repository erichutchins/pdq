use crate::index::Indexer as RustIndexer;
use crate::query::IndexQueryEngine;
use crate::search::Searcher as RustSearcher;
use crate::{PdqTableProviderBuilder, SearchResult as RustSearchResult};
use anyhow::Result;
use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::pyarrow::PyArrowType;
use datafusion::arrow::record_batch::RecordBatchIterator;
use datafusion::dataframe::DataFrame;
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::{col, lit};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_asyncio::tokio::future_into_py;
use std::path::Path;
use std::sync::Arc;

// Re-export SearchResult struct for Python
#[pyclass(frozen)]
#[derive(Clone)]
pub struct SearchResult {
    #[pyo3(get)]
    pub file_path: String,
    #[pyo3(get)]
    pub row_group: usize,
}

impl From<RustSearchResult> for SearchResult {
    fn from(result: RustSearchResult) -> Self {
        Self {
            file_path: result.file_path,
            row_group: result.row_group,
        }
    }
}

/// Indexer: Builds FST indices for Parquet files
#[pyclass]
pub struct Indexer {
    inner: RustIndexer,
}

#[pymethods]
impl Indexer {
    /// Create a new Indexer that will store indices in the specified directory
    #[new]
    fn new(output_dir: &str) -> Self {
        Self {
            inner: RustIndexer::new(output_dir),
        }
    }

    /// Build FST indices for a specific column across all Parquet files in a directory
    fn build_index(&self, data_dir: &str, column: &str) -> PyResult<()> {
        self.inner
            .build_index(Path::new(data_dir), column)
            .map_err(|e| PyRuntimeError::new_err(format!("Index build failed: {}", e)))
    }
}

/// Searcher: Performs fast lookups against FST indices
#[pyclass]
pub struct Searcher {
    inner: RustSearcher,
}

#[pymethods]
impl Searcher {
    /// Create a new Searcher for the specified index directory
    #[new]
    fn new(index_dir: &str) -> Self {
        Self {
            inner: RustSearcher::new(index_dir),
        }
    }

    /// Perform an exact match search across all indexed files
    fn exact_search(&self, column: &str, term: &str) -> PyResult<Vec<SearchResult>> {
        self.inner
            .exact_search(column, term)
            .map(|results| results.into_iter().map(SearchResult::from).collect())
            .map_err(|e| PyRuntimeError::new_err(format!("Search failed: {}", e)))
    }

    /// Perform a prefix search across all indexed files
    fn prefix_search(&self, column: &str, prefix: &str) -> PyResult<Vec<SearchResult>> {
        self.inner
            .search(column, prefix)
            .map(|results| results.into_iter().map(SearchResult::from).collect())
            .map_err(|e| PyRuntimeError::new_err(format!("Search failed: {}", e)))
    }

    /// Perform a range search across all indexed files
    fn range_search(&self, column: &str, start: &str, end: &str) -> PyResult<Vec<SearchResult>> {
        self.inner
            .range_search(column, start, end)
            .map(|results| results.into_iter().map(SearchResult::from).collect())
            .map_err(|e| PyRuntimeError::new_err(format!("Search failed: {}", e)))
    }
}

/// QueryEngine: Executes queries against Parquet files using FST indices
#[pyclass]
pub struct QueryEngine {
    index_dir: String,
    data_dir: String,
}

#[pymethods]
impl QueryEngine {
    /// Create a new QueryEngine with specified index and data directories
    #[new]
    fn new(index_dir: &str, data_dir: &str) -> Self {
        Self {
            index_dir: index_dir.to_string(),
            data_dir: data_dir.to_string(),
        }
    }

    /// Query Parquet files for records matching a specific value in a column
    fn query<'py>(&self, column: &str, term: &str, py: Python<'py>) -> PyResult<&'py PyAny> {
        let index_dir = self.index_dir.clone();
        let data_dir = self.data_dir.clone();
        let column = column.to_string();
        let term = term.to_string();

        future_into_py(py, async move {
            // First check if there are any matches using the Searcher
            let searcher = RustSearcher::new(&index_dir);
            let results = searcher.exact_search(&column, &term)?;

            if results.is_empty() {
                // No matches found, return None
                return Ok(None);
            }

            // Create the PdqTableProvider
            let table_provider = PdqTableProviderBuilder::new("pdq_query".to_string())
                .with_index_dir(&index_dir)
                .with_data_dir(&data_dir)
                .build()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create table provider: {}", e))?;

            // Create a new DataFusion context
            let ctx = SessionContext::new();
            ctx.register_table("pdq_data", Arc::new(table_provider))
                .map_err(|e| anyhow::anyhow!("Failed to register table: {}", e))?;

            // Build and execute the query
            let df = ctx
                .table("pdq_data")
                .map_err(|e| anyhow::anyhow!("Failed to get table: {}", e))?
                .filter(col(&column).eq(lit(term.clone())))
                .map_err(|e| anyhow::anyhow!("Failed to build filter: {}", e))?;

            // Execute the query and convert to RecordBatch
            let results = df
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Query execution failed: {}", e))?;

            if results.is_empty() {
                return Ok(None);
            }

            // Convert to PyArrow table
            let schema = results[0].schema();
            let iter = RecordBatchIterator::new(results.into_iter().map(Ok), schema);
            let table = iter
                .collect::<Result<_>>()
                .map_err(|e| anyhow::anyhow!("Failed to convert to table: {}", e))?;

            Ok(Some(PyArrowType::from_arrow(&table)))
        })
        .map_err(|e| PyRuntimeError::new_err(format!("Query failed: {}", e)))
    }

    /// Execute a SQL query against the indexed data
    fn sql_query<'py>(&self, sql: &str, py: Python<'py>) -> PyResult<&'py PyAny> {
        let index_dir = self.index_dir.clone();
        let data_dir = self.data_dir.clone();
        let sql = sql.to_string();

        future_into_py(py, async move {
            // Create the PdqTableProvider
            let table_provider = PdqTableProviderBuilder::new("pdq_data".to_string())
                .with_index_dir(&index_dir)
                .with_data_dir(&data_dir)
                .build()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create table provider: {}", e))?;

            // Create a new DataFusion context
            let ctx = SessionContext::new();
            ctx.register_table("pdq_data", Arc::new(table_provider))
                .map_err(|e| anyhow::anyhow!("Failed to register table: {}", e))?;

            // Execute the SQL query
            let df = ctx
                .sql(&sql)
                .await
                .map_err(|e| anyhow::anyhow!("SQL query failed: {}", e))?;

            // Collect results
            let results = df
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Failed to collect results: {}", e))?;

            if results.is_empty() {
                return Ok(None);
            }

            // Convert to PyArrow table
            let schema = results[0].schema();
            let iter = RecordBatchIterator::new(results.into_iter().map(Ok), schema);
            let table = iter
                .collect::<Result<_>>()
                .map_err(|e| anyhow::anyhow!("Failed to convert to table: {}", e))?;

            Ok(Some(PyArrowType::from_arrow(&table)))
        })
        .map_err(|e| PyRuntimeError::new_err(format!("SQL query failed: {}", e)))
    }
}

/// PDQ Python module
#[pymodule]
fn pdq(py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<Indexer>()?;
    m.add_class::<Searcher>()?;
    m.add_class::<QueryEngine>()?;
    m.add_class::<SearchResult>()?;

    // Add module-level attributes
    let version = env!("CARGO_PKG_VERSION");
    m.add("__version__", version)?;

    Ok(())
}
