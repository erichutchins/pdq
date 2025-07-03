use crate::{Result, calculate_file_hash};
use fst::{IntoStreamer, Set, Streamer};
use memmap2::Mmap;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug)]
pub struct IndexQueryEngine {
    index_dir: PathBuf,
}

impl IndexQueryEngine {
    pub fn new<P: AsRef<Path>>(index_dir: P) -> Self {
        Self {
            index_dir: index_dir.as_ref().to_path_buf(),
        }
    }

    /// Perform an exact match search across all FST indexes for a column
    /// Returns a mapping of file hashes to row group IDs that contain the search term
    pub fn exact_search(&self, column: &str, term: &str) -> Result<HashMap<String, Vec<usize>>> {
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
                    .join(format!("{}.fst", column));

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

        let file_row_groups: HashMap<String, Vec<usize>> =
            results?.into_iter().filter_map(|x| x).collect();

        Ok(file_row_groups)
    }

    /// Search a specific FST index file for a term
    /// Returns the row group IDs that contain the term
    fn search_index(&self, index_path: &Path, term: &str) -> Result<Vec<usize>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        let mut row_groups = Vec::new();

        // Create range query bounds for exact match
        // We want all keys that start with "term\x00" but not "term\x01"
        let start_key = format!("{}\x00", term);
        let end_key = format!("{}\x01", term);

        let mut stream = set
            .range()
            .ge(start_key.as_bytes())
            .lt(end_key.as_bytes())
            .into_stream();

        while let Some(key) = stream.next() {
            if let Some(row_group_id) = self.parse_row_group_from_key(key)? {
                row_groups.push(row_group_id);
            }
        }

        // Sort and deduplicate row groups
        row_groups.sort_unstable();
        row_groups.dedup();

        Ok(row_groups)
    }

    /// Parse row group ID from FST key
    /// Key format: "value\x00rg<row_group_id>"
    fn parse_row_group_from_key(&self, key: &[u8]) -> Result<Option<usize>> {
        let key_str = String::from_utf8(key.to_vec())?;

        // Find the null separator
        if let Some(null_pos) = key_str.find('\x00') {
            let row_group_part = &key_str[null_pos + 1..];

            // Parse "rg<number>" format
            if let Some(rg_prefix_pos) = row_group_part.find("rg") {
                let number_str = &row_group_part[rg_prefix_pos + 2..];
                return Ok(Some(number_str.parse::<usize>()?));
            }
        }

        Ok(None)
    }

    /// Prefix search - find all entries that start with the given prefix
    pub fn prefix_search(&self, column: &str, prefix: &str) -> Result<HashMap<String, Vec<usize>>> {
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
                    .join(format!("{}.fst", column));

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

        let file_row_groups: HashMap<String, Vec<usize>> =
            results?.into_iter().filter_map(|x| x).collect();

        Ok(file_row_groups)
    }

    /// Prefix search within a specific FST index
    fn prefix_search_index(&self, index_path: &Path, prefix: &str) -> Result<Vec<usize>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        let mut row_groups = Vec::new();

        // For prefix search, we want all keys that start with the prefix
        // Start with prefix followed by null separator
        let start_key = format!("{}\x00", prefix);

        // End with prefix followed by the next possible character + null
        // We increment the last character of the prefix to get the upper bound
        let end_key = if let Some(last_char) = prefix.chars().last() {
            let mut end_prefix = prefix.to_string();
            end_prefix.pop();
            end_prefix.push(char::from_u32(last_char as u32 + 1).unwrap_or('\u{10FFFF}'));
            format!("{}\x00", end_prefix)
        } else {
            "\x01".to_string()
        };

        let mut stream = set
            .range()
            .ge(start_key.as_bytes())
            .lt(end_key.as_bytes())
            .into_stream();

        while let Some(key) = stream.next() {
            // Verify the key actually matches our prefix
            let key_str = String::from_utf8(key.to_vec())?;
            if let Some(null_pos) = key_str.find('\x00') {
                let value_part = &key_str[..null_pos];
                if value_part.starts_with(prefix) {
                    if let Some(row_group_id) = self.parse_row_group_from_key(key)? {
                        row_groups.push(row_group_id);
                    }
                }
            }
        }

        row_groups.sort_unstable();
        row_groups.dedup();

        Ok(row_groups)
    }

    /// Range search - find all entries between start and end values (inclusive)
    pub fn range_search(
        &self,
        column: &str,
        start: &str,
        end: &str,
    ) -> Result<HashMap<String, Vec<usize>>> {
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
                    .join(format!("{}.fst", column));

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

        let file_row_groups: HashMap<String, Vec<usize>> =
            results?.into_iter().filter_map(|x| x).collect();

        Ok(file_row_groups)
    }

    /// Range search within a specific FST index
    fn range_search_index(&self, index_path: &Path, start: &str, end: &str) -> Result<Vec<usize>> {
        let file = File::open(index_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let set = Set::new(mmap)?;

        let mut row_groups = Vec::new();

        // Range search from start\x00 to end\x01 (to include end)
        let start_key = format!("{}\x00", start);
        let end_key = format!("{}\x01", end);

        let mut stream = set
            .range()
            .ge(start_key.as_bytes())
            .lt(end_key.as_bytes())
            .into_stream();

        while let Some(key) = stream.next() {
            if let Some(row_group_id) = self.parse_row_group_from_key(key)? {
                row_groups.push(row_group_id);
            }
        }

        row_groups.sort_unstable();
        row_groups.dedup();

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
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "parquet"))
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

        let hash_to_path: HashMap<String, PathBuf> =
            results?.into_iter().filter_map(|x| x).collect();

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
                if path.extension().map_or(false, |ext| ext == "fst") {
                    if let Some(column_name) = path.file_stem().and_then(|s| s.to_str()) {
                        columns.push(column_name.to_string());
                    }
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
        builder.insert("1.2.3.4\x00rg0")?;
        builder.insert("1.2.3.4\x00rg1")?;
        builder.insert("5.6.7.8\x00rg0")?;
        builder.finish()?;
        drop(writer);

        let engine = IndexQueryEngine::new(&index_dir);
        let results = engine.exact_search("src_ip", "1.2.3.4")?;

        assert_eq!(results.len(), 1);
        assert!(results.contains_key(file_hash));

        let row_groups = results.get(file_hash).unwrap();
        assert_eq!(row_groups.len(), 2);
        assert!(row_groups.contains(&0));
        assert!(row_groups.contains(&1));

        Ok(())
    }

    #[test]
    fn test_parse_row_group_from_key() -> Result<()> {
        let engine = IndexQueryEngine::new("/tmp");

        let key = "1.2.3.4\x00rg5".as_bytes();
        let result = engine.parse_row_group_from_key(key)?;
        assert_eq!(result, Some(5));

        let key = "invalid_key".as_bytes();
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
        builder.insert("192.168.1.1\x00rg0")?;
        builder.insert("192.168.1.2\x00rg1")?;
        builder.insert("192.168.2.1\x00rg0")?;
        builder.insert("10.0.0.1\x00rg2")?;
        builder.finish()?;
        drop(writer);

        let engine = IndexQueryEngine::new(&index_dir);
        let results = engine.prefix_search("src_ip", "192.168.1")?;

        assert_eq!(results.len(), 1);
        let row_groups = results.get(file_hash).unwrap();
        assert_eq!(row_groups.len(), 2);
        assert!(row_groups.contains(&0));
        assert!(row_groups.contains(&1));

        Ok(())
    }
}
