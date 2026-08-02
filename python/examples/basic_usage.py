#!/usr/bin/env python3
"""
PDQ Basic Usage Example

This example demonstrates how to use PDQ to:
1. Build an index for Parquet files
2. Search for files containing specific values
3. Query data using the index
4. Work with the results using both PyArrow and Polars
"""

import asyncio
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


async def main():
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

    # 2. Search for files containing a specific customer (ASYNC)
    print("\n2. Searching for files with customer C000050...")
    results = await pdq.search_files(
        column="customer_id", term="C000050", index_dir=str(index_dir)
    )

    if results is not None and len(results) > 0:
        print(f"Found {len(results)} match(es):")
        # Results are an Arrow table with file_path and row_group columns
        print(results.to_pandas())
    else:
        print("No results found")

    # 3. Query data for a specific customer (ASYNC)
    print("\n3. Querying data for customer C000050...")
    table = await pdq.query(
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

    # 4. Query and convert to Polars DataFrame (ASYNC)
    print("\n4. Querying with Polars integration...")
    polars_df = await pdq.query(
        column="customer_id",
        term="C000100",
        data_dir=str(data_dir),
        index_dir=str(index_dir),
        format="polars",
    )

    if polars_df is not None:
        # Apply some Polars operations
        result = (
            polars_df.filter(pl.col("amount") > 100)
            .select(["customer_id", "amount", "category"])
            .sort("amount", descending=True)
        )

        print("\nResults filtered with Polars:")
        print(result)
        print(f"Retrieved {len(result)} rows after filtering")
    else:
        print("No results found")

    # 5. Using the classes directly for more control (ASYNC)
    print("\n5. Using the PDQ classes directly...")

    # Create searcher
    searcher = pdq.Searcher(str(index_dir))

    # Search for customers with IDs starting with "C0002" (ASYNC)
    prefix_results = await searcher.prefix_search("customer_id", "C0002")

    if prefix_results is not None and len(prefix_results) > 0:
        # Convert to Pandas to show results
        prefix_df = pa.Table.from_batches(prefix_results).to_pandas()
        print(f"Found {len(prefix_df)} match(es) for prefix 'C0002':")
        # Group by file to show summary
        summary = prefix_df.groupby("file_path").size()
        print(summary.head())
    else:
        print("No results found")

    # 6. Create query engine and execute SQL (ASYNC)
    print("\n6. Executing SQL query...")
    engine = pdq.QueryEngine(str(index_dir), str(data_dir))

    # Execute a SQL query
    sql_result = await engine.sql_query(
        "SELECT customer_id, AVG(amount) as avg_amount, COUNT(*) as transaction_count "
        "FROM pdq_data "
        "WHERE customer_id >= 'C00010' AND customer_id < 'C00020' "
        "GROUP BY customer_id "
        "ORDER BY avg_amount DESC "
        "LIMIT 5"
    )

    if sql_result:
        print("\nSQL Query Results:")
        print(sql_result.to_pandas())
    else:
        print("No SQL results found")

    # 7. Demonstrate parallel searches with asyncio.gather
    print("\n7. Parallel searches with asyncio.gather...")

    # Search for multiple customers in parallel
    search_tasks = [
        searcher.exact_search("customer_id", f"C{i:06d}")
        for i in [100, 200, 300, 400, 500]
    ]

    parallel_results = await asyncio.gather(*search_tasks)

    # Count results (each is a list of record batches)
    found_count = sum(1 for r in parallel_results if r is not None and len(r) > 0)
    print(f"Searched 5 customers in parallel, found {found_count} with matches")

    # 8. Using sql_query convenience function with Polars output
    print("\n8. Using convenience function for SQL with Polars...")

    polars_result = await pdq.sql_query(
        sql="SELECT category, COUNT(*) as count, AVG(amount) as avg_amount "
        "FROM pdq_data "
        "GROUP BY category "
        "ORDER BY count DESC",
        data_dir=str(data_dir),
        index_dir=str(index_dir),
        format="polars",
    )

    if polars_result is not None:
        print("\nCategory Statistics:")
        print(polars_result)
    else:
        print("No results found")

    print("\n✅ All examples completed successfully!")


if __name__ == "__main__":
    asyncio.run(main())
