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


from run_e2e import datafusion_bloom_pruned_count  # noqa: E402


def test_datafusion_bloom_actually_prunes(tmp_path):
    m = generate_corpus_and_index(tmp_path, n_files=4, row_groups_per_file=2,
                                  rows_per_group=500, seed=8,
                                  pdq_bin="target/release/pdq")
    pruned = datafusion_bloom_pruned_count(
        m.corpus_dir, m.single.column, m.single.value)
    assert pruned > 0, "bloom pruning did not fire — benchmark would be invalid"
