use clap::{Arg, Command};
use pdq::{Result, index::Indexer, parquet_filter::ParquetFilter, search::Searcher};
use std::path::Path;
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
                    Arg::new("output")
                        .long("output")
                        .value_name("FORMAT")
                        .help("Output format: csv, json, jsonl, or ndjson")
                        .default_value("jsonl"),
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
            let output_format = sub_matches.get_one::<String>("output").unwrap();

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
                println!("   Index lookup: {:?}", query_time);
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

            let parquet_filter = ParquetFilter::new();
            let output = parquet_filter
                .query_with_index_engine(
                    Path::new(index_dir),
                    Path::new(data_path),
                    column,
                    term,
                    output_format,
                )
                .await?;

            let total_time = start_time.elapsed();

            println!("🎯 Query Complete!");
            println!("   Total time: {:?}", total_time);

            if output.is_empty() {
                println!("   Result: No matching records found in the data");
            } else {
                println!("   Result: Found matching data");
                if output_format == "csv"
                    || output_format == "json"
                    || output_format == "jsonl"
                    || output_format == "ndjson"
                {
                    println!("\n📋 Output:");
                    println!("{output}");
                } else {
                    println!("{output}");
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
