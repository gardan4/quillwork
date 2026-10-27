//! Raw-span token tree: a mutable XML tree that re-emits untouched content as its original bytes.
//!
//! See `docs/DESIGN.md` §2 ("How the XML layer works") and §3 (Serialise).
//!
//! * The document owns its source text. Every parsed node remembers where it came from:
//!   elements keep their start-tag span and end-tag span separately, every other node
//!   (text, CDATA, comment, PI, declaration) keeps its whole token.
//! * Nodes live in an arena and are addressed by [`NodeId`] (a `u32`), so bindings can hold
//!   plain handles (document + id) instead of borrows.
//! * Dirty state has two levels: a *tag-dirty* element regenerates only its start tag
//!   (untouched attributes keep their raw bytes, quote character and leading whitespace);
//!   a *content-dirty* element walks its children, and clean children are still copied
//!   verbatim. Anything never touched is copied as one slice.
//! * A document with no mutations serialises to exactly its input bytes.
//! * `<!DOCTYPE` is rejected outright; depth, node count, attribute count and text size are
//!   capped ([`Limits`]). Bad input returns an error; it never panics.
//!
//! ```
//! use quillwork_core::xml::{ns, Document};
//!
//! let src = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#;
//! let mut doc = Document::parse(src).unwrap();
//! let t = doc
//!     .descendants(doc.root())
//!     .find(|&n| doc.is(n, ns::W, "t") && doc.text_content(n) == "Hello")
//!     .unwrap();
//! doc.set_text(t, "Hi & bye").unwrap();
//! assert!(doc.to_bytes().ends_with(b"<w:t>Hi &amp; bye</w:t></w:r></w:p></w:body></w:document>"));
//! ```

mod edit;
mod escape;
pub mod ns;
mod parse;
mod write;

use std::borrow::Cow;
use std::collections::HashMap;

use crate::{Error, Result};

pub(crate) const NIL: u32 = u32::MAX;
/// Namespace id of "no namespace".
const NS_NONE: u32 = 0;
/// Namespace id of an element whose prefix is not bound.
const NS_UNBOUND: u32 = u32::MAX;

// Node flags.
/// Start tag must be regenerated (name or attributes changed).
const TAG_DIRTY: u8 = 1;
/// Children (or something below them) changed: walk them instead of copying the span.
const CONTENT_DIRTY: u8 = 2;
/// Name changed: the end tag must be regenerated too.
const NAME_DIRTY: u8 = 4;

/// Handle to a node in a [`Document`]. Ids are stable for the life of the document: removing a
/// node detaches it but never reuses its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    /// The raw index, for bindings.
    pub fn as_u32(self) -> u32 {
        self.0
    }
    /// Rebuild a handle from [`NodeId::as_u32`]. Every API call checks the id, so a stale or
    /// made-up one yields `None` / an error, never a panic.
    pub fn from_u32(raw: u32) -> Self {
        NodeId(raw)
    }
}

/// What a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// The document node: parent of the declaration, prolog/epilog nodes and the root element.
    Document,
    Element,
    Text,
    CData,
    Comment,
    ProcessingInstruction,
    /// The `<?xml ...?>` declaration.
    Declaration,
}

/// Parser limits. Exceeding one returns [`Error::Limit`].
#[derive(Debug, Clone)]
pub struct Limits {
    /// Maximum element nesting depth.
    pub max_depth: usize,
    /// Maximum number of nodes (parsed and created).
    pub max_nodes: usize,
    /// Maximum raw size in bytes of one text node or attribute value.
    pub max_text_len: usize,
    /// Maximum attributes on one element.
    pub max_attrs: usize,
    /// Maximum input size in bytes (hard ceiling: 4 GiB − 1, spans are `u32`).
    pub max_input_len: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 256,
            max_nodes: 8_000_000,
            max_text_len: 16 << 20,
            max_attrs: 1024,
            max_input_len: 512 << 20,
        }
    }
}

/// A string stored either in the source text or in the document's own string table.
#[derive(Debug, Clone, Copy)]
enum Raw {
    Src(u32, u32),
    Own(u32),
}

/// An attribute as parsed: offsets into the source.
/// The raw attribute (with its leading whitespace) is `src[lead .. val_end + 1]`; the quote
/// character is `src[val - 1]`.
#[derive(Debug, Clone, Copy)]
struct SrcAttr {
    lead: u32,
    name: u32,
    name_end: u32,
    val: u32,
    val_end: u32,
}

/// An attribute of an element whose attribute list has been edited.
#[derive(Debug, Clone, Copy)]
struct OwnAttr {
    name: Raw,
    /// Raw (escaped) value.
    value: Raw,
    quote: u8,
    /// Index into `src_attrs` if this attribute came from the source, else `NIL`.
    src: u32,
    /// The value was changed: write the source prefix (`lead`, name, `=`, quote) + new value.
    changed: bool,
}

#[derive(Debug, Clone, Copy)]
enum Attrs {
    /// `src_attrs[first .. first + len]`.
    Src { first: u32, len: u32 },
    /// `own_attrs[idx]`.
    Own(u32),
}

#[derive(Debug, Clone, Copy)]
struct Elem {
    name: Raw,
    ns: u32,
    /// Start tag span, `(NIL, NIL)` for created elements.
    start: (u32, u32),
    /// End tag span, `(NIL, NIL)` when self-closing in the source or created.
    end: (u32, u32),
    attrs: Attrs,
}

#[derive(Debug, Clone, Copy)]
enum Data {
    Document,
    Element(Elem),
    /// Any other node: its complete raw token (`<!--…-->`, `<![CDATA[…]]>`, `<?…?>`, escaped text).
    Leaf(NodeKind, Raw),
}

#[derive(Debug, Clone, Copy)]
struct Node {
    parent: u32,
    first: u32,
    last: u32,
    prev: u32,
    next: u32,
    flags: u8,
    data: Data,
}

impl Node {
    fn new(data: Data) -> Self {
        Node { parent: NIL, first: NIL, last: NIL, prev: NIL, next: NIL, flags: 0, data }
    }
}

/// A parsed XML part. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct Document {
    src: Box<str>,
    /// Length of the byte-order mark at the start of `src` (0 or 3).
    bom: u32,
    nodes: Vec<Node>,
    src_attrs: Vec<SrcAttr>,
    own_attrs: Vec<Vec<OwnAttr>>,
    strings: Vec<String>,
    ns_uris: Vec<Box<str>>,
    ns_map: HashMap<Box<str>, u32>,
    root: u32,
    limits: Limits,
}

/// Iterator over the children of a node.
pub struct Children<'a> {
    doc: &'a Document,
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        if self.next == NIL {
            return None;
        }
        let cur = self.next;
        self.next = self.doc.nodes[cur as usize].next;
        Some(NodeId(cur))
    }
}

/// Pre-order iterator over a node and everything below it.
pub struct Descendants<'a> {
    doc: &'a Document,
    root: u32,
    next: u32,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        if self.next == NIL {
            return None;
        }
        let cur = self.next;
        let nodes = &self.doc.nodes;
        self.next = if nodes[cur as usize].first != NIL {
            nodes[cur as usize].first
        } else {
            let mut n = cur;
            loop {
                if n == self.root {
                    break NIL;
                }
                let node = &nodes[n as usize];
                if node.next != NIL {
                    break node.next;
                }
                n = node.parent;
                if n == NIL {
                    break NIL;
                }
            }
        };
        Some(NodeId(cur))
    }
}

/// Iterator over an element's attributes as (qualified name, value).
pub struct AttrIter<'a> {
    doc: &'a Document,
    node: u32,
    i: usize,
}

impl<'a> Iterator for AttrIter<'a> {
    type Item = (&'a str, Cow<'a, str>);
    fn next(&mut self) -> Option<Self::Item> {
        let r = self.doc.attr_at(NodeId(self.node), self.i);
        self.i += 1;
        r
    }
}

impl Document {
    // ------------------------------------------------------------------ parse / serialise

    /// Parse a part with the default [`Limits`]. The bytes must be UTF-8 (a UTF-8 BOM is kept).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_with(bytes.to_vec(), &Limits::default())
    }

    /// Parse an owned buffer (no copy) with explicit limits.
    pub fn parse_with(bytes: Vec<u8>, limits: &Limits) -> Result<Self> {
        if bytes.len() > limits.max_input_len.min(u32::MAX as usize - 1) {
            return Err(Error::Limit(format!("xml part is {} bytes", bytes.len())));
        }
        if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
            return Err(Error::XmlParse { offset: 0, msg: "UTF-16 parts are not supported".into() });
        }
        let src = String::from_utf8(bytes)
            .map_err(|e| Error::XmlParse { offset: e.utf8_error().valid_up_to(), msg: "invalid UTF-8".into() })?;
        parse::parse(src.into_boxed_str(), limits.clone())
    }

    /// Serialise. Untouched content comes out as its original bytes; a document with no
    /// mutations returns exactly the input.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.src.len() + 64);
        self.write_to(&mut out);
        out
    }

    /// Serialise, appending to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        write::write_document(self, out, false);
    }

    /// Serialise by walking every node (never copying a whole subtree in one go). Gives the
    /// same bytes as [`Document::to_bytes`]; exists to test that every span is right.
    #[doc(hidden)]
    pub fn to_bytes_walked(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.src.len() + 64);
        write::write_document(self, &mut out, true);
        out
    }

    /// Serialise one subtree (as it would appear inside the document).
    pub fn node_to_bytes(&self, node: NodeId) -> Vec<u8> {
        let mut out = Vec::new();
        if self.valid(node) {
            write::write_subtree(self, node.0, &mut out, false);
        }
        out
    }

    /// Has anything been changed since parsing?
    pub fn is_modified(&self) -> bool {
        self.nodes[0].flags & CONTENT_DIRTY != 0
    }

    /// The original bytes this document was parsed from.
    pub fn source(&self) -> &[u8] {
        self.src.as_bytes()
    }

    /// Number of nodes in the arena (including detached ones).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    // ------------------------------------------------------------------ navigation

    /// The document node (parent of the root element and of prolog nodes).
    pub fn document_node(&self) -> NodeId {
        NodeId(0)
    }

    /// The root (document) element.
    pub fn root(&self) -> NodeId {
        NodeId(self.root)
    }

    #[inline]
    fn valid(&self, n: NodeId) -> bool {
        (n.0 as usize) < self.nodes.len()
    }

    #[inline]
    fn node(&self, n: NodeId) -> Option<&Node> {
        self.nodes.get(n.0 as usize)
    }

    #[inline]
    fn link(v: u32) -> Option<NodeId> {
        (v != NIL).then_some(NodeId(v))
    }

    pub fn kind(&self, n: NodeId) -> Option<NodeKind> {
        self.node(n).map(|node| match node.data {
            Data::Document => NodeKind::Document,
            Data::Element(_) => NodeKind::Element,
            Data::Leaf(k, _) => k,
        })
    }

    pub fn is_element_node(&self, n: NodeId) -> bool {
        matches!(self.node(n).map(|x| &x.data), Some(Data::Element(_)))
    }

    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.node(n).and_then(|x| Self::link(x.parent))
    }
    pub fn first_child(&self, n: NodeId) -> Option<NodeId> {
        self.node(n).and_then(|x| Self::link(x.first))
    }
    pub fn last_child(&self, n: NodeId) -> Option<NodeId> {
        self.node(n).and_then(|x| Self::link(x.last))
    }
    pub fn next_sibling(&self, n: NodeId) -> Option<NodeId> {
        self.node(n).and_then(|x| Self::link(x.next))
    }
    pub fn prev_sibling(&self, n: NodeId) -> Option<NodeId> {
        self.node(n).and_then(|x| Self::link(x.prev))
    }

    /// Children of `n`, in order.
    pub fn children(&self, n: NodeId) -> Children<'_> {
        Children { doc: self, next: self.node(n).map_or(NIL, |x| x.first) }
    }

    /// `n` and every node below it, in document order (pre-order).
    pub fn descendants(&self, n: NodeId) -> Descendants<'_> {
        let start = if self.valid(n) { n.0 } else { NIL };
        Descendants { doc: self, root: start, next: start }
    }

    /// Ancestors of `n`, nearest first (not including `n`).
    pub fn ancestors(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.parent(n), move |&p| self.parent(p))
    }

    /// Child elements of `n`.
    pub fn child_elements(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(n).filter(move |&c| self.is_element_node(c))
    }

    /// First child element of `n` named (`ns`, `local`).
    pub fn find_child(&self, n: NodeId, ns: &str, local: &str) -> Option<NodeId> {
        self.children(n).find(|&c| self.is(c, ns, local))
    }

    /// Descendant elements of `n` (including `n`) named (`ns`, `local`), in document order.
    pub fn descendants_named<'a>(
        &'a self,
        n: NodeId,
        ns: &'a str,
        local: &'a str,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.descendants(n).filter(move |&d| self.is(d, ns, local))
    }

    // ------------------------------------------------------------------ names

    #[inline]
    fn raw(&self, r: Raw) -> &str {
        match r {
            Raw::Src(a, b) => &self.src[a as usize..b as usize],
            Raw::Own(i) => &self.strings[i as usize],
        }
    }

    #[inline]
    fn elem(&self, n: NodeId) -> Option<&Elem> {
        match &self.node(n)?.data {
            Data::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Qualified name of an element (`w:p`), or the target of a processing instruction.
    pub fn name(&self, n: NodeId) -> Option<&str> {
        match &self.node(n)?.data {
            Data::Element(e) => Some(self.raw(e.name)),
            Data::Leaf(NodeKind::ProcessingInstruction, r) => {
                let body = &self.raw(*r)[2..];
                Some(body.split(|c: char| c.is_ascii_whitespace() || c == '?').next().unwrap_or(""))
            }
            _ => None,
        }
    }

    /// Local part of an element's name (`p` for `w:p`).
    pub fn local_name(&self, n: NodeId) -> Option<&str> {
        self.elem(n).map(|e| escape::split_qname(self.raw(e.name)).1)
    }

    /// Prefix of an element's name (`""` if none).
    pub fn prefix(&self, n: NodeId) -> Option<&str> {
        self.elem(n).map(|e| escape::split_qname(self.raw(e.name)).0)
    }

    /// Namespace URI of an element, `None` when it has no namespace or its prefix is unbound.
    pub fn namespace(&self, n: NodeId) -> Option<&str> {
        let e = self.elem(n)?;
        if e.ns == NS_NONE || e.ns == NS_UNBOUND { None } else { Some(&self.ns_uris[e.ns as usize]) }
    }

    /// Is `n` an element with this namespace URI and local name? `ns = ""` matches elements in
    /// no namespace.
    pub fn is(&self, n: NodeId, ns: &str, local: &str) -> bool {
        let Some(e) = self.elem(n) else { return false };
        if escape::split_qname(self.raw(e.name)).1 != local {
            return false;
        }
        match e.ns {
            NS_UNBOUND => false,
            NS_NONE => ns.is_empty(),
            id => &*self.ns_uris[id as usize] == ns,
        }
    }

    /// Namespace declarations made on element `n` itself, as (prefix, raw URI); `""` is the
    /// default namespace.
    pub fn namespace_decls(&self, n: NodeId) -> Vec<(&str, Cow<'_, str>)> {
        self.attrs(n).filter_map(|(k, v)| escape::xmlns_prefix(k).map(|p| (p, v))).collect()
    }

    /// The namespace URI `prefix` is bound to at `n` (`""` = default namespace).
    /// `None` if unbound (or the default namespace is undeclared).
    pub fn lookup_namespace(&self, n: NodeId, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(ns::XML);
        }
        let start = if self.is_element_node(n) { Some(n) } else { self.parent(n) };
        let mut cur = start;
        while let Some(e) = cur {
            for (k, v) in self.attrs(e) {
                if escape::xmlns_prefix(k) == Some(prefix) {
                    if v.is_empty() {
                        return None;
                    }
                    return self.ns_map.get_key_value(&*v).map(|(k, _)| &**k);
                }
            }
            cur = self.parent(e);
        }
        None
    }

    /// A prefix bound to `uri` in scope at `n` and not shadowed by a nearer declaration,
    /// nearest declaration first. `Some("")` means `uri` is the default namespace there.
    /// Use it to name new elements: `format!("{prefix}:{local}")`.
    pub fn find_prefix(&self, n: NodeId, uri: &str) -> Option<&str> {
        if uri == ns::XML {
            return Some("xml");
        }
        let mut seen: Vec<&str> = Vec::new();
        let start = if self.is_element_node(n) { Some(n) } else { self.parent(n) };
        let mut cur = start;
        while let Some(e) = cur {
            for (k, v) in self.attrs(e) {
                if let Some(p) = escape::xmlns_prefix(k) {
                    if seen.contains(&p) {
                        continue;
                    }
                    seen.push(p);
                    if v == uri {
                        return Some(p);
                    }
                }
            }
            cur = self.parent(e);
        }
        None
    }

    // ------------------------------------------------------------------ attributes

    /// Number of attributes on an element (including `xmlns` declarations).
    pub fn attr_count(&self, n: NodeId) -> usize {
        match self.elem(n).map(|e| e.attrs) {
            Some(Attrs::Src { len, .. }) => len as usize,
            Some(Attrs::Own(i)) => self.own_attrs[i as usize].len(),
            None => 0,
        }
    }

    /// Raw (escaped) qualified name and raw value of attribute `i`.
    fn attr_raw_at(&self, n: NodeId, i: usize) -> Option<(&str, &str)> {
        match self.elem(n)?.attrs {
            Attrs::Src { first, len } => {
                if i >= len as usize {
                    return None;
                }
                let a = self.src_attrs[first as usize + i];
                Some((&self.src[a.name as usize..a.name_end as usize], &self.src[a.val as usize..a.val_end as usize]))
            }
            Attrs::Own(idx) => {
                let a = self.own_attrs[idx as usize].get(i)?;
                Some((self.raw(a.name), self.raw(a.value)))
            }
        }
    }

    /// Attribute `i` as (qualified name, value).
    pub fn attr_at(&self, n: NodeId, i: usize) -> Option<(&str, Cow<'_, str>)> {
        self.attr_raw_at(n, i).map(|(k, v)| (k, escape::unescape(v, true)))
    }

    /// Attributes of an element in source order, as (qualified name, value).
    pub fn attrs(&self, n: NodeId) -> AttrIter<'_> {
        AttrIter { doc: self, node: n.0, i: 0 }
    }

    /// Attribute value by qualified name as written (`w:val`).
    pub fn attr(&self, n: NodeId, qname: &str) -> Option<Cow<'_, str>> {
        (0..self.attr_count(n))
            .filter_map(|i| self.attr_raw_at(n, i))
            .find(|(k, _)| *k == qname)
            .map(|(_, v)| escape::unescape(v, true))
    }

    /// Attribute value by namespace URI and local name. `ns = ""` matches unprefixed attributes
    /// (which are in no namespace). Namespace declarations are not matched.
    pub fn attr_ns(&self, n: NodeId, ns: &str, local: &str) -> Option<Cow<'_, str>> {
        for i in 0..self.attr_count(n) {
            let (k, v) = self.attr_raw_at(n, i)?;
            let (p, l) = escape::split_qname(k);
            if l != local || escape::xmlns_prefix(k).is_some() {
                continue;
            }
            let matches = if p.is_empty() { ns.is_empty() } else { self.lookup_namespace(n, p) == Some(ns) };
            if matches {
                return Some(escape::unescape(v, true));
            }
        }
        None
    }

    // ------------------------------------------------------------------ text

    /// Value of a leaf node: unescaped text of a text node, content of a CDATA section, comment,
    /// processing instruction (after the target) or declaration. `None` for elements.
    pub fn text(&self, n: NodeId) -> Option<Cow<'_, str>> {
        match self.node(n)?.data {
            Data::Leaf(kind, r) => {
                let raw = self.raw(r);
                Some(match kind {
                    NodeKind::Text => escape::unescape(raw, false),
                    NodeKind::CData => Cow::Borrowed(&raw[9..raw.len() - 3]),
                    NodeKind::Comment => Cow::Borrowed(&raw[4..raw.len() - 3]),
                    NodeKind::ProcessingInstruction | NodeKind::Declaration => {
                        let body = &raw[2..raw.len() - 2];
                        let target_end = body.find(|c: char| c.is_ascii_whitespace()).unwrap_or(body.len());
                        Cow::Borrowed(body[target_end..].trim_start())
                    }
                    _ => Cow::Borrowed(""),
                })
            }
            _ => None,
        }
    }

    /// Concatenated text of all text and CDATA nodes in the subtree (unescaped).
    pub fn text_content(&self, n: NodeId) -> String {
        let mut out = String::new();
        for d in self.descendants(n) {
            if let Data::Leaf(NodeKind::Text | NodeKind::CData, _) = self.nodes[d.0 as usize].data
                && let Some(t) = self.text(d)
            {
                out.push_str(&t);
            }
        }
        out
    }

    /// Source byte range of a parsed node (start of its start tag to end of its end tag);
    /// `None` for created nodes. Offsets do not move when the tree is edited.
    pub fn source_span(&self, n: NodeId) -> Option<std::ops::Range<usize>> {
        match self.node(n)?.data {
            Data::Document => Some(0..self.src.len()),
            Data::Element(e) if e.start.0 != NIL => {
                let end = if e.end.1 != NIL { e.end.1 } else { e.start.1 };
                Some(e.start.0 as usize..end as usize)
            }
            Data::Leaf(_, Raw::Src(a, b)) => Some(a as usize..b as usize),
            _ => None,
        }
    }
}
