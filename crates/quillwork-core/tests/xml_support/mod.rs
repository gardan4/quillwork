//! Shared helpers for the `xml_*` integration tests: corpus discovery and part extraction.
//!
//! The private corpus holds real client documents. Tests only ever print counts and sizes
//! from it, never names or content.
#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

pub fn synthetic_docx() -> Vec<PathBuf> {
    docx_in(&workspace_root().join("corpus/synthetic"))
}

/// The private corpus root: `$QUILLWORK_PRIVATE_CORPUS`, else a `quillwork-private-corpus` folder
/// next to an ancestor of the workspace. `None` when absent.
pub fn private_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("QUILLWORK_PRIVATE_CORPUS") {
        let d = PathBuf::from(root);
        return d.is_dir().then_some(d);
    }
    workspace_root().ancestors().map(|a| a.join("quillwork-private-corpus")).find(|d| d.is_dir())
}

/// The folder of private `.docx` files: the corpus root itself when it holds any, else its first
/// subfolder (in name order) that does. `None` when absent (tests then skip that half).
pub fn private_dir() -> Option<PathBuf> {
    let root = private_root()?;
    if !docx_in(&root).is_empty() {
        return Some(root);
    }
    let mut subs: Vec<PathBuf> =
        std::fs::read_dir(&root).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    subs.sort();
    subs.into_iter().find(|d| !docx_in(d).is_empty())
}

pub fn private_docx() -> Vec<PathBuf> {
    private_dir().map(|d| docx_in(&d)).unwrap_or_default()
}

fn docx_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("docx")))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Every `.xml` / `.rels` part of a docx, as (part name, bytes).
pub fn xml_parts(docx: &Path) -> Vec<(String, Vec<u8>)> {
    let f = std::fs::File::open(docx).expect("open docx");
    let mut z = zip::ZipArchive::new(f).expect("read zip");
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).expect("zip entry");
        let name = e.name().expect("entry name").into_owned();
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".xml") || lower.ends_with(".rels")) {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).expect("inflate");
        out.push((name, buf));
    }
    out
}

pub fn part(docx: &Path, name: &str) -> Option<Vec<u8>> {
    xml_parts(docx).into_iter().find(|(n, _)| n == name).map(|(_, b)| b)
}

/// The largest `word/document.xml` across both corpora.
pub fn biggest_document_xml() -> Vec<u8> {
    synthetic_docx()
        .into_iter()
        .chain(private_docx())
        .filter_map(|p| part(&p, "word/document.xml"))
        .max_by_key(|b| b.len())
        .expect("at least one document.xml in the corpus")
}

/// Assert `out` equals `orig` everywhere outside `orig[span]` (whose replacement may have any
/// length). Uses `assert!`, not `assert_eq!`, so a failure never prints document bytes (the
/// private corpus must not leak into logs).
pub fn assert_local(orig: &[u8], out: &[u8], span: std::ops::Range<usize>) {
    assert!(out[..span.start] == orig[..span.start], "bytes before the edited element changed");
    let tail = orig.len() - span.end;
    assert!(out.len() >= span.start + tail, "output shorter than untouched parts");
    assert!(out[out.len() - tail..] == orig[span.end..], "bytes after the edited element changed");
}
