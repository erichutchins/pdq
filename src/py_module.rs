use crate::IndexQueryEngine;
use crate::PdqTableProviderBuilder;
use crate::index::Indexer as RustIndexer;
use arrow_pyarrow::PyArrowType;
use datafusion::arrow::array::{StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::logical_expr::{col, lit};
use datafusion::prelude::SessionContext;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use std::path::Path;
use std::sync::Arc;

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
/// Returns results as PyArrow tables for zero-copy efficiency
#[pyclass]
pub struct Searcher {
    inner: Arc<IndexQueryEngine>,
}

#[pymethods]
impl Searcher {
    /// Create a new Searcher for the specified index directory
    #[new]
    fn new(index_dir: &str) -> Self {
        Self {
            inner: Arc::new(IndexQueryEngine::new(index_dir)),
        }
    }

    /// Perform an exact match search across all indexed files
    /// Returns PyArrow table with columns: file_path (string), row_group (uint64)
    fn exact_search<'py>(
        &self,
        column: &str,
        term: &str,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let column = column.to_string();
        let term = term.to_string();

        future_into_py(py, async move {
            // Run CPU-bound FST work in blocking thread pool
            let results = tokio::task::spawn_blocking(move || inner.exact_search(&column, &term))
                .await
                .map_err(|e| PyRuntimeError::new_err(format!("Task failed: {}", e)))?
                .map_err(|e| PyRuntimeError::new_err(format!("Search failed: {}", e)))?;

            // Convert to Arrow RecordBatch
            let batch = Self::matches_to_record_batch(results)
                .map_err(|e| PyRuntimeError::new_err(format!("Arrow conversion failed: {}", e)))?;

            // Return as PyArrow table (zero-copy)
            Python::attach(|py| {
                let py_batch = PyArrowType(vec![batch]);
                Ok(py_batch.into_pyobject(py)?.into_any().unbind())
            })
        })
    }

    /// Perform a prefix search across all indexed files
    /// Returns PyArrow table with columns: file_path (string), row_group (uint64)
    fn prefix_search<'py>(
        &self,
        column: &str,
        prefix: &str,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let column = column.to_string();
        let prefix = prefix.to_string();

        future_into_py(py, async move {
            // Run CPU-bound FST work in blocking thread pool
            let results =
                tokio::task::spawn_blocking(move || inner.prefix_search(&column, &prefix))
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("Task failed: {}", e)))?
                    .map_err(|e| PyRuntimeError::new_err(format!("Search failed: {}", e)))?;

            // Convert to Arrow RecordBatch
            let batch = Self::matches_to_record_batch(results)
                .map_err(|e| PyRuntimeError::new_err(format!("Arrow conversion failed: {}", e)))?;

            // Return as PyArrow table (zero-copy)
            Python::attach(|py| {
                let py_batch = PyArrowType(vec![batch]);
                Ok(py_batch.into_pyobject(py)?.into_any().unbind())
            })
        })
    }

    /// Perform a range search across all indexed files
    /// Returns PyArrow table with columns: file_path (string), row_group (uint64)
    fn range_search<'py>(
        &self,
        column: &str,
        start: &str,
        end: &str,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let column = column.to_string();
        let start = start.to_string();
        let end = end.to_string();

        future_into_py(py, async move {
            // Run CPU-bound FST work in blocking thread pool
            let results =
                tokio::task::spawn_blocking(move || inner.range_search(&column, &start, &end))
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("Task failed: {}", e)))?
                    .map_err(|e| PyRuntimeError::new_err(format!("Search failed: {}", e)))?;

            // Convert to Arrow RecordBatch
            let batch = Self::matches_to_record_batch(results)
                .map_err(|e| PyRuntimeError::new_err(format!("Arrow conversion failed: {}", e)))?;

            // Return as PyArrow table (zero-copy)
            Python::attach(|py| {
                let py_batch = PyArrowType(vec![batch]);
                Ok(py_batch.into_pyobject(py)?.into_any().unbind())
            })
        })
    }
}

impl Searcher {
    /// Convert FileMatches results to Arrow RecordBatch
    /// Schema: file_path (Utf8), row_group (UInt64)
    fn matches_to_record_batch(
        matches: Vec<crate::query::FileMatches>,
    ) -> anyhow::Result<RecordBatch> {
        // Flatten: one row per (file_path, row_group) pair
        let mut file_paths = Vec::new();
        let mut row_groups = Vec::new();

        for file_match in matches {
            let file_path_str = file_match.file_path.to_string_lossy().to_string();
            for row_group in file_match.row_groups {
                file_paths.push(file_path_str.clone());
                row_groups.push(row_group as u64);
            }
        }

        // Create Arrow arrays
        let file_path_array = Arc::new(StringArray::from(file_paths));
        let row_group_array = Arc::new(UInt64Array::from(row_groups));

        // Define schema
        let schema = Arc::new(Schema::new(vec![
            Field::new("file_path", DataType::Utf8, false),
            Field::new("row_group", DataType::UInt64, false),
        ]));

        // Create RecordBatch
        let batch = RecordBatch::try_new(schema, vec![file_path_array, row_group_array])?;

        Ok(batch)
    }
}

/// QueryEngine: Executes queries against Parquet files using FST indices
/// Reuses SessionContext for efficiency
/// Returns results as PyArrow tables/record batches
#[pyclass]
pub struct QueryEngine {
    ctx: Arc<SessionContext>,
}

#[pymethods]
impl QueryEngine {
    /// Create a new QueryEngine with specified index and data directories
    #[new]
    fn new(index_dir: &str, data_dir: &str) -> PyResult<Self> {
        // Build table provider once during initialization
        let table_provider = tokio::runtime::Runtime::new()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to create runtime: {}", e)))?
            .block_on(async {
                PdqTableProviderBuilder::new()
                    .with_index_dir(index_dir)
                    .with_data_dir(data_dir)
                    .build()
                    .await
            })
            .map_err(|e| {
                PyRuntimeError::new_err(format!("Failed to create table provider: {}", e))
            })?;

        // Create and configure SessionContext once
        let ctx = SessionContext::new();
        ctx.register_table("pdq_data", Arc::new(table_provider))
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to register table: {}", e)))?;

        Ok(Self { ctx: Arc::new(ctx) })
    }

    /// Query Parquet files for records matching a specific value in a column
    /// Returns PyArrow table (list of record batches)
    fn query<'py>(&self, column: &str, term: &str, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ctx = self.ctx.clone();
        let column = column.to_string();
        let term = term.to_string();

        future_into_py(py, async move {
            // The provider's scan() runs the FST pruning itself and short-circuits to
            // an empty plan when nothing matches, so we go straight to the query
            // instead of searching the index a second time here just to peek.
            let df = ctx
                .table("pdq_data")
                .await
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to get table: {}", e)))?
                .filter(col(&column).eq(lit(term.clone())))
                .map_err(|e| PyRuntimeError::new_err(format!("Failed to build filter: {}", e)))?;

            // Execute the query and convert to RecordBatch
            let results = df
                .collect()
                .await
                .map_err(|e| PyRuntimeError::new_err(format!("Query execution failed: {}", e)))?;

            if results.is_empty() {
                return Python::attach(|py| Ok(py.None()));
            }

            // Pass record batches directly to PyArrow for zero-copy conversion
            Python::attach(|py| {
                let py_batches = PyArrowType(results);
                Ok(py_batches.into_pyobject(py)?.into_any().unbind())
            })
        })
    }

    /// Execute a SQL query against the indexed data
    /// Returns PyArrow table (list of record batches)
    fn sql_query<'py>(&self, sql: &str, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ctx = self.ctx.clone();
        let sql = sql.to_string();

        future_into_py(py, async move {
            // Execute the SQL query using reused context
            let df = ctx
                .as_ref()
                .sql(&sql)
                .await
                .map_err(|e| PyRuntimeError::new_err(format!("SQL query failed: {}", e)))?;

            // Collect results
            let results = df.collect().await.map_err(|e| {
                PyRuntimeError::new_err(format!("Failed to collect results: {}", e))
            })?;

            if results.is_empty() {
                return Python::attach(|py| Ok(py.None()));
            }

            // Pass record batches directly to PyArrow for zero-copy conversion
            Python::attach(|py| {
                let py_batches = PyArrowType(results);
                Ok(py_batches.into_pyobject(py)?.into_any().unbind())
            })
        })
    }
}

/// PDQ Python module
#[pymodule]
fn pdq(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Indexer>()?;
    m.add_class::<Searcher>()?;
    m.add_class::<QueryEngine>()?;

    // Add module-level attributes
    let version = env!("CARGO_PKG_VERSION");
    m.add("__version__", version)?;

    Ok(())
}
