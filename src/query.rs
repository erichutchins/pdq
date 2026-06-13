use crate::calculate_file_hash;
use crate::key_format;
use anyhow::Result;
use fst::{IntoStreamer, Set, Streamer};
use memmap2::Mmap;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Represents matching results for a single file
#[derive(Debug, Clone)]
pub struct FileMatches {
    /// Actual path to the Parquet file
    pub file_path: PathBuf,
    /// Row groups within the file that contain matching values
    pub row_groups: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct IndexQueryEngine {
    index_dir: PathBuf,
    /// Maps file hash to actual file path for result resolution
    file_map: Arc<HashMap<String, PathBuf>>,
    /// Maps file hash to the total number of row groups in the Parquet file.
    /// Recorded at index time (metadata.txt line 4) so queries can build a
    /// ParquetAccessPlan without re-reading the Parquet footer.
    row_group_counts: Arc<HashMap<String, usize>>,
}

impl IndexQueryEngine {
    pub fn new<P: AsRef<Path>>(index_dir: P) -> Self {
        let index_dir = index_dir.as_ref().to_path_buf();
        let (file_map, row_group_counts) = Self::build_file_map(&index_dir);

        Self {
            index_dir,
            file_map: Arc::new(file_map),
            row_group_counts: Arc::new(row_group_counts),
        }
    }

    /// Total row group count recorded for a file hash at index time, if known.
    pub fn num_row_groups(&self, file_hash: &str) -> Option<usize> {
        self.row_group_counts.get(file_hash).copied()
    }

    /// Build file-hash → path and file-hash → row-group-count maps from the index directory.
    fn build_file_map(index_dir: &Path) -> (HashMap<String, PathBuf>, HashMap<String, usize>) {
        let mut map = HashMap::new();
        let mut counts = HashMap::new();

        // Read the index directory structure
        if let Ok(entries) = std::fs::read_dir(index_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let file_hash = entry.file_name().to_string_lossy().to_string();

                    // Check if there's a metadata file that stores the original path
                    let metadata_path = entry.path().join("metadata.txt");
                    if let Ok(contents) = std::fs::read_to_string(&metadata_path) {
                        let mut lines = contents.lines();
                        if let Some(original_path) = lines.next() {
                            map.insert(file_hash.clone(), PathBuf::from(original_path));
                            // Line 4 (index 3) holds the row group count, when present.
                            if let Some(count) = lines.nth(2).and_then(|l| l.parse::<usize>().ok())
                            {
                                counts.insert(file_hash, count);
                            }
                            continue;
                        }
                    }

                    // Fallback: use hash as identifier (for backward compatibility)
                    map.insert(
                        file_hash.clone(),
                        PathBuf::from(format!("file-{}", file_hash)),
                    );
                }
            }
        }

        (map, counts)
    }

    /// Perform an exact match search across all FST indexes for a column
    /// Returns file matches with actual paths and row group IDs that contain the search term
    pub fn exact_search(&self, column: &str, term: &str) -> Result<Vec<FileMatches>> {
        // Collect directory entries first for parallel processing
        let entries: Vec<_> = WalkDir::new(&self.index_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .collect();

        // Process FST files in parallel
        let results: Result<Vec<_>> = entries
            .par_iter()
            .map(|entry| {
                let file_hash = entry.file_name().to_string_lossy().to_string();
                let index_path = self
                    .index_dir
                    .join(&file_hash)
                    .join(format!("{column}.fst"));

                if index_path.exists() {
                    let row_groups = self.search_index(&index_path, term)?;
                    if !row_groups.is_empty() {
                        Ok(Some((file_hash, row_groups)))
                    } else {
                        Ok(None)
                    }
                } else {
                    Ok(None)
                }
            })
            .collect();

        let file_row_groups: HashMap<String, Vec<usize>> = results?.into_iter().flatten().collect();

        // Convert to FileMatches with actual paths
        let matches = file_row_groups
            .into_iter()
            .map(|(file_hash, row_groups)| {
                let file_path = self
                    .file_map
                    .get(&file_hash)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(format!("file-{}", file_hash)));

                FileMatches {
                    file_path,
                    row_groups,
                }
            })
            .collect();

        Ok(matches)
    }

    /// Search a specific FST index file for a term
    /// Returns the row group IDs that contain the term
    fn search_index(&self, index_path: &Path, term: &str) -> Result<Vec<usize>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        // Use HashSet for O(n) deduplication instead of sort + dedup
        let mut row_groups = HashSet::new();

        // Create range query bounds for exact match
        // We want all keys that start with "term\x00" but not "term\x01"
        let start_key = format!("{term}{}", key_format::VALUE_RG_SEPARATOR);
        let end_key = format!("{term}{}", key_format::RANGE_UPPER_BOUND_MARKER);

        let mut stream = set
            .range()
            .ge(start_key.as_bytes())
            .lt(end_key.as_bytes())
            .into_stream();

        while let Some(key) = stream.next() {
            if let Some(row_group_id) = self.parse_row_group_from_key(key)? {
                row_groups.insert(row_group_id);
            }
        }

        // Convert to sorted Vec for consistent output
        let mut row_groups: Vec<_> = row_groups.into_iter().collect();
        row_groups.sort_unstable();

        Ok(row_groups)
    }

    /// Parse row group ID from FST key using zero-copy byte operations
    /// Key format: "value\x00rg<row_group_id>"
    ///
    /// This implementation avoids allocating the full key string by working with bytes directly.
    /// We only convert the numeric part to UTF-8 for parsing, not the entire key.
    /// This is especially important in hot paths with many FST results.
    fn parse_row_group_from_key(&self, key: &[u8]) -> Result<Option<usize>> {
        // Find the separator byte from the right (handles values with embedded null bytes)
        // TODO: For hot paths with many keys, consider `memchr::memrchr()` for SIMD-optimized reverse search
        // Current implementation is sufficient for typical FST key lengths (< 100 bytes)
        let separator_byte = key_format::VALUE_RG_SEPARATOR as u8;
        // Search from the right to find the separator before row group metadata
        // This handles edge cases where the value itself might contain null bytes
        let null_pos = match key.iter().rposition(|&b| b == separator_byte) {
            Some(pos) => pos,
            None => return Ok(None), // No separator found - not a row group key
        };

        let row_group_bytes = &key[null_pos + 1..];

        // Validate that the row group section starts with the expected prefix '\x00rg'
        // This ensures we're parsing the correct format and not a malformed key
        if row_group_bytes.starts_with(key_format::ROW_GROUP_PREFIX.as_bytes()) {
            // Parse the row group number from bytes after "rg"
            let num_bytes = &row_group_bytes[key_format::ROW_GROUP_PREFIX.len()..];

            // Convert only the number part to UTF-8, not the entire key
            let num_str = std::str::from_utf8(num_bytes)?;
            return Ok(Some(num_str.parse::<usize>()?));
        }

        // Not a valid row group key (separator found but not followed by "rg")
        Ok(None)
    }

    /// Prefix search - find all entries that start with the given prefix
    pub fn prefix_search(&self, column: &str, prefix: &str) -> Result<Vec<FileMatches>> {
        // Collect directory entries first for parallel processing
        let entries: Vec<_> = WalkDir::new(&self.index_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .collect();

        // Process FST files in parallel
        let results: Result<Vec<_>> = entries
            .par_iter()
            .map(|entry| {
                let file_hash = entry.file_name().to_string_lossy().to_string();
                let index_path = self
                    .index_dir
                    .join(&file_hash)
                    .join(format!("{column}.fst"));

                if index_path.exists() {
                    let row_groups = self.prefix_search_index(&index_path, prefix)?;
                    if !row_groups.is_empty() {
                        Ok(Some((file_hash, row_groups)))
                    } else {
                        Ok(None)
                    }
                } else {
                    Ok(None)
                }
            })
            .collect();

        let file_row_groups: HashMap<String, Vec<usize>> = results?.into_iter().flatten().collect();

        // Convert to FileMatches with actual paths
        let matches = file_row_groups
            .into_iter()
            .map(|(file_hash, row_groups)| {
                let file_path = self
                    .file_map
                    .get(&file_hash)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(format!("file-{}", file_hash)));

                FileMatches {
                    file_path,
                    row_groups,
                }
            })
            .collect();

        Ok(matches)
    }

    /// Prefix search within a specific FST index
    fn prefix_search_index(&self, index_path: &Path, prefix: &str) -> Result<Vec<usize>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        // Use HashSet for O(n) deduplication - important for prefix searches
        // where multiple values in the same row group may match the prefix
        let mut row_groups = HashSet::new();

        // For prefix search, we want all keys that start with the prefix
        // Start with prefix followed by null separator
        let start_key = format!("{prefix}{}", key_format::VALUE_RG_SEPARATOR);

        // End with prefix followed by the next possible character + null
        // We increment the last character of the prefix to get the upper bound
        let end_key = if let Some(last_char) = prefix.chars().last() {
            let mut end_prefix = prefix.to_string();
            end_prefix.pop();
            end_prefix
                .push(char::from_u32(last_char as u32 + 1).unwrap_or(key_format::MAX_UNICODE_CHAR));
            format!("{end_prefix}{}", key_format::VALUE_RG_SEPARATOR)
        } else {
            key_format::RANGE_UPPER_BOUND_MARKER.to_string()
        };

        let mut stream = set
            .range()
            .ge(start_key.as_bytes())
            .lt(end_key.as_bytes())
            .into_stream();

        while let Some(key) = stream.next() {
            // Verify the key actually matches our prefix using byte operations
            // Find the null separator without allocating the full string
            if let Some(null_pos) = key
                .iter()
                .position(|&b| b == key_format::VALUE_RG_SEPARATOR as u8)
            {
                let value_bytes = &key[..null_pos];
                // Check if the value part starts with the prefix
                if value_bytes.len() >= prefix.len()
                    && value_bytes.starts_with(prefix.as_bytes())
                    && let Some(row_group_id) = self.parse_row_group_from_key(key)?
                {
                    row_groups.insert(row_group_id);
                }
            }
        }

        // Convert to sorted Vec for consistent output
        let mut row_groups: Vec<_> = row_groups.into_iter().collect();
        row_groups.sort_unstable();

        Ok(row_groups)
    }

    /// Range search - find all entries between start and end values (inclusive)
    pub fn range_search(&self, column: &str, start: &str, end: &str) -> Result<Vec<FileMatches>> {
        // Collect directory entries first for parallel processing
        let entries: Vec<_> = WalkDir::new(&self.index_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .collect();

        // Process FST files in parallel
        let results: Result<Vec<_>> = entries
            .par_iter()
            .map(|entry| {
                let file_hash = entry.file_name().to_string_lossy().to_string();
                let index_path = self
                    .index_dir
                    .join(&file_hash)
                    .join(format!("{column}.fst"));

                if index_path.exists() {
                    let row_groups = self.range_search_index(&index_path, start, end)?;
                    if !row_groups.is_empty() {
                        Ok(Some((file_hash, row_groups)))
                    } else {
                        Ok(None)
                    }
                } else {
                    Ok(None)
                }
            })
            .collect();

        let file_row_groups: HashMap<String, Vec<usize>> = results?.into_iter().flatten().collect();

        // Convert to FileMatches with actual paths
        let matches = file_row_groups
            .into_iter()
            .map(|(file_hash, row_groups)| {
                let file_path = self
                    .file_map
                    .get(&file_hash)
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from(format!("file-{}", file_hash)));

                FileMatches {
                    file_path,
                    row_groups,
                }
            })
            .collect();

        Ok(matches)
    }

    /// Range search within a specific FST index
    fn range_search_index(&self, index_path: &Path, start: &str, end: &str) -> Result<Vec<usize>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        // Use HashSet for O(n) deduplication - important for range searches
        // where multiple values in the same row group may match the range
        let mut row_groups = HashSet::new();

        // Range search from start\x00 to end\x01 (to include end)
        let start_key = format!("{start}{}", key_format::VALUE_RG_SEPARATOR);
        let end_key = format!("{end}{}", key_format::RANGE_UPPER_BOUND_MARKER);

        let mut stream = set
            .range()
            .ge(start_key.as_bytes())
            .lt(end_key.as_bytes())
            .into_stream();

        while let Some(key) = stream.next() {
            if let Some(row_group_id) = self.parse_row_group_from_key(key)? {
                row_groups.insert(row_group_id);
            }
        }

        // Convert to sorted Vec for consistent output
        let mut row_groups: Vec<_> = row_groups.into_iter().collect();
        row_groups.sort_unstable();

        Ok(row_groups)
    }

    /// Get a mapping from file hash to actual file path
    /// This resolves the file hash back to the original parquet file path
    pub fn resolve_file_paths(
        &self,
        file_hashes: &[String],
        data_dir: &Path,
    ) -> Result<HashMap<String, PathBuf>> {
        // Collect parquet files first for parallel processing
        let parquet_files: Vec<_> = WalkDir::new(data_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "parquet"))
            .collect();

        // Process files in parallel to compute hashes
        let results: Result<Vec<_>> = parquet_files
            .par_iter()
            .map(|entry| {
                let file_path = entry.path();
                let file_hash = calculate_file_hash(&file_path.to_string_lossy())?;

                if file_hashes.contains(&file_hash) {
                    Ok(Some((file_hash, file_path.to_path_buf())))
                } else {
                    Ok(None)
                }
            })
            .collect();

        let hash_to_path: HashMap<String, PathBuf> = results?.into_iter().flatten().collect();

        Ok(hash_to_path)
    }

    /// Get all available columns for a given file hash
    pub fn get_available_columns(&self, file_hash: &str) -> Result<Vec<String>> {
        let file_hash_dir = self.index_dir.join(file_hash);
        let mut columns = Vec::new();

        if file_hash_dir.exists() {
            for entry in std::fs::read_dir(&file_hash_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "fst")
                    && let Some(column_name) = path.file_stem().and_then(|s| s.to_str())
                {
                    columns.push(column_name.to_string());
                }
            }
        }

        columns.sort();
        Ok(columns)
    }

    /// List all indexed files (file hashes)
    pub fn list_indexed_files(&self) -> Result<Vec<String>> {
        // Collect directory entries and process in parallel
        let file_hashes: Vec<String> = WalkDir::new(&self.index_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .collect::<Vec<_>>()
            .par_iter()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();

        Ok(file_hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fst::SetBuilder;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_exact_search() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("test-index");
        fs::create_dir_all(&index_dir)?;

        // Create a test index
        let file_hash = "test123";
        let file_hash_dir = index_dir.join(file_hash);
        fs::create_dir_all(&file_hash_dir)?;

        let index_path = file_hash_dir.join("src_ip.fst");
        let index_file = File::create(&index_path)?;
        let mut writer = std::io::BufWriter::new(index_file);

        let mut builder = SetBuilder::new(&mut writer)?;
        builder.insert(format!("1.2.3.4{}rg0", key_format::VALUE_RG_SEPARATOR))?;
        builder.insert(format!("1.2.3.4{}rg1", key_format::VALUE_RG_SEPARATOR))?;
        builder.insert(format!("5.6.7.8{}rg0", key_format::VALUE_RG_SEPARATOR))?;
        builder.finish()?;
        drop(writer);

        let engine = IndexQueryEngine::new(&index_dir);
        let results = engine.exact_search("src_ip", "1.2.3.4")?;

        assert_eq!(results.len(), 1);
        // results is a Vec<FileMatches>, verify we got the expected match
        let match_result = &results[0];
        // expected path is constructed in build_file_map default case
        assert_eq!(
            match_result.file_path,
            PathBuf::from(format!("file-{}", file_hash))
        );

        let row_groups = &match_result.row_groups;
        assert_eq!(row_groups.len(), 2);
        assert!(row_groups.contains(&0));
        assert!(row_groups.contains(&1));

        Ok(())
    }

    #[test]
    fn test_parse_row_group_from_key() -> Result<()> {
        let engine = IndexQueryEngine::new("/tmp");

        // Standard case: simple value with separator and row group
        let key = "1.2.3.4\x00rg5".as_bytes();
        let result = engine.parse_row_group_from_key(key)?;
        assert_eq!(result, Some(5));

        // Invalid case: no separator
        let key = "invalid_key".as_bytes();
        let result = engine.parse_row_group_from_key(key)?;
        assert_eq!(result, None);

        // Edge case: value contains null bytes - should find the rightmost separator
        let key = b"value\x00with\x00nulls\x00rg42";
        let result = engine.parse_row_group_from_key(key)?;
        assert_eq!(result, Some(42));

        // Invalid case: separator but no "rg" prefix
        let key = "value\x00notrowgroup".as_bytes();
        let result = engine.parse_row_group_from_key(key)?;
        assert_eq!(result, None);

        Ok(())
    }

    #[test]
    fn test_prefix_search() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("test-index");
        fs::create_dir_all(&index_dir)?;

        let file_hash = "test456";
        let file_hash_dir = index_dir.join(file_hash);
        fs::create_dir_all(&file_hash_dir)?;

        let index_path = file_hash_dir.join("src_ip.fst");
        let index_file = File::create(&index_path)?;
        let mut writer = std::io::BufWriter::new(index_file);

        let mut builder = SetBuilder::new(&mut writer)?;
        builder.insert(format!("10.0.0.1{}rg2", key_format::VALUE_RG_SEPARATOR))?;
        builder.insert(format!("192.168.1.1{}rg0", key_format::VALUE_RG_SEPARATOR))?;
        builder.insert(format!("192.168.1.2{}rg1", key_format::VALUE_RG_SEPARATOR))?;
        builder.insert(format!("192.168.2.1{}rg0", key_format::VALUE_RG_SEPARATOR))?;
        builder.finish()?;
        drop(writer);

        let engine = IndexQueryEngine::new(&index_dir);
        let results = engine.prefix_search("src_ip", "192.168.1")?;

        assert_eq!(results.len(), 1);
        // results is a Vec<FileMatches>
        let match_result = &results[0];
        assert_eq!(
            match_result.file_path,
            PathBuf::from(format!("file-{}", file_hash))
        );

        let row_groups = &match_result.row_groups;
        assert_eq!(row_groups.len(), 2);
        assert!(row_groups.contains(&0));
        assert!(row_groups.contains(&1));

        Ok(())
    }

    /// Build an FST at `<index_dir>/<file_hash>/<column>.fst` from `value\x00rgN` keys.
    fn write_test_fst(
        index_dir: &Path,
        file_hash: &str,
        column: &str,
        keys: &[&str],
    ) -> Result<()> {
        let dir = index_dir.join(file_hash);
        fs::create_dir_all(&dir)?;
        let mut writer = std::io::BufWriter::new(File::create(dir.join(format!("{column}.fst")))?);
        let mut builder = SetBuilder::new(&mut writer)?;
        for key in keys {
            builder.insert(key)?;
        }
        builder.finish()?;
        Ok(())
    }

    #[test]
    fn test_range_search() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("test-index");
        fs::create_dir_all(&index_dir)?;

        let sep = key_format::VALUE_RG_SEPARATOR;
        write_test_fst(
            &index_dir,
            "rangehash",
            "code",
            &[
                &format!("100{sep}rg0"),
                &format!("150{sep}rg1"),
                &format!("200{sep}rg2"),
                &format!("250{sep}rg3"),
                &format!("300{sep}rg4"),
            ],
        )?;

        let engine = IndexQueryEngine::new(&index_dir);
        // Lexicographic range [150, 250] inclusive → row groups 1, 2, 3.
        let results = engine.range_search("code", "150", "250")?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].row_groups, vec![1, 2, 3]);

        // A range below everything matches nothing.
        assert!(engine.range_search("code", "000", "099")?.is_empty());
        Ok(())
    }

    #[test]
    fn test_get_available_columns() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("test-index");
        fs::create_dir_all(&index_dir)?;

        let sep = key_format::VALUE_RG_SEPARATOR;
        write_test_fst(&index_dir, "h1", "src_ip", &[&format!("1.2.3.4{sep}rg0")])?;
        write_test_fst(&index_dir, "h1", "dst_ip", &[&format!("5.6.7.8{sep}rg0")])?;
        // A non-fst sidecar file should be ignored.
        fs::write(index_dir.join("h1").join("metadata.txt"), "x\n")?;

        let engine = IndexQueryEngine::new(&index_dir);
        let columns = engine.get_available_columns("h1")?;
        assert_eq!(columns, vec!["dst_ip".to_string(), "src_ip".to_string()]);

        // Unknown hash → no columns.
        assert!(engine.get_available_columns("missing")?.is_empty());
        Ok(())
    }

    #[test]
    fn test_list_indexed_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("test-index");
        fs::create_dir_all(&index_dir)?;

        let sep = key_format::VALUE_RG_SEPARATOR;
        write_test_fst(&index_dir, "ha", "value", &[&format!("a{sep}rg0")])?;
        write_test_fst(&index_dir, "hb", "value", &[&format!("b{sep}rg0")])?;

        let engine = IndexQueryEngine::new(&index_dir);
        let mut hashes = engine.list_indexed_files()?;
        hashes.sort();
        assert_eq!(hashes, vec!["ha".to_string(), "hb".to_string()]);
        Ok(())
    }

    #[test]
    fn test_resolve_file_paths() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let data_dir = temp_dir.path().join("data");
        fs::create_dir_all(&data_dir)?;

        // resolve_file_paths only hashes the path string; file contents are irrelevant.
        let a = data_dir.join("a.parquet");
        let b = data_dir.join("b.parquet");
        fs::write(&a, b"")?;
        fs::write(&b, b"")?;
        let hash_a = calculate_file_hash(&a.to_string_lossy())?;

        let engine = IndexQueryEngine::new(temp_dir.path().join("index"));
        let resolved = engine.resolve_file_paths(std::slice::from_ref(&hash_a), &data_dir)?;

        assert_eq!(resolved.len(), 1, "only the requested hash should resolve");
        assert_eq!(resolved.get(&hash_a), Some(&a));
        Ok(())
    }
}
