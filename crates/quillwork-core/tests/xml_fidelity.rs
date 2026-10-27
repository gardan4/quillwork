//! Byte identity: parse + serialise with no edits returns exactly the input, both through the
//! clean fast path and through a forced per-node walk (which proves every span is right).

mod xml_support;

use quillwork_core::xml::Document;

const NS: &str = r#"xmlns:w="urn:w""#;

fn cases() -> Vec<(&'static str, Vec<u8>)> {
    let s = |x: String| x.into_bytes();
    vec![
        ("xml decl + CRLF", s(format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<w:a {NS}/>"))),
        ("BOM", [b"\xEF\xBB\xBF".as_slice(), format!("<?xml version=\"1.0\"?><w:a {NS}/>").as_bytes()].concat()),
        ("attr order", s(format!("<w:a {NS} w:z=\"1\" w:a=\"2\" w:m=\"3\"/>"))),
        ("attr before xmlns", s(format!("<w:a w:b=\"1\" {NS} xmlns:x=\"urn:x\" x:c=\"2\"/>"))),
        ("single-quoted attr", s(format!("<w:a {NS} w:b='1' w:c='a\"b'/>"))),
        ("whitespace in tags", s(format!("<w:a  {NS}  w:b = \"1\" ></w:a >"))),
        ("newlines in tags", s(format!("<w:a\r\n  {NS}\r\n  w:b=\"1\"\r\n/>"))),
        ("<x></x> vs <x/>", s(format!("<w:a {NS}><w:b></w:b><w:c/><w:d /></w:a>"))),
        ("text entities", s(format!("<w:a {NS}>&apos;&quot;&gt;&lt;&amp;&#169;&#xA9;&#x1F600;</w:a>"))),
        ("attr entities", s(format!("<w:a {NS} w:b=\"&#10;&apos;x&gt;&#9;&#xD;\"/>"))),
        ("raw tab/newline in attr", s(format!("<w:a {NS} w:b=\"x\ty\nz\"/>"))),
        ("comment + PI", s(format!("<!-- pre --><?pi top?><w:a {NS}><!-- c --><?pi data?></w:a><!-- post -->\n"))),
        ("CDATA", s(format!("<w:a {NS}><![CDATA[x<y&z]]]]><![CDATA[>]]></w:a>"))),
        ("whitespace text", s(format!("<w:a {NS}>\n  <w:b/>\n\t<w:c>  </w:c>\n</w:a>"))),
        ("xml:space preserve", s(format!("<w:a {NS}><w:t xml:space=\"preserve\">  two  spaces  </w:t></w:a>"))),
        ("redundant xmlns", s(format!("<w:a {NS}><w:b {NS}/></w:a>"))),
        ("mc:Ignorable", s(format!("<w:a {NS} xmlns:w14=\"urn:w14\" xmlns:mc=\"urn:mc\" mc:Ignorable=\"w14\"/>"))),
        ("default xmlns", s("<a xmlns=\"urn:d\"><b/><c xmlns=\"\"/></a>".to_string())),
        ("CR in text", s(format!("<w:a {NS}>x\r\ny\rz</w:a>"))),
        ("non-ASCII", s(format!("<w:a {NS} w:b=\"ü\">é€😀</w:a>"))),
        ("trailing whitespace", s(format!("<w:a {NS}/>\r\n\r\n"))),
        ("unprefixed rels", s("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"t\" Target=\"a.xml?x=1&amp;y=2\"/></Relationships>".to_string())),
    ]
}

#[test]
fn synthetic_cases_round_trip() {
    for (label, input) in cases() {
        let doc = Document::parse(&input).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(!doc.is_modified(), "{label}");
        assert_eq!(doc.to_bytes(), input, "{label}: to_bytes");
        assert_eq!(doc.to_bytes_walked(), input, "{label}: walked");
    }
}

/// Every `.xml` / `.rels` part of every corpus docx must round-trip byte-identical.
#[test]
fn corpus_parts_round_trip() {
    let mut report = Vec::new();
    for (label, files) in [("synthetic", xml_support::synthetic_docx()), ("private", xml_support::private_docx())] {
        if files.is_empty() {
            eprintln!("{label} corpus: absent, skipped");
            continue;
        }
        let (mut parts, mut bytes, mut nodes) = (0usize, 0usize, 0usize);
        for path in &files {
            for (name, data) in xml_support::xml_parts(path) {
                let doc = Document::parse(&data).unwrap_or_else(|e| {
                    // Name the part, not the file: private file names are not reported.
                    panic!("{label}: part {name} failed to parse: {e}")
                });
                assert!(doc.to_bytes() == data, "{label}: part {name} not identical (fast path)");
                assert!(doc.to_bytes_walked() == data, "{label}: part {name} not identical (walked)");
                parts += 1;
                bytes += data.len();
                nodes += doc.node_count();
            }
        }
        report.push(format!(
            "{label} corpus: {} docx, {parts} xml/rels parts, {bytes} bytes, {nodes} nodes: all byte-identical",
            files.len()
        ));
    }
    assert!(!report.is_empty(), "no corpus found at all");
    for line in report {
        eprintln!("{line}");
    }
}
