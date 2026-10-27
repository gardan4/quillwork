//! Behaviour of the package API on hand-built packages.

mod opc_common;

use opc_common::*;
use quillwork_core::Error;
use quillwork_core::opc::{Limits, Package};

const IMAGE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const STYLES_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
const STYLES_CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";

fn open(b: Vec<u8>) -> Package {
    Package::open(b, &Limits::default()).unwrap()
}

/// Central-directory records by name, with the local-header offset zeroed.
fn cd_records(b: &[u8]) -> Vec<(String, Vec<u8>)> {
    let eocd = (0..=b.len() - 22).rev().find(|&p| b[p..p + 4] == [0x50, 0x4b, 0x05, 0x06]).unwrap();
    let n = u16::from_le_bytes([b[eocd + 10], b[eocd + 11]]) as usize;
    let mut p = u32::from_le_bytes(b[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    for _ in 0..n {
        let v = |o: usize| u16::from_le_bytes([b[p + o], b[p + o + 1]]) as usize;
        let len = 46 + v(28) + v(30) + v(32);
        let mut rec = b[p..p + len].to_vec();
        rec[42..46].fill(0);
        out.push((String::from_utf8_lossy(&b[p + 46..p + 46 + v(28)]).into_owned(), rec));
        p += len;
    }
    out
}

fn archive_comment(b: &[u8]) -> &[u8] {
    let eocd = (0..=b.len() - 22).rev().find(|&p| b[p..p + 4] == [0x50, 0x4b, 0x05, 0x06]).unwrap();
    &b[eocd + 22..]
}

fn assert_only_insertion(before: &[u8], after: &[u8]) {
    let pre = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let suf = before[pre..].iter().rev().zip(after[pre..].iter().rev()).take_while(|(a, b)| a == b).count();
    assert_eq!(pre + suf, before.len(), "changed beyond an insertion");
}

fn verify(out: &[u8], n: usize) {
    assert_eq!(check_with_zip_crate(out).unwrap(), n);
    if let Some(r) = check_with_python(out, "api") {
        assert_eq!(r.unwrap(), n);
    }
}

/// Entries with every preservable quirk: descriptors with and without signature, extras,
/// comments, Unix made-by, external attributes.
fn quirky() -> Vec<u8> {
    let mut es = minimal_entries();
    es[1] = es[1].clone().descriptor(true);
    es[3] = es[3].clone().descriptor(false);
    es[4].cd_extra = vec![0x55, 0x54, 5, 0, 1, 0x10, 0x20, 0x30, 0x40];
    es[4].local_extra = vec![0x55, 0x54, 5, 0, 1, 0x10, 0x20, 0x30, 0x40];
    es[4].comment = b"entry comment".to_vec();
    es[4].made_by = 0x031E;
    es[4].ext_attr = 0o100644 << 16;
    es[2].comment = b"doc comment".to_vec();
    es[2].made_by = 0x0A2D;
    build_zip(&es, b"archive comment")
}

#[test]
fn edit_preserves_everything_else() {
    let input = quirky();
    let mut p = open(input.clone());
    assert!(p.entry_info("_rels/.rels").unwrap().has_data_descriptor);
    assert_eq!(p.save().unwrap(), input);

    let mut doc = p.part("word/document.xml").unwrap().into_owned();
    doc.extend_from_slice(b"<!-- edit -->");
    p.set_part("word/document.xml", doc.clone()).unwrap();
    let out = p.save().unwrap();

    let (a, b) = (spans(&input), spans(&out));
    for ((n, x), (_, y)) in a.iter().zip(&b) {
        if n != "word/document.xml" {
            assert_eq!(x, y, "{n} local header/data/descriptor changed");
        }
    }
    let (a, b) = (cd_records(&input), cd_records(&out));
    for ((n, x), (_, y)) in a.iter().zip(&b) {
        if n != "word/document.xml" {
            assert_eq!(x, y, "{n} central record changed");
        }
    }
    // The dirty entry keeps its made-by, comment and time/date.
    let (old, new) = (&a[2].1, &b[2].1);
    assert_eq!(old[4..6], new[4..6], "made-by");
    assert_eq!(old[12..16], new[12..16], "time/date");
    assert!(new.ends_with(b"doc comment"));
    assert_eq!(archive_comment(&out), b"archive comment");

    let re = open(out.clone());
    assert_eq!(&*re.part("word/document.xml").unwrap(), &doc[..]);
    verify(&out, 5);
}

#[test]
fn dirtying_a_descriptor_entry_drops_the_descriptor() {
    let input = quirky();
    let mut p = open(input);
    let mut r = p.part("_rels/.rels").unwrap().into_owned();
    r.extend_from_slice(b"\n");
    p.set_part("_rels/.rels", r.clone()).unwrap();
    let out = p.save().unwrap();
    let re = open(out.clone());
    let info = re.entry_info("_rels/.rels").unwrap();
    assert!(!info.has_data_descriptor);
    assert_eq!(info.flags & 8, 0);
    assert_eq!(&*re.part("_rels/.rels").unwrap(), &r[..]);
    // The other descriptor entry is untouched.
    assert!(re.entry_info("word/_rels/document.xml.rels").unwrap().has_data_descriptor);
    verify(&out, 5);
}

#[test]
fn stored_stays_stored_and_deflated_stays_deflated() {
    let mut p = open(minimal_docx());
    p.set_part("word/media/image1.png", b"new png bytes".to_vec()).unwrap();
    p.set_part("word/document.xml", DOC_XML.replace("Hello", "Bye").into_bytes()).unwrap();
    let re = open(p.save().unwrap());
    assert_eq!(re.entry_info("word/media/image1.png").unwrap().method, 0);
    assert_eq!(re.entry_info("word/document.xml").unwrap().method, 8);
    assert_eq!(&*re.part("word/media/image1.png").unwrap(), b"new png bytes");
}

#[test]
fn level_guess_reproduces_zlib_output() {
    // Varied XML big enough that zlib levels 1, 6 and 9 produce different streams.
    let mut body = String::new();
    for i in 0..3000u32 {
        body.push_str(&format!("<w:p><w:r><w:t>para {i} {}</w:t></w:r></w:p>", i.wrapping_mul(2_654_435_761) % 9973));
    }
    let xml = DOC_XML.replace("<w:body>", &format!("<w:body>{body}"));
    assert_ne!(deflate(xml.as_bytes(), 1), deflate(xml.as_bytes(), 9));
    for level in [1u32, 6, 9] {
        let mut es = minimal_entries();
        es[2] = E::new("word/document.xml", xml.as_bytes());
        es[2].level = level;
        let mut p = open(build_zip(&es, b""));
        let edited = xml.replace("para 17 ", "para seventeen ").into_bytes();
        p.set_part("word/document.xml", edited.clone()).unwrap();
        let out = p.save().unwrap();
        let span = spans(&out).into_iter().find(|(n, _)| n == "word/document.xml").unwrap().1;
        let want = deflate(&edited, level);
        assert!(span.ends_with(&want), "level {level} not reproduced");
    }
}

#[test]
fn reverting_an_edit_restores_the_original() {
    let input = minimal_docx();
    let mut p = open(input.clone());
    let orig = p.part("word/document.xml").unwrap().into_owned();
    p.set_part("word/document.xml", b"<x/>".to_vec()).unwrap();
    assert!(p.is_modified());
    assert!(p.entry_info("word/document.xml").unwrap().dirty);
    p.set_part("word/document.xml", orig).unwrap();
    assert!(!p.is_modified());
    assert_eq!(p.save().unwrap(), input);
}

#[test]
fn lookups_are_case_insensitive_and_slash_tolerant() {
    let p = open(minimal_docx());
    assert!(p.contains("/word/document.xml"));
    assert!(p.contains("WORD/Document.xml"));
    assert_eq!(p.part_str("/word/document.xml").unwrap(), DOC_XML);
    assert!(matches!(p.part("word/nope.xml"), Err(Error::NotFound(_))));
    assert_eq!(
        p.content_type_of("/WORD/document.xml"),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml")
    );
    assert_eq!(p.content_type_of("word/_rels/document.xml.rels"), Some(quillwork_core::opc::RELS_CONTENT_TYPE));
    let info = p.entry_info("word/media/image1.png").unwrap();
    assert_eq!((info.method, info.uncompressed_size, info.added, info.dirty), (0, PNG.len() as u64, false, false));
}

#[test]
fn add_and_remove_parts() {
    let input = minimal_docx();
    let mut p = open(input.clone());
    let ct0 = p.part("[Content_Types].xml").unwrap().into_owned();

    // Default for .xml is application/xml, so a styles part needs an Override.
    p.add_part("/word/styles.xml", b"<w:styles/>".to_vec(), STYLES_CT).unwrap();
    // A part whose Default already matches needs none.
    p.add_part("word/plain.xml", b"<a/>".to_vec(), "application/xml").unwrap();
    let ct1 = p.part("[Content_Types].xml").unwrap().into_owned();
    assert_only_insertion(&ct0, &ct1);
    assert_eq!(p.content_types().overrides().len(), 2);
    assert_eq!(p.content_type_of("word/styles.xml"), Some(STYLES_CT));
    assert_eq!(p.names().last(), Some("word/plain.xml"));
    assert!(p.entry_info("word/styles.xml").unwrap().added);

    assert!(matches!(p.add_part("word/STYLES.xml", vec![], STYLES_CT), Err(Error::Package(_))));
    assert!(matches!(p.add_part("word/x.bin", vec![], ""), Err(Error::Package(_))));
    assert!(matches!(p.add_part("../x.xml", vec![], STYLES_CT), Err(Error::Package(_))));
    assert!(matches!(p.set_part("word/missing.xml", vec![]), Err(Error::NotFound(_))));
    assert!(p.remove_part("[Content_Types].xml").is_err());

    let out = p.save().unwrap();
    let names: Vec<String> = open(out.clone()).names().map(str::to_owned).collect();
    assert_eq!(
        names[..5],
        [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/_rels/document.xml.rels",
            "word/media/image1.png"
        ]
    );
    assert_eq!(names[5..], ["word/styles.xml", "word/plain.xml"]);
    verify(&out, 7);

    // Removing the added parts restores the content types and, with them, the original file.
    p.remove_part("word/styles.xml").unwrap();
    p.remove_part("word/plain.xml").unwrap();
    assert_eq!(p.part("[Content_Types].xml").unwrap().as_ref(), &ct0[..]);
    assert!(!p.is_modified());
    assert_eq!(p.save().unwrap(), input);

    // Removing an original part.
    p.remove_part("word/media/image1.png").unwrap();
    assert!(p.is_modified());
    let out = p.save().unwrap();
    assert!(!open(out.clone()).contains("word/media/image1.png"));
    verify(&out, 4);
}

#[test]
fn register_override_replaces_and_default_conflicts() {
    let mut p = open(minimal_docx());
    p.register_override("word/document.xml", "application/x-test").unwrap();
    assert_eq!(p.content_type_of("word/document.xml"), Some("application/x-test"));
    assert_eq!(p.content_types().overrides().len(), 1);
    p.register_default("png", "image/png").unwrap();
    p.register_default("png", "image/png").unwrap();
    assert!(p.register_default("xml", "text/xml").is_err());
    assert_eq!(p.content_type_of("word/media/image1.png"), Some("image/png"));
}

#[test]
fn relationships() {
    let mut p = open(minimal_docx());
    let rels = p.rels_of("word/document.xml").unwrap();
    assert_eq!(rels.len(), 2);
    assert_eq!(p.resolve("word/document.xml", &rels[0]).as_deref(), Some("word/media/image1.png"));
    assert!(rels[1].external);
    assert_eq!(rels[1].target, "https://example.com/x");
    assert_eq!(p.resolve("word/document.xml", &rels[1]), None);
    let pkg = p.rels_of("").unwrap();
    assert_eq!(p.resolve("", &pkg[0]).as_deref(), Some("word/document.xml"));

    let before = p.part("word/_rels/document.xml.rels").unwrap().into_owned();
    p.add_part("word/styles.xml", b"<w:styles/>".to_vec(), STYLES_CT).unwrap();
    let a = p.add_relationship_to_part("word/document.xml", STYLES_REL, "word/styles.xml").unwrap();
    let b = p.add_relationship("word/document.xml", "urn:x-link", "https://example.org/?a=1&b=\"2\"", true).unwrap();
    assert_eq!((a.as_str(), b.as_str()), ("rIdQw1", "rIdQw2"));
    assert_only_insertion(&before, &p.part("word/_rels/document.xml.rels").unwrap());
    let rels = p.rels_of("word/document.xml").unwrap();
    assert_eq!(rels[2].target, "styles.xml");
    assert_eq!(p.resolve("word/document.xml", &rels[2]).as_deref(), Some("word/styles.xml"));
    assert_eq!(rels[3].target, "https://example.org/?a=1&b=\"2\"");
    assert!(rels[3].external);

    // A source without a rels part gets a fresh one; ids restart per rels part.
    let c = p.add_relationship_to_part("word/styles.xml", IMAGE_REL, "word/media/image1.png").unwrap();
    assert_eq!(c, "rIdQw1");
    let r = p.rels_of("word/styles.xml").unwrap();
    assert_eq!(r[0].target, "media/image1.png");
    assert!(p.contains("word/_rels/styles.xml.rels"));
    assert!(p.add_relationship("word/missing.xml", IMAGE_REL, "x", false).is_err());

    let out = p.save().unwrap();
    let re = open(out.clone());
    assert_eq!(re.rels_of("word/document.xml").unwrap(), p.rels_of("word/document.xml").unwrap());
    verify(&out, 7);
}

#[test]
fn self_closing_rels_root() {
    let mut es = minimal_entries();
    es[3] = E::new(
        "word/_rels/document.xml.rels",
        br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
    );
    let mut p = open(build_zip(&es, b""));
    assert!(p.rels_of("word/document.xml").unwrap().is_empty());
    p.add_relationship_to_part("word/document.xml", IMAGE_REL, "word/media/image1.png").unwrap();
    let r = p.rels_of("word/document.xml").unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(p.resolve("word/document.xml", &r[0]).as_deref(), Some("word/media/image1.png"));
}

#[test]
fn media_dedupe() {
    let mut p = open(minimal_docx());
    assert_eq!(p.media_dir(), "word/media/");
    // Identical bytes to an existing original part.
    assert_eq!(p.add_media(PNG.to_vec(), "png").unwrap(), "word/media/image1.png");
    assert!(!p.is_modified());
    let ct0 = p.part("[Content_Types].xml").unwrap().into_owned();
    let a = p.add_media(b"another image".to_vec(), "PNG").unwrap();
    assert_eq!(a, "word/media/imageQw1.png");
    assert_eq!(p.add_media(b"another image".to_vec(), ".png").unwrap(), a);
    // Same size, different bytes: a new part.
    let b = p.add_media(b"another imagf".to_vec(), "png").unwrap();
    assert_eq!(b, "word/media/imageQw2.png");
    let c = p.add_media(b"jpeg bytes".to_vec(), "jpg").unwrap();
    assert_eq!(c, "word/media/imageQw1.jpg");
    assert_eq!(p.content_type_of(&c), Some("image/jpeg"));
    assert_eq!(p.content_type_of(&a), Some("image/png"));
    assert_only_insertion(&ct0, &p.part("[Content_Types].xml").unwrap());
    assert!(p.add_media(vec![1], "exe").is_err());
}

#[test]
fn gc_removes_only_unreferenced_parts_added_this_session() {
    // An original part nothing points at must survive.
    let mut es = minimal_entries();
    es.push(E::new("word/unreferenced.xml", b"<a/>"));
    let mut p = open(build_zip(&es, b""));

    let kept = p.add_media(b"kept".to_vec(), "png").unwrap();
    p.add_relationship_to_part("word/document.xml", IMAGE_REL, &kept).unwrap();
    let lone = p.add_media(b"lone".to_vec(), "png").unwrap();
    // A chain: an unreferenced added part whose own rels are the only path to another one.
    p.add_part("word/chain.xml", b"<c/>".to_vec(), STYLES_CT).unwrap();
    let leaf = p.add_media(b"leaf".to_vec(), "png").unwrap();
    p.add_relationship_to_part("word/chain.xml", IMAGE_REL, &leaf).unwrap();

    let mut gone = p.gc_orphans().unwrap();
    gone.sort();
    let mut want = vec![lone.clone(), "word/chain.xml".into(), "word/_rels/chain.xml.rels".into(), leaf.clone()];
    want.sort();
    assert_eq!(gone, want);
    assert!(p.contains(&kept));
    assert!(p.contains("word/unreferenced.xml"));
    assert_eq!(p.content_types().override_for("word/chain.xml"), None);
    assert!(p.gc_orphans().unwrap().is_empty());
    verify(&p.save().unwrap(), 7);
}

#[test]
fn content_types_edit_is_reparsed() {
    let mut p = open(minimal_docx());
    let ct = p.part_str("[Content_Types].xml").unwrap().replace("application/xml", "text/xml");
    p.set_part("[Content_Types].xml", ct.into_bytes()).unwrap();
    assert_eq!(p.content_types().default_for("xml"), Some("text/xml"));
    assert!(p.set_part("[Content_Types].xml", b"<Nope/>".to_vec()).is_err());
}
