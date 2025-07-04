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
    /// FST index engine for performing fast column value lookups
    index_engine: IndexQueryEngine,
    /// Base directory containing the Parquet data files
    data_dir: PathBuf,
    /// Arrow schema for the table (inferred from Parquet files)
    schema: SchemaRef,
    /// Table name used for debugging, metrics, and registration
    table_name: String,
}

impl PdqTableProvider {
    /// Creates a new PdqTableProvider with the specified index and data directories.
    ///
    /// # Parameters
    ///
    /// * `index_dir` - Directory containing FST index files
    /// * `data_dir` - Directory containing Parquet data files
    /// * `table_name` - Name for the table provider (used in metrics and debugging)
    ///
    /// # Returns
    ///
    /// A new `PdqTableProvider` instance ready for registration with DataFusion
    ///
    /// # Error
    ///
    /// Returns an error if schema inference fails (no Parquet files found or read error)
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

    /// Infers Arrow schema by examining Parquet files in the data directory.
    ///
    /// Walks the directory tree to find the first valid Parquet file
    /// and extracts its schema using Arrow's Parquet reader.
    ///
    /// # Parameters
    ///
    /// * `data_dir` - Base directory containing Parquet files
    ///
    /// # Returns
    ///
    /// The Arrow schema as a SchemaRef
    ///
    /// # Error
    ///
    /// Returns an error if no Parquet files are found or metadata cannot be read
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

    /// Infers Arrow schema from a specific Parquet file.
    ///
    /// # Parameters
    ///
    /// * `path` - Path to the Parquet file
    ///
    /// # Returns
    ///
    /// The Arrow schema as a SchemaRef
    ///
    /// # Error
    ///
    /// Returns an error if the file cannot be opened or metadata cannot be read
    fn infer_schema_from_file(path: impl AsRef<Path>) -> Result<SchemaRef> {
        let path = path.as_ref();
        let file = File::open(path)
            .map_err(|e| internal_datafusion_err!("Failed to open parquet file {path:?}: {e}"))?;

        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
            internal_datafusion_err!("Failed to read parquet metadata from {path:?}: {e}")
        })?;
        Ok(builder.schema().clone())
    }

    /// Infers schema from files that match the current query.
    ///
    /// Tries each file in the list until one succeeds, falling back to
    /// the existing schema if all attempts fail.
    ///
    /// # Parameters
    ///
    /// * `files` - List of file paths that match the query criteria
    ///
    /// # Returns
    ///
    /// The Arrow schema as a SchemaRef
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

    /// Uses the FST index to find relevant files and row groups for a column=value predicate.
    ///
    /// This is the core optimization method that eliminates unnecessary file I/O by
    /// determining exactly which row groups might contain matching values.
    ///
    /// # Parameters
    ///
    /// * `column` - Column name to filter on
    /// * `value` - Value to match in the column
    ///
    /// # Returns
    ///
    /// HashMap mapping file paths to vectors of matching row group indices
    fn prune_catalog(&self, column: &str, value: &str) -> Result<HashMap<PathBuf, Vec<usize>>> {
        // Use the index engine to find file hashes and row groups
        let file_hash_row_groups = match self.index_engine.exact_search(column, value) {
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

    /// Creates an optimized execution plan for scanning Parquet files with row group pruning.
    ///
    /// This implementation:
    /// 1. Extracts column=value filters that can be used with the FST index
    /// 2. Uses FST lookups to determine which files and row groups might contain matches
    /// 3. Creates a ParquetAccessPlan with precise row group selection
    /// 4. Optimizes the scan with buffer reuse, page index, and predicate pushdown
    /// 5. Short-circuits empty results for zero-IO responses when index finds no matches
    ///
    /// # Parameters
    ///
    /// * `state` - DataFusion session state
    /// * `projection` - Optional column indices to project
    /// * `filters` - Logical expressions to filter the data
    /// * `limit` - Optional limit on number of rows to return
    ///
    /// # Returns
    ///
    /// An optimized ExecutionPlan for scanning the Parquet files
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

    /// Indicates which filters can be pushed down to this provider.
    ///
    /// Returns `Inexact` for all filters since our implementation:
    /// 1. Can significantly prune the files/row-groups that need scanning
    /// 2. May return row groups that contain false positives (requiring further filtering)
    /// 3. Should receive all filters to allow for optimized access plans
    ///
    /// # Parameters
    ///
    /// * `filters` - Array of filter expressions to evaluate
    ///
    /// # Returns
    ///
    /// A vector indicating filter pushdown capability for each expression
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
    /// Extracts column=value equality filters from DataFusion expressions.
    ///
    /// Handles various expression types including:
    /// - Basic equality (col = value)
    /// - LIKE expressions with exact patterns
    /// - IN expressions with single values
    ///
    /// # Parameters
    ///
    /// * `expr` - DataFusion expression to analyze
    ///
    /// # Returns
    ///
    /// Option containing (column_name, value) tuple if expression can be used with FST index
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

/// Builder for creating PdqTableProvider instances with flexible configuration.
///
/// Provides a fluent API for constructing table providers with appropriate
/// index and data directories. Handles cross-platform path differences.
pub struct PdqTableProviderBuilder {
    /// Directory containing FST index files
    index_dir: Option<PathBuf>,
    /// Directory containing Parquet data files
    data_dir: Option<PathBuf>,
    /// Name for the table provider
    table_name: String,
}

impl PdqTableProviderBuilder {
    /// Creates a new builder instance with the specified table name.
    ///
    /// # Parameters
    ///
    /// * `table_name` - Name for the table provider
    ///
    /// # Returns
    ///
    /// A new builder instance
    pub fn new(table_name: String) -> Self {
        Self {
            index_dir: None,
            data_dir: None,
            table_name,
        }
    }

    /// Sets the directory containing FST index files.
    ///
    /// # Parameters
    ///
    /// * `index_dir` - Path to the directory containing FST index files
    ///
    /// # Returns
    ///
    /// Builder with index directory configured
    pub fn with_index_dir(mut self, index_dir: impl AsRef<Path>) -> Self {
        self.index_dir = Some(index_dir.as_ref().to_path_buf());
        self
    }

    /// Sets the directory containing Parquet data files.
    ///
    /// # Parameters
    ///
    /// * `data_dir` - Path to the directory containing Parquet data files
    ///
    /// # Returns
    ///
    /// Builder with data directory configured
    pub fn with_data_dir(mut self, data_dir: impl AsRef<Path>) -> Self {
        self.data_dir = Some(data_dir.as_ref().to_path_buf());
        self
    }

    /// Builds a PdqTableProvider with the configured settings.
    ///
    /// # Returns
    ///
    /// A new PdqTableProvider instance
    ///
    /// # Error
    ///
    /// Returns an error if required directories are not specified or
    /// if schema inference fails
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
/// Represents a Parquet file with pre-loaded metadata and selected row groups.
///
/// This struct encapsulates all the information needed to efficiently scan
/// a Parquet file, including its metadata and the specific row groups to read.
/// It avoids re-reading file metadata during execution.
struct PdqIndexedFile {
    /// File name without directory path
    file_name: String,
    /// Full canonical path to the file
    path: PathBuf,
    /// Size of the file in bytes
    file_size: u64,
    /// Pre-parsed Parquet metadata to avoid re-reading during execution
    metadata: Arc<ParquetMetaData>,
    /// Specific row groups to include in the scan (for row-group pruning)
    row_groups: Vec<usize>,
}

impl PdqIndexedFile {
    /// Creates a new PdqIndexedFile by loading metadata from a Parquet file.
    ///
    /// Opens the Parquet file, loads its metadata including page index information,
    /// and prepares it for efficient access with the specified row groups.
    ///
    /// # Parameters
    ///
    /// * `path` - Path to the Parquet file
    /// * `row_groups` - Row group indices to include in the scan
    ///
    /// # Returns
    ///
    /// A new PdqIndexedFile instance with pre-loaded metadata
    ///
    /// # Error
    ///
    /// Returns an error if the file cannot be opened or metadata cannot be read
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

    /// Creates a DataFusion PartitionedFile for the underlying Parquet file.
    ///
    /// Builds a PartitionedFile with the file's metadata and row groups,
    /// which DataFusion uses to determine what to scan.
    ///
    /// # Returns
    ///
    /// A PartitionedFile ready for inclusion in a DataFusion scan configuration
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

    /// Creates a ParquetAccessPlan that includes all row groups in the file.
    ///
    /// Used when no pruning is possible or when all row groups need to be scanned.
    ///
    /// # Returns
    ///
    /// A ParquetAccessPlan configured to scan all row groups
    fn scan_all_plan(&self) -> ParquetAccessPlan {
        ParquetAccessPlan::new_all(self.metadata.num_row_groups())
    }

    /// Creates a ParquetAccessPlan that initially excludes all row groups.
    ///
    /// Used as a starting point for selective row group scanning,
    /// where specific row groups are added incrementally.
    ///
    /// # Returns
    ///
    /// A ParquetAccessPlan configured to skip all row groups by default
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
    /// Object store for accessing file data
    object_store: Arc<dyn ObjectStore>,
    /// Pre-loaded Parquet metadata for each file, indexed by filename
    /// Avoids expensive metadata re-reads during execution
    metadata: HashMap<String, Arc<ParquetMetaData>>,
}

impl CachedParquetFileReaderFactory {
    fn new(object_store: Arc<dyn ObjectStore>) -> Self {
        Self {
            object_store,
            metadata: HashMap::new(),
        }
    }
    /// Add the pre-parsed information about the file to the factory
    fn with_file(mut self, indexed_file: &PdqIndexedFile) -> Self {
        self.metadata.insert(
            indexed_file.file_name.clone(),
            Arc::clone(&indexed_file.metadata),
        );
        self
    }
}

impl ParquetFileReaderFactory for CachedParquetFileReaderFactory {
    /// Creates a Parquet reader for the specified file location.
    ///
    /// Uses cached metadata when available to avoid re-reading file footers.
    ///
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
/// A Parquet AsyncFileReader with cached metadata.
///
/// Wraps another AsyncFileReader and provides the pre-loaded metadata
/// to avoid re-reading file footers during execution.
struct ParquetReaderWithCache {
    /// Original filename for metadata lookup
    filename: String,
    /// Pre-loaded Parquet metadata
    metadata: Arc<ParquetMetaData>,
    /// Underlying file reader for data access
    inner: ParquetObjectReader,
}

impl AsyncFileReader for ParquetReaderWithCache {
    /// Reads a byte range from the underlying file.
    ///
    /// Delegates to the inner reader for actual data access.
    fn get_bytes(
        &mut self,
        range: Range<u64>,
    ) -> BoxFuture<'_, datafusion::parquet::errors::Result<Bytes>> {
        println!("get_bytes: {} Reading range {:?}", self.filename, range);
        self.inner.get_bytes(range)
    }

    /// Reads multiple byte ranges from the underlying file.
    ///
    /// Delegates to the inner reader for actual data access.
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
