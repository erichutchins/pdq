#!/bin/bash
set -e

uv sync

uv run maturin develop
