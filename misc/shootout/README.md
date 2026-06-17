# PDQ vs. Parquet Bloom Filter Shootout

Reproducible benchmark. See design:
`docs/superpowers/specs/2026-06-13-bloom-shootout-design.md`.

## Run it

```bash
# 1. Build PDQ with the shootout feature (CLI + gen-corpus + bench).
#    The corpus writer lives behind this feature because neither pyarrow
#    (through 23.x) nor DuckDB can emit Parquet bloom filters from Python,
#    so the corpus is written with the arrow-rs parquet writer in Rust.
cargo build --release --features shootout

# 2. Generate corpora + manifests + PDQ indexes (10/100/1000 files)
uv run misc/shootout/gen_data.py --root misc/shootout/corpora

# 3. Layer 1 — pruning cost (Rust)
cargo bench --features shootout --bench pruning_cost

# 4. Layer 2 — end-to-end, warm
uv run --with duckdb --with datafusion --with polars --with pyarrow \
  misc/shootout/run_e2e.py

# 4b. Layer 2 — end-to-end, cold (needs sudo for cache purge)
misc/shootout/run_cold.sh

# 5. Report
uv run misc/shootout/plot.py
open misc/shootout/results/REPORT.md
```

## Scaling beyond 1000 files

Generate a bigger ladder on a devcontainer/EC2 box with more disk:
`uv run misc/shootout/gen_data.py --ladder 1000,10000 --root /data/corpora`
then point the bench/orchestrator at `--root /data/corpora`.

## What each layer measures

- **Layer 1 (pruning):** time + bytes to decide which row groups to read.
  PDQ = mmap+scan the per-column FSTs; bloom = read footer + bloom blocks per
  file and probe. `bytes_read` for PDQ is the FST footprint scanned; for bloom
  it is the exact bytes faulted through a counting reader.
- **Layer 2 (end-to-end):** full query returning rows. Catches bloom's
  false-positive read penalty (PDQ has none) and engine differences.

## Components

- `gen_data.py` — owns the manifest; shells out to `pdq gen-corpus` (Rust) to
  write the bloom-filter corpus, then builds PDQ FST indexes.
- `oracle.py` — brute-force Polars row counts; ground truth for correctness.
- `run_e2e.py` — Layer-2 contestants (Polars full-scan, DuckDB, DataFusion+bloom,
  PDQ CLI), each validated against the oracle before timing.
- `plot.py` — renders `results/REPORT.md` + `results/pruning_scaling.png`.
- `src/corpus_gen.rs` / `pdq gen-corpus` — the feature-gated Rust corpus writer.
- `benches/pruning_cost.rs` — the Layer-1 Rust micro-bench.

`corpora/` and `results/` are gitignored.
