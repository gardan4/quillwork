//! `Package`: the OPC (zip) layer. Byte-identical round trip; edits re-write only what changed.

use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyBytes;
use quillwork_core::opc;

use crate::errors::IntoPyResult;
use crate::xml::{XmlDocument, XmlLimits};

/// Package limits, checked at open. Keyword arguments override the defaults
/// (200 MiB total, 100 MiB per part, ratio 100 above 1 MiB, 10 000 entries).
#[pyclass(module = "quillwork._native", frozen, eq)]
#[derive(Clone, PartialEq, Eq)]
pub struct Limits {
    #[pyo3(get)]
    pub max_total_uncompressed: u64,
    #[pyo3(get)]
    pub max_part_size: u64,
    #[pyo3(get)]
    pub max_ratio: u64,
    #[pyo3(get)]
    pub ratio_min_size: u64,
    #[pyo3(get)]
    pub max_entries: usize,
}

impl Limits {
    pub(crate) fn to_core(&self) -> opc::Limits {
        opc::Limits {
            max_total_uncompressed: self.max_total_uncompressed,
            max_part_size: self.max_part_size,
            max_ratio: self.max_ratio,
            ratio_min_size: self.ratio_min_size,
            max_entries: self.max_entries,
        }
    }

    pub(crate) fn core_or_default(limits: Option<&Limits>) -> opc::Limits {
        limits.map(Limits::to_core).unwrap_or_default()
    }
}

#[pymethods]
impl Limits {
    #[new]
    #[pyo3(signature = (*, max_total_uncompressed=None, max_part_size=None, max_ratio=None, ratio_min_size=None, max_entries=None))]
    fn new(
        max_total_uncompressed: Option<u64>,
        max_part_size: Option<u64>,
        max_ratio: Option<u64>,
        ratio_min_size: Option<u64>,
        max_entries: Option<usize>,
    ) -> Self {
        let d = opc::Limits::default();
        Limits {
            max_total_uncompressed: max_total_uncompressed.unwrap_or(d.max_total_uncompressed),
            max_part_size: max_part_size.unwrap_or(d.max_part_size),
            max_ratio: max_ratio.unwrap_or(d.max_ratio),
            ratio_min_size: ratio_min_size.unwrap_or(d.ratio_min_size),
            max_entries: max_entries.unwrap_or(d.max_entries),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Limits(max_total_uncompressed={}, max_part_size={}, max_ratio={}, ratio_min_size={}, max_entries={})",
            self.max_total_uncompressed, self.max_part_size, self.max_ratio, self.ratio_min_size, self.max_entries
        )
    }
}

/// One relationship from a `.rels` part.
#[pyclass(module = "quillwork._native", frozen, eq, hash)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Relationship {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub rel_type: String,
    /// The `Target` attribute as written (relative to the source's folder unless external).
    #[pyo3(get)]
    pub target: String,
    #[pyo3(get)]
    pub external: bool,
}

impl Relationship {
    fn from_core(r: opc::Relationship) -> Self {
        Relationship { id: r.id, rel_type: r.rel_type, target: r.target, external: r.external }
    }

    fn to_core(&self) -> opc::Relationship {
        opc::Relationship {
            id: self.id.clone(),
            rel_type: self.rel_type.clone(),
            target: self.target.clone(),
            external: self.external,
        }
    }
}

#[pymethods]
impl Relationship {
    fn __repr__(&self) -> String {
        format!(
            "Relationship(id={:?}, rel_type={:?}, target={:?}, external={})",
            self.id,
            self.rel_type,
            self.target,
            if self.external { "True" } else { "False" }
        )
    }
}

/// An open .docx (any OPC package). `save()` of an unmodified package returns the input bytes;
/// after edits only changed entries are re-written.
#[pyclass(module = "quillwork._native")]
pub struct Package {
    inner: opc::Package,
}

#[pymethods]
impl Package {
    /// Open a package. Raises `EncryptedError`, `LimitError` or `BadPackageError`.
    #[staticmethod]
    #[pyo3(signature = (data, limits=None))]
    fn open(py: Python<'_>, data: PyBackedBytes, limits: Option<PyRef<'_, Limits>>) -> PyResult<Package> {
        let limits = Limits::core_or_default(limits.as_deref());
        let inner = py.detach(move || opc::Package::open(data.to_vec(), &limits)).py()?;
        Ok(Package { inner })
    }

    /// Part names in archive order, as stored (no leading slash).
    fn names(&self) -> Vec<String> {
        self.inner.names().map(str::to_owned).collect()
    }

    fn __contains__(&self, name: &str) -> bool {
        self.inner.contains(name)
    }

    fn __len__(&self) -> usize {
        self.inner.names().count()
    }

    /// A part's content. Raises `NotFoundError`.
    fn part<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let bytes = py.detach(|| inner.part(name)).py()?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Replace an existing part's content. Raises `NotFoundError`.
    fn set_part(&mut self, py: Python<'_>, name: &str, data: PyBackedBytes) -> PyResult<()> {
        let inner = &mut self.inner;
        py.detach(move || inner.set_part(name, data.to_vec())).py()
    }

    /// Add a new part and register its content type.
    fn add_part(&mut self, py: Python<'_>, name: &str, data: PyBackedBytes, content_type: &str) -> PyResult<()> {
        let inner = &mut self.inner;
        py.detach(move || inner.add_part(name, data.to_vec(), content_type)).py()
    }

    /// Remove a part (its content-type override and relationships to it stay; see gc in core).
    fn remove_part(&mut self, name: &str) -> PyResult<()> {
        self.inner.remove_part(name).py()
    }

    /// Relationships of `source` (`""` for the package-level `_rels/.rels`).
    #[pyo3(signature = (source=""))]
    fn rels_of(&self, source: &str) -> PyResult<Vec<Relationship>> {
        Ok(self.inner.rels_of(source).py()?.into_iter().map(Relationship::from_core).collect())
    }

    /// The part a relationship of `source` points at; `None` for external or out-of-package targets.
    fn resolve(&self, source: &str, rel: PyRef<'_, Relationship>) -> Option<String> {
        self.inner.resolve(source, &rel.to_core())
    }

    /// The content type of a part ([Content_Types].xml override, else extension default).
    fn content_type_of(&self, name: &str) -> Option<String> {
        self.inner.content_type_of(name).map(str::to_owned)
    }

    /// Parse a part as XML (GIL released). Shortcut for `XmlDocument.parse(pkg.part(name))`
    /// without the intermediate Python bytes.
    #[pyo3(signature = (name, limits=None))]
    fn xml(&self, py: Python<'_>, name: &str, limits: Option<PyRef<'_, XmlLimits>>) -> PyResult<XmlDocument> {
        let limits = XmlLimits::core_or_default(limits.as_deref());
        let inner = &self.inner;
        let doc = py
            .detach(move || {
                let bytes = inner.part(name)?.into_owned();
                quillwork_core::xml::Document::parse_with(bytes, &limits)
            })
            .py()?;
        Ok(XmlDocument::from_core(doc))
    }

    /// Serialise `doc` into part `name` (GIL released). Shortcut for
    /// `pkg.set_part(name, doc.to_bytes())`.
    fn set_xml(&mut self, py: Python<'_>, name: &str, doc: PyRef<'_, XmlDocument>) -> PyResult<()> {
        let inner = &mut self.inner;
        let doc = doc.core();
        py.detach(move || inner.set_part(name, doc.to_bytes())).py()
    }

    /// True once any part was set, added or removed.
    #[getter]
    fn is_modified(&self) -> bool {
        self.inner.is_modified()
    }

    /// The package as bytes (GIL released). Unmodified: the input, byte for byte.
    fn save<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let inner = &self.inner;
        let out = py.detach(|| inner.save()).py()?;
        Ok(PyBytes::new(py, &out))
    }

    fn __repr__(&self) -> String {
        format!("<quillwork.Package parts={} modified={}>", self.inner.names().count(), self.inner.is_modified())
    }
}
