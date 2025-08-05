# PDQ Build Guide

This document describes the build and test steps for developing the `fst`-based index prototype and integrating it into Apache DataFusion for **row group level** Parquet filtering.

Use sub-agents as appropriate to minimize context

---

## 🚀 Key Optimization: Row Group Level Pruning

PDQ's critical advantage is **row group precision**:

1. **FST Key Format**: `value\x00rg<row_group_id>`
   - Not just "which files contain the value"
   - But "which specific row groups within each file"

2. **Elimination Granularity**:
   - Traditional: Skip entire files (coarse)
   - PDQ: Skip specific row groups within files (fine-grained)
   - Can eliminate 90%+ of I/O even when files contain target values

3. **Implementation**:
   ```rust
   // Create access plan that scans only relevant row groups
   let mut access_plan = ParquetAccessPlan::new_none(total_row_groups);
   for &row_group_idx in matching_row_groups {
       access_plan.scan(row_group_idx);
   }
   ```
