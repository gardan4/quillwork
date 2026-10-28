//! Public schema conformance without renumbering existing Rust document counters.
use quillwork_core::engine::Engine;
use serde_json::{Value, json};

fn validator(root: Value) -> jsonschema::Validator {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/schema");
    let mut registry = jsonschema::Registry::new();
    for file in
        ["common.schema.json", "view-model.schema.json", "patch.schema.json", "ops.schema.json", "error.schema.json"]
    {
        let schema: Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
        let id = schema["$id"].as_str().unwrap().to_owned();
        registry = registry.add(&id, schema).unwrap();
    }
    let registry = registry.prepare().unwrap();
    jsonschema::options().with_registry(&registry).build(&root).unwrap()
}

fn conforms(schema: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = schema.iter_errors(value).map(|e| format!("{} at {}", e, e.instance_path())).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn fresh_rust_reads_view_models_and_first_patch_conform_without_counter_migration() {
    let schemas = "https://quillwork.example/spec/v0/";
    let read_schema = validator(json!({"$ref":format!("{schemas}ops.schema.json#/$defs/ReadResult")}));
    let result_schema = validator(json!({"$ref":format!("{schemas}ops.schema.json#/$defs/OpResult")}));
    let vm_schema = validator(json!({"$ref":format!("{schemas}view-model.schema.json")}));
    let patch_schema = validator(json!({"$ref":format!("{schemas}patch.schema.json")}));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/fixtures/01-run-formatting.docx");
    let mut engine = Engine::open(std::fs::read(path).unwrap(), None).unwrap();
    assert_eq!(engine.version(), 0);
    conforms(&read_schema, &engine.read_op(&json!({"op":"read"})).unwrap());
    conforms(&vm_schema, &serde_json::to_value(&*engine.view_model().unwrap()).unwrap());
    let (bytes, sidecar) = engine.save().unwrap();
    let mut reopened = Engine::open(bytes, Some(&sidecar)).unwrap();
    assert_eq!(reopened.version(), 0, "persisted counters must not be silently normalized");
    reopened.view_model().unwrap();
    let result = reopened
        .apply_tx(&json!({"base_version":0, "ops":[
            {"op":"replace_text", "anchor":"b100", "find":"plain", "replace":"changed"}
        ]}))
        .unwrap();
    assert_eq!(result["base_version"], 0);
    assert_eq!(result["version"], 1);
    conforms(&result_schema, &result);
    conforms(&patch_schema, &result["patch"]);
}
