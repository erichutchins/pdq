# Narwhals Usage with PDQ

## What is Narwhals?

Narwhals is a lightweight compatibility layer that allows you to write **dataframe-agnostic code** that works with Pandas, Polars, and other dataframe libraries without modification.

## When Should You Use `format="narwhals"`?

### ❌ Most End Users Should NOT Use Narwhals

If you're writing application code and know which dataframe library you're using, **use that library directly**:

```python
# ✅ Good - Use the library you actually need
df = await pdq.query("user_id", "12345", "./data", format="polars")
result = df.filter(pl.col("amount") > 100)  # Native Polars API

# ✅ Good - Direct Pandas usage
df = await pdq.query("user_id", "12345", "./data", format="pandas")
result = df[df["amount"] > 100]  # Native Pandas API

# ❌ Avoid - Unnecessary layer
nw_frame = await pdq.query("user_id", "12345", "./data", format="narwhals")
result = nw_frame.filter(nw.col("amount") > 100)  # Same code, extra dependency
```

### ✅ Library Authors SHOULD Use Narwhals

If you're writing a **library** that needs to work with **any** dataframe library, use Narwhals:

```python
# Library code that works with both Polars and Pandas
import narwhals as nw
from narwhals.typing import FrameT

async def analyze_users(
    user_ids: list[str],
    data_dir: str,
    df_type: FrameT = None  # Can be Polars or Pandas
) -> FrameT:
    """
    Analyze users - works with any dataframe library!

    Args:
        user_ids: List of user IDs to analyze
        data_dir: Path to data
        df_type: Optional hint for return type

    Returns:
        DataFrame in the same format as input preference
    """
    import pdq

    # Get as Narwhals frame
    results = []
    for uid in user_ids:
        df = await pdq.query("user_id", uid, data_dir, format="narwhals")
        if df is not None:
            # Use Narwhals API - works on both Polars and Pandas
            filtered = df.filter(nw.col("amount") > 100)
            agg = filtered.group_by("category").agg(
                nw.col("amount").sum().alias("total")
            )
            results.append(agg)

    # Combine and return
    combined = nw.concat(results)
    return combined  # Returns Narwhals frame that wraps either Polars or Pandas
```

Users of your library can then use either Polars or Pandas:

```python
# User with Polars
import polars as pl
result = await analyze_users(["user1", "user2"], "./data")
polars_df = result.to_polars()  # Get native Polars DataFrame

# User with Pandas
import pandas as pd
result = await analyze_users(["user1", "user2"], "./data")
pandas_df = result.to_pandas()  # Get native Pandas DataFrame
```

## PDQ's Approach: Direct Conversions

PDQ uses **direct conversions** for efficiency:

```python
# PDQ's _convert_format() function:

if format == "arrow":
    return table  # Zero-copy, native Arrow

elif format == "pandas":
    return table.to_pandas()  # Direct Arrow -> Pandas

elif format == "polars":
    return pl.from_arrow(table)  # Direct Arrow -> Polars

elif format == "narwhals":
    return nw.from_arrow(table)  # Narwhals wrapper (for library authors)
```

This avoids unnecessary conversion steps:
- ❌ Avoid: `Arrow → Narwhals → Polars` (what you'd get with Narwhals as intermediary)
- ✅ Better: `Arrow → Polars` (direct conversion)

## Performance Comparison

```python
import time
import pdq

# Get 1 million row result
# Direct conversion (faster)
start = time.time()
df = await pdq.query("user_id", "12345", "./data", format="polars")
print(f"Direct: {time.time() - start:.3f}s")

# Via Narwhals (slower, adds overhead)
start = time.time()
nw_frame = await pdq.query("user_id", "12345", "./data", format="narwhals")
df = nw_frame.to_polars()
print(f"Via Narwhals: {time.time() - start:.3f}s")
```

## Real-World Example: Framework-Agnostic Library

Here's a complete example of a library that uses PDQ with Narwhals:

```python
# my_analytics_library.py
import narwhals as nw
from narwhals.typing import FrameT
import pdq

async def user_summary(
    user_ids: list[str],
    data_dir: str,
    index_dir: str = "pdq-index"
) -> FrameT:
    """
    Get summary statistics for users.

    Works with both Polars and Pandas - user chooses!
    """
    results = []

    for uid in user_ids:
        # Get as Narwhals frame (works with both backends)
        df = await pdq.query("user_id", uid, data_dir, index_dir, format="narwhals")

        if df is not None:
            # Use Narwhals API (backend-agnostic)
            summary = df.group_by("category").agg([
                nw.col("amount").sum().alias("total_amount"),
                nw.col("amount").mean().alias("avg_amount"),
                nw.len().alias("count")
            ])
            summary = summary.with_columns(
                nw.lit(uid).alias("user_id")
            )
            results.append(summary)

    if not results:
        return None

    # Concatenate all results
    return nw.concat(results)


# Users can use either Polars or Pandas:

# User 1: Prefers Polars
import polars as pl
summary = await user_summary(["user1", "user2"], "./data")
polars_df = summary.to_polars()
print(polars_df)

# User 2: Prefers Pandas
import pandas as pd
summary = await user_summary(["user1", "user2"], "./data")
pandas_df = summary.to_pandas()
print(pandas_df)
```

## Summary

| Use Case | Recommended Format | Why |
|----------|-------------------|-----|
| Application code with Polars | `format="polars"` | Direct, native API, faster |
| Application code with Pandas | `format="pandas"` | Direct, native API, faster |
| Need Arrow for other tools | `format="arrow"` | Zero-copy, interoperability |
| Writing a library for others | `format="narwhals"` | Framework-agnostic, flexible |

## Key Takeaway

**For PDQ users**: Use `"arrow"`, `"pandas"`, or `"polars"` directly.

**For library authors**: Use `"narwhals"` to make your library work with any dataframe backend.

PDQ supports Narwhals for the latter use case, but most users should use direct formats for better performance.
