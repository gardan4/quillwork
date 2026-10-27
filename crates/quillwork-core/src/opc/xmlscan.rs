//! A minimal scanner for the two small package XML formats (`[Content_Types].xml` and `.rels`):
//! reads the root's direct children with their attributes and byte spans, and splices new
//! children in before the root's closing tag so everything else stays byte-identical.

use std::ops::Range;

use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};

use crate::{Error, Result};

fn xerr(msg: impl Into<String>) -> Error {
    Error::Xml(msg.into())
}

#[derive(Debug, Clone)]
pub(crate) struct Child {
    /// Local name (prefix stripped).
    pub local: String,
    /// Attributes by local name, values unescaped.
    pub attrs: Vec<(String, String)>,
    /// Byte span of the whole element in the scanned bytes.
    pub span: Range<usize>,
}

impl Child {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
enum RootEnd {
    /// Position of the `<` of the closing tag.
    Close(usize),
    /// Span of the `/>` of a self-closing root.
    Empty(Range<usize>),
}

#[derive(Debug, Clone)]
pub(crate) struct Scan {
    pub root_qname: String,
    pub children: Vec<Child>,
    root_end: RootEnd,
}

impl Scan {
    /// The prefix (with colon) to use for new children so they land in the root's namespace.
    pub fn child_prefix(&self) -> &str {
        match self.root_qname.find(':') {
            Some(i) => &self.root_qname[..=i],
            None => "",
        }
    }

    /// `xml` with `fragment` inserted as the last child(ren) of the root.
    pub fn insert(&self, xml: &[u8], fragment: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(xml.len() + fragment.len() + self.root_qname.len() + 4);
        match &self.root_end {
            RootEnd::Close(pos) => {
                out.extend_from_slice(&xml[..*pos]);
                out.extend_from_slice(fragment.as_bytes());
                out.extend_from_slice(&xml[*pos..]);
            }
            RootEnd::Empty(r) => {
                out.extend_from_slice(&xml[..r.start]);
                out.push(b'>');
                out.extend_from_slice(fragment.as_bytes());
                out.extend_from_slice(b"</");
                out.extend_from_slice(self.root_qname.as_bytes());
                out.push(b'>');
                out.extend_from_slice(&xml[r.end..]);
            }
        }
        out
    }
}

/// `xml` without the bytes in `span`.
pub(crate) fn remove(xml: &[u8], span: Range<usize>) -> Vec<u8> {
    let mut out = Vec::with_capacity(xml.len());
    out.extend_from_slice(&xml[..span.start]);
    out.extend_from_slice(&xml[span.end..]);
    out
}

fn attrs_of(e: &BytesStart<'_>) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|err| xerr(format!("bad attribute: {err}")))?;
        let v =
            a.normalized_value(XmlVersion::Implicit1_0).map_err(|err| xerr(format!("bad attribute value: {err}")))?;
        out.push((a.key.local_name().into_inner().to_owned(), v.into_owned()));
    }
    Ok(out)
}

/// Scan `xml`, requiring a root element whose local name is `root_local`.
pub(crate) fn scan(xml: &[u8], root_local: &str) -> Result<Scan> {
    if xml.starts_with(&[0xFF, 0xFE]) || xml.starts_with(&[0xFE, 0xFF]) {
        return Err(xerr("UTF-16 package XML is not supported"));
    }
    let base = if xml.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    let text = std::str::from_utf8(&xml[base..]).map_err(|e| xerr(format!("not UTF-8: {e}")))?;
    let mut r = Reader::from_str(text);
    let mut depth = 0usize;
    let mut root_qname: Option<String> = None;
    let mut root_end = None;
    let mut children = Vec::new();
    let mut open_child: Option<Child> = None;
    loop {
        let before = base + r.buffer_position() as usize;
        let ev = r.read_event().map_err(|e| xerr(format!("{e}")))?;
        let after = base + r.buffer_position() as usize;
        match ev {
            Event::DocType(_) => return Err(xerr("DOCTYPE is not allowed")),
            Event::Start(e) | Event::Empty(e) if depth == 0 => {
                if root_qname.is_some() {
                    return Err(xerr("more than one root element"));
                }
                let qn = e.name().into_inner().to_owned();
                if e.local_name().into_inner() != root_local {
                    return Err(xerr(format!("expected root <{root_local}>, found <{qn}>")));
                }
                root_qname = Some(qn);
                if xml[..after].ends_with(b"/>") {
                    root_end = Some(RootEnd::Empty(after - 2..after));
                } else {
                    depth = 1;
                }
            }
            Event::Start(e) => {
                if depth == 1 {
                    open_child = Some(Child {
                        local: e.local_name().into_inner().to_owned(),
                        attrs: attrs_of(&e)?,
                        span: before..before,
                    });
                }
                depth += 1;
            }
            Event::Empty(e) => {
                if depth == 1 {
                    children.push(Child {
                        local: e.local_name().into_inner().to_owned(),
                        attrs: attrs_of(&e)?,
                        span: before..after,
                    });
                }
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    root_end = Some(RootEnd::Close(before));
                } else if depth == 1
                    && let Some(mut c) = open_child.take()
                {
                    c.span.end = after;
                    children.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match (root_qname, root_end) {
        (Some(root_qname), Some(root_end)) => Ok(Scan { root_qname, children, root_end }),
        _ => Err(xerr(format!("no complete <{root_local}> root element"))),
    }
}

/// Escape an attribute value (also tabs and newlines, so they survive normalisation).
pub(crate) fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}
