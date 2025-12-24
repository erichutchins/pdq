# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "numpy>=2.4.0",
#     "pyarrow>=22.0.0",
# ]
# ///

import os
import random
import argparse
from pathlib import Path
import pyarrow as pa
import pyarrow.parquet as pq
import numpy as np
from datetime import datetime, timedelta

def generate_ip():
    return f"{random.randint(1, 255)}.{random.randint(0, 255)}.{random.randint(0, 255)}.{random.randint(1, 255)}"

def create_mock_data(num_rows, seed_ips=None):
    """Create a mock dataset with some potential 'needles'."""
    ips = [generate_ip() for _ in range(num_rows - len(seed_ips or []))]
    if seed_ips:
        ips.extend(seed_ips)
    random.shuffle(ips)
    
    data = {
        "timestamp": [datetime.now() - timedelta(seconds=random.randint(0, 1000000)) for _ in range(num_rows)],
        "src_ip": ips,
        "dst_ip": [generate_ip() for _ in range(num_rows)],
        "bytes": np.random.randint(64, 1500, size=num_rows),
        "status_code": np.random.choice([200, 301, 302, 404, 500, 503], size=num_rows),
        "user_agent": np.random.choice([
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
            "curl/7.68.0",
            "Go-http-client/1.1"
        ], size=num_rows)
    }
    return pa.Table.from_pydict(data)

def fabricate_hierarchy(root_dir, depth=3, breadth=3, rows_per_file=1000, row_group_size=200, seed=42):
    """
    Fabricate a nested hierarchy of Parquet files.
    
    Example Structure (depth=2, breadth=2):
    root/
      d0_0/
        d1_0/
          data.parquet
        d1_1/
          data.parquet
      d0_1/
        ...
    """
    random.seed(seed)
    np.random.seed(seed)
    root = Path(root_dir)
    root.mkdir(parents=True, exist_ok=True)
    
    # We'll place a specific needle in a specific file to test search
    needle_ip = "192.168.133.7"
    needle_placed = False

    def _recursive_create(current_path, current_depth):
        nonlocal needle_placed
        if current_depth == depth:
            # Create a parquet file
            file_path = current_path / "data.parquet"
            seed_ips = []
            # Place the needle once in the entire hierarchy
            if not needle_placed and random.random() < 0.1:
                seed_ips = [needle_ip]
                needle_placed = True
                print(f"Placed needle {needle_ip} in {file_path}")
            
            table = create_mock_data(rows_per_file, seed_ips)
            pq.write_table(table, file_path, row_group_size=row_group_size)
            return

        for i in range(breadth):
            subdir = current_path / f"level_{current_depth}_node_{i}"
            subdir.mkdir(exist_ok=True)
            _recursive_create(subdir, current_depth + 1)

    _recursive_create(root, 0)
    
    # Ensure needle is placed if random failed
    if not needle_placed:
        file_path = root / "last_resort.parquet"
        print(f"Placed needle {needle_ip} in {file_path} (last resort)")
        table = create_mock_data(rows_per_file, [needle_ip])
        pq.write_table(table, file_path, row_group_size=row_group_size)

    print(f"\nSuccessfully generated nested hierarchy at: {root}")
    print(f"Needle IP to search for: {needle_ip}")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Fabricate nested Parquet hierarchy for PDQ testing.")
    parser.add_argument("--out", type=str, default="test_data", help="Output directory")
    parser.add_argument("--depth", type=int, default=2, help="Depth of hierarchy")
    parser.add_argument("--breadth", type=int, default=3, help="Breadth of each level")
    parser.add_argument("--rows", type=int, default=1000, help="Rows per file")
    parser.add_argument("--rg-size", type=int, default=250, help="Row group size")
    parser.add_argument("--seed", type=int, default=42, help="Random seed")
    
    args = parser.parse_args()
    fabricate_hierarchy(args.out, args.depth, args.breadth, args.rows, args.rg_size, args.seed)
