//! Parsing: quick-xml tokenises, we record spans and build the arena.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::escape::{self, xmlns_prefix};
use super::{Attrs, Data, Document, Elem, Limits, NIL, NS_NONE, NS_UNBOUND, Node, NodeKind, Raw, SrcAttr, ns};
use crate::{Error, Result};

fn perr(offset: usize, msg: impl Into<String>) -> Error {
    Error::XmlParse { offset, msg: msg.into() }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// Scan the attributes of the start tag `src[start..end]` (`<name ... >` or `<name .../>`),
/// pushing them onto `out`. Returns the end offset of the element name.
fn scan_start_tag(
    src: &str,
    start: usize,
    end: usize,
    self_closing: bool,
    limits: &Limits,
    out: &mut Vec<SrcAttr>,
) -> Result<usize> {
    let b = src.as_bytes();
    let close = if self_closing { end - 2 } else { end - 1 };
    let mut i = start + 1;
    while i < close && !is_ws(b[i]) {
        i += 1;
    }
    let name_end = i;
    if name_end == start + 1 {
        return Err(perr(start, "empty element name"));
    }
    escape::check_qname(&src[start + 1..name_end]).map_err(|_| perr(start, "invalid element name"))?;
    let first = out.len();
    loop {
        let lead = i;
        while i < close && is_ws(b[i]) {
            i += 1;
        }
        if i >= close {
            break;
        }
        if i == lead {
            return Err(perr(i, "attributes must be separated by whitespace"));
        }
        let name = i;
        while i < close && !is_ws(b[i]) && b[i] != b'=' {
            i += 1;
        }
        let name_end = i;
        while i < close && is_ws(b[i]) {
            i += 1;
        }
        if i >= close || b[i] != b'=' || name_end == name {
            return Err(perr(name, "malformed attribute"));
        }
        i += 1;
        while i < close && is_ws(b[i]) {
            i += 1;
        }
        if i >= close || (b[i] != b'"' && b[i] != b'\'') {
            return Err(perr(i, "attribute value must be quoted"));
        }
        let q = b[i];
        let val = i + 1;
        let Some(len) = b[val..close].iter().position(|&c| c == q) else {
            return Err(perr(val, "unterminated attribute value"));
        };
        let val_end = val + len;
        i = val_end + 1;
        if val_end - val > limits.max_text_len {
            return Err(Error::Limit(format!("attribute value of {} bytes", val_end - val)));
        }
        let qname = &src[name..name_end];
        escape::check_qname(qname).map_err(|_| perr(name, "invalid attribute name"))?;
        escape::validate_raw_value(&src[val..val_end]).map_err(|(o, m)| perr(val + o, m))?;
        if out.len() - first >= limits.max_attrs {
            return Err(Error::Limit(format!("more than {} attributes on one element", limits.max_attrs)));
        }
        out.push(SrcAttr {
            lead: lead as u32,
            name: name as u32,
            name_end: name_end as u32,
            val: val as u32,
            val_end: val_end as u32,
        });
    }
    // Duplicate attribute names are not well-formed.
    let attrs = &out[first..];
    let name_of = |a: &SrcAttr| &src[a.name as usize..a.name_end as usize];
    if attrs.len() > 1 {
        if attrs.len() <= 16 {
            for (k, a) in attrs.iter().enumerate() {
                if attrs[..k].iter().any(|p| name_of(p) == name_of(a)) {
                    return Err(perr(a.name as usize, "duplicate attribute"));
                }
            }
        } else {
            let mut seen = std::collections::HashSet::with_capacity(attrs.len());
            for a in attrs {
                if !seen.insert(name_of(a)) {
                    return Err(perr(a.name as usize, "duplicate attribute"));
                }
            }
        }
    }
    Ok(name_end)
}

pub(crate) struct NsTable {
    pub uris: Vec<Box<str>>,
    pub map: HashMap<Box<str>, u32>,
}

impl NsTable {
    pub fn new() -> Self {
        let mut t = NsTable { uris: Vec::new(), map: HashMap::new() };
        t.intern(""); // NS_NONE = 0
        t.intern(ns::XML);
        for (_, uri) in ns::CONVENTIONAL {
            t.intern(uri);
        }
        t
    }
    pub fn intern(&mut self, uri: &str) -> u32 {
        if let Some(&id) = self.map.get(uri) {
            return id;
        }
        let id = self.uris.len() as u32;
        self.uris.push(uri.into());
        self.map.insert(uri.into(), id);
        id
    }
}

pub(crate) fn parse(src: Box<str>, limits: Limits) -> Result<Document> {
    let bom = if src.starts_with('\u{FEFF}') { 3usize } else { 0 };
    let body = &src[bom..];
    let mut reader = Reader::from_str(body);
    {
        let cfg = reader.config_mut();
        cfg.trim_text(false);
        cfg.expand_empty_elements = false;
        cfg.check_end_names = true;
        cfg.check_comments = true;
        cfg.allow_dangling_amp = false;
        cfg.allow_unmatched_ends = false;
    }

    // Rough capacity guess: OOXML runs about one node per 25 bytes.
    let mut nodes: Vec<Node> = Vec::with_capacity(body.len() / 25 + 4);
    let mut src_attrs: Vec<SrcAttr> = Vec::with_capacity(body.len() / 40 + 4);
    let mut nst = NsTable::new();
    nodes.push(Node::new(Data::Document));

    let mut stack: Vec<u32> = vec![0];
    // In-scope namespace bindings (prefix, ns id) and, per open element, the scope length to
    // restore at its end tag.
    let mut scope: Vec<(&str, u32)> = Vec::new();
    let mut marks: Vec<usize> = Vec::new();
    let mut root = NIL;
    let mut last_text = NIL;
    let mut pos = bom;

    let append = |nodes: &mut Vec<Node>, parent: u32, mut node: Node| -> Result<u32> {
        if nodes.len() >= limits.max_nodes {
            return Err(Error::Limit(format!("more than {} xml nodes", limits.max_nodes)));
        }
        let id = nodes.len() as u32;
        node.parent = parent;
        let p = &mut nodes[parent as usize];
        node.prev = p.last;
        let prev = p.last;
        p.last = id;
        if prev == NIL {
            p.first = id;
        }
        nodes.push(node);
        if prev != NIL {
            nodes[prev as usize].next = id;
        }
        Ok(id)
    };

    loop {
        let ev = reader.read_event().map_err(|e| perr(bom + reader.buffer_position() as usize, e.to_string()))?;
        let end = bom + reader.buffer_position() as usize;
        let parent = *stack.last().expect("stack never empty");
        let at_top = stack.len() == 1;
        match ev {
            Event::Eof => break,
            Event::Start(_) | Event::Empty(_) => {
                let self_closing = matches!(ev, Event::Empty(_));
                if at_top && root != NIL {
                    return Err(perr(pos, "more than one root element"));
                }
                if stack.len() > limits.max_depth {
                    return Err(Error::Limit(format!("element nesting deeper than {}", limits.max_depth)));
                }
                if end - pos < 3 || src.as_bytes()[pos] != b'<' {
                    return Err(perr(pos, "unexpected tokeniser position"));
                }
                let first = src_attrs.len() as u32;
                let name_end = scan_start_tag(&src, pos, end, self_closing, &limits, &mut src_attrs)?;
                let len = src_attrs.len() as u32 - first;

                // Namespace declarations on this element.
                let mark = scope.len();
                for a in &src_attrs[first as usize..] {
                    let qn = &src[a.name as usize..a.name_end as usize];
                    if let Some(p) = xmlns_prefix(qn) {
                        let uri = escape::unescape(&src[a.val as usize..a.val_end as usize], true);
                        if p == "xml" || p == "xmlns" {
                            continue;
                        }
                        let id = if uri.is_empty() {
                            if !p.is_empty() {
                                return Err(perr(a.name as usize, "a prefix cannot be undeclared"));
                            }
                            NS_NONE
                        } else {
                            nst.intern(&uri)
                        };
                        scope.push((p, id));
                    }
                }
                let qname = &src[pos + 1..name_end];
                let prefix = escape::split_qname(qname).0;
                let ns_id = resolve(&scope, prefix);

                let elem = Elem {
                    name: Raw::Src(pos as u32 + 1, name_end as u32),
                    ns: ns_id,
                    start: (pos as u32, end as u32),
                    end: (NIL, NIL),
                    attrs: Attrs::Src { first, len },
                };
                let id = append(&mut nodes, parent, Node::new(Data::Element(elem)))?;
                if at_top {
                    root = id;
                }
                if self_closing {
                    scope.truncate(mark);
                } else {
                    stack.push(id);
                    marks.push(mark);
                }
                last_text = NIL;
            }
            Event::End(_) => {
                if at_top {
                    return Err(perr(pos, "unmatched end tag"));
                }
                stack.pop();
                if let Data::Element(e) = &mut nodes[parent as usize].data {
                    e.end = (pos as u32, end as u32);
                }
                scope.truncate(marks.pop().unwrap_or(0));
                last_text = NIL;
            }
            Event::Text(_) | Event::GeneralRef(_) => {
                if let Event::GeneralRef(r) = &ev {
                    let name: &str = r;
                    if escape::resolve_ref(name).is_none() {
                        return Err(perr(pos, "unknown or invalid entity reference"));
                    }
                }
                if at_top && (matches!(ev, Event::GeneralRef(_)) || !src.as_bytes()[pos..end].iter().all(|&b| is_ws(b)))
                {
                    return Err(perr(pos, "text outside the root element"));
                }
                // quick-xml splits text at references; merge adjacent pieces into one node.
                if last_text != NIL
                    && let Data::Leaf(NodeKind::Text, Raw::Src(a, b)) = &mut nodes[last_text as usize].data
                    && *b as usize == pos
                {
                    *b = end as u32;
                    if (*b - *a) as usize > limits.max_text_len {
                        return Err(Error::Limit(format!("text node larger than {} bytes", limits.max_text_len)));
                    }
                    pos = end;
                    continue;
                }
                if end - pos > limits.max_text_len {
                    return Err(Error::Limit(format!("text node larger than {} bytes", limits.max_text_len)));
                }
                last_text = append(
                    &mut nodes,
                    parent,
                    Node::new(Data::Leaf(NodeKind::Text, Raw::Src(pos as u32, end as u32))),
                )?;
            }
            Event::CData(_) | Event::Comment(_) | Event::PI(_) | Event::Decl(_) => {
                let kind = match ev {
                    Event::CData(_) => NodeKind::CData,
                    Event::Comment(_) => NodeKind::Comment,
                    Event::PI(_) => NodeKind::ProcessingInstruction,
                    _ => NodeKind::Declaration,
                };
                if kind == NodeKind::Declaration && pos != bom {
                    return Err(perr(pos, "XML declaration must come first"));
                }
                if kind == NodeKind::CData && at_top {
                    return Err(perr(pos, "CDATA outside the root element"));
                }
                if end - pos > limits.max_text_len {
                    return Err(Error::Limit(format!("node larger than {} bytes", limits.max_text_len)));
                }
                let (open, close) = match kind {
                    NodeKind::CData => ("<![CDATA[", "]]>"),
                    NodeKind::Comment => ("<!--", "-->"),
                    _ => ("<?", "?>"),
                };
                let tok = &src[pos..end];
                if tok.len() < open.len() + close.len() || !tok.starts_with(open) || !tok.ends_with(close) {
                    return Err(perr(pos, "unexpected tokeniser position"));
                }
                append(&mut nodes, parent, Node::new(Data::Leaf(kind, Raw::Src(pos as u32, end as u32))))?;
                last_text = NIL;
            }
            Event::DocType(_) => return Err(Error::XmlDoctype { offset: pos }),
        }
        pos = end;
    }

    if stack.len() != 1 {
        return Err(perr(src.len(), "unclosed element at end of input"));
    }
    if root == NIL {
        return Err(perr(src.len(), "no root element"));
    }
    if pos != src.len() {
        return Err(perr(pos, "trailing bytes after the last token"));
    }

    Ok(Document {
        src,
        bom: bom as u32,
        nodes,
        src_attrs,
        own_attrs: Vec::new(),
        strings: Vec::new(),
        ns_uris: nst.uris,
        ns_map: nst.map,
        root,
        limits,
    })
}

/// Resolve `prefix` against a scope stack (nearest binding wins).
pub(crate) fn resolve<S: AsRef<str>>(scope: &[(S, u32)], prefix: &str) -> u32 {
    if prefix == "xml" {
        return 1;
    }
    for (p, id) in scope.iter().rev() {
        if p.as_ref() == prefix {
            return *id;
        }
    }
    if prefix.is_empty() { NS_NONE } else { NS_UNBOUND }
}
