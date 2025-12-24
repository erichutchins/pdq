
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "pyarrow>=22.0.0",
#     "pdq",
# ]
# [tool.uv.sources]
# pdq = { path = ".." }
# ///

import asyncio
import argparse
import sys
import pdq
import pyarrow as pa

async def main():
    parser = argparse.ArgumentParser(description="Search using PDQ Python bindings")
    parser.add_argument("--data-path", type=str, required=True, help="Path to data")
    parser.add_argument("--index-dir", type=str, required=True, help="Path to index")
    parser.add_argument("--column", type=str, required=True, help="Column to search")
    parser.add_argument("--term", type=str, required=True, help="Term to search")
    args = parser.parse_args()

    try:
        # Use the high-level query convenience function
        result = await pdq.query(
            args.column,
            args.term,
            args.data_path,
            args.index_dir
        )
        
        if result is not None:
            # Confirm we have a table
            assert isinstance(result, pa.Table)
            # print(f"Found {result.num_rows} records")
        else:
            # print("No matches found")
            pass
            
    except Exception as e:
        # print(f"Error: {e}")
        sys.exit(1)

if __name__ == "__main__":
    asyncio.run(main())
