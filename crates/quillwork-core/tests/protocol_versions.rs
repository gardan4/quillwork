//! Public request-version compatibility, independent of package/document counters.
use quillwork_core::engine::{Engine, SPEC_VERSION};
use serde_json::{Value, json};

fn engine() -> Engine {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/fixtures/01-run-formatting.docx");
    Engine::open(std::fs::read(path).unwrap(), None).unwrap()
}

fn invalid_versions() -> Vec<Value> {
    vec![
        Value::Null,
        json!(true),
        json!(1),
        json!([]),
        json!({}),
        json!(""),
        json!("nonsense"),
        json!("0.1"),
        json!("v0.1.0"),
        json!("0.1.0.0"),
        json!("00.1.0"),
        json!("0.01.0"),
        json!("0.1.00"),
        json!(" 0.1.0"),
        json!("0.1.0\n"),
        json!("0.1.0-"),
        json!("0.1.0+"),
        json!("0.1.0-alpha..1"),
        json!("0.1.0-01"),
        json!("0.1.0-a_1"),
        json!("0.1.0+build..1"),
        json!("0.1.0+build+other"),
        json!("0.1.0-é"),
        json!("0.2.0"),
        json!("0.99.0"),
        json!("1.0.0"),
        json!("99.0.0"),
        json!("0.999999999999999999999999999999.0"),
    ]
}

#[test]
fn unsupported_and_malformed_versions_are_refused_on_every_read_path() {
    let mut engine = engine();
    let before = engine.save().unwrap();
    for spec in invalid_versions() {
        for op in ["read", "help", "list_changes"] {
            let error = engine.read_op(&json!({"op":op, "spec_version":spec})).unwrap_err();
            assert_eq!(error["error"]["code"], "INVALID_OP", "{op}: {spec}");
            assert_eq!(error["error"]["path"], "/spec_version");
            assert_eq!(error["spec_version"], SPEC_VERSION);
        }
    }
    assert_eq!(engine.save().unwrap(), before);
}

#[test]
fn unsupported_and_malformed_transaction_versions_are_refused_before_mutation() {
    for spec in invalid_versions() {
        let mut engine = engine();
        let before = engine.save().unwrap();
        let error = engine
            .apply_tx(&json!({"spec_version":spec, "base_version":engine.version(),
                "ops":[{"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"changed"}]
            }))
            .unwrap_err();
        assert_eq!(error["error"]["code"], "INVALID_OP", "{spec}");
        assert_eq!(error["error"]["path"], "/spec_version");
        assert_eq!(engine.save().unwrap(), before, "refused version {spec} must be atomic");
    }
}

#[test]
fn compatible_versions_include_historical_patch_and_semver_metadata() {
    for spec in [
        "0.0.0",
        "0.0.99",
        "0.1.0",
        "0.1.42",
        "0.1.999999999999999999999999999999",
        "0.1.0-alpha.1",
        "0.1.0-0",
        "0.1.0--",
        "0.1.0+build.001",
        "0.0.9-rc.2+build-a.01",
    ] {
        let mut engine = engine();
        for op in ["read", "help"] {
            let result = engine.read_op(&json!({"op":op, "spec_version":spec})).unwrap();
            assert_eq!(result["spec_version"], SPEC_VERSION);
        }
        let result = engine
            .apply_tx(&json!({"spec_version":spec, "base_version":engine.version(),
                "ops":[{"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"changed"}]
            }))
            .unwrap();
        assert_eq!(result["spec_version"], SPEC_VERSION);
        assert_eq!(result["patch"]["spec_version"], SPEC_VERSION);
    }
    let mut engine = engine();
    assert!(engine.read_op(&json!({"op":"read"})).is_ok());
    assert!(
        engine
            .apply_tx(&json!({"base_version":engine.version(), "ops":[
                {"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"changed"}
            ]}))
            .is_ok()
    );
}
