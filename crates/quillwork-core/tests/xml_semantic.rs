//! Semantic checks against an independent parser (roxmltree): what we write means what we said.

mod xml_support;

use quillwork_core::xml::{Document, ns};

const NASTY: &[&str] = &[
    "plain",
    "a & b",
    "x < y > z",
    "double \" quote",
    "single ' quote",
    "both \" and '",
    "tab\there",
    "line\nfeed",
    "carriage\rreturn",
    "crlf\r\npair",
    "  leading and trailing  ",
    "&amp; literally",
    "]]> and --",
    "unicode é € 😀",
    "",
];

#[test]
fn attribute_values_round_trip_through_roxmltree() {
    for quote in ['"', '\''] {
        let src = format!("<w:a xmlns:w=\"{}\" w:old={q}x{q}/>", ns::W, q = quote);
        for v in NASTY {
            let mut d = Document::parse(src.as_bytes()).unwrap();
            let r = d.root();
            d.set_attr(r, "w:old", v).unwrap();
            d.set_attr(r, "w:new", v).unwrap();
            let out = String::from_utf8(d.to_bytes()).unwrap();
            let rx = roxmltree::Document::parse(&out).unwrap_or_else(|e| panic!("{out}: {e}"));
            let re = rx.root_element();
            assert_eq!(re.attribute((ns::W, "old")), Some(*v), "{out}");
            assert_eq!(re.attribute((ns::W, "new")), Some(*v), "{out}");
            // And through our own parser.
            let back = Document::parse(out.as_bytes()).unwrap();
            assert_eq!(back.attr(back.root(), "w:old").as_deref(), Some(*v));
            assert_eq!(back.attr_ns(back.root(), ns::W, "new").as_deref(), Some(*v));
        }
    }
}

#[test]
fn text_values_round_trip_through_roxmltree() {
    for v in NASTY {
        let mut d = Document::parse(b"<a><t xml:space=\"preserve\">old</t></a>").unwrap();
        let t = d.first_child(d.root()).unwrap();
        d.set_text(t, v).unwrap();
        let out = String::from_utf8(d.to_bytes()).unwrap();
        let rx = roxmltree::Document::parse(&out).unwrap_or_else(|e| panic!("{out}: {e}"));
        let rt = rx.root_element().first_element_child().unwrap();
        assert_eq!(rt.text().unwrap_or(""), *v, "{out}");
        let back = Document::parse(out.as_bytes()).unwrap();
        assert_eq!(back.text_content(back.root()), *v);
    }
}

/// Our read API agrees with roxmltree on every element name, namespace, attribute and text in
/// the corpus document parts.
#[test]
fn read_api_agrees_with_roxmltree_on_corpus() {
    let files: Vec<_> = xml_support::synthetic_docx().into_iter().chain(xml_support::private_docx()).collect();
    let mut checked = 0usize;
    for path in files {
        for (name, data) in xml_support::xml_parts(&path) {
            let Ok(s) = std::str::from_utf8(&data) else { continue };
            let s = s.strip_prefix('\u{FEFF}').unwrap_or(s);
            let rx = roxmltree::Document::parse_with_options(
                s,
                roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: u32::MAX, ..Default::default() },
            )
            .unwrap_or_else(|e| panic!("roxmltree on {name}: {e}"));
            let d = Document::parse(&data).unwrap();
            let ours: Vec<_> = d.descendants(d.root()).filter(|&n| d.is_element_node(n)).collect();
            let theirs: Vec<_> = rx.root_element().descendants().filter(|n| n.is_element()).collect();
            assert_eq!(ours.len(), theirs.len(), "{name}: element count");
            for (&o, t) in ours.iter().zip(&theirs) {
                assert!(d.local_name(o) == Some(t.tag_name().name()), "{name}: local name");
                assert!(d.namespace(o) == t.tag_name().namespace(), "{name}: namespace");
                for a in t.attributes() {
                    let v = match a.namespace() {
                        Some(uri) => d.attr_ns(o, uri, a.name()),
                        None => d.attr_ns(o, "", a.name()),
                    };
                    assert!(v.as_deref() == Some(a.value()), "{name}: attribute value");
                }
                let our_text: String = d
                    .children(o)
                    .filter_map(|c| {
                        d.text(c).filter(|_| {
                            matches!(
                                d.kind(c),
                                Some(quillwork_core::xml::NodeKind::Text | quillwork_core::xml::NodeKind::CData)
                            )
                        })
                    })
                    .collect();
                let their_text: String = t.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect();
                assert!(our_text == their_text, "{name}: text");
                checked += 1;
            }
        }
    }
    eprintln!("{checked} elements agree with roxmltree");
    assert!(checked > 0);
}
