//! Inherited run formatting must support explicit off values in review projections.
mod opc_common;

use opc_common::{E, build_zip, minimal_entries};
use quillwork_core::engine::Engine;
use serde_json::{Value, json};

fn document(direct: bool) -> Vec<u8> {
    let marks = if direct { r#"<w:rPr><w:b w:val="0"/><w:i w:val="0"/><w:caps w:val="0"/></w:rPr>"# } else { "" };
    styled_document("<w:b/><w:i/><w:caps/>", marks)
}

fn styled_document(style_marks: &str, run_marks: &str) -> Vec<u8> {
    let mut entries = minimal_entries();
    let marks = run_marks;
    for entry in &mut entries {
        if entry.name == b"word/document.xml" {
            entry.data = format!(r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r>{marks}<w:t>Styled text</w:t></w:r></w:p></w:body></w:document>"#).into_bytes();
        } else if entry.name == b"word/_rels/document.xml.rels" {
            entry.data = String::from_utf8(entry.data.clone()).unwrap().replace("</Relationships>",r#"<Relationship Id="styles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#).into_bytes();
        } else if entry.name == b"[Content_Types].xml" {
            entry.data = String::from_utf8(entry.data.clone()).unwrap().replace("</Types>",r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#).into_bytes();
        }
    }
    entries.push(E::new("word/styles.xml",format!(r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:shd w:fill="DDEEFF"/></w:pPr><w:rPr>{style_marks}</w:rPr></w:style></w:styles>"#).as_bytes()));
    build_zip(&entries, b"")
}

fn vm(engine: &Engine) -> Value {
    let model = serde_json::from_str(&engine.view_model_json()).unwrap();
    static VALIDATOR: std::sync::OnceLock<jsonschema::Validator> = std::sync::OnceLock::new();
    let validator = VALIDATOR.get_or_init(|| {
        let schema: Value = serde_json::from_str(include_str!("../../../spec/schema/view-model.schema.json")).unwrap();
        let common: Value = serde_json::from_str(include_str!("../../../spec/schema/common.schema.json")).unwrap();
        let registry = jsonschema::Registry::new()
            .add(common["$id"].as_str().unwrap(), common.clone())
            .unwrap()
            .prepare()
            .unwrap();
        jsonschema::options().with_registry(&registry).build(&schema).unwrap()
    });
    assert!(validator.is_valid(&model), "view model violates published schema");
    model
}

fn assert_off(model: &Value) {
    let paragraph = &model["blocks"][0];
    assert_eq!(paragraph["fmt"]["b"], true);
    assert_eq!(paragraph["fmt"]["i"], true);
    assert_eq!(paragraph["fmt"]["caps"], true);
    assert_eq!(paragraph["inlines"][0]["marks"]["b"], false);
    assert_eq!(paragraph["inlines"][0]["marks"]["i"], false);
    assert_eq!(paragraph["inlines"][0]["marks"]["caps"], false);
}

#[test]
fn direct_off_values_are_present_in_view_model_and_survive_reopen() {
    let mut engine = Engine::open(document(true), None).unwrap();
    assert_off(&vm(&engine));
    let (bytes, sidecar) = engine.save().unwrap();
    assert_off(&vm(&Engine::open(bytes, Some(&sidecar)).unwrap()));
}

#[test]
fn tracked_format_removal_and_review_preserve_override_semantics() {
    for action in ["accept", "reject"] {
        let mut engine = Engine::open(document(false), None).unwrap();
        let initial = vm(&engine);
        assert!(initial["blocks"][0]["inlines"][0].get("marks").is_none());
        let result=engine.apply_tx(&json!({"base_version":engine.version(),"ops":[{"op":"set_text_format","anchor":"b100","find":"Styled text","marks":{"bold":false,"italic":false,"caps":false}}]})).unwrap();
        assert_off(&vm(&engine));
        engine
            .apply_tx(&json!({"base_version":engine.version(),"ops":[{"op":action,"change_set":result["change_set"]}]}))
            .unwrap();
        let (bytes, sidecar) = engine.save().unwrap();
        let reopened = Engine::open(bytes, Some(&sidecar)).unwrap();
        if action == "accept" {
            assert_off(&vm(&reopened));
        } else {
            assert!(vm(&reopened)["blocks"][0]["inlines"][0].get("marks").is_none());
        }
    }
}

const DECORATIONS: &str = r#"<w:u w:val="double"/><w:strike/><w:dstrike/><w:smallCaps/><w:vertAlign w:val="superscript"/><w:highlight w:val="yellow"/><w:rFonts w:ascii="Calibri"/><w:sz w:val="28"/><w:color w:val="AA0000"/>"#;

fn assert_decorated(model: &Value) {
    let paragraph = &model["blocks"][0];
    let marks = &paragraph["inlines"][0]["marks"];
    assert_eq!(marks["u"], "double");
    for property in ["strike", "dstrike", "smallCaps", "sup"] {
        assert_eq!(marks[property], true, "{property}");
    }
    assert_eq!(marks["highlight"], "yellow");
    assert_eq!(paragraph["fmt"]["font"], "Calibri");
    assert_eq!(paragraph["fmt"]["size"], 28);
    assert_eq!(paragraph["fmt"]["color"], "#AA0000");
    assert_eq!(paragraph["fmt"]["shade"], "#DDEEFF");
}

fn assert_decoration_reset(model: &Value) {
    let paragraph = &model["blocks"][0];
    let marks = &paragraph["inlines"][0]["marks"];
    for property in ["u", "strike", "dstrike", "smallCaps", "sup", "sub", "highlight"] {
        assert!(marks.get(property).is_none(), "{property}: {marks}");
    }
    assert_eq!(marks["color"], "auto");
    assert_eq!(marks["font"], "Arial");
    assert_eq!(marks["size"], 22);
    // No run highlight must retain the independent paragraph shading.
    assert_eq!(paragraph["fmt"]["shade"], "#DDEEFF");
}

#[test]
fn style_decorations_and_explicit_resets_are_effective_run_values() {
    let baseline = Engine::open(styled_document(DECORATIONS, ""), None).unwrap();
    assert_decorated(&vm(&baseline));
    let reset = r#"<w:rPr><w:u w:val="none"/><w:strike w:val="0"/><w:dstrike w:val="0"/><w:smallCaps w:val="0"/><w:vertAlign w:val="baseline"/><w:highlight w:val="none"/><w:color w:val="auto"/><w:rFonts w:ascii="Arial"/><w:sz w:val="22"/></w:rPr>"#;
    let mut engine = Engine::open(styled_document(DECORATIONS, reset), None).unwrap();
    assert_decoration_reset(&vm(&engine));
    let (bytes, sidecar) = engine.save().unwrap();
    assert_decoration_reset(&vm(&Engine::open(bytes, Some(&sidecar)).unwrap()));
}

#[test]
fn tracked_decoration_and_color_resets_survive_accept_and_restore_on_reject() {
    // dstrike is not a supported set_text_format operation; direct coverage is above.
    let style = DECORATIONS.replace("<w:dstrike/>", "").replace(r#"<w:vertAlign w:val="superscript"/>"#, "");
    for action in ["accept", "reject"] {
        let mut engine = Engine::open(styled_document(&style, ""), None).unwrap();
        assert_eq!(vm(&engine)["blocks"][0]["inlines"][0]["marks"]["u"], "double");
        let result = engine.apply_tx(&json!({"base_version":engine.version(),"ops":[{"op":"set_text_format","anchor":"b100","find":"Styled text","marks":{"underline":false,"strike":false,"small_caps":false,"highlight":"none","color":"auto","font":"Arial","size":11}}]})).unwrap();
        assert_decoration_reset(&vm(&engine));
        engine
            .apply_tx(&json!({"base_version":engine.version(),"ops":[{"op":action,"change_set":result["change_set"]}]}))
            .unwrap();
        let (bytes, sidecar) = engine.save().unwrap();
        let reopened = Engine::open(bytes, Some(&sidecar)).unwrap();
        if action == "accept" {
            assert_decoration_reset(&vm(&reopened));
        } else {
            let model = vm(&reopened);
            let marks = &model["blocks"][0]["inlines"][0]["marks"];
            assert_eq!(marks["u"], "double");
            assert_eq!(marks["strike"], true);
            assert_eq!(marks["smallCaps"], true);

            assert_eq!(marks["highlight"], "yellow");
            assert!(marks.get("color").is_none());
            assert!(marks.get("font").is_none());
            assert!(marks.get("size").is_none());
        }
    }
}

#[test]
fn inherited_vertical_alignment_false_clears_same_kind_and_preserves_opposite() {
    for (kind, mark, vm_mark, opposite) in
        [("superscript", "superscript", "sup", "subscript"), ("subscript", "subscript", "sub", "superscript")]
    {
        for find in [Some("Styled text"), None] {
            for action in ["accept", "reject"] {
                for remove in [mark, opposite] {
                    let style = format!(r#"<w:vertAlign w:val="{kind}"/>"#);
                    let mut engine = Engine::open(styled_document(&style, ""), None).unwrap();
                    let mut op = json!({"op":"set_text_format","anchor":"b100","marks":{remove:false}});
                    if let Some(find) = find {
                        op["find"] = json!(find);
                    }
                    let result = engine.apply_tx(&json!({"base_version":engine.version(),"ops":[op]})).unwrap();
                    let model = vm(&engine);
                    assert_eq!(
                        model["blocks"][0]["inlines"][0]["marks"][vm_mark].as_bool().unwrap_or(false),
                        remove != mark
                    );
                    engine.apply_tx(&json!({"base_version":engine.version(),"ops":[{"op":action,"change_set":result["change_set"]}]})).unwrap();
                    let (bytes, sidecar) = engine.save().unwrap();
                    let reopened = Engine::open(bytes, Some(&sidecar)).unwrap();
                    let model = vm(&reopened);
                    assert_eq!(
                        model["blocks"][0]["inlines"][0]["marks"][vm_mark].as_bool().unwrap_or(false),
                        action == "reject" || remove != mark,
                        "{kind}/{remove}/{action}/{find:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn off_values_override_default_styles_without_an_explicit_style_reference() {
    for source in ["default_style", "document_defaults"] {
        let mut package = quillwork_core::opc::Package::open(document(false), &Default::default()).unwrap();
        let xml = package.part_str("word/document.xml").unwrap().replace(r#"<w:pStyle w:val="Heading1"/>"#, "");
        package.set_part("word/document.xml", xml.into_bytes()).unwrap();
        let defaults = r#"<w:b/><w:i/><w:caps/><w:u w:val="double"/><w:strike/><w:smallCaps/>"#;
        let style = if source == "default_style" {
            format!(
                r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr>{defaults}</w:rPr></w:style>"#
            )
        } else {
            format!("<w:docDefaults><w:rPrDefault><w:rPr>{defaults}</w:rPr></w:rPrDefault></w:docDefaults>")
        };
        package.set_part("word/styles.xml", format!(r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{style}</w:styles>"#).into_bytes()).unwrap();
        let bytes = package.save().unwrap();
        for action in ["accept", "reject"] {
            let mut engine = Engine::open(bytes.clone(), None).unwrap();
            assert_eq!(vm(&engine)["blocks"][0]["fmt"]["b"], true);
            let result = engine.apply_tx(&json!({"base_version":engine.version(),"ops":[{"op":"set_text_format","anchor":"b100","find":"Styled text","marks":{"bold":false,"italic":false,"caps":false,"underline":false,"strike":false,"small_caps":false}}]})).unwrap();
            assert_off(&vm(&engine));
            let model = vm(&engine);
            let marks = &model["blocks"][0]["inlines"][0]["marks"];
            for key in ["u", "strike", "smallCaps"] {
                assert!(marks.get(key).is_none());
            }
            engine
                .apply_tx(
                    &json!({"base_version":engine.version(),"ops":[{"op":action,"change_set":result["change_set"]}]}),
                )
                .unwrap();
            let (saved, sidecar) = engine.save().unwrap();
            let reopened = Engine::open(saved, Some(&sidecar)).unwrap();
            if action == "accept" {
                assert_off(&vm(&reopened));
            } else {
                let model = vm(&reopened);
                let marks = &model["blocks"][0]["inlines"][0]["marks"];
                assert!(marks.get("b").is_none());
                assert_eq!(marks["u"], "double");
                assert_eq!(marks["strike"], true);
                assert_eq!(marks["smallCaps"], true);
            }
        }
    }
}
