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
