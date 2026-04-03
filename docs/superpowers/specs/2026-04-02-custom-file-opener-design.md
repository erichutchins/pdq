# PDQ: Custom FileOpener + DataFusion v53 Upgrade

**Date:** 2026-04-02
**Status:** Approved

## Background

The Apache DataFusion team published a [blog post](https://datafusion.apache.org/blog/2026/03/31/writing-table-providers/) (2026-03-31) on best practices for `TableProvider` implementations. The core principle:

> `scan()` runs during planning, not execution. Avoid I/O, network calls, or heavy computation.

PDQ's current `scan()` violates this: it opens every matched Parquet file to read its footer (via `load_pruned_metadata()`), blocking query planning proportional to the number of matched files.

This design fixes that by moving footer reads to execution time via a custom `FileOpener`, and simultaneously upgrades DataFusion from v51 to v53 (required, as the `FileSource` trait signature changed).

## Architecture

Two changes ship together:

1. **DataFusion v51 → v53** — `Cargo.toml` bump plus trait compliance fixes
2. **Custom `FileOpener`** — Parquet footer I/O moves from `scan()` (planning) into `open()` (execution)

After this change, `scan()` does zero disk I/O. It runs FST lookups (in-memory), builds a `PartitionedFile` list carrying only matched row group indices, and returns a `DataSourceExec` immediately.

## Components

### New Types (both in `src/provider.rs`)

**`PdqFileSource`** — implements `FileSource` (v53). Wraps an inner `ParquetSource` and delegates all trait methods to it except `create_file_opener()`, which returns a `PdqParquetOpener` instead of DataFusion's private internal `ParquetOpener`.

**`PdqParquetOpener`** — implements `FileOpener`. Holds object store, schema, projection, batch size, and predicate. Its `open()` method:
1. Extracts `Vec<usize>` (matched row group indices) from `PartitionedFile::extensions`
2. Constructs a `ParquetObjectReader` and awaits `ParquetRecordBatchStreamBuilder::new()` — the single footer read, at execution time
3. Builds `ParquetAccessPlan::new_none(total_rg)`, marks matched indices with `.scan(idx)`
4. Configures the builder with row group selection, projection, and batch size
5. Returns the stream wrapped in `RecordBatchStreamAdapter`

### Deleted Types

**`CachedParquetFileReaderFactory`** and **`ParquetReaderWithCache`** — deleted entirely. Their only job was avoiding footer re-reads that `PdqParquetOpener` makes unnecessary by design.

**`load_pruned_metadata()`** — deleted.

### Modified

**`scan()`** — `load_pruned_metadata()` call removed. After FST lookup, stores `Arc::new(matched_row_groups_vec)` in `PartitionedFile::extensions` (was `Arc<ParquetAccessPlan>`). Uses `PdqFileSource` instead of `ParquetSource`. The `CachedParquetFileReaderFactory` construction block is gone.

**`Cargo.toml`** — `datafusion`, `datafusion-common`, `datafusion-physical-expr`, `datafusion-expr` bumped from `51.0.0` to `53.0.0`.

## Data Flow

### Planning (`scan()`)

```
filters: &[Expr]
  → prune_with_fst_index()           ← in-memory FST lookups only
  → HashMap<file_hash, Vec<usize>>   ← matched row group indices per file
  → filter out any empty Vec<usize>  ← debug_assert, then skip
  → for each file:
      PartitionedFile {
          path, size,
          extensions: Arc<Vec<usize>>  ← index list only, no metadata
      }
  → FileScanConfig { files, PdqFileSource }
  → DataSourceExec                   ← returned immediately, no I/O
```

### Execution (per partition, driven by DataFusion)

```
PartitionedFile
  → PdqParquetOpener::open()
      → extract Vec<usize> from extensions
      → ParquetObjectReader::new(object_store, file_meta)
      → ParquetRecordBatchStreamBuilder::new(reader).await  ← footer read
      → total_rgs = builder.metadata().num_row_groups()
      → ParquetAccessPlan::new_none(total_rgs)
          .scan(idx) for each idx in Vec<usize> where idx < total_rgs
      → builder
          .with_row_groups(access_plan.row_group_indexes())
          .with_batch_size(batch_size)
          .with_projection(projection)
          .build()?
      → RecordBatchStreamAdapter::new(schema, stream)
  → SendableRecordBatchStream → DataFusion FilterExec → results
```

### Parallelism

Each matched Parquet file is its own `PartitionedFile` → its own partition → DataFusion executes partitions concurrently up to `target_partitions`. With the new approach, each partition's footer read and stream start are pipelined — file A can begin producing `RecordBatch`es while file C is still reading its footer. The old approach required all footers to be read before any file began streaming.

### What Is Traded Away

`ParquetSource`'s internal opener applies `PagePruningAccessPlanFilter` and bloom filter checks after reading the footer. `PdqParquetOpener` skips those. Row-level predicate filtering still happens via DataFusion's `FilterExec` downstream (since we declare `Inexact`), so correctness is preserved. For PDQ's primary use case (FST-based row group selection on equality queries), page-level Parquet pruning is not a meaningful loss.

## Error Handling

| Scenario | Behavior |
|---|---|
| Footer read failure | `DataFusionError` from that partition; other partitions continue |
| Stale row group index (`idx >= total_rg`) | Silently skipped; valid indices still scanned |
| File deleted between `scan()` and `open()` | Partition fails with `DataFusionError`; others continue |
| Empty row group list for a file | `debug_assert!(!row_groups.is_empty())` in `scan()`; file filtered before `PartitionedFile` creation |
| No files match FST | `file_row_groups` is empty; returns empty `DataSourceExec` immediately (unchanged) |

**Behavior change from today:** a single bad file no longer aborts the whole query at planning time — it fails its own partition at execution time. Other partitions still run. This is strictly better for the user.

## Testing

**Existing tests are the primary regression suite.** `provider_tests.rs` and `integration_tests.rs` cover end-to-end behavior and should all pass without modification.

**New unit tests for `PdqParquetOpener`:**

- `test_opener_reads_correct_row_groups` — 3-row-group file, `open()` with `vec![1]`, assert only row group 1's data in output
- `test_opener_skips_stale_index` — `vec![0, 99]` on a 2-row-group file, assert only row group 0 scanned, no panic
- `test_opener_empty_result_on_all_stale` — `vec![99]` only, assert zero batches produced

**New test for `scan()` guard:**

- `test_scan_filters_empty_row_group_list` — file hash mapping to `vec![]` produces no `PartitionedFile`

**Parallelism smoke test (add to integration tests):**

- Index over ≥4 Parquet files, query matching row groups across all files, assert correct total row count — verifies DataFusion drives all partitions and merges results correctly
