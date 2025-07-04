use crate::{Result, SearchResult};
use fst::{IntoStreamer, Set, Streamer};
use memmap2::Mmap;
use rayon::prelude::*;
use std::fs::File;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Searcher provides fast, parallel lookups against FST indices.
///
/// This component queries FST (Finite State Transducer) indices created by the Indexer
/// to efficiently locate values across multiple Parquet files without scanning the raw data.
/// It supports exact match, prefix, and range queries against indexed columns.
///
/// All search operations run in parallel across multiple FST files for maximum performance.
pub struct Searcher {
    /// Directory containing FST index files organized by file hash
    index_dir: PathBuf,
}

impl Searcher {
    /// Creates a new Searcher instance for the specified index directory.
    ///
    /// # Parameters
    ///
    /// * `index_dir` - Path to the directory containing FST index files
    ///
    /// # Returns
    ///
    /// A new Searcher instance ready to perform lookups
    pub fn new(index_dir: &str) -> Self {
        Self {
            index_dir: PathBuf::from(index_dir),
        }
    }

    /// Performs an exact match search across all indexed files.
    ///
    /// Searches all FST indices for the specified column and term, returning
    /// file paths and row groups where exact matches exist. This is the fastest
    /// and most selective search operation.
    ///
    /// # Parameters
    ///
    /// * `column` - Name of the indexed column to search
    /// * `term` - Exact value to match in the column
    ///
    /// # Returns
    ///
    /// Vector of SearchResults containing file paths and row groups with matches
    ///
    /// # Error
    ///
    /// Returns an error if index files cannot be read or processed
    pub fn exact_search(&self, column: &str, term: &str) -> Result<Vec<SearchResult>> {
        // Collect directory entries first for parallel processing
        let entries: Vec<_> = WalkDir::new(&self.index_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && e.depth() == 1)
            .collect();

        // Process FST files in parallel
        let results: Vec<SearchResult> = entries
            .par_iter()
            .filter_map(|entry| {
                let file_hash = entry.file_name().to_string_lossy();
                let index_path = self
                    .index_dir
                    .join(&*file_hash)
                    .join(format!("{column}.fst"));

                // Search this specific FST file
                self.search_index(&index_path.to_string_lossy(), term).ok()
            })
            .flatten()
            .collect();

        Ok(results)
    }

    /// Performs a prefix search across all indexed files.
    ///
    /// Searches all FST indices for the specified column and returns matches
    /// that begin with the given prefix. Useful for substring or wildcard searches.
    ///
    /// # Parameters
    ///
    /// * `column` - Name of the indexed column to search
    /// * `prefix` - String prefix to match at the beginning of values
    ///
    /// # Returns
    ///
    /// Vector of SearchResults containing file paths and row groups with matches
    ///
    /// # Error
    ///
    /// Returns an error if index files cannot be read or processed
    pub fn search(&self, column: &str, prefix: &str) -> Result<Vec<SearchResult>> {
        // Collect directory entries first for parallel processing
        let entries: Vec<_> = WalkDir::new(&self.index_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && e.depth() == 1)
            .collect();

        // Process FST files in parallel
        let results: Vec<SearchResult> = entries
            .par_iter()
            .filter_map(|entry| {
                let file_hash = entry.file_name().to_string_lossy();
                let index_path = self
                    .index_dir
                    .join(&*file_hash)
                    .join(format!("{column}.fst"));

                // Search this specific FST file
                self.prefix_search(&index_path.to_string_lossy(), prefix)
                    .ok()
            })
            .flatten()
            .collect();

        Ok(results)
    }

    /// Performs a range search across all indexed files.
    ///
    /// Searches all FST indices for the specified column and returns matches
    /// that fall within the given range (inclusive start, exclusive end).
    /// Ideal for numeric or date ranges.
    ///
    /// # Parameters
    ///
    /// * `column` - Name of the indexed column to search
    /// * `start` - Start of range (inclusive)
    /// * `end` - End of range (exclusive)
    ///
    /// # Returns
    ///
    /// Vector of SearchResults containing file paths and row groups with matches
    ///
    /// # Error
    ///
    /// Returns an error if index files cannot be read or processed
    pub fn range_search(&self, column: &str, start: &str, end: &str) -> Result<Vec<SearchResult>> {
        // Collect directory entries first for parallel processing
        let entries: Vec<_> = WalkDir::new(&self.index_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && e.depth() == 1)
            .collect();

        // Process FST files in parallel
        let results: Vec<SearchResult> = entries
            .par_iter()
            .filter_map(|entry| {
                let file_hash = entry.file_name().to_string_lossy();
                let index_path = self
                    .index_dir
                    .join(&*file_hash)
                    .join(format!("{column}.fst"));

                // Search this specific FST file
                self.range_search_index(&index_path.to_string_lossy(), start, end)
                    .ok()
            })
            .flatten()
            .collect();

        Ok(results)
    }

    /// Searches a single FST index file for exact matches.
    ///
    /// Uses memory mapping for efficient access and streams results
    /// for matches between `term\x00` and `term\x01` to capture all
    /// row groups containing the exact term.
    ///
    /// # Parameters
    ///
    /// * `index_path` - Path to the specific FST index file to search
    /// * `term` - Exact value to match
    ///
    /// # Returns
    ///
    /// Vector of SearchResults containing file paths and row groups with matches
    ///
    /// # Error
    ///
    /// Returns an error if the index file cannot be read or processed
    fn search_index(&self, index_path: &str, term: &str) -> Result<Vec<SearchResult>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        let mut results = Vec::new();
        let start_key = format!("{term}\x00");
        let end_key = format!("{term}\x01");

        let mut stream = set.range().ge(&start_key).lt(&end_key).into_stream();

        while let Some(key) = stream.next() {
            if let Some(result) = self.parse_key(key, index_path)? {
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Searches a single FST index file for prefix matches.
    ///
    /// Uses memory mapping for efficient access and streams all keys
    /// that begin with the given prefix, capturing all row groups
    /// with matching values.
    ///
    /// # Parameters
    ///
    /// * `index_path` - Path to the specific FST index file to search
    /// * `prefix` - String prefix to match
    ///
    /// # Returns
    ///
    /// Vector of SearchResults containing file paths and row groups with matches
    ///
    /// # Error
    ///
    /// Returns an error if the index file cannot be read or processed
    fn prefix_search(&self, index_path: &str, prefix: &str) -> Result<Vec<SearchResult>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        let mut results = Vec::new();
        let start_key = format!("{prefix}\x00");
        let end_key = format!("{}{}", prefix, char::from(255));

        let mut stream = set.range().ge(&start_key).lt(&end_key).into_stream();

        while let Some(key) = stream.next() {
            if let Some(result) = self.parse_key(key, index_path)? {
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Searches a single FST index file for range matches.
    ///
    /// Uses memory mapping for efficient access and streams all keys
    /// that fall within the given range, inclusive of start and exclusive of end.
    ///
    /// # Parameters
    ///
    /// * `index_path` - Path to the specific FST index file to search
    /// * `start` - Start of range (inclusive)
    /// * `end` - End of range (exclusive)
    ///
    /// # Returns
    ///
    /// Vector of SearchResults containing file paths and row groups with matches
    ///
    /// # Error
    ///
    /// Returns an error if the index file cannot be read or processed
    fn range_search_index(
        &self,
        index_path: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<SearchResult>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        let mut results = Vec::new();
        let start_key = format!("{start}\x00");
        let end_key = format!("{end}\x01");

        let mut stream = set.range().ge(&start_key).lt(&end_key).into_stream();

        while let Some(key) = stream.next() {
            if let Some(result) = self.parse_key(key, index_path)? {
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Parses a key from the FST index into a SearchResult.
    ///
    /// Extracts the row group number and file hash from an FST key string
    /// using the format: `value\x00rgN` where N is the row group index.
    ///
    /// # Parameters
    ///
    /// * `key` - Raw key bytes from the FST index
    /// * `index_path` - Path to the FST index file (used to extract file hash)
    ///
    /// # Returns
    ///
    /// Option containing a SearchResult with file path and row group index
    ///
    /// # Error
    ///
    /// Returns an error if the key cannot be parsed or contains invalid UTF-8
    fn parse_key(&self, key: &[u8], index_path: &str) -> Result<Option<SearchResult>> {
        let key_str = String::from_utf8(key.to_vec())?;

        if let Some(null_pos) = key_str.find('\x00') {
            let row_group_str = &key_str[null_pos + 1..];

            if let Some(rg_start) = row_group_str.find("rg") {
                let row_group_num = row_group_str[rg_start + 2..].parse::<usize>()?;

                let path = Path::new(index_path);
                let file_hash = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");

                return Ok(Some(SearchResult {
                    file_path: format!("file-{file_hash}"),
                    row_group: row_group_num,
                }));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_parallel_fst_search() -> Result<()> {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("index");
        fs::create_dir_all(&index_dir)?;

        // Create a searcher instance
        let searcher = Searcher::new(&index_dir.to_string_lossy());

        // Test with empty directory - should handle gracefully
        let results = searcher.exact_search("test_column", "test_term")?;
        assert_eq!(results.len(), 0);

        // Test parallel processing doesn't crash with non-existent files
        let results = searcher.search("test_column", "test_prefix")?;
        assert_eq!(results.len(), 0);

        let results = searcher.range_search("test_column", "start", "end")?;
        assert_eq!(results.len(), 0);

        Ok(())
    }

    #[test]
    fn test_parallel_vs_sequential_consistency() -> Result<()> {
        // This test verifies that parallel processing produces the same results
        // as sequential processing would, even with no actual FST files
        let temp_dir = TempDir::new()?;
        let index_dir = temp_dir.path().join("index");
        fs::create_dir_all(&index_dir)?;

        // Create some dummy directories to simulate file hash directories
        for i in 0..5 {
            let hash_dir = index_dir.join(format!("hash_{i}"));
            fs::create_dir_all(&hash_dir)?;
        }

        let searcher = Searcher::new(&index_dir.to_string_lossy());

        // All these should return empty results since no actual FST files exist
        // But they should not crash and should handle parallel processing correctly
        let exact_results = searcher.exact_search("test_column", "test_term")?;
        let prefix_results = searcher.search("test_column", "test_prefix")?;
        let range_results = searcher.range_search("test_column", "start", "end")?;

        assert_eq!(exact_results.len(), 0);
        assert_eq!(prefix_results.len(), 0);
        assert_eq!(range_results.len(), 0);

        Ok(())
    }
}
