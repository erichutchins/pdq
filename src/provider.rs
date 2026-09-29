use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::arrow::datatypes::{Schema, SchemaRef};
use datafusion::catalog::Session;
use datafusion::common::{DFSchema, DataFusionError, Result as DataFusionResult};
use datafusion::datasource::TableProvider;
use datafusion::datasource::listing::PartitionedFile;
use datafusion::datasource::physical_plan::parquet::{
    CachedParquetFileReaderFactory, ParquetAccessPlan,
};
use datafusion::datasource::physical_plan::{FileScanConfigBuilder, ParquetSource};
use datafusion::datasource::source::DataSourceExec;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::utils::conjunction;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::physical_plan::ExecutionPlan;
use futures::future::join_all;
use object_store::ObjectStoreExt;
use object_store::path::Path as ObjectStorePath;
use rayon::prelude::*;

use crate::calculate_file_hash;
use crate::query::IndexQueryEngine;

type PrunedRowGroups = HashMap<String, Vec<usize>>;
type HashToPath = HashMap<String, PathBuf>;

/// Default footer size hint (bytes) read from the tail of a Parquet file on the
/// first (cold) metadata fetch, so the footer + Thrift metadata come back in one
/// read instead of two. Over-hinting only over-reads once per file (then the
/// result is cached); under-hinting costs a second read. 512 KiB comfortably
/// covers the footer of the multi-row-group, multi-column files PDQ targets.
const DEFAULT_METADATA_SIZE_HINT: usize = 512 * 1024;

/// PDQ TableProvider.
///
/// Resolves equality filters against FST indexes to prune to specific Parquet
/// row groups, then hands DataFusion a standard [`ParquetSource`] scan with a
/// [`ParquetAccessPlan`] attached per file via [`PartitionedFile`] extensions.
/// DataFusion's own Parquet opener honors that access plan, so projection,
/// predicate pruning, and page-index pruning all work as usual. The row group
/// count needed to build the access plan is read from the FST index metadata,
/// so planning performs no Parquet footer I/O.
///
/// Parsed Parquet metadata (footer + page index) is cached in the session's
/// runtime [`FileMetadataCache`] via DataFusion's
/// [`CachedParquetFileReaderFactory`], so a long-lived `SessionContext` parses
/// each matched file's footer at most once until the file changes.
///
/// [`FileMetadataCache`]: datafusion::execution::cache::cache_manager::FileMetadataCache
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
    /// Footer size hint passed to the Parquet reader on the first metadata read.
    metadata_size_hint: Option<usize>,
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
            metadata_size_hint: Some(DEFAULT_METADATA_SIZE_HINT),
        })
    }

    /// Set the footer size hint (bytes) for the first metadata read of each file.
    pub fn set_metadata_size_hint(&mut self, hint: Option<usize>) {
        self.metadata_size_hint = hint;
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

    /// Total row group count for a matched file, preferring the value recorded
    /// in the FST index (zero I/O). Falls back to a single footer read for
    /// legacy indexes that predate row-group-count recording.
    fn row_group_count(&self, file_hash: &str, file_path: &Path) -> DataFusionResult<usize> {
        if let Some(count) = self.index_engine.num_row_groups(file_hash) {
            return Ok(count);
        }
        let file = std::fs::File::open(file_path)
            .map_err(|e| DataFusionError::Plan(format!("Failed to open {file_path:?}: {e}")))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
            DataFusionError::Plan(format!("Failed to read footer {file_path:?}: {e}"))
        })?;
        Ok(builder.metadata().num_row_groups())
    }
}

#[async_trait]
impl TableProvider for PdqTableProvider {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    /// Scan implementation: FST-pruned row groups handed to a stock ParquetSource
    /// via a per-file ParquetAccessPlan stored in PartitionedFile extensions.
    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> DataFusionResult<Arc<dyn ExecutionPlan>> {
        // FST lookups only — zero Parquet I/O at planning time.
        let (file_row_groups, hash_to_path) = self
            .prune_with_fst_index(filters)
            .map_err(|e| DataFusionError::Plan(format!("FST pruning failed: {e}")))?;

        let object_store_url = ObjectStoreUrl::parse("file://")?;

        // No matches → empty plan immediately, no I/O.
        if file_row_groups.is_empty() {
            let source = Arc::new(ParquetSource::new(self.schema.clone()));
            let config = FileScanConfigBuilder::new(object_store_url, source)
                .with_projection_indices(projection.cloned())?
                .with_limit(limit)
                .build();
            return Ok(DataSourceExec::from_data_source(config));
        }

        // Build a physical predicate so the stock ParquetSource can perform its own
        // statistics/page pruning on top of our row-group selection, and so its
        // expression adapter can rebase columns when a projection is pushed down.
        let df_schema = DFSchema::try_from(self.schema.clone())?;
        let predicate = conjunction(filters.to_vec())
            .map(|p| state.create_physical_expr(p, &df_schema))
            .transpose()?
            .unwrap_or_else(|| datafusion::physical_expr::expressions::lit(true));

        // Serve Parquet metadata from the session's runtime metadata cache so each
        // matched file's footer (and page index) is parsed at most once per
        // SessionContext, and apply the footer size hint so the first (cold) read
        // fetches it in one shot.
        let runtime = state.runtime_env();
        let object_store = runtime.object_store(&object_store_url)?;
        let reader_factory = Arc::new(CachedParquetFileReaderFactory::new(
            Arc::clone(&object_store),
            runtime.cache_manager.get_file_metadata_cache(),
        ));
        let mut parquet_source = ParquetSource::new(self.schema.clone())
            .with_predicate(predicate)
            .with_parquet_file_reader_factory(reader_factory)
            // Apply the predicate as a row filter *during* decode (late
            // materialization). On PDQ's unsorted corpus zonemaps can't prune, so
            // without this the whole matched row group is decoded and a FilterExec
            // above the scan drops all but the matching rows. We still report the
            // filters as Inexact, so that FilterExec stays as a correctness net.
            .with_pushdown_filters(true);
        if let Some(hint) = self.metadata_size_hint {
            parquet_source = parquet_source.with_metadata_size_hint(hint);
        }
        let source = Arc::new(parquet_source);
        let mut builder = FileScanConfigBuilder::new(object_store_url, source)
            .with_projection_indices(projection.cloned())?
            .with_limit(limit);

        // For each matched file, build a ParquetAccessPlan that scans only the
        // matched row groups. DataFusion's Parquet opener reads the footer at
        // execution time and honors the plan.
        let mut planned_files = Vec::with_capacity(file_row_groups.len());
        for (file_hash, row_groups) in &file_row_groups {
            if row_groups.is_empty() {
                continue;
            }
            let Some(file_path) = hash_to_path.get(file_hash) else {
                continue;
            };

            // Skip files removed since indexing (canonicalize fails). Queries
            // tolerate stale indexes whose Parquet files are gone; run `index
            // --prune` to drop the orphan indexes.
            //
            // dunce::canonicalize (not std::fs::canonicalize) because on Windows
            // std's version prefixes paths with the `\\?\` verbatim form, which
            // DataFusion's file:// URL handling can't round-trip.
            let Ok(canonical_path) = dunce::canonicalize(file_path) else {
                continue;
            };

            let total_rgs = self.row_group_count(file_hash, &canonical_path)?;
            let mut access_plan = ParquetAccessPlan::new_none(total_rgs);
            for &rg in row_groups {
                if rg < total_rgs {
                    access_plan.scan(rg);
                }
            }
            // All matched row groups were stale relative to the current file — skip it.
            if access_plan.row_group_indexes().is_empty() {
                continue;
            }

            // object_store's `file://` scheme expects `/`-delimited, percent-decoded
            // paths, not the OS-native (backslash-on-Windows) form `display()` gives
            // us — otherwise the backslashes get percent-encoded as literal filename
            // characters and then re-encoded again downstream, mangling the URL.
            let Ok(object_store_path) = ObjectStorePath::from_absolute_path(&canonical_path) else {
                continue;
            };
            planned_files.push((object_store_path, access_plan));
        }

        // Stat every planned file through the object store so each PartitionedFile
        // carries the real size *and* last-modified time. The metadata cache
        // validates entries against both, so a file rewritten in place is re-read
        // rather than served from a stale footer.
        let object_metas = join_all(
            planned_files
                .iter()
                .map(|(path, _)| object_store.head(path)),
        )
        .await;
        for ((_, access_plan), object_meta) in planned_files.into_iter().zip(object_metas) {
            let object_meta = match object_meta {
                Ok(meta) => meta,
                // Removed between canonicalize and stat — skip like above.
                Err(object_store::Error::NotFound { .. }) => continue,
                Err(e) => return Err(e.into()),
            };
            let partitioned_file =
                PartitionedFile::new_from_meta(object_meta).with_extension(access_plan);
            builder = builder.with_file(partitioned_file);
        }

        Ok(DataSourceExec::from_data_source(builder.build()))
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
            metadata_size_hint: Some(DEFAULT_METADATA_SIZE_HINT),
        };

        let filter = col("test_col").eq(lit("test_value"));
        let result = provider.extract_equality_filter(&filter);

        assert!(result.is_some());
        let (column, value) = result.unwrap();
        assert_eq!(column, "test_col");
        assert_eq!(value, "test_value");
    }
}
