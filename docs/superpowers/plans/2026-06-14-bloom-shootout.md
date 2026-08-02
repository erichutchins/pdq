# PDQ vs. Parquet Bloom Filter Shootout — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a reproducible, scaled benchmark that measures whether PDQ's FST side-index beats Parquet-native bloom filters for needle-in-a-haystack lookups, reporting both pruning-decision cost (bytes + time) and end-to-end query latency.

**Architecture:** A shared Python data generator emits a ladder of Parquet corpora (10/100/1000 files) written *with* per-column bloom filters, plus a `manifest.json` recording planted needle locations. A Rust `harness = false` bench (`benches/pruning_cost.rs`) measures Layer-1 pruning cost (PDQ FST scan vs. arrow-rs bloom read+probe) and emits JSON. A Python orchestrator measures Layer-2 end-to-end latency across four contestants (PDQ CLI, DataFusion+bloom, DuckDB, Polars full-scan), validating each against a ground-truth oracle before timing. A plotter renders the comparison.

**Tech Stack:** Rust (datafusion/parquet crate, fst, memmap2), Python 3.10+ via `uv` single-file scripts (pyarrow, duckdb, datafusion, polars, matplotlib, numpy), PDQ's existing CLI + `IndexQueryEngine`.

**Spec:** `docs/superpowers/specs/2026-06-13-bloom-shootout-design.md`

**Deviations from spec (intentional):**
- Layer-1 bench uses a custom `harness = false` main, not criterion — we need a labeled (mechanism × n_files) dataset plus a bytes-read metric that criterion does not emit cleanly.
- The PDQ Layer-2 contestant runs via the release CLI binary, not the Python bindings — avoids venv/import coupling and is PDQ's fastest documented path.

**Layout produced:**
```
benches/pruning_cost.rs            # Layer-1 Rust bench (harness=false)
src/bloom_probe.rs                 # bloom read+probe + counting reader (feature="shootout")
misc/shootout/
  gen_data.py                      # corpus + manifest generator (uv script)
  oracle.py                        # ground-truth row-set oracle (uv script + importable)
  run_e2e.py                       # Layer-2 end-to-end orchestrator (uv script)
  plot.py                          # report table + matplotlib plots (uv script)
  README.md                        # how to run the whole shootout
  corpora/                         # generated (gitignored)
  results/                         # generated outputs (gitignored)
tests/test_shootout_gen.py         # pytest: manifest correctness, bloom presence
tests/test_shootout_oracle.py      # pytest: oracle correctness
tests/test_shootout_contestants.py # pytest: each contestant matches oracle
```

---

## Phase 0: Scaffolding

### Task 0.1: Create directories and gitignore generated artifacts

**Files:**
- Create: `misc/shootout/.gitignore`
- Modify: `.gitignore` (repo root, append)

- [ ] **Step 1: Create the shootout output gitignore**

Create `misc/shootout/.gitignore`:

```gitignore
corpora/
results/
```

- [ ] **Step 2: Ensure repo-root gitignore excludes bench JSON in target**

Append to `.gitignore` (repo root) if not already present:

```gitignore
# Shootout generated artifacts
misc/shootout/corpora/
misc/shootout/results/
```

- [ ] **Step 3: Commit**

```bash
git add misc/shootout/.gitignore .gitignore
git commit -m "chore: scaffold shootout output dirs and gitignore"
```

### Task 0.2: Add the `shootout` feature and bench entry to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add a feature flag and bench stanza**

Add to `Cargo.toml` after the `[dependencies]` block (a `[features]` section does not yet exist, so create it), and add a `[[bench]]` entry:

```toml
[features]
# Benchmark-only code (bloom-filter probing, counting reader). Keeps this
# code out of normal library builds and shipped wheels.
shootout = []

[[bench]]
name = "pruning_cost"
harness = false
# Only build this bench when the feature is on, so plain `cargo test` / CI
# (which compile bench targets) don't fail on the feature-gated bloom_probe.
required-features = ["shootout"]
```

- [ ] **Step 2: Verify the manifest still parses**

Run: `cargo metadata --no-deps --format-version 1 > /dev/null && echo OK`
Expected: `OK` (no TOML errors).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "build: add shootout feature flag and pruning_cost bench entry"
```

---

## Phase 1: Data generation + manifest

The manifest is the contract every later phase depends on. Schema (JSON):

```json
{
  "corpus_dir": "misc/shootout/corpora/files_100/data",
  "index_dir":  "misc/shootout/corpora/files_100/index",
  "n_files": 100,
  "row_groups_per_file": 8,
  "rows_per_group": 100000,
  "indexed_columns": ["src_ip", "dst_ip", "domain_rev", "email"],
  "bloom_columns":   ["src_ip", "dst_ip", "domain_rev", "email"],
  "single": {"column": "src_ip", "value": "192.168.133.7",
             "file": ".../part_00042.parquet", "row_group": 3},
  "multi":  {"column": "src_ip",
             "locations": [{"value": "...", "file": "...", "row_group": 0}]},
  "prefix": {"column": "src_ip", "prefix": "192.168.133.",
             "locations": [{"value": "...", "file": "...", "row_group": 0}]},
  "domain_suffix": {"column": "domain_rev", "suffix": "evil.com",
                    "reversed_prefix": "com.evil.",
                    "locations": [{"value": "...", "file": "...", "row_group": 0}]}
}
```

`domain_rev` stores label-reversed domains: `www.evil.com` → `com.evil.www`, so a
suffix search for `*.evil.com` becomes a prefix search for `com.evil.`.

### Task 1.1: Manifest dataclass + JSON round-trip (TDD)

**Files:**
- Create: `misc/shootout/manifest.py`
- Test: `tests/test_shootout_gen.py`

- [ ] **Step 1: Write the failing test**

Create `tests/test_shootout_gen.py`:

```python
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "misc" / "shootout"))

from manifest import Manifest, Location  # noqa: E402


def test_manifest_round_trip(tmp_path):
    m = Manifest(
        corpus_dir="d", index_dir="i", n_files=2,
        row_groups_per_file=8, rows_per_group=10,
        indexed_columns=["src_ip"], bloom_columns=["src_ip"],
        single=Location(column="src_ip", value="1.2.3.4",
                        file="f.parquet", row_group=1),
        multi=[Location("src_ip", "9.9.9.9", "f.parquet", 0)],
        prefix_term="192.168.133.",
        prefix_locations=[Location("src_ip", "192.168.133.7", "f.parquet", 2)],
        domain_suffix="evil.com", domain_reversed_prefix="com.evil.",
        domain_locations=[Location("domain_rev", "com.evil.www", "f.parquet", 0)],
    )
    p = tmp_path / "manifest.json"
    m.save(p)
    loaded = Manifest.load(p)
    assert loaded == m
    assert json.loads(p.read_text())["single"]["row_group"] == 1
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_shootout_gen.py::test_manifest_round_trip -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'manifest'`.

- [ ] **Step 3: Write minimal implementation**

Create `misc/shootout/manifest.py`:

```python
"""Shared manifest schema for the PDQ vs. bloom-filter shootout."""
from __future__ import annotations

import json
from dataclasses import dataclass, asdict, field
from pathlib import Path


@dataclass(frozen=True)
class Location:
    column: str
    value: str
    file: str
    row_group: int


@dataclass
class Manifest:
    corpus_dir: str
    index_dir: str
    n_files: int
    row_groups_per_file: int
    rows_per_group: int
    indexed_columns: list[str]
    bloom_columns: list[str]
    single: Location
    multi: list[Location]
    prefix_term: str
    prefix_locations: list[Location]
    domain_suffix: str
    domain_reversed_prefix: str
    domain_locations: list[Location]

    def save(self, path: str | Path) -> None:
        Path(path).write_text(json.dumps(_to_jsonable(self), indent=2))

    @staticmethod
    def load(path: str | Path) -> "Manifest":
        raw = json.loads(Path(path).read_text())
        return _from_jsonable(raw)


def _to_jsonable(m: Manifest) -> dict:
    d = asdict(m)
    return d


def _loc_list(items: list[dict]) -> list[Location]:
    return [Location(**it) for it in items]


def _from_jsonable(raw: dict) -> Manifest:
    return Manifest(
        corpus_dir=raw["corpus_dir"],
        index_dir=raw["index_dir"],
        n_files=raw["n_files"],
        row_groups_per_file=raw["row_groups_per_file"],
        rows_per_group=raw["rows_per_group"],
        indexed_columns=raw["indexed_columns"],
        bloom_columns=raw["bloom_columns"],
        single=Location(**raw["single"]),
        multi=_loc_list(raw["multi"]),
        prefix_term=raw["prefix_term"],
        prefix_locations=_loc_list(raw["prefix_locations"]),
        domain_suffix=raw["domain_suffix"],
        domain_reversed_prefix=raw["domain_reversed_prefix"],
        domain_locations=_loc_list(raw["domain_locations"]),
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_shootout_gen.py::test_manifest_round_trip -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/manifest.py tests/test_shootout_gen.py
git commit -m "feat: add shootout manifest schema with json round-trip"
```

### Task 1.2: Corpus generator core — one corpus with planted needles (TDD)

**Files:**
- Create: `misc/shootout/gen_data.py`
- Test: `tests/test_shootout_gen.py` (append)

- [ ] **Step 1: Write the failing test**

Append to `tests/test_shootout_gen.py`:

```python
import pyarrow.parquet as pq  # noqa: E402
from gen_data import generate_corpus  # noqa: E402


def test_generate_corpus_plants_needle_and_blooms(tmp_path):
    m = generate_corpus(
        out_dir=tmp_path, n_files=3, row_groups_per_file=2,
        rows_per_group=500, seed=7,
    )
    # The single needle is where the manifest says it is.
    f = pq.ParquetFile(m.single.file)
    rg = f.read_row_group(m.single.row_group, columns=[m.single.column])
    vals = rg.column(0).to_pylist()
    assert m.single.value in vals

    # Bloom presence: pyarrow may or may not expose `has_bloom_filter` depending
    # on version. When it does, assert it; either way the authoritative,
    # functional bloom-presence gate is the Rust `probe_bloom` test in Phase 3,
    # which only passes if blooms were written and are readable.
    meta = pq.ParquetFile(m.single.file).metadata
    rg_meta = meta.row_group(m.single.row_group)
    col_names = [meta.schema.column(i).name for i in range(meta.num_columns)]
    idx = col_names.index(m.single.column)
    has_bloom = getattr(rg_meta.column(idx), "has_bloom_filter", None)
    if has_bloom is not None:
        assert has_bloom is True
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_shootout_gen.py::test_generate_corpus_plants_needle_and_blooms -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'gen_data'`.

- [ ] **Step 3: Write minimal implementation**

Create `misc/shootout/gen_data.py`:

```python
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy>=2.4.0", "pyarrow>=22.0.0"]
# ///
"""Generate a ladder of Parquet corpora (with bloom filters) + manifests."""
from __future__ import annotations

import argparse
import random
from pathlib import Path

import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq

from manifest import Manifest, Location

INDEXED = ["src_ip", "dst_ip", "domain_rev", "email"]
SINGLE_NEEDLE = "192.168.133.7"
PREFIX_TERM = "192.168.133."          # matches 192.168.133.x
DOMAIN_SUFFIX = "evil.com"
DOMAIN_REVERSED_PREFIX = "com.evil."  # matches *.evil.com (reversed labels)


def _rand_ip(rng: random.Random) -> str:
    return f"{rng.randint(1,255)}.{rng.randint(0,255)}.{rng.randint(0,255)}.{rng.randint(1,255)}"


def _reverse_domain(domain: str) -> str:
    return ".".join(reversed(domain.split(".")))


def _rand_domain(rng: random.Random) -> str:
    tld = rng.choice(["com", "net", "org", "io"])
    name = rng.choice(["alpha", "bravo", "delta", "omega", "zulu"])
    sub = rng.choice(["www", "api", "mail", "cdn"])
    return f"{sub}.{name}.{tld}"


def _build_table(n_rows: int, rng: random.Random, np_rng: np.random.Generator,
                 injected: dict[str, dict[int, str]]) -> pa.Table:
    """injected maps column -> {row_index: forced_value} for this row group."""
    cols = {
        "src_ip": [_rand_ip(rng) for _ in range(n_rows)],
        "dst_ip": [_rand_ip(rng) for _ in range(n_rows)],
        "domain_rev": [_reverse_domain(_rand_domain(rng)) for _ in range(n_rows)],
        "email": [f"user{rng.randint(0, 99999)}@{_rand_domain(rng)}"
                  for _ in range(n_rows)],
    }
    for c, cells in injected.items():
        for row, val in cells.items():
            cols[c][row] = val

    return pa.table({
        "src_ip": cols["src_ip"],
        "dst_ip": cols["dst_ip"],
        "domain_rev": cols["domain_rev"],
        "email": cols["email"],
        "bytes": np_rng.integers(64, 1500, size=n_rows),
    })


def generate_corpus(out_dir, n_files: int, row_groups_per_file: int,
                    rows_per_group: int, seed: int = 42) -> Manifest:
    out_dir = Path(out_dir)
    data_dir = out_dir / "data"
    index_dir = out_dir / "index"
    data_dir.mkdir(parents=True, exist_ok=True)
    index_dir.mkdir(parents=True, exist_ok=True)
    rng = random.Random(seed)
    np_rng = np.random.default_rng(seed)

    rows_per_file = row_groups_per_file * rows_per_group
    bloom_opts = {c: True for c in INDEXED}

    # Plant the single needle in the last file, last row group, row 0.
    needle_file_idx = n_files - 1
    needle_rg = row_groups_per_file - 1

    # Plant domain suffix + prefix samples in file 0, row group 0.
    prefix_value = "192.168.133.42"
    domain_value = _reverse_domain("www.evil.com")  # com.evil.www

    single: Location | None = None
    prefix_locations: list[Location] = []
    domain_locations: list[Location] = []
    multi_locations: list[Location] = []

    for fi in range(n_files):
        path = data_dir / f"part_{fi:05d}.parquet"
        # Build each row group as an independent table of exactly rows_per_group
        # rows, with distinct planted cells at known rows so nothing collides:
        #   row 0 -> multi-IOC value, row 1 -> prefix sample, row 2 -> needle.
        row_groups: list[pa.Table] = []
        for rgi in range(row_groups_per_file):
            injected: dict[str, dict[int, str]] = {}
            if rgi == 0:
                ioc = f"10.{fi // 256}.{fi % 256}.7"
                injected.setdefault("src_ip", {})[0] = ioc
                multi_locations.append(Location("src_ip", ioc, str(path), rgi))
            if fi == 0 and rgi == 0:
                injected.setdefault("src_ip", {})[1] = prefix_value
                injected.setdefault("domain_rev", {})[0] = domain_value
                prefix_locations.append(
                    Location("src_ip", prefix_value, str(path), rgi))
                domain_locations.append(
                    Location("domain_rev", domain_value, str(path), rgi))
            if fi == needle_file_idx and rgi == needle_rg:
                injected.setdefault("src_ip", {})[2] = SINGLE_NEEDLE
                single = Location("src_ip", SINGLE_NEEDLE, str(path), rgi)
            row_groups.append(
                _build_table(rows_per_group, rng, np_rng, injected))

        # Concatenate in order and write in a single pass with bloom filters.
        # Writing the combined table with row_group_size == rows_per_group
        # reproduces the same row-group boundaries, so planted row-group indices
        # hold.
        full = pa.concat_tables(row_groups)
        pq.write_table(full, path, row_group_size=rows_per_group,
                       use_dictionary=False, bloom_filter_options=bloom_opts)

    assert single is not None, "single needle not planted"
    multi = multi_locations

    m = Manifest(
        corpus_dir=str(data_dir), index_dir=str(index_dir),
        n_files=n_files, row_groups_per_file=row_groups_per_file,
        rows_per_group=rows_per_group, indexed_columns=INDEXED,
        bloom_columns=INDEXED, single=single, multi=multi,
        prefix_term=PREFIX_TERM, prefix_locations=prefix_locations,
        domain_suffix=DOMAIN_SUFFIX,
        domain_reversed_prefix=DOMAIN_REVERSED_PREFIX,
        domain_locations=domain_locations,
    )
    m.save(out_dir / "manifest.json")
    return m


def main() -> None:
    ap = argparse.ArgumentParser(description="Generate shootout corpora")
    ap.add_argument("--root", default="misc/shootout/corpora")
    ap.add_argument("--ladder", default="10,100,1000",
                    help="comma-separated file counts")
    ap.add_argument("--row-groups", type=int, default=8)
    ap.add_argument("--rows-per-group", type=int, default=100000)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()
    for n in [int(x) for x in args.ladder.split(",")]:
        out = Path(args.root) / f"files_{n}"
        print(f"Generating corpus: {n} files -> {out}")
        generate_corpus(out, n, args.row_groups, args.rows_per_group, args.seed)
        print(f"  manifest: {out / 'manifest.json'}")


if __name__ == "__main__":
    main()
```

> Implementation note: each row group is built as its own `rows_per_group`-row
> table, concatenated, and written in a single pass with `bloom_filter_options`.
> Writing the combined table with `row_group_size == rows_per_group` reproduces the
> per-row-group boundaries, so planted `(row_group)` indices in the manifest stay
> valid. The functional gate that blooms exist is the Phase 3 Rust `probe_bloom`
> test, not the pyarrow metadata attribute.

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_shootout_gen.py::test_generate_corpus_plants_needle_and_blooms -v`
Expected: PASS. If `has_bloom_filter` is False, the pinned pyarrow's bloom argument differs — switch the final write to the single-pass form and re-run; do not proceed until this passes.

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/gen_data.py tests/test_shootout_gen.py
git commit -m "feat: generate shootout corpora with bloom filters and manifest"
```

### Task 1.3: Build PDQ FST indexes for each generated corpus

**Files:**
- Modify: `misc/shootout/gen_data.py` (add index build step)

- [ ] **Step 1: Write the failing test**

Append to `tests/test_shootout_gen.py`:

```python
from gen_data import generate_corpus_and_index  # noqa: E402


def test_generate_and_index_builds_fst(tmp_path):
    m = generate_corpus_and_index(
        out_dir=tmp_path, n_files=2, row_groups_per_file=2,
        rows_per_group=300, seed=11, pdq_bin="target/release/pdq",
    )
    fst_files = list(Path(m.index_dir).rglob("src_ip.fst"))
    assert len(fst_files) == 2
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo build --release && uv run pytest tests/test_shootout_gen.py::test_generate_and_index_builds_fst -v`
Expected: FAIL with `ImportError: cannot import name 'generate_corpus_and_index'`.

- [ ] **Step 3: Write minimal implementation**

Add to `misc/shootout/gen_data.py`:

```python
import subprocess


def generate_corpus_and_index(out_dir, n_files, row_groups_per_file,
                              rows_per_group, seed=42,
                              pdq_bin="target/release/pdq") -> Manifest:
    m = generate_corpus(out_dir, n_files, row_groups_per_file,
                        rows_per_group, seed)
    for col in m.indexed_columns:
        subprocess.run(
            [pdq_bin, "index", "--path", m.corpus_dir,
             "--column", col, "--output", m.index_dir],
            check=True, capture_output=True,
        )
    return m
```

And call it from `main()` (replace the `generate_corpus(...)` line in the loop):

```python
        generate_corpus_and_index(out, n, args.row_groups,
                                  args.rows_per_group, args.seed)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_shootout_gen.py::test_generate_and_index_builds_fst -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/gen_data.py tests/test_shootout_gen.py
git commit -m "feat: build PDQ FST indexes during corpus generation"
```

---

## Phase 2: Ground-truth oracle

### Task 2.1: Oracle computes the true matching row count (TDD)

**Files:**
- Create: `misc/shootout/oracle.py`
- Test: `tests/test_shootout_oracle.py`

- [ ] **Step 1: Write the failing test**

Create `tests/test_shootout_oracle.py`:

```python
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "misc" / "shootout"))

from gen_data import generate_corpus  # noqa: E402
from oracle import exact_count, prefix_count  # noqa: E402


def test_oracle_finds_single_needle(tmp_path):
    m = generate_corpus(tmp_path, n_files=3, row_groups_per_file=2,
                        rows_per_group=400, seed=3)
    assert exact_count(m.corpus_dir, m.single.column, m.single.value) >= 1


def test_oracle_prefix_matches(tmp_path):
    m = generate_corpus(tmp_path, n_files=2, row_groups_per_file=2,
                        rows_per_group=400, seed=3)
    assert prefix_count(m.corpus_dir, "src_ip", m.prefix_term) >= 1
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_shootout_oracle.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'oracle'`.

- [ ] **Step 3: Write minimal implementation**

Create `misc/shootout/oracle.py`:

```python
# /// script
# requires-python = ">=3.10"
# dependencies = ["polars>=0.20.4"]
# ///
"""Ground-truth oracle: brute-force row counts used to validate contestants."""
from __future__ import annotations

from pathlib import Path

import polars as pl


def _scan(corpus_dir: str) -> pl.LazyFrame:
    pattern = str(Path(corpus_dir) / "**" / "*.parquet")
    return pl.scan_parquet(pattern)


def exact_count(corpus_dir: str, column: str, value: str) -> int:
    return (_scan(corpus_dir)
            .filter(pl.col(column) == value)
            .select(pl.len())
            .collect()
            .item())


def prefix_count(corpus_dir: str, column: str, prefix: str) -> int:
    return (_scan(corpus_dir)
            .filter(pl.col(column).str.starts_with(prefix))
            .select(pl.len())
            .collect()
            .item())


def in_count(corpus_dir: str, column: str, values: list[str]) -> int:
    return (_scan(corpus_dir)
            .filter(pl.col(column).is_in(values))
            .select(pl.len())
            .collect()
            .item())
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_shootout_oracle.py -v`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/oracle.py tests/test_shootout_oracle.py
git commit -m "feat: add brute-force oracle for shootout validation"
```

---

## Phase 3: Layer-1 Rust pruning-cost bench

### Task 3.1: Counting `ChunkReader` + bloom probe returning matched row groups (TDD)

**Files:**
- Create: `src/bloom_probe.rs`
- Modify: `src/lib.rs` (register feature-gated module)
- Test: inline `#[cfg(test)]` in `src/bloom_probe.rs`, run with `--features shootout`

- [ ] **Step 1: Write the failing test**

Create `src/bloom_probe.rs` with only the test (implementation comes in Step 3):

```rust
//! Benchmark-only: read Parquet bloom filters and probe them, plus a
//! byte-counting ChunkReader so the shootout can measure exact pruning I/O.
//! Compiled only under `--features shootout`.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn bloom_probe_finds_planted_value() {
        // Generated by the Python harness into a fixed fixture dir before
        // running: `uv run misc/shootout/gen_data.py --root /tmp/pdq_fix \
        //   --ladder 2 --row-groups 2 --rows-per-group 300`
        let file =
            Path::new("/tmp/pdq_fix/files_2/data/part_00001.parquet");
        if !file.exists() {
            eprintln!("fixture missing; skipping");
            return;
        }
        let (rgs, bytes) = probe_bloom(file, "src_ip", "192.168.133.7").unwrap();
        assert!(!rgs.is_empty());
        assert!(bytes > 0);
    }
}
```

- [ ] **Step 2: Register the module and run to verify it fails**

Add to `src/lib.rs` after the existing `pub mod` lines:

```rust
#[cfg(feature = "shootout")]
pub mod bloom_probe;
```

Run: `cargo test --features shootout bloom_probe 2>&1 | head -30`
Expected: FAIL — compile error, `cannot find function probe_bloom in this scope`.

- [ ] **Step 3: Write minimal implementation**

Replace the top of `src/bloom_probe.rs` (above the test module) with:

```rust
use anyhow::Result;
use bytes::Bytes;
use datafusion::parquet::file::reader::{ChunkReader, FileReader, SerializedFileReader};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A ChunkReader wrapper that tallies every byte handed out, so we can report
/// exact pruning I/O independent of OS page-cache behavior.
pub struct CountingReader {
    inner: File,
    pub bytes: Arc<AtomicU64>,
}

impl CountingReader {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self { inner: File::open(path)?, bytes: Arc::new(AtomicU64::new(0)) })
    }
}

pub struct CountingRead<R: Read> {
    inner: R,
    bytes: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl ChunkReader for CountingReader {
    type T = CountingRead<File>;

    fn get_read(&self, start: u64) -> datafusion::parquet::errors::Result<Self::T> {
        Ok(CountingRead {
            inner: self.inner.get_read(start)?,
            bytes: self.bytes.clone(),
        })
    }

    fn get_bytes(
        &self,
        start: u64,
        length: usize,
    ) -> datafusion::parquet::errors::Result<Bytes> {
        self.bytes.fetch_add(length as u64, Ordering::Relaxed);
        self.inner.get_bytes(start, length)
    }

    fn len(&self) -> u64 {
        self.inner.len()
    }
}

/// Probe the per-row-group bloom filter for `column` against `value`.
/// Returns (matched row groups, bytes read to make the decision).
pub fn probe_bloom(
    path: &Path,
    column: &str,
    value: &str,
) -> Result<(Vec<usize>, u64)> {
    let reader = CountingReader::new(path)?;
    let counter = reader.bytes.clone();
    let file_reader = SerializedFileReader::new(reader)?;
    let meta = file_reader.metadata();
    let col_idx = meta
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .position(|c| c.name() == column)
        .ok_or_else(|| anyhow::anyhow!("column {column} not found"))?;

    let mut matched = Vec::new();
    for rg in 0..meta.num_row_groups() {
        let rg_reader = file_reader.get_row_group(rg)?;
        if let Some(sbbf) = rg_reader.get_column_bloom_filter(col_idx) {
            if sbbf.check(&value) {
                matched.push(rg);
            }
        }
    }
    Ok((matched, counter.load(Ordering::Relaxed)))
}
```

- [ ] **Step 4: Generate the fixture and run the test**

Run:
```bash
uv run misc/shootout/gen_data.py --root /tmp/pdq_fix --ladder 2 --row-groups 2 --rows-per-group 300
cargo test --features shootout bloom_probe -- --nocapture
```
Expected: PASS (`bloom_probe_finds_planted_value`).

> If `get_column_bloom_filter` returns `None`, the reader was not configured to
> read blooms. Construct the reader via
> `SerializedFileReader::new_with_options(reader,
> ReadOptionsBuilder::new().with_reader_properties(...).build())` enabling bloom
> reads — adjust here and re-run. The test is the gate.

- [ ] **Step 5: Commit**

```bash
git add src/bloom_probe.rs src/lib.rs
git commit -m "feat: add feature-gated bloom probe with byte-counting reader"
```

### Task 3.2: PDQ FST pruning bytes-read helper (TDD)

**Files:**
- Modify: `src/bloom_probe.rs` (add `fst_index_bytes`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `src/bloom_probe.rs`:

```rust
    #[test]
    fn fst_bytes_sums_column_indexes() {
        let index_dir = Path::new("/tmp/pdq_fix/files_2/index");
        if !index_dir.exists() {
            eprintln!("fixture missing; skipping");
            return;
        }
        let bytes = fst_index_bytes(index_dir, "src_ip").unwrap();
        assert!(bytes > 0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --features shootout fst_bytes -- --nocapture 2>&1 | head -20`
Expected: FAIL — `cannot find function fst_index_bytes`.

- [ ] **Step 3: Write minimal implementation**

Add to `src/bloom_probe.rs` (above the test module):

```rust
use walkdir::WalkDir;

/// Total on-disk size of all `<column>.fst` files PDQ must consult — the
/// FST footprint scanned to make the pruning decision.
pub fn fst_index_bytes(index_dir: &Path, column: &str) -> Result<u64> {
    let target = format!("{column}.fst");
    let mut total = 0u64;
    for entry in WalkDir::new(index_dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_name().to_string_lossy() == target {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --features shootout fst_bytes -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bloom_probe.rs
git commit -m "feat: add FST index footprint measurement for shootout"
```

### Task 3.3: Bench main — time both mechanisms over the ladder, emit JSON

**Files:**
- Create: `benches/pruning_cost.rs`

- [ ] **Step 1: Write the bench main**

Create `benches/pruning_cost.rs`:

```rust
//! Layer-1 pruning-cost bench (harness = false). Run after generating corpora:
//!   uv run misc/shootout/gen_data.py --root misc/shootout/corpora
//!   cargo bench --features shootout --bench pruning_cost
//! Emits misc/shootout/results/pruning_cost.json

use pdq::bloom_probe::{fst_index_bytes, probe_bloom};
use pdq::IndexQueryEngine;
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
            n_files: n, mechanism: "pdq_fst".into(), workload: "single".into(),
            median_ms: ms, bytes_read: fst_bytes, matched_files: matched,
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
            n_files: n, mechanism: "bloom".into(), workload: "single".into(),
            median_ms: ms, bytes_read: bloom_bytes, matched_files: bloom_matched,
        });

        // --- Multi-IOC: probe the list of planted IOCs ---
        let iocs: Vec<String> = manifest["multi"]["locations"]
            .as_array().unwrap().iter()
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
            n_files: n, mechanism: "pdq_fst".into(), workload: "multi".into(),
            median_ms: ms, bytes_read: fst_bytes, matched_files: m_files,
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
            n_files: n, mechanism: "bloom".into(), workload: "multi".into(),
            median_ms: ms, bytes_read: mb_bytes, matched_files: mb_files,
        });

        eprintln!("done n={n}");
    }

    let out_dir = Path::new("misc/shootout/results");
    std::fs::create_dir_all(out_dir).unwrap();
    let out = out_dir.join("pruning_cost.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    println!("wrote {out:?} ({} rows)", rows.len());
}
```

- [ ] **Step 2: Add bench-only dev-dependencies**

`serde`, `serde_json`, and `walkdir` are needed by the bench. `serde`/`serde_json`
are already in `[dev-dependencies]`; `walkdir` is a normal dependency (available to
benches). Add `serde` derive availability to benches by confirming the dev-dep has
`features = ["derive"]` (it does). No Cargo change expected — verify in Step 3.

- [ ] **Step 3: Build the bench**

Run: `cargo build --features shootout --bench pruning_cost 2>&1 | tail -20`
Expected: compiles cleanly. If `serde::Serialize` derive is unavailable in the bench,
add to `[dev-dependencies]`: `serde = { version = "1.0", features = ["derive"] }`
(already present per Cargo.toml) and rebuild.

- [ ] **Step 4: Smoke-run against a tiny corpus**

Run:
```bash
uv run misc/shootout/gen_data.py --root misc/shootout/corpora --ladder 10 --row-groups 2 --rows-per-group 1000
cargo bench --features shootout --bench pruning_cost
cat misc/shootout/results/pruning_cost.json | head -20
```
Expected: JSON array with rows for `n_files=10`, both mechanisms, both workloads;
`matched_files` for `pdq_fst`/`single` ≥ 1; `bytes_read` > 0 for both.

- [ ] **Step 5: Commit**

```bash
git add benches/pruning_cost.rs
git commit -m "feat: add Layer-1 pruning-cost bench emitting JSON results"
```

---

## Phase 4: Layer-2 end-to-end orchestrator

### Task 4.1: Contestant adapters returning (row_count, seconds) (TDD)

**Files:**
- Create: `misc/shootout/run_e2e.py`
- Test: `tests/test_shootout_contestants.py`

- [ ] **Step 1: Write the failing test**

Create `tests/test_shootout_contestants.py`:

```python
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "misc" / "shootout"))

from gen_data import generate_corpus_and_index  # noqa: E402
from oracle import exact_count  # noqa: E402
from run_e2e import run_duckdb, run_datafusion_bloom, run_polars, run_pdq_cli  # noqa: E402


def _truth(m):
    return exact_count(m.corpus_dir, m.single.column, m.single.value)


def test_all_contestants_match_oracle(tmp_path):
    m = generate_corpus_and_index(tmp_path, n_files=3, row_groups_per_file=2,
                                  rows_per_group=500, seed=5,
                                  pdq_bin="target/release/pdq")
    truth = _truth(m)
    assert truth >= 1
    col, val = m.single.column, m.single.value
    for fn in (run_duckdb, run_polars, run_datafusion_bloom):
        rows, secs = fn(m.corpus_dir, col, val)
        assert rows == truth, f"{fn.__name__}: {rows} != {truth}"
        assert secs > 0
    rows, secs = run_pdq_cli(m.index_dir, m.corpus_dir, col, val,
                             pdq_bin="target/release/pdq")
    assert rows == truth
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo build --release && uv run --with duckdb --with datafusion --with polars --with pyarrow pytest tests/test_shootout_contestants.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'run_e2e'`.

- [ ] **Step 3: Write minimal implementation**

Create `misc/shootout/run_e2e.py`:

```python
# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "duckdb>=1.1.0", "datafusion>=43.0.0", "polars>=0.20.4",
#   "pyarrow>=22.0.0",
# ]
# ///
"""Layer-2 end-to-end orchestrator: time each contestant, validate vs oracle."""
from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path

import duckdb
import datafusion
import polars as pl

from manifest import Manifest
from oracle import exact_count


def _glob(corpus_dir: str) -> str:
    return str(Path(corpus_dir) / "**" / "*.parquet")


def run_polars(corpus_dir: str, column: str, value: str) -> tuple[int, float]:
    t = time.perf_counter()
    n = (pl.scan_parquet(_glob(corpus_dir))
         .filter(pl.col(column) == value)
         .select(pl.len()).collect().item())
    return n, time.perf_counter() - t


def run_duckdb(corpus_dir: str, column: str, value: str) -> tuple[int, float]:
    con = duckdb.connect()
    glob = _glob(corpus_dir)
    t = time.perf_counter()
    n = con.execute(
        f"SELECT count(*) FROM read_parquet('{glob}') WHERE {column} = ?",
        [value],
    ).fetchone()[0]
    return int(n), time.perf_counter() - t


def run_datafusion_bloom(corpus_dir: str, column: str, value: str) -> tuple[int, float]:
    cfg = (datafusion.SessionConfig()
           .set("datafusion.execution.parquet.bloom_filter_on_read", "true"))
    ctx = datafusion.SessionContext(cfg)
    ctx.register_parquet("t", corpus_dir, file_extension=".parquet")
    t = time.perf_counter()
    df = ctx.sql(f"SELECT count(*) AS c FROM t WHERE {column} = '{value}'")
    n = df.collect()[0].column(0)[0].as_py()
    return int(n), time.perf_counter() - t


def run_pdq_cli(index_dir: str, corpus_dir: str, column: str, value: str,
                pdq_bin: str = "target/release/pdq") -> tuple[int, float]:
    t = time.perf_counter()
    proc = subprocess.run(
        [pdq_bin, "query", "--column", column, "--term", value,
         "--index-dir", index_dir, "--data-path", corpus_dir,
         "--format", "jsonl"],
        check=True, capture_output=True, text=True,
    )
    secs = time.perf_counter() - t
    # Count jsonl data lines (those starting with '{').
    n = sum(1 for ln in proc.stdout.splitlines() if ln.startswith("{"))
    return n, secs


CONTESTANTS = {
    "polars_fullscan": run_polars,
    "duckdb_bloom": run_duckdb,
    "datafusion_bloom": run_datafusion_bloom,
}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="misc/shootout/corpora")
    ap.add_argument("--ladder", default="10,100,1000")
    ap.add_argument("--pdq-bin", default="target/release/pdq")
    ap.add_argument("--out", default="misc/shootout/results/e2e.json")
    args = ap.parse_args()

    results = []
    for n in [int(x) for x in args.ladder.split(",")]:
        base = Path(args.root) / f"files_{n}"
        m = Manifest.load(base / "manifest.json")
        col, val = m.single.column, m.single.value
        truth = exact_count(m.corpus_dir, col, val)
        for name, fn in CONTESTANTS.items():
            rows, secs = fn(m.corpus_dir, col, val)
            ok = rows == truth
            results.append({"n_files": n, "contestant": name, "workload": "single",
                            "rows": rows, "truth": truth, "correct": ok,
                            "seconds": secs})
            print(f"n={n} {name}: {secs*1000:.1f} ms rows={rows} ok={ok}")
        rows, secs = run_pdq_cli(m.index_dir, m.corpus_dir, col, val, args.pdq_bin)
        ok = rows == truth
        results.append({"n_files": n, "contestant": "pdq_cli", "workload": "single",
                        "rows": rows, "truth": truth, "correct": ok, "seconds": secs})
        print(f"n={n} pdq_cli: {secs*1000:.1f} ms rows={rows} ok={ok}")

    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(results, indent=2))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
```

- [ ] **Step 4: Run to verify it passes**

Run: `uv run --with duckdb --with datafusion --with polars --with pyarrow pytest tests/test_shootout_contestants.py -v`
Expected: PASS. If `run_datafusion_bloom` row count differs from truth, the
predicate or bloom config is off — verify with `ctx.sql("EXPLAIN ANALYZE ...")`
and confirm `row_groups_pruned_bloom_filter` appears; fix before continuing.

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/run_e2e.py tests/test_shootout_contestants.py
git commit -m "feat: add Layer-2 end-to-end orchestrator with oracle validation"
```

### Task 4.2: Verify DataFusion bloom pruning is actually active (correctness gate)

**Files:**
- Modify: `misc/shootout/run_e2e.py` (add an assertion helper)
- Test: `tests/test_shootout_contestants.py` (append)

- [ ] **Step 1: Write the failing test**

Append to `tests/test_shootout_contestants.py`:

```python
from run_e2e import datafusion_bloom_pruned_count  # noqa: E402


def test_datafusion_bloom_actually_prunes(tmp_path):
    m = generate_corpus_and_index(tmp_path, n_files=4, row_groups_per_file=2,
                                  rows_per_group=500, seed=8,
                                  pdq_bin="target/release/pdq")
    pruned = datafusion_bloom_pruned_count(
        m.corpus_dir, m.single.column, m.single.value)
    assert pruned > 0, "bloom pruning did not fire — benchmark would be invalid"
```

- [ ] **Step 2: Run to verify it fails**

Run: `uv run --with duckdb --with datafusion --with polars --with pyarrow pytest tests/test_shootout_contestants.py::test_datafusion_bloom_actually_prunes -v`
Expected: FAIL — `cannot import name 'datafusion_bloom_pruned_count'`.

- [ ] **Step 3: Write minimal implementation**

Add to `misc/shootout/run_e2e.py`:

```python
def datafusion_bloom_pruned_count(corpus_dir: str, column: str, value: str) -> int:
    """Return the row_groups_pruned_bloom_filter metric from EXPLAIN ANALYZE."""
    cfg = (datafusion.SessionConfig()
           .set("datafusion.execution.parquet.bloom_filter_on_read", "true"))
    ctx = datafusion.SessionContext(cfg)
    ctx.register_parquet("t", corpus_dir, file_extension=".parquet")
    plan = ctx.sql(
        f"EXPLAIN ANALYZE SELECT count(*) FROM t WHERE {column} = '{value}'"
    ).collect()
    text = "\n".join(
        plan[i].column(c)[r].as_py()
        for i in range(len(plan))
        for c in range(plan[i].num_columns)
        for r in range(plan[i].num_rows)
        if isinstance(plan[i].column(c)[r].as_py(), str)
    )
    import re
    matches = re.findall(r"row_groups_pruned_bloom_filter=(\d+)", text)
    return sum(int(x) for x in matches)
```

- [ ] **Step 4: Run to verify it passes**

Run: `uv run --with duckdb --with datafusion --with polars --with pyarrow pytest tests/test_shootout_contestants.py::test_datafusion_bloom_actually_prunes -v`
Expected: PASS. If `pruned == 0`, blooms aren't being used — the corpus needs
higher selectivity or the config key is wrong for the pinned datafusion version;
resolve before trusting any Layer-2 number.

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/run_e2e.py tests/test_shootout_contestants.py
git commit -m "test: assert DataFusion bloom pruning fires before benchmarking"
```

### Task 4.3: Cold-cache wrapper script

**Files:**
- Create: `misc/shootout/run_cold.sh`

- [ ] **Step 1: Write the wrapper**

Create `misc/shootout/run_cold.sh`:

```bash
#!/usr/bin/env bash
# Cold-cache Layer-2 run. macOS: needs `sudo purge`. Linux: drop_caches.
# Usage: misc/shootout/run_cold.sh [ladder]
set -euo pipefail
LADDER="${1:-10,100,1000}"

purge_cache() {
  if command -v purge >/dev/null 2>&1; then
    sync && sudo purge
  elif [ -w /proc/sys/vm/drop_caches ] || sudo -n true 2>/dev/null; then
    sync && echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
  else
    echo "WARN: cannot purge cache; results are warm" >&2
  fi
}

purge_cache
uv run --with duckdb --with datafusion --with polars --with pyarrow \
  misc/shootout/run_e2e.py --ladder "$LADDER" \
  --out misc/shootout/results/e2e_cold.json
```

- [ ] **Step 2: Make it executable and lint it**

Run: `chmod +x misc/shootout/run_cold.sh && bash -n misc/shootout/run_cold.sh && echo OK`
Expected: `OK` (syntax check passes).

- [ ] **Step 3: Commit**

```bash
git add misc/shootout/run_cold.sh
git commit -m "feat: add cold-cache wrapper for end-to-end shootout runs"
```

---

## Phase 5: Reporting

### Task 5.1: Render markdown table + plots from results JSON (TDD)

**Files:**
- Create: `misc/shootout/plot.py`
- Test: `tests/test_shootout_report.py`

- [ ] **Step 1: Write the failing test**

Create `tests/test_shootout_report.py`:

```python
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "misc" / "shootout"))

from plot import build_markdown_table  # noqa: E402


def test_markdown_table_has_rows(tmp_path):
    pruning = [
        {"n_files": 10, "mechanism": "pdq_fst", "workload": "single",
         "median_ms": 1.0, "bytes_read": 100, "matched_files": 1},
        {"n_files": 10, "mechanism": "bloom", "workload": "single",
         "median_ms": 5.0, "bytes_read": 9000, "matched_files": 1},
    ]
    md = build_markdown_table(pruning)
    assert "pdq_fst" in md and "bloom" in md
    assert "| 10 |" in md
```

- [ ] **Step 2: Run to verify it fails**

Run: `uv run pytest tests/test_shootout_report.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'plot'`.

- [ ] **Step 3: Write minimal implementation**

Create `misc/shootout/plot.py`:

```python
# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib>=3.8"]
# ///
"""Render the shootout report: markdown table + scaling plots."""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def build_markdown_table(pruning: list[dict]) -> str:
    header = ("| n_files | mechanism | workload | median_ms | bytes_read |"
              " matched_files |\n|---|---|---|---|---|---|\n")
    body = "".join(
        f"| {r['n_files']} | {r['mechanism']} | {r['workload']} |"
        f" {r['median_ms']:.3f} | {r['bytes_read']} | {r['matched_files']} |\n"
        for r in sorted(pruning, key=lambda r: (r["n_files"], r["workload"],
                                                r["mechanism"]))
    )
    return header + body


def _plot_scaling(pruning: list[dict], out_png: Path) -> None:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, (ax_t, ax_b) = plt.subplots(1, 2, figsize=(12, 5))
    for mech in ("pdq_fst", "bloom"):
        pts = sorted(
            [(r["n_files"], r["median_ms"], r["bytes_read"])
             for r in pruning
             if r["mechanism"] == mech and r["workload"] == "single"],
            key=lambda x: x[0])
        if not pts:
            continue
        xs = [p[0] for p in pts]
        ax_t.plot(xs, [p[1] for p in pts], marker="o", label=mech)
        ax_b.plot(xs, [p[2] for p in pts], marker="o", label=mech)
    for ax, title, ylab in ((ax_t, "Pruning time vs #files", "median ms"),
                            (ax_b, "Pruning bytes vs #files", "bytes read")):
        ax.set_xscale("log")
        ax.set_yscale("log")
        ax.set_xlabel("#files")
        ax.set_ylabel(ylab)
        ax.set_title(title)
        ax.legend()
        ax.grid(True, which="both", alpha=0.3)
    fig.tight_layout()
    fig.savefig(out_png, dpi=120)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default="misc/shootout/results")
    args = ap.parse_args()
    rdir = Path(args.results)
    pruning = json.loads((rdir / "pruning_cost.json").read_text())

    md = ["# Shootout Results\n", "## Layer 1: pruning cost\n",
          build_markdown_table(pruning)]
    e2e_path = rdir / "e2e.json"
    if e2e_path.exists():
        e2e = json.loads(e2e_path.read_text())
        md.append("\n## Layer 2: end-to-end (warm)\n")
        md.append("| n_files | contestant | rows | correct | ms |\n"
                  "|---|---|---|---|---|\n")
        for r in sorted(e2e, key=lambda r: (r["n_files"], r["contestant"])):
            md.append(f"| {r['n_files']} | {r['contestant']} | {r['rows']} |"
                      f" {r['correct']} | {r['seconds']*1000:.1f} |\n")

    (rdir / "REPORT.md").write_text("".join(md))
    _plot_scaling(pruning, rdir / "pruning_scaling.png")
    print(f"wrote {rdir / 'REPORT.md'} and {rdir / 'pruning_scaling.png'}")


if __name__ == "__main__":
    main()
```

- [ ] **Step 4: Run to verify it passes**

Run: `uv run pytest tests/test_shootout_report.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add misc/shootout/plot.py tests/test_shootout_report.py
git commit -m "feat: render shootout report table and scaling plots"
```

### Task 5.2: End-to-end driver doc

**Files:**
- Create: `misc/shootout/README.md`

- [ ] **Step 1: Write the runbook**

Create `misc/shootout/README.md`:

```markdown
# PDQ vs. Parquet Bloom Filter Shootout

Reproducible benchmark. See design:
`docs/superpowers/specs/2026-06-13-bloom-shootout-design.md`.

## Run it

```bash
# 1. Build PDQ (CLI + shootout feature)
cargo build --release

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
```

- [ ] **Step 2: Commit**

```bash
git add misc/shootout/README.md
git commit -m "docs: add shootout runbook"
```

---

## Phase 6 (stretch): consolidated-FST variant

**Outline only — not executable as written.** This is a deliberately deferred
stretch goal. Before implementing, run it back through the writing-plans skill to
expand the steps below into full TDD tasks with complete code. Attempt only after
Phases 1-5 produce a clean report.

### Task 6.1 (sketch): Consolidated index build + bench arm

**Files:**
- Modify: `src/bloom_probe.rs` or new `src/consolidated_fst.rs` (feature-gated)
- Modify: `benches/pruning_cost.rs`

- [ ] **Step 1:** Add a function that merges all per-file `<column>.fst` keys into
  one FST whose keys are `value\x00<file_id>\x00rgN`, written to
  `<index_dir>/_consolidated/<column>.fst`. Reuse `fst::SetBuilder`; iterate the
  existing per-file FSTs in sorted order with `fst::Set::stream()` and a
  `fst::map::OpBuilder`-style merge. Write a unit test that builds a consolidated
  FST from a 2-file fixture and asserts an exact-search over it returns the same
  `(file, rg)` pairs as querying the two per-file FSTs.
- [ ] **Step 2:** Add a `pdq_fst_consolidated` mechanism arm to the bench that
  opens the single consolidated FST once and probes it, recording its single-file
  byte footprint. This is the "true one-shot" number.
- [ ] **Step 3:** Re-run the bench and regenerate the report; the consolidated arm
  should show flat-vs-#files byte growth compared to per-file PDQ.
- [ ] **Step 4:** Commit.

---

## Self-Review Notes

- **Spec coverage:** contestants (PDQ/DF-bloom/DuckDB/Polars-floor) → Phase 4;
  consolidated-FST stretch → Phase 6; both-layered metrics → Phase 3 (pruning) +
  Phase 4 (e2e); cold/warm → Task 4.3 + `run_e2e` warm default; data-gen +
  manifest → Phase 1; correctness gate (row parity + bloom-active) → Tasks 4.1 &
  4.2; workloads single/multi/prefix → manifest carries all three; prefix showcase
  validated by oracle (`prefix_count`) and PDQ `search --type prefix` (driven from
  the README runbook). Reporting → Phase 5.
- **Prefix showcase race-vs-capability:** prefix has no bloom contestant by design;
  it is reported via the oracle prefix count + PDQ prefix search, not as a timed
  race row. This matches the spec ("capability demo, not a race").
- **Open risk carried forward:** `ScalarValue::to_string()` round-trip in PDQ's
  provider (spec open risks) is exercised implicitly by Task 4.1's PDQ-CLI parity
  check against the oracle; if PDQ returns the wrong count there, that bug is
  surfaced before any timing is trusted.
```
