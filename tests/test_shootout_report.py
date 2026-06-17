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
