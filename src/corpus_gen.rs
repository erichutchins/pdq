//! Benchmark-only: write a Parquet corpus *with per-column bloom filters* and
//! deterministically planted needles, for the PDQ vs. bloom-filter shootout.
//!
//! pyarrow (any version through 23) and DuckDB cannot emit Parquet bloom
//! filters from Python, so the shootout corpus is written here with the
//! arrow-rs `parquet` writer (reachable via datafusion). Compiled only under
//! `--features shootout`.

use anyhow::Result;
use datafusion::arrow::array::{Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use datafusion::parquet::schema::types::ColumnPath;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// Values planted at fixed positions; must match `gen_data.py`'s manifest.
pub const SINGLE_NEEDLE: &str = "192.168.133.7";
pub const PREFIX_VALUE: &str = "192.168.133.42";
/// Reverse-label encoding of `www.evil.com`.
pub const DOMAIN_VALUE: &str = "com.evil.www";

const BLOOM_COLUMNS: &[&str] = &["src_ip", "dst_ip", "domain_rev", "email"];

/// Tiny xorshift64 RNG — deterministic filler, no external crate needed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

fn rand_ip(rng: &mut Rng) -> String {
    format!(
        "{}.{}.{}.{}",
        1 + rng.below(255),
        rng.below(256),
        rng.below(256),
        1 + rng.below(255)
    )
}

fn rand_domain(rng: &mut Rng) -> String {
    let tld = ["com", "net", "org", "io"][rng.below(4) as usize];
    let name = ["alpha", "bravo", "delta", "omega", "zulu"][rng.below(5) as usize];
    let sub = ["www", "api", "mail", "cdn"][rng.below(4) as usize];
    format!("{sub}.{name}.{tld}")
}

fn reverse_domain(domain: &str) -> String {
    let mut labels: Vec<&str> = domain.split('.').collect();
    labels.reverse();
    labels.join(".")
}

fn writer_props(rows_per_group: usize) -> WriterProperties {
    let mut builder = WriterProperties::builder()
        .set_dictionary_enabled(false)
        .set_max_row_group_row_count(Some(rows_per_group));
    for col in BLOOM_COLUMNS {
        builder = builder.set_column_bloom_filter_enabled(ColumnPath::from(*col), true);
    }
    builder.build()
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("src_ip", DataType::Utf8, false),
        Field::new("dst_ip", DataType::Utf8, false),
        Field::new("domain_rev", DataType::Utf8, false),
        Field::new("email", DataType::Utf8, false),
        Field::new("bytes", DataType::Int64, false),
    ]))
}

/// Write a single row-group RecordBatch with deterministic random filler and
/// any planted overrides applied (column -> [(row, value)]).
fn build_batch(
    schema: &Arc<Schema>,
    n_rows: usize,
    rng: &mut Rng,
    injected: &[(&str, usize, &str)],
) -> Result<RecordBatch> {
    let mut src_ip: Vec<String> = (0..n_rows).map(|_| rand_ip(rng)).collect();
    let mut dst_ip: Vec<String> = (0..n_rows).map(|_| rand_ip(rng)).collect();
    let mut domain_rev: Vec<String> = (0..n_rows)
        .map(|_| reverse_domain(&rand_domain(rng)))
        .collect();
    let mut email: Vec<String> = (0..n_rows)
        .map(|_| format!("user{}@{}", rng.below(100000), rand_domain(rng)))
        .collect();
    let bytes: Vec<i64> = (0..n_rows).map(|_| 64 + rng.below(1436) as i64).collect();

    for (col, row, val) in injected {
        let target = match *col {
            "src_ip" => &mut src_ip,
            "dst_ip" => &mut dst_ip,
            "domain_rev" => &mut domain_rev,
            "email" => &mut email,
            other => anyhow::bail!("cannot inject into unknown column {other}"),
        };
        target[*row] = (*val).to_string();
    }

    Ok(RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(src_ip)),
            Arc::new(StringArray::from(dst_ip)),
            Arc::new(StringArray::from(domain_rev)),
            Arc::new(StringArray::from(email)),
            Arc::new(Int64Array::from(bytes)),
        ],
    )?)
}

/// Write `n_files` Parquet files, each with `row_groups_per_file` row groups of
/// `rows_per_group` rows, bloom filters on the indexed columns, and needles
/// planted at the fixed positions the Python manifest records:
///   - every file, row group 0, row 0: a unique multi-IOC `10.<hi>.<lo>.7`
///   - file 0, row group 0, row 1: the prefix sample `192.168.133.42`
///   - file 0, row group 0, row 0 (domain_rev): `com.evil.www`
///   - last file, last row group, row 2: the single needle `192.168.133.7`
pub fn write_corpus(
    data_dir: &Path,
    n_files: usize,
    row_groups_per_file: usize,
    rows_per_group: usize,
    seed: u64,
) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let schema = schema();
    let needle_file = n_files - 1;
    let needle_rg = row_groups_per_file - 1;

    for fi in 0..n_files {
        let path = data_dir.join(format!("part_{fi:05}.parquet"));
        let file = File::create(&path)?;
        let mut writer =
            ArrowWriter::try_new(file, schema.clone(), Some(writer_props(rows_per_group)))?;
        // Per-file RNG seed so files are independent but reproducible.
        let mut rng = Rng::new(seed.wrapping_add(fi as u64).wrapping_mul(0x9E3779B97F4A7C15));

        for rgi in 0..row_groups_per_file {
            let multi_ioc;
            let mut injected: Vec<(&str, usize, &str)> = Vec::new();
            if rgi == 0 {
                multi_ioc = format!("10.{}.{}.7", fi / 256, fi % 256);
                injected.push(("src_ip", 0, multi_ioc.as_str()));
            }
            if fi == 0 && rgi == 0 {
                injected.push(("src_ip", 1, PREFIX_VALUE));
                injected.push(("domain_rev", 0, DOMAIN_VALUE));
            }
            if fi == needle_file && rgi == needle_rg {
                injected.push(("src_ip", 2, SINGLE_NEEDLE));
            }
            let batch = build_batch(&schema, rows_per_group, &mut rng, &injected)?;
            writer.write(&batch)?;
            writer.flush()?; // force a row-group boundary at exactly rows_per_group
        }
        writer.close()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::parquet::file::properties::ReaderProperties;
    use datafusion::parquet::file::reader::{FileReader, SerializedFileReader};
    use datafusion::parquet::file::serialized_reader::ReadOptionsBuilder;

    #[test]
    fn writes_blooms_and_plants_needle() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        write_corpus(&data, 2, 2, 300, 11).unwrap();

        // Last file, last row group should contain the needle and a readable bloom.
        let path = data.join("part_00001.parquet");
        let options = ReadOptionsBuilder::new()
            .with_reader_properties(
                ReaderProperties::builder()
                    .set_read_bloom_filter(true)
                    .build(),
            )
            .build();
        let reader =
            SerializedFileReader::new_with_options(File::open(&path).unwrap(), options).unwrap();
        let meta = reader.metadata();
        let col_idx = meta
            .file_metadata()
            .schema_descr()
            .columns()
            .iter()
            .position(|c| c.name() == "src_ip")
            .unwrap();
        let last_rg = meta.num_row_groups() - 1;
        let rg = reader.get_row_group(last_rg).unwrap();
        let sbbf = rg
            .get_column_bloom_filter(col_idx)
            .expect("bloom filter should be present");
        assert!(sbbf.check(&SINGLE_NEEDLE));
    }
}
