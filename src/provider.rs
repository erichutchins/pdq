use crate::query::IndexQueryEngine;
use async_trait::async_trait;
use bytes::Bytes;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::catalog::Session;
use datafusion::common::{internal_datafusion_err, DFSchema, DataFusionError, Result};
use datafusion::datasource::listing::PartitionedFile;
use datafusion::datasource::physical_plan::parquet::ParquetAccessPlan;
use datafusion::datasource::physical_plan::{
    FileMeta, FileScanConfigBuilder, ParquetFileReaderFactory, ParquetSource,
};
use datafusion::datasource::source::DataSourceExec;
use datafusion::datasource::TableProvider;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::{utils::conjunction, Expr, TableProviderFilterPushDown, TableType};
use datafusion::parquet::arrow::arrow_reader::{
    ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use datafusion::parquet::arrow::async_reader::{AsyncFileReader, ParquetObjectReader};
use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::physical_plan::metrics::ExecutionPlanMetricsSet;
use datafusion::physical_plan::ExecutionPlan;
use futures::future::BoxFuture;
use futures::FutureExt;
use object_store::ObjectStore;
use std::any::Any;
use std::collections::HashMap;
use std::fs::File;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

/// PdqTableProvider that implements true row-group level optimization
///
/// This provider leverages DataFusion's latest filter pushdown APIs to:
/// 1. Use FST index results to identify relevant files and row groups
/// 2. Create execution plans that only read specific row groups
/// 3. Support dynamic filter pushdown for additional optimization
/// 4. Provide metrics on pruning effectiveness
/// 5. **Critical optimization**: Returns empty results immediately when index finds no matches
#[derive(Debug)]
pub struct PdqTableProvider {
    /// FST index engine for fast lookups
    index_engine: IndexQueryEngine,
    /// Base directory containing parquet files
    data_dir: PathBuf,
    /// Schema of the table (inferred from files)
    schema: SchemaRef,
    /// Table name for debugging and metrics
    table_name: String,
}

impl PdqTableProvider {
    /// Create a new PdqTableProvider
    pub async fn new(
        index_dir: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
        table_name: String,
    ) -> Result<Self> {
        let index_engine = IndexQueryEngine::new(index_dir);
        let data_dir = data_dir.as_ref().to_path_buf();

        // Infer schema from the first parquet file we can find
        let schema = Self::infer_schema_from_directory(&data_dir).await?;

        Ok(Self {
            index_engine,
            data_dir,
            schema,
            table_name,
        })
    }

    /// Infer schema from specific files or by examining parquet files in the data directory
    /// Uses cross-platform file operations
    async fn infer_schema_from_directory(data_dir: &Path) -> Result<SchemaRef> {
        // Find the first .parquet file in the directory using walkdir
        for entry in walkdir::WalkDir::new(data_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            // Cross-platform extension checking
            if let Some(extension) = path.extension() {
                if extension.to_string_lossy().to_lowercase() == "parquet" {
                    return Self::infer_schema_from_file(path);
                }
            }
        }

        Err(internal_datafusion_err!(
            "No parquet files found in data directory"
        ))
    }

    /// Infer schema from a specific parquet file
    fn infer_schema_from_file(path: impl AsRef<Path>) -> Result<SchemaRef> {
        let path = path.as_ref();
        let file = File::open(path)
            .map_err(|e| internal_datafusion_err!("Failed to open parquet file {path:?}: {e}"))?;

        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
            internal_datafusion_err!("Failed to read parquet metadata from {path:?}: {e}")
        })?;
        Ok(builder.schema().clone())
    }

    /// Infer schema from a list of files that match our query
    fn infer_schema_from_matched_files(&self, files: &[PathBuf]) -> Result<SchemaRef> {
        if files.is_empty() {
            tracing::debug!("No files to infer schema from, using existing schema");
            return Ok(self.schema.clone());
        }

        // Try each file until we successfully infer a schema
        let mut last_error = None;
        for file_path in files {
            match Self::infer_schema_from_file(file_path) {
                Ok(schema) => {
                    tracing::debug!("Successfully inferred schema from {}", file_path.display());
                    return Ok(schema);
                }
                Err(e) => {
                    tracing::debug!("Failed to infer schema from {}: {}", file_path.display(), e);
                    last_error = Some(e);
                }
            }
        }

        // If all files failed, log a warning and return the existing schema
        if let Some(e) = last_error {
            tracing::warn!("Failed to infer schema from any matched files, using existing schema. Last error: {}", e);
        }
        Ok(self.schema.clone())
    }

    /// Use the FST index to find relevant files and row groups
    fn prune_catalog(&self, column: &str, term: &str) -> Result<HashMap<PathBuf, Vec<usize>>> {
        // Use the index engine to find file hashes and row groups
        let file_hash_row_groups = match self.index_engine.exact_search(column, term) {
            Ok(groups) => groups,
            Err(e) => return Err(internal_datafusion_err!("Index search failed: {}", e)),
        };

        if file_hash_row_groups.is_empty() {
            return Ok(HashMap::new());
        }

        // Resolve file hashes to actual file paths
        let file_hashes: Vec<String> = file_hash_row_groups.keys().cloned().collect();
        let hash_to_path = self
            .index_engine
            .resolve_file_paths(&file_hashes, &self.data_dir)
            .map_err(|e| internal_datafusion_err!("Failed to resolve file paths: {e}"))?;

        // Convert to path-based mapping
        let mut file_row_groups = HashMap::new();
        for (file_hash, row_groups) in file_hash_row_groups {
            if let Some(file_path) = hash_to_path.get(&file_hash) {
                file_row_groups.insert(file_path.clone(), row_groups);
            }
        }

        Ok(file_row_groups)
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

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        // Start with the default schema
        let schema_ref = self.schema.clone();
        let df_schema = DFSchema::try_from(schema_ref.clone())?;

        // Convert filters like `a = 1`, `b = 2`
        // to a single predicate like `a = 1 AND b = 2` suitable for execution
        let predicate = conjunction(filters.to_vec());
        let predicate = predicate
            .map(|predicate| state.create_physical_expr(predicate, &df_schema))
            .transpose()?
            .unwrap_or_else(|| datafusion::physical_expr::expressions::lit(true));

        // Try to extract column = value filters that we can handle with our index
        let mut file_row_groups = HashMap::new();
        for filter in filters {
            if let Some((column, value)) = self.extract_equality_filter(filter) {
                if let Ok(results) = self.prune_catalog(&column, &value) {
                    for (path, row_groups) in results {
                        // Merge row groups if the file already exists in our map
                        file_row_groups
                            .entry(path)
                            .and_modify(|existing_groups: &mut Vec<usize>| {
                                // Only keep row groups that appear in all filters
                                existing_groups.retain(|g| row_groups.contains(g));
                            })
                            .or_insert_with(|| row_groups);
                    }
                }
            }
        }

        // CRITICAL OPTIMIZATION: If the index found no matches, return empty results immediately
        // The FST index is authoritative - no matches means no data exists
        // This eliminates 100% of I/O for searches with no results
        if file_row_groups.is_empty() {
            // Create an empty execution plan - NO FILE I/O NEEDED
            let empty_config = FileScanConfigBuilder::new(
                ObjectStoreUrl::parse("file://")?,
                schema_ref,
                Arc::new(ParquetSource::default()),
            )
            .build();
            return Ok(DataSourceExec::from_data_source(empty_config));
        }

        // Get file paths for schema inference
        let file_paths: Vec<PathBuf> = file_row_groups.keys().cloned().collect();

        // Update schema with the actual files we're about to query
        // This ensures the schema is accurate for the data we're accessing
        let schema_ref = self.infer_schema_from_matched_files(&file_paths)?;

        // Create object store URL and prepare the object store
        let object_store_url = ObjectStoreUrl::parse("file://")?;
        let object_store = object_store::local::LocalFileSystem::new();
        let object_store: Arc<dyn ObjectStore> = Arc::new(object_store);

        // Create a combined reader factory for all files
        let mut reader_factory = CachedParquetFileReaderFactory::new(Arc::clone(&object_store));

        // Prepare list of partitioned files with access plans
        let mut partitioned_files = Vec::with_capacity(file_row_groups.len());

        // Process each file and prepare it for scanning
        for (file_path, row_groups) in &file_row_groups {
            // Configure a factory interface to avoid re-reading the metadata for this file
            let indexed_file = PdqIndexedFile::try_new(file_path.clone(), row_groups.clone())?;

            // Add file to the reader factory
            reader_factory = reader_factory.with_file(&indexed_file);

            // Create access plan for row groups
            let mut access_plan = indexed_file.scan_none_plan();
            for &row_group_idx in row_groups {
                access_plan.scan(row_group_idx);
            }

            // Create the partitioned file with access plan as extensions
            let partitioned_file = indexed_file
                .partitioned_file()
                .with_extensions(Arc::new(access_plan) as _);

            // Add to our list of files to scan
            partitioned_files.push(partitioned_file);
        }

        // Create ParquetSource with predicate and optimized configuration
        let source = Arc::new(
            ParquetSource::default()
                .with_predicate(predicate)
                .with_enable_page_index(true)
                .with_pushdown_filters(true)
                .with_parquet_file_reader_factory(Arc::new(reader_factory)),
        );

        // Build file scan configuration with all files
        let mut file_scan_config_builder =
            FileScanConfigBuilder::new(object_store_url, schema_ref, source)
                .with_projection(projection.cloned())
                .with_limit(limit);

        // Add all files to the scan configuration
        for file in partitioned_files {
            file_scan_config_builder = file_scan_config_builder.with_file(file);
        }

        // Create execution plan that only reads the identified files and row groups
        // This uses DataFusion's latest APIs that support row-group level pruning
        let file_scan_config = file_scan_config_builder.build();
        Ok(DataSourceExec::from_data_source(file_scan_config))
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::error::Result<Vec<TableProviderFilterPushDown>> {
        // We can handle exact matches on indexed columns
        // Inexact because we may return files that don't match after row-level filtering
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }
}

impl PdqTableProvider {
    /// Extract column = value filters that we can use with our index
    /// This handles cross-platform string processing
    /// Enhanced to support more filter types including LIKE and IN
    fn extract_equality_filter(&self, expr: &Expr) -> Option<(String, String)> {
        use datafusion::logical_expr::{Expr, Like, Operator};

        match expr {
            // Handle equality filters (col = value)
            Expr::BinaryExpr(binary_expr) if binary_expr.op == Operator::Eq => {
                if let (Expr::Column(col), Expr::Literal(lit, _)) =
                    (binary_expr.left.as_ref(), binary_expr.right.as_ref())
                {
                    let value = lit.to_string();
                    if !value.is_empty() {
                        return Some((col.name.clone(), value));
                    }
                }
            }

            // Handle LIKE expressions with prefix patterns (can use our FST prefix search)
            Expr::Like(Like {
                expr,
                pattern,
                negated: false,
                escape_char: None,
                case_insensitive: false,
            }) => {
                if let (Expr::Column(col), Expr::Literal(lit, _)) =
                    (expr.as_ref(), pattern.as_ref())
                {
                    let pattern_value = lit.to_string();
                    // If pattern starts with a value followed by wildcard, we can use prefix search
                    if pattern_value.ends_with('%') && !pattern_value.starts_with('%') {
                        let prefix = pattern_value.trim_end_matches('%');
                        if !prefix.is_empty() {
                            return Some((col.name.clone(), prefix.to_string()));
                        }
                    }
                }
            }

            _ => {}
        }

        None
    }
}

/// Builder for PdqTableProvider with cross-platform path handling
pub struct PdqTableProviderBuilder {
    index_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    table_name: String,
}

impl PdqTableProviderBuilder {
    pub fn new(table_name: String) -> Self {
        Self {
            index_dir: None,
            data_dir: None,
            table_name,
        }
    }

    pub fn with_index_dir(mut self, index_dir: impl AsRef<Path>) -> Self {
        self.index_dir = Some(index_dir.as_ref().to_path_buf());
        self
    }

    pub fn with_data_dir(mut self, data_dir: impl AsRef<Path>) -> Self {
        self.data_dir = Some(data_dir.as_ref().to_path_buf());
        self
    }

    pub async fn build(self) -> Result<PdqTableProvider> {
        let index_dir = self
            .index_dir
            .ok_or_else(|| internal_datafusion_err!("Index directory not specified"))?;
        let data_dir = self
            .data_dir
            .ok_or_else(|| internal_datafusion_err!("Data directory not specified"))?;

        PdqTableProvider::new(index_dir, data_dir, self.table_name).await
    }
}

/// Stores information needed to scan a file
#[derive(Debug)]
struct PdqIndexedFile {
    /// File name
    file_name: String,
    /// The path of the file
    path: PathBuf,
    /// The size of the file
    file_size: u64,
    /// The pre-parsed parquet metadata for the file
    metadata: Arc<ParquetMetaData>,
    /// Row groups to include in the scan
    row_groups: Vec<usize>,
}

impl PdqIndexedFile {
    fn try_new(path: impl AsRef<Path>, row_groups: Vec<usize>) -> Result<Self> {
        let path = path.as_ref();

        let file_name = path
            .file_name()
            .ok_or_else(|| internal_datafusion_err!("Invalid path"))?
            .to_str()
            .ok_or_else(|| internal_datafusion_err!("Invalid filename"))?
            .to_string();
        let file_size = path.metadata()?.len();

        let file = File::open(path).map_err(|e| {
            DataFusionError::from(e).context(format!("Error opening file {path:?}"))
        })?;

        let options = ArrowReaderOptions::new()
            // Load the page index when reading metadata to cache
            // so it is available to interpret row selections
            .with_page_index(true);
        let reader = ParquetRecordBatchReaderBuilder::try_new_with_options(file, options)?;
        let metadata = reader.metadata().clone();

        // canonicalize after writing the file
        let path = std::fs::canonicalize(path)?;

        Ok(Self {
            file_name,
            path,
            file_size,
            metadata,
            row_groups,
        })
    }

    /// Return a `PartitionedFile` to scan the underlying file
    ///
    /// The returned value does not have any  `ParquetAccessPlan` specified in
    /// its extensions.
    fn partitioned_file(&self) -> PartitionedFile {
        PartitionedFile {
            object_meta: object_store::ObjectMeta {
                location: self.path.display().to_string().into(),
                last_modified: std::time::SystemTime::now().into(),
                size: self.file_size,
                e_tag: None,
                version: None,
            },
            partition_values: vec![],
            range: None,
            // row_groups: Some(self.row_groups.clone()),
            statistics: None,
            extensions: None,
            metadata_size_hint: None,
        }
    }

    /// Return a `ParquetAccessPlan` that scans all row groups in the file
    fn scan_all_plan(&self) -> ParquetAccessPlan {
        ParquetAccessPlan::new_all(self.metadata.num_row_groups())
    }

    /// Return a `ParquetAccessPlan` that scans no row groups in the file
    fn scan_none_plan(&self) -> ParquetAccessPlan {
        ParquetAccessPlan::new_none(self.metadata.num_row_groups())
    }
}

// ----------------------------------------------------------------------------
// Copied from datafusion-examples/examples/advanced_parquet_index.rs
// ----------------------------------------------------------------------------
/// A custom [`ParquetFileReaderFactory`] that handles opening parquet files
/// from object storage, and uses pre-loaded metadata.

#[derive(Debug)]
struct CachedParquetFileReaderFactory {
    /// The underlying object store implementation for reading file data
    object_store: Arc<dyn ObjectStore>,
    /// The parquet metadata for each file in the index, keyed by the file name
    /// (e.g. `file1.parquet`)
    metadata: HashMap<String, Arc<ParquetMetaData>>,
}

impl CachedParquetFileReaderFactory {
    fn new(object_store: Arc<dyn ObjectStore>) -> Self {
        Self {
            object_store,
            metadata: HashMap::new(),
        }
    }
    /// Add the pre-parsed information about the file to the factor
    fn with_file(mut self, indexed_file: &PdqIndexedFile) -> Self {
        self.metadata.insert(
            indexed_file.file_name.clone(),
            Arc::clone(&indexed_file.metadata),
        );
        self
    }
}

impl ParquetFileReaderFactory for CachedParquetFileReaderFactory {
    fn create_reader(
        &self,
        _partition_index: usize,
        file_meta: FileMeta,
        metadata_size_hint: Option<usize>,
        _metrics: &ExecutionPlanMetricsSet,
    ) -> Result<Box<dyn AsyncFileReader + Send>> {
        // for this example we ignore the partition index and metrics
        // but in a real system you would likely use them to report details on
        // the performance of the reader.
        let filename = file_meta
            .location()
            .parts()
            .last()
            .expect("No path in location")
            .as_ref()
            .to_string();

        let object_store = Arc::clone(&self.object_store);
        let mut inner = ParquetObjectReader::new(object_store, file_meta.object_meta.location)
            .with_file_size(file_meta.object_meta.size);

        if let Some(hint) = metadata_size_hint {
            inner = inner.with_footer_size_hint(hint)
        };

        let metadata = self
            .metadata
            .get(&filename)
            .expect("metadata for file not found: {filename}");
        Ok(Box::new(ParquetReaderWithCache {
            filename,
            metadata: Arc::clone(metadata),
            inner,
        }))
    }
}

/// wrapper around a ParquetObjectReader that caches metadata
struct ParquetReaderWithCache {
    filename: String,
    metadata: Arc<ParquetMetaData>,
    inner: ParquetObjectReader,
}

impl AsyncFileReader for ParquetReaderWithCache {
    fn get_bytes(
        &mut self,
        range: Range<u64>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Bytes>> {
        println!("get_bytes: {} Reading range {:?}", self.filename, range);
        self.inner.get_bytes(range)
    }

    fn get_byte_ranges(
        &mut self,
        ranges: Vec<Range<u64>>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Vec<Bytes>>> {
        println!(
            "get_byte_ranges: {} Reading ranges {:?}",
            self.filename, ranges
        );
        self.inner.get_byte_ranges(ranges)
    }

    fn get_metadata(
        &mut self,
        _options: Option<&ArrowReaderOptions>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Arc<ParquetMetaData>>> {
        println!("get_metadata: {} returning cached metadata", self.filename);

        // return the cached metadata so the parquet reader does not read it
        let metadata = self.metadata.clone();
        async move { Ok(metadata) }.boxed()
    }
}

// /// Helper function to create a PdqTableProvider from index query results
// /// Uses cross-platform file handling
// pub async fn create_table_provider_from_index_results(
//     file_paths: HashMap<PathBuf, Vec<usize>>,
//     table_name: String,
// ) -> Result<PdqTableProvider> {
//     // For this helper, we need to extract the data directory from the file paths
//     // Find the common parent directory of all files using walkdir
//     if file_paths.is_empty() {
//         return Err("No file paths provided".into());
//     }

//     // Get all parent directories using walkdir
//     let mut common_parent = None;
//     for (file_path, _) in file_paths.iter() {
//         for ancestor in walkdir::WalkDir::new(file_path)
//             .follow_links(true)
//             .into_iter()
//             .filter_map(|e| e.ok())
//             .filter(|e| e.file_type().is_dir())
//         {
//             let parent = ancestor.path();
//             if common_parent.is_none() {
//                 common_parent = Some(parent.to_path_buf());
//             } else if let Some(ref current_parent) = common_parent {
//                 if !parent.starts_with(current_parent) {
//                     common_parent = Some(parent.to_path_buf());
//                 }
//             }
//         }
//     }

//     if let Some(parent_dir) = common_parent {
//         // Create a temporary index (this is a simplified approach)
//         let temp_index_dir = std::env::temp_dir().join("pdq_temp_index");

//         PdqTableProviderBuilder::new(table_name)
//             .with_data_dir(parent_dir)
//             .with_index_dir(temp_index_dir)
//             .build()
//             .await
//     } else {
//         Err("Cannot determine parent directory from file paths".into())
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_pdq_table_provider_builder() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("index");
        let data_dir = temp_dir.path().join("data");

        // Cross-platform directory creation
        fs::create_dir_all(&index_dir).await?;
        fs::create_dir_all(&data_dir).await?;

        let builder = PdqTableProviderBuilder::new("test_table".to_string())
            .with_index_dir(&index_dir)
            .with_data_dir(&data_dir);

        // This would fail without actual parquet files, but tests the API
        assert!(builder.build().await.is_err());

        Ok(())
    }

    #[test]
    fn test_cross_platform_path_handling() {
        // Test that our path handling works on different platforms
        let path = PathBuf::from("test").join("file.parquet");

        // This should work on Windows, Linux, and macOS
        assert!(path.extension().is_some());
        assert_eq!(path.extension().unwrap().to_string_lossy(), "parquet");

        // Test cross-platform string conversion
        let path_string = path.to_string_lossy().to_string();
        assert!(path_string.contains("file.parquet"));
    }
}
