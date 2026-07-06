#!/usr/bin/env bash
# Cold-cache Layer-2 run. run_e2e.py --cold drops the OS page cache before EVERY
# timed sample itself (macOS `purge`, Linux drop_caches — both need sudo), so each
# sample is genuinely cold rather than just the first. Runs in the PROJECT venv so
# the in-process `pdq` module (built via `maturin develop`) is importable.
# Usage: misc/shootout/run_cold.sh [ladder]
set -euo pipefail
LADDER="${1:-10,100,1000}"

uv run --group bench python misc/shootout/run_e2e.py \
  --ladder "$LADDER" --cold \
  --out misc/shootout/results/e2e_cold.json
