#!/usr/bin/env python3
"""
PDQ Basic Usage Example

This example demonstrates how to use PDQ to:
1. Build an index for Parquet files
2. Search for files containing specific values
3. Query data using the index
4. Work with the results using both PyArrow and Polars
"""

import os
import pyarrow as pa
import pyarrow.parquet as pq
import numpy as np
import pdq
import polars as pl
from pathlib import Path


def create_sample_data(data_dir):
    """Create sample Parquet files for demonstration."""
    os.makedirs(data_dir, exist_ok=True)

    # Create sample data with customer IDs and transactions
    for file_idx in range(3):
        # Create data with 1000 rows per file
        customer_ids = [
            f"C{i:06d}" for i in range(file_idx * 1000, (file_idx + 1) * 1000)
        ]
        amounts = np.random.uniform(10, 1000, size=1000).round(2)
        dates = [
            f"2023-{np.random.randint(1, 13):02d}-{np.random.randint(1, 29):02d}"
            for _ in range(1000)
        ]
        categories = np.random.choice(
            ["food", "transport", "entertainment", "utilities", "other"], size=1000
        )

        # Create PyArrow table
        table = pa.Table.from_arrays(
            [
                pa.array(customer_ids),
                pa.array(amounts),
                pa.array(dates),
                pa.array(categories),
            ],
            names=["customer_id", "amount", "date", "category"],
        )

        # Write to parquet with row groups of 200 rows each
        pq.write_table(
            table,
            os.path.join(data_dir, f"transactions_{file_idx}.parquet"),
            row_group_size=200,
        )

    print(f"Created 3 sample Parquet files in {data_dir}")


def main():
    # Setup directories
    base_dir = Path("./pdq_example")
    data_dir = base_dir / "data"
    index_dir = base_dir / "index"

    # Create sample data
    create_sample_data(data_dir)

    # 1. Build an index for the customer_id column
    print("\n1. Building index for customer_id column...")
    pdq.build_index(
        data_dir=str(data_dir), column="customer_id", index_dir=str(index_dir)
    )
    print("Index built successfully!")

    # 2. Search for files containing a specific customer
    print("\n2. Searching for files with customer C000050...")
    results = pdq.search_files(
        column="customer_id", term="C000050", index_dir=str(index_dir)
    )

    print(f"Found {len(results)} matches:")
    for match in results:
        print(f"  File: {match.file_path}, Row Group: {match.row_group}")

    # 3. Query data for a specific customer
    print("\n3. Querying data for customer C000050...")
    table = pdq.query(
        column="customer_id",
        term="C000050",
        data_dir=str(data_dir),
        index_dir=str(index_dir),
    )

    # Convert to pandas and display
    if table:
        df = table.to_pandas()
        print("\nResults as Pandas DataFrame:")
        print(df)
        print(f"Retrieved {len(df)} rows")
    else:
        print("No results found")

    # 4. Query and convert to Polars LazyFrame
    print("\n4. Querying with Polars integration...")
    lazy_frame = pdq.query(
        column="customer_id",
        term="C000100",
        data_dir=str(data_dir),
        index_dir=str(index_dir),
        to_polars=True,
    )

    if lazy_frame:
        # Apply some Polars operations
        result = (
            lazy_frame.filter(pl.col("amount") > 100)
            .select(["customer_id", "amount", "category"])
            .sort("amount", descending=True)
            .collect()
        )

        print("\nResults filtered with Polars:")
        print(result)
        print(f"Retrieved {len(result)} rows after filtering")
    else:
        print("No results found")

    # 5. Using the classes directly for more control
    print("\n5. Using the PDQ classes directly...")

    # Create searcher
    searcher = pdq.Searcher(str(index_dir))
    # Search for customers with IDs starting with "C0002"
    prefix_results = searcher.prefix_search("customer_id", "C0002")

    print(f"Found {len(prefix_results)} matches for prefix 'C0002'")

    # Create query engine
    engine = pdq.QueryEngine(str(index_dir), str(data_dir))

    # Execute a SQL query
    print("\nExecuting SQL query...")
    sql_result = engine.sql_query(
        "SELECT customer_id, AVG(amount) as avg_amount, COUNT(*) as transaction_count "
        "FROM pdq_data "
        "WHERE customer_id LIKE 'C0001%' "
        "GROUP BY customer_id "
        "HAVING COUNT(*) = 1"
    )

    if sql_result:
        print("\nSQL Query Results:")
        print(sql_result.to_pandas())
    else:
        print("No SQL results found")


if __name__ == "__main__":
    main()
