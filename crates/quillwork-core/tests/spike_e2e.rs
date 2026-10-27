//! Spike go/no-go (DESIGN §2): the package and XML layers together.
//!
//! For every corpus file: a no-op open/save returns the input bytes, and changing the text of
//! one `w:t` changes nothing else — every other zip entry and every byte of `document.xml`
//! outside that element stay identical. Private files are reported by index only.

mod xml_support;

use quillwork_core::{opc, xml};
use std::path::PathBuf;

fn corpus_files() -> Vec<(String, PathBuf)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    let mut push_dir = |dir: PathBuf, label: &str| {
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut paths: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "docx"))
            .collect();
        paths.sort();
        for (i, p) in paths.into_iter().enumerate() {
            let name = if label == "private" {
                format!("private#{i}")
            } else {
                p.file_name().unwrap().to_string_lossy().into_owned()
            };
            out.push((name, p));
        }
    };
    push_dir(root.join("corpus/synthetic"), "synthetic");
    if let Some(private) = xml_support::private_dir() {
        push_dir(private, "private");
    }
    out
}

#[test]
fn noop_and_single_text_edit_are_local() {
    let files = corpus_files();
    assert!(!files.is_empty());
    for (name, path) in files {
        let input = std::fs::read(&path).unwrap();

        let pkg = opc::Package::open(input.clone(), &opc::Limits::default()).unwrap();
        assert_eq!(pkg.save().unwrap(), input, "{name}: no-op save changed bytes");

        let mut pkg = opc::Package::open(input.clone(), &opc::Limits::default()).unwrap();
        let original_xml = pkg.part("word/document.xml").unwrap().into_owned();
        let mut doc = xml::Document::parse(&original_xml).unwrap();
        let t = doc
            .descendants_named(doc.root(), xml::ns::W, "t")
            .find(|&t| !doc.text_content(t).is_empty())
            .unwrap_or_else(|| panic!("{name}: no w:t"));
        let span = doc.source_span(t).unwrap();
        doc.set_text(t, "QW_EDIT").unwrap();
        let edited_xml = doc.to_bytes();
        pkg.set_part("word/document.xml", edited_xml.clone()).unwrap();
        let saved = pkg.save().unwrap();

        // document.xml: identical outside the edited element.
        let prefix = &original_xml[..span.start];
        let suffix = &original_xml[span.end..];
        assert!(edited_xml.starts_with(prefix), "{name}: bytes before the edit changed");
        assert!(edited_xml.ends_with(suffix), "{name}: bytes after the edit changed");
        let middle = &edited_xml[prefix.len()..edited_xml.len() - suffix.len()];
        assert!(std::str::from_utf8(middle).unwrap().contains("QW_EDIT"), "{name}: edit not where expected");

        // Every other part is unchanged after re-opening the saved package.
        let reopened = opc::Package::open(saved, &opc::Limits::default()).unwrap();
        let before = opc::Package::open(input, &opc::Limits::default()).unwrap();
        let names: Vec<String> = before.names().map(str::to_owned).collect();
        assert_eq!(names, reopened.names().map(str::to_owned).collect::<Vec<_>>(), "{name}: entry list changed");
        for part in &names {
            let a = before.part(part).unwrap();
            let b = reopened.part(part).unwrap();
            if part == "word/document.xml" {
                assert_eq!(&*b, &edited_xml[..], "{name}: document.xml did not round-trip");
            } else {
                assert!(a == b, "{name}: {part} changed");
            }
        }
    }
}
