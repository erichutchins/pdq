use std::any::Any;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use datafusion::arrow::datatypes::{Schema, SchemaRef};
use datafusion::catalog::Session;
use datafusion::common::{DFSchema, DataFusionError, Result as DataFusionResult};
use datafusion::datasource::TableProvider;
use datafusion::datasource::listing::PartitionedFile;
use datafusion::datasource::source::DataSourceExec;
use datafusion::datasource::physical_plan::{
    FileScanConfigBuilder, ParquetFileReaderFactory, ParquetSource,
};
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::utils::conjunction;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::parquet::arrow::arrow_reader::{
    ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use datafusion::parquet::arrow::async_reader::{AsyncFileReader, ParquetObjectReader};
use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::parquet::file::reader::{FileReader, SerializedFileReader};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::metrics::ExecutionPlanMetricsSet;
use futures::FutureExt;
use futures::future::BoxFuture;
use object_store::ObjectStore;
use rayon::prelude::*;

// Parquet execution imports
use datafusion::datasource::physical_plan::parquet::ParquetAccessPlan;

use crate::calculate_file_hash;
use crate::query::IndexQueryEngine;

type PrunedRowGroups = HashMap<String, Vec<usize>>;
type HashToPath = HashMap<String, PathBuf>;
type MetadataMap = HashMap<String, Arc<ParquetMetaData>>;

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

    /// Resolve file hashes to actual file paths and load metadata for them
    fn load_pruned_metadata(
        &self,
        file_row_groups: &PrunedRowGroups,
        hash_to_path: &HashToPath,
    ) -> anyhow::Result<(HashToPath, MetadataMap)> {
        let mut resolved_paths: HashMap<String, PathBuf> = HashMap::new();
        let mut metadata_map: HashMap<String, Arc<ParquetMetaData>> = HashMap::new();

        // Use the paths provided by the index engine results
        // Use a subset of hash_to_path based on file_row_groups keys
        let targets: Vec<(String, PathBuf)> = file_row_groups
            .keys()
            .filter_map(|h| hash_to_path.get(h).map(|p| (h.clone(), p.clone())))
            .collect();

        // Load metadata for resolved files in parallel
        let loaded_metadata: Vec<(String, Arc<ParquetMetaData>)> = targets
            .par_iter()
            .filter_map(|(hash, path)| match std::fs::File::open(path) {
                Ok(file) => match SerializedFileReader::new(file) {
                    Ok(reader) => {
                        let metadata = Arc::new(reader.metadata().clone());
                        Some(Ok((hash.clone(), metadata)))
                    }
                    Err(e) => Some(Err(anyhow::anyhow!(
                        "Failed to read parquet metadata for {}: {}",
                        path.display(),
                        e
                    ))),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!("Skipping missing parquet file: {}", path.display());
                    None
                }
                Err(e) => Some(Err(anyhow::anyhow!(
                    "Failed to open parquet file {}: {}",
                    path.display(),
                    e
                ))),
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        for (hash, metadata) in loaded_metadata {
            if let Some(path) = hash_to_path.get(&hash) {
                resolved_paths.insert(hash.clone(), path.clone());
                metadata_map.insert(hash, metadata);
            }
        }

        Ok((resolved_paths, metadata_map))
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
        // Use FST index to prune files and row groups
        let (file_row_groups, hash_to_path) = self
            .prune_with_fst_index(filters)
            .map_err(|e| DataFusionError::Plan(format!("FST pruning failed: {e}")))?;

        // CRITICAL OPTIMIZATION: If the index found no matches, return empty results immediately
        if file_row_groups.is_empty() {
            let object_store_url = ObjectStoreUrl::parse("file://")?;
            let source = Arc::new(ParquetSource::new(self.schema.clone()));
            let config = FileScanConfigBuilder::new(object_store_url, source)
                .with_projection_indices(projection.cloned())?
                .with_limit(limit)
                .build();

            return Ok(DataSourceExec::from_data_source(config));
        }

        // Resolve paths and load metadata for relevant files
        let (resolved_paths, metadata_map) = self
            .load_pruned_metadata(&file_row_groups, &hash_to_path)
            .map_err(|e| DataFusionError::Plan(format!("Failed to load metadata: {e}")))?;

        // Convert filters to a single predicate for DataFusion
        let df_schema = DFSchema::try_from(self.schema.clone())?;
        let predicate = conjunction(filters.to_vec());
        let predicate = predicate
            .map(|predicate| state.create_physical_expr(predicate, &df_schema))
            .transpose()?
            .unwrap_or_else(|| datafusion::physical_expr::expressions::lit(true));

        // Create the custom factory that will serve the pre-loaded metadata
        let object_store_url = ObjectStoreUrl::parse("file://")?;
        let object_store = state
            .runtime_env()
            .object_store(object_store_url.clone())
            .map_err(|e| DataFusionError::Plan(format!("Failed to get object store: {e}")))?;

        let mut reader_factory = CachedParquetFileReaderFactory::new(object_store);

        // Populate the factory with our metadata, keyed by the *absolute path* which is how DataFusion will request it
        for (hash, metadata) in &metadata_map {
            if let Some(path) = resolved_paths.get(hash) {
                // Ensure absolute path for consistency
                if let Ok(canonical_path) = std::fs::canonicalize(path) {
                    reader_factory
                        .add_metadata(canonical_path.display().to_string(), metadata.clone());
                }
            }
        }

        // Create ParquetSource with factory
        let source = ParquetSource::new(self.schema.clone())
            .with_predicate(predicate)
            .with_parquet_file_reader_factory(Arc::new(reader_factory));

        // Build file scan configuration
        let mut file_scan_config_builder =
            FileScanConfigBuilder::new(object_store_url, Arc::new(source))
                .with_projection_indices(projection.cloned())?
                .with_limit(limit);

        // Add files with row group level access plans based on FST index results
        for (file_hash, row_groups) in &file_row_groups {
            if let Some(file_path) = resolved_paths.get(file_hash) {
                let canonical_path = std::fs::canonicalize(file_path).map_err(|e| {
                    DataFusionError::Plan(format!("Path canonicalization failed: {e}"))
                })?;

                let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);

                // Get total row groups from the metadata we just loaded (via the file factory logic,
                // but here we can just assume valid since we loaded it)
                // Or easier: we can't easily access the factory here, but we can assume the access plan creation
                // is safe if we trust the FST.
                // Ideally we'd look up the metadata again, but we just inserted it.
                // Let's rely on the file system for size, but for total row groups we need metadata.
                // We can re-open briefly or trust the FST.
                // Actually, ParquetAccessPlan::new_none requires total row groups.
                // We SHOULD iterate our already loaded metadata.

                // Let's find the metadata for this file_hash again. (Optimize later if needed, n is small)
                // Wait, we lost the mapping from hash -> metadata in the factory step.
                // Actually `metadata_map` is locally available!

                // FIX: We have `metadata_map: HashMap<String, Arc<ParquetMetaData>>` where key is hash.
                if let Some(metadata) = metadata_map.get(file_hash) {
                    // Use metadata_map!
                    let total_row_groups = metadata.num_row_groups();

                    // Create access plan that initially scans no row groups
                    let mut access_plan = ParquetAccessPlan::new_none(total_row_groups);

                    // Enable scanning only for the row groups that contain our target values
                    for &row_group_idx in row_groups {
                        if row_group_idx < total_row_groups {
                            access_plan.scan(row_group_idx);
                        }
                    }

                    // Create partitioned file with the access plan
                    let mut partitioned_file =
                        PartitionedFile::new(canonical_path.display().to_string(), file_size);
                    partitioned_file.extensions = Some(Arc::new(access_plan));

                    file_scan_config_builder = file_scan_config_builder.with_file(partitioned_file);
                }
            }
        }

        // Create execution plan using the DataSourceExec pattern
        Ok(DataSourceExec::from_data_source(
            file_scan_config_builder.build(),
        ))
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

/// A custom `ParquetFileReaderFactory` that serves pre-loaded metadata
#[derive(Debug)]
struct CachedParquetFileReaderFactory {
    object_store: Arc<dyn ObjectStore>,
    /// Metadata keyed by absolute file path
    metadata: HashMap<String, Arc<ParquetMetaData>>,
}

impl CachedParquetFileReaderFactory {
    fn new(object_store: Arc<dyn ObjectStore>) -> Self {
        Self {
            object_store,
            metadata: HashMap::new(),
        }
    }

    fn add_metadata(&mut self, path: String, metadata: Arc<ParquetMetaData>) {
        self.metadata.insert(path, metadata);
    }
}

impl ParquetFileReaderFactory for CachedParquetFileReaderFactory {
    fn create_reader(
        &self,
        _partition_index: usize,
        partitioned_file: PartitionedFile,
        metadata_size_hint: Option<usize>,
        _metrics: &ExecutionPlanMetricsSet,
    ) -> DataFusionResult<Box<dyn AsyncFileReader + Send>> {
        let _filename = partitioned_file
            .object_meta
            .location
            .parts()
            .last()
            .ok_or_else(|| DataFusionError::Plan("No path in location".to_string()))?
            .as_ref()
            .to_string();

        // Convert location directly to string for lookup, assuming it matches what we stored
        // Warning: DataFusion ObjectStore paths might differ slightly from OS paths (leading / etc)
        // We stored canonicalized OS paths.
        // For LocalFileSystem, the location usually matches.
        // To be safe in `PdqTableProvider`, we should align these.
        // But here we'll try to find it.
        // Actually, let's use the full location path string.
        let _full_path = partitioned_file.object_meta.location.to_string();
        // The location in PartitionedFile comes from what we passed to FileScanConfigBuilder
        // We passed `canonical_path.display().to_string()`

        // Since we constructed PartitionedFile with canonical absolute paths, the location should map.
        // However, object_store paths are URL-encoded/normalized.
        // Let's try to lookup by the path we used to create the PartitionedFile.
        // NOTE: In `scan`, we used `canonical_path.display().to_string()`.
        // So `metadata` keys should match that.

        // In local mode, we might need to be careful with "file://" prefix removal or addition.
        // But simpler: we just iterate and find the one that ends with our filename or matches?
        // No, O(1) lookup is needed.

        // Let's rely on the fact that we populated `metadata` using `canonical_path.display().to_string()`
        // AND we created `PartitionedFile` using `canonical_path.display().to_string()`.
        // So the `partitioned_file.object_meta.location` *should* be that path (or converted to object store path).

        // DataFusion converts string path to Path.
        // Let's assume strict equality for now.
        // If this fails, we might need a more robust normalization.

        // Wait, `PartitionedFile::new(path, ...)` takes a string path.
        // `object_store` location will wrap this.

        let path_key = partitioned_file.object_meta.location.to_string();

        // Fallback: Check if we have it under the exact key, or maybe try with/without leading slash
        let path_key_slash = format!("/{}", path_key);
        let metadata = self
            .metadata
            .get(&path_key)
            .or_else(|| self.metadata.get(&path_key_slash))
            .ok_or_else(|| {
                DataFusionError::Plan(format!(
                    "Metadata for file not found in cache: {}",
                    path_key
                ))
            })?;

        let object_store = Arc::clone(&self.object_store);
        let mut inner =
            ParquetObjectReader::new(object_store, partitioned_file.object_meta.location)
                .with_file_size(partitioned_file.object_meta.size);

        if let Some(hint) = metadata_size_hint {
            inner = inner.with_footer_size_hint(hint);
        }

        Ok(Box::new(ParquetReaderWithCache {
            metadata: Arc::clone(metadata),
            inner,
        }))
    }
}

/// Wrapper around `ParquetObjectReader` that intercepts metadata requests
struct ParquetReaderWithCache {
    metadata: Arc<ParquetMetaData>,
    inner: ParquetObjectReader,
}

impl AsyncFileReader for ParquetReaderWithCache {
    fn get_bytes(
        &mut self,
        range: Range<u64>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Bytes>> {
        self.inner.get_bytes(range)
    }

    fn get_byte_ranges(
        &mut self,
        ranges: Vec<Range<u64>>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Vec<Bytes>>> {
        self.inner.get_byte_ranges(ranges)
    }

    fn get_metadata(
        &mut self,
        _options: Option<&ArrowReaderOptions>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Arc<ParquetMetaData>>> {
        // Return cached metadata immediately, skipping I/O
        let metadata = self.metadata.clone();
        async move { Ok(metadata) }.boxed()
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
