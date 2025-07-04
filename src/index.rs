use crate::{calculate_file_hash, Result};
use fst::SetBuilder;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};
use std::fs::{create_dir_all, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Indexer builds FST (Finite State Transducer) indices for Parquet files.
///
/// The Indexer scans Parquet files and creates a column-specific FST index for
/// rapid value lookups. Each index maps column values to the specific row groups
/// where they appear, enabling precise row group pruning during queries.
///
/// # Index Structure
///
/// - Creates one FST file per (column, parquet_file) combination
/// - Stores indices in a directory structure based on file hashes
/// - Index keys are formatted as `value\x00rgN` where N is the row group index
pub struct Indexer {
    /// Directory where FST index files will be stored
    output_dir: String,
}

impl Indexer {
    /// Creates a new Indexer that will store indices in the specified directory.
    ///
    /// # Parameters
    ///
    /// * `output_dir` - Path to directory where FST index files will be stored
    ///
    /// # Returns
    ///
    /// A new Indexer instance ready to build indices
    pub fn new(output_dir: &str) -> Self {
        Self {
            output_dir: output_dir.to_string(),
        }
    }

    /// Builds FST indices for a specific column across all Parquet files in a directory.
    ///
    /// Recursively finds all Parquet files in the given directory and creates an FST
    /// index for the specified column in each file. Each index maps column values to
    /// row groups where they appear.
    ///
    /// # Parameters
    ///
    /// * `data_dir` - Directory containing Parquet files to index
    /// * `column` - Name of the column to index
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if index creation fails
    ///
    /// # Error
    ///
    /// Returns an error if directory creation fails or if any file cannot be indexed
    pub fn build_index(&self, data_dir: &Path, column: &str) -> Result<()> {
        create_dir_all(&self.output_dir)?;

        for entry in WalkDir::new(data_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "parquet"))
        {
            let file_path = entry.path();
            println!("Processing file: {}", file_path.display());

            self.index_file(file_path, column)?;
        }

        Ok(())
    }

    /// Creates an FST index for a specific column in a single Parquet file.
    ///
    /// For each row group in the file, extracts unique values from the specified column
    /// and builds a combined FST index. Keys in the index are formatted as `value\x00rgN`
    /// where N is the row group index, allowing precise row group lookups.
    ///
    /// # Parameters
    ///
    /// * `file_path` - Path to the Parquet file to index
    /// * `column` - Name of the column to extract values from
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error if indexing fails
    ///
    /// # Error
    ///
    /// Returns an error if the file cannot be opened, if the column does not exist,
    /// or if the FST index cannot be created
    ///
    /// # Index Storage
    ///
    /// The index is stored at: `{output_dir}/{file_hash}/{column}.fst`
    /// where `file_hash` is a hash of the file path for uniqueness
    fn index_file(&self, file_path: &Path, column: &str) -> Result<()> {
        let file = File::open(file_path)?;
        let reader = SerializedFileReader::new(file)?;

        let mut values = Vec::new();

        for row_group_idx in 0..reader.num_row_groups() {
            let _row_group = reader.get_row_group(row_group_idx)?;
            let batch_reader = ParquetRecordBatchReaderBuilder::try_new(File::open(file_path)?)?
                .with_row_groups(vec![row_group_idx])
                .build()?;

            for batch in batch_reader {
                let batch = batch?;

                if let Some(column_data) = batch.column_by_name(column) {
                    let string_array = column_data
                        .as_any()
                        .downcast_ref::<arrow::array::StringArray>();

                    if let Some(arr) = string_array {
                        for value in arr.iter().flatten() {
                            let key = format!("{value}\x00rg{row_group_idx}");
                            values.push(key);
                        }
                    }
                }
            }
        }

        values.sort_unstable();
        values.dedup();

        let file_hash = calculate_file_hash(&file_path.to_string_lossy())?;
        let index_dir = PathBuf::from(&self.output_dir).join(&file_hash);
        create_dir_all(&index_dir)?;

        let index_path = index_dir.join(format!("{column}.fst"));
        let index_file = File::create(&index_path)?;
        let mut writer = BufWriter::new(index_file);

        let mut builder = SetBuilder::new(&mut writer)?;
        for value in values {
            builder.insert(value)?;
        }
        builder.finish()?;

        println!("Created index: {}", index_path.display());
        Ok(())
    }
}
