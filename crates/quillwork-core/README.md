# quillwork-core

The Rust engine behind [Quillwork](https://github.com/gardan4/quillwork): lossless,
AI-friendly editing of Word (`.docx`) documents.

It keeps the zip package and every XML element as their original bytes, so a no-op save gives
back the input exactly. Edits are spliced in as native tracked changes (`w:ins`, `w:del`) and
comments. Agents read the document as an anchored text projection and send JSON transactions
(see `spec/` in the repository).

```rust
use quillwork_core::engine::Engine;

let mut doc = Engine::open(std::fs::read("contract.docx")?, None)?;
println!("{}", doc.read(r#"{"op": "read"}"#));
println!("{}", doc.apply_text(r#"{"base_version": 1, "ops": [
    {"op": "replace_text", "anchor": "b103", "find": "16 hours", "replace": "20 hours"}]}"#));
let (bytes, _sidecar) = doc.save()?;
```

A runnable version is `examples/quickstart.rs`. Alpha (0.0.x): the API will change.
Licensed under Apache-2.0.
