use anyhow::Result;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::file::reader::{FileReader, SerializedFileReader};
use fst::SetBuilder;
use std::collections::BTreeSet;
use std::fs::{File, create_dir_all};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::key_format;

/// Buffer capacity constants for string building
mod buffer_config {
    /// Initial capacity for key buffer during index building
    /// Most string values are relatively small, so 256 bytes is a reasonable default.
    /// The buffer will grow automatically if needed, with amortized cost.
    pub const KEY_BUFFER_INITIAL_CAPACITY: usize = 256;
}

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
        self.build_index_ext(data_dir, column, false)
    }

    /// Builds FST indices with optional forced re-indexing.
    pub fn build_index_ext(&self, data_dir: &Path, column: &str, force: bool) -> Result<()> {
        create_dir_all(&self.output_dir)?;

        for entry in WalkDir::new(data_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "parquet"))
        {
            let file_path = entry.path();
            if force || self.needs_indexing(file_path, column)? {
                println!("Indexing file: {}", file_path.display());
                self.index_file(file_path, column)?;
            } else {
                println!("Skipping up-to-date file: {}", file_path.display());
            }
        }

        Ok(())
    }

    /// Check if a file needs to be indexed or re-indexed.
    fn needs_indexing(&self, file_path: &Path, column: &str) -> Result<bool> {
        let file_path_str = file_path.to_string_lossy();
        let file_hash = crate::calculate_file_hash(&file_path_str)?;
        let index_dir = PathBuf::from(&self.output_dir).join(&file_hash);
        let index_path = index_dir.join(format!("{column}.fst"));
        let metadata_path = index_dir.join("metadata.txt");

        if !index_path.exists() || !metadata_path.exists() {
            return Ok(true);
        }

        // Check if the file has been modified since the index was created
        let file_metadata = std::fs::metadata(file_path)?;
        let mtime = file_metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let size = file_metadata.len();

        let metadata_content = std::fs::read_to_string(&metadata_path)?;
        let lines: Vec<&str> = metadata_content.lines().collect();

        if lines.len() < 3 {
            return Ok(true); // Old metadata format
        }

        let stored_mtime = lines[1].parse::<u64>().unwrap_or(0);
        let stored_size = lines[2].parse::<u64>().unwrap_or(0);

        Ok(mtime != stored_mtime || size != stored_size)
    }

    /// Prune FST indices for Parquet files that no longer exist.
    pub fn prune_orphans(&self) -> Result<usize> {
        let mut pruned_count = 0;
        let index_root = Path::new(&self.output_dir);

        if !index_root.exists() {
            return Ok(0);
        }

        for entry in std::fs::read_dir(index_root)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let metadata_path = path.join("metadata.txt");
                if metadata_path.exists() {
                    let contents = std::fs::read_to_string(&metadata_path)?;
                    if let Some(original_path_str) = contents.lines().next() {
                        let original_path = Path::new(original_path_str);
                        if !original_path.exists() {
                            println!("Pruning orphan index: {}", path.display());
                            std::fs::remove_dir_all(&path)?;
                            pruned_count += 1;
                        }
                    }
                } else {
                    // Directory without metadata, arguably an orphan or corrupted
                    println!(
                        "Pruning index directory without metadata: {}",
                        path.display()
                    );
                    std::fs::remove_dir_all(&path)?;
                    pruned_count += 1;
                }
            }
        }

        Ok(pruned_count)
    }

    /// Creates an FST index for a specific column in a single Parquet file.
    ///
    /// For each row group in the file, extracts unique values from the specified column
    /// and builds a combined FST index. Keys in the index are formatted as `value\x00rgN`
    /// where N is the row group index, allowing precise row group lookups.
    ///
    /// # Optimizations
    ///
    /// This implementation minimizes memory allocations through:
    /// - Pre-allocating a reusable buffer for key construction
    /// - Avoiding `format!()` macro overhead by writing directly to buffer
    /// - Using `BTreeSet` for automatic sorting and deduplication (O(log n) insertion)
    /// - Single pass through row groups with streaming output to FST builder
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

        // BTreeSet provides:
        // 1. Automatic deduplication (no need for separate dedup() call)
        // 2. Automatic sorting (O(log n) insertions instead of O(n log n) sort)
        // 3. Iterator in sorted order for FST builder
        let mut keys: BTreeSet<String> = BTreeSet::new();

        // Pre-allocate a reusable buffer for constructing keys
        // This avoids allocating a new String for every value
        let mut key_buffer = String::with_capacity(buffer_config::KEY_BUFFER_INITIAL_CAPACITY);

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
                        .downcast_ref::<datafusion::arrow::array::StringArray>();

                    if let Some(arr) = string_array {
                        for value in arr.iter().flatten() {
                            // Reuse the buffer to minimize allocations
                            key_buffer.clear();

                            // Build key: "value{separator}rg{row_group_idx}"
                            // Using direct operations instead of format!() to avoid macro overhead
                            key_buffer.push_str(value);
                            key_buffer.push(key_format::VALUE_RG_SEPARATOR);
                            key_buffer.push_str(key_format::ROW_GROUP_PREFIX);
                            Self::append_usize(&mut key_buffer, row_group_idx);

                            // BTreeSet handles insertion, sorting, and deduplication
                            // The clone here is unavoidable since BTreeSet takes ownership
                            keys.insert(key_buffer.clone());
                        }
                    }
                }
            }
        }

        let file_hash = crate::calculate_file_hash(&file_path.to_string_lossy())?;
        let index_dir = PathBuf::from(&self.output_dir).join(&file_hash);
        create_dir_all(&index_dir)?;

        // Save metadata for modification detection and path resolution
        let file_metadata = std::fs::metadata(file_path)?;
        let mtime = file_metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let size = file_metadata.len();

        let metadata_path = index_dir.join("metadata.txt");
        let metadata_content = format!("{}\n{}\n{}\n", file_path.to_string_lossy(), mtime, size);
        std::fs::write(&metadata_path, metadata_content.as_bytes())?;

        let index_path = index_dir.join(format!("{column}.fst"));
        let index_file = File::create(&index_path)?;
        let mut writer = BufWriter::new(index_file);

        let mut builder = SetBuilder::new(&mut writer)?;
        // BTreeSet iterator is already in sorted order, perfect for FST builder
        for key in keys {
            builder.insert(key)?;
        }
        builder.finish()?;

        println!("Created index: {}", index_path.display());
        Ok(())
    }

    /// Append a usize to the buffer using standard formatting.
    ///
    /// This avoids the overhead of the `format!()` macro by writing
    /// directly into the string buffer.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut buf = String::from("rg");
    /// Indexer::append_usize(&mut buf, 42);
    /// assert_eq!(buf, "rg42");
    /// ```
    #[inline]
    fn append_usize(buffer: &mut String, value: usize) {
        use std::fmt::Write;
        let _ = write!(buffer, "{value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_format;

    #[test]
    fn test_append_usize() {
        let mut buffer = String::from(key_format::ROW_GROUP_PREFIX);
        Indexer::append_usize(&mut buffer, 42);
        assert_eq!(buffer, format!("{}42", key_format::ROW_GROUP_PREFIX));

        let mut buffer = String::from(key_format::ROW_GROUP_PREFIX);
        Indexer::append_usize(&mut buffer, 0);
        assert_eq!(buffer, format!("{}0", key_format::ROW_GROUP_PREFIX));

        let mut buffer = String::from(key_format::ROW_GROUP_PREFIX);
        Indexer::append_usize(&mut buffer, 999);
        assert_eq!(buffer, format!("{}999", key_format::ROW_GROUP_PREFIX));
    }
}
