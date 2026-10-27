//! Python binding for Quillwork (PyO3, abi3, CPython 3.12+).
//!
//! The native module is `quillwork._native`; the public package `quillwork` (python/quillwork)
//! re-exports it next to the pure-Python exceptions, namespace constants and the JSON `Document`
//! wrapper. Design notes: docs/BINDING.md.
//!
//! Rules this crate keeps:
//! - Every core `Error` becomes a `quillwork.errors` exception in one place ([`errors::to_py`]).
//! - Open, parse, part inflation, serialisation and save run with the GIL released
//!   (`Python::detach`); bytes are copied once in and once out.
//! - The future op surface crosses the boundary as JSON strings (`Document.apply_json` and
//!   friends): no per-field PyO3 classes.

use pyo3::prelude::*;

mod document;
mod errors;
mod package;
mod xml;

/// `quillwork._native`
#[pymodule]
#[pyo3(name = "_native")]
fn native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("SPEC_VERSION", document::SPEC_VERSION)?;
    m.add_class::<package::Limits>()?;
    m.add_class::<package::Relationship>()?;
    m.add_class::<package::Package>()?;
    m.add_class::<xml::XmlLimits>()?;
    m.add_class::<xml::XmlDocument>()?;
    m.add_class::<document::JsonDocument>()?;
    m.add_function(wrap_pyfunction!(xml::conventional_namespaces, m)?)?;
    Ok(())
}
