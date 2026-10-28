//! Public synthetic editing-policy regressions; no Office password or identity bypass.
use quillwork_core::{
    engine::Engine,
    opc::Package,
    xml::{Document, ns},
};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/fixtures").join(name)).unwrap()
}

fn rewrite(bytes: Vec<u8>, part: &str, f: impl FnOnce(String) -> String) -> Vec<u8> {
    let mut package = Package::open(bytes, &Default::default()).unwrap();
    let xml = package.part_str(part).unwrap().into_owned();
    package.set_part(part, f(xml).into_bytes()).unwrap();
    package.save().unwrap()
}

fn restriction(edit: &str, enforcement: &str) -> Vec<u8> {
    rewrite(fixture("01-run-formatting.docx"), "word/settings.xml", |s| {
        s.replace(
            "</w:settings>",
            &format!(r#"<w:documentProtection w:edit="{edit}" w:enforcement="{enforcement}"/></w:settings>"#),
        )
    })
}

fn wrap(bytes: Vec<u8>, element: &str, index: usize, lock: &str) -> Vec<u8> {
    rewrite(bytes, "word/document.xml", |s| {
        let doc = Document::parse(s.as_bytes()).unwrap();
        let node = doc.descendants_named(doc.root(), ns::W, element).nth(index).unwrap();
        let target = String::from_utf8(doc.node_to_bytes(node)).unwrap();
        let wrapped = format!(
            r#"<w:sdt><w:sdtPr><w:lock w:val="{lock}"/></w:sdtPr><w:sdtContent>{target}</w:sdtContent></w:sdt>"#
        );
        s.replacen(&target, &wrapped, 1)
    })
}

fn apply(engine: &mut Engine, mode: &str, ops: Value) -> Result<Value, Value> {
    engine.apply_tx(&json!({"base_version":engine.version(), "mode":mode, "ops":ops}))
}

fn protected(engine: &mut Engine, mode: &str, ops: Value, reason: &str) {
    let before = engine.save().unwrap();
    let version = engine.version();
    let err = apply(engine, mode, ops).unwrap_err();
    assert_eq!(err["error"]["code"], "PROTECTED", "{err}");
    assert_eq!(err["error"]["reason"], reason);
    assert_eq!(engine.version(), version);
    assert_eq!(engine.save().unwrap(), before, "rejected operation must preserve bytes, anchors and history");
}

fn text_op() -> Value {
    json!({"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"simple"})
}

#[test]
fn enforced_document_modes_preserve_allowed_edit_categories() {
    for edit in ["readOnly", "forms", "futureMode"] {
        for mode in ["track", "direct"] {
            let mut engine = Engine::open(restriction(edit, "1"), None).unwrap();
            protected(&mut engine, mode, json!([text_op()]), "document_protection");
        }
    }
    for (edit, enforced) in [("none", "1"), ("readOnly", "0"), ("readOnly", "false")] {
        let mut engine = Engine::open(restriction(edit, enforced), None).unwrap();
        apply(&mut engine, "direct", json!([text_op()])).unwrap();
    }
    let mut comments = Engine::open(restriction("comments", "on"), None).unwrap();
    protected(&mut comments, "track", json!([text_op()]), "document_protection");
    apply(&mut comments, "direct", json!([{"op":"add_comment", "anchor":"b100", "text":"Review this"}])).unwrap();
    let mut tracked = Engine::open(restriction("trackedChanges", "true"), None).unwrap();
    protected(&mut tracked, "direct", json!([text_op()]), "document_protection");
    let mut overridden = text_op();
    overridden["mode"] = json!("direct");
    protected(&mut tracked, "track", json!([overridden]), "document_protection");
    let changed = apply(&mut tracked, "track", json!([text_op()])).unwrap();
    for action in ["accept", "reject"] {
        protected(
            &mut tracked,
            "track",
            json!([{"op":action, "change_set":changed["change_set"]}]),
            "document_protection",
        );
    }
    protected(&mut tracked, "track", json!([{"op":"set_track_changes", "on":false}]), "document_protection");
}

#[test]
fn table_mutations_respect_locked_ancestors_and_descendants() {
    for (element, index) in [("tbl", 0), ("tr", 0), ("tc", 0), ("r", 1)] {
        for mode in ["track", "direct"] {
            let bytes = wrap(fixture("04-tables.docx"), element, index, "contentLocked");
            let mut engine = Engine::open(bytes, None).unwrap();
            protected(
                &mut engine,
                mode,
                json!([{"op":"set_cell", "cell":"t1.r1.c1", "text":"LOCK BYPASS"}]),
                "locked_control",
            );
        }
    }
    for mode in ["track", "direct"] {
        let bytes = wrap(fixture("04-tables.docx"), "r", 1, "sdtContentLocked");
        let mut engine = Engine::open(bytes, None).unwrap();
        protected(&mut engine, mode, json!([{"op":"delete_row", "row":"t1.r1"}]), "locked_control");
    }
}

#[test]
fn unlocked_cells_and_delete_only_locks_remain_editable() {
    let bytes = wrap(fixture("04-tables.docx"), "r", 1, "contentLocked");
    let mut engine = Engine::open(bytes, None).unwrap();
    apply(&mut engine, "direct", json!([{"op":"set_cell", "cell":"t1.r1.c2", "text":"UNLOCKED"}])).unwrap();
    for mode in ["track", "direct"] {
        let bytes = wrap(fixture("04-tables.docx"), "tbl", 0, "sdtLocked");
        let mut engine = Engine::open(bytes, None).unwrap();
        apply(&mut engine, mode, json!([{"op":"set_cell", "cell":"t1.r1.c1", "text":"EDITABLE"}])).unwrap();
    }
    let bytes = wrap(fixture("04-tables.docx"), "r", 1, "sdtLocked");
    let mut engine = Engine::open(bytes, None).unwrap();
    protected(&mut engine, "direct", json!([{"op":"delete_row", "row":"t1.r1"}]), "locked_control");
}

#[test]
fn review_cannot_rewrite_locked_revisions_and_late_failure_is_atomic() {
    for action in ["accept", "reject"] {
        let bytes = wrap(fixture("08-tracked-changes.docx"), "p", 0, "contentLocked");
        let mut engine = Engine::open(bytes, None).unwrap();
        protected(&mut engine, "track", json!([{"op":action, "revision_ids":[102]}]), "locked_control");
    }
    let bytes = wrap(fixture("04-tables.docx"), "r", 1, "contentLocked");
    let mut engine = Engine::open(bytes, None).unwrap();
    protected(
        &mut engine,
        "direct",
        json!([
            {"op":"set_cell", "cell":"t1.r1.c2", "text":"EARLIER VALID OP"},
            {"op":"set_cell", "cell":"t1.r1.c1", "text":"LOCK BYPASS"}
        ]),
        "locked_control",
    );
}

#[test]
fn text_and_comment_categories_honor_content_lock_semantics() {
    let bytes = wrap(fixture("04-tables.docx"), "tbl", 0, "contentLocked");
    let mut engine = Engine::open(bytes, None).unwrap();
    protected(
        &mut engine,
        "track",
        json!([{"op":"replace_text", "anchor":"b101", "replace":"LOCK BYPASS"}]),
        "locked_control",
    );
    apply(&mut engine, "track", json!([{"op":"add_comment", "anchor":"b101", "text":"Review locked source"}])).unwrap();
    // An annotation does not unlock the underlying source for a later operation.
    protected(
        &mut engine,
        "direct",
        json!([{"op":"set_cell", "cell":"t1.r1.c1", "text":"LOCK BYPASS"}]),
        "locked_control",
    );
}

#[test]
fn formatting_restrictions_fail_closed_without_a_style_permission_policy() {
    let bytes = rewrite(restriction("none", "1"), "word/settings.xml", |s| {
        s.replace("w:edit=\"none\"", "w:edit=\"none\" w:formatting=\"1\"")
    });
    let mut engine = Engine::open(bytes, None).unwrap();
    protected(&mut engine, "track", json!([text_op()]), "document_protection");
    apply(&mut engine, "track", json!([{"op":"add_comment", "anchor":"b100", "text":"Comment remains permitted"}]))
        .unwrap();
}
