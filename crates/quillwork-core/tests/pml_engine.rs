//! Public PowerPoint API: package locality, guarded edits, identity and hostile boundaries.
mod opc_common;

use std::collections::BTreeMap;

use opc_common::{E, build_zip, spans};
use quillwork_core::Error;
use quillwork_core::opc::{Limits, Package};
use quillwork_core::pml::Presentation;
use serde_json::{Value, json};

const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn shape(id: u32, body: &str) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Shape {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{body}</p:txBody></p:sp>"#
    )
}

fn deck() -> Vec<u8> {
    build_zip(&entries(), b"synthetic PPTX archive")
}

fn entries() -> Vec<E> {
    let text = shape(
        2,
        r#"<a:p><a:r><a:rPr b="1"/><a:t>Hello </a:t></a:r><a:r><a:rPr i="1"/><a:t>{{cl</a:t></a:r><a:r><a:rPr lang="nl-NL"/><a:t>ient}}</a:t></a:r><a:r><a:rPr u="sng"/><a:t>, welcome.</a:t></a:r></a:p>"#,
    );
    let group = format!(
        r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="3" name="Group"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{}</p:grpSp>"#,
        shape(4, r#"<a:p><a:r><a:t>Grouped text</a:t></a:r></a:p>"#)
    );
    let table = r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="5" name="Table"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr/><a:tblGrid><a:gridCol w="100"/></a:tblGrid><a:tr h="100"><a:tc><a:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:t>Table cell</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#;
    let root = format!(
        r#"<p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{text}{group}{table}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#
    );
    let second = format!(
        r#"<p:sld xmlns:p="{P}" xmlns:a="{A}"><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:sld>"#,
        shape(2, r#"<a:p><a:r><a:t>Second slide</a:t></a:r></a:p>"#)
    );
    let notes = format!(
        r#"<p:notes xmlns:p="{P}" xmlns:a="{A}"><p:cSld><p:spTree>{}{}</p:spTree></p:cSld></p:notes>"#,
        shape(2, r#"<a:p><a:r><a:t>Speaker {{client}}</a:t></a:r></a:p>"#)
            .replace("<p:nvPr/>", "<p:nvPr><p:ph type=\"body\"/></p:nvPr>"),
        shape(3, r#"<a:p><a:r><a:t>23</a:t></a:r></a:p>"#)
            .replace("<p:nvPr/>", "<p:nvPr><p:ph type=\"sldNum\"/></p:nvPr>")
    );
    let ct = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/notesSlides/notesSlide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/></Types>"#;
    vec![
        E::new("[Content_Types].xml",ct.as_bytes()),
        E::new("_rels/.rels",format!(r#"<Relationships xmlns="{REL}"><Relationship Id="main" Type="{R}/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#).as_bytes()),
        E::new("ppt/presentation.xml",format!(r#"<p:presentation xmlns:p="{P}" xmlns:r="{R}"><p:sldIdLst><p:sldId id="256" r:id="s1"/><p:sldId id="300" r:id="s2"/></p:sldIdLst><p:sldSz cx="9144000" cy="5143500"/></p:presentation>"#).as_bytes()),
        E::new("ppt/_rels/presentation.xml.rels",format!(r#"<Relationships xmlns="{REL}"><Relationship Id="s1" Type="{R}/slide" Target="slides/slide1.xml"/><Relationship Id="s2" Type="{R}/slide" Target="slides/slide2.xml"/></Relationships>"#).as_bytes()),
        E::new("ppt/slides/slide1.xml",root.as_bytes()).descriptor(true),
        E::new("ppt/slides/slide2.xml",second.as_bytes()),
        E::new("ppt/slides/_rels/slide1.xml.rels",format!(r#"<Relationships xmlns="{REL}"><Relationship Id="notes" Type="{R}/notesSlide" Target="../notesSlides/notesSlide1.xml"/><Relationship Id="remote" Type="{R}/hyperlink" Target="https://example.invalid/do-not-fetch" TargetMode="External"/></Relationships>"#).as_bytes()),
        E::new("ppt/notesSlides/notesSlide1.xml",notes.as_bytes()),
        E::new("ppt/media/image1.png",b"public opaque fixture image bytes"),
        E::new("ppt/theme/theme1.xml",b"<unknown xmlns='urn:preserve-me' original='yes'/>")
    ]
}

fn rewrite(part: &str, f: impl FnOnce(String) -> String) -> Vec<u8> {
    let mut entries = entries();
    let entry = entries.iter_mut().find(|e| e.name == part.as_bytes()).unwrap();
    entry.data = f(String::from_utf8(entry.data.clone()).unwrap()).into_bytes();
    build_zip(&entries, b"synthetic PPTX archive")
}

fn apply(p: &mut Presentation, op: Value) -> Value {
    p.apply_tx(&json!({"base_version":p.version(),"base_sha256":p.sha256(),"ops":[op]})).unwrap()
}
fn code(error: Error) -> &'static str {
    if let Error::Edit { code, .. } = error { code } else { panic!("unexpected error: {error}") }
}

#[test]
fn exact_round_trip_and_native_anchors() {
    let bytes = deck();
    let p = Presentation::open(bytes.clone()).unwrap();
    assert_eq!(p.save().unwrap(), bytes);
    assert_eq!(p.version(), 1);
    let vm = p.describe().unwrap();
    assert_eq!(vm["slides"][0]["id"], "s256");
    assert_eq!(vm["slides"][1]["shapes"][0]["id"], "s300.sh2");
    assert!(p.read().contains("[s256.sh4.p1] \"Grouped text\""));
    assert!(p.read().contains("[s256.sh5.r1.c1.p1] \"Table cell\""));
    assert!(p.read().contains("[s256.n.sh2.p1 NOTES] \"Speaker {{client}}\""));
    assert!(!p.read().contains("s256.n.sh3"));
    assert_eq!(vm["external_relationships"].as_array().unwrap().len(), 1);
    assert!(p.review().unwrap()["changes"].as_array().unwrap().is_empty());
}

#[test]
fn opaque_graphic_metadata_tables_are_neither_projected_nor_editable() {
    for (wrapper, extension) in [
        ("urn:opaque-business-data", false),
        ("urn:opaque-business-data", true),
        ("http://schemas.openxmlformats.org/drawingml/2006/table", true),
    ] {
        let bytes = rewrite("ppt/slides/slide1.xml", |s| {
            s.replace(
                r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table">"#,
                &format!(
                    r#"<a:graphicData uri="{wrapper}">{}"#,
                    if extension { r#"<a:extLst><a:ext uri="urn:opaque">"# } else { "" }
                ),
            )
            .replace(
                "</a:graphicData>",
                if extension { "</a:ext></a:extLst></a:graphicData>" } else { "</a:graphicData>" },
            )
        });
        let mut p = Presentation::open(bytes.clone()).unwrap();
        assert!(!p.read().contains("Table cell"), "extension metadata is not slide text");
        let error = p
            .apply_tx(&json!({"base_version":1, "ops":[{
                "op":"replace_text", "anchor":"s256.sh5.r1.c1.p1", "find":"Table cell", "replace":"changed"
            }]}))
            .unwrap_err();
        assert_eq!(code(error), "NOT_FOUND");
        assert_eq!(p.save().unwrap(), bytes);
    }
    let bytes = rewrite("ppt/slides/slide1.xml", |s| {
        s.replace("</p:graphicFrame>", r#"<p:extLst><p:ext uri="urn:opaque"><a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>Opaque extension table</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></p:ext></p:extLst></p:graphicFrame>"#)
    });
    let mut p = Presentation::open(bytes.clone()).unwrap();
    assert!(!p.read().contains("Opaque extension table"));
    apply(
        &mut p,
        json!({"op":"replace_text", "anchor":"s256.sh5.r1.c1.p1", "find":"Table cell", "replace":"Real table edit"}),
    );
    let package = Package::open(p.save().unwrap(), &Limits::default()).unwrap();
    assert!(package.part_str("ppt/slides/slide1.xml").unwrap().contains("<a:t>Opaque extension table</a:t>"));
}

#[test]
fn foreign_nonvisual_metadata_cannot_replace_native_shape_identity() {
    let bytes = rewrite("ppt/slides/slide1.xml", |s| {
        s.replace(
            r#"<p:nvSpPr><p:cNvPr id="2""#,
            r#"<x:nvSpPr xmlns:x="urn:opaque"><p:cNvPr id="999" name="Opaque identity"/><p:nvPr><p:ph type="sldNum"/></p:nvPr></x:nvSpPr><p:nvSpPr><p:cNvPr id="2""#,
        )
    });
    let mut p = Presentation::open(bytes).unwrap();
    assert!(p.read().contains("[s256.sh2.p1]"));
    assert!(!p.read().contains("s256.sh999"));
    apply(&mut p, json!({"op":"replace_text", "anchor":"s256.sh2.p1", "find":"{{client}}", "replace":"Acme"}));
    assert!(p.read().contains("Hello Acme, welcome."));
}

#[test]
fn cross_run_replace_keeps_affixes_run_properties_and_raw_zip_entries() {
    let bytes = deck();
    let mut p = Presentation::open(bytes.clone()).unwrap();
    apply(
        &mut p,
        json!({"op":"replace_text","anchor":"s256.sh2.p1","find":"{{client}}","replace":"Zoë & 李","expect":"Hello {{client}}, welcome."}),
    );
    assert!(p.read().contains("Hello Zoë & 李, welcome."));
    let after = p.save().unwrap();
    let a: BTreeMap<_, _> = spans(&bytes).into_iter().collect();
    let b: BTreeMap<_, _> = spans(&after).into_iter().collect();
    for (name, raw) in a {
        if name != "ppt/slides/slide1.xml" {
            assert_eq!(raw, b[&name], "changed unrelated raw entry {name}");
        }
    }
    let pkg = Package::open(after.clone(), &Limits::default()).unwrap();
    let xml = pkg.part_str("ppt/slides/slide1.xml").unwrap();
    assert!(xml.contains(r#"<a:r><a:rPr b="1"/><a:t>Hello </a:t></a:r>"#));
    assert!(xml.contains(r#"<a:rPr i="1"/><a:t>Zoë &amp; 李</a:t>"#));
    assert!(xml.contains(r#"<a:r><a:rPr u="sng"/><a:t>, welcome.</a:t></a:r>"#));
    let reopened = Presentation::open(after).unwrap();
    assert!(reopened.read().contains("Hello Zoë & 李, welcome."));
    assert_eq!(p.review().unwrap()["changes"][0]["before"], "Hello {{client}}, welcome.");
}

#[test]
fn notes_table_and_group_edits_target_only_their_existing_parts() {
    let mut p = Presentation::open(deck()).unwrap();
    for (anchor, find, replace) in [
        ("s256.sh4.p1", "Grouped text", "Team"),
        ("s256.sh5.r1.c1.p1", "Table cell", "EUR 500"),
        ("s256.n.sh2.p1", "{{client}}", "Acme"),
    ] {
        apply(&mut p, json!({"op":"replace_text","anchor":anchor,"find":find,"replace":replace}));
    }
    let text = Presentation::open(p.save().unwrap()).unwrap().read();
    assert!(text.contains("Team") && text.contains("EUR 500") && text.contains("Speaker Acme"));
    assert_eq!(p.review().unwrap()["changes"].as_array().unwrap().len(), 3);
}

#[test]
fn failed_transaction_is_atomic_and_stale_versions_and_digests_refused() {
    let bytes = deck();
    let mut p = Presentation::open(bytes.clone()).unwrap();
    let hash = p.sha256().to_owned();
    let error=p.apply_tx(&json!({"base_version":1,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"Grouped","replace":"Changed"},{"op":"replace_text","anchor":"missing","find":"x","replace":"y"}]})).unwrap_err();
    assert_eq!(code(error), "NOT_FOUND");
    assert_eq!(p.save().unwrap(), bytes);
    assert_eq!(p.version(), 1);
    assert_eq!(p.sha256(), hash);
    apply(&mut p, json!({"op":"replace_text","anchor":"s256.sh4.p1","find":"Grouped","replace":"Changed"}));
    assert_eq!(code(p.apply_tx(&json!({"base_version":1,"ops":[{}]})).unwrap_err()), "CONFLICT");
    let mut reopened = Presentation::open(p.save().unwrap()).unwrap();
    assert_eq!(reopened.version(), 1);
    assert_eq!(
        code(reopened.apply_tx(&json!({"base_version":1,"base_sha256":hash,"ops":[{}]})).unwrap_err()),
        "CONFLICT"
    );
}

#[test]
fn strict_requests_unique_matches_and_explicit_occurrence() {
    let bytes = rewrite("ppt/slides/slide1.xml", |s| s.replace("Grouped text", "same same"));
    let mut p = Presentation::open(bytes).unwrap();
    let op = json!({"op":"replace_text","anchor":"s256.sh4.p1","find":"same","replace":"two"});
    assert_eq!(code(p.apply_tx(&json!({"base_version":1,"ops":[op.clone()]})).unwrap_err()), "AMBIGUOUS");
    let mut op = op;
    op["occurrence"] = json!(2);
    apply(&mut p, op);
    assert!(p.read().contains("same two"));
    for tx in [
        json!({"base_version":2,"mode":"track","ops":[{}]}),
        json!({"base_version":2,"spec_version":"99.0.0","ops":[{}]}),
        json!({"base_version":2,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"same","replace":"x","typo":true}]}),
    ] {
        assert_eq!(code(p.apply_tx(&tx).unwrap_err()), "INVALID_OP");
    }
    assert_eq!(code(p.apply_tx(&json!({"base_version":2,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"same","replace":"x","expect":"wrong"}]})).unwrap_err()),"STALE_ANCHOR");
}

#[test]
fn fields_and_breaks_are_readable_but_not_mutated() {
    let bytes = rewrite("ppt/slides/slide1.xml", |s| {
        s.replace("<a:t>Grouped text</a:t></a:r>","<a:t>Grouped</a:t></a:r><a:br/><a:fld id=\"{00000000-0000-0000-0000-000000000001}\" type=\"slidenum\"><a:t>9</a:t></a:fld>")
    });
    let mut p = Presentation::open(bytes.clone()).unwrap();
    assert!(p.read().contains("Grouped\\n9"));
    assert_eq!(code(p.apply_tx(&json!({"base_version":1,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"Grouped","replace":"Edit"}]})).unwrap_err()),"UNSUPPORTED");
    assert_eq!(p.save().unwrap(), bytes);
}

#[test]
fn reordered_slides_keep_native_text_anchors() {
    let bytes = rewrite("ppt/presentation.xml", |s| {
        s.replace(
            "<p:sldId id=\"256\" r:id=\"s1\"/><p:sldId id=\"300\" r:id=\"s2\"/>",
            "<p:sldId id=\"300\" r:id=\"s2\"/><p:sldId id=\"256\" r:id=\"s1\"/>",
        )
    });
    let p = Presentation::open(bytes).unwrap();
    assert_eq!(p.describe().unwrap()["slides"][0]["id"], "s300");
    assert!(p.read().contains("[s256.sh4.p1]"));
}

#[test]
fn invalid_or_ambiguous_package_identities_are_refused() {
    for bytes in [
        rewrite("ppt/presentation.xml", |s| s.replace("id=\"300\"", "id=\"256\"")),
        rewrite("ppt/slides/slide1.xml", |s| s.replace("id=\"4\"", "id=\"2\"")),
        rewrite("ppt/slides/slide1.xml", |s| s.replace("<p:sld ", "<p:notes ").replace("</p:sld>", "</p:notes>")),
        rewrite("ppt/_rels/presentation.xml.rels", |s| {
            s.replace(
                "Target=\"slides/slide1.xml\"",
                "Target=\"https://example.invalid/slide\" TargetMode=\"External\"",
            )
        }),
    ] {
        assert!(Presentation::open(bytes).is_err());
    }
    assert!(Presentation::open(opc_common::minimal_docx()).is_err());
}

#[test]
fn signatures_macros_and_write_protection_are_refused() {
    let mut signed = entries();
    signed.push(E::new("_xmlsignatures/sig1.xml", b"<opaque/>"));
    assert_eq!(code(Presentation::open(build_zip(&signed, b"")).err().unwrap()), "PROTECTED");
    let mut macros = entries();
    macros.push(E::new("ppt/vbaProject.bin", b"not executed"));
    assert_eq!(code(Presentation::open(build_zip(&macros, b"")).err().unwrap()), "UNSUPPORTED");
    let bytes =
        rewrite("ppt/presentation.xml", |s| s.replace("</p:presentation>", "<p:modifyVerifier/></p:presentation>"));
    assert_eq!(code(Presentation::open(bytes).err().unwrap()), "PROTECTED");
}

#[test]
fn explicit_visibility_signals_and_hidden_groups_are_labelled() {
    let bytes = rewrite("ppt/slides/slide1.xml", |s| {
        s.replace("id=\"3\" name=\"Group\"", "id=\"3\" name=\"Group\" hidden=\"1\"")
            .replace("<a:t>Grouped text</a:t>", "<a:rPr sz=\"100\"><a:solidFill><a:srgbClr val=\"FFFFFF\"><a:alpha val=\"0\"/></a:srgbClr></a:solidFill></a:rPr><a:t>Grouped\u{200B} text</a:t>")
    });
    let p = Presentation::open(bytes).unwrap();
    let vm = p.describe().unwrap();
    let group_child = vm["slides"][0]["shapes"].as_array().unwrap().iter().find(|s| s["shape_id"] == 4).unwrap();
    assert_eq!(group_child["hidden"], true);
    assert_eq!(
        group_child["paragraphs"][0]["visibility_warnings"],
        json!(["tiny_text", "white_text", "transparent_text", "zero_width_text"])
    );
    assert!(p.read().contains("HIDDEN VISIBILITY_WARNING=tiny_text,white_text,transparent_text,zero_width_text"));
    assert_eq!(vm["capabilities"]["inherited_visibility_resolved"], false);
}

#[test]
fn invalid_text_and_request_limits_leave_candidate_unchanged() {
    let bytes = deck();
    let mut p = Presentation::open(bytes.clone()).unwrap();
    let bad = json!({"base_version":1,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"Grouped","replace":"bad\u{0001}"}]});
    assert!(p.apply_tx(&bad).is_err());
    assert_eq!(p.save().unwrap(), bytes);
    assert_eq!(p.version(), 1);
    let bad = json!({"base_version":1,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"Grouped","replace":"x".repeat(1_000_001)}]});
    assert_eq!(code(p.apply_tx(&bad).unwrap_err()), "LIMIT");
    assert_eq!(p.save().unwrap(), bytes);
    let bad = json!({"base_version":1,"ops":vec![json!({});201]});
    assert_eq!(code(p.apply_tx(&bad).unwrap_err()), "LIMIT");
    assert_eq!(p.save().unwrap(), bytes);
}

#[test]
fn extension_metadata_text_never_shifts_visible_edit_offsets() {
    let metadata = r#"<a:extLst><a:ext uri="urn:synthetic-metadata"><a:t>SECRET</a:t></a:ext></a:extLst>"#;
    let bytes = rewrite("ppt/slides/slide1.xml", |s| {
        s.replace("<a:rPr b=\"1\"/>", &format!("<a:rPr b=\"1\">{metadata}</a:rPr>"))
    });
    let mut p = Presentation::open(bytes.clone()).unwrap();
    let expected = "Hello {{client}}, welcome.";
    assert_eq!(p.describe().unwrap()["slides"][0]["shapes"][0]["paragraphs"][0]["text"], expected);
    assert!(!p.read().contains("SECRET"));
    let error = p.apply_tx(&json!({"base_version":1,"ops":[{"op":"replace_text","anchor":"s256.sh2.p1","find":"SECRET","replace":"XXXXXX","expect":expected}]})).unwrap_err();
    assert_eq!(code(error), "NOT_FOUND");
    assert_eq!(p.save().unwrap(), bytes);
    apply(
        &mut p,
        json!({"op":"replace_text","anchor":"s256.sh2.p1","find":"{{client}}","replace":"Acme","expect":expected}),
    );
    let pkg = Package::open(p.save().unwrap(), &Limits::default()).unwrap();
    let xml = pkg.part_str("ppt/slides/slide1.xml").unwrap();
    assert!(xml.contains(metadata), "unrelated metadata must remain byte-identical");
    assert!(p.read().contains("Hello Acme, welcome."));
    // Fields are read-only, but their displayed result must use the same direct-child rule.
    let field_bytes = rewrite("ppt/slides/slide1.xml", |s| {
        s.replace("<a:t>Grouped text</a:t></a:r>", &format!("<a:t>Grouped</a:t></a:r><a:fld id=\"id\" type=\"slidenum\"><a:rPr>{metadata}</a:rPr><a:t>9</a:t></a:fld>"))
    });
    let field = Presentation::open(field_bytes).unwrap();
    assert!(field.read().contains("Grouped9"));
    assert!(!field.read().contains("SECRET"));
}

#[test]
fn custom_package_limits_remain_enforced_on_atomic_mutation() {
    let bytes = deck();
    let original = entries();
    let largest = original.iter().map(|e| e.data.len() as u64).max().unwrap();
    let total: u64 = original.iter().map(|e| e.data.len() as u64).sum();
    for limits in [
        Limits { max_part_size: largest + 10, ..Limits::default() },
        Limits { max_total_uncompressed: total + 10, ..Limits::default() },
    ] {
        let mut p = Presentation::open_with_limits(bytes.clone(), &limits).unwrap();
        let hash = p.sha256().to_owned();
        let error = p.apply_tx(&json!({"base_version":1,"ops":[{"op":"replace_text","anchor":"s256.sh4.p1","find":"Grouped text","replace":"x".repeat(10_000)}]})).unwrap_err();
        assert!(matches!(error, Error::Limit(_)), "expected retained package size limit: {error}");
        assert_eq!(p.save().unwrap(), bytes);
        assert_eq!(p.version(), 1);
        assert_eq!(p.sha256(), hash);
    }
}

#[test]
fn permissive_custom_limits_are_not_overridden_by_defaults() {
    let bytes = rewrite("ppt/slides/slide1.xml", |s| s.replace("Grouped text", &"z".repeat(1_100_000)));
    assert!(matches!(Presentation::open(bytes.clone()), Err(Error::Limit(_))));
    let limits = Limits { max_ratio: 10_000, ..Limits::default() };
    let p = Presentation::open_with_limits(bytes.clone(), &limits).unwrap();
    assert_eq!(p.save().unwrap(), bytes);
}
