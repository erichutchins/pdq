use anyhow::Result;
use arrow::csv::WriterBuilder;
use arrow::json::LineDelimitedWriter;
use arrow::record_batch::RecordBatch;
use clap::{Arg, Command};
use datafusion::{arrow::util::pretty, prelude::*};
use pdq::{index::Indexer, search::Searcher, PdqTableProviderBuilder};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("pdq")
        .version("0.1.0")
        .about("FST-based Parquet indexing and querying tool")
        .subcommand(
            Command::new("index")
                .about("Build FST index from Parquet files")
                .arg(
                    Arg::new("path")
                        .long("path")
                        .value_name("DIR")
                        .help("Directory containing Parquet files")
                        .required(true),
                )
                .arg(
                    Arg::new("column")
                        .long("column")
                        .value_name("COLUMN")
                        .help("Column name to index")
                        .required(true),
                )
                .arg(
                    Arg::new("output")
                        .long("output")
                        .value_name("DIR")
                        .help("Output directory for index files")
                        .default_value("pdq-index"),
                ),
        )
        .subcommand(
            Command::new("search")
                .about("Search the FST index")
                .arg(
                    Arg::new("column")
                        .long("column")
                        .value_name("COLUMN")
                        .help("Column name to search")
                        .required(true),
                )
                .arg(
                    Arg::new("term")
                        .long("term")
                        .value_name("TERM")
                        .help("Search term")
                        .required(true),
                )
                .arg(
                    Arg::new("index-dir")
                        .long("index-dir")
                        .value_name("DIR")
                        .help("Index directory")
                        .default_value("pdq-index"),
                )
                .arg(
                    Arg::new("type")
                        .long("type")
                        .value_name("TYPE")
                        .help("Search type: exact, prefix, or range")
                        .default_value("exact"),
                ),
        )
        .subcommand(
            Command::new("query")
                .about("Query Parquet files using DataFusion with index optimization")
                .arg(
                    Arg::new("column")
                        .long("column")
                        .value_name("COLUMN")
                        .help("Column name to search")
                        .required(true),
                )
                .arg(
                    Arg::new("term")
                        .long("term")
                        .value_name("TERM")
                        .help("Search term")
                        .required(true),
                )
                .arg(
                    Arg::new("index-dir")
                        .long("index-dir")
                        .value_name("DIR")
                        .help("Index directory")
                        .default_value("pdq-index"),
                )
                .arg(
                    Arg::new("data-path")
                        .long("data-path")
                        .value_name("DIR")
                        .help("Directory containing original Parquet files")
                        .required(true),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .value_name("FORMAT")
                        .help("Output format: csv, json, jsonl, table, or ndjson")
                        .default_value("table"),
                )
                .arg(
                    Arg::new("output")
                        .long("output")
                        .value_name("FILE")
                        .help("Write output to file instead of stdout")
                        .required(false),
                ),
        )
        .get_matches();

    match matches.subcommand() {
        Some(("index", sub_matches)) => {
            let path = sub_matches.get_one::<String>("path").unwrap();
            let column = sub_matches.get_one::<String>("column").unwrap();
            let output = sub_matches.get_one::<String>("output").unwrap();

            println!("Building index for column '{column}' from '{path}'...");

            let indexer = Indexer::new(output);
            indexer.build_index(Path::new(path), column)?;

            println!("Index built successfully in '{output}'");
        }
        Some(("search", sub_matches)) => {
            let column = sub_matches.get_one::<String>("column").unwrap();
            let term = sub_matches.get_one::<String>("term").unwrap();
            let index_dir = sub_matches.get_one::<String>("index-dir").unwrap();
            let search_type = sub_matches.get_one::<String>("type").unwrap();

            let searcher = Searcher::new(index_dir);

            let results = match search_type.as_str() {
                "exact" => searcher.exact_search(column, term)?,
                "prefix" => searcher.search(column, term)?,
                "range" => {
                    let end_term = format!("{term}~");
                    searcher.range_search(column, term, &end_term)?
                }
                _ => {
                    eprintln!("Unknown search type: {search_type}");
                    std::process::exit(1);
                }
            };

            if results.is_empty() {
                println!("No results found for term: {term}");
            } else {
                println!("Found {} matching row groups:", results.len());
                for result in results {
                    println!(
                        "  File: {}, Row Group: {}",
                        result.file_path, result.row_group
                    );
                }
            }
        }
        Some(("query", sub_matches)) => {
            let column = sub_matches.get_one::<String>("column").unwrap();
            let term = sub_matches.get_one::<String>("term").unwrap();
            let index_dir = sub_matches.get_one::<String>("index-dir").unwrap();
            let data_path = sub_matches.get_one::<String>("data-path").unwrap();
            let output_format = sub_matches.get_one::<String>("format").unwrap();
            let output_file = sub_matches.get_one::<String>("output");

            println!("🔍 PDQ Query Starting...");
            println!("   Term: '{term}' in column '{column}'");
            println!("   Index: {index_dir}");
            println!("   Data: {data_path}");

            let start_time = Instant::now();

            // First, check the index directly to show optimization in action
            let searcher = Searcher::new(index_dir);
            let index_results = searcher.exact_search(column, term)?;

            if index_results.is_empty() {
                let query_time = start_time.elapsed();
                println!("⚡ ZERO-MATCH OPTIMIZATION TRIGGERED!");
                println!("   Index lookup: {query_time:?}");
                println!("   Files scanned: 0");
                println!("   Bytes read: 0");
                println!("   Result: No matches found (authoritative from index)");
                return Ok(());
            }

            println!("📊 Index Results:");
            println!(
                "   Found {} matching row groups across {} files",
                index_results.iter().map(|_r| 1).sum::<usize>(),
                index_results
                    .iter()
                    .map(|r| &r.file_path)
                    .collect::<std::collections::HashSet<_>>()
                    .len()
            );

            for result in &index_results {
                println!("   📁 {}: row group {}", result.file_path, result.row_group);
            }

            // Create the PdqTableProvider using the builder
            let table_provider = PdqTableProviderBuilder::new()
                .with_table_name("pdq_table")
                .with_index_dir(index_dir)
                .with_data_dir(data_path)
                .build()
                .await?;

            // Create a new DataFusion context
            let ctx = SessionContext::new();

            // Register our table provider
            ctx.register_table("pdq_table", Arc::new(table_provider))?;

            // Build and execute the query
            let sql = format!("SELECT * FROM pdq_table WHERE {column} = '{term}'");
            println!("🔍 Executing SQL: {sql}");

            let df = ctx.sql(&sql).await?;

            // Execute and collect results
            let results = df.collect().await?;

            let total_time = start_time.elapsed();

            println!("🎯 Query Complete!");
            println!("   Total time: {total_time:?}");

            if results.is_empty() {
                println!("   Result: No matching records found in the data");
            } else {
                let row_count: usize = results.iter().map(|batch| batch.num_rows()).sum();
                println!("   Result: Found {row_count} matching records");

                // Handle output based on format and destination
                if output_format == "table" && output_file.is_none() {
                    // Pretty print to terminal if output is table and no file is specified
                    println!("\n📋 Output:");
                    pretty::print_batches(&results)?;
                } else {
                    // Write to file or stdout based on user preference
                    let dest: Box<dyn Write> = if let Some(file_path) = output_file {
                        println!("Writing results to file: {file_path}");
                        Box::new(BufWriter::new(File::create(file_path)?))
                    } else {
                        // Use buffered stdout with lock for performance
                        let stdout = std::io::stdout();
                        println!("\n📋 Output:");
                        Box::new(BufWriter::new(stdout.lock()))
                    };

                    // Format and write based on chosen format
                    match output_format.as_str() {
                        "csv" => write_batches_as_csv(&results, dest)?,
                        "json" | "jsonl" | "ndjson" => write_batches_as_json(&results, dest)?,
                        "table" => {
                            // If table format is requested but output is to file,
                            // default to CSV for better compatibility
                            write_batches_as_csv(&results, dest)?
                        }
                        _ => write_batches_as_json(&results, dest)?,
                    }
                }
            }
        }
        _ => {
            eprintln!("No subcommand provided. Use --help for usage information.");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Write record batches as CSV to the provided writer
fn write_batches_as_csv(batches: &[RecordBatch], writer: Box<dyn Write>) -> Result<()> {
    if batches.is_empty() {
        return Ok(());
    }

    let mut csv_writer = WriterBuilder::new().with_header(true).build(writer);

    for batch in batches {
        csv_writer.write(batch)?;
    }

    Ok(())
}

/// Write record batches as JSON to the provided writer
fn write_batches_as_json(batches: &[RecordBatch], writer: Box<dyn Write>) -> Result<()> {
    if batches.is_empty() {
        return Ok(());
    }

    let mut json_writer = LineDelimitedWriter::new(writer);

    // Write each batch as a separate JSON object
    for batch in batches {
        json_writer.write_batches(&[batch])?;
    }

    json_writer.finish()?;
    Ok(())
}
