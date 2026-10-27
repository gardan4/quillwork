//! Hostile-input tests: every one must be rejected with a typed error, never a panic.

mod opc_common;

use opc_common::*;
use quillwork_core::Error;
use quillwork_core::opc::{Limits, Package};

fn open(b: Vec<u8>) -> Result<Package, Error> {
    Package::open(b, &Limits::default())
}

fn with(extra: E) -> Vec<u8> {
    let mut es = minimal_entries();
    es.push(extra);
    build_zip(&es, b"")
}

/// Offsets of every central-directory record and local header, in order.
fn layout(b: &[u8]) -> (Vec<usize>, Vec<usize>, usize) {
    let eocd = (0..=b.len() - 22).rev().find(|&p| b[p..p + 4] == [0x50, 0x4b, 0x05, 0x06]).unwrap();
    let n = u16::from_le_bytes([b[eocd + 10], b[eocd + 11]]) as usize;
    let mut p = u32::from_le_bytes(b[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let (mut cds, mut locals) = (vec![], vec![]);
    for _ in 0..n {
        cds.push(p);
        locals.push(u32::from_le_bytes(b[p + 42..p + 46].try_into().unwrap()) as usize);
        let v = |o: usize| u16::from_le_bytes([b[p + o], b[p + o + 1]]) as usize;
        p += 46 + v(28) + v(30) + v(32);
    }
    (cds, locals, eocd)
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn is_limit(r: &Result<Package, Error>) -> bool {
    matches!(r, Err(Error::Limit(_)))
}
fn is_package(r: &Result<Package, Error>) -> bool {
    matches!(r, Err(Error::Package(_)))
}

#[test]
fn high_ratio_entry_is_rejected() {
    // 10 MiB of zeros deflates to ~10 KiB: ratio ~1000:1.
    let r = open(with(E::new("word/bomb.xml", &vec![0u8; 10 << 20])));
    assert!(is_limit(&r), "{:?}", r.err());
}

#[test]
fn understated_size_is_caught_while_inflating() {
    // Declare 100 bytes uncompressed but ship 10 MiB of deflated zeros (ratio check passes
    // on the declared size). Inflation must stop at the declared size, not expand the bomb.
    let mut b = with(E::new("word/bomb.xml", &vec![0u8; 10 << 20]));
    let (cds, locals, _) = layout(&b);
    let i = 5;
    put32(&mut b, cds[i] + 24, 100);
    put32(&mut b, locals[i] + 22, 100);
    match open(b) {
        Ok(p) => assert!(p.part("word/bomb.xml").is_err()),
        Err(e) => assert!(matches!(e, Error::Limit(_) | Error::Package(_)), "{e:?}"),
    }
}

#[test]
fn total_and_part_size_limits() {
    let b = minimal_docx();
    let l = Limits { max_total_uncompressed: 200, ..Limits::default() };
    assert!(is_limit(&Package::open(b.clone(), &l)));
    let l = Limits { max_part_size: 100, ..Limits::default() };
    assert!(is_limit(&Package::open(b.clone(), &l)));
    // Limits also apply to parts written later.
    let mut p = open(b).unwrap();
    let l2 = Limits { max_part_size: 1000, ..p.limits().clone() };
    let mut p2 = Package::open(p.original_bytes().to_vec(), &l2).unwrap();
    assert!(matches!(p2.set_part("word/document.xml", vec![b'x'; 2000]), Err(Error::Limit(_))));
    assert!(matches!(p2.add_part("word/big.xml", vec![b'x'; 2000], "application/xml"), Err(Error::Limit(_))));
    p.set_part("word/document.xml", vec![b'x'; 2000]).unwrap();
}

#[test]
fn too_many_entries() {
    let mut es = minimal_entries();
    for i in 0..10_001 {
        es.push(E::new(&format!("x/{i}.bin"), b"").stored());
    }
    assert!(is_limit(&open(build_zip(&es, b""))));
    let l = Limits { max_entries: 3, ..Limits::default() };
    assert!(is_limit(&Package::open(minimal_docx(), &l)));
    // An EOCD that claims 65535 entries over a tiny central directory.
    let mut b = minimal_docx();
    let (_, _, eocd) = layout(&b);
    put16(&mut b, eocd + 8, 0xFFFF);
    put16(&mut b, eocd + 10, 0xFFFF);
    let r = open(b);
    assert!(is_limit(&r) || is_package(&r), "{:?}", r.err());
}

#[test]
fn hostile_names_are_rejected() {
    for name in [
        "../evil.xml",
        "word/../../evil.xml",
        "/abs.xml",
        "word\\evil.xml",
        "C:/evil.xml",
        "word/./x.xml",
        "word//x.xml",
        "word/x\u{1}.xml",
        "",
    ] {
        let r = open(with(E::new(name, b"<x/>")));
        assert!(is_package(&r), "{name:?}: {:?}", r.err());
    }
    // A non-empty directory entry.
    assert!(is_package(&open(with(E::new("word/dir/", b"data")))));
    // An empty one is fine.
    assert!(open(with(E::new("word/dir/", b"").stored())).is_ok());
}

#[test]
fn duplicate_names_are_rejected() {
    assert!(is_package(&open(with(E::new("word/document.xml", b"<x/>")))));
    assert!(is_package(&open(with(E::new("WORD/Document.XML", b"<x/>")))));
}

#[test]
fn local_name_must_match_central() {
    let mut b = minimal_docx();
    let (_, locals, _) = layout(&b);
    b[locals[2] + 30] = b'W'; // "word/document.xml" → "Word/document.xml" in the local header only
    assert!(is_package(&open(b)));
}

#[test]
fn overlapping_entries_are_rejected() {
    // Stretch entry 0's compressed size (local + central agree) over entry 1.
    let mut b = minimal_docx();
    let (cds, locals, _) = layout(&b);
    let csize = u32::from_le_bytes(b[cds[0] + 20..cds[0] + 24].try_into().unwrap()) + 40;
    put32(&mut b, cds[0] + 20, csize);
    put32(&mut b, locals[0] + 18, csize);
    let r = open(b);
    assert!(is_package(&r) || is_limit(&r), "{:?}", r.err());

    // Two central records pointing at the same local header.
    let mut b = minimal_docx();
    let (cds, locals, _) = layout(&b);
    put32(&mut b, cds[1] + 42, locals[0] as u32);
    assert!(is_package(&open(b)));
}

#[test]
fn zip64_tricks_are_rejected() {
    // Zip64 sentinel in the EOCD without a zip64 record.
    let mut b = minimal_docx();
    let (_, _, eocd) = layout(&b);
    put32(&mut b, eocd + 16, 0xFFFF_FFFF);
    assert!(is_package(&open(b)));

    // Central size sentinel without a zip64 extra field.
    let mut b = minimal_docx();
    let (cds, _, _) = layout(&b);
    put32(&mut b, cds[2] + 24, 0xFFFF_FFFF);
    let r = open(b);
    assert!(is_package(&r) || is_limit(&r), "{:?}", r.err());

    // A zip64 extra that claims a 5 GB entry.
    let mut extra = vec![0x01, 0x00, 16, 0];
    extra.extend_from_slice(&(5u64 << 30).to_le_bytes());
    extra.extend_from_slice(&100u64.to_le_bytes());
    let mut e = E::new("word/z.xml", b"<x/>");
    e.cd_extra = extra;
    let mut b = with(e);
    let (cds, _, _) = layout(&b);
    put32(&mut b, cds[5] + 20, 0xFFFF_FFFF);
    put32(&mut b, cds[5] + 24, 0xFFFF_FFFF);
    let r = open(b);
    assert!(is_package(&r) || is_limit(&r), "{:?}", r.err());

    // Multi-disk.
    let mut b = minimal_docx();
    let (_, _, eocd) = layout(&b);
    put16(&mut b, eocd + 4, 1);
    assert!(is_package(&open(b)));
}

#[test]
fn encrypted_input_is_rejected() {
    let mut e = E::new("word/secret.xml", b"<x/>");
    e.flags = 1;
    assert!(matches!(open(with(e)), Err(Error::Encrypted(_))));

    // An OLE compound file carrying an EncryptedPackage stream (password-protected .docx).
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.resize(512, 0);
    for c in "EncryptedPackage".encode_utf16() {
        ole.extend_from_slice(&c.to_le_bytes());
    }
    ole.resize(4096, 0);
    let r = open(ole.clone());
    assert!(matches!(r, Err(Error::Encrypted(_))), "{:?}", r.err());
    // A plain OLE file (legacy .doc) is a package error.
    ole.truncate(512);
    assert!(is_package(&open(ole)));
}

#[test]
fn corrupt_data_is_an_error_not_a_panic() {
    // CRC mismatch on a deflated part.
    let mut b = minimal_docx();
    let (cds, locals, _) = layout(&b);
    let crc = u32::from_le_bytes(b[cds[2] + 16..cds[2] + 20].try_into().unwrap()) ^ 1;
    put32(&mut b, cds[2] + 16, crc);
    put32(&mut b, locals[2] + 14, crc);
    let p = open(b).unwrap();
    assert!(matches!(p.part("word/document.xml"), Err(Error::Package(_))));

    // CRC mismatch on a stored part.
    let mut b = minimal_docx();
    let (cds, locals, _) = layout(&b);
    let crc = u32::from_le_bytes(b[cds[4] + 16..cds[4] + 20].try_into().unwrap()) ^ 1;
    put32(&mut b, cds[4] + 16, crc);
    put32(&mut b, locals[4] + 14, crc);
    let p = open(b).unwrap();
    assert!(p.part("word/media/image1.png").is_err());

    // Garbage deflate stream.
    let mut b = minimal_docx();
    let (_, locals, _) = layout(&b);
    let nlen = u16::from_le_bytes([b[locals[2] + 26], b[locals[2] + 27]]) as usize;
    let data = locals[2] + 30 + nlen;
    for x in &mut b[data..data + 8] {
        *x = 0xFF;
    }
    if let Ok(p) = open(b) {
        assert!(p.part("word/document.xml").is_err());
    }

    // A stored entry whose sizes disagree.
    let mut b = minimal_docx();
    let (cds, _, _) = layout(&b);
    let u = u32::from_le_bytes(b[cds[4] + 24..cds[4] + 28].try_into().unwrap()) + 1;
    put32(&mut b, cds[4] + 24, u);
    assert!(open(b).is_err());
}

#[test]
fn not_a_package() {
    assert!(is_package(&open(vec![])));
    assert!(is_package(&open(b"hello world".to_vec())));
    assert!(is_package(&open(minimal_docx()[..100].to_vec())));
    // A valid zip with no [Content_Types].xml.
    let es: Vec<E> = minimal_entries().into_iter().skip(1).collect();
    assert!(is_package(&open(build_zip(&es, b""))));
    // A [Content_Types].xml with a DOCTYPE (entity expansion is never attempted).
    let mut es = minimal_entries();
    es[0] = E::new(
        "[Content_Types].xml",
        br#"<?xml version="1.0"?><!DOCTYPE Types [<!ENTITY a "aaaa">]><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#,
    );
    assert!(open(build_zip(&es, b"")).is_err());
}
