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


import pyarrow.parquet as pq  # noqa: E402
from gen_data import generate_corpus  # noqa: E402


def test_generate_corpus_plants_needle_and_blooms(tmp_path):
    m = generate_corpus(
        out_dir=tmp_path, n_files=3, row_groups_per_file=2,
        rows_per_group=500, seed=7,
    )
    # The single needle is where the manifest says it is.
    f = pq.ParquetFile(m.single.file)
    rg = f.read_row_group(m.single.row_group, columns=[m.single.column])
    vals = rg.column(0).to_pylist()
    assert m.single.value in vals

    # Bloom presence: pyarrow may or may not expose `has_bloom_filter` depending
    # on version. When it does, assert it; either way the authoritative,
    # functional bloom-presence gate is the Rust `probe_bloom` test in Phase 3,
    # which only passes if blooms were written and are readable.
    meta = pq.ParquetFile(m.single.file).metadata
    rg_meta = meta.row_group(m.single.row_group)
    col_names = [meta.schema.column(i).name for i in range(meta.num_columns)]
    idx = col_names.index(m.single.column)
    has_bloom = getattr(rg_meta.column(idx), "has_bloom_filter", None)
    if has_bloom is not None:
        assert has_bloom is True


from gen_data import generate_corpus_and_index  # noqa: E402


def test_generate_and_index_builds_fst(tmp_path):
    m = generate_corpus_and_index(
        out_dir=tmp_path, n_files=2, row_groups_per_file=2,
        rows_per_group=300, seed=11, pdq_bin="target/release/pdq",
    )
    fst_files = list(Path(m.index_dir).rglob("src_ip.fst"))
    assert len(fst_files) == 2
