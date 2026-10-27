//! Review, comment and table edits on the fixtures and the private corpus.
//!
//! Every edit must leave the validator at zero errors and the saved package must re-validate.
//! With `QUILLWORK_EDIT_OUT` set, each fixture output is written there with a line in
//! `manifest.jsonl` (original, intended paragraph indices, check kind) for the Python oracles.
//! With `QUILLWORK_PRIVATE_OUT` set, private outputs are written as `private-<i>-<kind>.docx`
//! for the same oracles; callers delete them afterwards. Only counts are printed for private
//! documents, never text.

mod xml_support;

use std::io::Write;
use std::path::{Path, PathBuf};

use quillwork_core::anchors::{Anchors, Target};
use quillwork_core::edit::{self, EditOutcome, comments, review, tables};
use quillwork_core::validate;
use quillwork_core::wml::text::{self, TextMode};
use quillwork_core::wml::{Loc, StoryKind, WordDoc};
use quillwork_core::xml::ns;
use serde::de::DeserializeOwned;
use serde_json::json;

const AUTHOR: &str = "Quillwork Test";
const DATE: &str = "2026-10-26T12:00:00Z";

fn fixture(name: &str) -> PathBuf {
    xml_support::workspace_root().join("corpus/fixtures").join(name)
}

fn open(path: &Path) -> (WordDoc, Anchors) {
    let doc = WordDoc::open(std::fs::read(path).unwrap()).unwrap();
    let anchors = Anchors::mint(&doc);
    (doc, anchors)
}

fn args<T: DeserializeOwned>(v: serde_json::Value) -> T {
    serde_json::from_value(v).unwrap()
}

type EditFn<A> =
    fn(&mut WordDoc, &mut Anchors, &mut dyn FnMut() -> u32, &str, &str, &A) -> quillwork_core::Result<EditOutcome>;

fn run<A: DeserializeOwned>(
    doc: &mut WordDoc,
    anchors: &mut Anchors,
    f: EditFn<A>,
    a: serde_json::Value,
) -> quillwork_core::Result<EditOutcome> {
    let mut alloc = edit::id_allocator(doc);
    f(doc, anchors, &mut alloc, AUTHOR, DATE, &args::<A>(a))
}

/// Zero validator errors in memory and after a save/reopen; returns the saved bytes.
fn checked(label: &str, doc: &mut WordDoc) -> Vec<u8> {
    let r = validate::check(doc);
    assert!(r.is_ok(), "{label}: {:?}", r.errors().map(|f| &f.code).collect::<Vec<_>>());
    let bytes = doc.save().unwrap();
    let r = validate::check_bytes(&bytes);
    assert!(r.is_ok(), "{label} (saved): {:?}", r.errors().map(|f| &f.code).collect::<Vec<_>>());
    bytes
}

fn body_story(doc: &WordDoc) -> usize {
    doc.stories().iter().position(|s| matches!(s.kind, StoryKind::Body)).unwrap()
}

fn body_handles(doc: &WordDoc, anchors: &Anchors) -> Vec<String> {
    anchors.handles(body_story(doc)).map(|(h, _)| h.to_owned()).collect()
}

/// The locality oracle's index of a body paragraph: outermost w:p in the main part, in order.
fn outer_index(doc: &WordDoc, loc: Loc) -> usize {
    let d = doc.part(loc.part);
    let mut i = 0;
    for n in d.descendants(d.root()) {
        if !d.is(n, ns::W, "p") {
            continue;
        }
        let mut nested = false;
        let mut cur = d.parent(n);
        while let Some(a) = cur {
            if d.is(a, ns::W, "p") {
                nested = true;
                break;
            }
            cur = d.parent(a);
        }
        if nested {
            continue;
        }
        if n == loc.node {
            return i;
        }
        i += 1;
    }
    panic!("paragraph not in its part")
}

fn index_of(doc: &WordDoc, anchors: &Anchors, handle: &str) -> usize {
    match anchors.resolve(doc, handle) {
        Some(Target::Block { para, .. }) => outer_index(doc, para),
        other => panic!("{handle}: {other:?}"),
    }
}

/// Outer indices of the paragraphs inside a table (for table-structure edits).
fn table_paras(doc: &WordDoc, anchors: &Anchors, table: &str) -> Vec<usize> {
    let Some(Target::Table { table }) = anchors.resolve(doc, table) else { panic!("{table}") };
    let d = doc.part(table.part);
    d.descendants(table.node)
        .filter(|&n| d.is(n, ns::W, "p"))
        .map(|n| outer_index(doc, Loc::new(table.part, n)))
        .collect()
}

/// Main-part text, empty lines dropped: the in-crate AcceptAll view keeps an empty line for a
/// paragraph whose mark is deleted, where accepting merges it (the oracle checks exact lines).
fn main_text(doc: &WordDoc, mode: TextMode) -> String {
    let d = doc.part(doc.main_part());
    text::text(d, d.root(), mode).lines().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// Columns of `t1` where a whole-column op is possible (no merged cell straddles them).
fn free_columns(path: &Path, delete: bool) -> Vec<u32> {
    (1..=8)
        .filter(|&c| {
            let (mut doc, mut anchors) = open(path);
            if delete {
                run(
                    &mut doc,
                    &mut anchors,
                    tables::delete_column,
                    json!({"table": "t1", "column": c, "mode": "direct"}),
                )
                .is_ok()
            } else {
                run(&mut doc, &mut anchors, tables::insert_column, json!({"table": "t1", "at": c, "mode": "direct"}))
                    .is_ok()
            }
        })
        .collect()
}

/// Main-part paragraph indices that carry a listed revision (what accept/reject-all may touch).
fn revision_paras(doc: &WordDoc, anchors: &Anchors, list: &review::ChangeList) -> Vec<usize> {
    let body: Vec<String> = anchors.handles(body_story(doc)).map(|(h, _)| h.to_owned()).collect();
    let mut v: Vec<usize> = list
        .revisions
        .iter()
        .flat_map(|r| r.anchors.iter())
        .filter(|h| body.contains(h))
        .map(|h| index_of(doc, anchors, h))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn count_changes(doc: &WordDoc, anchors: &Anchors) -> usize {
    review::list_changes(doc, anchors, &args(json!({}))).unwrap().revisions.len()
}

struct Out {
    dir: Option<PathBuf>,
}

impl Out {
    fn new(var: &str) -> Out {
        let dir = std::env::var(var).ok().map(PathBuf::from);
        if let Some(d) = &dir {
            std::fs::create_dir_all(d).unwrap();
        }
        Out { dir }
    }

    fn write(&self, name: &str, original: &Path, bytes: &[u8], intended: &[usize], kind: &str) {
        let Some(dir) = &self.dir else { return };
        let path = dir.join(format!("{name}.docx"));
        std::fs::write(&path, bytes).unwrap();
        let line = json!({"name": name, "original": original, "edited": path, "intended": intended, "kind": kind});
        let mut m = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("manifest.jsonl")).unwrap();
        writeln!(m, "{line}").unwrap();
    }
}

// ---------------------------------------------------------------------- tracked changes

#[test]
fn tracked_changes_fixture() {
    let out = Out::new("QUILLWORK_EDIT_OUT");
    let path = fixture("08-tracked-changes.docx");
    let (doc, anchors) = open(&path);
    let list = review::list_changes(&doc, &anchors, &args(json!({}))).unwrap();
    let total = list.revisions.len();
    eprintln!("08: {total} revisions listed");
    assert!(total > 0);
    let accept_view = main_text(&doc, TextMode::AcceptAll);
    let reject_view = main_text(&doc, TextMode::RejectAll);
    let touched = revision_paras(&doc, &anchors, &list);

    for (accept, kind) in [(true, "accept_all"), (false, "reject_all")] {
        let (mut doc, mut anchors) = open(&path);
        let a: review::ReviewArgs = args(json!({"all": true}));
        let o = review::apply(&mut doc, &mut anchors, &a, accept).unwrap();
        eprintln!("08 {kind}: resolved {} ids", o.revisions.len());
        assert_eq!(count_changes(&doc, &anchors), 0, "{kind} left revisions");
        let after = main_text(&doc, TextMode::AsIs);
        assert_eq!(&after, if accept { &accept_view } else { &reject_view }, "{kind}");
        let bytes = checked(kind, &mut doc);
        out.write(&format!("08-{kind}"), &path, &bytes, &touched, kind);
    }

    // One revision at a time: each single accept and reject must validate.
    for r in &list.revisions {
        for accept in [true, false] {
            let (mut doc, mut anchors) = open(&path);
            let a: review::ReviewArgs = args(json!({"revision_ids": [r.id]}));
            let o = review::apply(&mut doc, &mut anchors, &a, accept).unwrap();
            // A move resolves as a pair (moveFrom + moveTo); everything else alone.
            let expect = if r.move_name.is_some() { 2 } else { 1 };
            assert_eq!(o.revisions.len(), expect, "{} {}", r.kind, r.id);
            assert_eq!(count_changes(&doc, &anchors), total - expect, "{} {}", r.kind, r.id);
            checked(&format!("08 {} {} {accept}", r.kind, r.id), &mut doc);
        }
    }

    // By author.
    let author = list.revisions.iter().find_map(|r| r.author.clone()).unwrap();
    let by_author = list.revisions.iter().filter(|r| r.author.as_deref() == Some(&author)).count();
    let (mut doc, mut anchors) = open(&path);
    let a: review::ReviewArgs = args(json!({"author": author}));
    review::apply(&mut doc, &mut anchors, &a, false).unwrap();
    assert_eq!(count_changes(&doc, &anchors), total - by_author);
    let bytes = checked("08 reject by author", &mut doc);
    out.write("08-reject-author", &path, &bytes, &touched, "edit");
}

#[test]
fn tracked_changes_private_corpus() {
    let out = Out::new("QUILLWORK_PRIVATE_OUT");
    let mut with_changes = 0;
    for (i, path) in xml_support::private_docx().into_iter().enumerate() {
        let (doc, anchors) = open(&path);
        let list = review::list_changes(&doc, &anchors, &args(json!({}))).unwrap();
        let n = list.revisions.len();
        let touched = revision_paras(&doc, &anchors, &list);
        eprintln!("private#{i}: {n} revisions in {} body paragraphs", touched.len());
        if n == 0 {
            continue;
        }
        with_changes += 1;
        let accept_view = main_text(&doc, TextMode::AcceptAll);
        let reject_view = main_text(&doc, TextMode::RejectAll);
        for (accept, kind) in [(true, "accept_all"), (false, "reject_all")] {
            let (mut doc, mut anchors) = open(&path);
            let o = review::apply(&mut doc, &mut anchors, &args(json!({"all": true})), accept).unwrap();
            assert_eq!(count_changes(&doc, &anchors), 0, "private#{i} {kind}");
            let same = main_text(&doc, TextMode::AsIs) == *if accept { &accept_view } else { &reject_view };
            eprintln!("private#{i} {kind}: resolved {} ids, text view matches: {same}", o.revisions.len());
            assert!(same, "private#{i} {kind}: text differs from the {kind} view");
            let bytes = checked(&format!("private#{i} {kind}"), &mut doc);
            out.write(&format!("private-{i}-{kind}"), &path, &bytes, &touched, kind);
        }
    }
    if !xml_support::private_docx().is_empty() {
        assert!(with_changes > 0, "no private document with tracked changes");
    }
}

// ---------------------------------------------------------------------- comments

#[test]
fn comments_fixture() {
    let out = Out::new("QUILLWORK_EDIT_OUT");
    let path = fixture("07-comments.docx");
    let (doc, anchors) = open(&path);
    let list = comments::list_comments(&doc, &anchors, &args(json!({}))).unwrap();
    let before = list.comments.len();
    eprintln!("07: {before} comments, {} replies", list.comments.iter().map(|c| c.replies.len()).sum::<usize>());
    assert!(before > 0);
    let first = list.comments[0].id.clone();
    let host = list.comments[0].anchor.clone();

    // add on a paragraph that has no comment yet, with a find range
    let (mut doc, mut anchors) = open(&path);
    let handles = body_handles(&doc, &anchors);
    let target = handles
        .iter()
        .find(|h| Some(h.as_str()) != host.as_deref() && !main_para_text(&doc, &anchors, h).trim().is_empty())
        .unwrap()
        .clone();
    let word = main_para_text(&doc, &anchors, &target).split_whitespace().next().unwrap().to_owned();
    let idx = index_of(&doc, &anchors, &target);
    run(&mut doc, &mut anchors, comments::add_comment, json!({"anchor": target, "find": word, "text": "Check this."}))
        .unwrap();
    assert_eq!(comments::list_comments(&doc, &anchors, &args(json!({}))).unwrap().comments.len(), before + 1);
    let bytes = checked("07 add", &mut doc);
    out.write("07-add", &path, &bytes, &[idx], "edit");

    // reply (adds its own range markers in the host paragraph), resolve, edit (comment parts only)
    let host_idx: Vec<usize> = {
        let (doc, anchors) = open(&path);
        host.iter().map(|h| index_of(&doc, &anchors, h)).collect()
    };
    for (name, f) in [
        (
            "reply",
            Box::new(|d: &mut WordDoc, a: &mut Anchors| {
                run(d, a, comments::reply_comment, json!({"comment": first, "text": "Agreed."})).map(|_| ())
            }) as Box<dyn Fn(&mut WordDoc, &mut Anchors) -> quillwork_core::Result<()>>,
        ),
        (
            "resolve",
            Box::new(|d: &mut WordDoc, a: &mut Anchors| {
                run(d, a, comments::resolve_comment, json!({"comment": first, "done": true})).map(|_| ())
            }),
        ),
        (
            "edit",
            Box::new(|d: &mut WordDoc, a: &mut Anchors| {
                run(d, a, comments::edit_comment, json!({"comment": first, "text": "Reworded."})).map(|_| ())
            }),
        ),
    ] {
        let (mut doc, mut anchors) = open(&path);
        f(&mut doc, &mut anchors).unwrap();
        let bytes = checked(&format!("07 {name}"), &mut doc);
        let intended: &[usize] = if name == "reply" { &host_idx } else { &[] };
        out.write(&format!("07-{name}"), &path, &bytes, intended, "edit");
    }

    // delete: the comment's anchors leave its host paragraph
    let (mut doc, mut anchors) = open(&path);
    run(&mut doc, &mut anchors, comments::delete_comment, json!({"comment": first})).unwrap();
    assert!(comments::list_comments(&doc, &anchors, &args(json!({}))).unwrap().comments.iter().all(|c| c.id != first));
    let bytes = checked("07 delete", &mut doc);
    out.write("07-delete", &path, &bytes, &host_idx, "edit");
}

#[test]
fn comments_on_a_document_without_comment_parts() {
    let out = Out::new("QUILLWORK_EDIT_OUT");
    let path = fixture("01-run-formatting.docx");
    let (mut doc, mut anchors) = open(&path);
    assert!(doc.comments_part().is_none());
    let h = body_handles(&doc, &anchors)[0].clone();
    let idx = index_of(&doc, &anchors, &h);
    let o = run(&mut doc, &mut anchors, comments::add_comment, json!({"anchor": h, "text": "First comment."})).unwrap();
    let id = comments::list_comments(&doc, &anchors, &args(json!({}))).unwrap().comments[0].id.clone();
    run(&mut doc, &mut anchors, comments::reply_comment, json!({"comment": id, "text": "A reply."})).unwrap();
    run(&mut doc, &mut anchors, comments::resolve_comment, json!({"comment": id, "done": true})).unwrap();
    eprintln!("01 add+reply+resolve: {:?}", o.revisions);
    let bytes = checked("01 comment", &mut doc);
    let reopened = WordDoc::open(bytes.clone()).unwrap();
    let ra = Anchors::mint(&reopened);
    let l = comments::list_comments(&reopened, &ra, &args(json!({}))).unwrap();
    assert_eq!((l.comments.len(), l.comments[0].replies.len(), l.comments[0].done), (1, 1, true));
    out.write("01-comment", &path, &bytes, &[idx], "edit");
}

fn main_para_text(doc: &WordDoc, anchors: &Anchors, h: &str) -> String {
    match anchors.resolve(doc, h) {
        Some(Target::Block { para, .. }) => text::text(doc.part(para.part), para.node, TextMode::AcceptAll),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------- tables

#[test]
fn table_edits_fixture() {
    let out = Out::new("QUILLWORK_EDIT_OUT");
    let path = fixture("04-tables.docx");
    let (doc, anchors) = open(&path);
    let all = table_paras(&doc, &anchors, "t1");
    let row1: Vec<usize> = {
        let Some(Target::Row { row, .. }) = anchors.resolve(&doc, "t1.r1") else { panic!() };
        let d = doc.part(row.part);
        d.descendants(row.node)
            .filter(|&n| d.is(n, ns::W, "p"))
            .map(|n| outer_index(&doc, Loc::new(row.part, n)))
            .collect()
    };
    let cell11 = row1[0];

    let ins_cols = free_columns(&path, false);
    let del_cols = free_columns(&path, true);
    eprintln!("04 t1: insert_column possible at {ins_cols:?}, delete_column at {del_cols:?}");
    let (ins_at, del_at) = (ins_cols[ins_cols.len() / 2], del_cols[0]);
    let mut cases: Vec<(&str, serde_json::Value, Vec<usize>)> = Vec::new();
    for mode in ["track", "direct"] {
        cases.push(("insert_row", json!({"after": "t1.r1", "cells": ["a", "b"], "mode": mode}), row1.clone()));
        cases.push(("delete_row", json!({"row": "t1.r2", "mode": mode}), all.clone()));
        cases.push(("set_cell", json!({"cell": "t1.r1.c1", "text": "Changed", "mode": mode}), vec![cell11]));
        cases.push((
            "insert_column",
            json!({"table": "t1", "at": ins_at, "cells": ["x"], "confirm": true, "mode": mode}),
            all.clone(),
        ));
        cases.push((
            "delete_column",
            json!({"table": "t1", "column": del_at, "confirm": true, "mode": mode}),
            all.clone(),
        ));
    }
    cases.push(("merge_cells", json!({"from": "t1.r1.c1", "to": "t1.r1.c2", "mode": "direct"}), all.clone()));
    cases.push(("split_cell", json!({"cell": "t1.r1.c1", "cols": 2, "mode": "direct"}), all.clone()));

    for (op, a, intended) in cases {
        let (mut doc, mut anchors) = open(&path);
        let mode = a["mode"].as_str().unwrap().to_owned();
        let r = match op {
            "insert_row" => run(&mut doc, &mut anchors, tables::insert_row, a),
            "delete_row" => run(&mut doc, &mut anchors, tables::delete_row, a),
            "set_cell" => run(&mut doc, &mut anchors, tables::set_cell, a),
            "insert_column" => run(&mut doc, &mut anchors, tables::insert_column, a),
            "delete_column" => run(&mut doc, &mut anchors, tables::delete_column, a),
            "merge_cells" => run(&mut doc, &mut anchors, tables::merge_cells, a),
            "split_cell" => run(&mut doc, &mut anchors, tables::split_cell, a),
            _ => unreachable!(),
        };
        let o = r.unwrap_or_else(|e| panic!("{op} {mode}: {e}"));
        let label = format!("04-{op}-{mode}");
        eprintln!("{label}: {} revisions", o.revisions.len());
        let bytes = checked(&label, &mut doc);
        out.write(&label, &path, &bytes, &intended, "edit");

        // Tracked structure edits accept and reject cleanly.
        if mode == "track" && !o.revisions.is_empty() {
            for accept in [true, false] {
                let mut d2 = WordDoc::open(bytes.clone()).unwrap();
                let mut a2 = Anchors::mint(&d2);
                review::apply(&mut d2, &mut a2, &args(json!({"all": true})), accept).unwrap();
                assert_eq!(count_changes(&d2, &a2), 0);
                let kind = if accept { "accept" } else { "reject" };
                let b2 = checked(&format!("{label} {kind}"), &mut d2);
                if !accept {
                    // Reject restores the original text.
                    assert_eq!(
                        main_text(&d2, TextMode::AsIs),
                        main_text(&open(&path).0, TextMode::AsIs),
                        "{label} reject"
                    );
                }
                out.write(&format!("{label}-{kind}"), &path, &b2, &intended, &format!("{kind}_of_edit"));
            }
        }
    }

    // Tracked column ops need confirm.
    let (mut doc, mut anchors) = open(&path);
    let e = run(&mut doc, &mut anchors, tables::insert_column, json!({"table": "t1", "at": 1})).unwrap_err();
    assert!(e.to_string().contains("CONFIRM_REQUIRED"), "{e}");
    // Merge in track mode is refused.
    assert!(run(&mut doc, &mut anchors, tables::merge_cells, json!({"from": "t1.r1.c1", "to": "t1.r1.c2"})).is_err());

    // insert_table after the first paragraph
    let (mut doc, mut anchors) = open(&path);
    let h = body_handles(&doc, &anchors)[0].clone();
    for mode in ["track", "direct"] {
        let (mut doc, mut anchors) = open(&path);
        run(
            &mut doc,
            &mut anchors,
            tables::insert_table,
            json!({"after": h, "rows": 2, "cols": 3, "header_row": true, "data": [["A", "B", "C"], ["1", "2", "3"]], "mode": mode}),
        )
        .unwrap();
        let label = format!("04-insert_table-{mode}");
        let bytes = checked(&label, &mut doc);
        out.write(&label, &path, &bytes, &[], "edit");
        if mode == "track" {
            for accept in [true, false] {
                let mut d2 = WordDoc::open(bytes.clone()).unwrap();
                let mut a2 = Anchors::mint(&d2);
                review::apply(&mut d2, &mut a2, &args(json!({"all": true})), accept).unwrap();
                assert_eq!(count_changes(&d2, &a2), 0);
                let kind = if accept { "accept" } else { "reject" };
                let b2 = checked(&format!("{label} {kind}"), &mut d2);
                if !accept {
                    assert_eq!(
                        main_text(&d2, TextMode::AsIs),
                        main_text(&open(&path).0, TextMode::AsIs),
                        "{label} reject"
                    );
                }
                out.write(&format!("{label}-{kind}"), &path, &b2, &[], &format!("{kind}_of_edit"));
            }
        }
    }
    let _ = (&mut doc, &mut anchors);
}

#[test]
#[ignore]
fn private_revision_kinds() {
    for (i, path) in xml_support::private_docx().into_iter().enumerate() {
        let (doc, anchors) = open(&path);
        let list = review::list_changes(&doc, &anchors, &args(json!({}))).unwrap();
        let mut kinds = std::collections::BTreeMap::new();
        for r in &list.revisions {
            *kinds.entry((r.kind, r.anchors.is_empty())).or_insert(0) += 1;
        }
        let rows: Vec<Vec<usize>> = list
            .revisions
            .iter()
            .filter(|r| r.kind == "rowIns" || r.kind == "rowDel")
            .map(|r| {
                r.anchors
                    .iter()
                    .filter(|h| matches!(anchors.resolve(&doc, h), Some(Target::Block { .. })))
                    .map(|h| index_of(&doc, &anchors, h))
                    .collect()
            })
            .collect();
        eprintln!("private#{i}: {kinds:?} row revisions at paragraphs {rows:?}");
    }
}
