# Shootout Results
## Layer 1: pruning cost
| n_files | mechanism | workload | median_ms | bytes_read | matched_files |
|---|---|---|---|---|---|
| 10 | bloom | multi | 15.416 | 31534700 | 17 |
| 10 | pdq_fst | multi | 0.041 | 63550799 | 10 |
| 10 | bloom | single | 15.452 | 31534700 | 4 |
| 10 | pdq_fst | single | 0.017 | 63550799 | 1 |
| 100 | bloom | multi | 158.070 | 315346795 | 847 |
| 100 | pdq_fst | multi | 2.563 | 635777026 | 100 |
| 100 | bloom | single | 156.090 | 315346795 | 12 |
| 100 | pdq_fst | single | 0.077 | 635777026 | 1 |
| 1000 | bloom | multi | 1723.559 | 3153467148 | 79855 |
| 1000 | pdq_fst | multi | 263.207 | 6356943791 | 1196 |
| 1000 | bloom | single | 1494.609 | 3153467148 | 72 |
| 1000 | pdq_fst | single | 0.541 | 6356943791 | 1 |

## Layer 2: end-to-end (warm)
| n_files | contestant | rows | correct | median ms | p25–p75 ms |
|---|---|---|---|---|---|
| 10 | datafusion_bloom | 1 | True | 13.22 | 10.41–15.07 |
| 10 | duckdb_bloom | 1 | True | 7.25 | 7.06–7.36 |
| 10 | pdq | 1 | True | 6.53 | 6.16–6.74 |
| 10 | polars_fullscan | 1 | True | 34.17 | 33.62–35.01 |
| 100 | datafusion_bloom | 1 | True | 54.32 | 52.65–57.55 |
| 100 | duckdb_bloom | 1 | True | 28.56 | 28.19–29.10 |
| 100 | pdq | 1 | True | 6.55 | 6.27–6.92 |
| 100 | polars_fullscan | 1 | True | 333.17 | 331.09–334.94 |
| 1000 | datafusion_bloom | 1 | True | 528.27 | 522.23–536.60 |
| 1000 | duckdb_bloom | 1 | True | 227.66 | 225.62–230.03 |
| 1000 | pdq | 1 | True | 7.98 | 7.74–8.34 |
| 1000 | polars_fullscan | 1 | True | 3300.56 | 3291.15–3309.96 |

## Layer 2: end-to-end (cold)
| n_files | contestant | rows | correct | median ms | p25–p75 ms |
|---|---|---|---|---|---|
| 10 | datafusion_bloom | 1 | True | 26.01 | 25.74–26.79 |
| 10 | duckdb_bloom | 1 | True | 16.16 | 15.60–17.25 |
| 10 | pdq | 1 | True | 15.39 | 15.14–15.80 |
| 10 | polars_fullscan | 1 | True | 100.57 | 98.88–103.32 |
| 100 | datafusion_bloom | 1 | True | 265.32 | 175.77–318.68 |
| 100 | duckdb_bloom | 1 | True | 43.33 | 41.72–43.49 |
| 100 | pdq | 1 | True | 14.09 | 13.70–14.99 |
| 100 | polars_fullscan | 1 | True | 2133.16 | 1956.21–2135.17 |
| 1000 | datafusion_bloom | 1 | True | 3934.46 | 3913.61–3947.53 |
| 1000 | duckdb_bloom | 1 | True | 269.89 | 268.49–272.50 |
| 1000 | pdq | 1 | True | 15.48 | 14.83–16.85 |
| 1000 | polars_fullscan | 1 | True | 22220.53 | 21979.99–22221.95 |

## Build cost (corpus write + FST index)
| n_files | corpus write (s) | FST build (s) |
|---|---|---|
| 10 | 8.2 | 50.2 |
| 100 | 81.8 | 514.0 |
| 1000 | 817.7 | 5418.3 |
