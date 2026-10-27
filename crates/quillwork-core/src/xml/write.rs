//! Serialisation: copy clean spans, regenerate only what changed.

use super::{Attrs, CONTENT_DIRTY, Data, Document, Elem, NAME_DIRTY, NIL, TAG_DIRTY};

pub(crate) fn write_document(doc: &Document, out: &mut Vec<u8>, force_walk: bool) {
    if !force_walk && doc.nodes[0].flags & CONTENT_DIRTY == 0 {
        out.extend_from_slice(doc.src.as_bytes());
        return;
    }
    out.extend_from_slice(&doc.src.as_bytes()[..doc.bom as usize]);
    let mut c = doc.nodes[0].first;
    while c != NIL {
        write_subtree(doc, c, out, force_walk);
        c = doc.nodes[c as usize].next;
    }
}

/// Write the subtree rooted at `r`. Iterative, so tree depth never threatens the stack.
pub(crate) fn write_subtree(doc: &Document, r: u32, out: &mut Vec<u8>, force_walk: bool) {
    let nodes = &doc.nodes;
    let mut n = r;
    loop {
        let descend = write_open(doc, n, out, force_walk);
        if descend {
            n = nodes[n as usize].first;
            continue;
        }
        loop {
            if n == r {
                return;
            }
            let node = &nodes[n as usize];
            if node.next != NIL {
                n = node.next;
                break;
            }
            n = node.parent;
            write_close(doc, n, out);
        }
    }
}

/// Write node `n` (an element's start tag, or a whole leaf / clean subtree).
/// Returns true when the caller must visit the children and then call [`write_close`].
fn write_open(doc: &Document, n: u32, out: &mut Vec<u8>, force_walk: bool) -> bool {
    let src = doc.src.as_bytes();
    let node = &doc.nodes[n as usize];
    match &node.data {
        Data::Document => false,
        Data::Leaf(_, raw) => {
            out.extend_from_slice(doc.raw(*raw).as_bytes());
            false
        }
        Data::Element(e) => {
            let from_src = e.start.0 != NIL;
            let clean = node.flags & (TAG_DIRTY | CONTENT_DIRTY) == 0;
            if from_src && clean && !force_walk {
                let end = if e.end.1 != NIL { e.end.1 } else { e.start.1 };
                out.extend_from_slice(&src[e.start.0 as usize..end as usize]);
                return false;
            }
            let has_children = node.first != NIL;
            let src_end_tag = e.end.0 != NIL;
            let open_form = has_children || src_end_tag;
            if from_src && node.flags & TAG_DIRTY == 0 {
                let (a, b) = (e.start.0 as usize, e.start.1 as usize);
                if src_end_tag || !open_form {
                    out.extend_from_slice(&src[a..b]);
                } else {
                    // Self-closing in the source, children now: `<x a="1"/>` → `<x a="1">`.
                    out.extend_from_slice(&src[a..b - 2]);
                    out.push(b'>');
                }
            } else {
                write_start_tag(doc, e, open_form, out);
            }
            if open_form && !has_children {
                write_close(doc, n, out);
                return false;
            }
            open_form
        }
    }
}

fn write_close(doc: &Document, n: u32, out: &mut Vec<u8>) {
    let node = &doc.nodes[n as usize];
    if let Data::Element(e) = &node.data {
        if e.end.0 != NIL && node.flags & NAME_DIRTY == 0 {
            out.extend_from_slice(&doc.src.as_bytes()[e.end.0 as usize..e.end.1 as usize]);
        } else {
            out.extend_from_slice(b"</");
            out.extend_from_slice(doc.raw(e.name).as_bytes());
            out.push(b'>');
        }
    }
}

/// Regenerate a start tag. Untouched source attributes are copied raw (with their leading
/// whitespace and quote character); the source's whitespace before `>` / `/>` is kept.
fn write_start_tag(doc: &Document, e: &Elem, open_form: bool, out: &mut Vec<u8>) {
    let src = doc.src.as_bytes();
    out.push(b'<');
    out.extend_from_slice(doc.raw(e.name).as_bytes());
    match e.attrs {
        Attrs::Src { first, len } => {
            for a in &doc.src_attrs[first as usize..(first + len) as usize] {
                out.extend_from_slice(&src[a.lead as usize..a.val_end as usize + 1]);
            }
        }
        Attrs::Own(idx) => {
            for a in &doc.own_attrs[idx as usize] {
                if a.src != NIL {
                    let s = doc.src_attrs[a.src as usize];
                    if a.changed {
                        // Leading whitespace, name, `=`, opening quote as written; new value.
                        out.extend_from_slice(&src[s.lead as usize..s.val as usize]);
                        out.extend_from_slice(doc.raw(a.value).as_bytes());
                        out.push(a.quote);
                    } else {
                        out.extend_from_slice(&src[s.lead as usize..s.val_end as usize + 1]);
                    }
                } else {
                    out.push(b' ');
                    out.extend_from_slice(doc.raw(a.name).as_bytes());
                    out.push(b'=');
                    out.push(a.quote);
                    out.extend_from_slice(doc.raw(a.value).as_bytes());
                    out.push(a.quote);
                }
            }
        }
    }
    if e.start.0 != NIL {
        // Whitespace the source had before `>` or `/>`.
        let (a, b) = (e.start.0 as usize, e.start.1 as usize);
        let close = if src[b - 2] == b'/' { b - 2 } else { b - 1 };
        let mut t = close;
        while t > a && matches!(src[t - 1], b' ' | b'\t' | b'\r' | b'\n') {
            t -= 1;
        }
        out.extend_from_slice(&src[t..close]);
    }
    if open_form {
        out.push(b'>');
    } else {
        out.extend_from_slice(b"/>");
    }
}
