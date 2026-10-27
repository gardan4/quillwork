//! The read ops past `outline` / `read` / `find`: `get_block`, `list_changes`,
//! `list_comments`, `list_images`, `get_image`, `lint`, `list_fields` and `help`, plus the
//! `lint` of op results.
//!
//! Goldens (tests/read_ops_golden/) cover the synthetic fixtures in corpus/fixtures only.
//! `QUILLWORK_BLESS=1 cargo test -p quillwork-core --test read_ops` rewrites them. The
//! private corpus is only run through the ops: files are labelled `private#<i>`, and
//! assertion messages carry op names, error codes and counts, never document text.

mod xml_support;

use std::path::{Path, PathBuf};

use quillwork_core::engine::Engine;
use serde_json::{Value, json};

const NOW: i64 = 1_793_016_000;
const AGENT: &str = "Quillwork Test";

fn fixtures() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(xml_support::workspace_root().join("corpus/fixtures"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx" || x == "docm"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    v.sort();
    v
}

fn fixture(stem: &str) -> PathBuf {
    fixtures().into_iter().find(|(n, _)| n == stem).unwrap_or_else(|| panic!("fixture {stem}")).1
}

fn engine(path: &Path) -> Engine {
    let mut e = Engine::open(std::fs::read(path).unwrap(), None).unwrap();
    e.set_now(Some(NOW));
    e.set_agent(AGENT);
    e
}

fn ok(e: &Engine, req: Value) -> Value {
    e.read_op(&req).unwrap_or_else(|err| panic!("{req}: {}", err["error"]["code"]))
}

fn code(err: &Value) -> String {
    err["error"]["code"].as_str().unwrap_or("?").to_owned()
}

fn apply(e: &mut Engine, mode: &str, ops: Value) -> Value {
    let tx = json!({"base_version": e.version(), "mode": mode, "ops": ops});
    e.apply_tx(&tx).unwrap_or_else(|err| panic!("{tx}: {}", err["error"]))
}

// ------------------------------------------------------------------------------ schemas

struct Schemas {
    read_result: jsonschema::Validator,
    op_result: jsonschema::Validator,
    error: jsonschema::Validator,
}

fn schemas() -> Schemas {
    let dir = xml_support::workspace_root().join("spec/schema");
    let load =
        |f: &str| -> Value { serde_json::from_slice(&std::fs::read(dir.join(f)).expect("schema")).expect("json") };
    let build = |root: Value| {
        let mut o = jsonschema::options();
        for f in [
            "common.schema.json",
            "view-model.schema.json",
            "patch.schema.json",
            "ops.schema.json",
            "error.schema.json",
        ] {
            let s = load(f);
            let id = s["$id"].as_str().expect("$id").to_owned();
            o = o.with_resource(id, jsonschema::Resource::from_contents(s).expect("resource"));
        }
        o.build(&root).expect("schema compiles")
    };
    let ops = load("ops.schema.json")["$id"].as_str().unwrap().to_owned();
    let err = load("error.schema.json")["$id"].as_str().unwrap().to_owned();
    Schemas {
        read_result: build(json!({"$ref": format!("{ops}#/$defs/ReadResult")})),
        op_result: build(json!({"$ref": format!("{ops}#/$defs/OpResult")})),
        error: build(json!({"$ref": err})),
    }
}

/// Schema error paths only (safe to print for private documents). The engine numbers a freshly
/// opened document version 0 where the schema's `Version` starts at 1; that is not what these
/// tests check, so `minimum` errors on version fields are skipped.
fn schema_paths(v: &jsonschema::Validator, json: &Value) -> Vec<String> {
    v.iter_errors(json)
        .map(|e| format!("{} (schema {})", e.instance_path, e.schema_path))
        .filter(|e| !(e.split(' ').next().is_some_and(|p| p.ends_with("version")) && e.ends_with("/minimum)")))
        .take(5)
        .collect()
}

// ------------------------------------------------------------------------------ goldens

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/read_ops_golden")
}

fn check_golden(file: &str, got: &str, failures: &mut Vec<String>) {
    let path = golden_dir().join(file);
    if std::env::var_os("QUILLWORK_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("golden dir");
        std::fs::write(&path, got).expect("write golden");
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(want) if want == got => {}
        Ok(want) => {
            let line = want
                .lines()
                .zip(got.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(want.lines().count().min(got.lines().count()));
            failures.push(format!(
                "{file}: differs at line {}\n  want: {:?}\n  got:  {:?}",
                line + 1,
                want.lines().nth(line),
                got.lines().nth(line)
            ));
        }
        Err(_) => failures.push(format!("{file}: missing golden (run with QUILLWORK_BLESS=1)")),
    }
}

/// A result's payload for a golden: the text as is, data pretty-printed (get_image's base64
/// cut to its first 48 characters).
fn payload(r: &Value) -> String {
    if let Some(t) = r["text"].as_str() {
        return t.to_owned();
    }
    let mut d = r["data"].clone();
    if let Some(b) = d.get("base64").and_then(Value::as_str) {
        let head: String = b.chars().take(48).collect();
        d["base64"] = json!(format!("{head}… ({} chars)", b.len()));
    }
    serde_json::to_string_pretty(&d).unwrap()
}

/// The first image anchor of the document, if any.
fn first_image(e: &Engine) -> Option<String> {
    let r = ok(e, json!({"op": "list_images"}));
    r["data"]["images"].as_array()?.first().map(|i| i["id"].as_str().unwrap().to_owned())
}

#[test]
fn fixture_goldens() {
    let s = schemas();
    let mut failures = Vec::new();
    for (name, path) in fixtures() {
        let e = engine(&path);
        let vm = e.view_model().unwrap();
        let mut out = String::new();
        let mut reqs = vec![
            json!({"op": "list_changes"}),
            json!({"op": "list_comments"}),
            json!({"op": "list_images"}),
            json!({"op": "list_fields"}),
            json!({"op": "lint"}),
        ];
        if let Some(b) = vm.blocks.first() {
            reqs.push(json!({"op": "get_block", "anchor": b.id()}));
        }
        if let Some(img) = first_image(&e) {
            reqs.push(json!({"op": "get_image", "image": img}));
        }
        for req in reqs {
            match e.read_op(&req) {
                Ok(r) => {
                    let errs = schema_paths(&s.read_result, &r);
                    assert!(errs.is_empty(), "{name}: {req}: {errs:?}");
                    out.push_str(&format!("## {req}\n{}\n\n", payload(&r)));
                }
                Err(err) => {
                    let errs = schema_paths(&s.error, &err);
                    assert!(errs.is_empty(), "{name}: {req}: {errs:?}");
                    out.push_str(&format!(
                        "## {req}\n! {} {}\n\n",
                        code(&err),
                        err["error"]["message"].as_str().unwrap_or("")
                    ));
                }
            }
        }
        check_golden(&format!("{name}.txt"), &out, &mut failures);
    }
    let e = engine(&fixture("01-run-formatting"));
    let mut help = String::new();
    for req in [
        json!({"op": "help"}),
        json!({"op": "help", "name": "list_changes"}),
        json!({"op": "help", "name": "insert_field"}),
    ] {
        help.push_str(&format!("## {req}\n{}\n\n", payload(&ok(&e, req.clone()))));
    }
    check_golden("help.txt", &help, &mut failures);
    check_golden("lint-scenario.txt", &lint_scenario(), &mut failures);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Edits on 11-fields that trip each lint rule an edit can trip, with each op result's `lint`
/// and the document `lint` after.
fn lint_scenario() -> String {
    let mut e = engine(&fixture("11-fields"));
    let mut out = String::new();
    let steps = [
        (
            "direct",
            json!([{"op": "insert_paragraph", "after": "b103", "text": "Dear {name}, sign for [CLIENT NAME] here: .........."}]),
        ),
        ("direct", json!([{"op": "replace_text", "anchor": "b106", "find": "page", "replace": "page  number"}])),
        ("direct", json!([{"op": "insert_paragraph", "after": "b104", "text": "Deep heading", "style": "Heading3"}])),
        ("direct", json!([{"op": "replace_text", "anchor": "b102", "find": "Introduction", "replace": ""}])),
        ("track", json!([{"op": "delete_block", "anchor": "b105"}])),
    ];
    for (mode, ops) in steps {
        let r = apply(&mut e, mode, ops.clone());
        out.push_str(&format!("## {mode} {ops}\n{}\n\n", serde_json::to_string_pretty(&r["lint"]).unwrap()));
    }
    out.push_str(&format!("## lint\n{}\n", ok(&e, json!({"op": "lint"}))["text"].as_str().unwrap()));
    // Table ops lint the paragraphs inside the cells and rows they touch.
    let mut e = engine(&fixture("04-tables"));
    let steps = [
        json!([{"op": "set_cell", "cell": "t1.r2.c1", "text": "Fill  in {amount}"}]),
        json!([{"op": "insert_row", "after": "t1.r1", "cells": ["[NAME]", ""]}]),
    ];
    for ops in steps {
        let r = apply(&mut e, "direct", ops.clone());
        out.push_str(&format!("\n## direct {ops}\n{}\n", serde_json::to_string_pretty(&r["lint"]).unwrap()));
    }
    out
}

// ------------------------------------------------------------------------------ errors

#[test]
fn argument_errors() {
    let s = schemas();
    let e = engine(&fixture("09-images"));
    let cases = [
        (json!({"op": "get_block"}), "INVALID_OP", Some("/anchor")),
        (json!({"op": "get_block", "anchor": "x1"}), "INVALID_OP", Some("/anchor")),
        (json!({"op": "get_block", "anchor": 3}), "INVALID_OP", Some("/anchor")),
        (json!({"op": "get_block", "anchor": "bzzzz"}), "NOT_FOUND", None),
        (json!({"op": "get_block", "anchor": "b100", "mode": "markup"}), "INVALID_OP", Some("/mode")),
        (json!({"op": "list_changes", "change_set": "x"}), "INVALID_OP", Some("/change_set")),
        (json!({"op": "list_changes", "change_set": "cs_99"}), "NOT_FOUND", None),
        (json!({"op": "list_changes", "author": ""}), "INVALID_OP", Some("/author")),
        (json!({"op": "list_changes", "status": "done"}), "INVALID_OP", Some("/status")),
        (json!({"op": "list_changes", "since": 3}), "INVALID_OP", Some("/since")),
        (json!({"op": "list_comments", "open_only": "yes"}), "INVALID_OP", Some("/open_only")),
        (json!({"op": "list_images", "image": "img1"}), "INVALID_OP", Some("/image")),
        (json!({"op": "get_image"}), "INVALID_OP", Some("/image")),
        (json!({"op": "get_image", "image": "img1", "asset": "a_0000000000000000"}), "INVALID_OP", Some("/asset")),
        (json!({"op": "get_image", "image": "b100"}), "INVALID_OP", Some("/image")),
        (json!({"op": "get_image", "asset": "a_XYZ"}), "INVALID_OP", Some("/asset")),
        (json!({"op": "get_image", "image": "img99"}), "NOT_FOUND", None),
        (json!({"op": "get_image", "asset": "a_0000000000000000"}), "NOT_FOUND", None),
        (json!({"op": "lint", "fix": true}), "INVALID_OP", Some("/fix")),
        (json!({"op": "list_fields", "x": 1}), "INVALID_OP", Some("/x")),
        (json!({"op": "help", "name": "nope"}), "INVALID_OP", Some("/name")),
        (json!({"op": "help", "name": ""}), "INVALID_OP", Some("/name")),
        (json!({"op": "help", "name": 1}), "INVALID_OP", Some("/name")),
        (json!({"op": "lint", "spec_version": 0}), "INVALID_OP", Some("/spec_version")),
    ];
    for (req, want, path) in cases {
        let err = e.read_op(&req).expect_err(&req.to_string());
        assert_eq!(code(&err), want, "{req}: {err}");
        let errs = schema_paths(&s.error, &err);
        assert!(errs.is_empty(), "{req}: {errs:?}");
        if let Some(p) = path {
            assert_eq!(err["error"]["path"], json!(p), "{req}");
            let usage = err["error"]["usage"].as_str().unwrap_or_else(|| panic!("{req}: usage"));
            assert!(usage.starts_with(req["op"].as_str().unwrap()), "{req}: usage {usage}");
        }
    }
    // NOT_FOUND says what was not found.
    let err = e.read_op(&json!({"op": "list_changes", "change_set": "cs_99"})).unwrap_err();
    assert_eq!(err["error"]["what"], json!("change_set"));
    let err = e.read_op(&json!({"op": "get_image", "asset": "a_0000000000000000"})).unwrap_err();
    assert_eq!(err["error"]["what"], json!("asset"));
}

// ------------------------------------------------------------------------------ behaviour

#[test]
fn list_changes_follows_the_session() {
    let mut e = engine(&fixture("02-headings"));
    let vm = e.view_model().unwrap();
    let before = ok(&e, json!({"op": "list_changes"}));
    let n0 = before["data"]["revisions"].as_array().unwrap().len();
    let target = vm.blocks.iter().find_map(|b| match b {
        quillwork_core::project::vm::Block::P(p)
            if p.inlines.iter().any(|i| i.t.as_deref().is_some_and(|t| t.len() > 3)) =>
        {
            Some(p.id.clone())
        }
        _ => None,
    });
    let h = target.expect("a paragraph with text");
    let r = apply(&mut e, "track", json!([{"op": "insert_paragraph", "after": h, "text": "A tracked line."}]));
    let cs = r["change_set"].as_str().expect("change set").to_owned();

    let all = ok(&e, json!({"op": "list_changes"}));
    let revs = all["data"]["revisions"].as_array().unwrap();
    assert!(revs.len() > n0, "the insert shows up");
    let mine: Vec<&Value> = revs.iter().filter(|r| r["change_set"] == json!(cs)).collect();
    assert!(!mine.is_empty());
    for r in &mine {
        assert_eq!(r["agent"], json!(true));
        assert_eq!(r["author"], json!(AGENT));
        assert_eq!(r["story"], json!("b"));
        assert!(r["id"].is_u64());
    }
    assert!(mine.iter().any(|r| r["type"] == json!("ins") && r["text"].as_str().unwrap().contains("A tracked line.")));
    let sets = all["data"]["change_sets"].as_array().unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0]["id"], json!(cs));
    assert_eq!(sets[0]["status"], json!("open"));

    // Filters.
    let by_cs = ok(&e, json!({"op": "list_changes", "change_set": cs}));
    assert_eq!(by_cs["data"]["revisions"].as_array().unwrap().len(), mine.len());
    let by_ai = ok(&e, json!({"op": "list_changes", "author": "AI"}));
    assert_eq!(by_ai["data"]["revisions"].as_array().unwrap().len(), mine.len());
    let by_other = ok(&e, json!({"op": "list_changes", "author": "Somebody Else"}));
    assert!(by_other["data"]["revisions"].as_array().unwrap().is_empty());
    assert!(by_other["data"]["change_sets"].as_array().unwrap().is_empty());
    let open = ok(&e, json!({"op": "list_changes", "status": "open"}));
    assert!(open["data"]["revisions"].as_array().unwrap().len() >= mine.len());

    // Accepting closes the change set; its revisions leave the document.
    apply(&mut e, "track", json!([{"op": "accept", "change_set": cs}]));
    let accepted = ok(&e, json!({"op": "list_changes", "status": "accepted"}));
    assert_eq!(accepted["data"]["change_sets"][0]["id"], json!(cs));
    let after = ok(&e, json!({"op": "list_changes", "change_set": cs}));
    assert!(after["data"]["revisions"].as_array().unwrap().is_empty());
    assert!(
        ok(&e, json!({"op": "list_changes", "status": "open"}))["data"]["change_sets"].as_array().unwrap().is_empty()
    );
}

#[test]
fn list_comments_threads_and_open_only() {
    let mut e = engine(&fixture("07-comments"));
    let all = ok(&e, json!({"op": "list_comments"}));
    let comments = all["data"]["comments"].as_array().unwrap().clone();
    assert!(!comments.is_empty());
    for c in &comments {
        let keys: Vec<&str> = c.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 7, "{keys:?}");
        for k in ["id", "author", "done", "on", "anchor", "text", "replies"] {
            assert!(keys.contains(&k), "{k}");
        }
    }
    let first = comments[0]["id"].as_str().unwrap().to_owned();
    let n_open =
        ok(&e, json!({"op": "list_comments", "open_only": true}))["data"]["comments"].as_array().unwrap().len();
    // A reply lands under its thread; resolving it drops it from open_only.
    apply(&mut e, "track", json!([{"op": "reply_comment", "comment": first, "text": "Done, see the new wording."}]));
    let all = ok(&e, json!({"op": "list_comments"}));
    let thread = all["data"]["comments"].as_array().unwrap().iter().find(|c| c["id"] == json!(first)).unwrap().clone();
    let reply = thread["replies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["author"] == json!(AGENT))
        .unwrap_or_else(|| panic!("{thread}"));
    assert_eq!(reply["text"], json!("Done, see the new wording."));
    if thread["done"] == json!(false) {
        apply(&mut e, "track", json!([{"op": "resolve_comment", "comment": first, "done": true}]));
        let open = ok(&e, json!({"op": "list_comments", "open_only": true}));
        assert_eq!(open["data"]["comments"].as_array().unwrap().len(), n_open - 1);
        assert!(open["data"]["comments"].as_array().unwrap().iter().all(|c| c["id"] != json!(first)));
    }
}

#[test]
fn get_image_returns_the_bytes() {
    let e = engine(&fixture("09-images"));
    let list = ok(&e, json!({"op": "list_images"}));
    let images = list["data"]["images"].as_array().unwrap();
    assert!(!images.is_empty());
    for img in images {
        assert!(img["story"].is_string() && img["para"].is_string());
        let Some(asset) = img["asset"].as_str() else { continue };
        let by_image = ok(&e, json!({"op": "get_image", "image": img["id"]}));
        let by_asset = ok(&e, json!({"op": "get_image", "asset": asset}));
        assert_eq!(by_image["data"]["base64"], by_asset["data"]["base64"]);
        assert_eq!(by_asset["data"]["image"], Value::Null);
        let b64 = by_asset["data"]["base64"].as_str().unwrap();
        let bytes = by_asset["data"]["bytes"].as_u64().unwrap() as usize;
        assert_eq!(b64.len(), bytes.div_ceil(3) * 4);
        assert_eq!(img["media"]["mime"], by_asset["data"]["mime"]);
    }
}

#[test]
fn reads_see_each_version() {
    let mut e = engine(&fixture("03-lists"));
    let vm = e.view_model().unwrap();
    let h = vm
        .blocks
        .iter()
        .find_map(|b| matches!(b, quillwork_core::project::vm::Block::P(_)).then(|| b.id().to_owned()))
        .unwrap();
    let before = ok(&e, json!({"op": "get_block", "anchor": h}));
    apply(&mut e, "direct", json!([{"op": "insert_paragraph", "after": h, "text": "Fresh."}]));
    let after = ok(&e, json!({"op": "list_changes"}));
    assert_eq!(after["version"], json!(e.version()));
    let again = ok(&e, json!({"op": "get_block", "anchor": h}));
    assert_eq!(before["data"]["block"]["id"], again["data"]["block"]["id"]);
    // Without a host view model, the read ops do not switch op results to full patches.
    let mut e = engine(&fixture("03-lists"));
    ok(&e, json!({"op": "list_changes"}));
    let r = apply(&mut e, "direct", json!([{"op": "insert_paragraph", "after": h, "text": "Fresh."}]));
    assert_eq!(r["patch"]["upserts"], json!([]), "a host without a view model gets the short patch");
}

// ------------------------------------------------------------------------------ lint

fn rules(r: &Value) -> Vec<(String, String)> {
    r["lint"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["rule"].as_str().unwrap().to_owned(), f["anchor"].as_str().unwrap().to_owned()))
        .collect()
}

#[test]
fn op_results_lint_their_blocks() {
    let s = schemas();
    // Deleting the paragraph a REF points to breaks the reference, directly or tracked.
    for mode in ["direct", "track"] {
        let mut e = engine(&fixture("11-fields"));
        let lint = ok(&e, json!({"op": "lint"}));
        assert!(!lint["text"].as_str().unwrap().contains("broken_ref"), "no broken ref to start with");
        let target = {
            let text = ok(&e, json!({"op": "read", "budget": 1_000_000}))["text"].as_str().unwrap().to_owned();
            // The paragraph the REF field's bookmark sits in: "Here is the important clause."
            let line = text
                .lines()
                .find(|l| l.contains("Here is the important clause."))
                .expect("bookmarked paragraph")
                .trim_start();
            line[1..line.find([']', ' ']).unwrap()].to_owned()
        };
        let r = apply(&mut e, mode, json!([{"op": "delete_block", "anchor": target}]));
        let errs = schema_paths(&s.op_result, &r);
        assert!(errs.is_empty(), "{mode}: {errs:?}");
        assert!(rules(&r).iter().any(|(rule, _)| rule == "broken_ref"), "{mode}: {}", r["lint"]);
        // A tracked deletion keeps the bookmark in the deleted paragraph, so the document lint
        // sees it too; a direct one moves the bookmark to the next paragraph, which only the op
        // result (knowing what was deleted) can tell.
        let doc_lint = ok(&e, json!({"op": "lint"}))["text"].as_str().unwrap().contains("broken_ref");
        assert_eq!(doc_lint, mode == "track", "{mode}");
    }
    // A double space and an emptied paragraph.
    let mut e = engine(&fixture("02-headings"));
    let text = ok(&e, json!({"op": "read", "budget": 1_000_000}))["text"].as_str().unwrap().to_owned();
    let (h, word) = text
        .lines()
        .filter_map(|l| {
            let l = l.trim_start().strip_prefix('[')?;
            let (h, rest) = l.split_once("] ")?;
            let w = rest
                .split(' ')
                .find(|w| w.len() >= 4 && w.chars().all(char::is_alphabetic) && rest.matches(*w).count() == 1)?;
            Some((h.to_owned(), w.to_owned()))
        })
        .nth(1)
        .expect("a word");
    let r = apply(
        &mut e,
        "direct",
        json!([{"op": "replace_text", "anchor": h, "find": word, "replace": format!("{word}  again")}]),
    );
    assert!(rules(&r).contains(&("double_space".into(), h.clone())), "{}", r["lint"]);
    assert!(schema_paths(&s.op_result, &r).is_empty());
    let r2 = apply(&mut e, "direct", json!([{"op": "insert_paragraph", "after": h, "text": "Clean text."}]));
    assert!(
        rules(&r2).iter().all(|(rule, _)| rule != "double_space"),
        "lint only covers the changed blocks: {}",
        r2["lint"]
    );
    let lint = ok(&e, json!({"op": "lint"}))["text"].as_str().unwrap().to_owned();
    assert!(lint.lines().any(|l| l.starts_with(&format!("§ lint: info double_space {h} — "))), "{lint}");
}

#[test]
fn lint_text_format() {
    for (name, path) in fixtures() {
        let e = engine(&path);
        let t = ok(&e, json!({"op": "lint"}))["text"].as_str().unwrap().to_owned();
        for l in t.lines() {
            let rest = l.strip_prefix("§ lint: ").unwrap_or_else(|| panic!("{name}: {l}"));
            if rest == "none" || rest.starts_with("… (+") {
                continue;
            }
            let mut parts = rest.splitn(4, ' ');
            let sev = parts.next().unwrap();
            assert!(sev == "info" || sev == "warning", "{name}: {l}");
            assert!(parts.next().is_some_and(|r| r.chars().all(|c| c.is_ascii_lowercase() || c == '_')), "{name}: {l}");
            assert!(parts.next().is_some(), "{name}: {l}");
            assert!(parts.next().is_some_and(|m| m.starts_with("— ")), "{name}: {l}");
        }
    }
}

// ------------------------------------------------------------------------------ corpus

/// Every read op on every synthetic and private document: no crash, schema-valid results.
/// Prints counts only.
#[test]
fn read_ops_on_the_corpus() {
    let s = schemas();
    let mut docs: Vec<(String, PathBuf)> = fixtures();
    for p in xml_support::synthetic_docx() {
        docs.push((p.file_name().unwrap().to_string_lossy().into_owned(), p));
    }
    for (i, p) in xml_support::private_docx().into_iter().enumerate() {
        docs.push((format!("private#{i}"), p));
    }
    let (mut n, mut revs, mut comments, mut images, mut fields, mut findings, mut blocks) = (0, 0, 0, 0, 0, 0, 0);
    for (name, path) in &docs {
        let e = engine(path);
        let vm = e.view_model().unwrap_or_else(|_| panic!("{name}: view model"));
        let mut reqs = vec![
            json!({"op": "list_changes"}),
            json!({"op": "list_comments"}),
            json!({"op": "list_comments", "open_only": true}),
            json!({"op": "list_images"}),
            json!({"op": "list_fields"}),
            json!({"op": "lint"}),
        ];
        for b in vm.blocks.iter().take(25) {
            reqs.push(json!({"op": "get_block", "anchor": b.id()}));
        }
        if let Some(img) = first_image(&e) {
            reqs.push(json!({"op": "get_image", "image": img}));
        }
        for req in reqs {
            match e.read_op(&req) {
                Ok(r) => {
                    let errs = schema_paths(&s.read_result, &r);
                    assert!(errs.is_empty(), "{name}: {}: {errs:?}", req["op"]);
                    let d = &r["data"];
                    match req["op"].as_str().unwrap() {
                        "list_changes" => revs += d["revisions"].as_array().unwrap().len(),
                        "list_comments" if req.get("open_only").is_none() => {
                            comments += d["comments"].as_array().unwrap().len()
                        }
                        "list_images" => images += d["images"].as_array().unwrap().len(),
                        "list_fields" => {
                            fields +=
                                d["controls"].as_array().unwrap().len() + d["placeholders"].as_array().unwrap().len()
                        }
                        "lint" => {
                            findings += r["text"].as_str().unwrap().lines().filter(|l| *l != "§ lint: none").count()
                        }
                        "get_block" => blocks += 1,
                        _ => {}
                    }
                }
                // A linked or shape-only picture has no bytes to return.
                Err(err) if req["op"] == "get_image" && code(&err) == "UNSUPPORTED" => {}
                Err(err) => panic!("{name}: {}: {}", req["op"], code(&err)),
            }
        }
        n += 1;
    }
    eprintln!(
        "read ops on {n} documents: {revs} revisions, {comments} comment threads, {images} images, {fields} fields, {findings} lint lines, {blocks} blocks"
    );
}

/// The §9 text of an op result: `§ removed:`, `§ outline:` (only when headings changed),
/// `§ lint:` and `§ warning:`, in that order, after the changed lines.
#[test]
fn result_text_carries_outline_and_lint() {
    let mut e = engine(&fixture("11-fields"));
    let tx = json!({"base_version": e.version(), "mode": "direct", "ops": [
        {"op": "insert_paragraph", "after": "b104", "text": "Deep heading {name}", "style": "Heading3"},
        {"op": "delete_block", "anchor": "b102"},
    ]});
    let t = e.apply_text(&tx.to_string());
    let tags: Vec<&str> =
        t.lines().filter_map(|l| l.strip_prefix("§ ")).map(|l| l.split(':').next().unwrap_or(l)).collect();
    let order: Vec<&str> = tags.iter().copied().filter(|t| ["removed", "outline", "lint"].contains(t)).collect();
    assert_eq!(order.first(), Some(&"removed"), "{t}");
    assert_eq!(order.iter().filter(|t| **t == "outline").count(), 1, "{t}");
    assert!(order.windows(2).all(|w| !(w[0] == "lint" && w[1] != "lint")), "{t}");
    assert!(t.lines().any(|l| l.starts_with("§ outline: [") && l.contains("Deep heading")), "{t}");
    assert!(
        t.lines().any(|l| l.starts_with("§ lint: warning leftover_placeholder ") && l.ends_with("\"{name}\"")),
        "{t}"
    );
    assert!(t.lines().any(|l| l.starts_with("§ lint: warning heading_order ")), "{t}");

    // No headings changed and nothing to lint: neither line.
    let tx = json!({"base_version": e.version(), "mode": "direct", "ops": [
        {"op": "insert_paragraph", "after": "b103", "text": "Plain."},
    ]});
    let t = e.apply_text(&tx.to_string());
    assert!(!t.contains("§ outline:") && !t.contains("§ lint:"), "{t}");
}
