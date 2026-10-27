//! The validator over the fixtures and corpora: zero errors on every valid document, every
//! invalid fixture caught, and the speed budget on real documents.
//!
//! The private corpus holds real client documents: files are labelled `private#<i>`, only
//! finding codes and counts are printed, never text.

mod xml_support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use quillwork_core::validate::{self, Report, Severity};
use quillwork_core::wml::WordDoc;

fn valid_docs() -> Vec<(String, PathBuf)> {
    let dir = xml_support::workspace_root().join("corpus/fixtures");
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    v.sort();
    v.extend(
        xml_support::synthetic_docx().into_iter().map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p)),
    );
    for (i, p) in xml_support::private_docx().into_iter().enumerate() {
        v.push((format!("private#{i}"), p));
    }
    v
}

fn summary(r: &Report) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for f in &r.findings {
        let sev = if f.severity == Severity::Error { "E" } else { "W" };
        *m.entry(format!("{sev}:{}", f.code)).or_insert(0) += 1;
    }
    m
}

#[test]
fn valid_documents_have_no_errors() {
    let mut failures = Vec::new();
    for (label, path) in valid_docs() {
        let bytes = std::fs::read(&path).unwrap();
        let r = validate::check_bytes(&bytes);
        let s = summary(&r);
        eprintln!("{label}: {s:?}");
        if !r.is_ok() {
            // Codes only: messages may quote ids from private documents.
            failures.push(format!("{label}: {:?}", s.keys().filter(|k| k.starts_with("E:")).collect::<Vec<_>>()));
        }
        // check(doc) on the opened document agrees with check_bytes.
        let doc = WordDoc::open(bytes).unwrap();
        assert_eq!(validate::check(&doc).findings, r.findings, "{label}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn invalid_fixtures_are_caught() {
    let dir = xml_support::workspace_root().join("corpus/fixtures/invalid");
    let cases = [
        ("bad-attribute-value.docx", "value.invalid"),
        ("bad-element-order.docx", "order.child_order"),
        ("malformed-xml.docx", "xml.malformed"),
        ("missing-content-type.docx", "package.word_document_missing"),
        ("missing-rel-target.docx", "package.missing_target"),
        ("not-a-zip.docx", "package.not_a_zip"),
    ];
    for (file, code) in cases {
        let r = validate::check_bytes(&std::fs::read(dir.join(file)).unwrap());
        eprintln!("{file}: {:?}", summary(&r));
        assert!(!r.is_ok(), "{file} passed");
        assert!(r.errors().any(|f| f.code == code), "{file}: expected {code}, got {:?}", r.findings);
    }
}

#[test]
fn speed_on_real_documents() {
    let mut paras = 0usize;
    let mut total = std::time::Duration::ZERO;
    for (_, path) in valid_docs() {
        let doc = WordDoc::open(std::fs::read(&path).unwrap()).unwrap();
        let d = doc.part(doc.main_part());
        paras += d.descendants(d.root()).filter(|&n| d.local_name(n) == Some("p")).count();
        let t = Instant::now();
        std::hint::black_box(validate::check(&doc));
        total += t.elapsed();
    }
    let per_k = total.as_secs_f64() * 1000.0 / (paras.max(1) as f64 / 1000.0);
    eprintln!("validate: {paras} paragraphs in {total:?} = {per_k:.2} ms / 1000 paragraphs");
    let budget = if cfg!(debug_assertions) { 100.0 } else { 5.0 };
    assert!(per_k < budget, "{per_k:.2} ms / 1000 paragraphs");
}
