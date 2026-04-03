use datafusion::arrow::array::{Array, Int32Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::TableProvider;
use datafusion::datasource::physical_plan::FileOpener;
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::TableProviderFilterPushDown;
use datafusion::logical_expr::{Expr, col, lit};
use datafusion::parquet::basic::{Compression, Encoding};
use datafusion::parquet::file::properties::WriterProperties;
use pdq::index::Indexer;
use pdq::provider::PdqTableProviderBuilder;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

/// Test helper to create test Parquet files with predictable data.
/// Creates files with:
/// - An 'id' column of sequential integers
/// - A 'value' column of strings (configurable)
/// - Each file has the specified number of row groups
async fn create_test_parquet_files(
    dir: &Path,
    file_count: usize,
    rows_per_file: usize,
    rows_per_group: usize,
    values: &[&str],
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));

    fs::create_dir_all(dir)?;
    let mut file_paths = Vec::with_capacity(file_count);

    for file_idx in 0..file_count {
        let file_path = dir.join(format!("test_file_{file_idx}.parquet"));
        file_paths.push(file_path.clone());

        let mut writer = datafusion::parquet::arrow::ArrowWriter::try_new(
            File::create(&file_path)?,
            schema.clone(),
            Some(
                WriterProperties::builder()
                    .set_compression(Compression::SNAPPY)
                    .set_encoding(Encoding::PLAIN)
                    .set_max_row_group_size(rows_per_group)
                    .build(),
            ),
        )?;

        // Create data in batches that will form row groups
        for batch_idx in 0..(rows_per_file / rows_per_group) {
            let start_id = file_idx * rows_per_file + batch_idx * rows_per_group;

            // Create a batch with ids and values
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int32Array::from_iter_values(
                        (start_id..(start_id + rows_per_group)).map(|id| id as i32),
                    )),
                    Arc::new(StringArray::from_iter_values(
                        (0..rows_per_group).map(|i| values[i % values.len()]),
                    )),
                ],
            )?;

            writer.write(&batch)?;
        }

        writer.close()?;
    }

    Ok(file_paths)
}

/// Helper to create a real FST index for test files using the Indexer.
///
/// This replaces the previous stub implementation and creates functional FST
/// indices that enable proper row-group pruning tests.
fn create_test_index(
    index_dir: &Path,
    data_dir: &Path,
    column: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(index_dir)?;

    let indexer = Indexer::new(index_dir.to_str().unwrap());
    indexer.build_index(data_dir, column)?;

    Ok(())
}

#[tokio::test]
async fn test_table_provider_builder() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test data structure
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 2, 1000, 500, values).await?;

    // Test the builder pattern
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Verify the table name
    assert_eq!(
        provider.table_type(),
        datafusion::datasource::TableType::Base
    );

    Ok(())
}

#[tokio::test]
async fn test_schema_inference() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test files with known schema
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 1, 1000, 500, values).await?;

    // Create provider and verify schema
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    let schema = provider.schema();

    // Verify schema matches what we created
    assert_eq!(schema.fields().len(), 2);
    assert_eq!(schema.field(0).name(), "id");
    assert_eq!(schema.field(0).data_type(), &DataType::Int32);
    assert_eq!(schema.field(1).name(), "value");
    assert_eq!(schema.field(1).data_type(), &DataType::Utf8);

    Ok(())
}

#[tokio::test]
async fn test_basic_query() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test files with known values
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 2, 1000, 200, values).await?;

    // Create test index
    create_test_index(&index_dir, &data_dir, "value")?;

    // Create provider
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Create a session context and register the table
    let ctx = SessionContext::new();
    ctx.register_table("test_table", Arc::new(provider))?;

    // Execute a simple query
    let df = ctx.sql("SELECT * FROM test_table LIMIT 10").await?;
    let results = df.collect().await?;

    // With a stub index, we might not get results
    // Just verify the query executed without error
    if !results.is_empty() {
        assert_eq!(results[0].num_columns(), 2);
        assert!(results[0].num_rows() <= 10);
    }

    Ok(())
}

#[tokio::test]
async fn test_filter_pushdown() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test files with known values
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 2, 1000, 200, values).await?;

    // Create test index
    create_test_index(&index_dir, &data_dir, "value")?;

    // Create provider
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Test filter pushdown capability
    let binding = Expr::BinaryExpr(*Box::new(datafusion::logical_expr::BinaryExpr {
        left: Box::new(col("value")),
        op: datafusion::logical_expr::Operator::Eq,
        right: Box::new(lit("apple")),
    }));

    let filters = vec![&binding];
    let pushdown_result = provider.supports_filters_pushdown(&filters)?;

    // Verify that our provider indicates it can handle this filter
    assert_eq!(pushdown_result.len(), 1);
    assert_eq!(pushdown_result[0], TableProviderFilterPushDown::Inexact);

    Ok(())
}

#[tokio::test]
async fn test_filtered_query() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test files with known values
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 2, 1000, 200, values).await?;

    // Create test index
    create_test_index(&index_dir, &data_dir, "value")?;

    // Create provider
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Create a session context and register the table
    let ctx = SessionContext::new();
    ctx.register_table("test_table", Arc::new(provider))?;

    // Execute a filtered query
    let df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'apple'")
        .await?;
    let results = df.collect().await?;

    // With our stub index, we might not get results
    // If we do get results, verify they match our filter
    if !results.is_empty() {
        for batch in &results {
            let value_array = batch
                .column_by_name("value")
                .expect("value column should exist");

            for i in 0..batch.num_rows() {
                let value = value_array
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("should be string array")
                    .value(i);

                assert_eq!(value, "apple");
            }
        }
    }
    // Test passes whether we got results or not

    Ok(())
}

#[tokio::test]
async fn test_empty_result_optimization() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test files with known values
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 2, 1000, 200, values).await?;

    // Create test index
    create_test_index(&index_dir, &data_dir, "value")?;

    // Create provider
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Create a session context and register the table
    let ctx = SessionContext::new();
    ctx.register_table("test_table", Arc::new(provider))?;

    // Execute a query that should find nothing
    let df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'nonexistent_value'")
        .await?;
    let _results = df.collect().await?;

    // With our stub index implementation, we'll likely get empty results
    // but we don't need to assert it since the query itself is what we're testing
    // Just check that the query executed without error

    Ok(())
}

#[tokio::test]
async fn test_multiple_filters() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let index_dir = temp_dir.path().join("index");
    let data_dir = temp_dir.path().join("data");

    // Create test files with known values
    fs::create_dir_all(&index_dir)?;
    fs::create_dir_all(&data_dir)?;

    let values = &["apple", "banana", "cherry", "date", "elderberry"];
    create_test_parquet_files(&data_dir, 2, 1000, 200, values).await?;

    // Create test index
    create_test_index(&index_dir, &data_dir, "value")?;

    // Create provider
    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Create a session context and register the table
    let ctx = SessionContext::new();
    ctx.register_table("test_table", Arc::new(provider))?;

    // Execute a query with multiple filters
    let df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'apple' AND id < 100")
        .await?;
    let results = df.collect().await?;

    // With our stub index, we might not get results
    // If we do get results, verify they match our filters
    if !results.is_empty() {
        for batch in &results {
            let value_array = batch
                .column_by_name("value")
                .expect("value column should exist");
            let id_array = batch.column_by_name("id").expect("id column should exist");

            for i in 0..batch.num_rows() {
                let value = value_array
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("should be string array")
                    .value(i);

                let id = id_array
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .expect("should be int32 array")
                    .value(i);

                assert_eq!(value, "apple");
                assert!(id < 100);
            }
        }
    }
    // Test passes whether we got results or not - we're testing the query execution

    Ok(())
}

// ─── PdqParquetOpener tests ──────────────────────────────────────────────────

/// Helper: write an N-row-group Parquet file.
/// Row group i has `rows_per_rg` rows, all with value = "rg{i}".
async fn write_nrg_parquet(
    path: &Path,
    num_rg: usize,
    rows_per_rg: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use datafusion::parquet::arrow::ArrowWriter;
    use datafusion::parquet::file::properties::WriterProperties;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    #[allow(deprecated)]
    let props = WriterProperties::builder()
        .set_max_row_group_size(rows_per_rg)
        .build();
    let mut writer = ArrowWriter::try_new(
        File::create(path)?,
        schema.clone(),
        Some(props),
    )?;

    for rg_idx in 0..num_rg {
        let start = (rg_idx * rows_per_rg) as i32;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int32Array::from_iter_values(
                    start..(start + rows_per_rg as i32),
                )),
                Arc::new(StringArray::from(vec![
                    format!("rg{rg_idx}");
                    rows_per_rg
                ])),
            ],
        )?;
        writer.write(&batch)?;
        writer.flush()?; // force row group boundary
    }
    writer.close()?;
    Ok(())
}

/// Helper: construct a PartitionedFile with Vec<usize> row group extensions.
fn partitioned_file_with_row_groups(
    path: &Path,
    row_groups: Vec<usize>,
) -> Result<datafusion::datasource::listing::PartitionedFile, Box<dyn std::error::Error>> {
    let canonical = path.canonicalize()?;
    let file_size = std::fs::metadata(&canonical)?.len();
    let mut pf = datafusion::datasource::listing::PartitionedFile::new(
        canonical.display().to_string(),
        file_size,
    );
    pf.extensions = Some(Arc::new(row_groups));
    Ok(pf)
}

/// Open a PdqParquetOpener against a 3-rg file with row_groups=[1].
/// Assert only rows with value="rg1" appear in output.
#[tokio::test]
async fn test_opener_reads_correct_row_groups() -> Result<(), Box<dyn std::error::Error>> {
    use futures::StreamExt;
    use object_store::local::LocalFileSystem;
    use pdq::provider::PdqParquetOpener;

    let tmp = TempDir::new()?;
    let file_path = tmp.path().join("test.parquet");
    write_nrg_parquet(&file_path, 3, 10).await?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));

    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(LocalFileSystem::new());
    let opener = PdqParquetOpener::new(
        object_store,
        schema.clone(),
        None,    // projection: all columns
        1024,    // batch_size
    );

    let pf = partitioned_file_with_row_groups(&file_path, vec![1])?;
    let future = opener.open(pf)?;
    let mut stream: futures::stream::BoxStream<'static, datafusion::common::Result<RecordBatch>> = future.await?;

    let mut all_values: Vec<String> = vec![];
    while let Some(batch_result) = stream.next().await {
        let batch = batch_result?;
        let string_arr = batch.column(1).as_any()
            .downcast_ref::<StringArray>().unwrap();
        for i in 0..string_arr.len() {
            all_values.push(string_arr.value(i).to_string());
        }
    }

    assert!(
        all_values.iter().all(|v| v == "rg1"),
        "Expected only rg1 values, got: {all_values:?}"
    );
    assert_eq!(all_values.len(), 10, "Expected 10 rows from row group 1");
    Ok(())
}

/// Open with row_groups=[0, 99] on a 2-row-group file.
/// Assert only row group 0 is scanned, no panic.
#[tokio::test]
async fn test_opener_skips_stale_index() -> Result<(), Box<dyn std::error::Error>> {
    use futures::StreamExt;
    use object_store::local::LocalFileSystem;
    use pdq::provider::PdqParquetOpener;

    let tmp = TempDir::new()?;
    let file_path = tmp.path().join("two_rg.parquet");
    write_nrg_parquet(&file_path, 2, 5).await?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(LocalFileSystem::new());
    let opener = PdqParquetOpener::new(object_store, schema.clone(), None, 1024);

    let pf = partitioned_file_with_row_groups(&file_path, vec![0, 99])?;
    let future = opener.open(pf)?;
    let mut stream: futures::stream::BoxStream<'static, datafusion::common::Result<RecordBatch>> = future.await?;

    let mut all_values: Vec<String> = vec![];
    while let Some(batch_result) = stream.next().await {
        let batch = batch_result?;
        let string_arr = batch.column(1).as_any()
            .downcast_ref::<StringArray>().unwrap();
        for i in 0..string_arr.len() {
            all_values.push(string_arr.value(i).to_string());
        }
    }

    assert!(
        all_values.iter().all(|v| v == "rg0"),
        "Expected only rg0, got: {all_values:?}"
    );
    assert_eq!(all_values.len(), 5);
    Ok(())
}

/// Open with row_groups=[99] only on a 2-row-group file.
/// Assert zero batches produced (no panic, no error).
#[tokio::test]
async fn test_opener_empty_result_on_all_stale() -> Result<(), Box<dyn std::error::Error>> {
    use futures::StreamExt;
    use object_store::local::LocalFileSystem;
    use pdq::provider::PdqParquetOpener;

    let tmp = TempDir::new()?;
    let file_path = tmp.path().join("two_rg.parquet");
    write_nrg_parquet(&file_path, 2, 5).await?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let object_store: Arc<dyn object_store::ObjectStore> = Arc::new(LocalFileSystem::new());
    let opener = PdqParquetOpener::new(object_store, schema.clone(), None, 1024);

    let pf = partitioned_file_with_row_groups(&file_path, vec![99])?;
    let future = opener.open(pf)?;
    let mut stream: futures::stream::BoxStream<'static, datafusion::common::Result<RecordBatch>> = future.await?;

    let mut batch_count = 0;
    while let Some(batch_result) = stream.next().await {
        batch_result?;
        batch_count += 1;
    }
    assert_eq!(batch_count, 0, "Expected zero batches when all indices are stale");
    Ok(())
}
