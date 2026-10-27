//! Open a .docx, read it the way an agent does, make one tracked edit, save.
//!
//!     cargo run -p quillwork-core --example quickstart -- IN.docx OUT.docx
//!
//! With the sample from `examples/make_sample.py`, anchor `b103` is the first fee clause.

use quillwork_core::engine::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (input, output) = match (args.next(), args.next()) {
        (Some(i), Some(o)) => (i, o),
        _ => return Err("usage: quickstart IN.docx OUT.docx".into()),
    };

    let mut doc = Engine::open(std::fs::read(&input)?, None)?;
    doc.set_agent("AI");

    // Reads and transactions are JSON in, JSON out (spec/schema/ops.schema.json).
    let read: serde_json::Value = serde_json::from_str(&doc.read(r#"{"op": "read"}"#))?;
    println!("{}", read["text"].as_str().unwrap_or_default());

    let tx = serde_json::json!({
        "base_version": doc.version(),
        "ops": [{"op": "replace_text", "anchor": "b103", "find": "16 hours", "replace": "20 hours"}],
    });
    println!("{}", doc.apply_text(&tx.to_string()));

    let (bytes, _sidecar) = doc.save()?; // keep the sidecar to reopen with the same anchors
    std::fs::write(&output, bytes)?;
    Ok(())
}
