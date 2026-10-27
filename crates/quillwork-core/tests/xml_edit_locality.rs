//! Edit locality on real `word/document.xml` parts: after one edit, every byte outside the
//! touched element is identical to the source.

mod xml_support;

use quillwork_core::xml::{Document, NodeId, ns};
use xml_support::assert_local;

fn documents() -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    for (i, p) in xml_support::synthetic_docx().iter().enumerate() {
        v.extend(xml_support::part(p, "word/document.xml").map(|b| (format!("synthetic#{i}"), b)));
    }
    for (i, p) in xml_support::private_docx().iter().enumerate() {
        v.extend(xml_support::part(p, "word/document.xml").map(|b| (format!("private#{i}"), b)));
    }
    assert!(!v.is_empty());
    v
}

fn middle<T: Copy>(v: &[T]) -> T {
    v[v.len() / 2]
}

fn reparse(label: &str, out: &[u8]) {
    Document::parse(out).unwrap_or_else(|e| panic!("{label}: output does not re-parse: {e}"));
    let s = std::str::from_utf8(out).unwrap();
    roxmltree::Document::parse(s).unwrap_or_else(|e| panic!("{label}: roxmltree rejects output: {e}"));
}

#[test]
fn change_one_text() {
    for (label, src) in documents() {
        let mut doc = Document::parse(&src).unwrap();
        let root = doc.root();
        let ts: Vec<NodeId> = doc
            .descendants_named(root, ns::W, "t")
            .filter(|&t| doc.first_child(t).is_some_and(|c| doc.next_sibling(c).is_none()))
            .collect();
        let t = middle(&ts);
        let text = doc.first_child(t).unwrap();
        let span = doc.source_span(text).unwrap();
        doc.set_text(t, "changed & <new> \"text\"").unwrap();
        let out = doc.to_bytes();
        assert_local(&src, &out, span.clone());
        let replaced = &out[span.start..out.len() - (src.len() - span.end)];
        assert!(replaced == b"changed &amp; &lt;new&gt; \"text\"", "{label}");
        reparse(&label, &out);
        assert!(Document::parse(&out).unwrap().to_bytes() == out, "{label}");
    }
}

#[test]
fn add_attribute_to_one_paragraph() {
    for (label, src) in documents() {
        let mut doc = Document::parse(&src).unwrap();
        let root = doc.root();
        let ps: Vec<NodeId> =
            doc.descendants_named(root, ns::W, "p").filter(|&p| doc.first_child(p).is_some()).collect();
        let p = middle(&ps);
        // Only the start tag is touched: the region ends where the first child starts.
        let start = doc.source_span(p).unwrap().start;
        let first = doc.source_span(doc.first_child(p).unwrap()).unwrap().start;
        let before = String::from_utf8(src[start..first].to_vec()).unwrap();
        doc.set_attr(p, "w:rsidR", "00C0FFEE").unwrap();
        let out = doc.to_bytes();
        assert_local(&src, &out, start..first);
        let tag = std::str::from_utf8(&out[start..out.len() - (src.len() - first)]).unwrap();
        // The regenerated tag is the old one (raw attributes and all) plus the new attribute,
        // unless w:rsidR already existed, in which case only its value changed in place.
        if before.contains(" w:rsidR=\"") {
            assert_eq!(tag.len(), before.len(), "{label}");
        } else {
            let trimmed = before.trim_end_matches('>').trim_end();
            assert!(tag.starts_with(trimmed), "{label}: untouched attributes changed");
            assert!(tag.ends_with(" w:rsidR=\"00C0FFEE\">"), "{label}");
        }
        reparse(&label, &out);
    }
}

#[test]
fn insert_run_after_existing_run() {
    for (label, src) in documents() {
        let mut doc = Document::parse(&src).unwrap();
        let root = doc.root();
        let runs: Vec<NodeId> = doc.descendants_named(root, ns::W, "r").collect();
        let r = middle(&runs);
        let end = doc.source_span(r).unwrap().end;
        let new_r = doc.create_element("w:r").unwrap();
        let t = doc.create_element("w:t").unwrap();
        doc.set_attr(t, "xml:space", "preserve").unwrap();
        doc.set_text(t, " inserted ").unwrap();
        doc.append_child(new_r, t).unwrap();
        doc.insert_after(r, new_r).unwrap();
        assert_eq!(doc.namespace(new_r), Some(ns::W), "{label}: new element resolves its prefix");
        let out = doc.to_bytes();
        assert_local(&src, &out, end..end);
        let inserted = &out[end..out.len() - (src.len() - end)];
        assert!(inserted == b"<w:r><w:t xml:space=\"preserve\"> inserted </w:t></w:r>", "{label}");
        reparse(&label, &out);
    }
}

#[test]
fn remove_one_paragraph() {
    for (label, src) in documents() {
        let mut doc = Document::parse(&src).unwrap();
        let root = doc.root();
        let ps: Vec<NodeId> = doc.descendants_named(root, ns::W, "p").collect();
        let p = middle(&ps);
        let span = doc.source_span(p).unwrap();
        doc.remove(p).unwrap();
        let out = doc.to_bytes();
        assert_local(&src, &out, span.clone());
        assert_eq!(out.len(), src.len() - span.len(), "{label}");
        reparse(&label, &out);
    }
}

#[test]
fn edits_far_apart_stay_local() {
    // Two edits in different paragraphs: everything except the two regions is identical.
    for (label, src) in documents() {
        let mut doc = Document::parse(&src).unwrap();
        let root = doc.root();
        let ts: Vec<NodeId> = doc
            .descendants_named(root, ns::W, "t")
            .filter(|&t| doc.first_child(t).is_some_and(|c| doc.next_sibling(c).is_none()))
            .collect();
        if ts.len() < 4 {
            continue;
        }
        let (a, b) = (ts[ts.len() / 4], ts[3 * ts.len() / 4]);
        let (sa, sb) = (
            doc.source_span(doc.first_child(a).unwrap()).unwrap(),
            doc.source_span(doc.first_child(b).unwrap()).unwrap(),
        );
        doc.set_text(a, "A").unwrap();
        doc.set_text(b, "B").unwrap();
        let out = doc.to_bytes();
        let mut expect = Vec::new();
        expect.extend_from_slice(&src[..sa.start]);
        expect.extend_from_slice(b"A");
        expect.extend_from_slice(&src[sa.end..sb.start]);
        expect.extend_from_slice(b"B");
        expect.extend_from_slice(&src[sb.end..]);
        assert!(out == expect, "{label}");
    }
}
