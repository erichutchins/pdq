use anyhow::Result;
use datafusion::arrow::array::{Int32Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::execution::context::SessionContext;

use datafusion::parquet::basic::{Compression, Encoding};
use datafusion::parquet::file::properties::WriterProperties;
use pdq::{PdqTableProviderBuilder, index::Indexer, query::IndexQueryEngine};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

/// Integration test for PDQ: Creates test data, indexes it, and runs queries
/// to validate the complete pipeline works correctly.
#[tokio::test]
async fn test_complete_pdq_pipeline() -> Result<()> {
    // Create temporary directories for the test
    let test_dir = TempDir::new()?;
    let data_dir = test_dir.path().join("data");
    let index_dir = test_dir.path().join("index");

    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&index_dir)?;

    // Create test Parquet files with known data
    create_test_parquet_files(&data_dir, &["apple", "banana", "cherry", "apple", "banana"]).await?;

    // Build the FST index using our Indexer
    let indexer = Indexer::new(index_dir.to_str().unwrap());
    indexer.build_index(&data_dir, "value")?;

    // Verify the index was created successfully using the IndexQueryEngine
    let searcher = IndexQueryEngine::new(index_dir.to_str().unwrap());
    let apple_results = searcher.exact_search("value", "apple")?;
    assert!(
        !apple_results.is_empty(),
        "Index should find 'apple' values"
    );

    let nonexistent_results = searcher.exact_search("value", "nonexistent")?;
    assert!(
        nonexistent_results.is_empty(),
        "Index should not find nonexistent values"
    );

    // Query using PdqTableProvider
    let table_provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    // Create a session context and register the table
    let ctx = SessionContext::new();
    ctx.register_table("test_table", Arc::new(table_provider))?;

    // Test 1: Query for apple values
    let apple_df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'apple'")
        .await?;
    let apple_batches = apple_df.collect().await?;

    // Verify we got only apple values
    verify_string_column(&apple_batches, "value", |v| v == "apple")?;

    // Test 2: Query for nonexistent values (should be empty)
    let empty_df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'nonexistent'")
        .await?;
    let empty_batches = empty_df.collect().await?;
    assert!(
        empty_batches.is_empty() || empty_batches.iter().all(|b| b.num_rows() == 0),
        "Query for nonexistent value should return empty results"
    );

    // Test 3: Query with ID predicate
    let id_df = ctx.sql("SELECT * FROM test_table WHERE id < 3").await?;
    let id_batches = id_df.collect().await?;
    verify_i32_column(&id_batches, "id", |id| id < 3)?;

    // Test 4: Multiple predicates
    let complex_df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'apple' AND id < 3")
        .await?;
    let complex_batches = complex_df.collect().await?;

    // Verify we got only results that match both predicates
    verify_string_column(&complex_batches, "value", |v| v == "apple")?;
    verify_i32_column(&complex_batches, "id", |id| id < 3)?;

    Ok(())
}

/// Smoke test: index over 4 Parquet files, each with 3 row groups, and query
/// matching row groups across all of them. Verifies DataFusion drives all
/// partitions and merges results: 4 files × 3 row groups × 10 rows = 120.
#[tokio::test]
async fn test_multifile_parallel_scan() -> Result<()> {
    let test_dir = TempDir::new()?;
    let data_dir = test_dir.path().join("data");
    let index_dir = test_dir.path().join("index");

    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&index_dir)?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));

    for file_idx in 0..4usize {
        let file_path = data_dir.join(format!("file_{file_idx}.parquet"));
        let props = WriterProperties::builder()
            .set_max_row_group_row_count(Some(10))
            .build();
        let mut writer = datafusion::parquet::arrow::ArrowWriter::try_new(
            File::create(&file_path)?,
            schema.clone(),
            Some(props),
        )?;
        for rg in 0..3usize {
            let start = (file_idx * 30 + rg * 10) as i32;
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int32Array::from_iter_values(start..(start + 10))),
                    Arc::new(StringArray::from(vec!["target"; 10])),
                ],
            )?;
            writer.write(&batch)?;
            writer.flush()?;
        }
        writer.close()?;
    }

    let indexer = Indexer::new(index_dir.to_str().unwrap());
    indexer.build_index(&data_dir, "value")?;

    let provider = PdqTableProviderBuilder::new()
        .with_index_dir(&index_dir)
        .with_data_dir(&data_dir)
        .build()
        .await?;

    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))?;

    let df = ctx
        .sql("SELECT count(*) as n FROM t WHERE value = 'target'")
        .await?;
    let results = df.collect().await?;

    let count_arr = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<datafusion::arrow::array::Int64Array>()
        .unwrap();
    assert_eq!(
        count_arr.value(0),
        120,
        "Expected 4 files × 3 row groups × 10 rows = 120 total rows"
    );

    Ok(())
}

/// Creates test Parquet files with predictable data for testing.
async fn create_test_parquet_files(dir: &Path, values: &[&str]) -> Result<Vec<PathBuf>> {
    // Define schema: id (int32) and value (string)
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("value", DataType::Utf8, false),
    ]));

    // Create the file path
    let file_path = dir.join("test_data.parquet");

    // Create ID array (sequential integers)
    let ids: Vec<i32> = (0..values.len()).map(|i| i as i32).collect();

    // Create a RecordBatch with our test data
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int32Array::from(ids)),
            Arc::new(StringArray::from(values.to_vec())),
        ],
    )?;

    // Write with row group size of 2 to ensure multiple row groups
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .set_encoding(Encoding::PLAIN)
        .set_max_row_group_row_count(Some(2))
        .build();

    let mut writer = datafusion::parquet::arrow::ArrowWriter::try_new(
        File::create(&file_path)?,
        schema,
        Some(props),
    )?;

    writer.write(&batch)?;
    writer.close()?;

    Ok(vec![file_path])
}

/// Helper function to verify string column values match a predicate.
fn verify_string_column(
    batches: &[RecordBatch],
    column_name: &str,
    predicate: impl Fn(&str) -> bool,
) -> Result<()> {
    for batch in batches {
        let column = batch
            .column_by_name(column_name)
            .ok_or_else(|| anyhow::anyhow!("Column not found: {column_name}"))?;

        let array = column
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow::anyhow!("Column {column_name} is not a StringArray"))?;

        for row_idx in 0..batch.num_rows() {
            let value = array.value(row_idx);
            assert!(
                predicate(value),
                "Value in column {column_name} doesn't match predicate: {value:?}",
            );
        }
    }
    Ok(())
}

/// Helper function to verify i32 column values match a predicate.
fn verify_i32_column(
    batches: &[RecordBatch],
    column_name: &str,
    predicate: impl Fn(i32) -> bool,
) -> Result<()> {
    for batch in batches {
        let column = batch
            .column_by_name(column_name)
            .ok_or_else(|| anyhow::anyhow!("Column not found: {column_name}"))?;

        let array = column
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or_else(|| anyhow::anyhow!("Column {column_name} is not an Int32Array"))?;

        for row_idx in 0..batch.num_rows() {
            let value = array.value(row_idx);
            assert!(
                predicate(value),
                "Value in column {column_name} doesn't match predicate: {value:?}",
            );
        }
    }
    Ok(())
}
