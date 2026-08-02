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
