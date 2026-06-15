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
import re
import subprocess
import time
from pathlib import Path

import datafusion
import duckdb
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


def _datafusion_ctx(corpus_dir: str) -> "datafusion.SessionContext":
    cfg = (datafusion.SessionConfig()
           .set("datafusion.execution.parquet.bloom_filter_on_read", "true"))
    ctx = datafusion.SessionContext(cfg)
    ctx.register_parquet("t", corpus_dir, file_extension=".parquet")
    return ctx


def run_datafusion_bloom(corpus_dir: str, column: str, value: str) -> tuple[int, float]:
    ctx = _datafusion_ctx(corpus_dir)
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


def datafusion_bloom_pruned_count(corpus_dir: str, column: str, value: str) -> int:
    """Return the row_groups_pruned_bloom_filter metric from EXPLAIN ANALYZE."""
    ctx = _datafusion_ctx(corpus_dir)
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
    matches = re.findall(r"row_groups_pruned_bloom_filter=(\d+)", text)
    return sum(int(x) for x in matches)


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
