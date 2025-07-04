use crate::{calculate_file_hash, Result};
use fst::SetBuilder;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{FileReader, SerializedFileReader};
use std::fs::{create_dir_all, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct Indexer {
    output_dir: String,
}

impl Indexer {
    pub fn new(output_dir: &str) -> Self {
        Self {
            output_dir: output_dir.to_string(),
        }
    }

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
