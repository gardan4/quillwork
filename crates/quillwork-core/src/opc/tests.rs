use super::*;

#[test]
fn names_roundtrip() {
    assert_eq!(rels_part_for("word/document.xml"), "word/_rels/document.xml.rels");
    assert_eq!(rels_part_for(""), "_rels/.rels");
    assert_eq!(rels_part_for("/foo.xml"), "_rels/foo.xml.rels");
    assert_eq!(source_of_rels("word/_rels/document.xml.rels").as_deref(), Some("word/document.xml"));
    assert_eq!(source_of_rels("_rels/.rels").as_deref(), Some(""));
    assert_eq!(source_of_rels("_rels/foo.xml.rels").as_deref(), Some("foo.xml"));
    assert_eq!(source_of_rels("word/document.xml"), None);
    assert_eq!(source_of_rels("word/foo.rels"), None);
}

#[test]
fn resolve_targets() {
    assert_eq!(resolve_target("word/document.xml", "media/image1.png").as_deref(), Some("word/media/image1.png"));
    assert_eq!(resolve_target("word/document.xml", "/word/styles.xml").as_deref(), Some("word/styles.xml"));
    assert_eq!(resolve_target("word/document.xml", "../customXml/item1.xml").as_deref(), Some("customXml/item1.xml"));
    assert_eq!(resolve_target("", "word/document.xml").as_deref(), Some("word/document.xml"));
    assert_eq!(resolve_target("word/document.xml", "media/a%20b.png").as_deref(), Some("word/media/a b.png"));
    assert_eq!(resolve_target("word/document.xml", "../../etc/passwd"), None);
    assert_eq!(resolve_target("word/document.xml", "http://x/y.png"), None);
    assert_eq!(resolve_target("word/document.xml", "#bookmark"), None);
}

#[test]
fn relative_targets() {
    assert_eq!(relative_target("word/document.xml", "word/media/image1.png"), "media/image1.png");
    assert_eq!(relative_target("word/document.xml", "customXml/item1.xml"), "../customXml/item1.xml");
    assert_eq!(relative_target("", "word/document.xml"), "word/document.xml");
    assert_eq!(relative_target("word/document.xml", "word/a b.png"), "a%20b.png");
    for (s, p) in [
        ("word/document.xml", "word/media/x.png"),
        ("word/glossary/document.xml", "word/media/x.png"),
        ("", "docProps/core.xml"),
    ] {
        assert_eq!(resolve_target(s, &relative_target(s, p)).as_deref(), Some(p));
    }
}

#[test]
fn pct() {
    assert_eq!(pct_decode("a%20b%zz%4"), "a b%zz%4");
    assert_eq!(pct_encode("word/é x.png"), "word/%C3%A9%20x.png");
}

#[test]
fn name_validation() {
    for bad in ["/abs", "a\\b", "../x", "a/../b", "a//b", "a/./b", "C:/x", "a\u{0}b", ""] {
        assert!(zipfmt::validate_name(bad, false).is_err(), "{bad:?}");
    }
    assert!(zipfmt::validate_name("word/", true).is_ok());
    assert!(zipfmt::validate_name("word/", false).is_err());
    assert!(zipfmt::validate_name("[Content_Types].xml", false).is_ok());
}

#[test]
fn xmlscan_insert_and_selfclosing() {
    let xml = br#"<?xml version="1.0"?><Types xmlns="x"><Default Extension="xml" ContentType="a"/></Types>"#;
    let s = xmlscan::scan(xml, "Types").unwrap();
    assert_eq!(s.children.len(), 1);
    let out = s.insert(xml, "<Z/>");
    assert_eq!(out, br#"<?xml version="1.0"?><Types xmlns="x"><Default Extension="xml" ContentType="a"/><Z/></Types>"#);
    let xml = br#"<p:Types xmlns:p="x" />"#;
    let s = xmlscan::scan(xml, "Types").unwrap();
    assert_eq!(s.child_prefix(), "p:");
    assert_eq!(s.insert(xml, "<p:Z/>"), br#"<p:Types xmlns:p="x" ><p:Z/></p:Types>"#);
    assert!(xmlscan::scan(b"<!DOCTYPE x><Types/>", "Types").is_err());
    assert!(xmlscan::scan(b"<Other/>", "Types").is_err());
    assert!(xmlscan::scan(b"<Types>", "Types").is_err());
}

#[test]
fn content_type_lookup() {
    let ct = ContentTypes::parse(br#"<Types xmlns="x"><Default Extension="XML" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="doc"/></Types>"#).unwrap();
    assert_eq!(ct.content_type_of("word/document.xml"), Some("doc"));
    assert_eq!(ct.content_type_of("/WORD/Document.xml"), Some("doc"));
    assert_eq!(ct.content_type_of("word/styles.xml"), Some("application/xml"));
    assert_eq!(ct.content_type_of("word/media/a.png"), None);
}
