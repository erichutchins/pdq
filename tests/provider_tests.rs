use datafusion::arrow::array::{Array, Int32Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::datasource::TableProvider;
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
                    .set_max_row_group_row_count(Some(rows_per_group))
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

    // Every returned row must satisfy the filter.
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
    let results = df.collect().await?;
    assert!(
        results.iter().all(|b| b.num_rows() == 0),
        "Expected no rows for a value absent from the index"
    );

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

    // Every returned row must satisfy both filters.
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

    Ok(())
}

/// Verify that querying for a nonexistent value returns zero rows.
/// Pins the no-match → empty DataSourceExec path. Uses count(*) so a projection
/// is pushed down to the scan (guards against projection/schema mismatches).
#[tokio::test]
async fn test_scan_returns_empty_for_no_match() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let data_dir = tmp.path().join("data");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(&index_dir)?;

    create_test_parquet_files(&data_dir, 1, 10, 10, &["alpha"]).await?;
    create_test_index(&index_dir, &data_dir, "value")?;

    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))?;

    let df = ctx
        .sql("SELECT count(*) as n FROM t WHERE value = 'nonexistent'")
        .await?;
    let results = df.collect().await?;
    let count_arr = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .unwrap();
    assert_eq!(
        count_arr.value(0),
        0,
        "Expected zero rows for nonexistent value"
    );
    Ok(())
}

/// The cached Parquet reader factory must parse each matched file's footer at
/// most once across repeated queries: later queries reuse the cached
/// `ParquetMetaData` instead of re-reading the footer. This is the warm-path
/// optimization adapted from DataFusion's `parquet_advanced_index` example.
#[tokio::test]
async fn test_metadata_cache_parses_each_footer_once() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let data_dir = tmp.path().join("data");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(&index_dir)?;

    // Two files; "needle" lives in exactly one row group of exactly one file,
    // so a `value = 'needle'` query prunes down to a single matched file.
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    for (file_idx, has_needle) in [true, false].iter().enumerate() {
        let path = data_dir.join(format!("f{file_idx}.parquet"));
        let props = WriterProperties::builder()
            .set_max_row_group_row_count(Some(10))
            .build();
        let mut writer = datafusion::parquet::arrow::ArrowWriter::try_new(
            File::create(&path)?,
            schema.clone(),
            Some(props),
        )?;
        for rg in 0..3usize {
            let label = if *has_needle && rg == 1 {
                "needle"
            } else {
                "haystack"
            };
            let start = (rg * 10) as i32;
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int32Array::from_iter_values(start..(start + 10))),
                    Arc::new(StringArray::from(vec![label; 10])),
                ],
            )?;
            writer.write(&batch)?;
            writer.flush()?;
        }
        writer.close()?;
    }

    create_test_index(&index_dir, &data_dir, "value")?;

    let provider = Arc::new(
        PdqTableProviderBuilder::new()
            .with_index_dir(&index_dir)
            .with_data_dir(&data_dir)
            .build()
            .await?,
    );
    let ctx = SessionContext::new();
    ctx.register_table("t", provider.clone())?;

    // Run the same selective query several times against the long-lived provider.
    for _ in 0..3 {
        let df = ctx
            .sql("SELECT count(*) FROM t WHERE value = 'needle'")
            .await?;
        let _ = df.collect().await?;
    }

    let (hits, misses) = provider.metadata_cache_stats();
    assert_eq!(
        misses, 1,
        "footer parsed exactly once for the single matched file across 3 queries \
         (got {misses} parses, {hits} hits)"
    );
    assert!(
        hits >= 1,
        "later queries must reuse cached metadata (hits={hits})"
    );

    Ok(())
}

/// Sum every `name=<digits>` occurrence in an EXPLAIN ANALYZE dump (metrics are
/// reported per partition, so the same metric can appear multiple times).
fn sum_metric(text: &str, name: &str) -> i64 {
    let needle = format!("{name}=");
    let mut total = 0i64;
    let mut rest = text;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<i64>() {
            total += n;
        }
    }
    total
}

/// With filter pushdown enabled, an equality predicate is applied as a row filter
/// *during* Parquet decode (late materialization). On the unsorted corpus PDQ
/// targets, page-index zonemaps can't prune, so a matched row group still holds
/// many non-matching rows; the scan must prune them itself instead of decoding
/// the whole row group and leaning on a FilterExec above it. We assert the
/// `pushdown_rows_pruned` metric is non-zero, which only happens when the scan
/// builds a row filter.
#[tokio::test]
async fn test_filter_pushdown_prunes_rows_in_scan() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let data_dir = tmp.path().join("data");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(&index_dir)?;

    // One row group of 100 rows; exactly one row is the needle (max_row_group of
    // 1000 keeps all 100 in a single row group).
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let path = data_dir.join("mixed.parquet");
    let props = WriterProperties::builder()
        .set_max_row_group_row_count(Some(1000))
        .build();
    let mut writer = datafusion::parquet::arrow::ArrowWriter::try_new(
        File::create(&path)?,
        schema.clone(),
        Some(props),
    )?;
    let values: Vec<&str> = (0..100)
        .map(|i| if i == 42 { "needle" } else { "haystack" })
        .collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int32Array::from_iter_values(0..100)),
            Arc::new(StringArray::from(values)),
        ],
    )?;
    writer.write(&batch)?;
    writer.close()?;

    create_test_index(&index_dir, &data_dir, "value")?;

    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;
    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))?;

    let plan = ctx
        .sql("EXPLAIN ANALYZE SELECT * FROM t WHERE value = 'needle'")
        .await?
        .collect()
        .await?;
    let mut text = String::new();
    for batch in &plan {
        for col in 0..batch.num_columns() {
            if let Some(arr) = batch.column(col).as_any().downcast_ref::<StringArray>() {
                for i in 0..arr.len() {
                    if arr.is_valid(i) {
                        text.push_str(arr.value(i));
                        text.push('\n');
                    }
                }
            }
        }
    }

    let pruned = sum_metric(&text, "pushdown_rows_pruned");
    assert!(
        pruned >= 1,
        "row filter must prune non-matching rows inside the scan; \
         pushdown_rows_pruned={pruned}\nplan:\n{text}"
    );

    // Late materialization must not change results: exactly one needle row.
    let res = ctx
        .sql("SELECT count(*) FROM t WHERE value = 'needle'")
        .await?
        .collect()
        .await?;
    let count = res[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(count, 1, "row filter must return exactly the matching row");

    Ok(())
}

/// Row-group pruning correctness: a value that lives in a single row group must
/// return exactly that row group's rows. A count(*) query forces a projection
/// down to the scan, exercising the ParquetAccessPlan + projection path.
#[tokio::test]
async fn test_row_group_pruning_isolates_value() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let data_dir = tmp.path().join("data");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(&index_dir)?;

    // 3 row groups of 10 rows. "needle" lives only in row group 1.
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let file_path = data_dir.join("isolated.parquet");
    let props = WriterProperties::builder()
        .set_max_row_group_row_count(Some(10))
        .build();
    let mut writer = datafusion::parquet::arrow::ArrowWriter::try_new(
        File::create(&file_path)?,
        schema.clone(),
        Some(props),
    )?;
    for rg in 0..3usize {
        let label = if rg == 1 { "needle" } else { "haystack" };
        let start = (rg * 10) as i32;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int32Array::from_iter_values(start..(start + 10))),
                Arc::new(StringArray::from(vec![label; 10])),
            ],
        )?;
        writer.write(&batch)?;
        writer.flush()?;
    }
    writer.close()?;

    create_test_index(&index_dir, &data_dir, "value")?;

    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;
    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))?;

    // count(*) pushes a projection to the scan.
    let df = ctx
        .sql("SELECT count(*) as n FROM t WHERE value = 'needle'")
        .await?;
    let results = df.collect().await?;
    let count = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(count, 10, "needle occupies exactly one 10-row row group");

    // SELECT * must return only needle rows.
    let df = ctx.sql("SELECT * FROM t WHERE value = 'needle'").await?;
    let results = df.collect().await?;
    let mut total = 0;
    for batch in &results {
        let value_array = batch
            .column_by_name("value")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..batch.num_rows() {
            assert_eq!(value_array.value(i), "needle");
            total += 1;
        }
    }
    assert_eq!(total, 10);
    Ok(())
}
