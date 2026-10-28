//! Dedicated PPTX seam. Presentation review is an immutable candidate report, not Word tracking.
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyBytes;
use quillwork_core::{Error, pml};
use serde_json::Value;

use crate::errors::IntoPyResult;
use crate::package::Limits;

#[pyclass(module = "quillwork._native")]
pub struct JsonPresentation {
    inner: pml::Presentation,
}

#[pymethods]
impl JsonPresentation {
    #[staticmethod]
    #[pyo3(signature = (data, limits=None))]
    fn open(py: Python<'_>, data: PyBackedBytes, limits: Option<PyRef<'_, Limits>>) -> PyResult<Self> {
        let limits = limits.map(|l| Limits::core_or_default(Some(&*l)));
        let inner = py
            .detach(move || {
                let bytes = data.to_vec();
                pml::Presentation::open_with_limits(bytes, &limits.unwrap_or_default())
            })
            .py()?;
        Ok(Self { inner })
    }

    #[getter]
    fn version(&self) -> u64 {
        self.inner.version()
    }

    fn read(&self, py: Python<'_>) -> String {
        let inner = &self.inner;
        py.detach(move || inner.read())
    }

    fn describe_json(&self, py: Python<'_>) -> PyResult<String> {
        let inner = &self.inner;
        py.detach(move || inner.describe().map(|v| v.to_string())).py()
    }

    fn review_json(&self, py: Python<'_>) -> PyResult<String> {
        let inner = &self.inner;
        py.detach(move || inner.review().map(|v| v.to_string())).py()
    }

    fn apply_json(&mut self, py: Python<'_>, transaction: &str) -> PyResult<String> {
        let inner = &mut self.inner;
        py.detach(move || {
            let tx: Value = serde_json::from_str(transaction)
                .map_err(|e| Error::Edit { code: "INVALID_OP", message: format!("invalid transaction JSON: {e}") })?;
            inner.apply_tx(&tx).map(|v| v.to_string())
        })
        .py()
    }

    fn save<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let bytes = py.detach(move || inner.save()).py()?;
        Ok(PyBytes::new(py, &bytes))
    }
}
