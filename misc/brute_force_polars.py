
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "polars>=0.20.4",
# ]
# ///

import polars as pl
import argparse
import sys
from pathlib import Path

def main():
    parser = argparse.ArgumentParser(description="Brute force search using Polars")
    parser.add_argument("--path", type=str, required=True, help="Path to data")
    parser.add_argument("--column", type=str, required=True, help="Column to search")
    parser.add_argument("--term", type=str, required=True, help="Term to search")
    args = parser.parse_args()

    # Search for all parquet files in the hierarchy
    pattern = str(Path(args.path) / "**" / "*.parquet")
    
    try:
        # Scan with Polars (brute force)
        df = pl.scan_parquet(pattern)
        results = df.filter(pl.col(args.column) == args.term).collect()
        
        # We don't want to print everything to avoid bottlenecking on stdout
        # but we should print the count or confirm if found
        count = len(results)
        # print(f"Found {count} records")
        if count == 0:
            sys.exit(0)
    except Exception as e:
        # print(f"Error: {e}")
        sys.exit(1)

if __name__ == "__main__":
    main()
