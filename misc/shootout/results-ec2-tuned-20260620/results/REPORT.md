# Shootout Results
## Layer 1: pruning cost
| n_files | mechanism | workload | median_ms | bytes_read | matched_files |
|---|---|---|---|---|---|
| 10 | bloom | multi | 14.321 | 31534700 | 17 |
| 10 | pdq_fst | multi | 0.040 | 63550799 | 10 |
| 10 | bloom | single | 14.157 | 31534700 | 4 |
| 10 | pdq_fst | single | 0.018 | 63550799 | 1 |
| 100 | bloom | multi | 154.640 | 315346795 | 847 |
| 100 | pdq_fst | multi | 2.542 | 635777026 | 100 |
| 100 | bloom | single | 150.107 | 315346795 | 12 |
| 100 | pdq_fst | single | 0.073 | 635777026 | 1 |
| 1000 | bloom | multi | 1673.896 | 3153467148 | 79855 |
| 1000 | pdq_fst | multi | 254.484 | 6356943791 | 1196 |
| 1000 | bloom | single | 1447.332 | 3153467148 | 72 |
| 1000 | pdq_fst | single | 0.513 | 6356943791 | 1 |

## Layer 2: end-to-end (warm)
| n_files | contestant | rows | correct | median ms | p25–p75 ms |
|---|---|---|---|---|---|
| 10 | datafusion_bloom | 1 | True | 11.27 | 10.28–13.32 |
| 10 | duckdb_bloom | 1 | True | 8.41 | 8.20–9.41 |
| 10 | pdq | 1 | True | 5.20 | 4.80–5.50 |
| 10 | polars_fullscan | 1 | True | 66.63 | 66.18–67.36 |
| 100 | datafusion_bloom | 1 | True | 54.82 | 52.34–57.78 |
| 100 | duckdb_bloom | 1 | True | 27.74 | 27.48–28.29 |
| 100 | pdq | 1 | True | 5.80 | 5.64–5.97 |
| 100 | polars_fullscan | 1 | True | 659.47 | 657.71–662.25 |
| 1000 | datafusion_bloom | 1 | True | 505.63 | 501.07–513.07 |
| 1000 | duckdb_bloom | 1 | True | 200.84 | 200.39–202.15 |
| 1000 | pdq | 1 | True | 7.12 | 6.98–7.32 |
| 1000 | polars_fullscan | 1 | True | 18997.64 | 13535.48–28289.91 |

## Layer 2: end-to-end (cold)
| n_files | contestant | rows | correct | median ms | p25–p75 ms |
|---|---|---|---|---|---|
| 10 | datafusion_bloom | 1 | True | 32.39 | 32.07–32.57 |
| 10 | duckdb_bloom | 1 | True | 22.57 | 22.35–24.52 |
| 10 | pdq | 1 | True | 15.62 | 15.28–16.09 |
| 10 | polars_fullscan | 1 | True | 246.95 | 245.64–249.55 |
| 100 | datafusion_bloom | 1 | True | 205.67 | 204.74–314.70 |
| 100 | duckdb_bloom | 1 | True | 48.36 | 47.56–48.85 |
| 100 | pdq | 1 | True | 16.48 | 16.23–16.74 |
| 100 | polars_fullscan | 1 | True | 4939.13 | 4930.77–4953.23 |
| 1000 | datafusion_bloom | 1 | True | 3952.82 | 3928.96–3975.32 |
| 1000 | duckdb_bloom | 1 | True | 272.57 | 270.69–282.71 |
| 1000 | pdq | 1 | True | 16.91 | 16.28–18.81 |
| 1000 | polars_fullscan | 1 | True | 50532.09 | 50432.26–50558.59 |

## Build cost (corpus write + FST index)
| n_files | corpus write (s) | FST build (s) |
|---|---|---|
| 10 | 7.8 | 41.8 |
| 100 | 80.5 | 427.8 |
| 1000 | 827.9 | 5132.0 |
