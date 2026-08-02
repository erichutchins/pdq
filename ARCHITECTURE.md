# PDQ Architecture

**Pretty Darn Quick (PDQ)** is a secondary indexing system built for fast, exact-match searches of cybersecurity indicators across large-scale Parquet log files. It uses Apache Arrow, DataFusion, and `fst`-based row-group indexing.

---

## Goals

- Fast CLI- and Python-based indicator search (IPs, emails, hashes)
- Pinpoint exact Parquet row-groups to query
- Integrate seamlessly with DataFusion
- Immutable, per-column/per-file indexes for static telemetry
- Portable between local and cloud environments

---

## Indexing Strategy

- One `fst::Set` per file, per column.
- Each entry in the FST is:

  ```
  KEY: <value>\x00<rowgroup_id>
  ```

  Example:

  ```
  1.2.3.4\x00rg0
  ```

- This structure supports fast range lookups:

  ```rust
  set.range().ge("1.2.3.4\x00").lt("1.2.3.4\x01")
  ```

- Index is read-only and immutable. Built once per file version.

---

## Query Strategy

- A Rust `IndexQueryEngine` loads an FST and resolves search terms to:

  ```
  HashMap<file_path, Vec<rowgroup_id>>
  ```

- From this, we construct:

  ```rust
  Vec<(file_path, FileScanPlan)>
  ```

- DataFusion reads only the specified row-groups for each file using `ParquetExec`.

---

## Hive Partitioning

- We replicate Hive-style partition discovery internally (adapted from DataFusion’s `ListingTable`)
- This ensures our `TableProvider` supports path-based partition filters

---

## Design Summary

| Component      | Technology    | Purpose                              |
| -------------- | ------------- | ------------------------------------ |
| Index Builder  | `fst` crate   | Build immutable sorted set index     |
| Index Query    | `fst` + range | Range lookup on value+rowgroup keys  |
| Metadata Store | File system   | `pdq-index/<file-hash>/<column>.fst` |
| Query Engine   | DataFusion    | Parquet scan + row-group pruning     |
| Interface      | CLI + Python  | For both batch and ad hoc search     |

---

## Output Formats

- CSV
- NDJSON
- Polars DataFrame / LazyFrame

---

## Future Enhancements

- Bitmap or roaring-set sidecars for scalable row-group lookup
- Tantivy or SQLite-backed alternatives for hybrid filtering
- Multi-column / composite key indexing
- CIDR-aware matching for IPs
