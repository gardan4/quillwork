//! `XmlDocument`: the lossless XML layer. Nodes are plain `int` handles (core `NodeId`); every
//! call checks the id, so a stale one returns `None` or raises `XmlNodeError`, never crashes.

use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyBytes;
use quillwork_core::xml::{self, Document, NodeId, NodeKind};

use crate::errors::IntoPyResult;

type Node = u32;

fn id(n: Node) -> NodeId {
    NodeId::from_u32(n)
}

fn ids(it: impl Iterator<Item = NodeId>) -> Vec<Node> {
    it.map(NodeId::as_u32).collect()
}

/// XML parser limits. Keyword arguments override the defaults (depth 256, 8M nodes, 16 MiB per
/// text node, 1024 attributes per element, 512 MiB input).
#[pyclass(module = "quillwork._native", frozen, eq)]
#[derive(Clone, PartialEq, Eq)]
pub struct XmlLimits {
    #[pyo3(get)]
    pub max_depth: usize,
    #[pyo3(get)]
    pub max_nodes: usize,
    #[pyo3(get)]
    pub max_text_len: usize,
    #[pyo3(get)]
    pub max_attrs: usize,
    #[pyo3(get)]
    pub max_input_len: usize,
}

impl XmlLimits {
    pub(crate) fn to_core(&self) -> xml::Limits {
        xml::Limits {
            max_depth: self.max_depth,
            max_nodes: self.max_nodes,
            max_text_len: self.max_text_len,
            max_attrs: self.max_attrs,
            max_input_len: self.max_input_len,
        }
    }

    pub(crate) fn core_or_default(limits: Option<&XmlLimits>) -> xml::Limits {
        limits.map(XmlLimits::to_core).unwrap_or_default()
    }
}

#[pymethods]
impl XmlLimits {
    #[new]
    #[pyo3(signature = (*, max_depth=None, max_nodes=None, max_text_len=None, max_attrs=None, max_input_len=None))]
    fn new(
        max_depth: Option<usize>,
        max_nodes: Option<usize>,
        max_text_len: Option<usize>,
        max_attrs: Option<usize>,
        max_input_len: Option<usize>,
    ) -> Self {
        let d = xml::Limits::default();
        XmlLimits {
            max_depth: max_depth.unwrap_or(d.max_depth),
            max_nodes: max_nodes.unwrap_or(d.max_nodes),
            max_text_len: max_text_len.unwrap_or(d.max_text_len),
            max_attrs: max_attrs.unwrap_or(d.max_attrs),
            max_input_len: max_input_len.unwrap_or(d.max_input_len),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "XmlLimits(max_depth={}, max_nodes={}, max_text_len={}, max_attrs={}, max_input_len={})",
            self.max_depth, self.max_nodes, self.max_text_len, self.max_attrs, self.max_input_len
        )
    }
}

fn kind_name(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Document => "document",
        NodeKind::Element => "element",
        NodeKind::Text => "text",
        NodeKind::CData => "cdata",
        NodeKind::Comment => "comment",
        NodeKind::ProcessingInstruction => "processing_instruction",
        NodeKind::Declaration => "declaration",
    }
}

/// A parsed XML part. Untouched nodes serialise from their source bytes, so `to_bytes()` of an
/// unedited document equals the input.
#[pyclass(module = "quillwork._native")]
pub struct XmlDocument {
    inner: Document,
}

impl XmlDocument {
    pub(crate) fn from_core(inner: Document) -> Self {
        XmlDocument { inner }
    }

    pub(crate) fn core(&self) -> &Document {
        &self.inner
    }
}

#[pymethods]
impl XmlDocument {
    /// Parse UTF-8 XML (GIL released). Raises `XmlParseError` (with `.offset`), `XmlDoctypeError`
    /// or `LimitError`.
    #[staticmethod]
    #[pyo3(signature = (data, limits=None))]
    fn parse(py: Python<'_>, data: PyBackedBytes, limits: Option<PyRef<'_, XmlLimits>>) -> PyResult<XmlDocument> {
        let limits = XmlLimits::core_or_default(limits.as_deref());
        let inner = py.detach(move || Document::parse_with(data.to_vec(), &limits)).py()?;
        Ok(XmlDocument { inner })
    }

    /// Serialise (GIL released). Equal to the input when nothing was edited.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let inner = &self.inner;
        let out = py.detach(|| inner.to_bytes());
        PyBytes::new(py, &out)
    }

    /// Serialise one node and its subtree.
    fn node_to_bytes<'py>(&self, py: Python<'py>, node: Node) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.node_to_bytes(id(node)))
    }

    #[getter]
    fn is_modified(&self) -> bool {
        self.inner.is_modified()
    }

    #[getter]
    fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// The root element.
    #[getter]
    fn root(&self) -> Node {
        self.inner.root().as_u32()
    }

    /// The document node (parent of the declaration, prolog and root element).
    #[getter]
    fn document_node(&self) -> Node {
        self.inner.document_node().as_u32()
    }

    // ---- node info ----

    /// `"element"`, `"text"`, `"cdata"`, `"comment"`, `"processing_instruction"`,
    /// `"declaration"` or `"document"`; `None` for an unknown id.
    fn kind(&self, node: Node) -> Option<&'static str> {
        self.inner.kind(id(node)).map(kind_name)
    }

    /// Qualified name as written (`"w:t"`).
    fn name(&self, node: Node) -> Option<&str> {
        self.inner.name(id(node))
    }

    fn local_name(&self, node: Node) -> Option<&str> {
        self.inner.local_name(id(node))
    }

    fn prefix(&self, node: Node) -> Option<&str> {
        self.inner.prefix(id(node))
    }

    /// Namespace URI of an element.
    fn namespace(&self, node: Node) -> Option<&str> {
        self.inner.namespace(id(node))
    }

    /// True if `node` is the element `{ns}local` (core `Document::is`).
    fn matches(&self, node: Node, ns: &str, local: &str) -> bool {
        self.inner.is(id(node), ns, local)
    }

    /// The namespace URI `prefix` maps to in scope at `node` (`""` for the default namespace).
    fn lookup_namespace(&self, node: Node, prefix: &str) -> Option<&str> {
        self.inner.lookup_namespace(id(node), prefix)
    }

    // ---- navigation ----

    fn parent(&self, node: Node) -> Option<Node> {
        self.inner.parent(id(node)).map(NodeId::as_u32)
    }

    fn first_child(&self, node: Node) -> Option<Node> {
        self.inner.first_child(id(node)).map(NodeId::as_u32)
    }

    fn last_child(&self, node: Node) -> Option<Node> {
        self.inner.last_child(id(node)).map(NodeId::as_u32)
    }

    fn next_sibling(&self, node: Node) -> Option<Node> {
        self.inner.next_sibling(id(node)).map(NodeId::as_u32)
    }

    fn prev_sibling(&self, node: Node) -> Option<Node> {
        self.inner.prev_sibling(id(node)).map(NodeId::as_u32)
    }

    fn children(&self, node: Node) -> Vec<Node> {
        ids(self.inner.children(id(node)))
    }

    fn child_elements(&self, node: Node) -> Vec<Node> {
        ids(self.inner.child_elements(id(node)))
    }

    /// `node` and everything below it, in document order.
    fn descendants(&self, node: Node) -> Vec<Node> {
        ids(self.inner.descendants(id(node)))
    }

    /// Parent, grandparent, ... up to the document node.
    fn ancestors(&self, node: Node) -> Vec<Node> {
        ids(self.inner.ancestors(id(node)))
    }

    /// First child element `{ns}local` of `node`.
    fn find_child(&self, node: Node, ns: &str, local: &str) -> Option<Node> {
        self.inner.find_child(id(node), ns, local).map(NodeId::as_u32)
    }

    /// First element `{ns}local` at or below `node`, in document order.
    fn find(&self, node: Node, ns: &str, local: &str) -> Option<Node> {
        self.inner.descendants_named(id(node), ns, local).next().map(NodeId::as_u32)
    }

    /// Every element `{ns}local` at or below `node`, in document order.
    fn find_all(&self, node: Node, ns: &str, local: &str) -> Vec<Node> {
        ids(self.inner.descendants_named(id(node), ns, local))
    }

    // ---- attributes ----

    /// Attribute by qualified name as written (`"w:val"`), unescaped.
    fn attr(&self, node: Node, qname: &str) -> Option<String> {
        self.inner.attr(id(node), qname).map(|c| c.into_owned())
    }

    /// Attribute by namespace URI and local name.
    fn attr_ns(&self, node: Node, ns: &str, local: &str) -> Option<String> {
        self.inner.attr_ns(id(node), ns, local).map(|c| c.into_owned())
    }

    /// All attributes (including `xmlns` declarations), in source order.
    fn attrs(&self, node: Node) -> Vec<(String, String)> {
        self.inner.attrs(id(node)).map(|(k, v)| (k.to_owned(), v.into_owned())).collect()
    }

    fn set_attr(&mut self, node: Node, qname: &str, value: &str) -> PyResult<()> {
        self.inner.set_attr(id(node), qname, value).py()
    }

    fn set_attr_ns(&mut self, node: Node, ns: &str, local: &str, value: &str) -> PyResult<()> {
        self.inner.set_attr_ns(id(node), ns, local, value).py()
    }

    /// Remove an attribute; returns whether it existed.
    fn remove_attr(&mut self, node: Node, qname: &str) -> PyResult<bool> {
        self.inner.remove_attr(id(node), qname).py()
    }

    fn set_name(&mut self, node: Node, qname: &str) -> PyResult<()> {
        self.inner.set_name(id(node), qname).py()
    }

    // ---- text ----

    /// Value of a leaf node (unescaped text, CDATA, comment, PI body); `None` for elements --
    /// use `text_content` for an element's text.
    fn text(&self, node: Node) -> Option<String> {
        self.inner.text(id(node)).map(|c| c.into_owned())
    }

    /// Concatenated text of every text node below `node`.
    fn text_content(&self, node: Node) -> String {
        self.inner.text_content(id(node))
    }

    /// Set the value of a text node, CDATA section or comment. On an element, replace all its
    /// children with one text node. The value is escaped on output; an unchanged value is a no-op.
    fn set_text(&mut self, node: Node, text: &str) -> PyResult<()> {
        self.inner.set_text(id(node), text).py()
    }

    /// Byte range of an unedited node in the source, if it still has one.
    fn source_span(&self, node: Node) -> Option<(usize, usize)> {
        self.inner.source_span(id(node)).map(|r| (r.start, r.end))
    }

    // ---- structure ----

    /// A new detached element (`"w:p"`); its prefix must be in scope where it is inserted.
    fn create_element(&mut self, qname: &str) -> PyResult<Node> {
        Ok(self.inner.create_element(qname).py()?.as_u32())
    }

    fn create_text(&mut self, text: &str) -> PyResult<Node> {
        Ok(self.inner.create_text(text).py()?.as_u32())
    }

    fn create_comment(&mut self, text: &str) -> PyResult<Node> {
        Ok(self.inner.create_comment(text).py()?.as_u32())
    }

    /// A detached deep copy of `node`.
    fn clone_subtree(&mut self, node: Node) -> PyResult<Node> {
        Ok(self.inner.clone_subtree(id(node)).py()?.as_u32())
    }

    /// Make `child` the last child of `parent`. An attached `child` is moved, not copied
    /// (use `clone_subtree` to copy).
    fn append_child(&mut self, parent: Node, child: Node) -> PyResult<()> {
        self.inner.append_child(id(parent), id(child)).py()
    }

    /// Make `child` the first child of `parent`. An attached `child` is moved, not copied.
    fn prepend_child(&mut self, parent: Node, child: Node) -> PyResult<()> {
        self.inner.prepend_child(id(parent), id(child)).py()
    }

    /// Insert `new` right before `reference`. An attached `new` is moved, not copied.
    fn insert_before(&mut self, reference: Node, new: Node) -> PyResult<()> {
        self.inner.insert_before(id(reference), id(new)).py()
    }

    /// Insert `new` right after `reference`. An attached `new` is moved, not copied.
    fn insert_after(&mut self, reference: Node, new: Node) -> PyResult<()> {
        self.inner.insert_after(id(reference), id(new)).py()
    }

    /// Unlink `node` (it can be re-inserted).
    fn detach(&mut self, node: Node) -> PyResult<()> {
        self.inner.detach(id(node)).py()
    }

    /// Unlink `node` for good.
    fn remove(&mut self, node: Node) -> PyResult<()> {
        self.inner.remove(id(node)).py()
    }

    fn __repr__(&self) -> String {
        format!(
            "<quillwork.XmlDocument root={:?} nodes={} modified={}>",
            self.inner.name(self.inner.root()).unwrap_or(""),
            self.inner.node_count(),
            self.inner.is_modified()
        )
    }
}

/// The well-known (prefix, namespace URI) pairs from core `xml::ns::CONVENTIONAL`; the Python
/// constants in `quillwork.ns` are checked against this in the tests.
#[pyfunction]
pub fn conventional_namespaces() -> Vec<(&'static str, &'static str)> {
    xml::ns::CONVENTIONAL.to_vec()
}
