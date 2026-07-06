import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "misc" / "shootout"))

from plot import build_markdown_table, _e2e_table, _build_time_table  # noqa: E402


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


def test_e2e_table_renders_median_and_iqr():
    e2e = [
        {"n_files": 10, "contestant": "pdq", "rows": 1, "truth": 1,
         "correct": True, "median_ms": 0.5, "p25_ms": 0.4, "p75_ms": 0.7},
        {"n_files": 10, "contestant": "duckdb_bloom", "rows": 1, "truth": 1,
         "correct": True, "median_ms": 12.0, "p25_ms": 11.0, "p75_ms": 13.5},
    ]
    md = _e2e_table(e2e)
    assert "pdq" in md and "duckdb_bloom" in md
    assert "median ms" in md and "p25–p75 ms" in md
    assert "0.40–0.70" in md  # IQR rendered for pdq


def test_build_time_table_renders():
    records = [
        {"n_files": 10, "corpus_write_seconds": 3.2, "index_build_seconds": 1.1},
        {"n_files": 100, "corpus_write_seconds": 31.0, "index_build_seconds": 9.4},
    ]
    md = _build_time_table(records)
    assert "| 10 | 3.2 | 1.1 |" in md
    assert "| 100 | 31.0 | 9.4 |" in md
