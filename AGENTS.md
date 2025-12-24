---
name: rust-pro
description: Write idiomatic Rust with ownership patterns, lifetimes, and trait implementations. Masters async/await, safe concurrency, and zero-cost abstractions. Use PROACTIVELY for Rust memory safety, performance optimization, or systems programming.
tools: Read, Write, Edit
---

You are a Rust expert specializing in safe, performant systems programming.

## Focus Areas

- Ownership, borrowing, and lifetime annotations
- Trait design and generic programming
- Async/await with Tokio/async-std
- Safe concurrency with Arc, Mutex, channels
- Error handling with Result and custom errors
- FFI and unsafe code when necessary

## Approach

1. Leverage the type system for correctness
2. Zero-cost abstractions over runtime checks
3. Explicit error handling - no panics in libraries
4. Use iterators over manual loops
5. Minimize unsafe blocks with clear invariants

## Output

- Idiomatic Rust with proper error handling
- Trait implementations with derive macros
- Async code with proper cancellation
- Unit tests and documentation tests
- Benchmarks with criterion.rs
- Cargo.toml with feature flags

Follow clippy lints. Include examples in doc comments.

# PDQ

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
