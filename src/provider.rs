use std::any::Any;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::arrow::datatypes::{Schema, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::Session;
use datafusion::common::{DFSchema, DataFusionError, Result as DataFusionResult};
use datafusion::datasource::TableProvider;
use datafusion::datasource::listing::PartitionedFile;
use datafusion::datasource::source::DataSourceExec;
use datafusion::datasource::physical_plan::{
    FileScanConfigBuilder, FileSource, ParquetSource,
};
use datafusion::datasource::physical_plan::{FileOpenFuture, FileOpener, FileScanConfig};
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::utils::conjunction;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::arrow::async_reader::{ParquetObjectReader, ParquetRecordBatchStreamBuilder};
use datafusion::parquet::arrow::ProjectionMask;
use datafusion::physical_plan::ExecutionPlan;
use futures::StreamExt;
use futures::stream::BoxStream;
use object_store::ObjectStore;
use rayon::prelude::*;

// Parquet execution imports
use datafusion::datasource::physical_plan::parquet::ParquetAccessPlan;

use crate::calculate_file_hash;
use crate::query::IndexQueryEngine;

type PrunedRowGroups = HashMap<String, Vec<usize>>;
type HashToPath = HashMap<String, PathBuf>;

/// Enhanced PDQ TableProvider with DataFusion v49 improvements
///
/// This implementation provides:
/// 1. Modern FileScanConfigBuilder usage with ParquetSource
/// 2. PruningStatistics implementation leveraging FST indices
/// 3. Optimized metadata handling via `ParquetFileReaderFactory`
/// 4. Parallel schema inference
#[derive(Debug)]
pub struct PdqTableProvider {
    /// FST-based index query engine
    index_engine: Arc<IndexQueryEngine>,
    /// Schema of the table
    schema: SchemaRef,
    /// Table name for identification
    _table_name: String,
    /// Whether to use row-level selections
    use_row_selections: bool,
}

impl PdqTableProvider {
    /// Create a new PDQ TableProvider with modern DataFusion integration
    pub async fn new(
        index_engine: Arc<IndexQueryEngine>,
        data_dir: PathBuf,
        table_name: String,
        use_row_selections: bool,
    ) -> anyhow::Result<Self> {
        let schema = Self::infer_schema_from_directory(&data_dir).await?;

        Ok(Self {
            index_engine,
            schema,
            _table_name: table_name,
            use_row_selections,
        })
    }

    /// Enable or disable row-level selections
    pub fn set_use_row_selection(&mut self, use_row_selections: bool) {
        self.use_row_selections = use_row_selections;
    }

    /// Check if row selections are enabled
    pub fn use_row_selections(&self) -> bool {
        self.use_row_selections
    }

    /// Infer schema from all Parquet files in directory (parallelized)
    async fn infer_schema_from_directory(data_dir: &Path) -> anyhow::Result<SchemaRef> {
        use walkdir::WalkDir;

        // Collect all parquet files
        let parquet_files: Vec<_> = WalkDir::new(data_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "parquet"))
            .map(|e| e.path().to_path_buf())
            .collect();

        if parquet_files.is_empty() {
            return Err(anyhow::anyhow!("No Parquet files found in directory"));
        }

        // Process files in parallel using rayon to get schemas
        let schemas: Vec<SchemaRef> = parquet_files
            .par_iter()
            .map(|path| Self::infer_schema_from_file(path))
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Merge all schemas
        let mut merged_schema = schemas[0].clone();
        for schema in schemas.iter().skip(1) {
            let merged = Schema::try_merge(vec![(*merged_schema).clone(), (**schema).clone()])?;
            merged_schema = Arc::new(merged);
        }

        Ok(merged_schema)
    }

    /// Infer schema from a single Parquet file
    fn infer_schema_from_file(file_path: &Path) -> anyhow::Result<SchemaRef> {
        let file = std::fs::File::open(file_path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        Ok(builder.schema().clone())
    }

    /// Use FST index to prune files and row groups
    ///
    /// Optimized to use set intersection instead of O(n²) retain + contains.
    /// When multiple filters apply, we intersect the row groups to get only those
    /// that satisfy all conditions.
    fn prune_with_fst_index(
        &self,
        filters: &[Expr],
    ) -> anyhow::Result<(PrunedRowGroups, HashToPath)> {
        let mut file_row_groups: HashMap<String, HashSet<usize>> = HashMap::new();
        let mut hash_to_path: HashMap<String, PathBuf> = HashMap::new();

        for filter in filters {
            if let Some((column, value)) = self.extract_equality_filter(filter)
                && let Ok(results) = self.index_engine.exact_search(&column, &value)
            {
                for file_match in results {
                    let file_hash = calculate_file_hash(&file_match.file_path.to_string_lossy())?;
                    let row_group_set: HashSet<usize> = file_match.row_groups.into_iter().collect();

                    hash_to_path.insert(file_hash.clone(), file_match.file_path);

                    match file_row_groups.entry(file_hash) {
                        Entry::Occupied(mut e) => {
                            // Intersect with existing row groups (only keep those in both sets)
                            let intersection: HashSet<usize> = e
                                .get()
                                .iter()
                                .copied()
                                .filter(|g| row_group_set.contains(g))
                                .collect();
                            e.insert(intersection);
                        }
                        Entry::Vacant(e) => {
                            // First time seeing this file hash
                            e.insert(row_group_set);
                        }
                    }
                }
            }
        }

        // Convert HashSets back to sorted Vecs for consistency with API expectations
        let pruned_row_groups = file_row_groups
            .into_iter()
            .map(|(file_hash, row_group_set)| {
                let mut row_groups: Vec<usize> = row_group_set.into_iter().collect();
                row_groups.sort_unstable();
                (file_hash, row_groups)
            })
            .collect();

        Ok((pruned_row_groups, hash_to_path))
    }

    /// Extract column=value equality filters for FST index usage
    fn extract_equality_filter(&self, expr: &Expr) -> Option<(String, String)> {
        if let Expr::BinaryExpr(binary_expr) = expr {
            use datafusion::logical_expr::Operator;

            if binary_expr.op == Operator::Eq {
                if let (Expr::Column(column), Expr::Literal(scalar_value, _)) =
                    (&*binary_expr.left, &*binary_expr.right)
                {
                    return Some((column.name.clone(), scalar_value.to_string()));
                }
                if let (Expr::Literal(scalar_value, _), Expr::Column(column)) =
                    (&*binary_expr.left, &*binary_expr.right)
                {
                    return Some((column.name.clone(), scalar_value.to_string()));
                }
            }
        }
        None
    }

}

#[async_trait]
impl TableProvider for PdqTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    /// Enhanced scan implementation using modern DataFusion APIs and FST pruning
    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> DataFusionResult<Arc<dyn ExecutionPlan>> {
        // FST lookups only — zero disk I/O at planning time
        let (file_row_groups, hash_to_path) = self
            .prune_with_fst_index(filters)
            .map_err(|e| DataFusionError::Plan(format!("FST pruning failed: {e}")))?;

        // Note: PDQ only returns results for equality filters on indexed columns.
        // If filters is empty or contains no equality filters, prune_with_fst_index()
        // returns an empty map and this method returns an empty DataSourceExec.
        // Full-table scans without equality filters are not supported — callers
        // should always provide equality predicates on indexed columns.

        let object_store_url = ObjectStoreUrl::parse("file://")?;
        let object_store = state
            .runtime_env()
            .object_store(object_store_url.clone())
            .map_err(|e| DataFusionError::Plan(format!("Failed to get object store: {e}")))?;

        // No matches → empty plan immediately, no I/O
        if file_row_groups.is_empty() {
            let source = Arc::new(PdqFileSource::new(
                ParquetSource::new(self.schema.clone()),
                object_store,
            ));
            let config = FileScanConfigBuilder::new(object_store_url, source)
                .with_projection_indices(projection.cloned())?
                .with_limit(limit)
                .build();
            return Ok(DataSourceExec::from_data_source(config));
        }

        // Build predicate for downstream FilterExec
        let df_schema = DFSchema::try_from(self.schema.clone())?;
        let predicate = conjunction(filters.to_vec());
        let predicate = predicate
            .map(|p| state.create_physical_expr(p, &df_schema))
            .transpose()?
            .unwrap_or_else(|| datafusion::physical_expr::expressions::lit(true));

        let source = Arc::new(PdqFileSource::new(
            ParquetSource::new(self.schema.clone()).with_predicate(predicate),
            Arc::clone(&object_store),
        ));

        let mut file_scan_config_builder =
            FileScanConfigBuilder::new(object_store_url, source)
                .with_projection_indices(projection.cloned())?
                .with_limit(limit);

        // Store Vec<usize> of matched row group indices in extensions.
        // PdqParquetOpener reads the footer and uses these indices at execution time.
        for (file_hash, row_groups) in &file_row_groups {
            // Skip files where filter intersection eliminated all row groups.
            // This can happen when multiple equality filters on the same column
            // produce non-overlapping row group sets.
            if row_groups.is_empty() {
                continue;
            }

            if let Some(file_path) = hash_to_path.get(file_hash) {
                let canonical_path = std::fs::canonicalize(file_path).map_err(|e| {
                    DataFusionError::Plan(format!("Path canonicalization failed: {e}"))
                })?;
                let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);

                let mut partitioned_file =
                    PartitionedFile::new(canonical_path.display().to_string(), file_size);
                partitioned_file.extensions = Some(Arc::new(row_groups.clone()));

                file_scan_config_builder = file_scan_config_builder.with_file(partitioned_file);
            }
        }

        Ok(DataSourceExec::from_data_source(file_scan_config_builder.build()))
    }

    /// Enhanced filter pushdown support with FST-aware categorization
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DataFusionResult<Vec<TableProviderFilterPushDown>> {
        // PDQ can prune row groups with FST but still needs row-level filtering for all filters
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }
}

/// Builder for PDQ TableProvider with fluent API
pub struct PdqTableProviderBuilder {
    index_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    table_name: Option<String>,
    use_row_selections: bool,
}

impl PdqTableProviderBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            index_dir: None,
            data_dir: None,
            table_name: None,
            use_row_selections: false,
        }
    }

    /// Set the index directory
    pub fn with_index_dir<P: Into<PathBuf>>(mut self, index_dir: P) -> Self {
        self.index_dir = Some(index_dir.into());
        self
    }

    /// Set the data directory
    pub fn with_data_dir<P: Into<PathBuf>>(mut self, data_dir: P) -> Self {
        self.data_dir = Some(data_dir.into());
        self
    }

    /// Set the table name
    pub fn with_table_name<S: Into<String>>(mut self, table_name: S) -> Self {
        self.table_name = Some(table_name.into());
        self
    }

    /// Enable row-level selections
    pub fn with_row_selections(mut self, enabled: bool) -> Self {
        self.use_row_selections = enabled;
        self
    }

    /// Build the PDQ TableProvider
    pub async fn build(self) -> anyhow::Result<PdqTableProvider> {
        let index_dir = self
            .index_dir
            .ok_or_else(|| anyhow::anyhow!("Index directory is required"))?;
        let data_dir = self
            .data_dir
            .ok_or_else(|| anyhow::anyhow!("Data directory is required"))?;
        let table_name = self.table_name.unwrap_or_else(|| "pdq_table".to_string());

        let index_engine = Arc::new(IndexQueryEngine::new(index_dir));

        PdqTableProvider::new(index_engine, data_dir, table_name, self.use_row_selections).await
    }
}

impl Default for PdqTableProviderBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Custom FileOpener that reads Parquet footer at execution time.
///
/// Receives `Vec<usize>` (matched row group indices) from `PartitionedFile::extensions`
/// set by `scan()`. Opens the file, reads the footer, builds a row group selection,
/// and returns the record batch stream.
pub struct PdqParquetOpener {
    object_store: Arc<dyn ObjectStore>,
    projection: Option<Vec<usize>>,
    batch_size: usize,
}

impl PdqParquetOpener {
    pub fn new(
        object_store: Arc<dyn ObjectStore>,
        projection: Option<Vec<usize>>,
        batch_size: usize,
    ) -> Self {
        Self { object_store, projection, batch_size }
    }
}

impl FileOpener for PdqParquetOpener {
    fn open(
        &self,
        partitioned_file: PartitionedFile,
    ) -> datafusion::common::Result<FileOpenFuture> {
        let object_store = Arc::clone(&self.object_store);
        let projection = self.projection.clone();
        let batch_size = self.batch_size;

        // Extract Vec<usize> from extensions — matched row group indices from scan()
        let row_groups: Vec<usize> = partitioned_file
            .extensions
            .as_ref()
            .and_then(|ext| ext.downcast_ref::<Vec<usize>>())
            .cloned()
            .unwrap_or_default();

        let location = partitioned_file.object_meta.location.clone();
        let file_size = partitioned_file.object_meta.size;

        Ok(Box::pin(async move {
            // Footer read happens HERE, at execution time (not planning time)
            let reader = ParquetObjectReader::new(object_store, location)
                .with_file_size(file_size);
            let builder = ParquetRecordBatchStreamBuilder::new(reader).await?;

            let total_rgs = builder.metadata().num_row_groups();

            // Build access plan: start with none, enable only matched row groups
            let mut access_plan = ParquetAccessPlan::new_none(total_rgs);
            for &idx in &row_groups {
                if idx < total_rgs {
                    access_plan.scan(idx);
                }
            }

            let valid_rg_indexes = access_plan.row_group_indexes();

            if valid_rg_indexes.is_empty() {
                // All indices were stale — return empty stream
                let empty: BoxStream<'static, datafusion::common::Result<RecordBatch>> =
                    Box::pin(futures::stream::empty());
                return Ok(empty);
            }

            let mut builder = builder
                .with_row_groups(valid_rg_indexes)
                .with_batch_size(batch_size);

            if let Some(proj) = projection {
                let parquet_schema = builder.parquet_schema().clone();
                let mask = ProjectionMask::roots(&parquet_schema, proj);
                builder = builder.with_projection(mask);
            }

            let stream = builder.build()?;
            let mapped: BoxStream<'static, datafusion::common::Result<RecordBatch>> =
                Box::pin(stream.map(|r| r.map_err(DataFusionError::from)));
            Ok(mapped)
        }))
    }
}

/// FileSource that wraps ParquetSource but uses PdqParquetOpener for file opens.
///
/// Delegates all FileSource methods to the inner ParquetSource, except
/// create_file_opener() which returns a PdqParquetOpener. This moves
/// Parquet footer I/O from scan() (planning) to open() (execution).
pub struct PdqFileSource {
    inner: Arc<dyn FileSource>,
    object_store: Arc<dyn ObjectStore>,
}

impl Clone for PdqFileSource {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            object_store: Arc::clone(&self.object_store),
        }
    }
}

impl std::fmt::Debug for PdqFileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdqFileSource")
            .field("file_type", &self.inner.file_type())
            .finish()
    }
}

impl PdqFileSource {
    pub fn new(inner: ParquetSource, object_store: Arc<dyn ObjectStore>) -> Self {
        Self {
            inner: Arc::new(inner),
            object_store,
        }
    }
}

impl FileSource for PdqFileSource {
    fn create_file_opener(
        &self,
        _object_store: Arc<dyn ObjectStore>,
        base_config: &FileScanConfig,
        _partition: usize,
    ) -> datafusion::common::Result<Arc<dyn FileOpener>> {
        const DEFAULT_BATCH_SIZE: usize = 8192; // DataFusion default
        let batch_size = base_config.batch_size.unwrap_or(DEFAULT_BATCH_SIZE);
        Ok(Arc::new(PdqParquetOpener::new(
            Arc::clone(&self.object_store),
            None, // projection — let DataFusion handle at a higher level
            batch_size,
        )))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn table_schema(&self) -> &datafusion::datasource::table_schema::TableSchema {
        self.inner.table_schema()
    }

    fn with_batch_size(&self, batch_size: usize) -> Arc<dyn FileSource> {
        let new_inner = self.inner.with_batch_size(batch_size);
        Arc::new(PdqFileSource {
            inner: new_inner,
            object_store: Arc::clone(&self.object_store),
        })
    }

    fn metrics(&self) -> &datafusion::physical_plan::metrics::ExecutionPlanMetricsSet {
        self.inner.metrics()
    }

    fn file_type(&self) -> &str {
        self.inner.file_type()
    }

    /// Forward projection pushdown to the inner ParquetSource, then re-wrap in PdqFileSource.
    /// This is required for FileScanConfigBuilder::with_projection_indices() to succeed.
    fn try_pushdown_projection(
        &self,
        projection: &datafusion::physical_expr::projection::ProjectionExprs,
    ) -> datafusion::common::Result<Option<Arc<dyn FileSource>>> {
        match self.inner.try_pushdown_projection(projection)? {
            Some(new_inner) => Ok(Some(Arc::new(PdqFileSource {
                inner: new_inner,
                object_store: Arc::clone(&self.object_store),
            }))),
            None => Ok(None),
        }
    }

    // INTENTIONAL: try_pushdown_filters() is NOT forwarded.
    // PDQ performs its own row-group pruning via FST index (Vec<usize> in PartitionedFile::extensions).
    // Row-level filtering is handled by DataFusion's FilterExec downstream (TableProviderFilterPushDown::Inexact).
    // Page-level Parquet pruning (bloom filters, page index) is intentionally traded away.
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::datatypes::{DataType, Field};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_pdq_table_provider_builder() {
        let temp_dir = TempDir::new().unwrap();
        let index_dir = temp_dir.path().join("index");
        let data_dir = temp_dir.path().join("data");

        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();

        let result = PdqTableProviderBuilder::new()
            .with_index_dir(&index_dir)
            .with_data_dir(&data_dir)
            .with_table_name("test_table")
            .with_row_selections(true)
            .build()
            .await;

        // Should fail because no parquet files exist
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_equality_filter() {
        use datafusion::logical_expr::{col, lit};

        let temp_dir = TempDir::new().unwrap();
        let index_engine = Arc::new(IndexQueryEngine::new(temp_dir.path()));
        let schema = Arc::new(Schema::new(vec![Field::new(
            "test_col",
            DataType::Utf8,
            false,
        )]));

        let provider = PdqTableProvider {
            index_engine,
            schema,
            _table_name: "test".to_string(),
            use_row_selections: false,
        };

        let filter = col("test_col").eq(lit("test_value"));
        let result = provider.extract_equality_filter(&filter);

        assert!(result.is_some());
        let (column, value) = result.unwrap();
        assert_eq!(column, "test_col");
        assert_eq!(value, "test_value");
    }
}
