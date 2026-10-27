//! Shared helpers for the opc integration tests.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
pub const PKG_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
pub const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/x" TargetMode="External"/></Relationships>"#;
pub const DOC_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#;
pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake image bytes for tests";

/// One entry for [`build_zip`].
#[derive(Clone)]
pub struct E {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub deflate: bool,
    /// zlib level for deflated entries.
    pub level: u32,
    /// Write a data descriptor (flag bit 3); `Some(true)` with signature.
    pub descriptor: Option<bool>,
    pub local_extra: Vec<u8>,
    pub cd_extra: Vec<u8>,
    pub comment: Vec<u8>,
    pub made_by: u16,
    pub ext_attr: u32,
    pub flags: u16,
    pub time: u16,
    pub date: u16,
}

impl E {
    pub fn new(name: &str, data: &[u8]) -> E {
        E {
            name: name.as_bytes().to_vec(),
            data: data.to_vec(),
            deflate: true,
            level: 6,
            descriptor: None,
            local_extra: vec![],
            cd_extra: vec![],
            comment: vec![],
            made_by: 0x0014,
            ext_attr: 0,
            flags: 0,
            time: 0x6000,
            date: 0x5a21,
        }
    }
    pub fn stored(mut self) -> E {
        self.deflate = false;
        self
    }
    pub fn descriptor(mut self, sig: bool) -> E {
        self.descriptor = Some(sig);
        self
    }
}

pub fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

/// Build a zip by hand. Returns the bytes and each entry's local-header offset.
pub fn build_zip(entries: &[E], archive_comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for e in entries {
        let off = out.len() as u32;
        let comp = if e.deflate { deflate(&e.data, e.level) } else { e.data.clone() };
        let crc = crc32fast::hash(&e.data);
        let method: u16 = if e.deflate { 8 } else { 0 };
        let flags = e.flags | if e.descriptor.is_some() { 8 } else { 0 };
        let (lcrc, lc, lu) =
            if e.descriptor.is_some() { (0, 0, 0) } else { (crc, comp.len() as u32, e.data.len() as u32) };
        out.extend_from_slice(&0x04034b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&e.time.to_le_bytes());
        out.extend_from_slice(&e.date.to_le_bytes());
        out.extend_from_slice(&lcrc.to_le_bytes());
        out.extend_from_slice(&lc.to_le_bytes());
        out.extend_from_slice(&lu.to_le_bytes());
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(e.local_extra.len() as u16).to_le_bytes());
        out.extend_from_slice(&e.name);
        out.extend_from_slice(&e.local_extra);
        out.extend_from_slice(&comp);
        if let Some(sig) = e.descriptor {
            if sig {
                out.extend_from_slice(&0x08074b50u32.to_le_bytes());
            }
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        }
        cd.extend_from_slice(&0x02014b50u32.to_le_bytes());
        cd.extend_from_slice(&e.made_by.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&flags.to_le_bytes());
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&e.time.to_le_bytes());
        cd.extend_from_slice(&e.date.to_le_bytes());
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        cd.extend_from_slice(&(e.cd_extra.len() as u16).to_le_bytes());
        cd.extend_from_slice(&(e.comment.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&e.ext_attr.to_le_bytes());
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(&e.name);
        cd.extend_from_slice(&e.cd_extra);
        cd.extend_from_slice(&e.comment);
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&(archive_comment.len() as u16).to_le_bytes());
    out.extend_from_slice(archive_comment);
    out
}

/// A small, valid docx-shaped package.
pub fn minimal_entries() -> Vec<E> {
    vec![
        E::new("[Content_Types].xml", CT_XML.as_bytes()),
        E::new("_rels/.rels", PKG_RELS.as_bytes()),
        E::new("word/document.xml", DOC_XML.as_bytes()),
        E::new("word/_rels/document.xml.rels", DOC_RELS.as_bytes()),
        E::new("word/media/image1.png", PNG).stored(),
    ]
}

pub fn minimal_docx() -> Vec<u8> {
    build_zip(&minimal_entries(), b"")
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn docx_in(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut items: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    items.sort();
    for p in items {
        if p.is_dir() {
            docx_in(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx") || e.eq_ignore_ascii_case("docm")) {
            out.push(p);
        }
    }
}

pub fn synthetic_corpus() -> Vec<PathBuf> {
    let mut out = Vec::new();
    docx_in(&repo_root().join("corpus/synthetic"), &mut out);
    out
}

/// The private corpus directory, if present: `$QUILLWORK_PRIVATE_CORPUS`, else
/// `../quillwork-private-corpus` next to the repo (or next to the worktree folder).
pub fn private_corpus_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("QUILLWORK_PRIVATE_CORPUS") {
        let p = PathBuf::from(p);
        return p.is_dir().then_some(p);
    }
    let root = repo_root();
    [root.join("../quillwork-private-corpus"), root.join("../../quillwork-private-corpus")]
        .into_iter()
        .find(|p| p.is_dir())
}

pub fn private_corpus() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = private_corpus_dir() {
        docx_in(&d, &mut out);
    }
    out
}

/// Read every entry with the `zip` crate (which verifies CRCs). Returns the entry count.
pub fn check_with_zip_crate(bytes: &[u8]) -> Result<usize, String> {
    let mut za = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    for i in 0..za.len() {
        let mut f = za.by_index(i).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| format!("entry {i}: {e}"))?;
    }
    Ok(za.len())
}

/// Check with Python's `zipfile` (testzip verifies every CRC). `None` if python3 is missing.
pub fn check_with_python(bytes: &[u8], tag: &str) -> Option<Result<usize, String>> {
    let dir = std::env::temp_dir().join(format!("quillwork-opc-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("{tag}-{seq}.zip"));
    std::fs::write(&path, bytes).ok()?;
    let out = Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sys,zipfile\nz=zipfile.ZipFile(sys.argv[1])\nb=z.testzip()\nassert b is None, b\nprint(len(z.infolist()))",
        ])
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);
    let out = out.ok()?;
    Some(if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    })
}

/// Local header + data (+ descriptor) span of every entry, by name, via our own parser.
pub fn spans(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    // Walk the central directory directly so the check does not depend on Package internals.
    let eocd = (0..=bytes.len() - 22).rev().find(|&p| bytes[p..p + 4] == 0x06054b50u32.to_le_bytes()).unwrap();
    let n = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_off = u32::from_le_bytes(bytes[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let mut p = cd_off;
    let mut offs = Vec::new();
    for _ in 0..n {
        let nlen = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let elen = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let clen = u16::from_le_bytes([bytes[p + 32], bytes[p + 33]]) as usize;
        let off = u32::from_le_bytes(bytes[p + 42..p + 46].try_into().unwrap()) as usize;
        let name = String::from_utf8_lossy(&bytes[p + 46..p + 46 + nlen]).into_owned();
        offs.push((name, off));
        p += 46 + nlen + elen + clen;
    }
    let mut sorted: Vec<usize> = offs.iter().map(|(_, o)| *o).collect();
    sorted.push(cd_off);
    sorted.sort();
    offs.into_iter()
        .map(|(name, off)| {
            let next = *sorted.iter().find(|&&o| o > off).unwrap();
            (name, bytes[off..next].to_vec())
        })
        .collect()
}
