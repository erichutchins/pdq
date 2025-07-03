use crate::{Result, provider::create_table_provider_from_index_results, query::IndexQueryEngine};
use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::csv::WriterBuilder;
use datafusion::arrow::json::LineDelimitedWriter;
use datafusion::execution::context::SessionContext;
use datafusion::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ParquetFilter {
    ctx: SessionContext,
}

impl Default for ParquetFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ParquetFilter {
    pub fn new() -> Self {
        Self {
            ctx: SessionContext::new(),
        }
    }

    pub async fn query_with_datafusion(
        &self,
        file_paths: Vec<String>,
        column: &str,
        term: &str,
        output_format: &str,
    ) -> Result<String> {
        if file_paths.is_empty() {
            return Ok(String::new());
        }

        let mut all_results = Vec::new();

        for file_path in file_paths {
            if let Ok(metadata) = fs::metadata(&file_path) {
                if metadata.is_file() {
                    let df = self
                        .ctx
                        .read_parquet(&file_path, ParquetReadOptions::default())
                        .await?;

                    let filtered_df = df.filter(col(column).eq(lit(term)))?;

                    let results = filtered_df.collect().await?;
                    all_results.extend(results);
                }
            }
        }

        match output_format {
            "csv" => self.format_as_csv(all_results),
            "json" | "jsonl" | "ndjson" => self.format_as_jsonl(all_results),
            _ => self.format_as_jsonl(all_results), // Default to JSONL
        }
    }

    pub async fn query_with_row_groups(
        &self,
        file_row_groups: HashMap<String, Vec<usize>>,
        column: &str,
        term: &str,
        output_format: &str,
    ) -> Result<String> {
        if file_row_groups.is_empty() {
            return Ok(String::new());
        }

        // Convert string paths to PathBuf
        let file_row_groups_pathbuf: HashMap<PathBuf, Vec<usize>> = file_row_groups
            .into_iter()
            .map(|(path, row_groups)| (PathBuf::from(path), row_groups))
            .collect();

        let results = self
            .query_with_pdq_provider(file_row_groups_pathbuf, column, term)
            .await?;

        match output_format {
            "csv" => self.format_as_csv(results),
            "json" | "jsonl" | "ndjson" => self.format_as_jsonl(results),
            _ => self.format_as_jsonl(results), // Default to JSONL
        }
    }

    /// Query using the optimized PdqTableProvider with row-group pruning
    pub async fn query_with_pdq_provider(
        &self,
        file_row_groups: HashMap<PathBuf, Vec<usize>>,
        column: &str,
        term: &str,
    ) -> Result<Vec<RecordBatch>> {
        if file_row_groups.is_empty() {
            return Ok(Vec::new());
        }

        // Create the PdqTableProvider with row-group optimization
        let table_provider =
            create_table_provider_from_index_results(file_row_groups, "pdq_table".to_string())
                .await?;

        // Register the table provider
        self.ctx
            .register_table("pdq_table", Arc::new(table_provider))?;

        // Execute the query with native DataFusion filtering
        let df = self
            .ctx
            .table("pdq_table")
            .await?
            .filter(col(column).eq(lit(term)))?;

        let results = df.collect().await?;
        Ok(results)
    }

    /// Query using IndexQueryEngine and resolve file paths
    pub async fn query_with_index_engine(
        &self,
        index_dir: &Path,
        data_dir: &Path,
        column: &str,
        term: &str,
        output_format: &str,
    ) -> Result<String> {
        // Use IndexQueryEngine to find relevant file hashes and row groups
        let engine = IndexQueryEngine::new(index_dir);
        let file_hash_row_groups = engine.exact_search(column, term)?;

        if file_hash_row_groups.is_empty() {
            return Ok(String::new());
        }

        // Resolve file hashes to actual file paths
        let file_hashes: Vec<String> = file_hash_row_groups.keys().cloned().collect();
        let hash_to_path = engine.resolve_file_paths(&file_hashes, data_dir)?;

        // Convert to path-based mapping
        let mut file_row_groups = HashMap::new();
        for (file_hash, row_groups) in file_hash_row_groups {
            if let Some(file_path) = hash_to_path.get(&file_hash) {
                file_row_groups.insert(file_path.clone(), row_groups);
            }
        }

        let results = self
            .query_with_pdq_provider(file_row_groups, column, term)
            .await?;

        match output_format {
            "csv" => self.format_as_csv(results),
            "json" | "jsonl" | "ndjson" => self.format_as_jsonl(results),
            _ => self.format_as_jsonl(results), // Default to JSONL
        }
    }

    fn format_as_csv(&self, batches: Vec<RecordBatch>) -> Result<String> {
        if batches.is_empty() {
            return Ok(String::new());
        }

        let mut output = Vec::new();
        let mut cursor = Cursor::new(&mut output);

        let mut writer = WriterBuilder::new().with_header(true).build(&mut cursor);

        for batch in batches {
            writer.write(&batch)?;
        }

        drop(writer);
        Ok(String::from_utf8(output)?)
    }

    fn format_as_jsonl(&self, batches: Vec<RecordBatch>) -> Result<String> {
        if batches.is_empty() {
            return Ok(String::new());
        }

        let mut output = Vec::new();
        let mut cursor = Cursor::new(&mut output);

        let mut writer = LineDelimitedWriter::new(&mut cursor);

        for batch in batches {
            writer.write(&batch)?;
        }

        writer.finish()?;
        drop(writer);
        Ok(String::from_utf8(output)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::{Int64Array, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn create_test_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
        ]));

        let id_array = Int64Array::from(vec![1, 2, 3]);
        let name_array = StringArray::from(vec!["Alice", "Bob", "Charlie"]);

        RecordBatch::try_new(schema, vec![Arc::new(id_array), Arc::new(name_array)]).unwrap()
    }

    #[test]
    fn test_jsonl_formatting() -> Result<()> {
        let filter = ParquetFilter::new();
        let batch = create_test_batch();
        let batches = vec![batch];

        let result = filter.format_as_jsonl(batches)?;

        // Should have 3 lines (one per record)
        let lines: Vec<&str> = result.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);

        // Each line should be valid JSON
        for line in &lines {
            let _: serde_json::Value = serde_json::from_str(line)?;
        }

        // First line should contain the first record
        assert!(lines[0].contains("\"id\":1"));
        assert!(lines[0].contains("\"name\":\"Alice\""));

        Ok(())
    }

    #[test]
    fn test_empty_jsonl_formatting() -> Result<()> {
        let filter = ParquetFilter::new();
        let result = filter.format_as_jsonl(vec![])?;
        assert_eq!(result, "");
        Ok(())
    }

    #[test]
    fn test_csv_formatting() -> Result<()> {
        let filter = ParquetFilter::new();
        let batch = create_test_batch();
        let batches = vec![batch];

        let result = filter.format_as_csv(batches)?;

        // Should have header and 3 data rows
        let lines: Vec<&str> = result.trim().split('\n').collect();
        assert_eq!(lines.len(), 4); // header + 3 data rows

        // First line should be header
        assert!(lines[0].contains("id") && lines[0].contains("name"));

        // Data rows should contain values
        assert!(lines[1].contains("1") && lines[1].contains("Alice"));

        Ok(())
    }
}
