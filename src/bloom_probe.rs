//! Benchmark-only: read Parquet bloom filters and probe them, plus a
//! byte-counting ChunkReader so the shootout can measure exact pruning I/O.
//! Compiled only under `--features shootout`.

use anyhow::Result;
use bytes::Bytes;
use datafusion::parquet::file::properties::ReaderProperties;
use datafusion::parquet::file::reader::{ChunkReader, FileReader, Length, SerializedFileReader};
use datafusion::parquet::file::serialized_reader::ReadOptionsBuilder;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use walkdir::WalkDir;

/// A ChunkReader wrapper that tallies every byte handed out, so we can report
/// exact pruning I/O independent of OS page-cache behavior.
pub struct CountingReader {
    inner: File,
    pub bytes: Arc<AtomicU64>,
}

impl CountingReader {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            inner: File::open(path)?,
            bytes: Arc::new(AtomicU64::new(0)),
        })
    }
}

pub struct CountingRead<R: Read> {
    inner: R,
    bytes: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Length for CountingReader {
    fn len(&self) -> u64 {
        self.inner.len()
    }
}

impl ChunkReader for CountingReader {
    type T = CountingRead<BufReader<File>>;

    fn get_read(&self, start: u64) -> datafusion::parquet::errors::Result<Self::T> {
        Ok(CountingRead {
            inner: self.inner.get_read(start)?,
            bytes: self.bytes.clone(),
        })
    }

    fn get_bytes(
        &self,
        start: u64,
        length: usize,
    ) -> datafusion::parquet::errors::Result<Bytes> {
        self.bytes.fetch_add(length as u64, Ordering::Relaxed);
        self.inner.get_bytes(start, length)
    }
}

/// Probe the per-row-group bloom filter for `column` against `value`.
/// Returns (matched row groups, bytes read to make the decision).
pub fn probe_bloom(path: &Path, column: &str, value: &str) -> Result<(Vec<usize>, u64)> {
    let reader = CountingReader::new(path)?;
    let counter = reader.bytes.clone();
    let options = ReadOptionsBuilder::new()
        .with_reader_properties(
            ReaderProperties::builder()
                .set_read_bloom_filter(true)
                .build(),
        )
        .build();
    let file_reader = SerializedFileReader::new_with_options(reader, options)?;
    let meta = file_reader.metadata();
    let col_idx = meta
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .position(|c| c.name() == column)
        .ok_or_else(|| anyhow::anyhow!("column {column} not found"))?;

    let mut matched = Vec::new();
    for rg in 0..meta.num_row_groups() {
        let rg_reader = file_reader.get_row_group(rg)?;
        if let Some(sbbf) = rg_reader.get_column_bloom_filter(col_idx) {
            if sbbf.check(&value) {
                matched.push(rg);
            }
        }
    }
    Ok((matched, counter.load(Ordering::Relaxed)))
}

/// Total on-disk size of all `<column>.fst` files PDQ must consult — the
/// FST footprint scanned to make the pruning decision.
pub fn fst_index_bytes(index_dir: &Path, column: &str) -> Result<u64> {
    let target = format!("{column}.fst");
    let mut total = 0u64;
    for entry in WalkDir::new(index_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_name().to_string_lossy() == target {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus_gen::{SINGLE_NEEDLE, write_corpus};
    use crate::index::Indexer;

    #[test]
    fn bloom_probe_finds_planted_value() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        write_corpus(&data, 2, 2, 300, 11).unwrap();
        // Needle is in the last file (part_00001), last row group.
        let file = data.join("part_00001.parquet");
        let (rgs, bytes) = probe_bloom(&file, "src_ip", SINGLE_NEEDLE).unwrap();
        assert!(!rgs.is_empty(), "bloom should report a candidate row group");
        assert!(bytes > 0, "probing must read some bytes");
    }

    #[test]
    fn bloom_probe_misses_absent_value() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        write_corpus(&data, 1, 2, 300, 5).unwrap();
        let file = data.join("part_00000.parquet");
        // A value never planted and astronomically unlikely as random filler.
        let (rgs, _) = probe_bloom(&file, "src_ip", "203.0.113.255zzz").unwrap();
        assert!(rgs.is_empty(), "absent value should not match any row group");
    }

    #[test]
    fn fst_bytes_sums_column_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let index = dir.path().join("index");
        write_corpus(&data, 2, 2, 300, 11).unwrap();
        Indexer::new(index.to_str().unwrap())
            .build_index(&data, "src_ip")
            .unwrap();
        let bytes = fst_index_bytes(&index, "src_ip").unwrap();
        assert!(bytes > 0, "FST index footprint should be non-zero");
    }
}
