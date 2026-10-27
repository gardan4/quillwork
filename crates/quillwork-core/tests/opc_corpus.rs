//! Corpus identity suite for the package layer: synthetic files (always) and the private
//! corpus (when present). Only numbers are printed for private files, never content.

mod opc_common;

use std::path::Path;
use std::time::Instant;

use opc_common::*;
use quillwork_core::opc::{Limits, Package};

const IMAGE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

struct Outcome {
    noop_identical: bool,
    rebuilt_identical: bool,
    others_identical: usize,
    others_total: usize,
}

fn check_file(path: &Path, label: &str) -> Outcome {
    let input = std::fs::read(path).unwrap();
    let t = Instant::now();
    let pkg = Package::open(input.clone(), &Limits::default()).unwrap_or_else(|e| panic!("{label}: open failed: {e}"));
    let t_open = t.elapsed();

    // 1. No-op save is the input.
    let t = Instant::now();
    let noop = pkg.save().unwrap();
    let t_save = t.elapsed();
    let noop_identical = noop == input;
    assert!(noop_identical, "{label}: no-op save differs");

    // Every part inflates with a good CRC.
    let names: Vec<String> = pkg.names().map(str::to_owned).collect();
    for n in &names {
        pkg.part(n).unwrap_or_else(|e| panic!("{label}: part {n}: {e}"));
    }

    // 2. The splicer with nothing dirty reproduces a canonically laid-out file exactly.
    let rebuilt = pkg.save_rebuilt().unwrap();
    let rebuilt_identical = rebuilt == input;

    // 3. Replace document.xml with its own bytes + a comment.
    let mut pkg = pkg;
    let main = "word/document.xml";
    let mut doc = pkg.part(main).unwrap().into_owned();
    doc.extend_from_slice(b"<!-- quillwork -->");
    pkg.set_part(main, doc.clone()).unwrap();
    assert!(pkg.is_modified());
    let edited = pkg.save().unwrap();

    let before = spans(&input);
    let after = spans(&edited);
    assert_eq!(
        before.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        after.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "{label}: entry order changed"
    );
    let mut others_identical = 0;
    let mut others_total = 0;
    for ((name, a), (_, b)) in before.iter().zip(&after) {
        if name == main {
            continue;
        }
        others_total += 1;
        if a == b {
            others_identical += 1;
        }
    }
    assert_eq!(others_identical, others_total, "{label}: an untouched entry changed");

    let re = Package::open(edited.clone(), &Limits::default()).unwrap();
    assert_eq!(&*re.part(main).unwrap(), &doc[..], "{label}: edited part reads back wrong");
    assert_eq!(re.entry_info(main).unwrap().method, pkg.entry_info(main).unwrap().method);
    assert_eq!(check_with_zip_crate(&edited).unwrap(), names.len(), "{label}: zip crate");
    if let Some(r) = check_with_python(&edited, "edited") {
        assert_eq!(r.unwrap_or_else(|e| panic!("{label}: python zipfile: {e}")), names.len());
    }

    // Setting it back makes the package clean again: save is the original bytes.
    let orig_doc = Package::open(input.clone(), &Limits::default()).unwrap().part(main).unwrap().into_owned();
    pkg.set_part(main, orig_doc).unwrap();
    assert!(!pkg.is_modified());
    assert!(pkg.save().unwrap() == input, "{label}: reverting the edit does not restore the input");

    // 4. Media + relationship + content type: only appended bytes in the two package XMLs.
    let ct_before = pkg.part("[Content_Types].xml").unwrap().into_owned();
    let rels_before = pkg.part("word/_rels/document.xml.rels").ok().map(|p| p.into_owned());
    let media = pkg.add_media(PNG.to_vec(), "png").unwrap();
    assert_eq!(pkg.add_media(PNG.to_vec(), "png").unwrap(), media, "{label}: media not deduped");
    let id = pkg.add_relationship_to_part(main, IMAGE_REL, &media).unwrap();
    assert!(id.starts_with("rIdQw"));
    pkg.add_part("customXml/quillwork.xml", b"<q/>".to_vec(), "application/x-quillwork+xml").unwrap();
    let ct_after = pkg.part("[Content_Types].xml").unwrap().into_owned();
    assert_only_insertion(&ct_before, &ct_after, label);
    if let Some(rb) = rels_before {
        assert_only_insertion(&rb, &pkg.part("word/_rels/document.xml.rels").unwrap(), label);
    }
    // The unreferenced customXml part is ours and orphaned; the media is referenced.
    let gone = pkg.gc_orphans().unwrap();
    assert_eq!(gone, vec!["customXml/quillwork.xml".to_string()], "{label}: gc");
    let out = pkg.save().unwrap();
    let re = Package::open(out.clone(), &Limits::default()).unwrap();
    assert_eq!(&*re.part(&media).unwrap(), PNG);
    assert!(
        re.rels_of(main).unwrap().iter().any(|r| r.id == id && re.resolve(main, r).as_deref() == Some(media.as_str()))
    );
    assert_eq!(re.content_type_of(&media), Some("image/png"));
    assert!(!re.contains("customXml/quillwork.xml"));
    assert_eq!(re.content_types().override_for("customXml/quillwork.xml"), None);
    check_with_zip_crate(&out).unwrap();
    if let Some(r) = check_with_python(&out, "media") {
        r.unwrap_or_else(|e| panic!("{label}: python zipfile after media: {e}"));
    }

    println!(
        "{label}: {} bytes, {} entries, open {:.2} ms, no-op save {:.2} ms, no-op identical {}, rebuilt identical {}, untouched entries identical {}/{}",
        input.len(),
        names.len(),
        t_open.as_secs_f64() * 1e3,
        t_save.as_secs_f64() * 1e3,
        noop_identical,
        rebuilt_identical,
        others_identical,
        others_total
    );
    Outcome { noop_identical, rebuilt_identical, others_identical, others_total }
}

/// `after` must be `before` with one contiguous run of bytes inserted.
fn assert_only_insertion(before: &[u8], after: &[u8], label: &str) {
    let pre = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let suf = before[pre..].iter().rev().zip(after[pre..].iter().rev()).take_while(|(a, b)| a == b).count();
    assert_eq!(pre + suf, before.len(), "{label}: package XML changed beyond an insertion");
}

#[test]
fn synthetic_corpus_identity() {
    let files = synthetic_corpus();
    assert!(!files.is_empty(), "no synthetic corpus files");
    for f in &files {
        let o = check_file(f, &f.file_name().unwrap().to_string_lossy());
        assert!(o.noop_identical && o.rebuilt_identical);
        assert_eq!(o.others_identical, o.others_total);
    }
}

#[test]
fn private_corpus_identity() {
    let files = private_corpus();
    if files.is_empty() {
        eprintln!("private corpus absent: skipped");
        return;
    }
    let (mut noop, mut rebuilt, mut ent_same, mut ent_total) = (0, 0, 0, 0);
    for (i, f) in files.iter().enumerate() {
        // Label by index only: file names of client documents are not printed.
        let o = check_file(f, &format!("private#{}", i + 1));
        noop += o.noop_identical as usize;
        rebuilt += o.rebuilt_identical as usize;
        ent_same += o.others_identical;
        ent_total += o.others_total;
    }
    println!(
        "private corpus: {} files, no-op identical {noop}/{n}, rebuilt identical {rebuilt}/{n}, untouched entries identical {ent_same}/{ent_total}",
        files.len(),
        n = files.len()
    );
}
