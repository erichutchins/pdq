#!/bin/bash
set -euo pipefail

uv python install 3.13

uv sync

uv run maturin develop
