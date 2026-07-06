# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib>=3.8"]
# ///
"""Render the shootout report: markdown tables + scaling plots.

Markdown (REPORT.md) goes to --results (gitignored, ephemeral). The committable
chart PNGs go to --assets (tracked), so they can be embedded in BENCHMARKS.md
and render on GitHub. plot.py is pure JSON -> MD/PNG, so charts can be (re)built
from any pulled results dir without re-running the benchmark.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

# Stable color + label per contestant so warm/cold panels match.
_CONTESTANT_STYLE = {
    "pdq": ("#1f77b4", "PDQ (FST index)"),
    "duckdb_bloom": ("#ff7f0e", "DuckDB (bloom)"),
    "datafusion_bloom": ("#2ca02c", "DataFusion (bloom)"),
    "polars_fullscan": ("#d62728", "Polars (full scan)"),
}


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


def _plot_pruning_time(pruning: list[dict], out_png: Path) -> None:
    """Layer-1 pruning *time* vs #files, single + multi (log-log). The bytes
    column is deliberately NOT plotted — it compares PDQ's index footprint to the
    bloom path's actual reads, which are not apples-to-apples."""
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, 2, figsize=(12, 5))
    for ax, workload in zip(axes, ("single", "multi")):
        for mech, label, color in (("pdq_fst", "PDQ FST", "#1f77b4"),
                                    ("bloom", "Parquet bloom", "#ff7f0e")):
            pts = sorted((r["n_files"], r["median_ms"]) for r in pruning
                         if r["mechanism"] == mech and r["workload"] == workload)
            if pts:
                ax.plot([p[0] for p in pts], [p[1] for p in pts],
                        marker="o", label=label, color=color)
        ax.set_xscale("log")
        ax.set_yscale("log")
        ax.set_xlabel("# files")
        ax.set_ylabel("median ms")
        ax.set_title(f"Layer 1 pruning time — {workload}")
        ax.grid(True, which="both", alpha=0.3)
        ax.legend()
    fig.suptitle("Pruning decision: time vs corpus size (lower is better)")
    fig.tight_layout()
    fig.savefig(out_png, dpi=120)
    plt.close(fig)


def _plot_e2e(frames: list[tuple[str, list[dict]]], out_png: Path) -> None:
    """Layer-2 end-to-end median latency vs #files, one panel per cache state
    (warm/cold), one line per contestant (log-log). This is the headline: PDQ
    stays ~flat (it touches only the matched file) while the engines scale with
    file count."""
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, len(frames), figsize=(6 * len(frames), 5),
                             squeeze=False)
    for ax, (label, data) in zip(axes[0], frames):
        for cont, (color, clabel) in _CONTESTANT_STYLE.items():
            pts = sorted((r["n_files"], r["median_ms"]) for r in data
                         if r["contestant"] == cont)
            if pts:
                ax.plot([p[0] for p in pts], [p[1] for p in pts],
                        marker="o", label=clabel, color=color)
        ax.set_xscale("log")
        ax.set_yscale("log")
        ax.set_xlabel("# files")
        ax.set_ylabel("median ms")
        ax.set_title(f"End-to-end latency — {label}")
        ax.grid(True, which="both", alpha=0.3)
        ax.legend()
    fig.suptitle("Layer 2: single-needle query latency vs corpus size "
                 "(lower is better)")
    fig.tight_layout()
    fig.savefig(out_png, dpi=120)
    plt.close(fig)


def _e2e_table(e2e: list[dict]) -> str:
    md = ["| n_files | contestant | rows | correct | median ms |"
          " p25–p75 ms |\n|---|---|---|---|---|---|\n"]
    for r in sorted(e2e, key=lambda r: (r["n_files"], r["contestant"])):
        md.append(
            f"| {r['n_files']} | {r['contestant']} | {r['rows']} |"
            f" {r['correct']} | {r['median_ms']:.2f} |"
            f" {r['p25_ms']:.2f}–{r['p75_ms']:.2f} |\n")
    return "".join(md)


def _build_time_table(records: list[dict]) -> str:
    md = ["| n_files | corpus write (s) | FST build (s) |\n|---|---|---|\n"]
    for r in sorted(records, key=lambda r: r["n_files"]):
        md.append(f"| {r['n_files']} | {r.get('corpus_write_seconds', 0):.1f} |"
                  f" {r.get('index_build_seconds', 0):.1f} |\n")
    return "".join(md)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default="misc/shootout/results",
                    help="dir with *.json; REPORT.md is written here")
    ap.add_argument("--assets", default="misc/shootout/assets",
                    help="tracked dir for committable chart PNGs")
    args = ap.parse_args()
    rdir = Path(args.results)
    adir = Path(args.assets)
    adir.mkdir(parents=True, exist_ok=True)
    pruning = json.loads((rdir / "pruning_cost.json").read_text())

    md = ["# Shootout Results\n", "## Layer 1: pruning cost\n",
          build_markdown_table(pruning)]

    frames: list[tuple[str, list[dict]]] = []
    for label, fname in (("warm", "e2e.json"), ("cold", "e2e_cold.json")):
        path = rdir / fname
        if path.exists():
            data = json.loads(path.read_text())
            md.append(f"\n## Layer 2: end-to-end ({label})\n")
            md.append(_e2e_table(data))
            frames.append((label, data))

    bt_path = rdir / "build_time.json"
    if bt_path.exists():
        md.append("\n## Build cost (corpus write + FST index)\n")
        md.append(_build_time_table(json.loads(bt_path.read_text())))

    (rdir / "REPORT.md").write_text("".join(md))
    _plot_pruning_time(pruning, adir / "pruning_time.png")
    outputs = [rdir / "REPORT.md", adir / "pruning_time.png"]
    if frames:
        _plot_e2e(frames, adir / "e2e_scaling.png")
        outputs.append(adir / "e2e_scaling.png")
    print("wrote " + ", ".join(str(p) for p in outputs))


if __name__ == "__main__":
    main()
