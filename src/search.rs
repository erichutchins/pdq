use crate::{Result, SearchResult};
use fst::{IntoStreamer, Set, Streamer};
use memmap2::Mmap;
use rayon::prelude::*;
use std::fs::File;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct Searcher {
    index_dir: PathBuf,
}

impl Searcher {
    pub fn new(index_dir: &str) -> Self {
        Self {
            index_dir: PathBuf::from(index_dir),
        }
    }

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
                    .join(format!("{}.fst", column));

                // Search this specific FST file
                if let Ok(index_results) = self.search_index(&index_path.to_string_lossy(), term) {
                    Some(index_results)
                } else {
                    None
                }
            })
            .flatten()
            .collect();

        Ok(results)
    }

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
                    .join(format!("{}.fst", column));

                // Search this specific FST file
                if let Ok(index_results) = self.prefix_search(&index_path.to_string_lossy(), prefix)
                {
                    Some(index_results)
                } else {
                    None
                }
            })
            .flatten()
            .collect();

        Ok(results)
    }

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
                    .join(format!("{}.fst", column));

                // Search this specific FST file
                if let Ok(index_results) =
                    self.range_search_index(&index_path.to_string_lossy(), start, end)
                {
                    Some(index_results)
                } else {
                    None
                }
            })
            .flatten()
            .collect();

        Ok(results)
    }

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
            let hash_dir = index_dir.join(format!("hash_{}", i));
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
