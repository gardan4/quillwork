//! Public-fixture regressions for the October 2026 audit.
mod opc_common;

use std::collections::HashSet;

use opc_common::{E, build_zip};
use quillwork_core::engine::Engine;
use quillwork_core::wml::text::{self, TextMode};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/fixtures").join(name)).unwrap()
}

fn apply(engine: &mut Engine, ops: Value) -> Value {
    engine.apply_tx(&json!({"base_version": engine.version(), "ops": ops})).unwrap()
}

fn snapshot(engine: &mut Engine) -> (Vec<u8>, String) {
    engine.save().unwrap()
}

fn accepted(engine: &Engine) -> String {
    let doc = engine.doc();
    let d = doc.part(doc.main_part());
    text::text(d, d.root(), TextMode::AcceptAll)
}

#[test]
fn historic_change_set_ids_are_not_reused_or_replayed_after_reopen() {
    for action in ["accept", "reject"] {
        let mut engine = Engine::open(fixture("01-run-formatting.docx"), None).unwrap();
        let old = apply(
            &mut engine,
            json!([{"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"simple"}]),
        )["change_set"]
            .as_str()
            .unwrap()
            .to_owned();
        let old_ids: HashSet<_> = engine.change_sets()[0].revisions.iter().copied().collect();
        apply(&mut engine, json!([{"op":"accept", "change_set":old}]));
        let (bytes, sidecar) = snapshot(&mut engine);
        let mut engine = Engine::open(bytes, Some(&sidecar)).unwrap();
        apply(&mut engine, json!([{"op":"replace_text", "anchor":"b100", "find":"simple", "replace":"easy"}]));
        let new_ids: HashSet<_> = engine.change_sets()[1].revisions.iter().copied().collect();
        assert!(old_ids.is_disjoint(&new_ids), "historical revision ids must remain reserved");
        let before = snapshot(&mut engine);
        let error = engine
            .apply_tx(&json!({"base_version":engine.version(), "ops":[{"op":action, "change_set":old}]}))
            .unwrap_err();
        assert_eq!(error["error"]["code"], "NOT_FOUND");
        assert_eq!(snapshot(&mut engine), before);
        assert_eq!(engine.change_sets()[1].status(), "open");
        assert!(accepted(&engine).contains("easy"));
    }
}

#[test]
fn resolved_change_set_cannot_select_reused_ids_from_external_import() {
    let mut engine = Engine::open(fixture("01-run-formatting.docx"), None).unwrap();
    let old = apply(&mut engine, json!([{"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"simple"}]))
        ["change_set"].as_str().unwrap().to_owned();
    apply(&mut engine, json!([{"op":"accept", "change_set":old}]));
    let (_, sidecar) = snapshot(&mut engine);
    // A fresh editor can issue the same numeric ids. Attaching old history must not make a
    // resolved old change set own those new revisions.
    let mut external = Engine::open(fixture("01-run-formatting.docx"), None).unwrap();
    apply(&mut external, json!([{"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"imported"}]));
    let (bytes, _) = snapshot(&mut external);
    let mut imported = Engine::open(bytes, Some(&sidecar)).unwrap();
    let before = imported.read(r#"{"op":"list_changes"}"#);
    let error = imported
        .apply_tx(&json!({"base_version":imported.version(), "ops":[{"op":"accept", "change_set":old}]}))
        .unwrap_err();
    assert_eq!(error["error"]["code"], "NOT_FOUND");
    let after = imported.read(r#"{"op":"list_changes"}"#);
    let revisions = |s: &str| serde_json::from_str::<Value>(s).unwrap()["data"]["revisions"].clone();
    assert_eq!(revisions(&before), revisions(&after));
}

#[test]
fn stale_table_cells_are_refused_after_content_and_structure_changes() {
    for op in [
        json!({"op":"set_cell", "cell":"t1.r1.c1", "text":"FIRST EDIT", "mode":"direct"}),
        json!({"op":"replace_text", "anchor":"b101", "replace":"FIRST EDIT", "mode":"direct"}),
        json!({"op":"insert_row", "after":"t1.r1", "cells":["", "NEW ROW"], "mode":"direct"}),
    ] {
        let mut engine = Engine::open(fixture("04-tables.docx"), None).unwrap();
        let base = engine.version();
        apply(&mut engine, json!([op]));
        let before = snapshot(&mut engine);
        let error = engine
            .apply_tx(&json!({"base_version":base, "mode":"direct", "ops":[
                {"op":"set_cell", "cell":"t1.r2.c1", "text":"STALE OVERWRITE"}
            ]}))
            .unwrap_err();
        assert_eq!(error["error"]["code"], "STALE_ANCHOR");
        assert_eq!(snapshot(&mut engine), before);
    }
}

#[test]
fn merged_row_nonempty_continuation_is_rejected_atomically() {
    for mode in ["direct", "track"] {
        let mut engine = Engine::open(fixture("04-tables.docx"), None).unwrap();
        let before = snapshot(&mut engine);
        let error = engine
            .apply_tx(&json!({"base_version":engine.version(), "mode":mode, "ops":[
                {"op":"insert_row", "after":"t1.r2", "cells":["SUPPLIED-A", "SUPPLIED-B", "SUPPLIED-C"]}
            ]}))
            .unwrap_err();
        assert_eq!(error["error"]["code"], "UNSUPPORTED");
        assert_eq!(snapshot(&mut engine), before);
    }
}

#[test]
fn maximal_version_restores_without_iteration_and_cannot_overflow() {
    let mut engine = Engine::open(fixture("01-run-formatting.docx"), None).unwrap();
    let (bytes, sidecar) = snapshot(&mut engine);
    let mut sidecar: Value = serde_json::from_str(&sidecar).unwrap();
    // A billion versions exposes any linear restore without an unbounded test process.
    sidecar["engine"]["version"] = json!(1_000_000_000_u64);
    let started = std::time::Instant::now();
    let engine = Engine::open(bytes.clone(), Some(&sidecar.to_string())).unwrap();
    assert_eq!(engine.version(), 1_000_000_000);
    assert!(started.elapsed().as_secs() < 1, "restoring version must not replay commits");
    sidecar["engine"]["version"] = json!(u64::MAX);
    let mut engine = Engine::open(bytes, Some(&sidecar.to_string())).unwrap();
    let before = snapshot(&mut engine);
    let error = engine
        .apply_tx(&json!({"base_version":u64::MAX, "ops":[
            {"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"simple"}
        ]}))
        .unwrap_err();
    assert_eq!(error["error"]["code"], "BAD_VERSION");
    assert_eq!(snapshot(&mut engine), before);
}

#[test]
fn word_model_refuses_presentation_content_types_and_roots() {
    let cases = [
        (
            "ppt/presentation.xml",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
            r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#,
        ),
        (
            "word/document.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#,
        ),
        (
            "word/document.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            opc_common::DOC_XML,
        ),
    ];
    for (part, content_type, xml) in cases {
        let ct = format!(
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/{part}" ContentType="{content_type}"/></Types>"#
        );
        let rel = format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="{part}"/></Relationships>"#
        );
        let bytes = build_zip(
            &[
                E::new("[Content_Types].xml", ct.as_bytes()),
                E::new("_rels/.rels", rel.as_bytes()),
                E::new(part, xml.as_bytes()),
            ],
            b"",
        );
        assert!(Engine::open(bytes, None).is_err(), "unsupported Word main part {part}");
    }
}

#[test]
fn saving_between_insertion_and_rejection_does_not_retain_rejected_rows() {
    let original = fixture("04-tables.docx");
    let mut engine = Engine::open(original.clone(), None).unwrap();
    let inserted = apply(
        &mut engine,
        json!([
            {"op":"insert_row", "after":"t1.r1", "cells":["FIRST", "SECOND"]}
        ]),
    );
    let change_set = inserted["change_set"].as_str().unwrap();
    let (candidate, _) = snapshot(&mut engine);
    assert_ne!(candidate, original);
    apply(&mut engine, json!([{"op":"reject", "change_set":change_set}]));
    assert!(!accepted(&engine).contains("FIRST"));
    let (saved, sidecar) = snapshot(&mut engine);
    let reopened = Engine::open(saved.clone(), Some(&sidecar)).unwrap();
    assert!(!accepted(&reopened).contains("FIRST"), "save must synchronize a part returned to its original XML");
    assert_eq!(saved, original, "rejecting the inserted row restores the original package bytes");
}

#[test]
fn writing_into_a_reused_package_restores_original_text_bytes() {
    use quillwork_core::{opc::Package, wml::WordDoc, xml::ns};
    let original = fixture("01-run-formatting.docx");
    let mut package = Package::open(original.clone(), &Default::default()).unwrap();
    let mut doc = WordDoc::load(&package).unwrap();
    let main = doc.main_part();
    let node = doc.part(main).descendants_named(doc.part(main).root(), ns::W, "t").next().unwrap();
    let text = doc.part(main).text_content(node);
    doc.part_mut(main).set_text(node, "saved candidate").unwrap();
    doc.write_into(&mut package).unwrap();
    assert_ne!(package.save().unwrap(), original);
    doc.part_mut(main).set_text(node, &text).unwrap();
    doc.write_into(&mut package).unwrap();
    assert_eq!(package.save().unwrap(), original);
}
