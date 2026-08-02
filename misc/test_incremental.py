
# /// script
# requires-python = ">=3.10"
# dependencies = [
#     "pyarrow>=22.0.0",
#     "numpy>=2.4.0",
# ]
# ///

import os
import shutil
from pathlib import Path
import subprocess
import pyarrow as pa
import pyarrow.parquet as pq

def run_command(cmd):
    print(f"Running: {' '.join(cmd)}")
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"Error: {result.stderr}")
    return result

def main():
    test_dir = Path("test_incremental")
    if test_dir.exists():
        shutil.rmtree(test_dir)
    test_dir.mkdir()
    
    data_dir = test_dir / "data"
    index_dir = test_dir / "index"
    data_dir.mkdir()
    
    # 1. Create initial data
    file1 = data_dir / "file1.parquet"
    table1 = pa.Table.from_pydict({"src_ip": ["1.1.1.1", "1.1.1.2"], "val": [1, 2]})
    pq.write_table(table1, file1)
    
    file2 = data_dir / "file2.parquet"
    table2 = pa.Table.from_pydict({"src_ip": ["2.2.2.1", "2.2.2.2"], "val": [3, 4]})
    pq.write_table(table2, file2)
    
    print("\n--- Step 1: Initial Indexing ---")
    run_command(["./target/release/pdq", "index", "--path", str(data_dir), "--column", "src_ip", "--output", str(index_dir)])
    
    # 2. Add a new file
    print("\n--- Step 2: Add File ---")
    file3 = data_dir / "file3.parquet"
    table3 = pa.Table.from_pydict({"src_ip": ["3.3.3.3"], "val": [5]})
    pq.write_table(table3, file3)
    
    # Index again (incremental)
    run_command(["./target/release/pdq", "index", "--path", str(data_dir), "--column", "src_ip", "--output", str(index_dir)])
    
    # Verify file3 is indexed
    res = run_command(["./target/release/pdq", "query", "--column", "src_ip", "--term", "3.3.3.3", "--data-path", str(data_dir), "--index-dir", str(index_dir)])
    assert "Found 1 matching records" in res.stdout
    
    # 3. Modify a file
    print("\n--- Step 3: Modify File ---")
    # Change file 1 to have 1.1.1.3 instead of 1.1.1.2
    table1_mod = pa.Table.from_pydict({"src_ip": ["1.1.1.1", "1.1.1.3"], "val": [1, 99]})
    pq.write_table(table1_mod, file1)
    
    # Index again
    run_command(["./target/release/pdq", "index", "--path", str(data_dir), "--column", "src_ip", "--output", str(index_dir)])
    
    # Verify 1.1.1.3 is found and 1.1.1.2 is NOT found
    res = run_command(["./target/release/pdq", "query", "--column", "src_ip", "--term", "1.1.1.3", "--data-path", str(data_dir), "--index-dir", str(index_dir)])
    assert "Found 1 matching records" in res.stdout
    
    res = run_command(["./target/release/pdq", "query", "--column", "src_ip", "--term", "1.1.1.2", "--data-path", str(data_dir), "--index-dir", str(index_dir)])
    assert "No matches found" in res.stdout or "Found 0 matching records" in res.stdout or "ZERO-MATCH OPTIMIZATION" in res.stdout
    
    # 4. Delete a file
    print("\n--- Step 4: Delete File (and query gracefully) ---")
    os.remove(file2)
    
    # Query for 2.2.2.1 (which is in the index but file is gone)
    res = run_command(["./target/release/pdq", "query", "--column", "src_ip", "--term", "2.2.2.1", "--data-path", str(data_dir), "--index-dir", str(index_dir)])
    print(res.stdout)
    assert "Skipping missing parquet file" in res.stdout
    assert "No matching records found" in res.stdout
    
    # 5. Prune
    print("\n--- Step 5: Prune ---")
    res = run_command(["./target/release/pdq", "index", "--path", str(data_dir), "--column", "src_ip", "--output", str(index_dir), "--prune"])
    assert "Successfully pruned 1 orphan indices" in res.stdout
    
    # Verify search for 2.2.2.1 now triggers zero-match optimization immediately
    res = run_command(["./target/release/pdq", "query", "--column", "src_ip", "--term", "2.2.2.1", "--data-path", str(data_dir), "--index-dir", str(index_dir)])
    assert "ZERO-MATCH OPTIMIZATION TRIGGERED" in res.stdout

    print("\n✅ All incremental indexing tests passed!")

if __name__ == "__main__":
    main()
