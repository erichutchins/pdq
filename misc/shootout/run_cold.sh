#!/usr/bin/env bash
# Cold-cache Layer-2 run. macOS: needs `sudo purge`. Linux: drop_caches.
# Usage: misc/shootout/run_cold.sh [ladder]
set -euo pipefail
LADDER="${1:-10,100,1000}"

purge_cache() {
  if command -v purge >/dev/null 2>&1; then
    sync && sudo purge
  elif [ -w /proc/sys/vm/drop_caches ] || sudo -n true 2>/dev/null; then
    sync && echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null
  else
    echo "WARN: cannot purge cache; results are warm" >&2
  fi
}

purge_cache
uv run --with duckdb --with datafusion --with polars --with pyarrow \
  misc/shootout/run_e2e.py --ladder "$LADDER" \
  --out misc/shootout/results/e2e_cold.json
