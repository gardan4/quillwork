//! The Word model over the corpora: loading, enumeration, fidelity, text against an independent
//! roxmltree extraction, numbering coverage, and the oracle fixtures' expectations.
//!
//! The private corpus holds real client documents: only counts are printed, files are labelled
//! `private#<i>`, and assertions never print document text.

mod xml_support;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use quillwork_core::anchors::Anchors;
use quillwork_core::wml::view::{
    bookmarks, comment_markers, deep_blocks, drawings, paragraph_texts, paragraphs, revisions, sdts, tables,
};
use quillwork_core::wml::{Block, Numbering, RevisionKind, StoryKind, TextMode, WordDoc, fields};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

fn corpus() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = xml_support::synthetic_docx()
        .into_iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    for (i, p) in xml_support::private_docx().into_iter().enumerate() {
        v.push((format!("private#{i}"), p));
    }
    v
}

fn fixtures() -> Vec<(String, PathBuf, serde_json::Value)> {
    let dir = xml_support::workspace_root().join("corpus/fixtures");
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .filter_map(|p| {
            let exp = p.with_extension("expect.json");
            let exp: serde_json::Value = serde_json::from_slice(&std::fs::read(&exp).ok()?).ok()?;
            Some((p.file_name().unwrap().to_string_lossy().into_owned(), p, exp))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Every zip entry, name → bytes.
fn entries(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    let mut out = HashMap::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).expect("entry");
        if e.is_dir() {
            continue;
        }
        let name = e.name().expect("name").into_owned();
        let mut b = Vec::new();
        e.read_to_end(&mut b).expect("inflate");
        out.insert(name, b);
    }
    out
}

// ------------------------------------------------------------------ independent text oracle

fn is_props(l: &str) -> bool {
    matches!(
        l,
        "pPr"
            | "rPr"
            | "sdtPr"
            | "sdtEndPr"
            | "sectPr"
            | "tblPr"
            | "trPr"
            | "tcPr"
            | "tblGrid"
            | "tblPrEx"
            | "customXmlPr"
            | "smartTagPr"
            | "fldData"
    )
}

fn ignored_mc(n: roxmltree::Node) -> bool {
    let ns = n.tag_name().namespace();
    if ns != Some(MC) {
        return false;
    }
    let first_choice = n
        .parent()
        .and_then(|p| p.children().find(|c| c.tag_name().namespace() == Some(MC) && c.tag_name().name() == "Choice"));
    match n.tag_name().name() {
        "Fallback" => first_choice.is_some(),
        "Choice" => first_choice != Some(n),
        _ => false,
    }
}

fn oracle_text(n: roxmltree::Node, mode: TextMode, out: &mut String) {
    for c in n.children().filter(|c| c.is_element()) {
        if ignored_mc(c) {
            continue;
        }
        let (ns, l) = (c.tag_name().namespace(), c.tag_name().name());
        if ns == Some(M) && l == "t" {
            out.push_str(c.text().unwrap_or(""));
            continue;
        }
        if ns != Some(W) {
            oracle_text(c, mode, out);
            continue;
        }
        match l {
            "ins" | "moveTo" if mode == TextMode::RejectAll => {}
            "del" | "moveFrom" if mode == TextMode::AcceptAll => {}
            "instrText" | "delInstrText" | "txbxContent" | "drawing" | "pict" | "object" => {}
            l if is_props(l) => {}
            "t" => out.push_str(&c.children().filter_map(|t| t.text()).collect::<String>()),
            "delText" => {
                if mode != TextMode::AcceptAll {
                    out.push_str(&c.children().filter_map(|t| t.text()).collect::<String>());
                }
            }
            "tab" | "ptab" => out.push('\t'),
            "br" | "cr" => out.push('\n'),
            "noBreakHyphen" => out.push('\u{2011}'),
            "softHyphen" => out.push('\u{00AD}'),
            "sym" => {
                if let Some(ch) = c
                    .attribute((W, "char"))
                    .and_then(|v| u32::from_str_radix(v.trim(), 16).ok())
                    .and_then(char::from_u32)
                {
                    out.push(ch);
                }
            }
            _ => oracle_text(c, mode, out),
        }
    }
}

/// w:p start offset → (accept, reject) text, for every w:p in the part.
fn oracle_paragraphs(xml: &str) -> HashMap<usize, (String, String)> {
    let opts = roxmltree::ParsingOptions { nodes_limit: u32::MAX, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(xml, opts).expect("roxmltree parses");
    let mut out = HashMap::new();
    for n in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().namespace() == Some(W) && n.tag_name().name() == "p")
    {
        let mut a = String::new();
        let mut r = String::new();
        oracle_text(n, TextMode::AcceptAll, &mut a);
        oracle_text(n, TextMode::RejectAll, &mut r);
        out.insert(n.range().start, (a, r));
    }
    out
}

// ------------------------------------------------------------------ per-file walk

#[derive(Default, Debug)]
struct Counts {
    stories: usize,
    blocks: usize,
    paragraphs: usize,
    tables: usize,
    fields: usize,
    revisions: usize,
    comments: usize,
    drawings: usize,
    numbered: usize,
    labelled: usize,
    oracle_checked: usize,
}

fn check(label: &str, bytes: Vec<u8>) -> Counts {
    let orig = entries(&bytes);
    let mut doc = WordDoc::open(bytes).unwrap_or_else(|e| panic!("{label}: open failed: {e}"));
    let mut c = Counts::default();
    let num = Numbering::load(&doc);

    let stories = doc.stories().to_vec();
    c.stories = stories.len();
    let mut oracles: HashMap<String, HashMap<usize, (String, String)>> = HashMap::new();
    for st in &stories {
        let d = doc.part(st.part);
        c.blocks += deep_blocks(d, st.root).len();
        let paras = paragraphs(d, st.root);
        c.paragraphs += paras.len();
        c.tables += tables(d, st.root).len();
        c.fields += fields(d, st.root).len();
        c.revisions += revisions(d, st.root).len();
        c.drawings += drawings(d, st.root).len();
        let _ = (bookmarks(d, st.root), comment_markers(d, st.root), sdts(d, st.root));
        if matches!(st.kind, StoryKind::Comment { .. }) {
            c.comments += 1;
        }
        for (p, l) in num.labels(&doc, st) {
            if num.effective(d, p).is_some() {
                c.numbered += 1;
                if l.is_some() {
                    c.labelled += 1;
                }
            }
        }
        // Text against the roxmltree oracle, matched by source offset.
        let name = doc.part_name(st.part).to_owned();
        let oracle = oracles.entry(name.clone()).or_insert_with(|| {
            let raw = &orig[&name];
            oracle_paragraphs(std::str::from_utf8(raw).expect("utf-8 part"))
        });
        for (i, p) in paras.iter().enumerate() {
            let span = d.source_span(p.0).expect("parsed paragraph has a span");
            let (a, r) =
                oracle.get(&span.start).unwrap_or_else(|| panic!("{label}: paragraph {i} not found by oracle"));
            assert!(
                p.text(d, TextMode::AcceptAll) == *a,
                "{label}: accept text differs (story {:?}, para {i})",
                st.prefix()
            );
            assert!(
                p.text(d, TextMode::RejectAll) == *r,
                "{label}: reject text differs (story {:?}, para {i})",
                st.prefix()
            );
            c.oracle_checked += 1;
        }
    }

    // Minting changes nothing; saving writes every part back byte for byte.
    let _anchors = Anchors::mint(&doc);
    assert!(doc.modified_parts().is_empty(), "{label}: load + mint modified parts");
    let saved = doc.save().unwrap_or_else(|e| panic!("{label}: save failed: {e}"));
    let after = entries(&saved);
    assert_eq!(orig.len(), after.len(), "{label}: part count changed");
    for (name, b) in &orig {
        assert!(after.get(name).is_some_and(|x| x == b), "{label}: part {name} not byte-identical");
    }
    c
}

#[test]
fn corpus_loads_enumerates_and_round_trips() {
    let files = corpus();
    assert!(!files.is_empty(), "no corpus files");
    if xml_support::private_dir().is_none() {
        eprintln!("private corpus absent: synthetic only");
    }
    let mut total = Counts::default();
    for (label, path) in &files {
        let c = check(label, std::fs::read(path).expect("read"));
        eprintln!(
            "{label}: stories={} blocks={} paragraphs={} tables={} fields={} revisions={} comments={} drawings={} numbered={} labelled={} oracle_checked={}",
            c.stories,
            c.blocks,
            c.paragraphs,
            c.tables,
            c.fields,
            c.revisions,
            c.comments,
            c.drawings,
            c.numbered,
            c.labelled,
            c.oracle_checked
        );
        assert_eq!(c.numbered, c.labelled, "{label}: numbered paragraphs without a label");
        total.numbered += c.numbered;
        total.labelled += c.labelled;
    }
    eprintln!("total numbered={} labelled={}", total.numbered, total.labelled);
}

#[test]
fn fixtures_round_trip_and_match_oracle_text() {
    for (name, path, exp) in fixtures() {
        if exp["valid"] != serde_json::Value::Bool(true) {
            continue;
        }
        check(&name, std::fs::read(&path).expect("read"));
    }
}

#[test]
fn largest_file_load_time() {
    let Some((label, path)) =
        corpus().into_iter().max_by_key(|(_, p)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
    else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read");
    let size = bytes.len();
    let t = Instant::now();
    let doc = WordDoc::open(bytes).expect("open");
    let stories = doc.stories().len();
    let anchors = Anchors::mint(&doc);
    let el = t.elapsed();
    eprintln!(
        "largest: {label} {size} bytes, open+stories+mint {:.1} ms ({} stories, {} body handles){}",
        el.as_secs_f64() * 1000.0,
        stories,
        anchors.handles(0).count(),
        if cfg!(debug_assertions) { " [debug build]" } else { " [release build]" }
    );
}

// ------------------------------------------------------------------ oracle fixtures

fn strs(v: &serde_json::Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|s| s.as_str().unwrap_or_default().to_owned()).collect()).unwrap_or_default()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn open(path: &Path) -> WordDoc {
    WordDoc::open(std::fs::read(path).expect("read")).expect("open")
}

#[test]
fn fixtures_match_expectations() {
    let mut checked = 0;
    for (name, path, exp) in fixtures() {
        if exp["valid"] != serde_json::Value::Bool(true) {
            continue;
        }
        let doc = open(&path);
        let body = &doc.stories()[0];
        let d = doc.part(body.part);
        let paras = paragraphs(d, body.root);
        if let Some(n) = exp["paragraph_count"].as_u64() {
            assert_eq!(paras.len() as u64, n, "{name}: paragraph count");
        }
        for (key, mode) in [("accept", TextMode::AcceptAll), ("reject", TextMode::RejectAll)] {
            let want = strs(&exp["body"][key]);
            assert_eq!(paragraph_texts(d, body.root, mode), want, "{name}: body {key} text");
        }
        assert_eq!(strs(&exp["body"]["markup"]).len(), paras.len(), "{name}: markup paragraph count");

        if let Some(want) = exp["numbering"].as_array() {
            let num = Numbering::load(&doc);
            let got: Vec<serde_json::Value> = paras
                .iter()
                .map(|&p| match num.effective(d, p) {
                    Some(e) => serde_json::json!([e.num_id, e.ilvl]),
                    None => serde_json::Value::Null,
                })
                .collect();
            assert_eq!(&got, want, "{name}: numbering");
            // The style cascade agrees.
            if let Some(ss) = doc.style_sheet().expect("styles parse") {
                for (i, &p) in paras.iter().enumerate() {
                    let eff = ss.resolve_paragraph_at(d, p.0);
                    let theirs = eff.numbering.map(|n| (i64::from(n.num_id), usize::from(n.ilvl)));
                    let mine = num.effective(d, p).map(|e| (e.num_id, e.ilvl));
                    assert_eq!(theirs, mine, "{name}: numbering vs style cascade, paragraph {i}");
                }
            }
        }
        if let Some(n) = exp["tables"].as_u64() {
            assert_eq!(tables(d, body.root).len() as u64, n, "{name}: tables");
        }
        if let Some(want) = exp["fields"].as_array() {
            let mut got: Vec<String> = Vec::new();
            for st in doc.stories() {
                got.extend(fields(doc.part(st.part), st.root).into_iter().map(|f| norm(&f.instr)));
            }
            let mut want: Vec<String> = want.iter().map(|v| norm(v.as_str().unwrap())).collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "{name}: fields");
        }
        if let Some(want) = exp["bookmarks"].as_array() {
            let got: Vec<String> =
                bookmarks(d, body.root).into_iter().filter_map(|b| b.name(d)).filter(|n| n != "_GoBack").collect();
            let want: Vec<String> = want.iter().map(|v| v.as_str().unwrap().to_owned()).collect();
            assert_eq!(got, want, "{name}: bookmarks");
        }
        if let Some(want) = exp["comments"].as_array() {
            let ids: Vec<i64> = doc
                .stories()
                .iter()
                .filter_map(|s| if let StoryKind::Comment { id } = s.kind { Some(id) } else { None })
                .collect();
            let want: Vec<i64> = want.iter().map(|c| c["id"].as_str().unwrap().parse().unwrap()).collect();
            assert_eq!(ids, want, "{name}: comment stories");
            for (s, c) in doc
                .stories()
                .iter()
                .filter(|s| matches!(s.kind, StoryKind::Comment { .. }))
                .zip(exp["comments"].as_array().unwrap())
            {
                let t = paragraph_texts(doc.part(s.part), s.root, TextMode::AsIs).join("\n");
                assert_eq!(t, c["text"].as_str().unwrap(), "{name}: comment text");
            }
        }
        if let Some(rev) = exp["revisions"].as_object() {
            let mut got: HashMap<&str, u64> = HashMap::new();
            for r in revisions(d, body.root) {
                let k = match r.kind {
                    RevisionKind::Ins | RevisionKind::ParaMarkIns => "ins",
                    RevisionKind::Del | RevisionKind::ParaMarkDel => "del",
                    RevisionKind::MoveFrom => "moveFrom",
                    RevisionKind::MoveTo => "moveTo",
                    RevisionKind::ParaProps => "pPrChange",
                    RevisionKind::RunProps => "rPrChange",
                    _ => "other",
                };
                *got.entry(k).or_default() += 1;
            }
            for k in ["ins", "del", "moveFrom", "moveTo", "pPrChange", "rPrChange"] {
                assert_eq!(got.get(k).copied().unwrap_or(0), rev[k].as_u64().unwrap(), "{name}: revisions {k}");
            }
        }
        if let Some(img) = exp["images"].as_object() {
            let mut inline = 0;
            let mut anchor = 0;
            for st in doc.stories() {
                let sd = doc.part(st.part);
                for dr in drawings(sd, st.root) {
                    if sd.descendants(dr.0).any(|n| sd.local_name(n) == Some("inline")) {
                        inline += 1;
                    } else if sd.descendants(dr.0).any(|n| sd.local_name(n) == Some("anchor")) {
                        anchor += 1;
                    }
                }
            }
            assert_eq!(inline, img["inline"].as_u64().unwrap(), "{name}: inline drawings");
            assert_eq!(anchor, img["anchor"].as_u64().unwrap(), "{name}: anchored drawings");
        }
        if let Some(n) = exp["sdt"].as_u64() {
            assert_eq!(sdts(d, body.root).len() as u64, n, "{name}: content controls");
        }
        if let Some(stories) = exp["stories"].as_object() {
            for (key, want) in stories {
                let want = strs(want);
                let (part, sel) = key.split_once('#').map_or((key.as_str(), None), |(p, s)| (p, Some(s)));
                let found = doc.stories().iter().find(|s| {
                    doc.part_name(s.part) == part
                        && match (sel, &s.kind) {
                            (None, StoryKind::Header { .. } | StoryKind::Footer { .. }) => true,
                            (Some("textbox"), StoryKind::TextBox { .. }) => true,
                            (Some(id), StoryKind::Footnote { id: x } | StoryKind::Endnote { id: x }) => {
                                id.parse() == Ok(*x)
                            }
                            _ => false,
                        }
                });
                let s = found.unwrap_or_else(|| panic!("{name}: story {key} not found"));
                assert_eq!(paragraph_texts(doc.part(s.part), s.root, TextMode::AsIs), want, "{name}: story {key}");
            }
        }
        // Blocks enumerate without panicking.
        let _ = deep_blocks(d, body.root).iter().filter(|b| matches!(b, Block::Table(_))).count();
        checked += 1;
    }
    assert!(checked >= 17, "only {checked} fixtures checked");
}

#[test]
fn invalid_fixtures_fail_cleanly() {
    let dir = xml_support::workspace_root().join("corpus/fixtures/invalid");
    let mut n = 0;
    for e in std::fs::read_dir(dir).expect("invalid dir") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "docx") {
            // Either an error or a document; never a panic.
            if let Ok(doc) = WordDoc::open(std::fs::read(&p).expect("read")) {
                let _ = Anchors::mint(&doc);
            }
            n += 1;
        }
    }
    assert!(n > 0);
}
