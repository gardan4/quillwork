//! The ops engine end to end: per-op edits on the fixtures and synthetic documents, locality,
//! formatting preservation, change-set accept/reject round trips, and transaction semantics
//! (stale anchors, `expect`, rollback, versions, limits, sidecar persistence).
//!
//! The private corpus half prints counts by document index only, never names or text.

mod xml_support;

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use quillwork_core::anchors::Target;
use quillwork_core::engine::{Engine, sha256_hex};
use quillwork_core::ops::{self, handlers, render::accepted_text};
use quillwork_core::validate;
use quillwork_core::wml::text::{self, TextMode};
use quillwork_core::wml::{StoryKind, WordDoc};
use quillwork_core::xml::ns;
use serde_json::{Value, json};

const NOW: i64 = 1_793_016_000; // 2026-10-26T12:00:00Z

fn fixture(name: &str) -> PathBuf {
    xml_support::workspace_root().join("corpus/fixtures").join(name)
}

/// Fixtures 01–18 (17 is the .docm) plus the synthetic documents.
fn public_docs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(xml_support::workspace_root().join("corpus/fixtures"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .collect();
    v.sort();
    v.extend(xml_support::synthetic_docx());
    v
}

fn label(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

fn engine(bytes: Vec<u8>) -> Engine {
    let mut e = Engine::open(bytes, None).unwrap();
    e.set_now(Some(NOW));
    e.set_agent("Quillwork Test");
    e
}

fn tx(e: &mut Engine, ops: Value) -> Result<Value, Value> {
    let v = e.version();
    e.apply_tx(&json!({"base_version": v, "ops": ops}))
}

fn code(err: &Value) -> &str {
    err["error"]["code"].as_str().unwrap()
}

fn body_story(doc: &WordDoc) -> usize {
    doc.stories().iter().position(|s| matches!(s.kind, StoryKind::Body)).unwrap()
}

/// Body block handles with their accepted text, paragraphs only (tables skipped).
fn body(e: &Engine) -> Vec<(String, String)> {
    let doc = e.doc();
    e.anchors()
        .handles(body_story(doc))
        .filter_map(|(h, _)| match e.anchors().resolve(doc, h) {
            Some(Target::Block { para, .. }) => Some((h.to_owned(), accepted_text(doc.part(para.part), para.node))),
            _ => None,
        })
        .collect()
}

fn text_of(e: &Engine, handle: &str) -> String {
    match e.anchors().resolve(e.doc(), handle) {
        Some(Target::Block { para, .. }) => accepted_text(e.doc().part(para.part), para.node),
        other => panic!("{handle}: {other:?}"),
    }
}

/// Main-part text, empty lines dropped.
fn main_text(doc: &WordDoc, mode: TextMode) -> String {
    let d = doc.part(doc.main_part());
    text::text(d, d.root(), mode).lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
}

/// A word of ≥4 letters that occurs once in `t`.
fn unique_word(t: &str) -> Option<String> {
    t.split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 4)
        .find(|w| t.matches(*w).count() == 1)
        .map(str::to_owned)
}

/// A body paragraph (not the first or last block) with a unique word, plus that word.
fn pick(e: &Engine, skip: &[String]) -> Option<(String, String)> {
    let b = body(e);
    let n = b.len();
    b.into_iter()
        .enumerate()
        .filter(|(i, (h, _))| *i > 0 && *i + 1 < n && !skip.contains(h))
        .find_map(|(_, (h, t))| unique_word(&t).map(|w| (h, w)))
}

fn zip_part(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut e = z.by_name(name).ok()?;
    let mut buf = Vec::new();
    e.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn zip_xml_parts(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let name = e.name().unwrap().into_owned();
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            out.push((name, buf));
        }
    }
    out.sort();
    out
}

/// Byte slices of every outermost `w:p` in a part.
fn para_slices(xml: &[u8]) -> Vec<Vec<u8>> {
    let s = std::str::from_utf8(xml).unwrap();
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let off = xml.len() - s.len();
    let d =
        roxmltree::Document::parse_with_options(s, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
            .unwrap();
    d.descendants()
        .filter(|n| n.tag_name().namespace() == Some(ns::W) && n.tag_name().name() == "p")
        .filter(|n| {
            !n.ancestors().skip(1).any(|a| a.tag_name().namespace() == Some(ns::W) && a.tag_name().name() == "p")
        })
        .map(|n| xml[off + n.range().start..off + n.range().end].to_vec())
        .collect()
}

/// Paragraphs byte-identical at both ends (longest common prefix + suffix), and the original
/// paragraph count.
fn untouched(orig: &[u8], out: &[u8]) -> (usize, usize) {
    let a = para_slices(orig);
    let b = para_slices(out);
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let max_suf = a.len().min(b.len()) - pre;
    let suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    (pre + suf, a.len())
}

/// Original paragraphs that survive byte-identical anywhere in the output (multiset match).
fn preserved(orig: &[u8], out: &[u8]) -> usize {
    let mut pool: std::collections::HashMap<Vec<u8>, usize> = std::collections::HashMap::new();
    for p in para_slices(out) {
        *pool.entry(p).or_default() += 1;
    }
    para_slices(orig)
        .into_iter()
        .filter(|p| match pool.get_mut(p) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        })
        .count()
}

/// Every non-main XML part identical, except `allowed` ones.
fn other_parts_identical(orig: &[u8], out: &[u8], allowed: &[&str]) -> Vec<String> {
    let after: std::collections::HashMap<String, Vec<u8>> = zip_xml_parts(out).into_iter().collect();
    zip_xml_parts(orig)
        .into_iter()
        .filter(|(n, _)| n != "word/document.xml" && !allowed.contains(&n.as_str()))
        .filter(|(n, b)| after.get(n) != Some(b))
        .map(|(n, _)| n)
        .collect()
}

/// Revision / annotation ids and paraIds stay below 0x80000000.
fn ids_in_range(bytes: &[u8]) -> bool {
    let xml = zip_part(bytes, "word/document.xml").unwrap();
    let s = String::from_utf8_lossy(&xml);
    let re_id = regex::Regex::new(r#"\bw:id="(-?\d+)""#).unwrap();
    let re_pid = regex::Regex::new(r#"w14:(?:paraId|textId)="([0-9A-Fa-f]+)""#).unwrap();
    re_id.captures_iter(&s).all(|c| c[1].parse::<i64>().is_ok_and(|v| (0..0x8000_0000).contains(&v)))
        && re_pid.captures_iter(&s).all(|c| u32::from_str_radix(&c[1], 16).is_ok_and(|v| v < 0x8000_0000))
}

fn error_codes(r: &validate::Report) -> Vec<String> {
    let mut v: Vec<String> = r.errors().map(|f| f.code.clone()).collect();
    v.sort();
    v
}

/// Saved bytes re-validate with no error the original did not already have.
fn no_new_errors(orig: &[u8], out: &[u8]) -> bool {
    let before = error_codes(&validate::check_bytes(orig));
    error_codes(&validate::check_bytes(out)).iter().all(|c| before.contains(c))
}

// ---------------------------------------------------------------------- resolved views

/// The text of one `w:p` from saved XML: `w:t` and tabs, text boxes and fallbacks skipped.
fn xml_para_text(p: roxmltree::Node) -> String {
    let mut out = String::new();
    for n in p.descendants() {
        if n.ancestors()
            .skip(1)
            .take_while(|a| *a != p)
            .any(|a| matches!(a.tag_name().name(), "txbxContent" | "Fallback" | "p"))
        {
            continue;
        }
        match n.tag_name().name() {
            "t" if n.tag_name().namespace() == Some(ns::W) => out.push_str(n.text().unwrap_or("")),
            "tab" if n.parent().is_some_and(|r| r.tag_name().name() == "r") => out.push('\t'),
            _ => {}
        }
    }
    out
}

fn xml_blocks(container: roxmltree::Node, out: &mut Vec<String>) {
    for c in container.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "p" => out.push(xml_para_text(c)),
            "tbl" => {
                let cells: Vec<String> =
                    c.descendants().filter(|n| n.tag_name().name() == "p").map(xml_para_text).collect();
                out.push(format!("[table {}]", cells.join(" | ")));
            }
            "sdt" | "sdtContent" | "customXml" => xml_blocks(c, out),
            _ => {}
        }
    }
}

/// The body as a list of blocks: a paragraph's text, or a table with its cell paragraphs.
fn blocks(bytes: &[u8]) -> Vec<String> {
    let xml = String::from_utf8(zip_part(bytes, "word/document.xml").unwrap()).unwrap();
    let d = roxmltree::Document::parse_with_options(
        &xml,
        roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() },
    )
    .unwrap();
    let body = d.descendants().find(|n| n.tag_name().name() == "body").unwrap();
    let mut out = Vec::new();
    xml_blocks(body, &mut out);
    out
}

/// The saved package with every revision accepted (`"accept"`) or rejected (`"reject"`).
fn resolved(e: &mut Engine, op: &str) -> Vec<u8> {
    let (bytes, side) = e.save().unwrap();
    let mut c = Engine::open(bytes.clone(), Some(&side)).unwrap();
    c.set_now(Some(NOW));
    c.set_agent(ME);
    match tx(&mut c, json!([{"op": op, "all": true}])) {
        Ok(_) => c.save().unwrap().0,
        Err(_) => bytes,
    }
}

fn view(e: &mut Engine, op: &str) -> Vec<String> {
    blocks(&resolved(e, op))
}

/// `after` is `before` with exactly one block changed, to `want(old block)`.
fn one_block_changed(before: &[String], after: &[String], want: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!("block count {} -> {}", before.len(), after.len()));
    }
    let diff: Vec<usize> = (0..before.len()).filter(|&i| before[i] != after[i]).collect();
    match diff.as_slice() {
        [i] if want(&before[*i]).as_deref() == Some(after[*i].as_str()) => Ok(()),
        [i] => Err(format!("block {i} has the wrong accepted text")),
        _ => Err(format!("{} blocks changed", diff.len())),
    }
}

/// `after` is `before` with `lines` inserted right after a block whose text is `anchor`.
fn blocks_inserted(before: &[String], after: &[String], lines: &[String], anchor: &str) -> Result<(), String> {
    if after.len() != before.len() + lines.len() {
        return Err(format!("block count {} -> {}, want +{}", before.len(), after.len(), lines.len()));
    }
    let ok = (1..=before.len()).any(|j| {
        norm(&before[j - 1]) == norm(anchor)
            && after[..j] == before[..j]
            && after[j..j + lines.len()] == *lines
            && after[j + lines.len()..] == before[j..]
    });
    if ok { Ok(()) } else { Err("inserted paragraphs not where intended".into()) }
}

/// `after` is `before` without one block whose text is `gone`.
fn block_removed(before: &[String], after: &[String], gone: &str) -> Result<(), String> {
    if after.len() + 1 != before.len() {
        return Err(format!("block count {} -> {}, want -1", before.len(), after.len()));
    }
    let ok = (0..before.len())
        .any(|j| norm(&before[j]) == norm(gone) && after[..j] == before[..j] && after[j..] == before[j + 1..]);
    if ok { Ok(()) } else { Err("the deleted paragraph is not the one removed".into()) }
}

// ---------------------------------------------------------------------- per-op, every document

/// Top-level body paragraphs (not the first or last block) with a unique word that does not start the
/// paragraph, plus that word and the directly following block when it is a paragraph.
fn candidates(e: &Engine) -> Vec<(String, String, Option<String>)> {
    let doc = e.doc();
    let blocks: Vec<(String, Option<String>)> = e
        .anchors()
        .handles(body_story(doc))
        .map(|(h, _)| match e.anchors().resolve(doc, h) {
            Some(Target::Block { para, .. }) => {
                let d = doc.part(para.part);
                let top = d.parent(para.node).is_some_and(|p| d.is(p, ns::W, "body"));
                (h.to_owned(), top.then(|| accepted_text(d, para.node)))
            }
            _ => (h.to_owned(), None),
        })
        .collect();
    let n = blocks.len();
    let mut out = Vec::new();
    for i in 1..n.saturating_sub(1) {
        let (h, t) = &blocks[i];
        let Some(t) = t else { continue };
        let lead = t.trim_start();
        let Some(w) = t
            .split(|c: char| !c.is_alphabetic())
            .filter(|w| w.chars().count() >= 4 && !lead.starts_with(*w))
            .find(|w| t.matches(*w).count() == 1)
            .map(str::to_owned)
        else {
            continue;
        };
        let next = match &blocks[i + 1] {
            (nh, Some(_)) => Some(nh.clone()),
            _ => None,
        };
        out.push((h.clone(), w, next));
    }
    out
}

/// One op kind on one document. Paragraphs the op legitimately refuses (a field result, content
/// the engine only preserves) are skipped for the next candidate; anything else is a failure.
fn exercise(path: &Path, kind: &str) -> Result<(), String> {
    let orig = std::fs::read(path).unwrap();
    let cands = candidates(&engine(orig.clone()));
    let mut last = String::from("no candidate paragraph");
    for (h, word, next) in cands {
        if kind == "merge_paragraphs" && next.is_none() {
            continue;
        }
        match exercise_one(&orig, kind, &h, &word, next.as_deref()) {
            Ok(()) => return Ok(()),
            Err((true, m)) => last = m,
            Err((false, m)) => return Err(m),
        }
    }
    Err(last)
}

/// `Err((retry, message))`: `retry` when the op refused this paragraph for a documented reason.
fn exercise_one(orig: &[u8], kind: &str, h: &str, word: &str, next: Option<&str>) -> Result<(), (bool, String)> {
    let orig = orig.to_vec();
    let mut e = engine(orig.clone());
    let base_text = main_text(e.doc(), TextMode::AcceptAll);
    let op = op_clone(kind, h, word, next);
    let r = tx(&mut e, json!([op])).map_err(|err| {
        let c = code(&err).to_owned();
        (matches!(c.as_str(), "IN_FIELD" | "UNSUPPORTED"), format!("{c} {}", err["error"]["message"]))
    })?;
    let fail = |m: String| Err((false, m));
    assert_eq!(r["ok"], true);
    assert_eq!(r["version"], 1);
    let (bytes, _) = e.save().unwrap();
    if !no_new_errors(&orig, &bytes) {
        return fail("saved package has new validation errors".into());
    }
    if !ids_in_range(&bytes) {
        return fail("id out of range".into());
    }
    // Locality: all paragraphs but the touched ones (≤ 2 for split/merge/insert) byte-identical.
    let (same, total) =
        untouched(&zip_part(&orig, "word/document.xml").unwrap(), &zip_part(&bytes, "word/document.xml").unwrap());
    let touched = match kind {
        "merge_paragraphs" | "split_paragraph" => 2,
        "insert_paragraph" | "insert_break" => 1,
        _ => 1,
    };
    if same + touched < total {
        return fail(format!("locality: {same}/{total} paragraphs untouched"));
    }
    let allowed: &[&str] = match kind {
        "add_comment" => &[
            "word/comments.xml",
            "word/commentsExtended.xml",
            "word/commentsIds.xml",
            "word/commentsExtensible.xml",
            "[Content_Types].xml",
            "word/_rels/document.xml.rels",
            "word/people.xml",
        ],
        "set_list" => &["word/numbering.xml", "[Content_Types].xml", "word/_rels/document.xml.rels"],
        _ => &[],
    };
    let drift = other_parts_identical(&orig, &bytes, allowed);
    if !drift.is_empty() {
        return fail(format!("other parts changed: {drift:?}"));
    }
    // The accepted and rejected views are what the edit intended.
    if matches!(kind, "replace_text" | "find_and_replace" | "insert_paragraph" | "delete_block") {
        let mut e0 = engine(orig.clone());
        let anchor_text = text_of(&e0, h);
        let (a0, r0) = (view(&mut e0, "accept"), view(&mut e0, "reject"));
        let (a1, r1) = (view(&mut e, "accept"), view(&mut e, "reject"));
        let checked = match kind {
            "insert_paragraph" => {
                blocks_inserted(&a0, &a1, &["Een nieuwe alinea van Quillwork.".to_owned()], &anchor_text)
            }
            "delete_block" => block_removed(&a0, &a1, &anchor_text),
            _ => {
                let want = anchor_text.replacen(word, "Quillwork", 1);
                if text_of(&e, h) != want {
                    return fail("accepted paragraph text is not the intended new text".into());
                }
                one_block_changed(&a0, &a1, |t| Some(t.replacen(word, "Quillwork", 1)))
            }
        };
        checked.map_err(|m| (false, format!("accepted view: {m}")))?;
        if r1 != r0 {
            return fail("rejected view differs from the original".into());
        }
        TEXT_CHECKS.fetch_add(1, Ordering::Relaxed);
    }
    // Tracked edits reject back to the original text.
    if let Some(cs) = r["change_set"].as_str() {
        let mut e2 = engine(orig.clone());
        tx(&mut e2, json!([op_clone(kind, h, word, next)])).unwrap();
        tx(&mut e2, json!([{"op": "reject", "change_set": cs}]))
            .map_err(|err| (false, format!("reject: {}", code(&err))))?;
        if kind != "add_comment" && main_text(e2.doc(), TextMode::AcceptAll) != base_text {
            return fail("reject did not restore the text".into());
        }
    }
    Ok(())
}

/// The same op again (for a fresh engine).
fn op_clone(kind: &str, h: &str, word: &str, next: Option<&str>) -> Value {
    match kind {
        "replace_text" => json!({"op": "replace_text", "anchor": h, "find": word, "replace": "Quillwork"}),
        "insert_paragraph" => json!({"op": "insert_paragraph", "after": h, "text": "Een nieuwe alinea van Quillwork."}),
        "delete_block" => json!({"op": "delete_block", "anchor": h}),
        "set_text_format" => json!({"op": "set_text_format", "anchor": h, "find": word, "marks": {"bold": true}}),
        "add_comment" => json!({"op": "add_comment", "anchor": h, "find": word, "text": "Graag controleren."}),
        "split_paragraph" => json!({"op": "split_paragraph", "anchor": h, "at_text": word}),
        "merge_paragraphs" => json!({"op": "merge_paragraphs", "first": h, "second": next.unwrap()}),
        "set_paragraph" => json!({"op": "set_paragraph", "anchor": h, "align": "center"}),
        "insert_break" => json!({"op": "insert_break", "after": h, "kind": "page"}),
        "set_list" => json!({"op": "set_list", "anchor": h, "list": "bullet", "level": 0}),
        "find_and_replace" => {
            json!({"op": "find_and_replace", "find": word, "replace": "Quillwork", "count": 1, "anchor": h})
        }
        other => panic!("{other}"),
    }
}

static TEXT_CHECKS: AtomicUsize = AtomicUsize::new(0);

const KINDS: &[&str] = &[
    "replace_text",
    "find_and_replace",
    "insert_paragraph",
    "delete_block",
    "set_text_format",
    "add_comment",
    "split_paragraph",
    "merge_paragraphs",
    "set_paragraph",
    "insert_break",
    "set_list",
];

#[test]
fn every_op_on_every_public_document() {
    let mut failures = Vec::new();
    let mut ok = 0;
    for path in public_docs() {
        let name = label(&path);
        if candidates(&engine(std::fs::read(&path).unwrap())).is_empty() {
            eprintln!("{name}: no multi-paragraph body with a unique word, skipped");
            continue;
        }
        for kind in KINDS {
            match exercise(&path, kind) {
                Ok(()) => ok += 1,
                Err(m) => failures.push(format!("{name} {kind}: {m}")),
            }
        }
    }
    eprintln!(
        "per-op: {ok} ok, {} failed; {} edits with accepted/rejected text checked",
        failures.len(),
        TEXT_CHECKS.load(Ordering::Relaxed)
    );
    for f in &failures {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---------------------------------------------------------------------- formatting

/// A replace inside formatted runs: every inserted run carries the rPr of the run it replaces.
#[test]
fn replace_keeps_run_formatting() {
    let path = fixture("01-run-formatting.docx");
    let orig = std::fs::read(&path).unwrap();
    let mut checked = 0;
    for (h, t) in body(&engine(orig.clone())) {
        let Some(w) = unique_word(&t) else { continue };
        let mut e = engine(orig.clone());
        if tx(&mut e, json!([{"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork"}])).is_err() {
            continue;
        }
        let (bytes, _) = e.save().unwrap();
        let xml = String::from_utf8(zip_part(&bytes, "word/document.xml").unwrap()).unwrap();
        let d = roxmltree::Document::parse(&xml).unwrap();
        let rpr = |r: roxmltree::Node| {
            r.children().find(|c| c.tag_name().name() == "rPr").map(|c| xml[c.range()].to_owned()).unwrap_or_default()
        };
        for ins in d
            .descendants()
            .filter(|n| n.tag_name().name() == "ins" && n.attribute((ns::W, "author")) == Some("Quillwork Test"))
        {
            let Some(del) = ins.prev_sibling_element().filter(|p| p.tag_name().name() == "del") else { continue };
            let (Some(rd), Some(ri)) = (
                del.children().find(|c| c.tag_name().name() == "r"),
                ins.children().find(|c| c.tag_name().name() == "r"),
            ) else {
                continue;
            };
            assert_eq!(rpr(rd), rpr(ri), "01 {h}: inserted run lost the replaced run's formatting");
            checked += 1;
        }
    }
    eprintln!("01: {checked} del/ins run pairs compared");
    assert!(checked > 0);
}

/// A replacement that keeps the found text and adds to it inserts right after the match, not at
/// the end of the run holding it.
#[test]
fn pure_insertion_lands_after_the_match() {
    for mode in ["direct", "track"] {
        let path = fixture("02-headings.docx");
        let mut e = engine(std::fs::read(&path).unwrap());
        let (h, t) = body(&e).into_iter().find(|(_, t)| t.split_whitespace().count() >= 2).unwrap();
        let first = t.split_whitespace().next().unwrap().to_owned();
        let v = e.version();
        e.apply_tx(&json!({"base_version": v, "mode": mode,
            "ops": [{"op": "replace_text", "anchor": h, "find": first, "replace": format!("{first} Quillwork")}]}))
            .unwrap();
        let want = t.replacen(&first, &format!("{first} Quillwork"), 1);
        assert_eq!(text_of(&e, &h), want, "{mode} {h}");
    }
}

/// An inserted paragraph clones the anchor's paragraph style.
#[test]
fn insert_paragraph_clones_style() {
    let path = fixture("02-headings.docx");
    let orig = std::fs::read(&path).unwrap();
    let e0 = engine(orig.clone());
    let style = |e: &Engine, h: &str| match e.anchors().resolve(e.doc(), h) {
        Some(Target::Block { para, .. }) => {
            let d = e.doc().part(para.part);
            d.children(para.node)
                .find(|&c| d.is(c, ns::W, "pPr"))
                .and_then(|ppr| d.children(ppr).find(|&c| d.is(c, ns::W, "pStyle")))
                .and_then(|s| d.attr_ns(s, ns::W, "val").map(|v| v.to_string()))
        }
        _ => None,
    };
    let styled: Vec<String> = body(&e0).into_iter().map(|(h, _)| h).filter(|h| style(&e0, h).is_some()).collect();
    assert!(!styled.is_empty());
    for h in styled {
        let mut e = engine(orig.clone());
        let r = tx(&mut e, json!([{"op": "insert_paragraph", "before": h, "text": "Nieuwe alinea"}])).unwrap();
        let line = r["changed"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .find(|l| l.contains("Nieuwe alinea"))
            .unwrap()
            .to_owned();
        let new_h = line
            .split(|c: char| c.is_whitespace() || c == ']' || c == '[')
            .find(|s| s.starts_with('b'))
            .unwrap()
            .to_owned();
        // `before` clones the anchor's pPr; `after` a heading takes the body style by design.
        assert_eq!(style(&e, &new_h), style(&e, &h), "02 {h}: style not cloned");
    }
}

// ---------------------------------------------------------------------- change sets

#[test]
fn change_set_accept_and_reject_round_trip() {
    for path in xml_support::synthetic_docx() {
        let orig = std::fs::read(&path).unwrap();
        let mut e = engine(orig.clone());
        let before = main_text(e.doc(), TextMode::AcceptAll);
        let (h1, w1) = pick(&e, &[]).unwrap();
        let (h2, _) = pick(&e, std::slice::from_ref(&h1)).unwrap();
        let (h3, w3) = pick(&e, &[h1.clone(), h2.clone()]).unwrap();
        let ops = json!([
            {"op": "replace_text", "anchor": h1, "find": w1, "replace": "Quillwork"},
            {"op": "delete_block", "anchor": h2},
            {"op": "insert_paragraph", "after": h3, "text": "Toegevoegd door Quillwork."},
            {"op": "set_text_format", "anchor": h3, "find": w3, "marks": {"italic": true}},
        ]);
        let r = tx(&mut e, ops.clone()).unwrap();
        let cs = r["change_set"].as_str().unwrap().to_owned();
        assert_eq!(r["patch"]["changeSets"][0]["status"], "open");
        assert!(r["removed"].as_array().is_none_or(|a| a.iter().all(|h| h != &json!(h1))));
        let accepted_view = main_text(e.doc(), TextMode::AcceptAll);
        assert!(accepted_view.contains("Quillwork") && accepted_view.contains("Toegevoegd door Quillwork."));

        // Reject the change set: the original text comes back.
        let mut rj = engine(orig.clone());
        let r1 = tx(&mut rj, ops.clone()).unwrap();
        let rr = tx(&mut rj, json!([{"op": "reject", "change_set": r1["change_set"]}])).unwrap();
        assert_eq!(rr["change_set"], Value::Null);
        assert_eq!(rr["patch"]["changeSets"][0]["status"], "rejected");
        assert_eq!(main_text(rj.doc(), TextMode::AcceptAll), before, "{}: reject", label(&path));
        assert_eq!(main_text(rj.doc(), TextMode::AsIs), main_text(rj.doc(), TextMode::AcceptAll));

        // Accept it: the markup disappears and the accepted view stays.
        let ra = tx(&mut e, json!([{"op": "accept", "change_set": cs}])).unwrap();
        assert_eq!(ra["patch"]["changeSets"][0]["status"], "accepted");
        assert_eq!(main_text(e.doc(), TextMode::AsIs), accepted_view, "{}: accept", label(&path));
        let (bytes, _) = e.save().unwrap();
        assert!(no_new_errors(&orig, &bytes));
    }
}

#[test]
fn pre_existing_revisions_survive_own_review() {
    let path = fixture("08-tracked-changes.docx");
    let orig = std::fs::read(&path).unwrap();
    let mut e = engine(orig.clone());
    let foreign = |doc: &WordDoc| {
        let d = doc.part(doc.main_part());
        d.descendants(d.root())
            .filter(|&n| d.is(n, ns::W, "ins") || d.is(n, ns::W, "del"))
            .filter(|&n| d.attr_ns(n, ns::W, "author").is_some_and(|a| a != "Quillwork Test"))
            .count()
    };
    let n0 = foreign(e.doc());
    assert!(n0 > 0);
    let Some((h, w)) = pick(&e, &[]) else { panic!("08: no candidate") };
    let r = tx(&mut e, json!([{"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork"}])).unwrap();
    tx(&mut e, json!([{"op": "accept", "change_set": r["change_set"]}])).unwrap();
    assert_eq!(foreign(e.doc()), n0, "accepting our change set touched someone else's revisions");
}

// ---------------------------------------------------------------------- transactions

fn doc50() -> Vec<u8> {
    std::fs::read(xml_support::workspace_root().join("corpus/synthetic/doc50.docx")).unwrap()
}

#[test]
fn stale_anchor_and_expect() {
    let mut e = engine(doc50());
    let (h, w) = pick(&e, &[]).unwrap();
    let t0 = text_of(&e, &h);
    let r = tx(&mut e, json!([{"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork"}])).unwrap();
    assert_eq!(r["version"], 1);
    let t1 = text_of(&e, &h);
    let w2 = unique_word(&t1).filter(|x| x != "Quillwork").unwrap_or_else(|| "Quillwork".into());

    // An edit read at version 0 is stale.
    let stale = e
        .apply_tx(&json!({"base_version": 0, "ops": [{"op": "replace_text", "anchor": h, "find": w2, "replace": "X"}]}))
        .unwrap_err();
    assert_eq!(code(&stale), "STALE_ANCHOR");
    assert_eq!(stale["error"]["anchor"], json!(h));
    assert_eq!(stale["error"]["retryable"], true);
    assert_eq!(stale["error"]["version"], 1);
    let cur = stale["error"]["current"].as_array().unwrap();
    assert!(cur[0].as_str().unwrap().contains("Quillwork"));
    assert_eq!(e.version(), 1);

    // ...unless `expect` proves the agent saw the current text.
    let ok = e.apply_tx(&json!({"base_version": 0, "ops": [{"op": "replace_text", "anchor": h, "find": w2, "replace": "Xq", "expect": {"text": t1}}]}));
    assert!(ok.is_ok(), "{ok:?}");
    // A wrong expect.text is stale even at the current version.
    let t2 = text_of(&e, &h);
    let bad = tx(&mut e, json!([{"op": "delete_block", "anchor": h, "expect": {"text": t0}}])).unwrap_err();
    assert_eq!(code(&bad), "STALE_ANCHOR");
    // expect.hash: a 12-hex prefix of the accepted text's sha256.
    let wrong = tx(&mut e, json!([{"op": "delete_block", "anchor": h, "expect": {"hash": "sha256:000000000000"}}]))
        .unwrap_err();
    assert_eq!(code(&wrong), "STALE_ANCHOR");
    let hash = format!("sha256:{}", &sha256_hex(t2.as_bytes())[..12]);
    tx(&mut e, json!([{"op": "delete_block", "anchor": h, "expect": {"hash": hash}}])).unwrap();
}

#[test]
fn failed_op_rolls_back_the_whole_transaction() {
    let mut e = engine(doc50());
    let (h1, w1) = pick(&e, &[]).unwrap();
    let main = e.doc().main_part();
    let before = e.doc().part(main).to_bytes();
    let err = tx(
        &mut e,
        json!([
            {"op": "replace_text", "anchor": h1, "find": w1, "replace": "Quillwork"},
            {"op": "insert_paragraph", "after": h1, "text": "Nieuw"},
            {"op": "replace_text", "anchor": h1, "find": "zzz-not-there", "replace": "x"},
        ]),
    )
    .unwrap_err();
    assert_eq!(code(&err), "NOT_FOUND");
    assert_eq!(err["error"]["op_index"], 2);
    assert_eq!(err["error"]["op"], "replace_text");
    assert_eq!(err["error"]["version"], 0);
    assert_eq!(e.version(), 0);
    assert!(e.doc().part(main).to_bytes() == before, "rollback left changes behind");
    assert!(e.change_sets().is_empty());
    // The engine is still usable and ids restart where they were.
    let r = tx(&mut e, json!([{"op": "replace_text", "anchor": h1, "find": w1, "replace": "Quillwork"}])).unwrap();
    assert_eq!(r["change_set"], "cs_1");
}

#[test]
fn rollback_drops_added_parts() {
    // A document without comments: add_comment creates the comment parts, then a later op fails.
    let path = fixture("02-headings.docx");
    let orig = std::fs::read(&path).unwrap();
    let mut e = engine(orig.clone());
    assert!(!e.doc().package().unwrap().contains("word/comments.xml"));
    let parts = e.doc().part_indices().count();
    let (h, w) = pick(&e, &[]).unwrap();
    let err = tx(
        &mut e,
        json!([
            {"op": "add_comment", "anchor": h, "find": w, "text": "Opmerking"},
            {"op": "delete_block", "anchor": "b_nope"},
        ]),
    )
    .unwrap_err();
    assert_eq!(code(&err), "NOT_FOUND");
    assert_eq!(e.doc().part_indices().count(), parts);
    assert!(!e.doc().package().unwrap().contains("word/comments.xml"));
    let (bytes, _) = e.save().unwrap();
    assert!(zip_part(&bytes, "word/comments.xml").is_none());
    // Then the same comment commits.
    tx(&mut e, json!([{"op": "add_comment", "anchor": h, "find": w, "text": "Opmerking"}])).unwrap();
    let (bytes, _) = e.save().unwrap();
    assert!(zip_part(&bytes, "word/comments.xml").is_some());
    assert!(no_new_errors(&orig, &bytes));
}

#[test]
fn transaction_shape_errors() {
    let mut e = engine(doc50());
    let (h, _) = pick(&e, &[]).unwrap();
    let bad_version =
        e.apply_tx(&json!({"base_version": 9, "ops": [{"op": "delete_block", "anchor": h}]})).unwrap_err();
    assert_eq!(code(&bad_version), "BAD_VERSION");
    let many: Vec<Value> = (0..201).map(|_| json!({"op": "delete_block", "anchor": h})).collect();
    let limit = tx(&mut e, Value::Array(many)).unwrap_err();
    assert_eq!(code(&limit), "LIMIT");
    assert_eq!(limit["error"]["limit"]["name"], "ops_per_transaction");
    assert_eq!(limit["error"]["limit"]["actual"], 201);
    assert_eq!(limit["error"]["op_index"], Value::Null);
    let unknown_key =
        e.apply_tx(&json!({"base_version": 0, "ops": [{"op": "delete_block", "anchor": h}], "foo": 1})).unwrap_err();
    assert_eq!(code(&unknown_key), "INVALID_OP");
    assert_eq!(unknown_key["error"]["path"], "/foo");
    let no_base = e.apply_tx(&json!({"ops": [{"op": "delete_block", "anchor": h}]})).unwrap_err();
    assert_eq!(code(&no_base), "INVALID_OP");
    let empty = tx(&mut e, json!([])).unwrap_err();
    assert_eq!(code(&empty), "INVALID_OP");
    let bad_op = tx(&mut e, json!([{"op": "delete_block", "anchor": h}, {"op": "explode"}])).unwrap_err();
    assert_eq!(code(&bad_op), "INVALID_OP");
    assert_eq!(bad_op["error"]["op_index"], 1);
    let not_json: Value = serde_json::from_str(&e.apply("{nope")).unwrap();
    assert_eq!(code(&not_json), "INVALID_OP");
    let unimpl = tx(&mut e, json!([{"op": "insert_footnote", "anchor": h, "text": "x"}]));
    if let Err(u) = unimpl {
        assert!(matches!(code(&u), "UNSUPPORTED" | "INVALID_OP"));
    }
    let missing = tx(&mut e, json!([{"op": "delete_block", "anchor": "b_zz"}])).unwrap_err();
    assert_eq!(code(&missing), "NOT_FOUND");
    assert_eq!(e.version(), 0);
    // The text form.
    let text = e.apply_text(&json!({"base_version": 0, "ops": [{"op": "delete_block", "anchor": "b_zz"}]}).to_string());
    assert!(text.contains("NOT_FOUND"));
}

#[test]
fn sidecar_keeps_anchors_version_and_change_sets() {
    let mut e = engine(doc50());
    let (h, w) = pick(&e, &[]).unwrap();
    let handles_before: Vec<String> = body(&e).into_iter().map(|(h, _)| h).collect();
    let r = tx(&mut e, json!([{"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork"}])).unwrap();
    let cs = r["change_set"].as_str().unwrap().to_owned();
    let (bytes, sidecar) = e.save().unwrap();
    let mut e2 = Engine::open(bytes.clone(), Some(&sidecar)).unwrap();
    assert_eq!(e2.version(), 1);
    assert_eq!(body(&e2).into_iter().map(|(h, _)| h).collect::<Vec<_>>(), handles_before);
    assert_eq!(e2.change_sets().len(), 1);
    // Staleness survives the reopen.
    let stale = e2.apply_tx(&json!({"base_version": 0, "ops": [{"op": "delete_block", "anchor": h}]})).unwrap_err();
    assert_eq!(code(&stale), "STALE_ANCHOR");
    let ra = tx(&mut e2, json!([{"op": "accept", "change_set": cs}])).unwrap();
    assert_eq!(ra["patch"]["changeSets"][0]["status"], "accepted");
    // The next change set gets a fresh id.
    let (h2, w2) = pick(&e2, std::slice::from_ref(&h)).unwrap();
    let r2 = tx(&mut e2, json!([{"op": "replace_text", "anchor": h2, "find": w2, "replace": "Quillwork"}])).unwrap();
    assert_eq!(r2["change_set"], "cs_2");
    // Saving twice without edits gives the same bytes and anchors.
    let (b1, s1) = e2.save().unwrap();
    let (b2, s2) = e2.save().unwrap();
    assert!(b1 == b2);
    assert_eq!(s1, s2);
    // A bare anchors sidecar is accepted too.
    let bare = e2.anchors().sidecar(e2.doc()).to_json();
    assert!(Engine::open(b1, Some(&bare)).is_ok());
}

#[test]
fn set_track_changes_toggles_settings() {
    let mut e = engine(doc50());
    let has = |e: &Engine| {
        let Some(p) = e.doc().settings_part() else { return false };
        let d = e.doc().part(p);
        d.children(d.root()).any(|c| d.is(c, ns::W, "trackRevisions"))
    };
    let start = has(&e);
    tx(&mut e, json!([{"op": "set_track_changes", "on": !start}])).unwrap();
    assert_eq!(has(&e), !start);
    tx(&mut e, json!([{"op": "set_track_changes", "on": start}])).unwrap();
    assert_eq!(has(&e), start);
    tx(&mut e, json!([{"op": "set_track_changes", "on": true}])).unwrap();
    let (bytes, _) = e.save().unwrap();
    assert!(validate::check_bytes(&bytes).is_ok() || no_new_errors(&doc50(), &bytes));
}

#[test]
fn direct_mode_leaves_no_revisions() {
    let mut e = engine(doc50());
    let (h, w) = pick(&e, &[]).unwrap();
    let r = e.apply_tx(&json!({"base_version": 0, "mode": "direct", "ops": [{"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork"}]})).unwrap();
    assert_eq!(r["change_set"], Value::Null);
    assert!(text_of(&e, &h).contains("Quillwork"));
    assert_eq!(main_text(e.doc(), TextMode::AsIs), main_text(e.doc(), TextMode::AcceptAll));
}

/// Every op example in spec/examples/ops passes the shape check, and every catalogue op the
/// engine does not implement answers UNSUPPORTED rather than failing some other way.
#[test]
fn spec_examples_shape_check() {
    let dir = xml_support::workspace_root().join("spec/examples/ops");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let ops_v = v.get("ops").and_then(Value::as_array).cloned().unwrap_or_else(|| vec![v.clone()]);
        for (i, op) in ops_v.iter().enumerate() {
            // Read ops (list_images, …) are not in the edit catalogue.
            if ops::op_spec(op["op"].as_str().unwrap_or("")).is_none() {
                continue;
            }
            ops::check_shape(i, op).unwrap_or_else(|e| panic!("{}: {e:?}", label(&p)));
            n += 1;
        }
    }
    eprintln!("{n} example ops shape-checked");
    let mut e = engine(doc50());
    let (h, _) = pick(&e, &[]).unwrap();
    for spec in ops::OPS {
        if handlers::IMPLEMENTED.contains(&spec.name) {
            continue;
        }
        let r = tx(&mut e, json!([{"op": spec.name, "anchor": h}]));
        if let Err(err) = r {
            assert!(matches!(code(&err), "UNSUPPORTED" | "INVALID_OP"), "{}: {}", spec.name, code(&err));
        }
    }
    assert_eq!(e.version(), 0);
}

/// The multi-op example transaction, with its anchors mapped onto fixture 04's table and body.
#[test]
fn multi_op_example_on_tables_fixture() {
    let path = fixture("04-tables.docx");
    let orig = std::fs::read(&path).unwrap();
    let mut e = engine(orig.clone());
    // A paragraph outside the tables, so set_cell does not replace it.
    let (h, w) = body(&e)
        .into_iter()
        .find_map(|(h, t)| {
            let top = match e.anchors().resolve(e.doc(), &h) {
                Some(Target::Block { para, .. }) => {
                    let d = e.doc().part(para.part);
                    d.parent(para.node).is_some_and(|p| d.is(p, ns::W, "body"))
                }
                _ => false,
            };
            if top { unique_word(&t).map(|w| (h, w)) } else { None }
        })
        .unwrap();
    let mut t: Value = serde_json::from_str(
        &std::fs::read_to_string(xml_support::workspace_root().join("spec/examples/ops/_transaction-multi.json"))
            .unwrap(),
    )
    .unwrap();
    t["base_version"] = json!(0);
    t["ops"][0]["cell"] = json!("t1.r1.c1");
    t["ops"][1]["anchor"] = json!(h);
    t["ops"][1]["find"] = json!(w);
    t["ops"][2]["anchor"] = json!(h);
    let r = e.apply_tx(&t).unwrap_or_else(|err| panic!("{err}"));
    assert!(r["change_set"].is_string());
    let (bytes, _) = e.save().unwrap();
    assert!(no_new_errors(&orig, &bytes));
    assert!(ids_in_range(&bytes));
}

// ---------------------------------------------------------------------- private corpus

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The handle of the body paragraph `para` identifies (a text snippet): a prefix match first,
/// then containment.
fn find_para(e: &Engine, para: &str, must_contain: Option<&str>) -> Option<String> {
    let p = norm(para);
    let b = body(e);
    let ok = |t: &str| must_contain.is_none_or(|m| t.contains(m));
    b.iter()
        .find(|(_, t)| norm(t).starts_with(&p) && ok(t))
        .or_else(|| b.iter().find(|(_, t)| norm(t).contains(&p) && ok(t)))
        .map(|(h, _)| h.clone())
}

/// The engine's find folding, for comparing texts: typographic spaces, quotes and dashes read
/// plainly, whitespace runs collapse, `**` drops.
fn fold(s: &str) -> String {
    let m: String = s
        .chars()
        .map(|c| match c {
            '\u{00A0}' | '\u{202F}' | '\u{2007}' | '\u{2009}' | '\u{200A}' => ' ',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
            '\u{2013}' | '\u{2014}' | '\u{2011}' | '\u{2010}' => '-',
            c => c,
        })
        .collect();
    norm(&m.replace("**", ""))
}

/// Text checks for one applied private edit: `Ok(true)` checked and right, `Ok(false)` not
/// checkable (the edit's text does not occur verbatim even after folding).
fn private_text_check(
    op: &str,
    ed: &Value,
    h: &str,
    before_h: &str,
    after_h: &str,
    a0: &[String],
    a1: &[String],
) -> Result<bool, String> {
    let s = |k: &str| ed.get(k).and_then(Value::as_str).unwrap_or("");
    let folded = |v: &[String]| v.iter().map(|t| fold(t)).collect::<Vec<_>>();
    let (fa0, fa1) = (folded(a0), folded(a1));
    match op {
        "replace" => {
            let (old, new) = (fold(s("old")), fold(s("new")));
            if !fold(before_h).contains(&old) {
                return Ok(false);
            }
            let want = |t: &str| fold(&t.replacen(&old, &new, 1));
            if fold(after_h) != want(&fold(before_h)) {
                return Err(format!("{h}: accepted paragraph text is not the intended new text"));
            }
            one_block_changed(&fa0, &fa1, |t| t.contains(&old).then(|| want(t))).map(|_| true)
        }
        "insert_after" => {
            let lines: Vec<String> = s("text").split('\n').map(fold).collect();
            blocks_inserted(&fa0, &fa1, &lines, &fold(before_h)).map(|_| true)
        }
        "delete" => block_removed(&fa0, &fa1, &fold(before_h)).map(|_| true),
        _ => Ok(false),
    }
}

#[test]
fn private_corpus_edits() {
    let Some(dir) = xml_support::private_dir() else {
        eprintln!("private corpus absent; skipped");
        return;
    };
    let Ok(raw) = std::fs::read_to_string(dir.parent().unwrap().join("edits.json")) else {
        eprintln!("private edits.json absent; skipped");
        return;
    };
    let edits: serde_json::Map<String, Value> = serde_json::from_str(&raw).unwrap();
    let mut keys: Vec<&String> = edits.keys().collect();
    keys.sort();
    let (mut applied, mut total, mut failed_docs) = (0usize, 0usize, 0usize);
    let (mut text_checked, mut text_bad, mut unchecked) = (0usize, 0usize, 0usize);
    for (i, key) in keys.iter().enumerate() {
        let path = dir.join(key.as_str());
        let Ok(orig) = std::fs::read(&path) else {
            eprintln!("private#{i}: missing");
            continue;
        };
        let mut e = engine(orig.clone());
        let before = main_text(e.doc(), TextMode::AcceptAll);
        let rejected0 = view(&mut e, "reject");
        let (mut checked_here, mut bad_here) = (0usize, 0usize);
        let mut ok_here = 0;
        let mut outcome: Vec<&str> = Vec::new();
        for ed in edits[key.as_str()].as_array().unwrap() {
            total += 1;
            let op_name = ed.get("op").and_then(Value::as_str).unwrap_or("");
            let para = ed.get("para").and_then(Value::as_str).unwrap_or("");
            let old = ed.get("old").and_then(Value::as_str);
            let quote = ed.get("anchor").and_then(Value::as_str);
            let Some(h) = find_para(&e, para, old.or(quote)).or_else(|| find_para(&e, para, None)) else {
                outcome.push("para?");
                continue;
            };
            let op = match op_name {
                "replace" => {
                    json!({"op": "replace_text", "anchor": h, "find": old.unwrap_or(""), "replace": ed.get("new").and_then(Value::as_str).unwrap_or("")})
                }
                "insert_after" => {
                    json!({"op": "insert_paragraph", "after": h, "text": ed.get("text").and_then(Value::as_str).unwrap_or("")})
                }
                "delete" => json!({"op": "delete_block", "anchor": h}),
                "comment" => {
                    let mut o = json!({"op": "add_comment", "anchor": h, "text": ed.get("text").and_then(Value::as_str).unwrap_or("")});
                    if let Some(q) = quote.filter(|q| text_of(&e, &h).contains(q)) {
                        o["find"] = json!(q);
                    }
                    o
                }
                _ => {
                    outcome.push("op?");
                    continue;
                }
            };
            let text_edit = matches!(op_name, "replace" | "insert_after" | "delete");
            let before_h = text_of(&e, &h);
            let a0 = if text_edit { view(&mut e, "accept") } else { Vec::new() };
            match tx(&mut e, json!([op])) {
                Ok(_) => {
                    ok_here += 1;
                    outcome.push("ok");
                    if text_edit {
                        let after_h = if op_name == "replace" { text_of(&e, &h) } else { String::new() };
                        let a1 = view(&mut e, "accept");
                        let r = private_text_check(op_name, ed, &h, &before_h, &after_h, &a0, &a1).and_then(|c| {
                            if view(&mut e, "reject") == rejected0 {
                                Ok(c)
                            } else {
                                Err("rejected view differs from the original".into())
                            }
                        });
                        match r {
                            Ok(true) => checked_here += 1,
                            Ok(false) => unchecked += 1,
                            Err(_) => {
                                checked_here += 1;
                                bad_here += 1;
                            }
                        }
                    }
                }
                Err(err) => outcome.push(match code(&err) {
                    "NOT_FOUND" => "NOT_FOUND",
                    "AMBIGUOUS" => "AMBIGUOUS",
                    "UNSUPPORTED" => "UNSUPPORTED",
                    "INVARIANT" => "INVARIANT",
                    "STALE_ANCHOR" => "STALE",
                    _ => "other",
                }),
            }
        }
        applied += ok_here;
        let (bytes, _) = e.save().unwrap();
        let valid = no_new_errors(&orig, &bytes);
        let ids_ok = ids_in_range(&bytes);
        let (orig_main, out_main) =
            (zip_part(&orig, "word/document.xml").unwrap(), zip_part(&bytes, "word/document.xml").unwrap());
        let paras = para_slices(&orig_main).len();
        let same = preserved(&orig_main, &out_main);
        // Reject every change set: back to the original text.
        let css: Vec<String> = e.change_sets().iter().map(|c| c.id.clone()).collect();
        let mut restored = true;
        if !css.is_empty() {
            let ops: Vec<Value> = css.iter().map(|c| json!({"op": "reject", "change_set": c})).collect();
            let mut back = Engine::open(bytes.clone(), Some(&e.save().unwrap().1)).unwrap();
            restored = tx(&mut back, Value::Array(ops)).is_ok() && main_text(back.doc(), TextMode::AcceptAll) == before;
        }
        // Each edit rewrites at most its own paragraph (a comment touches one, an insert none).
        text_checked += checked_here;
        text_bad += bad_here;
        if !(valid && ids_ok && restored && bad_here == 0 && same + ok_here >= paras) {
            failed_docs += 1;
        }
        eprintln!(
            "private#{i}: {ok_here}/{} applied {outcome:?}; valid={valid} ids_ok={ids_ok} reject_restores={restored}; text checks {checked_here}, wrong {bad_here}; paragraphs {paras}, byte-identical {same}",
            outcome.len()
        );
    }
    eprintln!(
        "private: {applied}/{total} edits applied; text checks {text_checked} (wrong {text_bad}, not checkable {unchecked}); {failed_docs} documents with check failures"
    );
    assert_eq!(failed_docs, 0);
}

#[test]
fn ids_used_by_one_transaction_are_unique() {
    let mut e = engine(doc50());
    let mut skip = Vec::new();
    let mut ops_v = Vec::new();
    for _ in 0..5 {
        let Some((h, w)) = pick(&e, &skip) else { break };
        skip.push(h.clone());
        ops_v.push(json!({"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork"}));
    }
    let r = tx(&mut e, Value::Array(ops_v)).unwrap();
    let revs: Vec<u64> =
        r["patch"]["changeSets"][0]["revisions"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    let set: HashSet<u64> = revs.iter().copied().collect();
    assert_eq!(set.len(), revs.len());
    assert!(revs.iter().all(|&v| v < 0x8000_0000));
}

// ---------------------------------------------------------------------- edits inside a pending insertion

const ME: &str = "Quillwork Test";

/// A body paragraph (not the first or last block) with a unique word followed by at least three
/// more characters: `(handle, word, the three characters after it)`.
fn pick_inner(e: &Engine) -> (String, String, String) {
    let b = body(e);
    let n = b.len();
    b.into_iter()
        .enumerate()
        .filter(|(i, _)| *i > 0 && *i + 1 < n)
        .find_map(|(_, (h, t))| {
            let w = t
                .split(|c: char| !c.is_alphabetic())
                .filter(|w| w.chars().count() >= 4 && t.matches(*w).count() == 1)
                .find(|w| t[t.find(*w).unwrap() + w.len()..].chars().count() >= 3)?
                .to_owned();
            let tail: String = t[t.find(&w).unwrap() + w.len()..].chars().take(3).collect();
            Some((h, w, tail))
        })
        .expect("doc50 has a paragraph with an inner unique word")
}

/// Paragraph `h` in one view.
fn para_view(e: &Engine, h: &str, mode: TextMode) -> String {
    match e.anchors().resolve(e.doc(), h) {
        Some(Target::Block { para, .. }) => {
            let d = e.doc().part(para.part);
            text::text(d, para.node, mode).trim_end().to_owned()
        }
        other => panic!("{h}: {other:?}"),
    }
}

/// `(w:ins, w:del)` elements in paragraph `h`, and how many of the `w:del` sit inside a `w:ins`.
fn revisions_in(e: &Engine, h: &str) -> (usize, usize, usize) {
    let Some(Target::Block { para, .. }) = e.anchors().resolve(e.doc(), h) else { panic!("{h}") };
    let d = e.doc().part(para.part);
    let all: Vec<_> = d.descendants(para.node).collect();
    let ins = all.iter().filter(|&&n| d.is(n, ns::W, "ins")).count();
    let dels: Vec<_> = all.iter().copied().filter(|&n| d.is(n, ns::W, "del")).collect();
    let nested = dels.iter().filter(|&&n| d.ancestors(n).any(|a| d.is(a, ns::W, "ins"))).count();
    (ins, dels.len(), nested)
}

/// Every revision element's `w:id` in every XML part of the saved package is unique.
fn revision_ids_unique(bytes: &[u8]) -> Result<(), String> {
    let re = regex::Regex::new(
        r#"<w:(?:ins|del|moveFrom|moveTo|rPrChange|pPrChange|sectPrChange|tblPrChange|trPrChange|tcPrChange|numberingChange|cellIns|cellDel|cellMerge)\b[^>]*?\sw:id="(\d+)""#,
    )
    .unwrap();
    for (name, xml) in zip_xml_parts(bytes) {
        let s = String::from_utf8_lossy(&xml);
        let mut seen = HashSet::new();
        for c in re.captures_iter(&s) {
            if !seen.insert(c[1].to_owned()) {
                return Err(format!("{name}: revision id {} used twice", &c[1]));
            }
        }
    }
    Ok(())
}

/// doc50 with a unique inner word of one paragraph replaced by "Quillwork engine", tracked by
/// `author`; optionally saved and reopened. Returns the engine (agent: [`ME`]), the original
/// bytes, the handle, the three characters after the insertion, the first change set, and the
/// paragraph's accepted text before and after the first edit.
struct Setup {
    e: Engine,
    orig: Vec<u8>,
    h: String,
    tail: String,
    cs1: String,
    before: String,
    mid: String,
    /// [`revisions_in`] after the first edit.
    revs: (usize, usize, usize),
}

fn setup(author: &str, reopen: bool) -> Setup {
    let orig = doc50();
    let mut e = engine(orig.clone());
    e.set_agent(author);
    let (h, w, tail) = pick_inner(&e);
    let before = para_view(&e, &h, TextMode::AcceptAll);
    let r = tx(&mut e, json!([{"op": "replace_text", "anchor": h, "find": w, "replace": "Quillwork engine"}])).unwrap();
    let cs1 = r["change_set"].as_str().unwrap().to_owned();
    let mid = para_view(&e, &h, TextMode::AcceptAll);
    if reopen {
        let (b, sc) = e.save().unwrap();
        e = Engine::open(b, Some(&sc)).unwrap();
        e.set_now(Some(NOW));
    }
    e.set_agent(ME);
    let revs = revisions_in(&e, &h);
    Setup { e, orig, h, tail, cs1, before, mid, revs }
}

/// Every check after the second edit: valid package, unique revision ids, reject-all unchanged.
fn check_after(s: &mut Setup, what: &str) {
    assert_eq!(para_view(&s.e, &s.h, TextMode::RejectAll), s.before, "{what}: reject-all view changed");
    let (bytes, _) = s.e.save().unwrap();
    revision_ids_unique(&bytes).unwrap_or_else(|m| panic!("{what}: {m}"));
    assert!(no_new_errors(&s.orig, &bytes), "{what}: new validation errors");
    assert!(ids_in_range(&bytes), "{what}");
}

fn reject(e: &mut Engine, cs: &Value) -> Value {
    tx(e, json!([{"op": "reject", "change_set": cs}])).unwrap()
}

#[test]
fn same_author_edits_a_pending_insertion_in_place() {
    for reopen in [false, true] {
        let what = if reopen { "same author, reopened" } else { "same author, same document" };
        let mut s = setup(ME, reopen);
        let r = tx(
            &mut s.e,
            json!([{"op": "replace_text", "anchor": s.h, "find": "Quillwork engine", "replace": "Quillwork motor"}]),
        )
        .unwrap_or_else(|e| panic!("{what}: {e}"));
        // Rewritten inside the insertion: no new revision, no deletion stacked on it.
        assert_eq!(r["change_set"], Value::Null, "{what}");
        assert_eq!(revisions_in(&s.e, &s.h), s.revs, "{what}");
        assert_eq!(
            para_view(&s.e, &s.h, TextMode::AcceptAll),
            s.mid.replace("Quillwork engine", "Quillwork motor"),
            "{what}"
        );
        // The insertion's change set changed, so the patch carries it.
        let css = r["patch"]["changeSets"].as_array().unwrap();
        assert!(css.iter().any(|c| c["id"] == json!(s.cs1)), "{what}: {css:?}");
        check_after(&mut s, what);
        // Rejecting the insertion's change set restores the text from before both edits.
        let cs1 = json!(s.cs1);
        reject(&mut s.e, &cs1);
        assert_eq!(para_view(&s.e, &s.h, TextMode::AsIs), s.before, "{what}: reject");
    }
}

#[test]
fn same_author_cuts_a_pending_insertion_mid_word() {
    // The edit splits the insertion's run in the middle (this used to duplicate its w:id).
    for reopen in [false, true] {
        let what = if reopen { "reopened" } else { "same document" };
        let mut s = setup(ME, reopen);
        tx(&mut s.e, json!([{"op": "replace_text", "anchor": s.h, "find": "Quillwork", "replace": "Q"}]))
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(revisions_in(&s.e, &s.h), s.revs, "{what}");
        assert_eq!(para_view(&s.e, &s.h, TextMode::AcceptAll), s.mid.replace("Quillwork engine", "Q engine"), "{what}");
        check_after(&mut s, what);
    }
}

#[test]
fn other_author_nests_a_deletion_in_the_insertion() {
    for reopen in [false, true] {
        for (find, repl) in [("engine", "motor"), ("Quillwork", "Q"), ("ngine", "x")] {
            let what = format!("other author, reopen={reopen}, {find:?}");
            let mut s = setup("Other", reopen);
            let r = tx(&mut s.e, json!([{"op": "replace_text", "anchor": s.h, "find": find, "replace": repl}]))
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            let cs2 = r["change_set"].clone();
            assert!(cs2.is_string(), "{what}");
            let (_, dels, nested) = revisions_in(&s.e, &s.h);
            assert!(nested == s.revs.2 + 1 && dels == s.revs.1 + 1, "{what}: one new deletion, inside their insertion");
            let after = s.mid.replacen(find, repl, 1);
            assert_eq!(para_view(&s.e, &s.h, TextMode::AcceptAll), after, "{what}");
            check_after(&mut s, &what);
            // Rejecting our change set restores their text exactly; then theirs, the original.
            reject(&mut s.e, &cs2);
            assert_eq!(para_view(&s.e, &s.h, TextMode::AcceptAll), s.mid, "{what}: reject ours");
            let cs1 = json!(s.cs1);
            reject(&mut s.e, &cs1);
            assert_eq!(para_view(&s.e, &s.h, TextMode::AsIs), s.before, "{what}: reject theirs");
        }
    }
}

#[test]
fn replace_partly_overlapping_a_pending_insertion() {
    for author in [ME, "Other"] {
        for reopen in [false, true] {
            let what = format!("{author}, reopen={reopen}");
            let mut s = setup(author, reopen);
            let find = format!("engine{}", s.tail);
            let r = tx(&mut s.e, json!([{"op": "replace_text", "anchor": s.h, "find": find, "replace": "motor!"}]))
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            let after = s.mid.replacen(&find, "motor!", 1);
            assert_eq!(para_view(&s.e, &s.h, TextMode::AcceptAll), after, "{what}");
            check_after(&mut s, &what);
            // The new change set alone restores the text from before it.
            reject(&mut s.e, &r["change_set"]);
            assert_eq!(para_view(&s.e, &s.h, TextMode::AcceptAll), s.mid, "{what}: reject ours");
            let cs1 = json!(s.cs1);
            reject(&mut s.e, &cs1);
            assert_eq!(para_view(&s.e, &s.h, TextMode::AsIs), s.before, "{what}: reject first");
        }
    }
}

#[test]
fn same_author_deletes_a_whole_pending_insertion() {
    let mut s = setup(ME, true);
    let r = tx(&mut s.e, json!([{"op": "replace_text", "anchor": s.h, "find": "Quillwork engine", "replace": ""}]))
        .unwrap();
    // The insertion is withdrawn outright; its id leaves its change set.
    assert_eq!(r["change_set"], Value::Null);
    assert_eq!(revisions_in(&s.e, &s.h).0, s.revs.0 - 1);
    let cs = r["patch"]["changeSets"].as_array().unwrap().iter().find(|c| c["id"] == json!(s.cs1)).cloned().unwrap();
    assert_eq!(cs["revisions"].as_array().unwrap().len(), 1, "{cs}");
    check_after(&mut s, "withdrawn");
    let cs1 = json!(s.cs1);
    reject(&mut s.e, &cs1);
    assert_eq!(para_view(&s.e, &s.h, TextMode::AsIs), s.before);
}

// ---------------------------------------------------------------------- error envelopes vs spec

/// A JSON value's JSON-Schema type name(s).
fn type_ok(v: &Value, t: &Value) -> bool {
    let one = |t: &str| match t {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    };
    match t {
        Value::String(s) => one(s),
        Value::Array(a) => a.iter().any(|t| t.as_str().is_some_and(one)),
        _ => true,
    }
}

/// The parts of a schema object this check understands: required, type, enum, const, minItems.
fn check_props(err: &serde_json::Map<String, Value>, s: &Value, props: &Value, at: &str) -> Result<(), String> {
    for k in s["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !err.contains_key(k) {
            return Err(format!("{at}: missing {k}"));
        }
    }
    for (k, v) in err {
        let Some(p) = s["properties"].get(k).or_else(|| props.get(k)) else { continue };
        if !p["type"].is_null() && !type_ok(v, &p["type"]) {
            return Err(format!("{at}: {k} has the wrong type"));
        }
        if let Some(e) = p["enum"].as_array()
            && !e.contains(v)
        {
            return Err(format!("{at}: {k} = {v} is not in its enum"));
        }
        if !p["const"].is_null() && &p["const"] != v {
            return Err(format!("{at}: {k} must be {}", p["const"]));
        }
        if let (Some(min), Some(a)) = (p["minItems"].as_u64(), v.as_array())
            && (a.len() as u64) < min
        {
            return Err(format!("{at}: {k} needs {min}+ items"));
        }
    }
    Ok(())
}

/// Check an error envelope against spec/schema/error.schema.json: the envelope, the Error object's
/// required keys, types, enums and closed key set, and every per-code `if`/`then` rule. (No
/// JSON-Schema crate in the workspace; the Python binding's tests run the full validator.)
fn check_error_envelope(env: &Value) -> Result<(), String> {
    let schema: Value = serde_json::from_slice(
        &std::fs::read(xml_support::workspace_root().join("spec/schema/error.schema.json")).unwrap(),
    )
    .unwrap();
    let top = env.as_object().ok_or("envelope is not an object")?;
    let keys: Vec<&String> = top.keys().collect();
    if keys.iter().any(|k| !["spec_version", "ok", "error"].contains(&k.as_str()))
        || top.len() != 3
        || env["ok"] != json!(false)
    {
        return Err(format!("envelope keys {keys:?}"));
    }
    let def = &schema["$defs"]["Error"];
    let props = &def["properties"];
    let err = env["error"].as_object().ok_or("error is not an object")?;
    if let Some(k) = err.keys().find(|k| props.get(k.as_str()).is_none()) {
        return Err(format!("unknown key {k}"));
    }
    check_props(err, def, props, "Error")?;
    for rule in def["allOf"].as_array().unwrap() {
        let cond = &rule["if"]["properties"]["code"];
        let applies = cond.get("const").is_some_and(|c| c == &err["code"])
            || cond.get("enum").and_then(Value::as_array).is_some_and(|e| e.contains(&err["code"]));
        if applies {
            check_props(err, &rule["then"], &Value::Null, err["code"].as_str().unwrap_or("?"))?;
        }
    }
    Ok(())
}

#[test]
fn every_invalid_op_error_matches_the_error_schema() {
    let mut e = engine(doc50());
    let (h, _) = pick(&e, &[]).unwrap();
    let del = json!({"op": "delete_block", "anchor": h});
    // (request, expected path): request-level errors, op-shape errors and errors raised below the
    // op layer (which know neither their path nor the op's usage).
    let cases: Vec<(String, &str)> = vec![
        ("{nope".into(), ""),
        ("[1, 2]".into(), ""),
        (json!({"base_version": 0}).to_string(), "/ops"),
        (json!({"base_version": 0, "ops": []}).to_string(), "/ops"),
        (json!({"base_version": 0, "ops": [del], "foo": 1}).to_string(), "/foo"),
        (json!({"ops": [del]}).to_string(), "/base_version"),
        (json!({"base_version": 0, "ops": [del], "mode": "loud"}).to_string(), "/mode"),
        (json!({"base_version": 0, "ops": [del], "projection_mode": "x"}).to_string(), "/projection_mode"),
        (json!({"base_version": 0, "ops": [1]}).to_string(), "/ops/0"),
        (json!({"base_version": 0, "ops": [{"op": "explode"}]}).to_string(), "/ops/0/op"),
        (json!({"base_version": 0, "ops": [{"op": "delete_block"}]}).to_string(), "/ops/0/anchor"),
        (json!({"base_version": 0, "ops": [del, {"op": "reject", "all": true, "author": "x"}]}).to_string(), "/ops/1/all"),
        (json!({"base_version": 0, "ops": [{"op": "set_text_format", "anchor": format!("{h}..t1"), "marks": {"bold": true}}]}).to_string(), "/ops/0/anchor"),
    ];
    for (req, path) in &cases {
        let env: Value = serde_json::from_str(&e.apply(req)).unwrap();
        assert_eq!(code(&env), "INVALID_OP", "{req}: {env}");
        check_error_envelope(&env).unwrap_or_else(|m| panic!("{req}: {m}\n{env}"));
        assert_eq!(env["error"]["path"], json!(path), "{req}");
        assert!(env["error"]["usage"].as_str().is_some_and(|u| !u.is_empty()), "{req}");
        // The text form carries the same usage line.
        let text = e.apply_text(req);
        assert!(text.contains("INVALID_OP") && text.contains("§ usage: "), "{req}: {text}");
    }
    // The other codes on the same paths still validate.
    for req in [
        json!({"base_version": 9, "ops": [del]}),
        json!({"base_version": 0, "ops": [{"op": "delete_block", "anchor": "b_zz"}]}),
        json!({"base_version": 0, "ops": (0..201).map(|_| del.clone()).collect::<Vec<_>>()}),
    ] {
        let env: Value = serde_json::from_str(&e.apply(&req.to_string())).unwrap();
        check_error_envelope(&env).unwrap_or_else(|m| panic!("{m}\n{env}"));
    }
    assert_eq!(e.version(), 0);

    // An INVALID_OP raised inside the edit layer, which knows neither its path nor the op's usage.
    let ok = e.apply_tx(&json!({"base_version": 0, "ops": [{"op": "insert_table", "after": h, "rows": 2, "cols": 2}]}));
    assert!(ok.is_ok(), "{ok:?}");
    let cells: Vec<String> = (0..60).map(|i| format!("c{i}")).collect();
    let req = json!({"base_version": 1, "ops": [{"op": "insert_row", "after": "t1.r1", "cells": cells}]}).to_string();
    let env: Value = serde_json::from_str(&e.apply(&req)).unwrap();
    assert_eq!(env["error"]["code"], json!("INVALID_OP"), "{env}");
    check_error_envelope(&env).unwrap_or_else(|m| panic!("{m}\n{env}"));
    assert_eq!(env["error"]["path"], json!("/ops/0"));
    assert!(env["error"]["usage"].as_str().is_some_and(|u| u.starts_with("insert_row")), "{env}");
    assert!(e.apply_text(&req).contains("§ usage: insert_row"));
}

// ---------------------------------------------------------------------- synthetic documents

/// A docx with `body` (block XML, no `sectPr`) as its body, built on fixture 01's package.
fn docx_with_body(body: &str) -> Vec<u8> {
    use std::io::Write;
    let base = std::fs::read(fixture("01-run-formatting.docx")).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(&base)).unwrap();
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        if name == "word/document.xml" {
            buf = format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#,
                ns::W
            )
            .into_bytes();
        }
        w.start_file(name.as_str(), opts).unwrap();
        w.write_all(&buf).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn run_xml(rpr: &str, t: &str) -> String {
    let rpr = if rpr.is_empty() { String::new() } else { format!("<w:rPr>{rpr}</w:rPr>") };
    format!(r#"<w:r>{rpr}<w:t xml:space="preserve">{t}</w:t></w:r>"#)
}

fn p_xml(inner: &str) -> String {
    format!("<w:p>{inner}</w:p>")
}

fn tbl_xml(cells: &[String]) -> String {
    let grid: String = cells.iter().map(|_| r#"<w:gridCol w:w="3000"/>"#).collect();
    let tcs: String =
        cells.iter().map(|c| format!(r#"<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr>{c}</w:tc>"#)).collect();
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid>{grid}</w:tblGrid><w:tr>{tcs}</w:tr></w:tbl>"#
    )
}

/// Every paragraph handle in the body story (cells included) with its accepted text.
fn all_paras(e: &Engine) -> Vec<(String, String)> {
    let doc = e.doc();
    e.anchors()
        .handles(body_story(doc))
        .filter_map(|(h, _)| match e.anchors().resolve(doc, h) {
            Some(Target::Block { para, .. }) => Some((h.to_owned(), accepted_text(doc.part(para.part), para.node))),
            _ => None,
        })
        .collect()
}

fn handle_of(e: &Engine, text: &str) -> String {
    all_paras(e).into_iter().find(|(_, t)| t == text).map(|(h, _)| h).unwrap_or_else(|| panic!("no paragraph {text:?}"))
}

/// One run property, comparable across serialisations: name plus sorted attributes.
fn rpr_key(r: roxmltree::Node) -> String {
    let Some(rpr) = r.children().find(|c| c.tag_name().name() == "rPr") else { return String::new() };
    let mut parts: Vec<String> = rpr
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() != "rPrChange")
        .map(|c| {
            let mut a: Vec<String> = c.attributes().map(|a| format!("{}={}", a.name(), a.value())).collect();
            a.sort();
            format!("{}[{}]", c.tag_name().name(), a.join(","))
        })
        .collect();
    parts.sort();
    parts.join(";")
}

/// Per character of the paragraph whose (resolved) text is `text`: the character and its run's
/// properties. `None` if no paragraph, or more than one, has that text.
fn char_props(bytes: &[u8], text: &str) -> Option<Vec<(char, String)>> {
    let xml = String::from_utf8(zip_part(bytes, "word/document.xml").unwrap()).unwrap();
    let d = roxmltree::Document::parse(&xml).unwrap();
    let mut hits = d.descendants().filter(|n| n.tag_name().name() == "p" && xml_para_text(*n) == text);
    let p = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    let mut out = Vec::new();
    for t in p.descendants().filter(|n| n.tag_name().name() == "t") {
        let key = rpr_key(t.parent().unwrap());
        out.extend(t.text().unwrap_or("").chars().map(|c| (c, key.clone())));
    }
    Some(out)
}

/// No `w:ins` sits inside another `w:ins`.
fn no_nested_ins(bytes: &[u8]) -> bool {
    let xml = String::from_utf8(zip_part(bytes, "word/document.xml").unwrap()).unwrap();
    let d = roxmltree::Document::parse(&xml).unwrap();
    d.descendants()
        .filter(|n| n.tag_name().name() == "ins")
        .all(|n| !n.ancestors().skip(1).any(|a| a.tag_name().name() == "ins"))
}

// ---------------------------------------------------------------------- seeded property tests

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

const WORDS: &[&str] =
    &["kade", "koffie", "bonen", "roast", "alpha", "beta", "gamma", "delta", "zuid", "noord", "mokka", "crema"];
const PROPS: &[&str] = &[
    "",
    "<w:b/>",
    "<w:i/>",
    r#"<w:u w:val="single"/>"#,
    r#"<w:color w:val="C00000"/>"#,
    "<w:b/><w:i/>",
    r#"<w:vertAlign w:val="superscript"/>"#,
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Where {
    InRun,
    AcrossRuns,
    InCell,
}

/// Words with the index of the run each sits in.
struct Para {
    runs: Vec<(String, String)>,       // (rPr, text)
    words: Vec<(usize, usize, usize)>, // (byte start, byte end, run)
}

impl Para {
    fn text(&self) -> String {
        self.runs.iter().map(|(_, t)| t.as_str()).collect()
    }
    fn xml(&self) -> String {
        p_xml(&self.runs.iter().map(|(r, t)| run_xml(r, t)).collect::<String>())
    }
}

fn gen_para(rng: &mut Rng, min_runs: usize) -> Para {
    let n = min_runs + rng.below(4);
    let mut runs: Vec<(String, String)> = Vec::new();
    let mut words = Vec::new();
    let mut pos = 0;
    for i in 0..n {
        let k = 1 + rng.below(3);
        let mut t = String::new();
        // The space between two runs sits at the end of one or the start of the next.
        let lead = i > 0 && !runs[i - 1].1.ends_with(' ');
        if lead {
            t.push(' ');
        }
        for j in 0..k {
            if j > 0 {
                t.push(' ');
            }
            let w = *rng.pick(WORDS);
            words.push((pos + t.len(), pos + t.len() + w.len(), i));
            t.push_str(w);
        }
        if i + 1 < n && rng.below(2) == 0 {
            t.push(' ');
        }
        pos += t.len();
        // Neighbouring runs differ, so the run boundary is real.
        let mut rpr = rng.pick(PROPS).to_string();
        while i > 0 && runs[i - 1].0 == rpr {
            rpr = rng.pick(PROPS).to_string();
        }
        runs.push((rpr, t));
    }
    Para { runs, words }
}

/// A random word edit of `old` (a space-separated word list): replace, insert or delete words.
fn mutate(rng: &mut Rng, old: &str) -> String {
    let mut w: Vec<String> = old.split(' ').map(str::to_owned).collect();
    for _ in 0..1 + rng.below(2) {
        match rng.below(3) {
            0 => {
                let i = rng.below(w.len());
                w[i] = rng.pick(WORDS).to_string();
            }
            1 => {
                let i = rng.below(w.len() + 1);
                w.insert(i, rng.pick(WORDS).to_string());
            }
            _ if w.len() > 1 => {
                w.remove(rng.below(w.len()));
            }
            _ => w.push(rng.pick(WORDS).to_string()),
        }
    }
    w.join(" ")
}

/// Letter runs and space runs (the engine's token classes for this vocabulary).
fn toks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices().skip(1) {
        if (c == ' ') != s[..i].ends_with(' ') {
            out.push(&s[start..i]);
            start = i;
        }
    }
    if !s.is_empty() {
        out.push(&s[start..]);
    }
    out
}

/// One generated case; `Err` describes the first broken property.
fn prop_case(seed: u64, at: Where) -> Result<(), String> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let (para, a, b, old) = loop {
        let para = gen_para(&mut rng, if at == Where::AcrossRuns { 2 } else { 1 });
        let text = para.text();
        let nw = para.words.len();
        let (a, b) = match at {
            Where::InRun => {
                let run = para.words[rng.below(nw)].2;
                let idx: Vec<usize> = (0..nw).filter(|&i| para.words[i].2 == run).collect();
                let a = idx[rng.below(idx.len())];
                let last = *idx.last().unwrap();
                (a, a + rng.below(last - a + 1))
            }
            _ => {
                let a = rng.below(nw);
                (a, a + rng.below(nw - a))
            }
        };
        if at == Where::AcrossRuns && para.words[a].2 == para.words[b].2 {
            continue;
        }
        let old = text[para.words[a].0..para.words[b].1].to_owned();
        // Unique counting overlaps ("delta delta" in "delta delta delta" is not).
        if (0..text.len()).filter(|&i| text[i..].starts_with(&old)).count() == 1 {
            break (para, a, b, old);
        }
    };
    let new = loop {
        let n = mutate(&mut rng, &old);
        if n != old {
            break n;
        }
    };
    let text = para.text();
    let start = para.words[a].0;
    let end = para.words[b].1;
    let want = format!("{}{new}{}", &text[..start], &text[end..]);
    let target = para.xml();
    let body = match at {
        Where::InCell => format!(
            "{}{}{}",
            p_xml(&run_xml("", "Intro")),
            tbl_xml(&[p_xml(&run_xml("", "Links")), target]),
            p_xml(&run_xml("", "Slot"))
        ),
        _ => format!("{}{target}{}", p_xml(&run_xml("", "Intro")), p_xml(&run_xml("", "Slot"))),
    };
    let orig = docx_with_body(&body);
    if std::env::var("QW_DEBUG").is_ok_and(|v| v == seed.to_string()) {
        eprintln!("runs {:?}\nold {old:?}\nnew {new:?}", para.runs);
    }
    let mut e = engine(orig.clone());
    let a0 = view(&mut e, "accept");
    let h = handle_of(&e, &text);
    tx(&mut e, json!([{"op": "replace_text", "anchor": h, "find": old, "replace": new}]))
        .map_err(|err| format!("replace failed: {}", code(&err)))?;
    if text_of(&e, &h) != want {
        return Err(format!("accepted paragraph {:?}, want {want:?}", text_of(&e, &h)));
    }
    let (bytes, _) = e.save().unwrap();
    if !no_new_errors(&orig, &bytes) {
        return Err("new validation errors".into());
    }
    if !ids_in_range(&bytes) {
        return Err("id out of range".into());
    }
    revision_ids_unique(&bytes)?;
    let (acc, rej) = (resolved(&mut e, "accept"), resolved(&mut e, "reject"));
    one_block_changed(&a0, &blocks(&acc), |t| Some(t.replacen(&text, &want, 1)))?;
    if blocks(&rej) != a0 {
        return Err("rejected view differs from the original".into());
    }
    // Formatting: the rejected paragraph is the original character for character; in the
    // accepted one, everything outside the edit and the tokens the diff keeps keep their rPr.
    let before = char_props(&orig, &text).ok_or("original paragraph not found")?;
    let after = char_props(&acc, &want).ok_or("accepted paragraph not found")?;
    if char_props(&rej, &text).as_ref() != Some(&before) {
        return Err("rejected paragraph lost run formatting".into());
    }
    let (ot, nt) = (toks(&old), toks(&new));
    let pre: usize = ot.iter().zip(&nt).take_while(|(x, y)| x == y).map(|(x, _)| x.chars().count()).sum();
    let mut suf = 0;
    for (x, y) in ot.iter().rev().zip(nt.iter().rev()) {
        if x != y
            || suf + x.chars().count() > old.chars().count() - pre
            || suf + y.chars().count() > new.chars().count() - pre
        {
            break;
        }
        suf += x.chars().count();
    }
    let (head, tail) = (text[..start].chars().count() + pre, text[end..].chars().count() + suf);
    if before[..head] != after[..head] || before[before.len() - tail..] != after[after.len() - tail..] {
        return Err("an unchanged span lost its run formatting".into());
    }
    Ok(())
}

fn prop_run(at: Where, cases: u64) {
    let mut failures = Vec::new();
    for seed in 0..cases {
        if let Err(m) = prop_case(seed, at) {
            failures.push(format!("seed {seed}: {m}"));
        }
    }
    eprintln!("{at:?}: {cases} cases, {} failed", failures.len());
    assert!(failures.is_empty(), "{at:?}: {failures:#?}");
}

#[test]
fn prop_word_edits_within_one_run() {
    prop_run(Where::InRun, 500);
}

#[test]
fn prop_word_edits_across_runs() {
    prop_run(Where::AcrossRuns, 500);
}

#[test]
fn prop_word_edits_in_table_cells() {
    prop_run(Where::InCell, 500);
}

// ---------------------------------------------------------------------- insertions inside a run

struct Edited {
    orig: Vec<u8>,
    e: Engine,
    acc: Vec<u8>,
    rej: Vec<u8>,
}

/// Apply `ops(engine)` to a document with `body`; check validity and revision ids.
fn edited(body: &str, ops: impl Fn(&Engine) -> Value) -> Edited {
    let orig = docx_with_body(body);
    let mut e = engine(orig.clone());
    let o = ops(&e);
    tx(&mut e, o).unwrap_or_else(|err| panic!("{err}"));
    let (bytes, _) = e.save().unwrap();
    assert!(no_new_errors(&orig, &bytes), "new validation errors");
    assert!(ids_in_range(&bytes));
    revision_ids_unique(&bytes).unwrap();
    let (acc, rej) = (resolved(&mut e, "accept"), resolved(&mut e, "reject"));
    Edited { orig, e, acc, rej }
}

/// A replace in paragraph `text` (the second body paragraph, between "Intro" and "Slot").
fn replace_in(runs: &str, text: &str, find: &str, replace: &str) -> Edited {
    let body = format!("{}{}{}", p_xml(&run_xml("", "Intro")), p_xml(runs), p_xml(&run_xml("", "Slot")));
    edited(&body, |e| json!([{"op": "replace_text", "anchor": handle_of(e, text), "find": find, "replace": replace}]))
}

/// The accepted paragraph reads `want`, the rejected one `text` with its original formatting, and
/// every inserted character carries the properties of a neighbour.
fn check_insert(x: &Edited, text: &str, want: &str) -> Vec<(char, String)> {
    assert_eq!(blocks(&x.acc), vec!["Intro".to_owned(), want.to_owned(), "Slot".to_owned()]);
    assert_eq!(blocks(&x.rej), blocks(&x.orig));
    let before = char_props(&x.orig, text).unwrap();
    assert_eq!(char_props(&x.rej, text).unwrap(), before, "reject lost formatting");
    let after = char_props(&x.acc, want).unwrap();
    // Common prefix and suffix keep their properties; inserted characters copy a neighbour's.
    let pre = before.iter().zip(&after).take_while(|(a, b)| a == b).count();
    let suf = before.iter().rev().zip(after.iter().rev()).take_while(|(a, b)| a == b).count().min(before.len() - pre);
    assert_eq!(pre + suf, before.len(), "an original character changed: {after:?}");
    let around: Vec<&String> =
        [pre.checked_sub(1).map(|i| &before[i].1), before.get(pre).map(|c| &c.1)].into_iter().flatten().collect();
    for (_, p) in &after[pre..after.len() - suf] {
        assert!(around.contains(&p), "inserted text took foreign formatting {p:?}, neighbours {around:?}");
    }
    after
}

#[test]
fn insertion_in_the_middle_of_a_run() {
    let x = replace_in(
        &run_xml("<w:b/>", "Kade B.V. levert koffie."),
        "Kade B.V. levert koffie.",
        "Kade B.V.",
        "Kade Coffee B.V.",
    );
    let after = check_insert(&x, "Kade B.V. levert koffie.", "Kade Coffee B.V. levert koffie.");
    assert!(after.iter().all(|(_, p)| p == "b[]"));
}

#[test]
fn insertion_at_run_start_end_and_boundary() {
    let runs = format!(
        "{}{}{}",
        run_xml("<w:b/>", "alpha "),
        run_xml("<w:i/>", "beta gamma"),
        run_xml(r#"<w:u w:val="single"/>"#, " delta")
    );
    let t = "alpha beta gamma delta";
    // At the start of a run.
    check_insert(&replace_in(&runs, t, "beta", "kade beta"), t, "alpha kade beta gamma delta");
    // At the end of a run.
    check_insert(&replace_in(&runs, t, "gamma", "gamma kade"), t, "alpha beta gamma kade delta");
    // Across a run boundary.
    check_insert(&replace_in(&runs, t, "gamma delta", "gamma kade delta"), t, "alpha beta gamma kade delta");
    // At the very start and end of the paragraph.
    check_insert(&replace_in(&runs, t, "alpha", "kade alpha"), t, "kade alpha beta gamma delta");
    check_insert(&replace_in(&runs, t, "delta", "delta kade"), t, "alpha beta gamma delta kade");
}

#[test]
fn insertion_next_to_mixed_formatting() {
    let runs = format!(
        "{}{}{}",
        run_xml("<w:b/>", "Kade"),
        run_xml("<w:i/><w:color w:val=\"C00000\"/>", " B.V."),
        run_xml("", " levert")
    );
    check_insert(
        &replace_in(&runs, "Kade B.V. levert", "Kade B.V.", "Kade Coffee B.V."),
        "Kade B.V. levert",
        "Kade Coffee B.V. levert",
    );
    let runs = format!(
        "{}{}",
        run_xml("<w:b/>", "de 20"),
        run_xml(r#"<w:vertAlign w:val="superscript"/>"#, "ste van de maand")
    );
    check_insert(
        &replace_in(&runs, "de 20ste van de maand", "van de maand", "van de hele maand"),
        "de 20ste van de maand",
        "de 20ste van de hele maand",
    );
    check_insert(
        &replace_in(&runs, "de 20ste van de maand", "20ste van", "20ste dag van"),
        "de 20ste van de maand",
        "de 20ste dag van de maand",
    );
}

fn pending_ins(author: &str) -> String {
    format!(
        r#"{}<w:ins w:id="901" w:author="{author}" w:date="2026-10-01T00:00:00Z">{}</w:ins>{}"#,
        run_xml("", "Intro "),
        run_xml("<w:b/>", "alpha beta"),
        run_xml("", " tail")
    )
}

#[test]
fn insertion_inside_an_own_pending_insertion() {
    let mut x = replace_in(&pending_ins(ME), "Intro alpha beta tail", "alpha beta", "alpha gamma beta");
    assert_eq!(blocks(&x.acc)[1], "Intro alpha gamma beta tail");
    assert_eq!(blocks(&x.rej)[1], "Intro  tail");
    assert!(no_nested_ins(&x.e.save().unwrap().0));
    // The new word joins the insertion it was typed into: still one revision, bold throughout.
    let h = handle_of(&x.e, "Intro alpha gamma beta tail");
    assert_eq!(revisions_in(&x.e, &h), (1, 0, 0));
    let after = char_props(&x.acc, "Intro alpha gamma beta tail").unwrap();
    assert!(after[6..22].iter().all(|(_, p)| p == "b[]"));
}

#[test]
fn insertion_inside_a_foreign_pending_insertion() {
    let mut x = replace_in(&pending_ins("Other"), "Intro alpha beta tail", "alpha beta", "alpha gamma beta");
    assert_eq!(blocks(&x.acc)[1], "Intro alpha gamma beta tail");
    assert_eq!(blocks(&x.rej)[1], "Intro  tail");
    let (bytes, _) = x.e.save().unwrap();
    assert!(no_nested_ins(&bytes), "our insertion nests in someone else's");
    // Ours sits between the two halves of theirs; rejecting only ours restores their text.
    let h = handle_of(&x.e, "Intro alpha gamma beta tail");
    assert_eq!(revisions_in(&x.e, &h).0, 3);
    let cs = x.e.change_sets()[0].id.clone();
    tx(&mut x.e, json!([{"op": "reject", "change_set": cs}])).unwrap();
    assert_eq!(text_of(&x.e, &h), "Intro alpha beta tail");
    let after = char_props(&x.acc, "Intro alpha gamma beta tail").unwrap();
    assert!(after[6..22].iter().all(|(_, p)| p == "b[]"));
}

// ---------------------------------------------------------------------- paragraph marks before a table

fn tbl(cells: &[&str]) -> String {
    tbl_xml(&cells.iter().map(|c| p_xml(&run_xml("", c))).collect::<Vec<_>>())
}

fn ps(texts: &[&str]) -> String {
    texts.iter().map(|t| p_xml(&run_xml("", t))).collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Tracked paragraph marks with no `w:p` right after them (a table, a content control or the end
/// of the container follows). Word and Docxodus can't merge those away: they leave an empty
/// paragraph, whatever our own accept does.
fn stranded_marks(bytes: &[u8]) -> usize {
    let xml = String::from_utf8(zip_part(bytes, "word/document.xml").unwrap()).unwrap();
    let d = roxmltree::Document::parse(&xml).unwrap();
    let tracked = |p: &roxmltree::Node| {
        p.children()
            .find(|c| c.tag_name().name() == "pPr")
            .and_then(|ppr| ppr.children().find(|c| c.tag_name().name() == "rPr"))
            .is_some_and(|rpr| rpr.children().any(|c| matches!(c.tag_name().name(), "ins" | "del")))
    };
    d.descendants()
        .filter(|n| n.tag_name().name() == "p" && tracked(n))
        .filter(|p| p.next_sibling_element().is_none_or(|n| n.tag_name().name() != "p"))
        .count()
}

/// Accepted and rejected block lists after `ops`; rejected must equal the original, and no
/// tracked mark is stranded.
fn block_edit(body: &str, ops: impl Fn(&Engine) -> Value) -> Vec<String> {
    let mut x = edited(body, ops);
    assert_eq!(stranded_marks(&x.e.save().unwrap().0), 0, "a tracked paragraph mark can't merge");
    assert_eq!(blocks(&x.rej), blocks(&x.orig), "reject did not restore the blocks");
    blocks(&x.acc)
}

#[test]
fn delete_the_paragraph_before_a_table() {
    let body = format!("{}{}{}", ps(&["A", "B"]), tbl(&["Cell"]), ps(&["C"]));
    let acc = block_edit(&body, |e| json!([{"op": "delete_block", "anchor": handle_of(e, "B")}]));
    assert_eq!(acc, strs(&["A", "[table Cell]", "C"]));
    // Two paragraphs before the table: one merge chain.
    let body = format!("{}{}{}", ps(&["X", "A", "B"]), tbl(&["Cell"]), ps(&["C"]));
    let acc = block_edit(
        &body,
        |e| json!([{"op": "delete_block", "anchor": handle_of(e, "A")}, {"op": "delete_block", "anchor": handle_of(e, "B")}]),
    );
    assert_eq!(acc, strs(&["X", "[table Cell]", "C"]));
    // Between two tables there is no paragraph to carry the mark: an empty one stays.
    let body = format!("{}{}{}{}", tbl(&["T1"]), ps(&["B"]), tbl(&["T2"]), ps(&["C"]));
    let acc = block_edit(&body, |e| json!([{"op": "delete_block", "anchor": handle_of(e, "B")}]));
    assert_eq!(acc, strs(&["[table T1]", "", "[table T2]", "C"]));
}

#[test]
fn delete_the_last_paragraph_of_a_container() {
    let acc = block_edit(&ps(&["A", "B"]), |e| json!([{"op": "delete_block", "anchor": handle_of(e, "B")}]));
    assert_eq!(acc, strs(&["A"]));
    let body = format!("{}{}{}", ps(&["A"]), tbl_xml(&[ps(&["X", "Y"])]), ps(&["C"]));
    let acc = block_edit(&body, |e| json!([{"op": "delete_block", "anchor": handle_of(e, "Y")}]));
    assert_eq!(acc, strs(&["A", "[table X]", "C"]));
}

#[test]
fn insert_after_the_paragraph_before_a_table() {
    let body = format!("{}{}{}", ps(&["A", "B"]), tbl(&["Cell"]), ps(&["C"]));
    let acc = block_edit(&body, |e| json!([{"op": "insert_paragraph", "after": handle_of(e, "B"), "text": "N"}]));
    assert_eq!(acc, strs(&["A", "B", "N", "[table Cell]", "C"]));
    let acc = block_edit(&body, |e| json!([{"op": "insert_paragraph", "after": handle_of(e, "B"), "text": "N1\nN2"}]));
    assert_eq!(acc, strs(&["A", "B", "N1", "N2", "[table Cell]", "C"]));
    // Inserted, then the stand-in deleted in the same transaction: its mark carries both.
    let acc = block_edit(
        &body,
        |e| json!([{"op": "insert_paragraph", "after": handle_of(e, "B"), "text": "N"}, {"op": "delete_block", "anchor": handle_of(e, "B")}]),
    );
    assert_eq!(acc, strs(&["A", "N", "[table Cell]", "C"]));
    // A page break before the table.
    let acc = block_edit(&body, |e| json!([{"op": "insert_break", "after": handle_of(e, "B"), "kind": "page"}]));
    assert_eq!(acc.len(), 5);
}

#[test]
fn insert_after_the_last_paragraph_of_a_container() {
    let acc =
        block_edit(&ps(&["A", "B"]), |e| json!([{"op": "insert_paragraph", "after": handle_of(e, "B"), "text": "N"}]));
    assert_eq!(acc, strs(&["A", "B", "N"]));
    let body = format!("{}{}{}", ps(&["A"]), tbl(&["X"]), ps(&["C"]));
    let acc = block_edit(&body, |e| json!([{"op": "insert_paragraph", "after": handle_of(e, "X"), "text": "N"}]));
    assert_eq!(acc, strs(&["A", "[table X | N]", "C"]));
}

#[test]
fn move_a_paragraph_next_to_a_table() {
    let body = format!("{}{}{}", ps(&["A", "B"]), tbl(&["Cell"]), ps(&["C"]));
    // C was the body's last paragraph: a body can't end in a table, so an empty paragraph stays.
    let acc =
        block_edit(&body, |e| json!([{"op": "move_block", "anchor": handle_of(e, "C"), "after": handle_of(e, "B")}]));
    assert_eq!(acc, strs(&["A", "B", "C", "[table Cell]", ""]));
    let body = format!("{}{}{}", ps(&["A", "B"]), tbl(&["Cell"]), ps(&["C", "D"]));
    let acc =
        block_edit(&body, |e| json!([{"op": "move_block", "anchor": handle_of(e, "C"), "after": handle_of(e, "B")}]));
    assert_eq!(acc, strs(&["A", "B", "C", "[table Cell]", "D"]));
    let acc =
        block_edit(&body, |e| json!([{"op": "move_block", "anchor": handle_of(e, "B"), "after": handle_of(e, "C")}]));
    assert_eq!(acc, strs(&["A", "[table Cell]", "C", "B", "D"]));
}
