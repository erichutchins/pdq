# PDQ Benchmarks — FST Secondary Index vs. Parquet Bloom Filters

A scaled head-to-head between PDQ's per-column **FST side index** and **Parquet-native
bloom filters** for the workload PDQ targets: finding rare cybersecurity indicators
(IPs, hashes, domains, emails) across many Parquet log files.

> **Status: canonical (EC2), improved execution path.** This run uses PDQ's
> cached-metadata + row-filter-pushdown provider (commit `dabf5e6`); see
> [What changed since the prior run](#what-changed-since-the-prior-run). An earlier
> *canonical* run (before that commit) reported PDQ **lost warm at n=10 and lost cold
> at every scale**; both of those losses are gone here. An even earlier headline
> ("~47× faster and ~496× less I/O") was a benchmark bug — see
> [Correction notice](#correction-notice). Every number below is from the EC2 run on a
> dedicated AWS `i4i.xlarge` over the full **n = 10 / 100 / 1000** ladder (warm + cold).
> Absolute latencies are machine-dependent; byte/footprint counts are exact.
>
> **What the canonical run shows:**
> 1. **Layer 1 (pruning decision): PDQ wins by orders of magnitude at every scale** —
>    ~900–2800× faster for a single indicator, with **zero false positives**.
> 2. **Layer 2 (end-to-end, warm): PDQ wins at every scale, including n=10.** PDQ's
>    latency is essentially **flat** (6.5 → 6.6 → 8.0 ms across the ladder) because it
>    touches only the matched file and materializes only the matching row, while the
>    engines footer-parse every file. At n=1000 PDQ is **28× faster than DuckDB** and
>    **66× faster than DataFusion**. **Caveat unchanged:** the engines run *without*
>    metadata/footer caching, so the warm *magnitudes* are against un-tuned engines (see
>    [Known limitations](#known-limitations-from-adversarial-review)). The flat scaling /
>    direction is structural and survives engine caching; the multiples would shrink.
> 3. **Layer 2 (end-to-end, cold): PDQ now wins at every scale too** — the reverse of the
>    prior canonical. Cold latency is also flat (~14–15 ms), so at n=1000 PDQ is **17×
>    faster than DuckDB** cold. Because "cold" drops the page cache before every sample,
>    this result is **not** subject to the engine-caching caveat (caching is a warm
>    concept).

---

## What changed since the prior run

The prior canonical run predates PDQ's improved execution path. This run includes
(commit `dabf5e6`, modeled on DataFusion's `parquet_advanced_index` example):

- **Cached Parquet metadata** — a custom `ParquetFileReaderFactory` serves
  `ParquetMetaData` from a process-lifetime cache, so a long-lived engine parses each
  matched file's footer at most once instead of on every query (`src/provider.rs`).
- **Footer size hint** — the first (cold) metadata read fetches the footer in one shot.
- **Row-filter pushdown** (`with_pushdown_filters(true)`) — the equality predicate is
  applied *during* Parquet decode (late materialization). On this **random, unsorted**
  corpus, min/max zonemaps and the page index cannot prune within a row group, so before
  this change the whole matched row group (100k rows × 5 columns) was decoded and a
  `FilterExec` above the scan discarded all but the matching row. Now only the matching
  row is materialized.

Net effect: PDQ's per-query cost became essentially **constant** (it touches only the
matched file and decodes ~1 row), which is what turned the prior warm-n=10 loss into a
win and the prior cold losses into wins. The pruning Layer-1 micro-bench also amortizes
the FST index handle across the query batch (commit `af28001`), the way a long-lived
service reuses an open index.

---

## Correction notice

The original Layer-1 "multi-IOC" result measured the bloom path with a nested loop that
called a full `probe_bloom(file, value)` — `File::open` + **re-parse the Parquet footer**
+ reload the row-group bloom bitsets — once **per (file × indicator)**:

```
for file in files {            // 1000 files
    for ioc in indicators {    // 1000 indicators
        probe_bloom(file, ioc) // re-opens & re-parses file every time
    }
}
```

The raw JSON proved it: multi-IOC bloom `bytes_read` at n=1000 was `3,152,795,148,000`
(3.15 TB) — *exactly 1000×* the single-lookup figure, i.e. every file's footer+blooms
were re-read once per indicator. No real engine screens a watchlist that way: you open
each file once, load each row group's bloom once, then probe all N indicators against the
resident bitset (a few hash ops each).

**The fix:** `probe_blooms_multi()` opens each file and faults its blooms **once**, then
probes the whole watchlist in memory (`src/bloom_probe.rs`); the symmetric
`IndexQueryEngine::exact_search_multi()` mmaps each FST once and probes all terms
(`src/query.rs`). The Layer-1 bench (`benches/pruning_cost.rs`) uses both. The "496× less
I/O" claim is gone — the byte columns are footprint vs. actual-reads and not comparable
(see the note below the Layer-1 tables); PDQ's *advantage is latency + exactness*.

---

## Setup

| | |
|---|---|
| **Host** | AWS EC2 `i4i.xlarge` — 4 vCPU, 32 GiB, local NVMe, Ubuntu 24.04 |
| **Build** | `cargo build --release --features shootout` + `maturin develop --release` |
| **PDQ / Rust DataFusion** | DataFusion 54.0.0 (`default-features = false`); provider uses cached `ParquetMetaData` + `with_pushdown_filters(true)` + footer size hint |
| **Layer-2 engine contestants** | DuckDB + the **Python** `datafusion`/`polars` packages (installed via the `bench` dependency group) — *separate* from the Rust pin. ⚠️ Exact contestant versions were **not pinned** in the results JSON (a reproducibility gap; engine ranking is version-sensitive). |
| **Engine tuning** | ⚠️ **No Parquet metadata/footer caching** was enabled on DuckDB or DataFusion (no `enable_object_cache`, no metadata cache). This handicaps the engines on **warm** Layer-2 (it is moot cold). See limitations. |
| **Thread pinning** | all engines set to the same width (`SHOOTOUT_THREADS`, default = ncpu = 4) |

### Corpus

A deterministic ladder of **10 / 100 / 1000** Parquet files. Each file = **8 row groups ×
100,000 rows = 800,000 rows**; 5 columns (`src_ip`, `dst_ip`, `domain_rev`, `email`,
`bytes`); per-column bloom filters on the 4 indexed columns; **dictionary encoding
enabled**. At n=10 the corpus is **540 MB** (54 MB/file); at n=1000 that scales to
~54 GB / 800M rows / 8,000 row groups. PDQ FST indexes are built on the same 4 columns.

Needles, planted at fixed positions (`manifest.json`):
- **Single needle** `192.168.133.7` — in the last file's last row group only (1 of 8,000
  row groups at n=1000). The selective "needle in a haystack" case.
- **Multi-IOC** `10.<hi>.<lo>.7` — one unique value in row group 0 of every file; an
  N-indicator watchlist sweep.

### What each layer measures

- **Layer 1 — pruning cost** (`benches/pruning_cost.rs`): time + bytes to decide which row
  groups match, **no row reads**. PDQ mmaps + traverses the per-column FSTs; bloom reads
  each file's footer + bloom blocks through a counting reader. Both amortize the per-file
  index open across a watchlist (`*_multi` paths). Median of 20 iterations, warm. **Byte
  semantics differ and are not directly comparable** — see the note below the tables.
- **Layer 2 — end-to-end** (`misc/shootout/run_e2e.py`): the full query returning the
  matching row, validated against a brute-force Polars oracle. **All engines are warm and
  long-lived** — the connection/context/engine is built once *outside* the timer, then the
  query is run 30× and we report **median + IQR**. PDQ is measured **in-process** via its
  Python binding (`QueryEngine`). A `--cold` mode drops the OS page cache before every
  sample (5 samples, no warmup).

---

## Layer 1 — pruning cost (canonical, EC2)

Warm cache, median of 20. This is the decision step only — *which* row groups match, no row
data read.

### Single lookup (one indicator)

| n_files | PDQ FST | Bloom | PDQ faster | PDQ footprint¹ | Bloom bytes¹ |
|--:|--:|--:|--:|--:|--:|
| 10 | 0.017 ms | 15.45 ms | **909×** | 63.6 MB | 31.5 MB |
| 100 | 0.077 ms | 156.1 ms | **2027×** | 635.8 MB | 315.3 MB |
| 1000 | 0.541 ms | 1494.6 ms | **2762×** | 6.36 GB | 3.15 GB |

### Multi-indicator watchlist (probe all N planted indicators, both amortized)

| n_files | PDQ FST | Bloom | PDQ faster | PDQ footprint¹ | Bloom bytes¹ |
|--:|--:|--:|--:|--:|--:|
| 10 | 0.041 ms | 15.42 ms | **376×** | 63.6 MB | 31.5 MB |
| 100 | 2.56 ms | 158.1 ms | **61.7×** | 635.8 MB | 315.3 MB |
| 1000 | 263.2 ms | 1723.6 ms | **6.5×** | 6.36 GB | 3.15 GB |

PDQ wins the **pruning decision** at every scale: an mmap'd FST traversal beats opening and
footer-parsing every file. The win is **structural, not a measurement artifact** — the
bloom path's dominant cost is re-parsing each Parquet footer + Thrift metadata and
materializing the bloom bitset per file; the FST walk is an O(key-length) automaton
traversal with the index handle amortized across the batch. The gap narrows on the n=1000
*multi* sweep (6.5× vs ~2700× single) because the bloom footer-parse cost amortizes
perfectly across the whole watchlist while PDQ pays a per-term automaton walk that
amortizes less completely. PDQ still wins.

¹ **The byte columns are NOT apples-to-apples — do not read an I/O "winner" into them.**
- **Bloom bytes** = exact bytes the Parquet reader pulled to make the decision (footer +
  Thrift + bloom bitsets), tallied through a counting `ChunkReader`.
- **PDQ footprint** = the *entire* on-disk size of the `<column>.fst` file(s) — the index
  footprint, **not** what the query reads. Because the FST is **mmap'd**, the lookup faults
  only the pages along the automaton path, far less than the whole file. PDQ's true
  per-query faulted bytes are **smaller than this figure and were not measured**. So this
  column is a **storage** statement (PDQ's index is ~2× the bytes the bloom path pulls),
  not a per-query I/O comparison. The honest per-query story is the **time** column.

---

## Layer 2 — end-to-end query (canonical, EC2)

Full query returning the single matching row; all contestants validated correct against the
oracle. Warm = median of 30 in-process runs; cold = median of 5, OS page cache dropped
before every sample.

### Warm (long-lived engines, hot cache)

| n_files | pdq | duckdb_bloom | datafusion_bloom | polars_fullscan |
|--:|--:|--:|--:|--:|
| 10 | **6.53** | 7.25 | 13.22 | 34.17 |
| 100 | **6.55** | 28.56 | 54.32 | 333.17 |
| 1000 | **7.98** | 227.66 | 528.27 | 3300.56 |

*(median ms)*

**PDQ wins end-to-end warm at every scale — including n=10.** PDQ's latency is essentially
**flat** across a 100× growth in corpus size (6.53 → 6.55 → 7.98 ms) because it consults
the FST index, opens only the **1 matched file**, and decodes only the matching row. The
engines scale roughly linearly with file count (DuckDB 7.25 → 28.56 → 227.66 ms) because
each warm query re-opens and footer-parses every file. At n=1000 PDQ is **28× faster than
DuckDB** and **66× faster than DataFusion**.

> ⚠️ **Read the warm multiples as un-tuned-engine, not best-case-engine.** Neither engine
> was run with Parquet metadata/footer caching (`misc/shootout/run_e2e.py` — DuckDB has no
> `SET enable_object_cache=true`; DataFusion sets only `bloom_filter_on_read` +
> `target_partitions`). A long-lived, high-QPS service would cache that metadata, parsing
> the footers once rather than per query. **PDQ's per-query footer count is O(matches)
> while the engines' is O(files) even when cached**, so the **flat scaling and the
> direction of the win are structural and survive caching**; the **28×/66× magnitudes
> would shrink** (the engines' per-footer cost drops from a Thrift re-parse to a cache
> lookup, though they still probe N blooms per query). We did **not** re-run with caching
> on, so treat the warm multiples as an upper bound.

### Cold (OS page cache dropped before every sample)

| n_files | pdq | duckdb_bloom | datafusion_bloom | polars_fullscan |
|--:|--:|--:|--:|--:|
| 10 | **15.39** | 16.16 | 26.01 | 100.57 |
| 100 | **14.09** | 43.33 | 265.32 | 2133.16 |
| 1000 | **15.48** | 269.89 | 3934.46 | 22220.53 |

*(median ms)*

**Cold, PDQ now wins at every scale — the reverse of the prior canonical.** This is the
direct payoff of the row-filter pushdown: on first-touch I/O PDQ faults in only the
matched file's footer, the matched row group's filter-column chunk, and the one matching
row — a small, **N-independent** read (cold latency is flat at ~14–15 ms). The engines must
read every file's footer + bloom blocks cold, so they scale with N (DuckDB 16 → 43 →
270 ms). At n=1000 PDQ is **17× faster than DuckDB**, **254× faster than DataFusion**, and
**1400× faster than a full Polars scan** cold. **Because every cold sample drops the page
cache, this result is independent of the engine-caching caveat** — caching cannot help a
genuinely cold read. The prior canonical's "PDQ loses cold at every scale" no longer holds
with the pushdown execution path.

---

## Correctness — PDQ is exact, bloom is not

`matched_files` = files (single) or (indicator × file) candidates (multi) each mechanism
flags.

**Single needle** (truth: present in exactly 1 file):

| n_files | PDQ (exact) | Bloom candidates | Bloom false positives |
|--:|--:|--:|--:|
| 10 | 1 | 4 | 3 |
| 100 | 1 | 12 | 11 |
| 1000 | 1 | 72 | 71 |

**Multi sweep** ((indicator × file) matches; PDQ counts true occurrences — ≥ N because
random-filler collisions are *real* matches, not errors — while bloom counts probabilistic
candidates):

| n_files | PDQ (exact) | Bloom candidates |
|--:|--:|--:|
| 10 | 10 | 17 |
| 100 | 100 | 847 |
| 1000 | 1,196 | 79,855 |

PDQ's FST returns the exact (file, row-group) set — **zero false positives**. The candidate
count grows with N (at n=1000 the single-needle bloom flags 72 candidate files, 71 false,
where PDQ flags 1), **but this is a correctly-sized ~1% bloom behaving as designed, scaling
linearly with file count — not bloom degradation** (effective per-row-group fpp ≈ 0.9%,
the parquet-rs default). A real engine also prunes with min/max statistics and the page
index *first*, so most bloom false positives never become full row-group scans — though on
this random corpus those zonemaps prune little, which is exactly why PDQ's exactness +
row-filter pushdown pay off.

---

## Storage footprint (n=10 measured, dictionary on)

Measured directly on the deterministic corpus: bloom sizes via DuckDB
`parquet_metadata(bloom_filter_length)`, FST via on-disk `*.fst` sizes. The generator is
deterministic, so the n=1000 figures scale linearly (100× the n=10 corpus).

| | FST index | Bloom filters | raw Parquet (incl. blooms) |
|---|--:|--:|--:|
| **total (4 columns), n=10** | **182 MB** | **30.0 MB** | **540 MB** |
| per file | 18.2 MB | 3.0 MB | 54.0 MB |
| `src_ip` (high-cardinality) | 60.6 MB | 10.0 MB | — |
| `domain_rev` (low-cardinality) | ~0 MB | 0.011 MB | — |
| **n=1000 (linear extrapolation)** | **~18 GB** | **~3 GB** | ~54 GB |

The FST index is **~6× the bloom filters** and is a *separate* side structure on top of the
Parquet; the blooms are embedded. Both are cardinality-driven and both collapse to
near-zero for the low-cardinality `domain_rev` column. This larger footprint is the
deliberate trade for exactness and footer-free pruning — and, with row-filter pushdown, it
no longer costs PDQ the cold race.

### Build cost (one-time precompute, EC2)

| n_files | corpus write (s) | FST index build (s) |
|--:|--:|--:|
| 10 | 8.2 | 50.2 |
| 100 | 81.8 | 514.0 |
| 1000 | 817.7 | 5418.3 |

FST build scales roughly linearly (~6.6× the corpus-write time). This is a real precompute
cost the engines don't pay; it amortizes under high-QPS, long-lived indicator search — the
same regime in which the latency wins apply.

---

## What survives, what was retracted

**Retracted (older headlines):**
- ❌ "47× faster on multi-IOC" — an artifact of re-opening footers per indicator.
- ❌ "496× less I/O" — the byte columns are footprint vs. actual-reads and not comparable.

**No longer true (prior canonical, pre-`dabf5e6`):**
- ⛔ "PDQ loses end-to-end warm at n=10" — PDQ now wins warm at n=10 (6.53 vs 7.25 ms).
- ⛔ "Cold cache: PDQ loses to DuckDB at every scale" — PDQ now wins cold at every scale
  (17× at n=1000), thanks to row-filter pushdown.

**Survives / newly demonstrated (full ladder):**
- ✅ **Faster, exact pruning decisions (Layer 1)** — ~900–2800× faster single-indicator at
  every scale, with **zero false positives**.
- ✅ **Flat warm latency → wins at every scale (Layer 2)** — 6.5–8.0 ms across a 100×
  corpus growth; 28×/66× vs DuckDB/DataFusion at n=1000. **Direction is structural; the
  magnitude is against engines without metadata caching** (re-scope before quoting as a
  tuned-engine result).
- ✅ **Flat cold latency → wins at every scale (Layer 2)** — ~14–15 ms; 17× vs DuckDB at
  n=1000. Not subject to the engine-caching caveat (cold = no cache by definition).
- ✅ **Exactness end-to-end** — zero false positives vs the bloom's correctly-sized ~1% tail.

**Costs / limits that are real:**
- ⚠️ **Storage:** ~6× the blooms (~18 GB at n=1000) as a separate side index.
- ⚠️ **Build time:** ~50 s to index n=10, ~90 min at n=1000; the engines have zero precompute.
- ⚠️ **Warm multiples are vs un-tuned engines** — see limitations; magnitude shrinks with
  engine metadata caching on (direction does not).

---

## Known limitations (from adversarial review)

1. **Warm Layer-2 runs the engines without metadata/footer caching** (`run_e2e.py`). The
   single biggest caveat on the **warm** multiples. Direction holds (PDQ is O(matches),
   engines O(files) even when cached), magnitude does not, until re-run with
   `enable_object_cache` (DuckDB) + a DataFusion metadata cache. **Does not affect the cold
   results** (every cold sample drops the cache).
2. **No in-harness proof that DuckDB uses its bloom.** Only DataFusion has a pruning sanity
   gate (`datafusion_bloom_pruned_count`). DuckDB pruning via bloom was confirmed manually
   in earlier runs; the harness can't prove its own "duckdb_bloom" label.
3. **Query-shape asymmetry.** Engines run `SELECT count(*)`; PDQ materializes all 5 columns
   of the matching row(s). This is *conservative for PDQ* (the engines do strictly less
   work), so it doesn't flatter PDQ.
4. **Contestant versions unpinned.** The Layer-2 `datafusion`/`duckdb`/`polars` are the
   Python packages, independent of the Rust pin, and their versions aren't written to the
   results JSON. Engine ranking is version-sensitive. Reproducibility gap.
5. **Cold n=100 DataFusion is noisy** (median 265 ms, p25 176 ms over 5 samples): a wide
   spread. Don't read its 2-significant-figure precision as tight.

## Caveats

- **bytes_read is not apples-to-apples** (FST footprint vs. exact faults). Don't read a
  bytes "winner" into the Layer-1 tables; the time columns are the per-query comparison.
- **Selective workload only** (a rare needle in 1 of 8,000 row groups) — PDQ's best case.
  Non-selective queries can't be pruned and a full scan is the right tool.
- **Random, unsorted corpus** means min/max zonemaps and the page index can't prune — which
  is precisely why row-filter pushdown matters here. Real logs often have
  temporal/sortedness the engines exploit; results would differ.

---

## Reproduce

```bash
cargo build --release --features shootout            # CLI: gen-corpus, index, bench
uv sync --group bench
uv run --group bench maturin develop --release       # in-process pdq for Layer 2
uv run misc/shootout/gen_data.py --root misc/shootout/corpora --ladder 10,100,1000
cargo bench --features shootout --bench pruning_cost  # Layer 1
uv run --group bench python misc/shootout/run_e2e.py --ladder 10,100,1000   # Layer 2 (warm)
misc/shootout/run_cold.sh 10,100,1000                 # Layer 2 (cold; per-sample purge)
uv run misc/shootout/plot.py                          # REPORT.md + plot
```

The full ladder needs ~54 GB of disk; run it on an NVMe box. `misc/shootout/ec2_bootstrap.sh`
is a one-shot bootstrap for an EC2 `i4i.xlarge` (mount NVMe → build → generate → bench →
report → tarball). See `misc/shootout/README.md` for the runbook.
