use super::*;
use crate::edit::test_util::{NS, doc, doc_with};

const P: &str = r#"<w:p w14:paraId="00000001" w14:textId="77777777"><w:r><w:t>ok</w:t></w:r></w:p>"#;
const COMMENTS_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";

fn codes(r: &Report) -> Vec<String> {
    r.findings.iter().map(|f| format!("{:?}:{}", f.severity, f.code)).collect()
}

fn errors_of(body: &str) -> Vec<String> {
    let d = doc(body);
    check(&d).errors().map(|f| f.code.clone()).collect()
}

#[test]
fn clean_document_has_no_findings() {
    let body = format!(
        r#"{P}<w:p w14:paraId="00000002" w14:textId="77777777"><w:pPr><w:pStyle w:val="Heading1"/><w:keepNext/><w:spacing w:after="0"/><w:jc w:val="center"/><w:rPr><w:b/></w:rPr></w:pPr><w:ins w:id="1" w:author="a" w:date="2026-01-01T00:00:00Z"><w:r><w:rPr><w:b w:val="0"/><w:sz w:val="20"/></w:rPr><w:t xml:space="preserve"> x </w:t></w:r></w:ins><w:ins w:id="2" w:author="a"><w:del w:id="3" w:author="b"><w:r><w:delText>gone</w:delText></w:r></w:del></w:ins><w:bookmarkStart w:id="0" w:name="b"/><w:bookmarkEnd w:id="0"/></w:p><w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/><w:jc w:val="center"/></w:tblPr><w:tblGrid><w:gridCol w:w="10"/><w:gridCol w:w="10"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vAlign w:val="center"/></w:tcPr><w:p/><w:bookmarkStart w:id="9" w:name="c"/><w:bookmarkEnd w:id="9"/></w:tc></w:tr></w:tbl><w:sectPr><w:footerReference w:type="default" r:id="rIdX0"/><w:headerReference w:type="default" r:id="rIdX0"/><w:pgSz w:w="11906" w:h="16838"/><w:docGrid w:linePitch="360"/></w:sectPr>"#
    );
    let d = doc_with(
        &body,
        &[(
            "word/footer1.xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer",
            &format!(r#"<w:ftr {NS}><w:p/></w:ftr>"#),
        )],
    );
    let r = check(&d);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
}

#[test]
fn properties_must_come_first() {
    assert_eq!(errors_of(r#"<w:p><w:r><w:t>a</w:t></w:r><w:pPr/></w:p>"#), ["order.properties_not_first"]);
    assert_eq!(errors_of(r#"<w:p><w:r><w:t>a</w:t><w:rPr/></w:r></w:p>"#), ["order.properties_not_first"]);
    assert_eq!(
        errors_of(
            r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tblPr/><w:tr><w:tc><w:p/><w:tcPr/></w:tc></w:tr></w:tbl>"#
        ),
        ["order.properties_not_first", "order.properties_not_first"]
    );
    assert_eq!(
        errors_of(r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p/></w:tc><w:trPr/></w:tr></w:tbl>"#),
        ["order.properties_not_first"]
    );
}

#[test]
fn schema_order_inside_properties() {
    let r = check(&doc(r#"<w:p><w:pPr><w:jc w:val="center"/><w:pStyle w:val="Normal"/></w:pPr></w:p>"#));
    assert_eq!(codes(&r), ["Error:order.child_order"]);
    assert!(r.findings[0].message.contains("pStyle"));
    assert_eq!(r.findings[0].path.as_deref(), Some("/w:document[1]/w:body[1]/w:p[1]/w:pPr[1]/w:pStyle[1]"));
    // rPr order is a warning; tcPr order an error; duplicates an error.
    let r = check(&doc(r#"<w:p><w:r><w:rPr><w:sz w:val="2"/><w:b/></w:rPr></w:r></w:p>"#));
    assert_eq!(codes(&r), ["Warning:order.child_order"]);
    assert_eq!(
        errors_of(
            r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:tcPr><w:vAlign w:val="top"/><w:tcW w:w="1"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#
        ),
        ["order.child_order"]
    );
    assert_eq!(
        errors_of(r#"<w:p><w:pPr><w:jc w:val="left"/><w:jc w:val="left"/></w:pPr></w:p>"#),
        ["order.child_duplicate"]
    );
    // trPr: base children in any order, ins/del after them.
    assert!(errors_of(r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="1"/><w:cantSplit/><w:ins w:id="1" w:author="a"/></w:trPr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#).is_empty());
    assert_eq!(
        errors_of(
            r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:trPr><w:ins w:id="1" w:author="a"/><w:cantSplit/></w:trPr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#
        ),
        ["order.child_order"]
    );
    assert_eq!(errors_of(r#"<w:sectPr/><w:p/>"#), ["order.sectpr_not_last"]);
}

#[test]
fn enum_values() {
    assert_eq!(errors_of(r#"<w:p><w:pPr><w:jc w:val="middle"/></w:pPr></w:p>"#), ["value.invalid"]);
    assert_eq!(
        errors_of(
            r#"<w:tbl><w:tblPr><w:jc w:val="both"/></w:tblPr><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#
        ),
        ["value.invalid"]
    );
    assert_eq!(errors_of(r#"<w:p><w:r><w:rPr><w:b w:val="yes"/></w:rPr></w:r></w:p>"#), ["value.invalid"]);
    assert!(errors_of(r#"<w:p><w:r><w:rPr><w:b w:val="0"/><w:i w:val="true"/></w:rPr></w:r></w:p>"#).is_empty());
}

#[test]
fn tracked_change_structure() {
    assert_eq!(
        errors_of(r#"<w:p><w:del w:id="1" w:author="a"><w:r><w:t>x</w:t></w:r></w:del></w:p>"#),
        ["track.text_in_deletion"]
    );
    assert_eq!(
        errors_of(
            r#"<w:p><w:ins w:id="1" w:author="a"><w:ins w:id="2" w:author="a"><w:r><w:t>x</w:t></w:r></w:ins></w:ins></w:p>"#
        ),
        ["track.nested"]
    );
    assert_eq!(
        errors_of(
            r#"<w:p><w:del w:id="1" w:author="a"><w:ins w:id="2" w:author="a"><w:r><w:delText>x</w:delText></w:r></w:ins></w:del></w:p>"#
        ),
        ["track.nested"]
    );
    assert_eq!(
        errors_of(
            r#"<w:p><w:ins w:id="1" w:author="a"><w:r><w:t>x</w:t></w:r></w:ins><w:del w:id="1" w:author="a"><w:r><w:delText>y</w:delText></w:r></w:del></w:p>"#
        ),
        ["id.revision_duplicate"]
    );
    // A paragraph-mark marker and a text-box paragraph inside a deleted drawing are fine.
    assert!(errors_of(r#"<w:p><w:pPr><w:rPr><w:del w:id="5" w:author="a"/></w:rPr></w:pPr><w:del w:id="6" w:author="a"><w:r><w:pict><w:txbxContent><w:p><w:r><w:t>t</w:t></w:r></w:p></w:txbxContent></w:pict></w:r></w:del></w:p>"#).is_empty());
    let r = check(&doc(r#"<w:p><w:r><w:delText>stray</w:delText></w:r></w:p>"#));
    assert_eq!(codes(&r), ["Warning:track.deltext_outside_deletion"]);
}

#[test]
fn para_ids() {
    assert_eq!(errors_of(r#"<w:p w14:paraId="80000000"/>"#), ["id.para_id_invalid"]);
    assert_eq!(errors_of(r#"<w:p w14:paraId="00000000"/>"#), ["id.para_id_invalid"]);
    assert_eq!(errors_of(r#"<w:p w14:paraId="12AB"/>"#), ["id.para_id_invalid"]);
    assert_eq!(errors_of(r#"<w:p w14:paraId="1234ABCD" w14:textId="FFFFFFFF"/>"#), ["id.text_id_invalid"]);
    assert_eq!(errors_of(r#"<w:p w14:paraId="1234ABCD"/><w:p w14:paraId="1234abcd"/>"#), ["id.para_id_duplicate"]);
    // Fallback copies of a text box repeat ids by design.
    let tb = r#"<w:txbxContent><w:p w14:paraId="0000AAAA"/></w:txbxContent>"#;
    let body = format!(
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>{tb}</w:drawing></mc:Choice><mc:Fallback><w:pict>{tb}</w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"#
    );
    assert!(errors_of(&body).is_empty());
}

#[test]
fn bookmarks() {
    let r = check(&doc(r#"<w:p><w:bookmarkStart w:id="1" w:name="a"/></w:p><w:p><w:bookmarkEnd w:id="2"/></w:p>"#));
    assert_eq!(codes(&r), ["Warning:bookmark.unpaired", "Warning:bookmark.unpaired"]);
    assert_eq!(
        errors_of(
            r#"<w:p><w:bookmarkStart w:id="1" w:name="a"/><w:bookmarkStart w:id="1" w:name="b"/><w:bookmarkEnd w:id="1"/></w:p>"#
        ),
        ["id.bookmark_duplicate"]
    );
}

fn comments_xml(inner: &str) -> String {
    format!(r#"<w:comments {NS}>{inner}</w:comments>"#)
}

#[test]
fn comment_ranges_and_references() {
    let c = comments_xml(
        r#"<w:comment w:id="0" w:author="a"><w:p w14:paraId="0000C001"/></w:comment><w:comment w:id="1" w:author="a"><w:p w14:paraId="0000C002"/></w:comment>"#,
    );
    let ok = r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>x</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:r><w:commentReference w:id="1"/></w:r></w:p>"#;
    let d = doc_with(ok, &[("word/comments.xml", COMMENTS_REL, &c)]);
    assert!(check(&d).findings.is_empty(), "{:?}", check(&d).findings);

    let bad = r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:commentRangeEnd w:id="7"/><w:r><w:commentReference w:id="9"/></w:r></w:p>"#;
    let d = doc_with(bad, &[("word/comments.xml", COMMENTS_REL, &c)]);
    let r = check(&d);
    let mut got = codes(&r);
    got.sort();
    assert_eq!(
        got,
        [
            "Error:comment.missing",
            "Error:comment.range_unclosed",
            "Error:comment.range_unopened",
            "Warning:comment.unreferenced"
        ]
    );
    // References without a comments part.
    assert_eq!(errors_of(r#"<w:p><w:r><w:commentReference w:id="0"/></w:r></w:p>"#), ["comment.missing"]);
    // Duplicate comment ids.
    let dup = comments_xml(r#"<w:comment w:id="0"><w:p/></w:comment><w:comment w:id="0"><w:p/></w:comment>"#);
    let d = doc_with(
        r#"<w:p><w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
        &[("word/comments.xml", COMMENTS_REL, &dup)],
    );
    assert_eq!(check(&d).errors().map(|f| f.code.as_str()).collect::<Vec<_>>(), ["id.comment_duplicate"]);
}

#[test]
fn comment_extension_parts() {
    let c = comments_xml(
        r#"<w:comment w:id="0"><w:p w14:paraId="0000C001"/></w:comment><w:comment w:id="1"><w:p w14:paraId="0000C002"/></w:comment>"#,
    );
    let ex = r#"<w15:commentsEx xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml"><w15:commentEx w15:paraId="0000C001" w15:done="0"/><w15:commentEx w15:paraId="0000C002" w15:paraIdParent="0000C009" w15:done="maybe"/></w15:commentsEx>"#;
    let ids = r#"<w16cid:commentsIds xmlns:w16cid="http://schemas.microsoft.com/office/word/2016/wordml/cid"><w16cid:commentId w16cid:paraId="0000C001" w16cid:durableId="9ABCDEF0"/><w16cid:commentId w16cid:paraId="0000C002" w16cid:durableId="1234ABCD"/></w16cid:commentsIds>"#;
    let body = r#"<w:p><w:r><w:commentReference w:id="0"/><w:commentReference w:id="1"/></w:r></w:p>"#;
    let base = "http://schemas.microsoft.com/office/2011/relationships/";
    let (rel_ex, rel_ids) = (
        format!("{base}commentsExtended"),
        "http://schemas.microsoft.com/office/2016/09/relationships/commentsIds".to_owned(),
    );
    let d = doc_with(
        body,
        &[
            ("word/comments.xml", COMMENTS_REL, &c),
            ("word/commentsExtended.xml", &rel_ex, ex),
            ("word/commentsIds.xml", &rel_ids, ids),
        ],
    );
    let mut got: Vec<String> = check(&d).errors().map(|f| f.code.clone()).collect();
    got.sort();
    assert_eq!(got, ["comment.parent_dangling", "id.long_hex_invalid", "value.invalid"]);
}

#[test]
fn tables() {
    assert_eq!(
        errors_of(
            r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p/><w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl></w:tc></w:tr></w:tbl>"#
        ),
        ["table.cell_not_ending_in_p"]
    );
    assert_eq!(
        errors_of(r#"<w:tbl><w:tblGrid><w:gridCol/></w:tblGrid><w:tr><w:tc><w:tcPr/></w:tc></w:tr></w:tbl>"#),
        ["table.cell_without_p"]
    );
    let r = check(&doc(
        r#"<w:tbl><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#,
    ));
    assert_eq!(codes(&r), ["Warning:table.grid_mismatch"]);
    let r = check(&doc(
        r#"<w:tbl><w:tblGrid><w:gridCol/><w:gridCol/><w:gridCol/></w:tblGrid><w:tr><w:trPr><w:gridBefore w:val="1"/></w:trPr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#,
    ));
    assert!(r.findings.is_empty(), "{:?}", r.findings);
}

#[test]
fn relationships_and_ignorable() {
    assert_eq!(
        errors_of(r#"<w:p><w:hyperlink r:id="rId404"><w:r><w:t>x</w:t></w:r></w:hyperlink></w:p>"#),
        ["rel.unresolved"]
    );
    // A locally declared r prefix still counts.
    assert_eq!(
        errors_of(
            r#"<w:p><w:r><w:drawing><a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:embed="rId7"/></w:drawing></w:r></w:p>"#
        ),
        ["rel.unresolved"]
    );
    let mut parts = crate::wml::MemParts::new();
    parts.insert(
        "word/document.xml",
        format!(r#"<w:document {NS} mc:Ignorable="w14 w15"><w:body><w:p/></w:body></w:document>"#).into_bytes(),
    );
    parts.insert(
        "_rels/.rels",
        br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_vec(),
    );
    let d = WordDoc::load(&parts).unwrap();
    assert_eq!(check(&d).errors().map(|f| f.code.as_str()).collect::<Vec<_>>(), ["xml.ignorable_undeclared"]);
}

#[test]
fn whitespace_without_preserve() {
    let r = check(&doc(r#"<w:p><w:r><w:t>trailing </w:t></w:r></w:p>"#));
    assert_eq!(codes(&r), ["Warning:text.space_not_preserved"]);
}

#[test]
fn numbering_order() {
    let num = format!(
        r#"<w:numbering {NS}><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:abstractNum w:abstractNumId="0"/><w:num w:numId="2"><w:abstractNumId w:val="5"/></w:num></w:numbering>"#
    );
    let d = doc_with(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr></w:p>"#,
        &[(
            "word/numbering.xml",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering",
            &num,
        )],
    );
    assert_eq!(
        codes(&check(&d)),
        ["Error:numbering.order", "Warning:numbering.missing_abstract", "Warning:numbering.missing_num"]
    );
}

#[test]
fn anchors_name_the_paragraph() {
    let d = doc(r#"<w:p><w:r><w:t>a</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="nope"/></w:pPr></w:p>"#);
    let a = Anchors::mint(&d);
    let r = check_with(&d, Some(&a));
    let f = &r.findings[0];
    let target = a.handles(0).nth(1).map(|(h, _)| h.to_owned());
    assert_eq!(f.anchor, target);
    assert!(f.anchor.is_some());
    let json = serde_json::to_string(f).unwrap();
    assert!(json.contains(r#""severity":"error""#), "{json}");
}

#[test]
fn modified_parts_are_reparsed() {
    let mut d = doc(P);
    let main = d.main_part();
    let x = d.part_mut(main);
    let root = x.root();
    x.set_attr(root, "w:x", "1").unwrap();
    assert!(check(&d).is_ok());
}

#[test]
fn speed_1000_paragraphs() {
    let mut body = String::new();
    for i in 0..1000u32 {
        body.push_str(&format!(
            r#"<w:p w14:paraId="{:08X}" w14:textId="77777777"><w:pPr><w:pStyle w:val="Normal"/><w:spacing w:after="120"/><w:jc w:val="both"/></w:pPr><w:r><w:rPr><w:b/><w:sz w:val="22"/></w:rPr><w:t xml:space="preserve">Paragraph {i} with </w:t></w:r><w:ins w:id="{}" w:author="a"><w:r><w:t>inserted</w:t></w:r></w:ins><w:bookmarkStart w:id="{i}" w:name="b{i}"/><w:r><w:t>tail.</w:t></w:r><w:bookmarkEnd w:id="{i}"/></w:p>"#,
            i + 1,
            i + 1
        ));
    }
    let d = doc(&body);
    assert!(check(&d).findings.is_empty());
    let runs = 20;
    let t = std::time::Instant::now();
    for _ in 0..runs {
        std::hint::black_box(check(&d));
    }
    let per = t.elapsed() / runs;
    eprintln!("validate: {per:?} per 1000 paragraphs");
    // 5 ms in release; debug builds are ~10x slower.
    let budget = if cfg!(debug_assertions) { 100 } else { 5 };
    assert!(per.as_millis() < budget, "{per:?}");
}
