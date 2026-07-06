#!/usr/bin/env bash
# One-shot EC2 bootstrap for the PDQ vs. Parquet-bloom shootout (full ladder).
#
# Target: i4i.xlarge (or any Nitro instance with a local NVMe instance store),
# Ubuntu 24.04 LTS. Everything heavy (toolchains, caches, the repo, and the
# ~110 GB corpus) is kept on the instance-store NVMe so the small root EBS
# volume is untouched. The instance store is EPHEMERAL — copy results off the
# box before you terminate it.
#
# Source can arrive two ways (auto-detected in step 5):
#   - scp delivery: a source tarball at ~/pdq-src.tar.gz (preferred here)
#   - git delivery: clone $PDQ_REPO @ $PDQ_BRANCH
#
# Run it inside tmux so an SSH drop doesn't kill the ~2 h run:
#   tmux new -s shootout 'bash ~/ec2_bootstrap.sh 2>&1 | tee ~/shootout.log'
#
# Override defaults via env: PDQ_BRANCH=, LADDER=10,100,1000, PDQ_SRC_TARBALL=
set -euo pipefail

REPO="${PDQ_REPO:-https://github.com/erichutchins/pdq.git}"
BRANCH="${PDQ_BRANCH:-dev/init}"
LADDER="${LADDER:-10,100,1000}"
DATA="/data"

echo "==> [1/8] Mount NVMe instance store at $DATA"
if mountpoint -q "$DATA"; then
  echo "    $DATA already mounted; reusing"
else
  # Identify the AWS instance-store NVMe by model. We deliberately do NOT fall
  # back to size/heuristics — never auto-format a disk we cannot positively ID.
  DEV="$(lsblk -dn -o NAME,MODEL | grep -i 'Instance Storage' | awk '{print "/dev/"$1; exit}')"
  if [ -z "${DEV:-}" ]; then
    echo "ERROR: could not identify the instance-store NVMe by model. Devices:" >&2
    lsblk -o NAME,SIZE,TYPE,MOUNTPOINT,MODEL >&2
    echo "Mount it manually, then re-run:  sudo mkfs.ext4 -F /dev/nvmeXn1 && sudo mount /dev/nvmeXn1 $DATA" >&2
    exit 1
  fi
  if findmnt -S "$DEV" >/dev/null 2>&1; then
    echo "ERROR: $DEV is already mounted; refusing to format." >&2; exit 1
  fi
  echo "    formatting $DEV (ext4) and mounting at $DATA"
  sudo mkfs.ext4 -F "$DEV"
  sudo mkdir -p "$DATA"
  sudo mount "$DEV" "$DATA"
  sudo chown "$(id -u):$(id -g)" "$DATA"
fi
df -h "$DATA"

# Keep toolchains, package caches, repo, and corpora on the big NVMe.
export CARGO_HOME="$DATA/.cargo"
export RUSTUP_HOME="$DATA/.rustup"
export UV_CACHE_DIR="$DATA/.cache/uv"
mkdir -p "$CARGO_HOME" "$RUSTUP_HOME" "$UV_CACHE_DIR"

echo "==> [2/8] Install OS build deps"
export DEBIAN_FRONTEND=noninteractive
sudo apt-get update -y
# python3-dev provides libpython3.12.so: a plain `cargo build` links the PyO3
# cdylib against libpython directly (no extension-module feature), so the dev
# lib must be present or the final link fails with `unable to find -lpython3.X`.
sudo apt-get install -y build-essential pkg-config libssl-dev python3-dev git curl tmux

echo "==> [3/8] Install Rust toolchain"
if ! command -v cargo >/dev/null 2>&1; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path
fi
# shellcheck disable=SC1091
. "$CARGO_HOME/env"

echo "==> [4/8] Install uv"
if ! command -v uv >/dev/null 2>&1; then
  curl -fsSL https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"

echo "==> [5/8] Stage source into $DATA/pdq"
SRC_TARBALL="${PDQ_SRC_TARBALL:-$HOME/pdq-src.tar.gz}"
if [ ! -f "$DATA/pdq/Cargo.toml" ]; then
  if [ -f "$SRC_TARBALL" ]; then
    echo "    extracting $SRC_TARBALL (scp delivery)"
    mkdir -p "$DATA/pdq"
    tar xzf "$SRC_TARBALL" -C "$DATA/pdq"
  else
    echo "    cloning $REPO @ $BRANCH (git delivery)"
    git clone --branch "$BRANCH" "$REPO" "$DATA/pdq"
  fi
fi
cd "$DATA/pdq"
# If this is a git checkout, make sure we're on the right branch and current.
if [ -d "$DATA/pdq/.git" ]; then
  git fetch origin "$BRANCH" && git checkout "$BRANCH" && git pull --ff-only origin "$BRANCH"
fi

echo "==> [6/8] Build PDQ (release, --features shootout) + Python module"
cargo build --release --features shootout
# Layer 2 measures PDQ in-process, so build the maturin extension into the
# project venv (and pull in the Layer-2 contestant + plotting deps).
uv sync --group bench
uv run --group bench maturin develop --release

echo "==> [7/8] Generate corpora + FST indexes (ladder=$LADDER)"
# Captures corpus-write + FST-build time into results/build_time.json.
uv run misc/shootout/gen_data.py --root misc/shootout/corpora --ladder "$LADDER"
echo "    corpus on disk:"; du -sh misc/shootout/corpora/* 2>/dev/null || true

echo "==> Layer 1: pruning cost (Rust micro-bench)"
cargo bench --features shootout --bench pruning_cost

echo "==> Layer 2: end-to-end (warm; in-process pdq, project venv)"
uv run --group bench python misc/shootout/run_e2e.py --ladder "$LADDER"

echo "==> Layer 2: end-to-end (cold; per-sample sudo drop_caches)"
bash misc/shootout/run_cold.sh "$LADDER"

echo "==> [8/8] Render report"
uv run misc/shootout/plot.py

TARBALL="$HOME/shootout-results.tar.gz"
tar -czf "$TARBALL" -C "$DATA/pdq/misc/shootout" results
echo
echo "================= DONE ================="
echo "Results dir : $DATA/pdq/misc/shootout/results"
echo "Tarball     : $TARBALL"
echo
echo "----- REPORT.md -----"
cat "$DATA/pdq/misc/shootout/results/REPORT.md" 2>/dev/null || echo "(REPORT.md missing)"
echo
echo "Pull results to your laptop, then TERMINATE the instance (NVMe is wiped on stop/terminate):"
echo "  scp -i <key.pem> ubuntu@<public-ip>:$TARBALL ."
