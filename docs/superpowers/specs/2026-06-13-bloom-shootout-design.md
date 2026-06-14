# PDQ vs. Parquet Bloom Filter — Scaled Shootout Design

**Date:** 2026-06-13
**Status:** Approved design, pending spec review

## Goal

Test, at scale, whether PDQ's separate FST side-index beats Parquet-native bloom
filters for needle-in-a-haystack lookups over a directory of Parquet log files.

The driving hypothesis (sharpened): the cost of bloom-filter pruning is **I/O +
metadata traversal**, not computation. To prune with bloom filters a reader must,
per file, read the footer to locate bloom offsets, then read each row group's
bloom block for the target column, then probe — so pruning cost scales with
`#files × #row-groups` and *must touch every file*. PDQ instead reads a compact,
separate index and never opens the data files to make the pruning decision.

Honesty caveat baked into the design: **PDQ as it stands is N reads (one FST per
file), not literally "one shot."** True one-shot needs a consolidated index, so
that variant is a stretch contestant (below) to quantify how much the current
per-file layout costs.

Where PDQ has no bloom equivalent at all: **prefix / range**, including
suffix-match on domains/emails via reverse-label encoding. That is a capability
showcase, not a race.

## Contestants

One shared Parquet corpus, written **with per-column bloom filters** on the
indexed columns:

- **PDQ-FST** — current PDQ: per-file FST + `ParquetAccessPlan`. Ignores the
  embedded bloom filters; builds and uses its own FST index.
- **DataFusion + bloom** — same engine as PDQ; native bloom row-group pruning
  (`datafusion.execution.parquet.bloom_filter_on_read = true`). The controlled
  comparison — only the pruning mechanism differs.
- **DuckDB** — reads the same bloom-filter Parquet. Real-world "what people reach
  for" anchor; different engine, so its number is confounded and labeled as such.
- **Full-scan floor** — Polars `scan_parquet().filter().collect()` (existing
  `misc/brute_force_polars.py`). The "do nothing" reference every number is
  compared against.
- **PDQ-FST-consolidated** *(stretch)* — a single merged FST across all files,
  to show the true one-shot ceiling vs. today's per-file layout. Built only if
  the head-to-head lands with time to spare.

## Metrics — "both, layered"

### Layer 1 — pruning-decision cost (Rust micro-bench)

Measures wall-clock **and bytes read** to decide *which* row groups to scan,
excluding the cost of reading matched data.

- PDQ: open/mmap the FSTs, range-probe, collect row groups.
- Bloom: read footer(s) + bloom blocks for the target column across all files,
  probe.

Reported as a curve vs. `#files`. The Rust harness controls every read, so byte
counts are exact and OS-independent — bytes-read is the headline, machine-neutral
number that directly visualizes "touch every file."

### Layer 2 — end-to-end query wall-clock (Python orchestrator)

Full query returning matching rows, per contestant, cold and warm. Captures what
Layer 1 deliberately excludes: bloom's **false-positive read penalty** (bloom
occasionally sends the reader to a row group that doesn't contain the value;
PDQ has zero false positives) and raw engine differences.

## Cold vs. warm

- **Warm:** repeated in-process runs; report median.
- **Cold:** fresh process + cache purge before each measured run.
  - macOS (dev machine): `sync && sudo purge`.
  - Linux (CI/EC2): `echo 3 | sudo tee /proc/sys/vm/drop_caches`.
- **Larger-than-RAM:** at the top of the ladder, run a corpus exceeding RAM so
  "warm" cannot hold the whole dataset — the realistic "scaled" case.

## Data generation + manifest

Extend the `misc/` tooling to emit a **scale ladder** of corpora and a JSON
**manifest** that both harness layers consume.

- **Scale ladder (initial):** `#files ∈ {10, 100, 1000}`, each file ~8 row groups
  × ~100k rows/group (~800k rows/file; ~800M rows at the top rung). File count is
  the primary scaling axis. Bigger rungs (10k+) run later on a devcontainer/EC2.
- **Columns:** `src_ip`, `dst_ip`, `domain` (stored reverse-label encoded so
  `*.evil.com` suffix search becomes a prefix search), `email`, plus filler.
  High-cardinality so pruning is meaningful.
- **Bloom filters:** written via pyarrow (preferred) or DuckDB `COPY`; the harness
  verifies they are actually present by inspecting Parquet metadata.
- **Manifest contents:** corpus path, schema, each planted needle's
  `(value → file, row_group)`, the multi-IOC list with known selectivity, and the
  prefix/reverse-domain terms with their expected match sets.

## Correctness gate (first-class)

Before any timing:

1. Assert every contestant returns the identical row set per query
   (order-insensitive). A fast wrong answer is disqualified.
2. Assert the bloom filters were actually **written** (Parquet metadata) and that
   DataFusion's bloom pruning is actually **active** (verify via the
   `row_groups_pruned_bloom_filter` execution metric). Otherwise PDQ would look
   good for the wrong reason.

## Workloads

1. **Single exact match** — the planted needle. Race across all contestants.
2. **Multi-IOC sweep** — N needles (e.g., 100 / 1k) in one query. Layer 1 probes
   the FST engine directly so the index mechanism is measured fairly; Layer 2
   exposes PDQ's SQL/provider-path `IN`/`OR` gap (the provider pushes only single
   equality) — a useful finding, not a disqualification.
3. **Prefix / reverse-domain showcase** — subnet prefix on IPs and `*.evil.com`
   via reversed labels. Capability demo; bloom has no equivalent.

## Output / reporting

- Markdown results table + matplotlib plots: pruning-cost-vs-`#files` curve
  (log-log), end-to-end latency bars per workload (cold/warm), bytes-read
  comparison.
- Written to `misc/shootout/results/`.

## Repo placement

- Rust micro-bench: `benches/pruning_cost.rs` (criterion; `[[bench]]` +
  criterion dev-dependency in `Cargo.toml`).
- Python orchestrator + data-gen: `misc/shootout/` (uv single-file scripts:
  data generation, run, plot).
- Spec: this file.

## Out of scope

- **Object store (S3/minio):** PDQ is hardcoded to local files (`file://`,
  `std::fs`, mmap); remote support is its own project. Decided out.
- **Selectivity sweep:** not selected; single-match and multi-IOC already give
  two selectivity points.

## Open risks / things to verify during implementation

- Exact pyarrow / DuckDB API for emitting per-column bloom filters, and that
  DataFusion reads them (config + metric verification above).
- macOS `sudo purge` needs sudo; document the prompt or gate cold runs behind a
  flag.
- PDQ's equality match goes through `ScalarValue::to_string()` in the provider —
  confirm it round-trips the indexed value for the test data (no quoting/escaping
  surprises).
