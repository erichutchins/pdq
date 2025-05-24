use pyo3::prelude::*;

#[pymodule]
fn pdq(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.set_item("placeholder", "Coming soon!")?;
    Ok(())
}
