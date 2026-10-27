//! The read side through the engine: `outline` / `read` / `find` read ops, `render` against the
//! projection, and op results whose `patch` takes the host's view model to the engine's next one.
//!
//! The private corpus holds real client documents: files are labelled `private#<i>`, and
//! assertion messages carry anchors, op names and error codes only, never document text.

mod xml_support;

use std::path::PathBuf;

use quillwork_core::anchors::Target;
use quillwork_core::engine::Engine;
use quillwork_core::ops::render::accepted_text;
use quillwork_core::project::vm::ViewModel;
use quillwork_core::wml::StoryKind;
use serde_json::{Value, json};

const NOW: i64 = 1_793_016_000;

/// Fixtures, synthetic, then private documents, labelled.
fn docs() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(xml_support::workspace_root().join("corpus/fixtures"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    v.sort();
    for p in xml_support::synthetic_docx() {
        v.push((p.file_name().unwrap().to_string_lossy().into_owned(), p));
    }
    for (i, p) in xml_support::private_docx().into_iter().enumerate() {
        v.push((format!("private#{i}"), p));
    }
    v
}

fn engine(path: &PathBuf) -> Engine {
    let mut e = Engine::open(std::fs::read(path).unwrap(), None).unwrap();
    e.set_now(Some(NOW));
    e.set_agent("Quillwork Test");
    e
}

fn read(e: &Engine, req: Value) -> Result<Value, Value> {
    e.read_op(&req)
}

fn code(err: &Value) -> String {
    err["error"]["code"].as_str().unwrap_or("?").to_owned()
}

/// Body paragraph handles with their accepted text.
fn body(e: &Engine) -> Vec<(String, String)> {
    let doc = e.doc();
    let Some(story) = doc.stories().iter().position(|s| matches!(s.kind, StoryKind::Body)) else { return Vec::new() };
    e.anchors()
        .handles(story)
        .filter_map(|(h, _)| match e.anchors().resolve(doc, h) {
            Some(Target::Block { para, .. }) => Some((h.to_owned(), accepted_text(doc.part(para.part), para.node))),
            _ => None,
        })
        .collect()
}

/// A body paragraph (not first or last) with a word of ≥4 letters that occurs once in it.
fn pick(e: &Engine) -> Option<(String, String, String)> {
    let b = body(e);
    let n = b.len();
    (1..n.saturating_sub(1)).find_map(|i| {
        let (h, t) = &b[i];
        let w = t
            .split(|c: char| !c.is_alphabetic())
            .filter(|w| w.chars().count() >= 4)
            .find(|w| t.matches(*w).count() == 1)?;
        Some((h.clone(), w.to_owned(), b[i + 1].0.clone()))
    })
}

fn op(kind: &str, h: &str, word: &str, next: &str) -> Value {
    match kind {
        "replace_text" => json!({"op": "replace_text", "anchor": h, "find": word, "replace": "Quillwork"}),
        "insert_paragraph" => json!({"op": "insert_paragraph", "after": h, "text": "Een nieuwe alinea van Quillwork."}),
        "delete_block" => json!({"op": "delete_block", "anchor": h}),
        "set_text_format" => json!({"op": "set_text_format", "anchor": h, "find": word, "marks": {"bold": true}}),
        "add_comment" => json!({"op": "add_comment", "anchor": h, "find": word, "text": "Graag controleren."}),
        "split_paragraph" => json!({"op": "split_paragraph", "anchor": h, "at_text": word}),
        "merge_paragraphs" => json!({"op": "merge_paragraphs", "first": h, "second": next}),
        "set_paragraph" => json!({"op": "set_paragraph", "anchor": h, "align": "center", "style": "Heading1"}),
        other => panic!("{other}"),
    }
}

const KINDS: &[&str] = &[
    "replace_text",
    "insert_paragraph",
    "delete_block",
    "set_text_format",
    "add_comment",
    "split_paragraph",
    "merge_paragraphs",
    "set_paragraph",
];

/// `before` with `patch` applied equals `after` (patches never remove assets).
fn patch_lands(before: &ViewModel, patch: &Value, after: &ViewModel) -> Result<(), String> {
    let p: quillwork_core::project::Patch =
        serde_json::from_value(patch.clone()).map_err(|e| format!("patch does not parse: {e}"))?;
    let mut got = before.clone();
    p.apply(&mut got).map_err(|_| "patch does not apply".to_owned())?;
    got.assets.retain(|k, _| after.assets.contains_key(k));
    if got != *after {
        let differ: Vec<&str> =
            got.blocks.iter().zip(&after.blocks).filter(|(a, b)| a != b).map(|(a, _)| a.id()).take(5).collect();
        let (g, a) = (serde_json::to_value(&got).unwrap(), serde_json::to_value(after).unwrap());
        let fields: Vec<&String> =
            a.as_object().unwrap().keys().filter(|k| g.get(k.as_str()) != a.get(k.as_str())).collect();
        let comments: Vec<String> = match (g["comments"].as_array(), a["comments"].as_array()) {
            (Some(x), Some(y)) => x
                .iter()
                .zip(y)
                .filter(|(p, q)| p != q)
                .map(|(p, q)| {
                    let keys: Vec<&String> =
                        q.as_object().unwrap().keys().filter(|k| p.get(k.as_str()) != q.get(k.as_str())).collect();
                    format!("{}: {keys:?}", q["id"])
                })
                .collect(),
            _ => Vec::new(),
        };
        return Err(format!(
            "patched view model differs in {fields:?} (blocks {} vs {}, first differing {differ:?}, comments {} vs {} {comments:?})",
            got.blocks.len(),
            after.blocks.len(),
            g["comments"].as_array().map_or(0, Vec::len),
            a["comments"].as_array().map_or(0, Vec::len),
        ));
    }
    Ok(())
}

#[test]
fn read_ops_through_the_engine() {
    for (name, path) in docs() {
        let e = engine(&path);
        let outline = read(&e, json!({"op": "outline"})).unwrap_or_else(|err| panic!("{name}: outline {}", code(&err)));
        assert!(outline["text"].as_str().unwrap().starts_with("§ doc"), "{name}: outline header");
        assert_eq!(outline["version"], json!(e.version()), "{name}");
        let all = read(&e, json!({"op": "read", "mode": "markup", "budget": 1_000_000}))
            .unwrap_or_else(|err| panic!("{name}: read {}", code(&err)));
        assert!(all["next"].is_null(), "{name}: one window");
        let text = all["text"].as_str().unwrap();
        // render() lines are the projection's lines.
        let handles: Vec<String> = body(&e).into_iter().map(|(h, _)| h).collect();
        let lines = e.render(&handles);
        assert_eq!(lines.len(), handles.len(), "{name}: one line per handle");
        // A paragraph alone in its table cell prints inside the row line, so only handles the
        // read prints on a line of their own are compared.
        let mut compared = 0;
        for (h, l) in handles.iter().zip(&lines) {
            let own = |t: &&str| {
                t.trim_start().starts_with(&format!("[{h}]")) || t.trim_start().starts_with(&format!("[{h} "))
            };
            if let Some(t) = text.lines().find(own) {
                assert_eq!(t.trim_start(), l, "{name}: render({h}) differs from the markup read");
                compared += 1;
            }
        }
        assert!(compared > 0 || handles.is_empty(), "{name}: no line compared");
        if let Some((h, word, _)) = pick(&e) {
            let found = read(&e, json!({"op": "find", "query": word}))
                .unwrap_or_else(|err| panic!("{name}: find {}", code(&err)));
            assert!(found["text"].as_str().unwrap().contains(&format!("[{h}")), "{name}: find misses {h}");
            let one = read(&e, json!({"op": "read", "range": h}))
                .unwrap_or_else(|err| panic!("{name}: read {h} {}", code(&err)));
            assert!(one["text"].as_str().unwrap().starts_with(&format!("[{h}")), "{name}: read {h}");
        }
    }
    // Errors.
    let (_, path) = &docs()[0];
    let e = engine(path);
    let cases = [
        (json!({"op": "read", "range": "bzzzz"}), "NOT_FOUND", None),
        (json!({"op": "read", "range": "b_zz"}), "INVALID_OP", Some("/range")),
        (json!({"op": "read", "budget": 10}), "INVALID_OP", Some("/budget")),
        (json!({"op": "read", "mode": "raw"}), "INVALID_OP", Some("/mode")),
        (json!({"op": "read", "foo": 1}), "INVALID_OP", Some("/foo")),
        (json!({"op": "outline", "range": "b1"}), "INVALID_OP", Some("/range")),
        (json!({"op": "find", "query": ""}), "INVALID_OP", Some("/query")),
        (json!({"op": "find", "query": "(", "regex": true}), "INVALID_OP", Some("/query")),
        (json!({"op": "find", "query": "x", "story": "zz9"}), "NOT_FOUND", None),
        (json!({"op": "nope"}), "INVALID_OP", Some("/op")),
        (json!({"op": "list_images", "x": 1}), "INVALID_OP", Some("/x")),
        (json!([1]), "INVALID_OP", Some("")),
    ];
    for (req, want, path) in cases {
        let err = read(&e, req.clone()).expect_err(&req.to_string());
        assert_eq!(code(&err), want, "{req}");
        assert_eq!(err["ok"], json!(false));
        if let Some(p) = path {
            assert_eq!(err["error"]["path"], json!(p), "{req}");
            assert!(err["error"]["usage"].is_string(), "{req}: usage");
        }
    }
    assert!(e.read("not json").contains("\"INVALID_OP\""));
    assert!(e.read(r#"{"op":"outline"}"#).contains("\"ok\":true"));
}

#[test]
fn op_results_patch_the_view_model() {
    let mut checked = 0;
    let mut outlines = 0;
    let mut folded = 0;
    for (name, path) in docs() {
        for kind in KINDS {
            let mut e = engine(&path);
            let Some((h, word, next)) = pick(&e) else { continue };
            let before = e.view_model().unwrap_or_else(|_| panic!("{name}: view model"));
            let outline_line = |e: &Engine| {
                let r = e.read_op(&json!({"op": "outline"})).unwrap();
                r["text"].as_str().unwrap().lines().find(|l| l.starts_with("§ outline:")).map(str::to_owned)
            };
            let outline_before = outline_line(&e);
            let r = match e.apply_tx(&json!({"base_version": e.version(), "ops": [op(kind, &h, &word, &next)]})) {
                Ok(r) => r,
                // Ops may refuse a block (a field, a split at the start); only a crash fails here.
                Err(err) => {
                    assert!(code(&err) != "INTERNAL", "{name}: {kind} {h}: {}", code(&err));
                    continue;
                }
            };
            let after = e.view_model().unwrap();
            assert_eq!(after.version, e.version(), "{name}: {kind}");
            let patch = &r["patch"];
            assert_eq!(patch["base_version"], json!(before.version), "{name}: {kind}");
            assert_eq!(patch["version"], json!(after.version), "{name}: {kind}");
            if let Err(msg) = patch_lands(&before, patch, &after) {
                panic!("{name}: {kind} {h}: {msg}");
            }
            // The op's lines are the projection's lines of the new version.
            let markup = e.read_op(&json!({"op": "read", "mode": "markup", "budget": 1_000_000})).unwrap();
            let markup = markup["text"].as_str().unwrap();
            let accepted = e.read_op(&json!({"op": "read", "budget": 1_000_000})).unwrap();
            let accepted = accepted["text"].as_str().unwrap();
            for l in r["changed"].as_array().into_iter().flatten() {
                let l = l.as_str().unwrap();
                // A comment's own paragraphs (`[c1-0] …`) are printed in the read's appendix as
                // the comment entry, not as block lines.
                if l.starts_with("[c") {
                    continue;
                }
                // A paragraph alone in its cell is printed inside its row line, and TOC entry
                // paragraphs fold into their TOC line: those get no line of their own in a read.
                let anchor = l.trim_start().trim_start_matches('[').split([' ', ']']).next().unwrap_or("");
                let starts = |t: &&str| {
                    let t = t.trim_start();
                    t.starts_with(&format!("[{anchor}]")) || t.starts_with(&format!("[{anchor} "))
                };
                if !l.starts_with(' ') && !markup.lines().any(|t| starts(&t)) && !accepted.lines().any(|t| starts(&t)) {
                    folded += 1;
                    continue;
                }
                let own = |t: &str| t.trim_start() == l.trim_start();
                assert!(
                    markup.lines().any(own) || accepted.lines().any(own),
                    "{name}: {kind}: a result line is not a projection line{}",
                    if name.starts_with("private#") { String::new() } else { format!(": {l:?}") }
                );
            }
            let outline_after = outline_line(&e);
            if outline_after == outline_before {
                assert!(r["outline"].is_null(), "{name}: {kind}: outline set though unchanged");
            } else {
                assert_eq!(
                    r["outline"].as_str(),
                    outline_after.as_deref(),
                    "{name}: {kind}: outline is the new outline line"
                );
            }
            outlines += usize::from(r["outline"].is_string());
            checked += 1;
        }
    }
    assert!(checked > 0);
    eprintln!(
        "op_results_patch_the_view_model: {checked} op results, {outlines} with a new outline, {folded} lines folded into a row or TOC"
    );
}

#[test]
fn reads_follow_the_current_version() {
    let (_, path) = docs().into_iter().find(|(n, _)| !n.starts_with("private#") && n.ends_with(".docx")).unwrap();
    let mut e = engine(&path);
    let (h, word, _) = pick(&e).expect("a paragraph");
    // No view model held: the patch carries removes and change sets only.
    let r = e.apply_tx(&json!({"base_version": 0, "ops": [{"op": "replace_text", "anchor": h, "find": word, "replace": "Quillwork"}]})).unwrap();
    assert!(r["patch"]["upserts"].as_array().unwrap().is_empty());
    assert!(r["patch"]["changeSets"].is_array());
    let read = e.read_op(&json!({"op": "read", "range": h, "mode": "markup"})).unwrap();
    assert_eq!(read["version"], json!(1));
    assert!(read["text"].as_str().unwrap().contains("Quillwork"));
    let found = e.read_op(&json!({"op": "find", "query": "Quillwork"})).unwrap();
    assert!(found["text"].as_str().unwrap().contains(&format!("[{h}")));
    // A failed transaction leaves reads as they were.
    let before = e.read_op(&json!({"op": "read", "mode": "markup", "budget": 1_000_000})).unwrap();
    assert!(e.apply_tx(&json!({"base_version": 1, "ops": [{"op": "delete_block", "anchor": "bzzzz"}]})).is_err());
    assert_eq!(e.read_op(&json!({"op": "read", "mode": "markup", "budget": 1_000_000})).unwrap(), before);
    // Saving re-serializes the package; reads still resolve.
    e.save().unwrap();
    assert_eq!(e.read_op(&json!({"op": "read", "mode": "markup", "budget": 1_000_000})).unwrap(), before);
    let vm = e.view_model_json();
    assert!(vm.starts_with('{') && vm.contains("\"version\":1"));
}
