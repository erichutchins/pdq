//! Layer-1 pruning-cost bench (harness = false). Run after generating corpora:
//!   cargo build --release --features shootout
//!   uv run misc/shootout/gen_data.py --root misc/shootout/corpora
//!   cargo bench --features shootout --bench pruning_cost
//! Emits misc/shootout/results/pruning_cost.json

use pdq::IndexQueryEngine;
use pdq::bloom_probe::{fst_index_bytes, probe_bloom};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

const LADDER: &[usize] = &[10, 100, 1000];
const ITERS: usize = 20;
const WARMUP: usize = 3;

#[derive(Serialize)]
struct Row {
    n_files: usize,
    mechanism: String, // "pdq_fst" | "bloom"
    workload: String,  // "single" | "multi"
    median_ms: f64,
    bytes_read: u64,
    matched_files: usize,
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn time_it<F: FnMut()>(mut f: F) -> f64 {
    for _ in 0..WARMUP {
        f();
    }
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    median(samples)
}

fn manifest_value(manifest: &serde_json::Value, key: &str) -> String {
    manifest[key]["value"].as_str().unwrap().to_string()
}

fn corpus_files(data_dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(data_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "parquet"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn main() {
    let root = Path::new("misc/shootout/corpora");
    let mut rows: Vec<Row> = Vec::new();

    for &n in LADDER {
        let base = root.join(format!("files_{n}"));
        let manifest_path = base.join("manifest.json");
        if !manifest_path.exists() {
            eprintln!("skip {n}: no manifest at {manifest_path:?}");
            continue;
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let data_dir = base.join("data");
        let index_dir = base.join("index");
        let files = corpus_files(&data_dir);
        let column = manifest["single"]["column"].as_str().unwrap().to_string();
        let needle = manifest_value(&manifest, "single");

        // --- PDQ FST, single ---
        let engine = IndexQueryEngine::new(&index_dir);
        let fst_bytes = fst_index_bytes(&index_dir, &column).unwrap();
        let mut matched = 0usize;
        let ms = time_it(|| {
            matched = engine.exact_search(&column, &needle).unwrap().len();
        });
        rows.push(Row {
            n_files: n,
            mechanism: "pdq_fst".into(),
            workload: "single".into(),
            median_ms: ms,
            bytes_read: fst_bytes,
            matched_files: matched,
        });

        // --- Bloom, single ---
        let files_c = files.clone();
        let col_c = column.clone();
        let needle_c = needle.clone();
        let mut bloom_bytes = 0u64;
        let mut bloom_matched = 0usize;
        let ms = time_it(|| {
            let mut b = 0u64;
            let mut hits = 0usize;
            for f in &files_c {
                let (rgs, bytes) = probe_bloom(f, &col_c, &needle_c).unwrap();
                b += bytes;
                if !rgs.is_empty() {
                    hits += 1;
                }
            }
            bloom_bytes = b;
            bloom_matched = hits;
        });
        rows.push(Row {
            n_files: n,
            mechanism: "bloom".into(),
            workload: "single".into(),
            median_ms: ms,
            bytes_read: bloom_bytes,
            matched_files: bloom_matched,
        });

        // --- Multi-IOC: probe the list of planted IOCs ---
        let iocs: Vec<String> = manifest["multi"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["value"].as_str().unwrap().to_string())
            .collect();

        let engine2 = IndexQueryEngine::new(&index_dir);
        let iocs_c = iocs.clone();
        let col_c = column.clone();
        let mut m_files = 0usize;
        let ms = time_it(|| {
            let mut hits = 0usize;
            for v in &iocs_c {
                hits += engine2.exact_search(&col_c, v).unwrap().len();
            }
            m_files = hits;
        });
        rows.push(Row {
            n_files: n,
            mechanism: "pdq_fst".into(),
            workload: "multi".into(),
            median_ms: ms,
            bytes_read: fst_bytes,
            matched_files: m_files,
        });

        let files_c = files.clone();
        let iocs_c = iocs.clone();
        let col_c = column.clone();
        let mut mb_bytes = 0u64;
        let mut mb_files = 0usize;
        let ms = time_it(|| {
            let mut b = 0u64;
            let mut hits = 0usize;
            for f in &files_c {
                for v in &iocs_c {
                    let (rgs, bytes) = probe_bloom(f, &col_c, v).unwrap();
                    b += bytes;
                    if !rgs.is_empty() {
                        hits += 1;
                    }
                }
            }
            mb_bytes = b;
            mb_files = hits;
        });
        rows.push(Row {
            n_files: n,
            mechanism: "bloom".into(),
            workload: "multi".into(),
            median_ms: ms,
            bytes_read: mb_bytes,
            matched_files: mb_files,
        });

        eprintln!("done n={n}");
    }

    let out_dir = Path::new("misc/shootout/results");
    std::fs::create_dir_all(out_dir).unwrap();
    let out = out_dir.join("pruning_cost.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    println!("wrote {out:?} ({} rows)", rows.len());
}
