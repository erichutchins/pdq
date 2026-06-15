"""Shared manifest schema for the PDQ vs. bloom-filter shootout."""
from __future__ import annotations

import json
from dataclasses import dataclass, asdict
from pathlib import Path


@dataclass(frozen=True)
class Location:
    column: str
    value: str
    file: str
    row_group: int


@dataclass
class Manifest:
    corpus_dir: str
    index_dir: str
    n_files: int
    row_groups_per_file: int
    rows_per_group: int
    indexed_columns: list[str]
    bloom_columns: list[str]
    single: Location
    multi: list[Location]
    prefix_term: str
    prefix_locations: list[Location]
    domain_suffix: str
    domain_reversed_prefix: str
    domain_locations: list[Location]

    def save(self, path: str | Path) -> None:
        Path(path).write_text(json.dumps(_to_jsonable(self), indent=2))

    @staticmethod
    def load(path: str | Path) -> "Manifest":
        raw = json.loads(Path(path).read_text())
        return _from_jsonable(raw)


def _to_jsonable(m: Manifest) -> dict:
    d = asdict(m)
    return d


def _loc_list(items: list[dict]) -> list[Location]:
    return [Location(**it) for it in items]


def _from_jsonable(raw: dict) -> Manifest:
    return Manifest(
        corpus_dir=raw["corpus_dir"],
        index_dir=raw["index_dir"],
        n_files=raw["n_files"],
        row_groups_per_file=raw["row_groups_per_file"],
        rows_per_group=raw["rows_per_group"],
        indexed_columns=raw["indexed_columns"],
        bloom_columns=raw["bloom_columns"],
        single=Location(**raw["single"]),
        multi=_loc_list(raw["multi"]),
        prefix_term=raw["prefix_term"],
        prefix_locations=_loc_list(raw["prefix_locations"]),
        domain_suffix=raw["domain_suffix"],
        domain_reversed_prefix=raw["domain_reversed_prefix"],
        domain_locations=_loc_list(raw["domain_locations"]),
    )
