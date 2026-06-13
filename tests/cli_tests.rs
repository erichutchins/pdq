//! End-to-end tests that drive the compiled `pdq` CLI binary.

use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use datafusion::arrow::array::StringArray;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use tempfile::TempDir;

/// Write a single-column (Utf8 `value`) Parquet file.
fn write_parquet(path: &Path, values: &[&str]) {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Utf8,
        false,
    )]));
    let mut writer =
        ArrowWriter::try_new(File::create(path).unwrap(), schema.clone(), None).unwrap();
    writer
        .write(
            &RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(values.to_vec()))])
                .unwrap(),
        )
        .unwrap();
    writer.close().unwrap();
}

/// Run the `pdq` binary with the given args.
fn pdq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdq"))
        .args(args)
        .output()
        .expect("failed to execute pdq binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn test_cli_index_search_query() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let index = tmp.path().join("index");
    fs::create_dir_all(&data).unwrap();
    write_parquet(&data.join("d.parquet"), &["needle", "other", "needle"]);

    let data = data.to_str().unwrap();
    let index = index.to_str().unwrap();

    // index
    let out = pdq(&[
        "index", "--path", data, "--column", "value", "--output", index,
    ]);
    assert!(out.status.success(), "index failed: {}", stdout(&out));

    // search (exact) — finds the term
    let out = pdq(&[
        "search",
        "--column",
        "value",
        "--term",
        "needle",
        "--index-dir",
        index,
    ]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("Found"),
        "search output: {}",
        stdout(&out)
    );

    // search (no match)
    let out = pdq(&[
        "search",
        "--column",
        "value",
        "--term",
        "absent",
        "--index-dir",
        index,
    ]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("No results found"));

    // query (exact match) returns rows as JSONL
    let out = pdq(&[
        "query",
        "--column",
        "value",
        "--term",
        "needle",
        "--index-dir",
        index,
        "--data-path",
        data,
        "--format",
        "jsonl",
    ]);
    assert!(out.status.success(), "query failed: {}", stdout(&out));
    let s = stdout(&out);
    assert!(s.contains(r#""value":"needle""#), "query output: {s}");

    // query (no match) hits the zero-I/O fast path
    let out = pdq(&[
        "query",
        "--column",
        "value",
        "--term",
        "absent",
        "--index-dir",
        index,
        "--data-path",
        data,
    ]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("ZERO-MATCH"));
}

#[test]
fn test_cli_search_prefix() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    let index = tmp.path().join("index");
    fs::create_dir_all(&data).unwrap();
    write_parquet(
        &data.join("ips.parquet"),
        &["192.168.1.1", "192.168.1.2", "10.0.0.1"],
    );

    let data = data.to_str().unwrap();
    let index = index.to_str().unwrap();

    let out = pdq(&[
        "index", "--path", data, "--column", "value", "--output", index,
    ]);
    assert!(out.status.success());

    let out = pdq(&[
        "search",
        "--column",
        "value",
        "--term",
        "192.168.1",
        "--index-dir",
        index,
        "--type",
        "prefix",
    ]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("Found"),
        "prefix search output: {}",
        stdout(&out)
    );
}

#[test]
fn test_cli_no_subcommand_errors() {
    let out = pdq(&[]);
    assert!(
        !out.status.success(),
        "expected non-zero exit with no subcommand"
    );
}
