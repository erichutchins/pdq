# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Generate a ladder of Parquet corpora (with bloom filters) + manifests.

The corpus bytes are written by the feature-gated Rust generator
(`pdq gen-corpus`, built with `--features shootout`) because neither pyarrow
(through 23.x) nor DuckDB can emit Parquet bloom filters from Python. This
script owns the *manifest* — it records where each needle was planted, matching
the deterministic planting scheme in `src/corpus_gen.rs`.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path

from manifest import Manifest, Location

INDEXED = ["src_ip", "dst_ip", "domain_rev", "email"]
SINGLE_NEEDLE = "192.168.133.7"
PREFIX_VALUE = "192.168.133.42"
PREFIX_TERM = "192.168.133."          # matches 192.168.133.x
DOMAIN_SUFFIX = "evil.com"
DOMAIN_REVERSED_PREFIX = "com.evil."  # matches *.evil.com (reversed labels)


def _reverse_domain(domain: str) -> str:
    return ".".join(reversed(domain.split(".")))


DOMAIN_VALUE = _reverse_domain("www.evil.com")  # com.evil.www


def generate_corpus(out_dir, n_files: int, row_groups_per_file: int,
                    rows_per_group: int, seed: int = 42,
                    pdq_bin: str = "target/release/pdq",
                    timings: dict | None = None) -> Manifest:
    out_dir = Path(out_dir)
    data_dir = out_dir / "data"
    index_dir = out_dir / "index"
    data_dir.mkdir(parents=True, exist_ok=True)
    index_dir.mkdir(parents=True, exist_ok=True)

    # Write the corpus (with per-column bloom filters) via the Rust generator.
    t0 = time.perf_counter()
    subprocess.run(
        [pdq_bin, "gen-corpus", "--out", str(data_dir),
         "--files", str(n_files), "--row-groups", str(row_groups_per_file),
         "--rows-per-group", str(rows_per_group), "--seed", str(seed)],
        check=True, capture_output=True,
    )
    if timings is not None:
        timings["corpus_write_seconds"] = time.perf_counter() - t0

    # Record planted locations — must mirror src/corpus_gen.rs exactly:
    #   every file, row group 0, row 0 -> multi-IOC 10.<hi>.<lo>.7
    #   file 0, row group 0, row 1     -> prefix sample 192.168.133.42
    #   file 0, row group 0, row 0 (domain_rev) -> com.evil.www
    #   last file, last row group, row 2 -> single needle 192.168.133.7
    needle_file_idx = n_files - 1
    needle_rg = row_groups_per_file - 1

    multi_locations: list[Location] = []
    prefix_locations: list[Location] = []
    domain_locations: list[Location] = []
    single: Location | None = None

    for fi in range(n_files):
        path = str(data_dir / f"part_{fi:05d}.parquet")
        ioc = f"10.{fi // 256}.{fi % 256}.7"
        multi_locations.append(Location("src_ip", ioc, path, 0))
        if fi == 0:
            prefix_locations.append(Location("src_ip", PREFIX_VALUE, path, 0))
            domain_locations.append(Location("domain_rev", DOMAIN_VALUE, path, 0))
        if fi == needle_file_idx:
            single = Location("src_ip", SINGLE_NEEDLE, path, needle_rg)

    assert single is not None, "single needle not planted"

    m = Manifest(
        corpus_dir=str(data_dir), index_dir=str(index_dir),
        n_files=n_files, row_groups_per_file=row_groups_per_file,
        rows_per_group=rows_per_group, indexed_columns=INDEXED,
        bloom_columns=INDEXED, single=single, multi=multi_locations,
        prefix_term=PREFIX_TERM, prefix_locations=prefix_locations,
        domain_suffix=DOMAIN_SUFFIX,
        domain_reversed_prefix=DOMAIN_REVERSED_PREFIX,
        domain_locations=domain_locations,
    )
    m.save(out_dir / "manifest.json")
    return m


def generate_corpus_and_index(out_dir, n_files, row_groups_per_file,
                              rows_per_group, seed=42,
                              pdq_bin="target/release/pdq",
                              timings: dict | None = None) -> Manifest:
    m = generate_corpus(out_dir, n_files, row_groups_per_file,
                        rows_per_group, seed, pdq_bin, timings)
    per_column: dict[str, float] = {}
    for col in m.indexed_columns:
        t0 = time.perf_counter()
        subprocess.run(
            [pdq_bin, "index", "--path", m.corpus_dir,
             "--column", col, "--output", m.index_dir],
            check=True, capture_output=True,
        )
        per_column[col] = time.perf_counter() - t0
    if timings is not None:
        timings["index_build_per_column_seconds"] = per_column
        timings["index_build_seconds"] = sum(per_column.values())
    return m


def main() -> None:
    ap = argparse.ArgumentParser(description="Generate shootout corpora")
    ap.add_argument("--root", default="misc/shootout/corpora")
    ap.add_argument("--ladder", default="10,100,1000",
                    help="comma-separated file counts")
    ap.add_argument("--row-groups", type=int, default=8)
    ap.add_argument("--rows-per-group", type=int, default=100000)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--pdq-bin", default="target/release/pdq")
    ap.add_argument("--build-time-out",
                    default="misc/shootout/results/build_time.json",
                    help="where to record corpus-write + FST-build timings")
    args = ap.parse_args()
    build_records = []
    for n in [int(x) for x in args.ladder.split(",")]:
        out = Path(args.root) / f"files_{n}"
        print(f"Generating corpus: {n} files -> {out}")
        timings: dict = {}
        generate_corpus_and_index(out, n, args.row_groups,
                                  args.rows_per_group, args.seed, args.pdq_bin,
                                  timings)
        timings["n_files"] = n
        build_records.append(timings)
        print(f"  manifest: {out / 'manifest.json'}  "
              f"(corpus {timings.get('corpus_write_seconds', 0):.1f}s, "
              f"index {timings.get('index_build_seconds', 0):.1f}s)")

    out_path = Path(args.build_time_out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(build_records, indent=2))
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
