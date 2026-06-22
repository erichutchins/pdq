# PDQ Benchmarks — FST Secondary Index vs. Parquet Bloom Filters

A scaled head-to-head between PDQ's per-column **FST side index** and **Parquet-native
bloom filters** for the workload PDQ targets: finding rare cybersecurity indicators
(IPs, hashes, domains, emails) across many Parquet log files.

> **Status: canonical (EC2), tuned engines, matched query shape, version-stamped.** This is
> the cleanest run to date: PDQ uses its cached-metadata + row-filter-pushdown provider
> (commit `dabf5e6`), **and the engine contestants are tuned with their standard config
> knobs** (DuckDB Parquet metadata cache on; DataFusion filter pushdown on), all running a
> **matched `SELECT *`** query shape, with **library versions stamped into the results**
> (DuckDB 1.5.4, DataFusion 53.0.0, Polars 1.36.1, PyArrow 22.0.0). Run on a dedicated AWS
> `i4i.xlarge` over the full **n = 10 / 100 / 1000** ladder (warm + cold). Absolute
> latencies are machine-dependent; byte/footprint counts are exact.
>
> **What the canonical run shows:**
> 1. **Layer 1 (pruning decision): PDQ wins decisively at every scale**, with **zero false
>    positives**. (The single-lookup table shows ~800–2800×, but that row is
>    amortization-asymmetric — see note ²; the symmetric, both-sides-resident comparison is
>    the *multi* table, 6.6× at n=1000.)
> 2. **Layer 2 (end-to-end, warm): PDQ wins at every scale, including n=10** — and now
>    **against tuned engines on a matched query shape.** PDQ's latency is essentially flat
>    (5.2 → 5.8 → 7.1 ms across the ladder) because it touches only the matched file; the
>    engines scale with file count. At n=1000 PDQ is **28× faster than DuckDB** and **71×
>    faster than DataFusion**. **The previous "un-tuned engine" caveat is resolved:** turning
>    on DuckDB's metadata cache moved its n=1000 warm only 227 → 201 ms (−12%), confirming
>    the gap is **structural** (PDQ is O(matches), the engines are O(files)), not a
>    footer-parsing artifact.
> 3. **Layer 2 (end-to-end, cold): PDQ wins at every scale, including n=10.** Cold latency
>    is flat (~15–17 ms); at n=1000 PDQ is **16× faster than DuckDB**. Cold now caches
>    metadata symmetrically (both PDQ and DuckDB), and DuckDB's in-engine file cache survives
>    the page-cache drop — i.e. cold, if anything, *flatters* DuckDB — yet PDQ still wins.

---

## What changed across runs

This benchmark has been through several corrections; for the full lineage see git history.
The two changes that produced the current result:

1. **PDQ's execution path** (commit `dabf5e6`): a lazily-populated cache of parsed Parquet
   metadata (so a long-lived engine parses each matched file's footer once), a footer size
   hint, and `with_pushdown_filters(true)` — the equality predicate applied as a row filter
   *during* decode (late materialization). On this random, unsorted corpus, zonemaps can't
   prune within a row group, so before this change the whole matched row group was decoded
   and a `FilterExec` discarded all but the matching row. (`src/provider.rs`.)
2. **A fair fight for the engines** (this run): the contestants are tuned with **standard
   config knobs only** (no custom code), run a **matched query shape**, and the run **stamps
   library versions**. See the [Setup](#setup) table. The headline finding is that tuning
   the engines barely moved them — so PDQ's win is structural, not an artifact of
   un-tuned engines or an unequal unit of work.

---

## Correction notice (historical)

An early Layer-1 "multi-IOC" result reported "~47× faster / ~496× less I/O." **That was a
benchmark bug.** The bloom path called a full `probe_bloom(file, value)` — `File::open` +
**re-parse the Parquet footer** + reload the bloom bitsets — once **per (file × indicator)**,
so bloom `bytes_read` scaled N× (exactly 1000× the single-lookup figure = 3.15 TB at
n=1000). No real engine screens a watchlist that way. The fix (`probe_blooms_multi()` in
`src/bloom_probe.rs` + `IndexQueryEngine::exact_search_multi()` in `src/query.rs`) opens each
file and faults its blooms **once**, then probes the whole watchlist in memory. With the fix
the "less I/O" claim disappears entirely (the byte columns are footprint vs. actual-reads and
not comparable — see the note below the Layer-1 tables); PDQ's advantage is **latency +
exactness**.

---

## Setup

| | |
|---|---|
| **Host** | AWS EC2 `i4i.xlarge` — 4 vCPU, 32 GiB, local NVMe, Ubuntu 24.04 |
| **Build** | `cargo build --release --features shootout` + `maturin develop --release` |
| **PDQ / Rust DataFusion** | DataFusion 54.0.0 (`default-features = false`); provider uses cached `ParquetMetaData` + `with_pushdown_filters(true)` + footer size hint |
| **Layer-2 contestants** | DuckDB **1.5.4**, DataFusion (Python) **53.0.0**, Polars **1.36.1**, PyArrow **22.0.0** — stamped into every results row. (The Python DataFusion is *separate* from the Rust pin inside PDQ.) |
| **Engine tuning (standard knobs only)** | DuckDB: `SET parquet_metadata_cache=true` (cache parsed footers across queries; `enable_external_file_cache`, which caches file bytes, is on by default). DataFusion: `pushdown_filters=true` + `reorder_filters=true` + `bloom_filter_on_read=true`. **No custom `ParquetFileReaderFactory`** — DataFusion 53's Python API exposes no standard cross-query metadata cache, so its warm queries still re-parse footers (the one residual handicap; see limitations). |
| **Query shape** | **Matched:** every contestant runs `SELECT *` and materializes the matching row(s) — the same unit of work as PDQ (which returns the row, not a count). |
| **Thread pinning** | all engines set to the same width (`SHOOTOUT_THREADS`, default = ncpu = 4) |

### Corpus

A deterministic ladder of **10 / 100 / 1000** Parquet files. Each file = **8 row groups ×
100,000 rows = 800,000 rows**; 5 columns (`src_ip`, `dst_ip`, `domain_rev`, `email`,
`bytes`); per-column bloom filters on the 4 indexed columns; **dictionary encoding enabled**.
At n=10 the corpus is **540 MB**; at n=1000 it is ~54 GB / 800M rows / 8,000 row groups. PDQ
FST indexes are built on the same 4 columns.

Needles, planted at fixed positions (`manifest.json`):
- **Single needle** `192.168.133.7` — in the last file's last row group only (1 of 8,000
  row groups at n=1000). The selective "needle in a haystack" case.
- **Multi-IOC** `10.<hi>.<lo>.7` — one unique value in row group 0 of every file; an
  N-indicator watchlist sweep.

### What each layer measures

- **Layer 1 — pruning cost** (`benches/pruning_cost.rs`): time + bytes to decide which row
  groups match, **no row reads**. PDQ mmaps + traverses the per-column FSTs; bloom reads each
  file's footer + bloom blocks through a counting reader. Median of 20 iterations, warm.
  **Byte semantics differ and are not directly comparable** — see the note below the tables.
- **Layer 2 — end-to-end** (`misc/shootout/run_e2e.py`): the full query returning the
  matching row, validated against a brute-force Polars oracle. **All engines are warm and
  long-lived** — the connection/context/engine is built once *outside* the timer, then the
  query is run 30× and we report **median + IQR**. PDQ is measured **in-process** via its
  Python binding (`QueryEngine`). A `--cold` mode drops the OS page cache before every sample
  (5 samples, no warmup); note that engines' *in-process* caches survive that drop (see the
  cold section).

---

## Layer 1 — pruning cost (canonical, EC2)

Warm cache, median of 20. This is the decision step only — *which* row groups match, no row
data read.

### Single lookup (one indicator)

| n_files | PDQ FST | Bloom | PDQ faster² | PDQ footprint¹ | Bloom bytes¹ |
|--:|--:|--:|--:|--:|--:|
| 10 | 0.018 ms | 14.16 ms | **786×** | 63.6 MB | 31.5 MB |
| 100 | 0.073 ms | 150.1 ms | **2056×** | 635.8 MB | 315.3 MB |
| 1000 | 0.513 ms | 1447.3 ms | **2821×** | 6.36 GB | 3.15 GB |

² **The single-lookup multiple is amortization-asymmetric — do not read it as ~2800× "true."**
The bench reuses a resident FST handle across all 20 iterations (the index mmap is opened
once and cached), but the bloom path re-runs `File::open` + footer Thrift parse + bloom-bitset
load *inside* every timed iteration (`src/bloom_probe.rs`). So this row compares an *amortized*
FST against a *cold-every-time* bloom. The **structural** win (an automaton walk beats a
footer parse) is real, but for an apples-to-apples, both-sides-resident pruning comparison use
the **multi** table below, where both sides amortize the per-file open within a single call.

### Multi-indicator watchlist (probe all N planted indicators, both amortized)

| n_files | PDQ FST | Bloom | PDQ faster | PDQ footprint¹ | Bloom bytes¹ |
|--:|--:|--:|--:|--:|--:|
| 10 | 0.040 ms | 14.32 ms | **358×** | 63.6 MB | 31.5 MB |
| 100 | 2.54 ms | 154.6 ms | **60.8×** | 635.8 MB | 315.3 MB |
| 1000 | 254.5 ms | 1673.9 ms | **6.6×** | 6.36 GB | 3.15 GB |

The gap narrows on the n=1000 *multi* sweep (6.6× vs ~2800× single) because the bloom
footer-parse cost amortizes perfectly across the whole watchlist while PDQ pays a per-term
automaton walk that amortizes less completely. PDQ still wins, exactly, at every scale.

¹ **The byte columns are NOT apples-to-apples — do not read an I/O "winner" into them.**
- **Bloom bytes** = exact bytes the Parquet reader pulled to make the decision (footer +
  Thrift + bloom bitsets), tallied through a counting `ChunkReader`.
- **PDQ footprint** = the *entire* on-disk size of the `<column>.fst` file(s). Because the FST
  is **mmap'd**, the lookup faults only the pages along the automaton path, far less than the
  whole file; PDQ's true per-query faulted bytes are smaller and were not measured. This is a
  **storage** statement, not a per-query I/O comparison. The honest per-query story is the
  **time** column.

---

## Layer 2 — end-to-end query (canonical, EC2)

Full query returning the single matching row; all contestants validated correct against the
oracle, all running a matched `SELECT *`. Warm = median of 30 in-process runs; cold = median
of 5, OS page cache dropped before every sample.

### Warm (long-lived, tuned engines, hot cache)

| n_files | pdq | duckdb_bloom | datafusion_bloom | polars_fullscan |
|--:|--:|--:|--:|--:|
| 10 | **5.20** | 8.41 | 11.27 | 66.63 |
| 100 | **5.80** | 27.74 | 54.82 | 659.47 |
| 1000 | **7.12** | 200.84 | 505.63 | 18997.64 |

*(median ms; polars n=1000 is a noisy full scan, IQR ~13.5–28 s)*

**PDQ wins warm at every scale, against tuned engines, on a matched query shape.** PDQ's
latency is essentially **flat** across a 100× growth in corpus size (5.20 → 5.80 → 7.12 ms):
it consults the FST index, opens only the **1 matched file**, and (via row-filter pushdown)
decodes only the matching row. The engines scale roughly linearly with file count because each
query touches every file's row-group metadata + blooms. At n=1000 PDQ is **28× faster than
DuckDB** and **71× faster than DataFusion**.

> **Why this is now a tuned-engine result.** DuckDB runs with `parquet_metadata_cache=true`
> (so it parses each footer once, not per query) and DataFusion with `pushdown_filters` on;
> all engines run the same `SELECT *`. Tuning DuckDB's metadata cache moved its n=1000 warm
> only **227 → 201 ms (−12%)** — because the dominant warm cost isn't footer *parsing*, it's
> visiting all N files' row-group metadata + probing their blooms every query. That work is
> **O(files)** no matter how it's cached, while PDQ is **O(matches)**. So the win is
> structural; the multiples are no longer "un-tuned engine" figures. **One residual handicap:**
> DataFusion 53's Python API exposes no standard cross-query metadata cache (only a custom
> Rust reader factory would add one), so its warm number still includes footer re-parsing —
> but since the same knob barely helped DuckDB, this is unlikely to change the ranking.

### Cold (OS page cache dropped before every sample)

| n_files | pdq | duckdb_bloom | datafusion_bloom | polars_fullscan |
|--:|--:|--:|--:|--:|
| 10 | **15.62** | 22.57 | 32.39 | 246.95 |
| 100 | **16.48** | 48.36 | 205.67 | 4939.13 |
| 1000 | **16.91** | 272.57 | 3952.82 | 50532.09 |

*(median ms)*

**PDQ wins cold at every scale, including n=10.** Cold latency is flat at ~15–17 ms because
PDQ touches only the matched file (footer + the matched row group's filter-column chunk + the
one matching row); the engines read every file's metadata + blooms and scale with N. At
n=1000 PDQ is **16× faster than DuckDB** and **234× faster than DataFusion**.

> **On what "cold" means here (honest framing).** The harness drops the **OS page cache**
> before each sample, but it cannot evict an engine's **in-process** caches, which persist
> across samples. So: PDQ's mmap'd *data* is genuinely cold (re-faulted each sample) while its
> parsed metadata stays cached; DuckDB keeps **both** its metadata cache *and* its file-byte
> cache (`enable_external_file_cache`, on by default) — so DuckDB's cold is only partly cold
> (its cold n=1000 of 273 ms is barely above its warm 201 ms). DataFusion and Polars have no
> such cache and are genuinely cold. Net: cold caches metadata symmetrically for PDQ and
> DuckDB, and if anything **flatters DuckDB** (its data stays resident while PDQ's is
> evicted) — and PDQ still wins 16×. The earlier "cold is not footer-symmetric" concern is
> resolved; the residual asymmetry now runs *against* PDQ, so the win is conservative.

---

## Correctness — PDQ is exact, bloom is not

`matched_files` = files (single) or (indicator × file) candidates (multi) each mechanism flags.

**Single needle** (truth: present in exactly 1 file):

| n_files | PDQ (exact) | Bloom candidates | Bloom false positives |
|--:|--:|--:|--:|
| 10 | 1 | 4 | 3 |
| 100 | 1 | 12 | 11 |
| 1000 | 1 | 72 | 71 |

**Multi sweep** ((indicator × file) matches; PDQ counts true occurrences — ≥ N because
random-filler collisions are *real* matches — while bloom counts probabilistic candidates):

| n_files | PDQ (exact) | Bloom candidates |
|--:|--:|--:|
| 10 | 10 | 17 |
| 100 | 100 | 847 |
| 1000 | 1,196 | 79,855 |

PDQ's FST returns the exact (file, row-group) set — **zero false positives**. The candidate
count grows with N (at n=1000 the single-needle bloom flags 72 candidate files, 71 false),
but this is a correctly-sized ~1% bloom (effective per-row-group fpp ≈ 0.9%, the parquet-rs
default) scaling linearly with file count — not bloom degradation.

---

## Storage footprint (n=10 measured, dictionary on)

Measured directly on the deterministic corpus: bloom sizes via DuckDB
`parquet_metadata(bloom_filter_length)`, FST via on-disk `*.fst` sizes. The n=1000 figures
scale linearly (100× the n=10 corpus).

| | FST index | Bloom filters | raw Parquet (incl. blooms) |
|---|--:|--:|--:|
| **total (4 columns), n=10** | **182 MB** | **30.0 MB** | **540 MB** |
| per file | 18.2 MB | 3.0 MB | 54.0 MB |
| `src_ip` (high-cardinality) | 60.6 MB | 10.0 MB | — |
| `domain_rev` (low-cardinality) | ~0 MB | 0.011 MB | — |
| **n=1000 (linear extrapolation)** | **~18 GB** | **~3 GB** | ~54 GB |

The FST index is **~6× the bloom filters** and is a *separate* side structure on top of the
Parquet; the blooms are embedded. Both are cardinality-driven and both collapse to near-zero
for the low-cardinality `domain_rev` column. This larger footprint is the deliberate trade for
exactness and footer-free pruning.

### Build cost (one-time precompute, EC2)

| n_files | corpus write (s) | FST index build (s) |
|--:|--:|--:|
| 10 | 7.8 | 41.8 |
| 100 | 80.5 | 427.8 |
| 1000 | 827.9 | 5132.0 |

FST build scales roughly linearly. This is a real precompute cost the engines don't pay; it
amortizes under high-QPS, long-lived indicator search — the regime in which the latency wins
apply.

---

## What survives, what was retracted

**Retracted (older headlines):**
- ❌ "47× faster on multi-IOC" / "496× less I/O" — artifacts of re-opening footers per
  indicator and of comparing footprint vs. actual reads.

**Survives — now demonstrated against tuned engines, matched shape, version-stamped:**
- ✅ **Faster, exact pruning decisions (Layer 1)** — orders of magnitude, zero false positives
  (single multiple amortization-asymmetric; symmetric number is the *multi* table, 6.6× at
  n=1000 — note ²).
- ✅ **Flat warm latency → wins at every scale (Layer 2)** — 5.2–7.1 ms across a 100× corpus
  growth; 28×/71× vs DuckDB/DataFusion at n=1000. **Structural, not un-tuned-engine:** turning
  on DuckDB's metadata cache barely moved it (−12%).
- ✅ **Flat cold latency → wins at every scale (Layer 2)** — ~15–17 ms; 16× vs DuckDB at
  n=1000, including a clean n=10 win. The residual cold cache asymmetry runs *against* PDQ.
- ✅ **Exactness end-to-end** — zero false positives vs the bloom's correctly-sized ~1% tail.

**Costs / limits that are real:**
- ⚠️ **Storage:** ~6× the blooms (~18 GB at n=1000) as a separate side index.
- ⚠️ **Build time:** ~42 s to index n=10, ~85 min at n=1000; the engines have zero precompute.
- ⚠️ **Selective + random workload only** — PDQ's best case (see caveats).

---

## Known limitations

Two rounds of adversarial review (DuckDB/DataFusion-maintainer mindset) shaped this. The big
ones from earlier runs are now **addressed** by this tuned, matched-shape, version-stamped run:
engines un-tuned → tuned with standard knobs; query-shape asymmetry → matched `SELECT *`;
contestant versions unrecorded → stamped into the JSON; cold not footer-symmetric → metadata
now cached symmetrically (and the residual runs against PDQ). What remains:

1. **DataFusion 53's Python API has no standard cross-query metadata cache.** Its warm number
   still re-parses footers; only a custom Rust `ParquetFileReaderFactory` would fix it (out of
   scope: standard knobs only). The same knob barely helped DuckDB, so this is unlikely to
   change the ranking — but DataFusion's warm multiple is not a fully-tuned figure.
2. **"Cold" is not uniformly cold.** Engines' in-process caches survive the OS page-cache drop
   (DuckDB's file-byte + metadata caches; PDQ's metadata cache). Cold is genuinely cold only
   for PDQ's mmap'd data, DataFusion, and Polars. As noted, this flatters DuckDB cold, so PDQ's
   cold win is conservative.
3. **Layer-1 single-lookup is amortization-asymmetric** (resident FST vs bloom re-opened per
   iteration). The ~800–2800× single multiple is inflated by that mismatch; the **multi** table
   is the symmetric pruning comparison.
4. **No in-harness proof that DuckDB uses its bloom.** Only DataFusion has a pruning sanity
   gate; DuckDB pruning via bloom was confirmed manually.
5. **Cold n=100 DataFusion is noisy** (median 206 ms, p25 205 / p75 315 over 5 samples).

## Caveats

- **bytes_read is not apples-to-apples** (FST footprint vs. exact faults). The time columns are
  the per-query comparison.
- **Selective workload only** (a rare needle in 1 of 8,000 row groups) — PDQ's best case.
  Non-selective queries can't be pruned and a full scan is the right tool.
- **Random, unsorted corpus** means min/max zonemaps and the page index can't prune — which is
  precisely why row-filter pushdown matters here. Real logs often have temporal/sortedness the
  engines exploit; results would differ. (Sorting/partitioning would help the engines but only
  along *one* column — the multi-column independence PDQ provides is the point.)

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
