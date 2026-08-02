"""Layer-2 end-to-end orchestrator: time each contestant, validate vs oracle.

Each contestant is split into a one-time **setup** (open the connection, register
the table, build the query engine — the work a long-lived service does once) and a
**run** closure that performs only the per-query work we actually time. We take
the median + IQR over many repetitions so a single noisy sample can't dominate,
and PDQ is measured **in-process** via its Python binding (`QueryEngine`) rather
than as a CLI subprocess, so it is comparable to the other in-process engines.

Run it in the PROJECT venv (so `import pdq` resolves to the maturin-built
extension), after `uv run --group bench maturin develop`:

    uv run --group bench python misc/shootout/run_e2e.py --ladder 10,100,1000

Use --cold to drop the OS page cache before every timed sample (needs sudo;
Linux drop_caches or macOS `purge`).
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import shutil
import statistics
import subprocess
import time
from pathlib import Path

# Pin every engine's thread pool to the same width BEFORE importing the engines:
# polars and rayon (PDQ) read these env vars once, at import / first pool use.
_THREADS = os.environ.get("SHOOTOUT_THREADS") or str(os.cpu_count() or 1)
os.environ.setdefault("POLARS_MAX_THREADS", _THREADS)
os.environ.setdefault("RAYON_NUM_THREADS", _THREADS)
THREADS = int(_THREADS)

import datafusion  # noqa: E402
import duckdb  # noqa: E402
import polars as pl  # noqa: E402
import pdq  # noqa: E402  — maturin-built in-process module

from manifest import Manifest  # noqa: E402
from oracle import exact_count  # noqa: E402


def _versions() -> dict:
    """Installed versions of every contestant + PDQ, stamped into each result
    row so the numbers travel with the toolchain that produced them."""
    import importlib.metadata as md
    import sys

    out = {"python": sys.version.split()[0]}
    for dist in ("pdq", "duckdb", "datafusion", "polars", "pyarrow"):
        try:
            out[dist] = md.version(dist)
        except Exception:
            out[dist] = "unknown"
    return out


def _glob(corpus_dir: str) -> str:
    return str(Path(corpus_dir) / "**" / "*.parquet")


# --- Contestants: setup(manifest) -> run(column, value) -> int -----------------
#
# setup() does everything a warm service amortizes (connect, register, build the
# engine). The returned run() does only the per-query work we time.


def setup_polars(m: Manifest):
    # scan_parquet resolves the schema once; each collect re-reads (full scan).
    lf = pl.scan_parquet(_glob(m.corpus_dir))

    def run(column: str, value: str) -> int:
        # Matched query shape: materialize the full matching row(s) (all columns),
        # same unit of work as PDQ — then count them. `.height` is the row count.
        return lf.filter(pl.col(column) == value).collect().height

    return run


def setup_duckdb(m: Manifest):
    con = duckdb.connect()
    con.execute(f"PRAGMA threads={THREADS}")
    # Standard knob (DuckDB >=1.x): cache parsed Parquet metadata across queries
    # so a warm, long-lived connection parses each file's footer once instead of
    # per query. (`enable_object_cache` is a no-op placeholder in 1.5.x;
    # `enable_external_file_cache`, which caches file bytes, is already on.)
    con.execute("SET parquet_metadata_cache=true")
    # A view over read_parquet: the connection + plan + (now) parsed metadata are
    # amortized; each query still probes the parquet bloom filters to prune.
    con.execute(
        f"CREATE VIEW t AS SELECT * FROM read_parquet('{_glob(m.corpus_dir)}')"
    )

    def run(column: str, value: str) -> int:
        # Matched query shape: SELECT * materializes the matching row(s) (all
        # columns), same unit of work as PDQ; fetchall() then count.
        return len(con.execute(
            f"SELECT * FROM t WHERE {column} = ?", [value]
        ).fetchall())

    return run


def _datafusion_ctx(corpus_dir: str) -> "datafusion.SessionContext":
    # Standard SessionConfig knobs only (no custom ParquetFileReaderFactory).
    # pushdown_filters = apply the predicate as a row filter during decode (late
    # materialization, parity with PDQ); reorder_filters lets it evaluate the
    # cheapest predicate first. Note: DataFusion 53's Python API exposes NO
    # cross-query Parquet metadata cache knob, so warm queries still re-parse
    # footers — that gap can only be closed with a custom reader factory (Rust).
    cfg = (datafusion.SessionConfig()
           .set("datafusion.execution.parquet.bloom_filter_on_read", "true")
           .set("datafusion.execution.parquet.pushdown_filters", "true")
           .set("datafusion.execution.parquet.reorder_filters", "true")
           .set("datafusion.execution.target_partitions", str(THREADS)))
    ctx = datafusion.SessionContext(cfg)
    ctx.register_parquet("t", corpus_dir, file_extension=".parquet")
    return ctx


def setup_datafusion(m: Manifest):
    ctx = _datafusion_ctx(m.corpus_dir)

    def run(column: str, value: str) -> int:
        # Matched query shape: SELECT * materializes the matching row(s) (all
        # columns), same unit of work as PDQ; sum the returned batch rows.
        batches = ctx.sql(
            f"SELECT * FROM t WHERE {column} = '{value}'"
        ).collect()
        return sum(b.num_rows for b in batches)

    return run


def setup_pdq(m: Manifest):
    # One event loop + one QueryEngine for the whole iteration set; the engine
    # ctor does the FST mmap + DataFusion provider wiring once (outside timing).
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    engine = pdq.QueryEngine(m.index_dir, m.corpus_dir)

    async def _query(column: str, value: str):
        # query() must be *called* while the loop is running: pyo3-async-runtimes
        # binds to the running loop when the awaitable is created, not awaited.
        return await engine.query(column, value)

    def run(column: str, value: str) -> int:
        # query() returns the matching rows (not just a count); for the selective
        # needle that is ~1 row, so materialization cost is negligible.
        batches = loop.run_until_complete(_query(column, value))
        if not batches:
            return 0
        return sum(b.num_rows for b in batches)

    return run


SETUPS = {
    "polars_fullscan": setup_polars,
    "duckdb_bloom": setup_duckdb,
    "datafusion_bloom": setup_datafusion,
    "pdq": setup_pdq,
}


def datafusion_bloom_pruned_count(corpus_dir: str, column: str, value: str) -> int:
    """Return the row_groups_pruned_bloom_filter metric from EXPLAIN ANALYZE.

    A sanity gate: if this is 0 the "bloom" contestant is silently doing a full
    scan and the comparison would be meaningless.
    """
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


# --- Cache purge (cold mode) ---------------------------------------------------


def detect_purge_cmd() -> str | None:
    if shutil.which("purge"):  # macOS
        return "sync && sudo purge"
    if Path("/proc/sys/vm/drop_caches").exists():  # Linux
        return "sync && echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null"
    return None


def purge(cmd: str | None) -> None:
    if cmd:
        subprocess.run(cmd, shell=True, check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


# --- Timing --------------------------------------------------------------------


def measure(run, column, value, iters: int, warmup: int,
            purge_cmd: str | None) -> list[float]:
    for _ in range(warmup):
        run(column, value)
    samples_ms: list[float] = []
    for _ in range(iters):
        purge(purge_cmd)  # no-op unless cold mode supplied a command
        t = time.perf_counter()
        run(column, value)
        samples_ms.append((time.perf_counter() - t) * 1000.0)
    return samples_ms


def summarize(samples_ms: list[float]) -> dict:
    s = sorted(samples_ms)
    if len(s) >= 4:
        q1, _, q3 = statistics.quantiles(s, n=4)
    else:
        q1, q3 = s[0], s[-1]
    return {
        "median_ms": statistics.median(s),
        "p25_ms": q1,
        "p75_ms": q3,
        "min_ms": s[0],
        "max_ms": s[-1],
        "iters": len(s),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="misc/shootout/corpora")
    ap.add_argument("--ladder", default="10,100,1000")
    ap.add_argument("--iters", type=int, default=30)
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--cold", action="store_true",
                    help="drop the OS page cache before every timed sample")
    ap.add_argument("--contestant", action="append", dest="contestants",
                    choices=list(SETUPS),
                    help="restrict to named contestant(s); repeatable")
    ap.add_argument("--out", default="misc/shootout/results/e2e.json")
    args = ap.parse_args()

    purge_cmd = None
    iters, warmup = args.iters, args.warmup
    if args.cold:
        purge_cmd = detect_purge_cmd()
        if purge_cmd is None:
            print("WARN: no cache-purge mechanism available; 'cold' run is warm",
                  flush=True)
        # Cold = first-touch I/O. No warmup, and fewer samples (each is preceded
        # by a full cache drop, so every sample is genuinely cold).
        warmup = 0
        iters = min(iters, 5)

    names = args.contestants or list(SETUPS)
    versions = _versions()
    print(f"threads={THREADS} iters={iters} warmup={warmup} "
          f"cold={args.cold} contestants={names}", flush=True)
    print(f"versions={versions}", flush=True)

    results = []
    for n in [int(x) for x in args.ladder.split(",")]:
        base = Path(args.root) / f"files_{n}"
        m = Manifest.load(base / "manifest.json")
        col, val = m.single.column, m.single.value
        truth = exact_count(m.corpus_dir, col, val)

        if "datafusion_bloom" in names:
            pruned = datafusion_bloom_pruned_count(m.corpus_dir, col, val)
            if pruned <= 0:
                print(f"WARN n={n}: datafusion bloom pruned 0 row groups — the "
                      f"'bloom' contestant may be full-scanning", flush=True)

        for name in names:
            run = SETUPS[name](m)
            rows = run(col, val)  # correctness probe (also a free warm-up)
            ok = rows == truth
            samples = measure(run, col, val, iters, warmup, purge_cmd)
            stats = summarize(samples)
            results.append({
                "n_files": n, "contestant": name, "workload": "single",
                "rows": rows, "truth": truth, "correct": ok,
                "cold": bool(args.cold), "threads": THREADS, **stats,
                "versions": versions,
            })
            print(f"n={n} {name}: median {stats['median_ms']:.2f} ms "
                  f"(p25 {stats['p25_ms']:.2f} / p75 {stats['p75_ms']:.2f}) "
                  f"rows={rows} ok={ok}", flush=True)

    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(results, indent=2))
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
