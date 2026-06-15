import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "misc" / "shootout"))

from manifest import Manifest, Location  # noqa: E402


def test_manifest_round_trip(tmp_path):
    m = Manifest(
        corpus_dir="d", index_dir="i", n_files=2,
        row_groups_per_file=8, rows_per_group=10,
        indexed_columns=["src_ip"], bloom_columns=["src_ip"],
        single=Location(column="src_ip", value="1.2.3.4",
                        file="f.parquet", row_group=1),
        multi=[Location("src_ip", "9.9.9.9", "f.parquet", 0)],
        prefix_term="192.168.133.",
        prefix_locations=[Location("src_ip", "192.168.133.7", "f.parquet", 2)],
        domain_suffix="evil.com", domain_reversed_prefix="com.evil.",
        domain_locations=[Location("domain_rev", "com.evil.www", "f.parquet", 0)],
    )
    p = tmp_path / "manifest.json"
    m.save(p)
    loaded = Manifest.load(p)
    assert loaded == m
    assert json.loads(p.read_text())["single"]["row_group"] == 1
