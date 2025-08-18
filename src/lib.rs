pub mod index;
pub mod provider;
pub mod query;
pub mod search;

// #[cfg(feature = "pyo3")]
mod py_module;

pub use provider::{PdqTableProvider, PdqTableProviderBuilder};

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub file_path: String,
    pub row_group: usize,
}

pub fn calculate_file_hash(file_path: &str) -> anyhow::Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    file_path.hash(&mut hasher);
    Ok(format!("{:x}", hasher.finish()))
}
