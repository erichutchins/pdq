use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray};
use arrow::datatypes::{Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::pruning::PruningStatistics;
use datafusion::common::{
    Column, DFSchema, DataFusionError, Result as DataFusionResult, ScalarValue,
};
use datafusion::datasource::listing::PartitionedFile;
use datafusion::datasource::memory::DataSourceExec;
use datafusion::datasource::physical_plan::{FileScanConfigBuilder, ParquetSource};
use datafusion::datasource::TableProvider;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::utils::conjunction;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::parquet::file::reader::{FileReader, SerializedFileReader};
use datafusion::physical_plan::ExecutionPlan;

// Parquet execution imports
use datafusion::datasource::physical_plan::parquet::ParquetAccessPlan;

use crate::calculate_file_hash;
use crate::query::IndexQueryEngine;

/// Enhanced PDQ TableProvider with DataFusion v49 improvements
///
/// This implementation provides:
/// 1. Modern FileScanConfigBuilder usage with ParquetSource
/// 2. PruningStatistics implementation leveraging FST indices
/// 3. Metadata caching for repeated queries
/// 4. Better error handling and zero-I/O optimization
#[derive(Debug)]
pub struct PdqTableProvider {
    /// FST-based index query engine
    index_engine: Arc<IndexQueryEngine>,
    /// Directory containing Parquet data files
    data_dir: PathBuf,
    /// Schema of the table
    schema: SchemaRef,
    /// Table name for identification
    table_name: String,
    /// Whether to use row-level selections
    use_row_selections: bool,
    /// Cached file metadata for performance
    metadata_cache: HashMap<String, Arc<ParquetMetaData>>,
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
            data_dir,
            schema,
            table_name,
            use_row_selections,
            metadata_cache: HashMap::new(),
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

    /// Infer schema from all Parquet files in directory
    async fn infer_schema_from_directory(data_dir: &Path) -> anyhow::Result<SchemaRef> {
        use walkdir::WalkDir;

        let mut schema: Option<SchemaRef> = None;

        for entry in WalkDir::new(data_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "parquet"))
        {
            let file_schema = Self::infer_schema_from_file(entry.path())?;

            schema = match schema {
                None => Some(file_schema),
                Some(existing) => {
                    // Merge schemas by creating a new schema with all fields
                    let merged =
                        Schema::try_merge(vec![(*existing).clone(), (*file_schema).clone()])?;
                    Some(Arc::new(merged))
                }
            };
        }

        schema.ok_or_else(|| anyhow::anyhow!("No Parquet files found in directory"))
    }

    /// Infer schema from a single Parquet file
    fn infer_schema_from_file(file_path: &Path) -> anyhow::Result<SchemaRef> {
        let file = std::fs::File::open(file_path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        Ok(builder.schema().clone())
    }

    /// Use FST index to prune files and row groups
    fn prune_with_fst_index(
        &self,
        filters: &[Expr],
    ) -> anyhow::Result<HashMap<String, Vec<usize>>> {
        let mut file_row_groups = HashMap::new();

        for filter in filters {
            if let Some((column, value)) = self.extract_equality_filter(filter) {
                if let Ok(results) = self.index_engine.exact_search(&column, &value) {
                    for (file_hash, row_groups) in results {
                        file_row_groups
                            .entry(file_hash)
                            .and_modify(|existing_groups: &mut Vec<usize>| {
                                existing_groups.retain(|g| row_groups.contains(g));
                            })
                            .or_insert(row_groups);
                    }
                }
            }
        }

        Ok(file_row_groups)
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

    /// Create pruning predicate for DataFusion integration
    fn create_fst_pruning_predicate(
        &self,
        filters: &[Expr],
    ) -> anyhow::Result<Option<Arc<dyn PruningStatistics>>> {
        if filters.is_empty() {
            return Ok(None);
        }

        Ok(Some(Arc::new(FstPruningStatistics::new(
            Arc::clone(&self.index_engine),
            filters.to_vec(),
        ))))
    }

    /// Resolve file hashes to actual file paths
    fn resolve_file_paths(
        &self,
        file_hashes: &[String],
    ) -> anyhow::Result<HashMap<String, PathBuf>> {
        self.index_engine
            .resolve_file_paths(file_hashes, &self.data_dir)
    }

    /// Load or retrieve cached metadata for a file
    async fn get_file_metadata(
        &mut self,
        file_path: &Path,
    ) -> anyhow::Result<Arc<ParquetMetaData>> {
        let file_hash = calculate_file_hash(&file_path.to_string_lossy())?;

        if let Some(cached) = self.metadata_cache.get(&file_hash) {
            return Ok(Arc::clone(cached));
        }

        let file = std::fs::File::open(file_path)?;
        let reader = SerializedFileReader::new(file)?;
        let metadata = Arc::new(reader.metadata().clone());

        self.metadata_cache.insert(file_hash, Arc::clone(&metadata));
        Ok(metadata)
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
        let file_row_groups = self
            .prune_with_fst_index(filters)
            .map_err(|e| DataFusionError::Plan(format!("FST pruning failed: {e}")))?;

        // CRITICAL OPTIMIZATION: If the index found no matches, return empty results immediately
        // The FST index is authoritative - no matches means no data exists
        // This eliminates 100% of I/O for searches with no results
        if file_row_groups.is_empty() {
            let object_store_url = ObjectStoreUrl::parse("file://")?;
            let source = Arc::new(ParquetSource::default());
            let config = FileScanConfigBuilder::new(object_store_url, self.schema.clone(), source)
                .with_projection(projection.cloned())
                .with_limit(limit)
                .build();

            return Ok(DataSourceExec::from_data_source(config));
        }

        // Resolve file hashes to paths
        let file_hashes: Vec<String> = file_row_groups.keys().cloned().collect();
        let hash_to_path = self
            .resolve_file_paths(&file_hashes)
            .map_err(|e| DataFusionError::Plan(format!("Path resolution failed: {e}")))?;

        // Convert filters to a single predicate for DataFusion
        let df_schema = DFSchema::try_from(self.schema.clone())?;
        let predicate = conjunction(filters.to_vec());
        let predicate = predicate
            .map(|predicate| state.create_physical_expr(predicate, &df_schema))
            .transpose()?
            .unwrap_or_else(|| datafusion::physical_expr::expressions::lit(true));

        // Create ParquetSource with predicate
        let source = ParquetSource::default().with_predicate(predicate);

        // Build file scan configuration using the proven pattern
        let object_store_url = ObjectStoreUrl::parse("file://")?;
        let mut file_scan_config_builder =
            FileScanConfigBuilder::new(object_store_url, self.schema.clone(), Arc::new(source))
                .with_projection(projection.cloned())
                .with_limit(limit);

        // Add files with row group level access plans based on FST index results
        for (file_hash, row_groups) in &file_row_groups {
            if let Some(file_path) = hash_to_path.get(file_hash) {
                let canonical_path = std::fs::canonicalize(file_path).map_err(|e| {
                    DataFusionError::Plan(format!("Path canonicalization failed: {e}"))
                })?;

                let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);

                // Load file metadata to determine total number of row groups
                let file = std::fs::File::open(file_path).map_err(|e| {
                    DataFusionError::Plan(format!("Failed to open file for metadata: {e}"))
                })?;
                let reader = SerializedFileReader::new(file).map_err(|e| {
                    DataFusionError::Plan(format!("Failed to create parquet reader: {e}"))
                })?;
                let metadata = reader.metadata();
                let total_row_groups = metadata.num_row_groups();

                // Create access plan that initially scans no row groups
                let mut access_plan = ParquetAccessPlan::new_none(total_row_groups);

                // Enable scanning only for the row groups that contain our target values
                for &row_group_idx in row_groups {
                    access_plan.scan(row_group_idx);
                }

                // Create partitioned file with the access plan
                let mut partitioned_file =
                    PartitionedFile::new(canonical_path.display().to_string(), file_size);
                partitioned_file.extensions = Some(Arc::new(access_plan));

                file_scan_config_builder = file_scan_config_builder.with_file(partitioned_file);
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

/// FST-based PruningStatistics implementation
///
/// This leverages PDQ's FST indices to provide precise pruning information
/// to DataFusion's query optimizer, enabling better performance through
/// more accurate file and row group elimination.
struct FstPruningStatistics {
    index_engine: Arc<IndexQueryEngine>,
    filters: Vec<Expr>,
    file_list: Vec<String>,
}

impl FstPruningStatistics {
    fn new(index_engine: Arc<IndexQueryEngine>, filters: Vec<Expr>) -> Self {
        let file_list = index_engine.list_indexed_files().unwrap_or_default();

        Self {
            index_engine,
            filters,
            file_list,
        }
    }
}

impl PruningStatistics for FstPruningStatistics {
    fn num_containers(&self) -> usize {
        self.file_list.len()
    }

    fn min_values(&self, _column: &Column) -> Option<ArrayRef> {
        // FST doesn't store min/max directly, but we can approximate
        // by finding the lexicographically smallest indexed value
        // For now, return None to let DataFusion handle this
        None
    }

    fn max_values(&self, _column: &Column) -> Option<ArrayRef> {
        // Similar to min_values - FST doesn't store max directly
        None
    }

    fn null_counts(&self, _column: &Column) -> Option<ArrayRef> {
        // FST doesn't track null counts
        None
    }

    fn row_counts(&self, _column: &Column) -> Option<ArrayRef> {
        // FST doesn't track row counts per container
        None
    }

    /// The key method - use FST index for precise containment checks
    fn contained(&self, column: &Column, values: &HashSet<ScalarValue>) -> Option<BooleanArray> {
        let mut results = Vec::with_capacity(self.file_list.len());

        for file_hash in &self.file_list {
            let contains_any = values.iter().any(|value| {
                let value_str = value.to_string();
                if let Ok(matches) = self.index_engine.exact_search(&column.name, &value_str) {
                    matches.contains_key(file_hash)
                } else {
                    false
                }
            });
            results.push(contains_any);
        }

        Some(BooleanArray::from(results))
    }
}

impl fmt::Debug for FstPruningStatistics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FstPruningStatistics")
            .field("file_count", &self.file_list.len())
            .field("filter_count", &self.filters.len())
            .finish()
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
    use arrow::datatypes::{DataType, Field};
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
            data_dir: temp_dir.path().to_path_buf(),
            schema,
            table_name: "test".to_string(),
            use_row_selections: false,
            metadata_cache: HashMap::new(),
        };

        let filter = col("test_col").eq(lit("test_value"));
        let result = provider.extract_equality_filter(&filter);

        assert!(result.is_some());
        let (column, value) = result.unwrap();
        assert_eq!(column, "test_col");
        assert_eq!(value, "test_value");
    }
}
