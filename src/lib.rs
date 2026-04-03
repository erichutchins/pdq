pub mod index;
pub mod provider;
pub mod query;

// #[cfg(feature = "pyo3")]
mod py_module;

/// Key format constants for FST index entries
///
/// These constants define the format used for keys stored in FST indices.
/// Keys have the structure: `value\x00rgN` where:
/// - `value` is the indexed string value
/// - `\x00` is the separator
/// - `rg` is the row group prefix
/// - `N` is the row group index number
pub mod key_format {
    /// Separator between the indexed value and row group metadata
    pub const VALUE_RG_SEPARATOR: char = '\x00';

    /// Character used as upper bound marker in range queries
    /// Used with `lt()` to create inclusive upper bounds
    pub const RANGE_UPPER_BOUND_MARKER: char = '\x01';

    /// Prefix for row group identifiers in index keys
    pub const ROW_GROUP_PREFIX: &str = "rg";

    /// Maximum byte value used for prefix search upper bound
    /// When searching for values starting with a prefix, we use char::from(255)
    /// to create the exclusive upper bound
    pub const PREFIX_SEARCH_UPPER_BOUND: char = '\u{00FF}';

    /// Maximum valid Unicode code point
    /// Used as fallback when incrementing a character would exceed valid Unicode range.
    /// This is the theoretical maximum Unicode scalar value (U+10FFFF).
    pub const MAX_UNICODE_CHAR: char = '\u{10FFFF}';
}

pub use provider::{PdqTableProvider, PdqTableProviderBuilder, PdqParquetOpener, PdqFileSource};
pub use query::IndexQueryEngine;

pub fn calculate_file_hash(file_path: &str) -> anyhow::Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    file_path.hash(&mut hasher);
    Ok(format!("{:x}", hasher.finish()))
}
