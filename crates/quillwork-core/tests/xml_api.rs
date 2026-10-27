//! Read and mutation API behaviour on small hand-written documents.

use quillwork_core::Error;
use quillwork_core::xml::{Document, NodeId, NodeKind, ns};

const W: &str = ns::W;

fn doc(s: &str) -> Document {
    Document::parse(s.as_bytes()).unwrap()
}

fn out(d: &Document) -> String {
    String::from_utf8(d.to_bytes()).unwrap()
}

const BODY: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\r\n",
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
    r#"<w:body><w:p w:rsidR='00A1'><w:r><w:t>Hello </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">big world</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>Second &amp; last</w:t></w:r></w:p></w:body></w:document>"#
);

/// The usage example from the report: find the paragraph containing some text and change the
/// text of one of its runs.
#[test]
fn usage_example() {
    let mut d = Document::parse(BODY.as_bytes()).unwrap();
    let body = d.find_child(d.root(), W, "body").unwrap();
    let p = d.child_elements(body).find(|&p| d.text_content(p).contains("big world")).unwrap();
    let t = d.descendants_named(p, W, "t").find(|&t| d.text_content(t) == "big world").unwrap();
    d.set_text(t, "small world").unwrap();
    assert_eq!(out(&d), BODY.replace("big world", "small world"));
}

#[test]
fn navigation_and_names() {
    let d = doc(BODY);
    let root = d.root();
    assert_eq!(d.kind(d.document_node()), Some(NodeKind::Document));
    let top: Vec<_> = d.children(d.document_node()).map(|c| d.kind(c).unwrap()).collect();
    assert_eq!(top, [NodeKind::Declaration, NodeKind::Text, NodeKind::Element]);
    assert_eq!(d.name(root), Some("w:document"));
    assert_eq!(d.local_name(root), Some("document"));
    assert_eq!(d.prefix(root), Some("w"));
    assert_eq!(d.namespace(root), Some(W));
    assert!(d.is(root, W, "document"));
    let body = d.find_child(root, W, "body").unwrap();
    let ps: Vec<_> = d.child_elements(body).collect();
    assert_eq!(ps.len(), 2);
    assert_eq!(d.next_sibling(ps[0]), Some(ps[1]));
    assert_eq!(d.prev_sibling(ps[1]), Some(ps[0]));
    assert_eq!(d.parent(ps[0]), Some(body));
    assert_eq!(d.last_child(body), Some(ps[1]));
    assert_eq!(d.ancestors(ps[0]).collect::<Vec<_>>(), [body, root, d.document_node()]);
    assert_eq!(d.descendants_named(root, W, "t").count(), 3);
    assert_eq!(d.text_content(body), "Hello big worldSecond & last");
    assert_eq!(d.find_prefix(ps[0], ns::R), Some("r"));
    assert_eq!(d.find_prefix(ps[0], "urn:nope"), None);
    assert_eq!(d.lookup_namespace(ps[0], "w"), Some(W));
    assert_eq!(d.lookup_namespace(ps[0], "xml"), Some(ns::XML));
    let t2 = d.descendants_named(root, W, "t").nth(1).unwrap();
    assert_eq!(d.attr(t2, "xml:space").as_deref(), Some("preserve"));
    assert_eq!(d.attr_ns(t2, ns::XML, "space").as_deref(), Some("preserve"));
    assert_eq!(d.attr_ns(ps[0], W, "rsidR").as_deref(), Some("00A1"));
    assert_eq!(d.attr_ns(ps[0], "", "rsidR"), None);
    assert_eq!(d.attr_count(root), 2);
    assert_eq!(d.namespace_decls(root).len(), 2);
    let txt = d.first_child(t2).unwrap();
    assert_eq!(d.kind(txt), Some(NodeKind::Text));
    assert_eq!(d.text(txt).as_deref(), Some("big world"));
    let last_t = d.descendants_named(root, W, "t").nth(2).unwrap();
    assert_eq!(d.text(d.first_child(last_t).unwrap()).as_deref(), Some("Second & last"));
    // Invalid ids never panic.
    let bogus = NodeId::from_u32(u32::MAX - 7);
    assert_eq!(d.kind(bogus), None);
    assert_eq!(d.children(bogus).count(), 0);
    assert_eq!(d.text_content(bogus), "");
}

#[test]
fn default_and_undeclared_namespaces() {
    let d = doc(r#"<a xmlns="urn:d" xmlns:x="urn:x"><b/><c xmlns=""><x:e/></c><y:f/></a>"#);
    let root = d.root();
    let kids: Vec<_> = d.child_elements(root).collect();
    assert_eq!(d.namespace(root), Some("urn:d"));
    assert!(d.is(kids[0], "urn:d", "b"));
    assert_eq!(d.namespace(kids[1]), None, "xmlns=\"\" undeclares the default");
    assert!(d.is(d.first_child(kids[1]).unwrap(), "urn:x", "e"));
    assert_eq!(d.namespace(kids[2]), None, "unbound prefix is tolerated, but has no namespace");
    assert_eq!(d.find_prefix(kids[0], "urn:d"), Some(""));
}

#[test]
fn set_attr_keeps_quote_order_and_raw_neighbours() {
    let mut d = doc(r#"<w:a xmlns:w="urn:w"  w:x='1'   w:y="&#9;raw" w:z='3' />"#);
    let r = d.root();
    d.set_attr(r, "w:x", "it's \"q\"").unwrap();
    assert_eq!(out(&d), r#"<w:a xmlns:w="urn:w"  w:x='it&apos;s "q"'   w:y="&#9;raw" w:z='3' />"#);
    d.set_attr(r, "w:new", "a\tb\nc\rd<&>").unwrap();
    assert_eq!(
        out(&d),
        r#"<w:a xmlns:w="urn:w"  w:x='it&apos;s "q"'   w:y="&#9;raw" w:z='3' w:new="a&#x9;b&#xA;c&#xD;d&lt;&amp;&gt;" />"#
    );
    assert_eq!(d.attr(r, "w:new").as_deref(), Some("a\tb\nc\rd<&>"));
    assert!(d.remove_attr(r, "w:y").unwrap());
    assert!(!d.remove_attr(r, "w:y").unwrap());
    assert_eq!(
        out(&d),
        r#"<w:a xmlns:w="urn:w"  w:x='it&apos;s "q"' w:z='3' w:new="a&#x9;b&#xA;c&#xD;d&lt;&amp;&gt;" />"#
    );
}

#[test]
fn setting_same_value_is_a_no_op() {
    let mut d = doc(r#"<a b="x&amp;y"><c>t&lt;</c></a>"#);
    let r = d.root();
    d.set_attr(r, "b", "x&y").unwrap();
    let c = d.first_child(r).unwrap();
    d.set_text(c, "t<").unwrap();
    assert!(!d.is_modified());
}

#[test]
fn set_attr_ns_uses_existing_prefix() {
    let mut d = doc(r#"<w:p xmlns:w="urn:w"><w:r/></w:p>"#);
    let r = d.first_child(d.root()).unwrap();
    d.set_attr_ns(r, "urn:w", "rsidR", "1").unwrap();
    d.set_attr_ns(r, "urn:w", "rsidR", "2").unwrap();
    d.set_attr_ns(r, "", "plain", "3").unwrap();
    assert!(matches!(d.set_attr_ns(r, "urn:missing", "x", "4"), Err(Error::XmlNode(_))));
    assert_eq!(out(&d), r#"<w:p xmlns:w="urn:w"><w:r w:rsidR="2" plain="3"/></w:p>"#);
}

#[test]
fn self_closing_element_gains_children() {
    let mut d = doc(r#"<a><b x="1" /></a>"#);
    let b = d.first_child(d.root()).unwrap();
    let c = d.create_element("c").unwrap();
    d.append_child(b, c).unwrap();
    assert_eq!(out(&d), r#"<a><b x="1" ><c/></b></a>"#);
}

#[test]
fn emptied_element_keeps_open_close_form() {
    let mut d = doc(r#"<a><b>x</b><c><d/></c></a>"#);
    let b = d.first_child(d.root()).unwrap();
    let c = d.next_sibling(b).unwrap();
    d.set_text(b, "").unwrap();
    d.remove(d.first_child(c).unwrap()).unwrap();
    assert_eq!(out(&d), r#"<a><b></b><c></c></a>"#);
}

#[test]
fn set_text_on_element_with_mixed_children() {
    let mut d = doc(r#"<a><b>x<!--c--><i/>y</b></a>"#);
    let b = d.first_child(d.root()).unwrap();
    d.set_text(b, "new").unwrap();
    assert_eq!(out(&d), r#"<a><b>new</b></a>"#);
    assert_eq!(d.text_content(b), "new");
}

#[test]
fn set_text_on_cdata_and_comment() {
    let mut d = doc(r#"<a><![CDATA[x]]><!--y--></a>"#);
    let cd = d.first_child(d.root()).unwrap();
    let cm = d.next_sibling(cd).unwrap();
    d.set_text(cd, "1 < 2").unwrap();
    d.set_text(cm, " note ").unwrap();
    assert!(d.set_text(cd, "]]>").is_err());
    assert!(d.set_text(cm, "a--b").is_err());
    assert_eq!(out(&d), r#"<a><![CDATA[1 < 2]]><!-- note --></a>"#);
}

#[test]
fn rename_regenerates_both_tags() {
    let mut d = doc(r#"<w:r xmlns:w="urn:w"><w:t a='1' >x</w:t ></w:r>"#);
    let t = d.first_child(d.root()).unwrap();
    d.set_name(t, "w:delText").unwrap();
    assert_eq!(out(&d), r#"<w:r xmlns:w="urn:w"><w:delText a='1' >x</w:delText></w:r>"#);
    assert!(d.is(t, "urn:w", "delText"));
}

#[test]
fn move_clone_and_order() {
    let mut d = doc(r#"<a><b><x>1</x></b><c/></a>"#);
    let b = d.first_child(d.root()).unwrap();
    let c = d.next_sibling(b).unwrap();
    let x = d.first_child(b).unwrap();
    // A clone of a clean source subtree serialises from its source bytes.
    let x2 = d.clone_subtree(x).unwrap();
    d.append_child(c, x2).unwrap();
    assert_eq!(out(&d), r#"<a><b><x>1</x></b><c><x>1</x></c></a>"#);
    // Editing the clone does not touch the original.
    d.set_text(x2, "2").unwrap();
    assert_eq!(out(&d), r#"<a><b><x>1</x></b><c><x>2</x></c></a>"#);
    // Moving: inserting an attached node moves it.
    d.insert_before(b, x).unwrap();
    assert_eq!(out(&d), r#"<a><x>1</x><b></b><c><x>2</x></c></a>"#);
    let y = d.create_element("y").unwrap();
    d.prepend_child(c, y).unwrap();
    let z = d.create_text("z&").unwrap();
    d.insert_after(y, z).unwrap();
    assert_eq!(out(&d), r#"<a><x>1</x><b></b><c><y/>z&amp;<x>2</x></c></a>"#);
}

#[test]
fn structural_errors() {
    let mut d = doc(r#"<a><b><c/></b></a>"#);
    let root = d.root();
    let b = d.first_child(root).unwrap();
    let c = d.first_child(b).unwrap();
    assert!(d.append_child(c, b).is_err(), "cycle");
    assert!(d.append_child(b, b).is_err(), "self");
    assert!(d.insert_after(b, b).is_err());
    assert!(d.detach(root).is_err());
    assert!(d.detach(d.document_node()).is_err());
    assert!(d.insert_before(root, d.document_node()).is_err());
    let e = d.create_element("e").unwrap();
    assert!(d.insert_after(root, e).is_err(), "second root element");
    assert!(d.insert_after(e, c).is_err(), "detached reference has no parent");
    let ws = d.create_text("\n").unwrap();
    d.insert_after(root, ws).unwrap();
    let cm = d.create_comment(" end ").unwrap();
    d.append_child(d.document_node(), cm).unwrap();
    assert!(d.create_element("1bad").is_err());
    assert!(d.create_element("a:b:c").is_err());
    assert!(d.create_text("bell\u{7}").is_err());
    assert!(d.set_attr(b, "xmlns:xml", "urn:x").is_err());
    let text = d.create_text("t").unwrap();
    assert!(d.set_attr(text, "a", "1").is_err(), "attributes only on elements");
    assert!(d.set_text(NodeId::from_u32(9999), "x").is_err());
    assert_eq!(out(&d), "<a><b><c/></b></a>\n<!-- end -->");
    // A detached node can be inserted again.
    d.detach(c).unwrap();
    d.append_child(root, c).unwrap();
    assert_eq!(out(&d), "<a><b></b><c/></a>\n<!-- end -->");
}

#[test]
fn namespace_changes_reresolve() {
    let mut d = doc(r#"<p:a xmlns:p="urn:1"><p:b/></p:a>"#);
    let root = d.root();
    let b = d.first_child(root).unwrap();
    d.set_attr(b, "xmlns:p", "urn:2").unwrap();
    assert!(d.is(b, "urn:2", "b"));
    assert!(d.is(root, "urn:1", "a"));
    // Moving an element under a different binding re-resolves it.
    let mut d2 = doc(r#"<r xmlns:p="urn:1"><s xmlns:p="urn:2"/><p:t/></r>"#);
    let s = d2.first_child(d2.root()).unwrap();
    let t = d2.next_sibling(s).unwrap();
    assert!(d2.is(t, "urn:1", "t"));
    d2.append_child(s, t).unwrap();
    assert!(d2.is(t, "urn:2", "t"));
    // A created element with an undeclared prefix has no namespace until it is declared.
    let u = d2.create_element("q:u").unwrap();
    d2.append_child(d2.root(), u).unwrap();
    assert_eq!(d2.namespace(u), None);
    d2.set_attr(d2.root(), "xmlns:q", "urn:q").unwrap();
    assert!(d2.is(u, "urn:q", "u"));
}

#[test]
fn node_to_bytes_and_spans() {
    let src = r#"<a><b k="v">t</b></a>"#;
    let mut d = doc(src);
    let b = d.first_child(d.root()).unwrap();
    assert_eq!(d.source_span(b), Some(3..17));
    assert_eq!(d.node_to_bytes(b), br#"<b k="v">t</b>"#);
    d.set_attr(b, "k", "w").unwrap();
    assert_eq!(d.node_to_bytes(b), br#"<b k="w">t</b>"#);
    let n = d.create_element("n").unwrap();
    assert_eq!(d.source_span(n), None);
}
