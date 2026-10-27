//! Anchors against spec/anchors.md: grammar, minting over the corpora, stability across loads,
//! save/re-open with the sidecar, in-session edits and re-attach, `ensure_para_id`.
//!
//! The private corpus holds real client documents: failures print handles, ids and counts only.

mod wml_support;
mod xml_support;

use std::collections::HashSet;
use std::path::PathBuf;

use quillwork_core::anchors::{self, Anchors, Sidecar, Target};
use quillwork_core::wml::view::{blocks, drawings, paragraphs, tables};
use quillwork_core::wml::{Block, Loc, StoryKind, WordDoc};
use regex::Regex;

const BLOCK: &str = r"^(b[0-9a-hjkmnp-tv-z]{3,}|(h|f|fn|en|c|tb)[1-9][0-9]*-[0-9a-hjkmnp-tv-z]+)$";
const TABLE: &str = r"^t[1-9][0-9]*$";
const ROW: &str = r"^t[1-9][0-9]*\.r[1-9][0-9]*$";
const CELL: &str = r"^t[1-9][0-9]*\.r[1-9][0-9]*\.c[1-9][0-9]*$";
const IMAGE: &str = r"^img[1-9][0-9]*$";
const COMMENT: &str = r"^c[1-9][0-9]*$";
const STORY: &str = r"^(b|(h|f|fn|en|c|tb)[1-9][0-9]*)$";

fn all_docs() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = xml_support::synthetic_docx()
        .into_iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    let fx = xml_support::workspace_root().join("corpus/fixtures");
    let mut f: Vec<PathBuf> = std::fs::read_dir(fx)
        .expect("fixtures")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .collect();
    f.sort();
    v.extend(f.into_iter().map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p)));
    for (i, p) in xml_support::private_docx().into_iter().enumerate() {
        v.push((format!("private#{i}"), p));
    }
    v
}

fn open(path: &PathBuf) -> WordDoc {
    WordDoc::open(std::fs::read(path).expect("read")).expect("open")
}

/// Handles per story, in document order.
fn handle_lists(doc: &WordDoc, a: &Anchors) -> Vec<Vec<String>> {
    (0..doc.stories().len()).map(|i| a.handles(i).map(|(h, _)| h.to_owned()).collect()).collect()
}

fn others(a: &Anchors) -> (Vec<String>, Vec<String>) {
    (a.table_anchors().map(|(t, _)| t.to_owned()).collect(), a.image_anchors().map(|(t, _)| t.to_owned()).collect())
}

type Validator = fn(&str) -> bool;

#[test]
fn spec_examples_agree_with_regex_and_validators() {
    let path = xml_support::workspace_root().join("spec/examples/anchors.json");
    let ex: serde_json::Value = serde_json::from_slice(&std::fs::read(path).expect("anchors.json")).expect("json");
    let kinds: [(&str, &str, Validator); 7] = [
        ("BlockAnchor", BLOCK, anchors::is_block_anchor),
        ("TableAnchor", TABLE, anchors::is_table_anchor),
        ("RowAnchor", ROW, anchors::is_row_anchor),
        ("CellAnchor", CELL, anchors::is_cell_anchor),
        ("ImageAnchor", IMAGE, anchors::is_image_anchor),
        ("CommentId", COMMENT, anchors::is_comment_id),
        ("StoryId", STORY, anchors::is_story_id),
    ];
    let mut n = 0;
    for (kind, re, f) in kinds {
        let re = Regex::new(re).unwrap();
        for (set, want) in [("valid", true), ("invalid", false)] {
            for s in ex[set][kind].as_array().unwrap_or_else(|| panic!("{set}.{kind}")) {
                let s = s.as_str().unwrap();
                assert_eq!(re.is_match(s), want, "regex {kind} {s:?}");
                assert_eq!(f(s), want, "validator {kind} {s:?}");
                n += 1;
            }
        }
    }
    assert!(n > 50);
}

#[test]
fn minted_handles_match_spec_and_resolve() {
    let block = Regex::new(BLOCK).unwrap();
    let table = Regex::new(TABLE).unwrap();
    let image = Regex::new(IMAGE).unwrap();
    let comment = Regex::new(COMMENT).unwrap();
    let story = Regex::new(STORY).unwrap();
    let cell = Regex::new(CELL).unwrap();
    for (label, path) in all_docs() {
        let doc = open(&path);
        let a = Anchors::mint(&doc);
        let mut seen = HashSet::new();
        let mut n_handles = 0;
        for (si, st) in doc.stories().iter().enumerate() {
            let sid = a.story_id(si).expect("story id");
            assert!(story.is_match(sid), "{label}: story id {sid}");
            if matches!(st.kind, StoryKind::Comment { .. }) {
                assert!(comment.is_match(sid), "{label}: comment id {sid}");
                assert!(matches!(a.resolve(&doc, sid), Some(Target::Comment { story, .. }) if story == si));
            }
            assert_eq!(st.prefix(), sid.trim_end_matches(|c: char| c.is_ascii_digit()), "{label}: prefix of {sid}");
            let d = doc.part(st.part);
            let paras = paragraphs(d, st.root);
            let hs: Vec<(&str, Loc)> = a.handles(si).collect();
            assert_eq!(hs.len(), paras.len(), "{label}: every paragraph of {sid} has a handle");
            for (k, ((h, loc), p)) in hs.iter().zip(&paras).enumerate() {
                assert!(block.is_match(h), "{label}: handle {h}");
                assert!(seen.insert(h.to_string()), "{label}: duplicate handle {h}");
                assert_eq!(loc.node, p.0);
                assert_eq!(a.handle_of(*loc), Some(*h));
                assert_eq!(a.resolve(&doc, h), Some(Target::Block { story: si, para: *loc }));
                // Serials count up from the story's first serial.
                let (w, first) = anchors::serial_rule(st.prefix());
                let want = if sid == "b" {
                    format!("b{}", anchors::encode_serial(first + k as u64, w))
                } else {
                    format!("{sid}-{}", anchors::encode_serial(first + k as u64, w))
                };
                assert_eq!(*h, want, "{label}: fresh minting is sequential");
                n_handles += 1;
            }
        }
        if let Some((h, _)) = a.handles(0).next() {
            assert_eq!(h, "b100", "{label}: first body handle");
        }
        for (k, (t, loc)) in a.table_anchors().enumerate() {
            assert!(table.is_match(t), "{label}: table {t}");
            assert_eq!(t, format!("t{}", k + 1));
            assert_eq!(a.resolve(&doc, t), Some(Target::Table { table: loc }));
            let c = format!("{t}.r1.c1");
            assert!(cell.is_match(&c));
            if let Some(Target::Cell { table: tl, .. }) = a.resolve(&doc, &c) {
                assert_eq!(tl, loc);
            }
        }
        let total_tables: usize = doc.stories().iter().map(|s| tables(doc.part(s.part), s.root).len()).sum();
        assert_eq!(a.table_anchors().count(), total_tables, "{label}: every table has an anchor");
        let total_imgs: usize = doc.stories().iter().map(|s| drawings(doc.part(s.part), s.root).len()).sum();
        assert_eq!(a.image_anchors().count(), total_imgs, "{label}: every drawing has an anchor");
        for (k, (t, loc)) in a.image_anchors().enumerate() {
            assert!(image.is_match(t), "{label}: image {t}");
            assert_eq!(t, format!("img{}", k + 1));
            assert_eq!(a.resolve(&doc, t), Some(Target::Image { drawing: loc }));
        }
        eprintln!(
            "{label}: stories={} paragraph handles={} tables={} images={}",
            doc.stories().len(),
            n_handles,
            a.table_anchors().count(),
            a.image_anchors().count()
        );
        assert!(a.resolve(&doc, "b0zz").is_none());
        assert!(a.resolve(&doc, "t999.r1").is_none());
    }
}

#[test]
fn minting_is_stable_across_loads() {
    for (label, path) in all_docs() {
        let (d1, d2) = (open(&path), open(&path));
        let (a1, a2) = (Anchors::mint(&d1), Anchors::mint(&d2));
        assert!(handle_lists(&d1, &a1) == handle_lists(&d2, &a2), "{label}: handles differ between loads");
        assert!(others(&a1) == others(&a2), "{label}: table/image anchors differ between loads");
        assert!(a1.sidecar(&d1) == a2.sidecar(&d2), "{label}: sidecars differ between loads");
    }
}

#[test]
fn save_and_reopen_with_sidecar_keeps_handles() {
    for (label, path) in all_docs() {
        let mut doc = open(&path);
        let a = Anchors::mint(&doc);
        let json = a.sidecar(&doc).to_json();
        let saved = doc.save().expect("save");
        let doc2 = WordDoc::open(saved).expect("reopen");
        let sc = Sidecar::from_json(&json).expect("sidecar parses");
        let b = Anchors::attach(&doc2, &sc).expect("attach");
        assert!(handle_lists(&doc, &a) == handle_lists(&doc2, &b), "{label}: handles changed across save/re-open");
        assert!(others(&a) == others(&b), "{label}: table/image anchors changed across save/re-open");
        assert!(b.sidecar(&doc2).to_json() == json, "{label}: sidecar changed across save/re-open");
    }
}

/// Insert `<w:p><w:r><w:t>…</w:t></w:r></w:p>` after the middle top-level body paragraph.
/// Returns the index (in `paragraphs()` order) the new paragraph lands at.
fn insert_paragraph(doc: &mut WordDoc, text: &str) -> Option<usize> {
    let body = doc.stories()[0].clone();
    let d = doc.part(body.part);
    let top: Vec<_> = blocks(d, body.root)
        .into_iter()
        .filter_map(|b| if let Block::Paragraph(p) = b { Some(p) } else { None })
        .filter(|p| d.parent(p.0) == Some(body.root))
        .collect();
    let after = *top.get(top.len() / 2)?;
    let pos = paragraphs(d, body.root).iter().position(|p| *p == after)? + 1;
    let d = doc.part_mut(body.part);
    let p = d.create_element("w:p").unwrap();
    let r = d.create_element("w:r").unwrap();
    let t = d.create_element("w:t").unwrap();
    let tx = d.create_text(text).unwrap();
    d.append_child(t, tx).unwrap();
    d.append_child(r, t).unwrap();
    d.append_child(p, r).unwrap();
    d.insert_after(after.0, p).unwrap();
    Some(pos)
}

#[test]
fn inserted_paragraph_keeps_old_handles() {
    for (label, path) in all_docs() {
        let mut doc = open(&path);
        let mut a = Anchors::mint(&doc);
        let before = handle_lists(&doc, &a);
        let old_sidecar = a.sidecar(&doc);
        let Some(pos) = insert_paragraph(&mut doc, "Quillwork inserted this paragraph for a test.") else { continue };

        // In session: node identity.
        a.sync(&doc);
        let after = handle_lists(&doc, &a);
        let mut want = before.clone();
        let new_handle = after[0][pos].clone();
        assert!(!before[0].contains(&new_handle), "{label}: new paragraph reused {new_handle}");
        want[0].insert(pos, new_handle.clone());
        assert!(after == want, "{label}: in-session sync changed old handles");

        // Outside the session: save, re-open, re-attach with the sidecar from before the edit.
        let saved = doc.save().expect("save");
        let doc2 = WordDoc::open(saved.clone()).expect("reopen");
        let b = Anchors::attach(&doc2, &old_sidecar).expect("attach");
        let re = handle_lists(&doc2, &b);
        for (si, (old, now)) in before.iter().zip(&re).enumerate() {
            let mut now = now.clone();
            if si == 0 {
                let fresh = now.remove(pos);
                assert!(!old.contains(&fresh), "{label}: re-attach gave the new paragraph an old handle {fresh}");
            }
            assert!(*old == now, "{label}: re-attach changed old handles in story {si}");
        }

        // And with the post-edit sidecar: exactly the session's handles.
        let doc3 = WordDoc::open(saved).expect("reopen");
        let c = Anchors::attach(&doc3, &a.sidecar(&doc)).expect("attach");
        assert!(handle_lists(&doc3, &c) == after, "{label}: post-edit sidecar re-attach differs");
    }
}

#[test]
fn deleted_handles_are_never_reused() {
    let body = (0..6).map(|i| wml_support::p(&format!("para {i}"), None, None)).collect::<String>();
    let mut doc = wml_support::mini(&body, None, None);
    let mut a = Anchors::mint(&doc);
    let hs: Vec<String> = a.handles(0).map(|(h, _)| h.to_owned()).collect();
    assert_eq!(hs, ["b100", "b101", "b102", "b103", "b104", "b105"]);
    let (_, loc) = a.handles(0).nth(5).unwrap();
    doc.part_mut(loc.part).remove(loc.node).unwrap();
    a.sync(&doc);
    assert!(a.resolve(&doc, "b105").is_none());
    // Round-trip the sidecar, then add a paragraph: it gets b106, not b105.
    let sc = Sidecar::from_json(&a.sidecar(&doc).to_json()).unwrap();
    let main = doc.main_part();
    let root = doc.stories()[0].root;
    let d = doc.part_mut(main);
    let p = d.create_element("w:p").unwrap();
    d.append_child(root, p).unwrap();
    let a2 = Anchors::attach(&doc, &sc).unwrap();
    let hs: Vec<String> = a2.handles(0).map(|(h, _)| h.to_owned()).collect();
    assert_eq!(hs, ["b100", "b101", "b102", "b103", "b104", "b106"]);
}

#[test]
fn reattach_follows_moves_and_edits_by_text() {
    let texts = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    let body: String = texts.iter().map(|t| wml_support::p(t, None, None)).collect();
    let doc = wml_support::mini(&body, None, None);
    let sc = Anchors::mint(&doc).sidecar(&doc);
    // New version: delta moved first, bravo edited, echo deleted, a new paragraph at the end.
    let texts2 = ["delta", "alpha", "bravo two", "charlie", "foxtrot", "golf"];
    let body2: String = texts2.iter().map(|t| wml_support::p(t, None, None)).collect();
    let doc2 = wml_support::mini(&body2, None, None);
    let a = Anchors::attach(&doc2, &sc).unwrap();
    let hs: Vec<&str> = a.handles(0).map(|(h, _)| h).collect();
    // delta b103, alpha b100, bravo two (same slot between alpha and charlie) b101,
    // charlie b102, foxtrot b105, golf new b106.
    assert_eq!(hs, ["b103", "b100", "b101", "b102", "b105", "b106"]);
}

#[test]
fn para_ids_win_over_text() {
    let mk = |pairs: &[(&str, &str)]| -> String {
        pairs.iter().map(|(id, t)| format!(r#"<w:p w14:paraId="{id}"><w:r><w:t>{t}</w:t></w:r></w:p>"#)).collect()
    };
    let w14 = r#" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#;
    let wrap = |b: String| {
        format!(r#"<w:sdt><w:sdtContent>{}</w:sdtContent></w:sdt>"#, b).replace("<w:p ", &format!("<w:p{w14} "))
    };
    let doc =
        wml_support::mini(&wrap(mk(&[("00000001", "same"), ("00000002", "same"), ("00000003", "x")])), None, None);
    let sc = Anchors::mint(&doc).sidecar(&doc);
    // Swap the two identical-text paragraphs by id and rewrite the third's text entirely.
    let doc2 = wml_support::mini(
        &wrap(mk(&[("00000002", "same"), ("00000001", "same"), ("00000003", "totally new")])),
        None,
        None,
    );
    let a = Anchors::attach(&doc2, &sc).unwrap();
    let hs: Vec<&str> = a.handles(0).map(|(h, _)| h).collect();
    assert_eq!(hs, ["b101", "b100", "b102"]);
}

#[test]
fn ensure_para_id_stamps_w14() {
    let body: String = (0..3).map(|i| wml_support::p(&format!("p{i}"), None, None)).collect();
    let mut doc = wml_support::mini(&body, None, None);
    let mut a = Anchors::mint(&doc);
    assert!(doc.modified_parts().is_empty());
    let id = a.ensure_para_id(&mut doc, "b101").unwrap();
    assert!(id != 0 && id < 0x8000_0000);
    assert_eq!(a.ensure_para_id(&mut doc, "b101").unwrap(), id, "idempotent");
    assert!(a.ensure_para_id(&mut doc, "t1").is_err());
    let id2 = a.ensure_para_id(&mut doc, "b102").unwrap();
    assert_ne!(id, id2);

    let mods = doc.modified_parts();
    assert_eq!(mods.len(), 1);
    let xml = String::from_utf8(mods[0].1.clone()).unwrap();
    let x = roxmltree::Document::parse(&xml).unwrap();
    let root = x.root_element();
    let w14 = "http://schemas.microsoft.com/office/word/2010/wordml";
    assert!(root.namespaces().any(|n| n.uri() == w14 && n.name() == Some("w14")), "w14 declared on the root");
    let ign = root
        .attribute(("http://schemas.openxmlformats.org/markup-compatibility/2006", "Ignorable"))
        .expect("mc:Ignorable");
    assert!(ign.split_whitespace().any(|p| p == "w14"));
    let ps: Vec<_> = x.descendants().filter(|n| n.has_tag_name((wml_support::W, "p"))).collect();
    let hex = Regex::new("^[0-9A-F]{8}$").unwrap();
    assert!(ps[0].attribute((w14, "paraId")).is_none(), "only the asked paragraphs are stamped");
    for p in &ps[1..] {
        let pid = p.attribute((w14, "paraId")).unwrap();
        let tid = p.attribute((w14, "textId")).unwrap();
        assert!(hex.is_match(pid) && hex.is_match(tid));
        assert!(u32::from_str_radix(pid, 16).unwrap() < 0x8000_0000);
    }
    assert_eq!(ps[1].attribute((w14, "paraId")), Some(format!("{id:08X}").as_str()));

    // The stamped id travels in the sidecar and keeps the handle through a text rewrite.
    a.sync(&doc);
    let sc = a.sidecar(&doc);
    assert_eq!(sc.blocks["b101"].para_id.as_deref(), Some(format!("{id:08X}").as_str()));
}

#[test]
fn story_numbers_are_document_wide() {
    for (label, path) in all_docs() {
        let doc = open(&path);
        let a = Anchors::mint(&doc);
        let mut next: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        for (si, st) in doc.stories().iter().enumerate() {
            let id = a.story_id(si).unwrap();
            let p = st.prefix();
            if p == "b" {
                assert_eq!(id, "b");
                continue;
            }
            let n = next.entry(p).or_insert(1);
            let want = format!("{p}{n}");
            assert_eq!(id, want, "{label}: story numbering");
            *n += 1;
        }
    }
}
