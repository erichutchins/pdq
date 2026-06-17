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
