use anyhow::Result;
use datafusion::arrow::array::{Int32Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::execution::context::SessionContext;

use datafusion::parquet::basic::{Compression, Encoding};
use datafusion::parquet::file::properties::WriterProperties;
use pdq::{index::Indexer, search::Searcher, PdqTableProviderBuilder};
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

    // Verify the index was created successfully using the Searcher
    let searcher = Searcher::new(index_dir.to_str().unwrap());
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
    let table_provider = PdqTableProviderBuilder::new("test_table".to_string())
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
    verify_query_results(&apple_batches, "value", |v: &str| v == "apple")?;

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
    verify_query_results(&id_batches, "id", |id: i32| id < 3)?;

    // Test 4: Multiple predicates
    let complex_df = ctx
        .sql("SELECT * FROM test_table WHERE value = 'apple' AND id < 3")
        .await?;
    let complex_batches = complex_df.collect().await?;

    // Verify we got only results that match both predicates
    verify_query_results(&complex_batches, "value", |v: &str| v == "apple")?;
    verify_query_results(&complex_batches, "id", |id: i32| id < 3)?;

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
        .set_max_row_group_size(2)
        .build();

    let mut writer =
        parquet::arrow::ArrowWriter::try_new(File::create(&file_path)?, schema, Some(props))?;

    writer.write(&batch)?;
    writer.close()?;

    Ok(vec![file_path])
}

/// Helper function to verify query results match a predicate.
fn verify_query_results<T, F>(
    batches: &[RecordBatch],
    column_name: &str,
    predicate: F,
) -> Result<()>
where
    T: std::fmt::Debug + Clone,
    F: Fn(T) -> bool,
{
    if batches.is_empty() {
        return Ok(());
    }

    for batch in batches {
        let column = batch
            .column_by_name(column_name)
            .ok_or_else(|| anyhow::anyhow!("Column not found: {column_name}"))?;

        for row_idx in 0..batch.num_rows() {
            let scalar_value = match column.data_type() {
                DataType::Int32 => {
                    let array = column
                        .as_any()
                        .downcast_ref::<Int32Array>()
                        .ok_or_else(|| anyhow::anyhow!("Failed to downcast to Int32Array"))?;
                    let value: T = unsafe { std::mem::transmute_copy(&array.value(row_idx)) };
                    value
                }
                DataType::Utf8 => {
                    let array = column
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .ok_or_else(|| anyhow::anyhow!("Failed to downcast to StringArray"))?;
                    let value = array.value(row_idx);
                    // This is a bit of a hack but works for our test cases
                    let value: T = unsafe { std::mem::transmute_copy::<&str, T>(&value) };
                    value
                }
                _ => anyhow::bail!(
                    "Unsupported column type for testing: {:?}",
                    column.data_type()
                ),
            };

            assert!(
                predicate(scalar_value.clone()),
                "Value in column {column_name} doesn't match predicate: {scalar_value:?}",
            );
        }
    }

    Ok(())
}
